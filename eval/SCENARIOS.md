# Scenario comparison: an agent with cairn against an agent with grep

Pre-registered. Everything below the protocol was written **before any arm was run**, for
the reason `RESULTS.md` gives about flattering results: a scenario set chosen after seeing
the numbers is a confirmation exercise, and so is a prediction written afterwards.

Results go in `RESULTS.md` when the runs are done. This file is not edited to match them.

## What is being measured, and why it is not tokens

**Round trips.** One inference, one tool call, one result — the loop an agent actually
pays for. A round trip costs seconds of inference; a cairn query costs about a
millisecond. Tokens are recorded too, but they are the secondary number: the acceptance
rule's "half the wall clock" is, on any agent run, almost entirely round trips.

Several tool calls issued together in one turn are **one** round trip. That is the honest
unit — they cost one inference and run concurrently — and it is the unit that stops the
grep arm being penalised for batching, which is exactly what a competent agent does.

## The acceptance rule

Unchanged from `RESULTS.md`: same answer as the baseline → at most half the round trips
*or* half the wall clock. A better answer → no more than the baseline. **A worse answer
fails at any price.**

The stated goal is stronger than the rule: **never worse, and in various cases somewhat
better.**

## Protocol

- **Arms.** `cairn` — the cairn skill loaded, the `cairn` binary available, `grep`/`rg`
  denied. `grep` — `grep`/`rg` available, cairn denied, no skill loaded. Both arms may
  read files and list directories; the restriction is on the search tool, not on reading,
  because an agent that cannot read a file is not a baseline anyone would use.
- **Corpus.** `repos/backend`, the checkout the index is built from. Each arm starts in
  the repository root with no prior context.
- **Three runs per arm**, median reported with the spread. A baseline varied 1.79× on an
  open-ended question in an earlier measurement, so a single-run delta under ~15% is
  noise.
- **Grading.** Each answer is compared against the key below and graded
  *same* / *better* / *worse*. The key is fixed here, before the runs, and was built by
  hand from both tools plus reading the code — not from whatever an arm happens to say.
- **Skill overhead.** The cairn skill is **2 391 tokens** at 3.7 chars/token
  (`skill/SKILL.md`, 8 850 chars). It is **excluded** from the per-question numbers and
  reported once, here and in the result: it is a training asymmetry, not a property of the
  tool, and it is paid once per session rather than once per question.
- **Stale index.** The index was rebuilt at schema v18 immediately before the runs.
  Scenario 9 deliberately breaks that, and says so.

## Scenarios

Six where cairn is expected to win, four where grep is. The split is deliberate: a
scenario set with no cases the tool should lose is not a measurement.

### 1 — edges (cairn expected)

> In `srcpy/domains/assistant/repository/quota.py`, I want to add a required argument to
> `get_quota_status`. Which call sites would I have to update?

**Key.** Two call sites, in `repository/auth.py` and `repository/quota.py`. Seven symbols
in the repository share the name `get_quota_status` — a handler, an API endpoint, and four
generated stubs — and none of those four is a call site of this one.

**Prediction.** cairn 3 round trips (the name is ambiguous, so the first call costs a
disambiguation), grep 4–6. Same answer both arms; the grep arm's risk is including the
generated stubs. Rule met on round trips.

### 2 — identity (cairn expected)

> Where is the `Client` model that belongs to folders, and which code writes to it?

**Key.** `srcpy/domains/assistant/rds/folder/models.py:56-60`, used at 7 sites in
`factories.py` and `repository/folder.py`.

**Prediction.** cairn 3, grep 5–7. `grep -rn '\bClient\b'` returns 227 hits across both
languages, most of them generated protobuf; the grep arm's cost is in narrowing, and its
risk is answering about a different `Client`. cairn's own `symbol Client` also floods
(15 matches, limit reached) — the difference is that each row carries a path and a kind,
so the narrowing is free.

### 3 — boundary (cairn expected)

> Which Go code ends up calling the Python `FolderServiceHandler`, and through which RPCs?

**Key.** 10 call sites across the gRPC boundary: 9 in
`srcgo/domains/assistant/grpc/resttransform/folder.go`, plus
`shareService.GetSharedObject` in `share.go`, which calls `FolderService.GetFolder`. The
tenth is the one a reader assembling this by hand misses.

**Prediction.** cairn 1–2, grep 6–10. Nothing in the Go tree names the Python class and
nothing in the Python tree names the Go one; the only join is the generated artefacts. The
grep arm has to find the proto service name, then the generated client, then its call
sites. **Predicted worse answer from the grep arm** — it misses `share.go` — which under
the rule fails at any price.

