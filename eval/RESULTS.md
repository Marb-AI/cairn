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
*wording* had never been examined. `SCENARIOS.md` now carries that audit. **Six of the ten
name the shape of their own answer**, and scenario 4 — the one this round built a command
for — tells the arm outright that the chain continues past the first hop, so its depth is
supplied rather than discovered.

**The sharpest finding is not about wording.** Scenario 10 is the single standing loss in
the whole measurement, and its verdict rests on a criterion that was never registered: the
key lists two SQL files, the question asks about SQL, and grep was awarded the scenario for
finding a `SUM`/`MIN` divergence against a **Go handler** that neither mentions. That
criterion came from the round-two falsifier list and has been carried forward as settled
through three further rounds. It is a fair criterion — a real divergence between a script
and the code it mirrors is worth more than the answer asked for, and the acceptance rule
already rewards that — so it is now written into the key, with the verdict unchanged and
the addition that answering only what was asked is graded *same*, not *worse*.

The cheap fixes are applied: both arms' instruction sizes are now a table in the protocol
with an instruction to re-measure (grep **10 002** characters, cairn **14 060** — the cairn
arm gets 40% *more* than the baseline, reversing what round three recorded), the toolkit
asymmetry is stated along with which way it runs, and scenario 10's key matches what
scenario 10 was graded on.

**The expensive half was done the same day.** Scenarios 1, 4 and 9 are reworded, each
keeping its old text and the reason it leaked:

| # | was | is |
|---|---|---|
| 1 | "**In `srcpy/…/quota.py`**, I want to add a required argument to `get_quota_status`…" | "I want to add a required argument to `get_quota_status`…" |
| 4 | "…what serves it, **and where does that land**?" | "What happens when it is called? **Follow it as far as it goes.**" |
| 9 | "**I just added** `quota_headroom` to the quota repository. Is anything calling it **yet**?" | "Is anything calling `quota_headroom`?" |

Round six's prediction table is **withdrawn before it was ever run**: its `3` for scenario 4
was a prediction about a question that told the arm the chain continues. The live figure is
4, and scenarios 1 and 9 carry fresh predictions too.

**What it costs, said plainly: three series restart.** Rounds 1–5 for those scenarios
measured different questions. Their numbers stay in this file, but they are not a baseline
the next round improves on — which means the headline
**0.97 → 0.80 → 0.65 → 0.57 → 0.45 now spans two different question sets and should not be
quoted again.** It was already undercut by the noise finding on scenarios 2 and 5; this
finishes it. Seven scenarios keep their history, including 3 and 6, which are the ones the
tool's case actually rests on. And the comparison against `grep` survives every rewording
intact, because both arms always get the same question — that is the measurement that
matters and it was never in doubt.

Scenarios 3, 6 and 10 keep the tool's vocabulary ("deployed services", "through which
RPCs") deliberately. Those are the questions a person actually asks about a service
architecture; neutralising them would measure a vaguer question than anyone has. The leak
in 1, 4 and 9 was different in kind — they gave away the specific fact being graded, not
the domain the question lives in.

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


---

## Round six: the three reworded questions, measured — 2026-08-05

18 runs, 3 per arm per scenario, both arms re-run because all three questions changed.
Pre-registered in `SCENARIOS.md`; the predictions there were written before any run and are
reproduced below unedited.

| # | cairn | grep | ratio | I predicted | verdict |
|---|---|---|---|---|---|
| s01 | [3, 5, 6] med **5** | [6, 7, 7] med **7** | **0.71** | cairn 2, grep 5–7 | **fails the rule** |
| s04 | [11, 13, 13] med **13** | [20, 27, 32] med **27** | **0.48** | cairn 4, grep 7–10 | passes, marginally |
| s09 | [2, 3, 3] med **3** | [4, 4, 6] med **4** | **0.75** | cairn 2, grep 1–2 | passes on quality |

**Five of six arm predictions missed.** Only s09's cairn figure (2 predicted, 3 measured)
was close. s04 was wrong by a factor of three on one arm and nearly four on the other.

### Grading against the reworded keys

- **s01 — equal answers, rule failed.** All six runs, both arms, spotted that seven symbols
  share the name and said which one they were answering for, which is exactly what the new
  key demands. Equal quality at 0.71 fails the rule's ≤0.5.
- **s04 — equal answers, rule met at 0.48.** Both arms traced all four hops and both found a
  real defect in the target repository (below). Grep added the absence of a panic-recovery
  interceptor and the stale proto HTTP annotation; cairn added nothing grep lacked. Calling
  these equal is the honest reading, and 0.48 then passes.
