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
        # Symbols that actually call across a service boundary. Added after a binary with
        # a deliberately re-injected defect ran the whole file and reported nothing: the
        # three strata above select on reference count, and the code that starts a chain
        # is typically a route handler nothing calls. Every cross-boundary check was
        # running on symbols that cross no boundary, which is a check that cannot fail.
        "chain starts": """SELECT DISTINCT h.handle FROM edges e
                             JOIN symbols rpc ON rpc.id = e.dst_symbol
                             JOIN symbols art ON art.def_file_id = rpc.def_file_id
                                             AND art.id <> rpc.id
                                             AND rpc.container_leaf_id = art.name_id
                                             AND art.kind = 1
                             JOIN service_links t ON t.via_symbol = art.id AND t.role = 1
                             JOIN symbols s ON s.id = e.src_symbol
                             JOIN files f ON f.id = s.def_file_id AND f.generated = 0
                             JOIN handles h ON h.symbol_id = s.id
                            WHERE e.kind = 0 ORDER BY h.handle""",
    }
    rng = random.Random(20260805)
    for label, q in strata.items():
        rows = [r[0] for r in c.execute(q)]
        take = rows if len(rows) <= n else rng.sample(rows, n)
        picks.extend((label, h) for h in sorted(take))
    return picks


def prefixes(db, n):
    """Directories with enough hand-written code to be worth surveying, deterministically.

    Two levels down: a whole-tree prefix answers about everything and a leaf answers about
    almost nothing, and `unreached` is a question people ask about a package."""
    c = sqlite3.connect(db)
    seen = {}
    for (path,) in c.execute(
        "SELECT p.s FROM files f JOIN strings p ON p.id = f.path_id "
        "WHERE f.generated = 0 AND coalesce(f.is_test, 0) = 0"
    ):
        parts = path.split("/")
        if len(parts) > 2:
            d = "/".join(parts[:3])
            seen[d] = seen.get(d, 0) + 1
    ranked = sorted(seen.items(), key=lambda kv: (-kv[1], kv[0]))
    return [d for d, _ in ranked[:n]]


