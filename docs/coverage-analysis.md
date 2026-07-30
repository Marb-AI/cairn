# Analýza pokrytí — zvládneme zaindexovat tenhle kód?

**Repo:** an internal repository · ověřeno 30. 7. 2026 · doprovod k [architecture.md](architecture.md)

> Naměřená čísla jsou v [spike-0-results.md](spike-0-results.md). Tenhle dokument je
> analýza čtením kódu; kde se s měřením rozešel, platí měření — viz §9.

Otázka: máme popsané všechny postupy potřebné k tomu, abychom nad tímhle konkrétním
kódem uměli odpovědět na strukturální dotazy? Tenhle dokument prochází kategorii po
kategorii a u každé říká **ověřeno / mezera / mimo rozsah**.

> **Tenhle dokument je důkaz, ne zadání.** Repo slouží k ověření, že *obecné* řešení
> funguje. Každý nález je proto v architektuře zapsaný jako instance nějakého tvaru
> (§1.1), ne jako podpora konkrétního frameworku. Kdykoli se dole píše „FastAPI" nebo
> „grpclib", odpovídá tomu v architektuře řádek v tabulce pravidel, ne `if` v Rustu.
> Zkouška opačným směrem — co stojí přidat JS/TS — je v architecture §17.

Metoda: čtení kódu, ne spuštění indexeru. Indexery doběhly ve fázi 0 —
viz [spike-0-results.md](spike-0-results.md).

---

## 0. Shrnutí

| Kategorie | Stav | Kde |
|---|---|---|
| Symboly, definice, reference | ✅ ověřeno | §1 |
| Call graph uvnitř jazyka | ✅ ověřeno | §2 |
| Procesní entrypointy z compose | ✅ ověřeno, všech 15 | §3 |
| gRPC plocha (71 služeb) | ✅ ověřeno, jeden vzor na jazyk | §4 |
| Cross-language hrany Go ↔ Python | ✅ ověřeno | §4.3 |
| HTTP endpointy | ✅ ověřeno, 4 vzory — **ne „jiná liga"** | §5 |
| Django ORM | ⚠️ podmíněno stub balíčky | §6 |
| Dynamika (`getattr`, `importlib`) | ⚠️ 123 výskytů → `unknown` | §7 |
| Generovaný kód | ✅ ověřeno, ale 65 % Go | §8 |
| ~~Chybějící Python stuby~~ | ❌ **mylný nález, odvoláno** | §9 |

Závěr: **postupy jsou popsané a stačí.** Nic není blokující. Tři položky jsou částečné
a všechny tři mají definované chování (`unknown` / `degraded`), ne tichý fail.

**Žádná z položek v tabulce nepotřebuje LLM.** To je záměr, ne náhoda — invariant D15
říká, že se index staví kompletně deterministicky a model smí znalost jen obohatit.
Tenhle dokument je zároveň doklad, že to na reálném repu vychází: 71 gRPC služeb,
122 HTTP rout, 15 entrypointů a cross-language hrany se dají získat parsováním,
konvencí a joinem — bez jediného volání modelu.

---

## 1. Symboly, definice, reference

**Postup:** SCIP indexery na dávkovou cestu, LSP na dirty soubory (architecture §4).
Výstup jsou occurrences se stabilním symbol ID; reference vzniknou joinem přes symbol ID (§5.4).

**Co v repu:**
- Python 218 193 řádků / 1 184 souborů, Go 158 874 / 516
- Standardní layout: `srcpy/domains/<doména>/<vrstva>/`, `srcgo/domains/<doména>/…`
- Importy jsou explicitní a převážně absolutní (`from domains.orders.repository import chat as chat_repo`)

**Verdikt: ✅.** Nic exotického. Aliasy importů (`as chat_repo`) jsou přesně ten případ,
kde grep selhává a name resolution vyhrává — dobrý demo materiál pro §6.2 skill.

**Pozor na jednu věc:** handlery importují lazy uvnitř funkce (`get_handlers()` má 24
importů v těle). Pyright to zvládá, ale znamená to, že **modulový import graf není úplný** —
závislost existuje jen uvnitř funkce. Pro `deps_api_hash` (architecture §5.2) to nevadí,
protože ten se počítá z rozřešených importů, ne z top-level příkazů. Stojí za ověření
ve fázi 0.

---

## 2. Call graph uvnitř jazyka

