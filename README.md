# cairn

Lokální MCP server pro navigaci v codebase. Perzistentní, obsahem klíčovaný graf struktury
kódu, který odpovídá LLM agentovi na strukturální dotazy deterministicky a kompaktně —
místo 12 kol grepování.

- **Není agent.** Je to orientační vrstva pod agentem.
- **Nepíše parsery.** Staví na SCIP indexerech a language serverech.
- **Zná topologii, ne jen kód.** Docker Compose a Dockerfile dávají grafu kořeny
  (entrypointy) a oddíly (služby) — teprve tím dávají dosažitelnost a blast radius smysl.
- **Nikdy nelže.** Každá odpověď přiznává, co neví (`unknown:`) a co je zastaralé (`stale:`).

Cílový stack první verze: **Python + Go** (gRPC, Django ORM), nasazené přes Docker Compose.

→ [docs/architecture.md](docs/architecture.md)

## Stav

Návrh v0.1. Žádný kód. Další krok: fáze 0 spike — ověřit `scip-python` na reálném repu.
