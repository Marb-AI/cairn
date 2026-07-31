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
//! | Go server | `Register<Svc>Server` | in `srcgo/schema/orders_api/` |
//! | Go client | `<Svc>Client` | `orders_api.AuthServiceClient` |
//!
//! The package comes from the directory the generated file sits in, so
//! `srcpy/schema/orders_api/…` and `srcgo/schema/orders_api/…` resolve to the same
//! service identity. Two services may share a name across packages — `orders_api`
//! and `orders_fe` both define `AuthService` and they are *different* services — so
//! the package is part of the key, not decoration.
//!
//! Everything here is a convention, which per D16 belongs in a rule pack once that
//! engine exists. It is written out longhand for now, in one file, so the boundary
//! stays visible.

use crate::Store;
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
fn classify(name: &str) -> Option<(String, ServiceRole)> {
    let (stem, role) = if let Some(rest) = name.strip_prefix("Register") {
        (rest.strip_suffix("Server")?, ServiceRole::Serves)
    } else if let Some(s) = name.strip_suffix("Base") {
        (s, ServiceRole::Serves)
    } else if let Some(s) = name.strip_suffix("Stub") {
        (s, ServiceRole::Calls)
    } else if let Some(s) = name.strip_suffix("Client") {
        (s, ServiceRole::Calls)
    } else {
        return None;
    };
    canonical_service(stem).map(|svc| (svc, role))
}

/// Fold a generated stem onto the `service` declaration it came from.
fn canonical_service(stem: &str) -> Option<String> {
    // A constructor: `NewChatServiceClient` -> `ChatService`.
    let stem = stem.strip_prefix("New").unwrap_or(stem);
    // A per-method streaming type: `OrderMirrorService_MirrorOrdersClient`.
    let stem = match stem.find('_') {
        Some(i) => &stem[..i],
        None => stem,
    };
    // Only protoc's own artefacts qualify, and it derives them all from `service Foo`.
    if !stem.contains("Service") || stem.is_empty() {
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
    let after = path.split("/schema/").nth(1)?;
    let first = after.split('/').next()?;
    if first.is_empty() || first.contains('.') {
        return None; // a file directly under schema/, not a package directory
    }
    Some(first)
}

impl Store {
    /// Build the service graph from generated symbol names and their use sites.
    pub fn link_services(&mut self) -> Result<LinkStats> {
        let mut stats = LinkStats::default();
        self.conn.execute_batch(
            "DELETE FROM service_links; DELETE FROM proto_services;",
        )?;

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
                let (Some((svc, role)), Some(pkg)) = (classify(&name), package_of(&path)) else {
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

            for (artefact_id, pkg, svc, role) in artefacts {
                let service_id: i64 =
                    ins_svc.query_row(params![pkg, svc], |r| r.get(0))?;
                stats.services += 1;

                let mut link = |symbol_id: i64, role: ServiceRole| -> Result<()> {
                    ins_link.execute(params![
                        service_id,
                        symbol_id,
                        role as i64,
                        artefact_id
                    ])?;
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
                }
            }
        }
        tx.commit()?;
        // `services` counts artefacts processed, not distinct services.
        stats.services = self
            .conn
            .query_row("SELECT count(*) FROM proto_services", [], |r| r.get(0))?;
        Ok(stats)
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
            let Some(caller) = self.symbol(caller_id)? else { continue };
            out.push(CrossLink { pkg, service, symbol: caller });
        }
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
            let Some(symbol) = self.symbol(id)? else { continue };
            out.push(CrossLink { pkg, service, symbol });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str) -> Option<(String, ServiceRole)> {
        classify(name)
    }

    #[test]
    fn classifies_both_sides_of_the_boundary() {
        assert_eq!(c("AuthServiceBase"), Some(("AuthService".into(), ServiceRole::Serves)));
        assert_eq!(c("AuthServiceStub"), Some(("AuthService".into(), ServiceRole::Calls)));
        assert_eq!(c("AuthServiceClient"), Some(("AuthService".into(), ServiceRole::Calls)));
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
    fn leaves_ordinary_names_alone() {
        // Names that merely end in one of the suffixes are not service artefacts.
        assert_eq!(c("DatabaseClient"), None);
        assert_eq!(c("Base"), None);
        assert_eq!(c("HttpStub"), None);
        assert_eq!(c("RegisterUser"), None);
        assert_eq!(c("NewClient"), None);
    }

    #[test]
    fn package_comes_from_the_schema_directory() {
        assert_eq!(
            package_of("srcpy/schema/orders_api/__init__.py"),
            Some("orders_api")
        );
        assert_eq!(
            package_of("srcgo/schema/orders_fe/service_auth_grpc.pb.go"),
            Some("orders_fe")
        );
        // A file sitting directly under schema/ belongs to no package.
        assert_eq!(package_of("srcpy/schema/openapi.py"), None);
        assert_eq!(package_of("srcpy/domains/orders/x.py"), None);
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
