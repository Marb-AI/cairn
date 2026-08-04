# Coverage analysis — can we index this code at all?

**Repo:** an internal repository · verified 2026-07-30 · companion to [architecture.md](architecture.md)

> The measured numbers are in [spike-0-results.md](spike-0-results.md). This document is an
> analysis by reading code; where the two disagreed, the measurement wins — see §9.

The question: have we described every technique needed to answer structural questions over
this particular codebase? This document goes category by category and marks each one
**verified / gap / out of scope**.

> **This document is evidence, not a specification.** The repository is here to confirm that
> the *general* solution works. Every finding is therefore recorded in the architecture as an
> instance of some shape (§1.1), not as support for a particular framework. Wherever "FastAPI"
> or "grpclib" appears below, what corresponds to it in the architecture is a row in a rule
> table, not an `if` in Rust. The test in the other direction — what it costs to add JS/TS —
> is in architecture §17.

Method: reading code, not running the indexer. The indexers were run in phase 0 — see
[spike-0-results.md](spike-0-results.md).

---

## 0. Summary

| Category | State | Where |
|---|---|---|
| Symbols, definitions, references | ✅ verified | §1 |
| Call graph within one language | ✅ verified | §2 |
| Process entrypoints from compose | ✅ verified, all 15 | §3 |
| gRPC surface (71 services) | ✅ verified, one pattern per language | §4 |
| Cross-language edges Go ↔ Python | ✅ verified | §4.3 |
| HTTP endpoints | ✅ verified, 4 patterns — **not "a different league"** | §5 |
| Django ORM | ⚠️ conditional on stub packages | §6 |
| Dynamism (`getattr`, `importlib`) | ⚠️ 123 occurrences → `unknown` | §7 |
| Generated code | ✅ verified, but 65% of Go | §8 |
| ~~Missing Python stubs~~ | ❌ **mistaken finding, withdrawn** | §9 |

Conclusion: **the techniques are described and they are enough.** Nothing is blocking. Three
items are partial and all three have defined behaviour (`unknown` / `degraded`), not a silent
failure.

**None of the items in the table needs an LLM.** That is by design, not by luck — invariant
D15 says the index is built entirely deterministically and a model may only enrich the
knowledge. This document is also evidence that it holds up on a real repository: 71 gRPC
services, 122 HTTP routes, 15 entrypoints and the cross-language edges can all be obtained by
parsing, convention and a join — without a single call to a model.

---

## 1. Symbols, definitions, references

**Technique:** SCIP indexers on the batch path, LSP for dirty files (architecture §4). The
output is occurrences with stable symbol IDs; references come from a join on the symbol ID
(§5.4).

**What is in the repository:**
- Python 218,193 lines / 1,184 files, Go 158,874 / 516
- A standard layout: `srcpy/domains/<domain>/<layer>/`, `srcgo/domains/<domain>/…`
- Imports are explicit and mostly absolute (`from domains.orders.repository import chat as chat_repo`)

**Verdict: ✅.** Nothing exotic. Import aliases (`as chat_repo`) are exactly the case where
grep fails and name resolution wins — good demonstration material for the skill in §6.2.

**One thing to watch:** handlers import lazily inside functions (`get_handlers()` has 24
imports in its body). Pyright copes, but it means **the module import graph is not complete** —
the dependency exists only inside the function. That does not matter for `deps_api_hash`
(architecture §5.2), which is computed from resolved imports rather than from top-level
statements. Worth confirming in phase 0.

---

## 2. Call graph within one language

**Technique:** L1 derivation by joining occurrences (architecture §3, §5.4). SCIP gives
`definition` / `reference` roles; `calls` edges come from references to callable symbols.

**What is in the repository:** a layered architecture, `handlers → repository → models`, with
ordinary calls. Go has `NewHandler(app)` constructors and methods on structs.

**Verdict: ✅.** The standard case SCIP indexers are built for.

**Not covered** (and this is expected): calls through a callback passed as a value, and
dependency injection through the `app` object in Go. The second is common in this repository —
`area.NewHandler(app)` receives a container and pulls its dependencies out of it. The call
graph will therefore have an edge into `NewHandler` but not into whatever the handler takes
out of `app`. **A known limitation of static analysis, not a hole in the design** — it belongs
in `unknown`, and possibly later in L3 from runtime.

---

## 3. Process entrypoints from compose

**Technique:** architecture §8.2–8.4, the chain `compose → Dockerfile → command → symbol`.

**Verified in both languages:**

