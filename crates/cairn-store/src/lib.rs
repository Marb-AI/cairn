//! Local projection of the code graph: SQLite plus the ingest and query paths.
//!
//! Layering note (architecture D6): this database is never a source of truth. It is
//! rebuildable from content-addressed facts at any time, which is what lets ingest
//! run with `synchronous = OFF` and lets the schema drop-and-rebuild instead of
//! migrating.

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

pub mod batch;
pub mod graph;
pub mod ingest;
pub mod query;
pub mod schema;

pub use batch::{BatchStats, BatchWriter};
pub use graph::{Direction, Walk, WalkNode};
pub use query::{Occurrence, SymbolRow};

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
}

impl EdgeSource {
    pub fn label(self) -> &'static str {
        match self {
            EdgeSource::Scip => "L1, exact",
            EdgeSource::Weak => "L1-W, unverified",
        }
    }
}

pub struct Store {
    pub conn: Connection,
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
        let store = Store { conn };
        store.check_schema_version()?;
        Ok(store)
    }

    fn check_schema_version(&self) -> Result<()> {
        let found: Option<String> = self.get_meta("schema_version")?;
        match found.as_deref().map(str::parse::<i64>) {
            Some(Ok(v)) if v == schema::SCHEMA_VERSION => Ok(()),
            Some(Ok(v)) => anyhow::bail!(
                "index was built by schema v{v}, this binary speaks v{} - re-run `cairn index`",
                schema::SCHEMA_VERSION
            ),
            _ => anyhow::bail!("index is missing its schema version - re-run `cairn index`"),
        }
    }

    pub fn open_in_memory() -> Result<Store> {
        let conn = Connection::open_in_memory()?;
        schema::apply(&conn)?;
        Ok(Store { conn })
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
        Ok(Store { conn })
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
pub fn symbol_hash(symbol: &str) -> [u8; 16] {
    let full = blake3::hash(symbol.as_bytes());
    let mut out = [0u8; 16];
    out.copy_from_slice(&full.as_bytes()[..16]);
    out
}