### 4 — the name changes shape across the hop (cairn expected)

> The Python endpoint `get_shared_object` — what serves it, and where does that land?

**Key.** `srcpy/domains/assistant/api/endpoints.py:943-945` calls the Go
`shareService.GetSharedObject` (`share.go:33-90`) over `assistant_fe.ShareService`, and
that handler calls `assistant_api.FolderService.GetFolder` on the Python side. The name is
`get_shared_object` on one side of each hop and `GetSharedObject` on the other.

**Prediction.** cairn 2–3, grep 5–8. A grep arm that tries both spellings gets there;
one that does not, stops at the endpoint. This is the scenario where I am least confident
of the grep arm's failure — an agent that knows protobuf conventions will try
`GetSharedObject` unprompted, and then the gap narrows to the second hop.

### 5 — sets (cairn expected)

> What under `srcpy/domains/assistant/lib/due_diligence` does production never call?

**Key.** Four symbols: `OkoliTyp` (`ortofoto.py:49-54`), `JusticeClient.__aenter__` and
`__aexit__` (`justice.py:151-155`), `source_label` (`noise.py:258-263`).

**Prediction.** cairn 1, grep 10+. The question is over a set, and grep answers it only by
enumerating every symbol in the package and searching for each. **Predicted worse answer
from the grep arm**: not wrong, incomplete, and — the part that matters — incomplete
without saying so.

### 6 — completeness (cairn expected)

> I am changing `FolderServiceHandler`. Which deployed services does that touch?

**Key.** Three: `assistant-grpc` in process, `assistant-proxy` and `assistant-api` across
two network hops (`assistant_fe.FolderService` and `assistant_fe.ShareService` into the
proxy, `assistant_api.FolderService` into grpc). Six services start nothing and run code
on demand, so nothing static can attribute to them — an answer that does not say so is
overclaiming.

**Prediction.** cairn 1, grep 8–12. The grep arm needs `compose.yaml`, the Dockerfiles and
the call graph, and cannot see the on-demand gap at all. **Predicted worse answer from the
grep arm.**

Noted because it is the honest thing to note: `affects` reported **one** service here
until a fix made on 2026-08-04, before any of these runs. Asked about a handler class
rather than one of its methods it never expanded into the class's RPCs. So this scenario
would have failed for cairn a day earlier, and the fix was not motivated by these runs —
it came out of building the answer key, which is the same shape of risk `RESULTS.md`
records for the ranking fix.

### 7 — a string literal (grep expected)

> Where is the `X-Api-Key` header set, and which client sends it?

**Key.** `srcpy/libs/kontomatik/client.py:87`, in `KontomatikClient._headers`. Seven
occurrences in total, the rest in tests.

**Prediction.** grep 2 (find the line, read around it for the enclosing function), cairn
1–2 — `cairn literal` returns the line *with* its enclosing function, so it may tie or
win. Predicted **tie**, not a cairn loss: this is the scenario where the skill's own
advice ("comments, config, string literals → grep") is most likely to be wrong, and it
is in the set to find that out.

### 8 — configuration (grep expected)

> Which services publish `MCP_SERVER_PORT`, and what is the default?

**Key.** `assistant-mcp` in `compose.yaml` publishes `${MCP_SERVER_PORT}` and passes it to
uvicorn. The default comes from the environment file, not from any indexed source.

**Prediction.** grep 2, cairn 2–3. `cairn topology` names the service and its ports, so
the first half is one call, but the default value is not in the index at all and the cairn
arm has to read the env file anyway. Predicted **grep wins on round trips**, same answer.

### 9 — a file I just edited (grep expected, and cairn may be *worse*)

> *(A function is added to `srcpy/domains/assistant/repository/quota.py` immediately
> before the run.)* I just added `quota_headroom` to the quota repository. Is anything
> calling it yet?

**Key.** Nothing calls it; it was added seconds earlier. The edit is reverted after the
runs.

**Prediction.** grep 1, cairn 1–2. This is **the one scenario where cairn can be worse for
a reason that shrinking the output cannot fix**: the index is behind the tree. The tool is
expected to say `stale:` rather than answer confidently — and the whole question is
whether it does. If cairn answers "no callers" without flagging staleness the answer is
*accidentally right and unusable*, and I will grade it **worse**, because the same shape
on a symbol that does have callers is a wrong answer with a confident face.

### 10 — nothing indexed for it (grep expected)

> Which SQL under `tools/sql/geoplatform/estate_ranking` produces the ranked results, and
> what does `04_rank_distance_inversions.sql` measure?

