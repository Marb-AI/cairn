//! Byte-level output gate: every read command, recorded, compared.
//!
//! The other three test layers each assert something *about* an answer — an exit code
//! from the contract, a substring that must appear, a count that must hold. None of them
//! would notice a refactor that moved a line, dropped a column, or reworded a note while
//! keeping all of that true. The connector split moves a great deal of code behind the
//! answers, and the eval in `eval/` cannot stand in for a regression test: it drives an
//! LLM, so it is neither deterministic nor cheap.
//!
//! So this records bytes. Every read command against a mechanically chosen sample of the
//! fixture index, stdout and exit code, compared against files committed beside it. A
//! change that was meant to be invisible shows up as a diff; a change that was meant to
//! be visible shows up as a diff a reviewer reads before blessing it.
//!
//! `CAIRN_BLESS=1 cargo test --test golden` rewrites the recordings.
//!
//! Three things keep it honest rather than merely green:
//!
//!   * The fixture corpus, never a real checkout. `sweep.rs` prefers a workstation's own
//!     index because a wider corpus finds more; here the point is the opposite — the
//!     recording has to be the same on every machine or it cannot be committed. A real
//!     index is still worth walking, and `scripts/golden-local.sh` does exactly that
//!     walk against one, into a directory to diff by hand.
//!   * Paths that differ per machine are replaced by placeholders, and nothing else is
//!     normalised. Every byte a command prints beyond that is part of the gate.
//!   * Stdout *and* the exit code, per invocation. Exit codes are the contract an agent
//!     branches on (architecture 6.1.1), and a refactor can change one without touching
//!     a single printed character.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
use common::build_fixture_index;

/// How many symbols the walk lands on.
///
/// Fixed, not configurable: the recordings are committed for this sample, and a sample
/// that varied with the environment would mean the files could not be compared at all.
/// `sweep.rs` takes `CAIRN_SWEEP_SAMPLE` because its assertions are properties; these
/// are bytes. Local mode (see `scripts/golden-local.sh`) is where a wider walk belongs.
const SAMPLE: usize = 24;

/// How many `path` cases to record. Pairs of neighbouring sampled handles: most have no
/// route between them, which is itself an answer worth locking down.
const PATH_PAIRS: usize = 8;

/// How many sampled symbols also contribute their name and directory as query inputs.
/// Four is enough for the commands that take text rather than a handle to be exercised
/// on any index without the recordings doubling in size.
const DERIVED_INPUTS: usize = 4;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn binary() -> PathBuf {
    let mut p = std::env::current_exe().expect("test binary");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("cairn")
}

/// One recorded invocation: the arguments, and the file it is recorded in.
struct Case {
    /// Which recording this belongs to. One file per command keeps a diff readable —
    /// a single large file would put every command's drift in one review.
    file: &'static str,
    args: Vec<String>,
}

fn case(file: &'static str, args: &[&str]) -> Case {
    Case {
        file,
        args: args.iter().map(|a| a.to_string()).collect(),
    }
}

