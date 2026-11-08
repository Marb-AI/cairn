//! The connector boundary: who may contribute facts to the index, and how.
//!
//! The core is a graph of symbols, edges and the honesty that goes with them. Everything
//! that knows what a *protobuf service* is, or a *compose file*, or a cron entry, is a
//! convention about one ecosystem — and the architecture has planned this split from the
//! start (D16, §1.1, §7.1: "a binder is a small plugin that produces edges"). The
//! generality audit records the debt: three modules that read the world through one
//! stack's conventions while the core claims to be language-agnostic.
//!
//! So the conventions sit behind a connector. The core learns from a manifest what kinds
//! of node and edge a connector contributes and how each kind propagates; it never learns
//! what any of them *mean*.
//!
//! Two shapes of connector, and the difference is deliberate:
//!
//!   * **In-process**, which is what `DefaultConnector` is. It is cairn's own behaviour,
//!     moved rather than rewritten, and it keeps privileged access to the store: it
//!     writes `service_links`, the deployment tables and the rest exactly as before. A
//!     facts-only path for it would have been a rewrite, and a rewrite is not something
//!     to do in the same change as a move.
//!   * **External processes**, which are the public extension path and have no store
//!     access at all: they emit facts and the core validates them. That is phase 4.
//!
//! What a connector must never do is call a model or the network (D15). `cairn index`
//! runs offline with no key, and every connector runs inside that promise.

pub mod declared;
pub mod external;
pub mod manifest;

pub use declared::DeclaredNode;
pub use external::{EdgeFact, ExternalConnector, Facts, NodeFact};

use crate::Store;
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

/// How a change travels along an edge kind.
///
/// The core reads this and nothing else about a kind. `emits` and `writes` mean nothing
/// here; what matters is whether a change to the target reaches the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Propagates {
    /// A change to the target affects the source. Blast radius, `affects`, `reaches` and
    /// `path` traverse these the way they traverse calls.
    Dependency,
    /// Grouping: an rpc is part of a domain. Used for scoping and aggregation, never
    /// traversed for impact — a change to one member of a container does not reach its
    /// siblings just because they share it.
    Containment,
    /// Display only.
    None,
}

impl Propagates {
    pub fn parse(s: &str) -> Option<Propagates> {
        match s {
            "dependency" => Some(Propagates::Dependency),
            "containment" => Some(Propagates::Containment),
            "none" => Some(Propagates::None),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Propagates::Dependency => "dependency",
            Propagates::Containment => "containment",
            Propagates::None => "none",
        }
    }
}

/// Which tier a connector's facts land in.
///
/// One variant today, and it is still an enum: the tier is a property of the connector
/// rather than of the core, and a future connector that could only produce candidates
/// would need to say so rather than having its guesses filed as exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Stated by a deterministic tool reading declarations it owns: exact, and rebuilt
    /// on every index. `EdgeSource::Declared`.
    Declared,
}

impl Tier {
    pub fn parse(s: &str) -> Option<Tier> {
        match s {
            "declared" => Some(Tier::Declared),
            _ => None,
        }
    }
}

/// What a connector declares about itself.
///
/// Read by the core to decide what it will accept. A fact whose kind is not in the
/// manifest is rejected and reported rather than stored, because a connector that can
/// invent kinds at run time is a connector whose output cannot be reasoned about.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    /// What the connector reports as its own version, for `cairn status`. `None` for one
    /// that has no notion of it.
    pub version: Option<String>,
    /// Globs over the repository. A change under one of these is what makes this
    /// connector's facts stale.
    pub inputs: Vec<String>,
    pub tier: Tier,
    pub node_kinds: Vec<String>,
    /// Edge kind name -> how it propagates.
    pub edge_kinds: BTreeMap<String, Propagates>,
    /// blake3-128 of the manifest as written, for the cache key: a manifest that
    /// changed is a connector whose facts may have changed, whatever its inputs did.
    pub digest: Vec<u8>,
    /// What to run, for an external connector. Empty for an in-process one.
    pub command: Vec<String>,
    /// How long the command may take before it is killed and the run treated as failed.
    pub timeout_s: u64,
}

/// What a connector is given to work with.
///
/// The in-process connector takes the store; an external one will never see it. Kept as
/// one type so the core's pipeline does not branch on which kind it is running.
pub struct ConnectorCtx<'a> {
    pub store: &'a mut Store,
    /// Repository root. `None` when SCIP files were ingested without a tree to read, in
    /// which case a connector that needs the tree reports that it could not run.
    pub repo: Option<&'a Path>,
}

