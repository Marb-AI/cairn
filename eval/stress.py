#!/usr/bin/env python3
"""Drive cairn against itself and report where it contradicts itself.

Every defect this project found by measurement had the same shape: two commands
disagreeing about the same fact. `affects` on a handler class said one deployed service
where its own methods said three. `usage` said two call sites where `graph --aspect
callers` said four. `reaches` was exact in one direction and empty in the other. An
attribution named the enclosing class where `literal` named the method.

None of those needed an agent. They needed one command's answer compared to another's,
over enough symbols that the disagreement shows up — which is a shell loop, costs nothing,
and is exact rather than sampled. Agent runs are for round trips, the metric that needs a
model in the loop. Correctness does not.

Symbols are chosen by a rule, not by hand: a deterministic stratified sample from the
index, so a later run covers the same ground and a new failure is a change rather than a
different draw.

Usage: stress.py <db> <repo> [sample-size]
"""

import json
import random
import sqlite3
import subprocess
import sys

BIN = "/home/workspaces/cairn/bin/cairn-bin"


def run(db, *args):
    out = subprocess.run(
        [BIN, "--db", db, *args], capture_output=True, text=True, timeout=120
    )
    return out.returncode, out.stdout, out.stderr


def sample(db, n):
    """A stratified, deterministic draw: handler types, methods, plain functions, both
    languages. Ordered by hash so the same index always yields the same set."""
    c = sqlite3.connect(db)
    picks = []
    strata = {
        "handler types": """SELECT DISTINCT h.handle FROM service_links l
                              JOIN symbols s ON s.id = l.symbol_id AND s.kind = 1
                              JOIN handles h ON h.symbol_id = s.id
                             WHERE l.role = 0 ORDER BY h.handle""",
        "py functions": """SELECT h.handle FROM symbols s
                             JOIN handles h ON h.symbol_id = s.id
                             JOIN files f ON f.id = s.def_file_id AND f.generated = 0
                            WHERE s.lang = 1 AND s.kind = 3 AND s.ref_count > 2
                            ORDER BY h.handle""",
        "go functions": """SELECT h.handle FROM symbols s
                             JOIN handles h ON h.symbol_id = s.id
                             JOIN files f ON f.id = s.def_file_id AND f.generated = 0
                            WHERE s.lang = 2 AND s.kind = 3 AND s.ref_count > 2
                            ORDER BY h.handle""",
    }
    rng = random.Random(20260805)
    for label, q in strata.items():
        rows = [r[0] for r in c.execute(q)]
        take = rows if len(rows) <= n else rng.sample(rows, n)
        picks.extend((label, h) for h in sorted(take))
    return picks


def services_in(text):
    """Deployed service names from an `affects` answer: the `in-process` block and the
    left-hand side of each hop. Not the RPC names, which sit on the indented detail line
    under a hop and look identical to a bare service name."""
    out, in_proc = set(), False
    for line in text.splitlines():
        if line.startswith("in-process"):
            in_proc = True
            continue
        if line.startswith("over the network"):
            in_proc = False
            continue
        if line.startswith(("suppressed:", "unknown", "stale:", "calls out")):
            in_proc = False
            continue
        if in_proc and line.startswith("  ") and line.strip():
            out.add(line.split()[0])
        elif not in_proc and "->" in line:
            out.add(line.split("->")[0].strip().removesuffix(" ~").strip())
            rhs = line.split("->")[1].strip().split()
            if rhs:
                out.add(rhs[0])
    return {s for s in out if s and s != "?"}


def handles_in(text):
    """Handles a listing printed, as `[abc]`."""
    out = set()
    for part in text.split("["):
        end = part.find("]")
        if 0 < end <= 6 and part[:end].isalnum():
            out.add(part[:end])
    return out


class Findings:
    def __init__(self):
        self.rows = []

    def note(self, kind, subject, detail, repro):
        self.rows.append((kind, subject, detail, repro))


def check_reaches_symmetry(db, handle, f, generated):
    """If `reaches X` names C as a caller across the boundary, then `reaches C --outgoing`
    must name X. One direction being exact while the other is empty is the defect that
    kept agents rebuilding chains by hand.

    Two corrections to the invariant, both made after the check reported a difference
    that was not one:

    * Generated definitions are skipped. Asked about a generated server stub, the incoming
      direction reports its callers; the outgoing direction resolves those callers to the
      *hand-written* handler that really serves the RPC. Different symbols on purpose.
    * The return trip may name the type or any of its members. `reaches <type>` answers for
      every RPC the type serves; `--outgoing` answers with the method that serves the one
      RPC being called. A method of the type is the same answer at a finer grain.

    Between them those two accounted for every hit the first version reported. An invariant
    that is too strong does not find defects, it manufactures them.
    """
    if handle in generated:
        return
    code, out, _ = run(db, "reaches", handle)
    if code != 0:
        return
    family = members_of(db, handle) | {handle}
    for caller in handles_in(out) - {handle}:
        c2, out2, _ = run(db, "reaches", caller, "--outgoing")
        if c2 == 0 and handles_in(out2) & family:
            continue
        f.note(
            "reaches is not symmetric",
            handle,
            f"`reaches {handle}` names [{caller}], but `reaches {caller} --outgoing` "
            f"does not name [{handle}]",
            f"cairn reaches {handle}; cairn reaches {caller} --outgoing",
        )
        return


