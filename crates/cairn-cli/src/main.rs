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
    /// Deployed services and what each one runs.
    Topology,
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

/// Repo-relative paths mentioned by an answer, for staleness marking.
fn paths_of<'a>(
    defs: impl Iterator<Item = Option<&'a cairn_store::Occurrence>>,
) -> Vec<String> {
    defs.flatten().map(|d| d.path.clone()).collect()
}

fn default_db() -> PathBuf {
    PathBuf::from(".cairn/index.sqlite")
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
                    &["compose.yaml", "compose.yml", "docker-compose.yml",
                      "compose.local.yaml", "compose.override.yaml"],
                )?;
                if !topo.services.is_empty() {
                    let d = store.link_deployment(root, &topo)?;
                    println!(
                        "deploy:   {} services from {}, {} with a resolved entrypoint",
                        d.services,
                        topo.sources.join(" + "),
                        d.with_entrypoint
                    );
                    // Naming them rather than counting: an unresolved entrypoint makes
                    // live code look unreachable, which is the failure mode 8.4 warns
                    // about.
                    if !d.unresolved.is_empty() {
                        println!(
                            "          unresolved: {}",
                            d.unresolved.join(", ")
                        );
                    }
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

        Cmd::Context { query, limit } => {
            let store = open(&db)?;
            let coverage = store.coverage_summary()?;
            let res = store.context(&query, limit)?;
            let found = !res.seeds.is_empty();
            let paths = paths_of(res.seeds.iter().map(|s| s.symbol.def.as_ref()));
            print!(
                "{}",
                cairn_fmt::context(&query, &res, &coverage, &mut budget)
                    .mark_stale(dirty.as_deref(), &paths)
                    .render()
            );
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Unreached { prefix, limit } => {
            let store = open(&db)?;
            let rows = store.unreached(&prefix, limit)?;
            let found = !rows.is_empty();
            let paths = paths_of(rows.iter().map(|r| r.symbol.def.as_ref()));
            print!(
                "{}",
                cairn_fmt::unreached(&prefix, &rows, &mut budget)
                    .mark_stale(dirty.as_deref(), &paths)
                    .render()
            );
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Outline { prefix, limit } => {
            let store = open(&db)?;
            let rows = store.outline(&prefix, limit)?;
            let found = !rows.is_empty();
            let paths = paths_of(rows.iter().map(|r| r.symbol.def.as_ref()));
            print!(
                "{}",
                cairn_fmt::outline(&prefix, &rows, &mut budget)
                    .mark_stale(dirty.as_deref(), &paths)
                    .render()
            );
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Usage { handle, include_tests, limit } => {
            let store = open(&db)?;
            let symbol_id = resolve(&store, &handle)?;
            let sym = store.symbol(symbol_id)?.context("handle has no symbol")?;
            let rows = store.usage_by_file(symbol_id, include_tests, limit)?;
            let found = !rows.is_empty();
            print!("{}", cairn_fmt::usage(&sym, &rows, &mut budget).render());
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Symbol { query, limit } => {
            let store = open(&db)?;
            let coverage = store.coverage_summary()?;
            let rows = store.find_symbols(&query, limit)?;
            let found = !rows.is_empty();
            let paths = paths_of(rows.iter().map(|r| r.def.as_ref()));
            print!(
                "{}",
                cairn_fmt::symbols(&rows, &query, &coverage, &mut budget)
                    .mark_stale(dirty.as_deref(), &paths)
                    .render()
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
            let Some(symbol_id) = store.resolve_handle(&handle)? else {
                // An unknown handle is a query error, not an empty result: the agent
                // asked about something we cannot even identify.
                eprintln!("cairn: no symbol with handle '{handle}' (run `cairn symbol` first)");
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
            let (refs, suppressed) = store.references(symbol_id, include_generated, effective_limit)?;
            let found = !refs.is_empty();
            let paths: Vec<String> = refs.iter().map(|r| r.path.clone()).collect();
            let ctx = if context == "auto" {
                cairn_fmt::SiteContext::auto(cli_budget, refs.len())
            } else {
                cairn_fmt::SiteContext::parse(&context)
                    .with_context(|| format!("unknown --context '{context}' (none|line|block|<n>|auto)"))?
            };
            let mut source = match (ctx, repo) {
                (cairn_fmt::SiteContext::None, _) => None,
                (_, Some(root)) => Some(Source::new(root)),
                (_, None) => anyhow::bail!("--context prints source, so it needs --repo <dir>"),
            };
            print!(
                "{}",
                cairn_fmt::references_with_context(
                    &sym,
                    &refs,
                    suppressed,
                    source.as_mut(),
                    ctx,
                    &mut budget
                )
                .mark_stale(dirty.as_deref(), &paths)
                .render()
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
            let symbol_id = resolve(&store, &handle)?;
            let view = View::parse(&view)
                .with_context(|| format!("unknown view '{view}' (list|tree)"))?;
            let detail = Detail::parse(&detail)
                .with_context(|| format!("unknown detail '{detail}' (skeleton|signature|doc|body)"))?;
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
                print!("{}", env.render());
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
            let title = format!(
                "{label} [{handle}] {root}   depth={depth} fanout={fanout}   [L1, exact]"
            );
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
            print!("{}", env.render());
            Ok(if w.nodes.len() > 1 { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Topology => {
            let store = open(&db)?;
            let rows = store.deploy_services()?;
            let found = !rows.is_empty();
            print!("{}", cairn_fmt::topology(&rows, &mut budget).render());
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Affects { handle, depth, fanout } => {
            let store = open(&db)?;
            let symbol_id = resolve(&store, &handle)?;
            let sym = store.symbol(symbol_id)?.context("handle has no symbol")?;
            let a = store.affects(symbol_id, depth, fanout)?;
            let found = !a.in_process.is_empty() || !a.hops.is_empty();
            print!("{}", cairn_fmt::affects(&sym, &a, &mut budget).render());
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Runs { handle, depth } => {
            let store = open(&db)?;
            let symbol_id = resolve(&store, &handle)?;
            let sym = store.symbol(symbol_id)?.context("handle has no symbol")?;
            let (services, via) = store.services_running_attributed(symbol_id, depth)?;
            let found = !services.is_empty();
            let blind = store.services_without_entrypoint()?;
            print!(
                "{}",
                cairn_fmt::runs_in(&sym, &services, depth, via.as_ref(), &blind).render()
            );
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Reaches { handle, outgoing } => {
            let store = open(&db)?;
            let symbol_id = resolve(&store, &handle)?;
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
            // does by hand (eval/RESULTS.md, task E).
            if !outgoing {
                let precise = store.rpc_callers(symbol_id)?;
                if !precise.is_empty() {
                    print!("{}", cairn_fmt::rpc_reaches(&sym, &precise, &mut budget).render());
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
            print!(
                "{}",
                cairn_fmt::cross_language(&sym, &services, &links, outgoing, via.as_ref(), &mut budget)
                    .render()
            );
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Path { from, to, max_depth, detail, repo } => {
            let store = open(&db)?;
            let src = resolve(&store, &from)?;
            let dst = resolve(&store, &to)?;
            let detail = Detail::parse(&detail)
                .with_context(|| format!("unknown detail '{detail}' (skeleton|signature|doc|body)"))?;
            let mut source = make_source(detail, repo)?;
            match store.call_path(src, dst, max_depth)? {
                Some(hops) => {
                    print!(
                        "{}",
                        cairn_fmt::path(&hops, detail, source.as_mut(), &mut budget).render()
                    );
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

        Cmd::Verify { repo, flag_stale } => {
            let store = open(&db)?;
            if flag_stale {
                let root = repo.clone().context("--flag-stale needs --repo")?;
                let n = store.flag_stale_manual_edges(&root)?;
                let m = store.flag_stale_concept_links(&root)?;
                println!("flagged {n} hand-authored links and {m} concept links for review");
            }
            let rep = store.verify(repo.as_deref())?;
            print!("{}", cairn_fmt::verify(&rep).render());
            // A degraded index must be distinguishable by exit code, not just by
            // reading the text (6.1.1).
            Ok(if rep.is_clean() { exit::FOUND } else { exit::DEGRADED })
        }

        Cmd::Link { from, to, note, by } => {
            let store = open(&db)?;
            let src = resolve(&store, &from)?;
            let dst = resolve(&store, &to)?;
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
            let symbol_id = resolve(&store, &handle)?;
            let sym = store.symbol(symbol_id)?.context("handle has no symbol")?;
            let links = store.asserted_links(symbol_id)?;
            let found = !links.is_empty();
            print!("{}", cairn_fmt::asserted(&store, &sym, &links, &mut budget)?.render());
            Ok(if found { exit::FOUND } else { exit::NOT_FOUND })
        }

        Cmd::Concept(sub) => run_concept(sub, &db, &mut budget),

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
            cairn_daemon::Daemon::new(&repo, &socket, indexed, &roots).run()?;
            Ok(exit::FOUND)
        }

        Cmd::Live { path } => {
            let socket = cairn_daemon::socket_path(&db);
            let Some(mut client) = cairn_daemon::Client::connect(&socket) else {
                anyhow::bail!(
                    "the live view needs a running daemon (`cairn daemon --repo <dir>`)"
                );
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
            print!("{}", cairn_fmt::live_overlay(&path, &live, &indexed, &mut budget).render());
            Ok(exit::FOUND)
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
                    Ok(if d.is_empty() { exit::FOUND } else { exit::DEGRADED })
                }
                None => {
                    // An unknown dirty set and an empty one look the same in an answer.
                    // Conflating them is the silent staleness this design forbids.
                    println!("daemon     not running");
                    println!(
                        "stale: NOT TRACKED - start `cairn daemon --repo <dir>` for live \
                         staleness, or run `cairn verify --repo <dir>` for a one-off check"
                    );
                    Ok(exit::DEGRADED)
                }
            }
        }
    }
}

fn run_concept(sub: ConceptCmd, db: &PathBuf, budget: &mut Budget) -> Result<u8> {
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
        ConceptCmd::Link { name, handle, rel, note, ns } => {
            let concept = store
                .concept_find(&ns, &name)?
                .with_context(|| format!("no concept {ns}/{name} (create it first)"))?;
            let symbol_id = resolve(&store, &handle)?;
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
            print!("{}", cairn_fmt::concept(&store, &concept, &links, budget)?.render());
            Ok(exit::FOUND)
        }
        ConceptCmd::List { ns } => {
            let list = store.concept_list(ns.as_deref())?;
            let found = !list.is_empty();
            print!("{}", cairn_fmt::concept_list(&list, budget).render());
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