- **s09 — cairn better, and cheaper.** Every cairn run stated that the symbol is absent from
  the index and that the file changed since indexing, then answered from the working tree.
  The grep runs answered correctly but inferred newness from code shape (no `db_async`
  wrapper) rather than from the tool telling them. Better answer at 3 against 4 meets the
  rule's "no more than baseline".

### What the rewording actually did, including to me

**s01 got harder for both arms, which was the point.** Grep went 4/4/5 on the old wording to
6/7/7 on the new one: naming the file really had been doing the work. But cairn's own
number rose too — 2 in round five to 5 here — and the ambiguity fix that round five credited
with the win is now doing its job against a question that no longer hands the answer over.

**s04 moved into the open-ended class, and I did not see it coming.** "Follow it as far as
it goes" invites depth without bounding it. Both arms spent 16–33 tool calls, read Go
transform code, and returned answers well past the key. `RESULTS.md` already contained the
warning — *"the expensive ones are expensive because they are open questions an agent can
answer at several depths, and no number of runs changes that"* — and I reworded s04 into
that class and then predicted a single number for it. **The old 7 and the new 13 are not
comparable in either direction**, and s04 may now need the 7–13 run treatment scenarios 2
and 5 need.

**No total is quoted.** Three scenarios, all freshly reworded, two of them arguably
open-ended. The sum of medians is 21 against 38; it is in this file for completeness and it
is not a headline.

### Measurement caveat, stated because it is close to the line

Turn reconstruction groups tool calls separated by less than 1.0 s. Five runs recorded a
widest within-turn gap of 0.77–0.93 s under three-way parallelism. Nothing crossed the
threshold, but the margin is thin enough that a busier machine would start splitting turns
and inflating counts. Either the runs need to be serialised or the gap needs to come from
the data rather than from a constant.

### The runs found a real bug in the target repository

Independently, in all six s04 runs: `srcgo/domains/assistant/grpc/resttransform/share.go:78`
constructs `transform.NewFolderTransformer(s.app, nil)`, and
`transform/folder.go:91` dereferences that principal with a plain field access
(`f.principal.UserId`). Fetching a shared folder containing at least one estate is a nil
pointer dereference; an empty folder escapes via the early return at `folder.go:76`. The
grep arm added that the rest-transform server installs only Sentry interceptors and no
recovery interceptor, so this takes the process down rather than returning 500, and that
there is no Go test covering `GetSharedObject` at all.

That is not a cairn result — both arms found it. It is reported here because it is the most
valuable thing the round produced and it belongs to the backend, not to this tool.

### Three defects in cairn, found by the round, fixed after it

Deliberately not fixed mid-round: changing the instrument between runs is what makes
results incomparable.

1. **A second off-by-one.** `purpose.rs:151` hand-formats `r.line` in `for change`'s binding
   block, printing `handlers/quota.py:42` where the call is on 43. Two s01 arms caught it by
   opening the file. The invariant added this morning did not cover it — it compares
   *definition* lines for `for understand`/`reaches`/`expand`, and this is a *reference*
   line in `for change`.
2. **`--context auto` costs a round trip.** `refs <h> --context auto` without `--repo` exits
   with an instruction instead of an answer, and the arm re-runs it with `--repo .`. That
   happened in 2 of 3 s01 cairn runs and is most of the gap between the 3-turn run and the
   5- and 6-turn ones. The binary already resolved the index from the repository root; the
   flag asks the caller for a value it has just computed.
3. **`for understand` over-claims "followed to the end".** It follows RPC hops but not the
   in-language calls *between* them, so a chain of the shape RPC → local call → RPC stops
   early while printing that it reached the end. For `get_shared_object` the Go proxy locally
   calls `FolderTransformer.loadEstates`, which makes a fourth hop to
   `EstateServiceHandler.list_estates`. All three s04 cairn arms found it by reading code;
   one wrote *"treat the hop list as a floor, not a ceiling."* This is the command built
   earlier the same day, and the sentence is as much the defect as the walk is.

### Rig defects found before the round could start

- **`/usr/local/bin/cairn` is a 0.1.1 install with no `for` subcommand.** Bare `cairn`
  resolves to it. Round five's logs were checked for the retry signature and do not have it,
  so those numbers stand — but an arm invoking bare `cairn` in a session without
  `eval/armbin` on PATH would silently measure a nine-day-old binary, and for commands
  present in both versions it would not even error. The arms now use the absolute path.
- **`score.py` would have borrowed round-one grep medians** for exactly these three
  scenarios — numbers measured against the old wording. It now refuses and says so.

### The three fixes, and the one that did not work

Applied after the round closed, on the build the round measured.

