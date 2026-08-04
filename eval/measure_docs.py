#!/usr/bin/env python3
"""Comparative run: cost of answering a documentation question, grep vs `cairn docs`.

What is measured: characters entering context. Converted to tokens at 3.7 chars/token,
which is cairn's own constant (cairn-fmt/src/budget.rs), so the accounting is the tool's
rather than one invented for the occasion.

Queries are chosen by a rule, not by hand: the most document-frequent two-word phrases in
the corpus, excluding stopwords, appearing in at least three documents. Terms spread over
several documents are the ones where "which document holds this?" is a real question,
which is the problem being tested.

The baseline is deliberately the most generous version of grep: hit lines, then ONE whole
file - the one with the most hits. A real agent frequently reads more than one. Making
the comparison harder to win is the point.

Not measured, and stated rather than rounded off: whether either arm actually reaches the
answer. Both have the term inside what they read; that is the whole of the equivalence.
"""
import collections
import os
import re
import subprocess
import sys

CHARS_PER_TOKEN = 3.7
REPO = sys.argv[1]
CAIRN = sys.argv[2]
DB = sys.argv[3]

STOP = set("""a an the and or but if then else for of to in on at by with from as is are
was were be been being it its this that these those there here what which who whom how
why when where all any both each few more most other some such no nor not only own same
so than too very can will just should now do does did done have has had having you your
we our they them he she his her i me my one two three can could would may might must
into over under again further once about above below up down out off through during
before after between against because while until also them their have has""".split())

WORD = re.compile(r"[a-z][a-z0-9_.-]+")


def read(p):
    with open(p, encoding="utf-8", errors="replace") as f:
        return f.read()


def docs():
    out = []
    for root, dirs, files in os.walk(REPO):
        dirs[:] = [d for d in dirs if not d.startswith(".")]
        for f in files:
            if f.lower().endswith(".md"):
                out.append(os.path.relpath(os.path.join(root, f), REPO))
    return sorted(out)


# Overlapping, so both "a b" and "b c" are seen, but anchored at word edges: without the
# lookbehind this matched inside words and produced "ing product" and "ring product" as
# separate queries, which is three samples of one phrase and none of them a phrase.
BIGRAM = re.compile(
    r"(?=(?<![a-z0-9_.-])([a-z][a-z0-9_.-]{2,}) ([a-z][a-z0-9_.-]{2,})(?![a-z0-9_.-]))"
)


