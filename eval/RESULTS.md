# Measured results

Numbers, with what they do and do not support. A withdrawn result stays here rather than
being deleted — the point of writing them down is that a later run can contradict an
earlier one, and that only works if the earlier one is still readable.

**Acceptance rule.** Same answer as the baseline → at most half the tokens *or* half the
wall clock. A better answer → no more than the baseline. A worse answer fails at any
price.

---

## An agent with cairn against an agent with grep — 2026-08-04 (pilot, one run per arm)

Pre-registered in `eval/SCENARIOS.md`: ten scenarios, answer keys and a predicted
round-trip count per arm, all written before any arm was run. Nothing below was chosen
after seeing a number.

**Metric.** Round trips — one inference, one tool call, one result. Several calls issued
together in one turn count as one, because they cost one inference. Reconstructed from a
`PreToolUse` hook log by timing (`eval/hook_log.py`, `eval/cluster.py`): within-turn gaps
never exceeded 0.97 s, between-turn gaps were 1.8-2.6 s, and the threshold sits at 1.0 s
in the empty band between. Wall clock is first to last logged call, so it includes
inference.

**Arms.** `cairn`: the skill loaded, the binary on PATH, all content search denied.
`grep`: `grep`/`rg`/the Grep tool, no cairn, no skill. Both could read files and list
directories. Compliance was audited from the log afterwards — no violation in either arm
in any of the 20 runs.

### Result

| # | class | expected | cairn | grep | turns | wall |
|---|---|---|---|---|---|---|
| 1 | edges — who calls this | cairn | 8 | 7 | 1.14 | 1.26 |
| 2 | identity — the right `Client` | cairn | 15 | 11 | 1.36 | 0.95 |
| 3 | boundary — Go into a Python handler | cairn | **6** | 16 | **0.38** | 0.69 |
| 4 | the name changes shape across the hop | cairn | 16 | 9 | 1.78 | 1.99 |
| 5 | sets — what production never calls | cairn | **9** | 18 | **0.50** | 0.47 |
| 6 | completeness — which services a change touches | cairn | **4** | 24 | **0.17** | 0.11 |
| 7 | a string literal | grep | 7 | 4 | 1.75 | 1.35 |
| 8 | configuration | grep | 17 | 3 | 5.67 | 9.52 |
| 9 | a file I just edited | grep | 8 | 5 | 1.60 | 2.42 |
| 10 | nothing indexed for it | grep | 7 | 3 | 2.33 | 3.25 |
| | **total** | | **97** | **100** | **0.97** | 0.93 |

Ratios are cairn ÷ grep; below 1 is cairn cheaper. Medians are 8 turns for both arms.

**Answer grading against the pre-registered keys: cairn was never worse.** Same answer in
8 scenarios, better in 1 (scenario 5, where it also named `OkoliTyp` and the
`__aenter__`/`__aexit__` pair that static reachability reports and grep's enumeration
missed), worse in 0. The grep arm was marginally better in scenario 2, finding a mass
delete in a data migration that cascades to the model — a write the index cannot see
because the migration names the parent, not the child.

### What this supports

**On the three questions the tool is built for, it wins by a lot.** Scenario 6 — the whole
blast radius of a handler class — cost 4 round trips and 15 seconds against 24 and 133.
Scenario 3, the cross-language boundary, 6 against 16. Scenario 5, a set question, 9
against 18. All three meet the acceptance rule on round trips and on wall clock, with
room to spare.

**The answers held up.** Ten scenarios, no case where the cairn arm's answer was worse
than the baseline's. That half of the stated goal is met in this pilot.

### What it does not

**Over the ten scenarios together the tool is a wash: 97 round trips against 100, medians
identical.** Three large wins are paid for by seven losses, four of them substantial. The
per-scenario spread runs from 0.17× to 5.67×, so an aggregate over a scenario mix is
mostly a statement about the mix.

**"Never worse" fails on cost.** The cairn arm spent more round trips in 7 of 10
scenarios. Scenario 8 (which service publishes a port, what is the default) is the worst:
17 against 3, and 75 seconds against 8, because with content search denied the arm read
every compose file in the repository by hand. That is partly an artefact of the arm
design — a real agent has both tools — but the skill tells it to prefer cairn, and the
question is one the skill's own "not for config" line should have deflected in one call.

**Four of the six scenarios predicted as cairn wins were losses.** Scenarios 1, 2, 4 went
to grep. The prediction was wrong in a consistent direction: the cairn arm keeps querying
after it has the answer. Scenario 4 is the clearest — 16 round trips to trace a chain that
`path` and `reaches` answer, because the FastAPI endpoint has no static caller, so
`affects`, `runs` and `entrypoints --reaches` all returned nothing and the arm rebuilt the
chain by hand. This is task E's failure mode from the earlier measurement, unfixed: the
tool got better and the run did not get cheaper.

**Single runs.** One run per arm, as agreed for a pilot. An earlier baseline varied 1.79×
on an open-ended question, so scenario 1 (1.14×) and scenario 2 (1.36×) say nothing on
their own, and even scenario 5 (0.50×) sits close enough to the rule's boundary to need
the three-run protocol before it is quoted. Scenarios 6, 8 and 10 are outside any
plausible noise band.

**The instruction sheets are not the same size.** The cairn arm reads 10 106 characters of
preamble, the grep arm 1 106. The skill's 2 391 tokens are excluded from the per-question
numbers as a training asymmetry, but a longer instruction sheet plausibly encourages more
tool use, and this pilot cannot separate that from the tool's own effect. For the full
protocol the grep arm should get a preamble of comparable length, or the cairn arm a
shorter one.

**Both arms were told not to stop early.** "An incomplete answer counts against this arm
harder than an extra command does" is in both preambles. It is the right instruction for
grading answers and it inflates round trips in both arms; the comparison survives it, the
absolute numbers do not transfer to an agent under time pressure.

### Three defects the runs found

None was reported; all three came out of building the answer keys or reading a run.

1. **`affects` on a handler class reported one deployed service where its methods reported
   three.** The hop walk goes inward through callers and a type's own methods are not
   among them, so the class expanded into none of its RPCs. `reaches` had had this fix
   since task D; `affects`, the command whose stated purpose is to be the complete answer,
   had not. Fixed before scenario 6 ran, so that scenario measures the fixed tool — and
   would have failed a day earlier.
2. **A hop row named the wrong file.** Rows were grouped without the call site's path and
   kept whichever came first, printing `in folder.go` for a route that lives in
   `share.go`. A row of that answer is a place to go and look.
3. **`usage` silently dropped test files while reporting `suppressed: none`.** Found by
   the scenario 1 cairn arm, which cross-checked against `graph --aspect callers`, got
   four call sites against `usage`'s two, and said so in its answer. The filter is the
   right default; claiming nothing was filtered is not. Now stated in the envelope, with
   the `--include-tests` command to see them.

The tool was frozen after fix 3 and before scenario 2, so scenarios 2-10 all ran against
one build. Scenario 1's cairn arm was re-run on the fixed build; its pre-fix run is kept
as `runs/DISCARDED-s01-cairn-prefix-usage-bug.jsonl`.

### One more defect, not yet fixed