**1 and 2 worked.** `for change` now prints `handlers/quota.py:43`, the line the call is
actually on, and `refs <h> --context auto` answers from the repository it resolved the index
from instead of spending a round trip asking for it. The location invariant was widened to
cover `for change` as well — leaving it out is precisely how the second instance of that
defect shipped, since the check covered *definition* rows in three commands and the command
that got it wrong printed a *reference* row in a fourth.

**3 did not.** The walk now follows two local levels at each hop, and across a sample of 28
chain starts that recovered **1 hop in 41** — real, and cheap (0.07 s to 0.09 s). It does
**not** recover the hop the arms found. The reason is not a bound:

```
$ cairn graph 3yv4 --aspect calls --depth 2 | grep -c RpcToRest
0
```

`shareService.GetSharedObject` calls `ft.RpcToRest(...)` on a local variable, and scip-go
resolves no call edge for it. The hop is invisible to *any* walk over this index, at any
fan-out. It is the receiver-resolution class the tool already documents for Python
attributes, showing up in Go.

So the part of the fix that addresses the finding is the part that changes what is claimed:
the header no longer says "followed to the end", it says what it followed, and the envelope
now states the gap outright — *"this list is a floor, not a ceiling… a handler that
delegates to a helper it constructs hides whatever that helper reaches."* A printed bound
can be widened by whoever reads it; an unresolved edge cannot, and the only honest move is
to say so where the answer is.

That is the round's most useful lesson about this tool: **three independent agents found a
gap by reading a file, and the fix for it was not code but a sentence.**

### The corpus was patched afterwards, which changes what s04 can find again

The nil-principal bug the round surfaced has been fixed in `repos/backend`:
`share.go:78` now passes `&common_types.Principal{SecretToken: secret}` instead of `nil`,
and `transform/folder.go:91` uses the generated nil-safe getters and accepts a principal
carrying either a user id **or** a secret token. Both files keep their exact line count, so
every line number this file and `SCENARIOS.md` cite — `share.go:33-90`, `folder.go:91` —
still resolves after reindexing.

The fix is the same shape the estate branch already had: the downstream Python handler
guards personalisation with `if message.principal and message.principal.user_id:` and works
fine without one, so the Go-side guard was stricter than the service it called. Estate ids
still come from the `SharedObject` the secret unlocked, so nothing is newly exposed.
Compiled against `golang:1.26-alpine`; `gofmt` clean.

**A later reader should expect s04 to stop producing that finding.** Six of six runs
reported it in this round; a re-run against the patched corpus will not, and that is a
change in the corpus rather than a regression in either arm. `repos/backend` has no `.git`,
so the change lives only in this working copy — the applicable patch is kept outside the
repository, since a private backend's source does not belong in this one.

---

## The daemon defect the standing check finally caught — 2026-08-05

`check_staleness_agrees` was added this morning and passed every run. It fired for the
first time on a real disagreement, produced by ordinary work rather than by a probe: after
patching two Go files in the corpus and reindexing, `verify --repo` reported `stale: none`
while `status` reported `2 modified`.

**Cause: the daemon's index snapshot was read once at start-up and compared against for
ever.** `DirtyTracker` holds `indexed: HashMap<path, hash>` from the moment it started, and
`.cairn` is in `IGNORED_DIRS`, so no file-system event ever arrives when the index is
rebuilt. Edit a file and the daemon is right; reindex and it goes on being wrong until
someone restarts it.

The fix gives the tracker the index path and a closure that re-reads the snapshot, checked
on each request — one `stat` when nothing has moved. Only the files already believed dirty
are re-checked afterwards, because a rebuild records what is on disk and can therefore only
ever *clean* a file. The closure exists so that `cairn-daemon` does not gain a dependency on
`cairn-store`: a file watcher has no business opening a database.

### And a second, louder half of the same fault

With `modified` fixed, `status` still reported **17 created** against `verify`'s `none`.
Those files are real — `.py` and `.go` under `tools/` and `infra/` — but no SCIP run has
ever been pointed at them: the recorded roots are `srcpy` and `srcgo`. The `created` set
filtered by *extension* and not by *whether an indexer could ever have read the path*.

This is the same false-loudness the extension filter itself was added for, one level in.
The `created` set is now filtered by the recorded roots as well, falling back to
extension-only when an index records none so an older index degrades to the previous
behaviour rather than to silence. Both commands now say `stale: none` on a clean tree.

Two tests pin it, each verified to fail without its fix: one reindexes under the tracker
and asserts the dirty set catches up, one asserts a `.py` outside every root is not news.

**Running total for the shell harness: five real defects, seven false findings.** This is
the first that the harness found in the course of ordinary work rather than in a run aimed
at finding something — which is the argument for standing checks over audits.

### s01 re-measured on the fixed binary — 2026-08-05

