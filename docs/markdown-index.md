# Indexing markdown

Written 2026-08-04. Built as `cairn docs`; unifying the documents, and the handbook that
would describe how, are still open.

## Why now

Markdown in a codebase has changed over the last two years. It used to be a README
somebody wrote once and then let rot. Today it is largely written by agents, and that
means it is **genuinely structured**: headings that mean something, conventions set out in
lists, ADRs with a shape. The structure that used to have to be guessed at is simply
there.

Markdown is also what an agent reads most — conventions, rules, architecture. And it reads
it the most expensive way there is: the whole file, often several files around it, to work
out which one was the right one. The suspicion worth measuring: **more tokens go on
markdown than on code**, and most of them on things the agent never uses.

## What to do about it

**Built as `cairn docs`.** The same thing cairn does with code: progressive disclosure.
Not handle → skeleton → body, but **document → section → line range**.

```
cairn docs                       the map: every markdown file, its title, its size in
                                 words, and its top-level headings
cairn docs <path.md>             that document's sections, each with a range and what
                                 it costs to read
cairn docs --about "<words>"     sections that name it (`about`) and sections that
                                 mention it (`3x`)
```

Every answer is `path:start-end`. That is the product: the agent reads thirty lines, not
four files. On cairn's own documentation it looks like this — `docs/architecture.md` is
tens of thousands of words, and `cairn docs docs/development.md` comes back with six
sections, the one being looked for weighing 233:

```
  Working on cairn                    725w  docs/development.md:1-100
    Building                          233w  docs/development.md:6-39
      Checking the Windows build ...   93w  docs/development.md:26-39
```

### Two things it can do that grep cannot

**The range.** grep returns a line in a seven-thousand-word document and leaves the reader
to guess how much around it to take. That guess is exactly what turns one question into
four files.

**The difference between "is about it" and "mentions it".** A heading that names the thing
means the section **is** the answer. Words in the body mean it comes up there. grep cannot
tell those apart, because it does not know where a section begins or ends. A mention is
also attributed to the **smallest** section containing it — handing back the chapter would
mean handing back four hundred lines to answer a question that lives in twelve.

### What is stored

Headings, ranges, word counts. **Never the prose.** Bodies are read from disk at query
time for `--about` — ten documents are a few hundred kilobytes and reading them costs
milliseconds, whereas a copy of the prose in the index would be a second, staler copy of
something already on disk.

The parser is not a renderer: ATX headings, aware of fenced blocks and front matter.
Setext underlines are not handled — `---` is also a thematic break and a front-matter
fence, and guessing between them would put invented sections into a map whose only job is
to be trusted.

Limits worth saying out loud: `--about` is a case-insensitive substring. It finds **where
a subject is written about**, not what is said about it, and it does not match synonyms.

## The other half: unify the documents

If documents of the same type share a skeleton, an agent learns to reach for them the same
way — and stops reading three files to find a fourth. That, though, is **not cairn's job**;
it is an optimisation of a particular repository.

Where it belongs: in a handbook on *getting the most out of cairn*. cairn can say which
shape of document it can exploit best, and a repository can move towards it. The same
logic as `.cairn/rules.yaml` — the tool describes the convention it reads and leaves the
project to decide whether to adopt it.

## Measured

The comparative run is in [`eval/RESULTS.md`](../eval/RESULTS.md), the harness in
`eval/measure_docs.py`. Measured on two corpora — cairn's own documentation (10 files,
193k characters) and a private repository (205 files, 1.6M characters). In summary:

- against an agent that **reads the whole document**: median **0.12–0.21 (−79 to −88%)**,
  the acceptance rule met almost everywhere
- against an agent that reads **only a ±20-line window around the hit**: median
  **0.68–0.89 (−11 to −32%)**, the acceptance rule met in a minority of cases

**The larger corpus did better, not worse.** The worry was that ten documents is few
enough for `grep -rn` to already answer "which file", so the win would collapse on a real
repository. It went the other way: against the hard baseline the median improved from 0.89
to 0.68 as the corpus grew twentyfold. Two corpora are not a trend, but it is the opposite
of the feared direction.

Where it loses: **phrases spread across a quarter of the corpus.** Those have no single
home, and "which document holds this?" has no answer for them.

What an agent actually reads was not measured — both arms are fixed strategies — nor was
answer quality, which the acceptance rule distinguishes. Two earlier runs are **withdrawn**
and stay readable in `RESULTS.md`, together with why they were wrong.