`cairn status` reports the daemon's view as `590 created ... reindex due: too many files
differ`, in a tree where `cairn verify --repo .` correctly finds exactly one changed file.
The watcher counts files that were never in the index — docs, YAML, proto — as newly
created, so it advises a 37-second reindex on a clean tree. Recorded during scenario 9 and
left alone to keep the build frozen for the run.

---

## What the pilot's findings were worth — 2026-08-04, after the fixes

Three changes, then a re-measurement of the two worst cairn losses. **Single runs after a
fix, not a second pilot.** The grep numbers they are compared against are the pilot's,
which is fair because nothing in the grep arm changed.

**The changes.**

1. **The watcher's false staleness is fixed.** `walk_new` reported every file absent from
   the index as newly created, and only Python and Go are ever in it. On the same clean
   tree the daemon now says **17 created** instead of 590, and `reindex due` is gone — 1700
   Python and Go files on disk against 1683 indexed, so the 17 are real.
2. **`affects` and `runs` no longer answer a confident zero for a framework route
   handler.** Both said `0 deployed service(s) — (no service entrypoint reaches it)` about
   a live public endpoint, because FastAPI registers routes by decorator and no call
   reaches them. Both now fall back to the file the symbol sits in and label the weaker
   claim: `runs` prints `[L1 + L0-D, via the file, not a call path]` in the header rather
   than only in a note, and `affects` marks the service `~`. The `exact` label is now
   earned by the direct answer alone.
3. **The skill gained a "stop when you have the answer" section**, and a line sending
   environment variables, ports and compose files to grep.

### Re-measurement

| | pilot cairn | after | grep (pilot) |
|---|---|---|---|
| 4 — where a chain lands | 16 | **16** (v3) / 4 (v2) | 9 |
| 8 — which service publishes a port | 17 | 11 | 3 |

**The scenario 4 result is the one that matters, and it is not the flattering one.**

- **v2**, with the stop rule as first written: **4 round trips**, down from 16. It also
  stopped at the first hop and never followed the handler's branch, so it missed the
  estate/folder fan-out that both the pilot run and the grep arm reported. **A worse
  answer, which under the rule fails at any price.** The saving was not a better route to
  the answer; it was less answer.
- **v3**, after adding the carve-out that the rule is about repetition and not depth:
  **16 round trips again**, and the most complete answer any arm gave — it found a
  second-order hop (`FolderTransformer` calling `ListEstates`) that neither the pilot nor
  grep reached, and a nil `principal` passed at `share.go:78` that would panic the proxy
  on any shared folder holding an estate.

So, stated plainly: **once completeness is held fixed, the stop rule buys nothing on this
question.** The pilot's diagnosis — "the arm keeps querying after it has the answer" — was
wrong for scenario 4. It was not re-querying; the chain really is that long, and 16 turns
is what following it costs. What the earlier run wasted was smaller than it looked.

Scenario 8 improved from 17 to 11 and is still 3.7× grep. It cannot get much better in
this arm: the question is answered by reading compose and `.env` files, the arm is denied
content search, so it enumerates them by filename and reads each in full. The skill's new
"config → grep" line cannot help an arm that has no grep. **That gap is mostly an artefact
of the arm design**, and the honest reading is that a real agent — which has both tools —
would grep and spend 3.

### What this does and does not change

Unchanged: fixes 1 and 2 remove two confident-and-wrong outputs, which is the failure
class this project exists to prevent, and neither was measured for speed because neither
was about speed.

Changed: the pilot's main causal claim is now doubtful. "The cairn arm spends its saved
round trips on extra queries" survives as a description of scenarios 1 and 2 at best; on
scenario 4 the extra turns were the work. Before the three-run protocol is worth running,
the losing scenarios need re-reading run by run to separate re-querying from depth —
counting turns cannot tell them apart, and this pair of runs shows what happens when the
distinction is guessed at.

---

## Repetition against depth: every cairn turn classified — 2026-08-04

The separation the section above says is needed, done from `eval/runs/*.jsonl` by grouping
calls into turns and reading what each turn asked. Categories:

- **resolve** — orientation and getting a handle. Almost always batched into turn 1, so it
  costs about one turn per run and is not where the money goes.
- **answer** — the turn that produced the answer.
- **depth** — a *different* subject: the next hop of a chain, another symbol, a file read
  to confirm. Legitimate; the question was that big.
- **repetition** — the *same* question asked a second way, or another attempt after the
  tool has already reported a miss.
- **off-question** — a command nobody asked for.
- **tool-gap** — a turn spent because the tool answered wrongly or emptily, since fixed.

| # | turns | resolve | answer | depth | repetition | off-question | tool-gap |
|---|---|---|---|---|---|---|---|
| 1 | 8 | 1 | 1 | 3 | **3** | 1 | (1) |
| 2 | 15 | 1 | 1 | 10 | **1** | **2** | – |
| 3 | 6 | 1 | 1 | 4 | – | – | – |
| 4 | 16 | 1 | 1 | 11 | – | – | **3** |
| 5 | 9 | 1 | 1 | 7 | – | – | – |
| 6 | 4 | 1 | 1 | 2 | – | – | – |
| 7 | 7 | 1 | 1 | 3 | **2** | – | – |
| 8 | 17 | 1 | – | 11 | **5** | – | – |
| 9 | 8 | 1 | 1 | 5 | **1** | – | – |
| 10 | 7 | 1 | 1 | 5 | – | – | – |
| | **97** | 10 | 9 | 61 | **12** | **3** | **4** |

**About 19 of 97 turns — a fifth — is removable. Four fifths is depth.** Remove every
identified waste turn and the arm reads 78 against grep's 100: a real edge, and nothing
like the acceptance rule's half.

### The three shapes

**Scenario 7 is the clean case of the pilot's original claim.** `cairn literal "X-Api-Key"`
returned the line, the enclosing function and its handle **in turn 1**, complete. The
remaining six turns were elaboration — `outline`, `usage`, `refs`, then the same question
twice more as `literal "KONTOMATIK_API_KEY"` and `symbol KONTOMATIK_API_KEY`. The tool won
the question and the run lost it. Grep took 4.

**Scenario 8 is repetition of a different kind: probing a stated miss.** `literal
"MCP_SERVER_PORT"` missed, because `literal` covers Python and Go source only and the
answer lives in compose and `.env`. The arm then tried `literal "MCP_SERVER"`, `literal
"MCP"`, `literal "mcp"`, `symbol mcp_server_port`, `symbol MCP_PORT`, `docs --about`, and
two forms of `usage` — five turns spread across the run, each re-asking a tool that had
already said it does not cover this. The skill's "a miss reports what *is* indexed, that
is not an invitation to try three more spellings" is aimed exactly here.

**Scenario 4 was never repetition.** Eleven of its sixteen turns were distinct hops of a
three-service chain with a fan-out; three were spent on `affects`, `runs` and
`entrypoints --reaches` all returning confident zeros for a framework route handler, which
is fixed. Nothing in it asked the same question twice.

### What each arm's cost actually scales with

Reading the two extremes side by side answers this better than the totals do.

- **Grep's cost scales with how scattered the answer is.** Scenario 6 took it 24 turns:
  it found the handler in two, then rebuilt the deployment graph from proto files,
  `clients.py`, the Go transform layer, `cmd/` directories, the MCP package, nginx configs
  and deploy workflows — eight places, none of which names the others.
- **Cairn's cost scales with how deep the chain is**, plus a floor of about one turn to
  resolve a handle before it can answer anything.
- **So cairn wins exactly where one command is the whole answer** — `affects` (scenario 6,
  4 turns), `reaches` (3, 6 turns), `unreached` (5, 9 turns). All three are set-shaped.
- **And loses where the answer is one grep away.** Scenario 8's entire answer came out of
  a single `grep -rn MCP_SERVER_PORT .` — every occurrence, every file type, three turns
  including the confirming reads.

The honest summary of the tool's position: it is not a general replacement and the
aggregate was never going to show one. It converts a scattered question into one command,
and it charges a handle-resolution turn for the privilege. Whether that is worth it is
decided per question, and the skill is the only place that decision can be made.

---

## The full protocol: 3 runs per arm, 60 runs — 2026-08-04

Same ten scenarios and the same pre-registered keys. What changed since the pilot:

1. **Both arms now get a real tool guide of comparable length.** The pilot's confound was
   10 505 characters against 1 106; the grep arm's guide was written up to 10 002 against
   cairn's 12 241 (1.22×). It is a genuine ripgrep guide — search shapes, narrowing,
   generated-code exclusion, import aliases, tracing a chain by hand, registration-based
   wiring — and it carries the same stop-rule the cairn skill has. A weak baseline guide
   would have been a subtler way of handicapping the baseline than no guide at all.
2. **The three fixes** — the `usage` test-filter disclosure, `affects`/`runs` on framework
   route handlers, and the skill's stop rule with its depth carve-out.
3. **Runs are parallel**, keyed by the `agent_id` the hook payload carries. Turn
   reconstruction is unaffected: within-turn gaps stayed ≤ 0.99 s and the result is flat
   across thresholds from 0.8 s to 2.0 s (ratio 0.81 / 0.80 / 0.78 / 0.78). Wall clock is
   *not* reported — under concurrency it measures the machine, not the arm. Round trips
   carry the wall-clock claim, which is the argument the metric was chosen on.

### Result — round trips, median of 3

| # | class | cairn | grep | ratio | pilot ratio |
|---|---|---|---|---|---|
| 6 | which services a change touches | **1** (1,1,1) | 15 (15,15,15) | **0.07** | 0.17 |
| 3 | Go into a Python handler | **1** (1,1,2) | 8 (7,8,11) | **0.12** | 0.38 |
| 5 | what production never calls | **5** (3,5,8) | 14 (11,14,18) | **0.36** | 0.50 |
| 7 | a string literal | **2** (2,2,3) | 4 (4,4,4) | **0.50** | 1.75 |
| 10 | nothing indexed for it | 4 (4,4,4) | 5 (4,5,8) | 0.80 | 2.33 |
| 4 | where a chain lands | 11 (7,11,17) | 10 (9,10,11) | 1.10 | 1.78 |
| 2 | the right `Client` | 9 (8,9,9) | 7 (6,7,9) | 1.29 | 1.36 |
| 1 | who calls this | 6 (5,6,7) | 4 (4,4,5) | 1.50 | 1.14 |
| 8 | which service publishes a port | 11 (9,11,21) | 4 (4,4,5) | 2.75 | 5.67 |
| 9 | a file I just edited | 9 (8,9,11) | 3 (3,3,3) | 3.00 | 1.60 |
| | **sum of medians** | **59** | **74** | **0.80** | 0.97 |

**Both arms got faster; cairn got faster by more.** Against the pilot the grep arm went
from 100 round trips to 74 (−26%) on the strength of its new guide alone, and the cairn arm
from 97 to 59 (−39%). The ratio moved from 0.97 — a dead heat — to **0.80**.

**The acceptance rule is met on 4 of 10 scenarios** (≤ half the round trips at equal
answer): 6, 3, 5, and 7 exactly at 0.50. In the pilot it was 3.

### What the fixes bought, scenario by scenario

- **Scenario 6: 4 → 1 round trip, all three runs identical.** `cairn affects` answers it,
  and the skill now says to stop there. Against 15 for grep.
- **Scenario 7 reversed, 7 → 2, from a 1.75 loss to a 0.50 win.** This was the pilot's
  clearest case of a tool that had already won being made to lose by elaboration. The
  answer arrived in turn 1 both times; the difference is that the arm now stops.
- **Scenario 3: 6 → 1.** Two of three runs answered in a single command.
- **Scenario 10: 7 → 4**, from a loss to a marginal win — the arm stops probing an index
  that has already said it holds no SQL.
- **Scenario 4: 16 → 11**, and the variance is the story: 7, 11, 17. The `affects`/`runs`
  fix removes the confident zeros, but how far an arm walks a three-hop chain with a
  fan-out is a judgement, and the three runs made it differently.

### Where it still loses, and why each is different

- **Scenario 9 (3.00) is the honest loss.** The index is behind the tree by construction,
  so the arm spends turns establishing *what the index can still vouch for* — `verify
  --repo` for the changed-file set, then reading the one dirty file. Grep needs one search.
  This is the case flagged in the pre-registration as unfixable by shrinking output, and it
  is unfixable: an index cannot answer about a file it has not read.
- **Scenario 8 (2.75) is mostly the arm design.** Denied content search, the arm reads
  every compose and `.env` file by name. A real agent greps and spends 4.
- **Scenarios 1 and 2 (1.50, 1.29) are depth**, per the turn-by-turn classification above:
  a signature change over an async wrapper, and an MTI model with three write paths.
- **Scenario 4 (1.10) is now a tie inside the run-to-run spread.**

### Answer quality

Graded against the pre-registered keys. **Equivalent on 9 of 10.** The exception is
scenario 10, and it goes against cairn: two of three grep runs found that the SQL scripts
`SUM` the decay over every in-radius POI while the Go handler takes `MIN` over the nearest
one, and that the scripts read table `poi` where the handler reads `poi_poi` — a real
divergence between a script and the production code it claims to mirror. No cairn run
found it, because none went looking at the Go handler. Cairn was 0.80 on cost there, so
under the rule a *better* answer at no more cost wins: **grep takes scenario 10.**

Two cairn answers were better than their grep counterparts without costing more: scenario
4 run 1 found the nil `principal` at `share.go:78` that panics the proxy on a shared folder
holding an estate, and scenario 5 correctly separated `source_label` (genuinely dead) from
`OkoliTyp` and the `__aenter__`/`__aexit__` pair (static-analysis artifacts) where the grep
runs reported only the first.

### Compliance

60 runs audited from the logs. No grep-arm run invoked cairn. One cairn-arm call matched
the content-search filter: scenario 8 run 2 used `find . -name "*.env*" | grep -v "^./.git"`
— grep as a filter over a filename list, searching no file contents. The run is kept and
this is the disclosure. It matters which way the choice cuts: **keeping it flatters cairn**
— excluding that run moves scenario 8's median from 11 to 15 and its ratio from 2.75 to
3.75, and the total from 0.80 to 0.85.

### What this supports, and what it does not

**Supported.** On the three set-shaped questions the tool exists for, it is 7× to 14×
cheaper in round trips than a competently-guided grep agent, at an equivalent answer, three
runs each with no overlap between the arms' ranges. Scenario 6 is 1 against 15 with zero
variance on either side.

**Supported.** Across a ten-scenario mix chosen before the numbers, the tool is a 20%
saving overall — real, and an order of magnitude smaller than its best cases, because the
mix is half questions it is not for.

**Not supported: "never worse".** Cairn costs more on five of ten scenarios, and on
scenario 10 it also answered less completely. The stated goal is not met, and the two
scenarios that fail worst (9 and 8) fail for reasons no output change fixes — a stale index
and a question about files no indexer reads.

**Not measured: tokens.** Round trips only, as pre-registered. The subagent token counts
are visible in the transcript and were not collected systematically; on the runs where
cairn wins on turns it also reads far less, but that is an impression, not a number.

**One run per arm was the wrong resolution.** Scenario 4 spans 7 to 17 round trips across
three identical prompts, and scenario 8 spans 9 to 21. The pilot's single-run figures for
both sat inside those ranges and meant nothing on their own; only scenarios 6, 3 and 7 were
tight enough that one run would have been honest.

---

## `cairn docs` against grep — 2026-08-04

Reproduce: `docker compose run --rm dev python3 eval/measure_docs.py <repo> <cairn> <db>`

**Metric.** Characters entering context, converted at 3.7 chars/token — cairn's own
constant from `cairn-fmt/src/budget.rs`, so the accounting is the tool's rather than one
invented for the occasion.

**Arms.** Two baselines, because one of them flatters the tool being tested.

| arm | what enters context |
|---|---|
| `whole` | `grep -rn` hits, then the file with most hits, read in full |
| `window` | `grep -rn` hits, then 41 lines around the first hit only |
| `cairn` | `cairn docs --about <term>` output, then its top-ranked line range |

**Queries.** Chosen by a rule, not by hand: two-word phrases occurring literally in the
text, excluding stopwords, ranked by document frequency. Reported in two bands, because
one corpus is 61% templated reports whose boilerplate wins a document-frequency ranking
outright — and a phrase in most of the corpus is not a "which document holds this?"
question, which was the stated reason for using document frequency at all. Both bands are
measured rather than one filtered out.

- **discriminating** — in ≥3 documents and at most 10% of them
- **ubiquitous** — in more than 25% of documents

### Result

| corpus | band | n | vs whole | vs window | cairn worse | rule met (window) |
|---|---|---|---|---|---|---|
| cairn's own docs, 10 files / 193k chars | discriminating | 8 | **0.12 (−88%)** | **0.89 (−11%)** | 3/8 | 1/8 |
| | ubiquitous | 8 | 0.13 (−87%) | 0.77 (−23%) | 1/8 | 3/8 |
| a private 205-file / 1.6M-char corpus | discriminating | 8 | **0.21 (−79%)** | **0.68 (−32%)** | 0/8 | 3/8 |
| | ubiquitous | 1 | 0.65 | 1.35 (+35%) | 1/1 | 0/1 |

Against the whole-file baseline the rule is met on **32/33** query-band combinations
across both corpora. Against the window baseline it is met on **7/25** in the
discriminating and ubiquitous bands combined.

Query terms for the second corpus are deliberately not reproduced here: it is not this
repository's code and its vocabulary does not belong in this repository's files. The
harness prints them when run against it.

### What this supports

Against an agent that reads a whole document to answer a question about it, `cairn docs`
costs a fifth to an eighth as much, on both corpora, in both bands.

**The larger corpus did better, not worse.** The obvious worry was that ten documents is
small enough for `grep -rn` to already answer "which file", so the win would shrink on a
real repository. It went the other way: against the hard baseline the median improved from
0.89 to 0.68 as the corpus grew twentyfold. Two corpora is not a trend, but it is the
opposite of the feared direction.

### What it does not

**The acceptance rule is not met against the window baseline.** Median −32% at best,
against a bar of −50%. The truth sits between the two baselines and this run does not say
where, because *what an agent actually reads was not measured*. Both arms are fixed
strategies, not observed behaviour.

**Answer quality was not evaluated.** The rule distinguishes "same answer" from "better
answer" and this measures neither; all that is verified is that the term appears inside
what each arm read. A case can be made that a section is the better answer — a 41-line
window can cut mid-explanation, and the first hit need not be the relevant one — and under
the rule a better answer only has to cost no more than the baseline, which every median
does meet. That is a reading, not a finding.

**Ubiquitous phrases are where it loses.** A phrase in a quarter of the corpus has no
single home, and `cairn docs` costs more than grep on several of them. The tool is for
"which document holds this", and that question has to have an answer.

**Three runs were not done.** Both arms are deterministic — three consecutive runs were
byte-identical — so a median over runs is the same number. The three-run habit exists for
agent runs, which vary. The spread reported is across queries.

### Withdrawn

Two earlier runs of this experiment are superseded. Both are listed because each was
wrong in a way worth not repeating.

**Run 1 — median 0.18 (−82%), rule met 7/10.** Had only the whole-file baseline. The
window arm was added afterwards specifically to attack the result and cut the median win
from −84% to −26%. It also exposed a real defect: for one query cairn returned
`README.md:1-233`, the whole file, and cost 40% *more* than grep. Mentions were ranked by
raw count, which systematically prefers long sections because longer text contains more of
everything — a 1900-word preamble with three mentions beat a 94-word section with one.
Ranking now uses density, tie-broken by the shorter range (`docs.rs`,
`a_long_section_does_not_win_by_being_long`). **The fix was motivated by the measurement,
which is the exact shape a flattering result takes**, so the pre-fix number stays here.

**Run 2 — cairn corpus, median 0.16 / 0.74.** Right conclusion, broken query selection.
Phrases were built by pairing adjacent *tokens*, so `srcpy/domains/assistant` produced the
query "domains assistant", which occurs nowhere as text: half the queries matched nothing
and were dropped. The replacement extracts phrases from the raw text with a real space
between the words. A second bug in the same selector matched inside words, offering "ing
product", "ring product" and "uring product" as three separate queries — three samples of
one phrase, none of them a phrase.


---

## Round two: `cairn for` — 3 runs per scenario, 2026-08-04

Cairn arm only. The grep arm is unchanged, so its 74 round trips carry over as the
baseline. Predictions were written into `SCENARIOS.md` before any of these runs.

### Result

| # | grep | round 1 | round 2 | predicted | ratio |
|---|---|---|---|---|---|
| 6 | 15 | 1 | **1** (1,1,2) | 1 | **0.07** |
| 3 | 8 | 1 | **1** (1,1,1) | 1 | **0.12** |
| 5 | 14 | 5 | 8 (7,8,9) | 5 | 0.57 |
| 10 | 5 | 4 | **3** (3,3,5) | 3-4 | 0.60 |
| 8 | 4 | 11 | **3** (3,3,4) | 1-2 | 0.75 |
| 4 | 10 | 11 | 10 (8,10,16) | 11 | 1.00 |
| 7 | 4 | 2 | 4 (2,4,5) | 2 | 1.00 |
| 1 | 4 | 6 | 5 (5,5,6) | 3-4 | 1.25 |
| 2 | 7 | 9 | 9 (6,9,12) | 9 | 1.29 |
| 9 | 3 | 9 | **4** (4,4,4) | 1-2 | 1.33 |
| | **74** | **59** | **48** | ~40 | **0.65** |

**The total improved from 0.80 to 0.65.** Predictions were right on 5 of 10, directionally
right but too optimistic on 3 (8, 9, 1), and **wrong in direction on 2** — the two that got
worse.

### What `for find` bought

- **Scenario 8: 11 → 3.** One `for find MCP_SERVER_PORT` carries the publishing service,
  the default, and the prod/staging absence. It now beats grep, which the pilot lost 5.67×.
- **Scenario 9: 9 → 4.** The tree is the truth, so the staleness that cost 3× is gone. All
  three runs took exactly 4 and all three named the stale index as a *limit of the graph
  commands* rather than as a doubt about the answer.
- **Scenario 10: 4 → 3**, and it now beats grep.

### The two regressions, which share one cause

**Scenario 7 went 2 → 4, and that is a falsification criterion I wrote down in advance.**
Reading the runs says why, and it is not the command:

```
1. cairn for find "X-Api-Key"      <- the complete answer, with `in _headers [mjd]`
2. cairn expand mjd --detail body
3. cairn usage mjd
4. cairn refs mjd --context auto
```

**Scenario 5 went 5 → 8 on a code path that did not change at all.** Same shape: `unreached`
answered in turn 2 in both rounds; round one stopped at turn 3, round two ran six more
turns confirming each symbol one at a time.

Both are the elaboration failure the stop rule fixed in round one — and the cause is my own
edit. Putting `for` at the front of the skill pushed *Stop when you have the answer* further
down the document, and on these two scenarios that rule was worth more than the new command.
The mechanism did not change; the ordering of the instructions did.

A second, smaller contributor: `for find` hands back a handle (`[mjd]`), and a handle is an
invitation. The attribution that makes the row worth more than a grep line is also what
gives the next query something to grab.

### The acceptance rule went the other way from the total

**Met on 2 of 10, down from 4** — scenarios 6 and 3 only. Scenarios 5 and 7 met it in round
one and no longer do. So the headline number improved by 19% while the count of scenarios
that clear the bar halved. Both statements are true and they are about different things:
the total is dominated by fixing the worst losses, the rule is a per-scenario test that two
former wins now fail.

### Answer quality

Equivalent to round one across all ten. Scenario 10's cairn runs still do not reach the Go
handler, so the `SUM`/`MIN` divergence two grep runs found remains unfound by this arm —
unchanged, and still the one place a grep answer is better.

### What this says about the design

**The intent-first entry point works where the mechanism was missing.** Scenarios 8, 9 and
10 were coverage losses and all three are now wins or near-wins, at 3-4 round trips against
grep's 3-5. That is the part of the thesis — the agent states its purpose well and picks its
mechanism badly — holding up.

**It says nothing yet about the assembly.** `for change` was built for scenario 1, which
moved 6 → 5, inside the run-to-run spread. Two of the three round-two runs on that scenario
did not use `for change` at all. Whether a purpose that *fuses* mechanisms pays is not
answered here; only the purpose that *routes to a missing one* is.

**And the skill is now the binding constraint, not the command surface.** Two scenarios
regressed purely from where a paragraph sits in a 13 900-character document. That is a
larger effect than most of what was built this session, and it is the cheapest thing left
to fix — the next change to try is moving the stop rule above the command list, and
measuring only that.

---

## Round three: the skill only — 2026-08-04

**No code changed.** The binary is byte-identical to round two; only `skill/SKILL.md` was
edited. It is a bundle of four changes, not one variable — position, consolidation, one new
line about handles, and 13 907 → 12 069 characters — so nothing below can be attributed to
position alone. `SCENARIOS.md` says which four and why.

| # | grep | r1 | r2 | **r3** | predicted | ratio |
|---|---|---|---|---|---|---|
| 6 | 15 | 1 | 1 | **1** | 1 | **0.07** |
| 3 | 8 | 1 | 1 | **1** | 1 | **0.12** |
| 5 | 14 | 5 | 8 | **7** | 5 | **0.50** |
| 10 | 5 | 4 | 3 | **3** | 3 | 0.60 |
| 7 | 4 | 2 | 4 | **3** | 2 | 0.75 |
| 8 | 4 | 11 | 3 | **3** | 3 | 0.75 |
| 2 | 7 | 9 | 9 | **6** | 8 | 0.86 |
| 4 | 10 | 11 | 10 | **9** | 10 | 0.90 |
| 1 | 4 | 6 | 5 | **5** | 5 | 1.25 |
| 9 | 3 | 9 | 4 | **4** | 4 | 1.33 |
| | **74** | 59 | 48 | **42** | ~42 | **0.57** |

**The total prediction was exact — and it was right for partly the wrong reasons.** Seven
scenarios landed on their predicted number. The two the change was aimed at did not: 7
recovered only to 3 (predicted 2), 5 only to 7 (predicted 5). What made the total come out
right was scenario 2 improving to 6 when 8 was predicted. Two wrong predictions cancelling
into a correct aggregate is exactly the coincidence that makes a headline number worth less
than the rows under it.

### The diagnosis was partly right, and the wrong part is instructive

Round two blamed the regressions on the stop rule moving down the page. Reading all three
rounds of scenario 5 together:

| round | runs | median |
|---|---|---|
| 1 | 3, 5, 8 | 5 |
| 2 | 7, 8, 9 | 8 |
| 3 | 3, 7, 8 | 7 |

Round two's *minimum* (7) sits above rounds one and three's minimum (3), so something real
did shift and shifted back. But rounds one and three cover nearly the same range with
medians of 5 and 7, which means **this scenario's spread is wide enough that a single
median is a weak statement about it**. The pilot's habit of three runs was the right call
and is still not quite enough here.

Scenario 7 is tighter — 2,2,3 then 2,4,5 then 3,3,3 — and the recovery is real but partial.
Position was a cause; it was not the only one.

**No scenario got worse**, and scenario 4 stayed at 9 against a floor of 8 written into the
pre-registration, so the shorter stop rule did not buy round trips by shortening a chain
question. That falsifier did not fire.

### Where this leaves the whole measurement

| | round trips | ratio | rule met |
|---|---|---|---|
| pilot, 1 run/arm | 97 : 100 | 0.97 | — |
| round 1, 3 runs | 59 : 74 | 0.80 | 4/10 |
| round 2, `for` built | 48 : 74 | 0.65 | 2/10 |
| round 3, skill retuned | **42 : 74** | **0.57** | 3/10 |

**Cairn now wins or ties 8 of 10 scenarios.** The two losses are small and both understood:
scenario 1 (5 against 4) is depth through an async wrapper, and scenario 9 (4 against 3) is
the freshly-edited file, where the arm spends turns establishing that the tree search is the
right instrument *because* the graph is stale.

**The acceptance rule is met on 3 of 10** — 6, 3, and 5 at exactly 0.50. It has never been
met on more than 4, in any round, while the total nearly halved. The rule is a per-scenario
bar and the total is a mix; they have moved in opposite directions twice now, and both
numbers stay in this table for that reason.

**Answer quality is unchanged from round one across all ten.** Scenario 10's cairn runs
still stop at the SQL and do not reach the Go handler, so the `SUM`/`MIN` divergence two
grep runs found in the full protocol remains the one place a grep answer is better.

### What the last two rounds actually taught

**The instruction sheet moves the number about as much as the tool does.** Round two added a
command and gained 0.15; round three edited a document and gained 0.08, with no code change
at all. That is not an argument against building — scenario 8 went 11 → 3 because `for find`
exists — but it is a measured statement that a tool with a badly ordered guide gives back
most of what it earned.

---

## Round four: five root causes, three fixed — 2026-08-04

Round three's traces were near-identical across runs, so the causes were readable rather
than inferred. Three of the five were fixed; `SCENARIOS.md` says which two were left and
why, and carries the predictions.

| # | grep | r3 | **r4** | predicted | ratio |
|---|---|---|---|---|---|
| 6 | 15 | 1 | **1** (1,1,1) | 1 | **0.07** |
| 3 | 8 | 1 | **1** (1,1,1) | 1 | **0.12** |
| 5 | 14 | 7 | **3** (3,3,4) | 7 | **0.21** |
| 8 | 4 | 3 | **1** (1,1,3) | 1 | **0.25** |
| 9 | 3 | 4 | **1** (1,1,1) | 2 | **0.33** |
| 10 | 5 | 3 | **3** (3,3,4) | 3 | 0.60 |
| 2 | 7 | 6 | **5** (5,5,5) | 6 | 0.71 |
| 7 | 4 | 3 | 3 (2,3,3) | 1-2 | 0.75 |
| 4 | 10 | 9 | 9 (8,9,13) | 4 | 0.90 |
| 1 | 4 | 5 | 6 (5,6,7) | 5 | 1.50 |
| | **74** | 42 | **33** | ~31 | **0.45** |

### What the fixes bought

- **`for change` on a symbol the index does not hold** now searches the tree and says why
  the graph cannot answer. Scenario 9: **4 → 1**, all three runs identical, and it beats
  grep's 3. Predicted 2; the arms did better, because the redirect's output was the whole
  answer and none of them opened the file afterwards.
- **`for find` carrying ±2 lines** per hit. Scenario 8: **3 → 1**, exactly as predicted —
  the arm no longer opens three compose files to see whether a `ports:` key sits above the
  match.
- **`reaches --outgoing`**, which returned zero for any function that *uses* a generated
  client rather than being one: now 3 handlers where it had 0, in 17 ms.

### Two predictions failed, and one of them is a named falsifier

**Scenario 4 did not move: 9, against a predicted 4.** The pre-registration said this
would falsify the diagnosis. Reading the traces, it half does:

- Run 1 got the entire chain in **three turns** using the fixed `--outgoing`, then spent
  six more re-reading the files it had already been pointed at and re-confirming with
  `affects` and `runs`.
- Run 2 (13 turns) never got a clean first hop: `--outgoing` on the *Python endpoint*
  returns nothing, because the call goes through `env.clients.share` — attribute access on
  a client registry, which the index cannot resolve to the generated stub. So the fix
  answers the middle of the chain and not its entry, and the question always starts at the
  entry.

So the missing direction was *a* cost and not *the* cost. The remaining ones are an
unresolvable attribute call and, again, elaboration after the answer.

**Scenario 7 did not move either: 3, against a predicted 1-2.** The context lines were
supposed to remove the file read; two of three runs still read `client.py`. The diagnosis
was right for scenario 8 and wrong for 7, and the difference is probably that 8's answer
is a value (which a line shows) while 7's is a set of call sites (which it does not).

**Scenario 1 got slightly worse, 5 → 6**, on causes deliberately left unfixed. Within its
spread, but pointing the wrong way.

**Scenario 5 improved to 3 from 7, and nothing was aimed at it.** Its runs across four
rounds: 5, 8, 7, 3 — with spreads of [3,5,8], [7,8,9], [3,7,8], [3,3,4]. The last is the
tightest and the lowest, but three rounds of overlap say most of that history is spread,
not signal.

### Where the whole measurement now stands

| | round trips | ratio | rule met |
|---|---|---|---|
| pilot, 1 run/arm | 97 : 100 | 0.97 | — |
| round 1, 3 runs | 59 : 74 | 0.80 | 4/10 |
| round 2, `for` built | 48 : 74 | 0.65 | 2/10 |
| round 3, skill retuned | 42 : 74 | 0.57 | 3/10 |
| round 4, three defects fixed | **33 : 74** | **0.45** | **6/10** |

**Cairn wins 9 of 10 scenarios and the acceptance rule is met on 6** — the first round in
which the rule and the total moved the same way, and the first in which more than four
scenarios clear it. The single loss is scenario 1, at 6 against 4.

Answer quality is unchanged: equivalent on nine, and scenario 10 still stops at the SQL
without reaching the Go handler, which remains the one place a grep answer is better.

### What is left, stated so it is not read as finished

- **Scenario 1's two causes are untouched by choice.** An ambiguous name costs a whole
  turn (`for change get_quota_status` → exit 2 → `for change wes`, in every run of every
  round), and `for change` gives call sites without their source, does not follow the
  module-level `a = wrap(f)` binding one hop, and does not state the dynamic-dispatch
  check — so the arm runs `refs`, `refs`, `weaklinks` after it.
- **The first hop of a chain is invisible** when the client is reached by attribute access.
  That is the same class as the ORM-attribute limit the tool already declines to guess at,
  and it is what keeps scenario 4 at 9.
- **Elaboration is still the largest single cost** in the runs that lose. Scenario 4 run 1
  had the answer at turn 3 and stopped at turn 9. Three rounds of skill edits have moved
  this and not solved it.

---

## Round five: the last two causes — 2026-08-05, **incomplete**

Both remaining causes were in scenario 1: an ambiguous name cost a whole round trip, and
`for change` returned call sites without source, without the hop through the module-level
`a = wrap(f)` binding, and without the dynamic-dispatch check. Both fixed. `for change` now
answers for the most-referenced non-generated candidate, printing that choice and the
alternatives, and returns four blocks instead of one.

**The round did not finish.** The session's subagent limit (200) was reached with scenarios
8 and 10 unrun and scenario 9 at two runs of three. What is below is what was measured; no
total for all ten is stated, because two of the ten were not measured and this file does not
carry numbers that were not.

| # | grep | r4 | **r5** | predicted | ratio |
|---|---|---|---|---|---|
| 6 | 15 | 1 | **1** (1,1,1) | 1 | **0.07** |
| 3 | 8 | 1 | **1** (1,1,1) | 1 | **0.12** |
| 1 | 4 | 6 | **2** (2,2,4) | **2** | **0.50** |
| 5 | 14 | 3 | 8 (7,8,11) | 3 | 0.57 |
| 4 | 10 | 9 | **7** (6,7,11) | 9 | 0.70 |
| 7 | 4 | 3 | 3 (2,3,3) | 3 | 0.75 |
| 2 | 7 | 5 | 10 (9,10,11) | 4-5 | 1.43 |
| 9 | 3 | 1 | 1 (1,1 — two runs) | 1 | 0.33 |
| 8 | 4 | 1 | **not run** | 1 | — |
| 10 | 5 | 3 | **not run** | 3 | — |

### The fix worked exactly where it was aimed

**Scenario 1: 6 → 2, the predicted number.** It was the only scenario cairn still lost, and
it is now 0.50. One call carries what five turns used to assemble: the sites with their
source, the wrapper hop to `handlers/quota.py`, the deployed radius, and one line saying no
string literal names the symbol. The ambiguity is resolved in the same call, with the choice
and the alternatives printed.

**The ranked choice was right in all three runs** — every one answered about the repository
function the question named, none about the handler or the endpoint. That was the sharper
falsifier (buying round trips with correctness) and it did not fire.

**Scenario 4 also improved, 9 → 7**, which was predicted to stay at 9.

### Two scenarios moved the wrong way, and neither is attributable to the fix

**Scenario 2 doubled: 5 → 10.** It shares the ambiguous-name path, so the fix was the
suspect. The traces say otherwise: **not one of the three runs invoked `for change`.** They
used `symbol`, `usage`, `refs`, `graph`, `reaches` — and then traced the write path up
through handlers, endpoints and a backoffice check. Its history across five rounds is
9, 9, 6, 5, 10.

**Scenario 5 went 3 → 8**, also with nothing aimed at it. Its history: 5, 8, 7, 3, 8.

Both were predicted "unchanged", and the pre-registration said that predicting unchanged is
a claim rather than a hedge. The claim failed twice. **The honest reading is that these two
scenarios have a spread wider than three-run medians can resolve**, and that several
per-scenario numbers in the four preceding rounds — in both directions — were draws from
that spread rather than effects of anything built.

### What this leaves standing, and what it undercuts

**Standing:** the three scenarios that have been 1 or 1-ish in every round since the tool
gained the command for them (6, 3), the literal and stale-file cases (7, 9), and now
scenario 1 at 2 against 4. Those are tight — no run-to-run range wider than two — and they
are where the acceptance rule is met.

**Undercut:** any reading of the round-by-round totals as a clean progression. The totals
0.97 → 0.80 → 0.65 → 0.57 → 0.45 were computed from medians of three, and two scenarios have
now demonstrably swung by a factor of two or three with no cause. The direction of travel is
real — scenario 8 went 17 → 1 and stayed, scenario 9 went 9 → 1 and stayed — but the second
decimal place of any total in this file is not.

**What the next round has to do before it measures anything else** is establish how many
runs scenarios 2 and 5 actually need. Five rounds of three-run medians have produced two
scenarios whose numbers cannot be trusted, and no amount of further tool work fixes a
measurement that cannot tell an effect from a draw.

---

## How many runs the noisy scenarios need — 2026-08-05

Computed from the 15 runs each of scenarios 2 and 5 already in `runs/`, no new agent runs.

| | within-round SD | round medians | swing |
|---|---|---|---|
| s03 | 0.25 | 1, 1, 1, 1, 1 | 0 |
| s06 | 0.25 | 15, 1, 1, 1, 1 | (the tool changed) |
| **s02** | **1.24** | 9, 9, 6, 5, 10 | 5 |
| **s05** | **1.69** | 5, 8, 7, 3, 8 | 5 |

**A permutation test says the between-round swing is noise.** Shuffling the 15 runs of each
scenario between rounds produces a swing of 5 or more with p = 0.33 (s02) and p = 0.73
(s05). Round membership explains nothing about either. The earlier conclusion — that three
runs cannot tell an effect from a draw for these two — holds, and now has a number behind
it.

Bootstrapping the median of *n* runs against the pooled within-round spread:

| | ±1 turn | ±0 turns |
|---|---|---|
| s02 | **7 runs** | 13 |
| s05 | **13 runs** | not reached at 21 |
| s03, s06 | 3 (already there) | 3 |

So the honest options for those two are 7 and 13 runs, or dropping them from any headline
number and reporting them as unresolved with their range. Four to seven times the cost, for
the two scenarios the tool's case does not rest on. The cheap scenarios are cheap because
they are decided in one command; the expensive ones are expensive because they are open
questions an agent can answer at several depths, and no number of runs changes that.

---

## A harness that drives cairn from the shell — 2026-08-05

`eval/stress.py`. Every defect this project found by measurement had one shape: two
commands disagreeing about the same fact. None of them needed a model in the loop. This
runs command pairs over a deterministic stratified sample of the index and reports the
disagreements — 24 symbols in 24 seconds, no agents.

**First run: three classes of finding. One was real.**

- **Real, and fixed: `affects` was not deterministic.** On a symbol attributed to three
  services, the list came back in a different order on each invocation.
  `reachable_by_service` returned a `HashMap` straight into a `Vec`, and Rust randomises
  that order per process; every consumer preserved it. Now sorted at the source. Five
  consecutive invocations are byte-identical.
- **Not real: `reaches` asymmetry.** Asked about a *generated* server stub, the incoming
  direction reports its callers while the outgoing direction resolves them to the
  hand-written handler that serves the RPC — different symbols on purpose.
- **Not real: `affects` on a class smaller than on its method.** The checker read RPC names
  off the hop detail lines as though they were service names.

After correcting both invariants — skip generated definitions; accept the type *or a
member* as the return trip — the sample yields one finding, itself a third granularity
difference: `--outgoing` has two modes, the precise one naming handler symbols and the
convention fallback naming services, so their outputs cannot be compared by shape. That is
a real inconsistency and not a wrong answer; it is written down rather than fixed.

**The honest hit rate is one real defect in three first-run findings**, and the two false
ones were both the invariant being stronger than the contract. That is the cost of this
approach and it is still cheap: an afternoon of agent runs costs more than every stress run
this file will ever need, and the defect it found — an answer that reorders itself between
invocations — is one that would have quietly undermined every diff-based claim here.

### The harness, extended — 2026-08-05

Six more invariants, each citing the sentence it enforces, because the first version's two
false findings were both the check being stronger than the contract:

| check | contract it rests on |
|---|---|
| envelope and exit codes | "Answers end with `unknown:` / `suppressed:` / `stale:`"; exit 0/1/2/3 |
| a cut list admits it | "`--budget` is a ceiling … and reports what it dropped" |
| `runs` agrees with `affects` in-process | two commands, one fact: which services run this |
| `literal` agrees with `for find` | both answer "whose line is this", by different routes |
| `verify` agrees with `status` on staleness | one-off check against watcher view of the same tree |
| a printed handle resolves | handles are a shortest-unique prefix; a collision shows up nowhere else |

**Second real defect found: `status` was missing two thirds of its own envelope.** It
printed `stale:` and nothing else, while its sibling report `verify` printed all three
lines. It is the command the agent guide says to run first, so it was the one answer where
a reader who had learned to check `unknown:` found nothing there — and it does have unknown
content, the partial coverage mechanisms, which it was printing only as prose. Now emitted
under the standard labels, with a fixture case pinning it.

Two more checker corrections, same lesson as before: handles were being read out of
explanatory prose (`answered for the enclosing type [qsu]` names the subject, not a
caller), and the literal query named a column the schema does not have.

The one finding that survives is the `--outgoing` two-modes inconsistency described above.
It is in a `KNOWN` list keyed by check *and* symbol, so a clean run says "no new
contradictions" while the same class on a different symbol still surfaces. A harness that
reports the same thing every run stops being read; one that suppresses a whole check hides
the regression it exists to catch.

**Running total for the shell harness: two real defects, both of a kind no agent run would
have surfaced** — one answer that reordered itself between invocations, one that omitted
the envelope the guide tells readers to trust. Four false findings, all from invariants
written stronger than the contract, all corrected in place with the reason.

### `reaches --outgoing` unified — 2026-08-05

The one finding the harness kept reporting. The command had two ways of answering and they
did not look alike: from real call edges it named handler symbols, from a client binding it
named services. Nothing in the output said which you had, so the two could not be compared
— by an agent or by the harness, which could only report the difference and never check it.

Both now emit the same rows. What differs is the claim, said in the header and the line
under it:

```
[3yv4] shareService.GetSharedObject — calls 3 handler(s) across gRPC   [L1, convention]
  … each row is the handler that serves an RPC this code was seen to call

[mk6] registerGoAgent — can reach 65 handler(s) across gRPC  [L1, convention, by client binding]
  … this code holds a generated client for these services … what it *can* reach, not what
    it was seen to reach
```

**One mistake made and caught on the way, of the kind this repository keeps making.** The
binding form initially inner-joined the generated client to filter handler members down to
real RPCs — 65 rows became 61, which looked like an improvement. It was not: services whose
client link carries no `via_symbol` were dropped **whole**, one service vanishing rather
than one helper row. The harness caught it immediately, because the symmetry check stopped
passing for a symbol it had passed for before. Now a LEFT JOIN, so a service with no client
artefact keeps its rows and the envelope names it:

```
unknown: 8 service(s) have no generated client in this index to list their RPCs
         (assistant_api.AgentAdvisoryService, …), so every member of their handler is
         shown - private helpers included.
```

That is the trade taken deliberately: an unfiltered row that says it is unfiltered beats a
filtered list that quietly lost a service.

**The harness now reports no contradictions on a 24-symbol sample with an empty allowlist**,
which is the first time it has. The allowlist was emptied rather than left with the entry in
it, so the finding coming back would be a failure rather than a known.

### The harness runs in CI — 2026-08-05

Over the fixture corpus, the same one `tests/corpus.rs` and `tests/sweep.rs` fall back to,
so the check really runs on a runner rather than only on a workstation with a private
checkout. The image already carries python3; the step builds the index the way a user would
— `cairn index`, then ask it questions — and takes seconds.

Both paths were verified against the `ci` service rather than assumed: a deliberately
injected finding exits 1 and fails the job, and the unmodified harness exits 0. A check that
cannot fail is decoration.

**The fixture found one thing the private corpus had not**, and it turned out to be the
checker again: `graph --aspect callers` looked nondeterministic because the `stale:` line
changes between two identical invocations — the watcher starts itself on first use, so the
first call says "not tracked yet" and a later one gives a real verdict. That is the envelope
working, not the answer moving. The determinism check now compares answers without their
staleness line, and says why.

One check was also narrowed rather than left to misfire: `verify` against `status` only runs
when the index sits at `<repo>/.cairn/index.sqlite`, because the watcher derives the
repository as the database's grandparent and a fixture index in a temp directory has it
watching the wrong tree. A disagreement there would have been a fact about the harness.

Running total for the shell harness: **two real defects, six false findings**, every false
one from a check written stronger than the contract it enforces. Both real ones were
invisible to `cargo test` and to sixty agent runs.

---

## `for understand`, and the defect building it exposed — 2026-08-05

Pre-registered as round six in `SCENARIOS.md`. **No agent runs.** Everything below is
either a correctness result or a statement about the harness; the round-trip predictions
for scenario 4 are unmeasured and stay unmeasured until an arm is run.

### The previous note about scenario 4 was wrong, and how it was wrong matters

Round five predicted scenario 4 would stay at 9 because "the first hop is still an
attribute call the index cannot resolve". It went to 7, and the stated reason was false.
The first hop is not unresolved. It is **resolved on one surface and hidden on another**:

| command | what it says about `get_shared_object` |
|---|---|
| `graph fsw --aspect calls` | `AppClients.share`, `env`, `ApplicationEnvironment.clients` — the attribute plumbing, and no RPC |
| `reaches fsw --outgoing` | `shareService.GetSharedObject` at `srcgo/.../share.go:33`, exactly |

`graph` drops generated code on outward call edges, deliberately and for a good reason
(protobuf message types crowded out the real callees, and an agent asked to trace an entry
point once read 68 rows of which three were calls). The RPC stub is generated, so the hop
falls in that hole. The tool held the answer and knowing which command held it was the
agent's problem — which is the exact failure `for` exists to remove.

Walked by hand, the whole chain is two hops and terminates: `fsw` → `3yv4` (Go) → three
Python handlers, none of which has outgoing targets.

### What was built

`cairn for understand <symbol>`, the outward mirror of `for change`. Three blocks, one
call, each naming the command behind it: the chain followed transitively to its end, the
in-language callees, and the services that run it. Both caps on the walk — depth 4, 40
hops — are *printed* when they bite, because a branch that stopped at a cap and a branch
that ended look identical on the page.

The subject resolution is now shared with `for change` rather than copied. Four measured
fixes live in that path (the spoken text redirect, inline resolution, the ranked choice for
an ambiguous name, the tree fallback for a symbol the index lacks), and a second purpose
re-earning any of them by hand is how a fix quietly stops applying.

The skill grew **12 069 → 12 856 characters**, +6.5%. Length was itself a variable in round
three and this is a change in it, stated rather than left for a later round to attribute.

### A real defect, found by building on top of the mechanism

`reaches --outgoing` printed definition lines **0-based** where every other command in the
binary prints 1-based. SCIP counts from 0 and `Occurrence::location` does the conversion;
that one renderer formatted `path:line` by hand and skipped it.

So the two directions of one command named the same definition a line apart:
`reaches 3yv4` said `share.go:33-90`, `reaches fsw --outgoing` said `share.go:32`. Checked
against the file: `func (s *shareService) GetSharedObject` is on line 33. The `--outgoing`
direction was wrong.

Invisible to 165 tests and to sixty agent runs, and not the sort of thing anyone verifies
by eye — an off-by-one in a line number reads as correct right up until you open the file.
Fixed, pinned by a unit test in `cairn-fmt`, and now covered by a standing harness check.

### The harness had a hole exactly where its newest checks were pointed

The new checks were verified the way the CI step was: build a binary with the defect
deliberately re-injected, and confirm the run fails. **It did not.** The whole file ran
against the buggy binary and reported no contradictions.

The cause was the sample, not the checks. All three strata select on reference count
(`ref_count > 2`) or on being a handler type — and the code that *starts* a service chain
is typically a route handler that nothing calls. Every cross-boundary check was running on
symbols that cross no boundary. A check that cannot fail is decoration, and four of them
had just been written into that position.

A fourth stratum, **chain starts** — symbols with a call edge into a generated client —
fixes it. With it: injected defect → exit 1 on both corpora, unmodified → exit 0. The
fixture corpus contributes one such symbol, which is thin but enough for CI to catch it.

### One more false finding, same cause as the six before it

The symmetry check reported that `reaches r5hr` names `[b8]` while `reaches b8 --outgoing`
does not name `[r5hr]`. It was not a defect. `r5hr` is `websocket.streamAgentChat` — Go,
lowercase, **unexported, and therefore not an RPC at all**. The incoming direction says so
on the page (`answered for the enclosing type [uv7] websocket: a service binding names the
handler, not each of its RPC methods`) and answers for the type; the outgoing direction
correctly comes back with the sibling that *is* an RPC.

The check simply did not read the sentence the answer printed. It now widens the family to
the named type's members when that line is present.

### The widened sample then found a third real defect, and it was a large one

The first run of the widened sample — 48 symbols — reported another asymmetry, and this one
was **not** the check being too strong. `reaches r2yr` named `[ysuf]` as a caller across
gRPC. Both are Go, both are `FolderTransformer.loadEstates` and `estateService.ListEstates`,
and `cairn runs` puts both in **one process**, `assistant-proxy`. Nothing can call anything
across a service boundary inside a single process.

Read from the source: `estateService` is declared
`type estateService struct { assistant_fe.EstateServiceServer }`. It embeds the `_fe`
interface and nothing else, and its `ListEstates` takes `*assistant_fe.EstateFilter`. The
index nevertheless held a `serves` link from it to `assistant_api.EstateService` — the
service it is a *client* of.

The cause is in `link_services`. The `embedders` query matched the embedded field to the
generated artefact **by name alone**, and `assistant_api` and `assistant_fe` both declare
`EstateServiceServer`. This module's own header says the package is part of the key and not
decoration; that query was the one place it was not.

**It affected all nineteen Go proxy handlers** — `authService`, `folderService`,
`quotaService`, `shareService`, every one of them — each recorded as serving the internal
API service it calls. The direction of an entire tier was inverted in the graph.

The fix requires the artefact to be *referenced on the field's own line*, which the
occurrence table resolves to a specific symbol and therefore a specific package. Measured
before and after on the target repo:

| | before | after |
|---|---|---|
| embed links (artefact, type) | 119 | 100 |
| serve links in the index | 295 | 276 |
| call links | 321 | 321 |
| types left with **no** binding | — | **0** |

All 19 dropped links are that collision, and no handler lost its real binding — which was
the outcome that would have been worse than the bug, since a silent zero from `reaches` is
indistinguishable from a service nothing calls. Scenario 3's key answer is unchanged: 10
callers, nine in `folder.go` plus `shareService.GetSharedObject` in `share.go`, including
the tenth that a reader assembling it by hand misses.

**What the wrong rows actually were.** Running `affects` and `reaches` over 25 handler types
against the before and after indexes, 4 of 50 answers changed, and the changed ones changed
a lot: `reaches searchService` went from **14 callers to 7**. Every one of the seven removed
was `searchService.<method>` — *the type's own methods, listed as callers of the type across
a network boundary*. A type cannot call itself over gRPC. The seven that remain are the real
ones, all Python, all reaching it over `assistant_fe`:

```
before  [sce]  searchService.CreateSearch      go  …/resttransform/search.go   [assistant_api.…]
after   [vz]   search_and_score_estates        py  …/api/endpoints.py          [assistant_fe.…]
```

So on those symbols the answer was half wrong, and wrong in the direction that reads as
thorough — more rows, each individually plausible, none of them flagged.

Pinned by a `link_services` test that fails without the fix (verified by removing the join
and watching it fail), and the harness that found it now runs clean at 48 symbols.

**Running total for the shell harness: four real defects, seven false findings.** Every
false one came from a check written stronger than the contract it enforces; the last two
real ones were found only because the sample was widened, which is the more useful lesson —
the checks were not weak, they were pointed at symbols that could not exercise them.

A fifth invariant was added afterwards, and it is the one that would have named the
`link_services` defect on sight rather than three steps removed: **nothing reaches itself
across a boundary.** A handler type cannot be on both ends of one, so its own methods are
not among its callers. Verified both ways against the kept pre-fix index — 2 findings there,
0 after. It also caught itself first: the initial version read the answer's *header*, which
names the subject, and so reported every symbol. That would have been false finding number
eight.

### Fixing the root cause made a workaround removable, and the workaround was a fourth defect

Widening the sample again — 120 symbols — turned up `reaches ixa` naming
`[4xa] Command._run` as a caller, while `reaches 4xa --outgoing` answered **0 targets**.
Both are Python. The outgoing direction had a same-language filter, and its own comment
says what it was for:

> Both sides of a boundary can be registered as servers of a service the convention spells
> the same way, so the join returns the caller itself and its same-language siblings
> alongside the real targets.

That is a description of the `link_services` collision fixed above. The filter was a
workaround for it — and it was suppressing, along with the phantom rows, **every case of one
Python service calling another**. The `assistant` CLI commands reaching `dataplatform-grpc`
over `dataplatform_api.EstateProviderService` are exactly that: the incoming direction listed
nine of them, the outgoing direction returned zero for each, and a zero is indistinguishable
from a service nothing calls.

With the root cause gone, the honest exclusion is the caller's own **file** — a "target"
defined beside the caller is the handler this code *is*, or its sibling. Measured over a
deterministic 1-in-5 sample of the 326 symbols that start a chain: **10 of 66 were a silent
zero before**, 15%, every one of them a same-language service call.

`reaches --outgoing` also stopped claiming "in the other language" in its header, because it
no longer means it. A boundary is between processes; the cross-language case is the one
nothing else can answer, which is why the file is named for it, but it was never the
definition.

**This is the more useful shape of the whole afternoon**: one wrong join produced a wrong
answer, a workaround written to hide the wrong answer produced a second and opposite wrong
answer, and neither was visible until a sample was widened enough to put a symbol of the
right shape in front of the checks.

### The contract sweep was not running the commands the guide recommends first

`tests/sweep.rs` runs every read command against a mechanically chosen sample. Its command
table put the handle immediately after the command word, which cannot express
`for understand <h>` — so `for change` and `for understand`, the two entry points the skill
sends an agent to before anything else, were the only commands in the binary it never ran.
The table is now (before-handle, after-handle) pairs and both are in it. 40 symbols × 14
commands against the real index: 75 s, no contract violation, no latency ceiling reached.

### Where knowledge of the measured repository still leaks into a result

Asked directly whether the fixes encode anything about the checkout they were found on. The
mechanisms do not — each is a statement about SCIP or about protobuf, and the two unit
tests that were written with real package names and a real path have been rewritten to a
synthetic gateway. But "no repository-specific strings in the code" is not the same claim
as "no repository-specific influence on a result", and three places remain where it is:

1. **`stress.py` hardcoded a word from the private checkout.** The envelope check ran
   `for find Kontomatik`. On the fixture corpus — the one CI runs — that string does not
   exist, `for find` exits 1, and the assertion's `if code != 0: continue` skipped. So the
   only command that reads the working tree was unchecked exactly where the check runs
   automatically. Now the needle is drawn from the corpus's own literals; both corpora
   return exit 0, so the assertion fires on both. **A constant borrowed from one repository
   is the quietest way to disable a check on every other**, and this is the second time in
   one day that a check turned out not to run.

2. **`tests/sweep.rs` and `tests/corpus.rs` prefer the real index when it exists.** On a
   workstation they sweep the private checkout; on a runner they fall back to the fixture.
   That is deliberate and it is why the fixture exists, but it means a local green and a CI
   green are not the same statement. `eval/corpus/cases.yaml`, the hand-written expectations
   about that checkout, is untracked — the repository carries only the fixture's cases.

3. **Every number in this file is from one repository.** 119 → 100 embed links, 10 of 66
   silent zeros, the round-trip ratios: all measured on a single Python+Go codebase. The
   defects behind them are general — a package split plus Go embedding, a 0-based line
   number — but *how much* they matter anywhere else is unmeasured, and the roadmap has
   said since the start that generality is a claim about the design and not a measurement.

### The harness now reports which checks actually ran

Two checks turned out not to run, on two consecutive audits, and both times the file
printed `no contradictions found`. Auditing a third time by hand would find the third one
eventually; making the run say it is better. Every check now records the moment it commits
to an assertion, and the summary names any that never got there:

```
14/15 checks reached an assertion on this corpus
  never got past their guards here - not a pass, an absence of evidence:
    - chain followed to where it says
```

Deliberately not an error. A corpus with no ambiguous names has nothing for that check to
do, and failing the run would train people to ignore the line.

It immediately paid for itself: **the real index exercises 15 of 15, the fixture only 13**,
so CI was two assertions weaker than a workstation while reporting the same clean result.

* **`staleness agrees` never ran in CI.** Its guard requires the index at
  `<repo>/.cairn/index.sqlite`, because the watcher derives the repository as the database's
  grandparent — and the CI step built the fixture index in `/tmp`. Fixed by building it
  where the convention puts it, which is also what a user does. CI is now 14 of 15. cairn
  writes a `.gitignore` into that directory itself, so nothing new is tracked.
* **`chain followed to where it says` still does not run in CI**, and cannot: the fixture
  has one symbol that starts a chain and that chain is one hop deep. Closing it means giving
  the fixture a second service to hop to and regenerating its SCIP — real work on the test
  corpus, named here rather than left as a silent zero.

### The questions the agent runs are asked, audited

Separately from the tool: five rounds of round-trip numbers rest on ten questions whose
*wording* had never been examined. `SCENARIOS.md` now carries that audit. The short version:
**six of the ten name the shape of their own answer**, and scenario 4 — the one this round
built a command for — tells the arm outright that the chain continues past the first hop, so
its depth is supplied rather than discovered. Two figures in the protocol were also stale:
the skill is 12 856 characters, not the 8 850 quoted, and the cairn arm's instructions are
now **40% longer than the grep arm's** rather than shorter as round three recorded.

None of that invalidates the direction of travel. It does mean the per-scenario numbers
measure something slightly narrower than the file has been claiming, and rewording a
question restarts its series — so the audit separates the fixes that cost a history from
the ones that cost nothing.

### Gates

166 tests (was 164), clippy clean on Linux and on the Windows target, sweep clean on both
the fixture corpus and the real index, harness clean on both at 21 and 48 symbols.

`for understand fsw` is 0.07 s against 0.01 s for the single `reaches --outgoing` it
follows transitively — *n* queries where the mechanism does one, and still two orders of
magnitude under anything a round trip costs.

`cargo fmt --check` **failed on HEAD before any change of mine**, at two sites in
`affects.rs` and `protolink.rs`, which means CI's Format step is red on `main` as it
stands. `rust-toolchain.toml` says `channel = "stable"` rather than a version, so a
rebuilt image picks up a newer rustfmt and the tree drifts without a commit. Both sites are
formatted in this change; pinning the channel is a separate decision and is not made here.
