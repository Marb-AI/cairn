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
pub const SCHEMA_VERSION: i64 = 12;

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
    gen_via   INTEGER NOT NULL DEFAULT 0,
    -- Test file. Unlike `generated` this really is a naming convention, because that
    -- is what test runners themselves key on.
    is_test   INTEGER NOT NULL DEFAULT 0,
    -- blake3-128 of the file contents at index time. Lets `cairn verify` state
    -- exactly which files have changed since, instead of guessing at staleness -
    -- and it is the foundation the dirty overlay builds on (architecture 4.3).
    content_hash BLOB
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
    def_generated INTEGER NOT NULL DEFAULT 0,
    -- Last line of the definition's enclosing range, when the indexer emits one.
    -- Only definitions with a body get it (~22 % of all definitions), which is
    -- exactly the set that can *contain* a reference - measured coverage of
    -- reference attribution is 87 % for Go and 92 % for Python.
    def_end_line  INTEGER,
    -- Docstring or doc comment, straight from the index. Free: SCIP already carries it
    -- (77.7 % of Python symbols, 10.5 % of Go ones on the target repo), and it is the
    -- best bridge there is between a feature name and a symbol - "OAuth" often appears
    -- in no identifier at all but in the first line of the docs (architecture 4.5).
    doc           TEXT
);