/// What a connector run produced.
#[derive(Debug, Default)]
pub struct ConnectorReport {
    /// What this connector could not determine, in its own words. Merged into the
    /// `unknown` section of every answer (D8): a gap that is only visible at index time
    /// is a gap nobody reads.
    pub unknown: Vec<String>,
    /// Progress lines for `cairn index`, in the order they should be printed. Returned
    /// rather than printed so a connector does not have to know whether anybody is
    /// watching — the daemon reruns these with nothing attached to stdout.
    pub log: Vec<String>,
    /// How many facts this run contributed, for `cairn status`.
    pub facts: i64,
    /// What the connector reported as its own version, when it was asked.
    pub version: Option<String>,
    /// The run failed and whatever facts are in place are the previous run's. Reported
    /// by `cairn status`; the reason is in `unknown` and reaches every answer.
    pub failed: bool,
}

pub trait Connector {
    fn name(&self) -> &str;
    fn manifest(&self) -> &Manifest;
    fn run(&self, ctx: &mut ConnectorCtx) -> Result<ConnectorReport>;
}

/// Today's behaviour, behind the boundary: protobuf service links, compose deployment,
/// cron and on-demand entrypoints.
///
/// This is the connector that ships, and it is why the split is worth doing: everything
/// it knows is a convention about one stack — `protoc-gen-go-grpc` naming, a compose
/// file, a crontab — and none of it belongs in a core that answers questions about any
/// language. The code is moved, not rewritten: it still reads the rule pack
/// (`rules/default.yaml`, architecture D16) and still writes the store directly.
pub struct DefaultConnector {
    manifest: Manifest,
}

impl Default for DefaultConnector {
    fn default() -> Self {
        DefaultConnector::new()
    }
}

impl DefaultConnector {
    pub fn new() -> DefaultConnector {
        let mut edge_kinds = BTreeMap::new();
        // The two relations this connector contributes, named the way an external one
        // would have to name them. `serves` and `calls` are how a service boundary is
        // crossed, and crossing one is how a change travels — so both propagate as
        // dependencies, which is what the walks already do with them.
        edge_kinds.insert("serves".to_string(), Propagates::Dependency);
        edge_kinds.insert("calls_rpc".to_string(), Propagates::Dependency);
        DefaultConnector {
            manifest: Manifest {
                name: "default".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
                // What this connector reads. A compose file or a crontab changing means
                // its facts are stale, which is what the daemon keys its reruns on.
                inputs: vec![
                    "compose*.y*ml".to_string(),
                    "docker-compose*.y*ml".to_string(),
                    "**/Dockerfile".to_string(),
                    "**/crontab*".to_string(),
                    "**/*.proto".to_string(),
                ],
                tier: Tier::Declared,
                node_kinds: vec!["service".to_string(), "rpc".to_string()],
                edge_kinds,
                // In-process: there is nothing to spawn, nothing to time out, and no
                // cache - the work is a few queries and runs with the index.
                digest: Vec::new(),
                command: Vec::new(),
                timeout_s: 0,
            },
        }
    }

    /// Compose files this connector reads, in the order a later one overrides an earlier.
    const COMPOSE_FILES: &'static [&'static str] = &[
        "compose.yaml",
        "compose.yml",
        "docker-compose.yml",
        "compose.local.yaml",
        "compose.override.yaml",
    ];
}

impl Connector for DefaultConnector {
    fn name(&self) -> &str {
        &self.manifest.name
    }

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, ctx: &mut ConnectorCtx) -> Result<ConnectorReport> {
        let mut report = ConnectorReport::default();

        // Cross-language links, derived once the whole index is in place: the two sides
        // of a service boundary usually arrive from different SCIP files.
        let links = ctx.store.link_services()?;
        report.log.push(format!(
            "services: {} gRPC services, {} serve links, {} call links",
            links.services, links.serves, links.calls
        ));
        report.facts += (links.serves + links.calls) as i64;

        // Deployment needs a tree: a compose file is not in any SCIP index.
        let Some(root) = ctx.repo else {
            report.unknown.push(
                "the deployment layer was not built: this index was ingested from SCIP \
                 files with no repository to read, so no compose file, Dockerfile or cron \
                 entry was seen. `runs`, `affects`, `topology` and `entrypoints` answer \
                 from nothing"
                    .to_string(),
            );
            return Ok(report);
        };

        let topo = crate::deploy::parse_compose(root, Self::COMPOSE_FILES)?;
        if topo.services.is_empty() {
            return Ok(report);
        }
        let d = ctx.store.link_deployment(root, &topo)?;
        report.log.push(format!(
            "deploy:   {} services from {}, {} with a resolved entrypoint{}",
            d.services,
            topo.sources.join(" + "),
            d.with_entrypoint,
            if d.on_demand > 0 {
                format!(", {} on-demand entrypoint(s) from cron", d.on_demand)
            } else {
                String::new()
            }
        ));
        // Naming them rather than counting: an unresolved entrypoint makes live code look
        // unreachable, which is the failure mode architecture 8.4 warns about.
        if !d.unresolved.is_empty() {
            report
                .log
                .push(format!("          unresolved: {}", d.unresolved.join(", ")));
        }
        report.facts += d.services as i64;
        Ok(report)
    }
}

