#!/usr/bin/env python3
"""Account for every mention of a symbol's name in the tree, or say which ones you cannot.

The other half of the reliability question. `stress.py` asks whether two commands
contradict each other; this asks whether a *complete-looking* list is complete — the
"five of six, and you lose one" failure, which contradicts nothing and is invisible to
every check in that file.

The method is only sound for a name carried by **exactly one symbol in the whole index**.
With a homonym, a textual mention may belong to the other symbol and nobody can say which,
so the question is not answerable and the symbol is skipped rather than guessed at. On the
target repository that is 8,646 of 11,396 hand-written types and functions — 76%, and the
24% is a stated limit of the proof, not of the tool.

Every mention is put in exactly one bucket:

  accounted    `cairn refs` lists that file and line
  string       inside a quoted literal, or already reported by `cairn weaklinks`
  attribute    `x.name` — instance attribute access, which the index documents as
               unresolved and tells the caller to grep for
  unexplained  none of the above

`unexplained` is the answer to "where is the potential fake info". Measured on this
repository it is dominated by keyword arguments and field declarations that share the
symbol's name — `Card(recommended_switch_year=...)`. Those are not references and a
reference list is not wrong to omit them, but **a rename breaks them**, so a caller who
reads the list as "everything that changes" is misled.

Usage: completeness.py <db> <repo> [sample-size]
"""

import random
import re
import sqlite3
import subprocess
import sys
import os

BIN = os.environ.get("CAIRN_BIN", "/home/workspaces/cairn/bin/cairn-bin")
LINE = re.compile(r"\s+(\S+\.(?:py|go)):(\d+)")


def run(db, *args):
    return subprocess.run(
        [BIN, "--db", db, *args], capture_output=True, text=True, timeout=120
    ).stdout


def sites(text):
    """`path:line` pairs a listing printed, whatever else is on the line."""
    return {(m.group(1), int(m.group(2))) for m in map(LINE.match, text.splitlines()) if m}


def unambiguous(db, n):
    """Names carried by exactly one symbol anywhere, sampled deterministically.

    `count(DISTINCT s.id) = 1` over the *whole* index, generated code included: a
    protobuf stub sharing the name is exactly the case that makes a mention ambiguous.
    """
    c = sqlite3.connect(db)
    rows = [
        r
        for r in c.execute(
            """SELECT n.s, min(h.handle) FROM symbols s
                 JOIN strings n ON n.id = s.name_id
                 JOIN handles h ON h.symbol_id = s.id
                WHERE s.kind IN (1, 3) AND length(n.s) >= 8
                GROUP BY n.s HAVING count(DISTINCT s.id) = 1"""
        )
    ]
    random.Random(20260806).shuffle(rows)
    return rows[:n]


def classify(db, repo, name, handle):
    """One symbol's mentions, bucketed. Returns (counts, unexplained rows)."""
    c = sqlite3.connect(db)
    graph = sites(run(db, "refs", handle, "--include-generated", "--limit", "900"))
    weak = sites(run(db, "weaklinks", handle))
    row = c.execute(
        """SELECT p.s, s.def_line FROM symbols s
             JOIN handles h ON h.symbol_id = s.id
             JOIN files f ON f.id = s.def_file_id
             JOIN strings p ON p.id = f.path_id
            WHERE h.handle = ?""",
        (handle,),
    ).fetchone()
    out = {"accounted": 0, "string": 0, "attribute": 0, "unexplained": 0}
    lost = []
    grep = subprocess.run(
        ["grep", "-rnw", "--include=*.py", "--include=*.go", name, repo],
        capture_output=True,
        text=True,
    ).stdout
    for line in grep.splitlines():
        try:
            path, ln, txt = line.split(":", 2)
        except ValueError:
            continue
        rel = path.replace(repo.rstrip("/") + "/", "")
        ln = int(ln)
        # The definition is not a reference to itself.
        if row and rel == row[0] and ln == row[1] + 1:
            continue
        stripped = txt.strip()
        if stripped.startswith(("#", "//", "*")):
            continue
        key = (rel, ln)
        if key in graph:
            out["accounted"] += 1
            continue
        before = txt[: txt.find(name)]
        # An odd number of quotes before the name means it sits inside one.
        if key in weak or (before.count('"') + before.count("'")) % 2 == 1:
            out["string"] += 1
        elif before.rstrip().endswith("."):
            out["attribute"] += 1
        else:
            out["unexplained"] += 1
            lost.append((name, rel, ln, stripped[:70]))
    return out, lost


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    db, repo = sys.argv[1], sys.argv[2]
    n = int(sys.argv[3]) if len(sys.argv) > 3 else 40
    names = unambiguous(db, n)
    total = {"accounted": 0, "string": 0, "attribute": 0, "unexplained": 0}
    lost = []
    for name, handle in names:
        counts, rows = classify(db, repo, name, handle)
        for k in total:
            total[k] += counts[k]
        lost.extend(rows)
    seen = sum(total.values())
    if not seen:
        print("no mentions found - is the repo path right?")
        return 1
    print(f"{len(names)} symbols whose name is carried by exactly one symbol in the index")
    print(f"{seen} code mentions of them (comments and the definition line excluded)\n")
    for k in ("accounted", "string", "attribute", "unexplained"):
        print(f"  {k:12} {total[k]:>5}  {100 * total[k] / seen:5.1f}%")
    print(
        f"\n`accounted` is what `cairn refs` listed. `string` is covered by `weaklinks`.\n"
        f"`attribute` is the instance-attribute limit the tool documents and tells you to\n"
        f"grep for. `unexplained` is the risk: mostly keyword arguments and field\n"
        f"declarations sharing the name, which a reference list is not wrong to omit and\n"
        f"which a rename still breaks."
    )
    if lost:
        print(f"\nfirst {min(10, len(lost))} unexplained:")
        for name, path, ln, txt in lost[:10]:
            print(f"   {name}  {path}:{ln}\n      {txt}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