-- Derived relations. One table for every edge kind so that exact (L1), weak (L1-W),
-- deployment (L0-D) and statistical (L3) edges share a shape and are separated in
-- answers by `source` and `confidence`, never silently mixed (architecture 3, 5.4).
CREATE TABLE IF NOT EXISTS edges (
    -- Null for edges that have a site but no known source symbol, which is the normal
    -- case for weak lexical links: we know a literal in a file names this symbol, not
    -- which function it sits in.
    src_symbol INTEGER REFERENCES symbols(id),
    dst_symbol INTEGER NOT NULL REFERENCES symbols(id),
    kind       INTEGER NOT NULL,   -- see EdgeKind
    source     INTEGER NOT NULL,   -- see EdgeSource
    confidence REAL    NOT NULL DEFAULT 1.0,
    -- Where the edge was observed, for `calls`: lets an answer cite the call site.
    file_id    INTEGER REFERENCES files(id),
    line       INTEGER,
    -- Free text for hand-authored edges: why this link exists at all.
    note       TEXT,
    -- Set when the anchor file changed after a manual edge was recorded, so the edge
    -- may no longer hold. Never auto-cleared: only a fresh judgement clears it.
    needs_review INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS occurrences (
    file_id   INTEGER NOT NULL REFERENCES files(id),
    symbol_id INTEGER NOT NULL REFERENCES symbols(id),
    line      INTEGER NOT NULL,
    col_start INTEGER NOT NULL,
    col_end   INTEGER NOT NULL,
    role      INTEGER NOT NULL,
    -- Last line of this definition's body, when the indexer emits an enclosing
    -- range. Null for references and for definitions without a body.
    enc_end   INTEGER
);

-- gRPC services, recovered from generated symbol names rather than from .proto files
-- (protolink.rs). The package is part of the key: orders_api.AuthService and
-- orders_fe.AuthService are different services that share a name.
CREATE TABLE IF NOT EXISTS proto_services (
    id   INTEGER PRIMARY KEY,
    pkg  TEXT NOT NULL,
    name TEXT NOT NULL,
    UNIQUE(pkg, name)
);

CREATE TABLE IF NOT EXISTS service_links (
    service_id INTEGER NOT NULL REFERENCES proto_services(id),
    symbol_id  INTEGER NOT NULL REFERENCES symbols(id),
    role       INTEGER NOT NULL,   -- 0 serves, 1 calls
    -- Generated artefact the link was recovered through, so an answer can show its work.
    via_symbol INTEGER REFERENCES symbols(id),
    UNIQUE(service_id, symbol_id, role)
);

-- Deployable units from the compose file, each resolved to the symbol its start
-- command runs. Fifteen services in the target repo are built from two source trees,
-- so only reachability from these entry symbols says which service runs a given module
-- (deploy.rs).
CREATE TABLE IF NOT EXISTS deploy_services (
    name          TEXT PRIMARY KEY,
    command       TEXT,
    build_context TEXT,
    image         TEXT,
    ports         TEXT NOT NULL DEFAULT '',
    aliases       TEXT NOT NULL DEFAULT '',
    -- The file the start command lands in, not a single symbol in it. `python -m mod`
    -- executes the whole module, and picking one symbol picked the wrong one: the first
    -- definition in a server module is usually a type alias that calls nothing, so
    -- reachability from it found no handlers at all.
    entry_file    INTEGER REFERENCES files(id)
);

-- Code a service runs *after* it has started: a cron entry, a management command, a
-- `docker exec`. Kept apart from deploy_services because the relationship is different —
-- a service has one start command and any number of on-demand entrypoints, and the two
-- carry different confidence. Measured: without this, a container started with
-- `tail -f /dev/null` looks like it runs nothing while a nightly job in it reaches deep
-- into the codebase (eval/RESULTS.md, task E).
CREATE TABLE IF NOT EXISTS deploy_on_demand (
    service    TEXT NOT NULL,
    -- The cron expression, when the trigger is one. Null for an entrypoint script with
    -- no schedule behind it.
    schedule   TEXT,
    -- Repo-relative path of the runner script, so an answer can cite its evidence.
    script     TEXT NOT NULL,
    command    TEXT NOT NULL,
    entry_file INTEGER REFERENCES files(id),
    UNIQUE(service, script, command)
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
CREATE INDEX IF NOT EXISTS sym_has_doc   ON symbols(def_file_id) WHERE doc IS NOT NULL;
CREATE INDEX IF NOT EXISTS sym_by_defspan ON symbols(def_file_id, def_line, def_end_line);
CREATE INDEX IF NOT EXISTS edge_out       ON edges(src_symbol, kind);
CREATE INDEX IF NOT EXISTS edge_in        ON edges(dst_symbol, kind);
CREATE INDEX IF NOT EXISTS file_is_test   ON files(is_test);
CREATE INDEX IF NOT EXISTS slink_by_symbol  ON service_links(symbol_id, role);
CREATE INDEX IF NOT EXISTS slink_by_service ON service_links(service_id, role);
"#;

/// Authored knowledge: concepts and hand-written links.
///
/// Lives in its own database file, attached as `k`, for two reasons that both bite:
///
/// * **Lifecycle.** The main store is a projection and is dropped and rebuilt whenever
///   the schema changes or the repo is reindexed. Authored knowledge is the one thing
///   here that cannot be re-derived, so it must not share that fate.
/// * **Identity.** Symbol rowids are assigned per ingest and differ between rebuilds.
///   Authored rows therefore reference symbols by their content hash (`symbol_hash`),
///   which is stable across machines and rebuilds by construction (5.1).
pub const SQL_KNOWLEDGE: &str = r#"
CREATE TABLE IF NOT EXISTS k.concepts (
    id      INTEGER PRIMARY KEY,
    ns      TEXT NOT NULL,
    name    TEXT NOT NULL,
    note    TEXT,
    author  INTEGER NOT NULL,        -- EdgeSource: 2 agent, 3 human
    UNIQUE(ns, name)
);

CREATE TABLE IF NOT EXISTS k.concept_links (
    concept_id  INTEGER NOT NULL REFERENCES concepts(id),
    symbol_hash BLOB NOT NULL,       -- stable across rebuilds, unlike a rowid
    rel         TEXT NOT NULL,
    note        TEXT,
    -- Anchor: path plus the file's content hash at the time of the claim. Stored as
    -- text, because the files table belongs to the rebuildable side.
    anchor_path TEXT,
    anchor_line INTEGER,
    anchor_hash BLOB,
    needs_review INTEGER NOT NULL DEFAULT 0,
    UNIQUE(concept_id, symbol_hash, rel)
);

CREATE TABLE IF NOT EXISTS k.links (
    src_hash    BLOB NOT NULL,
    dst_hash    BLOB NOT NULL,
    rel         TEXT NOT NULL,
    note        TEXT,
    author      INTEGER NOT NULL,
    anchor_path TEXT,
    anchor_line INTEGER,
    anchor_hash BLOB,
    needs_review INTEGER NOT NULL DEFAULT 0,
    UNIQUE(src_hash, dst_hash, rel)
);

CREATE INDEX IF NOT EXISTS k.clink_by_symbol ON concept_links(symbol_hash);
CREATE INDEX IF NOT EXISTS k.links_by_src    ON links(src_hash);
CREATE INDEX IF NOT EXISTS k.links_by_dst    ON links(dst_hash);
"#;

/// Attach the knowledge database beside the projection and make sure it exists.
pub fn attach_knowledge(conn: &Connection, path: &std::path::Path) -> Result<()> {
    conn.execute("ATTACH DATABASE ?1 AS k", [path.to_string_lossy()])?;
    conn.execute_batch(SQL_KNOWLEDGE)?;
    Ok(())
}

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
                   o.file_id, o.line, o.col_start, o.col_end, f.generated, o.enc_end,
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
                (SELECT generated FROM _defs d WHERE d.symbol_id = symbols.id AND d.rn = 1), 0),
            def_end_line  = (SELECT enc_end FROM _defs d WHERE d.symbol_id = symbols.id AND d.rn = 1);

        DROP TABLE _refc;
        DROP TABLE _defs;
        "#,
    )?;
    derive_call_edges(conn)?;
    assign_handles(conn)?;
    conn.execute_batch("ANALYZE;")?;
    Ok(())
}