**Postup:** L1 derivace joinem occurrences (architecture §3, §5.4). SCIP dává role
`definition` / `reference`; hrany `calls` vznikají z referencí na volatelné symboly.

**Co v repu:** vrstvená architektura `handlers → repository → models`, běžná volání.
Go má `NewHandler(app)` konstruktory a metody na strukturách.

**Verdikt: ✅.** Standardní případ, na který jsou SCIP indexery stavěné.

**Nepokrývá** (a je to očekávané): volání přes callback předaný jako hodnota,
dependency injection přes `app` objekt v Go. To druhé je v repu časté —
`area.NewHandler(app)` dostane kontejner a z něj si tahá závislosti. Call graph tak bude
mít hranu do `NewHandler`, ale ne do toho, co si handler z `app` vytáhne. **Známé omezení
statické analýzy, ne díra v návrhu** — patří do `unknown`, případně později do L3 z runtime.

---

## 3. Procesní entrypointy z compose

**Postup:** architecture §8.2–8.4, řetěz `compose → Dockerfile → command → symbol`.

**Ověřeno na obou jazycích:**

```
# Python — přímočaré
services.orders-grpc.command = "python3 -m domains.orders.grpc.server"
  → vzor `python -m`  →  srcpy/domains/orders/grpc/server.py

# Go — dva hopy
services.scoring-grpc.command = "/bin/grpcserver"
  → srcgo/Dockerfile: COPY --from=builder /out/grpcserver /bin/grpcserver
  → srcgo/Dockerfile: RUN xx-go build -o /out/grpcserver ./domains/orders/cmd/grpcserver/server.go
  → srcgo/domains/orders/cmd/grpcserver/server.go :: main
```

**Verdikt: ✅ pro všech 15 služeb.** Žádný `entrypoint.sh` obal, což byla obava
v architecture §8.4. Všechny `command:` jsou buď `python3 -m …`, `/bin/<binárka>`,
nebo `manage.py <cmd>`.

**Potvrzené komplikace, které už jsou v návrhu zapsané:**
- kotvy `<<: *base-service`, `build: *build-go` (§8.3)
- wrapper `xx-go` místo `go build` (§8.2)
- 8 binárek z jednoho Dockerfile → mapování `-o` cesta ↔ balíček musí být tabulka, ne jeden záznam
- `volumes: ["./srcpy:/app/"]` jako autoritativní mapa cest (§8.7)

---

## 4. gRPC plocha

71 `service` definic ve 139 `.proto` souborech. Tohle je největší část systému a zároveň
místo, kde má cairn největší delta oproti grepu.

### 4.1 Python — je to dědičnost, ne registrace

Repo používá **grpclib**, ne `grpcio`. Neexistuje tedy `add_XxxServicer_to_server`.
Vazba je čistá dědičnost:

```python
class ChatServiceHandler(DjangoExceptionHandlerMixin, orders_api.ChatServiceBase):
```

**To je zásadně dobrá zpráva:** hrana handler → proto služba je obyčejná `implements`,
kterou L0 dá zadarmo. Proto binder musí umět jedinou věc navíc — mapovat generovanou bázi
`ChatServiceBase` zpět na `proto:ChatService`, což je konvence protoc.

Registrace do serveru je navíc statická a čitelná: `get_handlers()` vrací literální seznam
konstruktorů (`AuthServiceHandler(), ChatServiceHandler(), …`), takže i vazba
**služba → které handlery v ní běží** je staticky rozřešitelná.

### 4.2 Go — jeden vzor volání

```go
regions_api.RegisterAreaQueryServiceServer(server, area.NewHandler(app))
orders_fe.RegisterAuthServiceServer(s, resttransform.NewAuthService(app))
```

Kanonický `protoc-gen-go-grpc` vzor `Register<Service>Server(s, impl)`. Volání jsou navíc
v `cmd/*/server.go`, tedy přímo v entrypointu dosažitelném z compose.

**Verdikt: ✅.** Jeden rozpoznávaný vzor na jazyk, oba triviální.

### 4.3 Cross-language hrana

Řetěz, kvůli kterému celý §7/§8 existuje, na tomhle repu drží:

