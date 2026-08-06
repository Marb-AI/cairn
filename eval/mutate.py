#!/usr/bin/env python3
"""Does the suite catch a regression, or only agree with the code?

235 checks are green. Nine have been shown able to fail. The other 226 are in the state the
weak-link layer was in all day: green, and green either way. This applies realistic
regressions - each one a defect this repository has actually had, or the obvious way a
guarantee gets dropped - and reports which the suite notices.

A mutation that survives names a guarantee nothing is defending.

Sources are restored from copies, not from git: `git checkout` on a working file destroyed
an hour of uncommitted work earlier today.
"""
import pathlib
import shutil
import tempfile
import subprocess
import sys

SCRATCH = pathlib.Path(tempfile.mkdtemp(prefix="cairn-mutate-"))
CI = ["docker", "compose", "run", "--rm", "ci", "sh", "-c"]

# (name, file, find, replace) - each is a plausible regression, not a random edit.
MUT = [
    ("every negative exits 0 instead of 1",
     "crates/cairn-cli/src/main.rs",
     "if found { exit::FOUND } else { exit::NOT_FOUND }",
     "exit::FOUND",
     "all"),
    ("empty query accepted by `symbol`",
     "crates/cairn-cli/src/main.rs",
     'eprintln!("cairn: empty query - give a name, or part of one");\n                return Ok(exit::ERROR);',
     'eprintln!("cairn: empty query");'),
    ("`--limit` cut no longer declared",
     "crates/cairn-fmt/src/lib.rs",
     '            "more matches beyond --limit; raise it, or narrow the query - a first page \\\n             that does not say so is indistinguishable from the whole answer"\n                .to_string(),',
     '            String::new(),'),
    ("staleness marking becomes a no-op",
     "crates/cairn-fmt/src/lib.rs",
     "pub fn mark_stale(mut self, dirty: Option<&[String]>, mentioned: &[String]) -> Self {",
     "pub fn mark_stale(mut self, dirty: Option<&[String]>, mentioned: &[String]) -> Self {\n        if true {\n            let _ = (&mut self, dirty, mentioned);\n            return self;\n        }"),
    ("tree probe claims a checked absence without reading",
     "crates/cairn-cli/src/main.rs",
     "let f = treefind::search(&root, &query, 200);",
     "let f = treefind::search(&root, \"\\u{0}unmatchable\\u{0}\", 200);"),
    ("collapse hides lines without declaring them",
     "crates/cairn-fmt/src/lib.rs",
     '            "{} line(s) in test or generated files, counted in the header but not listed \\\n             - `cairn for find \\"{needle}\\" --all` lists every hit",\n            hits.len() - listed.len()',
     '            "{} line(s)", 0'),
    ("collapse fires on every answer, however small",
     "crates/cairn-fmt/src/lib.rs",
     "const CLASSIFY_AT: usize = 20;",
     "const CLASSIFY_AT: usize = 1;"),
    ("empty body allowed again",
     "crates/cairn-fmt/src/lib.rs",
     "        && hits.iter().any(|h| !derived(h));",
     "        && true;"),
    ("derived root no longer corroborated",
     "crates/cairn-cli/src/main.rs",
     "plausible_root(&root, &paths, |p| p.exists()).then_some(root)",
     "{ let _ = paths; Some(root) }"),
    ("a watcher is started on the filesystem root",
     "crates/cairn-cli/src/main.rs",
     "    if is_filesystem_root(root) {\n        return false;\n    }",
     "    if false {\n        return false;\n    }"),
    ("generated definitions counted as hand-written",
     "crates/cairn-fmt/src/lib.rs",
     ".all(|r| r.def.as_ref().map(|d| d.generated).unwrap_or(false));",
     ".all(|_r| false);"),
]


def run(cmd, timeout=1500):
    return subprocess.run(CI + [cmd], capture_output=True, text=True, timeout=timeout)


def suite():
    """The whole green wall: workspace tests plus the corpus cases."""
    # The whole output, filtered here rather than by `tail` in the shell: a failure in an
    # early crate scrolled off the end and was read as SURVIVED, which is the same mistake
    # in the harness that the harness exists to find in the tool.
    r = run("cargo test --workspace 2>&1")
    out = r.stdout + r.stderr
    # Order matters, and getting it wrong cost a third plausible table: `cargo test` stops
    # after a target fails, so a *caught* mutation never reaches `tests/corpus.rs` and a
    # "did the suite run?" guard placed first reports it as if nothing had been checked.
    # A failure anywhere is a catch; only a run with no failures needs the guard.
    if "FAILED" in out or "test failed" in out:
        return "CAUGHT", out
    if "error[E" in out or "could not compile" in out:
        return "BUILD-FAIL", out
    if "Running tests/corpus.rs" not in out:
        return "SUITE-NOT-RUN", out
    return "SURVIVED", out


def main():
    results = []
    for entry in MUT:
        name, path, find, repl = entry[:4]
        every = len(entry) > 4
        f = pathlib.Path(path)
        keep = SCRATCH / (f.name + ".keep")
        shutil.copy(f, keep)
        src = f.read_text()
        if find not in src:
            print(f"SKIP      {name}  (anchor not found in {path})", flush=True)
            results.append((name, "ANCHOR-GONE"))
            continue
        f.write_text(src.replace(find, repl) if every else src.replace(find, repl, 1))
        try:
            verdict, out = suite()
        finally:
            shutil.copy(keep, f)
        results.append((name, verdict))
        print(f"{verdict:<11} {name}", flush=True)
        if verdict == "BUILD-FAIL":
            for line in out.splitlines():
                if line.startswith("error"):
                    print(f"              {line[:100]}")
                    break

    print()
    caught = [n for n, v in results if v == "CAUGHT"]
    survived = [n for n, v in results if v == "SURVIVED"]
    other = [(n, v) for n, v in results if v not in ("CAUGHT", "SURVIVED")]
    print(f"{len(caught)}/{len(caught) + len(survived)} realistic regressions are caught by the suite")
    if survived:
        print("\nNOT defended by any check:")
        for n in survived:
            print(f"  - {n}")
    if other:
        print("\ninconclusive (mutation did not compile or anchor moved):")
        for n, v in other:
            print(f"  - {n}: {v}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
