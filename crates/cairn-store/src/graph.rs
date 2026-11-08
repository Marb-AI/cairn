//! Walks over derived edges: callers, callees, implementations.
//!
//! Breadth and depth are always bounded by the caller (architecture 18.1). An unbounded
//! walk on a real codebase returns thousands of nodes, which spends exactly the tokens
//! this tool exists to save.

use crate::{EdgeKind, EdgeSource, Store, SymbolRow};
use anyhow::Result;
use rusqlite::params;
use std::collections::HashSet;

/// One step out from a symbol: where it lands, how the edge was derived, who declared
/// it, and the call site when there is one.
///
/// A struct rather than the tuple it used to be: `origin` made it a four-tuple, and
/// `(i64, EdgeSource, Option<String>, Option<String>)` at three call sites is a puzzle
/// every time it is read.
pub struct Neighbour {
    pub symbol_id: i64,
    pub source: EdgeSource,
    /// Which connector stated the edge, for a declared one; `None` otherwise.
    pub origin: Option<String>,
    /// Where the edge was observed, as `file:line`.
    pub site: Option<String>,
}

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
    /// Which connector stated the edge, for a declared one; `None` otherwise.
    pub origin: Option<String>,
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

impl Walk {
    /// Keep only the nodes in `keep` (and the root), and report how many went.
    ///
    /// The tree has to survive the filter: a node whose parent was dropped is re-pointed
    /// at its nearest surviving ancestor, and depth is recomputed from that, so what is
    /// printed is still a tree of real edges rather than an indented list with holes in
    /// it. Used by `--within`, where the question is "this walk, inside that container".
    pub fn retain_ids(&mut self, keep: &HashSet<i64>) -> usize {
        let mut surviving: Vec<usize> = Vec::new();
        // Index in the old list -> index in the new one, for the nodes that stay.
        let mut moved: Vec<Option<usize>> = vec![None; self.nodes.len()];
        for (i, node) in self.nodes.iter().enumerate() {
            // The root is the subject of the answer, not a result in it.
            if i == 0 || keep.contains(&node.symbol.id) {
                moved[i] = Some(surviving.len());
                surviving.push(i);
            }
        }
        let dropped = self.nodes.len() - surviving.len();
        let old = std::mem::take(&mut self.nodes);
        let mut kept: Vec<WalkNode> = Vec::new();
        for (new_index, old_index) in surviving.iter().enumerate() {
            let mut node = old[*old_index].clone();
            // Walk up until an ancestor survived. `None` only for the root.
            let mut parent = node.parent;
            let mut mapped = None;
            while let Some(p) = parent {
                if let Some(at) = moved[p] {
                    mapped = Some(at);
                    break;
                }
                parent = old[p].parent;
            }
            node.parent = mapped;
            node.depth = match mapped {
                Some(at) => kept[at].depth + 1,
                None if new_index == 0 => 0,
                None => 1,
            };
            kept.push(node);
        }
        self.nodes = kept;
        dropped
    }
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
            origin: None,
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
                for n in rows {
                    if !seen.insert(n.symbol_id) {
                        walk.revisited += 1;
                        continue;
                    }
                    let Some(sym) = self.symbol(n.symbol_id)? else {
                        continue;
                    };
                    walk.nodes.push(WalkNode {
                        symbol: sym,
                        depth: level,
                        parent: Some(idx),
                        site: n.site,
                        source: n.source,
                        origin: n.origin,
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

    /// The same walk, returning only the symbol ids it reached.
    ///
    /// `walk` builds a `SymbolRow` per node, and a row is three queries: the symbol, its
    /// persistent handle, and its definition site. That is the right shape for a command
    /// that prints the nodes. It is the wrong shape for one that tests them against a set
    /// and keeps a handful — `affects` was building 922 rows to look at 120 of them, which
    /// measured at 130 ms of a 190 ms command.
    ///
    /// Traversal order, the fanout cut and the revisit rule are the walk's, so the set of
    /// ids is the same set. Only `truncated` is not reported, because a caller that does
    /// not render the nodes has nothing to say about the ones that were cut.
    pub fn walk_ids(
        &self,
        root: i64,
        kind: EdgeKind,
        dir: Direction,
        depth: usize,
        fanout: usize,
        exclude_tests: bool,
    ) -> Result<Vec<i64>> {
        let mut seen: HashSet<i64> = HashSet::from([root]);
        let mut reached = vec![root];
        let mut frontier = vec![root];

        for _ in 1..=depth {
            let mut next = Vec::new();
            for parent_id in frontier {
                let (rows, _) = self.neighbours(parent_id, kind, dir, fanout, exclude_tests)?;
                for n in rows {
                    if !seen.insert(n.symbol_id) {
                        continue;
                    }
                    reached.push(n.symbol_id);
                    next.push(n.symbol_id);
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        Ok(reached)
    }

    /// Direct neighbours plus the total, so truncation can be reported.
    ///
    /// One row per neighbour, so a caller that references the same symbol several times
    /// gets one line and one site. Which site used to be whichever row the grouping
    /// happened to keep — undefined, and in practice the first. For a Python function
    /// that imports a name locally and then calls it, the first reference is the `import`
    /// statement, so cairn reported the import line as the place the call happens.
    /// Reported from an audit, where that line was copied into a fix for a document whose
    /// bug was a wrong line reference.
    ///
    /// `MAX` makes the choice defined instead of incidental, and picks the last reference
    /// rather than the first: a local import always precedes the use it exists for, so the
    /// last one is the call. Where a symbol is genuinely called more than once, any of them
    /// is a true call site and the last is as good as the first.
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
    ) -> Result<(Vec<Neighbour>, i64)> {
        let (from_col, to_col) = match dir {
            Direction::In => ("dst_symbol", "src_symbol"),
            Direction::Out => ("src_symbol", "dst_symbol"),
        };
        // A `calls` edge is really "a reference inside this body", which for parameters,
        // type variables and locals is not a call by any reading. Walking *inwards* that
        // barely shows, because the answer is aggregated onto the enclosing function.
        // Walking outwards it is ruinous: a forward trace from a gRPC handler returned 68
        // rows of which three were calls — the rest were parameters, protobuf message
        // types and stdlib names with no definition in this repository. An agent asked to
        // trace an entry point to its database write read that list and then went symbol
        // by symbol, which is where task F's 34 tool calls went (the measurement record).
        //
        // Only applied outwards, and only to call edges: `impls` genuinely wants types,
        // and the inward direction is not polluted.
        let outward_calls = dir == Direction::Out && kind == EdgeKind::Calls;
        let noise_filter = if outward_calls {
            // 4 TypeParameter, 5 Parameter, 6 Meta, 7 Local; and anything with no
            // definition here, which is a stdlib or third-party name we cannot follow.
            // Generated code as well: reading `message.user_id` off a protobuf request is
            // a field access, not a call, and the generated message types crowded out the
            // real callees once the parameters were gone. Behaviour does not live there.
            "AND other.kind NOT IN (4, 5, 6, 7) AND other.def_file_id IS NOT NULL \
             AND coalesce(otherf.generated, 0) = 0"
        } else {
            ""
        };
        let rank = crate::SOURCE_RANK_SQL;
        // Declared dependency edges travel with calls, and only with calls. A connector
        // that says an rpc is implemented by a handler is stating the same kind of thing
        // a call edge states - a change to the target reaches the source - so a walk
        // that followed one and not the other would answer "nothing calls this" about a
        // handler the whole system reaches through a contract. `impls` and `member` are
        // about types and membership, where a declared dependency has no business, and
        // containment never propagates at all.
        let declared = if kind == EdgeKind::Calls {
            format!(
                " OR (e.kind = {} AND e.declared_kind IN
                        (SELECT id FROM edge_kinds WHERE propagates = 0))",
                EdgeKind::Declared as i64
            )
        } else {
            String::new()
        };
        let sql = format!(
            r#"
            SELECT e.{to_col}, e.source, p.s, MAX(e.line), e.origin
              FROM edges e
              LEFT JOIN files   f ON f.id = e.file_id
              LEFT JOIN strings p ON p.id = f.path_id
              JOIN symbols other ON other.id = e.{to_col}
              LEFT JOIN files otherf ON otherf.id = other.def_file_id
             WHERE e.{from_col} = ?1 AND (e.kind = ?2{declared})
               AND (?4 = 0 OR coalesce(otherf.is_test, 0) = 0)
               {noise_filter}
             GROUP BY e.{to_col}
             ORDER BY {rank} ASC, e.confidence DESC
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
                Ok(Neighbour {
                    symbol_id: r.get::<_, i64>(0)?,
                    source: EdgeSource::from_i64(r.get::<_, i64>(1)?),
                    origin: r.get::<_, Option<String>>(4)?,
                    site,
                })
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
              WHERE e.{from_col} = ?1 AND (e.kind = ?2{declared})
                AND (?3 = 0 OR coalesce(otherf.is_test, 0) = 0)
                {noise_filter}"
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
    pub fn call_path(&self, from: i64, to: i64, max_depth: usize) -> Result<Option<Vec<PathHop>>> {
        use std::collections::{HashMap, VecDeque};

        if from == to {
            let Some(sym) = self.symbol(from)? else {
                return Ok(None);
            };
            return Ok(Some(vec![PathHop {
                symbol: sym,
                site: None,
            }]));
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
            let Some(sym) = self.symbol(id)? else {
                continue;
            };
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
    pub fn tests_reaching(
        &self,
        symbol_id: i64,
        depth: usize,
        limit: usize,
    ) -> Result<Vec<SymbolRow>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EdgeKind;
    use rusqlite::params;

    /// A chain a -> b -> c and a second caller d of b, so the walk has a level to widen on
    /// and a node reachable two ways.
    fn call_chain(store: &Store) -> Vec<i64> {
        let c = &store.conn;
        let sid = |s: &str| -> i64 {
            c.execute("INSERT OR IGNORE INTO strings(s) VALUES (?1)", params![s])
                .unwrap();
            c.query_row("SELECT id FROM strings WHERE s = ?1", params![s], |r| {
                r.get(0)
            })
            .unwrap()
        };
        c.execute(
            "INSERT INTO files(path_id, lang, is_test, generated) VALUES (?1, 1, 0, 0)",
            params![sid("w.py")],
        )
        .unwrap();
        let file = c.last_insert_rowid();
        let add = |name: &str, hash: u8| -> i64 {
            c.execute(
                "INSERT INTO symbols(hash, name_id, kind, lang, ref_count, def_file_id)
                 VALUES (?1, ?2, 2, 1, 0, ?3)",
                params![vec![hash; 16], sid(name), file],
            )
            .unwrap();
            c.last_insert_rowid()
        };
        let ids: Vec<i64> = ["a", "b", "c", "d"]
            .iter()
            .enumerate()
            .map(|(i, n)| add(n, i as u8 + 1))
            .collect();
        // Calls edges: a->b, b->c, d->b. Walking *in* from c finds b, then a and d.
        for (src, dst) in [(ids[0], ids[1]), (ids[1], ids[2]), (ids[3], ids[1])] {
            c.execute(
                "INSERT INTO edges(src_symbol, dst_symbol, kind, file_id, line, confidence,
                                   source)
                 VALUES (?1, ?2, 0, ?3, 1, 1.0, 0)",
                params![src, dst, file],
            )
            .unwrap();
        }
        ids
    }

    #[test]
    fn the_id_walk_reaches_exactly_what_the_row_walk_reaches() {
        // The whole justification for the second walk is that it costs less for the same
        // set. If the two ever disagree, `affects` silently answers about a different
        // graph than every command that renders one - so the equivalence is the test, at
        // every depth including the one that stops early.
        let store = Store::open_in_memory().unwrap();
        let ids = call_chain(&store);
        for depth in 0..=3 {
            let rows = store
                .walk(ids[2], EdgeKind::Calls, Direction::In, depth, 50, true)
                .unwrap();
            let from_rows: Vec<i64> = rows.nodes.iter().map(|n| n.symbol.id).collect();
            let from_ids = store
                .walk_ids(ids[2], EdgeKind::Calls, Direction::In, depth, 50, true)
                .unwrap();
            assert_eq!(from_rows, from_ids, "depth {depth}");
        }
    }

    #[test]
    fn the_fanout_cut_applies_to_both_walks_alike() {
        // A limit that drops a caller has to drop it from both, or the cheaper walk
        // answers a question the expensive one would have refused.
        let store = Store::open_in_memory().unwrap();
        let ids = call_chain(&store);
        let rows = store
            .walk(ids[1], EdgeKind::Calls, Direction::In, 2, 1, true)
            .unwrap();
        let from_rows: Vec<i64> = rows.nodes.iter().map(|n| n.symbol.id).collect();
        let from_ids = store
            .walk_ids(ids[1], EdgeKind::Calls, Direction::In, 2, 1, true)
            .unwrap();
        assert_eq!(from_rows, from_ids);
        assert_eq!(from_ids.len(), 2, "the root and one of its two callers");
    }
}