```
compose: orders-proxy (go)  ──command──►  cmd/resttransform/server.go :: main
                                                  │
                              orders_fe.RegisterAuthServiceServer(s, NewAuthService(app))
                                                  │
                                        proto: orders_fe.AuthService
                                                  │
                          class AuthServiceHandler(…, orders_api.AuthServiceBase)
                                                  │
compose: orders-grpc (py)  ◄──────────  srcpy/domains/orders/grpc/handlers/auth.py
```

Otázka „kdo volá tenhle Python handler" má odpověď v Go kódu, a naopak. **Ani grep,
ani pyright, ani gopls to samostatně nedají.**

---

## 5. HTTP endpointy — obava se nepotvrdila

Zadání znělo, že endpointy jsou „jiná liga, musel by ten tool znát všechny frameworky".
Na reálném repu to tak není: jsou to **čtyři vzory a jeden z nich má jeden výskyt.**

| framework | vzor | rozsah |
|---|---|---|
| FastAPI | `x = APIRouter(prefix="/orders", tags=[…])` + `@x.get("/y", operation_id=…)` + `app.include_router(x, dependencies=[…])` | **122 endpointů, 20 routerů, 1 app** |
| gRPC | §4 | 71 služeb |
| Django | 3× `urls.py`, `urlpatterns` / `path()` / `include()`, + `admin.site` (4× `admin.py`) | admin |
| Go HTTP | `http.NewServeMux()` + `mux.HandleFunc("GET /{key}", …)` | **1 soubor** |

Žádné chi, gin, echo, Flask, Starlette-přímo ani DRF. Skládání cesty je textová konkatenace
`prefix` + cesta z dekorátoru.

### 5.1 Dva dárky navíc

**`operation_id="signup"`** je na každém FastAPI endpointu. Je stabilnější než cesta
(cesty se mění, operation_id ne, protože se z něj generuje klient) a je to lepší primární
klíč routy než URL.

**Autentizace je staticky viditelná.** Router se přidává buď holý, nebo se závislostí:

```python
app.include_router(endpoints.beta_access)                                        # public
app.include_router(endpoints.financial, dependencies=[Depends(get_authenticator())])
```

Z toho plyne, že `cairn topology` umí bez jakéhokoli LLM říct
**„122 rout, z toho 12 neautentizovaných"** — a pro auditní doménu, což je podle
brainstormingu cílový trh, je to samostatně prodejný výstup.

### 5.2 Levný únik, kdyby vzory nestačily

Většina frameworků umí vypsat svou routovací tabulku: FastAPI `app.openapi()`,
Django `get_resolver().url_patterns`, Flask `app.url_map`. Repo navíc už OpenAPI generuje
(`protoc-gen-openapiv2`, `tools/pbgen/openapi`, `api/openapi_config.py`).

Cena je, že se musí naimportovat aplikace — tedy **runtime probe, ne statická analýza**.
Patří proto do L3 vedle coverage, ne do L0. Doporučení: statické vzory teď, runtime dump
jako opt-in booster. Rozhodně ne obráceně, protože by to z read-only nástroje udělalo něco,
co spouští cizí kód.

Když se statika a runtime rozejdou, je to **nález, ne chyba** — přesně stejná dualita jako
u zbytku návrhu.

**Verdikt: ✅**, s tím, že Go routery obecně (chi/gin/echo) zůstávají na seznamu
„až bude doložený výskyt" (architecture §8.9).

---

## 6. Django ORM

**Co v repu:** Django 5.2.6, 4× `admin.py`, modely v `domains/*/rds/`, `pytest-django`,
`DJANGO_SETTINGS_MODULE` per služba.

**Postup:** architecture §4.4 — spolehnout se na `django-types` / `django-stubs`, ne psát
vlastní plugin.

**Verdikt: ⚠️ podmíněné.** Pyright bez stubů na `Model.objects` a reverse accessorech
selhává tiše. Nutné ověřit ve fázi 0 jako samostatné číslo, oddělené od §9.

Pozitivum: `DJANGO_SETTINGS_MODULE=domains.orders.grpc.settings` je v compose
per služba — takže konfigurace stubů jde odvodit, ne hádat.

Použití je navíc konzervativní: ORM + admin, žádné Django views mimo admin,
žádné DRF serializery. To je ta nejlepší varianta.

---

## 7. Dynamika

**Nalezeno:** 123 výskytů `importlib` nebo `getattr(` v `srcpy` mimo testy.

**Postup:** architecture §6.3 — vrátit kandidáty a přiznat nejistotu, nikdy nemlčet.

