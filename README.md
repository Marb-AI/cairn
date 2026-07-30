# cairn

Lokální CLI pro navigaci v codebase. Perzistentní, obsahem klíčovaný graf struktury kódu,
který odpovídá LLM agentovi na strukturální dotazy deterministicky a kompaktně —
místo 12 kol grepování.

```
$ cairn blast a4 --depth 2
$ cairn topology
$ cairn refs a4 --kind callers
```

- **Není agent.** Je to orientační vrstva pod agentem.
- **Bez LLM.** Celý index se postaví offline, bez API klíče. Model smí znalost jen
  obohatit, nikdy ji nezakládá.
- **Nezná žádný jazyk.** Jádro pracuje s jazykově neutrálním schématem; znalost
  ekosystémů žije v deklarativních pravidlech. Přidání jazyka = provider + balíček
  pravidel, nula změn v jádře.
- **Nepíše parsery.** Staví na SCIP indexerech a language serverech.
- **Zná topologii, ne jen kód.** Docker Compose a Dockerfile dávají grafu kořeny
  (entrypointy) a oddíly (služby) — teprve tím dávají dosažitelnost a blast radius smysl.
- **Nikdy nelže.** Každá odpověď přiznává, co neví (`unknown:`) a co je zastaralé (`stale:`).

Agentovi se představuje skillem, ne MCP serverem — stejně jako `gh` nebo `rg`.

Cílový stack první verze: **Python + Go** (gRPC, Django ORM), nasazené přes Docker Compose.

→ [docs/architecture.md](docs/architecture.md)
→ [docs/coverage-analysis.md](docs/coverage-analysis.md) — ověření na reálném repu

## Build

Všechno běží v Dockeru — daemon, language servery, indexery i build. Na hostiteli
se nic neinstaluje, žádné `cargo`, Node ani Go toolchain. Viz §2.1.

## Stav

Návrh v0.2, žádný kód. Kalibrováno na reálném repu (§16).

Další krok: **fáze 0 spike** v kontejneru — `make pbgen` → `scip-python` + `scip-go`,
měřeno s vygenerovanými protobuf stuby i bez nich.