def shared_names(db, n):
    """Names carried by more than one non-generated symbol, deterministically sampled.

    The ambiguity path is the one place `for` decides something on the caller's behalf, so
    it is the one place two purposes can quietly answer about different code. A name that
    only ever means one thing exercises none of it."""
    c = sqlite3.connect(db)
    rows = [
        r[0]
        for r in c.execute(
            """SELECT n.s FROM symbols s
                 JOIN strings n ON n.id = s.name_id
                 JOIN files f ON f.id = s.def_file_id AND f.generated = 0
                WHERE s.kind = 3
                GROUP BY n.s HAVING count(DISTINCT s.id) > 1
                ORDER BY n.s"""
        )
    ]
    rng = random.Random(20260805)
    return sorted(rows if len(rows) <= n else rng.sample(rows, n))


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
    """Findings, and — just as important — which checks got far enough to have one.

    Every check here opens with guards: the command has to succeed, the symbol has to have
    callers, the list has to be long enough to be cut. A check whose guards never open on
    a given corpus reports nothing, and reporting nothing is indistinguishable from passing.
    Twice in one day that was the actual state of affairs — four cross-boundary checks
    running only on symbols that cross no boundary, and an envelope assertion skipping
    because its search term existed in one repository and not the other. Both times the
    file printed `no contradictions found`.

    So `reached` is recorded at the point a check commits to an assertion, and the summary
    names any check that never got there. That turns "this corpus cannot exercise this
    check" from something you discover by deliberately breaking the binary into something
    the run says out loud.
    """

    def __init__(self):
        self.rows = []
        self.reached = {}

    def note(self, kind, subject, detail, repro):
        self.rows.append((kind, subject, detail, repro))

    def ran(self, check):
        """Past the guards, about to compare two answers."""
        self.reached[check] = self.reached.get(check, 0) + 1


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
    * **When the answer says it was given for the enclosing type, the family is that
      type's.** A service binding names the handler, not each of its methods, so asked
      about one method the incoming direction says so on the page and answers for the
      whole type. The outgoing direction then comes back with a *sibling* — and for an
      unexported helper like Go's `websocket.streamAgentChat`, which is no RPC at all,
      the sibling is the only honest answer there is. Reading the line the tool prints is
      the difference between checking the contract and inventing a stronger one.

    Between them those three accounted for every hit this check has ever reported. An
    invariant that is too strong does not find defects, it manufactures them.
    """
    if handle in generated:
        return
    code, out, _ = run(db, "reaches", handle)
    if code != 0:
        return
    family = members_of(db, handle) | {handle}
    # `answered for the enclosing type [uv7] websocket: ...`
    for line in out.splitlines():
        if line.strip().startswith("answered for the enclosing type ["):
            owner = line.split("[", 1)[1].split("]", 1)[0]
            family |= members_of(db, owner) | {owner}
    for caller in handles_in(out) - {handle}:
        f.ran("reaches symmetry")
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


def check_nothing_reaches_itself_across_a_boundary(db, handle, f):
    """`reaches` answers "who reaches this **across a gRPC boundary**". A boundary is
    between two processes, so a handler type cannot be on both ends of one: its own methods
    are not callers of it, and neither is the type itself.

    Cheap, and it is the check that would have named the `link_services` defect on sight
    rather than by way of a symmetry failure three steps removed. `reaches searchService`
    listed seven of its own methods among fourteen callers, each row individually plausible
    — a Go function, a real file, a real RPC name — and the whole set reading as more
    thorough than the correct answer of seven.
    """
    code, out, _ = run(db, "reaches", handle)
    if code != 0:
        return
    family = members_of(db, handle) | {handle}
    owner = enclosing_type(db, handle)
    if owner:
        family |= members_of(db, owner) | {owner}
    # Result rows only. The header names the subject (`[6gj5] searchService — 7 caller(s)`)
    # and reading that as a row makes every answer report itself, which is the check
    # failing on its own output rather than on the tool's.
    rows = "\n".join(l for l in out.splitlines() if l.startswith("  "))
    if rows.strip():
        f.ran("nothing reaches itself")
    for caller in handles_in(rows) & family:
        f.note(
            "reaches names the subject's own family as a caller across the boundary",
            handle,
            f"[{caller}] is [{handle}] or a member of its type, and a type cannot call "
            f"itself over gRPC",
            f"cairn reaches {handle}",
        )
        return


def enclosing_type(db, handle):
    """The type a method belongs to, by the same containment the store's queries use."""
    c = sqlite3.connect(db)
    row = c.execute(
        "SELECT t.handle FROM symbols me JOIN handles h ON h.symbol_id = me.id "
        "JOIN symbols ty ON ty.def_file_id = me.def_file_id AND ty.kind = 1 "
        "AND ty.name_id = me.container_leaf_id AND ty.id <> me.id "
        "JOIN handles t ON t.symbol_id = ty.id WHERE h.handle = ? LIMIT 1",
        (handle,),
    ).fetchone()
    return row[0] if row else None


