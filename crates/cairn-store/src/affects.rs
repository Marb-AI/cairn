//! One question, one answer: which deployed services a change to this symbol touches.
//!
//! Every part of this already existed as a separate command — `runs` for the in-process
//! side, `reaches` for one gRPC hop, `topology` for what starts a container. Measured
//! (the measurement record, task E), assembling them by hand cost an agent 39 tool calls and a
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
use rusqlite::params;
use std::collections::{HashMap, HashSet};

/// A service that executes the symbol in its own process.
#[derive(Debug, Clone)]
pub struct InProcess {
    pub service: String,
    /// What the deployment starts, which is the evidence for the attribution.
    pub command: Option<String>,
    /// True when the service was attributed through the file the symbol sits in rather
    /// than through a call path — the same weaker claim a hop's `from` side already
    /// makes, and marked the same way.
    pub by_file: bool,
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

/// A service this code calls out to over the network.
#[derive(Debug, Clone)]
pub struct Outgoing {
    pub pkg: String,
    pub service: String,
    /// Deployed services that serve it, where the topology can name them.
    pub served_by: Vec<String>,
    /// The symbol on this side that holds the client.
    pub via: SymbolRow,
}

#[derive(Debug, Clone, Default)]
pub struct Affects {
    pub in_process: Vec<InProcess>,
    pub hops: Vec<Hop>,
    /// What this code calls over the network. A change here changes what those services
    /// receive, which is part of a blast radius even though their own code is untouched.
    pub outgoing: Vec<Outgoing>,
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
        // One walk per service, then set membership — not one walk per symbol. The
        // difference stopped mattering when membership edges made the walks large enough
        // that a measured run gave up waiting (the measurement record, task E).
        let sets = self.reachable_by_service(depth)?;
        let mut runs_memo: HashMap<i64, Vec<String>> = HashMap::new();
        let mut runs = |store: &Store, id: i64| -> Result<Vec<String>> {
            if let Some(hit) = runs_memo.get(&id) {
                return Ok(hit.clone());
            }
            let mut found: Vec<String> = sets
                .iter()
                .filter(|(_, reach)| reach.contains(&id))
                .map(|(n, _)| n.clone())
                .collect();
            // A dispatched method with nothing reaching it directly is attributed to the
            // type that owns it, exactly as `runs` does.
            if found.is_empty() {
                if let Some(owner) = store.enclosing_type(id)? {
                    found = sets
                        .iter()
                        .filter(|(_, reach)| reach.contains(&owner.id))
                        .map(|(n, _)| n.clone())
                        .collect();
                }
            }
            runs_memo.insert(id, found.clone());
            Ok(found)
        };

        // Measured (scenario 4): asked about a FastAPI route handler, this printed
        // "affects 0 deployed service(s) — (no service entrypoint reaches it)" about a
        // live public endpoint. A framework registers its routes by decorator, so the
        // handler has no static caller and never will; reading that as "no service" is
        // reporting dead code. The hop side already solved this by attributing through
        // the file and marking it `~`, and the same weaker claim is worth far more here
        // than a confident zero: the agent that got the zero rebuilt the whole chain by
        // hand, which cost more round trips than the question was worth.
        let mut direct = runs(self, symbol_id)?;
        let by_file = direct.is_empty();
        if by_file {
            direct = self.services_running_file_in(symbol_id, &sets)?;
        }
        for service in direct {
            let command = self.service_command(&service)?;
            out.in_process.push(InProcess {
                service,
                command,
                by_file,
            });
        }

        // What this code calls out to. Measured (task K): every baseline arm named the
        // downstream prompt service as affected - a change to the request this function
        // builds changes what that service receives - and every cairn arm missed it,
        // because `affects` only ever looked at who reaches the symbol, never at what the
        // symbol reaches.
        out.outgoing = self.outgoing_services(symbol_id, &sets)?;

        // Only symbols that implement an RPC can start a hop, and there are far fewer of
        // them than there are callers of a repository function. Testing membership in a
        // set built once is what keeps this a single query per hop rather than one per
        // caller.
        let handlers = self.rpc_handler_methods()?;