/// Every read command, against the sample.
///
/// Commands that write are absent by construction: `index`, `weak`, `link`, `concept
/// add|link|drop`, `llm verify --check`, `daemon`, `live`, `skill`, `config <assignment>`.
/// `config` without one reads installation settings that belong to the machine rather
/// than to the index, so it cannot be recorded either.
fn cases(sampled: &[Sampled], repo: &Path) -> Vec<Case> {
    let repo = repo.to_string_lossy().to_string();
    // Answers about the whole index, recorded once.
    let mut out = vec![
        case("status", &["status"]),
        case("verify", &["verify"]),
        case("verify", &["verify", "--repo", &repo]),
        case("topology", &["topology"]),
        case("entrypoints", &["entrypoints"]),
        case("rules", &["rules"]),
        case("docs", &["docs"]),
        case("docs", &["docs", "README.md"]),
        case("docs", &["docs", "--about", "telemetry"]),
        case("concept", &["concept", "list"]),
    ];

    // Queries rather than handles, and two kinds of input.
    //
    // Hand-picked ones name the fixture corpus: a dispatched handler, a registry, a term
    // that only exists in a literal. They are the cases worth recording and they are only
    // as stable as the corpus — adding a symbol to it changes these recordings, the same
    // cost `cases.yaml` already pays and says so out loud.
    //
    // Derived ones come out of the walk itself, so these commands are exercised against
    // any index. Without them, `scripts/golden-local.sh` on a real checkout would record
    // "nothing by that name" for every query case: the fixture's invented names exist
    // nowhere else, and the commands that take a name rather than a handle would be the
    // ones the local walk could not say anything about.
    for q in ["Dispatcher", "handle_severe_cold", "Registry", "ingest"] {
        out.push(case("symbol", &["symbol", q]));
    }
    out.push(case(
        "symbol",
        &["symbol", "ThisSymbolDoesNotExistAnywhere"],
    ));
    for q in ["alerting", "telemetry ingest"] {
        out.push(case("context", &["context", q]));
    }
    for prefix in ["srcgo/internal/station", "srcpy/alerting", "srcgo"] {
        out.push(case("outline", &["outline", prefix]));
        out.push(case("unreached", &["unreached", prefix]));
    }
    for text in ["telemetry", "severe", "station_id"] {
        out.push(case("literal", &["literal", text, "--repo", &repo]));
    }
    out.push(case(
        "for-find",
        &["for", "find", "severe", "--repo", &repo],
    ));
    out.push(case(
        "for-find",
        &["for", "find", "severe", "--all", "--repo", &repo],
    ));

    for s in sampled.iter().take(DERIVED_INPUTS) {
        out.push(case("symbol", &["symbol", &s.name]));
        out.push(case("context", &["context", &s.name]));
        out.push(case("literal", &["literal", &s.name, "--repo", &repo]));
        out.push(case("for-find", &["for", "find", &s.name, "--repo", &repo]));
    }
    // Unique in walk order rather than sorted: the recording reads in the order the walk
    // produced, and `dedup` alone would keep a directory that reappears after another.
    let mut dirs: Vec<&String> = Vec::new();
    for dir in sampled
        .iter()
        .take(DERIVED_INPUTS)
        .filter_map(|s| s.dir.as_ref())
    {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    for dir in dirs {
        out.push(case("outline", &["outline", dir]));
        out.push(case("unreached", &["unreached", dir]));
    }

    // Per symbol. Every command that takes a handle, and for the ones whose flags change
    // what is *computed* rather than only how it prints, both ways.
    for s in sampled {
        let h = &s.handle;
        out.push(case("refs", &["refs", h]));
        out.push(case("refs", &["refs", h, "--include-generated"]));
        out.push(case(
            "refs",
            &["refs", h, "--context", "line", "--repo", &repo],
        ));
        // The budget is a separate code path from `--limit`, and what it cut has to come
        // back in `suppressed`. Three truncation defects had the shape of cutting silently.
        out.push(case("refs", &["refs", h, "--budget", "300"]));
        out.push(case("usage", &["usage", h]));
        out.push(case("usage", &["usage", h, "--include-tests"]));
        out.push(case("expand", &["expand", h]));
        out.push(case(
            "expand",
            &["expand", h, "--detail", "doc", "--repo", &repo],
        ));
        out.push(case(
            "expand",
            &["expand", h, "--detail", "body", "--repo", &repo],
        ));
        for aspect in ["callers", "calls", "impls", "tests"] {
            out.push(case("graph", &["graph", h, "--aspect", aspect]));
        }
        out.push(case(
            "graph",
            &["graph", h, "--aspect", "callers", "--view", "list"],
        ));
        out.push(case(
            "graph",
            &["graph", h, "--aspect", "callers", "--exclude-tests"],
        ));
        out.push(case("reaches", &["reaches", h]));
        out.push(case("reaches", &["reaches", h, "--outgoing"]));
        out.push(case("runs", &["runs", h]));
        out.push(case("affects", &["affects", h]));
        out.push(case("weaklinks", &["weaklinks", h]));
        out.push(case("links", &["links", h]));
        out.push(case("entrypoints", &["entrypoints", "--reaches", h]));
        out.push(case("for-change", &["for", "change", h]));
        out.push(case(
            "for-understand",
            &["for", "understand", h, "--repo", &repo],
        ));
    }

    for pair in sampled.windows(2).take(PATH_PAIRS) {
        out.push(case("path", &["path", &pair[0].handle, &pair[1].handle]));
    }

    // A hand-picked input and a derived one can be the same string — a sampled symbol
    // living in a directory the list above already names. Recording that answer twice
    // only gives a reviewer the same block to read again.
    let mut seen: Vec<(&str, Vec<String>)> = Vec::new();
    out.retain(|c| {
        let key = (c.file, c.args.clone());
        if seen.contains(&key) {
            return false;
        }
        seen.push(key);
        true
    });

    out
}

/// The invocation as a reader would have to type it.
///
/// Quoted where it matters: one sampled symbol has an empty name, and `cairn symbol ""`
/// printed as `$ cairn symbol` — an invocation that means something else entirely. The
/// same goes for a query with a space in it.
fn shell(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.is_empty() || a.contains(char::is_whitespace) {
                format!("'{a}'")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One symbol the walk landed on, with what the query commands need to name it.
struct Sampled {
    handle: String,
    name: String,
    /// Directory the symbol is defined in, for `outline` and `unreached`.
    dir: Option<String>,
}

/// The sample, ordered and stepped exactly as `sweep.rs` does it.
///
/// Same walk on purpose: the two tests then cover the same symbols, so a property the
/// sweep catches and a byte the recording catches are about the same answer.
fn walk(db: &Path, sample: usize) -> Vec<Sampled> {
    let conn = rusqlite::Connection::open(db).expect("opening the index");
    let mut stmt = conn
        .prepare(
            "SELECT h.handle, n.s, p.s
               FROM handles h
               JOIN symbols s ON s.id = h.symbol_id
               JOIN strings n ON n.id = s.name_id
               LEFT JOIN files   f ON f.id = s.def_file_id
               LEFT JOIN strings p ON p.id = f.path_id
              ORDER BY h.symbol_id",
        )
        .expect("listing handles");
    let all: Vec<Sampled> = stmt
        .query_map([], |r| {
            Ok(Sampled {
                handle: r.get(0)?,
                name: r.get(1)?,
                dir: r
                    .get::<_, Option<String>>(2)?
                    .and_then(|p| p.rsplit_once('/').map(|(d, _)| d.to_string())),
            })
        })
        .expect("reading handles")
        .filter_map(|h| h.ok())
        .collect();
    drop(stmt);
    drop(conn);

    // Forced odd for the reason the sweep gives: symbols arrive grouped by file, and an
    // even stride can land on the same position within each of them for a whole run.
    let stride = ((all.len() / sample.max(1)).max(1) | 1).max(1);
    all.into_iter().step_by(stride).take(sample).collect()
}

/// Everything about an answer that is allowed to differ between two machines.
///
/// Only paths. A recording that normalised anything else would stop being a gate for it,
/// and the drift a gate is for is exactly the kind nobody predicted.
fn scrub(text: &str, db: &Path, repo: &Path) -> String {
    let mut out = text.to_string();
    // Longest first: the database sits inside the scratch directory, and replacing the
    // directory first would leave a half-substituted path behind.
    let db_dir = db.parent().map(|p| p.to_path_buf());
    for (from, to) in [
        (Some(db.to_path_buf()), "<db>"),
        (Some(repo.to_path_buf()), "<repo>"),
        (db_dir, "<dbdir>"),
    ] {
        if let Some(from) = from {
            out = out.replace(&from.to_string_lossy().to_string(), to);
        }
    }
    out
}

/// Run one case and render it as the recording stores it.
fn record(bin: &Path, db: &Path, repo: &Path, c: &Case) -> String {
    let out = Command::new(bin)
        .arg("--db")
        .arg(db)
        .args(&c.args)
        .output()
        .unwrap_or_else(|e| panic!("running cairn {}: {e}", c.args.join(" ")));

    let mut block = String::new();
    // The echoed command is scrubbed too. `--repo` carries an absolute path, and it was
    // recorded verbatim at first — which made every recording specific to the checkout
    // that produced it while the answers underneath them were portable.
    let _ = writeln!(
        block,
        "$ cairn {}",
        scrub(&shell(&c.args), db, repo).trim_end()
    );
    let _ = writeln!(
        block,
        "exit {}",
        out.status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string())
    );
    let stdout = scrub(&String::from_utf8_lossy(&out.stdout), db, repo);
    block.push_str(&stdout);
    if !stdout.ends_with('\n') {
        block.push('\n');
    }
    block.push('\n');
    block
}

/// What a recording says about itself. The committed ones and the local ones are read in
/// different situations: one is reviewed in a diff, the other is produced twice and
/// compared by hand, and a file that claimed to be the first while being the second would
/// send a reader looking for a corpus the numbers did not come from.
const COMMITTED_HEADER: &str = "\
# Recorded output of one cairn command against the fixture corpus. Do not edit by hand:
# CAIRN_BLESS=1 cargo test --test golden rewrites every file here.
#
# A diff in this file is a change in what the tool says. Read it before blessing it:
# `<db>`, `<dbdir>` and `<repo>` stand in for paths that differ per machine, and nothing
# else in an answer is normalised.
";

const LOCAL_HEADER: &str = "\
# Recorded output of one cairn command against a local index, by
# scripts/golden-local.sh. Not committed and not compared by any test: produce this twice
# around a change and diff the two directories.
#
# `<db>`, `<dbdir>` and `<repo>` stand in for paths, so two runs against the same checkout
# are comparable; nothing else in an answer is normalised.
";

/// Group the cases into one rendering per file, in case order.
fn render(
    bin: &Path,
    db: &Path,
    repo: &Path,
    header: &str,
    cases: &[Case],
) -> BTreeMap<&'static str, String> {
    let mut files: BTreeMap<&'static str, String> = BTreeMap::new();
    for c in cases {
        let entry = files.entry(c.file).or_insert_with(|| {
            let mut s = header.to_string();
            s.push('\n');
            s
        });
        entry.push_str(&record(bin, db, repo, c));
    }
    files
}

/// Where the first difference is, with enough around it to read.
fn first_difference(expected: &str, actual: &str) -> String {
    let (e, a): (Vec<&str>, Vec<&str>) = (expected.lines().collect(), actual.lines().collect());
    let at = e
        .iter()
        .zip(&a)
        .position(|(x, y)| x != y)
        .unwrap_or(e.len().min(a.len()));
    let from = at.saturating_sub(4);
    let mut s = String::new();
    // The `$ cairn ...` line above the difference is what names the invocation that drifted.
    if let Some(cmd) = e[..=at.min(e.len().saturating_sub(1))]
        .iter()
        .rfind(|l| l.starts_with("$ cairn "))
    {
        let _ = writeln!(s, "  in: {cmd}");
    }
    let _ = writeln!(s, "  line {}:", at + 1);
    for (label, lines) in [("recorded", &e), ("now     ", &a)] {
        for (i, l) in lines.iter().enumerate().skip(from).take(9) {
            let mark = if i == at { ">" } else { " " };
            let _ = writeln!(s, "  {label} {mark} {:>5}: {l}", i + 1);
        }
    }
    s
}

#[test]
fn every_read_command_prints_what_it_printed_before() {
    let root = workspace_root();
    let bin = binary();
    assert!(bin.exists(), "cairn not built at {}", bin.display());
    let bless = std::env::var_os("CAIRN_BLESS").is_some();

    // Local mode: the same walk against a real index, written to a directory rather than
    // compared, so a change can be diffed on a codebase far larger than the fixture.
    // Nothing is asserted here — the developer reads the diff.
    if let (Some(db), Some(out)) = (
        std::env::var_os("CAIRN_GOLDEN_DB"),
        std::env::var_os("CAIRN_GOLDEN_OUT"),
    ) {
        let db = PathBuf::from(db);
        let out = PathBuf::from(out);
        assert!(db.exists(), "no index at {}", db.display());
        let repo = std::env::var_os("CAIRN_GOLDEN_REPO")
            .map(PathBuf::from)
            .or_else(|| db.parent().and_then(|d| d.parent()).map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."));
        let sample = std::env::var("CAIRN_GOLDEN_SAMPLE")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(SAMPLE);
        let handles = walk(&db, sample);
        assert!(
            handles.len() >= 2,
            "{} has {} handles, too few to walk",
            db.display(),
            handles.len()
        );
        std::fs::create_dir_all(&out).expect("output directory");
        let files = render(&bin, &db, &repo, LOCAL_HEADER, &cases(&handles, &repo));
        for (name, body) in &files {
            std::fs::write(out.join(format!("{name}.txt")), body).expect("writing a recording");
        }
        eprintln!(
            "wrote {} recordings for {} symbols of {} to {}",
            files.len(),
            handles.len(),
            db.display(),
            out.display()
        );
        return;
    }

    let Some(db) = build_fixture_index(&root, &bin, "golden") else {
        // Unlike the sweep, this cannot fall back to a real index: the recordings are
        // committed, so they are only comparable against the corpus they were made from.
        eprintln!("SKIP: no fixture SCIP to build an index from");
        return;
    };
    let repo = root.join("crates/cairn-cli/tests/fixtures/corpus");
    let handles = walk(&db, SAMPLE);
    assert_eq!(
        handles.len(),
        SAMPLE,
        "the fixture index yielded {} of {SAMPLE} sampled handles, so the recordings \
         cover less than they claim",
        handles.len()
    );

    let dir = root.join("crates/cairn-cli/tests/golden");
    let files = render(&bin, &db, &repo, COMMITTED_HEADER, &cases(&handles, &repo));

    if bless {
        std::fs::create_dir_all(&dir).expect("golden directory");
        // Recordings for commands that no longer exist would otherwise sit here looking
        // authoritative while nothing compares against them.
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let keep = e
                    .path()
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| files.contains_key(s))
                    .unwrap_or(false);
                if !keep && e.path().extension().is_some_and(|x| x == "txt") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        for (name, body) in &files {
            std::fs::write(dir.join(format!("{name}.txt")), body).expect("writing a recording");
        }
        eprintln!(
            "blessed {} recordings across {} symbols",
            files.len(),
            handles.len()
        );
        return;
    }

    let mut drifted: Vec<String> = Vec::new();
    for (name, body) in &files {
        let path = dir.join(format!("{name}.txt"));
        match std::fs::read_to_string(&path) {
            Ok(expected) if expected == *body => {}
            Ok(expected) => drifted.push(format!(
                "{name}.txt differs:\n{}",
                first_difference(&expected, body)
            )),
            Err(_) => drifted.push(format!("{name}.txt is missing")),
        }
    }
    assert!(
        drifted.is_empty(),
        "{} of {} recordings changed. If the change is intended, read the diff and \
         bless it with `CAIRN_BLESS=1 cargo test --test golden`:\n{}",
        drifted.len(),
        files.len(),
        drifted.join("\n")
    );
    eprintln!(
        "{} recordings identical across {} symbols",
        files.len(),
        handles.len()
    );
}
