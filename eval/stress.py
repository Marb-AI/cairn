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

Every check states the contract it rests on. That is not decoration: the first version of
this file reported three classes of finding and two were the invariant being stronger than
the contract, which is the failure mode of the whole idea. If a check cannot cite the
sentence it enforces, it does not belong here.

Usage: stress.py <db> <repo> [sample-size]
Environment: CAIRN_BIN overrides the binary, which CI needs because the release build
lands in the container's target directory rather than beside this file.
"""

import os
import random
import sqlite3
import subprocess
import sys

BIN = os.environ.get("CAIRN_BIN", "/home/workspaces/cairn/bin/cairn-bin")


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


# Lines that explain the answer rather than being it. A handle inside one of these is
# prose — `answered for the enclosing type [qsu]` names the subject, not a caller — and
# reading it as a result row is how this file reported an asymmetry that was a sentence.
PROSE = ("answered for the", "via:", "every RPC this", "this RPC only", "where this lands")


def handles_in(text):
    """Handles a listing printed as result rows, as `[abc]`. Prose lines excluded."""
    out = set()
    for line in text.splitlines():
        stripped = line.strip()
        if any(stripped.startswith(p) for p in PROSE):
            continue
        for part in line.split("["):
            end = part.find("]")
            if 0 < end <= 6 and part[:end].isalnum():
                out.add(part[:end])
    return out


# Findings already diagnosed and written up, so a run that reports only these is a clean
# run. Keyed by check *and* symbol: the same class on a different symbol is new and still
# surfaces. Suppressing a whole check would hide the regression this file exists to catch.
KNOWN: dict = {
    # Emptied once `--outgoing` was unified: both ways of answering it now emit the same
    # rows and differ in the claim they print, so the symmetry check applies to both. A
    # finding reappearing here is a regression, which is the point of not leaving it
    # allowlisted after the fix.
}


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


def without_stale(text):
    """The answer without its `stale:` line.

    That line is allowed to change between two identical invocations and it is right that
    it does: the watcher starts itself on first use, so the first call reports "not tracked
    yet" and a later one reports a real verdict. Comparing whole outputs called that
    nondeterminism twice on the fixture corpus. The *answer* must be stable; the freshness
    of the index underneath it is exactly what is not."""
    return "\n".join(l for l in text.splitlines() if not l.startswith("stale:"))


def check_determinism(db, handle, f):
    """The same question twice must give the same answer. A listing that reorders itself
    cannot be diffed, and every claim in this repository's results rests on being able to
    re-run a command and compare."""
    for cmd in (["usage", handle], ["affects", handle], ["graph", handle, "--aspect", "callers"]):
        _, a, _ = run(db, *cmd)
        _, b, _ = run(db, *cmd)
        if without_stale(a) != without_stale(b):
            f.note(
                "answer is not deterministic",
                handle,
                "two identical invocations differed",
                "cairn " + " ".join(cmd),
            )
            return


ENVELOPE_COMMANDS = [
    ["status"],
    ["topology"],
    ["entrypoints"],
    ["docs"],
    ["verify"],
]


def check_envelope_and_exit_codes(db, repo, f):
    """Contract, from the agent guide: every answer ends with `unknown:` / `suppressed:` /
    `stale:`, and exit codes are 0 found, 1 nothing, 2 bad query, 3 degraded.

    An answer without its envelope is the failure this project exists to prevent — a list
    that looks complete because nothing said otherwise. An exit code outside the contract
    is worse, because the caller acts on it without reading anything."""
    for cmd in ENVELOPE_COMMANDS + [["for", "find", "Kontomatik", "--repo", repo]]:
        code, out, err = run(db, *cmd)
        label = "cairn " + " ".join(cmd)
        if code not in (0, 1, 2, 3):
            f.note("exit code outside the contract", label, f"exit {code}", label)
            continue
        if code != 0:
            continue
        for field in ("suppressed:", "stale:"):
            if field not in out:
                f.note(
                    "answer without its envelope",
                    label,
                    f"exit 0 and no `{field}` line",
                    label,
                )
                break


def check_budget_admits_what_it_cut(db, handle, f):
    """Contract: `--budget <tokens>` "is a ceiling: the tool fills it with the highest-ranked
    rows and reports what it dropped".

    So a budget small enough to cut must produce a non-empty `suppressed:`. Silent
    truncation is the exact shape of three defects already in RESULTS.md."""
    wide_code, wide, _ = run(db, "usage", handle, "--include-tests")
    if wide_code != 0:
        return
    wide_rows = [l for l in wide.splitlines() if l.startswith("     ") and "x  " in l]
    if len(wide_rows) < 3:
        return
    code, tight, _ = run(db, "--budget", "60", "usage", handle, "--include-tests")
    if code != 0:
        return
    tight_rows = [l for l in tight.splitlines() if l.startswith("     ") and "x  " in l]
    if len(tight_rows) < len(wide_rows) and "suppressed: none" in tight:
        f.note(
            "a cut list says nothing was cut",
            handle,
            f"{len(wide_rows)} files at full budget, {len(tight_rows)} at 60, "
            f"and `suppressed: none`",
            f"cairn --budget 60 usage {handle} --include-tests",
        )


def check_runs_agrees_with_affects(db, handle, f):
    """Both answer "which deployed services run this code" — `runs` alone, `affects` as the
    in-process half of a wider answer. Two commands, one fact, so they must not disagree.

    Compared as sets and only when both are confident: `affects` marks a service `~` when
    it was attributed through the file rather than a call path, and `runs` labels the same
    case in its header, so a difference there is a difference of confidence, not of fact."""
    rc, rout, _ = run(db, "runs", handle)
    ac, aout, _ = run(db, "affects", handle)
    if rc != 0 or ac != 0:
        return
    if "via the file" in rout or " ~ " in aout:
        return
    runs_set = {
        l.strip().split()[0]
        for l in rout.splitlines()
        if l.startswith("  ") and l.strip() and not l.strip().startswith("(")
    }
    in_proc = set()
    seen_header = False
    for line in aout.splitlines():
        if line.startswith("in-process"):
            seen_header = True
            continue
        if seen_header:
            if not line.startswith("  ") or not line.strip():
                break
            if not line.strip().startswith("("):
                in_proc.add(line.split()[0])
    if runs_set and in_proc and runs_set != in_proc:
        f.note(
            "runs and affects disagree about the same services",
            handle,
            f"runs says {sorted(runs_set)}, affects in-process says {sorted(in_proc)}",
            f"cairn runs {handle}; cairn affects {handle}",
        )


def check_literal_agrees_with_find(db, repo, f):
    """`literal` and `for find` both answer "whose line is this" for the same line. They
    reach it by different routes — one from indexed literals, one from a tree search plus
    an attribution lookup — so agreement is a real cross-check, and it is the one that
    would have caught the off-by-one where the enclosing *class* answered for a method."""
    c = sqlite3.connect(db)
    texts = [
        r[0]
        for r in c.execute(
            "SELECT DISTINCT text FROM literals "
            "WHERE length(text) BETWEEN 8 AND 40 "
            "AND instr(text, char(10)) = 0 AND enclosing IS NOT NULL "
            "ORDER BY text LIMIT 200"
        )
    ][::17][:8]
    for text in texts:
        lc, lout, _ = run(db, "literal", text, "--context", "none")
        fc, fout, _ = run(db, "for", "find", text, "--repo", repo)
        if lc != 0 or fc != 0:
            continue
        # literal: "  path:line  in Owner.name [handle]"
        want = {}
        for line in lout.splitlines():
            if " in " not in line or not line.startswith("  "):
                continue
            where, _, owner = line.strip().partition("  in ")
            if ":" in where and "[" in owner:
                want[where] = owner.split("[")[1].rstrip("] ").strip()
        # for find: file header, then "  <line> > text  <- in name [handle]"
        got, path = {}, None
        for line in fout.splitlines():
            if line and not line.startswith(" ") and "/" in line:
                path = line.split()[0]
            elif " > " in line and "<- in " in line and path:
                num = line.strip().split(">")[0].strip()
                h = line.split("<- in ")[1]
                if "[" in h:
                    got[f"{path}:{num}"] = h.split("[")[1].split("]")[0].strip()
        for where, handle in want.items():
            if where in got and got[where] != handle:
                f.note(
                    "literal and for find name different owners for one line",
                    text[:30],
                    f"{where}: literal says [{handle}], for find says [{got[where]}]",
                    f'cairn literal "{text}"; cairn for find "{text}"',
                )
                return


def check_staleness_agrees(db, repo, f):
    """`verify --repo` is the one-off comparison of tree against index; `status` reports the
    watcher's view of the same thing. When verify finds nothing changed, status must not
    claim modified files — the daemon reporting a clean tree as dirty is a defect already
    fixed once here, and it is worth a standing check.

    Only when the index sits where the convention puts it. The watcher derives the
    repository as the grandparent of the database, so an index in a temp directory — which
    is how the fixture corpus is indexed — has it watching the wrong tree, and the
    disagreement then says something about the harness rather than about cairn."""
    if db.replace("\\", "/").split("/")[-2:-1] != [".cairn"]:
        return
    vc, vout, _ = run(db, "verify", "--repo", repo)
    sc, sout, _ = run(db, "status")
    if vc not in (0, 1) or sc not in (0, 1):
        return
    verify_clean = "stale: none" in vout or "0 files changed" in vout
    status_line = next((l for l in sout.splitlines() if l.startswith("stale:")), "")
    if verify_clean and " modified" in status_line:
        n = status_line.split(" modified")[0].split()[-1]
        if n.isdigit() and int(n) > 0:
            f.note(
                "status calls a tree dirty that verify calls clean",
                "(tree)",
                f"verify: no changed files; status: {status_line.strip()}",
                f"cairn verify --repo {repo}; cairn status",
            )


def check_handles_resolve(db, handle, f):
    """A handle printed by one command must be accepted by another. Handles are the
    shortest unique prefix of a hash, so a collision or a truncation bug shows up here and
    nowhere else — as `no symbol with handle`, on a handle the tool itself just printed."""
    code, out, _ = run(db, "graph", handle, "--aspect", "callers")
    if code != 0:
        return
    for h in sorted(handles_in(out))[:6]:
        c2, _, err = run(db, "runs", h)
        if c2 == 2 and "no symbol with handle" in err:
            f.note(
                "a printed handle does not resolve",
                handle,
                f"[{h}] was printed by `graph` and rejected by `runs`",
                f"cairn graph {handle} --aspect callers; cairn runs {h}",
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
    # Whole-tool checks, once rather than per symbol.
    check_envelope_and_exit_codes(db, repo, f)
    check_literal_agrees_with_find(db, repo, f)
    check_staleness_agrees(db, repo, f)
    for i, (label, handle) in enumerate(picks, 1):
        check_reaches_symmetry(db, handle, f, generated)
        check_affects_covers_methods(db, handle, f)
        check_usage_within_refs(db, handle, f)
        check_determinism(db, handle, f)
        check_budget_admits_what_it_cut(db, handle, f)
        check_runs_agrees_with_affects(db, handle, f)
        check_handles_resolve(db, handle, f)
        if i % 10 == 0:
            print(f"  ...{i}/{len(picks)}", flush=True)

    print()
    if not f.rows:
        print("no contradictions found")
        return 0
    fresh = [r for r in f.rows if (r[0], r[1]) not in KNOWN]
    old = [r for r in f.rows if (r[0], r[1]) in KNOWN]
    for kind, subject, _, _ in old:
        print(f"  known: {kind} on [{subject}] — {KNOWN[(kind, subject)]}\n")
    if not fresh:
        print("no new contradictions")
        return 0
    seen = set()
    print(f"{len(fresh)} new contradiction(s):\n")
    for kind, subject, detail, repro in fresh:
        if kind in seen:
            continue
        seen.add(kind)
        print(f"  {kind}")
        print(f"    on [{subject}]: {detail}")
        print(f"    repro: {repro}")
        print(f"    ({sum(1 for r in fresh if r[0] == kind)} symbol(s) show this)\n")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
