//! Local projection of the code graph: SQLite plus the ingest and query paths.
//!
//! Layering note (architecture D6): this database is never a source of truth. It is
//! rebuildable from content-addressed facts at any time, which is what lets ingest
//! run with `synchronous = OFF` and lets the schema drop-and-rebuild instead of
//! migrating.

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

pub mod affects;
pub mod attribute;
pub mod batch;
pub mod build;
pub mod concepts;
pub mod config;
pub mod context;
pub mod conventions;
pub mod coverage;
pub mod deploy;
pub mod docs;
pub mod graph;
pub mod ingest;
pub mod llmverify;
pub mod ondemand;
pub mod protolink;
pub mod query;
pub mod rules;
pub mod schema;
pub mod survey;
pub mod verify;
pub mod weak;

pub use affects::{Affects, Hop, InProcess, Outgoing};
pub use batch::{BatchStats, BatchWriter};
pub use concepts::{Concept, ConceptLink};
pub use config::Config;
pub use context::{ContextResult, Seed, SeedSource};
pub use coverage::{Area, State, TreeLanguage};
pub use deploy::{
    Attribution, DeployServiceRow, DeployStats, Entrypoint, Service, Topology, Trigger,
};
pub use docs::{Document, DocumentRow, Hit, Section, SectionRow};
pub use graph::{Direction, PathHop, Walk, WalkNode};
pub use llmverify::{Check, Standing, Verdict};
pub use protolink::{Chain, ChainHop, CrossLink, RpcCaller, ServiceRole};
pub use query::{Occurrence, SymbolRow};
// `SymbolRow::kind` is public and its type was not reachable from this crate, so anything
// downstream that wanted to build one had to depend on `cairn-scip` for a single name.
pub use cairn_scip::SymbolKind;
pub use rules::Rules;
pub use survey::{OutlineEntry, Unreached, UnreachedSymbol};
pub use verify::Report;
pub use weak::{LiteralSite, LiteralStats};

/// Seconds since the epoch, rendered as a timestamp without pulling in a date library.
///
/// Here rather than beside either caller: the session log and a recorded verdict both
/// want one, and a second hand-rolled calendar is a second place to get a leap year
/// wrong.
pub fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Civil-from-days, the standard algorithm. Enough to sort and to read.
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Language tag. Stored as an integer; the set is closed on purpose (D16 puts
/// per-language knowledge in rule packs, not in the core schema).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum Lang {
    Unknown = 0,
    Python = 1,
    Go = 2,
    TypeScript = 3,
}

impl Lang {
    pub fn from_i64(v: i64) -> Lang {
        match v {
            1 => Lang::Python,
            2 => Lang::Go,
            3 => Lang::TypeScript,
            _ => Lang::Unknown,
        }
    }

    /// Short tag used in output. Two characters keeps result lines narrow (6.3).
    pub fn tag(self) -> &'static str {
        match self {
            Lang::Python => "py",
            Lang::Go => "go",
            Lang::TypeScript => "ts",
            Lang::Unknown => "??",
        }
    }

    /// Derived from the SCIP tool name, not from file extensions.
    pub fn from_scip_scheme(scheme: &str) -> Lang {
        match scheme {
            "scip-python" => Lang::Python,
            "scip-go" => Lang::Go,
            "scip-typescript" => Lang::TypeScript,
            _ => Lang::Unknown,
        }
    }
}

/// How a file was judged to be generated. Ordered by trustworthiness — the retraction
/// in docs/spike-0-results.md section 5 is exactly about not trusting the weakest one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum GeneratedVia {
    No = 0,
    HeaderMarker = 1,
    GitAttributes = 2,
    PathPattern = 3,
}

/// Edge kinds. Numeric values are persisted, so they may be appended to but not
/// reordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum EdgeKind {
    Calls = 0,
    Implements = 1,
    /// Lexical candidate: a string literal naming this symbol (architecture 18.4).
    WeakRef = 2,
    /// A link the static pass cannot see, asserted by whoever read the code.
    Asserted = 3,
    /// A type owns this member. Derived at index time so reachability can cross from a
    /// registered class into the methods it puts on the live path without paying a join
    /// per node.
    Member = 4,
}

/// Where an edge came from. Answers group by this so an exact edge is never presented
/// next to a statistical one without saying so (architecture 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum EdgeSource {
    /// Derived from the SCIP index: exact.
    Scip = 0,
    /// Weak, lexical: a candidate, not a fact (L1-W, architecture 18.4).
    Weak = 1,
    /// Recorded by an agent that read the code and concluded the link exists.
    /// Everything at 2 and above is hand-authored and survives reindexing.
    Agent = 2,
    /// Recorded by a human.
    Human = 3,
}

impl EdgeSource {
    pub fn label(self) -> &'static str {
        match self {
            EdgeSource::Scip => "L1, exact",
            EdgeSource::Weak => "L1-W, unverified",
            EdgeSource::Agent => "L2, agent-asserted",
            EdgeSource::Human => "L2, human-asserted",
        }
    }
}

