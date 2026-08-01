# Cairn — architektura

**Status:** návrh v0.4 · 30. 7. 2026 · **D3 ověřeno měřením, fáze 0 uzavřena**
**Vstup:** brainstorming „Code Knowledge MCP" · kalibrace na an internal repository (§16)
**Rozhodnuto:** CLI + skill místo MCP · Python + Go současně · přenositelné artefakty od začátku · vše v Dockeru

Doprovodné dokumenty:
[coverage-analysis.md](coverage-analysis.md) — ověření čtením kódu, že popsané postupy stačí ·
[spike-0-results.md](spike-0-results.md) — naměřená čísla z fáze 0 (**verdikt GO**)

---

## 0. Teze v jedné větě

Cairn je **lokální daemon s CLI frontendem**, který drží perzistentní, obsahem klíčovaný graf
struktury codebase a odpovídá agentovi na navigační dotazy deterministicky, kompaktně
a s explicitně přiznanou nejistotou — aby agent nemusel grepovat 12 kol.

Není to agent, není to IDE plugin, není to náhrada LLM. Je to **orientační vrstva pod LLM**.

**Tvrdý invariant (D15):** celý index se postaví **bez jediného LLM volání**. Parsování,
symboly, reference, call graph, topologie, entrypointy, routy, git signály — všechno je
deterministické. LLM smí znalost jen *obohatit* (shrnutí, role, invarianty), nikdy ji
nezakládá. Praktický test: **`cairn index` musí doběhnout offline, bez API klíče, a všechny
L0/L1 dotazy musí odpovídat stejně jako s ním.** Viz §3.1.

---

## 1. Hraniční rozhodnutí (co určuje všechno ostatní)

| # | Rozhodnutí | Volba | Proč |
|---|---|---|---|
| D1 | Rozhraní | **CLI binárka + skill. Žádné MCP.** | Agent umí `gh`, `rg`, `jq` — CLI je nativní tvar nástroje, ne náhražka. Odpadá protokol, autorizace i rozpočet na schémata. MCP je později tenká slupka nad týmž query enginem, ne přepis. Viz §6.0. |
| D2 | Procesní model | **Tenký CLI frontend + perzistentní daemon** | Každé zavolání CLI je nový proces, LSP servery startují sekundy až minuty. Stav musí přežít invokaci i session — to je zároveň hlavní diferenciátor oproti Sereně. |
| D3 | Zdroj L0 faktů | **SCIP indexery (bulk) + LSP (hot path)** | Nepsat parsery. SCIP navíc dává hotové schéma stabilních symbol ID. Viz §4. |
| D4 | Schéma faktů | **SCIP jako interní model** | Symbol ID nezávislé na pozici v souboru → per-blob cache je korektní a artefakty jsou přenositelné. |
| D5 | Klíčování cache | **`blob_id` + `deps_api_hash`** | Změna těla funkce neinvaliduje závislé soubory. Viz §5.2 — nejdůležitější detail celého návrhu. |
| D6 | Úložiště | **CAS na disku (sdílitelné) + SQLite (lokální projekce)** | Sdílená cache = přenos souborů, ne replikace DB. Bazel remote cache, ne Postgres. |
| D7 | Latence | **Dotaz má deadline, nikdy neblokuje** | Když čerstvá fakta nejsou, odpověz z cache a přiznej stáří. Agent nesmí čekat na indexaci. |
| D8 | Kontrakt odpovědi | **Každá odpověď nese `unknown:` a `stale:`** | Nepřesná odpověď zastaví agentovo hledání. Přiznaná mezera ne. |
| D9 | Kořeny grafu | **Deployment topologie (compose + Dockerfile) je prvotřídní zdroj faktů** | Call graph bez kořenů je polévka. Compose je jediný strojově čitelný popis systému jako celku a je nutně udržovaný. Viz §8. |
| D10 | Velikost indexu | **Interning + varint + zstd v CAS; nekomprimované projekce v SQLite** | Velikost je hlavně otázka přenosu (studený start = stažení). Serializační schéma je den 1, protože se pak nemigruje. Viz §5.5. |
| D11 | Index vs. git | **Index se do repa necommituje. Do gitu jde jen malý textový souhrn topologie.** | Odvozená data v gitu jsou klasická past (`node_modules`, build outputy). Konflikty nejsou problém k vyřešení, ale příznak. Viz §5.6. |
| D12 | Komentáře | **Extrahovat, indexovat pro fulltext, ale nikdy nevydávat za fakt** | Komentáře jsou nejlepší most mezi jménem featury a symbolem. Zároveň bývají zastaralé. Viz §4.5. |
| D13 | Běhové prostředí | **Všechno v Dockeru — daemon, language servery, indexery, build.** Na hostiteli nesmí být `cargo`, Node ani Go toolchain | Zadání projektu. Má architektonické důsledky pro cesty a watcher, ne jen pro build. Viz §2.1. |
| D14 | Codegen | **Část kódu vyrábí build. Detekovat a přiznat degradaci — nikdy nespouštět.** | Platí napříč ekosystémy (protobuf, GraphQL, Prisma, OpenAPI). Spouštět by porušilo read-only a je to zbytečné: v repu s CI artefakty po prvním buildu existují. Viz §4.6. |
| D15 | Role LLM | **Index se staví kompletně bez LLM. LLM smí znalost jen obohatit, nikdy založit.** | Determinismus je celý pitch — jakmile by struktura záležela na modelu, ztrácí se to, kvůli čemu nástroj existuje. Testovatelné: `cairn index` běží offline bez klíče. Viz §3.1. |
| D16 | Rozšiřitelnost | **Jádro nezná žádný jazyk ani framework. Znalost ekosystémů žije v deklarativních pravidlech, ne v kódu jádra.** | Testovací repo je důkaz funkčnosti, ne specifikace. Další cíl je JS/TS a nesmí to znamenat přepis. Viz §1.1. |

### 1.1 Co je jádro a co je znalost ekosystému (D16)

Nejsnazší způsob, jak tenhle projekt pokazit, je napsat nástroj, který umí jedno repo.
Testovací repo (§16) slouží k **ověření, že obecné řešení funguje** — ne jako zadání.
Konkrétní nálezy z něj jsou v dokumentu značené jako *důkaz*, ne jako specifikace.

Rozdělení do tří vrstev, které určuje, kam co patří a co stojí přidání dalšího ekosystému:

```
  ┌──────────────────────────────────────────────────────────────┐
  │ A · JÁDRO — nezná jazyk ani framework                        │
  │   snapshot · CAS · deps_api_hash · SCIP schéma · graf ·      │
  │   ranking · handles · formátování · git L3 · daemon · CLI    │
  │   Přidání jazyka sem NESAHÁ.                                 │
  ├──────────────────────────────────────────────────────────────┤
  │ B · PRAVIDLA — data, ne kód                                  │
  │   rules/python.toml · go.toml · typescript.toml              │
  │   entrypointy · routy · registrace služeb · konvence jmen    │
  │   generovaného kódu · detekce generovaných souborů           │
  │   Přidání frameworku = pravidlo, ne commit do Rustu.         │
  ├──────────────────────────────────────────────────────────────┤
  │ C · ADAPTÉRY — malý kód, vzácně                              │
  │   language provider (indexer + LSP + gramatika komentářů)    │
  │   binder, který pravidlo neunese (parser .proto, tsconfig)   │
  └──────────────────────────────────────────────────────────────┘
```

### Vrstva A je většina hodnoty

Symboly, reference, call graph, komentáře, blast radius, co-change z gitu — nic z toho
neví, jakým jazykem je kód napsaný. Stojí to na SCIP schématu, které je jazykově neutrální
z definice. **Tahle vrstva funguje na JS/TS ve chvíli, kdy existuje `scip-typescript`** —
a ten existuje.

### Vrstva B: uzavřená sada tvarů, ne obecný DSL

Pokušení je napsat query jazyk nad AST. To je past — skončí to vlastním parserem
v jiném převleku. Reálné případy z Pythonu, Go, TS i JS se ale skládají z malé uzavřené
množiny **tvarů**:

| tvar | příklad |
|---|---|
| `call_pattern` | `Register{Service}Server(s, $impl)` · `app.get($path, $handler)` |
| `decorator` | `@$router.get($path)` · `@shared_task` |
| `inherits` | `class $X($pkg.{Service}Base)` |
| `collection_literal` | `urlpatterns = [ path($p, $view), … ]` |
| `command_string` | `python -m $mod` · `next start` · `/bin/$binary` |
| `path_convention` | `app/api/**/route.ts` → `$method /api/**` |

Šest tvarů, ne obecný jazyk. **Nový tvar se přidává, až když ho vyžádají aspoň dva
nezávislé reálné případy** — to je pojistka proti bobtnání.

Pravidlo je pak data:

```toml
[[rule]]
id    = "grpc-go.register"
lang  = "go"
shape = "call_pattern"
match = { name = "Register(?<service>\\w+)Server", args = ["$server", "$impl"] }
emit  = { edge = "implements", from = "$impl", to = "proto:{service}" }
```

Pravidla se dodávají v balíčcích (`rules/*.toml`), ale **repo si je smí přebít nebo doplnit**
v `.cairn/rules.toml`. Interní framework, který nikdo jiný nemá, je tak řešitelný bez forku.

### Vrstva C: kdy je kód v pořádku

Když tvar nestačí. Parser `.proto`, čtení `tsconfig.json` `paths`, resolver
multi-stage Dockerfile. Držet malé a vzácné; každý nový adaptér je závazek na údržbu.

### Testovací podmínka pro D16

**Přidání JS/TS smí znamenat: jeden language provider (vrstva C) + jeden balíček
pravidel (vrstva B). Nula změn ve vrstvě A.** Jestli to tak nevyjde, je návrh špatně.
Konkrétní průchod tímhle cvičením je v §17.

---

## 2. Procesní topologie

```
  agent (coding agent / …) nebo člověk nebo CI
            │  spustí příkaz, čte stdout
            ▼
     ┌──────────────┐   spustí daemon, pokud neběží
     │ cairn refs a4│   bezstavový, ~5 MB RSS, start <30 ms
     └──────┬───────┘
            │  unix socket (Windows: named pipe), length-prefixed msgpack
            ▼
     ┌──────────────────────────────────────────────────────┐
     │  cairnd  — jeden proces na stroj, N workspaců        │
     │                                                       │
     │   query engine  ──►  store (CAS + SQLite)            │
     │        ▲                    ▲                         │
     │        │                    │                         │
     │   scheduler ──┬── LSP pool ─┤   pyright-langserver    │
     │               │             │   gopls                 │
     │               ├── SCIP runs ┤   scip-python, scip-go  │
     │               ├── watcher ──┤   notify(2)             │
     │               └── git ──────┘   gix                   │
     └──────────────────────────────────────────────────────┘
                        │ (fáze 6, volitelné)
                        ▼  GET/PUT /cas/{blake3}
                 sdílená cache týmu
```

**Proč tenký frontend:** agent zavolá `cairn` desetkrát za minutu a pokaždé je to nový proces.
Kdyby si každý startoval LSP pool, nedoběhl by ani první dotaz. Frontend je hloupý pipe;
veškerý stav a všechny subprocesy vlastní daemon.

**Rozpočet na start CLI je 30 ms.** To je tvrdý požadavek plynoucí z D1 — u MCP se platil
start jednou za session, u CLI při každém dotazu. Znamená to: žádné parsování konfigurace
mimo potřebu, žádné skenování filesystému, connect na socket a hned dotaz.

**Životnost daemonu:** auto-start z frontendu (jako `gopls`/`tmux`), idle timeout ~30 min bez
připojeného klienta, ale **index na disku zůstává** — restart daemonu je studený start procesu,
ne studený start znalosti.

### 2.1 Všechno běží v Dockeru (D13)

Na hostiteli nesmí být `cargo`, `rustup`, Node ani Go toolchain. To není jen build policy —
mění to tři věci v architektuře.

```
Coding agent
   │  stdio
   ▼
docker compose run --rm cairn refs a4    ← frontend, jednorázový, bez stavu
   │  unix socket ve sdíleném volume
   ▼
služba `cairn-daemon`                    ← dlouhoběžící compose služba
   ├── /workspace   ← bind mount repa, read-only
   ├── /cache       ← named volume: CAS + SQLite, přežívá restart
   └── v image: pyright-langserver, gopls, scip-python, scip-go
```

Agent spouští příkazy, takže `docker compose run --rm` je z jeho pohledu totéž co binárka.
V praxi se to schová za shell wrapper `cairn` na `PATH`, aby to agent psal přirozeně.

**Důsledek 1 — cesty.** Uvnitř kontejneru je repo `/workspace/srcpy/…`, na hostiteli
`/home/user/backend/srcpy/…`. Kdyby odpovědi nesly kontejnerové cesty, agent by soubory
neotevřel. Řeší to pravidlo, které v návrhu už je z jiného důvodu: §5.1 bod 1 zakazuje
absolutní cesty kvůli přenositelnosti artefaktů. **Všechno je relativní ke kořeni workspace**,
takže `srcpy/domains/orders/grpc/server.py:42` funguje v kontejneru i na hostiteli.
Ta dvě rozhodnutí se potkávají náhodou, ale hezky.

**Důsledek 2 — watcher.** `inotify` přes bind mount funguje nativně na Linuxu.
Na Docker Desktopu (macOS/Windows) je nespolehlivý — fallback na polling s delším intervalem,
nebo nechat frontend posílat explicitní „tenhle soubor se změnil". Riziko je v §14.

**Důsledek 3 — velikost image.** Daemon image musí obsahovat Node (pyright), Go toolchain
(gopls, scip-go) i Python (scip-python). Není to malé, ale je to jednorázové a přesně to
už testovací repo dělá se svými `pbgen` a `go-compiler` image.

Spike nástroje (§13, fáze 0) běží stejným způsobem — `scip-python` ani `scip-go`
se na hostitele neinstalují.

---

## 3. Vrstvy znalosti

Beze změny oproti brainstormingu, jen s explicitním kontraktem přesnosti:

