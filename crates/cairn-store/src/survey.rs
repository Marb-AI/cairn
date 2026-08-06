//! Set-shaped queries: questions about a whole subtree rather than one symbol.
//!
//! This exists because of a measurement. Asked "which functions in this package are
//! called only from tests", an agent with cairn spent 41 tool calls doing `symbol` then
//! `graph --aspect callers` once per symbol, for 178 symbols — and only matched the
//! grep-and-script baseline (the measurement record). The index held the answer to the whole
//! question the entire time; there was simply no way to ask for it.
//!
//! That is the general lesson, not a one-off: per-symbol commands make an agent pay a
//! round trip and a response envelope for every candidate. Where a question is naturally
//! about a *set*, the tool has to answer it as a set.

use crate::{Store, SymbolRow};
use anyhow::Result;
use rusqlite::params;

/// Why a symbol appears in a reachability survey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unreached {
    /// Nothing calls it at all, tests included.
    Never,
    /// Only test code calls it.
    TestsOnly,
}

impl Unreached {
    pub fn label(self) -> &'static str {
        match self {
            Unreached::Never => "no callers at all",
            Unreached::TestsOnly => "tests only",
        }
    }
}

#[derive(Debug, Clone)]
pub struct UnreachedSymbol {
    pub symbol: SymbolRow,
    pub why: Unreached,
    pub test_callers: i64,
    /// Production references that are not calls: a re-export, a name in a table, a type
    /// used to build something. Zero means the symbol can go on its own; anything else is
    /// what else has to change with it.
    pub prod_refs: i64,
}

/// One entry in a module outline.
#[derive(Debug, Clone)]
pub struct OutlineEntry {
    pub symbol: SymbolRow,
    pub caller_count: i64,
    pub production_callers: i64,
    /// A service binding reaches this, so a static caller count of zero says nothing.
    ///
    /// Fixing `unreached` was not enough (task M): `outline` kept labelling thirty RPC
    /// methods `test-only`, an agent correctly disbelieved every one of them, and spent a
    /// third more than the baseline checking. A wrong label is not cheaper than no label.
    pub dispatched: bool,
}

impl Store {
    /// How many indexed files sit under a path prefix.
    ///
    /// The question every set-shaped answer has to ask before reporting an empty set.
    /// `unreached tools/pbgen` said "0 symbols … everything here has a production caller",
    /// marked `[L1, exact]`, exit 0 — over a directory holding four Python files that no
    /// indexer has ever read. `outline` said "0 of 0 definitions". Both were describing
    /// the index's silence as a property of the code.
    pub fn indexed_under(&self, prefix: &str) -> Result<i64> {
        let like = format!("{}%", prefix.trim_end_matches('/'));
        Ok(self.conn.query_row(
            "SELECT count(*) FROM files f JOIN strings p ON p.id = f.path_id
              WHERE p.s LIKE ?1",
            params![like],
            |r| r.get(0),
        )?)
    }