def members_of(db, handle):
    """Methods of a type, by the same containment the store's own queries use."""
    c = sqlite3.connect(db)
    row = c.execute(
        "SELECT s.name_id, s.def_file_id FROM symbols s JOIN handles h "
        "ON h.symbol_id = s.id WHERE h.handle = ?",
        (handle,),
    ).fetchone()
    if not row or row[1] is None:
        return set()
    return {
        r[0]
        for r in c.execute(
            "SELECT h.handle FROM symbols m JOIN handles h ON h.symbol_id = m.id "
            "WHERE m.def_file_id = ? AND m.container_leaf_id = ? AND m.kind = 3",
            (row[1], row[0]),
        )
    }


def check_affects_covers_methods(db, handle, f):
    """A handler class must affect at least the services its own methods affect. The class
    is the name in the file and the name an outline hands back, so an answer smaller than
    its parts is the one people act on.

    Services, not RPC names: the hop lines carry RPCs in the same indented shape, and
    reading those as services made this report five differences that were only ever
    differences of RPC.
    """
    code, out, _ = run(db, "affects", handle)
    if code != 0:
        return
    whole = services_in(out)
    for m in sorted(members_of(db, handle))[:6]:
        mc, mout, _ = run(db, "affects", m)
        if mc != 0:
            continue
        part = services_in(mout)
        missing = part - whole
        if missing:
            f.note(
                "affects on a class is smaller than on its method",
                handle,
                f"method [{m}] reaches {sorted(missing)}, the class does not",
                f"cairn affects {handle}; cairn affects {m}",
            )
            return


def check_usage_within_refs(db, handle, f):
    """Every file `usage --include-tests` names must appear in `refs`. They answer the same
    question at different grain; one holding a file the other does not is a filter nobody
    was told about."""
    uc, uout, _ = run(db, "usage", handle, "--include-tests")
    rc, rout, _ = run(db, "refs", handle, "--include-generated", "--limit", "200")
    if uc != 0 or rc != 0:
        return
    ufiles = {
        l.split()[-1].removesuffix("[test]").strip()
        for l in uout.splitlines()
        if l.startswith("     ") and "x  " in l
    }
    for path in ufiles:
        if path and path not in rout:
            f.note(
                "usage names a file refs does not",
                handle,
                f"{path} appears in `usage --include-tests` and not in `refs`",
                f"cairn usage {handle} --include-tests; cairn refs {handle}",
            )
            return


def check_determinism(db, handle, f):
    """The same question twice must give the same bytes. A listing that reorders itself
    cannot be diffed, and every claim in this repository's results rests on being able to
    re-run a command and compare."""
    for cmd in (["usage", handle], ["affects", handle], ["graph", handle, "--aspect", "callers"]):
        _, a, _ = run(db, *cmd)
        _, b, _ = run(db, *cmd)
        if a != b:
            f.note(
                "answer is not deterministic",
                handle,
                "two identical invocations differed",
                "cairn " + " ".join(cmd),
            )
            return


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    db, repo = sys.argv[1], sys.argv[2]
    n = int(sys.argv[3]) if len(sys.argv) > 3 else 12
    f = Findings()
    c = sqlite3.connect(db)
    generated = {
        r[0]
        for r in c.execute(
            "SELECT h.handle FROM symbols s JOIN handles h ON h.symbol_id = s.id "
            "JOIN files fl ON fl.id = s.def_file_id WHERE fl.generated = 1"
        )
    }
    picks = sample(db, n)
    print(f"{len(picks)} symbols, stratified and seeded\n")
    for i, (label, handle) in enumerate(picks, 1):
        check_reaches_symmetry(db, handle, f, generated)
        check_affects_covers_methods(db, handle, f)
        check_usage_within_refs(db, handle, f)
        check_determinism(db, handle, f)
        if i % 10 == 0:
            print(f"  ...{i}/{len(picks)}", flush=True)

    print()
    if not f.rows:
        print("no contradictions found")
        return 0
    seen = set()
    print(f"{len(f.rows)} contradiction(s):\n")
    for kind, subject, detail, repro in f.rows:
        if kind in seen:
            continue
        seen.add(kind)
        print(f"  {kind}")
        print(f"    on [{subject}]: {detail}")
        print(f"    repro: {repro}")
        print(f"    ({sum(1 for r in f.rows if r[0] == kind)} symbols show this)\n")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