| | Obsah | Zdroj | Kontrakt | Invalidace |
|---|---|---|---|---|
| **L0** Strukturální fakta | definice, výskyty, importy, typy | SCIP indexer / LSP | **100 % recall, jinak nevracet** | per-blob, ms |
| **L0-C** Komentáře | docstringy, komentáře, TODO, markdown | SCIP + tree-sitter (§4.5) | **exaktně extrahované, sémanticky nedůvěryhodné** — jen pro vyhledávání, nikdy jako tvrzení | per-blob, ms |
| **L0-D** Deployment fakta | služby, entrypointy, porty, env, routy, mapa kontejner↔repo | compose, Dockerfile, urls.py (§8) | exaktní tam, kde parsuje; jinak `unknown` | per-soubor, ms |
| **L1** Odvozená struktura | reference, call graph, blast radius, reverse deps, dosažitelnost ze služby | join nad L0 + L0-D, čistý kód | 100 % vůči L0 | inkrementální, ms |
| **L2** Sémantika | summaries, role, invarianty, koncepty | LLM, lazy | smí zastarat, má `confidence` + `age` | volná, na pozadí |
| **L3** Execution | co se mění společně, test impact, runtime call graph | git log, coverage | statistický, vrací skóre | per-commit / per-test-run |

**Klíčové:** L0 a L1 nikdy nemíchat s L2/L3 v jedné nekvalifikované odpovědi. Když
`cairn blast` vrátí 4 statické volající a 3 co-change kandidáty, musí být v odpovědi
vizuálně oddělené — jinak agent vezme statistiku za fakt.

### 3.1 Dělicí čára: L2 je jediná vrstva s LLM (D15)

```
  ┌─────────────────────────────────────────────────────────┐
  │  L0 · L0-C · L0-D · L1 · L3                             │
  │  deterministické · offline · bez API klíče · 100 % recall│
  │  ── postaví se samo, kompletně, opakovatelně ──          │
  └─────────────────────────────────────────────────────────┘
                            ▲
                            │  smí přidávat, nikdy nezakládá
  ┌─────────────────────────┴───────────────────────────────┐
  │  L2 — shrnutí, role, invarianty, koncepty                │
  │  volitelné · lazy · s confidence · vždy odstranitelné    │
  └─────────────────────────────────────────────────────────┘
```

Tři pravidla, která z toho plynou a jsou testovatelná:

1. **`cairn index` běží offline.** Bez sítě, bez klíče, bez modelu. V CI to je jeden test.
2. **Smazání celé L2 nesmí změnit ani jednu L0/L1/L3 odpověď.** Regresní test:
   spustit sadu dotazů, vyprázdnit L2, spustit znovu, porovnat. Rozdíl = chyba.
3. **L2 nikdy nevstupuje do výpočtu.** Nesmí ovlivnit ranking, dosažitelnost, blast radius
   ani seed. Smí se jen zobrazit — vždy označené, jako u komentářů (§4.5).

**Kdo L2 vyrábí, když cairn nemá LLM ani MCP sampling (D1).** Nejlevnější zdroj je
**volající agent sám**: model, který zrovna četl `TokenValidator`, ho umí popsat zadarmo,
protože tu práci už udělal. Proto:

```
cairn note <handle> --summary "…" [--confidence high|low]
```

Není to porušení „read-only" (§6.1) — cairn nezapisuje do repa, jen do vlastní cache.
Zápis do zdrojáků zůstává zakázaný.

Dávkové obohacení vlastním klíčem (`cairn enrich --model …`) je až druhá varianta a je
striktně opt-in. Výchozí instalace nikam nevolá.

---

## 4. Získávání L0: tři rychlosti

Nejčastější chyba u nástrojů tohoto typu: postavit všechno na LSP. LSP je **dotazovací**
protokol, ne indexační. „Dej mi reference všech symbolů" = O(n) round-tripů = hodiny.

### 4.1 Studená / dávková cesta — SCIP indexery

`scip-python` (Sourcegraph, postavené nad pyright) a `scip-go` proběhnou celý projekt
a vyplivnou SCIP index: pro každý dokument seznam **occurrences** (rozsah + symbol ID + role
definition/reference/write) a **symbol information** (dokumentace, vztahy).

Co tím dostaneme zadarmo:
- stabilní, na pozici nezávislá symbol ID (`scip-python python . . auth/oauth.py/TokenValidator#validate().`)
- hotový model, který je *navržený* pro cross-repo a cross-language propojení
- ekosystém dalších indexerů, až se bude přidávat jazyk (TS, Java, Rust, Ruby)

Nevýhoda: běží nad celým projektem, ne inkrementálně. Proto:

### 4.2 Horká cesta — LSP pro dirty soubory

Rozpracovaný / neuložený soubor jde přes `pyright-langserver` resp. `gopls`:
`documentSymbol`, `references`, `definition`, `implementation`, `callHierarchy`.
Výsledek se přemapuje do stejného SCIP schématu a **překryje** bázi.