Three cairn runs, same question, same arm file. **The grep arm was not re-run**: the
question is unchanged and the two fixes since round six (`--context auto`, and the
`for change` line number) are cairn-side only, so its [6, 7, 7] carries over. Borrowing it
is stated rather than silent, which is the whole reason `score.py` refuses to do it for a
question that changed.

| | round six | rerun | |
|---|---|---|---|
| cairn | [3, 5, 6] med **5** | [4, 4, 4] med **4** | |
| grep | [6, 7, 7] med **7** | (reused) | |
| ratio | **0.71** | **0.57** | rule needs ≤0.50 |

**s01 still fails the rule**, which is what was predicted before the runs — one turn was
recoverable and one turn was not enough. Answer quality is unchanged and equal to grep's:
all three runs named the ambiguity, said which of the seven symbols they answered for, and
listed that symbol's sites including the hop through the `db_async` binding.

**The spread collapsed from 3 to 0.** Round six's runs ranged 3–6; these are 4, 4, 4. That
is the more interesting number, because it says the variance *was* the defects rather than
the question: the wasted `--repo` round trip appeared in two runs of three, and the runs
that opened files to check the off-by-one line were the long ones. Fixing both removed the
median turn **and** the disagreement between runs.

**What it means for the scenario.** s01 is now a tight, reproducible 4 against 7 with equal
answers. Under the acceptance rule that is a loss, and three rounds of evidence say the
remaining four turns are not obviously removable: one call to `for change`, one to inspect
the other candidates, and two file reads to quote the lines. The honest conclusion is that
**this question is one a competent grep agent answers well**, and the tool's case does not
rest on it — it rests on 3, 4 and 6, where a name search has nothing to offer.

### A daemon nobody starts is a daemon nobody stops — 2026-08-05

Counted while looking at something else: **135 cairn daemons alive at once**, each holding a
language-server pool. 67 watched a scratchpad directory from a session that had ended hours
before; **63 watched `crates/cairn-cli/tests/fixtures/corpus`** — one per `cargo test` run
that built a fixture index and asked it a question. One watched the repository anybody
actually cared about.

The cause is the design working as intended in one direction only. *Nobody should have to
know the daemon exists*, so any command that finds an index and no watcher starts one. There
was no matching sentence for stopping.

Two exits now, both through the ordinary shutdown request rather than `exit()` — that is the
path that stops the language servers, and a watchdog that leaves those behind has moved the
leak rather than fixed it:

* **Idle**: nothing asked for 30 minutes. Long enough to survive a person thinking, short
  enough that a test run's daemon is gone well before the next run starts.
* **Gone**: the repository no longer exists. Verified end to end — a daemon on a temp
  repository exited ~60 s after the directory was deleted, one poll interval.

A test asserts the window rather than the constant's spelling: at least 10 minutes so a
pause for thought does not cost a respawn, at most an hour so a day's test runs cannot pile
up, and a poll faster than the window or it never fires.

**The 133 orphans were killed by PID**, after listing them and confirming the live daemon was
not among them — `pkill -f` on a pattern that also matches the shell running it is how an
earlier session spent several rounds debugging a problem it was causing itself.

### The turn threshold now comes from the data, and the metric turns out to be flat

Every round-trip number in this file rests on one constant: tool calls less than **1.0 s**
apart were one turn, more were two. That constant was chosen when the two bands were
"1.8–2.6 s between turns against sub-100 ms within one". Under three-way parallelism the
within-turn band stretched, and round six recorded within-turn gaps of 0.93 s — close
enough to the line to make every number in the round suspect.

Pooled over 44 runs, the distribution is cleanly bimodal and the constant is in the wrong
place:

```
within-turn band   … 0.89  0.92  0.92  0.93  0.93  0.94  1.03  1.03
                                    ← empty, 0.90 s →
between-turn band  1.93  2.10  2.17  2.21  2.26  2.29  2.34  2.42 …
```

**1.0 sat inside the lower mode**, not between the modes. `score.py` and `cluster.py` now
find the widest empty band in the pooled gaps of the run set being scored and put the
threshold at its midpoint, printing the band it came from — and falling back to the old
constant, saying so, when a set is too small to show a valley (round 6b, three runs, does
exactly this).

**The important result is that it barely matters, and that is the point.** Sweeping the
threshold across the whole plausible range:

| threshold | s01 r6 | s04 r6 | s09 r6 | s01 rerun |
|---|---|---|---|---|
| 0.8 | 6 | 14 | 3 | 4 |
| **1.0 (old)** | **5** | **13** | **3** | **4** |
| 1.2 – 2.2 | 5 | **12** | 3 | 4 |

