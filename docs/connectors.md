# Writing a connector

A connector contributes facts cairn cannot derive on its own: a service contract, a
domain, an entity, an event — anything declared somewhere other than in the code the
indexers read. The core stores them, ranks them with SCIP's own facts, walks them, and
never learns what any of them mean.

This page is the contract. It is enough to write a connector against; it says nothing
about any particular project model, because that is the connector's business and not
cairn's.

## The shape of it

```
.cairn/
  index.sqlite
  connectors/
    w17.yaml          ← one manifest per connector
  cache/connectors/
    w17.jsonl         ← the facts of the last successful run
    w17.key           ← what those facts were computed from
```

`cairn index` runs every manifest it finds there, in name order, after the SCIP ingest and
before anything derived. Nothing under `.cairn/connectors/` means nothing changes: with no
manifest, every command answers exactly as it did before connectors existed, and a test
holds that.

## The manifest

```yaml
connector: w17                                  # the namespace of every id it states
protocol: 1
command: ["w17ctl", "cairn", "facts", "--jsonl"]
inputs: ["**/*.proto"]                          # globs; a change here makes facts stale
timeout_s: 120
tier: declared
node_kinds: [domain, contract, rpc, entity, event]
edge_kinds:
  implemented_by: { propagates: dependency }
  calls_generated: { propagates: dependency }
  writes:          { propagates: dependency }
  reads:           { propagates: dependency }
  emits:           { propagates: dependency }
  consumes:        { propagates: dependency }
  part_of:         { propagates: containment }
```

Everything is checked at load and nothing is defaulted silently — an unknown field, an
unreadable tier, a `propagates` the core has no traversal for, a name with a colon in it:
all refused, with the reason named, and reported as a gap on every answer rather than on
stderr during an index nobody was watching.

`propagates` is the only thing the core reads about a kind:

| value | what the core does with it |
|---|---|
| `dependency` | A change to the target affects the source. Blast radius, `affects`, `reaches` and `path` traverse it exactly as they traverse a call. |
| `containment` | Grouping. `--within <node>` scopes an answer to what a container holds; a change to one member never reaches its siblings through it. |
| `none` | Display only. |

`emits` means nothing here. That a change to the event reaches the emitter is the whole of
what cairn needs to know, and `propagates: dependency` is how you say it.

## The protocol

cairn runs `command` with the repository root as the working directory, empty stdin, and
two environment variables:

```
CAIRN_PROTOCOL=1
CAIRN_REPO=/abs/path/to/repo
```

stdout is JSON lines, one fact per line:

```json
{"node":{"id":"w17:domain/orders","kind":"domain","name":"orders"}}
{"node":{"id":"w17:rpc/orders.OrderService.Create","kind":"rpc","anchor":"proto/orders.proto:14"}}
{"edge":{"from":"w17:rpc/orders.OrderService.Create","to":"w17:domain/orders","kind":"part_of"}}
{"edge":{"from":"w17:rpc/orders.OrderService.Create","to":"scip:<scip symbol>","kind":"implemented_by"}}
{"unknown":"rpc Refund: no handler found"}
```

* **Node ids are namespaced** by the connector's name. `w17:domain/orders` is how a caller
  addresses the node — `cairn affects w17:domain/orders`, `cairn graph w17:rpc/...` — and
  the part after the colon is what the answer prints.
* **`anchor`** is `file:line` in the repository. It becomes the node's definition, so an
  answer can point at the declaration instead of saying it has none.
* **Code is referenced by its full SCIP symbol string**, prefixed `scip:`. Not by a handle:
  a handle is short because it is meant to be typed and is only stable while the index that
  minted it is. The SCIP string is position-independent and survives a rebuild (D4), and
  `cairn <command> --format json` prints it for every symbol in an answer, which is where
  a connector gets it from.
* **`unknown`** is what the connector could not determine, in its own words. It is carried
  on *every* answer cairn gives afterwards, prefixed with the connector's name. This is
  the most useful line a connector can print: a gap that only shows up at index time is a
  gap nobody reads.

### What happens when a connector is wrong

Nothing is dropped silently. Every one of these is reported on every answer:

| what | what cairn does |
|---|---|
| a malformed line | rejects that line, names it by number, keeps the rest of the run |
| a node or edge kind not in the manifest | the same |
| an id that is neither `<connector>:...` nor `scip:...` | the same |
| a line setting more than one of node/edge/unknown | the same — what it states is ambiguous |
| an edge naming a node this run never declared | the edge is dropped and counted |
| a `scip:` reference this index does not have | **the edge is kept**, its target is *expected* rather than resolved (§7.2), and the reference is named |
| a non-zero exit, or a timeout | no fact from this run is used; the previous run's cached facts stay in place and every answer says they may be stale |

A connector failing never fails `cairn index`. The rest of the answer is still worth
having, and what is missing is stated rather than left to be inferred from an absence.

### Determinism, and no network

A connector must be deterministic over its inputs and must not touch the network (D15).
`cairn index` runs offline with no key, and a connector runs inside that promise. Inside
cairn's own Docker setup this is enforced by the container; outside it, it is a
requirement of the connector, and the connector's own determinism test is what holds it:
two runs over unchanged inputs must produce the same facts.

That is also what makes the cache meaningful. cairn reruns a connector only when its
**cache key** changes, and the key is

```
blake3(the manifest as written, the connector's --version output, the content of every file its inputs match)
```

`--version` is the whole command with `--version` appended. A connector that does not
understand the flag is not broken; it just contributes nothing to the key.

## While you are working

A running daemon maps every changed path through the `inputs` globs and reruns the
connectors that match — only those. A query made during a rerun does not wait for it
(D7): it answers from the previous run's facts and says so.

```
stale: connector w17 has inputs that changed since its facts were built, so anything
       declared here is the previous run's. A refresh is in flight; nothing waits for it
```

A refresh that fails leaves the connector marked stale, because the facts in the index
are still the previous run's and nothing has replaced them.

## What it looks like once it works

```
$ cairn graph nph --aspect callers
callers of [nph] Registry.Add   depth=2 fanout=8   [L1, exact]
[nph] Registry.Add                        fn     go  srcgo/internal/station/registry.go:52-66
  [ay9] rpc/Station.Register              ??     ??  proto/telemetry.proto:7  [L1, declared by w17]
...
unknown: connector w17: rpc Station.Retire: no handler found
```

The label names the connector, because "declared" without a name leaves a reader unable to
ask the next question. `cairn status` lists every connector with its version, what it
contributed, and whether its last run failed.

## Testing one

`crates/cairn-cli/tests/fixtures/connectors/` holds a fixture connector: a shell script
that prints fixed JSONL and, on request, fails, hangs, or emits malformed lines.
`crates/cairn-cli/tests/connectors.rs` drives it through every case above. A connector
living in its own repository should do the same thing from the other side: run itself
twice and compare, which is the test cairn cannot write for you.
