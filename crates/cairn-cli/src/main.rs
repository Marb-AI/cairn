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
mod skill;
mod track;
mod treefind;

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
    /// I am following this through. What does it call, and where does the chain land?
    ///
    /// The outward mirror of `change`. The first hop out of a function that talks to
    /// another service is invisible to `graph --aspect calls`, which drops generated
    /// code, and exact in `reaches --outgoing` - so the answer existed and knowing which
    /// command held it was the agent's problem. Here it is one call, followed to the end
    /// of the chain rather than one hop per round trip.
    Understand,
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
        /// `find`: list every hit, including the ones in test and generated files that a
        /// large answer otherwise reports as a count.
        #[arg(long)]
        all: bool,
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
        /// Repository root, for corroborating a miss against the working tree. Derived
        /// from the index location when it can be confirmed, so it is rarely needed.
        #[arg(long)]
        repo: Option<PathBuf>,
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
/// The two envelope lines `status` was missing.
///
/// Every other answer ends with `unknown:` / `suppressed:` / `stale:`, and the agent guide
/// tells the reader that is where the honesty lives. `status` printed only `stale:` — and
/// it is the command the guide says to run first, so it was the one answer where a reader
/// who had learned to check `unknown:` found nothing there. Its sibling report `verify`
/// prints all three. Found by the stress harness comparing the two.
fn envelope_tail(store: &Store) {
    println!("suppressed: none");
    let partial: Vec<String> = store
        .coverage(None)
        .map(|c| {
            c.iter()
                .filter(|a| {
                    !matches!(
                        a.state,
                        cairn_store::State::Indexed | cairn_store::State::Verified
                    )
                })
                .map(|a| a.name.clone())
                .collect()
        })
        .unwrap_or_default();
    if partial.is_empty() {
        println!("unknown: none");
    } else {
        println!(
            "unknown: {} mechanism(s) did not complete ({}), so answers resting on them \
             are incomplete and will not say so",
            partial.len(),
            partial.join(", ")
        );
    }
}

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

    // `<repo>/.cairn/index.sqlite` — the repository is two levels up, *when the index sits
    // where `cairn index` puts it*. Taken on faith it is how a watcher ends up on a tree
    // nobody asked about: `--db /w/ts.sqlite` derives `/` and the daemon starts watching
    // the whole filesystem, which does not fail, it hangs. Corroborated against paths the
    // index holds, the same way a miss corroborates before claiming an absence.
    let Some(repo) = open(db).ok().and_then(|s| confirmed_repo(db, &s)) else {
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
        // One run per project root. A Go module's root sees every package inside it, so
        // there is one; a JavaScript workspace has one per member, because `apps/a` and
        // `apps/b` are separate compilations with separate dependency trees. Indexing only
        // the shallowest there covered a retired app and left the live one out, and the
        // index would have answered about the rest of the repository as if it were empty.
        for (n, root) in f.roots.iter().enumerate() {
            let tag = if f.roots.len() == 1 {
                f.language.name.to_string()
            } else {
                // Distinct output names, or each project would overwrite the last and the
                // ingest would read one of them as the whole language.
                format!("{}-{n}", f.language.name)
            };
            let where_ = root
                .strip_prefix(repo)
                .ok()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| format!(" {}", p.display()))
                .unwrap_or_default();
            print!("  {:<10} indexing{where_}  ", f.language.name);
            let _ = std::io::stdout().flush();
            match index::run_indexer(f, root, &tag, repo, out_rel) {
                index::Outcome::Indexed { scip, seconds } => {
                    println!("{seconds:.1}s");
                    produced.push(scip);
                }
                index::Outcome::Failed(e) => {
                    println!("failed: {e}");
                    if !failed.contains(&f.language.name) {
                        failed.push(f.language.name);
                    }
                }
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
                if stats.duplicate_occurrences > 0 {
                    eprintln!(
                        "cairn: {} occurrence(s) were repeated exactly by the indexer and \
                         counted once. The same symbol at the same position is the same \
                         fact; left in, they would inflate every reference count",
                        stats.duplicate_occurrences
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
            // Derive the weak layer here rather than leaving it to a command nobody runs.
            //
            // It was a separate `cairn weak`, and on this repository it had never been
            // run: 45,884 literals recorded, zero edges derived. Every `weaklinks` answer
            // said "no literal in the repo spells this name" and every `for change` said
            // "nothing reaches it by a name resolved at run time" — for every symbol,
            // from an empty table. A layer that has to be built by hand is a layer that
            // is missing, and a missing layer that reports as clean is worse than no
            // layer at all.
            if let Some(root) = repo.as_deref() {
                match cairn_store::weak::derive_weak_links(&mut store, root) {
                    Ok(w) => println!(
                        "weak:     {} candidate links from {} literals in {} files",
                        w.candidates, w.literals_seen, w.files_scanned
                    ),
                    // Not fatal: the index is useful without it, and the commands that
                    // depend on it now say so rather than reporting a clean bill.
                    Err(e) => eprintln!("cairn: weak-link layer not built: {e:#}"),
                }
            }
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
            all,
        } => {
            // An empty subject is a mistake, not a query, and the guard belongs here
            // rather than in the resolver: `for find` never reaches the resolver. `symbol`
            // has rejected an empty query since the beginning; the `for` family did not.
            // `for change ""` answered for one of the ten symbols whose name is empty, and
            // `for find ""` returned every line of every file it read with exit 0. The
            // entry point the skill sends an agent to first was the one that lacked it.
            if subj.trim().is_empty() {
                eprintln!(
                    "cairn: empty query - `cairn for {}` needs a name, or part of one. An \
                     empty string matches everything, which is never the question.",
                    match purpose {
                        Purpose::Change => "change",
                        Purpose::Understand => "understand",
                        Purpose::Find => "find",
                    }
                );
                return Ok(exit::ERROR);
            }
            let store = open(&db)?;
            match purpose {
                Purpose::Find => {
                    // `<repo>/.cairn/index.sqlite`, so the repository is two levels up
                    // — the same derivation `spawn_daemon` makes. Searching the tree from
                    // the working directory instead would answer about whatever subtree
                    // the caller happened to stand in.
                    let root = repo
                        .or_else(|| {
                            db.parent()
                                .and_then(|d| d.parent())
                                .map(|p| p.to_path_buf())
                        })
                        .unwrap_or_else(|| PathBuf::from("."));
                    let (env, found) =
                        purpose::find(&store, &root, &subj, limit, all, &mut budget)?;
                    emit(env);
                    Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
                }
                // Both symbol purposes take the same road to a symbol - the spoken
                // redirect, the ranked choice for an ambiguous name, the tree fallback
                // for something the index does not hold - and every one of those steps
                // is a measured fix, not a nicety. Sharing them is what stops a second
                // purpose from silently re-earning the round trips the first one paid to
                // remove.
                Purpose::Change | Purpose::Understand => {
                    let root = repo.clone().or_else(|| {
                        db.parent()
                            .and_then(|d| d.parent())
                            .map(|p| p.to_path_buf())
                    });
                    let symbol_id = match resolve_for_purpose(
                        &store,
                        purpose,
                        &subj,
                        root.as_deref(),
                        limit,
                        &mut budget,
                    )? {
                        Subject::Symbol(id) => id,
                        Subject::Answered(code) => return Ok(code),
                    };
                    let (env, found) = match purpose {
                        // The repository root, so the call sites can carry their source —
                        // the block the arm asked for with a second `refs` every time.
                        Purpose::Change => {
                            purpose::change(&store, root.as_deref(), symbol_id, &mut budget)?
                        }
                        _ => purpose::understand(&store, symbol_id, &mut budget)?,
                    };
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
            let mut rows = store.unreached(&prefix, limit)?;
            // A file named `x.web.ts` next to `x.ts` is one module with two
            // implementations, and which one ships is decided by the bundler from the
            // platform - no import ever names the variant. So every symbol that lives only
            // in a variant has no static caller and looked like dead code: on a workspace
            // of 44 such files, `unreached` reported one as having "no production caller"
            // and marked it `[L1, exact]`. Reachability cannot see a bundler, so it may
            // not make that claim; the rows are withheld and counted instead.
            // From the rule pack, not from a list compiled in here. The first version of
            // this held the four infixes literally, which is one ecosystem's convention
            // welded into the tool: a repository that names its variants differently, or a
            // language with no such mechanism, had no way to say so.
            let selected = store.rules.build_selected.path_contains.clone();
            let variant = |p: &str| selected.iter().any(|infix| p.contains(infix.as_str()));
            let before = rows.len();
            rows.retain(|r| {
                !r.symbol
                    .def
                    .as_ref()
                    .map(|d| variant(&d.path))
                    .unwrap_or(false)
            });
            let hidden = before - rows.len();
            let found = !rows.is_empty();
            let paths = paths_of(rows.iter().map(|r| r.symbol.def.as_ref()));
            let gap = unindexed_prefix_note(&store, &prefix, repo_for(&db).as_deref());
            let mut env = cairn_fmt::unreached(&prefix, &rows, hidden, &mut budget)
                .mark_stale(dirty.as_deref(), &paths);
            if let Some(note) = &gap {
                env = env.unknown(note.clone());
            }
            if hidden > 0 {
                env = env.unknown(format!(
                    "{hidden} symbol(s) here are defined only in a platform variant \
                     (.web/.ios/.android/.native) and are NOT listed: the bundler picks \
                     those by platform, so no import names them and reachability cannot \
                     see who calls them. Whether they are dead is unchecked, not answered"
                ));
            }
            emit(env);
            // Degraded, not "nothing found": a caller acting on the exit code alone must
            // not read an unindexed path as a clean one.
            Ok(match (found, gap.is_some() || hidden > 0) {
                (true, _) => exit::FOUND,
                (false, true) => exit::DEGRADED,
                (false, false) => exit::NOT_FOUND,
            })
        }

        Cmd::Outline { prefix, limit } => {
            let store = open(&db)?;
            let (rows, total) = store.outline(&prefix, limit)?;
            let found = !rows.is_empty();
            let paths = paths_of(rows.iter().map(|r| r.symbol.def.as_ref()));
            let gap = unindexed_prefix_note(&store, &prefix, repo_for(&db).as_deref());
            let mut env = cairn_fmt::outline(&prefix, &rows, total, &mut budget)
                .mark_stale(dirty.as_deref(), &paths);
            if let Some(note) = &gap {
                env = env.unknown(note.clone());
            }
            emit(env);
            Ok(match (found, gap.is_some()) {
                (true, _) => exit::FOUND,
                (false, true) => exit::DEGRADED,
                (false, false) => exit::NOT_FOUND,
            })
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

        Cmd::Symbol { query, limit, repo } => {
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
            // Only on a miss. A hit needs no corroboration, and reading the tree for every
            // successful lookup would make the common case pay for the rare one. An
            // explicit `--repo` is the caller stating which tree to read, so it is taken as
            // given; a derived one is the tool's own inference and has to hold up.
            let tree = (!found)
                .then(|| {
                    repo.filter(|r| r.is_dir())
                        .or_else(|| confirmed_repo(&db, &store))
                })
                .flatten()
                .map(|root| {
                    let f = treefind::search(&root, &query, 200);
                    cairn_fmt::TreeProbe {
                        hits: f.hits.len(),
                        files: f.files_read,
                        truncated: f.truncated,
                    }
                });
            let paths = paths_of(rows.iter().map(|r| r.def.as_ref()));
            emit(
                cairn_fmt::symbols(
                    &rows,
                    &query,
                    &coverage,
                    rows.len() < limit,
                    tree,
                    &mut budget,
                )
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
                // Fall back to the repository the index was found in rather than failing.
                //
                // This used to bail. Measured in round six: 2 of 3 arms ran `refs <h>
                // --context auto`, got the instruction instead of an answer, and re-ran it
                // with `--repo .` — a whole round trip spent supplying a value the binary
                // had already computed, since `<repo>/.cairn/index.sqlite` is how it found
                // the index in the first place. Same shape as round four's second cause: a
                // failure that could have been an answer.
                (_, None) => db
                    .parent()
                    .and_then(|d| d.parent())
                    .map(|root| Source::new(root.to_path_buf())),
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
            // The sixth layer, fixed after the other five. "0 services, 0 with a resolved
            // entrypoint" was printed with `unknown: none` on a repository whose topology
            // had simply not been derivable - and `affects`, which rests on it, then said
            // "affects 0 deployed service(s)" in the same confident voice. A repository
            // with no compose file cairn can read is not a repository where nothing is
            // deployed.
            let gap = deployment_gap(&store);
            let mut env = cairn_fmt::topology(&rows, &mut budget);
            if let Some(note) = &gap {
                env = env.unknown(note.clone());
            }
            emit(env);
            Ok(match (found, gap.is_some()) {
                (_, true) => exit::DEGRADED,
                (true, false) => exit::FOUND,
                (false, false) => exit::NOT_FOUND,
            })
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
            let literal_gap = cairn_store::LayerCounts::unchecked(
                store.layer_counts().literals,
                "string literals",
                "Reindex; until then `cairn for find` reads the tree directly and is                  never stale.",
            );
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
            if let Some(note) = &literal_gap {
                env = env.unknown(note.clone());
            }
            emit(env);
            Ok(match (found, literal_gap.is_some()) {
                (true, _) => exit::FOUND,
                (false, true) => exit::DEGRADED,
                (false, false) => exit::NOT_FOUND,
            })
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
                    let gap = cairn_store::LayerCounts::unchecked(
                        store.layer_counts().doc_sections,
                        "documentation sections",
                        "Reindex, or the tree has no markdown the indexer recognised.",
                    );
                    let mut env = cairn_fmt::doc_search(&rows, &q, &mut budget);
                    if let Some(note) = &gap {
                        env = env.unknown(note.clone());
                    }
                    emit(env);
                    Ok(match (found, gap.is_some()) {
                        (true, _) => exit::FOUND,
                        (false, true) => exit::DEGRADED,
                        (false, false) => exit::NOT_FOUND,
                    })
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
            let gap = deployment_gap(&store);
            let mut env = cairn_fmt::affects(&sym, &a, &mut budget);
            if let Some(note) = &gap {
                env = env.unknown(note.clone());
            }
            emit(env);
            Ok(match (found, gap.is_some()) {
                (_, true) => exit::DEGRADED,
                (true, false) => exit::FOUND,
                (false, false) => exit::NOT_FOUND,
            })
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
            let gap = deployment_gap(&store);
            let mut env = cairn_fmt::runs_in(&sym, &services, depth, &via, &blind);
            if let Some(note) = &gap {
                env = env.unknown(note.clone());
            }
            emit(env);
            Ok(match (found, gap.is_some()) {
                (_, true) => exit::DEGRADED,
                (true, false) => exit::FOUND,
                (false, false) => exit::NOT_FOUND,
            })
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
            // One shape for both ways of answering the outgoing question. They used to be
            // different commands wearing one name — call edges named handler symbols, the
            // client binding named services — and nothing in the output said which you
            // had. Now the rows are the same and the *claim* differs, which is the part
            // that was actually different all along.
            if outgoing {
                let precise = store.rpc_targets(symbol_id)?;
                if !precise.is_empty() {
                    emit(cairn_fmt::rpc_targets(
                        &sym,
                        &precise,
                        &[],
                        true,
                        &mut budget,
                    ));
                    return Ok(exit::FOUND);
                }
                let (bound, unchecked) = store.rpc_targets_by_binding(symbol_id)?;
                if !bound.is_empty() {
                    emit(cairn_fmt::rpc_targets(
                        &sym,
                        &bound,
                        &unchecked,
                        false,
                        &mut budget,
                    ));
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
            let mut env = cairn_fmt::cross_language(
                &sym,
                &services,
                &links,
                outgoing,
                via.as_ref(),
                &mut budget,
            );
            // A zero from the graph is the one answer the graph cannot vouch for: it is
            // the mechanism that failed. So corroborate it against the text before
            // printing it as a finding — a call on an unresolved receiver
            // (`a.client.RaiseAlert(...)`) emits no edge, and this command's whole promise
            // is the direction that call goes.
            //
            // Evidence, never a verdict. A name that matches an RPC may be a local call
            // that happens to share it, and the tool cannot tell — so it says what it saw
            // and where, and leaves the judgement to whoever is reading.
            if outgoing && !found {
                let root = repo_for(&db);
                if let Some(sites) = unresolved_rpc_calls(&store, &sym, root.as_deref())? {
                    env = env.unknown(sites);
                }
            }
            // No service graph at all means every cross-boundary answer is unchecked, in
            // both directions. The same lesson as the weak layer, one layer down: this
            // index records 73 services today, but a repository whose generated code sits
            // somewhere the classifier does not recognise gets zero — and that has already
            // happened once, to repositories generating into `gen/` or `pb/`.
            let no_graph = !store.has_service_graph();
            if no_graph {
                env = env.unknown(
                    "this index holds NO service links at all, so this answer is UNCHECKED \
                     rather than empty. Either nothing here speaks gRPC, or the generated \
                     code sits where the artefact classifier did not look - `cairn status` \
                     reports the service count",
                );
            }
            emit(env);
            Ok(if found {
                exit::FOUND
            } else if no_graph {
                exit::DEGRADED
            } else {
                exit::NOT_FOUND
            })
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
                    let mut env = cairn_fmt::Envelope::new(format!(
                        "no call path from [{from}] to [{to}] within {max_depth} hops\n"
                    ))
                    .unknown(
                        "only static calls were followed; a dynamic dispatch on the way \
                         would not appear here",
                    );
                    // The one corroboration this negative admits: does the source body
                    // simply name the destination? If it does, the missing path is one
                    // unresolved edge away rather than genuinely absent, and the caller
                    // should be told where to look instead of being told "no".
                    if let (Some(a), Some(b)) = (store.symbol(src)?, store.symbol(dst)?) {
                        if let Some(hits) = unlinked_calls_in_body(
                            &store,
                            &a,
                            repo_for(&db).as_deref(),
                            Some(&b.name),
                        ) {
                            let where_ = a.def.as_ref().map(|d| d.path.clone()).unwrap_or_default();
                            // A name match, said as one. `filter(` in a Django
                            // migration is a queryset method, not the indexed symbol
                            // named `filter`, and the tool cannot tell — so it reports
                            // how many symbols share the name and sends the reader to
                            // the line rather than asserting the call.
                            let homonyms =
                                store.symbols_named(&b.name).map(|v| v.len()).unwrap_or(1);
                            env = env.unknown(format!(
                                "but [{from}]'s own body calls something named `{}` at \
                                 {where_}:{}, which the graph resolved no edge for. {} \
                                 Read the line: this is UNRESOLVED, not a proven absence.",
                                b.name,
                                hits[0].1,
                                if homonyms > 1 {
                                    format!(
                                        "{homonyms} symbols share that name, so it may be \
                                         another of them rather than [{to}]."
                                    )
                                } else {
                                    format!("Only [{to}] has that name.")
                                }
                            ));
                        }
                    }
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
            let mut env = cairn_fmt::weak_links(&sym, &sites, &mut budget);
            // An unbuilt layer is not an empty one. Reported here rather than left to the
            // shape of the answer, and as `unknown:` because that is the field a caller is
            // told to read for exactly this.
            if !cairn_store::weak::is_built(&store) {
                env = env.unknown(
                    "the weak-link layer has NOT been derived for this index, so this \
                     answer is UNCHECKED rather than empty. `cairn weak --repo <dir>` \
                     builds it; until then no conclusion about dynamic references \
                     follows from this command",
                );
            }
            emit(env);
            // Degraded, not "nothing found": the caller has to be able to tell the two
            // apart from the exit code alone, without reading the envelope.
            Ok(if found {
                exit::FOUND
            } else if cairn_store::weak::is_built(&store) {
                exit::NOT_FOUND
            } else {
                exit::DEGRADED
            })
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

            // The reload closure, rather than a dependency: the daemon watches files and
            // has no business opening a database, so it asks for the snapshot instead.
            let db_for_reload = db.clone();
            cairn_daemon::Daemon::new(&repo, &socket, indexed, &roots, container)
                .watch_index(
                    &db,
                    Box::new(move || {
                        cairn_store::Store::open(&db_for_reload)
                            .ok()?
                            .file_hashes()
                            .ok()
                    }),
                )
                .run()?;
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
                    envelope_tail(&store);
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
                    envelope_tail(&store);
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
/// The repository an index describes: `<repo>/.cairn/index.sqlite`, so two levels up.
/// The note `affects`, `runs` and `topology` all owe when nothing was deployed.
///
/// The three answer one question at different grain and rest on one layer, so they were
/// fixed one at a time and the first fix covered one of them: `topology` learned to say
/// UNCHECKED while `affects` and `runs` went on printing "(no service entrypoint reaches
/// it)" and exiting 1. A repository with no compose file cairn can read is not one where
/// nothing is deployed, and saying so in one place is what stops the next command that
/// reads this layer from having to remember.
fn deployment_gap(store: &Store) -> Option<String> {
    cairn_store::LayerCounts::unchecked(
        store.layer_counts().deploy_entrypoints,
        "deployment entrypoints",
        "Nothing here can say which service runs a symbol. `cairn rules` shows the start \
         commands this index knows how to read.",
    )
}

fn repo_for(db: &Path) -> Option<PathBuf> {
    db.parent()
        .and_then(|d| d.parent())
        .map(|p| p.to_path_buf())
}

/// Is this the root of a filesystem, rather than a repository?
///
/// A second, independent guard, because the consequence is not a wrong answer but a hung
/// command: an index one level below the root derives `/`, and a watcher pointed there
/// walks the entire filesystem. Corroboration would usually catch it too — every indexed
/// path would have to exist relative to `/` — and on a container image where the tree is
/// mounted at `/w` a handful of them plausibly do. Two cheap guards for a failure mode
/// with no error message are worth more than one clever one.
fn is_filesystem_root(p: &Path) -> bool {
    p.parent().is_none() || p.as_os_str() == "/"
}

/// The whole decision, separated from where its inputs come from so it can be tested.
///
/// Keeping this inline cost nothing until the guard needed defending: a test that only
/// exercises `is_filesystem_root` proves the predicate works and says nothing about whether
/// anything calls it, which is the shape of check this repository has spent a day removing.
fn plausible_root(root: &Path, indexed: &[String], exists: impl Fn(&Path) -> bool) -> bool {
    if is_filesystem_root(root) {
        return false;
    }
    // One is enough to establish it is the right tree; requiring all of them would fail on
    // a file deleted since the index was built, which is a different fact entirely.
    indexed.iter().any(|p| exists(&root.join(p)))
}

/// The repository this index describes, only where the tree on disk agrees that it is.
///
/// `repo_for` applies the `<repo>/.cairn/index.sqlite` convention, which is right whenever
/// the index sits where `cairn index` puts it and wrong the moment `--db` points elsewhere.
/// It was wrong in this repository's own test harness, which builds its index under
/// `/tmp`: a search derived that way read six unrelated files and reported "nothing in the
/// working tree" as a checked absence. A negative from the wrong tree is worse than no
/// negative, so the root is confirmed against paths the index actually holds before any
/// answer is allowed to rest on it.
fn confirmed_repo(db: &Path, store: &Store) -> Option<PathBuf> {
    let root = repo_for(db)?;
    if !root.is_dir() {
        return None;
    }
    let paths = store.sample_paths(8).ok()?;
    plausible_root(&root, &paths, |p| p.exists()).then_some(root)
}

/// Names in a symbol's body that are RPCs of a service this repository speaks, when the
/// graph resolved no outgoing hop at all.
///
/// The corroboration of a negative. `reaches --outgoing` returning nothing is either "this
/// code calls no service" or "the indexer could not follow the call" — and the graph cannot
/// distinguish them, because the graph is what came up empty. The text can: a body that
/// spells `RaiseAlert` where `RaiseAlert` is an RPC of a known service has something in it
/// worth reading, whatever the edges say.
///
/// Measured on the target repository: 375 hand-written production functions contain a call
/// whose name matches a known RPC and 53 of them get `0 targets`. Some of those 53 are local
/// calls that merely share a name — so this reports the sites and says so, rather than
/// inventing hops the index cannot support.
fn unresolved_rpc_calls(
    store: &Store,
    sym: &cairn_store::SymbolRow,
    root: Option<&Path>,
) -> Result<Option<String>> {
    let (Some(root), Some(def), Some(end)) = (root, sym.def.as_ref(), sym.def_end_line) else {
        return Ok(None);
    };
    let Ok(text) = std::fs::read_to_string(root.join(&def.path)) else {
        return Ok(None);
    };
    let names = store.rpc_name_set()?;
    let mut hits: Vec<String> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let n = i as i64;
        if n < def.line || n > end {
            continue;
        }
        for (rpc, service) in &names {
            // `.Name(` — a call through something, which is exactly the shape whose
            // receiver the indexer failed to resolve. A bare mention is not enough.
            if line.contains(&format!(".{rpc}(")) {
                hits.push(format!(
                    "{}:{} calls .{rpc}(, an RPC of {service}",
                    def.path,
                    n + 1
                ));
                break;
            }
        }
        if hits.len() >= 6 {
            break;
        }
    }
    if hits.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "the graph resolved no hop, but the body spells {} name(s) that are RPCs of \
         services this repository speaks - so this zero is UNCONFIRMED, not clean. Each \
         may be a call the indexer could not follow, or a local call that happens to \
         share the name; read them: {}",
        hits.len(),
        hits.join("; ")
    )))
}

/// Names a symbol's body calls that the index knows but did not link.
///
/// The corroboration for an empty callee list, and for a call path that was not found. Both
/// are negatives produced by the call graph, and the call graph is the thing that came up
/// empty — so the body is read from the tree and every call-shaped name in it is checked
/// against the index. A name the index has never heard of is stdlib or third-party and is
/// correctly absent; a name it holds is an edge that should have existed.
///
/// `only` narrows it to one name, which is what `path` needs: the question there is not
/// "what did you miss" but "did you miss *this*".
fn unlinked_calls_in_body(
    store: &Store,
    sym: &cairn_store::SymbolRow,
    root: Option<&Path>,
    only: Option<&str>,
) -> Option<Vec<(String, i64)>> {
    let (root, def, end) = (root?, sym.def.as_ref()?, sym.def_end_line?);
    let text = std::fs::read_to_string(root.join(&def.path)).ok()?;
    let mut seen: Vec<(String, i64)> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let n = i as i64;
        if n < def.line || n > end {
            continue;
        }
        let bytes = line.as_bytes();
        for (s, e) in word_spans(line) {
            if bytes.get(e).copied() != Some(b'(') {
                continue;
            }
            let w = &line[s..e];
            if w.len() < 5 || w == sym.name || only.is_some_and(|o| o != w) {
                continue;
            }
            if !names.iter().any(|x| x == w) {
                names.push(w.to_string());
                seen.push((w.to_string(), n + 1));
            }
        }
    }
    let known = store.known_symbol_names(&names).ok()?;
    let hits: Vec<(String, i64)> = seen
        .into_iter()
        .filter(|(n, _)| known.iter().any(|k| k == n))
        .take(6)
        .collect();
    (!hits.is_empty()).then_some(hits)
}

/// Identifier spans in a line, so a name is only matched whole.
fn word_spans(line: &str) -> Vec<(usize, usize)> {
    let b = line.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if ident(b[i]) && !(i > 0 && ident(b[i - 1])) {
            let mut j = i;
            while j < b.len() && ident(b[j]) {
                j += 1;
            }
            out.push((i, j));
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Says so when a set-shaped question was asked about a path the index does not cover.
///
/// The same corroboration as `unresolved_rpc_calls`, for the other direction: there, the
/// graph came up empty and the text was asked whether anything was there; here, the *index*
/// covers nothing under the prefix and the tree is asked the same question.
///
/// Without it `unreached tools/pbgen` answers "everything here has a production caller" and
/// `outline tools/pbgen` answers "0 of 0 definitions", both `unknown: none`, both exit 0,
/// about four Python files no indexer has read. The roots are named in the message because
/// that is almost always the reason — this repository indexed `srcpy` and `srcgo`, and
/// every path outside them is invisible by construction rather than by accident.
fn unindexed_prefix_note(store: &Store, prefix: &str, root: Option<&Path>) -> Option<String> {
    if store.indexed_under(prefix).unwrap_or(1) > 0 {
        return None;
    }
    let on_disk = root.map(|r| count_indexable(&r.join(prefix))).unwrap_or(0);
    let roots = store
        .language_roots()
        .map(|rs| {
            rs.iter()
                .map(|(l, p)| format!("{l}={p}"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    Some(format!(
        "NO FILE under `{prefix}` is in this index, so this answer is UNCHECKED rather \
         than empty - nothing here has been ruled out. The working tree holds {on_disk} \
         indexable file(s) there.{}",
        if roots.is_empty() {
            String::new()
        } else {
            format!(" The indexers were pointed at: {roots}.")
        }
    ))
}

/// Files under a directory an indexer could have read, counted from the tree.
fn count_indexable(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut n = 0;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            n += count_indexable(&p);
        } else if p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| matches!(x, "py" | "pyi" | "go"))
        {
            n += 1;
        }
    }
    n
}

/// What resolving a `for` subject produced.
enum Subject {
    Symbol(i64),
    /// The caller was already answered - a redirect spoken, a tree search run, or a
    /// failure explained - and this is the exit code that answer carries.
    Answered(u8),
}

/// The road from a `for` subject to a symbol, shared by every purpose that needs one.
///
/// Four measured fixes live here, and the reason they are in one function rather than
/// copied per purpose is that each was worth a round trip when it was missing:
///
/// 1. **The spoken redirect**, before resolution. Asked to change a compose variable, one
///    run spent eight calls across four symbol commands on a thing no symbol command
///    answers. Saying so costs nothing and names the purpose that fits.
/// 2. **Resolution inline** rather than through `subject`, which bails on a miss and so
///    could never reach the fallback below.
/// 3. **A ranked choice for an ambiguous name.** Listing candidates and exiting 2 cost a
///    whole turn in every run of every round; answering for the most-referenced
///    non-generated candidate, with the choice and the alternatives printed, costs none
///    and is one copy-paste from being overridden.
/// 4. **The tree fallback.** Three runs asked about a function written seconds earlier,
///    got a bare failure, and spent two turns discovering the index was stale. The tree
///    knows, so it answers in the turn that would have been spent failing.
fn resolve_for_purpose(
    store: &Store,
    purpose: Purpose,
    subj: &str,
    root: Option<&Path>,
    limit: usize,
    budget: &mut Budget,
) -> Result<Subject> {
    let cmd = match purpose {
        Purpose::Change => "for change",
        Purpose::Understand => "for understand",
        Purpose::Find => "for find",
    };
    if purpose::looks_like_text(subj) {
        eprintln!(
            "cairn: '{subj}' looks like text rather than a symbol. `cairn {cmd}` answers \
             about code that a call graph reaches; a value, a key or a header lives in \
             files no indexer reads. Try `cairn for find \"{subj}\"` - it searches the \
             tree and says whose line each hit is."
        );
        return Ok(Subject::Answered(exit::ERROR));
    }
    let resolved = match store.resolve_handle(subj)? {
        Some(id) => Some(id),
        None => {
            let named = store.symbols_named(subj)?;
            match named.len() {
                1 => Some(named[0].id),
                0 => None,
                _ => {
                    let plausible = purpose::change_candidates(store, subj)?;
                    match plausible.split_first() {
                        Some((best, rest)) => {
                            eprintln!(
                                "cairn: '{subj}' names {} symbols. Answering for [{}] {} \
                                 ({} references, the most of any); {} generated \
                                 definition(s) ignored. Others: {}",
                                named.len(),
                                best.handle,
                                best.qualified(),
                                best.ref_count,
                                named.len() - plausible.len(),
                                if rest.is_empty() {
                                    "none".to_string()
                                } else {
                                    rest.iter()
                                        .map(|s| {
                                            format!(
                                                "[{}] {}",
                                                s.handle,
                                                s.def
                                                    .as_ref()
                                                    .map(|d| d.path.clone())
                                                    .unwrap_or_default()
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                }
                            );
                            Some(best.id)
                        }
                        // Every candidate is generated. For `change` that means nothing
                        // here is a change anyone would make; for `understand` it means
                        // the chain would be read off a stub rather than off the code
                        // that uses it. Different sentence, same dead end.
                        None => {
                            let coverage = store.coverage_summary()?;
                            let why = match purpose {
                                Purpose::Change => format!(
                                    "every symbol named '{subj}' is in generated code, \
                                     which is not edited by hand. Nothing here is a \
                                     change you would make"
                                ),
                                _ => format!(
                                    "every symbol named '{subj}' is in generated code. \
                                     Following a chain from a stub says what the \
                                     generator wired up, not what this codebase does \
                                     with it - ask about the code that calls it"
                                ),
                            };
                            emit(
                                cairn_fmt::symbols(&named, subj, &coverage, true, None, budget)
                                    .unknown(why),
                            );
                            return Ok(Subject::Answered(exit::ERROR));
                        }
                    }
                }
            }
        }
    };
    if let Some(id) = resolved {
        return Ok(Subject::Symbol(id));
    }
    let root = root
        .map(|r| r.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let (env, found) = purpose::find(store, &root, subj, limit, false, budget)?;
    if found {
        eprintln!(
            "cairn: no symbol '{subj}' in the index, so the graph cannot answer about it \
             - but the working tree contains the text. `for find` ran instead; if this is \
             code you just wrote, that is why."
        );
        emit(env);
        return Ok(Subject::Answered(exit::FOUND));
    }
    eprintln!(
        "cairn: nothing called '{subj}' in the index or the working tree. Check the \
         spelling, or `cairn symbol {subj}` for a partial-name search."
    );
    Ok(Subject::Answered(exit::ERROR))
}

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
                cairn_fmt::symbols(&named, given, &coverage, true, None, &mut b).unknown(format!(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_index_beside_the_filesystem_root_is_not_a_repository() {
        // Probing TypeScript support with `--db /w/ts.sqlite` derived `/` as the tree and
        // the daemon started watching the whole filesystem. It printed `watching /`, took
        // no error path, and the command never returned. A watcher with nothing to watch
        // fails loudly; a watcher with everything to watch does not fail at all.
        assert_eq!(
            repo_for(Path::new("/w/ts.sqlite")).as_deref(),
            Some(Path::new("/"))
        );
        assert!(is_filesystem_root(Path::new("/")));
        assert!(!is_filesystem_root(Path::new("/home/work/repo")));
    }

    #[test]
    fn a_root_derived_onto_the_filesystem_is_refused_however_plausible_its_paths_look() {
        // The guard has to be checked where it is *used*, not only where it is defined.
        // `/w/ts.sqlite` derives `/`, and on a container image `/srcpy/...` can genuinely
        // exist, so the corroboration alone would have let this through.
        let indexed = ["srcpy/alerting/dispatch.py".to_string()];
        assert!(
            !plausible_root(Path::new("/"), &indexed, |_| true),
            "accepted the filesystem root as a repository"
        );
        assert!(plausible_root(
            Path::new("/home/work/repo"),
            &indexed,
            |_| true
        ));
    }

    #[test]
    fn a_root_whose_indexed_paths_are_absent_is_the_wrong_tree() {
        let indexed = ["srcpy/alerting/dispatch.py".to_string()];
        assert!(!plausible_root(Path::new("/tmp"), &indexed, |_| false));
        assert!(!plausible_root(Path::new("/tmp"), &[], |_| true));
    }

    #[test]
    fn the_ordinary_layout_still_yields_the_repository() {
        assert_eq!(
            repo_for(Path::new("/home/work/repo/.cairn/index.sqlite")).as_deref(),
            Some(Path::new("/home/work/repo"))
        );
        assert!(!is_filesystem_root(Path::new("/home/work/repo")));
    }
}