From 1.2 s to 2.2 s every median is identical: the metric is a plateau, and the derived
threshold sits in the middle of it rather than on its edge. The old constant was the only
value in the range that gave a different answer, and the difference was an inflation.

**What changes:** round six's s04 median goes 13 → **12**, ratio 0.48 → **0.44**. Two
batched calls in two different runs were being counted as separate turns. s01, s09 and the
s01 rerun are unchanged at every threshold, and **round five is unchanged in every
scenario** — recomputed at its own derived 1.43 s it reproduces 2/2/4, 9/10/11, 7/8/11 and
6/7/11 exactly as recorded.

The correction moves a number in this tool's favour, which is why it is derived rather than
chosen. The valley is where it is; the same rule would have moved it the other way had the
data said so, and the plateau means no honest choice within it changes a conclusion.

### `unreached` was wrong about a third category, and the first fix for it was worse

A new invariant pairs `unreached` against `usage`: the first says "symbols under a path
that production code never calls" and prints `no callers`, the second says how many sites
use a symbol. Two commands, one fact.

It fired immediately on `OkoliTyp`, an enum listed as having no production caller while
`usage` reported it at **10 sites** — every one of them the lookup table built from its own
members, ten lines below the definition. Both commands were literally right: a type is
referenced, not called. But `unreached` exists to find deletable code, and a reader acting
on that row deletes live code.

**The first fix was to drop such symbols from the list, and it was wrong.** Applied, it
turned up a second case immediately: `flush`, a *function*, whose single production
reference is a re-export in the package `__init__.py`. Dropping it would have lost a true
positive — `flush` really is uncalled, it just costs two lines to remove instead of one.
The distinction I had drawn, types versus functions, does not survive contact with that:
in both cases the real fact is *there is a reference that breaks when this goes*.

So the rows stay and the count is printed: `no calls, 10 ref(s)` against a bare
`no callers`. The finding is kept, the trap is gone, and the invariant now only flags a row
that claims nothing while `usage` reports sites — a row that states its own reference count
has already told the reader what the other command would.

This is the third category this command has been wrong about (handlers, then constructors,
now anything referenced without being called), and the second time today that a fix had to
be undone in favour of stating the truth rather than filtering it.

**A guard in the check itself was also wrong, and the coverage line caught it.** `usage`
exits 1 for a symbol with no sites — which is the answer this check wants, not a failure.
Treating it as an error meant the check only ever reached an assertion when it was about to
report one, and `16 checks, 15 reached` said so on the next run. The line added this morning
to catch checks that do not run caught one written this afternoon.

**Running total: six real defects, seven false findings**, and the harness is now 16 checks —
16 of 16 exercised on the real corpus, 15 on the fixture.

### `symbol` announced a truncated list and denied truncating it

Found while diagnosing s02 — the one scenario where cairn measured **worse** than grep
(1.43) and which had never been diagnosed. The first command an arm runs there is
`symbol Client`, and it answers:

```
15 matches for "Client" (--limit reached, there may be more)
suppressed: none
```

The header says the list is a first page; the envelope says nothing was left out. That is
the exact contradiction this design exists to prevent, in the command that starts the most
crowded question in the scenario set.

The cause is that the two cuts are different. A *budget* cut happens in the formatter,
which reports it; a `--limit` cut happens in the query, so the rows arrive already
truncated, `shown == rows.len()`, and the branch that reports a cut cannot fire. Fixed: the
envelope now names the `--limit` cut too.

**And the check written for exactly this class could not see it.** `tests/sweep.rs` asserts
that no answer reports a cut while claiming `suppressed: none` — by looking for the strings
`beyond --limit`, `beyond the` and `more references`. `symbol` says
`--limit reached, there may be more`, which is none of them. A list of phrasings maintained
by hand drifts behind the output it describes; both spellings are now in it.

That is two defects from one look: **seven real in the tool, and the second in the harness
itself.** It is also the first concrete lead on s02, whose cost has always been attributed
to "the name floods" — the flood is real, and the tool was telling the arm the flood was
the whole answer.

### s02, the one scenario cairn lost outright, re-measured — 2026-08-05

Round five measured s02 at **1.43**: cairn 10 round trips against grep's 7, on an equal
answer. That is the only place in the record where the tool was worse than the baseline, and
it had never been diagnosed — the cost was attributed to "the name floods" and left there.

Diagnosing it produced the `symbol` defect above: the first command an arm runs there
returned 15 matches, said "there may be more", and declared `suppressed: none`. Re-measured
after that fix, with the question unchanged so round one's grep numbers stand:

| | round five | now |
|---|---|---|
| cairn | [9, 10, 11] med **10** | [5, 5, 6] med **5** |
| grep | [6, 7, 9] med **7** | (unchanged) |
| ratio | **1.43** | **0.71** |