Latence: 10–100 ms na soubor. To je přesně ten dotaz, na kterém nejvíc záleží
(„právě jsem změnil signaturu, koho jsem rozbil").

### 4.2b LSP pool — druhá polovina overlaye

Dávkový indexer neumí odpovědět na soubor, který se změnil, bez plného běhu. Teplý
language server ano, a **naměřeno**: po editaci stojí `documentSymbol` 4–5 ms u pyrightu
a 3,6–7,3 ms u gopls, `references` 94–115 ms a 23–27 ms (spike-0-results §4.2c).

Celý `cairn live <soubor>` — start procesu, socket, LSP dotaz, dotaz do indexu i
formátování — vychází na **11 ms medián**.

Tři věci, které přineslo měření a promítly se do kódu:

- **Klient musí odpovídat na požadavky serveru.** pyright si během startu vyžádá
  `workspace/configuration` a než dostane odpověď, neobslouží nic. První verze benchmarku
  je ignorovala a „naměřila" 180s timeout na každém dotazu.
- **První dotaz je jiná kategorie.** pyrightu trvalo první `references` 1 353 ms i po
  zahřátí, proti 130 ms teplým. Proto pool zahřívá servery na pozadí při startu a proto
  má klient **různé timeouty podle druhu požadavku**: `dirty` se ptá při každém volání
  CLI a musí mít těsný strop, aby zaseknutý daemon nezdržoval běžný dotaz; LSP dotaz
  dostane prostor na studený případ.
- **Jazyky nejsou symetrické.** pyright je na hot path zhruba 4× pomalejší než gopls —
  což je obrácený obrázek než u dávkové cesty, kde naopak Go nemá levný částečný reindex.

#### Co overlay ukazuje

Ne živý výpis, ale **porovnání**: co server vidí teď proti tomu, co má index.

```
$ cairn live srcpy/domains/orders/mcp/middleware.py
+ …:136-138  BrandNewMiddleware
+ …:137-138  BrandNewMiddleware.on_call_tool
stale: the index is behind for this file: 2 new, 0 moved, 1 gone
```

Porovnávat se musí **kvalifikovanými jmény**. Dvě třídy v jednom souboru mohou mít
metodu téhož jména a porovnání holých jmen je spáruje a vymyslí přesun, který se nestal.

Zbývající nepřesnost, přiznaná: index zná `__init__`, které `documentSymbol` nevypisuje,
takže se hlásí jako `gone`. Je to jeden záznam a je poctivě označený, ne skrytý.

### 4.3b Daemon drží živý stav, ne dotazy

Naivní varianta by nechala daemona proxovat všechny dotazy. **Zamítnuto po měření:**
SQLite ve WAL zvládne souběžné čtenáře a start CLI je ~1 ms, takže proxy by přidala
latenci a nekoupila nic.

Daemon existuje kvůli tomu, co **jednorázový proces mít nemůže: živý stav.** Dnes watcher,
který běží už předtím, než se někdo zeptal; zítra teplé language servery. Odpovídá proto
na jedinou otázku — *co se změnilo od indexace* — a CLI si to složí do sekce `stale:`.
Až přijde LSP pool, připojí se ze stejného důvodu a protokol přiroste o jeden požadavek,
místo aby změnil tvar.

Tři věci, které se ukázaly jako podstatné:

- **Špinavost se měří proti indexu, ne proti poslední události.** Soubor je špinavý, když
  se jeho obsah liší od zaindexovaného — takže úprava a její vrácení nezanechá nic.
  Kdyby se počítaly události, `git checkout` by označil půl stromu bez jediné reálné změny.
- **Prázdná a neznámá množina nejsou totéž.** Bez daemona se nehlásí „čisto", ale
  `stale: not tracked`. Splynutí těch dvou je přesně ta tichá zastaralost, kterou D8 zakazuje.
- **Označuje se odpověď, ne index.** Dotaz na symbol ze změněného souboru to přizná;
  dotaz vedle zůstane čistý. Plošné „index je starý" by se naučilo ignorovat.

### 4.3 Overlay není zvláštní mechanismus

Protože je všechno klíčované obsahem, „dirty soubor" je jen soubor s jiným `blob_id`.
Jediná měnitelná věc v systému je **snapshot**:

```
snapshot = { relativní cesta → blob_id }
```

- `head_snapshot` — čte se z git tree (gix), zdarma
- `working_snapshot` — filesystem + watcher, výchozí pro dotazy

Přepnutí větve = výměna snapshotu ≈ 0 práce, protože fakta pod ním jsou nezměněná.
Rebase / amend / squash / force-push = úplně bez efektu, commit hashe systém nezajímají.

> **Architektonická páteř:** snapshot je jediná mutable věc; všechno pod ním
> jsou immutable, obsahem adresovaná fakta.

### 4.4 Riziko, které je nutné ověřit do 2 týdnů

`scip-python` je Sourcegraph projekt s kolísavou údržbou a Django ORM je přesně to,
na čem pyright klopýtá. **První úkol po založení repa: pustit scip-python na testovací
repo a změřit, kolik symbolů zůstane nevyřešených.** Pokud > ~15 %, plán se mění
(fallback: LSP bulk crawl s omezením na exportované symboly, pomalejší studený start).

Pro Django byla domněnka, že to levně vyřeší stub balíčky `django-types` /
`django-stubs` nakonfigurované pro pyright, a že to pokryje 90 %.

**Změřeno a neplatí.** `django-types` se nainstaloval automaticky do indexační kopie
a index vzrostl o 2,6 % výskytů — ale `LedgerEntry.ledger_category` se posunulo
z 0 rozřešených míst užití na 5 ve dvou souborech, zatímco to jméno se vyskytuje
ve **33 souborech**.

Důvod: problém není typ pole, ale typ *držitele*. `for tx in transactions`, kde
`transactions` přišlo z querysetu, není bez mypy pluginu (který pyright spustit neumí)
typované jako `LedgerEntry`, takže `tx.ledger_category` se nemá k čemu rozřešit.
Stuby popisují model, ne to, co z něj queryset vrací.

**Důsledek pro návrh:** Python strana má na ORM-těžkém kódu **strukturální strop**, který
konfigurace neodstraní. Zbývají tři cesty a všechny jsou dražší, než §4.4 předpokládala:

| cesta | kdo to musí udělat |
|---|---|
| anotace v repu (`tx: LedgerEntry`) | vlastník repa — mění zdrojáky |
| runtime trace (§9) — skutečné typy z běhu testů | cairn, ale je to celá vrstva L3 |
| přiznat mez v odpovědi | **hotovo** — atribut na typu nese výhradu, že jde o dolní odhad |

Do té doby platí to poslední: nerozřeší to ani jednu referenci navíc, ale mění tichou
špatnou odpověď na přiznanou.

### 4.5 Třetí rychlost: komentáře a dokumentace

Komentáře jsou **nejlepší existující most mezi jménem featury a symbolem**. „OAuth" se
často nevyskytuje v žádném identifikátoru, ale je hned v prvním řádku docstringu. Bez nich
stojí `cairn context` na fuzzy matchi jmen a cest, což je ta slabší polovina §6.4.

#### Odkud

| zdroj | jak | cena |
|---|---|---|
| Docstringy symbolů | `SymbolInformation.documentation` — **SCIP to už nese**, jen to nezahazovat | nula |
| Inline a blokové komentáře, modulové hlavičky | tree-sitter | ms/soubor |
| Markdown v repu (README, ADR, `docs/`) | prostý parser + nadpisy jako oddíly | ms |

#### Tady je tree-sitter správný nástroj

V brainstormingu je tree-sitter odmítnutý pro L0 — správně, dá parse tree, ne name
resolution, a u C# nebo Djanga selže tiše. **Komentáře jsou přesná výjimka: není co
rozřešovat.** Je to čistě lexikální a poziční extrakce. Žádné overloady, žádná generika,
žádné partial classes. Tree-sitter je tu levný, přesný a jazykově univerzální — a přidání
dalšího jazyka stojí jednu gramatiku, ne celý indexer.

#### Přiřazení ke symbolu, ne textová polévka

Komentář se váže na nejbližší následující definici (leading blok) nebo na obklopující symbol
(inline). Tím je fulltext **scopovaný**: shoda v komentáři vrátí symbol s handlem, ne
„soubor, kde se to někde vyskytuje". Nepřiřazené komentáře (modulové hlavičky) se váží
na soubor.

#### Kontrakt pravdivosti — jiný než u zbytku L0

Komentář je extrahovaný **exaktně** (text je text), ale jeho *tvrzení* je neověřené a bývá
zastaralé. Proto:

- komentáře se používají pro **vyhledání kandidátů**, nikdy jako fakt v odpovědi
- když se komentář v odpovědi cituje, je označený `[comment, unverified]`
- v `symbols_fts` mají vlastní sloupec s **nižší vahou** než jméno symbolu: shoda ve jméně
  > shoda v docstringu > shoda v inline komentáři
- **zakomentovaný kód se detekuje a downrankuje** (řádek, který se parsuje jako kód) —
  jinak je to největší zdroj šumu ve fulltextu

Tenhle rozdíl je nutné držet: je to jediná část L0, která je exaktně extrahovaná, ale
sémanticky nedůvěryhodná. Míchat ji s referencemi by rozbilo kontrakt „L0 = 100 % nebo `unknown`".

#### Vedlejší produkty zadarmo

`TODO` / `FIXME` / `HACK` / `XXX` jako vlastní `kind` hrany. Pro auditní doménu — což je
podle brainstormingu cílový trh — je „ukaž mi všechny FIXME v kódu dosažitelném z veřejného
endpointu" (§8.7) dotaz, na který dnes neodpoví nic.

Invalidace beze změny: komentáře jsou per-soubor, obsahem klíčované jako všechno ostatní,
a nezávisí na `deps_api_hash` (nemají závislosti).

### 4.6 Chybějící codegen — indexovat jde, mlčet se nesmí

**Obecný jev:** část kódu nemusí v pracovním stromě existovat, protože ji vyrábí build.
Předpoklad „co je v repu, to je celý kód" neplatí u protobuf/gRPC, GraphQL codegen,
OpenAPI klientů, Thriftu, ORM stub generátorů, .NET source generators,
Prisma klienta i `next build` typů. Napříč jazyky, ne v jednom.

Následek není „pár nerozřešených referencí". Když na generovaném symbolu visí dědičnost
nebo typ, chybějící artefakt sebere **celou plochu**, která přes něj vede.

#### Chování: detekovat, indexovat, přiznat

Pravidlo (vrstva B, §1.1) popisuje pro daný ekosystém dvojici *vstupy → očekávané výstupy*:

```toml
[[codegen]]
id       = "protobuf.python"
inputs   = ["**/*.proto"]
produces = ["**/*_pb2.py", "**/*_pb2_grpc.py"]
hint     = "run your protobuf generation step"
```

| stav | chování |
|---|---|
| výstupy existují a nejsou starší než vstupy | indexuje se normálně |
| chybí, nebo jsou zastaralé | **indexuje se dál, ale index je `degraded:`** |

Druhý řádek je celá pointa. Bez něj by nástroj tvrdil „3 reference", kde jich je 200 —
přesně ten tichý fail, kterému se vyhýbá D8. Příznak jde do `cairn status`
**a do každé odpovědi**:

```
degraded: generated sources missing or stale (protobuf.python).
          References crossing that boundary are incomplete.
          hint: run your protobuf generation step
```

#### Cairn nic nespouští

Dřívější verze návrhu tady měla „prepare krok", který si codegen sám pustí. Zrušeno,
ze dvou důvodů. Za prvé by to porušilo read-only kontrakt (§6.1) — codegen zapisuje
do pracovního stromu. Za druhé je to zbytečné: **v repu s CI, které generovaný kód hlídá,
existují artefakty v každém checkoutu, kde někdo jednou buildil nebo pustil testy.**
Stav „chybí" je přechodný a týká se hlavně čerstvého clonu.

Zůstává tedy jen detekce a poctivé přiznání. Levné, univerzální, bez vedlejších účinků.

*Poznámka k důkazu: dřívější verze sem uváděla testovací repo jako příklad chybějících
Python stubů. Bylo to měření špatně — stuby jsou commitnuté, jen je betterproto2 sype do
`__init__.py` místo `*_pb2.py`. Mechanismus platí obecně, tohle repo ale jeho příkladem
není. Viz [spike-0-results.md](spike-0-results.md) §5.*

---

## 5. Storage a cache

### 5.1 Dva druhy dat

```
~/.cache/cairn/
  cas/                      immutable, obsahem adresované, SDÍLITELNÉ
    blake3/ab/cd/abcd…      FileFacts záznam (msgpack, deterministická serializace)
    blake3/…                celý SCIP index pro tree_hash (hrubá granularita)
  ws/<workspace-id>/
    index.sqlite            LOKÁLNÍ projekce, kdykoliv přepočitatelná z CAS
    snapshot.bin
```

**CAS = pravda a sdílený artefakt. SQLite = materializovaný pohled.**

Důsledek pro sdílenou cache (rozhodnuto že ano): sync vrstva je hloupý přenos souborů —
`GET /cas/{hash}`, `PUT /cas/{hash}`. Immutable, žádná invalidace, žádné konflikty,
žádná replikace DB. Sémantika Bazel remote cache / Nix binary cache.

**Co to stojí dnes** (a je to celá cena za to, že se návrh nebude přepisovat):
1. žádné absolutní cesty v CAS záznamech — vše relativně ke kořeni workspace
2. deterministická serializace — žádné pořadí `HashMap`, seřazené kolekce
3. každý záznam nese `schema_version` + `indexer_id@version` (např. `scip-python@0.6.0`, `pyright@1.1.403`)
4. žádná lokální ID (rowid, pointery) v přenositelných strukturách — interning ano, ale
   **lokálně v rámci jednoho záznamu** (§5.5), nikdy odkazem do globální tabulky
5. **adresuje se hash nekomprimovaného obsahu, ukládá se komprimovaně** — komprese je pak
   čistě detail úložiště a změna kompresního slovníku nezpůsobí churn celé CAS

### 5.2 Klíčování — nejdůležitější detail

Naivní `key = blob_id` je **nekorektní**: fakta o souboru závisí na jeho závislostech
(`from .models import User` se nevyřeší bez `models.py`).

Naivní `key = (blob_id, hash celého dependency closure)` je **k ničemu**: změna jednoho
listu invaliduje celý strom nad ním.

Volba:

```
key = (blob_id, deps_api_hash, indexer_version, schema_version)

deps_api_hash = hash( pro každý importovaný modul: jeho seřazená množina
                      exportovaných symbolů + jejich signatury )
```

Tj. **hash veřejného rozhraní závislostí, ne jejich obsahu.** Změna těla funkce
v `models.py` → `deps_api_hash` se nemění → všechny závislé soubory zůstávají v cache.
Změna signatury → invaliduje se přesně to, co se invalidovat má.

Je to stejný trik jako header jars v Bazelu nebo interface hashe v Rustově inkrementální
kompilaci. Konverguje, protože `deps_api_hash` se počítá z už zacachovaných faktů
importovaných modulů — ne z nového parsování.

*(Cykly v importech: SCC se hashuje jako celek. U Pythonu to je vzácné a malé, u Go
to zakazuje kompilátor.)*

### 5.3 Dvě granularity sdílení

| Granularita | Klíč | Kdy pomáhá |
|---|---|---|
| Celý index projektu | `(git tree_hash, indexer_versions)` | Nový člen týmu / CI / čerstvý clone → **studený start = stažení, ne indexace** |
| Per-file facts | `(blob_id, deps_api_hash, …)` | Denní práce, sdílení mezi větvemi a mezi vývojáři |

### 5.4 SQLite schéma (skica)

```sql
-- interning: řetězce žijí právě jednou
strings(id INTEGER PK, s TEXT UNIQUE)          -- cesty, jména, deskriptory
symbols(id INTEGER PK,
        parent_id INTEGER REFERENCES symbols,  -- prefix sdílení: třída → metoda
        desc_id   INTEGER REFERENCES strings,  -- jen poslední deskriptor
        lang, kind, flags)
files(id INTEGER PK, path_id INTEGER REFERENCES strings, blob_id BLOB, lang, generated BOOL)

occurrences(file_id, symbol_id, line, col_start, col_end, role)
   INDEX (symbol_id, role)          -- cairn refs
   INDEX (file_id, line)            -- „co je na tomhle řádku"

edges(src_symbol, dst_symbol, kind, confidence, source)
   -- kind:   calls | implements | overrides | binds | tests | co_changes
   --         | entrypoint | routes_to | reads_env | talks_to
   -- source: scip | lsp | proto | compose | dockerfile | route | env | git | trace
services(id INTEGER PK, name_id, lang, kind)   -- kind: built | external
comments(file_id, symbol_id NULL, line, kind, text)   -- §4.5
   -- kind: docstring | leading | inline | module | todo | commented_out
handles(symbol_id, handle TEXT UNIQUE)
unknowns(file_id, line, reason, hint)

-- FTS5, sloupce s klesající vahou; pohání `cairn symbol` a seed pro `cairn context` (§6.4)
search_fts(name, path, docstring, comment, commit_msg, doc_md)
```

Dvě věci ve schématu, které se dělají den 1, protože pozdější zavedení je migrace:

- **`strings` interning.** Cesty a jména deskriptorů se opakují v každém výskytu.
- **`symbols.parent_id`.** SCIP symbol je hierarchický řetězec
  (`… auth/oauth.py/TokenValidator#validate().`). Ukládat celý řetězec u každého symbolu
  znamená u třídy s 30 metodami 30× zopakovat cestu i jméno třídy. Parent pointer +
  poslední deskriptor to složí za běhu a zároveň dá zadarmo dotaz „všechny členy této třídy".

Jednotný `edges` s `kind` + `source` + `confidence` je záměr: L1 (statické, confidence 1.0),
L0-D (deployment, §8), L3 (statistické, confidence < 1) i binders (§7) žijí ve stejné tabulce
a odpovědní vrstva je odděluje podle `source`.

**Zápisy:** jediný writer task (SQLite WAL), čtení z read poolu. Dotaz nikdy nečeká na zápis.

### 5.5 Velikost indexu a serializace

Velikost není kosmetika: **studený start pro nového člena týmu = stažení indexu.** Proto je
rozpočet definovaný přenosem, ne diskem.

**Cíl:** plný index pro repo o 500k řádcích ≤ 50 MB komprimovaně, aby cold start
přes sdílenou cache vyšel pod 10 s na běžné lince. *(K ověření ve fázi 0 — surový SCIP index
takového repa bývá řádově stovky MB, takže je potřeba 5–10×.)*

#### Napětí, které je nutné rozřešit explicitně

Interning na int32 a přenositelnost artefaktů jdou proti sobě: globálně přidělené ID je
z definice lokální a nepřenositelné. Řešení je mít **dvě reprezentace**, ne kompromis:

| | CAS záznam (trvalý, sdílený) | SQLite projekce (dotazovací) |
|---|---|---|
| Optimalizuje | velikost | latenci |
| Interning | **lokální tabulka řetězců uvnitř záznamu** — záznam je samopopisný | globální `strings` tabulka |
| Reference | int32 index do lokální tabulky | globální rowid |
| Komprese | zstd, adresuje se nekomprimovaný hash | žádná |
| Čte se | při plnění cache, ne při dotazu | při každém dotazu |

Tím napětí mizí: záznam v CAS je samostatně dekódovatelný na jakémkoli stroji, a přesto
uvnitř neopakuje ani jeden řetězec. Rozhodnutí D6 (dva sklady) bylo správné právě proto.

#### Konkrétní techniky, sestupně podle výnosu

1. **Lokální tabulka symbolů a řetězců v každém dokumentu.** Dokument typicky odkazuje
   desítky až stovky symbolů, ale má tisíce výskytů → int16/int32 index místo řetězce.
   *(Tohle SCIP ve svém formátu už dělá — přebíráme, nevymýšlíme.)*
2. **Delta + varint na pozice.** Výskyty seřadit podle pozice a ukládat rozdíly řádků
   a sloupců. Většina delt se vejde do jednoho bajtu.
3. **Prefixová dekompozice symbolů.** Totéž co `parent_id` v SQLite, jen v serializované
   podobě: `(parent_index, suffix)`.
4. **Role jako bitfield**, ne enum string.
5. **zstd s trénovaným slovníkem.** CAS je hodně malých, vzájemně velmi podobných záznamů —
   přesně ten případ, kde samostatná komprese malého souboru selhává a sdílený slovník
   dává násobky. Slovník je verzovaný artefakt v CAS jako každý jiný; protože se adresuje
   nekomprimovaný obsah, jeho výměna nezpůsobí přeadresování ničeho.
6. **Generovaný kód ukládat, ale odděleně.** `*_pb2.py` a `*.pb.go` bývají většina bajtů
   indexu a téměř nikdy nejsou v odpovědi (§7.3). Vlastní CAS namespace → sdílená cache
   je může přeskočit a stáhnout lazy.

Inkrementalita je tady spojenec, jak jsi psal: záznam se komprimuje jednou a čte mnohokrát,
takže si můžeme dovolit dražší kompresi, než kdyby se přepisoval celý index.

#### Kde je hranice

Interning, varint a zstd jsou **schéma a serializace** — levné, permanentní, pozdější
zavedení je bolestivá migrace. Vlastní storage engine, mmap a B+ tree jsou něco jiného
a v §13 zůstávají na seznamu „nikdy". Tenhle rozdíl je snadné rozmazat: obojí se dá popsat
jako „optimalizace úložiště". Není to totéž — jedno je tvar dat, druhé je vlastní databáze.

### 5.6 Index a git

Otázka zní, jestli index commitovat do repa a co s konflikty. Odpověď má tři patra a první
z nich mění zadání.

#### Konflikt způsobuje monolit, ne binárnost

Jeden soubor obsahující celý index bude konfliktovat při každém merge **bez ohledu na formát**.
Textový formát nedá řešitelný konflikt, jen nečitelný — deset tisíc řádků přeházených
záznamů, kde „vyřeš ručně" nedává smysl. Vlastní textový formát tenhle problém neřeší,
jen ho převleče.

Naproti tomu **obsahem adresované záznamy konfliktovat nemohou z definice.** §5.1 bod 2
vyžaduje deterministickou serializaci — takže dva vývojáři, kteří zaindexují stejný blob,
vyprodukují **bajt po bajtu stejný soubor**. Sloučení dvou CAS je sjednocení množin, ne merge.
Není co řešit.

Není to náhoda: **git object store je přesně tentýž nápad.** Immutable objekty pojmenované
hashem obsahu. Nikdo neřeší konflikty v `.git/objects`.

#### Ale index do repa stejně nepatří

Zabiják není konflikt, je to **bloat**. Git si pamatuje každou verzi navždy, index se mění
prakticky při každém commitu a binární obsah se nedeltuje dobře. Po pár stovkách commitů
je clone nepoužitelný — a zpětně se to čistí jen přepsáním historie.

K tomu se přidává, že je to **odvozená data**. Stejná kategorie jako `node_modules`, build
outputy a generovaný kód: každý, kdo to jednou commitnul, toho litoval. Index je z definice
kdykoli přepočitatelný z obsahu repa (§5.1) — to je celá pointa content addressingu.

Plus šum: každý PR by měl v diffu megabajty změn, které nikdo nečte.

#### Čitelnost je vlastnost CLI, ne formátu

Námitka „nečitelný, nedifovatelný" má správnou odpověď v nástroji, ne ve formátu:

```
cairn inspect <hash>        → čitelný dump záznamu
cairn diff <hash> <hash>    → rozdíl dvou verzí faktů o souboru
```

Přesně jako `git cat-file -p`. Nikdo kvůli čitelnosti nedělá git objekty textové.
A diffovat dva CAS záznamy je stejně vzácná operace jako diffovat dva git blob objekty —
zajímá tě to jednou za čas při ladění indexeru, ne v běžné práci.

#### Jak tedy sdílet mezi vývojáři

| varianta | infra navíc | bloat repa | kdy |
|---|---|---|---|
| **Nesdílet** — každý indexuje lokálně | žádná | žádný | **fáze 1–4.** Studený start ~60 s je snesitelný |
| **Git jako transport na vlastním refu** | žádná | ano, ale prunovatelný | nejlevnější sdílení bez serveru |
| **CI artefakt** — CI indexuje `main`, ostatní stahují | CI job | žádný | tým, který už CI má |
| **CAS server** | server | žádný | fáze 5, monetizace |

K druhé variantě, protože je nejzajímavější: CAS objekty se ukládají pod vlastní ref
(`refs/cairn/cache`), který **není větev a není v pracovním stromě**. Nikdy se nemerguje,
nikdy se nečekoutuje, do `git log` nezasahuje. Objekty jsou immutable a hash-pojmenované,
takže `git push`/`fetch` na ten ref je sjednocení — konflikt nemůže nastat. Ref se dá kdykoli
zahodit a force-pushnout znovu, protože je to čistě cache. Fetch je volitelný.

Tím se dá „sdílená cache" postavit **bez jediného serveru**, jen na tom, co tým už má.
Zůstává růst objektové databáze, ale je řízený a odděleny od historie kódu.

#### Co do repa naopak patří: textový souhrn topologie

Tvoje analogie s migracemi je správná — jen ji přiložit na správnou věc. Do gitu nepatří
`node_modules`, ale **lockfile**. Tady je tím lockfilem topologie (§8.8):

```
.cairn/
  topology.txt      ← COMMITOVAT: ~300 řádků, textové, čitelné, diffovatelné
  cache/            ← .gitignore
```

Vlastnosti, které z toho dělají opak indexu: je to malé, sémantické, mění se zřídka
(jen když se opravdu změní tvar systému) a **konflikt je smysluplný** — dva lidé přidali
službu — a řeší se regenerací, přesně jak jsi psal.

Hodnota navíc, kterou dnes nikdo nemá: **architektonický diff v code review.**
Když PR přidá službu, otevře port, přidá cross-service volání nebo endpoint, je to
v diffu vidět jako pět řádků, místo aby se to muselo najít v kódu.

```
 services (6)
   gateway   go    cmd/gateway/main.go:22          :8080 → public
+  billing   go    cmd/billing/main.go:14          :50052 grpc
 edges
+  gateway → billing    grpc BillingService     [proto + env BILLING_ADDR]
+  billing → postgres   env DATABASE_URL
 public surface
-  :8080  gateway  14 HTTP routes
+  :8080  gateway  17 HTTP routes
```

A v CI `cairn topology --check` selže, když commitnutý souhrn neodpovídá vygenerovanému —
stejná mechanika jako `go mod tidy -diff` nebo `cargo fmt --check`.

---

## 6. CLI rozhraní

### 6.0 Proč CLI a ne MCP (D1)

Původní verze návrhu stavěla na MCP. Je to zbytečné kolo navíc.

Agent umí spouštět příkazy a `gh`, `rg`, `jq` nebo `docker` používá plynule bez jakéhokoli
protokolu. CLI **není náhražka MCP, je to nativní tvar nástroje**; MCP je obálka, která
u lokálního read-only nástroje neřeší žádný problém, který by existoval.

Co odpadá:

- implementace protokolu a životního cyklu serveru
- **rozpočet na definice nástrojů.** Ten byl u MCP tvrdý, protože schémata jdou v každém
  requestu. U CLI je popis v skillu, který se načte jen když je relevantní — omezení
  „max 6 nástrojů" prostě zmizí
- autorizace, transport, remote varianta

Co se získává:

- **testovatelnost** — formát odpovědi je podle §6.3 samotný produkt a v terminálu je
  okamžitě vidět; u MCP potřebuješ k jeho vyhodnocení běžícího agenta
- použitelnost v CI, Makefilu a skriptech, kam MCP nedosáhne
- triviální iterace

Co se ztrácí — poctivě dvě věci:

1. **Objevitelnost.** MCP host vidí schémata nástrojů vždy; CLI musí někdo agentovi
   představit. Skill nebo dva řádky v `AGENTS.md`. Instalace skillu je ale srovnatelně
   snadná jako instalace MCP serveru, takže je to spíš přesun než ztráta.
2. **MCP sampling.** Odpadá možnost nechat LLM krok proběhnout na modelu hosta (§6.4).
   Ukazuje se ale, že je to zlepšení — viz tam.

**Není to sázka.** Produkt je query engine + formátovací vrstva; CLI i případné pozdější
MCP jsou tenké frontendy nad `cairn-daemon`. Přidat MCP později stojí jeden crate,
ne přepis.

### 6.1 Sada příkazů

```
cairn symbol <query> [--lang] [--limit]   vstupní bod přes jméno / pattern
cairn context <query>                     vstupní bod přes koncept  (§6.4)
cairn refs <handle> [--kind]              callers | impls | overrides | writes | all
cairn tests <handle>                      testy pokrývající symbol (L0 + L3)
cairn blast <handle> [--depth]            co rozbiju změnou  (L1 + L3, oddělené)
cairn expand <handle> <what> [--depth]    body | doc | neighbors | file_skeleton
cairn topology                            mapa služeb a jejich vazeb  (§8.8)
cairn status                              co je zaindexované, co zastaralé, co degradované
cairn note <handle> --summary …           zápis L2 poznámky do cache  (§3.1, D15)
```

Rozpočet už není tvrdý, ale **zdrženlivost zůstává** — agent musí umět vybrat správný
příkaz a osm zapamatovatelných je lepší než třicet. Nový podpříkaz jen tehdy, když
existující kombinace odpověď nedá.

Zvažované a zamítnuté: samostatný `implementations` (je to `refs --kind=impls`),
`definition` (to je výstup `symbol`), cokoliv na zápis — cairn je read-only, záměrně (§4.6).

### 6.1.1 CLI pro agenta, ne pro člověka

Ergonomie se liší a je potřeba se rozhodnout pro agenta:

- **žádná interaktivita.** Nikdy prompt, nikdy pager, nikdy čekání na `stdin`.
- **stabilní výstup.** Žádná detekce TTY, žádné barvy, žádné spinner artefakty
  ve `stdout`. Diagnostika jde na `stderr`.
- **exit kódy něco znamenají:** `0` nález, `1` bez nálezu, `2` chyba dotazu,
  `3` index degradovaný (§4.6) — agent tak pozná rozdíl mezi „nic tam není"
  a „nevidím tam".
- **text je výchozí, `--json` je únikový východ** pro skripty. Ne naopak: text je produkt (§6.3).
- **žádný stav mezi voláními** kromě handlů, které jsou perzistentní (§6.5).

### 6.2 Skill je produktová práce

U MCP to byly popisy nástrojů, u CLI je to skill — a je to větší prostor, ne menší.
Agent umí grep a sáhne po něm reflexivně; skill musí říct **kdy je cairn lepší**, ne co dělá:

> **Hledání použití symbolu.** Použij `cairn refs <handle>` místo grepu. Grep najde
> komentáře, stringy a stejnojmenné symboly z jiných modulů — a nenajde volání přes alias
> importu ani přes gRPC hranici mezi Pythonem a Go. `cairn refs` vrací kompaktní seznam
> s handly, které jdou rozbalit přes `cairn expand`.
>
> **Orientace v neznámé části systému.** Začni `cairn topology`, ne čtením souborů.

Výhoda skillu oproti popisům nástrojů: unese celý workflow („začni tímhle, pak expanduj,
na hledání referencí nepoužívej grep") a neplatí se, dokud není relevantní.

Signál kvality zůstává: **jestli agent sáhne po `cairn` i bez skillu** — protože ho vidí
v `AGENTS.md` nebo v historii — je nástroj zjevně lepší než grep. Když ho tam musíš tlačit,
buď není, nebo to neumíš dost rychle ukázat.

### 6.3 Formát odpovědi = produkt

Ne JSON. Kompaktní, řádkový, ASCII.

```
$ cairn symbol validate
3 matches (2 suppressed: generated)
[a4] TokenValidator.validate(token: str) -> Claims    py  auth/oauth.py:142
[a7] SessionValidator.Validate(tok string) (*Claims, error)
                                                      go  internal/auth/session.go:88
[b1] validate(schema, payload)                        py  utils/schema.py:31
```

```
$ cairn blast a4 --depth 2

static callers (4)                                        [L1, exact]
  [c1] LoginHandler.post           py  api/login.py:55
  [c2] RefreshHandler.post         py  api/refresh.py:31
  [c3] AuthInterceptor.Intercept   go  internal/grpc/auth.go:44   via proto AuthService.Verify
  [c4] worker.session_gc           py  workers/gc.py:12
transitive depth 2: 11 more in api/ (7), workers/ (3), internal/grpc/ (1)

tests covering (3)                                        [L0+L3]
  tests/test_oauth.py::test_expired_token
  tests/test_oauth.py::test_clock_skew
  internal/auth/session_test.go::TestValidateExpiry

co-changed (git, 200 commits)                             [L3, statistical]
  auth/keys.py 0.72 · api/login.py 0.61 · proto/auth.proto 0.44

unknown (1)
  plugins/loader.py:22 — dynamic dispatch via getattr(mod, name); name from config,
  not statically resolvable. Candidates: plugins/*.py (7 files). Grep suggested.

stale: none
```

Poznámky k formátu:
- **`unknown:` je povinná sekce každé odpovědi.** Prázdná = `unknown: none`. Když
  chybí, agent předpokládá úplnost — a to je ta tichá chyba, která zastaví hledání.
- **`suppressed:` taky.** Kolik jsme zahodili a jak si to vyžádat. Tiché ořezání se čte
  jako „pokryto všechno".
- Vrstva každého bloku je označená (`[L1, exact]` vs `[L3, statistical]`).
- Handle `[a4]` — 2–4 znaky, viz §6.5.

### 6.4 `cairn context` — vstupní bod přes koncept

„Dej mi kontext k OAuthu" není symbolový dotaz. Seed se získává lacino, pak se expanduje
deterministicky. Pořadí podle ceny:

0. **Deployment topologie** — když termín odpovídá jménu compose služby, adresáři jejího
   buildu nebo routě, je to nesrovnatelně lepší seed než fuzzy match na jména. „OAuth"
   v projektu se službou `auth` je vyřešený dotaz, ne heuristika. Viz §8.
1. **Lexikálně** — FTS5 nad jmény symbolů a cestami (`*Auth*`, `*Token*`, `/auth/`). ~60 % zbytku.
2. **Komentáře a docstringy** (§4.5) — často jediné místo, kde jméno featury vůbec zazní.
   Shoda vrací symbol s handlem, ne soubor, protože komentáře jsou přiřazené k symbolům.
3. **Testy** — jména testů jsou nejlepší dokumentace konceptu v projektu.
4. **Git** — FTS5 nad commit messages a PR titulky; soubory měněné společně v commitech zmiňujících termín.
5. **Dokumenty** — README, ADR, `docs/`.
6. **Nic z toho nezabralo** — vrátit slabý seed a **přiznat to**.

**Postaveno.** Docstringy jsou zadarmo: SCIP je nese pro **77,7 % Python symbolů**
a 10,5 % Go symbolů na testovacím repu (4,1 MB textu), takže se při ingestu jen
nesmí zahodit. Ověřeno, že to funguje na termínech, které v žádném identifikátoru
nejsou — `cairn context "fail-closed"` najde symboly výhradně přes prózu.

Dvě věci, které rozhodly o použitelnosti a nebyly zřejmé předem:

- **Generovaný kód musí spadnout dolů.** První verze na dotaz „quota" vrátila
  protobuf fieldy jménem `quota` a pohřbila `QuotaModule`, jehož vlastní dokumentace
  říká, že je to kvótový klient. Potlačit, ne vyloučit — termín, který žije jen
  v generovaném kódu, má pořád něco vrátit.
- **Váha podle druhu symbolu.** Typ nebo funkce *může být* „ta část systému, na kterou
  se ptám"; field ne. Bez toho vyhrávají shody jmen na atributech.

Každý seed nese **štítek, odkud pochází** (`[concept+name+doc]`). „Tohle někdo pojmenoval"
a „tohle se fuzzy trefilo do jména" si zaslouží velmi různou míru důvěry a agent to
nemá jak poznat, když mu to neřekneme.

Bod 6 je díky D1 jednodušší, než byl. Původní návrh sem chtěl LLM krok přes MCP sampling.
U CLI sampling neexistuje — a ukazuje se, že je to zlepšení: **volající agent LLM sám je.**
Cairn nemá dělat horší verzi toho, co si zavolá o řádek výš. Takže:

```
$ cairn context "oauth"
low confidence — no strong seed for this term
best guesses (5)
  [k2] domains/orders/grpc/handlers/auth.py :: AuthServiceHandler   [name]
  [k7] proto/orders_api/auth.proto :: AuthService                   [name]
  …
hint: no compose service, route prefix or test name matched "oauth".
      Try `cairn topology`, or grep for the domain term this project uses.
```

Žádný API klíč, žádné vlastní náklady, žádná závislost na podpoře v hostiteli.
Když seed sedí, cachuje se jako L2 artefakt.

Pak: expanze 1 hop přes call graph, ranking (§6.6), a vrátit **kostru 10–15 uzlů bez těl**.

> Past, na kterou je potřeba dát pozor: když `cairn context oauth` vrátí 40 souborů
> i s obsahem, spálil jsi stejné tokeny jako explorace, jen naráz. Úspora nevzniká
> z toho, že máš graf — vzniká z toho, že vracíš málo a přesně.

### 6.5 Handles

Krátký kód pro progressive disclosure. Požadavky: krátký (token cost), deterministický,
stabilní napříč sessions.

Řešení: **nejkratší unikátní prefix hashe symbolu, s perzistovanou tabulkou přiřazení.**
`blake3(scip_symbol)` → base32 → zkrátit na 2 znaky, při kolizi prodloužit na 3, 4…
Přiřazení se uloží do `handles`, takže je stabilní i po přidání symbolů. Typicky 2–4 znaky.

Handle musí jít použít i po restartu daemonu a v příští session — agent si ho může
poznamenat do svých poznámek.

### 6.6 Ranking — kde se rozhoduje o kvalitě

`cairn symbol` může vrátit 200 shod. Vracíme 15. Který výběr, tam žije celá teze
„vracet málo a přesně". Signály:

1. přesná shoda jména > prefix > substring > fuzzy
2. **není generovaný kód** (§7.3) — tvrdý downrank
3. není test (pokud se dotaz netýká testů)
4. **je dosažitelný z entrypointu** (§8.7) — mrtvý kód dolů
5. in-degree v call grafu (centralita)
6. čerstvost změny (git, poslední 90 dní)
7. blízkost k už zmíněným handlům v této session (session affinity)

Ranking je testovatelná komponenta — patří do měřicího harnessu (§10), ne do „doladíme potom".

---

## 7. Cross-language a binders

Reálné systémy jsou skoro vždy víc než jeden jazyk a hranice mezi nimi je právě to místo,
kde každý single-language nástroj oslepne. Proto cross-language není pozdní fáze, ale
základní schopnost.

**Obecný tvar problému:** existuje *sdílený kontrakt* — IDL, schéma, konvence — a několik
jazykových stran, které ho implementují nebo konzumují. Kontrakt sám je v repu jako
artefakt (`.proto`, `.graphql`, OpenAPI dokument, sdílený typový balíček). Úkolem binderu
je propojit uzel kontraktu se symboly na obou stranách.

Ten tvar je stejný pro gRPC, GraphQL, OpenAPI i sdílené TS typy mezi frontendem a BFF.
Liší se jen pravidlo, kterým se pozná, který symbol ke kterému kusu kontraktu patří.

### 7.1 Binder = malý plugin, který vyrábí hrany mezi symbol ID

Signatura konceptuálně: `fn bind(snapshot) -> Vec<Edge>`. Nic víc. Binders zapisují
do stejné `edges` tabulky s `source = binder_name`.

### 7.2 Proto binder — první instance obecného tvaru

```
proto/auth.proto
  service AuthService { rpc Verify(VerifyReq) returns (VerifyResp); }
        │                                    │
        ├── generuje ──► auth_pb2_grpc.py ──► AuthServiceServicer.Verify   (py)
        └── generuje ──► auth_grpc.pb.go  ──► AuthServiceClient.Verify     (go)
```

Binder přečte `.proto` (přes `protobuf` descriptor set nebo prosté parsování — tady je
vlastní parser výjimečně obhajitelný, gramatika je triviální) a vytvoří hrany:

- `proto:AuthService.Verify` → `py:AuthServiceServicer.Verify` (implements)
- `proto:AuthService.Verify` → `go:AuthServiceClient.Verify` (calls)
- a tím tranzitivně: `go` volající → `py` handler

**To je ten skok, který grep ani žádný single-language nástroj neudělá:** „kdo volá tenhle
Python handler" má správnou odpověď v Go kódu.

#### Vazbu implementace ↔ kontrakt nese pravidlo, ne kód binderu

Binder z kontraktu vytáhne uzly (`proto:AuthService.Verify`). **Jak se pozná
implementace, je pravidlo vrstvy B (§1.1)** — protože se to liší nejen mezi jazyky,
ale i mezi knihovnami téhož jazyka:

| stack | tvar | pravidlo |
|---|---|---|
| Python / grpclib | `inherits` | `class $X(…, $pkg.{Service}Base)` |
| Python / grpcio | `call_pattern` | `add_{Service}Servicer_to_server($impl, $srv)` |
| Go / protoc-gen-go-grpc | `call_pattern` | `Register{Service}Server($srv, $impl)` |
| TS / connect-es, nice-grpc | `collection_literal` | mapa metod → handlery |

Stojí za pozornost, že u dědičnosti **binder nepotřebuje dělat nic navíc** — `implements`
hranu dá L0 zadarmo a zbývá jen namapovat jméno generované báze zpět na kontrakt.
Konvence pojmenování je taky pravidlo, ne kód.

*Důkaz (§16): v testovacím repu jsou první tři řádky tabulky reálné —
`class ChatServiceHandler(…, orders_api.ChatServiceBase)` na Python straně,
`regions_api.RegisterAreaQueryServiceServer(server, area.NewHandler(app))` na Go straně.
Dva různé tvary v jednom repu jsou přesně ten důvod, proč to nesmí být zadrátované.*

#### Kontrakt existuje i bez vygenerovaného kódu

Hrana `kontrakt → očekávaný symbol` jde postavit i tehdy, když generovaný artefakt chybí,
protože pojmenování je dané konvencí. Cíl se pak označí `expected`, ne `resolved`.
Kód, který generované typy *importuje*, se bez nich nerozřeší nikdy — a to je ta drahá
část, odtud degradovaný režim v §4.6.

Nedělat z toho ambici generovat cokoli vlastními silami. Konvence stačí na hranu,
na tělo je potřeba build.

### 7.3 Generated-code detekce (malá fičura, obrovský efekt)

Generovaný kód bývá objemově většina repa a v odpovědích ho skoro nikdo nechce.
Bez potlačení každý dotaz utone.

Detekce je jazykově neutrální a stojí na třech signálech, v tomhle pořadí:

1. **hlavičkový marker** — `Code generated by … DO NOT EDIT.` (Go), `@generated` (běžné
   v JS/TS ekosystému i jinde), `# Generated by …`
2. **`.gitattributes linguist-generated`** — repo si to samo označuje
3. **cestové vzory z pravidel** (vrstva B) — `**/*_pb2.py`, `**/*.pb.go`, `**/generated/**`,
   `.next/**`, `dist/**`

Efekt: sbalit do jednoho řádku —
`+ 47 refs in generated code (suppressed; rerun with --include-generated)`.
Plus vlastní CAS namespace, aby šel generovaný kód ve sdílené cache přeskočit (§5.5).

*Důkaz rozsahu (§16): 103 176 ze 158 874 řádků Go v testovacím repu je generovaných — 65 %.
Ve frontend repu bude podíl jiný, ale problém tentýž.*

### 7.4 Další binders (později, stejný mechanismus)

GraphQL schéma ↔ resolvery ↔ klientské dotazy · OpenAPI ↔ server ↔ generovaný klient ·
ORM model ↔ tabulka ↔ migrace · env var ↔ čtení konfigurace · sdílené typy mezi repy ·
SQL v raw dotazech.

---

## 8. Deployment topologie — entrypointy a hranice služeb

### 8.1 Proč to není doplněk

Call graph bez kořenů je polévka. Bez entrypointů neumíš odpovědět na otázky, které
u webového projektu padnou nejdřív:

- Je tenhle kód vůbec dosažitelný?
- Ve které službě tenhle symbol běží?
- Který endpoint sem vede?
- Co se musí nasadit, když tohle změním?

`docker-compose.yml` dává grafu dvě věci, které z jazykových serverů nikdy nevypadnou:
**kořeny** (entrypointy) a **oddíly** (služby). Teprve tím se dosažitelnost a blast radius
stanou smysluplné.

A hlavně: compose je **jediný soubor v repu, který popisuje systém jako celek** — je strojově
čitelný, je nutně udržovaný (jinak nejede `docker compose up`) a de facto je to nejlepší
existující dokumentace toho, jak spolu komponenty komunikují. Ignorovat ho a hledat topologii
v kódu je práce navíc pro horší výsledek.

### 8.2 Řetěz: služba → proces → symbol → routa

Oba příklady jsou **ověřené na testovacím repu** (§16), ne ilustrace.

**Python — mapa vzniká z volume mountu, ne z Dockerfile:**

```
compose.yaml
  x-build-py: &build-py
    context: srcpy
  services.orders-grpc:
    <<: *base-service                          ← kotva: init, env_file
    build: *build-py                           ← kotva: context srcpy
    command: python3 -m domains.orders.grpc.server
    environment: [DJANGO_SETTINGS_MODULE=domains.orders.grpc.settings]
    volumes: ["./srcpy:/app/"]                 ← mapa kontejner ↔ repo
         │
         ▼  launcher resolver  (§8.4)
  srcpy/domains/orders/grpc/server.py :: __main__
         │
         ▼  route binder  (§8.6) / proto binder (§7.2)
  grpc OrderService.* → handlery
```

**Go — dva hopy přes multi-stage build:**

```
  services.scoring-grpc:
    build: *build-go                           ← kotva: context srcgo
    command: /bin/grpcserver
         │
         ▼  srcgo/Dockerfile, runtime stage
  COPY --from=builder /out/grpcserver /bin/grpcserver
         │
         ▼  srcgo/Dockerfile, builder stage
  RUN CGO_ENABLED=0 xx-go build -o /out/grpcserver \
        ./domains/orders/cmd/grpcserver/server.go
         │
         ▼  launcher resolver  (§8.4)
  srcgo/domains/orders/cmd/grpcserver/server.go :: main
```

Každá šipka je deterministická a levná. Žádný LLM, žádná heuristika — jen parsování
a tabulka známých vzorů. Ale těch šipek je víc, než se na první pohled zdá: u Go vede cesta
přes dvě `COPY --from` / `-o` mapování a přes wrapper `xx-go` (buildx cross-compile),
ne přes holé `go build`.

### 8.3 Deployment deskriptor — obecně a pak compose

Compose je **jedna instance obecnějšího pojmu: deskriptoru, který pojmenovává procesy
a jejich startovní příkazy.** Jádro z každého takového deskriptoru chce vždy totéž:

| co jádro potřebuje | proč |
|---|---|
| seznam nasaditelných jednotek | oddíly grafu |
| startovní příkaz každé z nich | vstup pro launcher resolver (§8.4) |
| most jednotka → zdrojový adresář | kde ten kód vůbec leží |
| mapa běhová cesta ↔ cesta v repu | překlad stack trace, runtime trace (§9) |
| co je dostupné zvenku | veřejná plocha systému |
| konfigurace a vazby mezi jednotkami | cross-service hrany (§8.5) |

Známé deskriptory a jejich pokrytí těch šesti položek:

| deskriptor | pokrývá | poznámka |
|---|---|---|
| **Docker Compose + Dockerfile** | vše | první implementace |
| `package.json` `scripts` | jednotky, příkazy | v JS/TS repu často jediný zdroj; monorepo přes workspaces |
| Procfile / systemd unit | jednotky, příkazy | triviální parser |
| Kubernetes / Helm | vše, ale přes šablonování | §8.9 — až bude compose ověřený |
| `Makefile` cíle | příkazy | poslední záchrana, nespolehlivé |

Jádro pracuje s tím sjednoceným tvarem; každý deskriptor je adaptér vrstvy C (§1.1).
**Repo bez Dockeru tedy není mimo rozsah** — jen dostane méně vyplněných políček
a řekne to v `unknown:`.

#### compose (`docker-compose.yml`, `compose.yaml`, override soubory, `profiles`)

| pole | co z toho je |
|---|---|
| `services.*` | seznam nasaditelných jednotek = **oddíly grafu** |
| `build.context` / `build.dockerfile` | most compose → Dockerfile → zdrojový adresář |
| `image` bez `build` | externí závislost (postgres, redis, nats) — uzel, ale ne kód |
| `depends_on` | hrany mezi službami |
| `ports` / `expose` | co je dostupné zvenku = **veřejná plocha systému** |
| `networks` | kdo na koho vůbec dosáhne |
| `environment` / `env_file` | vstup pro env binder (§8.5) |
| `command` | override — má přednost před `CMD` v Dockerfile |
| `healthcheck` | často nejpřesnější ukazatel, kde služba doopravdy poslouchá |
| `volumes` | **mapa kontejner ↔ repo pro lokální vývoj** — přebíjí `COPY` z Dockerfile (§8.7) |
| `networks.*.aliases` | další DNS jména služby — bez nich env binder ztrácí hrany (§8.5) |

**Korekce po ověření na testovacím repu:** dřívější verze tohoto návrhu odkládala
`x-` extensions jako okrajové. To je špatně a v praxi to znamená neparsovat nic:

- **`x-` bloky nesou kotvy.** V testovacím repu žijí v `x-base-service`, `x-build-go`,
  `x-build-py` a `x-healthcheck-*` úplně všechny sdílené definice; služby si je berou
  přes merge klíč `<<: *base-service`. Bez rozřešení kotev a aliasů nedostaneš ani
  build context, ani `env_file`.
- **Interpolace umí být vnořená.** `${IMAGE_PREFIX:-${COMPOSE_PROJECT_NAME:-platform}}` je
  reálný řádek. Potřeba implementovat compose interpolaci včetně `:-` defaultů a `.env`.
- **`name:` na úrovni souboru** určuje jméno projektu a tím i DNS jména.

YAML parser tedy musí zachovat kotvy a merge klíče, ne je jen načíst do mapy.
Kam nechodit dál: šablonování build args, `.dockerignore` sémantika, `profiles` kombinatorika.

**Dockerfile:**

| direktiva | co z toho je |
|---|---|
| `WORKDIR` + `COPY`/`ADD` | **mapa kontejnerová cesta ↔ cesta v repu** — nutná i pro L3 runtime trace (§9) |
| `ENTRYPOINT` + `CMD` | skutečný startovní příkaz (rozlišit shell vs exec form) |
| `FROM … AS build` + `COPY --from=build` | která stage produkuje runtime artefakt |
| `RUN go build -o /app/server ./cmd/server` | most binárka ↔ balíček |

Merge sémantiku compose (override soubory, `extends`) respektovat. Do `x-` extensions,
šablonování build args a `.dockerignore` sémantiky **nechodit** — tam začíná králičí nora
a hodnota strmě klesá.

### 8.4 Launcher resolver — příkaz → symbol

Vstupem je **řetězec startovního příkazu**, ať přišel odkud chce (§8.3). Výstupem je
symbol, nebo poctivé `unknown`. Není to interpret shellu, ale **tvar `command_string`
z §1.1** — pravidla v datech, resolver v jádře.

```toml
[[launcher]]
id = "python.module"; lang = "python"
match = "python3? -m (?<mod>[\\w.]+)"
emit  = { module = "{mod}", symbol = "__main__" }
```

Ukázky pravidel napříč ekosystémy — účel je vidět, že se liší jen data:

| ekosystém | příkaz | kořen |
|---|---|---|
| Python | `python -m pkg.server` | `pkg/server/__main__.py` |
| Python | `gunicorn pkg.wsgi:application` | symbol `application` |
| Python | `celery -A proj worker` | **každý `@shared_task` je vlastní kořen** |
| Go | `/bin/srv` | zpětně přes `build -o` → `cmd/srv/main.go::main` |
| Node | `node dist/server.js` | přes source map / build config zpět do `src/` |
| Node | `next start` | **konvence: `app/**/route.ts`, `pages/api/**`** (§8.6) |
| Node | `npm run start` | rozbalit přes `package.json` `scripts` a řešit znovu |
| JVM | `java -jar app.jar` | manifest `Main-Class` |

Dvě věci, které pravidlo neunese a patří do vrstvy C:

- **rekurze přes nepřímost.** `npm run start` → `scripts.start` → další příkaz.
  Resolver musí umět zavolat sám sebe s limitem hloubky.
- **build artefakt ≠ zdroj.** U Go to řeší `-o` mapování z Dockerfile, u Node bundler
  (`dist/`, `.next/`) a tam je most buď source map, nebo konfigurace bundleru.
  Tohle je u JS/TS podstatně horší než u Go a je to hlavní riziko §17.

Obalové skripty (`entrypoint.sh`, `docker-entrypoint.sh`) jsou v praxi časté: přečíst
a hledat závěrečný `exec …`.

Když se cokoli z toho nepovede rozřešit, jde to do **`unknown:`** — ne tichý fail. To je
přímý důsledek D8 a tady na tom záleží víc než kdekoli jinde, protože **chybějící kořen
tiše prohlásí živý kód za mrtvý** (§8.7).

### 8.5 Env binder

`environment:` a `env_file:` dají množinu proměnných. V kódu se hledají čtení:
`os.environ[…]`, `os.getenv`, `os.Getenv`, `settings.X` u Djanga, `envconfig`/`viper` u Go.

Vznikají dva druhy hran a ten druhý je cennější:

- `auth.env.DATABASE_URL` → `db/session.py:14` — **kdo to čte**
- `gateway.env.AUTH_GRPC_ADDR = auth:50051` → **služba `auth`** — kdo je cíl

Druhá hrana je doložená runtime vazba mezi službami: host v URL odpovídá jménu jiné compose
služby. Dohromady s proto binderem (§7.2) vzniká uzavřený obrázek — `gateway` (Go) volá
`AuthService.Verify`, implementaci má `auth` (Python), a compose potvrzuje, že to jsou
dva různé procesy komunikující přes `auth:50051`. Ani jeden z těch tří zdrojů to neví sám.

**Korekce: párovat na jméno služby nestačí.** V testovacím repu má `orders-grpc`
v `networks.default.aliases` navíc `orders-api_grpc-python`
a `orders_grpc-python`. URL v env proměnné jiné služby míří na alias, ne na
jméno služby — naivní párování by tu hranu neviděla vůbec. Binder proto staví
**tabulku všech DNS jmen** (jméno služby + `container_name` + všechny aliasy napříč sítěmi)
a páruje proti ní.

`DJANGO_SETTINGS_MODULE` stojí za zvláštní zmínku: je to env proměnná, ale zároveň
**ukazatel na modul** (`domains.orders.grpc.settings`). Dává per-službu konfiguraci
Djanga a je to zároveň nejlevnější způsob, jak správně nakonfigurovat `django-stubs`
pro každou službu zvlášť (§4.4).

### 8.6 Route binder — request-level entrypointy

Compose dává kořeny na úrovni procesů. Webový projekt potřebuje kořeny na úrovni requestů.

Obava „na tohle bychom museli znát všechny frameworky" je pochopitelná, ale při pohledu
na reálné frameworky se rozpadá: **routu deklaruje jeden ze čtyř tvarů z §1.1** a jádro
z nich skládá cestu vždy stejně.

| tvar | frameworky | pravidlo |
|---|---|---|
| `decorator` | FastAPI, Flask, NestJS, Spring | `@$router.{method}($path)` |
| `call_pattern` | Express, chi, gin, stdlib `ServeMux` | `$app.{method}($path, $handler)` |
| `collection_literal` | Django `urlpatterns`, Vue/React Router | seznam `($path, $handler)` |
| `path_convention` | **Next.js, Remix, SvelteKit, Nuxt** | `app/**/route.ts` → cesta ze struktury adresářů |

Skládání cesty je pak textové: prefix routeru/mountu + cesta z pravidla + vnoření.

**`path_convention` je ten tvar, který přijde s JS/TS** a v Pythonu ani Go nemá obdobu —
routa není nikde deklarovaná, je zakódovaná v *cestě k souboru*. Proto je v seznamu
šesti tvarů (§1.1) od začátku, ne až jako pozdější dodatek.

**Dvě informace navíc, které stojí za to vytáhnout, když je framework nabízí:**

- **stabilní identita routy.** FastAPI `operation_id`, NestJS jméno metody, Next.js cesta
  souboru. Je stabilnější než URL a je to lepší primární klíč než cesta.
- **autentizace.** Když se middleware/guard/dependency připíná deklarativně
  (`dependencies=[Depends(auth)]`, `@UseGuards(...)`, `middleware.ts`), jde staticky
  odvodit, které routy jsou veřejné. Pro auditní doménu je to samostatně prodejný výstup.

*Důkaz (§16): v testovacím repu jsou reálně tři z těch čtyř tvarů — FastAPI dekorátory
(122 endpointů, včetně staticky viditelné autentizace), Django `urlpatterns` a jediný
stdlib `ServeMux` v Go. Žádné chi, gin ani echo. Detaily v
[coverage-analysis.md](coverage-analysis.md).*

**Levný univerzální únik, kdyby vzory nestačily.** Většina web frameworků umí vypsat
svou routovací tabulku: FastAPI `app.openapi()`, Django `get_resolver().url_patterns`,
Flask `app.url_map`. Cena je, že se musí naimportovat aplikace — je to tedy runtime
probe, ne statická analýza, a patří do L3 (§9), ne do L0. Má to přesně tutéž dualitu jako
zbytek návrhu: **statika je úplná ale přibližná, runtime je přesný ale jen po dosažitelnou
část.** Když se ty dva zdroje rozejdou, je to nález, ne chyba.

Doporučení: statické vzory teď, runtime dump jako opt-in booster ve stejné fázi jako
coverage (§9). Rozhodně ne obráceně — runtime probe by z read-only nástroje udělal něco,
co spouští cizí kód.

Výstup je hrana `route:POST /onboarding/signup` → handler symbol. Tím jde odpovědět na
„který endpoint vede k tomuhle kódu", což je u auditu a code review nejčastější otázka vůbec.

### 8.7 Co z toho plyne pro ostatní vrstvy

**Service attribution.** Reachability z kořenů dá každému symbolu štítek služeb.
Levné a mění to tvar odpovědí:

```
$ cairn blast a4 --depth 2
…
services affected (2)                                     [L0-D + L1]
  auth      py   direct
  gateway   go   via proto AuthService.Verify
externally reachable via
  POST /oauth/token · POST /oauth/refresh                  [route]
```

**Dead code.** Symbol nedosažitelný z žádného kořene a nepokrytý testem je kandidát.
Vracet jako signál s confidence, nikdy jako fakt — reflexe, dynamický import a nerozřešený
entrypoint to umí obejít.

**Lepší seed pro `cairn context`** — zařazeno jako zdroj 0 v §6.4.

**Předpoklad pro L3 runtime trace.** Stack trace z běžícího kontejneru říká
`/app/domains/orders/grpc/server.py`, repo říká `srcpy/domains/orders/grpc/server.py`.
Bez mapy je runtime trace nepoužitelný. Proto tahle sekce předchází §9.

**Korekce, odkud ta mapa je.** Dřívější verze ji brala z `WORKDIR` + `COPY` v Dockerfile.
Na testovacím repu to nestačí: `orders-grpc` má `volumes: ["./srcpy:/app/"]`, což
`COPY . .` z buildu **přebije** — v běžícím kontejneru je pod `/app` bind mount, ne
zkopírovaný obsah. Pořadí priority je tedy:

1. `volumes` bind mount ve compose *(autoritativní pro lokální vývoj — náš případ)*
2. `WORKDIR` + `COPY`/`ADD` v Dockerfile *(platí pro produkční image bez mountů)*
3. `build.context` jako poslední záchrana

**Služba není image.** V testovacím repu běží 15 služeb ze **dvou** build image
(`x-build-py`, `x-build-go`) — `orders-grpc`, `orders-api`, `catalog-pipeline`
a další sdílejí týž strom `srcpy`. Ze souborového systému proto **nejde zjistit, do které
služby modul patří**; řekne to jedině dosažitelnost z entrypointu. To není okrajový případ,
to je nejsilnější argument pro existenci celé téhle sekce.

### 8.8 `cairn topology` — mapa systému na ~400 tokenů

Ideální první příkaz, který agent v neznámém repu spustí. Skill to říká výslovně:
*„Než začneš číst soubory, spusť `cairn topology`."* Service attribution se pak
propisuje jako anotace do odpovědí ostatních příkazů.

Původní návrh z toho dělal MCP resource, aby se ušetřil rozpočet nástrojů. S D1
ten důvod zmizel — je to prostě podpříkaz, a navíc si ho může spustit i člověk.

Reálný tvar pro testovací repo (§16), zkráceno:

```
$ cairn topology

services (16, from compose.yaml + compose.local.yaml)
  orders-api          py   domains/orders/api/app.py            :8000  → public
  orders-grpc         py   domains/orders/grpc/server.py        :50051 grpc
  orders-proxy        go   cmd/resttransform/server.go             :8081
  orders-admin   py   manage.py runserver (django admin)      :8002
  orders-tools          py   domains/orders/mcp/…                 :8003
  scoring-grpc           go   cmd/grpcserver/server.go                :50052 grpc
  regions-grpc       go   cmd/server/server.go                    :50053 grpc
  catalog-pipeline  py   domains/catalog/…                  —
  media-grpc             go   cmd/server.go                           :50054 grpc
  postgres               ext  postgres:16                             :5432
  … 6 more

edges
  orders-proxy → orders-grpc   grpc orders_fe.*   [proto + net alias]
  orders-api   → orders-grpc   grpc orders_api.*  [proto]
  orders-grpc  → postgres         env DATABASE_URL
  …

public surface
  :8000  orders-api  122 HTTP routes (20 routers, 12 unauthenticated)
  :50051 orders-grpc 24 grpc services / 71 proto services total

unknown (0)
stale: none
```

Řádek `12 unauthenticated` není kosmetika — plyne z toho, že
`app.include_router(x, dependencies=[Depends(get_authenticator())])` nese informaci
o autentizaci staticky (§8.6). Pro auditní doménu je to samo o sobě prodejný výstup.

### 8.9 Co teď ne

Kubernetes / Helm (stejný mechanismus, jiný parser — až bude compose ověřený),
Terraform, šablonování build args, obecné Go routery, service mesh konfigurace.
Compose + Dockerfile je 90 % hodnoty za 10 % práce.

---

## 9. L3 — execution knowledge

Nejlepší poměr hodnota/cena v celém dokumentu. Bez LLM.

**Z git historie** (gix, žádný shell-out):
- **co-change matice** z `git log --name-only` nad posledními N commity; skóre = PMI / lift,
  ne prostý počet (jinak vyhraje `README.md` a `go.mod`)
- **test impact heuristika** — testy měněné společně se zdrojem
- **recency & ownership** pro ranking

**Z běhu** (fáze 2+):
- Python: `coverage.py` s **contexts** (`--context=test`) → mapa test → řádky. To je skutečný
  test impact, ne heuristika. Jeden pytest plugin.
- Go: `go test -coverprofile` per balíček, případně per test.
- Dynamické importy / skutečný call graph: `sys.settrace`. Precedent MonkeyType (Instagram).

L3 artefakty jdou do stejného CAS + `edges` s `confidence < 1.0` a `source = git|coverage|trace`.
V odpovědi vždy vizuálně oddělené od L1.

---

## 10. Měření — komponenta, ne příloha

Bez tohohle je celý projekt hypotéza. `cairn-eval` je crate, ne skript.

20 reálných úkolů z testovacího repa, spuštěné proti baseline agentovi a proti cairn agentovi:

| Metrika | Cíl | Poznámka |
|---|---|---|
| Tokeny na úkol | −50 % | hlavní teze |
| Kol do první editace | −50 % | zkrácení explorační smyčky |
| Wall clock | ≤ baseline | nesmí být pomalejší |
| Recall na L0/L1 dotazech | **100 %** | pod 100 % je produkt nebezpečný |
| Latence dotazu p95 | ≤ 20 ms | na 500k řádcích |
| Studený start | ≤ 60 s | bez sdílené cache; s ní ≤ 10 s |

**Baseline musí mít zapnutý prompt caching.** Bez toho porovnáváš proti slaměnému panákovi.

Recall se měří proti zlatému standardu vygenerovanému nezávisle (hrubá síla: LSP crawl
přes všechny symboly, jednou, offline).

---

## 11. Rozvržení kódu

```
cairn/
  crates/
    cairn-cli       binárka: `cairn symbol|refs|blast|topology|status|daemon|index|eval`
    cairn-proto     sdílené typy, msgpack, socket protokol frontend↔daemon
    cairn-skill     skill pro agenta (§6.2) — text, ne kód, ale verzuje se s CLI
    cairn-fmt       renderer kompaktních odpovědí  ← produktová plocha, testuje se snapshoty
    cairn-daemon    supervizor, socket server, scheduler, deadliny
    cairn-store     CAS, SQLite projekce, snapshot, cache klíčování
    cairn-index     SCIP ingest, LSP klient pool, extrakce faktů
    cairn-rules     engine šesti tvarů (§1.1) + načítání rules/*.toml a .cairn/rules.toml
    cairn-lang      language providers (vrstva C): python · go · typescript
    cairn-binders   adaptéry, které pravidlo neunese: proto, deployment deskriptory
    cairn-graph     L1 derivace: reference, call graph, blast radius, reachability, ranking
    cairn-git       gix, co-change, test impact, snapshoty z tree
    cairn-eval      měřicí harness
  docs/
    architecture.md
    adr/
```

Rozdělení odpovídá vrstvám z §1.1. `cairn-rules` a `cairn-lang` jsou hranice, přes kterou
se přidávají ekosystémy; `cairn-graph`, `cairn-store` a `cairn-fmt` o žádném jazyce nevědí
a **při přidání jazyka se do nich nesahá** — to je testovatelná podmínka D16, ne zbožné přání.

Balíčky pravidel (`rules/*.toml`) se do binárky vestavějí, ale jdou přebít souborem
`.cairn/rules.toml` v repu.

**Runtime:** tokio. **Klíčové crates:** `gix`, `rusqlite` (bundled, WAL), `notify`, `blake3`,
`rmp-serde`, `zstd` (s trénovaným slovníkem, §5.5), `tower-lsp` nebo vlastní tenký LSP klient,
`scip` (protobuf schéma), `tree-sitter` + gramatiky **jen na komentáře** (§4.5),
YAML parser zachovávající merge sémantiku compose.

**Watcher:** `notify` s 50ms debounce, ignore `.git`, `node_modules`, `__pycache__`,
`target`, `vendor`, respektovat `.gitignore`.

---

## 12. Latence a rozvrh práce

Princip D7: **dotaz má deadline (výchozí 200 ms) a nikdy neblokuje na indexaci.**

```
query přijde
  ├─ vše čerstvé v SQLite?          → odpověz  (~2–20 ms)
  ├─ dotčený soubor je dirty?       → LSP re-resolve jen toho souboru (10–100 ms)
  │                                    stihl se do deadline? → odpověz
  │                                    nestihl? → odpověz ze staré báze + `stale: auth/oauth.py`
  └─ vůbec nezaindexováno?          → odpověz co víš + `stale: cold index in progress (37%)`
```

Fronta na pozadí, prioritně: dirty soubory > jejich přímé závislé > zbytek projektu > L3 > L2.

---

## 13. Roadmapa

| Fáze | Obsah | Výstup |
|---|---|---|
| **0** — týden 1 | Spike na testovacím repu (§16), **celý v kontejneru** (D13): `make pbgen` → `scip-python` + `scip-go` → podíl nevyřešených symbolů, čas, **syrová velikost indexu** (vstup pro §5.5). Měřit **dvakrát: s vygenerovanými stuby a bez nich** — rozdíl je cena §4.6. | Go/no-go pro D3, kalibrace D10 |
| **1** — týdny 2–6 | Daemon + store + CAS + snapshot + dirty overlay. Extrakce komentářů (§4.5) — je zadarmo a schéma FTS ji musí mít od začátku. `cairn symbol|refs|expand`. CLI frontend + skill. | Použitelný produkt |
| **2a** — týden 7 | Compose + Dockerfile binder, launcher resolver, `cairn topology`, service attribution, commitnutelný `.cairn/topology.txt` + `topology --check`. | Mapa systému; nejlevnější kus v celém plánu |
| **2b** — týdny 8–9 | Proto binder + route binder + generated-code detekce → cross-language a cross-service hrany. `cairn blast`. | **Diferenciátor, který nikdo nemá** |
| **3** — týdny 10–12 | L3 z gitu (co-change, test impact). `cairn tests`. `cairn-eval` a první měření proti baseline. | **Tady se rozhodne, jestli teze platí** |
| **4** | `cairn context`, ranking, progressive disclosure tuning. Skill. | Produktová vrstva |
| **4b** | **Druhý ekosystém: JS/TS** (§17). Provider + balíček pravidel, nula změn v jádře. | Ověření D16 v praxi |
| **5a** | Sdílení přes `refs/cairn/cache` — nepotřebuje žádnou infrastrukturu (§5.6). | Sdílená cache za pár dní |
| **5b** | CAS server (sync vrstva nad hotovým CAS). | Monetizace |
| **6** | L2 sémantika. Coverage contexts. | Až nad hotovou strukturou |

**Nikdy** (dokud to prokazatelně nebolí): vlastní parsery, vlastní storage engine,
vlastní event sourcing, mmap + B+ tree.

Git už je append-only event log a `git log` je replay. Vlastní event store je měsíce práce
duplikující něco, co je zadarmo.

---

## 14. Rizika

| Riziko | Dopad | Mitigace |
|---|---|---|
| ~~`scip-python` neustojí Django~~ | — | **Vyřešeno měřením: 0,11 % nerozřešených.** Fáze 0 uzavřena |
| Studený start ~3 min místo 60 s | Horší první dojem | Naměřeno. Sdílená cache (§5.6) se tím posouvá z „monetizace" na „to, co dělá první běh snesitelným" |
| scip-python nemá per-file inkrementalitu | LSP hot path je povinný, ne volitelný | Potvrzeno měřením — §4.2 je od fáze 1, ne později |
| Package field v SCIP symbolu není hranice projektu | Třetí strany se tiše mísí do odpovědí | scip-python mylně připisuje ~37k referencí projektu. Vlastnictví odvozovat z indexované množiny souborů |
| Agent nástroj nepoužije, sáhne po grepu | Produkt neexistuje | Skill jako produktová práce (§6.2). Měřit podíl `cairn` volání vůči grepu, i bez skillu |
| `deps_api_hash` nekonverguje na reálném kódu | Cache je k ničemu | Změřit hit rate ve fázi 1; fallback na hrubší klíč |
| Serena / konkurence dorazí dřív | Delta zmizí | Delta je perzistence + cross-language binders + L3, ne „máme graf". Zaměřit se na ně |
| Recall < 100 % na L0 | **Produkt je nebezpečný** | Zlatý standard + regrese v CI. Radši vrátit `unknown` než hádat |
| Nerozřešený entrypoint → živý kód označen za mrtvý | Tichá a velmi škodlivá chyba | Nerozřešený launcher je vždy `unknown`; dead-code signál se **nikdy** nevrací, pokud v projektu zbyl byť jeden nerozřešený kořen |
| Index nedosáhne velikostního cíle | Sdílená cache ztrácí smysl | Změřit ve fázi 0 na surovém SCIP indexu. Techniky §5.5 jsou přírůstkové, dá se přidávat |
| Binders bobtnají (K8s, Terraform, každý Go router) | Scope creep zadními vrátky | §8.9 je závazný seznam. Nový binder jen s doloženým výskytem v testovacím repu |
| Komentáře zaplaví fulltext šumem | `cairn context` zhorší, ne zlepší | Vážené FTS sloupce, detekce zakomentovaného kódu, měřit precision seedu odděleně (§10) |
| Agent vezme zastaralý komentář jako fakt | Tichá chyba typu, kterému se celý návrh vyhýbá | Komentář v odpovědi je vždy `[comment, unverified]`; nikdy nevstupuje do L0/L1 tvrzení |
| `refs/cairn/cache` nafoukne objektovou DB | Pomalý clone/fetch | Ref je prunovatelný a force-pushovatelný; fetch volitelný. Když bolí, přejít na CI artefakt |
| Indexace nad nevygenerovaným codegenem | Tichý propad recallu, který vypadá jako selhání indexeru | §4.6: detekovat, degradovat, přiznat v každé odpovědi. Nikdy neindexovat naslepo |
| `inotify` přes bind mount na Docker Desktopu (D13) | Dirty overlay přestane fungovat, agent dostává zastaralá data | Fallback na polling; frontend umí poslat explicitní invalidaci. Na Linuxu nativně OK |
| Kontejnerové cesty prosáknou do odpovědí | Agent nedokáže otevřít soubor, o kterém mu cairn říká | Absolutní cesty jsou zakázané už kvůli §5.1; snapshot testy `cairn-fmt` to hlídají |
| Návrh se utáhne na testovací repo | Přenos na JS/TS = přepis | D16 a §17. Pravidla v datech; při přidání jazyka se nesmí sáhnout do vrstvy A |
| Jazyk pravidel bobtná do DSL | Vlastní parser v jiném převleku | Uzavřená šestice tvarů (§1.1). Nový tvar až na dva nezávislé reálné případy |
| JS/TS bundling rozbije řetěz příkaz → symbol | Entrypointy ve frontend repu nedohledatelné | §17.4: přeskočit bundler přes konvenci zdrojů; kde nejde, poctivé `unknown` |
| Scope creep | Rok bez produktu | V dokumentu je jeden produkt. Držet fázi 0–3 |

---

## 15. Otevřené k rozhodnutí

1. **Jméno a licence** — `cairn` je pracovní. Open core (server MIT, sdílená cache placená)?
2. **Ne-kódová znalost** — PR diskuze, issues, ADR. „Proč je tohle takhle" tam bývá častěji
   než v AST. Zapadá jako další binder + L2, ale je to samostatný produkt. Zatím mimo scope.
3. **Multi-repo** — SCIP to umí ze své podstaty. Až bude jeden repo hotový.
4. **Windows** — named pipes místo unix socketu; jinak beze změny. Kdy?
5. **`orders-tools`** — testovací repo už jeden MCP server provozuje. Stojí za to zjistit,
   co dělá, než postavíme druhý: buď je to nesouvisející doména, nebo je to signál o tom,
   jak tým MCP používá.

---

## 16. Kalibrace na testovacím repu

an internal repository, změřeno 30. 7. 2026.

**Tahle sekce je důkaz, ne specifikace.** Čísla dole slouží ke kalibraci fáze 0 a k doložení,
že mechanismy z §7 a §8 nejsou hypotézy. Nic z toho nesmí být zadrátované v jádře —
viz D16 a §1.1. Zkouška opačným směrem, na JS/TS, je v §17.

| | |
|---|---|
| Velikost pracovního stromu | 34 MB |
| **Naměřeno ve fázi 0** | [spike-0-results.md](spike-0-results.md) |
| Python | 218 193 řádků / 1 184 souborů · Django 5.2.6, pytest-django |
| Go | 158 874 řádků / 516 souborů |
| Proto | 9 768 řádků / 139 souborů |
| **Generovaný Go** | **103 176 řádků = 65 % Go kódu** (220 × `.pb.go`) |
| Generovaný Python | 13 souborů / **48 952 řádků**, 51 `*ServiceBase` (betterproto2 sype do `__init__.py`, ne `*_pb2.py` — snadno se přehlédne) |
| Compose služby | 16 (15 vlastních + postgres) ze **2 build image** |
| Compose soubory | `compose.yaml`, `compose.local.yaml`, `compose.test.yaml` |
| Dockerfily | 7 (`srcpy`, `srcgo`, jejich interpreter/compiler báze, `pbgen`, sentinel, postgres) |
| Go binárek z jednoho Dockerfile | 8, přes `xx-go build -o` + `COPY --from=builder` |

Co z toho přímo vyplynulo do návrhu: §2.1 (D13), §4.6 (D14), korekce v §7.2, §7.3, §8.2,
§8.3, §8.5 a §8.7.

Co je potřeba doměřit ve fázi 0:

1. podíl symbolů, které `scip-python` nerozřeší — **zvlášť s `make pbgen` a bez něj**
2. syrová velikost SCIP indexu pro obě části → kalibrace cíle 50 MB (§5.5)
3. jestli launcher resolver (§8.4) trefí všech 15 služeb, nebo kolik skončí v `unknown`
4. kolik `.proto` služeb má obě strany (Go klient i Python servicer) — velikost §7.2 delty
5. jestli `orders-admin` / `catalog-admin` jsou Django admin, a tedy
   jestli je potřeba route binder i pro admin URL

---

## 17. Zkouška abstrakce: co stojí přidat JS/TS

Podmínka z D16 zní: **přidání ekosystému = jeden language provider (vrstva C) + jeden
balíček pravidel (vrstva B), nula změn ve vrstvě A.** Tady je průchod pro JS/TS,
protože to je reálně další cíl (frontend repo). Slouží zároveň jako kontrola, jestli
návrh drží — kdyby vyšlo, že je potřeba sahat do jádra, je návrh špatně **teď**,
ne za rok.

### 17.1 Vrstva A — beze změny

Snapshot, CAS, `deps_api_hash`, graf, ranking, handles, formátování, git L3, daemon, CLI.
Nic z toho neví, jaký jazyk indexuje. ✅

### 17.2 Vrstva C — jeden provider

| slot | pro JS/TS |
|---|---|
| dávkový indexer | `scip-typescript` (Sourcegraph, pokrývá JS i TS) |
| LSP pro dirty soubory | `typescript-language-server` / `tsserver` |
| gramatika komentářů | `tree-sitter-typescript`, `tree-sitter-tsx` |
| resolver konfigurace | **`tsconfig.json` `paths` / `baseUrl`** — nutné, jinak se `@/lib/x` nerozřeší |

Poslední řádek je jediná skutečně nová práce ve vrstvě C. Je to obdoba toho, co u Pythonu
dělá `DJANGO_SETTINGS_MODULE` a u Go `go.mod` — každý ekosystém má svoje místo, kde je
uložené mapování modulů, a jádro ho nesmí předpokládat.

### 17.3 Vrstva B — balíček pravidel

```
rules/typescript.toml
  launcher:  node dist/*.js · next start · vite preview · nest start · npm run <script>
  routes:    express call_pattern · nest decorator · next/remix path_convention
  codegen:   prisma client · graphql-codegen · next build types · openapi klienti
  generated: **/dist/** · **/.next/** · **/*.generated.ts · @generated marker
  contract:  tRPC router · GraphQL schéma · sdílené TS typy
```

Nic z toho nevyžaduje nový tvar kromě `path_convention` — a ten je v šestici od začátku
právě proto, že se vědělo, že přijde s JS/TS.

### 17.4 Kde to bude nepříjemné (a je lepší to vědět teď)

| problém | proč je horší než u Python/Go | co s tím |
|---|---|---|
| **Bundling** | Nasazuje se `dist/` nebo `.next/`, ne zdroj. Řetěz „příkaz → symbol" se láme na artefaktu, který v repu ani není. | Přeskočit bundler: pravidlo mapuje `next start` rovnou na konvenci zdrojů, ne na build výstup. Kde to nejde, `unknown:`. |
| **Monorepo** | pnpm/yarn workspaces, turbo. „Služba" může být balíček, ne kontejner. | Deskriptor `package.json` workspaces (§8.3) jako zdroj jednotek vedle compose. |
| **Roztříštěnost routerů** | Express, Fastify, Nest, Next, Remix, SvelteKit vedle sebe. | Právě proto pravidla v datech. Přidat vzor = řádek v TOML. |
| **`node_modules`** | Obrovské, `scip-typescript` je umí zatáhnout do indexu. | Tvrdé vyloučení + vlastní CAS namespace jako u generovaného kódu (§7.3). |
| **Sdílené typy mezi repy** | Frontend importuje typy generované z backendového kontraktu. | To je multi-repo (§15 bod 3), zatím mimo. Zůstane `unknown` na hranici. |

### 17.5 Verdikt

Podmínka D16 vychází: **nula změn ve vrstvě A, jeden provider, jeden balíček pravidel.**
Jediná strukturální novinka je tvar `path_convention` a ten je v návrhu už teď.

Bundling je reálné riziko a je specifické pro JS/TS. Není to ale díra v architektuře —
je to místo, kde bude `unknown:` sekce delší, což je přesně to, k čemu je.

---

## 18. Řízení kontextu — nástroj neví, kolik toho chce agent

Dosud návrh mlčky předpokládal, že cairn sám ví, co vrátit. To je špatně: **kolik kontextu
a jakého tvaru je potřeba, ví jen volající**, a mění se to dotaz od dotazu. Audit chce
šířku, oprava konkrétní chyby hloubku, a agent s 20 tisíci volnými tokeny chce něco jiného
než agent s dvěma sty.

Rozhraní tedy potřebuje **ovládání**, ne jen dotazy. Ne dvacet přepínačů, ale čtyři
ortogonální osy.

### 18.1 Čtyři osy

| osa | co říká | příklad |
|---|---|---|
| **detail** | kolik z každého uzlu | `--detail skeleton\|signature\|doc\|body` |
| **šířka × hloubka** | jak daleko se jde | `--depth 2 --fanout 8` |
| **aspekt** | které hrany se prochází | `--aspect callers,impls,tests,routes,services` |
| **rozpočet** | tvrdý strop | `--budget 2000` (tokenů) |
| **pohled** | jak se to vykreslí | `--view list\|tree\|path\|skeleton` |

První tři jsou zřejmé. Čtvrtá je ta zajímavá. Pátá je ta, která rozhoduje o tom,
jestli se kód nerozpadne.

#### Detail platí na průchod, ne jen na jeden symbol

Osa detailu není o zobrazení jednoho symbolu — je o tom, **kolik kódu se vytiskne z každého
uzlu, kterým procházíme**. To je tvar, který potřebuje audit: „projdi volající téhle funkce
a ukaž mi jejich těla", protože hledání edge cases, porušených konvencí, bezpečnostních děr
i výkonnostních problémů se nedá dělat ze seznamu jmen.

Ve výchozím stavu je to vypnuté, protože je to nejdražší věc, kterou nástroj umí vydat —
a právě proto je to zároveň místo, kde `--budget` (§18.2) rozhoduje nejvíc. Dvě pojistky:
tělo jednoho symbolu má vlastní strop v řádcích, aby jedna dlouhá funkce nespolkla celý
rozpočet a průchod neskončil po prvním uzlu; a když indexer nezná rozsah těla, vypíše se
jen definiční řádek **a řekne se to**, místo aby se hádalo, kde tělo končí.

#### Pohled je oddělený od výběru

Jakmile přibude call graph, přibudou i způsoby, jak tutéž znalost zobrazit — plochý
seznam, strom, cesta A→B, kostra souboru, graf hran. Kombinace **osy × pohledy** roste
násobně, a kdyby si každý dotaz nesl vlastní formátování, skončí to jako N×M kopií.

Proto tvrdé rozdělení: **cairn-store vybírá** (co, jak hluboko, po kterých hranách)
a vrací neutrální `Walk`; **cairn-fmt vykresluje** a o dotazech nic neví. Nový pohled
je pak jeden `match` arm, ne nový dotaz, a nový dotaz umí okamžitě všechny pohledy.

To je zároveň důvod, proč `--view` není jen kosmetika: `tree` a `list` mají velmi
odlišnou cenu v tokenech za tutéž informaci, takže je to ve skutečnosti další
rozpočtová páka.

### 18.2 Rozpočet jako prvotřídní vstup

Dnešní agent musí velikost odpovědi hádat přes `--limit` a pak litovat. Obrátit to:
**volající řekne strop, nástroj je odpovědný za to, že ho vyplní tím nejcennějším** —
a povinně vypíše, co kvůli tomu vypustil.

```
$ cairn blast a4 --budget 1500

static callers (4 of 11 shown, ranked)            [L1, exact]
  [c1] LoginHandler.post           api/login.py:55
  …
suppressed: 7 callers below the budget cut
            (expand: cairn blast a4 --budget 4000, or --aspect callers --detail skeleton)
```

Proč to sedí k tezi produktu: celý pitch je „levnější kontext". Nechat agenta hádat limit
znamená, že buď utratí zbytečně, nebo si vyžádá druhé kolo — a druhé kolo je přesně ta
explorační smyčka, kterou máme rušit. **Rozpočet je jediné místo, kde nástroj může
optimalizovat lépe než volající**, protože jako jediný ví, co všechno má k dispozici
a jak je to seřazené.

Odhad tokenů: `cairn-fmt` počítá vlastní výstup, takže strop je vůči skutečné odpovědi,
ne vůči odhadu. Přibližný počet (znaky / 3,7) stačí — nemá cenu tahat tokenizér.

### 18.3 Zápis: nejen shrnutí

`cairn note` (§3.1) zapisuje L2 shrnutí. Volající ale ví i věci, které se vyplatí uložit
a které nejsou shrnutí:

| co | proč to agent ví a cairn ne |
|---|---|
| **potvrzení nebo vyvrácení slabé hrany** (§18.4) | agent kód přečetl a viděl, jestli to volání skutečně existuje |
| **role symbolu** („tohle je jediný vstupní bod do plateb") | plyne z úkolu, ne z AST |
| **negativní znalost** („tady jsem hledal, není to tu") | ušetří příští session celé kolo |
| **doménový alias** („čemu tady říkají *order*, jinde je *listing*") | most mezi žargonem a jmény |

Všechno jde do L2, tedy s `confidence`, odstranitelné, a **nikdy nevstupuje do L0/L1
výpočtu** (D15). Negativní znalost je z toho nejlevnější a nejpodceněnější: „tady to není"
je informace, kterou dnes každá session objevuje znovu.

### 18.4 Skryté vazby — co statika nevidí a přesto jde najít

Ptáš se, jestli existují vazby, které jsme neodhalili. Existují, a některé jsou levné.
Společné mají to, že jsou **nejisté** — proto nesmí do L1 mezi exaktní hrany, ale patří
do vlastní vrstvy **L1-W (weak)** s confidence a s povinným označením v odpovědi.

| detektor | mechanismus | cena |
|---|---|---|
| **Řetězcové literály shodné se jménem symbolu** | index literálů × jména symbolů | triviální, vysoký recall |
| Jméno v konfiguraci / env / feature flagu | totéž nad hodnotami z §8.5 | triviální |
| Django `"app.Model"`, jména Celery úloh, DI klíče | vzor `call_pattern` s literálem | pravidlo |
| Jméno tabulky v SQL ↔ ORM model | lexikální shoda | levné |
| URL literál v jedné službě ↔ routa v jiné | průnik §8.6 a literálů | levné, cross-service |
| Jméno pytest fixture ↔ parametr testu | lexikální, per framework | pravidlo |
| Co se mění společně | git (§9) | už v návrhu |

První řádek stojí za rozvedení, protože je to nejlepší poměr cena/výnos v celé tabulce:
**každý řetězcový literál v repu, který se přesně shoduje se jménem nějakého symbolu, je
kandidát na dynamickou referenci.** Pokrývá `getattr`, `importlib`, registry pluginů,
routing přes stringy i serializační mapy — tedy velkou část těch 123 dynamických míst
z coverage analýzy. Je to čistě deterministické (D15), je to jeden join, a výsledek se
vrací jako:

```
weak links (2)                                    [L1-W, unverified]
  plugins/loader.py:22   literal "TokenValidator" matches [a4]   (getattr call nearby)
  config/services.yaml:8 literal "auth.TokenValidator" matches [a4]
```

Agent to buď potvrdí přečtením kódu, nebo zamítne — a přes §18.3 to může zapsat zpátky,
takže se příště neptá.

### 18.4b Ručně dopsané vazby a jejich zastarání

Slabé hrany z §18.4 jsou strojové kandidáty. Vedle nich musí jít **vazbu prostě
napsat**: agent nebo člověk kód přečetl a ví, že spojení existuje, i když ho statika
nikdy neuvidí — dispatch přes konfiguraci, kontrakt držený konvencí, runtime závislost.

```
cairn link <od> <do> --note "proč" --by agent|human
```

Zásadní je, co se s takovou vazbou stane při reindexu. **Je ukotvená v místě v kódu**
(soubor + řádek definice zdrojového symbolu) a to místo se hashuje. Když se změní:

- **nesmí se tiše zahodit** — byla by to ztráta práce, kterou statika neumí zopakovat
- **nesmí se tiše ponechat** — zastaralé tvrzení by se vydávalo za fakt

Takže se **označí `needs_review` a poctivě se to reportuje**: tady vznikla díra,
kterou statický průchod neumí zacelit, a je potřeba na ni znovu pustit model.
Příznak se nikdy nemaže automaticky — smaže ho jedině nový úsudek.

Provenience je součástí typu hrany (`L2, agent-asserted` / `L2, human-asserted`),
takže se ručně psaná vazba nikdy nesmíchá s exaktní.

### 18.6 Jeden graf, ne dva

Zvažovaná varianta: nechat agenta stavět si vedle našeho grafu vlastní, s primitivními
grafovými nástroji. **Zamítnuto**, a stojí za to napsat proč, protože ta potřeba za tím
je reálná.

**Dvě pravdy jsou horší než jedna neúplná.** Teze produktu je, že existuje vrstva, které
agent věří a přestane si ji hlídat. S vlastním grafem bez provenience a invalidace by je
musel při každém dotazu smiřovat — a to je ta explorační smyčka, jen o patro výš.

**Invalidace se nepřenáší.** Náš graf zneplatňují hashe obsahu; ruční vazba funguje jen
proto, že je ukotvená v kódu (§18.4b). Volný graf nemá kde být ukotvený, takže tiše shnije.

**A byla by to jiná firma.** Obecné grafové úložiště je memory produkt, ne navigace
v kódu — patřilo by na seznam „nikdy" hned vedle vlastních parserů a storage enginu.

#### Co ta potřeba reálně byla: uzly, které nejsou symboly

Chyběl přesně jeden typ uzlu. „OAuth flow", „billing doména" nejsou symboly a žádný
indexer je nevydá — takže agent, který se něco dozví, je nemá kam uložit. Řešením není
druhý graf, ale **koncepty v tom našem**, plus hrany koncept ↔ symbol. Mimochodem je to
i to, co potřebuje `cairn context` (§6.4).

Tři podmínky, které z toho nedělají grafovou databázi:

| podmínka | proč |
|---|---|
| **Ukotvení** k místu v kódu, s hashem | jinak se tvrzení nedá zneplatnit a tiše shnije |
| **Jmenné prostory** | domněnky jedné session jdou filtrovat i zahodit vcelku, bez dopadu na sdílené |
| **Žádné property, žádný dotazovací jazyk** | koncept má jméno, poznámku a vazby; víc už je graf DB |

Typ vztahu (`part-of`, `entry-point`, `owns`) je naopak volný text — slovník patří tomu,
kdo tvrdí, a uzavřený výčet by ho jen vyhnal zpátky k vlastnímu úložišti.

#### Autorská znalost žije ve vlastním souboru

`index.sqlite` je projekce a při reindexu i změně schématu se zahazuje. Autorská znalost
je jediná věc, která se **nedá znovu odvodit**, takže sdílet ten osud nesmí — bydlí
v `index-knowledge.sqlite` vedle a připojuje se přes `ATTACH`.

Z toho plyne druhá věc: **autorské řádky odkazují na symboly hashem, ne rowid.** Rowid se
přiděluje při každé indexaci znovu a po přestavbě by visel v prázdnu. Hash je stabilní
napříč přestavbami i stroji ze své podstaty (§5.1).

Když symbol z indexu zmizí úplně — kód byl přejmenován nebo smazán — vazba se
**nezahazuje ani netváří jako platná**, ale reportuje se jako `symbol gone`.

#### Co tím zároveň podporujeme

Scénář „postavím si vlastní pohled" jsme neodstřihli — `path:start-end` v každém řádku
a reference graph znamenají, že agent si vlastní graf **postavit může**, kdykoli chce.
Podporujeme to tím, že jsme dobrý *zdroj*, ne tím, že mu budeme dělat databázi.

### 18.5 Co tím nechceme

Ne agenta uvnitř nástroje. Osy z §18.1 jsou parametry deterministického výběru, ne
plánovač. A ne neomezenou sadu detektorů: každý slabý detektor, který má nízkou precision
a nikdo ho nepotvrzuje, jen nafukuje `weak links` a učí agenta tu sekci ignorovat —
což by zabilo i ty užitečné.