```
# Python — direct
services.orders-grpc.command = "python3 -m domains.orders.grpc.server"
  → the `python -m` pattern  →  srcpy/domains/orders/grpc/server.py

# Go — two hops
services.scoring-grpc.command = "/bin/grpcserver"
  → srcgo/Dockerfile: COPY --from=builder /out/grpcserver /bin/grpcserver
  → srcgo/Dockerfile: RUN xx-go build -o /out/grpcserver ./domains/orders/cmd/grpcserver/server.go
  → srcgo/domains/orders/cmd/grpcserver/server.go :: main
```

**Verdict: ✅ for all 15 services.** No `entrypoint.sh` wrapper, which was the worry in
architecture §8.4. Every `command:` is either `python3 -m …`, `/bin/<binary>`, or
`manage.py <cmd>`.

**Confirmed complications, already recorded in the design:**
- anchors `<<: *base-service`, `build: *build-go` (§8.3)
- the `xx-go` wrapper instead of `go build` (§8.2)
- 8 binaries from one Dockerfile → the mapping from `-o` path to package has to be a table,
  not a single entry
- `volumes: ["./srcpy:/app/"]` as the authoritative path map (§8.7)

---

## 4. The gRPC surface

71 `service` definitions across 139 `.proto` files. This is the largest part of the system and
also where cairn's delta over grep is biggest.

### 4.1 Python — it is inheritance, not registration

The repository uses **grpclib**, not `grpcio`. So there is no `add_XxxServicer_to_server`. The
binding is plain inheritance:

```python
class ChatServiceHandler(DjangoExceptionHandlerMixin, orders_api.ChatServiceBase):
```

**That is fundamentally good news:** the handler → proto service edge is an ordinary
`implements`, which L0 gives for free. The proto binder therefore needs exactly one extra
capability — mapping the generated base `ChatServiceBase` back to `proto:ChatService`, which is
a protoc convention.

Registration into the server is static and readable besides: `get_handlers()` returns a literal
list of constructors (`AuthServiceHandler(), ChatServiceHandler(), …`), so even the binding
**service → which handlers run in it** is statically resolvable.

### 4.2 Go — one call pattern

```go
regions_api.RegisterAreaQueryServiceServer(server, area.NewHandler(app))
orders_fe.RegisterAuthServiceServer(s, resttransform.NewAuthService(app))
```

The canonical `protoc-gen-go-grpc` pattern `Register<Service>Server(s, impl)`. The calls are
also in `cmd/*/server.go`, that is, directly in the entrypoint reachable from compose.

**Verdict: ✅.** One recognised pattern per language, both trivial.

### 4.3 The cross-language edge

The chain that the whole of §7/§8 exists for holds on this repository:

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

"Who calls this Python handler" has its answer in Go code, and vice versa. **Neither grep, nor
pyright, nor gopls can produce that on its own.**

---

## 5. HTTP endpoints — the worry did not hold up

The brief said endpoints were "a different league, the tool would have to know every
framework". On the real repository that is not so: there are **four patterns and one of them
has a single occurrence.**

| framework | pattern | scale |
|---|---|---|
| FastAPI | `x = APIRouter(prefix="/orders", tags=[…])` + `@x.get("/y", operation_id=…)` + `app.include_router(x, dependencies=[…])` | **122 endpoints, 20 routers, 1 app** |
| gRPC | §4 | 71 services |
| Django | 3× `urls.py`, `urlpatterns` / `path()` / `include()`, + `admin.site` (4× `admin.py`) | admin |
| Go HTTP | `http.NewServeMux()` + `mux.HandleFunc("GET /{key}", …)` | **1 file** |

No chi, gin, echo, Flask, Starlette directly, or DRF. Assembling a path is textual
concatenation of `prefix` and the path from the decorator.

### 5.1 Two extra gifts

**`operation_id="signup"`** is on every FastAPI endpoint. It is more stable than the path
(paths change, operation ids do not, because the client is generated from them) and it is a
better primary key for a route than the URL.

**Authentication is statically visible.** A router is added either bare or with a dependency:

```python
app.include_router(endpoints.beta_access)                                        # public
app.include_router(endpoints.financial, dependencies=[Depends(get_authenticator())])
```

From which it follows that `cairn topology` can say, with no LLM whatsoever, **"122 routes, 12
of them unauthenticated"** — and for the audit domain, which the brainstorming names as the
target market, that is a saleable output on its own.

### 5.2 A cheap escape hatch, if the patterns were not enough

Most frameworks can print their own routing table: FastAPI `app.openapi()`, Django
`get_resolver().url_patterns`, Flask `app.url_map`. The repository also already generates
OpenAPI (`protoc-gen-openapiv2`, `tools/pbgen/openapi`, `api/openapi_config.py`).

The price is that the application has to be imported — that is, **a runtime probe, not static
analysis**. It therefore belongs in L3 alongside coverage, not in L0. Recommendation: static
patterns now, a runtime dump as an opt-in booster. Definitely not the other way round, because
that would turn a read-only tool into one that executes someone else's code.

