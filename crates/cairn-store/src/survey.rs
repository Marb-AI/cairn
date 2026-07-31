//! Set-shaped queries: questions about a whole subtree rather than one symbol.
//!
//! This exists because of a measurement. Asked "which functions in this package are
//! called only from tests", an agent with cairn spent 41 tool calls doing `symbol` then
//! `graph --aspect callers` once per symbol, for 178 symbols — and only matched the
//! grep-and-script baseline (eval/RESULTS.md). The index held the answer to the whole
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
}

/// One entry in a module outline.
#[derive(Debug, Clone)]
pub struct OutlineEntry {
    pub symbol: SymbolRow,
    pub caller_count: i64,
    pub production_callers: i64,
}

impl Store {
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
                       AND coalesce(cf.is_test, 0) = 1)  AS test_callers
              FROM symbols s
              JOIN files   f ON f.id = s.def_file_id
              JOIN strings p ON p.id = f.path_id
             WHERE p.s LIKE ?1
               AND f.generated = 0
               AND coalesce(f.is_test, 0) = 0
               AND s.kind IN (1, 3)
             GROUP BY s.id
            HAVING prod_callers = 0
             ORDER BY test_callers DESC, s.ref_count DESC
             LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(params![like, limit as i64], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, _prod, tests) = row?;
            let Some(symbol) = self.symbol(id)? else { continue };
            out.push(UnreachedSymbol {
                symbol,
                why: if tests > 0 { Unreached::TestsOnly } else { Unreached::Never },
                test_callers: tests,
            });
        }
        Ok(out)
    }

    /// What a module or directory contains, with how used each thing is.
    ///
    /// The question "what is in here" was the first move of every agent that had to work
    /// in an unfamiliar package, and answering it meant listing files and reading them.
    pub fn outline(&self, prefix: &str, limit: usize) -> Result<Vec<OutlineEntry>> {
        let like = format!("{}%", prefix.trim_end_matches('/'));
        let mut stmt = self.conn.prepare(
            r#"
            SELECT s.id,
                   (SELECT count(*) FROM edges e WHERE e.dst_symbol = s.id AND e.kind = 0),
                   (SELECT count(*) FROM edges e
                      JOIN symbols c   ON c.id = e.src_symbol
                      LEFT JOIN files cf ON cf.id = c.def_file_id
                     WHERE e.dst_symbol = s.id AND e.kind = 0
                       AND coalesce(cf.is_test, 0) = 0)
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
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, callers, prod) = row?;
            let Some(symbol) = self.symbol(id)? else { continue };
            out.push(OutlineEntry {
                symbol,
                caller_count: callers,
                production_callers: prod,
            });
        }
        Ok(out)
    }

    /// Everything that reads or writes a symbol, grouped by the file that owns the site.
    ///
    /// "What is the blast radius of changing this" is a question about a set of sites,
    /// and answering it one reference at a time makes the caller reassemble the grouping
    /// by hand from a flat list.
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
        let rows = stmt.query_map(params![symbol_id, include_tests as i64, limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)? != 0))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}
