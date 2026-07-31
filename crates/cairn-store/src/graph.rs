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
        exclude_tests: bool,
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
                let (rows, total) = self.neighbours(parent_id, kind, dir, fanout, exclude_tests)?;
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
    /// Direct neighbours plus the total, so truncation can be reported.
    ///
    /// `exclude_tests` exists because it is the commonest filter there is when asking
    /// "does anything actually use this": a symbol called only from tests is dead in
    /// production, and that distinction is the whole question. Measurement found this
    /// gap - the first analysis run had to pipe the output through grep.
    fn neighbours(
        &self,
        symbol_id: i64,
        kind: EdgeKind,
        dir: Direction,
        limit: usize,
        exclude_tests: bool,
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
              JOIN symbols other ON other.id = e.{to_col}
              LEFT JOIN files otherf ON otherf.id = other.def_file_id
             WHERE e.{from_col} = ?1 AND e.kind = ?2
               AND (?4 = 0 OR coalesce(otherf.is_test, 0) = 0)
             GROUP BY e.{to_col}
             ORDER BY e.source ASC, e.confidence DESC
             LIMIT ?3
            "#
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(
            params![symbol_id, kind as i64, limit as i64, exclude_tests as i64],
            |r| {
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
            },
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }

        // The total counts what the same filter would have matched, so "N beyond
        // --fanout" never includes rows the caller asked to exclude.
        let count_sql = format!(
            "SELECT count(DISTINCT e.{to_col}) FROM edges e
               JOIN symbols other ON other.id = e.{to_col}
               LEFT JOIN files otherf ON otherf.id = other.def_file_id
              WHERE e.{from_col} = ?1 AND e.kind = ?2
                AND (?3 = 0 OR coalesce(otherf.is_test, 0) = 0)"
        );
        let total: i64 = self.conn.query_row(
            &count_sql,
            params![symbol_id, kind as i64, exclude_tests as i64],
            |r| r.get(0),
        )?;
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

/// One hop of a resolved path.
#[derive(Debug, Clone)]
pub struct PathHop {
    pub symbol: SymbolRow,
    /// Call site inside the *previous* symbol that leads here.
    pub site: Option<String>,
}

impl Store {
    /// Shortest call path from one symbol to another.
    ///
    /// Answers "how does this reach that", which is the question behind most
    /// architecture spelunking and most incident work. Breadth-first from the source,
    /// so the first path found is a shortest one; nothing here tries to find *all*
    /// paths, because on a real graph that is unbounded and unreadable.
    ///
    /// Returns `None` when no path exists within `max_depth`, which is a real answer
    /// and must not be confused with "there is no path at all".
    pub fn call_path(
        &self,
        from: i64,
        to: i64,
        max_depth: usize,
    ) -> Result<Option<Vec<PathHop>>> {
        use std::collections::{HashMap, VecDeque};

        if from == to {
            let Some(sym) = self.symbol(from)? else {
                return Ok(None);
            };
            return Ok(Some(vec![PathHop { symbol: sym, site: None }]));
        }

        // parent[node] = (previous node, call site) so the path can be rebuilt.
        let mut parent: HashMap<i64, (i64, Option<String>)> = HashMap::new();
        let mut seen: HashSet<i64> = HashSet::from([from]);
        let mut queue: VecDeque<(i64, usize)> = VecDeque::from([(from, 0usize)]);

        let mut stmt = self.conn.prepare_cached(
            r#"
            SELECT e.dst_symbol, p.s, e.line
              FROM edges e
              LEFT JOIN files   f ON f.id = e.file_id
              LEFT JOIN strings p ON p.id = f.path_id
             WHERE e.src_symbol = ?1 AND e.kind = 0
            "#,
        )?;

        let mut found = false;
        while let Some((node, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let rows = stmt.query_map(params![node], |r| {
                let path: Option<String> = r.get(1)?;
                let line: Option<i64> = r.get(2)?;
                Ok((
                    r.get::<_, i64>(0)?,
                    match (path, line) {
                        (Some(p), Some(l)) => Some(format!("{p}:{}", l + 1)),
                        _ => None,
                    },
                ))
            })?;
            for row in rows {
                let (next, site) = row?;
                if !seen.insert(next) {
                    continue;
                }
                parent.insert(next, (node, site));
                if next == to {
                    found = true;
                    break;
                }
                queue.push_back((next, depth + 1));
            }
            if found {
                break;
            }
        }
        if !found {
            return Ok(None);
        }

        let mut chain = vec![to];
        let mut cursor = to;
        while let Some((prev, _)) = parent.get(&cursor) {
            chain.push(*prev);
            cursor = *prev;
        }
        chain.reverse();

        let mut hops = Vec::with_capacity(chain.len());
        for id in chain {
            let Some(sym) = self.symbol(id)? else { continue };
            let site = parent.get(&id).and_then(|(_, s)| s.clone());
            hops.push(PathHop { symbol: sym, site });
        }
        Ok(Some(hops))
    }

    /// Tests that reach this symbol, through the call graph.
    ///
    /// A test is a symbol defined in a file the language's own runner would collect
    /// (see `conventions`). This is derived, not a separate index: coverage-based test
    /// impact is the L3 story (architecture 9), and when the two disagree that is a
    /// finding rather than a bug.
    pub fn tests_reaching(&self, symbol_id: i64, depth: usize, limit: usize) -> Result<Vec<SymbolRow>> {
        use std::collections::VecDeque;

        let mut seen: HashSet<i64> = HashSet::from([symbol_id]);
        let mut queue: VecDeque<(i64, usize)> = VecDeque::from([(symbol_id, 0usize)]);
        let mut out = Vec::new();

        let mut stmt = self.conn.prepare_cached(
            r#"
            SELECT e.src_symbol, coalesce(f.is_test, 0)
              FROM edges e
              JOIN symbols s ON s.id = e.src_symbol
              LEFT JOIN files f ON f.id = s.def_file_id
             WHERE e.dst_symbol = ?1 AND e.kind = 0
            "#,
        )?;

        while let Some((node, d)) = queue.pop_front() {
            if d >= depth || out.len() >= limit {
                continue;
            }
            let rows = stmt.query_map(params![node], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? != 0))
            })?;
            for row in rows {
                let (caller, is_test) = row?;
                if !seen.insert(caller) {
                    continue;
                }
                if is_test {
                    if let Some(sym) = self.symbol(caller)? {
                        out.push(sym);
                        if out.len() >= limit {
                            break;
                        }
                    }
                    // A test does not need walking further: whatever it calls is
                    // reached *by* the test, which is what we already recorded.
                    continue;
                }
                queue.push_back((caller, d + 1));
            }
        }
        Ok(out)
    }
}

impl Store {
    /// Sites whose string literals name this symbol (architecture 18.4).
    ///
    /// Returned separately from exact edges, and every caller must label them as
    /// unverified. Confirming or refuting one is cheap for an agent that has the file
    /// open, and the result can be written back so the next session does not re-ask.
    pub fn weak_sites(&self, symbol_id: i64, limit: usize) -> Result<Vec<(String, f64)>> {
        let mut stmt = self.conn.prepare_cached(
            r#"
            SELECT p.s, e.line, e.confidence
              FROM edges e
              JOIN files   f ON f.id = e.file_id
              JOIN strings p ON p.id = f.path_id
             WHERE e.dst_symbol = ?1 AND e.kind = 2
             ORDER BY f.is_test ASC, p.s ASC, e.line ASC
             LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(params![symbol_id, limit as i64], |r| {
            Ok((
                format!("{}:{}", r.get::<_, String>(0)?, r.get::<_, i64>(1)? + 1),
                r.get::<_, f64>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}
