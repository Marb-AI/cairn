//! SQLite schema — the local, always-rebuildable projection (architecture D6, 5.4).
//!
//! Two things here are deliberate and expensive to retrofit, so they are in from the
//! start (architecture 5.5):
//!
//! * **String interning.** Paths, module paths and names repeat in nearly every row.
//! * **Symbols are stored by hash, not by string.** A SCIP symbol string is long and
//!   highly redundant (`scip-python python pkg ver `a.b.c`/Class#method().`). We only
//!   ever need it for equality and for deriving a handle, and both work on a hash.
//!   Display is reconstructed from the interned name/module/container parts.

use anyhow::Result;
use rusqlite::Connection;

/// Bumped whenever the schema changes in a way that invalidates existing databases.
/// The ingest path drops and rebuilds rather than migrating: the store is a projection,
/// never a source of truth.
pub const SCHEMA_VERSION: i64 = 2;

pub const SQL: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Interned strings: paths, module paths, symbol names, package names.
CREATE TABLE IF NOT EXISTS strings (
    id INTEGER PRIMARY KEY,
    s  TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS files (
    id        INTEGER PRIMARY KEY,
    path_id   INTEGER NOT NULL UNIQUE REFERENCES strings(id),
    lang      INTEGER NOT NULL,
    generated INTEGER NOT NULL DEFAULT 0,
    -- How `generated` was decided: 0 = not generated, 1 = header marker,
    -- 2 = .gitattributes, 3 = path pattern (weakest, see architecture 7.3).
    gen_via   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS symbols (
    id           INTEGER PRIMARY KEY,
    hash         BLOB    NOT NULL UNIQUE,  -- blake3-128 of the full SCIP symbol string
    name_id      INTEGER NOT NULL REFERENCES strings(id),
    module_id    INTEGER          REFERENCES strings(id),
    container_id INTEGER          REFERENCES strings(id),
    pkg_id       INTEGER          REFERENCES strings(id),
    kind         INTEGER NOT NULL,
    lang         INTEGER NOT NULL,
    -- Denormalised for ranking. Computed once after ingest (see `finalize`): as
    -- correlated subqueries these dominated query time, because they had to be
    -- evaluated for every candidate row before ORDER BY could apply LIMIT.
    ref_count     INTEGER NOT NULL DEFAULT 0,
    def_file_id   INTEGER REFERENCES files(id),
    def_line      INTEGER,
    def_col_start INTEGER,
    def_col_end   INTEGER,
    def_generated INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS occurrences (
    file_id   INTEGER NOT NULL REFERENCES files(id),
    symbol_id INTEGER NOT NULL REFERENCES symbols(id),
    line      INTEGER NOT NULL,
    col_start INTEGER NOT NULL,
    col_end   INTEGER NOT NULL,
    role      INTEGER NOT NULL
);

-- Short, stable, deterministic codes for progressive disclosure (architecture 6.5).
-- Assigned lazily and persisted, so a handle stays valid across sessions.
CREATE TABLE IF NOT EXISTS handles (
    symbol_id INTEGER PRIMARY KEY REFERENCES symbols(id),
    handle    TEXT NOT NULL UNIQUE
);
"#;

/// Indexes are created after bulk ingest — building them incrementally during a
/// 350k-row insert is several times slower.
pub const SQL_INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS occ_by_symbol ON occurrences(symbol_id, role);
CREATE INDEX IF NOT EXISTS occ_by_file   ON occurrences(file_id, line);
CREATE INDEX IF NOT EXISTS sym_by_name   ON symbols(name_id);
CREATE INDEX IF NOT EXISTS sym_rank       ON symbols(name_id, def_generated, ref_count DESC);
CREATE INDEX IF NOT EXISTS sym_by_module ON symbols(module_id);
"#;

pub fn apply(conn: &Connection) -> Result<()> {
    conn.execute_batch(SQL)?;
    Ok(())
}

pub fn create_indexes(conn: &Connection) -> Result<()> {
    conn.execute_batch(SQL_INDEXES)?;
    Ok(())
}

/// Pragmas for bulk ingest. `synchronous = OFF` is safe here because the database is
/// a projection: if the process dies mid-ingest we rebuild from the SCIP index.
pub fn tune_for_bulk_load(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous  = OFF;
         PRAGMA temp_store   = MEMORY;
         PRAGMA cache_size   = -262144;",
    )?;
    Ok(())
}

/// Fills the denormalised columns on `symbols` after a bulk ingest.
///
/// Done as two set-based passes over temp tables rather than per-row updates: on the
/// spike corpus that is 71k symbols against 418k occurrences.
pub fn finalize(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TEMP TABLE IF NOT EXISTS _refc AS
            SELECT symbol_id, count(*) AS c
              FROM occurrences WHERE (role & 1) = 0
             GROUP BY symbol_id;
        CREATE UNIQUE INDEX IF NOT EXISTS _refc_i ON _refc(symbol_id);

        -- Prefer a definition in non-generated code when a symbol has several.
        CREATE TEMP TABLE IF NOT EXISTS _defs AS
            SELECT o.symbol_id,
                   o.file_id, o.line, o.col_start, o.col_end, f.generated,
                   row_number() OVER (PARTITION BY o.symbol_id
                                      ORDER BY f.generated ASC, o.file_id ASC) AS rn
              FROM occurrences o JOIN files f ON f.id = o.file_id
             WHERE (o.role & 1) = 1;
        CREATE UNIQUE INDEX IF NOT EXISTS _defs_i ON _defs(symbol_id, rn);

        UPDATE symbols SET ref_count =
            coalesce((SELECT c FROM _refc WHERE _refc.symbol_id = symbols.id), 0);

        UPDATE symbols SET
            def_file_id   = (SELECT file_id   FROM _defs d WHERE d.symbol_id = symbols.id AND d.rn = 1),
            def_line      = (SELECT line      FROM _defs d WHERE d.symbol_id = symbols.id AND d.rn = 1),
            def_col_start = (SELECT col_start FROM _defs d WHERE d.symbol_id = symbols.id AND d.rn = 1),
            def_col_end   = (SELECT col_end   FROM _defs d WHERE d.symbol_id = symbols.id AND d.rn = 1),
            def_generated = coalesce(
                (SELECT generated FROM _defs d WHERE d.symbol_id = symbols.id AND d.rn = 1), 0);

        DROP TABLE _refc;
        DROP TABLE _defs;
        ANALYZE;
        "#,
    )?;
    Ok(())
}

/// Query-path pragmas.
///
/// `journal_mode` is deliberately absent: it is persisted in the database file, and
/// re-declaring it on open takes a write lock. The rest are per-connection and cheap.
pub fn tune_for_query(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -65536;",
    )?;
    Ok(())
}
