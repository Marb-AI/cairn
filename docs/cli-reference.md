# CLI reference

Every command, every flag, as the binary itself reports them.

**Generated** — do not edit by hand. Rebuild with:

```
docker compose run --rm ci bash docs/gen-cli-reference.sh > docs/cli-reference.md
```

The intended reader is an agent: it is exhaustive rather than friendly, and it assumes the
shape of the tool is already known. For that, start with the [README](../README.md), and
for when *not* to reach for a command at all, [`skill/SKILL.md`](../skill/SKILL.md).

## Two things that apply everywhere

**Exit codes are part of the contract.** `0` found, `1` nothing found, `2` bad query or an
unusable index, `3` degraded — the index is there but cannot be trusted for this answer.
An agent that treats a confident `0` over a broken index as an answer is the failure this
distinction exists to prevent.

**Every answer ends with an envelope**: `suppressed:` what was cut to fit the budget,
`unknown:` what the mechanism cannot see, `stale:` what has changed since indexing. A
section reading `none` is a claim; a missing section is a bug.

## Machine-readable output

`--format json` prints the same answer as one `cairn.answer/1` object, with the envelope
as fields rather than as lines. Text is the default and stays the primary format for an
agent: braces and repeated keys cost several times the tokens of an aligned table, and
cheaper context is the point. JSON is for a program — a wrapper, an editor, a check in CI.

```
{
  "schema": "cairn.answer/1",
  "command": "refs",
  "rows": 10,
  "truncated": false,
  "unknown": [],
  "suppressed": [],
  "stale": { "tracked": false, "dirty": [], "mentioned": ["srcgo/..."], "notes": ["..."] },
  "body": "...the same text as above the envelope lines...",
  "data": null
}
```

What holds across both formats:

* **Exit codes are identical.** The code is the contract; it cannot depend on how the
  answer was printed.
* **The envelope is always present.** An empty `unknown` means none; a missing one would
  be the same silent error a missing `unknown:` line is. `stale.tracked` is the
  distinction a bare empty list cannot make: `false` means nothing was watching the tree,
  not that nothing had changed.
* **`body` is the text answer** without the three envelope lines, since those are fields
  here. `--budget` cuts a JSON answer exactly as it cuts a text one, and what it cut is
  in `suppressed`.
* **`data`** is the answer as fields. It is `null` for commands that still have only a
  text renderer; the envelope around it is there either way.

## cairn

```
Local code navigation for agents

Usage: cairn [OPTIONS] <COMMAND>

Commands:
  index        Index the repository you are standing in
  skill        Install the agent guide into this repository's .claude/skills/
  for          Say what you are doing; cairn picks how to answer it
  context      Entry point by concept: turn "the OAuth stuff" into symbols to start from
  unreached    Symbols under a path that production code never calls
  outline      What a module or directory contains, and how used each thing is
  usage        Where a symbol is used, grouped by file - the blast radius of changing it
  symbol       Find symbols by name
  refs         Show references to a symbol
  graph        Walk the call graph or implementation relations
  rules        The conventions cairn reads the world with, and where they came from
  config       Show or change cairn's own settings
  topology     Deployed services and what each one runs
  entrypoints  Every way code gets started: start commands, cron entries, on-demand runners
  affects      Every deployed service a change here touches, in-process and over the network
  runs         Which deployed services can run this code - the filesystem cannot say
  reaches      Who reaches this across a gRPC boundary - the query no name search can answer
  path         Shortest call path between two symbols: how does one reach the other
  expand       Show a symbol in more detail
  weak         Build the weak-link layer: string literals that name a symbol
  weaklinks    Sites whose string literals name this symbol - candidate dynamic references
  verify       Report what the index does NOT know, and whether it still matches the repo
  link         Record a link the static pass cannot see
  links        Hand-authored links touching a symbol
  concept      Named nodes that are not symbols, and their links to code
  daemon       Run the live-state daemon: watches the repo and reports what has changed
  live         Symbols in a file as the language server sees it *now* - the dirty overlay. Answers about a changed file that the index cannot
  literal      Find a string literal, and get whose function it sits in
  docs         The documentation map: which document holds what, and what each costs to read
  status       What is indexed, and how stale it is
  llm          Claims the index cannot check about itself, put to whoever is reading
  help         Print this message or the help of the given subcommand(s)

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
  -V, --version          Print version
```

## cairn index

```
Index the repository you are standing in.

Takes nothing in the normal case: the working directory is the repository, the languages are whatever the tree actually contains, and the indexers are run for you. Passing .scip files instead skips that and ingests them directly.

Usage: cairn index [OPTIONS] [INDEXES]...

Arguments:
  [INDEXES]...
          Ingest these .scip files rather than producing any. Rarely needed

Options:
      --db <DB>
          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory

      --repo <REPO>
          Repository root. Defaults to the working directory

      --budget <BUDGET>
          Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again

      --without-skill
          Do not install the agent guide into .claude/skills/

      --format <FORMAT>
          Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both
          
          [default: text]
          [possible values: text, json]

  -h, --help
          Print help (see a summary with '-h')
```