When the static and runtime views disagree, that is **a finding, not a fault** — exactly the
same duality as everywhere else in the design.

**Verdict: ✅**, with Go routers in general (chi/gin/echo) staying on the "when there is a
documented occurrence" list (architecture §8.9).

---

## 6. Django ORM

**What is in the repository:** Django 5.2.6, 4× `admin.py`, models in `domains/*/rds/`,
`pytest-django`, `DJANGO_SETTINGS_MODULE` per service.

**Technique:** architecture §4.4 — rely on `django-types` / `django-stubs` rather than writing
a plugin.

**Verdict: ⚠️ conditional.** Without stubs, pyright fails silently on `Model.objects` and on
reverse accessors. This has to be confirmed in phase 0 as a separate number, kept apart
from §9.

On the positive side: `DJANGO_SETTINGS_MODULE=domains.orders.grpc.settings` is in compose per
service — so the stub configuration can be derived rather than guessed.

The usage is conservative besides: ORM plus admin, no Django views outside admin, no DRF
serializers. That is the best case available.

---

## 7. Dynamism

**Found:** 123 occurrences of `importlib` or `getattr(` in `srcpy` outside tests.

**Technique:** architecture §6.3 — return candidates and admit the uncertainty, never stay
silent.

**Verdict: ⚠️ expected, with defined behaviour.** This is precisely the material that makes
`unknown:` a mandatory section of every answer. In phase 0 it is worth sorting them — the
suspicion is that most are `getattr(obj, "attr", default)` on a known object, which is not
dynamic dispatch and bothers nobody. The genuinely problematic ones are only `importlib` on a
variable and `getattr` with a non-literal name.

The number for phase 0: **how many of the 123 are actually unresolvable**.

---

## 8. Generated code

**Found:** 220 `.pb.go` files = **103,176 of 158,874 lines of Go, that is 65%**.

**Technique:** architecture §7.3 — detection by header marker, suppression down to a single
line.

**Verdict: ✅, but raise its priority.** At 65%, without suppression most answers would be a
list of `.pb.go` files. That is not cosmetics, it is the difference between a usable and an
unusable tool. It also bears directly on index size (architecture §5.5) — hence the separate
CAS namespace for generated files.

---

## 9. Missing generated code — WITHDRAWN

**The original claim was wrong.** An earlier version of this section claimed the Python
protobuf stubs were not in the repository and that this cost the entire Python-side gRPC
surface. That is not true.

**The stubs are committed.** betterproto2 does not generate `*_pb2.py` but one large
`__init__.py` per proto package: 13 files, **48,952 lines, 51 `*ServiceBase` classes**. The
original check counted files and looked for a name pattern this generator never produces.

Evidenced by measurement (the spike, §5 there): the index contains all 51 `*ServiceBase`
definitions and all 34 `*ServiceHandler`s, and regenerating the stubs changes nothing — 2,183
references to `*ServiceBase` before and after.

**What remains valid:** the general mechanism in architecture §4.6 (D14) — a generated artefact
can be missing, and other ecosystems have no CI safety net. This repository, though, is not an
example of it and must not be cited as one.

**The lesson for the design, which this strengthened rather than weakened:** detection of
generated code (§8) must not rest on file-name patterns. `srcpy/schema/orders_api/__init__.py`
is 48 thousand lines of generated code under a name that looks like an ordinary package. What
decides is **the header marker and `.gitattributes`**, not the extension — exactly the order
architecture §7.3 puts them in.

## 10. What this document does not cover

- **Performance.** Whether a query finishes within 20 ms over 377k lines — phase 0.
- **Recall.** Whether it is 100% on L0/L1 — needs a gold standard, phase 1.
- **Index size.** Raw SCIP index → calibration against the 50 MB target.
- **`compose.local.yaml` / `compose.test.yaml`.** The merge semantics of override files were
  only checked against `compose.yaml`.
- **`infra/sentinel`, `e2e/`, `tools/`.** Outside the two main trees, not gone through.

---

## 11. What the analysis implies for the plan

1. ~~The codegen step is part of phase 0.~~ Withdrawn, see §9 — the stubs are in the
   repository. What remains is the general rule: **detect generated code by marker, not by file
   name** (§9, last paragraph).
2. **The route binder is cheaper than expected** — 4 patterns instead of "every framework".
   Consider moving it from phase 2b to 2a, because `122 routes, 12 of them unauthenticated` is
   a striking output to have right at the start.
3. **The proto binder is simpler than expected** — for Python it is inheritance, which L0 gives
   for free; all that is needed is the mapping of a protoc convention.
4. **Move generated-code detection up.** 65% of the Go code is not an edge case.
5. **Sort the 123 dynamic occurrences** — it gives a real estimate of how large the `unknown`
   sections will be.
