//! Storing what a connector declared: nodes, edge kinds, and edges into code.
//!
//! A declared node is a row in `symbols`. That is the whole trick, and it is what makes
//! `cairn graph w17:rpc/orders.OrderService.Create` work without a second graph: every
//! walk, every handle, every renderer already deals in symbols, so a node that is one
//! needs none of them changed. `declared_nodes` carries what a symbol row cannot — the
//! connector that stated it and the kind it gave it, which the core stores and never
//! interprets.
//!
//! Everything here is rebuilt on each run of the connector that owns it. A node the
//! connector stopped emitting has to disappear rather than linger as a fact nobody
//! states any more, which is why the first thing a run does is drop its own namespace.

use super::{Manifest, NodeFact, Propagates};
use crate::{EdgeKind, EdgeSource, Store};
use anyhow::Result;
use rusqlite::params;

/// A declared node, as commands see it.
#[derive(Debug, Clone)]
pub struct DeclaredNode {
    pub symbol_id: i64,
    pub connector: String,
    pub kind: String,
    /// The id the connector gave it, which is also how a caller names it.
    pub node_id: String,
}

impl Propagates {
    pub fn as_i64(self) -> i64 {
        match self {
            Propagates::Dependency => 0,
            Propagates::Containment => 1,
            Propagates::None => 2,
        }
    }

    pub fn from_i64(v: i64) -> Propagates {
        match v {
            0 => Propagates::Dependency,
            1 => Propagates::Containment,
            _ => Propagates::None,
        }
    }
}