def pick_terms(paths, n=8):
    """Bigrams by document frequency, reported in two bands.

    Extracted from the raw text with a real space between the words, so every query is a
    phrase that literally occurs. The first version tokenised first and paired adjacent
    tokens, which manufactured phrases like "domains assistant" out of
    `srcpy/domains/assistant` — half the queries then matched nothing at all.

    Two bands, because one corpus turned out to be 61% templated issue reports and
    document frequency alone picked their boilerplate. A phrase in most of the corpus is
    not a "which document holds this?" question — no document holds it — and that was the
    stated reason for using document frequency in the first place. Rather than filter
    those out, which would be choosing the queries after seeing the answer, both bands are
    measured and reported.

      discriminating  in >= 3 documents, and at most 10% of them
      ubiquitous      in more than 25% of documents
    """
    df = collections.Counter()
    tf = collections.Counter()
    for p in paths:
        text = read(os.path.join(REPO, p)).lower()
        seen = set()
        for m in BIGRAM.finditer(text):
            a, b = m.group(1), m.group(2)
            if a in STOP or b in STOP:
                continue
            g = f"{a} {b}"
            tf[g] += 1
            seen.add(g)
        for g in seen:
            df[g] += 1
    total = len(paths)
    disc = [(df[g], tf[g], g) for g in df if 3 <= df[g] <= max(3, total // 10)]
    ubiq = [(df[g], tf[g], g) for g in df if df[g] > total // 4]
    for band in (disc, ubiq):
        band.sort(key=lambda x: (-x[0], -x[1], x[2]))
    return [g for _, _, g in disc[:n]], [g for _, _, g in ubiq[:n]]


def run(args, cwd=None):
    r = subprocess.run(args, capture_output=True, text=True, cwd=cwd or REPO)
    return r.stdout


WINDOW = 20


def grep_arm(term, paths):
    """Two baselines, because one of them flatters the tool being tested.

    `whole`  hit lines, then the single file with the most hits, read in full. What an
             agent does when the hit is in a document it does not know.
    `window` hit lines, then 41 lines around the first hit only. The floor: the least a
             competent agent could get away with, and a much harder thing to beat. If the
             tool only wins against `whole`, the win is mostly "grep made it read a
             42k-word architecture document", which says more about the document.
    """
    hits = run(["grep", "-rn", "--include=*.md", "-i", term, "."])
    per_file = collections.Counter()
    first_line = {}
    for line in hits.splitlines():
        p = line.split(":", 1)[0]
        per_file[p] += 1
        if p not in first_line:
            try:
                first_line[p] = int(line.split(":")[1])
            except (IndexError, ValueError):
                first_line[p] = 1
    if not per_file:
        return len(hits), 0, 0, None
    best = per_file.most_common(1)[0][0]
    lines = read(os.path.join(REPO, best)).splitlines()
    whole = sum(len(l) + 1 for l in lines)
    n = first_line.get(best, 1)
    win = lines[max(0, n - 1 - WINDOW) : n + WINDOW]
    return len(hits), whole, sum(len(l) + 1 for l in win), best


def cairn_arm(term):
    """The tool's answer, then the top-ranked range. Envelope included - it is real cost."""
    out = run([CAIRN, "--db", DB, "docs", "--about", term])
    rng = re.search(r"([^\s]+\.md):(\d+)-(\d+)", out)
    if not rng:
        return len(out), 0, None
    path, a, b = rng.group(1), int(rng.group(2)), int(rng.group(3))
    try:
        lines = read(os.path.join(REPO, path)).splitlines()
    except OSError:
        return len(out), 0, None
    section = "\n".join(lines[a - 1 : b])
    return len(out), len(section), f"{path}:{a}-{b}"


def band(name, terms, paths):
    print(f"\n=== {name} ({len(terms)} queries)")
    header = (f"{'query':<26} {'whole':>8} {'window':>8} {'cairn':>8} "
              f"{'vs whole':>9} {'vs win':>7}")
    print(header)
    print("-" * len(header))
    r_whole, r_win = [], []
    dropped = []
    for t in terms:
        gh, gwhole, gwin, _ = grep_arm(t, paths)
        ch, cb, _ = cairn_arm(t)
        if gh == 0:
            dropped.append(t)
            continue
        a, b, c = gh + gwhole, gh + gwin, ch + cb
        r_whole.append(c / a)
        r_win.append(c / b)
        print(f"{t:<26} {a/CHARS_PER_TOKEN:>8.0f} {b/CHARS_PER_TOKEN:>8.0f} "
              f"{c/CHARS_PER_TOKEN:>8.0f} {(c/a-1)*100:>+8.0f}% {(c/b-1)*100:>+6.0f}%")
    if not r_whole:
        print("  (no query in this band matched)")
        return
    for label, rs in (("whole", r_whole), ("window", r_win)):
        rs = sorted(rs)
        mid = rs[len(rs) // 2]
        print(f"  vs {label:<7} median {mid:.2f} ({(mid-1)*100:+.0f}%)  "
              f"range {min(rs):.2f}..{max(rs):.2f}  "
              f"cairn worse {sum(1 for r in rs if r >= 1.0)}/{len(rs)}  "
              f"rule met {sum(1 for r in rs if r <= 0.5)}/{len(rs)}")
    if dropped:
        print(f"  dropped (grep matched nothing): {', '.join(dropped)}")


def main():
    paths = docs()
    print(f"corpus: {len(paths)} documents, "
          f"{sum(len(read(os.path.join(REPO, p))) for p in paths)} chars")
    disc, ubiq = pick_terms(paths)
    band("discriminating (in 3..10% of documents)", disc, paths)
    band("ubiquitous (in >25% of documents)", ubiq, paths)


main()