## cairn skill

```
Install the agent guide into this repository's .claude/skills/.

Done for you by `cairn index`; this is here for the case where that was skipped, or where the guide has moved on since.

Usage: cairn skill [OPTIONS]

Options:
      --db <DB>
          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory

      --budget <BUDGET>
          Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again

      --format <FORMAT>
          Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both
          
          [default: text]
          [possible values: text, json]

  -h, --help
          Print help (see a summary with '-h')
```

## cairn for

```
Say what you are doing; cairn picks how to answer it.

The command surface below this one is named after mechanisms - `refs`, `graph`, `path`, `usage` - and measurement showed that is the wrong shape for the moment an agent asks. It knows its purpose reliably and picks the mechanism badly: one run spent eight calls across four commands on a question no symbol command answers, and on another the mechanism that fits (`path`) needs both ends of a chain when the whole question is what the far end is.

So: purpose first. Every block of the answer names the command that produced it, which is the way down to the mechanism when this shape is not what you needed.

Usage: cairn for [OPTIONS] <PURPOSE> <SUBJECT>

Arguments:
  <PURPOSE>
          Possible values:
          - change:     I am going to modify this symbol. What breaks, and how far does it reach?
          - understand: I am following this through. What does it call, and where does the chain land?
          - find:       Where is this text - a value, a key, a header, a name - and whose line is it?

  <SUBJECT>
          A handle, a symbol name, or - for `find` - any text

Options:
      --db <DB>
          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory

      --limit <LIMIT>
          [default: 60]

      --budget <BUDGET>
          Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again

      --repo <REPO>
          Repository root. Defaults to the working directory

      --all
          `find`: list every hit, including the ones in test and generated files that a large answer otherwise reports as a count

      --format <FORMAT>
          Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both
          
          [default: text]
          [possible values: text, json]

  -h, --help
          Print help (see a summary with '-h')
```

## cairn context

```
Entry point by concept: turn "the OAuth stuff" into symbols to start from

Usage: cairn context [OPTIONS] <QUERY>

Arguments:
  <QUERY>  

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --limit <LIMIT>    [default: 12]
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn unreached

```
Symbols under a path that production code never calls

Usage: cairn unreached [OPTIONS] <PREFIX>

Arguments:
  <PREFIX>  Repo-relative path prefix, e.g. srcpy/domains/orders/lib/pricing

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --limit <LIMIT>    [default: 60]
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn outline

```
What a module or directory contains, and how used each thing is

Usage: cairn outline [OPTIONS] <PREFIX>

Arguments:
  <PREFIX>  

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --limit <LIMIT>    [default: 80]
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn usage

```
Where a symbol is used, grouped by file - the blast radius of changing it

Usage: cairn usage [OPTIONS] <HANDLE>

Arguments:
  <HANDLE>  

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --include-tests    
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --limit <LIMIT>    [default: 40]
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn symbol

```
Find symbols by name

Usage: cairn symbol [OPTIONS] <QUERY>

Arguments:
  <QUERY>  

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --limit <LIMIT>    [default: 15]
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --repo <REPO>      Repository root, for corroborating a miss against the working tree. Derived from the index location when it can be confirmed, so it is rarely needed
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn refs

```
Show references to a symbol

Usage: cairn refs [OPTIONS] <HANDLE>

Arguments:
  <HANDLE>  

Options:
      --db <DB>            Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --include-generated  
      --budget <BUDGET>    Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --limit <LIMIT>      [default: 40]
      --context <CONTEXT>  How much source to show at each site: none | line | block | <n> | auto. `auto` divides --budget by the number of sites, so few sites get a block and many get a line. Needs --repo. Far cheaper than opening the files [default: none]
      --format <FORMAT>    Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
      --repo <REPO>        
  -h, --help               Print help
```

## cairn graph

```
Walk the call graph or implementation relations

Usage: cairn graph [OPTIONS] <HANDLE>

Arguments:
  <HANDLE>
          

