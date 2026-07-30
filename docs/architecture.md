# Cairn — architektura

**Status:** návrh v0.1 · 30. 7. 2026
**Vstup:** brainstorming „Code Knowledge MCP"
**Rozhodnuto:** Python + Go současně · přenositelné (sdílitelné) artefakty od začátku · lokální binárka

---

## 0. Teze v jedné větě

Cairn je **lokální daemon s MCP frontendem**, který drží perzistentní, obsahem klíčovaný graf
struktury codebase a odpovídá agentovi na navigační dotazy deterministicky, kompaktně
a s explicitně přiznanou nejistotou — aby agent nemusel grepovat 12 kol.

Není to agent, není to IDE plugin, není to náhrada LLM. Je to **orientační vrstva pod LLM**.

---

## 1. Hraniční rozhodnutí (co určuje všechno ostatní)

| # | Rozhodnutí | Volba | Proč |
|---|---|---|---|
| D1 | Transport | **MCP přes stdio** | „Executable binárka na localhostu" a MCP stdio jsou totéž — host spustí proces, mluví po stdin/stdout. Žádný port, žádný HTTP, žádná autentizace. Jednodušší varianta neexistuje. |
| D2 | Procesní model | **Tenký MCP frontend + perzistentní daemon** | MCP server se spouští při každé session agenta. LSP servery startují sekundy až minuty. Stav musí přežít session — to je zároveň hlavní diferenciátor oproti Sereně. |
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

---

## 2. Procesní topologie

```
  Claude Code / Cursor / Zed / Codex
            │  MCP, stdio, JSON-RPC
            ▼
     ┌──────────────┐   spustí daemon, pokud neběží
     │  cairn mcp   │   bezstavový, ~5 MB RSS, start <10 ms
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

**Proč tenký frontend:** agent host může spustit 3 session paralelně. Tři kopie LSP poolu
by sežraly 6 GB RAM a reindexovaly totéž. Frontend je hloupý pipe; veškerý stav a všechny
subprocesy vlastní daemon.

**Životnost daemonu:** auto-start z frontendu (jako `gopls`/`tmux`), idle timeout ~30 min bez
připojeného klienta, ale **index na disku zůstává** — restart daemonu je studený start procesu,
ne studený start znalosti.

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
`blast_radius` vrátí 4 statické volající a 3 co-change kandidáty, musí být v odpovědi
vizuálně oddělené — jinak agent vezme statistiku za fakt.

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

Pro Django je levné řešení `django-types` / `django-stubs` stub balíčky
nakonfigurované pro pyright — ne vlastní plugin. Když se ORM používá jen na ORM
(což je náš případ), tohle pokryje 90 %.

### 4.5 Třetí rychlost: komentáře a dokumentace

Komentáře jsou **nejlepší existující most mezi jménem featury a symbolem**. „OAuth" se
často nevyskytuje v žádném identifikátoru, ale je hned v prvním řádku docstringu. Bez nich
stojí `get_context` na fuzzy matchi jmen a cest, což je ta slabší polovina §6.4.

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
   INDEX (symbol_id, role)          -- find_refs
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

-- FTS5, sloupce s klesající vahou; pohání find_symbol a seed pro get_context (§6.4)
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

## 6. MCP interface

### 6.1 Sada nástrojů — 6

Definice nástrojů jsou v kontextu při každém requestu. Rozpočet je tvrdý.

```
find_symbol(query, lang?, limit?)      → vstupní bod přes jméno / pattern
get_context(query)                     → vstupní bod přes koncept  (§6.4)
find_refs(handle, kind?)               → kind: callers | impls | overrides | writes | all
find_tests(handle)                     → testy pokrývající symbol (L0 + L3)
blast_radius(handle, depth?)           → co rozbiju změnou  (L1 + L3, oddělené)
expand(handle, what, depth?)           → what: body | doc | neighbors | file_skeleton
```

`status` **není nástroj** — je to MCP *resource* (`cairn://status`). Neplatí se v každém requestu.

Zvažované a zamítnuté: samostatný `find_implementations` (splynul do `find_refs(kind=impls)`),
`find_definition` (to je výstup `find_symbol`), cokoliv na zápis (cairn je read-only, záměrně).

### 6.2 Popisy nástrojů jsou produktová práce

Claude umí grep a sáhne po něm reflexivně. Popis musí říct **kdy je cairn lepší**, ne co dělá:

> `find_refs` — Najde všechna použití symbolu napříč Pythonem i Go, včetně volání přes
> gRPC hranici. **Použij místo grepu**, když hledáš uživatele funkce/třídy/metody: grep
> najde i komentáře, stringy a stejnojmenné symboly z jiných modulů, a nenajde volání
> přes alias importu. Vrací kompaktní seznam s handly pro další rozbalení.