    /// A handful of indexed paths, for checking that a directory really is the tree this
    /// index describes.
    ///
    /// `<repo>/.cairn/index.sqlite` is the convention, so the repository is two levels up
    /// — but it is a convention, not a guarantee, and `--db` can point anywhere. The test
    /// harness builds its index under `/tmp`, and a search derived that way read `/tmp`
    /// and reported "nothing in the working tree" about a tree it had never opened. A
    /// derived root has to be corroborated before an answer rests on it.
    pub fn sample_paths(&self, n: usize) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.s FROM files f JOIN strings p ON p.id = f.path_id
              WHERE f.generated = 0 LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![n as i64], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Which of these names the index holds as a hand-written type or function.
    ///
    /// For corroborating an empty callee list. `graph --aspect calls` deliberately drops
    /// parameters, locals and anything with no definition here — a stdlib `round(` is
    /// noise, and excluding it is right. What that exclusion cannot distinguish is a name
    /// the index has never heard of from one it knows perfectly well and simply failed to
    /// link, and the second is a missing edge rather than a correct silence.
    ///
    /// Measured on the target repo: of 59 sampled hand-written functions, 13 got an empty
    /// callee list, and **12 of those 13 name a symbol this index knows** — `_fmt_czk`,
    /// `compute_f1`, `_augment_system_prompt`. Those are calls, and the answer said the
    /// function calls nothing.
    pub fn known_symbol_names(&self, candidates: &[String]) -> Result<Vec<String>> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let holes = vec!["?"; candidates.len()].join(",");
        let sql = format!(
            "SELECT DISTINCT n.s FROM symbols s
               JOIN strings n ON n.id = s.name_id
               JOIN files f ON f.id = s.def_file_id AND f.generated = 0
              WHERE s.kind IN (1, 3) AND n.s IN ({holes})"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(candidates), |r| r.get(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Symbols under `prefix` that no production code calls.
    ///
    /// Restricted to things that can meaningfully be "called" — types and functions —
    /// because a field with no callers is not a finding, it is a field. Generated code
    /// is excluded: it is unreached by construction and would bury the real answers.
    pub fn unreached(&self, prefix: &str, limit: usize) -> Result<Vec<UnreachedSymbol>> {
        let like = format!("{}%", prefix.trim_end_matches('/'));
        let mut stmt = self.conn.prepare(
            r#"
            SELECT s.id,
                   (SELECT count(*) FROM edges e
                      JOIN symbols c   ON c.id = e.src_symbol
                      LEFT JOIN files cf ON cf.id = c.def_file_id
                     WHERE e.dst_symbol = s.id AND e.kind = 0
                       AND coalesce(cf.is_test, 0) = 0)  AS prod_callers,
                   (SELECT count(*) FROM edges e
                      JOIN symbols c   ON c.id = e.src_symbol
                      LEFT JOIN files cf ON cf.id = c.def_file_id
                     WHERE e.dst_symbol = s.id AND e.kind = 0
                       AND coalesce(cf.is_test, 0) = 1)  AS test_callers,
                   -- References that are not calls, in production code, excluding the
                   -- definition. A symbol with none is deletable; a symbol with some has
                   -- something that breaks when it goes, and the row has to say so.
                   --
                   -- Measured two ways, both found by the stress harness: an enum with
                   -- ten references building the table below it, and a function whose one
                   -- reference is a re-export in the package `__init__`. Dropping such
                   -- rows was the first fix and it was wrong — both are still deletion
                   -- candidates, they just cost more than one line. Naming the count
                   -- keeps the finding and removes the trap.
                   (SELECT count(*) FROM occurrences o
                      JOIN files rf ON rf.id = o.file_id
                     WHERE o.symbol_id = s.id AND (o.role & 1) = 0
                       AND coalesce(rf.is_test, 0) = 0
                       AND rf.generated = 0)             AS prod_refs
              FROM symbols s
              JOIN strings n ON n.id = s.name_id
              JOIN files   f ON f.id = s.def_file_id
              JOIN strings p ON p.id = f.path_id
             WHERE p.s LIKE ?1
               AND f.generated = 0
               AND coalesce(f.is_test, 0) = 0
               AND s.kind IN (1, 3)
               -- Code a service binding reaches is not dead, it is dispatched. Measured
               -- (task M): asked which symbols in a gRPC handlers package production never
               -- calls, this reported all 35 of them, and the right answer is none — every
               -- one is an RPC method invoked over the wire from Go. A command whose whole
               -- promise is "what production never calls" was wrong about an entire
               -- directory, which is worse than being expensive.
               -- A constructor is called by naming its type, and the call graph attributes
               -- that to the type symbol. `__init__` therefore always shows zero callers,
               -- which is an artefact of how instantiation is modelled and not a finding.
               -- Surfaced by a measured run (task M) that had to reason its way past it.
               AND n.s NOT IN ('__init__', '__new__')
               AND NOT EXISTS (
                   SELECT 1 FROM service_links l WHERE l.symbol_id = s.id AND l.role = 0)
               AND NOT EXISTS (
                   SELECT 1
                     FROM symbols t
                     JOIN service_links l ON l.symbol_id = t.id AND l.role = 0
                    WHERE t.def_file_id = s.def_file_id
                      AND s.container_leaf_id = t.name_id)
             GROUP BY s.id
            HAVING prod_callers = 0
             ORDER BY test_callers DESC, s.ref_count DESC
             LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(params![like, limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, _prod, tests, refs) = row?;
            let Some(symbol) = self.symbol(id)? else {
                continue;
            };
            out.push(UnreachedSymbol {
                symbol,
                why: if tests > 0 {
                    Unreached::TestsOnly
                } else {
                    Unreached::Never
                },
                test_callers: tests,
                prod_refs: refs,
            });
        }
        Ok(out)
    }

    /// What a module or directory contains, with how used each thing is.
    ///
    /// The question "what is in here" was the first move of every agent that had to work
    /// in an unfamiliar package, and answering it meant listing files and reading them.
    /// What a module or directory contains, with how used each thing is, and the total.
    ///
    /// The count matters: `outline` used to apply its limit in SQL and return the rows
    /// with no indication that there were more. A measured run (task M) asked about a
    /// directory holding 91 definitions, got 80, noticed two files missing entirely and
    /// re-ran per file to recover them. Silent truncation is the failure the envelope
    /// exists to prevent, and it was in one of the three commands the guide tells agents
    /// to prefer.
    pub fn outline(&self, prefix: &str, limit: usize) -> Result<(Vec<OutlineEntry>, i64)> {
        let like_total = format!("{}%", prefix.trim_end_matches('/'));
        let total: i64 = self.conn.query_row(
            "SELECT count(*) FROM symbols s
               JOIN files f ON f.id = s.def_file_id
               JOIN strings p ON p.id = f.path_id
              WHERE p.s LIKE ?1 AND f.generated = 0 AND s.kind IN (1, 3)",
            params![like_total],
            |r| r.get(0),
        )?;

        let like = format!("{}%", prefix.trim_end_matches('/'));
        let mut stmt = self.conn.prepare(
            r#"
            SELECT s.id,
                   (SELECT count(*) FROM edges e WHERE e.dst_symbol = s.id AND e.kind = 0),
                   (SELECT count(*) FROM edges e
                      JOIN symbols c   ON c.id = e.src_symbol
                      LEFT JOIN files cf ON cf.id = c.def_file_id
                     WHERE e.dst_symbol = s.id AND e.kind = 0
                       AND coalesce(cf.is_test, 0) = 0),
                   (EXISTS (SELECT 1 FROM service_links l
                             WHERE l.symbol_id = s.id AND l.role = 0)
                    OR EXISTS (SELECT 1
                                 FROM symbols t
                                 JOIN service_links l ON l.symbol_id = t.id AND l.role = 0
                                WHERE t.def_file_id = s.def_file_id
                                  AND s.container_leaf_id = t.name_id))
              FROM symbols s
              JOIN files   f ON f.id = s.def_file_id
              JOIN strings p ON p.id = f.path_id
             WHERE p.s LIKE ?1
               AND f.generated = 0
               AND s.kind IN (1, 3)
             ORDER BY p.s, s.def_line
             LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(params![like, limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)? != 0,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, callers, prod, dispatched) = row?;
            let Some(symbol) = self.symbol(id)? else {
                continue;
            };
            out.push(OutlineEntry {
                symbol,
                caller_count: callers,
                production_callers: prod,
                dispatched,
            });
        }
        Ok((out, total))
    }

    /// Everything that reads or writes a symbol, grouped by the file that owns the site.
    ///
    /// "What is the blast radius of changing this" is a question about a set of sites,
    /// and answering it one reference at a time makes the caller reassemble the grouping
    /// by hand from a flat list.
    /// Sites and files this symbol has in test files: what `usage_by_file` leaves out
    /// when tests were not asked for, so the answer can say so instead of reporting a
    /// filtered count as the whole count.
    pub fn usage_in_tests(&self, symbol_id: i64) -> Result<(i64, usize)> {
        self.conn
            .query_row(
                r#"
                SELECT coalesce(sum(n), 0), count(*) FROM (
                    SELECT count(*) AS n
                      FROM occurrences o
                      JOIN files f ON f.id = o.file_id
                     WHERE o.symbol_id = ?1 AND (o.role & 1) = 0
                       AND coalesce(f.is_test, 0) = 1
                     GROUP BY o.file_id)
                "#,
                params![symbol_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as usize)),
            )
            .map_err(Into::into)
    }

    pub fn usage_by_file(
        &self,
        symbol_id: i64,
        include_tests: bool,
        limit: usize,
    ) -> Result<Vec<(String, i64, bool)>> {
        let mut stmt = self.conn.prepare_cached(
            r#"
            SELECT p.s, count(*), coalesce(f.is_test, 0)
              FROM occurrences o
              JOIN files   f ON f.id = o.file_id
              JOIN strings p ON p.id = f.path_id
             WHERE o.symbol_id = ?1 AND (o.role & 1) = 0
               AND (?2 = 1 OR coalesce(f.is_test, 0) = 0)
             GROUP BY p.s
             ORDER BY coalesce(f.is_test, 0) ASC, count(*) DESC
             LIMIT ?3
            "#,
        )?;
        let rows = stmt.query_map(
            params![symbol_id, include_tests as i64, limit as i64],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)? != 0,
                ))
            },
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}