Options:
      --aspect <ASPECT>
          Possible values:
          - callers: Who calls this symbol
          - calls:   What this symbol calls
          - impls:   Implementations of this interface, or what this type implements
          - tests:   Tests that reach this symbol through the call graph
          
          [default: callers]

      --db <DB>
          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory

      --budget <BUDGET>
          Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again

      --depth <DEPTH>
          How many hops out from the root
          
          [default: 2]

      --fanout <FANOUT>
          How many neighbours to follow per node
          
          [default: 8]

      --format <FORMAT>
          Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both
          
          [default: text]
          [possible values: text, json]

      --view <VIEW>
          Layout: `tree` shows how each node was reached, `list` is flat and cheaper
          
          [default: tree]

      --detail <DETAIL>
          How much of each node to print: skeleton | signature | doc | body. Anything but `skeleton` needs --repo. Use `body` for audit passes
          
          [default: skeleton]

      --exclude-tests
          Skip nodes defined in test files. The question behind "who uses this" is usually whether anything in production does

      --repo <REPO>
          Repo root, required when --detail prints source

  -h, --help
          Print help (see a summary with '-h')
```

## cairn rules

```
The conventions cairn reads the world with, and where they came from.

What a start command looks like, how a protobuf generator names things, what marks a file as generated, where tests live. Copy the output to `.cairn/rules.yaml` and edit it to change any of them without rebuilding (architecture D16).

Usage: cairn rules [OPTIONS]

Options:
      --db <DB>
          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory

      --budget <BUDGET>
          Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again

      --format <FORMAT>
          Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both
          
          [default: text]
          [possible values: text, json]

  -h, --help
          Print help (see a summary with '-h')
```

## cairn config

```
Show or change cairn's own settings.

These belong to the installation rather than to any repository, so they live beside the binary and one setting serves every checkout on the machine.

Usage: cairn config [OPTIONS] [ASSIGNMENT]

Arguments:
  [ASSIGNMENT]
          A setting to change, as `key=value`. Without one, prints what is in effect. `key=unset` restores the default

Options:
      --db <DB>
          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory

      --budget <BUDGET>
          Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again

      --format <FORMAT>
          Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both
          
          [default: text]
          [possible values: text, json]

  -h, --help
          Print help (see a summary with '-h')
```

## cairn topology

```
Deployed services and what each one runs

Usage: cairn topology [OPTIONS]

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn entrypoints

```
Every way code gets started: start commands, cron entries, on-demand runners.

One row per entrypoint rather than per service, each ending in the file it lands in - `cairn outline <that path>` is the way down from here. With --reaches it answers the audit direction instead: which of them can actually run this symbol.

Usage: cairn entrypoints [OPTIONS]

Options:
      --db <DB>
          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory

      --reaches <REACHES>
          Only entrypoints from which this symbol can be run

      --budget <BUDGET>
          Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again

      --format <FORMAT>
          Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both
          
          [default: text]
          [possible values: text, json]

  -h, --help
          Print help (see a summary with '-h')
```

## cairn affects

```
Every deployed service a change here touches, in-process and over the network.

One call instead of `runs` plus `reaches` per hop plus `topology`: measurement showed the cost of this question was in assembling those by hand, not in asking.

Usage: cairn affects [OPTIONS] <HANDLE>

Arguments:
  <HANDLE>
          

Options:
      --db <DB>
          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory

      --depth <DEPTH>
          [default: 12]

      --budget <BUDGET>
          Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again

      --fanout <FANOUT>
          [default: 200]

      --format <FORMAT>
          Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both
          
          [default: text]
          [possible values: text, json]

  -h, --help
          Print help (see a summary with '-h')
```

## cairn runs

```
Which deployed services can run this code - the filesystem cannot say

Usage: cairn runs [OPTIONS] <HANDLE>

Arguments:
  <HANDLE>  

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --depth <DEPTH>    [default: 12]
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn reaches

```
Who reaches this across a gRPC boundary - the query no name search can answer

Usage: cairn reaches [OPTIONS] <HANDLE>

Arguments:
  <HANDLE>  

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --outgoing         Show what this symbol reaches instead of what reaches it
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn path

```
Shortest call path between two symbols: how does one reach the other

Usage: cairn path [OPTIONS] <FROM> <TO>

Arguments:
  <FROM>  
  <TO>    

Options:
      --db <DB>                Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --max-depth <MAX_DEPTH>  [default: 8]
      --budget <BUDGET>        Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --detail <DETAIL>        [default: skeleton]
      --format <FORMAT>        Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
      --repo <REPO>            
  -h, --help                   Print help
```

## cairn expand

```
Show a symbol in more detail

Usage: cairn expand [OPTIONS] <HANDLE>

Arguments:
  <HANDLE>  

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --detail <DETAIL>  `skeleton` = identity only, `doc` = leading comment, `body` = source text [default: skeleton]
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --repo <REPO>      Repo root, needed for `--detail body|doc`
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn weak

```
Build the weak-link layer: string literals that name a symbol

Usage: cairn weak [OPTIONS] --repo <REPO>

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --repo <REPO>      Repo root; file paths in the index are relative to it
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn weaklinks

```
Sites whose string literals name this symbol - candidate dynamic references

Usage: cairn weaklinks [OPTIONS] <HANDLE>

Arguments:
  <HANDLE>  

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --limit <LIMIT>    [default: 30]
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn verify

```
Report what the index does NOT know, and whether it still matches the repo