**Every cairn run now beats the grep median**, where before every cairn run lost to it.

Two honest qualifications. First, **three runs cannot resolve s02** — this file's own
bootstrap says it needs seven for a ±1 median, and that has not changed. What three runs
can support is the *direction*: a factor of two is far outside the ±1 band those seven runs
were needed to see. Second, the fix is not the whole cause. Round five's traces show none of
its three runs invoked `for change` at all; these all do, on the first or second turn. Some
of the gain is the tool telling the truth about its own truncation, and some is the arms
using the assembled answer that already existed.

At 0.71 it still fails the acceptance rule, like s01. But it is no longer a loss, and the
claim "cairn is worse than grep somewhere" no longer has an instance.

### CI reaches 16 of 16, and the last gap was two defects rather than a small fixture

`chain followed to where it says` had never run in CI. The stated reason was that the
fixture's only chain is one hop deep. That was true, and it was not the whole reason.

**First: the fixture could not express a second hop at all.** Its Go side calls the alert
client as `a.client.RaiseAlert(...)` — a method on a struct field — and scip-go emits no
call edge for that. The Python side of the same corpus calls its stub through an attribute
and *is* resolved, so the asymmetry is between the two indexers, not between the two call
shapes. The fixture was reproducing D1, the limitation `for understand` documents.

A bounded experiment settled what scip-go will resolve: a package-level function taking the
client as a parameter. The fixture now has one, called from the handler, and SCIP was
regenerated. Checked for leakage from the private corpus afterwards, source and `.scip`
both: clean.

**Second, and this one is mine: `LOCAL_FANOUT` was 4 and 4 was too small.** I chose it
this afternoon with the reasoning that a handler delegating to "one or two helpers" is the
shape being recovered. A handler's callees are not only its helpers — they are every field,
constant and type it touches — and the helper that makes the call is not reliably in the
first four. With the fixture's chain now expressible, 4 still found one hop and 12 found
two. Measured on the real corpus at 12: 0.03–0.14 s against a 10 s ceiling. The cost of
being generous is nothing; the cost of being tight was a silently short chain.

**Third, the same guard mistake as an hour earlier.** The check bailed when
`reaches <target> --outgoing` exited 1 — which is "no targets", the ordinary state of the
far end of a chain and exactly what the check wants to confirm. So it only ever reached an
assertion when a hop had further hops. Caught the same way as last time: the coverage line
reported a check that never ran.

Two of those three are defects in work done today, found because CI's own coverage number
was honest about not running something.

### s02 at the seven runs it was always going to need

The three-run re-measurement above put s02 at 0.71 and said, in the same paragraph, that
three runs cannot resolve this scenario. They could not:

| | runs | median | ratio |
|---|---|---|---|
| round five | [9, 10, 11] | 10 | **1.43** |
| three runs | [5, 5, 6] | 5 | 0.71 |
| **seven runs** | **[5, 5, 5, 6, 6, 7, 8]** | **6** | **0.86** |

**The seven-run number supersedes the three-run one, and it moved against the tool.** 0.71
was a flattering draw from a spread of 5–8; the honest figure is 0.86. That is the bootstrap
section of this file being right about its own scenario, on the scenario it was written for.

Checked before believing it: the four later runs were made while two wide hunts were
saturating the machine, so the obvious suspect was contention stretching gaps and splitting
turns. It is not — the widest within-turn gaps are 0.15–0.86 s in the later batch against
0.58–0.85 s in the earlier one. The spread is the arms, not the machine.

**What stands:** s02 is no longer a loss. Cairn was worse than grep by 43% and is now better
by 14%. **What does not:** 0.86 is nowhere near the acceptance rule's 0.5, and the earlier
0.71 should not be quoted.

The measurement cost is worth recording too. Seven runs to move one scenario from
"unresolved" to "0.86", after four runs were thrown away because the logging hook had been
unregistered as tidying-up minutes before they launched — the same class of self-inflicted
gap this file keeps finding in the tool, in the procedure instead.

---

## The root cause, named: cairn could not tell "I looked and found nothing" from "I did not look" — 2026-08-05

Asked to treat the tool as an agent's *only* source of context — where a falsehood becomes
wrong code that nobody can trace back — the right question stopped being "how many defects"
and became "what shape are they".

Every correctness defect found today is the same shape:

| defect | what it said | what was true |
|---|---|---|
| same-language filter | `0 targets` | the hop existed, in the same language |
| `symbol` truncation | `15 matches`, `suppressed: none` | a first page |
| daemon stale index | `2 modified` | the tree was clean |
| `for understand` bound | "followed to the end" | a floor |
| `LOCAL_FANOUT` 4 | one hop | two |
| `unreached` on a live enum | `no callers` | ten references |
| **the weak layer** | **"no string literal anywhere names this symbol"** | **the layer was never built** |