**Key.** `01_ranked_results_single_criterion.sql` produces the ranking;
`04_rank_distance_inversions.sql` is read to answer the second half. No SQL is indexed —
cairn covers Python and Go only.

**Prediction.** grep 2, cairn 3 — one wasted call establishing that nothing is indexed,
then the same work. The cairn arm's cost here is the wasted call **plus** the 2 391-token
skill it loaded to no purpose, which is the honest way to state the overhead even though
it is excluded from the per-question number.

## What would falsify the case for the tool

- Any scenario where the cairn arm's answer is **worse** and the cause is not staleness.
- Scenario 9 answering confidently without `stale:`.
- The grep arm matching cairn's round trips on 3, 5 or 6 — those are the set-shaped and
  boundary questions the tool exists for, and if a competent grep agent gets there in
  comparable turns, the tool's case rests on 1, 2 and 4 alone.
- A cairn arm that spends its saved round trips on extra queries. The earlier measurement
  (task E) found exactly this: 39 tool calls and a third more tokens than working with no
  tool at all, across three rounds in which the tool got better and the run got dearer.
  Round trips are counted per run, not per query, so this is visible here rather than
  hidden.


---

# Round two: `cairn for` — pre-registered 2026-08-04

Written before any run of the new command. The grep arm is **unchanged**, so its 74 round
trips from the full protocol stand as the baseline and only the cairn arm is re-run.

## What changed in the tool

`cairn for <purpose> <subject>` — purpose first, mechanism chosen for you, every block of
the answer naming the command behind it. Two purposes built:

- `for change <symbol>` — call sites at depth 1 (tests included, which `usage` drops) plus
  the deployed radius, in one call.
- `for find "<text>"` — substring search over the **working tree**, so it covers every
  file type and is never stale, with per-hit attribution: enclosing function and handle,
  markdown section and range, and a per-query line naming the services whose start command
  or ports carry the text.

The skill now leads with `for`; the mechanism commands moved to a second section. The
instruction "config, environment variables, ports → grep" was removed, because it is now
false.

## Predictions

Against the full protocol's cairn medians and the same grep medians.

| # | class | grep | cairn was | predicted | why |
|---|---|---|---|---|---|
| 8 | which service publishes a port | 4 | 11 | **1-2** | one `for find` carries the publishing service, the default, and the prod/staging absence |
| 9 | a file I just edited | 3 | 9 | **1-2** | the tree is the truth; staleness cannot arise |
| 10 | nothing indexed for it | 5 | 4 | **3-4** | `for find` locates the scripts, but the answer still needs the files read |
| 1 | who calls this | 4 | 6 | **3-4** | `for change` fuses the three calls that ran separately |
| 7 | a string literal | 4 | 2 | **2** | already won; `for find` should hold it, with wider coverage |
| 6 | which services a change touches | 15 | 1 | **1** | unchanged path |
| 3 | Go into a Python handler | 8 | 1 | **1** | unchanged path |
| 5 | what production never calls | 14 | 5 | **5** | unchanged path |
| 2 | the right `Client` | 7 | 9 | **9** | no purpose built for disambiguation yet |
| 4 | where a chain lands | 10 | 11 | **11** | no `trace` purpose built yet |
| | **sum of medians** | **74** | **59** | **~40** | ratio ~0.54 |

**The honest health warning on that table:** my scenario-level predictions have been wrong
in *direction* on 4 of 6 in the first pre-registration, and the "stop rule will save round
trips" prediction was wrong in a way that cost answer quality. Treat every number above as
a claim to be falsified, not an estimate to be confirmed.

## What would falsify the design

- **`for find` costing more than the `literal` it replaces on scenario 7.** Wider coverage
  that costs turns is a worse tool, not a broader one.
- **Any answer getting worse.** Scenario 10's grep runs found a `SUM`/`MIN` divergence
  between the SQL scripts and the Go handler; if `for find` makes the cairn arm stop at
  the SQL and never reach the handler, the cheaper answer is the wrong trade.
- **The redirect firing on a real symbol.** `looks_like_text` is crude by design; a
  hyphen or an ALL_CAPS name that is genuinely a symbol would be refused work it can do.
  The unit tests pin the shapes seen so far, and a false positive in a run is a defect.
- **Agents ignoring the second tier.** If a run takes the `for` answer and never drops to
  a mechanism when the shape is wrong, the ladder is decoration and the fused answer is
  hiding the gaps its blocks used to state.
- **The attribution being wrong rather than absent.** Only 25 616 of 66 296 definitions
  carry a body extent, so a hit inside a function without one is attributed to the
  enclosing class. That is a silent downgrade — the row says "in KontomatikClient" and
  means "somewhere in it" — and it is the next thing to fix if a run trips on it.