Usage: cairn verify [OPTIONS]

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --repo <REPO>      Repo root. Without it staleness cannot be checked and the report says so
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --flag-stale       Mark hand-authored links whose anchor file has changed
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn link

```
Record a link the static pass cannot see

Usage: cairn link [OPTIONS] --note <NOTE> <FROM> <TO>

Arguments:
  <FROM>  
  <TO>    

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --note <NOTE>      Why this link exists. Required: an unexplained assertion is unreviewable
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --by <BY>          Who is asserting it [default: agent]
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn links

```
Hand-authored links touching a symbol

Usage: cairn links [OPTIONS] <HANDLE>

Arguments:
  <HANDLE>  

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn concept

```
Named nodes that are not symbols, and their links to code

Usage: cairn concept [OPTIONS] <COMMAND>

Commands:
  add   Create or update a concept
  link  Attach a symbol to a concept
  show  Show a concept and everything attached to it
  list  List concepts, optionally in one namespace
  drop  Discard a whole namespace in one move
  help  Print this message or the help of the given subcommand(s)

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn daemon

```
Run the live-state daemon: watches the repo and reports what has changed

Usage: cairn daemon [OPTIONS] --repo <REPO>

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --repo <REPO>      
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --stop             Stop a running daemon instead of starting one
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn live

```
Symbols in a file as the language server sees it *now* - the dirty overlay. Answers about a changed file that the index cannot

Usage: cairn live [OPTIONS] <PATH>

Arguments:
  <PATH>  

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn literal

```
Find a string literal, and get whose function it sits in.

The thing SCIP cannot carry, because a literal is not a symbol: a header name, a dict key, a feature flag. grep finds the line faster and is never stale — what it cannot say is whose line it is, which is the question you were going to ask next.

The surrounding source comes back by default. Asking for it separately would cost an inference, and an inference costs more than everything this command does.

Usage: cairn literal [OPTIONS] <TEXT>

Arguments:
  <TEXT>
          Text to look for inside string literals. Case-insensitive substring

Options:
      --context <CONTEXT>
          How much source at each site: none | line | block | <n>
          
          [default: block]

      --db <DB>
          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory

      --budget <BUDGET>
          Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again

      --repo <REPO>
          Repo root. Worked out from the index's own location when omitted, which is right whenever the index sits at the conventional `<repo>/.cairn/`

      --format <FORMAT>
          Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both
          
          [default: text]
          [possible values: text, json]

      --limit <LIMIT>
          [default: 30]

  -h, --help
          Print help (see a summary with '-h')
```

## cairn docs

```
The documentation map: which document holds what, and what each costs to read.

Headings and line ranges, never the prose. With no argument it lists the documents; with a path it gives that one's section skeleton; with --about it finds the sections whose heading, or where that heading sits, names your words.

The answer is always a line range, so the next step is reading thirty lines rather than four files. For a word inside the prose, grep is still the tool.

Usage: cairn docs [OPTIONS] [PATH]

Arguments:
  [PATH]
          A markdown path, for that document's sections

Options:
      --about <ABOUT>
          Sections whose heading or trail names this

      --db <DB>
          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory

      --budget <BUDGET>
          Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again

      --format <FORMAT>
          Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both
          
          [default: text]
          [possible values: text, json]

  -h, --help
          Print help (see a summary with '-h')
```

## cairn status

```
What is indexed, and how stale it is

Usage: cairn status [OPTIONS]

Options:
      --db <DB>          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory
      --budget <BUDGET>  Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again
      --format <FORMAT>  Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both [default: text] [possible values: text, json]
  -h, --help             Print help
```

## cairn llm

```
Claims the index cannot check about itself, put to whoever is reading.

No model is called: cairn is a CLI an agent drives, and the agent is the one that can go and look. With no arguments it prints the claims that need judging, each with the evidence and what would falsify it. `--check <id>` records the answer.

Advisory throughout. Nothing here changes an exit code or blocks a command - a deterministic tool that refused to work because a judgement disagreed would have traded away the thing that makes it worth trusting.

Usage: cairn llm [OPTIONS] <COMMAND>

Commands:
  verify  List the claims that need judging, or record a verdict on one
  help    Print this message or the help of the given subcommand(s)

Options:
      --db <DB>
          Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite at or above the working directory

      --budget <BUDGET>
          Ceiling on the size of the answer, in tokens. The tool fills it with the highest-ranked rows and reports what it left out, so you do not have to guess a --limit and then ask again

      --format <FORMAT>
          Print the answer as text for a reader or as JSON for a program. Exit codes are the same in both
          
          [default: text]
          [possible values: text, json]

  -h, --help
          Print help (see a summary with '-h')
```