**They are all the same bug: an absence rendered as a finding.** Not one of them returned a
wrong row. Every one returned a confident *negative* — and a negative is the most dangerous
thing this tool can say, because a negative is what an agent acts on when it decides a
rename is safe, a symbol is dead, or a chain has ended.

### The purest instance, and the worst

`cairn weak` derives the weak-link layer. `cairn index` did not run it, and on this
repository nobody ever had: **45,884 literals recorded, zero edges derived.** From that empty
table, `weaklinks` reported *"no literal in the repo spells this name"* and `for change`
printed, for **every symbol in the repository**:

> no string literal anywhere names this symbol, so nothing reaches it by a name resolved at
> run time

That sentence is the last line an agent reads before concluding a rename is safe. It was a
claim about the world made from a table nobody had filled in.

Built now: **1,236 candidate links across 455 symbols** — every one of which had been getting
the clean bill. The symbol that exposed it, `total_dsti_monthly_impact`, has two: its own
`__all__` entries in `finance/__init__.py:136` and `liability.py:16`. Rename it on the old
answer and the package's export surface breaks silently, plus a regex in `test_no_drift.py`
that spells the name in a string.

### The fix, in three parts

1. **`cairn index` derives the layer.** A pass that must be run by hand is a pass that is
   missing; a missing pass that reports as clean is worse than no pass at all.
2. **The layer records that it was built** (`weak.candidates` in `meta`), so absence and
   emptiness are distinguishable at all.
3. **Every answer that rests on it consults that.** `for change` now prints *"the weak-link
   layer has NOT been built … UNCHECKED - not clean"*, `weaklinks` adds it to `unknown:`,
   and — because a caller must be able to tell without reading prose — `weaklinks` exits
   **3 (degraded)** rather than 1 (nothing found) when the layer is missing.

### What this does not fix, and what comes next

The same distinction has to be made everywhere the tool can return an empty set from a
partial source. `reaches --outgoing` returning nothing, `graph --aspect calls` on a symbol
whose receivers do not resolve, `unreached` on a package the indexer skipped — each is an
absence that currently reads as a finding. The weak layer was the one where the gap was
total; the others are gaps of degree.

The measured evidence that they are gaps of degree: over 26 names, comparing graph
references against whole-word text occurrences in `.py`/`.go`, **26 of 26 had text the graph
did not account for**, by 1 to 17 occurrences. Much of that is comments and homonyms — but
`__all__` entries and a test regex were in there, and those are the ones that break.

**The general rule this establishes:** a negative may only be printed by a component that
can prove it looked. Everything else must say it did not.

### Corroborating the negatives with the text — 2026-08-05

The rule from the section above — *a negative may only be printed by a component that can
prove it looked* — applied where the graph is the thing that failed.

**The measurement first.** Every RPC method name a generated client in the index exposes
(334 of them), searched for as `.Name(` in hand-written, non-generated, non-test code:

- **1 305** RPC-shaped calls, in 193 files
- **375** distinct production functions contain one
- **53 of those 375** get `0 targets` from `reaches --outgoing`

A 14% silent-zero rate on the exact question that command exists to answer. The cause is
D1: `a.client.RaiseAlert(...)` is a call on an unresolved receiver, scip-go emits no edge,
and the graph then reports the absence of its own evidence as a fact about the world.

**The fix is evidence, not a verdict.** When `--outgoing` resolves nothing, the body is read
from the working tree and scanned for names that are RPCs of services this repository
speaks. If any are found the answer says so:

```
the graph resolved no hop, but the body spells 1 name(s) that are RPCs of services this
repository speaks - so this zero is UNCONFIRMED, not clean. Each may be a call the indexer
could not follow, or a local call that happens to share the name; read them:
srcpy/domains/assistant/repository/chat.py:34 calls .begin_operation(, an RPC of
assistant_api.QuotaService
```

**Deliberately not a hop.** Some of those 53 are local calls that merely share a name with
an RPC — `repository/auth.py:49` calls `quota_repo.get_quota_status(...)`, which is a
function, not the RPC of the same name. The tool cannot tell the two apart, so it does not
try: it reports what it saw, where, and hands the judgement to the reader. That is the
border moved rather than guessed at.

Checked both ways: a symbol whose body contains no RPC-shaped call still gets a clean zero
with no added note, so the signal does not fire on everything and become noise.