## Not built, and named so the absence is not read as a finding

`for understand` (the chain question, scenario 4's 7-17 spread), `for audit`, and
`for document`. The last has no measurement behind it at all: no scenario in this file
exercises documenting, so nothing here says what it would need.


---

# Round three: the skill, not the tool — pre-registered 2026-08-04

**No code changed.** The binary is byte-identical to round two. Only `skill/SKILL.md` was
edited, so anything that moves is caused by the instruction sheet.

## What changed, and why this is a bundle rather than one variable

Round two's diagnosis was that moving *Stop when you have the answer* down the page cost
scenarios 5 and 7 their wins. The clean experiment would change position alone. It does
not — four things changed together, and the result cannot attribute the effect to any one
of them:

1. **Position.** The stop rule is now section 2, above everything except `for`. In round
   one it was section 3; in round two, section 4 behind 1 700 more characters.
2. **Consolidation.** "Reach for it when" and "Do not, and save the round trip" were two
   sections saying overlapping things; they are now one list, "Which one to reach for".
3. **One new line**, from reading the scenario 7 log: *a handle in the output is for your
   next question, not for re-asking this one.* Three of the extra turns in that scenario
   were the arm querying the handle `for find` had just handed it.
4. **Length.** 13 907 → 12 069 characters, closer to the grep arm's 10 002 than round two
   was.

## Predictions

| # | grep | round 2 | predicted | why |
|---|---|---|---|---|
| 7 | 4 | 4 | **2** | the round-one number, if position was the cause |
| 5 | 14 | 8 | **5** | same |
| 8 | 4 | 3 | 3 | unaffected |
| 9 | 3 | 4 | 4 | unaffected |
| 10 | 5 | 3 | 3 | unaffected |
| 1 | 4 | 5 | 5 | unaffected |
| 2 | 7 | 9 | 8 | may tighten slightly |
| 3 | 8 | 1 | 1 | already minimal |
| 4 | 10 | 10 | 10 | depth, not repetition — must *not* drop |
| 6 | 15 | 1 | 1 | already minimal |
| | **74** | **48** | **~42** | ratio ~0.57 |

## What would falsify the diagnosis

- **Scenarios 5 and 7 not recovering.** Then position was not the cause and round two's
  explanation was wrong — the regression would have to be run-to-run variance, which
  would also mean round one's 2 and 5 were flattering single draws.
- **Scenario 4 dropping below 8.** That question needs depth; a stop rule that shortens it
  is buying round trips with answer quality, which is the failure the depth carve-out was
  written for. Answers get graded, not just counted.
- **Everything improving by a similar margin.** That would point at length rather than
  position, and would say the skill is simply too long — a different fix.


---

# Round four: five root causes, read from the round-three logs — pre-registered 2026-08-04

Round three's runs were unusually uniform — three identical traces per scenario on both
losses — so the causes are readable rather than inferred. Each fix below names the turns it
is meant to remove.

| # | root cause | evidence | fix |
|---|---|---|---|
| 1 | **`reaches --outgoing` returned zero** for any function that *uses* a generated client. `service_links` binds the client *type*, not the call site, so the outgoing direction only ever answered for the artefact itself. | agents reached for it in three separate rounds, always on the chain question, always got 0, and rebuilt the chain by hand | `rpc_targets`: this symbol's calls → the client member → the service → the handler serving it. 3 handlers where it had 0, in 17 ms |
| 2 | **`for change` on a symbol the index does not hold dead-ended.** `subject` bails on a miss, so the redirect written for it could never run. | s09 turns 1-2, identical in all three runs: `for change` fails, then `symbol` + `status` to find out why | resolve inline; on a miss, search the tree, say the graph cannot answer and why, and return the find result |
| 3 | **`for find` gave the line without its surroundings**, so the arm opened files to see what the line was part of. | s08 read three compose files after the answer; s07 read `client.py` in all three runs | ±2 lines per hit, dropped above 30 hits — the same "spend it where it fits" rule `refs --context auto` uses |
| 4 | an ambiguous name costs a whole turn (`for change get_quota_status` → exit 2 → `for change wes`) | s01 turn 1, all three runs | **not fixed** — see below |
| 5 | `for change` is under-assembled: no call-site source, does not follow the module-level `a = wrap(f)` binding one hop, does not state the dynamic-dispatch check | s01 turns 3-5, all three runs: `refs wes`, `refs np5`, `weaklinks wes` | **not fixed** |

Causes 4 and 5 are left alone deliberately. Both are in scenario 1, both would need
judgement calls about what to fold into an assembled answer, and fixing them in the same
round as three others would leave five changes and no way to attribute the result. They are
the next round's work.

## Predictions

| # | grep | r3 | predicted | why |
|---|---|---|---|---|
| 4 | 10 | 9 | **4** | the chain is `for change`/`reaches --outgoing` per hop; the direction that returned zero now answers |
| 8 | 4 | 3 | **1** | the hits now carry the `ports:` line above them |
| 7 | 4 | 3 | **1-2** | the enclosing function was already there; the surrounding lines are what sent it to the file |
| 9 | 3 | 4 | **2** | one turn saved by the redirect, not two: the arm still reads the file it was told about |
| 1 | 4 | 5 | 5 | causes 4 and 5 untouched |
| 2 | 7 | 6 | 6 | unaffected |
| 3 | 8 | 1 | 1 | unaffected |
| 5 | 14 | 7 | 7 | unaffected |
| 6 | 15 | 1 | 1 | unaffected |
| 10 | 5 | 3 | 3 | unaffected |
| | **74** | 42 | **~31** | ratio ~0.42 |

## What would falsify it

- **Scenario 4 not moving.** Then the chain's cost was never the missing direction, and
  three rounds of agents reaching for `--outgoing` were reaching for the wrong thing.
- **Any answer getting worse**, particularly scenario 4: `--outgoing` is convention-matched,
  so a wrong handler in that list is worse than no list. The rows are graded, not counted.
- **Scenario 8 or 7 not moving.** Then the file reads were not about surrounding context and
  the diagnosis of causes 3 was wrong.
- **A new latency regression.** The first form of `rpc_targets` took 11.8 s and the sweep
  caught it; the rewritten form is 17 ms. If the sweep fails again, the fix is not free and
  the trade has to be stated rather than assumed.


---

# Round five: the last two causes — pre-registered 2026-08-05

Both live in scenario 1, the only scenario still lost, and both were left out of round four
on purpose so that round's result could be attributed. Fixed now:

**Cause 4 — a shared name cost a whole round trip.** `for change get_quota_status` listed
seven candidates and exited 2; the arm re-ran with `for change wes`. Identical in every run
of every round. It now answers for the most-referenced candidate that is not generated,
prints that choice and the alternatives on stderr, and drops generated definitions as a
judgement the *intent* licenses — nobody hand-edits a protobuf stub, so it cannot be what
"I am going to modify this" means. For this name that removes four of seven candidates and
the remaining ranking is 5 / 2 / 0 references, which picks the repository function.

**Cause 5 — `for change` was under-assembled.** Round three's trace, identical in all three
runs: `for change`, then `refs` for source at the sites, then `refs` again on the async
wrapper, then `weaklinks`. Those are not four questions. It now returns call sites *with
their source*, follows the module-level `a = wrap(f)` binding one hop to the callers that
actually break, gives the deployed radius, and states the dynamic-dispatch check in one
line instead of leaving it to be asked.

## Predictions

| # | grep | r4 | predicted | why |
|---|---|---|---|---|
| 1 | 4 | 6 | **2** | one call now carries what the five turns assembled |
| 2 | 7 | 5 | 4-5 | shares the ambiguity path (`Client` names many); may gain a turn |
| 9 | 3 | 1 | 1 | unchanged |
| 8 | 4 | 1 | 1 | unchanged |
| 6 | 15 | 1 | 1 | unchanged |
| 3 | 8 | 1 | 1 | unchanged |
| 5 | 14 | 3 | 3 | unchanged |
| 10 | 5 | 3 | 3 | unchanged |
| 7 | 4 | 3 | 3 | unchanged |
| 4 | 10 | 9 | 9 | the first hop is still an attribute call the index cannot resolve |
| | **74** | 33 | **~28** | ratio ~0.38 |

Predicting "unchanged" for six scenarios is a claim, not a hedge: round four predicted the
same for scenario 5 and it moved from 7 to 3. If several move again, the spread is larger
than four rounds of three-run medians can see, and that is the finding rather than any
ratio.

## What would falsify it

- **Scenario 1 not reaching 3 or better.** Then the five turns were not the four missing
  blocks, and the assembly theory is wrong for the one scenario built to test it.
- **The ranked choice being wrong on any run.** `for change` now picks. If an arm answers
  about the handler or the endpoint when the question named the repository file, the
  ambiguity fix has bought round trips with correctness, which fails at any price.
- **Answers getting longer without getting better.** Four blocks in one call is more output
  per turn; if the arms start reading past it or missing what is in it, the assembly is
  cheaper to run and dearer to use.