/// One connector's last run, as `cairn status` reports it.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct ConnectorRun {
    pub name: String,
    pub version: Option<String>,
    /// Unix seconds. `null` for a connector that is declared but has not run.
    pub ran_at: Option<i64>,
    pub facts: i64,
    /// What it could not determine. Carried on every answer, not only here.
    pub unknown: Vec<String>,
    /// The last run failed, so these facts are the previous run's.
    pub failed: bool,
}

impl Store {
    /// Record what a connector run produced, replacing whatever it said before.
    pub fn record_connector_run(
        &mut self,
        manifest: &Manifest,
        report: &ConnectorReport,
        ran_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO connectors(name, version, ran_at, facts, failed, unknown)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(name) DO UPDATE SET version = excluded.version,
                                             ran_at  = excluded.ran_at,
                                             facts   = excluded.facts,
                                             failed  = excluded.failed,
                                             unknown = excluded.unknown",
            rusqlite::params![
                manifest.name,
                // What the connector itself reported, when it was asked, and otherwise
                // what the manifest knows. An external connector's version is the one
                // that matters: it is in the cache key.
                report.version.clone().or_else(|| manifest.version.clone()),
                ran_at,
                report.facts,
                report.failed as i64,
                if report.unknown.is_empty() {
                    None
                } else {
                    Some(report.unknown.join("\n"))
                },
            ],
        )?;
        Ok(())
    }

    /// Every connector this index was built with.
    pub fn connector_runs(&self) -> Result<Vec<ConnectorRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, version, ran_at, facts, unknown, failed FROM connectors
              ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ConnectorRun {
                name: r.get(0)?,
                version: r.get(1)?,
                ran_at: r.get(2)?,
                facts: r.get(3)?,
                unknown: r
                    .get::<_, Option<String>>(4)?
                    .map(|s| s.lines().map(|l| l.to_string()).collect())
                    .unwrap_or_default(),
                failed: r.get::<_, i64>(5)? != 0,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// What the connectors behind this index could not determine.
    ///
    /// Read once per process and merged into every answer, the same way a partial index
    /// is. A gap that only `cairn index` printed is a gap the reader of an answer never
    /// sees, and an answer that does not carry its gaps is the silent error D8 is about.
    pub fn connector_unknown(&self) -> Result<Vec<String>> {
        Ok(self
            .connector_runs()?
            .into_iter()
            .flat_map(|c| {
                let who = c.name;
                c.unknown
                    .into_iter()
                    .map(move |u| format!("connector {who}: {u}"))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_is_recorded_and_read_back_with_what_it_could_not_see() {
        let mut store = Store::open_in_memory().unwrap();
        let c = DefaultConnector::new();
        let report = ConnectorReport {
            unknown: vec!["no compose file".to_string()],
            facts: 7,
            ..Default::default()
        };
        store
            .record_connector_run(c.manifest(), &report, 1_762_000_000)
            .unwrap();
        let runs = store.connector_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].name, "default");
        assert_eq!(runs[0].facts, 7);
        assert_eq!(runs[0].ran_at, Some(1_762_000_000));
        // Attributed, because an unattributed gap cannot be chased: the reader has to
        // know which connector could not see the thing.
        assert_eq!(
            store.connector_unknown().unwrap(),
            vec!["connector default: no compose file".to_string()]
        );
    }

    #[test]
    fn a_second_run_replaces_the_first_rather_than_accumulating() {
        // Connector facts are derived data, rebuilt on every index. A row per run would
        // make `cairn status` list the same connector five times and `unknown` repeat a
        // gap that was fixed two builds ago.
        let mut store = Store::open_in_memory().unwrap();
        let c = DefaultConnector::new();
        for facts in [1, 2] {
            let report = ConnectorReport {
                facts,
                ..Default::default()
            };
            store
                .record_connector_run(c.manifest(), &report, facts)
                .unwrap();
        }
        let runs = store.connector_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].facts, 2);
        assert!(store.connector_unknown().unwrap().is_empty());
    }
}
