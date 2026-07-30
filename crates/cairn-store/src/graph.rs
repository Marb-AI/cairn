//! Walks over derived edges: callers, callees, implementations.
//!
//! Breadth and depth are always bounded by the caller (architecture 18.1). An unbounded
//! walk on a real codebase returns thousands of nodes, which spends exactly the tokens
//! this tool exists to save.

use crate::{EdgeKind, EdgeSource, Store, SymbolRow};
use anyhow::Result;
use rusqlite::params;
use std::collections::HashSet;

/// Which way an edge is followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Who calls this symbol.
    In,
    /// What this symbol calls.
    Out,
}

/// One node in a bounded walk.
#[derive(Debug, Clone)]
pub struct WalkNode {
    pub symbol: SymbolRow,
    pub depth: usize,
    /// Index of the parent in the walk's node list; `None` for the root.
    pub parent: Option<usize>,
    /// Where the edge was observed, for call edges.
    pub site: Option<String>,
    pub source: EdgeSource,
    /// Children that existed but were cut by `fanout`.
    pub truncated_children: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Walk {
    pub nodes: Vec<WalkNode>,
    /// Nodes reached again by another path; counted, not repeated.
    pub revisited: usize,
    /// Total children dropped by the fanout limit, across all nodes.
    pub truncated: usize,
}

impl Store {
    /// Breadth-first walk over edges of one kind.
    ///
    /// Breadth-first rather than depth-first on purpose: with a budget the caller is
    /// far more likely to want "everything one hop away" than one deep chain.
    pub fn walk(
        &self,
        root: i64,
        kind: EdgeKind,
        dir: Direction,
        depth: usize,
        fanout: usize,
    ) -> Result<Walk> {
        let mut walk = Walk::default();
        let Some(root_sym) = self.symbol(root)? else {
            return Ok(walk);
        };
        walk.nodes.push(WalkNode {
            symbol: root_sym,
            depth: 0,
            parent: None,
            site: None,
            source: EdgeSource::Scip,
            truncated_children: 0,
        });

        let mut seen: HashSet<i64> = HashSet::from([root]);
        let mut frontier = vec![0usize];

        for level in 1..=depth {
            let mut next = Vec::new();
            for &idx in &frontier {
                let parent_id = walk.nodes[idx].symbol.id;
                let (rows, total) = self.neighbours(parent_id, kind, dir, fanout)?;
                let mut added = 0;
                for (sym_id, source, site) in rows {
                    if !seen.insert(sym_id) {
                        walk.revisited += 1;
                        continue;
                    }
                    let Some(sym) = self.symbol(sym_id)? else {
                        continue;
                    };
                    walk.nodes.push(WalkNode {
                        symbol: sym,
                        depth: level,
                        parent: Some(idx),
                        site,
                        source,
                        truncated_children: 0,
                    });
                    next.push(walk.nodes.len() - 1);
                    added += 1;
                }
                let dropped = total.saturating_sub(added as i64) as usize;
                walk.nodes[idx].truncated_children = dropped;
                walk.truncated += dropped;
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        Ok(walk)
    }

    /// Direct neighbours plus the total count, so truncation can be reported.
    fn neighbours(
        &self,
        symbol_id: i64,
        kind: EdgeKind,
        dir: Direction,
        limit: usize,
    ) -> Result<(Vec<(i64, EdgeSource, Option<String>)>, i64)> {
        let (from_col, to_col) = match dir {
            Direction::In => ("dst_symbol", "src_symbol"),
            Direction::Out => ("src_symbol", "dst_symbol"),
        };
        let sql = format!(
            r#"
            SELECT e.{to_col}, e.source, p.s, e.line
              FROM edges e
              LEFT JOIN files   f ON f.id = e.file_id
              LEFT JOIN strings p ON p.id = f.path_id
             WHERE e.{from_col} = ?1 AND e.kind = ?2
             GROUP BY e.{to_col}
             ORDER BY e.source ASC, e.confidence DESC
             LIMIT ?3
            "#
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![symbol_id, kind as i64, limit as i64], |r| {
            let path: Option<String> = r.get(2)?;
            let line: Option<i64> = r.get(3)?;
            let site = match (path, line) {
                (Some(p), Some(l)) => Some(format!("{p}:{}", l + 1)),
                _ => None,
            };
            Ok((
                r.get::<_, i64>(0)?,
                match r.get::<_, i64>(1)? {
                    1 => EdgeSource::Weak,
                    _ => EdgeSource::Scip,
                },
                site,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }

        let count_sql =
            format!("SELECT count(DISTINCT {to_col}) FROM edges WHERE {from_col} = ?1 AND kind = ?2");
        let total: i64 = self
            .conn
            .query_row(&count_sql, params![symbol_id, kind as i64], |r| r.get(0))?;
        Ok((out, total))
    }

    /// How many reference occurrences of this symbol have no attributable caller.
    ///
    /// These are module-level references (imports, top-level constants) — the honest
    /// answer is that they exist and have no enclosing body, not that they are missing.
    pub fn unattributed_refs(&self, symbol_id: i64) -> Result<i64> {
        let refs: i64 = self.conn.query_row(
            "SELECT count(*) FROM occurrences WHERE symbol_id = ?1 AND (role & 1) = 0",
            params![symbol_id],
            |r| r.get(0),
        )?;
        let attributed: i64 = self.conn.query_row(
            "SELECT count(*) FROM edges WHERE dst_symbol = ?1 AND kind = 0",
            params![symbol_id],
            |r| r.get(0),
        )?;
        Ok((refs - attributed).max(0))
    }
}