pub struct Store {
    pub conn: Connection,
    /// False when the authored-knowledge sidecar could not be opened and a memory one
    /// stood in. Everything still works; nothing written survives the process. Carried
    /// so a caller can say so before printing "recorded".
    knowledge_is_durable: bool,
    /// Conventions this store reads the world with. Defaults to the built-in pack; an
    /// index run overrides it from `.cairn/rules.yaml` when there is one, so a repository
    /// whose commands or generated code do not look like the defaults can say so without
    /// a rebuild (architecture D16).
    pub rules: rules::Rules,
}

impl Store {
    /// Open an existing store for querying.
    ///
    /// Deliberately does *no* schema work. Every CLI invocation is a fresh process
    /// (D1), so anything done here is paid per query: running `CREATE TABLE IF NOT
    /// EXISTS` seven times and re-declaring `journal_mode` on each open cost more than
    /// the queries themselves. The schema is established by `reset`, and its version is
    /// checked rather than re-applied.
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        schema::tune_for_query(&conn)?;
        let knowledge_is_durable = schema::attach_knowledge(&conn, &knowledge_path(path))?;
        let store = Store {
            conn,
            knowledge_is_durable,
            rules: rules::Rules::default(),
        };
        store.check_schema_version()?;
        Ok(store)
    }

    /// Whether anything written to the authored side will still be there next time.
    ///
    /// False means a memory sidecar stood in for one that could not be opened — a
    /// read-only checkout, a directory someone removed. Every write still succeeds and
    /// every read still works; nothing outlives the process. A caller about to print
    /// "recorded" has to know which of those it is doing.
    pub fn knowledge_is_durable(&self) -> bool {
        self.knowledge_is_durable
    }

    fn check_schema_version(&self) -> Result<()> {
        let found: Option<String> = self.get_meta("schema_version")?;
        match found.as_deref().map(str::parse::<i64>) {
            Some(Ok(v)) if v == schema::SCHEMA_VERSION => Ok(()),
            Some(Ok(v)) => anyhow::bail!(
                "index was built by schema v{v}, this binary speaks v{} - re-run `cairn index`",
                schema::SCHEMA_VERSION
            ),
            // Also what a reader sees mid-rebuild: `index` is not one transaction, so the
            // schema version is briefly absent while it runs. Every read fails for that
            // window, which is honest but unavailable — and it must be reported as
            // *degraded* rather than as a bad query, or an agent concludes it asked wrong.
            _ => anyhow::bail!(
                "index is incomplete - it is being rebuilt, or the last `cairn index` did \
                 not finish. Retry, or re-run `cairn index`"
            ),
        }
    }

    pub fn open_in_memory() -> Result<Store> {
        let conn = Connection::open_in_memory()?;
        schema::apply(&conn)?;
        schema::attach_knowledge(&conn, std::path::Path::new(":memory:"))?;
        Ok(Store {
            conn,
            knowledge_is_durable: false,
            rules: rules::Rules::default(),
        })
    }

    /// Drops and recreates the schema. Cheap because the store is a projection.
    pub fn reset(path: &Path) -> Result<Store> {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        for suffix in ["-wal", "-shm"] {
            let side = path.with_extension(format!(
                "{}{suffix}",
                path.extension().and_then(|e| e.to_str()).unwrap_or("")
            ));
            let _ = std::fs::remove_file(side);
        }
        let conn = Connection::open(path)?;
        schema::tune_for_bulk_load(&conn)?;
        schema::apply(&conn)?;
        // Deliberately attached *after* the projection was wiped: authored knowledge
        // survives every rebuild, which is the whole reason it lives in its own file.
        let knowledge_is_durable = schema::attach_knowledge(&conn, &knowledge_path(path))?;
        Ok(Store {
            conn,
            knowledge_is_durable,
            rules: rules::Rules::default(),
        })
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            (key, value),
        )?;
        Ok(())
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
        let mut rows = stmt.query([key])?;
        Ok(match rows.next()? {
            Some(r) => Some(r.get(0)?),
            None => None,
        })
    }

    /// One-line description of what is indexed, for answers that would otherwise leave
    /// the caller guessing whether their code is covered at all.
    ///
    /// A miss is ambiguous without it: "no such symbol" and "that language is not in
    /// this index" look identical, and an agent that cannot tell them apart probes
    /// repeatedly. Measured: an arm asked about frontend code that lives in a different
    /// repository and spent four times the baseline's tool calls establishing it was
    /// not there.
    pub fn coverage_summary(&self) -> Result<String> {
        let roots = self.language_roots()?;
        if roots.is_empty() {
            return Ok("nothing recorded about what is indexed".to_string());
        }
        let parts: Vec<String> = roots.iter().map(|(l, r)| format!("{r}/ ({l})")).collect();
        Ok(format!("indexed: {}", parts.join(", ")))
    }

    /// Per-language source roots recorded at index time, as `(lang tag, relative dir)`.
    pub fn language_roots(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM meta WHERE key LIKE 'root.%'")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?
                    .trim_start_matches("root.")
                    .to_string(),
                r.get::<_, String>(1)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn counts(&self) -> Result<Counts> {
        let one = |sql: &str| -> Result<i64> { Ok(self.conn.query_row(sql, [], |r| r.get(0))?) };
        Ok(Counts {
            files: one("SELECT count(*) FROM files")?,
            symbols: one("SELECT count(*) FROM symbols")?,
            occurrences: one("SELECT count(*) FROM occurrences")?,
            generated_files: one("SELECT count(*) FROM files WHERE generated = 1")?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Counts {
    pub files: i64,
    pub symbols: i64,
    pub occurrences: i64,
    pub generated_files: i64,
}

/// blake3-128 of a SCIP symbol string.
///
/// This is both the primary key for symbols and the seed for handles (6.5), so it must
/// stay stable forever: same symbol string anywhere, on any machine, same hash.
/// Where authored knowledge lives, beside the projection it annotates.
pub fn knowledge_path(index_path: &Path) -> std::path::PathBuf {
    index_path.with_file_name(
        index_path
            .file_stem()
            .map(|s| format!("{}-knowledge.sqlite", s.to_string_lossy()))
            .unwrap_or_else(|| "knowledge.sqlite".to_string()),
    )
}

pub fn symbol_hash(symbol: &str) -> [u8; 16] {
    let full = blake3::hash(symbol.as_bytes());
    let mut out = [0u8; 16];
    out.copy_from_slice(&full.as_bytes()[..16]);
    out
}

impl Store {
    /// Whether a derived layer holds anything at all.
    ///
    /// The generalisation of a bug found three times in one evening and then twice more
    /// when the same question was put to the remaining layers: the weak links, the service
    /// graph, an unindexed path prefix, the documentation sections, the literals. Each time
    /// an empty derived source produced a confident negative — and `docs --about` and
    /// `literal` went further and labelled theirs `[L0, exact]`.
    ///
    /// Exposed as one place so the next layer added to this store has an obvious thing to
    /// call, rather than each command re-deciding what its own silence means.
    pub fn layer_counts(&self) -> LayerCounts {
        let n = |sql: &str| -> i64 { self.conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
        LayerCounts {
            doc_sections: n("SELECT count(*) FROM doc_sections"),
            literals: n("SELECT count(*) FROM literals"),
            service_links: n("SELECT count(*) FROM service_links"),
            // The deployment side, which the five layers above were fixed without. On a
            // repository whose topology resolves nothing - a frontend, a library, anything
            // with no compose file cairn can read - `topology` said "0 services" with
            // `unknown: none` and `affects` said "affects 0 deployed service(s)". Two
            // confident zeros from a layer that had not been built.
            deploy_entrypoints: n(
                "SELECT count(*) FROM deploy_services WHERE entry_file IS NOT NULL",
            ),
        }
    }
}

/// Row counts for the derived layers a command can answer a negative from.
#[derive(Debug, Clone, Copy)]
pub struct LayerCounts {
    pub doc_sections: i64,
    pub literals: i64,
    pub service_links: i64,
    pub deploy_entrypoints: i64,
}

impl LayerCounts {
    /// The sentence to add when a layer is empty, or `None` when it holds something.
    ///
    /// One wording for every layer on purpose. An agent that learns what
    /// "UNCHECKED rather than empty" means once should not have to learn it again per
    /// command, and a caller matching on the text has one string to match.
    pub fn unchecked(n: i64, layer: &str, how: &str) -> Option<String> {
        (n == 0).then(|| {
            format!(
                "this index holds NO {layer} at all, so this answer is UNCHECKED rather \
                 than empty - nothing has been ruled out. {how}"
            )
        })
    }
}

#[cfg(test)]
mod layer_count_tests {
    use super::*;

    #[test]
    fn every_counted_layer_names_a_column_that_exists() {
        // `layer_counts` swallows a SQL error and returns 0, which is right when a table is
        // legitimately empty and catastrophic when a column has been renamed: every command
        // that reads it would report UNCHECKED on every repository, and nothing would say
        // why. The first version of the deployment count asked for `entrypoint_symbol`,
        // which is spelled `entry_file`; it compiled, ran, and returned 0 everywhere.
        let store = Store::open_in_memory().expect("in-memory store");
        let counts = store.layer_counts();
        // Zero is the honest answer for an empty store. What is being checked is that the
        // queries *ran* — a failed one is indistinguishable from an empty table here, so
        // each is re-issued directly and its error surfaced.
        for sql in [
            "SELECT count(*) FROM doc_sections",
            "SELECT count(*) FROM literals",
            "SELECT count(*) FROM service_links",
            "SELECT count(*) FROM deploy_services WHERE entry_file IS NOT NULL",
        ] {
            store
                .conn
                .query_row(sql, [], |r| r.get::<_, i64>(0))
                .unwrap_or_else(|e| panic!("layer count query failed: {sql}: {e}"));
        }
        assert_eq!(counts.deploy_entrypoints, 0);
    }
}
