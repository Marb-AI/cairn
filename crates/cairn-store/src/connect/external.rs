//! Running an external connector, and deciding what to believe from it.
//!
//! The public extension path, and the one with no privileges: the connector is a process
//! that prints facts, and the core decides which of them it can store. It never sees the
//! database, never learns a handle, and cannot reach the network from inside cairn's own
//! container.
//!
//! The protocol is deliberately boring — one JSON object per line on stdout — and the
//! validation is deliberately loud. Every failure is reported as a gap on every answer:
//! a malformed line, a kind the manifest never declared, a `scip:` reference this index
//! does not have, a non-zero exit. A connector that half-worked and said nothing would
//! leave answers quietly missing facts, which is the one failure mode this whole design
//! exists to prevent (D8).
//!
//! Determinism (D15, I3): nothing here calls a model, and the connector is required not
//! to touch the network. Inside cairn's Docker setup that is enforced by the container
//! it runs in; outside it, it is a documented requirement of the connector, and its own
//! determinism test is what holds it — two runs over the same inputs must produce the
//! same facts, which is also what makes the cache key meaningful.

use super::{Connector, ConnectorCtx, ConnectorReport, Manifest, Propagates};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// How long to wait for `--version`, which is not the connector's real work.
const VERSION_TIMEOUT_S: u64 = 10;

/// Limits on the input walk. Generous enough for a monorepo, bounded so a pathological
/// tree cannot turn `cairn index` into a filesystem crawl.
const WALK: crate::tree::Limits = crate::tree::Limits {
    max_depth: 24,
    max_files: 200_000,
};

/// One line of the connector's output.
///
/// Exactly one of the three is set. `deny_unknown_fields` so a connector written against
/// a later protocol is told its facts were rejected rather than having them silently
/// dropped.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    #[serde(default)]
    node: Option<NodeFact>,
    #[serde(default)]
    edge: Option<EdgeFact>,
    #[serde(default)]
    unknown: Option<String>,
}

/// A node the connector declares: a domain, a contract, an entity — something that is
/// not a code symbol.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeFact {
    /// Namespaced by connector name: `w17:domain/orders`.
    pub id: String,
    /// One of the manifest's `node_kinds`.
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    /// `file:line` in the repository, where the declaration lives.
    #[serde(default)]
    pub anchor: Option<String>,
}

/// An edge the connector declares, between its own nodes or into code.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeFact {
    pub from: String,
    pub to: String,
    /// One of the manifest's `edge_kinds`.
    pub kind: String,
}

/// Everything one run produced, after parsing and before anything is stored.
#[derive(Debug, Default)]
pub struct Facts {
    pub nodes: Vec<NodeFact>,
    pub edges: Vec<EdgeFact>,
    /// What the connector itself reported it could not determine.
    pub unknown: Vec<String>,
    /// What the core rejected, with the line number that produced it.
    pub rejected: Vec<String>,
}

/// Parse JSONL into facts, validating every line against the manifest.
///
/// Rejection is per line and never fatal: one bad line costs that line, is reported with
/// its number, and the rest of the run still contributes. A whole run thrown away for a
/// typo would make a connector useless at exactly the moment it is being written.
pub fn parse_facts(stdout: &str, manifest: &Manifest) -> Facts {
    let mut facts = Facts::default();
    for (i, raw) in stdout.lines().enumerate() {
        let n = i + 1;
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let parsed: Line = match serde_json::from_str(line) {
            Ok(l) => l,
            Err(e) => {
                facts
                    .rejected
                    .push(format!("line {n} is not a fact this protocol knows ({e})"));
                continue;
            }
        };
        match (parsed.node, parsed.edge, parsed.unknown) {
            (Some(node), None, None) => {
                if !manifest.node_kinds.contains(&node.kind) {
                    facts.rejected.push(format!(
                        "line {n}: node kind {:?} is not in the manifest's node_kinds",
                        node.kind
                    ));
                    continue;
                }
                if !is_namespaced(&node.id, &manifest.name) {
                    facts.rejected.push(format!(
                        "line {n}: node id {:?} is not namespaced as `{}:...`",
                        node.id, manifest.name
                    ));
                    continue;
                }
                facts.nodes.push(node);
            }
            (None, Some(edge), None) => {
                if !manifest.edge_kinds.contains_key(&edge.kind) {
                    facts.rejected.push(format!(
                        "line {n}: edge kind {:?} is not in the manifest's edge_kinds",
                        edge.kind
                    ));
                    continue;
                }
                for end in [&edge.from, &edge.to] {
                    if !is_namespaced(end, &manifest.name) && !end.starts_with("scip:") {
                        facts.rejected.push(format!(
                            "line {n}: edge end {end:?} is neither `{}:...` nor `scip:...`",
                            manifest.name
                        ));
                    }
                }
                facts.edges.push(edge);
            }
            (None, None, Some(u)) => facts.unknown.push(u),
            _ => facts.rejected.push(format!(
                "line {n} sets more than one of node, edge and unknown, so what it \
                 states is ambiguous"
            )),
        }
    }
    // An edge whose end was rejected above is still in `edges`; dropping it there would
    // have meant two passes over the same line. Removed here, once, so nothing half
    // validated reaches the store.
    let name = manifest.name.clone();
    facts.edges.retain(|e| {
        [&e.from, &e.to]
            .iter()
            .all(|end| is_namespaced(end, &name) || end.starts_with("scip:"))
    });
    facts
}