def check_unreached_is_really_unused(db, prefix, f):
    """Contract: `unreached` lists "symbols under a path that production code never
    calls", and the rows say `no callers`. So nothing it names may have production use
    sites, which is the same fact `usage` reports.

    The two disagreed on an enum: `unreached` said no callers, `usage` said ten sites, and
    all ten were the lookup table built from its own members ten lines below it. Both were
    literally right — a type is referenced, not called — and a reader acting on the command
    whose purpose is finding deletable code would have deleted live code. That is the
    failure this file exists to catch, and it is the third time this command has been
    wrong about a whole category (handlers, then constructors, now types).
    """
    code, out, _ = run(db, "unreached", prefix, "--limit", "40")
    if code != 0:
        return
    # Only the rows claiming *nothing*. A row that says `no calls, N ref(s)` has already
    # told the reader what `usage` would, so the two do not disagree — which is the whole
    # difference between an answer that states its gap and one that hides it.
    bare = set()
    for line in out.splitlines():
        if not line.startswith("  no callers"):
            continue
        for h in handles_in(line):
            bare.add(h)
    for handle in sorted(bare):
        # 0 found, 1 nothing - and "nothing" is the answer this check wants, not a
        # failure. Treating exit 1 as an error skipped every genuinely unused symbol and
        # left the check reaching an assertion only when it was about to report one, which
        # the coverage line then correctly called an absence of evidence.
        uc, uout, _ = run(db, "usage", handle)
        if uc not in (0, 1):
            continue
        head = uout.splitlines()[0] if uout else ""
        f.ran("unreached is really unused")
        if " used at 0 sites" in head:
            continue
        f.note(
            "unreached says no callers where usage says used",
            handle,
            f"`unreached {prefix}` lists [{handle}] as `no callers` with no reference "
            f"count, but `usage` says: {head.strip()}",
            f"cairn unreached {prefix}; cairn usage {handle}",
        )
        return


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
        f.ran("affects covers its methods")
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
        f.ran("usage within refs")
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
        f.ran("determinism")
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


def a_text_that_is_in_this_repo(db):
    """A literal the corpus actually contains, so `for find` returns something.

    This used to be a fixed word taken from the checkout the harness was written on. On
    any other corpus `for find` then exited 1 and the envelope assertion below skipped
    itself — so the one command that reads the working tree was unchecked precisely on
    the fixture, which is the corpus CI runs. A constant borrowed from one repository is
    the quietest way to disable a check on every other."""
    c = sqlite3.connect(db)
    row = c.execute(
        # Trimmed, and no whitespace or braces inside: a needle like `+ {positive}` is a
        # format string whose spacing the tree search has to match exactly, which tests
        # the fixture's quoting rather than the command.
        "SELECT trim(text) t FROM literals "
        " WHERE length(t) BETWEEN 6 AND 30 AND t GLOB '[A-Za-z]*'"
        "   AND t NOT GLOB '*[ {}%\"]*' ORDER BY t LIMIT 1"
    ).fetchone()
    return row[0] if row else "the"


def first_handle(db):
    """Any real symbol, chosen the same way on every run. The envelope check needs a
    subject that resolves and does not care which one it is."""
    c = sqlite3.connect(db)
    row = c.execute(
        "SELECT h.handle FROM symbols s JOIN handles h ON h.symbol_id = s.id "
        "JOIN files f ON f.id = s.def_file_id AND f.generated = 0 "
        "WHERE s.kind = 3 ORDER BY h.handle LIMIT 1"
    ).fetchone()
    return row[0] if row else "x"


def check_envelope_and_exit_codes(db, repo, f):
    """Contract, from the agent guide: every answer ends with `unknown:` / `suppressed:` /
    `stale:`, and exit codes are 0 found, 1 nothing, 2 bad query, 3 degraded.

    An answer without its envelope is the failure this project exists to prevent — a list
    that looks complete because nothing said otherwise. An exit code outside the contract
    is worse, because the caller acts on it without reading anything."""
    for cmd in ENVELOPE_COMMANDS + [
        ["for", "find", a_text_that_is_in_this_repo(db), "--repo", repo],
        # The assembled answers too, and by handle so they resolve on any corpus. One
        # command was already found missing two thirds of its own envelope; the ones that
        # fuse several blocks are where a missing line is least likely to be noticed by
        # eye, because there is so much else on the page.
        ["for", "understand", first_handle(db)],
        ["for", "change", first_handle(db)],
    ]:
        code, out, err = run(db, *cmd)
        label = "cairn " + " ".join(cmd)
        if code not in (0, 1, 2, 3):
            f.note("exit code outside the contract", label, f"exit {code}", label)
            continue
        if code != 0:
            continue
        f.ran("envelope and exit codes")
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
    f.ran("budget admits what it cut")
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
    if runs_set and in_proc:
        f.ran("runs agrees with affects")
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
            if where in got:
                f.ran("literal agrees with for find")
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
    f.ran("staleness agrees")
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
        f.ran("printed handles resolve")
        c2, _, err = run(db, "runs", h)
        if c2 == 2 and "no symbol with handle" in err:
            f.note(
                "a printed handle does not resolve",
                handle,
                f"[{h}] was printed by `graph` and rejected by `runs`",
                f"cairn graph {handle} --aspect callers; cairn runs {h}",
            )
            return


