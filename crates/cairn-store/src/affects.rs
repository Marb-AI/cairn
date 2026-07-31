//! One question, one answer: which deployed services a change to this symbol touches.
//!
//! Every part of this already existed as a separate command — `runs` for the in-process
//! side, `reaches` for one gRPC hop, `topology` for what starts a container. Measured
//! (eval/RESULTS.md, task E), assembling them by hand cost an agent 39 tool calls and a
//! third more tokens than working with no tool at all, across three rounds in which the
//! tool got steadily better and the run got steadily dearer.
//!
//! The spend was never in the queries; it was in not knowing when to stop. A per-symbol
//! command answers a fragment, and a fragment invites another question. So this returns
//! the whole shape of the answer — in-process services, then each network hop with the
//! RPC that carries it — and names what it cannot see, so the caller can tell a complete
//! answer from a partial one without going to check.
//!
//! The same lesson as `survey`: where a question is about a set, answer the set.

use crate::{Direction, EdgeKind, Store, SymbolRow};
use anyhow::Result;
use std::collections::{HashMap, HashSet};

/// A service that executes the symbol in its own process.
#[derive(Debug, Clone)]
pub struct InProcess {
    pub service: String,
    /// What the deployment starts, which is the evidence for the attribution.
    pub command: Option<String>,
}

/// One service-to-service call on the way to the symbol.
#[derive(Debug, Clone)]
pub struct Hop {
    /// Services running the calling side. Empty when the caller sits in a container that
    /// starts nothing, which is stated rather than dropped.
    pub from: Vec<String>,
    /// Services running the handler.
    pub to: Vec<String>,
    pub pkg: String,
    pub service: String,
    pub rpc: String,
    /// True when `from` had to be attributed through the call site's file rather than
    /// through a call path — a framework route handler has no static caller.
    pub from_by_file: bool,
    /// Where the call is made — a real call site, not a naming convention.
    pub call_site: SymbolRow,
    pub handler: SymbolRow,
}

#[derive(Debug, Clone, Default)]
pub struct Affects {
    pub in_process: Vec<InProcess>,
    pub hops: Vec<Hop>,
    /// Services that start nothing, so reachability can never attribute anything to them.
    pub blind: Vec<String>,
    /// Hops beyond the limit, if the chain kept going.
    pub truncated_hops: bool,
}

/// How many service-to-service hops to follow. Three covers gateway -> proxy -> service,
/// which is the deepest chain in the corpus this was measured against; the fourth is
/// there so exceeding it is visible rather than silently cut.
const MAX_HOPS: usize = 4;

impl Store {
    /// Every deployed service a change to this symbol would affect.
    pub fn affects(&self, symbol_id: i64, depth: usize, fanout: usize) -> Result<Affects> {
        let mut out = Affects {
            blind: self.services_without_entrypoint()?,
            ..Default::default()
        };

        // Attribution is a bounded breadth-first walk per symbol and this asks for it once
        // per hop candidate, so the same question is put many times. Memoised, because a
        // command that costs seconds gets used as a fallback rather than as the answer.
        let mut runs_memo: HashMap<i64, Vec<String>> = HashMap::new();
        let mut runs = |store: &Store, id: i64| -> Result<Vec<String>> {
            if let Some(hit) = runs_memo.get(&id) {
                return Ok(hit.clone());
            }
            let (svc, _) = store.services_running_attributed(id, depth)?;
            runs_memo.insert(id, svc.clone());
            Ok(svc)
        };

        for service in runs(self, symbol_id)? {
            let command = self.service_command(&service)?;
            out.in_process.push(InProcess { service, command });
        }

        // Only symbols that implement an RPC can start a hop, and there are far fewer of
        // them than there are callers of a repository function. Testing membership in a
        // set built once is what keeps this a single query per hop rather than one per
        // caller.
        let handlers = self.rpc_handler_methods()?;

        let mut frontier = vec![symbol_id];
        let mut seen_calls: HashSet<i64> = HashSet::new();
        let mut hop_count = 0usize;

        while !frontier.is_empty() {
            if hop_count >= MAX_HOPS {
                out.truncated_hops = true;
                break;
            }
            hop_count += 1;
            let mut next = Vec::new();

            for root in frontier {
                // Everything that reaches this symbol inside its own process. The hop
                // starts wherever that closure meets an RPC handler.
                let walk =
                    self.walk(root, EdgeKind::Calls, Direction::In, depth, fanout, true)?;
                for node in &walk.nodes {
                    if !handlers.contains(&node.symbol.id) {
                        continue;
                    }
                    let to = runs(self, node.symbol.id)?;
                    for caller in self.rpc_callers(node.symbol.id)? {
                        if !seen_calls.insert(caller.symbol.id) {
                            continue;
                        }
                        let mut from = runs(self, caller.symbol.id)?;
                        let mut from_by_file = false;
                        if from.is_empty() {
                            from = self.services_running_file(caller.symbol.id, depth)?;
                            from_by_file = !from.is_empty();
                        }
                        out.hops.push(Hop {
                            from,
                            from_by_file,
                            to: to.clone(),
                            pkg: caller.pkg,
                            service: caller.service,
                            rpc: caller.rpc,
                            call_site: caller.symbol.clone(),
                            handler: node.symbol.clone(),
                        });
                        next.push(caller.symbol.id);
                    }
                }
            }
            frontier = next;
        }
        Ok(out)
    }

    /// What the deployment starts for a service, as recorded by the topology pass.
    fn service_command(&self, name: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT command FROM deploy_services WHERE name = ?1")?;
        let mut rows = stmt.query([name])?;
        match rows.next()? {
            Some(r) => Ok(r.get(0)?),
            None => Ok(None),
        }
    }

    /// Methods that implement an RPC: members of a type bound to a service as a server.
    ///
    /// Membership by container rather than by line range, because scip-go emits no
    /// enclosing range for a type and span containment would drop the whole Go side.
    fn rpc_handler_methods(&self) -> Result<HashSet<i64>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT DISTINCT m.id
              FROM service_links l
              JOIN symbols t ON t.id = l.symbol_id AND t.kind = 1
              JOIN strings tn ON tn.id = t.name_id
              JOIN symbols m ON m.def_file_id = t.def_file_id AND m.id <> t.id
              JOIN strings c ON c.id = m.container_id
               AND (c.s = tn.s OR c.s LIKE '%/' || tn.s || '#')
             WHERE l.role = 0
            "#,
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        let mut out = HashSet::new();
        for r in rows {
            out.insert(r?);
        }
        Ok(out)
    }
}
