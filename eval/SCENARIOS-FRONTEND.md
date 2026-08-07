# Frontend scenarios: an agent with cairn against an agent with grep

Pre-registered, on the same terms as `SCENARIOS.md`: **everything below was written before
any arm was run**, including the answer keys and the per-scenario prediction. Results go in
`RESULTS.md`. This file is not edited to match them.

The corpus is a TypeScript monorepo — a pnpm workspace of three Expo apps, 1,059 source
files, indexed as one index of 28,726 symbols. It is the first corpus cairn was not built
against.

## Why this set exists

Cairn's two measured winning classes on the backend were a cross-language gRPC boundary and
a deployment topology. **A frontend has neither.** The prediction on record before these
runs is therefore that the median here is *worse* than the backend's 0.50, and that what
survives is ordinary navigation. A set chosen to avoid that would not be a measurement.

Same protocol as `SCENARIOS.md`: round trips as the unit, batched calls count as one,
three runs per arm, medians with spread, and the acceptance rule unchanged — same answer
→ at most half the round trips or half the wall clock; better answer → no more than the
baseline; **a worse answer fails at any price**.

Two differences from the backend set, both stated because they are real:

- **No stale-index scenario.** The index is rebuilt immediately before the runs.
- **The grep arm keeps its larger toolkit** (`grep`, `rg`, `find`, `sed`, `awk`, Read); the
  cairn arm has `cairn` and Read. That runs in the baseline's favour, as before.

## Scenarios

Three: one where cairn is expected to win, one where grep is, one where neither has an
obvious edge. The split is deliberate.

### F1 — a multi-hop trace through generated code (cairn expected)

> In the rixby app, the API call that deletes a chat: what wraps it, and where in the UI is
> it used?

**Why cairn might win.** Three things have to go right at once. The call is defined in
generated code; it is wrapped by *more* generated code; and the same name is defined again
in a second app in the same workspace, so a text search answers about two codebases at
once. This is the frontend's nearest thing to the gRPC boundary the tool wins on.

**Key.**

| step | where |
|---|---|
| the call | `apps/rixby/api/sdk.gen.ts:2363` — `deleteChat`, generated |
| the wrapper | `apps/rixby/api/@tanstack/react-query.gen.ts:2692` — `deleteChatMutation`, generated, calls `deleteChat` at :2705 |
| the use | `apps/rixby/components/chat/Chat.tsx:589` — `...deleteChatMutation()`, imported at :4 |

An answer that names `apps/marb_old` as though it were part of this app's flow is **worse**,
not merely longer: `marb_old` is a retired app, excluded from the workspace, and a change
made there does nothing. An answer that stops at the generated wrapper without reaching
`Chat.tsx` is worse. Naming all three, in the rixby app, is *same*.

### F2 — a string that is not a symbol (grep expected)

> What does the help button in the Intercom bubble say in English and Czech, and which
> component renders it?

**Why grep should win.** The subject is a translation key. It lives in a JSON file no
indexer reads, it is passed to `t(...)` as a string literal, and there is no symbol
anywhere for a call graph to have an edge to. Cairn's text search reads the tree and should
find it; it is not obvious what the index adds, and the guide tells the agent as much.

**Key.**

| | |
|---|---|
| key | `intercom.openLabel` |
| English | `Help & bug reports` — `apps/rixby/locales/en/translation.json` |
| Czech | `Nápověda a hlášení chyb` — `apps/rixby/locales/cs/translation.json` |
| rendered by | `apps/rixby/components/Intercom/IntercomBubble.tsx:169` |

Naming the sibling key `intercom.openLabelUnread` (:165) as well is *same*, not better —
it is adjacent and either arm trips over it.

### F3 — an ordinary reference question (neither expected)

> I want to change what the `useChatFilters` hook returns. Which components would I have to
> update?

**Why it is here.** This is the bulk of what anyone actually asks a code-navigation tool,
and the name is distinctive enough that a single `grep -rn` answers it. It is included
because a set of only edge cases says nothing about the ordinary day, and because s01 —
the same shape on the backend — is the scenario cairn loses.

**Key.** Defined at `apps/rixby/hooks/useChatFilters.ts:22`. Four components destructure
its return value:

| component | call |
|---|---|
| `components/chat/Chat.tsx` | :887 |
| `components/chat/ChatWidget.tsx` | :533 |
| `components/chat/ChatSearch.tsx` | :40 |
| `components/chat/ChatSummaryAside.tsx` | :521 |

Listing the import lines as well is *same*. Missing any of the four is **worse**. Including
`apps/marb_old` is worse for the reason given in F1.

## Prediction, recorded before the runs

- **F1**: cairn wins. The generated-code hop and the cross-app namesake are both things a
  name search cannot resolve without reading.
- **F2**: grep wins or ties. There is no graph to use.
- **F3**: grep wins or ties, as s01 does.

**Overall: no better than the backend's median, and plausibly worse.** If that is what the
runs show, the honest conclusion is that cairn's value is concentrated in codebases with a
service boundary — which is a finding about where to use it, not a failure of it.