def chain_hops(text):
    """The hops `for understand` printed, as (depth, handle).

    Depth is the indent: two spaces per hop, which is the only thing on the page that
    says whether a row is the first hop or the fourth."""
    out = []
    for line in text.splitlines():
        if " -> [" not in line:
            continue
        indent = len(line) - len(line.lstrip(" "))
        handle = line.split(" -> [", 1)[1].split("]", 1)[0]
        out.append((indent // 2, handle))
    return out


def check_understand_matches_its_own_citation(db, handle, f):
    """Contract, from the tool's own first rule: "every block names the command that
    produced it". `for understand` cites `cairn reaches <h> --outgoing`, so its first hop
    must be that command's answer — not a superset, not a subset.

    This is the check a fused answer needs and a mechanism does not. Every block cairn
    assembles is a claim that running the named command would give you the same rows, and
    an assembly that quietly diverges from its citation is worse than one that never cited
    anything: it tells the reader where to look and then disagrees with what they find.
    """
    uc, uout, _ = run(db, "for", "understand", handle)
    rc, rout, _ = run(db, "reaches", handle, "--outgoing")
    if uc not in (0, 1) or rc not in (0, 1):
        return
    # A row reached through something this code calls is not produced by
    # `reaches <root> --outgoing` and does not claim to be: it prints `via [h]` and the
    # block cites `reaches h --outgoing` for it. Checking those against the root was this
    # check being stronger than the contract - but only after the contract was corrected,
    # because for a while the block really did cite a command that would not return them.
    first, via = set(), set()
    for line in uout.splitlines():
        if " -> [" not in line or (len(line) - len(line.lstrip(" "))) // 2 != 1:
            continue
        target = line.split(" -> [", 1)[1].split("]", 1)[0]
        if " via [" in line:
            via.add((line.split(" via [", 1)[1].split("]", 1)[0], target))
        else:
            first.add(target)
    cited = handles_in(rout) - {handle}
    if first or cited or via:
        f.ran("for understand matches its citation")
    for source, target in sorted(via):
        vc, vout, _ = run(db, "reaches", source, "--outgoing")
        if vc in (0, 1) and target not in handles_in(vout):
            f.note(
                "a `via` row is not produced by the command it cites",
                handle,
                f"[{target}] is printed as reached via [{source}], but "
                f"`reaches {source} --outgoing` does not name it",
                f"cairn for understand {handle}; cairn reaches {source} --outgoing",
            )
            return
    if first != cited:
        f.note(
            "for understand disagrees with the command it cites",
            handle,
            f"first hop is {sorted(first)}, `reaches --outgoing` says {sorted(cited)}",
            f"cairn for understand {handle}; cairn reaches {handle} --outgoing",
        )


def check_the_chain_was_followed_to_where_it_says(db, handle, f):
    """`for understand` claims the chain is "followed to the end". So for every hop it
    prints, that target's own outgoing targets must appear too — unless the answer said
    it stopped, which it does in `unknown:` when the depth cap bites.

    A walk that drops a branch silently is the exact failure the transitive form was built
    to remove: one call that looks complete and is not is worse than four calls that each
    admit their scope.
    """
    code, out, _ = run(db, "for", "understand", handle)
    if code not in (0, 1):
        return
    hops = chain_hops(out)
    if not hops:
        return
    if "the walk stopped at" in out:
        return
    printed = {h for _, h in hops}
    for depth, target in hops:
        # Only the levels the walk actually continued past; the deepest row's children
        # are what the cap would have cut, and the guard above covers that case.
        if depth >= 4:
            continue
        # 0 found, 1 nothing. "Nothing" is the ordinary case for the far end of a chain
        # and is exactly what this check wants to confirm, so treating exit 1 as a failure
        # meant the check only ran when a hop had further hops - the same guard mistake
        # made in `unreached is really unused` an hour earlier, and caught the same way,
        # by the coverage line reporting a check that never reached an assertion.
        tc, tout, _ = run(db, "reaches", target, "--outgoing")
        if tc not in (0, 1):
            continue
        f.ran("chain followed to where it says")
        missing = (handles_in(tout) - {target, handle}) - printed
        if missing:
            f.note(
                "the chain stops without saying it stopped",
                handle,
                f"[{target}] at depth {depth} reaches {sorted(missing)}, which the chain "
                f"does not print and the envelope does not mention",
                f"cairn for understand {handle}; cairn reaches {target} --outgoing",
            )
            return


def check_both_purposes_resolve_the_same_subject(db, name, f):
    """`for change` and `for understand` share one resolution path — the text redirect, the
    ranked choice for an ambiguous name, the tree fallback. Contract: the choice is printed
    with its alternatives, so the caller can override it in one copy-paste.

    That only holds if the two purposes make the *same* choice. Two commands answering
    about different symbols from the same word, each printing a defensible reason, is the
    confident-and-wrong failure the whole tool is written against.
    """
    cc, _, cerr = run(db, "for", "change", name)
    uc, _, uerr = run(db, "for", "understand", name)
    if cc not in (0, 1) or uc not in (0, 1):
        return
    pick = lambda err: (
        err.split("Answering for [", 1)[1].split("]", 1)[0]
        if "Answering for [" in err
        else None
    )
    a, b = pick(cerr), pick(uerr)
    if a and b:
        f.ran("both purposes pick the same symbol")
    if a and b and a != b:
        f.note(
            "the two purposes pick different symbols for one name",
            name,
            f"`for change` answers for [{a}], `for understand` for [{b}]",
            f"cairn for change {name}; cairn for understand {name}",
        )


def check_printed_line_is_where_the_definition_is(db, handle, f):
    """Contract: a printed `path:line` is the 1-based number an editor opens
    (`Occurrence::location` — "SCIP lines are 0-based"). Every renderer that formats
    `path:line` by hand instead of calling that helper skips the conversion, and the answer
    then points one line above the `def` or `func` keyword.

    Compared against the index the renderer was handed, which is the narrowest form of the
    question: not "is the index right about this symbol" — that is a different check and a
    different failure — but "did this command print what it was given". `reaches
    --outgoing` did not, while `reaches` on the same symbol did, so the two directions of
    one command named one definition a line apart.
    """
    truth = {}
    c = sqlite3.connect(db)
    # `for change` is in this list because leaving it out is how the second instance of
    # this defect shipped: the check covered definition rows in three commands, and the
    # one that got it wrong printed a *reference* row in a fourth. Two agent runs caught
    # it by opening the file, which is the job this check exists to do without them.
    for cmd in (
        ["for", "understand", handle],
        ["for", "change", handle],
        ["reaches", handle, "--outgoing"],
        ["expand", handle],
    ):
        code, out, _ = run(db, *cmd)
        if code != 0:
            continue
        for line in out.splitlines():
            stripped = line.strip()
            if any(stripped.startswith(p) for p in PROSE):
                continue
            for h, path, num in printed_locations(line):
                if h not in truth:
                    truth[h] = c.execute(
                        "SELECT p.s, sy.def_line FROM symbols sy "
                        "JOIN handles hh ON hh.symbol_id = sy.id "
                        "JOIN files fl ON fl.id = sy.def_file_id "
                        "JOIN strings p ON p.id = fl.path_id "
                        "WHERE hh.handle = ?",
                        (h,),
                    ).fetchone()
                row = truth[h]
                # Only when this row is naming that symbol's own definition. A call site
                # is a different fact about the same handle and is allowed to differ.
                if not row or row[0] != path:
                    continue
                want = row[1] + 1
                f.ran("printed line is the definition line")
                if num != want:
                    f.note(
                        "a printed line is not the line the definition is on",
                        h,
                        f"`cairn {' '.join(cmd)}` says {path}:{num}, the index has the "
                        f"definition at {path}:{want} (SCIP counts from 0, output from 1)",
                        "cairn " + " ".join(cmd),
                    )
                    return


def printed_locations(line):
    """(handle, path, line) for every `path:line` on a line that carries a handle.

    The handle is the last one printed before the location, which is the layout every
    listing in this tool uses: `[abc] Name  py  path/to/file.py:12`."""
    out, current = [], None
    for token in line.replace("(", " ").replace(")", " ").split():
        if token.startswith("[") and token.endswith("]") and token[1:-1].isalnum():
            current = token[1:-1]
        elif ":" in token and "/" in token and current:
            path, _, num = token.rpartition(":")
            num = num.split("-")[0]
            if num.isdigit():
                out.append((current, path, int(num)))
    return out


# Every check in this file, so one that never opened its guards can be named. Kept as a
# literal list rather than derived from the functions that ran: a check deleted from the
# loop by accident would otherwise vanish from the report along with its coverage, which
# is the same silence this is here to break.
ALL_CHECKS = [
    "reaches symmetry",
    "nothing reaches itself",
    "affects covers its methods",
    "usage within refs",
    "determinism",
    "envelope and exit codes",
    "budget admits what it cut",
    "runs agrees with affects",
    "literal agrees with for find",
    "staleness agrees",
    "printed handles resolve",
    "for understand matches its citation",
    "chain followed to where it says",
    "both purposes pick the same symbol",
    "printed line is the definition line",
    "unreached is really unused",
]


def report_coverage(f):
    """Which checks reached an assertion, and which never got past their guards.

    A silent check is the failure mode of this whole file, and it has happened twice: four
    cross-boundary checks drawing only symbols that cross no boundary, and an envelope
    assertion whose search term did not exist in the corpus being checked. Neither showed
    up as anything other than `no contradictions found`.

    This is deliberately *not* an error. A corpus that genuinely has no ambiguous names has
    nothing for that check to do, and failing the run would train people to ignore it. It
    is printed, every time, so the reader knows the difference between "held" and "never
    asked"."""
    idle = [c for c in ALL_CHECKS if c not in f.reached]
    live = len(ALL_CHECKS) - len(idle)
    print(f"{live}/{len(ALL_CHECKS)} checks reached an assertion on this corpus")
    if idle:
        print("  never got past their guards here - not a pass, an absence of evidence:")
        for c in idle:
            print(f"    - {c}")
    print()


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
    shared = shared_names(db, n)
    print(
        f"{len(picks)} symbols and {len(shared)} shared names, stratified and seeded\n"
    )
    # Whole-tool checks, once rather than per symbol.
    check_envelope_and_exit_codes(db, repo, f)
    check_literal_agrees_with_find(db, repo, f)
    check_staleness_agrees(db, repo, f)
    for name in shared:
        check_both_purposes_resolve_the_same_subject(db, name, f)
    for prefix in prefixes(db, 6):
        check_unreached_is_really_unused(db, prefix, f)
    for i, (label, handle) in enumerate(picks, 1):
        check_reaches_symmetry(db, handle, f, generated)
        check_nothing_reaches_itself_across_a_boundary(db, handle, f)
        check_affects_covers_methods(db, handle, f)
        check_usage_within_refs(db, handle, f)
        check_determinism(db, handle, f)
        check_budget_admits_what_it_cut(db, handle, f)
        check_runs_agrees_with_affects(db, handle, f)
        check_handles_resolve(db, handle, f)
        check_understand_matches_its_own_citation(db, handle, f)
        check_the_chain_was_followed_to_where_it_says(db, handle, f)
        check_printed_line_is_where_the_definition_is(db, handle, f)
        if i % 10 == 0:
            print(f"  ...{i}/{len(picks)}", flush=True)

    print()
    report_coverage(f)
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
