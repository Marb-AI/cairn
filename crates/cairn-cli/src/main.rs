//! `cairn` — local code navigation for coding agents.
//!
//! CLI rather than an MCP server (architecture D1): an agent runs commands natively,
//! and this way the tool also works in CI, a Makefile and a human terminal. Startup
//! cost is on the hot path (every query is a fresh process), so nothing expensive
//! happens before the subcommand is known.

use anyhow::{Context, Result};
use cairn_fmt::{Budget, View};
use cairn_store::{ingest, Direction, EdgeKind, Store};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

/// Exit codes are part of the contract: an agent must be able to tell "nothing is
/// there" from "I cannot see" (architecture 6.1.1).
mod exit {
    pub const FOUND: u8 = 0;
    pub const NOT_FOUND: u8 = 1;
    pub const ERROR: u8 = 2;
    pub const DEGRADED: u8 = 3;
}

#[derive(Parser)]
#[command(name = "cairn", version, about = "Local code navigation for agents")]
struct Cli {
    /// Index database. Defaults to .cairn/index.sqlite under the current directory.
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

#[derive(Subcommand)]
enum Cmd {
    /// Load a SCIP index into the store, replacing what is there.
    Index {
        /// One or more .scip files.
        indexes: Vec<PathBuf>,
        /// Repo root, used to detect generated code by header marker rather than by
        /// filename pattern. Strongly recommended: filename patterns lie.
        #[arg(long)]
        repo: Option<PathBuf>,
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
    },
    /// Shortest call path between two symbols: how does one reach the other.
    Path {
        from: String,
        to: String,
        #[arg(long, default_value_t = 8)]
        max_depth: usize,
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
    /// What is indexed, and how stale it is.
    Status,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("cairn: {e:#}");
            ExitCode::from(exit::ERROR)
        }
    }
}

fn default_db() -> PathBuf {
    PathBuf::from(".cairn/index.sqlite")
}

fn run() -> Result<u8> {
    let cli = Cli::parse();
    let db = cli.db.unwrap_or_else(default_db);
    let mut budget = Budget::from_opt(cli.budget);

    match cli.cmd {
        Cmd::Index { indexes, repo } => {
            if indexes.is_empty() {
                anyhow::bail!("give at least one .scip file");
            }
            if let Some(parent) = db.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            let started = Instant::now();
            let mut store = Store::reset(&db)?;
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
                if repo.is_some() && stats.generated_files > 0 && stats.marker_detected == 0 {
                    eprintln!(
                        "cairn: warning - {} files flagged generated by path pattern and none \
                         by header marker; the path prefix is probably wrong",
                        stats.generated_files
                    );
                }
            }
            let c = store.counts()?;
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

        Cmd::Symbol { query, limit } => {
            let store = open(&db)?;
            let rows = store.find_symbols(&query, limit)?;
            let found = !rows.is_empty();
            print!("{}", cairn_fmt::symbols(&rows, &query, &mut budget).render());
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Refs {
            handle,
            include_generated,
            limit,
        } => {
            let store = open(&db)?;
            let Some(symbol_id) = store.resolve_handle(&handle)? else {
                // An unknown handle is a query error, not an empty result: the agent
                // asked about something we cannot even identify.
                eprintln!("cairn: no symbol with handle '{handle}' (run `cairn symbol` first)");
                return Ok(exit::ERROR);
            };
            let sym = store
                .symbol(symbol_id)?
                .context("handle resolved to a missing symbol")?;
            let (refs, suppressed) = store.references(symbol_id, include_generated, limit)?;
            let found = !refs.is_empty();
            print!(
                "{}",
                cairn_fmt::references(&sym, &refs, suppressed).render()
            );
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Graph {
            handle,
            aspect,
            depth,
            fanout,
            view,
        } => {
            let store = open(&db)?;
            let symbol_id = resolve(&store, &handle)?;
            let view = View::parse(&view)
                .with_context(|| format!("unknown view '{view}' (list|tree)"))?;
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
                print!("{}", env.render());
                return Ok(if found { exit::FOUND } else { exit::NOT_FOUND });
            }
            let (kind, dir, label) = match aspect {
                Aspect::Callers => (EdgeKind::Calls, Direction::In, "callers of"),
                Aspect::Calls => (EdgeKind::Calls, Direction::Out, "calls from"),
                Aspect::Impls => (EdgeKind::Implements, Direction::In, "implementations of"),
                Aspect::Tests => unreachable!("handled above"),
            };
            let w = store.walk(symbol_id, kind, dir, depth, fanout)?;
            let root = w
                .nodes
                .first()
                .map(|n| n.symbol.qualified())
                .unwrap_or_default();
            let title = format!(
                "{label} [{handle}] {root}   depth={depth} fanout={fanout}   [L1, exact]"
            );
            let mut env = cairn_fmt::walk(&w, &title, view, &mut budget);
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
            print!("{}", env.render());
            Ok(if w.nodes.len() > 1 { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Path { from, to, max_depth } => {
            let store = open(&db)?;
            let src = resolve(&store, &from)?;
            let dst = resolve(&store, &to)?;
            match store.call_path(src, dst, max_depth)? {
                Some(hops) => {
                    print!("{}", cairn_fmt::path(&hops, &mut budget).render());
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
                    print!("{}", env.render());
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
            let symbol_id = resolve(&store, &handle)?;
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
                        print!("{}", env.render());
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
                    for (i, line) in lines
                        .iter()
                        .enumerate()
                        .take(end + 1)
                        .skip(start)
                    {
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
            print!("{}", env.render());
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
            let symbol_id = resolve(&store, &handle)?;
            let sym = store.symbol(symbol_id)?.context("handle has no symbol")?;
            let sites = store.weak_sites(symbol_id, limit)?;
            let found = !sites.is_empty();
            let env = cairn_fmt::weak_links(&sym, &sites, &mut budget);
            print!("{}", env.render());
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Status => {
            if !db.exists() {
                println!("no index at {}", db.display());
                println!("degraded: nothing indexed yet - run `cairn index <file.scip>`");
                return Ok(exit::DEGRADED);
            }
            let store = open(&db)?;
            let c = store.counts()?;
            println!("index      {}", db.display());
            println!("files      {}", c.files);
            println!("symbols    {}", c.symbols);
            println!("occurrence {}", c.occurrences);
            println!("generated  {} files", c.generated_files);
            println!("stale: unknown (no snapshot tracking yet)");
            Ok(exit::FOUND)
        }
    }
}

/// Resolving a handle that does not exist is a query error, not an empty result:
/// the caller asked about something we cannot even identify.
fn resolve(store: &Store, handle: &str) -> Result<i64> {
    store
        .resolve_handle(handle)?
        .with_context(|| format!("no symbol with handle '{handle}' (run `cairn symbol` first)"))
}

fn open(db: &PathBuf) -> Result<Store> {
    if !db.exists() {
        anyhow::bail!(
            "no index at {} - run `cairn index <file.scip> --repo <dir>` first",
            db.display()
        );
    }
    Store::open(db)
}
