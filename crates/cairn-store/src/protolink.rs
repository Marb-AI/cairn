//! Cross-language reachability through gRPC service boundaries.
//!
//! This is the claim in architecture §7 that nothing else can answer: "which Go code
//! reaches this Python handler". A name search cannot do it at any budget, because the
//! two sides share no identifier — the Go caller says `AuthServiceClient`, the Python
//! handler says `AuthServiceBase`, and neither string appears in the other's file.
//!
//! What links them is that both are generated from the same `.proto` service, and the
//! generator's naming convention makes that recoverable without parsing a single
//! `.proto` file:
//!
//! | side | shape | example |
//! |---|---|---|
//! | Python server | `<Svc>Base` | `orders_api.AuthServiceBase` |
//! | Python client | `<Svc>Stub` | `orders_api.AuthServiceStub` |
//! | Go server | `<Svc>Server`, embedded, or `Register<Svc>Server` | `orders_api.PricingServiceServer` |
//! | Go client | `<Svc>Client` | `orders_api.AuthServiceClient` |
//!
//! The package comes from the directory the generated file sits in, so
//! `srcpy/schema/orders_api/…` and `srcgo/schema/orders_api/…` resolve to the same
//! service identity. Two services may share a name across packages — `orders_api`
//! and `orders_fe` both define `AuthService` and they are *different* services — so
//! the package is part of the key, not decoration.
//!
//! **A boundary is between processes, not between languages.** The cross-language case is
//! what nothing else can answer and so it is what this file is named for, but a Python CLI
//! calling a Python gRPC service crosses exactly the same boundary and has to be answered
//! the same way. Treating "other language" as the test once hid every one of those.
//!
//! Everything here is a convention, which per D16 belongs in a rule pack once that
//! engine exists. It is written out longhand for now, in one file, so the boundary
//! stays visible.

use crate::{Store, SymbolRow};
use anyhow::Result;
use rusqlite::params;

/// How a symbol relates to a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum ServiceRole {
    /// Implements it: a handler, or the function that registers one.
    Serves = 0,
    /// Calls it through a generated client or stub.
    Calls = 1,
}

impl ServiceRole {
    pub fn label(self) -> &'static str {
        match self {
            ServiceRole::Serves => "serves",
            ServiceRole::Calls => "calls",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LinkStats {
    pub services: usize,
    pub serves: usize,
    pub calls: usize,
}

/// Split a generated symbol name into the service it belongs to, and which side it is.
///
/// Returns `(canonical service name, role)`. `None` when the name is not a generated
/// service artefact — which is most names, so this is the filter as much as the parser.
///
/// Canonicalisation matters more than it looks. A generator emits several artefacts per
/// service and they are *not* spelled alike: `NewChatServiceClient` (constructor),
/// `chatServiceClient` (unexported implementing struct),
/// `OrderMirrorService_MirrorOrdersClient` (per-method streaming type). Taken
/// literally they produced 237 "services" against 71 real `service` declarations — and
/// every spurious service is a place two unrelated symbols can be joined by a false
/// cross-language edge. Folding them onto one name is what makes the edges trustworthy.
///
/// Only the tests call this: production always has a rule pack to hand and goes straight
/// to `classify_with`.
#[cfg(test)]
fn classify(name: &str) -> Option<(String, ServiceRole)> {
    classify_with(name, &crate::rules::Rules::default())
}

/// Split a generated symbol name using a rule pack rather than a hardcoded chain.
///
/// The shapes are the same ones that were here before, now in
/// `src/rules/default.yaml` (architecture D16). Order is meaning: `Register<Svc>Server`
/// must be tried before the bare `Server` suffix it contains, and the pack keeps them in
/// that order rather than relying on the order of `else if` arms.
fn classify_with(name: &str, rules: &crate::rules::Rules) -> Option<(String, ServiceRole)> {
    for b in &rules.proto.bindings {
        let stem = match (&b.prefix, &b.suffix) {
            (Some(p), Some(sfx)) => match name.strip_prefix(p.as_str()) {
                Some(rest) => match rest.strip_suffix(sfx.as_str()) {
                    Some(st) => st,
                    None => continue,
                },
                None => continue,
            },
            (None, Some(sfx)) => match name.strip_suffix(sfx.as_str()) {
                Some(st) => st,
                None => continue,
            },
            (Some(p), None) => match name.strip_prefix(p.as_str()) {
                Some(st) => st,
                None => continue,
            },
            (None, None) => continue,
        };
        let role = match b.role.as_str() {
            "serves" => ServiceRole::Serves,
            "calls" => ServiceRole::Calls,
            _ => continue,
        };
        if let Some(svc) = canonical_service_with(stem, rules) {
            return Some((svc, role));
        }
    }
    None
}

/// Fold a generated stem onto the `service` declaration it came from, per the pack.
fn canonical_service_with(stem: &str, rules: &crate::rules::Rules) -> Option<String> {
    let mut stem = stem;
    for p in &rules.proto.strip_prefixes {
        if let Some(rest) = stem.strip_prefix(p.as_str()) {
            stem = rest;
            break; // one prefix, longest-first in the pack
        }
    }
    // A per-method streaming type: `OrderMirrorService_MirrorOrdersClient`.
    let stem = match stem.find('_') {
        Some(i) => &stem[..i],
        None => stem,
    };
    if !stem.contains(rules.proto.service_marker.as_str()) || stem.is_empty() {
        return None;
    }
    // Go's unexported implementing struct differs only in case: `chatServiceClient`.
    let mut chars = stem.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().collect::<String>() + chars.as_str())
}

/// Proto package a generated file belongs to: the directory under `schema/`.
///
/// `srcpy/schema/orders_api/__init__.py` and
/// `srcgo/schema/orders_api/service_auth_grpc.pb.go` both yield `orders_api`,
/// which is what makes the two sides meet.
fn package_of(path: &str) -> Option<&str> {
    // The directory the generated file sits in, whatever it is called.
    //
    // This used to split on the literal `/schema/`, which is where this repository puts
    // generated protobuf code. A repository that generates into `gen/`, `pb/` or `proto/`
    // got *zero* cross-language links and no indication of it — `reaches` would report no
    // callers, which is indistinguishable from a service nothing calls. That is exactly
    // the silent failure the envelope design exists to prevent, sitting in the mechanism
    // the measurements value most (tasks D and L).
    //
    // The parent directory is what the two languages actually have in common: both
    // generators emit `<anything>/<pkg>/…`, and `<pkg>` is the proto package. No layout
    // name is assumed.
    let (dir, file) = path.rsplit_once('/')?;
    if file.is_empty() {
        return None;
    }
    let name = dir.rsplit('/').next()?;
    if name.is_empty() || name.contains('.') {
        return None; // not a package directory
    }
    Some(name)
}

impl Store {
    /// Build the service graph from generated symbol names and their use sites.
    pub fn link_services(&mut self) -> Result<LinkStats> {
        let mut stats = LinkStats::default();
        self.conn
            .execute_batch("DELETE FROM service_links; DELETE FROM proto_services;")?;

        // 1. Every generated service artefact, with the package it belongs to.
        let mut artefacts: Vec<(i64, String, String, ServiceRole)> = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT s.id, n.s, p.s
                   FROM symbols s
                   JOIN strings n ON n.id = s.name_id
                   JOIN files   f ON f.id = s.def_file_id
                   JOIN strings p ON p.id = f.path_id
                  WHERE f.generated = 1",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (id, name, path) = row?;
                let (Some((svc, role)), Some(pkg)) =
                    (classify_with(&name, &self.rules), package_of(&path))
                else {
                    continue;
                };
                artefacts.push((id, pkg.to_string(), svc, role));
            }
        }