**What this does not close.** The corroboration is a lower bound too — it only knows the
names in the index's own client artefacts, and only the `.Name(` shape. A service reached
through a hand-written transport, a queue, or a differently-spelled wrapper is still
invisible, and the envelope still says so. The change is that "invisible" no longer prints
as "absent".

### Two more negatives closed, and a third measured and left alone — 2026-08-06

**`unreached` and `outline` over a path the index does not cover.** Asked about
`tools/pbgen`, a directory holding four Python files no indexer has ever read:

```
0 symbols under tools/pbgen with no production caller       [L1, exact]
  (everything here has a production caller)
unknown: none            exit 0
```

That is an affirmative claim — *everything here has a caller* — made about code the index
has never seen, marked exact, with nothing unknown. `outline` was the same shape: `0 of 0
definitions`.

Both now check whether any indexed file matches the prefix, and when none does they say so,
count what the working tree actually holds there, and name the roots the indexers were
pointed at — which is nearly always the explanation:

```
NO FILE under `tools/pbgen` is in this index, so this answer is UNCHECKED rather than
empty - nothing here has been ruled out. The working tree holds 4 indexable file(s)
there. The indexers were pointed at: py=srcpy go=srcgo.
```

Both exit **3 (degraded)** rather than 1, so a caller reading only the exit code cannot
mistake an unindexed path for a clean one. A path the index does cover is unaffected: still
exit 0, no note.

**The third one was measured and deliberately not built.** The candidate was a
graph-versus-text count on `refs`: the tree spells `total_dsti_monthly_impact` 23 times and
the graph accounts for 6. Over 25 single-symbol names, **17 were fully accounted for** once
graph references and weak links were both counted, and the eight that were not turned out to
be an artefact of my own measurement — `GetPhotoAnalysis` looked like "graph 0, code 30"
because it is **9 symbols of that name, 8 of them generated**, and I was comparing one
symbol's references against every occurrence of the word.

So the honest result is that this signal is mostly homonyms and comments, and the part that
does break — `__all__`, string dispatch — is already carried by the weak layer now that it is
built. Adding the note would have been the loud-staleness mistake this codebase already made
once: a number nobody can act on, printed often enough that the section stops being read.

**Measured, found uninformative, not built** — and recorded here so the next person does not
re-derive the idea and reach a different conclusion from a cruder measurement, as I did.

### The last two negatives, and two measurement errors of my own — 2026-08-06

**`path` now corroborates.** "No call path from A to B within N hops" is a negative produced
by the call graph, so the graph cannot vouch for it. It now reads A's body from the tree and
looks for B's name specifically:

```
but [22a]'s own body calls something named `filter` at …/0003_split_draft_profile.py:28,
which the graph resolved no edge for. 34 symbols share that name, so it may be another of
them rather than [d4b2]. Read the line: this is UNRESOLVED, not a proven absence.
```

Note what it does *not* say. `filter(` in a Django migration is a queryset method, not the
indexed symbol named `filter`, and the tool cannot tell — so it reports the name match, the
homonym count, and the line, and asserts nothing. Targeted at one name because the caller
named it: "did you miss *this*" is answerable where "what did you miss" is not.

**`graph --aspect calls` was implemented and then reverted**, on the measurement.

And here the honest part. My first measurement said 12 of 13 empty callee lists name a
symbol the index knows — which would have made this the strongest signal of the three. It
was wrong: the regex matched each function's **own definition line**, so `def compute_f1(`
counted as `compute_f1` being called. Redone with the definition line excluded, the figure
is **3 of 13**, and two of those three are homonyms in Django migrations (`filter`,
`ChatObject`). A signal that fires three times in sixty and is mostly wrong when it does is
the loud-staleness mistake again, so the code came out.

**That is twice in two hours** that a quick measurement over-reported and the fix it argued
for turned out to be unwarranted — the `refs`-versus-text count was the other. Both were
caught by looking at a concrete case before shipping, and both are recorded because the
temptation each time was to trust the aggregate.

### Where the negatives stand now

| negative | before | now |
|---|---|---|
| weak layer unbuilt | "no string literal names this symbol" | built at index time; unbuilt says UNCHECKED, exit 3 |
| `reaches --outgoing` = 0 | "0 targets" | body scanned for RPC names, sites reported |
| `unreached` / `outline`, unindexed path | "everything here has a caller" | NO FILE … is in this index, exit 3 |
| `path` not found | "no call path within N hops" | plus the name match in the source body, with homonym count |
| `graph --aspect calls` empty | "calls nothing" | **unchanged — measured, not worth it** |
| `refs` under-count vs text | N references | **unchanged — measured, not worth it** |

Four closed, two measured and deliberately left. The rule holds in both directions: a
negative must be corroborated, *and* a corroboration that cannot carry its own weight must
not ship.