fn is_namespaced(id: &str, connector: &str) -> bool {
    id.strip_prefix(connector)
        .and_then(|rest| rest.strip_prefix(':'))
        .map(|rest| !rest.is_empty())
        .unwrap_or(false)
}

/// An external connector: a manifest and the process it describes.
pub struct ExternalConnector {
    manifest: Manifest,
}

impl ExternalConnector {
    pub fn new(manifest: Manifest) -> ExternalConnector {
        ExternalConnector { manifest }
    }

    /// Where this connector's cached output lives, beside the index.
    ///
    /// Beside rather than inside: the cache has to survive a reindex, which drops and
    /// rebuilds every table. The architecture's CAS (D5, D6) is where this belongs and
    /// it does not exist yet; when it does, this is one function to move.
    fn cache_paths(db: &Path, name: &str) -> (PathBuf, PathBuf) {
        let dir = db
            .parent()
            .map(|d| d.join("cache/connectors"))
            .unwrap_or_else(|| PathBuf::from("cache/connectors"));
        (
            dir.join(format!("{name}.jsonl")),
            dir.join(format!("{name}.key")),
        )
    }

    /// blake3 of everything that decides whether the facts could have changed: the
    /// manifest, the connector's own version, and the contents of every file its
    /// `inputs` match.
    ///
    /// Content hashes rather than mtimes, for the same reason the index uses them: a
    /// checkout rewrites every mtime and changes nothing.
    pub fn cache_key(&self, repo: &Path, version: Option<&str>) -> String {
        let mut h = blake3::Hasher::new();
        h.update(&self.manifest.digest);
        h.update(version.unwrap_or("").as_bytes());
        for path in self.inputs(repo) {
            let rel = path.to_string_lossy().to_string();
            h.update(rel.as_bytes());
            match std::fs::read(repo.join(&path)) {
                Ok(bytes) => {
                    h.update(blake3::hash(&bytes).as_bytes());
                }
                // An unreadable input is itself part of the key: it may become readable,
                // and the run that sees it should not be served from a cache built
                // without it.
                Err(_) => {
                    h.update(b"<unreadable>");
                }
            }
        }
        h.finalize().to_hex().to_string()
    }

    /// The repository files this connector's `inputs` match.
    pub fn inputs(&self, repo: &Path) -> Vec<PathBuf> {
        if self.manifest.inputs.is_empty() {
            return Vec::new();
        }
        let globs = self.manifest.inputs.clone();
        crate::tree::walk(repo, WALK, &mut |rel| {
            super::manifest::matches_any(rel, &globs)
        })
    }