/// Assign a handle to every symbol, in bulk, at ingest.
///
/// These used to be handed out lazily on first display, which meant **a read query
/// wrote to the database**. That breaks a read-only deployment, contends between
/// concurrent readers, and showed up the moment the binary was run as a user who did
/// not own the index. Handles are deterministic anyway (a prefix of the symbol hash),
/// so there is no reason to defer the work.
///
/// Shortest unique prefix: try two characters, lengthen on collision. Done in SQL so it
/// is one pass rather than 71k round trips.
fn assign_handles(conn: &Connection) -> Result<()> {
    // Encoding in Rust rather than SQL: base32 over a byte string is awkward in SQL and
    // the collision walk needs a running set of what is taken, which is natural here.
    let mut rows: Vec<(i64, Vec<u8>)> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT id, hash FROM symbols ORDER BY id")?;
        let it = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
        for r in it {
            rows.push(r?);
        }
    }
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let mut stmt = conn.prepare("SELECT handle FROM handles")?;
        let it = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for h in it {
            taken.insert(h?);
        }
    }
    let mut insert = conn.prepare("INSERT OR IGNORE INTO handles(symbol_id, handle) VALUES (?1, ?2)")?;
    for (id, hash) in rows {
        let full = crate::query::encode_handle(&hash);
        for len in 2..=full.len() {
            let candidate = &full[..len];
            if taken.contains(candidate) {
                continue;
            }
            taken.insert(candidate.to_string());
            insert.execute(rusqlite::params![id, candidate])?;
            break;
        }
    }
    Ok(())
}

/// Derive `calls` edges: which symbol's body contains each reference.
///
/// SCIP gives no call graph and never fills `enclosing_symbol`, but it does give an
/// enclosing range for definitions that have a body. Attributing a reference to the
/// *innermost* such definition reconstructs the edge. References that fall in no body
/// are module-level (imports, top-level constants) and correctly have no caller —
/// they are counted, not hidden, so `unknown:` can report them.
fn derive_call_edges(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DELETE FROM edges WHERE kind = 0 AND source = 0;

        INSERT INTO edges(src_symbol, dst_symbol, kind, source, confidence, file_id, line)
        SELECT caller, callee, 0, 0, 1.0, file_id, line FROM (
            SELECT s.id AS caller, o.symbol_id AS callee, o.file_id, o.line,
                   row_number() OVER (
                       PARTITION BY o.file_id, o.line, o.col_start, o.symbol_id
                       ORDER BY (s.def_end_line - s.def_line) ASC
                   ) AS rn
              FROM occurrences o
              JOIN symbols s
                ON s.def_file_id = o.file_id
               AND s.def_end_line IS NOT NULL
               AND s.def_line   <= o.line
               AND s.def_end_line >= o.line
             WHERE (o.role & 1) = 0
               AND s.id <> o.symbol_id
        ) WHERE rn = 1;

        -- Second pass: references that sit at module level, where the binding itself is
        -- the caller.
        --
        -- `arecalculate_plan = db_async(recalculate_plan)` is a real, compiler-resolved
        -- reference, but it is in no function body, so the pass above drops it and the
        -- async half of an entire repository layer becomes unreachable. Measured: the
        -- gRPC handler calls `arecalculate_plan`, and `cairn path` reported no route from
        -- the handler to code it plainly reaches (eval/RESULTS.md, task E).
        --
        -- The symbol *defined* on that line is the binding being built, so it is the
        -- honest source of the edge. General beyond this idiom: `const x = memoize(y)`,
        -- `var h = http.HandlerFunc(f)`, a class attribute built from a factory.
        INSERT INTO edges(src_symbol, dst_symbol, kind, source, confidence, file_id, line)
        SELECT binder, callee, 0, 0, 1.0, file_id, line FROM (
            SELECT s.id AS binder, o.symbol_id AS callee, o.file_id, o.line,
                   row_number() OVER (
                       PARTITION BY o.file_id, o.line, o.col_start, o.symbol_id
                       ORDER BY s.def_col_start ASC
                   ) AS rn
              FROM occurrences o
              JOIN symbols s
                ON s.def_file_id = o.file_id
               AND s.def_line = o.line
               AND s.id <> o.symbol_id
             WHERE (o.role & 1) = 0
               AND NOT EXISTS (
                   SELECT 1 FROM symbols encl
                    WHERE encl.def_file_id = o.file_id
                      AND encl.def_end_line IS NOT NULL
                      AND encl.def_line <= o.line
                      AND encl.def_end_line >= o.line
                      AND encl.id <> o.symbol_id)
        ) WHERE rn = 1;
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