impl Store {
    /// Everything one connector declared, gone: its nodes, their symbol rows, its edges
    /// and its edge kinds.
    pub fn drop_declared(&mut self, connector: &str) -> Result<()> {
        // The ids first: once `declared_nodes` is gone there is nothing left that says
        // which symbol rows belonged to this connector.
        let ids: Vec<i64> = {
            let mut stmt = self
                .conn
                .prepare("SELECT symbol_id FROM declared_nodes WHERE connector = ?1")?;
            let rows = stmt.query_map(params![connector], |r| r.get::<_, i64>(0))?;
            rows.collect::<std::result::Result<Vec<i64>, _>>()?
        };
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM edges WHERE kind = ?1 AND origin = ?2",
            params![EdgeKind::Declared as i64, connector],
        )?;
        // Before the symbols, not after: `declared_nodes.symbol_id` references them, and
        // deleting the other way round fails the foreign key.
        tx.execute(
            "DELETE FROM declared_nodes WHERE connector = ?1",
            params![connector],
        )?;
        for id in &ids {
            // A declared node's symbol row exists only for that node, so it goes with
            // it. A *code* symbol a connector referenced is not ours to delete.
            tx.execute("DELETE FROM handles WHERE symbol_id = ?1", params![id])?;
            tx.execute("DELETE FROM symbols WHERE id = ?1", params![id])?;
        }
        tx.execute(
            "DELETE FROM edge_kinds WHERE connector = ?1",
            params![connector],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Store one declared node, returning the symbol row that now represents it.
    pub fn declare_node(
        &mut self,
        manifest: &Manifest,
        node: &NodeFact,
        repo: Option<&std::path::Path>,
    ) -> Result<i64> {
        // The id is hashed the way a SCIP symbol string is, so a node has a stable
        // identity and a stable handle across rebuilds for exactly as long as the
        // connector keeps calling it the same thing.
        let hash = crate::symbol_hash(&node.id);
        // `w17:domain/orders` -> name `domain/orders`. The kind is in the id by the
        // protocol's own convention, and keeping it in the displayed name is what makes
        // a result line readable without a column the rest of the index does not have.
        let local = node
            .id
            .split_once(':')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_else(|| node.id.clone());
        let display = node.name.clone().unwrap_or_else(|| local.clone());
        let name_id = self.intern(&display)?;
        let module_id = self.intern(&manifest.name)?;

        // An anchor is a definition: `proto/orders.proto:14` is where the declaration
        // lives, and an answer that can point at it is worth far more than one that says
        // `<no definition indexed>`.
        let (file_id, line) = match node.anchor.as_deref().and_then(parse_anchor) {
            Some((path, line)) => (self.anchor_file(&path, repo)?, Some(line)),
            None => (None, None),
        };

        self.conn.execute(
            "INSERT INTO symbols(hash, scip, name_id, module_id, kind, lang, ref_count,
                                 def_file_id, def_line)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, ?6, ?7)
             ON CONFLICT(hash) DO UPDATE SET scip = excluded.scip",
            params![
                &hash[..],
                node.id,
                name_id,
                module_id,
                cairn_scip::SymbolKind::Unknown as i64,
                file_id,
                line,
            ],
        )?;
        let symbol_id: i64 = self.conn.query_row(
            "SELECT id FROM symbols WHERE hash = ?1",
            params![&hash[..]],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO declared_nodes(symbol_id, connector, kind, node_id)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(symbol_id) DO UPDATE SET kind = excluded.kind",
            params![symbol_id, manifest.name, node.kind, node.id],
        )?;
        Ok(symbol_id)
    }

    /// The file a declared anchor points at, created if the index has never seen it.
    ///
    /// A `.proto` file is not in any SCIP index, so the file an rpc is declared in is
    /// usually unknown to the store — and without a row there is nothing for the node's
    /// definition to point at, which would cost every declared node its location. The
    /// content hash is recorded when the tree is at hand, so `cairn verify` can tell
    /// whether the declaration has changed since; without it the file is counted as one
    /// whose staleness cannot be checked, which is exactly what it is.
    fn anchor_file(&self, path: &str, repo: Option<&std::path::Path>) -> Result<Option<i64>> {
        if let Some(id) = self.file_id_for_path(path)? {
            return Ok(Some(id));
        }
        let path_id = self.intern(path)?;
        let hash = repo
            .map(|root| root.join(path))
            .and_then(|full| std::fs::read(full).ok())
            .map(|bytes| blake3::hash(&bytes).as_bytes()[..16].to_vec());
        self.conn.execute(
            "INSERT INTO files(path_id, lang, generated, gen_via, is_test, content_hash)
             VALUES (?1, ?2, 0, 0, 0, ?3)
             ON CONFLICT(path_id) DO UPDATE SET content_hash = excluded.content_hash",
            params![path_id, crate::Lang::of_path(path) as i64, hash],
        )?;
        self.file_id_for_path(path)
    }

    /// The id of a declared edge kind, created on first use.
    pub fn declared_edge_kind(
        &mut self,
        connector: &str,
        name: &str,
        propagates: Propagates,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO edge_kinds(connector, name, propagates) VALUES (?1, ?2, ?3)
             ON CONFLICT(connector, name) DO UPDATE SET propagates = excluded.propagates",
            params![connector, name, propagates.as_i64()],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM edge_kinds WHERE connector = ?1 AND name = ?2",
            params![connector, name],
            |r| r.get(0),
        )?)
    }

    pub fn insert_declared_edge(
        &mut self,
        from: i64,
        to: i64,
        kind_id: i64,
        connector: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO edges(src_symbol, dst_symbol, kind, source, confidence, origin,
                               declared_kind)
             VALUES (?1, ?2, ?3, ?4, 1.0, ?5, ?6)",
            params![
                from,
                to,
                EdgeKind::Declared as i64,
                EdgeSource::Declared as i64,
                connector,
                kind_id
            ],
        )?;
        Ok(())
    }

    /// The symbol a `scip:` reference names, and whether the index already had it.
    ///
    /// A reference the index does not have is kept as *expected* rather than resolved
    /// (architecture 7.2): the row is created with no definition, which is exactly how a
    /// third-party reference from SCIP is already represented, and the caller reports the
    /// gap. The alternative — dropping the edge — would make a contract that exists look
    /// like one nobody implements.
    pub fn symbol_for_scip(&mut self, scip: &str) -> Result<(i64, bool)> {
        let hash = crate::symbol_hash(scip);
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM symbols WHERE hash = ?1",
                params![&hash[..]],
                |r| r.get(0),
            )
            .ok();
        if let Some(id) = existing {
            return Ok((id, true));
        }
        // Parsed the way the ingest parses one, so an expected symbol prints with the
        // same name a resolved one would.
        let parsed = cairn_scip::ParsedSymbol::parse(scip);
        let name = parsed
            .as_ref()
            .map(|p| p.display_name().to_string())
            .unwrap_or_else(|| scip.to_string());
        let name_id = self.intern(&name)?;
        self.conn.execute(
            "INSERT INTO symbols(hash, scip, name_id, kind, lang, ref_count)
             VALUES (?1, ?2, ?3, ?4, 0, 0)",
            params![
                &hash[..],
                scip,
                name_id,
                parsed
                    .as_ref()
                    .map(|p| p.kind() as i64)
                    .unwrap_or(cairn_scip::SymbolKind::Unknown as i64),
            ],
        )?;
        Ok((self.conn.last_insert_rowid(), false))
    }

    /// The declared node a caller named, if any.
    ///
    /// `cairn affects w17:domain/orders` resolves here. A colon is what distinguishes
    /// one from a handle or a symbol name, and no handle or SCIP display name contains
    /// one.
    pub fn declared_node(&self, node_id: &str) -> Result<Option<DeclaredNode>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT symbol_id, connector, kind, node_id FROM declared_nodes WHERE node_id = ?1",
        )?;
        let mut rows = stmt.query(params![node_id])?;
        Ok(match rows.next()? {
            Some(r) => Some(DeclaredNode {
                symbol_id: r.get(0)?,
                connector: r.get(1)?,
                kind: r.get(2)?,
                node_id: r.get(3)?,
            }),
            None => None,
        })
    }

    /// The declared node a symbol *is*, if it is one.
    pub fn declared_node_of(&self, symbol_id: i64) -> Result<Option<DeclaredNode>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT symbol_id, connector, kind, node_id FROM declared_nodes WHERE symbol_id = ?1",
        )?;
        let mut rows = stmt.query(params![symbol_id])?;
        Ok(match rows.next()? {
            Some(r) => Some(DeclaredNode {
                symbol_id: r.get(0)?,
                connector: r.get(1)?,
                kind: r.get(2)?,
                node_id: r.get(3)?,
            }),
            None => None,
        })
    }

    /// Symbols contained in a declared node, through containment edges.
    ///
    /// Containment is not traversed for impact — a change to one member does not reach
    /// its siblings — so it has its own query rather than being folded into the walks.
    /// This is what `--within` filters on.
    pub fn contained_by(&self, container: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.src_symbol FROM edges e
               JOIN edge_kinds k ON k.id = e.declared_kind
              WHERE e.dst_symbol = ?1 AND e.kind = ?2 AND k.propagates = 1
                AND e.src_symbol IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![container, EdgeKind::Declared as i64], |r| {
            r.get::<_, i64>(0)
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Everything a symbol is contained in, with the kind of each container.
    ///
    /// For grouping an answer: a row's container is what `--within` scopes to, and what
    /// a grouped answer puts a heading on.
    pub fn containers_of(&self, symbol_id: i64) -> Result<Vec<DeclaredNode>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT d.symbol_id, d.connector, d.kind, d.node_id
               FROM edges e
               JOIN edge_kinds k ON k.id = e.declared_kind
               JOIN declared_nodes d ON d.symbol_id = e.dst_symbol
              WHERE e.src_symbol = ?1 AND e.kind = ?2 AND k.propagates = 1
              ORDER BY d.node_id",
        )?;
        let rows = stmt.query_map(params![symbol_id, EdgeKind::Declared as i64], |r| {
            Ok(DeclaredNode {
                symbol_id: r.get(0)?,
                connector: r.get(1)?,
                kind: r.get(2)?,
                node_id: r.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Declared edges touching a symbol, as annotations: the kind, and the other end.
    ///
    /// What `cairn flow` and the symbol-level answers show next to a call: "writes
    /// w17:entity/Order (declared by w17)". Both directions, because a handler *emits*
    /// and an entity *is written by*, and which side the caller is on decides which
    /// reads naturally.
    pub fn declared_annotations(&self, symbol_id: i64) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT k.name, coalesce(d.node_id, s.scip, ''), e.origin
               FROM edges e
               JOIN edge_kinds k ON k.id = e.declared_kind
               JOIN symbols s ON s.id = e.dst_symbol
               LEFT JOIN declared_nodes d ON d.symbol_id = e.dst_symbol
              WHERE e.src_symbol = ?1 AND e.kind = ?2
              ORDER BY k.name, 2",
        )?;
        let rows = stmt.query_map(params![symbol_id, EdgeKind::Declared as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// How many declared facts this index holds, for `verify`.
    pub fn declared_counts(&self) -> Result<(i64, i64)> {
        let nodes: i64 = self
            .conn
            .query_row("SELECT count(*) FROM declared_nodes", [], |r| r.get(0))?;
        let edges: i64 = self.conn.query_row(
            "SELECT count(*) FROM edges WHERE kind = ?1",
            params![EdgeKind::Declared as i64],
            |r| r.get(0),
        )?;
        Ok((nodes, edges))
    }
}

/// `proto/orders.proto:14` -> the path and the 0-based line the store keeps.
fn parse_anchor(anchor: &str) -> Option<(String, i64)> {
    let (path, line) = anchor.rsplit_once(':')?;
    let line: i64 = line.trim().parse().ok()?;
    if path.is_empty() || line < 1 {
        return None;
    }
    Some((path.to_string(), line - 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connect::{EdgeFact, Facts};

    fn manifest() -> Manifest {
        let mut edge_kinds = std::collections::BTreeMap::new();
        edge_kinds.insert("implemented_by".to_string(), Propagates::Dependency);
        edge_kinds.insert("part_of".to_string(), Propagates::Containment);
        Manifest {
            name: "w17".to_string(),
            version: None,
            inputs: vec![],
            tier: super::super::Tier::Declared,
            node_kinds: vec!["domain".to_string(), "rpc".to_string()],
            edge_kinds,
            command: vec![],
            timeout_s: 5,
            digest: vec![],
        }
    }

    fn facts() -> Facts {
        Facts {
            nodes: vec![
                NodeFact {
                    id: "w17:domain/orders".to_string(),
                    kind: "domain".to_string(),
                    name: Some("orders".to_string()),
                    anchor: None,
                },
                NodeFact {
                    id: "w17:rpc/Create".to_string(),
                    kind: "rpc".to_string(),
                    name: None,
                    anchor: Some("proto/orders.proto:14".to_string()),
                },
            ],
            edges: vec![
                EdgeFact {
                    from: "w17:rpc/Create".to_string(),
                    to: "w17:domain/orders".to_string(),
                    kind: "part_of".to_string(),
                },
                EdgeFact {
                    from: "w17:rpc/Create".to_string(),
                    to: "scip:scip-go gomod x . `x`/Handler#Create().".to_string(),
                    kind: "implemented_by".to_string(),
                },
            ],
            unknown: vec![],
            rejected: vec![],
        }
    }

    #[test]
    fn declared_facts_land_as_symbols_edges_and_kinds() {
        let mut store = Store::open_in_memory().unwrap();
        let applied = store
            .apply_declared_facts(&manifest(), &facts(), None)
            .unwrap();
        assert_eq!(applied.nodes, 2);
        assert_eq!(applied.edges, 2);
        // The code reference was not in the index, so it is expected rather than
        // resolved - and said so.
        assert_eq!(applied.unknown.len(), 1, "{:#?}", applied.unknown);
        assert!(applied.unknown[0].contains("expected rather than resolved"));

        let node = store.declared_node("w17:rpc/Create").unwrap().unwrap();
        assert_eq!(node.kind, "rpc");
        let row = store.symbol(node.symbol_id).unwrap().unwrap();
        assert_eq!(row.name, "rpc/Create");
        // The anchor is a definition, so an answer can point at the declaration.
        assert_eq!(
            row.def.as_ref().map(|d| d.location()),
            Some("proto/orders.proto:14".to_string())
        );
        assert!(!row.handle.is_empty(), "a declared node is addressable");

        // Containment, which groups rather than propagating.
        let contained = store
            .contained_by(
                store
                    .declared_node("w17:domain/orders")
                    .unwrap()
                    .unwrap()
                    .symbol_id,
            )
            .unwrap();
        assert_eq!(contained, vec![node.symbol_id]);
        let containers = store.containers_of(node.symbol_id).unwrap();
        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0].node_id, "w17:domain/orders");
    }

    #[test]
    fn a_rerun_replaces_the_namespace_rather_than_adding_to_it() {
        // The property that keeps declared nodes from drifting from their source: what
        // the connector no longer states stops being a fact.
        let mut store = Store::open_in_memory().unwrap();
        store
            .apply_declared_facts(&manifest(), &facts(), None)
            .unwrap();
        let mut second = facts();
        second.nodes.remove(1);
        second.edges.clear();
        let applied = store
            .apply_declared_facts(&manifest(), &second, None)
            .unwrap();
        assert_eq!(applied.nodes, 1);
        assert_eq!(applied.edges, 0);
        assert!(store.declared_node("w17:rpc/Create").unwrap().is_none());
        assert_eq!(store.declared_counts().unwrap(), (1, 0));
    }

    #[test]
    fn an_edge_naming_a_node_nobody_declared_is_dropped_and_reported() {
        let mut store = Store::open_in_memory().unwrap();
        let mut f = facts();
        f.nodes.clear();
        f.edges.truncate(1);
        let applied = store.apply_declared_facts(&manifest(), &f, None).unwrap();
        assert_eq!(applied.edges, 0);
        assert!(
            applied.unknown.iter().any(|u| u.contains("never declared")),
            "{:#?}",
            applied.unknown
        );
    }
}