        // A handler class is asked about at least as often as one of its methods - it is
        // the name in the file, and the name an outline hands back. The walk below goes
        // inward through callers, and a type's own methods are not among its callers, so
        // asking about the class found no RPC handler to start a hop from: the class
        // reported one in-process service where every one of its methods reported three,
        // and nothing in the envelope said the methods had not been looked at. `reaches`
        // already answers a class for all of its RPCs at once; this is the same fix on
        // the command whose whole purpose is to be the complete answer.
        let mut frontier = vec![symbol_id];
        frontier.extend(self.own_handler_members(symbol_id, &handlers)?);
        let mut seen_calls: HashSet<i64> = HashSet::new();
        let mut walked: HashSet<i64> = HashSet::new();
        let mut hop_count = 0usize;

        while !frontier.is_empty() {
            if hop_count >= MAX_HOPS {
                out.truncated_hops = true;
                break;
            }
            hop_count += 1;
            let mut next = Vec::new();

            for root in frontier {
                // Walking a root twice yields the same nodes. Without this, hop N+1's
                // frontier re-walked symbols hop N had already covered, and on a symbol
                // with thirty RPC routes into it that repetition was most of the runtime.
                if !walked.insert(root) {
                    continue;
                }
                // Everything that reaches this symbol inside its own process. The hop
                // starts wherever that closure meets an RPC handler.
                let walk = self.walk(root, EdgeKind::Calls, Direction::In, depth, fanout, true)?;
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
                            // Against the precomputed sets, not a fresh walk per symbol in
                            // the file. This loop calls it for up to forty symbols and the
                            // old form rebuilt every service's reachability each time —
                            // twenty-seven of the twenty-nine seconds `affects` took on a
                            // hot symbol were here, and profiling found it only after three
                            // wrong guesses.
                            from = self.services_running_file_in(caller.symbol.id, &sets)?;
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

    /// Services this symbol calls over the network, and who serves them.
    fn outgoing_services(
        &self,
        symbol_id: i64,
        sets: &[(String, std::collections::HashSet<i64>)],
    ) -> Result<Vec<Outgoing>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT DISTINCT ps.pkg, ps.name, mine.symbol_id
              FROM service_links mine
              JOIN proto_services ps ON ps.id = mine.service_id
             WHERE mine.symbol_id = ?1 AND mine.role = 1
            "#,
        )?;
        let rows = stmt.query_map(params![symbol_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (pkg, service, via_id) = row?;
            let Some(via) = self.symbol(via_id)? else {
                continue;
            };
            // Who serves it: the non-generated implementors, then which deployment runs
            // them. Named where the topology can say, left empty rather than guessed.
            let mut servers = self.conn.prepare(
                r#"
                SELECT DISTINCT l.symbol_id
                  FROM service_links l
                  JOIN proto_services ps ON ps.id = l.service_id
                  JOIN symbols s ON s.id = l.symbol_id
                  JOIN files f ON f.id = s.def_file_id AND f.generated = 0
                 WHERE ps.pkg = ?1 AND ps.name = ?2 AND l.role = 0
                "#,
            )?;
            let ids = servers.query_map(params![pkg, service], |r| r.get::<_, i64>(0))?;
            // Membership in the precomputed sets, not a fresh walk per server: this loop
            // ran `services_running` once per implementor, and once those walks grew it
            // turned a four-second command into a five-minute one.
            let mut served_by: Vec<String> = Vec::new();
            for id in ids {
                let id = id?;
                for (name, reach) in sets {
                    if reach.contains(&id) && !served_by.contains(name) {
                        served_by.push(name.clone());
                    }
                }
            }
            out.push(Outgoing {
                pkg,
                service,
                served_by,
                via,
            });
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

    /// The members of a type that is bound to a service as a server.
    ///
    /// Members by container rather than by line range, for the reason
    /// `rpc_handler_methods` gives: scip-go emits no enclosing range for a type, so span
    /// containment drops the whole Go side.
    ///
    /// Intersected with the handler set to answer "is this a handler class at all" —
    /// which is what the set really says, since it holds every member of every served
    /// type and not only the ones implementing an RPC. An ordinary class does not expand
    /// into its methods here; only a handler does, which is the granularity `reaches`
    /// settled on. Helpers on a handler come along and cost nothing: `rpc_callers` finds
    /// no RPC of that name and the walk yields nothing for them.
    fn own_handler_members(&self, type_id: i64, handlers: &HashSet<i64>) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT m.id
              FROM symbols t
              JOIN symbols m ON m.def_file_id = t.def_file_id AND m.id <> t.id
               AND m.container_leaf_id = t.name_id
             WHERE t.id = ?1 AND t.kind = 1
            "#,
        )?;
        let rows = stmt.query_map(params![type_id], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            let id = r?;
            if handlers.contains(&id) {
                out.push(id);
            }
        }
        Ok(out)
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
               AND m.container_leaf_id = t.name_id
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

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    /// Build a handler class with three members: two RPC methods and a helper.
    ///
    /// Membership is by `container_leaf_id` and the same file, which is what the queries
    /// key on — a Go type carries no enclosing range, so nothing here can rely on one.
    fn handler_with_members(store: &Store) -> (i64, Vec<i64>, i64) {
        let c = &store.conn;
        c.execute(
            "INSERT INTO strings(s) VALUES ('h.py'),('FolderHandler'),('get_folder'),
                                           ('list_folders'),('_page'),('folder')",
            [],
        )
        .unwrap();
        let sid = |s: &str| -> i64 {
            c.query_row("SELECT id FROM strings WHERE s = ?1", params![s], |r| {
                r.get(0)
            })
            .unwrap()
        };
        c.execute(
            "INSERT INTO files(path_id, lang, generated) VALUES (?1, 1, 0)",
            params![sid("h.py")],
        )
        .unwrap();
        let file: i64 = c.last_insert_rowid();

        let add = |name: &str, kind: i64, container: Option<i64>, hash: u8| -> i64 {
            c.execute(
                "INSERT INTO symbols(hash, name_id, kind, lang, ref_count, def_file_id,
                                     container_leaf_id)
                 VALUES (?1, ?2, ?3, 1, 0, ?4, ?5)",
                params![vec![hash; 16], sid(name), kind, file, container],
            )
            .unwrap();
            c.last_insert_rowid()
        };
        let ty = add("FolderHandler", 1, None, 1);
        let name = sid("FolderHandler");
        let methods = vec![
            add("get_folder", 3, Some(name), 2),
            add("list_folders", 3, Some(name), 3),
        ];
        // A helper on the same class. It is a member and it is not an RPC.
        let helper = add("_page", 3, Some(name), 4);

        c.execute(
            "INSERT INTO proto_services(pkg, name) VALUES ('folder', 'FolderService')",
            [],
        )
        .unwrap();
        let svc = c.last_insert_rowid();
        c.execute(
            "INSERT INTO service_links(service_id, symbol_id, role) VALUES (?1, ?2, 0)",
            params![svc, ty],
        )
        .unwrap();
        (ty, methods, helper)
    }

    #[test]
    fn a_handler_class_carries_the_rpcs_of_its_methods() {
        // `affects` walks inward through callers, and a type's own methods are not among
        // its callers. Asked about the class, it found no RPC handler to start a hop
        // from and reported the in-process service alone — three services' worth of blast
        // radius missing, with nothing in the envelope admitting it. The class has to
        // answer as the union of its RPCs, which is what `reaches` already does.
        let store = Store::open_in_memory().unwrap();
        let (ty, methods, _) = handler_with_members(&store);
        let handlers = store.rpc_handler_methods().unwrap();

        let seeded = store.own_handler_members(ty, &handlers).unwrap();
        for m in &methods {
            assert!(seeded.contains(m), "every RPC method seeds the hop walk");
        }
    }

    #[test]
    fn an_ordinary_class_does_not_expand_into_its_methods() {
        // The intersection with the handler set is what keeps this to handler classes.
        // Without it every type would seed its members, which is a different command's
        // question and a much larger walk.
        let store = Store::open_in_memory().unwrap();
        let (ty, _, _) = handler_with_members(&store);
        // Same shape, no service binding: a class that serves nothing.
        store
            .conn
            .execute("DELETE FROM service_links WHERE symbol_id = ?1", [ty])
            .unwrap();
        let handlers = store.rpc_handler_methods().unwrap();
        assert!(handlers.is_empty(), "nothing is served, so nothing is a handler");
        assert!(store.own_handler_members(ty, &handlers).unwrap().is_empty());
    }

    #[test]
    fn a_method_seeds_nothing_further() {
        // Only a type expands. Asked about one method, the answer stays that method's -
        // it is the narrower claim and the one the caller asked for.
        let store = Store::open_in_memory().unwrap();
        let (_, methods, _) = handler_with_members(&store);
        let handlers = store.rpc_handler_methods().unwrap();
        assert!(store
            .own_handler_members(methods[0], &handlers)
            .unwrap()
            .is_empty());
    }
}