    /// What the connector says its version is, for the cache key and for `status`.
    ///
    /// The command with `--version` appended, not just its first word: `["w17ctl",
    /// "cairn", "facts"]` is one invocation, and asking `w17ctl --version` would report
    /// the version of a wrapper rather than of the thing producing facts.
    ///
    /// Best-effort: a connector that does not understand the flag is not broken, it just
    /// contributes nothing to the cache key. The run that matters is the one below.
    fn version(&self, repo: &Path) -> Option<String> {
        let (argv0, rest) = self.manifest.command.split_first()?;
        let out = run_with_timeout(
            Command::new(argv0)
                .args(rest)
                .arg("--version")
                .current_dir(repo)
                .env("CAIRN_PROTOCOL", super::manifest::PROTOCOL.to_string())
                .env("CAIRN_REPO", repo),
            VERSION_TIMEOUT_S,
        )
        .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

/// Run a command, killing it if it outlives the timeout.
///
/// `std` has no timed wait, so this polls. The alternative is a thread per child and a
/// channel, which for a once-per-index call buys nothing and can leak a thread when the
/// child outlives the process.
fn run_with_timeout(cmd: &mut Command, timeout_s: u64) -> Result<std::process::Output> {
    use std::time::{Duration, Instant};
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {:?}", cmd.get_program()))?;
    // Empty stdin, closed immediately: the protocol says a connector reads nothing, and
    // a child waiting on a pipe nobody writes to would hang until the timeout.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"");
    }
    let deadline = Instant::now() + Duration::from_secs(timeout_s.max(1));
    loop {
        if let Some(_status) = child.try_wait()? {
            // `wait_with_output` reaps the pipes; the child is already gone.
            return Ok(child.wait_with_output()?);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let out = child.wait_with_output()?;
            anyhow::bail!(
                "timed out after {timeout_s}s{}",
                first_line(&String::from_utf8_lossy(&out.stderr))
                    .map(|l| format!(": {l}"))
                    .unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| l.to_string())
}

impl Connector for ExternalConnector {
    fn name(&self) -> &str {
        &self.manifest.name
    }

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, ctx: &mut ConnectorCtx) -> Result<ConnectorReport> {
        let mut report = ConnectorReport::default();
        let name = &self.manifest.name;
        let Some(repo) = ctx.repo else {
            report.unknown.push(format!(
                "not run: this index was built from SCIP files with no repository, and a \
                 connector reads the tree. Everything {name} contributes is missing here"
            ));
            return Ok(report);
        };
        let db = ctx.store.path().to_path_buf();
        let (cached_facts, cached_key) = Self::cache_paths(&db, name);

        let version = self.version(repo);
        let key = self.cache_key(repo, version.as_deref());
        let previous_key = std::fs::read_to_string(&cached_key)
            .ok()
            .map(|k| k.trim().to_string());

        // Served from cache only when nothing that could change the facts has changed:
        // the manifest, the connector's version, or any file its inputs match.
        let fresh = previous_key.as_deref() == Some(key.as_str());
        let stdout = if fresh {
            match std::fs::read_to_string(&cached_facts) {
                Ok(text) => {
                    report.log.push(format!(
                        "{name}: unchanged inputs, {} fact line(s) from cache",
                        text.lines().filter(|l| !l.trim().is_empty()).count()
                    ));
                    text
                }
                // The key is there and the facts are not. Rerun rather than report
                // nothing: a missing cache file is a cache problem, not a fact.
                Err(_) => self.run_fresh(repo, &mut report, &cached_facts, &cached_key, &key),
            }
        } else {
            self.run_fresh(repo, &mut report, &cached_facts, &cached_key, &key)
        };

        let facts = parse_facts(&stdout, &self.manifest);
        report.unknown.extend(facts.unknown.iter().cloned());
        report.unknown.extend(facts.rejected.iter().cloned());
        let applied = ctx
            .store
            .apply_declared_facts(&self.manifest, &facts, Some(repo))?;
        report.unknown.extend(applied.unknown);
        report.facts = applied.nodes + applied.edges;
        report.log.push(format!(
            "{name}: {} node(s), {} edge(s){}",
            applied.nodes,
            applied.edges,
            if facts.rejected.is_empty() {
                String::new()
            } else {
                format!(", {} line(s) rejected", facts.rejected.len())
            }
        ));
        report.version = version;
        Ok(report)
    }
}

impl ExternalConnector {
    /// Run the process, and on success replace the cache.
    ///
    /// A failed run keeps whatever was cached: stale facts a reader is told about beat no
    /// facts a reader has to notice are missing. The failure itself becomes a gap on
    /// every answer, which is what makes the staleness readable rather than silent.
    fn run_fresh(
        &self,
        repo: &Path,
        report: &mut ConnectorReport,
        cached_facts: &Path,
        cached_key: &Path,
        key: &str,
    ) -> String {
        let mut cmd = Command::new(&self.manifest.command[0]);
        cmd.args(&self.manifest.command[1..])
            // The repository root, so a connector resolves its own relative paths the
            // way the index records them.
            .current_dir(repo)
            .env("CAIRN_PROTOCOL", super::manifest::PROTOCOL.to_string())
            .env("CAIRN_REPO", repo);
        let outcome = run_with_timeout(&mut cmd, self.manifest.timeout_s);
        let why = match outcome {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                if let Some(dir) = cached_facts.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                // Facts first, then the key: a crash between the two leaves a key that
                // does not match, which costs one rerun. The other order would serve
                // yesterday's facts as today's.
                if std::fs::write(cached_facts, &text).is_ok() {
                    let _ = std::fs::write(cached_key, key);
                }
                return text;
            }
            Ok(out) => format!(
                "exited {}{}",
                out.status.code().unwrap_or(-1),
                first_line(&String::from_utf8_lossy(&out.stderr))
                    .map(|l| format!(": {l}"))
                    .unwrap_or_default()
            ),
            Err(e) => format!("{e:#}"),
        };
        self.failed(report, cached_facts, &why)
    }

    /// Record a failed run and fall back to the cached facts, if there are any.
    fn failed(&self, report: &mut ConnectorReport, cached_facts: &Path, why: &str) -> String {
        let name = &self.manifest.name;
        report.failed = true;
        match std::fs::read_to_string(cached_facts) {
            Ok(text) => {
                report.unknown.push(format!(
                    "failed ({why}), so these facts are the previous run's and may be \
                     stale - nothing from this run was used"
                ));
                report
                    .log
                    .push(format!("{name}: failed, kept cached facts"));
                text
            }
            Err(_) => {
                report.unknown.push(format!(
                    "failed ({why}) and has no cached facts, so everything it \
                     contributes is missing from every answer here"
                ));
                report.log.push(format!("{name}: failed, nothing cached"));
                String::new()
            }
        }
    }
}

/// What applying one run's facts changed.
#[derive(Debug, Default)]
pub struct Applied {
    pub nodes: i64,
    pub edges: i64,
    pub unknown: Vec<String>,
}

impl crate::Store {
    /// Store one connector run's facts, replacing everything it said before.
    ///
    /// The namespace is dropped and rebuilt, which is what keeps declared nodes from
    /// drifting from their source: a node the connector stopped emitting disappears
    /// rather than lingering as a fact nobody states any more.
    pub fn apply_declared_facts(
        &mut self,
        manifest: &Manifest,
        facts: &Facts,
        repo: Option<&Path>,
    ) -> Result<Applied> {
        let mut applied = Applied::default();
        self.drop_declared(&manifest.name)?;

        // Node ids this run declared, so an edge can tell a declared end from a code one
        // and an edge naming a node that was never declared can be reported.
        let mut node_ids: BTreeMap<String, i64> = BTreeMap::new();
        for node in &facts.nodes {
            let id = self.declare_node(manifest, node, repo)?;
            node_ids.insert(node.id.clone(), id);
            applied.nodes += 1;
        }

        let mut unresolved: Vec<String> = Vec::new();
        let mut undeclared: Vec<String> = Vec::new();
        for edge in &facts.edges {
            let kind = manifest
                .edge_kinds
                .get(&edge.kind)
                .copied()
                .unwrap_or(Propagates::None);
            let kind_id = self.declared_edge_kind(&manifest.name, &edge.kind, kind)?;
            let mut end = |id: &str| -> Result<Option<i64>> {
                if let Some(found) = node_ids.get(id) {
                    return Ok(Some(*found));
                }
                if let Some(scip) = id.strip_prefix("scip:") {
                    // Kept as *expected* rather than resolved (architecture 7.2): the
                    // symbol gets a row with no definition, exactly as a third-party
                    // reference from SCIP does, and the gap is reported.
                    let (symbol_id, resolved) = self.symbol_for_scip(scip)?;
                    if !resolved {
                        unresolved.push(scip.to_string());
                    }
                    return Ok(Some(symbol_id));
                }
                undeclared.push(id.to_string());
                Ok(None)
            };
            let (Some(from), Some(to)) = (end(&edge.from)?, end(&edge.to)?) else {
                continue;
            };
            self.insert_declared_edge(from, to, kind_id, &manifest.name)?;
            applied.edges += 1;
        }

        unresolved.sort();
        unresolved.dedup();
        if !unresolved.is_empty() {
            applied.unknown.push(format!(
                "{} code reference(s) are not in this index, so their edges are expected \
                 rather than resolved ({}{}). The code may not be indexed, or the \
                 generated artefact may not be on disk",
                unresolved.len(),
                unresolved
                    .iter()
                    .take(3)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                if unresolved.len() > 3 { ", ..." } else { "" }
            ));
        }
        undeclared.sort();
        undeclared.dedup();
        if !undeclared.is_empty() {
            applied.unknown.push(format!(
                "{} edge(s) name a node this run never declared ({}{}), so they were \
                 dropped",
                undeclared.len(),
                undeclared
                    .iter()
                    .take(3)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                if undeclared.len() > 3 { ", ..." } else { "" }
            ));
        }
        // Handles for everything new, in one pass, by the same shortest-unique-prefix
        // rule every other symbol gets. A declared node without one would be a node no
        // command could be pointed at.
        crate::schema::assign_handles(&self.conn)?;
        Ok(applied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn manifest() -> Manifest {
        let mut edge_kinds = BTreeMap::new();
        edge_kinds.insert("implemented_by".to_string(), Propagates::Dependency);
        edge_kinds.insert("part_of".to_string(), Propagates::Containment);
        Manifest {
            name: "w17".to_string(),
            version: None,
            inputs: vec!["**/*.proto".to_string()],
            tier: super::super::Tier::Declared,
            node_kinds: vec!["domain".to_string(), "rpc".to_string()],
            edge_kinds,
            command: vec!["true".to_string()],
            timeout_s: 5,
            digest: vec![1, 2, 3],
        }
    }

    #[test]
    fn good_facts_parse_and_bad_lines_are_reported_with_their_number() {
        let stdout = concat!(
            r#"{"node":{"id":"w17:domain/orders","kind":"domain","name":"orders"}}"#,
            "\n",
            r#"{"node":{"id":"w17:rpc/Create","kind":"rpc","anchor":"proto/orders.proto:14"}}"#,
            "\n",
            r#"{"edge":{"from":"w17:rpc/Create","to":"w17:domain/orders","kind":"part_of"}}"#,
            "\n",
            r#"{"edge":{"from":"w17:rpc/Create","to":"scip:a b c","kind":"implemented_by"}}"#,
            "\n",
            r#"{"unknown":"rpc Refund: no handler found"}"#,
            "\n",
            "not json at all\n",
            r#"{"node":{"id":"w17:thing/x","kind":"invented"}}"#,
            "\n",
            r#"{"edge":{"from":"w17:rpc/Create","to":"w17:domain/orders","kind":"invented"}}"#,
            "\n",
            r#"{"node":{"id":"nothing-namespaced","kind":"domain"}}"#,
            "\n",
            r#"{"edge":{"from":"w17:rpc/Create","to":"elsewhere:thing","kind":"part_of"}}"#,
            "\n",
        );
        let facts = parse_facts(stdout, &manifest());
        assert_eq!(facts.nodes.len(), 2, "two declarable nodes");
        assert_eq!(facts.edges.len(), 2, "two declarable edges");
        assert_eq!(facts.unknown, vec!["rpc Refund: no handler found"]);
        assert_eq!(facts.rejected.len(), 5, "{:#?}", facts.rejected);
        // The line number is the point: "a node kind was wrong" is not actionable.
        assert!(facts.rejected.iter().any(|r| r.starts_with("line 6")));
        assert!(facts.rejected.iter().any(|r| r.contains("node kind")));
        assert!(facts.rejected.iter().any(|r| r.contains("edge kind")));
        assert!(facts.rejected.iter().any(|r| r.contains("namespaced")));
    }

    #[test]
    fn a_line_claiming_two_things_at_once_is_rejected() {
        let stdout = r#"{"node":{"id":"w17:domain/x","kind":"domain"},"unknown":"and also this"}"#;
        let facts = parse_facts(stdout, &manifest());
        assert!(facts.nodes.is_empty());
        assert_eq!(facts.rejected.len(), 1);
        assert!(facts.rejected[0].contains("ambiguous"));
    }

    #[test]
    fn a_field_the_protocol_does_not_know_is_rejected_rather_than_ignored() {
        // A connector written against a later protocol has to be told, or it will look
        // as though its facts were accepted.
        let stdout = r#"{"node":{"id":"w17:domain/x","kind":"domain","colour":"red"}}"#;
        let facts = parse_facts(stdout, &manifest());
        assert!(facts.nodes.is_empty());
        assert_eq!(facts.rejected.len(), 1);
    }
}
