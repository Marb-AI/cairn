//! `cairn` — local code navigation for coding agents.
//!
//! CLI rather than an MCP server (architecture D1): an agent runs commands natively,
//! and this way the tool also works in CI, a Makefile and a human terminal. Startup
//! cost is on the hot path (every query is a fresh process), so nothing expensive
//! happens before the subcommand is known.

use anyhow::{Context, Result};
use cairn_fmt::{Budget, Detail, Source, View};
use cairn_store::{ingest, Direction, EdgeKind, Store};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

/// Exit codes are part of the contract: an agent must be able to tell "nothing is
/// there" from "I cannot see" (architecture 6.1.1).
mod docker;
mod index;
mod purpose;
mod treefind;
mod skill;
mod track;

mod exit {
    pub const FOUND: u8 = 0;
    pub const NOT_FOUND: u8 = 1;
    pub const ERROR: u8 = 2;
    pub const DEGRADED: u8 = 3;
}

#[derive(Parser)]
#[command(name = "cairn", version, about = "Local code navigation for agents")]
struct Cli {
    /// Index database. Defaults to $CAIRN_DB, else the nearest .cairn/index.sqlite
    /// at or above the working directory.
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    /// Ceiling on the size of the answer, in tokens. The tool fills it with the
    /// highest-ranked rows and reports what it left out, so you do not have to guess
    /// a --limit and then ask again.
    #[arg(long, global = true)]
    budget: Option<usize>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum ConceptCmd {
    /// Create or update a concept.
    Add {
        name: String,
        #[arg(long, default_value = "")]
        note: String,
        #[arg(long, default_value = cairn_store::concepts::DEFAULT_NS)]
        ns: String,
        #[arg(long, default_value = "agent")]
        by: String,
    },
    /// Attach a symbol to a concept.
    Link {
        name: String,
        handle: String,
        /// Free-text relation: part-of, entry-point, owns, ...
        #[arg(long, default_value = "part-of")]
        rel: String,
        #[arg(long, default_value = "")]
        note: String,
        #[arg(long, default_value = cairn_store::concepts::DEFAULT_NS)]
        ns: String,
    },
    /// Show a concept and everything attached to it.
    Show {
        name: String,
        #[arg(long, default_value = cairn_store::concepts::DEFAULT_NS)]
        ns: String,
    },
    /// List concepts, optionally in one namespace.
    List {
        #[arg(long)]
        ns: Option<String>,
    },
    /// Discard a whole namespace in one move.
    Drop { ns: String },
}

#[derive(Subcommand)]
enum LlmCmd {
    /// List the claims that need judging, or record a verdict on one.
    Verify {
        /// Record against this check id instead of listing. Ids come from the listing.
        #[arg(long)]
        check: Option<String>,
        /// The claim holds.
        #[arg(long, conflicts_with = "broken")]
        holds: bool,
        /// The claim does not hold. Say why in --note: a bare "wrong" cannot be acted on.
        #[arg(long)]
        broken: bool,
        /// What was found. Required with --broken.
        #[arg(long)]
        note: Option<String>,
        /// Repo root, for reading the commit a verdict is anchored to.
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

/// Which relation a graph command follows.
#[derive(Clone, Copy, clap::ValueEnum)]
enum Aspect {
    /// Who calls this symbol.
    Callers,
    /// What this symbol calls.
    Calls,
    /// Implementations of this interface, or what this type implements.
    Impls,
    /// Tests that reach this symbol through the call graph.
    Tests,
}

/// What the caller is doing. Deliberately a closed set rather than free text: matching a
/// sentence means either calling a model, which D1 forbids, or keyword-matching while
/// pretending to understand - and a tool that guesses intent silently is the exact
/// failure this project keeps finding and fixing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Purpose {
    /// I am going to modify this symbol. What breaks, and how far does it reach?
    Change,
    /// Where is this text - a value, a key, a header, a name - and whose line is it?
    Find,
}

#[derive(Subcommand)]
enum Cmd {
    /// Index the repository you are standing in.
    ///
    /// Takes nothing in the normal case: the working directory is the repository, the
    /// languages are whatever the tree actually contains, and the indexers are run for
    /// you. Passing .scip files instead skips that and ingests them directly.
    Index {
        /// Ingest these .scip files rather than producing any. Rarely needed.
        indexes: Vec<PathBuf>,
        /// Repository root. Defaults to the working directory.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Do not install the agent guide into .claude/skills/.
        #[arg(long)]
        without_skill: bool,
    },
    /// Install the agent guide into this repository's .claude/skills/.
    ///
    /// Done for you by `cairn index`; this is here for the case where that was skipped,
    /// or where the guide has moved on since.
    Skill,
    /// Say what you are doing; cairn picks how to answer it.
    ///
    /// The command surface below this one is named after mechanisms - `refs`, `graph`,
    /// `path`, `usage` - and measurement showed that is the wrong shape for the moment
    /// an agent asks. It knows its purpose reliably and picks the mechanism badly: one
    /// run spent eight calls across four commands on a question no symbol command
    /// answers, and on another the mechanism that fits (`path`) needs both ends of a
    /// chain when the whole question is what the far end is.
    ///
    /// So: purpose first. Every block of the answer names the command that produced it,
    /// which is the way down to the mechanism when this shape is not what you needed.
    For {
        #[arg(value_enum)]
        purpose: Purpose,
        /// A handle, a symbol name, or - for `find` - any text.
        subject: String,
        #[arg(long, default_value_t = 60)]
        limit: usize,
        /// Repository root. Defaults to the working directory.
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Entry point by concept: turn "the OAuth stuff" into symbols to start from.
    Context {
        query: String,
        #[arg(long, default_value_t = 12)]
        limit: usize,
    },
    /// Symbols under a path that production code never calls.
    Unreached {
        /// Repo-relative path prefix, e.g. srcpy/domains/orders/lib/pricing
        prefix: String,
        #[arg(long, default_value_t = 60)]
        limit: usize,
    },
    /// What a module or directory contains, and how used each thing is.
    Outline {
        prefix: String,
        #[arg(long, default_value_t = 80)]
        limit: usize,
    },
    /// Where a symbol is used, grouped by file - the blast radius of changing it.
    Usage {
        handle: String,
        #[arg(long)]
        include_tests: bool,
        #[arg(long, default_value_t = 40)]
        limit: usize,
    },
    /// Find symbols by name.
    Symbol {
        query: String,
        #[arg(long, default_value_t = 15)]
        limit: usize,
    },
    /// Show references to a symbol.
    Refs {
        handle: String,
        #[arg(long)]
        include_generated: bool,
        #[arg(long, default_value_t = 40)]
        limit: usize,
        /// How much source to show at each site: none | line | block | <n> | auto.
        /// `auto` divides --budget by the number of sites, so few sites get a block and
        /// many get a line. Needs --repo. Far cheaper than opening the files.
        #[arg(long, default_value = "none")]
        context: String,
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Walk the call graph or implementation relations.
    Graph {
        handle: String,
        #[arg(long, value_enum, default_value_t = Aspect::Callers)]
        aspect: Aspect,
        /// How many hops out from the root.
        #[arg(long, default_value_t = 2)]
        depth: usize,
        /// How many neighbours to follow per node.
        #[arg(long, default_value_t = 8)]
        fanout: usize,
        /// Layout: `tree` shows how each node was reached, `list` is flat and cheaper.
        #[arg(long, default_value = "tree")]
        view: String,
        /// How much of each node to print: skeleton | signature | doc | body.
        /// Anything but `skeleton` needs --repo. Use `body` for audit passes.
        #[arg(long, default_value = "skeleton")]
        detail: String,
        /// Skip nodes defined in test files. The question behind "who uses this" is
        /// usually whether anything in production does.
        #[arg(long)]
        exclude_tests: bool,
        /// Repo root, required when --detail prints source.
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// The conventions cairn reads the world with, and where they came from.
    ///
    /// What a start command looks like, how a protobuf generator names things, what marks
    /// a file as generated, where tests live. Copy the output to `.cairn/rules.yaml` and
    /// edit it to change any of them without rebuilding (architecture D16).
    Rules,
    /// Show or change cairn's own settings.
    ///
    /// These belong to the installation rather than to any repository, so they live
    /// beside the binary and one setting serves every checkout on the machine.
    Config {
        /// A setting to change, as `key=value`. Without one, prints what is in effect.
        /// `key=unset` restores the default.
        assignment: Option<String>,
    },
    /// Deployed services and what each one runs.
    Topology,
    /// Every way code gets started: start commands, cron entries, on-demand runners.
    ///
    /// One row per entrypoint rather than per service, each ending in the file it lands
    /// in - `cairn outline <that path>` is the way down from here. With --reaches it
    /// answers the audit direction instead: which of them can actually run this symbol.
    Entrypoints {
        /// Only entrypoints from which this symbol can be run.
        #[arg(long)]
        reaches: Option<String>,
    },
    /// Every deployed service a change here touches, in-process and over the network.
    ///
    /// One call instead of `runs` plus `reaches` per hop plus `topology`: measurement
    /// showed the cost of this question was in assembling those by hand, not in asking.
    Affects {
        handle: String,
        #[arg(long, default_value_t = 12)]
        depth: usize,
        #[arg(long, default_value_t = 200)]
        fanout: usize,
    },
    /// Which deployed services can run this code - the filesystem cannot say.
    Runs {
        handle: String,
        #[arg(long, default_value_t = 12)]
        depth: usize,
    },
    /// Who reaches this across a gRPC boundary - the query no name search can answer.
    Reaches {
        handle: String,
        /// Show what this symbol reaches instead of what reaches it.
        #[arg(long)]
        outgoing: bool,
    },
    /// Shortest call path between two symbols: how does one reach the other.
    Path {
        from: String,
        to: String,
        #[arg(long, default_value_t = 8)]
        max_depth: usize,
        #[arg(long, default_value = "skeleton")]
        detail: String,
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Show a symbol in more detail.
    Expand {
        handle: String,
        /// `skeleton` = identity only, `doc` = leading comment, `body` = source text.
        #[arg(long, default_value = "skeleton")]
        detail: String,
        /// Repo root, needed for `--detail body|doc`.
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Build the weak-link layer: string literals that name a symbol.
    Weak {
        /// Repo root; file paths in the index are relative to it.
        #[arg(long)]
        repo: PathBuf,
    },
    /// Sites whose string literals name this symbol - candidate dynamic references.
    Weaklinks {
        handle: String,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// Report what the index does NOT know, and whether it still matches the repo.
    Verify {
        /// Repo root. Without it staleness cannot be checked and the report says so.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Mark hand-authored links whose anchor file has changed.
        #[arg(long)]
        flag_stale: bool,
    },
    /// Record a link the static pass cannot see.
    Link {
        from: String,
        to: String,
        /// Why this link exists. Required: an unexplained assertion is unreviewable.
        #[arg(long)]
        note: String,
        /// Who is asserting it.
        #[arg(long, default_value = "agent")]
        by: String,
    },
    /// Hand-authored links touching a symbol.
    Links { handle: String },
    /// Named nodes that are not symbols, and their links to code.
    #[command(subcommand)]
    Concept(ConceptCmd),
    /// Run the live-state daemon: watches the repo and reports what has changed.
    Daemon {
        #[arg(long)]
        repo: PathBuf,
        /// Stop a running daemon instead of starting one.
        #[arg(long)]
        stop: bool,
    },
    /// Symbols in a file as the language server sees it *now* - the dirty overlay.
    /// Answers about a changed file that the index cannot.
    Live { path: String },
    /// Find a string literal, and get whose function it sits in.
    ///
    /// The thing SCIP cannot carry, because a literal is not a symbol: a header name, a
    /// dict key, a feature flag. grep finds the line faster and is never stale — what it
    /// cannot say is whose line it is, which is the question you were going to ask next.
    ///
    /// The surrounding source comes back by default. Asking for it separately would cost
    /// an inference, and an inference costs more than everything this command does.
    Literal {
        /// Text to look for inside string literals. Case-insensitive substring.
        text: String,
        /// How much source at each site: none | line | block | <n>.
        #[arg(long, default_value = "block")]
        context: String,
        /// Repo root. Worked out from the index's own location when omitted, which is
        /// right whenever the index sits at the conventional `<repo>/.cairn/`.
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// The documentation map: which document holds what, and what each costs to read.
    ///
    /// Headings and line ranges, never the prose. With no argument it lists the
    /// documents; with a path it gives that one's section skeleton; with --about it
    /// finds the sections whose heading, or where that heading sits, names your words.
    ///
    /// The answer is always a line range, so the next step is reading thirty lines
    /// rather than four files. For a word inside the prose, grep is still the tool.
    Docs {
        /// A markdown path, for that document's sections.
        path: Option<String>,
        /// Sections whose heading or trail names this.
        #[arg(long, conflicts_with = "path")]
        about: Option<String>,
    },
    /// What is indexed, and how stale it is.
    Status,
    /// Claims the index cannot check about itself, put to whoever is reading.
    ///
    /// No model is called: cairn is a CLI an agent drives, and the agent is the one that
    /// can go and look. With no arguments it prints the claims that need judging, each
    /// with the evidence and what would falsify it. `--check <id>` records the answer.
    ///
    /// Advisory throughout. Nothing here changes an exit code or blocks a command - a
    /// deterministic tool that refused to work because a judgement disagreed would have
    /// traded away the thing that makes it worth trusting.
    #[command(name = "llm")]
    Llm {
        #[command(subcommand)]
        cmd: LlmCmd,
    },
}

fn main() -> ExitCode {
    let started = Instant::now();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let code = match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cairn: {e:#}");
            // An unreadable or half-built index is *degraded* — "I cannot see" — not a bad
            // query. The distinction is the whole point of having both codes: an agent
            // that reads ERROR concludes it asked wrong and rephrases, when what it should
            // do is retry or rebuild. Reads during a rebuild land here.
            let msg = format!("{e:#}");
            let degraded = msg.contains("no index at")
                || msg.contains("index is incomplete")
                || msg.contains("not a database")
                || msg.contains("unable to open database")
                || msg.contains("schema v")
                || msg.contains("no such table")
                || msg.contains("file is encrypted or is not a database");
            if degraded {
                exit::DEGRADED
            } else {
                exit::ERROR
            }
        }
    };

    // Recorded after the fact so nothing here can change the answer or its exit code.
    report(&argv, code, started.elapsed());
    ExitCode::from(code)
}

/// Log the command, and report peak memory, when the pack asks for either.
///
/// Both are read from the pack rather than a flag: they are deployment decisions, and a
/// flag would mean every caller had to remember to pass it.
fn report(argv: &[String], code: u8, elapsed: std::time::Duration) {
    let db = std::env::args()
        .skip_while(|a| a != "--db")
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_db);
    // Installation settings, not repository ones: one binary serves every checkout on the
    // machine, and whether it records sessions should not depend on which directory you
    // are standing in.
    let Ok(cfg) = cairn_store::Config::load() else {
        return;
    };

    if cfg.memory_peak {
        if let Some(kb) = track::peak_rss_kb() {
            eprintln!("cairn: peak memory {:.1} MB", kb as f64 / 1024.0);
        }
    }
    // After the fact, and that is all it is: nothing here interrupts anything. Reporting
    // it is still worth doing — indexing a repository far larger than any this has been
    // run against is the case where the number matters, and finding out afterwards beats
    // not finding out.
    if let (Some(kb), Some(limit)) = (track::peak_rss_kb(), cfg.memory_limit_bytes()) {
        if kb * 1024 > limit {
            eprintln!(
                "cairn: used {:.1} MB, above the {:.1} MB ceiling - raise memory_limit_mb \
                 in the installation config if this is expected",
                kb as f64 / 1024.0,
                limit as f64 / (1024.0 * 1024.0)
            );
        }
    }
    if !cfg.tracking {
        return;
    }
    // The first non-flag argument is the command; the second, when there is one, is the
    // handle or query. Flags are recorded by name only — a value could carry content, and
    // this file is not the place for content.
    // Skipping a flag is not enough: most of cairn's flags take a value, and treating that
    // value as positional made `--db <path>` look like the command being run.
    let mut positional: Vec<&String> = Vec::new();
    let mut skip_next = false;
    for a in argv {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a.starts_with('-') {
            // A value follows unless the next token is itself a flag.
            skip_next = true;
            continue;
        }
        positional.push(a);
    }
    let mut positional = positional.into_iter();
    let Some(command) = positional.next() else {
        return;
    };
    let subject = positional.next();
    let flags: Vec<String> = argv
        .iter()
        .filter(|a| a.starts_with("--"))
        .cloned()
        .collect();
    // What the answer carried, as the caller saw it. Read here rather than passed down
    // from `run` so that the record and the printed answer cannot describe two different
    // things: both come from the same envelope, a moment apart.
    let (rows, truncated) = track::observed();
    track::append(
        &db,
        &track::Record {
            command,
            subject: subject.map(|s| s.as_str()),
            flags,
            exit: code,
            rows,
            truncated,
            elapsed,
        },
    );
}

/// Print an answer, and remember what it carried.
///
/// Every envelope leaves through here. Printing and recording being one step is the
/// point: the session log is read to ask why an agent ran the same query four times, and
/// that question is only answerable if the row count and the "I left something out" flag
/// in the log are the ones that were in front of it when it decided to ask again.
fn emit(env: cairn_fmt::Envelope) {
    track::observe(env.rows, env.truncated());
    print!("{}", env.render());
}

/// Repo-relative paths mentioned by an answer, for staleness marking.
fn paths_of<'a>(defs: impl Iterator<Item = Option<&'a cairn_store::Occurrence>>) -> Vec<String> {
    defs.flatten().map(|d| d.path.clone()).collect()
}

/// Where the index is, when the caller did not say.
///
/// Precedence: `--db`, then `$CAIRN_DB`, then the nearest `.cairn/index.sqlite` at or above
/// the working directory, then that path relative to here.
///
/// The upward search is the part that matters. It used to be the bare relative path, so
/// cairn only worked when run from the exact directory holding `.cairn/` — an agent that
/// had moved into a subdirectory, which they do constantly, got "no index" and would
/// reasonably conclude the tool was not set up. Git solves this by looking upward for
/// `.git`; there is no reason to make people think about it here either.
fn default_db() -> PathBuf {
    if let Some(from_env) = std::env::var_os("CAIRN_DB") {
        return PathBuf::from(from_env);
    }
    if let Ok(cwd) = std::env::current_dir() {
        // The repository root, so one binary can serve many checkouts and each keeps its
        // own index at a place that does not depend on where you happen to be standing.
        // `.git` is a directory in a normal clone and a file in a worktree or submodule.
        for dir in cwd.ancestors() {
            if dir.join(".git").exists() {
                return dir.join(".cairn/index.sqlite");
            }
        }
        // No git: fall back to the nearest existing .cairn, so a plain directory still
        // works.
        for dir in cwd.ancestors() {
            let candidate = dir.join(".cairn/index.sqlite");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from(".cairn/index.sqlite")
}

/// Would this command be better for having a file watcher behind it?
///
/// Everything that reads the index would. The exceptions are the commands that are not
/// about a repository's contents at all, plus `index` itself — it is about to replace the
/// file the watcher would be tracking — and `daemon`, which starts one on its own terms.
fn wants_a_watcher(cmd: &Cmd) -> bool {
    !matches!(
        cmd,
        Cmd::Daemon { .. } | Cmd::Index { .. } | Cmd::Config { .. } | Cmd::Rules
    )
}

/// Start a watcher for this index in the background, and do not wait for it.
///
/// Best-effort throughout: every failure here is silent, because none of them should cost
/// the caller the answer they actually asked for. A repository with no watcher gets an
/// honest `stale:` line, which is the same thing it got before this existed.
fn spawn_daemon(db: &Path) {
    use std::process::Stdio;

    // `<repo>/.cairn/index.sqlite` — the repository is two levels up.
    let Some(repo) = db.parent().and_then(|d| d.parent()) else {
        return;
    };
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // Somewhere for it to complain to. Without this the daemon's stderr would land in the
    // caller's terminal, minutes later and out of nowhere.
    let log = db.parent().map(|d| d.join("daemon.log")).and_then(|p| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok()
    });

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--db")
        .arg(db)
        .arg("daemon")
        .arg("--repo")
        .arg(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log.map(Stdio::from).unwrap_or_else(Stdio::null));

    // Detach, or the watcher dies with the shell that happened to run the first query —
    // including on a Ctrl-C aimed at something else entirely.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: no console, and not on the
        // receiving end of the parent's Ctrl-C.
        cmd.creation_flags(0x0000_0008 | 0x0000_0200);
    }

    let _ = cmd.spawn();
}

/// Say what installing the guide did, in one line or none.
fn report_skill(installed: skill::Installed) {
    match installed {
        skill::Installed::Written(p) => println!("skill:    {} (agent guide)", p.display()),
        // Silent when nothing changed: a line every single build saying a file is the same
        // as it was is noise, and noise in a build log is how real notices get missed.
        skill::Installed::AlreadyCurrent => {}
    }
}

/// The index directory, relative to the repository root.
///
/// Relative because the indexers run inside a container where the repository is mounted at
/// a fixed path: an absolute host path would mean nothing there.
fn index_dir_rel(db: &Path, repo: &Path) -> PathBuf {
    db.parent()
        .and_then(|d| d.strip_prefix(repo).ok())
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".cairn"))
}

/// Index whatever the tree contains, and return the SCIP files produced.
///
/// Reports per language rather than stopping at the first gap, and says plainly when a
/// language is present that cairn cannot read — an index that silently covers half a
/// repository is the same confident incompleteness every answer here is built to avoid.
fn produce_indexes(repo: &Path, out_rel: &Path, survey: &index::Survey) -> Result<Vec<PathBuf>> {
    use std::io::Write;

    if survey.found.is_empty() && survey.unsupported.is_empty() {
        anyhow::bail!(
            "found no source files under {} ({} looked at).\n\
             If the code is somewhere else, run this from that directory.",
            repo.display(),
            survey.total
        );
    }

    println!("repository: {}", repo.display());
    for f in &survey.found {
        println!(
            "  {:<10} {:>6} files  ({:.0}%)",
            f.language.name,
            f.files,
            f.share * 100.0
        );
    }
    for (language, files, share) in &survey.unsupported {
        println!(
            "  {:<10} {:>6} files  ({:.0}%)  not indexed",
            language.to_lowercase(),
            files,
            share * 100.0
        );
    }

    if survey.found.is_empty() {
        let names: Vec<&str> = survey.unsupported.iter().map(|(l, _, _)| *l).collect();
        anyhow::bail!(
            "this looks like a {} repository, and cairn indexes Go and Python only.",
            names.join(" and ")
        );
    }

    if !docker::available() {
        anyhow::bail!("{}", docker::NO_DOCKER);
    }
    if matches!(docker::ensure_image()?, docker::Image::Built) {
        println!(
            "indexers: built {} (once per cairn version, shared by every repository)",
            docker::image_tag()
        );
    }

    let mut produced = Vec::new();
    let mut failed = Vec::new();
    for f in &survey.found {
        print!("  {:<10} indexing  ", f.language.name);
        let _ = std::io::stdout().flush();
        match index::run_indexer(f, repo, out_rel) {
            index::Outcome::Indexed { scip, seconds } => {
                println!("{seconds:.1}s");
                produced.push(scip);
            }
            index::Outcome::Failed(e) => {
                println!("failed: {e}");
                failed.push(f.language.name);
            }
        }
    }

    if produced.is_empty() {
        anyhow::bail!("no language could be indexed");
    }

    // The warning is loud on purpose. Everything downstream reports what it cannot see,
    // and a whole language missing from the index is the largest thing it cannot see.
    let mut absent: Vec<String> = survey
        .unsupported
        .iter()
        .map(|(l, _, _)| l.to_string())
        .collect();
    absent.extend(failed.iter().map(|s| s.to_string()));
    if !absent.is_empty() {
        println!(
            "\nWARNING: {} is in this repository and is not in the index. Answers will be\n\
             \x20        incomplete rather than empty - nothing here can see that code.",
            absent.join(", ")
        );
    }
    Ok(produced)
}

/// Make the index directory, and make it uncommittable.
///
/// A `.gitignore` holding `*` inside `.cairn/` means the directory ignores itself and
/// everything in it, including the ignore file. Written on every index build rather than
/// once, because the failure it prevents — a multi-hundred-megabyte SQLite file in someone
/// history — is not worth an if.
fn ensure_index_dir(db: &Path) -> Result<()> {
    let Some(dir) = db.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    if dir.file_name().is_some_and(|n| n == ".cairn") {
        let ignore = dir.join(".gitignore");
        let want = "# cairn's index is a projection of the code; rebuild it, never commit it.\n*\n";
        if std::fs::read_to_string(&ignore).unwrap_or_default() != want {
            std::fs::write(&ignore, want)
                .with_context(|| format!("writing {}", ignore.display()))?;
        }
    }
    Ok(())
}

fn run() -> Result<u8> {
    let cli = Cli::parse();
    let db = cli.db.unwrap_or_else(default_db);
    let cli_budget = cli.budget;
    let mut budget = Budget::from_opt(cli.budget);
    // Asked once per invocation, best-effort: a missing daemon is a normal state and
    // must never fail a query, only change what the answer can claim about freshness.
    let dirty: Option<Vec<String>> =
        cairn_daemon::client::dirty_if_running(&cairn_daemon::socket_path(&db)).map(|d| {
            d.modified
                .into_iter()
                .chain(d.removed)
                .collect::<Vec<String>>()
        });
    // Nobody should have to know the daemon exists. If this repository has an index and
    // nothing is watching it, start one and carry on — the command it was asked to run
    // does not wait, so the only cost is that this one answer cannot report freshness.
    if dirty.is_none() && db.exists() && wants_a_watcher(&cli.cmd) {
        spawn_daemon(&db);
    }

    match cli.cmd {
        Cmd::Index {
            indexes,
            repo,
            without_skill,
        } => {
            // The working directory is the repository. No check for `.git`: standing
            // somewhere is the intent, and a monorepo subtree or a plain directory of
            // sources is a perfectly good thing to want indexed.
            let repo = match repo {
                Some(r) => r,
                None => std::env::current_dir().context("reading the working directory")?,
            };
            // Before anything is written, including the guide: this is the one directory
            // where the whole command is a mistake.
            index::refuse_own_directory(&repo)?;
            ensure_index_dir(&db)?;
            let repo = Some(repo);

            // An index nothing knows how to use is half a setup, so one command leaves the
            // repository ready rather than leaving a second step written down somewhere
            // the person who needs it will not look.
            //
            // Before indexing, not after: indexing is the part that can fail — a missing
            // indexer, a language nobody has installed the toolchain for — and the guide
            // is still worth having when it does.
            if !without_skill {
                match skill::install(repo.as_deref().unwrap()) {
                    Ok(what) => report_skill(what),
                    // Never fatal. A read-only checkout should still get an index.
                    Err(e) => eprintln!("cairn: could not install the agent guide: {e:#}"),
                }
            }

            // Nothing was named, so work out what is here and produce it. The survey is
            // kept: what the tree held is half of every coverage answer, and re-walking
            // it at query time would answer about the tree as it is now rather than the
            // one this index was built from.
            let (indexes, survey) = if indexes.is_empty() {
                let repo = repo.as_deref().unwrap();
                let survey = index::scan(repo)?;
                let produced = produce_indexes(repo, &index_dir_rel(&db, repo), &survey)?;
                (produced, Some(survey))
            } else {
                (indexes, None)
            };
            let started = Instant::now();
            // Build beside the live index, then swap. Measured: while `index` ran, twelve
            // of twelve concurrent reads failed, because a rebuild is not one transaction
            // and a reader saw a half-wiped database. A rename is atomic, so a reader gets
            // the old index or the new one and never a mixture.
            let _lock = cairn_store::build::BuildLock::acquire(&db)?;
            cairn_store::build::clear_staging(&db);
            let building = cairn_store::build::building_path(&db);
            let mut store = Store::reset(&building)?;
            // A repository whose conventions differ from the defaults says so here, once,
            // rather than getting silently wrong answers later (architecture D16).
            let rules_path = db.parent().map(|d| d.join("rules.yaml"));
            store.rules = cairn_store::Rules::load(rules_path.as_deref())?;
            if let Some(p) = rules_path.as_deref().filter(|p| p.exists()) {
                println!("rules:    {}", p.display());
            }
            for path in &indexes {
                let t = Instant::now();
                let stats = ingest::ingest_path(&mut store, path, repo.as_deref())?;
                println!(
                    "{}: {} documents, {} symbols, {} occurrences, {} generated \
                     ({} by marker), prefix {:?}, {} batch flushes in {:.1}s",
                    path.display(),
                    stats.documents,
                    stats.symbols,
                    stats.occurrences,
                    stats.generated_files,
                    stats.marker_detected,
                    stats.path_prefix,
                    stats.batch_flushes,
                    t.elapsed().as_secs_f64()
                );
                if stats.paths_outside_repo > 0 {
                    eprintln!(
                        "cairn: dropped {} documents whose paths escaped the workspace root \
                         (indexer build cache or similar)",
                        stats.paths_outside_repo
                    );
                }
                if repo.is_some() && stats.generated_files > 0 && stats.marker_detected == 0 {
                    eprintln!(
                        "cairn: warning - {} files flagged generated by path pattern and none \
                         by header marker; the path prefix is probably wrong",
                        stats.generated_files
                    );
                }
            }
            if let Some(s) = &survey {
                let langs: Vec<cairn_store::TreeLanguage> = s
                    .found
                    .iter()
                    .map(|f| cairn_store::TreeLanguage {
                        name: f.language.name.to_string(),
                        files: f.files as i64,
                        indexable: true,
                    })
                    .chain(
                        s.unsupported
                            .iter()
                            .map(|(l, n, _)| cairn_store::TreeLanguage {
                                name: l.to_string(),
                                files: *n as i64,
                                indexable: false,
                            }),
                    )
                    .collect();
                store.set_tree_survey(&langs, s.protos as i64)?;
            }

            // Literals: the pass the weak layer was already making, kept this time.
            if let Some(root) = repo.as_deref() {
                let l = cairn_store::weak::index_literals(&mut store, root)?;
                if l.literals > 0 {
                    println!(
                        "literals: {} in {} files, {} inside a function",
                        l.literals, l.files, l.attributed
                    );
                }
            }

            // Markdown, where there is a tree to read it from. Independent of the SCIP
            // side: documents are not symbols and nothing about them waits on the graph.
            if let Some(root) = repo.as_deref() {
                let (files, sections) = store.link_docs(root)?;
                if files > 0 {
                    println!("docs:     {files} markdown files, {sections} sections");
                }
            }

            // Cross-language links are derived once the whole index is in place: the
            // two sides of a service boundary usually arrive from different SCIP files.
            let links = store.link_services()?;
            println!(
                "services: {} gRPC services, {} serve links, {} call links",
                links.services, links.serves, links.calls
            );
            if let Some(root) = repo.as_deref() {
                let topo = cairn_store::deploy::parse_compose(
                    root,
                    &[
                        "compose.yaml",
                        "compose.yml",
                        "docker-compose.yml",
                        "compose.local.yaml",
                        "compose.override.yaml",
                    ],
                )?;
                if !topo.services.is_empty() {
                    let d = store.link_deployment(root, &topo)?;
                    println!(
                        "deploy:   {} services from {}, {} with a resolved entrypoint{}",
                        d.services,
                        topo.sources.join(" + "),
                        d.with_entrypoint,
                        if d.on_demand > 0 {
                            format!(", {} on-demand entrypoint(s) from cron", d.on_demand)
                        } else {
                            String::new()
                        }
                    );
                    // Naming them rather than counting: an unresolved entrypoint makes
                    // live code look unreachable, which is the failure mode 8.4 warns
                    // about.
                    if !d.unresolved.is_empty() {
                        println!("          unresolved: {}", d.unresolved.join(", "));
                    }
                }
            }
            let c = store.counts()?;
            // Closed before the swap: an open connection keeps a WAL beside the file it
            // belongs to, and a WAL left next to the promoted database is how a rebuild
            // that reported success produces an index nothing can read.
            drop(store);
            cairn_store::build::promote(&building, &db)?;
            println!(
                "store: {} files, {} symbols, {} occurrences in {:.1}s -> {}",
                c.files,
                c.symbols,
                c.occurrences,
                started.elapsed().as_secs_f64(),
                db.display()
            );
            Ok(exit::FOUND)
        }

        Cmd::For {
            purpose,
            subject: subj,
            limit,
            repo,
        } => {
            let store = open(&db)?;
            match purpose {
                Purpose::Find => {
                    // `<repo>/.cairn/index.sqlite`, so the repository is two levels up
                    // — the same derivation `spawn_daemon` makes. Searching the tree from
                    // the working directory instead would answer about whatever subtree
                    // the caller happened to stand in.
                    let root = repo
                        .or_else(|| db.parent().and_then(|d| d.parent()).map(|p| p.to_path_buf()))
                        .unwrap_or_else(|| PathBuf::from("."));
                    let (env, found) = purpose::find(&store, &root, &subj, limit, &mut budget)?;
                    emit(env);
                    Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
                }
                Purpose::Change => {
                    // The spoken redirect. It fires before resolution, so the caller is
                    // told which purpose fits rather than being handed an empty symbol
                    // search and left to work it out - which is what cost eight calls in
                    // the run this exists for.
                    if purpose::looks_like_text(&subj) {
                        eprintln!(
                            "cairn: '{subj}' looks like text rather than a symbol. \
                             `cairn for change` answers about code that a call graph \
                             reaches; a value, a key or a header lives in files no \
                             indexer reads. Try `cairn for find \"{subj}\"` - it \
                             searches the tree and says whose line each hit is."
                        );
                        return Ok(exit::ERROR);
                    }
                    // Resolved here rather than through `subject`, which bails on a
                    // miss — so the redirect below could never run. Measured: three runs
                    // asked `for change` about a function added seconds earlier, got a
                    // bare failure, and spent two more turns working out that the index
                    // was stale. The tree knows. Answer from it in the turn that would
                    // otherwise have been spent failing.
                    let resolved = match store.resolve_handle(&subj)? {
                        Some(id) => Some(id),
                        None => {
                            let named = store.symbols_named(&subj)?;
                            match named.len() {
                                1 => Some(named[0].id),
                                0 => None,
                                _ => {
                                    // A shared name used to cost a whole round trip: the
                                    // command listed candidates, exited 2, and the arm
                                    // re-ran with a handle. In every run of every round.
                                    // So answer instead — for the most-referenced
                                    // candidate that is not generated, saying so at the
                                    // top with the others listed. The choice is visible
                                    // and one copy-paste from being overridden, which is
                                    // the difference between this and guessing.
                                    let plausible = purpose::change_candidates(&store, &subj)?;
                                    match plausible.split_first() {
                                        Some((best, rest)) => {
                                            eprintln!(
                                                "cairn: '{subj}' names {} symbols. Answering \
                                                 for [{}] {} ({} references, the most of \
                                                 any); {} generated definition(s) ignored. \
                                                 Others: {}",
                                                named.len(),
                                                best.handle,
                                                best.qualified(),
                                                best.ref_count,
                                                named.len() - plausible.len(),
                                                if rest.is_empty() {
                                                    "none".to_string()
                                                } else {
                                                    rest.iter()
                                                        .map(|s| format!(
                                                            "[{}] {}",
                                                            s.handle,
                                                            s.def
                                                                .as_ref()
                                                                .map(|d| d.path.clone())
                                                                .unwrap_or_default()
                                                        ))
                                                        .collect::<Vec<_>>()
                                                        .join(", ")
                                                }
                                            );
                                            Some(best.id)
                                        }
                                        // Every candidate is generated, so there is
                                        // nothing here anyone would edit.
                                        None => {
                                            let coverage = store.coverage_summary()?;
                                            emit(cairn_fmt::symbols(
                                                &named, &subj, &coverage, true, &mut budget,
                                            )
                                            .unknown(format!(
                                                "every symbol named '{subj}' is in generated \
                                                 code, which is not edited by hand. Nothing \
                                                 here is a change you would make"
                                            )));
                                            return Ok(exit::ERROR);
                                        }
                                    }
                                }
                            }
                        }
                    };
                    let Some(symbol_id) = resolved else {
                        let root = repo
                            .or_else(|| {
                                db.parent().and_then(|d| d.parent()).map(|p| p.to_path_buf())
                            })
                            .unwrap_or_else(|| PathBuf::from("."));
                        let (env, found) = purpose::find(&store, &root, &subj, limit, &mut budget)?;
                        if found {
                            eprintln!(
                                "cairn: no symbol '{subj}' in the index, so the graph \
                                 cannot answer about it - but the working tree contains \
                                 the text. `for find` ran instead; if this is code you \
                                 just wrote, that is why."
                            );
                            emit(env);
                            return Ok(exit::FOUND);
                        }
                        eprintln!(
                            "cairn: nothing called '{subj}' in the index or the working \
                             tree. Check the spelling, or `cairn symbol {subj}` for a \
                             partial-name search."
                        );
                        return Ok(exit::ERROR);
                    };
                    // The repository root, so the call sites can carry their source —
                    // the block the arm asked for with a second `refs` every time.
                    let root = repo
                        .clone()
                        .or_else(|| db.parent().and_then(|d| d.parent()).map(|p| p.to_path_buf()));
                    let (env, found) =
                        purpose::change(&store, root.as_deref(), symbol_id, &mut budget)?;
                    emit(env);
                    Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
                }
            }
        }

        Cmd::Context { query, limit } => {
            if query.trim().is_empty() {
                eprintln!("cairn: empty query - describe the feature in a word or two");
                return Ok(exit::ERROR);
            }
            let store = open(&db)?;
            let coverage = store.coverage_summary()?;
            let res = store.context(&query, limit)?;
            let found = !res.seeds.is_empty();
            let paths = paths_of(res.seeds.iter().map(|s| s.symbol.def.as_ref()));
            emit(
                cairn_fmt::context(&query, &res, &coverage, &mut budget)
                    .mark_stale(dirty.as_deref(), &paths),
            );
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Unreached { prefix, limit } => {
            let store = open(&db)?;
            let rows = store.unreached(&prefix, limit)?;
            let found = !rows.is_empty();
            let paths = paths_of(rows.iter().map(|r| r.symbol.def.as_ref()));
            emit(
                cairn_fmt::unreached(&prefix, &rows, &mut budget)
                    .mark_stale(dirty.as_deref(), &paths),
            );
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Outline { prefix, limit } => {
            let store = open(&db)?;
            let (rows, total) = store.outline(&prefix, limit)?;
            let found = !rows.is_empty();
            let paths = paths_of(rows.iter().map(|r| r.symbol.def.as_ref()));
            emit(
                cairn_fmt::outline(&prefix, &rows, total, &mut budget)
                    .mark_stale(dirty.as_deref(), &paths),
            );
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Usage {
            handle,
            include_tests,
            limit,
        } => {
            let store = open(&db)?;
            let Some(symbol_id) = subject(&store, &handle, cli_budget)? else {
                return Ok(exit::ERROR);
            };
            let sym = store.symbol(symbol_id)?.context("handle has no symbol")?;
            let rows = store.usage_by_file(symbol_id, include_tests, limit)?;
            let found = !rows.is_empty();
            // Only when the filter was actually applied: with --include-tests the rows
            // above already hold them, and reporting them again as missing would be a
            // second wrong answer in the other direction.
            let filtered = if include_tests {
                (0, 0)
            } else {
                store.usage_in_tests(symbol_id)?
            };
            emit(cairn_fmt::usage(
                &sym,
                &rows,
                rows.len() < limit,
                filtered,
                &mut budget,
            ));
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Symbol { query, limit } => {
            // An empty or blank query is a caller mistake, not a search. It used to match
            // everything — fifteen arbitrary symbols returned as "15 matches" with
            // `unknown: none`, which is the tool asserting it found something and knows of
            // no gaps. An agent interpolating an empty variable would believe it.
            if query.trim().is_empty() {
                eprintln!("cairn: empty query - give a name, or part of one");
                return Ok(exit::ERROR);
            }
            let store = open(&db)?;
            let coverage = store.coverage_summary()?;
            let rows = store.find_symbols(&query, limit)?;
            let found = !rows.is_empty();
            let paths = paths_of(rows.iter().map(|r| r.def.as_ref()));
            emit(
                cairn_fmt::symbols(&rows, &query, &coverage, rows.len() < limit, &mut budget)
                    .mark_stale(dirty.as_deref(), &paths),
            );
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Refs {
            handle,
            include_generated,
            limit,
            context,
            repo,
        } => {
            let store = open(&db)?;
            // An unknown handle or name is a query error, not an empty result: the agent
            // asked about something we cannot even identify.
            let Some(symbol_id) = subject(&store, &handle, cli_budget)? else {
                return Ok(exit::ERROR);
            };
            let sym = store
                .symbol(symbol_id)?
                .context("handle resolved to a missing symbol")?;
            // A long partial list reads as authoritative. Where the resolver is known
            // to under-report, keep it short: the point is the routing note, not the
            // list.
            let effective_limit = if cairn_fmt::routing_note(&sym).is_some() {
                limit.min(8)
            } else {
                limit
            };
            let (refs, suppressed, total) =
                store.references(symbol_id, include_generated, effective_limit)?;
            let found = !refs.is_empty();
            let paths: Vec<String> = refs.iter().map(|r| r.path.clone()).collect();
            let ctx = if context == "auto" {
                cairn_fmt::SiteContext::auto(cli_budget, refs.len())
            } else {
                cairn_fmt::SiteContext::parse(&context).with_context(|| {
                    format!("unknown --context '{context}' (none|line|block|<n>|auto)")
                })?
            };
            let mut source = match (ctx, repo) {
                (cairn_fmt::SiteContext::None, _) => None,
                (_, Some(root)) => Some(Source::new(root)),
                (_, None) => anyhow::bail!("--context prints source, so it needs --repo <dir>"),
            };
            emit(
                cairn_fmt::references_with_context(
                    &sym,
                    &refs,
                    suppressed,
                    total,
                    source.as_mut(),
                    ctx,
                    &mut budget,
                )
                .mark_stale(dirty.as_deref(), &paths),
            );
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Graph {
            handle,
            aspect,
            depth,
            fanout,
            view,
            detail,
            repo,
            exclude_tests,
        } => {
            let store = open(&db)?;
            let Some(symbol_id) = subject(&store, &handle, cli_budget)? else {
                return Ok(exit::ERROR);
            };
            let view =
                View::parse(&view).with_context(|| format!("unknown view '{view}' (list|tree)"))?;
            let detail = Detail::parse(&detail).with_context(|| {
                format!("unknown detail '{detail}' (skeleton|signature|doc|body)")
            })?;
            let mut source = make_source(detail, repo)?;
            // `tests` is a filtered reachability question rather than a plain walk,
            // so it takes its own path through the store.
            if matches!(aspect, Aspect::Tests) {
                let sym = store.symbol(symbol_id)?.context("handle has no symbol")?;
                let rows = store.tests_reaching(symbol_id, depth.max(3), 40)?;
                let found = !rows.is_empty();
                let mut env = cairn_fmt::tests(&sym, &rows, &mut budget);
                if !found {
                    env = env.unknown(
                        "no test reaches this through static calls; a test may still \
                         exercise it dynamically (fixtures, parametrisation, reflection)",
                    );
                }
                emit(env);
                return Ok(if found { exit::FOUND } else { exit::NOT_FOUND });
            }
            let (kind, dir, label) = match aspect {
                Aspect::Callers => (EdgeKind::Calls, Direction::In, "callers of"),
                Aspect::Calls => (EdgeKind::Calls, Direction::Out, "calls from"),
                Aspect::Impls => (EdgeKind::Implements, Direction::In, "implementations of"),
                Aspect::Tests => unreachable!("handled above"),
            };
            let w = store.walk(symbol_id, kind, dir, depth, fanout, exclude_tests)?;
            let root = w
                .nodes
                .first()
                .map(|n| n.symbol.qualified())
                .unwrap_or_default();
            let title =
                format!("{label} [{handle}] {root}   depth={depth} fanout={fanout}   [L1, exact]");
            let paths = paths_of(w.nodes.iter().map(|n| n.symbol.def.as_ref()));
            let mut env = cairn_fmt::walk(&w, &title, view, detail, source.as_mut(), &mut budget)
                .mark_stale(dirty.as_deref(), &paths);
            // References with no enclosing body are module-level, not missing. Say so
            // rather than letting the count look like a gap.
            if matches!(aspect, Aspect::Callers) {
                let orphan = store.unattributed_refs(symbol_id)?;
                if orphan > 0 {
                    env = env.unknown(format!(
                        "{orphan} references sit outside any function body \
                         (imports, module-level use) and have no caller"
                    ));
                }
            }
            emit(env);
            Ok(if w.nodes.len() > 1 {
                exit::FOUND
            } else {
                exit::NOT_FOUND
            })
        }

        Cmd::Skill => {
            let repo = std::env::current_dir().context("reading the working directory")?;
            report_skill(skill::install(&repo)?);
            Ok(exit::FOUND)
        }
        Cmd::Config { assignment } => {
            let mut cfg = cairn_store::Config::load()?;
            match assignment {
                None => {
                    let where_from = cairn_store::Config::source()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "defaults (nothing has been set yet)".to_string());
                    println!("# from: {where_from}");
                    let width = cairn_store::config::SETTINGS
                        .iter()
                        .map(|(k, _)| k.len())
                        .max()
                        .unwrap_or(0);
                    for (key, description) in cairn_store::config::SETTINGS {
                        println!(
                            "{key:<width$}  {:<8}  {description}",
                            cfg.get(key).unwrap_or_default()
                        );
                    }
                    Ok(exit::FOUND)
                }
                Some(a) => {
                    let (key, value) = a.split_once('=').with_context(|| {
                        format!("expected key=value, got {a:?} (try `cairn config` to see them)")
                    })?;
                    cfg.set(key.trim(), value.trim())?;
                    let path = cfg.save()?;
                    println!("{key} = {}", cfg.get(key.trim()).unwrap_or_default());
                    println!("# saved to {}", path.display());
                    Ok(exit::FOUND)
                }
            }
        }
        Cmd::Rules => {
            let path = db.parent().map(|d| d.join("rules.yaml"));
            let from = match path.as_deref() {
                Some(p) if p.exists() => format!("{}", p.display()),
                _ => "built-in defaults (no rules.yaml beside the index)".to_string(),
            };
            println!("# effective rule pack, from: {from}");
            match cairn_store::Rules::load(path.as_deref()) {
                Ok(_) => {}
                Err(e) => {
                    eprintln!("cairn: {e:#}");
                    return Ok(exit::ERROR);
                }
            }
            if let Some(p) = path.as_deref().filter(|p| p.exists()) {
                print!("{}", std::fs::read_to_string(p)?);
            } else {
                print!("{}", cairn_store::Rules::builtin_text());
            }
            Ok(exit::FOUND)
        }

        Cmd::Topology => {
            let store = open(&db)?;
            let rows = store.deploy_services()?;
            let found = !rows.is_empty();
            emit(cairn_fmt::topology(&rows, &mut budget));
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Llm {
            cmd:
                LlmCmd::Verify {
                    check,
                    holds,
                    broken,
                    note,
                    repo,
                },
        } => {
            let store = open(&db)?;
            // `<repo>/.cairn/index.sqlite` — the repository is two levels up, same as the
            // watcher works it out.
            let root = repo
                .or_else(|| db.parent().and_then(|d| d.parent()).map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from("."));
            let head = cairn_store::llmverify::head_commit(&root);

            let Some(id) = check else {
                let checks = store.verification_checks()?;
                let verdicts = store.verdicts()?;
                let with_standing: Vec<_> = checks
                    .into_iter()
                    .map(|c| {
                        let v = verdicts.iter().find(|v| v.check_id == c.id);
                        let s = store.standing(v, head.as_deref());
                        (c, s)
                    })
                    .collect();
                let any = !with_standing.is_empty();
                emit(cairn_fmt::verification_plan(
                    &with_standing,
                    head.as_deref(),
                    cairn_store::llmverify::tree_is_dirty(&root),
                    &mut budget,
                ));
                return Ok(if any { exit::FOUND } else { exit::NOT_FOUND });
            };

            if holds == broken {
                eprintln!(
                    "cairn: say which - --holds or --broken. A check recorded as neither \
                     is a row that means nothing"
                );
                return Ok(exit::ERROR);
            }
            if broken && note.as_deref().unwrap_or("").trim().is_empty() {
                // An unexplained "this is wrong" cannot be acted on and cannot be
                // re-checked, which makes it worse than not recording anything.
                eprintln!("cairn: --broken needs --note saying what is wrong");
                return Ok(exit::ERROR);
            }
            let Some(target) = store
                .verification_checks()?
                .into_iter()
                .find(|c| c.id == id)
            else {
                eprintln!("cairn: no claim with id '{id}' (run `cairn llm verify` for the list)");
                return Ok(exit::ERROR);
            };
            store.record_verdict(
                &target.id,
                holds,
                note.as_deref(),
                head.as_deref(),
                &target.area,
                &cairn_store::now_iso8601(),
            )?;
            println!(
                "recorded: {} {} for {}",
                target.id,
                if holds { "holds" } else { "BROKEN" },
                target.area
            );
            if head.is_none() {
                println!(
                    "note: the commit could not be determined, so this verdict will read \
                     as expired - it cannot be told apart from one made against an older tree"
                );
            } else if cairn_store::llmverify::tree_is_dirty(&root) {
                println!(
                    "note: the tree has uncommitted changes, so the commit this is \
                     anchored to does not describe what was looked at"
                );
            }
            Ok(exit::FOUND)
        }

        Cmd::Literal {
            text,
            context,
            repo,
            limit,
        } => {
            let store = open(&db)?;
            let rows = store.literals(&text, limit)?;
            let found = !rows.is_empty();
            let ctx = cairn_fmt::SiteContext::parse(&context)
                .with_context(|| format!("unknown --context '{context}' (none|line|block|<n>)"))?;
            // Worked out, not asked for. Requiring --repo would put a flag between the
            // caller and the thing that makes this worth calling at all, and the
            // repository sits two levels above the index whenever the index is where
            // `cairn index` puts it. `--repo` stays for when it is not.
            let root = repo.or_else(|| {
                let dir = db.parent()?;
                // Only where the convention actually holds. Guessing that the
                // grandparent is a repository because it usually is would read files
                // from wherever the index happened to be put.
                (dir.file_name()? == ".cairn")
                    .then(|| dir.parent())?
                    .map(PathBuf::from)
            });
            let mut source = match (ctx, &root) {
                (cairn_fmt::SiteContext::None, _) => None,
                (_, Some(r)) => Some(Source::new(r.clone())),
                (_, None) => None,
            };
            let mut env = cairn_fmt::literals(&rows, &text, source.as_mut(), ctx, &mut budget);
            if ctx != cairn_fmt::SiteContext::None && root.is_none() {
                // Asked for source and could not read any. Printing the locations alone
                // would look like the answer rather than like half of it.
                env = env.unknown(
                    "the source could not be shown: this index is not at \
                     <repo>/.cairn/index.sqlite, so the repository root could not be \
                     worked out. Pass --repo <dir>",
                );
            }
            emit(env);
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Docs { path, about } => {
            let store = open(&db)?;
            match (path, about) {
                (Some(p), _) => {
                    let rows = store.document_sections(&p)?;
                    let found = !rows.is_empty();
                    emit(cairn_fmt::doc_sections(&rows, &mut budget));
                    Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
                }
                (None, Some(q)) => {
                    // Bodies are read from disk, so this needs the repository. Same way
                    // the watcher works it out: `<repo>/.cairn/index.sqlite`.
                    let root = db
                        .parent()
                        .and_then(|d| d.parent())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from("."));
                    let rows = store.sections_matching(&root, &q)?;
                    let found = !rows.is_empty();
                    emit(cairn_fmt::doc_search(&rows, &q, &mut budget));
                    Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
                }
                (None, None) => {
                    let docs = store.documents()?;
                    let found = !docs.is_empty();
                    emit(cairn_fmt::documents(&docs, &mut budget));
                    Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
                }
            }
        }

        Cmd::Entrypoints { reaches } => {
            let store = open(&db)?;
            let sym = match reaches.as_deref() {
                Some(given) => {
                    let Some(id) = subject(&store, given, cli_budget)? else {
                        return Ok(exit::ERROR);
                    };
                    Some(store.symbol(id)?.context("handle has no symbol")?)
                }
                None => None,
            };
            let rows = store.entrypoints(sym.as_ref().map(|s| s.id))?;
            let found = !rows.is_empty();
            // Only worth naming when listing everything. Filtered by --reaches, a service
            // that starts nothing is simply not an answer to the question asked.
            let blind = if sym.is_some() {
                Vec::new()
            } else {
                store.services_without_entrypoint()?
            };
            emit(cairn_fmt::entrypoints(
                &rows,
                sym.as_ref(),
                &blind,
                &mut budget,
            ));
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Affects {
            handle,
            depth,
            fanout,
        } => {
            let store = open(&db)?;
            let Some(symbol_id) = subject(&store, &handle, cli_budget)? else {
                return Ok(exit::ERROR);
            };
            let sym = store.symbol(symbol_id)?.context("handle has no symbol")?;
            let a = store.affects(symbol_id, depth, fanout)?;
            let found = !a.in_process.is_empty() || !a.hops.is_empty();
            emit(cairn_fmt::affects(&sym, &a, &mut budget));
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Runs { handle, depth } => {
            let store = open(&db)?;
            let Some(symbol_id) = subject(&store, &handle, cli_budget)? else {
                return Ok(exit::ERROR);
            };
            let sym = store.symbol(symbol_id)?.context("handle has no symbol")?;
            let (services, via) = store.services_running_attributed(symbol_id, depth)?;
            let found = !services.is_empty();
            let blind = store.services_without_entrypoint()?;
            emit(cairn_fmt::runs_in(&sym, &services, depth, &via, &blind));
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Reaches { handle, outgoing } => {
            let store = open(&db)?;
            let Some(symbol_id) = subject(&store, &handle, cli_budget)? else {
                return Ok(exit::ERROR);
            };
            let sym = store.symbol(symbol_id)?.context("handle has no symbol")?;
            let mut services = store.services_of(symbol_id)?;
            let cross = |id| -> anyhow::Result<_> {
                Ok(if outgoing {
                    store.cross_language_targets(id)?
                } else {
                    store.cross_language_callers(id)?
                })
            };
            // A method implements exactly one RPC, and its real call sites are indexed.
            // Answer from those rather than from the handler-wide convention: it is the
            // narrower and the stronger claim, and it is the narrowing an agent otherwise
            // does by hand (the measurement record, task E).
            // The outgoing direction, answered from real call edges rather than from the
            // client-artefact binding. Without this it reported zero for every function
            // that *uses* a generated client, which is every function anyone asks about.
            if outgoing {
                let precise = store.rpc_targets(symbol_id)?;
                if !precise.is_empty() {
                    emit(cairn_fmt::rpc_targets(&sym, &precise, &mut budget));
                    return Ok(exit::FOUND);
                }
            }
            if !outgoing {
                // A handler type: answer for all of its RPCs at once rather than making
                // the caller ask once per method (the measurement record, task D).
                if matches!(sym.kind, cairn_scip::SymbolKind::Type) {
                    let precise = store.rpc_callers_of_type(symbol_id)?;
                    if !precise.is_empty() {
                        emit(cairn_fmt::rpc_reaches(&sym, &precise, true, &mut budget));
                        return Ok(exit::FOUND);
                    }
                }
                let precise = store.rpc_callers(symbol_id)?;
                if !precise.is_empty() {
                    emit(cairn_fmt::rpc_reaches(&sym, &precise, false, &mut budget));
                    return Ok(exit::FOUND);
                }
            }
            let mut links = cross(symbol_id)?;
            // Same fallback, same reason: the service binding lives on the handler class,
            // not on each RPC method, so asking about a method answered "nothing crosses
            // a boundary here" about code that serves a live RPC.
            let mut via = None;
            if links.is_empty() {
                if let Some(owner) = store.enclosing_type(symbol_id)? {
                    let owner_links = cross(owner.id)?;
                    if !owner_links.is_empty() {
                        links = owner_links;
                        services = store.services_of(owner.id)?;
                        via = Some(owner);
                    }
                }
            }
            let found = !links.is_empty();
            emit(cairn_fmt::cross_language(
                &sym,
                &services,
                &links,
                outgoing,
                via.as_ref(),
                &mut budget,
            ));
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Path {
            from,
            to,
            max_depth,
            detail,
            repo,
        } => {
            let store = open(&db)?;
            let (Some(src), Some(dst)) = (
                subject(&store, &from, cli_budget)?,
                subject(&store, &to, cli_budget)?,
            ) else {
                return Ok(exit::ERROR);
            };
            let detail = Detail::parse(&detail).with_context(|| {
                format!("unknown detail '{detail}' (skeleton|signature|doc|body)")
            })?;
            let mut source = make_source(detail, repo)?;
            match store.call_path(src, dst, max_depth)? {
                Some(hops) => {
                    emit(cairn_fmt::path(&hops, detail, source.as_mut(), &mut budget));
                    Ok(exit::FOUND)
                }
                None => {
                    // "Not within this bound" is a different statement from "never",
                    // and the difference matters to whoever asked.
                    let env = cairn_fmt::Envelope::new(format!(
                        "no call path from [{from}] to [{to}] within {max_depth} hops\n"
                    ))
                    .unknown(
                        "only static calls were followed; a dynamic dispatch on the way \
                         would not appear here",
                    );
                    emit(env);
                    Ok(exit::NOT_FOUND)
                }
            }
        }

        Cmd::Expand {
            handle,
            detail,
            repo,
        } => {
            let store = open(&db)?;
            let Some(symbol_id) = subject(&store, &handle, cli_budget)? else {
                return Ok(exit::ERROR);
            };
            let sym = store.symbol(symbol_id)?.context("handle has no symbol")?;
            let mut body = format!("{}\n", cairn_fmt::symbol_line(&sym));
            let mut env;
            match detail.as_str() {
                "skeleton" => {
                    env = cairn_fmt::Envelope::new(body);
                }
                "body" | "doc" => {
                    let Some(def) = &sym.def else {
                        env = cairn_fmt::Envelope::new(body);
                        env = env.unknown("no definition indexed, nothing to show");
                        emit(env);
                        return Ok(exit::NOT_FOUND);
                    };
                    let Some(root) = repo else {
                        anyhow::bail!("--detail {detail} needs --repo <dir> to read source");
                    };
                    let full = root.join(&def.path);
                    let text = std::fs::read_to_string(&full)
                        .with_context(|| format!("reading {}", full.display()))?;
                    let lines: Vec<&str> = text.lines().collect();
                    let start = def.line as usize;
                    let end = sym
                        .def_end_line
                        .map(|e| (e as usize).min(lines.len().saturating_sub(1)))
                        .unwrap_or(start);
                    for (i, line) in lines.iter().enumerate().take(end + 1).skip(start) {
                        if !budget.push(&mut body, &format!("{:>6} {line}", i + 1)) {
                            break;
                        }
                    }
                    env = cairn_fmt::Envelope::new(body);
                    if sym.def_end_line.is_none() {
                        env = env.unknown(
                            "the indexer gave no body extent for this symbol; showing its \
                             definition line only",
                        );
                    }
                }
                other => anyhow::bail!("unknown detail '{other}' (skeleton|doc|body)"),
            }
            emit(env);
            Ok(exit::FOUND)
        }

        Cmd::Weak { repo } => {
            let mut store = open(&db)?;
            let stats = cairn_store::weak::derive_weak_links(&mut store, &repo)?;
            println!(
                "scanned {} files, {} literals, {} candidate weak links",
                stats.files_scanned, stats.literals_seen, stats.candidates
            );
            println!("these are candidates, not facts - they are reported as [L1-W, unverified]");
            Ok(exit::FOUND)
        }

        Cmd::Weaklinks { handle, limit } => {
            let store = open(&db)?;
            let Some(symbol_id) = subject(&store, &handle, cli_budget)? else {
                return Ok(exit::ERROR);
            };
            let sym = store.symbol(symbol_id)?.context("handle has no symbol")?;
            let sites = store.weak_sites(symbol_id, limit)?;
            let found = !sites.is_empty();
            let env = cairn_fmt::weak_links(&sym, &sites, &mut budget);
            emit(env);
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Verify { repo, flag_stale } => {
            let store = open(&db)?;
            if flag_stale {
                let root = repo.clone().context("--flag-stale needs --repo")?;
                let n = store.flag_stale_manual_edges(&root)?;
                let m = store.flag_stale_concept_links(&root)?;
                println!("flagged {n} hand-authored links and {m} concept links for review");
            }
            let rep = store.verify(repo.as_deref())?;
            emit(cairn_fmt::verify(&rep));
            // A degraded index must be distinguishable by exit code, not just by
            // reading the text (6.1.1).
            Ok(if rep.is_clean() {
                exit::FOUND
            } else {
                exit::DEGRADED
            })
        }

        Cmd::Link { from, to, note, by } => {
            let store = open(&db)?;
            let (Some(src), Some(dst)) = (
                subject(&store, &from, cli_budget)?,
                subject(&store, &to, cli_budget)?,
            ) else {
                return Ok(exit::ERROR);
            };
            let source = match by.as_str() {
                "agent" => cairn_store::EdgeSource::Agent,
                "human" => cairn_store::EdgeSource::Human,
                other => anyhow::bail!("unknown --by '{other}' (agent|human)"),
            };
            // Anchor the link to the source symbol's definition site, so that when
            // that code changes the link can be flagged rather than silently trusted.
            let anchor = store
                .symbol(src)?
                .and_then(|s| s.def.map(|d| (d.path, d.line)))
                .and_then(|(path, line)| {
                    store
                        .file_id_for_path(&path)
                        .ok()
                        .flatten()
                        .map(|fid| (fid, line))
                });
            store.add_link(src, dst, source, &note, anchor)?;
            println!("recorded {by} link [{from}] -> [{to}]");
            if anchor.is_none() {
                println!(
                    "note: no anchor file for the source symbol, so this link cannot be \
                     flagged when the code changes"
                );
            }
            Ok(exit::FOUND)
        }

        Cmd::Links { handle } => {
            let store = open(&db)?;
            let Some(symbol_id) = subject(&store, &handle, cli_budget)? else {
                return Ok(exit::ERROR);
            };
            let sym = store.symbol(symbol_id)?.context("handle has no symbol")?;
            let links = store.asserted_links(symbol_id)?;
            let found = !links.is_empty();
            emit(cairn_fmt::asserted(&store, &sym, &links, &mut budget)?);
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Concept(sub) => run_concept(sub, &db, &mut budget, cli_budget),

        Cmd::Daemon { repo, stop } => {
            let socket = cairn_daemon::socket_path(&db);
            if stop {
                match cairn_daemon::Client::connect(&socket) {
                    Some(mut c) => {
                        c.shutdown()?;
                        println!("daemon stopped");
                        return Ok(exit::FOUND);
                    }
                    None => {
                        println!("no daemon running on {}", socket.display());
                        return Ok(exit::NOT_FOUND);
                    }
                }
            }
            if cairn_daemon::Client::connect(&socket).is_some() {
                anyhow::bail!("a daemon is already listening on {}", socket.display());
            }
            let store = open(&db)?;
            let indexed = store.file_hashes()?;
            let roots = store.language_roots()?;
            drop(store);
            println!(
                "starting daemon for {} ({} indexed files, roots: {})",
                repo.display(),
                indexed.len(),
                if roots.is_empty() {
                    "none recorded - reindex to enable the LSP overlay".to_string()
                } else {
                    roots
                        .iter()
                        .map(|(l, r)| format!("{l}={r}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                }
            );
            // The language servers live in the repository's container, not on this
            // machine. Failing to start it is not fatal: the watcher is still worth having
            // on its own, and the pool already reports a server it could not reach.
            let container = match docker::ensure_container(&repo) {
                Ok(name) => Some(name),
                Err(e) => {
                    eprintln!("cairn daemon: no container, so no live overlay: {e:#}");
                    None
                }
            };
            let container = container.as_deref().map(|n| (n, docker::MOUNT));

            cairn_daemon::Daemon::new(&repo, &socket, indexed, &roots, container).run()?;
            // The container is deliberately left running. It costs a sleeping process and
            // a mount, and tearing it down on every daemon exit raced the next daemon
            // starting one — which is how a restart ended up with no live overlay at all.
            Ok(exit::FOUND)
        }

        Cmd::Live { path } => {
            let socket = cairn_daemon::socket_path(&db);
            let Some(mut client) = cairn_daemon::Client::connect(&socket) else {
                anyhow::bail!("the live view needs a running daemon (`cairn daemon --repo <dir>`)");
            };
            let live: Vec<cairn_fmt::LiveSymbolView> = client
                .file_symbols(&path)?
                .into_iter()
                .map(|s| cairn_fmt::LiveSymbolView {
                    name: s.name,
                    kind: s.kind,
                    start_line: s.start_line,
                    end_line: s.end_line,
                    container: s.container,
                })
                .filter(|s| !s.is_local_variable())
                .collect();
            let store = open(&db)?;
            let indexed = store.symbols_in_file(&path)?;
            emit(cairn_fmt::live_overlay(&path, &live, &indexed, &mut budget));
            Ok(exit::FOUND)
        }

        Cmd::Status => {
            if !db.exists() {
                println!("no index at {}", db.display());
                println!("degraded: nothing indexed yet - run `cairn index` in this repository");
                return Ok(exit::DEGRADED);
            }
            let store = open(&db)?;
            let c = store.counts()?;
            println!("index      {}", db.display());
            println!("files      {}", c.files);
            println!("symbols    {}", c.symbols);
            println!("occurrence {}", c.occurrences);
            println!("generated  {} files", c.generated_files);

            // Cross-language linking either found something or it did not, and a zero used
            // to be invisible: `reaches` would report no callers, which reads as "nothing
            // calls this" rather than "this mechanism produced nothing". It is one row of
            // the coverage block now, alongside every other mechanism that can be empty
            // for more than one reason.
            let root = db.parent().and_then(|d| d.parent());
            let head = root.and_then(cairn_store::llmverify::head_commit);
            print!("{}", cairn_fmt::coverage(&store.coverage(head.as_deref())?));

            let socket = cairn_daemon::socket_path(&db);
            match cairn_daemon::Client::connect(&socket) {
                Some(mut client) => {
                    let st = client.status()?;
                    let d = client.dirty()?;
                    println!(
                        "daemon     watching {} ({} files, up {}s)",
                        st.repo, st.files_tracked, st.uptime_secs
                    );
                    if !d.complete {
                        println!("stale: initial scan still running - the set below is partial");
                    }
                    if d.is_empty() && d.complete {
                        println!("stale: none");
                    } else {
                        println!(
                            "stale: {} modified, {} created, {} removed (generation {})",
                            d.modified.len(),
                            d.created.len(),
                            d.removed.len(),
                            d.generation
                        );
                        for p in d.modified.iter().chain(&d.removed).take(8) {
                            println!("       {p}");
                        }
                    }
                    if let Some(why) = &d.reindex_due {
                        println!("reindex due: {why}");
                        println!("             run `cairn index <scip...> --repo <dir>`");
                    }
                    // Drifted files are reported, not signalled by the exit code. Every
                    // other command already works this way: an answer touching a changed
                    // file gets a `stale:` line and still exits 0, because the index can
                    // be read and most of what it says is still true. `status` returning
                    // 3 for the same condition made it the one command that called a
                    // readable index unreadable.
                    Ok(exit::FOUND)
                }
                None => {
                    // An unknown dirty set and an empty one look the same in an answer.
                    // Conflating them is the silent staleness this design forbids.
                    println!("daemon     not running");
                    println!(
                        "stale: NOT TRACKED - the file watcher is still starting, so edits \
                         made since the index was built are not visible yet. `cairn verify \
                         --repo <dir>` is the one-off check"
                    );
                    // Not degraded, and this is the case that made the distinction matter.
                    // `status` is itself one of the commands that spawns the watcher, so
                    // the first `status` in a repository reports a daemon it has just
                    // started and that has not finished coming up - a race this command
                    // creates and then grades itself on. Measured in the session logs: two
                    // agents got exit 3 here, and the guide tells them 3 means they are in
                    // the wrong directory. Both ignored it and queried on, correctly.
                    Ok(exit::FOUND)
                }
            }
        }
    }
}

fn run_concept(
    sub: ConceptCmd,
    db: &Path,
    budget: &mut Budget,
    cli_budget: Option<usize>,
) -> Result<u8> {
    use cairn_store::concepts::DEFAULT_NS;
    let store = open(db)?;
    match sub {
        ConceptCmd::Add { name, note, ns, by } => {
            let author = match by.as_str() {
                "agent" => cairn_store::EdgeSource::Agent,
                "human" => cairn_store::EdgeSource::Human,
                other => anyhow::bail!("unknown --by '{other}' (agent|human)"),
            };
            store.concept_upsert(&ns, &name, &note, author)?;
            println!("concept {ns}/{name} recorded");
            Ok(exit::FOUND)
        }
        ConceptCmd::Link {
            name,
            handle,
            rel,
            note,
            ns,
        } => {
            let concept = store
                .concept_find(&ns, &name)?
                .with_context(|| format!("no concept {ns}/{name} (create it first)"))?;
            let Some(symbol_id) = subject(&store, &handle, cli_budget)? else {
                return Ok(exit::ERROR);
            };
            let anchored = store.concept_link(concept.id, symbol_id, &rel, &note)?;
            println!("{ns}/{name} --{rel}--> [{handle}]");
            if !anchored {
                // Without an anchor nothing can ever invalidate this claim, so it can
                // never be more than a claim. Say so at the moment it is made.
                println!(
                    "note: that symbol has no definition site, so this link has no anchor \
                     and can never be checked against the code"
                );
            }
            Ok(exit::FOUND)
        }
        ConceptCmd::Show { name, ns } => {
            let Some(concept) = store.concept_find(&ns, &name)? else {
                eprintln!("cairn: no concept {ns}/{name}");
                return Ok(exit::NOT_FOUND);
            };
            let links = store.concept_links(concept.id)?;
            emit(cairn_fmt::concept(&store, &concept, &links, budget)?);
            Ok(exit::FOUND)
        }
        ConceptCmd::List { ns } => {
            let list = store.concept_list(ns.as_deref())?;
            let found = !list.is_empty();
            emit(cairn_fmt::concept_list(&list, budget));
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }
        ConceptCmd::Drop { ns } => {
            if ns == DEFAULT_NS {
                // Cheap guard: dropping the default namespace is almost always a
                // mistake, and it is the one holding accumulated knowledge.
                eprintln!(
                    "cairn: refusing to drop the default namespace '{DEFAULT_NS}' - \
                     name a session namespace instead"
                );
                return Ok(exit::ERROR);
            }
            let (concepts, links) = store.concept_drop_ns(&ns)?;
            println!("dropped {concepts} concepts and {links} links in namespace '{ns}'");
            Ok(exit::FOUND)
        }
    }
}

/// Source access is only set up when a detail level actually needs it, so the common
/// skeleton path never touches the filesystem.
fn make_source(detail: Detail, repo: Option<PathBuf>) -> Result<Option<Source>> {
    if !detail.needs_source() {
        return Ok(None);
    }
    let root = repo.context("this --detail level prints source, so it needs --repo <dir>")?;
    Ok(Some(Source::new(root)))
}

/// Resolving a handle that does not exist is a query error, not an empty result:
/// the caller asked about something we cannot even identify.
/// A handle, or a name when it names exactly one symbol.
///
/// Handles are short and stable and stay the recommended form — they keep a result line
/// narrow and they survive between sessions. What changes is that they stop being the
/// *only* form. Requiring one meant every relational question cost two calls, `symbol`
/// then the question, where grep's floor is one: on any query simple enough for grep to
/// answer in a single pass, cairn lost before the answers were even compared, and lost
/// to a property of this decision rather than of anything it does.
///
/// `Ok(None)` means the name was ambiguous and the candidates have been printed. The
/// tool does not pick: choosing the "best" homonym is the tool guessing which symbol was
/// meant, and it guesses nowhere else. An ambiguous name therefore costs exactly what it
/// costs today — the list, then the question again with a handle — and an unambiguous
/// one saves the round trip.
///
/// The cost of this, stated: a command that works today can start printing a list
/// tomorrow because somebody added a second symbol of that name. That is honest, but it
/// is a change the caller did not make, and it is the reason handles still exist.
fn subject(store: &Store, given: &str, cli_budget: Option<usize>) -> Result<Option<i64>> {
    if let Some(id) = store.resolve_handle(given)? {
        return Ok(Some(id));
    }
    let named = store.symbols_named(given)?;
    match named.len() {
        1 => Ok(Some(named[0].id)),
        0 => anyhow::bail!(
            "no symbol with handle or name '{given}'. `cairn symbol {given}` searches by \
             part of a name and reports what is indexed if nothing matches"
        ),
        _ => {
            let coverage = store.coverage_summary()?;
            let mut b = Budget::from_opt(cli_budget);
            emit(
                cairn_fmt::symbols(&named, given, &coverage, true, &mut b).unknown(format!(
                    "'{given}' is the name of {} symbols, so this command cannot tell \
                     which one you mean. Run it again with one of the handles above",
                    named.len()
                )),
            );
            Ok(None)
        }
    }
}

fn open(db: &Path) -> Result<Store> {
    if !db.exists() {
        anyhow::bail!(
            // The commonest cause by far is standing in the wrong place — a workspace
            // holding two checkouts, say — so that is named before the fix that assumes
            // the tool was never set up at all.
            "no index at {}.\n\
             cairn reads the index of the repository you are standing in. If you are above \
             it, or beside it, cd into the repository and try again.\n\
             If it has never been indexed: run `cairn index` there.",
            db.display()
        );
    }
    let store = Store::open(db)?;
    // Said once, here, rather than at each of the commands that write. A sidecar that
    // could not be opened does not stop anything working — it silently un-does it, and
    // the tool spent its whole life so far printing "recorded" over a memory database.
    if !store.knowledge_is_durable() {
        eprintln!(
            "cairn: warning - notes, links, concepts and verdicts cannot be saved here \
             ({} is not writable). Everything else works; anything you record will be \
             gone when this command exits",
            cairn_store::knowledge_path(db).display()
        );
    }
    Ok(store)
}