        let tx = self.conn.transaction()?;
        {
            let mut ins_svc = tx.prepare(
                "INSERT INTO proto_services(pkg, name) VALUES (?1, ?2)
                 ON CONFLICT(pkg, name) DO UPDATE SET pkg = excluded.pkg
                 RETURNING id",
            )?;
            let mut ins_link = tx.prepare(
                "INSERT INTO service_links(service_id, symbol_id, role, via_symbol)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(service_id, symbol_id, role) DO NOTHING",
            )?;
            // Symbols that *use* an artefact, attributed to whatever function they sit
            // in. The enclosing symbol is the meaningful end of the edge: knowing that
            // `AuthServiceClient` is referenced somewhere is useless; knowing that
            // `agent.AgentClient` references it is the answer.
            let mut users = tx.prepare(
                "SELECT DISTINCT encl.id
                   FROM occurrences o
                   JOIN files f ON f.id = o.file_id
                   JOIN symbols encl
                     ON encl.def_file_id = o.file_id
                    AND encl.def_end_line IS NOT NULL
                    AND encl.def_line <= o.line AND encl.def_end_line >= o.line
                  WHERE o.symbol_id = ?1 AND (o.role & 1) = 0
                    AND f.generated = 0",
            )?;
            // The Python side binds by inheritance, which SCIP already gives us as an
            // implements edge, so the handler is found by walking that edge back.
            let mut implementors = tx.prepare(
                "SELECT e.src_symbol FROM edges e WHERE e.dst_symbol = ?1 AND e.kind = 1",
            )?;
            // Go binds by embedding the generated interface in a struct. The `users`
            // query above cannot see it: the reference sits inside a type declaration
            // rather than a function body, and scip-go emits no enclosing range for a
            // type, so the occurrence is attributed to nothing and the whole Go serving
            // side vanishes. What it does emit is the embedded field as a symbol of its
            // own, named after the interface and containered by the struct - so resolve
            // the container to a type in the same file and bind that.
            let mut embedders = tx.prepare(
                "SELECT DISTINCT t.id
                   FROM symbols art
                   JOIN symbols field ON field.name_id = art.name_id AND field.id <> art.id
                   JOIN files ff ON ff.id = field.def_file_id AND ff.generated = 0
                   -- The artefact must actually be *referenced* on the field's own line.
                   --
                   -- Matching the embedded field to the artefact by name alone bound a
                   -- struct to every package that spells the interface the same way. Two
                   -- proto packages routinely do: a gateway tier serves the outward
                   -- service and calls the inward one, and the generator gives both the
                   -- same `<Svc>ServiceServer`. Every such gateway was then recorded as
                   -- serving the very service it is a client of, which inverts the
                   -- direction of a whole tier - and `reaches` reported callers across a
                   -- boundary for two symbols inside one process.
                   --
                   -- The occurrence carries the resolved symbol, so it tells the two
                   -- apart where the name cannot. Measured on a repository with this
                   -- shape: 119 embed links to 100, every one dropped being that
                   -- collision, and no type left without a binding - which would have
                   -- been worse than the bug, since a silent zero from `reaches` reads
                   -- as a service nothing calls.
                   JOIN occurrences o ON o.symbol_id = art.id
                                     AND o.file_id = field.def_file_id
                                     AND o.line = field.def_line
                   JOIN strings c ON c.id = field.container_id
                   JOIN symbols t ON t.def_file_id = field.def_file_id AND t.kind = 1
                   JOIN strings tn ON tn.id = t.name_id
                  WHERE art.id = ?1
                    AND (c.s = tn.s OR c.s LIKE '%/' || tn.s || '#')",
            )?;

            for (artefact_id, pkg, svc, role) in artefacts {
                let service_id: i64 = ins_svc.query_row(params![pkg, svc], |r| r.get(0))?;
                stats.services += 1;

                let mut link = |symbol_id: i64, role: ServiceRole| -> Result<()> {
                    ins_link.execute(params![service_id, symbol_id, role as i64, artefact_id])?;
                    Ok(())
                };

                let rows = users.query_map(params![artefact_id], |r| r.get::<_, i64>(0))?;
                for r in rows {
                    link(r?, role)?;
                    match role {
                        ServiceRole::Serves => stats.serves += 1,
                        ServiceRole::Calls => stats.calls += 1,
                    }
                }

                if role == ServiceRole::Serves {
                    let rows =
                        implementors.query_map(params![artefact_id], |r| r.get::<_, i64>(0))?;
                    for r in rows {
                        link(r?, ServiceRole::Serves)?;
                        stats.serves += 1;
                    }
                    let rows = embedders.query_map(params![artefact_id], |r| r.get::<_, i64>(0))?;
                    for r in rows {
                        link(r?, ServiceRole::Serves)?;
                        stats.serves += 1;
                    }
                }
            }
        }
        tx.commit()?;
        // `services` counts artefacts processed, not distinct services.
        // Counted from the tables, not accumulated during the loop: the counters were
        // incremented once per artefact, so a symbol linked by several artefacts was
        // counted several times and `index` reported 477 serve links where `status`,
        // reading the table, reported 295. Two numbers for one fact is one too many.
        stats.services = self
            .conn
            .query_row("SELECT count(*) FROM proto_services", [], |r| r.get(0))?;
        stats.serves = self.conn.query_row(
            "SELECT count(*) FROM service_links WHERE role = 0",
            [],
            |r| r.get(0),
        )?;
        stats.calls = self.conn.query_row(
            "SELECT count(*) FROM service_links WHERE role = 1",
            [],
            |r| r.get(0),
        )?;
        Ok(stats)
    }

    /// Services and links recorded, so `status` can show a zero rather than hide it.
    pub fn link_counts(&self) -> Result<(i64, i64, i64)> {
        let services = self
            .conn
            .query_row("SELECT count(*) FROM proto_services", [], |r| r.get(0))?;
        let serves = self.conn.query_row(
            "SELECT count(*) FROM service_links WHERE role = 0",
            [],
            |r| r.get(0),
        )?;
        let calls = self.conn.query_row(
            "SELECT count(*) FROM service_links WHERE role = 1",
            [],
            |r| r.get(0),
        )?;
        Ok((services, serves, calls))
    }

    /// Who reaches this symbol across a service boundary.
    ///
    /// The walk: the symbol serves some service (directly, or because a function that
    /// registers it does), and other code calls that service through a generated
    /// client. Those callers are in a different language and share no identifier with
    /// the symbol, which is why nothing else finds them.
    pub fn cross_language_callers(&self, symbol_id: i64) -> Result<Vec<CrossLink>> {
        let mut stmt = self.conn.prepare_cached(
            r#"
            SELECT DISTINCT ps.pkg, ps.name, caller.id
              FROM service_links mine
              JOIN proto_services ps ON ps.id = mine.service_id
              JOIN service_links theirs
                ON theirs.service_id = mine.service_id AND theirs.role = 1
              JOIN symbols caller ON caller.id = theirs.symbol_id
             WHERE mine.symbol_id = ?1 AND mine.role = 0
               AND caller.id <> ?1
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
            let (pkg, service, caller_id) = row?;
            let Some(caller) = self.symbol(caller_id)? else {
                continue;
            };
            out.push(CrossLink {
                pkg,
                service,
                symbol: caller,
            });
        }
        Ok(out)
    }

    /// Callers of the one RPC this method implements, not of every RPC its handler owns.
    ///
    /// `cross_language_callers` answers at the granularity of the handler class, because
    /// that is where the generator's naming convention binds. Measured (task E), an agent
    /// asked which services a change to one repository function would affect, got the
    /// whole handler's caller set back, and spent a large share of the run reading Go and
    /// Python by hand to narrow it to the RPCs that actually reach the change. Every edge
    /// it reconstructed was already in the index.
    ///
    /// This walks one step further than the convention: from the service, to the
    /// generated client type that calls it, to the member of that type with the same RPC
    /// name, to that member's real call sites. Those last edges are compiler-derived, so
    /// the result is exact where `cross_language_callers` is conventional — and it keeps
    /// the two proto packages apart, which matters here because `orders_api` and
    /// `orders_fe` carry the same service names to different processes.
    pub fn rpc_callers(&self, method_id: i64) -> Result<Vec<RpcCaller>> {
        let Some(me) = self.symbol(method_id)? else {
            return Ok(Vec::new());
        };
        let Some(owner) = self.enclosing_type(method_id)? else {
            return Ok(Vec::new());
        };

        let mut stmt = self.conn.prepare_cached(
            r#"
            SELECT DISTINCT ps.pkg, ps.name, rpc_name.s, caller.id
              FROM service_links mine
              JOIN proto_services ps ON ps.id = mine.service_id
              JOIN service_links theirs
                ON theirs.service_id = mine.service_id AND theirs.role = 1
              JOIN symbols art ON art.id = theirs.via_symbol AND art.kind = 1
              JOIN strings art_name ON art_name.id = art.name_id
              -- Membership by container, not by line range: a Go interface method carries
              -- no enclosing range, so span containment silently drops every Go client -
              -- which is the whole other side of the boundary. Same file keeps the two
              -- proto packages apart, since both spell the service the same way.
              JOIN symbols rpc
                ON rpc.def_file_id = art.def_file_id AND rpc.id <> art.id
               AND rpc.container_leaf_id = art.name_id
              JOIN strings rpc_name ON rpc_name.id = rpc.name_id
              JOIN edges e ON e.dst_symbol = rpc.id AND e.kind = 0
              JOIN symbols caller ON caller.id = e.src_symbol
              JOIN files cf ON cf.id = caller.def_file_id AND cf.generated = 0
             WHERE mine.symbol_id = ?1 AND mine.role = 0
               AND caller.id <> ?2
            "#,
        )?;
        let rows = stmt.query_map(params![owner.id, method_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (pkg, service, rpc, caller_id) = row?;
            if !same_rpc(&rpc, &me.name) {
                continue;
            }
            let Some(symbol) = self.symbol(caller_id)? else {
                continue;
            };
            out.push(RpcCaller {
                pkg,
                service,
                rpc,
                symbol,
            });
        }
        Ok(out)
    }

    /// Per-RPC callers for every RPC a handler type serves, in one query pair.
    ///
    /// `rpc_callers` answers about one method, which is right when that is the question
    /// and wrong when the question is about the handler. Measured (task D): asked which
    /// Go code reaches a Python handler, an agent called `reaches` once per RPC method and
    /// spent more than the run it was meant to beat. Same shape as `affects` — a question
    /// about a set has to be answered as a set.
    pub fn rpc_callers_of_type(&self, type_id: i64) -> Result<Vec<RpcCaller>> {
        // The handler's own methods, so a client method is only reported when the handler
        // actually implements that RPC.
        let mut mine = self.conn.prepare(
            r#"
            SELECT n.s FROM symbols t
              JOIN strings tn ON tn.id = t.name_id
              JOIN symbols m ON m.def_file_id = t.def_file_id AND m.id <> t.id
               AND m.container_leaf_id = t.name_id
              JOIN strings n ON n.id = m.name_id
             WHERE t.id = ?1
            "#,
        )?;
        let rows = mine.query_map(params![type_id], |r| r.get::<_, String>(0))?;
        let mut names: Vec<String> = Vec::new();
        for r in rows {
            names.push(r?);
        }
        if names.is_empty() {
            return Ok(Vec::new());
        }

        let mut stmt = self.conn.prepare(
            r#"
            SELECT DISTINCT ps.pkg, ps.name, rpc_name.s, caller.id
              FROM service_links mine
              JOIN proto_services ps ON ps.id = mine.service_id
              JOIN service_links theirs
                ON theirs.service_id = mine.service_id AND theirs.role = 1
              JOIN symbols art ON art.id = theirs.via_symbol AND art.kind = 1
              JOIN strings art_name ON art_name.id = art.name_id
              JOIN symbols rpc
                ON rpc.def_file_id = art.def_file_id AND rpc.id <> art.id
              JOIN strings cont ON cont.id = rpc.container_id
               AND (cont.s = art_name.s OR cont.s LIKE '%/' || art_name.s || '#')
              JOIN strings rpc_name ON rpc_name.id = rpc.name_id
              JOIN edges e ON e.dst_symbol = rpc.id AND e.kind = 0
              JOIN symbols caller ON caller.id = e.src_symbol
              JOIN files cf ON cf.id = caller.def_file_id AND cf.generated = 0
             WHERE mine.symbol_id = ?1 AND mine.role = 0
            "#,
        )?;
        let rows = stmt.query_map(params![type_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (pkg, service, rpc, caller_id) = row?;
            if !names.iter().any(|n| same_rpc(n, &rpc)) {
                continue;
            }
            let Some(symbol) = self.symbol(caller_id)? else {
                continue;
            };
            out.push(RpcCaller {
                pkg,
                service,
                rpc,
                symbol,
            });
        }
        out.sort_by(|a, b| (&a.rpc, a.symbol.id).cmp(&(&b.rpc, b.symbol.id)));
        Ok(out)
    }

    /// The reverse: which handlers a caller reaches through service boundaries.
    pub fn cross_language_targets(&self, symbol_id: i64) -> Result<Vec<CrossLink>> {
        let mut stmt = self.conn.prepare_cached(
            r#"
            SELECT DISTINCT ps.pkg, ps.name, impl.id
              FROM service_links mine
              JOIN proto_services ps ON ps.id = mine.service_id
              JOIN service_links theirs
                ON theirs.service_id = mine.service_id AND theirs.role = 0
              JOIN symbols impl ON impl.id = theirs.symbol_id
             WHERE mine.symbol_id = ?1 AND mine.role = 1
               AND impl.id <> ?1
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
            let (pkg, service, id) = row?;
            let Some(symbol) = self.symbol(id)? else {
                continue;
            };
            out.push(CrossLink {
                pkg,
                service,
                symbol,
            });
        }
        Ok(out)
    }

    /// Services a symbol serves or calls, for reporting.
    pub fn services_of(&self, symbol_id: i64) -> Result<Vec<(String, String, ServiceRole)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT ps.pkg, ps.name, l.role
               FROM service_links l JOIN proto_services ps ON ps.id = l.service_id
              WHERE l.symbol_id = ?1",
        )?;
        let rows = stmt.query_map(params![symbol_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                if r.get::<_, i64>(2)? == 1 {
                    ServiceRole::Calls
                } else {
                    ServiceRole::Serves
                },
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

#[derive(Debug, Clone)]
pub struct CrossLink {
    pub pkg: String,
    pub service: String,
    pub symbol: crate::SymbolRow,
}

/// A caller of one specific RPC, rather than of the handler that owns it.
#[derive(Debug, Clone)]
pub struct RpcCaller {
    pub pkg: String,
    pub service: String,
    /// The RPC as the caller's language spells it — `GetPricingDashboard` in Go, where the
    /// handler being asked about spells it `get_pricing_dashboard`.
    pub rpc: String,
    pub symbol: crate::SymbolRow,
}

/// Method names across the boundary differ only in case and underscores, because both
/// spellings are the generator's rendering of the same proto RPC.
impl Store {
    /// What this function calls across a service boundary, and which handler serves it.
    ///
    /// The mirror of `rpc_callers`, and it did not exist. `cross_language_targets` answers
    /// only for a symbol that *is* a client artefact, because `service_links` binds the
    /// generated client type — not the function that uses it. So asked what a Go handler
    /// calls into Python, the outgoing direction returned nothing while the incoming
    /// direction on the same symbol was exact.
    ///
    /// Measured: agents reached for `--outgoing` in three separate rounds, always on the
    /// chain question ("where does this land"), always got zero, and rebuilt the chain by
    /// hand. It is the one direction the tool advertises and does not deliver.
    ///
    /// The walk is the reverse of the incoming one: this symbol's calls, to a member of a
    /// generated client, to the service that client speaks, to the handler serving it, to
    /// the handler method of the same RPC. The last step crosses the naming convention
    /// (`GetFolder` ↔ `get_folder`), which is why the result is labelled conventional.
    pub fn rpc_targets(&self, symbol_id: i64) -> Result<Vec<RpcCaller>> {
        // Two steps on purpose. Done as one join it was 11.8 s on a hot symbol and the
        // sweep caught it: the last hop expands every method of every serving type and
        // then throws most away, so the cost is the caller's fan-out times the size of
        // each handler's file. Asking first *which RPCs this calls* keeps that expansion
        // to the handful of services actually involved.
        let mut calls = self.conn.prepare_cached(
            r#"
            SELECT DISTINCT ps.id, ps.pkg, ps.name, rpc_name.s
              FROM edges e
              JOIN symbols rpc ON rpc.id = e.dst_symbol
              JOIN strings rpc_name ON rpc_name.id = rpc.name_id
              JOIN symbols art ON art.def_file_id = rpc.def_file_id AND art.id <> rpc.id
                              AND rpc.container_leaf_id = art.name_id AND art.kind = 1
              JOIN service_links theirs ON theirs.via_symbol = art.id AND theirs.role = 1
              JOIN proto_services ps ON ps.id = theirs.service_id
             WHERE e.src_symbol = ?1 AND e.kind = 0
            "#,
        )?;
        let rows = calls.query_map(params![symbol_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut wanted = Vec::new();
        for row in rows {
            wanted.push(row?);
        }

        // The normalisation the convention needs (`GetFolder` <-> `get_folder`) done in
        // SQL, so the join filters instead of returning every method for Rust to sift.
        let mut serve = self.conn.prepare_cached(
            r#"
            SELECT DISTINCT hm.id
              FROM service_links mine
              JOIN symbols handler ON handler.id = mine.symbol_id AND handler.kind = 1
              JOIN symbols hm ON hm.def_file_id = handler.def_file_id
                             AND hm.container_leaf_id = handler.name_id
                             AND hm.id <> handler.id
              JOIN strings hm_name ON hm_name.id = hm.name_id
              JOIN files hf ON hf.id = hm.def_file_id AND hf.generated = 0
             WHERE mine.service_id = ?1 AND mine.role = 0
               AND replace(lower(hm_name.s), '_', '') = replace(lower(?2), '_', '')
            "#,
        )?;

        // What has to be excluded is the caller's own file, not the caller's own language.
        //
        // This used to drop every same-language row, because a name collision in
        // `link_services` bound a gateway type to both packages of a same-named service
        // and the join then returned the caller and its siblings alongside the real
        // targets. Dropping by language hid that — and hid, with it, **every service that
        // calls another service written in the same language**, which is the common case
        // in any single-language deployment. Measured on a mixed Python/Go repository: the
        // incoming direction listed nine callers of one handler, the outgoing direction
        // answered `0 targets` for each of them, and a silent zero is indistinguishable
        // from a service nothing calls.
        //
        // With the collision fixed at its root, the file is the honest exclusion: a
        // "target" defined beside the caller is the handler this code *is*, or its
        // sibling, and not something it reaches over a network.
        let me = self.symbol(symbol_id)?;
        let my_file = me.as_ref().and_then(|m| m.def.as_ref()).map(|d| &d.path);
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (service_id, pkg, service, rpc) in wanted {
            let ids = serve.query_map(params![service_id, rpc], |r| r.get::<_, i64>(0))?;
            for id in ids {
                let id = id?;
                if id == symbol_id || !seen.insert(id) {
                    continue;
                }
                let Some(symbol) = self.symbol(id)? else {
                    continue;
                };
                if let (Some(mine), Some(theirs)) = (my_file, symbol.def.as_ref().map(|d| &d.path))
                {
                    if mine == theirs {
                        continue;
                    }
                }
                out.push(RpcCaller {
                    pkg: pkg.clone(),
                    service: service.clone(),
                    rpc: rpc.clone(),
                    symbol,
                });
            }
        }
        Ok(out)
    }

    /// The same answer as `rpc_targets`, for a symbol that *holds* a generated client
    /// rather than calling one.
    ///
    /// The two used to be different commands wearing one name: the call-edge form named
    /// handler symbols, the binding form named services, and nothing in the output said
    /// which you had got. An agent could not compare them, and the stress harness could
    /// only report the difference, never check it.
    ///
    /// So this returns the same rows: the handlers that serve the services this symbol is
    /// registered as a client of. The claim is weaker — it says this code holds a client
    /// for that service, not that a call was seen — and the caller labels it as such
    /// rather than the shape doing it silently.
    pub fn rpc_targets_by_binding(&self, symbol_id: i64) -> Result<(Vec<RpcCaller>, Vec<String>)> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT DISTINCT ps.pkg, ps.name, hm_name.s, rpc_name.s, hm.id
              FROM service_links mine
              JOIN proto_services ps ON ps.id = mine.service_id
              -- The server side of the same service, and its members.
              JOIN service_links theirs
                ON theirs.service_id = ps.id AND theirs.role = 0
              JOIN symbols handler ON handler.id = theirs.symbol_id AND handler.kind = 1
              JOIN symbols hm ON hm.def_file_id = handler.def_file_id
                             AND hm.container_leaf_id = handler.name_id
                             AND hm.id <> handler.id
              JOIN strings hm_name ON hm_name.id = hm.name_id
              JOIN files hf ON hf.id = hm.def_file_id AND hf.generated = 0
              -- The generated client for the same service, and *its* members: the RPC
              -- names. Without them every member of the handler class came back, private
              -- helpers included - 65 rows where the service has a dozen RPCs.
              --
              -- LEFT, not inner. As an inner join it dropped every service whose client
              -- link carries no `via_symbol`, silently and entirely: one whole service
              -- vanished from the answer rather than one helper row. Absent names mean
              -- "cannot tell which members are RPCs here", and the row survives with that
              -- said, because dropping what cannot be checked is the failure this
              -- codebase keeps finding in itself.
              LEFT JOIN service_links client
                ON client.service_id = ps.id AND client.role = 1
                AND client.via_symbol IS NOT NULL
              LEFT JOIN symbols art ON art.id = client.via_symbol AND art.kind = 1
              LEFT JOIN symbols rpc ON rpc.def_file_id = art.def_file_id
                              AND rpc.id <> art.id
                              AND rpc.container_leaf_id = art.name_id
              LEFT JOIN strings rpc_name ON rpc_name.id = rpc.name_id
             WHERE mine.symbol_id = ?1 AND mine.role = 1
            "#,
        )?;
        let rows = stmt.query_map(params![symbol_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        let me = self.symbol(symbol_id)?;
        // Which (service, member) pairs a client artefact vouched for, and which services
        // had no artefact to ask. Collected first because the rows arrive interleaved.
        let mut raw = Vec::new();
        let mut vouched: std::collections::HashSet<(String, i64)> = Default::default();
        let mut unnamed: std::collections::HashSet<String> = Default::default();
        for row in rows {
            let (pkg, service, handler_method, rpc, id) = row?;
            let key = format!("{pkg}.{service}");
            match &rpc {
                Some(r) if same_rpc(r, &handler_method) => {
                    vouched.insert((key.clone(), id));
                }
                Some(_) => {}
                None => {
                    unnamed.insert(key.clone());
                }
            }
            raw.push((pkg, service, handler_method, id, key));
        }
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (pkg, service, handler_method, id, key) in raw {
            if id == symbol_id {
                continue;
            }
            // Keep a member when the client artefact named it as an RPC, or when there was
            // no artefact to name anything for that service.
            let keep = vouched.contains(&(key.clone(), id)) || unnamed.contains(&key);
            if !keep || !seen.insert(id) {
                continue;
            }
            let Some(symbol) = self.symbol(id)? else {
                continue;
            };
            // Same filter as the precise form: this command answers "in the other
            // language", so a row in the caller's own is not the answer.
            if me.as_ref().is_some_and(|m| m.lang == symbol.lang) {
                continue;
            }
            out.push(RpcCaller {
                pkg,
                service,
                rpc: handler_method,
                symbol,
            });
        }
        out.sort_by(|a, b| (&a.pkg, &a.service, &a.rpc).cmp(&(&b.pkg, &b.service, &b.rpc)));
        // Services whose members could not be checked against an RPC list. Returned rather
        // than swallowed: their rows may include private helpers, and a row that might not
        // be an entry point has to say so instead of sitting in the list looking like one.
        let mut unchecked: Vec<String> = unnamed.into_iter().collect();
        unchecked.sort();
        Ok((out, unchecked))
    }
}

/// How many in-language callees of a hop are asked whether *they* cross a boundary.
///
/// Every one costs an `rpc_targets` query, and the walk asks at two local levels, so this
/// is the number that decides whether a chain answers in milliseconds or in seconds. Four
/// is enough for the delegation shape it exists to catch (a handler calling one or two
/// helpers) and cheap enough that a wide handler does not blow the latency ceiling.
const LOCAL_FANOUT: usize = 4;

/// One service hop: where it leaves from, where it lands, and how strong the claim is.
#[derive(Debug, Clone)]
pub struct ChainHop {
    /// The symbol on the near side of this hop.
    pub from: SymbolRow,
    /// Where it lands, and the RPC that carried it.
    pub to: RpcCaller,
    /// Hops from the root. The first is 1.
    pub depth: usize,
    /// True when the hop came from a call edge that was actually observed, false when it
    /// came from a client binding — which says this code holds a client for that service,
    /// not that a call was seen.
    pub exact: bool,
    /// This target was already reached by an earlier route, so it is shown and not walked
    /// again. Without the flag a reader takes a silently truncated branch for a leaf.
    pub already_reached: bool,
    /// The in-language function that actually made the call, when the hop was not made by
    /// `from` itself. A Go handler that delegates to a transformer which then calls
    /// another service reaches it *through* that transformer, and a chain that prints the
    /// handler as the caller is telling the reader to look in the wrong file.
    pub via: Option<SymbolRow>,
}

/// A chain of service hops, with everything the walk did not do said out loud.
#[derive(Debug, Clone, Default)]
pub struct Chain {
    pub hops: Vec<ChainHop>,
    /// Symbols the depth cap stopped the walk at. Stated as "not asked" rather than "cut
    /// off": whether they would have answered is exactly what was not checked, and
    /// claiming a branch continues is as wrong as letting the reader assume it ended.
    pub not_followed: Vec<SymbolRow>,
    /// Hops dropped because the total cap bit.
    pub cut_by_breadth: usize,
    /// Services the binding fallback could not check its rows against.
    pub unchecked: Vec<String>,
}

impl Store {
    /// Follow the outgoing service hops as far as they go, rather than one per call.
    ///
    /// `rpc_targets` answers one hop. The chain question — "where does this land" — is
    /// almost never one hop, and the measured cost of that mismatch is a round trip per
    /// hop *plus* the turns spent working out which command does not hide the first one:
    /// `graph --aspect calls` suppresses generated code, so the call into the stub is
    /// invisible there while `reaches --outgoing` has it exactly.
    ///
    /// Two bounds, and both are reported rather than applied quietly:
    ///
    /// * **Depth.** Symbols the cap stopped at are returned in `not_followed`, because a
    ///   branch that stopped and a branch that ended look identical on the page otherwise.
    /// * **Breadth.** The last hop is matched by the generator's naming convention, so
    ///   whatever the convention over-matches is multiplied at every level below it.
    ///
    /// The binding fallback runs at the root only. Deeper down it would turn one symbol
    /// that merely *holds* a client into every handler of that service, and a weak claim
    /// compounded three levels deep is a chain a reader would believe and should not.
    pub fn rpc_chain(&self, root: i64, max_depth: usize, max_hops: usize) -> Result<Chain> {
        let mut chain = Chain::default();
        let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::from([root]);
        let mut frontier = vec![root];

        for depth in 1..=max_depth {
            let mut next = Vec::new();
            for &from_id in &frontier {
                let Some(from) = self.symbol(from_id)? else {
                    continue;
                };
                let mut exact = true;
                let mut targets: Vec<(Option<SymbolRow>, RpcCaller)> = self
                    .rpc_targets(from_id)?
                    .into_iter()
                    .map(|t| (None, t))
                    .collect();
                if targets.is_empty() && depth == 1 {
                    let (bound, unchecked) = self.rpc_targets_by_binding(from_id)?;
                    targets = bound.into_iter().map(|t| (None, t)).collect();
                    exact = false;
                    chain.unchecked = unchecked;
                }
                // Hops made by something this symbol calls in its own language.
                //
                // Round six found this missing, three arms independently: the Go proxy
                // serving `GetSharedObject` does not call the next service itself — it
                // builds a folder transformer, and *that* calls `ListEstates`. The walk
                // followed RPC edges only, so it stopped one hop short while printing
                // "followed to the end". Two local levels, narrow fan-out: the shape being
                // recovered is "handler delegates to a helper that makes the call", which
                // is one or two calls deep, and every extra level multiplies the queries.
                for node in self
                    .walk(
                        from_id,
                        crate::EdgeKind::Calls,
                        crate::Direction::Out,
                        2,
                        LOCAL_FANOUT,
                        true,
                    )?
                    .nodes
                    .iter()
                    .filter(|n| n.symbol.id != from_id)
                {
                    for t in self.rpc_targets(node.symbol.id)? {
                        targets.push((Some(node.symbol.clone()), t));
                    }
                }
                for (via, to) in targets {
                    if chain.hops.len() >= max_hops {
                        chain.cut_by_breadth += 1;
                        continue;
                    }
                    let already_reached = !seen.insert(to.symbol.id);
                    if !already_reached {
                        next.push(to.symbol.id);
                    }
                    chain.hops.push(ChainHop {
                        from: from.clone(),
                        to,
                        depth,
                        exact,
                        already_reached,
                        via,
                    });
                }
            }
            if next.is_empty() {
                return Ok(chain);
            }
            // The symbols still on the frontier when the cap bites are the ones whose own
            // targets were never asked for.
            if depth == max_depth {
                for id in &next {
                    if let Some(sym) = self.symbol(*id)? {
                        chain.not_followed.push(sym);
                    }
                }
            }
            frontier = next;
        }
        Ok(chain)
    }
}

fn same_rpc(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> String {
        s.chars()
            .filter(|c| *c != '_')
            .flat_map(|c| c.to_lowercase())
            .collect()
    };
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str) -> Option<(String, ServiceRole)> {
        classify(name)
    }

    /// A Go struct embedding one package's server interface, where a second package
    /// declares an interface of exactly the same name.
    ///
    /// The shape is a proxy tier: an edge service that *serves* the outward-facing
    /// service and *calls* the inward-facing one, both generated from a `.proto` that
    /// spells the service identically in two packages. Nothing about it is specific to
    /// any repository — it follows from protobuf's package split plus Go's embedding,
    /// and any codebase with a gateway in front of an internal API has it.
    #[test]
    fn embedding_one_packages_server_does_not_serve_another_packages_namesake() {
        let mut store = Store::open_in_memory().unwrap();
        let c = &store.conn;
        let sid = |s: &str| -> i64 {
            c.query_row(
                "INSERT INTO strings(s) VALUES (?1) ON CONFLICT(s) DO UPDATE SET s = excluded.s
                 RETURNING id",
                params![s],
                |r| r.get(0),
            )
            .unwrap()
        };
        let file = |path: &str, generated: i64| -> i64 {
            c.execute(
                "INSERT INTO files(path_id, lang, generated) VALUES (?1, 2, ?2)",
                params![sid(path), generated],
            )
            .unwrap();
            c.last_insert_rowid()
        };
        let add =
            |name: &str, kind: i64, f: i64, line: i64, container: Option<i64>, h: u8| -> i64 {
                c.execute(
                    "INSERT INTO symbols(hash, name_id, kind, lang, ref_count, def_file_id,
                                     def_line, container_id, container_leaf_id)
                 VALUES (?1, ?2, ?3, 2, 0, ?4, ?5, ?6, ?6)",
                    params![vec![h; 16], sid(name), kind, f, line, container],
                )
                .unwrap();
                c.last_insert_rowid()
            };

        // The two generated interfaces: same name, different proto packages.
        let edge = file("srcgo/schema/station_edge/service_station_grpc.pb.go", 1);
        let core = file("srcgo/schema/station_core/service_station_grpc.pb.go", 1);
        let art_edge = add("StationServiceServer", 1, edge, 10, None, 1);
        add("StationServiceServer", 1, core, 10, None, 2);

        // The hand-written gateway:
        //   type stationGateway struct { station_edge.StationServiceServer }
        let hand = file("srcgo/gateway/station.go", 0);
        let owner = sid("`x/gateway`/stationGateway#");
        add("stationGateway", 1, hand, 15, None, 3);
        add("StationServiceServer", 2, hand, 16, Some(owner), 4);
        // Only the edge interface is referenced on the embedding line. That occurrence is
        // the entire difference between the two packages, and matching by name ignored it.
        c.execute(
            "INSERT INTO occurrences(file_id, symbol_id, line, col_start, col_end, role)
             VALUES (?1, ?2, 16, 1, 2, 8)",
            params![hand, art_edge],
        )
        .unwrap();

        store.link_services().unwrap();
        let served: Vec<String> = store
            .conn
            .prepare(
                "SELECT ps.pkg FROM service_links l
                   JOIN proto_services ps ON ps.id = l.service_id
                   JOIN symbols s ON s.id = l.symbol_id
                   JOIN strings n ON n.id = s.name_id
                  WHERE l.role = 0 AND n.s = 'stationGateway'",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            served,
            vec!["station_edge".to_string()],
            "the struct embeds only the edge interface; binding it to the core service of \
             the same name makes the gateway a server of the service it is a client of, \
             and `reaches` then reports callers across a boundary for two symbols in one \
             process"
        );
    }

    #[test]
    fn classifies_both_sides_of_the_boundary() {
        assert_eq!(
            c("AuthServiceBase"),
            Some(("AuthService".into(), ServiceRole::Serves))
        );
        assert_eq!(
            c("AuthServiceStub"),
            Some(("AuthService".into(), ServiceRole::Calls))
        );
        assert_eq!(
            c("AuthServiceClient"),
            Some(("AuthService".into(), ServiceRole::Calls))
        );
        assert_eq!(
            c("RegisterAuthServiceServer"),
            Some(("AuthService".into(), ServiceRole::Serves))
        );
    }

    #[test]
    fn folds_the_generator_variants_onto_one_service() {
        // All four of these are artefacts of `service Chat`; treating them as distinct
        // services invents boundaries and therefore invents edges across them.
        for name in [
            "ChatServiceClient",
            "NewChatServiceClient",
            "chatServiceClient",
            "ChatService_StreamRepliesClient",
        ] {
            assert_eq!(
                c(name).map(|(s, _)| s),
                Some("ChatService".to_string()),
                "{name} should fold onto ChatService"
            );
        }
    }

    #[test]
    fn classifies_the_go_serving_side() {
        // The half that was missing: grpc-go binds a server by embedding the generated
        // interface, so these names are how a Go service says "I implement this".
        assert_eq!(
            c("AuthServiceServer"),
            Some(("AuthService".into(), ServiceRole::Serves))
        );
        assert_eq!(
            c("UnimplementedAuthServiceServer"),
            Some(("AuthService".into(), ServiceRole::Serves))
        );
        assert_eq!(
            c("UnsafeAuthServiceServer"),
            Some(("AuthService".into(), ServiceRole::Serves))
        );
        // grpc-go's guard method, which is not a service of its own.
        assert_eq!(
            c("mustEmbedUnimplementedAuthServiceServer"),
            Some(("AuthService".into(), ServiceRole::Serves))
        );
        // Still not a service artefact, despite the suffix.
        assert_eq!(c("HttpServer"), None);
    }

    #[test]
    fn matches_rpc_names_across_the_generators_two_spellings() {
        assert!(same_rpc("get_pricing_dashboard", "GetPricingDashboard"));
        assert!(same_rpc("GetPricingDashboard", "get_pricing_dashboard"));
        assert!(!same_rpc("get_pricing_dashboard", "GetPricingState"));
    }

    #[test]
    fn leaves_ordinary_names_alone() {
        // Names that merely end in one of the suffixes are not service artefacts.
        assert_eq!(c("DatabaseClient"), None);
        assert_eq!(c("Base"), None);
        assert_eq!(c("HttpStub"), None);
        assert_eq!(c("RegisterUser"), None);
        assert_eq!(c("NewClient"), None);
    }

    #[test]
    fn package_comes_from_the_containing_directory_whatever_it_is_called() {
        assert_eq!(
            package_of("srcpy/schema/orders_api/__init__.py"),
            Some("orders_api")
        );
        assert_eq!(
            package_of("srcgo/schema/orders_fe/service_auth_grpc.pb.go"),
            Some("orders_fe")
        );
        // The layout name is not assumed: these are the shapes other repositories use,
        // and they used to yield nothing at all.
        assert_eq!(
            package_of("gen/go/billing_v1/service.pb.go"),
            Some("billing_v1")
        );
        assert_eq!(package_of("pb/orders/orders_pb2.py"), Some("orders"));
        assert_eq!(package_of("x.py"), None); // no directory to take a name from

        // A file directly under its tree now yields that directory's name rather than
        // nothing. Harmless: this is only ever asked about files already classified as
        // generated, and a package name only reaches the graph if a symbol in that file
        // also classifies as a service artefact, which `openapi.py` has none of.
        assert_eq!(package_of("srcpy/schema/openapi.py"), Some("schema"));
    }

    #[test]
    fn the_package_is_part_of_the_identity() {
        // orders_api.AuthService and orders_fe.AuthService are different
        // services that happen to share a name; conflating them would invent edges.
        assert_ne!(
            package_of("srcpy/schema/orders_api/__init__.py"),
            package_of("srcpy/schema/orders_fe/__init__.py")
        );
    }
}