Signál kvality (z brainstormingu, souhlas): **jestli agent nástroj používá bez skillu, je
dobře.** Skill jako doplněk pro workflow, ale výchozí chování musí fungovat bez něj —
většina lidí nasadí jen MCP server.

### 6.3 Formát odpovědi = produkt

Ne JSON. Kompaktní, řádkový, ASCII.

```
find_symbol("validate")
3 matches (2 suppressed: generated)
[a4] TokenValidator.validate(token: str) -> Claims    py  auth/oauth.py:142
[a7] SessionValidator.Validate(tok string) (*Claims, error)
                                                      go  internal/auth/session.go:88
[b1] validate(schema, payload)                        py  utils/schema.py:31
```

```
blast_radius[a4] depth=2

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

### 6.4 `get_context` — vstupní bod přes koncept

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
6. **LLM fallback** — až když skóre 0–5 nedosáhne prahu.

K bodu 6: použít **MCP sampling** (`sampling/createMessage`) — LLM call proběhne na modelu
hosta, cairn nepotřebuje API klíč ani nemá vlastní náklady. *Caveat: sampling nepodporují
všichni hosti. Fallback = vrátit seed set s poznámkou „low confidence, concept not cached"
a nechat expanzi na agentovi.* Výsledek se cachuje jako L2 artefakt.

Pak: expanze 1 hop přes call graph, ranking (§6.6), a vrátit **kostru 10–15 uzlů bez těl**.

> Past, na kterou je potřeba dát pozor: když `get_context("oauth")` vrátí 40 souborů
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

`find_symbol` může vrátit 200 shod. Vracíme 15. Který výběr, tam žije celá teze
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

Protože testovací repo je Python + Go přes gRPC, cross-language není fáze 5 — je to den 1
a zároveň to je ta část, kterou dnes nedělá nikdo.

### 7.1 Binder = malý plugin, který vyrábí hrany mezi symbol ID

Signatura konceptuálně: `fn bind(snapshot) -> Vec<Edge>`. Nic víc. Binders zapisují
do stejné `edges` tabulky s `source = binder_name`.

### 7.2 Proto binder (první a nejcennější)

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

### 7.3 Generated-code detekce (malá fičura, obrovský efekt)

gRPC repo je plné `*_pb2.py`, `*_pb2_grpc.py`, `*.pb.go`. Bez detekce každý dotaz utone.

Detekce: hlavičkové markery (`Code generated by protoc-gen-go. DO NOT EDIT.`,
`# Generated by the gRPC Python protocol compiler`), cestové vzory, `.gitattributes
linguist-generated`. Efekt: sbalit do jednoho řádku —
`+ 47 refs in generated code (suppressed; call find_refs(handle, include_generated=true))`.

### 7.4 Další binders (později, stejný mechanismus)

Django ORM model ↔ tabulka ↔ migrace · OpenAPI · env var ↔ config čtení · SQL v raw queries.

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

```
docker-compose.yml
  services.auth:
    build: { context: ./services/auth }
    ports: ["50051:50051"]
    environment: { DATABASE_URL: postgres://…, JWT_ISSUER: … }
         │
         ▼  services/auth/Dockerfile
  WORKDIR /app
  COPY services/auth /app                    ← mapa kontejner ↔ repo
  ENTRYPOINT ["gunicorn", "auth.wsgi:application", "--workers", "4"]
         │
         ▼  launcher resolver  (§8.4)
  services/auth/auth/wsgi.py :: application  ← symbol, dostane handle
         │
         ▼  route binder  (§8.6)
  POST /oauth/token → TokenView.post → [a4] TokenValidator.validate
```

Každá šipka je deterministická a levná. Žádný LLM, žádná heuristika — jen parsování
a tabulka známých vzorů.

### 8.3 Co se parsuje

**compose** (`docker-compose.yml`, `compose.yaml`, override soubory, `profiles`):

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

Ne obecný interpret shellu, ale **tabulka rozpoznávaných vzorů**. Deset vzorů pokryje
drtivou většinu reálných projektů.

Python:
- `gunicorn pkg.wsgi:application` → symbol `application` v `pkg/wsgi.py`
- `uvicorn app.main:app` → `app` v `app/main.py`
- `python -m pkg.server` → `pkg/server/__main__.py`, resp. `pkg/server.py`
- `celery -A proj worker` → **každý `@shared_task` je vlastní kořen**
- `manage.py <cmd>` → `management/commands/<cmd>.py::Command.handle`
- `pytest` → testovací kořeny

Go:
- `/app/server` → zpětně přes `RUN go build -o /app/server ./cmd/server` → `cmd/server/main.go::main`
- `go run ./cmd/x` → přímo

`entrypoint.sh` je v praxi hodně častý: přečíst skript a hledat závěrečný `exec …`.
Když se to nepovede rozřešit, jde to do **`unknown:`** — ne tichý fail. To je přímý
důsledek D8 a tady na tom záleží víc než kdekoli jinde, protože chybějící kořen
tiše prohlásí živý kód za mrtvý.

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

### 8.6 Route binder — request-level entrypointy

Compose dává kořeny na úrovni procesů. Webový projekt potřebuje kořeny na úrovni requestů.

- **Django** — `urls.py` je seznam `path()` / `re_path()` / `include()`; staticky čitelný AST včetně vnoření
- **FastAPI / Flask** — dekorátory `@app.get("/x")`, `@router.post(…)`
- **gRPC** — pokryto proto binderem (§7.2)
- **Go** — `http.HandleFunc`, chi/gin/echo `r.Get("/x", h)`; vzorově rozpoznatelné, ale
  registrace bývá dynamická → častěji `unknown`

Výstup je hrana `route:POST /oauth/token` → handler symbol. Tím jde odpovědět na
„který endpoint vede k tomuhle kódu", což je u auditu a code review nejčastější otázka vůbec.

Rozsah: Django + FastAPI/Flask + gRPC pokrývá testovací repo. Obecné Go routery jsou
králičí nora — omezit se na 2–3 nejčastější a zbytek přiznat.

### 8.7 Co z toho plyne pro ostatní vrstvy

**Service attribution.** Reachability z kořenů dá každému symbolu štítek služeb.
Levné a mění to tvar odpovědí:

```
blast_radius[a4] depth=2
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

**Lepší seed pro `get_context`** — zařazeno jako zdroj 0 v §6.4.

**Předpoklad pro L3 runtime trace.** Stack trace z běžícího kontejneru říká
`/app/auth/oauth.py`, repo říká `services/auth/auth/oauth.py`. Mapa z `WORKDIR` + `COPY`
ten překlad dělá — bez ní je runtime trace nepoužitelný. Proto tahle sekce předchází §9.

### 8.8 `cairn://topology` — resource, ne nástroj

Rozpočet nástrojů je 6 a chci ho udržet. Topologie je malá, stabilní a čte se jednou
za session → **MCP resource**, který se neplatí v každém requestu. Service attribution
se propisuje jako anotace do odpovědí stávajících nástrojů.

```
cairn://topology                                    ~300 tokenů

services (6)
  gateway   go    cmd/gateway/main.go:22          :8080 → public
  auth      py    services/auth/auth/wsgi.py      :50051 grpc
  worker    py    celery -A proj worker           —
  postgres  ext   postgres:16                     :5432
  redis     ext   redis:7                         :6379
  nats      ext   nats:2.10                       :4222

edges
  gateway → auth       grpc AuthService        [proto + env AUTH_GRPC_ADDR]
  gateway → postgres   env DATABASE_URL
  auth    → postgres   env DATABASE_URL
  worker  → redis      env CELERY_BROKER_URL   [depends_on]

public surface
  :8080  gateway  14 HTTP routes  ·  :50051  auth  3 grpc services

unknown (1)
  service `migrate` runs `manage.py migrate` — one-shot, no long-running root
stale: none
```

Riziko: agent si resource nemusí nikdy vyžádat. **Sedmý nástroj
`find_entrypoints(service?, route?)` přidat až tehdy, když to měření (§10) ukáže** —
je to věc k ověření, ne k předjímání. Skill mezitím může agentovi říct, ať topologii
přečte jako první.

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
    cairn-cli       binárka: `cairn mcp | daemon | index | query | eval`
    cairn-proto     sdílené typy, msgpack, socket protokol frontend↔daemon
    cairn-mcp       MCP protokol, definice nástrojů, popisy nástrojů
    cairn-fmt       renderer kompaktních odpovědí  ← produktová plocha, testuje se snapshoty
    cairn-daemon    supervizor, socket server, scheduler, deadliny
    cairn-store     CAS, SQLite projekce, snapshot, cache klíčování
    cairn-index     SCIP ingest, LSP klient pool, extrakce faktů
    cairn-binders   proto · compose · dockerfile · routes · env  (§7, §8)
    cairn-graph     L1 derivace: reference, call graph, blast radius, reachability, ranking
    cairn-git       gix, co-change, test impact, snapshoty z tree
    cairn-eval      měřicí harness
  docs/
    architecture.md
    adr/
```

`cairn-binders` je záměrně samostatný crate s jedním úzkým rozhraním
(`fn bind(snapshot) -> Vec<Edge>`): binders jsou to, co se bude přidávat nejčastěji,
a nesmí být zapletené do indexačního jádra.

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
| **0** — týden 1 | Spike na testovacím repu: `scip-python` + `scip-go` → podíl nevyřešených symbolů, čas, **syrová velikost indexu** (vstup pro cíl §5.5), podíl generovaného kódu. Ručně projít compose + Dockerfile a ověřit, že řetěz §8.2 drží. | Go/no-go pro D3, kalibrace D10 |
| **1** — týdny 2–6 | Daemon + store + CAS + snapshot + dirty overlay. Extrakce komentářů (§4.5) — je zadarmo a schéma FTS ji musí mít od začátku. `find_symbol`, `find_refs`, `expand`. MCP frontend. | Použitelný produkt |
| **2a** — týden 7 | Compose + Dockerfile binder, launcher resolver, `cairn://topology`, service attribution, commitnutelný `.cairn/topology.txt` + `topology --check`. | Mapa systému; nejlevnější kus v celém plánu |
| **2b** — týdny 8–9 | Proto binder + route binder + generated-code detekce → cross-language a cross-service hrany. `blast_radius`. | **Diferenciátor, který nikdo nemá** |
| **3** — týdny 10–12 | L3 z gitu (co-change, test impact). `find_tests`. `cairn-eval` a první měření proti baseline. | **Tady se rozhodne, jestli teze platí** |
| **4** | `get_context`, ranking, progressive disclosure tuning. Skill. | Produktová vrstva |
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
| `scip-python` neustojí Django | Fáze 1 slipne | Fáze 0 spike, do 1 týdne. Fallback: LSP bulk crawl |
| Agent nástroj nepoužije, sáhne po grepu | Produkt neexistuje | Popisy nástrojů jako produktová práce (§6.2). Měřit tool-call rate bez skillu |
| `deps_api_hash` nekonverguje na reálném kódu | Cache je k ničemu | Změřit hit rate ve fázi 1; fallback na hrubší klíč |
| Serena / konkurence dorazí dřív | Delta zmizí | Delta je perzistence + cross-language binders + L3, ne „máme graf". Zaměřit se na ně |
| Recall < 100 % na L0 | **Produkt je nebezpečný** | Zlatý standard + regrese v CI. Radši vrátit `unknown` než hádat |
| Nerozřešený entrypoint → živý kód označen za mrtvý | Tichá a velmi škodlivá chyba | Nerozřešený launcher je vždy `unknown`; dead-code signál se **nikdy** nevrací, pokud v projektu zbyl byť jeden nerozřešený kořen |
| Index nedosáhne velikostního cíle | Sdílená cache ztrácí smysl | Změřit ve fázi 0 na surovém SCIP indexu. Techniky §5.5 jsou přírůstkové, dá se přidávat |
| Binders bobtnají (K8s, Terraform, každý Go router) | Scope creep zadními vrátky | §8.9 je závazný seznam. Nový binder jen s doloženým výskytem v testovacím repu |
| Komentáře zaplaví fulltext šumem | `get_context` zhorší, ne zlepší | Vážené FTS sloupce, detekce zakomentovaného kódu, měřit precision seedu odděleně (§10) |
| Agent vezme zastaralý komentář jako fakt | Tichá chyba typu, kterému se celý návrh vyhýbá | Komentář v odpovědi je vždy `[comment, unverified]`; nikdy nevstupuje do L0/L1 tvrzení |
| `refs/cairn/cache` nafoukne objektovou DB | Pomalý clone/fetch | Ref je prunovatelný a force-pushovatelný; fetch volitelný. Když bolí, přejít na CI artefakt |
| Scope creep | Rok bez produktu | V dokumentu je jeden produkt. Držet fázi 0–3 |

---

## 15. Otevřené k rozhodnutí

1. **Jméno a licence** — `cairn` je pracovní. Open core (server MIT, sdílená cache placená)?
2. **Ne-kódová znalost** — PR diskuze, issues, ADR. „Proč je tohle takhle" tam bývá častěji
   než v AST. Zapadá jako další binder + L2, ale je to samostatný produkt. Zatím mimo scope.
3. **Multi-repo** — SCIP to umí ze své podstaty. Až bude jeden repo hotový.
4. **Windows** — named pipes místo unix socketu; jinak beze změny. Kdy?