**Verdikt: ⚠️ očekávané, s definovaným chováním.** Tohle je přesně ten materiál, kvůli
kterému je `unknown:` povinná sekce každé odpovědi. Ve fázi 0 stojí za to je roztřídit —
podezření je, že většina jsou `getattr(obj, "attr", default)` na známém objektu, což není
dynamický dispatch a nikoho netrápí. Skutečně problematické jsou jen `importlib` na
proměnné a `getattr` s neliterálním jménem.

Číslo do fáze 0: **kolik ze 123 je skutečně nerozřešitelných**.

---

## 8. Generovaný kód

**Nalezeno:** 220 `.pb.go` = **103 176 ze 158 874 řádků Go, tedy 65 %**.

**Postup:** architecture §7.3 — detekce hlavičkovým markerem, potlačení do jednoho řádku.

**Verdikt: ✅, ale povýšit prioritu.** Při 65 % by bez potlačení byla většina odpovědí
seznam `.pb.go` souborů. To není kosmetika, to je rozdíl mezi použitelným a nepoužitelným
nástrojem. Zároveň to má přímý dopad na velikost indexu (architecture §5.5) — proto
oddělený CAS namespace pro generované soubory.

---

## 9. Chybějící generovaný kód — ODVOLÁNO

**Původní tvrzení bylo špatně.** Dřívější verze téhle sekce tvrdila, že Python protobuf
stuby v repu nejsou a že to stojí celou gRPC plochu Python strany. Není to pravda.

**Stuby jsou commitnuté.** betterproto2 negeneruje `*_pb2.py`, ale jeden velký
`__init__.py` na proto balíček: 13 souborů, **48 952 řádků, 51 tříd `*ServiceBase`**.
Původní kontrola počítala soubory a hledala vzor jména, který tenhle generátor nikdy
nevyrobí.

Doloženo měřením (spike, §5 tam): index obsahuje všech 51 definic `*ServiceBase`
i všech 34 `*ServiceHandler`, a přegenerování stubů nezmění nic — 2 183 referencí
na `*ServiceBase` před i po.

**Co z toho zůstává v platnosti:** obecný mechanismus v architecture §4.6 (D14) —
generovaný artefakt může chybět a jiné ekosystémy nemají CI pojistku. Toto repo ale
není jeho příkladem a nesmí se jako příklad citovat.

**Poučení do návrhu, které tím naopak zesílilo:** detekce generovaného kódu (§8) se
nesmí opírat o vzory jmen souborů. `srcpy/schema/orders_api/__init__.py` je
48 tisíc řádků generovaného kódu ve jménu, které vypadá jako běžný balíček.
Rozhoduje **hlavičkový marker a `.gitattributes`**, ne přípona — přesně jak to
architecture §7.3 řadí.

## 10. Co tenhle dokument nepokrývá

- **Výkon.** Jestli dotaz doběhne do 20 ms na 377k řádcích — fáze 0.
- **Recall.** Jestli je 100 % na L0/L1 — potřebuje zlatý standard, fáze 1.
- **Velikost indexu.** Syrový SCIP index → kalibrace cíle 50 MB.
- **`compose.local.yaml` / `compose.test.yaml`.** Merge sémantika override souborů se
  ověřovala jen na `compose.yaml`.
- **`infra/sentinel`, `e2e/`, `tools/`.** Mimo hlavní dva stromy, neprocházeno.

---

## 11. Co z analýzy plyne pro plán

1. ~~Codegen krok je součást fáze 0.~~ Odvoláno, viz §9 — stuby jsou v repu.
   Zůstává obecné pravidlo: **detekce generovaného kódu podle markeru, ne podle
   jména souboru** (§9, poslední odstavec).
2. **Route binder je levnější, než se čekalo** — 4 vzory místo „všechny frameworky".
   Zvážit posun z fáze 2b do 2a, protože `122 rout, z toho 12 neautentizovaných`
   je efektní výstup hned na začátku.
3. **Proto binder je jednodušší, než se čekalo** — pro Python je to dědičnost, kterou
   L0 dá zadarmo; potřeba je jen mapování konvence protoc.
4. **Generated-code detekce nahoru.** 65 % Go kódu není okrajový případ.
5. **Roztřídit 123 dynamických výskytů** — dá reálný odhad velikosti `unknown` sekcí.
