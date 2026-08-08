//! Deployment topology: which service runs which code.
//!
//! The strongest finding in the coverage analysis was that **this cannot be answered
//! from the filesystem**. Fifteen compose services in the target repo are built from two
//! source trees, so `srcpy/domains/orders/...` belongs to `orders-grpc`,
//! `orders-api`, `catalog-pipeline` or none of them, and nothing in the directory
//! layout says which. Only reachability from each service's entrypoint does.
//!
//! That is the same shape as the two question classes the tool measurably wins:
//! a bounded question answered from an edge the caller cannot see (the measurement record).
//!
//! The chain, and every link is a parse rather than a guess:
//!
//! ```text
//!   compose service          command: python3 -m domains.orders.grpc.server
//!     -> launcher rule       python -m <mod>  ->  <mod>/__main__.py or <mod>.py
//!     -> entrypoint symbol
//!     -> call graph          everything reachable from it runs in this service
//! ```
//!
//! For Go the command names a binary, so there is one more hop through the Dockerfile:
//! `/bin/grpcserver` <- `COPY --from=builder /out/grpcserver` <- `RUN xx-go build -o
//! /out/grpcserver ./cmd/grpcserver/server.go`.
//!
//! Anchors and merge keys are load-bearing here, not decoration: in this repo every
//! shared definition lives in an `x-` block and services pull it in with `<<:
//! *base-service`, so a parser that does not resolve them gets no build context and no
//! command at all.

use crate::Store;
use anyhow::{Context, Result};
use rusqlite::params;
use serde_yaml::Value;
use std::collections::HashMap;
use std::path::Path;

/// One row of the stored deployment table: name, start command, entry file path, ports,
/// and whether the entry file was resolved to a symbol at all.
///
/// A tuple rather than a struct because it crosses into `cairn-fmt` only to be rendered,
/// and naming it is enough to say what the positions mean.
pub type DeployServiceRow = (String, Option<String>, Option<String>, String, bool);

/// How an on-demand entrypoint is named in `deploy_reach`.
///
/// Written by the reachability walk and read back by anything that wants to ask which
/// entrypoint reaches a symbol, so it is one string in one place: a label that differs
/// by a space between the writer and the reader joins on nothing and reports it as "no
/// entrypoint reaches this", which is the confident-and-wrong answer, not an error.
const ENTRY_LABEL_SQL: &str = "service || ' (' || coalesce('cron ' || schedule, 'on demand') \
                               || ': ' || command || ')'";

/// What starts a piece of code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// The service's own start command: it runs when the service comes up.
    Start,
    /// A schedule, held as the cron expression that carries it.
    Cron(String),
    /// Run when something asks: `docker exec`, a management command, by hand.
    OnDemand,
}

impl Trigger {
    pub fn label(&self) -> String {
        match self {
            Trigger::Start => "start".to_string(),
            Trigger::Cron(s) => format!("cron {s}"),
            Trigger::OnDemand => "on demand".to_string(),
        }
    }
}

/// How a service came to be attributed to a symbol, strongest first.
///
/// The distinction is part of the answer rather than decoration: "a call path reaches
/// this" and "the file this sits in is loaded there" are different claims, and a reader
/// who cannot tell them apart will act on the second as if it were the first.
#[derive(Debug, Clone)]
pub enum Attribution {
    /// A call path from the service's entrypoint reaches the symbol.
    Direct,
    /// Nothing calls the symbol statically, but its enclosing type is reached — the
    /// shape of a method invoked from a dispatch table.
    ///
    /// Boxed because the row is two orders of magnitude larger than the other two
    /// variants, and this enum is returned from a function called per symbol.
    ViaType(Box<crate::SymbolRow>),
    /// Neither, but a service loads the file it lives in. The shape of a framework route
    /// handler, which is registered by decorator and called from the server loop.
    ViaFile,
}

/// One way into the codebase, whatever opens it.
///
/// A service is not the unit here: a container with three cron jobs is one service and
/// four entrypoints, and asking "how does this code get run" means asking about the four.
#[derive(Debug, Clone)]
pub struct Entrypoint {
    pub service: String,
    pub trigger: Trigger,
    /// The command as the deployment states it. Absent for a service that declares none
    /// and takes its image default.
    pub command: Option<String>,
    /// Repo-relative path the command lands in, when it resolved. `None` is the case
    /// that matters: everything only this entrypoint runs looks unreachable.
    pub entry_path: Option<String>,
    /// The runner script an on-demand entry came from — the evidence for the whole
    /// chain, so an answer can be checked rather than believed.
    pub script: Option<String>,
    /// A container held open on purpose (`sleep infinity`, `tail -f /dev/null`).
    ///
    /// Distinct from an unresolved command, which is why it is here: both land in no
    /// code, but one is an answer and the other is a failure to get one. Reporting an
    /// idle sidecar as an entrypoint that "did not resolve" trains a reader to skim the
    /// `unknown:` line, and then the real ones go past unread.
    pub idle: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Service {
    pub name: String,
    /// Start command, after compose `command:` overrides the image's own.
    pub command: Option<String>,
    /// Build context directory, relative to the repo root.
    pub build_context: Option<String>,
    /// Image, for services that are not built from source (postgres, redis).
    pub image: Option<String>,
    pub ports: Vec<String>,
    /// Extra DNS names. Without these an env var pointing at one service by alias looks
    /// like it points nowhere.
    pub aliases: Vec<String>,
    pub depends_on: Vec<String>,
    /// Repo path a container path maps to, from a bind mount. Overrides the Dockerfile's
    /// own COPY for local development, which is the case that matters here.
    pub mount: Option<(String, String)>,
}

impl Service {
    pub fn is_external(&self) -> bool {
        self.build_context.is_none() && self.image.is_some()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Topology {
    pub services: Vec<Service>,
    /// Files parsed, so an answer can say what it was built from.
    pub sources: Vec<String>,
}

/// Parse compose files, later ones overriding earlier ones.
pub fn parse_compose(repo: &Path, files: &[&str]) -> Result<Topology> {
    let mut topo = Topology::default();
    let mut merged: HashMap<String, Value> = HashMap::new();

    for name in files {
        let path = repo.join(name);
        if !path.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut doc: Value =
            serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        // Resolve `<<:` merge keys. Without this every service in this repo comes back
        // empty, because build context, env and init all arrive through a merge.
        doc.apply_merge().ok();
        topo.sources.push(name.to_string());

        let Some(services) = doc.get("services").and_then(|v| v.as_mapping()) else {
            continue;
        };
        for (k, v) in services {
            let Some(name) = k.as_str() else { continue };
            // Compose merges override files field by field; replacing the whole service
            // drops everything the base file defined. Measured: doing that left 16 of 17
            // services with no command at all, because compose.local.yaml redefines them
            // with only a volume or two.
            match merged.get_mut(name) {
                Some(existing) => deep_merge(existing, v),
                None => {
                    merged.insert(name.to_string(), v.clone());
                }
            }
        }
    }

    for (name, v) in merged {
        topo.services.push(service_from_value(&name, &v));
    }
    topo.services.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(topo)
}

/// Merge `overlay` onto `base`, recursing into mappings.
///
/// Scalars and sequences are replaced wholesale, which is compose's own rule: an
/// override file that lists `ports` replaces the base's ports rather than appending.
fn deep_merge(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Mapping(b), Value::Mapping(o)) => {
            for (k, ov) in o {
                match b.get_mut(k) {
                    Some(bv) => deep_merge(bv, ov),
                    None => {
                        b.insert(k.clone(), ov.clone());
                    }
                }
            }
        }
        (b, o) => *b = o.clone(),
    }
}

fn as_string_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Sequence(seq)) => seq
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn service_from_value(name: &str, v: &Value) -> Service {
    let mut svc = Service {
        name: name.to_string(),
        ..Default::default()
    };

    // `command` may be a string or an argv list; both mean the same thing here.
    svc.command = match v.get("command") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Sequence(seq)) => Some(
            seq.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    };

    svc.build_context = match v.get("build") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Mapping(m)) => m
            .get(Value::String("context".into()))
            .and_then(|c| c.as_str())
            .map(|s| s.trim_start_matches("./").to_string()),
        _ => None,
    };
    svc.image = v
        .get("image")
        .and_then(|i| i.as_str())
        .map(|s| s.to_string());
    svc.ports = as_string_list(v.get("ports"));
    svc.depends_on = match v.get("depends_on") {
        Some(Value::Sequence(seq)) => seq
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect(),
        // The long form is a mapping of service -> condition.
        Some(Value::Mapping(m)) => m
            .keys()
            .filter_map(|k| k.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    };

    if let Some(Value::Mapping(nets)) = v.get("networks") {
        for (_, net) in nets {
            svc.aliases.extend(as_string_list(net.get("aliases")));
        }
    }

    // A bind mount is authoritative for local development: it replaces whatever the
    // image copied in, so it, not the Dockerfile, says where the code really comes from.
    for m in as_string_list(v.get("volumes")) {
        let mut parts = m.split(':');
        let (Some(host), Some(container)) = (parts.next(), parts.next()) else {
            continue;
        };
        let host = host.trim_start_matches("./").trim_end_matches('/');
        if host.is_empty() || host.starts_with('.') {
            continue; // named volume or dotfile cache, not source
        }
        svc.mount = Some((
            host.to_string(),
            container.trim_end_matches('/').to_string(),
        ));
        break;
    }

    svc
}

/// Resolve a start command to the module or package that implements it.
///
/// A table of shapes, not a shell interpreter — the same `command_string` form as the
/// launcher rules in architecture 8.4. Returns a repo-relative hint, which the caller
/// matches against indexed files.
pub fn resolve_command(command: &str) -> Option<CommandTarget> {
    resolve_command_with(command, &crate::rules::Rules::default())
}

/// Resolve a start command against a rule pack.
///
/// The shapes used to be an `if` chain here; they are now data (architecture D16,
/// `src/rules/default.yaml`). Behaviour is unchanged for a repository that follows the
/// usual conventions — the pack is the same set of rules, written down.
pub fn resolve_command_with(command: &str, rules: &crate::rules::Rules) -> Option<CommandTarget> {
    use crate::rules::TargetRule;
    let cmd = command.trim();
    let words: Vec<&str> = cmd.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }

    for rule in &rules.commands {
        let matched_word = if let Some(suffix) = &rule.word_ends_with {
            match words.iter().position(|w| w.ends_with(suffix.as_str())) {
                Some(i) => Some(i),
                None => continue,
            }
        } else {
            None
        };
        let applies = rule.word_ends_with.is_some()
            || rule
                .argv0_starts_with
                .iter()
                .any(|p| words[0].starts_with(p.as_str()))
            || rule
                .argv0_ends_with
                .iter()
                .any(|p| words[0].ends_with(p.as_str()))
            || rule
                .command_starts_with
                .iter()
                .any(|p| cmd.starts_with(p.as_str()))
            || (rule.argv0_starts_with.is_empty()
                && rule.argv0_ends_with.is_empty()
                && rule.command_starts_with.is_empty());
        if !applies {
            continue;
        }

        match &rule.target {
            TargetRule::ModuleAfterFlag { flag } => {
                if let Some(i) = words.iter().position(|w| w == flag) {
                    if let Some(m) = words.get(i + 1) {
                        return Some(CommandTarget::PythonModule(m.to_string()));
                    }
                }
            }
            TargetRule::ModuleBeforeColon => {
                if let Some(spec) = words
                    .iter()
                    .skip(1)
                    .find(|w| w.contains(':') && !w.starts_with('-'))
                {
                    if let Some(m) = spec.split(':').next() {
                        if !m.is_empty() {
                            return Some(CommandTarget::PythonModule(m.to_string()));
                        }
                    }
                }
            }
            TargetRule::Idle => return Some(CommandTarget::Idle),
            TargetRule::SubcommandAfterMatch => {
                // The word *after* the match, not the last word: a runner script ends its
                // line with `"$@"` to forward arguments.
                if let Some(sub) = matched_word.and_then(|i| words.get(i + 1)) {
                    if !sub.starts_with('-') && !sub.starts_with('"') && !sub.starts_with('$') {
                        return Some(CommandTarget::DjangoCommand(sub.to_string()));
                    }
                }
            }
            TargetRule::AppAfterFlag { flag } => {
                if let Some(i) = words.iter().position(|w| w == flag) {
                    if let Some(app) = words.get(i + 1) {
                        return Some(CommandTarget::CeleryApp(app.to_string()));
                    }
                }
            }
            TargetRule::Binary => {
                // The basename: the Dockerfile names it after `build -o`, and the
                // container path it is copied to is a deployment detail.
                if words[0].starts_with('/') || words[0].starts_with("./") {
                    let name = words[0].rsplit('/').next().unwrap_or(words[0]);
                    if !name.is_empty() {
                        return Some(CommandTarget::Binary(name.to_string()));
                    }
                }
            }
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandTarget {
    PythonModule(String),
    Binary(String),
    DjangoCommand(String),
    CeleryApp(String),
    /// The container runs nothing: `tail -f /dev/null` and friends. Distinguishing this
    /// from "could not resolve" matters, because one is a gap and the other is a fact.
    Idle,
}

/// Map a built binary back to the package it was built from, by reading the Dockerfile.
///
/// Two hops, because the runtime stage copies from the build stage:
/// `COPY --from=builder /out/grpcserver /bin/grpcserver` and
/// `RUN xx-go build -o /out/grpcserver ./cmd/grpcserver/server.go`. The build wrapper is
/// `xx-go` here rather than `go`, which is why this matches on `build -o` rather than on
/// the command name.
pub fn binary_to_package(dockerfile: &str, binary: &str) -> Option<String> {
    let mut out_path: Option<String> = None;
    for line in dockerfile.lines() {
        let l = line.trim();
        if l.starts_with("COPY") && l.contains("--from=") && l.contains(binary) {
            // COPY --from=builder /out/X /bin/X
            let mut words = l.split_whitespace().rev();
            let dest = words.next()?;
            let src = words.next()?;
            if dest.ends_with(binary) {
                out_path = Some(src.to_string());
            }
        }
    }
    let target = out_path.unwrap_or_else(|| binary.to_string());
    for line in dockerfile.lines() {
        let l = line.trim();
        if !l.contains("build -o") {
            continue;
        }
        let words: Vec<&str> = l.split_whitespace().collect();
        let Some(i) = words.iter().position(|w| *w == "-o") else {
            continue;
        };
        let produced = words.get(i + 1)?;
        if *produced != target && !produced.ends_with(binary) {
            continue;
        }
        // The package or file follows the output path.
        return words
            .get(i + 2)
            .map(|p| p.trim_start_matches("./").trim_end_matches('/').to_string());
    }
    None
}

impl Store {
    /// Record the topology and attribute each service to its entrypoint symbol.
    pub fn link_deployment(&mut self, repo: &Path, topo: &Topology) -> Result<DeployStats> {
        let mut stats = DeployStats::default();
        self.conn
            .execute_batch("DELETE FROM deploy_services; DELETE FROM deploy_on_demand;")?;

        // Cron entries and runner scripts: what a service runs after it has started.
        for od in crate::ondemand::scan(repo)? {
            // Resolved in the build context of the service the cron line names, the same
            // one its start command uses. Without it `python -m alerting.dispatch` looks
            // for `/alerting/dispatch.py` while the index holds `srcpy/alerting/
            // dispatch.py`, and every `python -m` cron entry silently failed to resolve —
            // which is the failure this whole mechanism exists to prevent, one level in.
            // A management command resolves by suffix and never needed the context, which
            // is why the gap survived: the shape that was measured happened to be immune.
            let context = topo
                .services
                .iter()
                .find(|s| s.name == od.service)
                .and_then(|s| s.build_context.as_deref());
            let entry = match resolve_command_with(&od.command, &self.rules) {
                Some(target) => self.entry_file_for_target(repo, &target, context)?,
                None => None,
            };
            if entry.is_some() {
                stats.on_demand += 1;
            }
            self.conn.execute(
                "INSERT OR IGNORE INTO deploy_on_demand(service, schedule, script, command,
                                                        entry_file)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![od.service, od.schedule, od.script, od.command, entry],
            )?;
        }

        for svc in &topo.services {
            let entry = self.resolve_entry_file(repo, svc)?;
            let idle = svc
                .command
                .as_deref()
                .and_then(|c| resolve_command_with(c, &self.rules))
                .map(|t| t == CommandTarget::Idle)
                .unwrap_or(false);
            if entry.is_some() {
                stats.with_entrypoint += 1;
            } else if idle {
                stats.idle += 1;
            } else if !svc.is_external() {
                stats.unresolved.push(svc.name.clone());
            }
            self.conn.execute(
                "INSERT INTO deploy_services(name, command, build_context, image, ports,
                                             aliases, entry_file)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(name) DO UPDATE SET command = excluded.command,
                     entry_file = excluded.entry_file",
                params![
                    svc.name,
                    svc.command,
                    svc.build_context,
                    svc.image,
                    svc.ports.join(","),
                    svc.aliases.join(","),
                    entry
                ],
            )?;
            stats.services += 1;
        }
        // Reachability is a function of the index, so it is computed with the index.
        // Every `runs` and `affects` call used to rebuild it, which put a 1.5s floor under
        // both.
        self.materialise_reach(12)?;
        Ok(stats)
    }

    /// File the service's start command lands in, or None when it could not be resolved.
    fn resolve_entry_file(&self, repo: &Path, svc: &Service) -> Result<Option<i64>> {
        let Some(command) = &svc.command else {
            return Ok(None);
        };
        let Some(target) = resolve_command_with(command, &self.rules) else {
            return Ok(None);
        };
        // Paths in the index are repo-relative and prefixed by the source root, which is
        // the build context for a built service.
        self.entry_file_for_target(repo, &target, svc.build_context.as_deref())
    }

    /// The file a resolved command lands in. Split out from `resolve_entry_file` so a
    /// cron entry, which has a command but no compose service behind it, resolves by the
    /// same rules as a start command.
    fn entry_file_for_target(
        &self,
        repo: &Path,
        target: &CommandTarget,
        build_context: Option<&str>,
    ) -> Result<Option<i64>> {
        let prefix = build_context.unwrap_or_default().to_string();

        let path_hint = match target {
            CommandTarget::PythonModule(m) => {
                let rel = m.replace('.', "/");
                Some(format!("{prefix}/{rel}.py"))
            }
            CommandTarget::Idle => return Ok(None),
            CommandTarget::Binary(bin) => {
                let dockerfile = repo.join(&prefix).join("Dockerfile");
                let text = std::fs::read_to_string(dockerfile).unwrap_or_default();
                binary_to_package(&text, bin).map(|pkg| format!("{prefix}/{pkg}"))
            }
            // A management command is found by the convention that names the file after
            // the command. Same class of thing as the protobuf naming convention, and
            // stated as a convention wherever it is used.
            CommandTarget::DjangoCommand(name) => {
                let mut stmt = self.conn.prepare_cached(
                    "SELECT f.id FROM files f JOIN strings p ON p.id = f.path_id
                      WHERE p.s LIKE ?1 ORDER BY length(p.s) ASC LIMIT 1",
                )?;
                let mut rows = stmt.query(params![format!("%/{name}.py")])?;
                return Ok(match rows.next()? {
                    Some(r) => Some(r.get(0)?),
                    None => None,
                });
            }
            _ => None,
        };
        let Some(hint) = path_hint else {
            return Ok(None);
        };

        let like = format!("{hint}%");
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.id FROM files f JOIN strings p ON p.id = f.path_id
              WHERE p.s = ?1 OR p.s LIKE ?2
              ORDER BY length(p.s) ASC LIMIT 1",
        )?;
        let mut rows = stmt.query(params![hint, like])?;
        Ok(match rows.next()? {
            Some(r) => Some(r.get(0)?),
            None => None,
        })
    }

    /// Services the reachability walk can never attribute anything to.
    ///
    /// A container started with `tail -f /dev/null` runs no code at boot and everything
    /// it does run arrives later — a cron entry, a management command, `docker exec`.
    /// Reachability therefore attributes nothing to it, which is not the same as it
    /// running nothing, and `runs` must say so or it silently under-reports. Measured:
    /// task E, where the baseline found a nightly cron job in such a container and the
    /// cairn run did not (the measurement record).
    pub fn services_without_entrypoint(&self) -> Result<Vec<String>> {
        // A service with a resolved cron entry is no longer blind: something it runs is
        // in the graph. It may still run more than that, which is what the wording says.
        let mut stmt = self.conn.prepare(
            "SELECT name FROM deploy_services
              WHERE entry_file IS NULL
                AND name NOT IN (SELECT service FROM deploy_on_demand
                                  WHERE entry_file IS NOT NULL)
              ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// `services_running`, but answered for the enclosing type when the symbol itself is
    /// dispatched.
    ///
    /// An RPC method is invoked from a dispatch table, so no static call reaches it and
    /// the walk attributes it to nothing — which reads as dead code for something serving
    /// live traffic. Its class is the honest unit of attribution. Kept here rather than in
    /// the CLI so every caller gets it, which is how `affects` came to report `?` for the
    /// service on both ends of every hop.
    pub fn services_running_attributed(
        &self,
        symbol_id: i64,
        max_depth: usize,
    ) -> Result<(Vec<String>, Attribution)> {
        // Reuse one walk rather than building two: `runs` asked for the symbol, then for
        // its enclosing type, and each walk became expensive once membership edges landed.
        let sets = self.reachable_by_service(max_depth)?;
        let direct: Vec<String> = sets
            .iter()
            .filter(|(_, reach)| reach.contains(&symbol_id))
            .map(|(n, _)| n.clone())
            .collect();
        if !direct.is_empty() {
            return Ok((direct, Attribution::Direct));
        }
        if let Some(owner) = self.enclosing_type(symbol_id)? {
            let via: Vec<String> = sets
                .iter()
                .filter(|(_, reach)| reach.contains(&owner.id))
                .map(|(n, _)| n.clone())
                .collect();
            if !via.is_empty() {
                return Ok((via, Attribution::ViaType(Box::new(owner))));
            }
        }
        // Last resort, and the weakest of the three: the file. A framework route handler
        // is reached by neither a call nor a dispatch table its class owns, so both
        // stronger attributions come back empty and the honest answer is "the module this
        // lives in is loaded by that service". Measured (scenario 4): without it, `runs`
        // on a live public endpoint said `0 service(s)`, which reads as dead code and
        // sent the agent off to rebuild the chain by hand.
        let by_file = self.services_running_file_in(symbol_id, &sets)?;
        if !by_file.is_empty() {
            return Ok((by_file, Attribution::ViaFile));
        }
        Ok((Vec::new(), Attribution::Direct))
    }

    /// Services that run *some* symbol in this symbol's file.
    ///
    /// Weaker than reaching the symbol itself, and stated as such wherever it is used. It
    /// exists because a framework route handler has no static caller at all: FastAPI
    /// registers `endpoints.py`'s functions by decorator and calls them from the server
    /// loop, so reachability attributes them to nothing while the module they live in is
    /// plainly loaded by a service. Importing a module executes it, which is the same rule
    /// the entrypoint seeding already relies on — applied one level out.
    /// The file-level fallback, answered from precomputed reachable sets.
    pub fn services_running_file_in(
        &self,
        symbol_id: i64,
        sets: &[(String, std::collections::HashSet<i64>)],
    ) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT s.id FROM symbols s
              WHERE s.def_file_id = (SELECT def_file_id FROM symbols WHERE id = ?1)
                AND s.id <> ?1
              ORDER BY s.ref_count DESC
              LIMIT 40",
        )?;
        let rows = stmt.query_map(params![symbol_id], |r| r.get::<_, i64>(0))?;
        let mut found: Vec<String> = Vec::new();
        for r in rows {
            let id = r?;
            for name in match self.services_reaching(id)? {
                Some(v) => v,
                None => sets
                    .iter()
                    .filter(|(_, reach)| reach.contains(&id))
                    .map(|(n, _)| n.clone())
                    .collect(),
            } {
                if !found.contains(&name) {
                    found.push(name.clone());
                }
            }
            if !found.is_empty() {
                break;
            }
        }
        Ok(found)
    }

    pub fn services_running_file(&self, symbol_id: i64, max_depth: usize) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT s.id FROM symbols s
              WHERE s.def_file_id = (SELECT def_file_id FROM symbols WHERE id = ?1)
                AND s.id <> ?1
              ORDER BY s.ref_count DESC
              LIMIT 40",
        )?;
        let rows = stmt.query_map(params![symbol_id], |r| r.get::<_, i64>(0))?;
        let mut found: Vec<String> = Vec::new();
        for r in rows {
            for svc in self.services_running(r?, max_depth)? {
                if !found.contains(&svc) {
                    found.push(svc);
                }
            }
            if !found.is_empty() {
                break;
            }
        }
        Ok(found)
    }

    /// Store the reachable set of every service, so queries become lookups.
    pub fn materialise_reach(&mut self, max_depth: usize) -> Result<()> {
        let sets = self.reachable_by_service_walk(max_depth)?;
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM deploy_reach", [])?;
        {
            let mut ins = tx.prepare(
                "INSERT OR IGNORE INTO deploy_reach(service, symbol_id) VALUES (?1, ?2)",
            )?;
            for (name, reach) in &sets {
                for id in reach {
                    ins.execute(params![name, id])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The stored sets, or a live walk when nothing was stored — an index built before
    /// this existed still answers, just slowly, rather than answering wrongly.
    /// Which deployed services reach this one symbol.
    ///
    /// `deploy_reach` is keyed `(symbol_id, service)` without a rowid, so this is an index
    /// seek. The caller used to load the whole table instead - 89,602 rows across 13
    /// services on one repository - build a set per service, and then ask about a handful
    /// of ids. Every use of those sets was this question; none of them needed the map.
    /// That load was the whole of `affects` at 259 ms against 7 ms for `refs`.
    ///
    /// Returns `None` when the table has not been materialised, so the caller can fall
    /// back to the walk rather than read an empty table as an answer.
    pub fn services_reaching(&self, symbol_id: i64) -> Result<Option<Vec<String>>> {
        if !self.deploy_reach_materialised()? {
            return Ok(None);
        }
        let mut stmt = self
            .conn
            .prepare_cached("SELECT service FROM deploy_reach WHERE symbol_id = ?1 ORDER BY 1")?;
        let rows = stmt.query_map(params![symbol_id], |r| r.get::<_, String>(0))?;
        Ok(Some(rows.collect::<std::result::Result<Vec<_>, _>>()?))
    }

    /// Has the reachability table been built at all?
    pub fn deploy_reach_materialised(&self) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT EXISTS(SELECT 1 FROM deploy_reach)", [], |r| {
                r.get::<_, i64>(0)
            })?
            == 1)
    }

    pub fn reachable_by_service(
        &self,
        max_depth: usize,
    ) -> Result<Vec<(String, std::collections::HashSet<i64>)>> {
        use std::collections::HashMap;
        let mut stmt = self
            .conn
            .prepare("SELECT service, symbol_id FROM deploy_reach")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut map: HashMap<String, std::collections::HashSet<i64>> = HashMap::new();
        for r in rows {
            let (name, id) = r?;
            map.entry(name).or_default().insert(id);
        }
        if map.is_empty() {
            return self.reachable_by_service_walk(max_depth);
        }
        // Sorted, because a HashMap iterates in an order that changes per process and
        // every consumer here preserves it. Found by the stress harness: `affects` on a
        // symbol attributed to three services listed them in a different order on each
        // invocation. An answer that reorders itself cannot be diffed, and every claim in
        // eval/ rests on being able to re-run a command and compare.
        let mut out: Vec<_> = map.into_iter().collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Everything each service can reach, computed once.
    ///
    /// `services_running` rebuilds a breadth-first walk per symbol, which was fine until
    /// membership edges made those walks large: `affects` asks it once per hop candidate,
    /// so the cost multiplied and a measured run abandoned the command mid-answer
    /// (the measurement record, task E after the rule pack). One walk per service, then set
    /// membership, is the same answer for a fraction of the work.
    fn reachable_by_service_walk(
        &self,
        max_depth: usize,
    ) -> Result<Vec<(String, std::collections::HashSet<i64>)>> {
        use std::collections::{HashSet, VecDeque};
        let mut entries: Vec<(String, Vec<i64>)> = Vec::new();
        {
            let mut svc_stmt = self.conn.prepare(&format!(
                "SELECT name, entry_file FROM deploy_services WHERE entry_file IS NOT NULL
                 UNION ALL
                 SELECT {ENTRY_LABEL_SQL}, entry_file
                   FROM deploy_on_demand WHERE entry_file IS NOT NULL"
            ))?;
            let mut seed_stmt = self.conn.prepare(
                "SELECT id FROM symbols WHERE def_file_id = ?1
                 UNION
                 SELECT o.symbol_id FROM occurrences o
                  WHERE o.file_id = ?1 AND (o.role & 1) = 0
                    AND NOT EXISTS (
                        SELECT 1 FROM symbols encl
                         WHERE encl.def_file_id = o.file_id
                           AND encl.def_end_line IS NOT NULL
                           AND encl.def_line <= o.line AND encl.def_end_line >= o.line)",
            )?;
            let rows =
                svc_stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            for r in rows {
                let (name, file_id) = r?;
                let seeds = seed_stmt.query_map(params![file_id], |r| r.get::<_, i64>(0))?;
                let mut v = Vec::new();
                for s in seeds {
                    v.push(s?);
                }
                entries.push((name, v));
            }
        }

        let mut stmt = self.conn.prepare_cached(
            "SELECT dst_symbol FROM edges WHERE src_symbol = ?1 AND kind IN (0, 4)",
        )?;
        let mut out = Vec::new();
        for (name, seeds) in entries {
            let mut seen: HashSet<i64> = seeds.iter().copied().collect();
            let mut queue: VecDeque<(i64, usize)> = seeds.into_iter().map(|s| (s, 0)).collect();
            while let Some((node, d)) = queue.pop_front() {
                if d >= max_depth {
                    continue;
                }
                let rows = stmt.query_map(params![node], |r| r.get::<_, i64>(0))?;
                for r in rows {
                    let next = r?;
                    if seen.insert(next) {
                        queue.push_back((next, d + 1));
                    }
                }
            }
            out.push((name, seen));
        }
        Ok(out)
    }

    /// Services whose entrypoint can reach this symbol.
    ///
    /// This is the question the filesystem cannot answer: fifteen services share two
    /// source trees, so only reachability says which of them actually runs a given
    /// module.
    pub fn services_running(&self, symbol_id: i64, max_depth: usize) -> Result<Vec<String>> {
        use std::collections::{HashSet, VecDeque};
        // Seed from every symbol defined in the entry file: running a module executes
        // all of it, and the meaningful edges leave from whichever function assembles
        // the handlers, not from the first definition in the file.
        let mut entries: Vec<(String, Vec<i64>)> = Vec::new();
        {
            // Start commands and on-demand entrypoints seed the same walk. The label
            // differs because the claim differs: one is what the deployment starts, the
            // other is what a schedule or an operator invokes, and an answer that blurs
            // them is claiming more than it knows.
            let mut svc_stmt = self.conn.prepare(
                "SELECT name, entry_file FROM deploy_services WHERE entry_file IS NOT NULL
                 UNION ALL
                 SELECT service || ' (' || coalesce('cron ' || schedule, 'on demand')
                        || ': ' || command || ')', entry_file
                   FROM deploy_on_demand WHERE entry_file IS NOT NULL",
            )?;
            // Symbols defined in the entry file, plus everything it references at
            // module level. The second half matters: running a module executes its
            // top-level statements, so `mcp.add_middleware(ScopeEnforcementMiddleware())`
            // genuinely starts that class - but a module-level reference has no
            // enclosing function and therefore no call edge, so without this the
            // middleware looked unreachable from any service.
            let mut seed_stmt = self.conn.prepare(
                "SELECT id FROM symbols WHERE def_file_id = ?1
                 UNION
                 SELECT o.symbol_id FROM occurrences o
                  WHERE o.file_id = ?1 AND (o.role & 1) = 0
                    AND NOT EXISTS (
                        SELECT 1 FROM symbols encl
                         WHERE encl.def_file_id = o.file_id
                           AND encl.def_end_line IS NOT NULL
                           AND encl.def_line <= o.line AND encl.def_end_line >= o.line)",
            )?;
            let rows =
                svc_stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            for r in rows {
                let (name, file_id) = r?;
                let seeds = seed_stmt.query_map(params![file_id], |r| r.get::<_, i64>(0))?;
                let mut v = Vec::new();
                for s in seeds {
                    v.push(s?);
                }
                entries.push((name, v));
            }
        }

        let mut out = Vec::new();
        let mut stmt = self
            .conn
            .prepare_cached("SELECT dst_symbol FROM edges WHERE src_symbol = ?1 AND kind = 0")?;
        // Registering a handler class puts all of its methods on the live path, but there
        // is no call edge from a class to the methods it owns, so the walk arrived at the
        // class and stopped. Measured (task K): a mechanically chosen catalog
        // function sat four hops below a registered gRPC handler and `affects` reported
        // zero services for it, in-process and over the network both. The mirror image of
        // the method-to-enclosing-type fallback, missed because every task before K
        // started from a method rather than ending at one.
        let mut members = self
            .conn
            .prepare_cached("SELECT dst_symbol FROM edges WHERE src_symbol = ?1 AND kind = 4")?;
        for (name, seeds) in entries {
            let mut seen: HashSet<i64> = seeds.iter().copied().collect();
            let mut queue: VecDeque<(i64, usize)> = seeds.iter().map(|s| (*s, 0usize)).collect();
            let mut found = seen.contains(&symbol_id);
            while let Some((node, d)) = queue.pop_front() {
                if node == symbol_id {
                    found = true;
                    break;
                }
                if d >= max_depth {
                    continue;
                }
                let rows = stmt.query_map(params![node], |r| r.get::<_, i64>(0))?;
                for r in rows {
                    let next = r?;
                    if seen.insert(next) {
                        queue.push_back((next, d + 1));
                    }
                }
                let rows = members.query_map(params![node], |r| r.get::<_, i64>(0))?;
                for r in rows {
                    let next = r?;
                    if seen.insert(next) {
                        queue.push_back((next, d + 1));
                    }
                }
            }
            if found {
                out.push(name);
            }
        }
        Ok(out)
    }

    /// Every way code gets started, start commands and on-demand entries together.
    ///
    /// `reaches` restricts the answer to entrypoints from which that symbol can be run.
    /// That is the audit direction: not "who calls this" but "is this on a path something
    /// actually starts", which is the question behind whether a change is live. It is
    /// also a free consistency check — this and `runs` derive the same fact from opposite
    /// ends, so when they disagree the index has just reported a hole in itself.
    ///
    /// Idle services (`sleep infinity` and friends) are listed with no command target
    /// rather than dropped: a container that starts nothing still runs whatever is sent
    /// to it later, and omitting it says the opposite.
    pub fn entrypoints(&self, reaches: Option<i64>) -> Result<Vec<Entrypoint>> {
        // The filter is applied by label so both kinds go through one path. Start
        // commands are named by the service, on-demand entries by the built label, which
        // is exactly what the reachability walk stored.
        let filter = match reaches {
            Some(_) => "WHERE label IN (SELECT service FROM deploy_reach WHERE symbol_id = ?1)",
            None => "",
        };
        let sql = format!(
            "SELECT * FROM (
                 SELECT d.name AS label, d.name AS service, NULL AS schedule, d.command,
                        p.s AS entry_path, NULL AS script, 0 AS on_demand
                   FROM deploy_services d
                   LEFT JOIN files   f ON f.id = d.entry_file
                   LEFT JOIN strings p ON p.id = f.path_id
                 UNION ALL
                 SELECT {ENTRY_LABEL_SQL} AS label, o.service, o.schedule, o.command,
                        p.s AS entry_path, o.script, 1 AS on_demand
                   FROM deploy_on_demand o
                   LEFT JOIN files   f ON f.id = o.entry_file
                   LEFT JOIN strings p ON p.id = f.path_id
             ) {filter}
             ORDER BY service, on_demand, schedule, command"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let map = |r: &rusqlite::Row| {
            let schedule: Option<String> = r.get("schedule")?;
            let on_demand: i64 = r.get("on_demand")?;
            let command: Option<String> = r.get("command")?;
            let idle = command
                .as_deref()
                .and_then(|c| resolve_command_with(c, &self.rules))
                .map(|t| t == CommandTarget::Idle)
                .unwrap_or(false);
            Ok(Entrypoint {
                service: r.get("service")?,
                trigger: match (on_demand, schedule) {
                    (0, _) => Trigger::Start,
                    (_, Some(s)) => Trigger::Cron(s),
                    (_, None) => Trigger::OnDemand,
                },
                command,
                entry_path: r.get("entry_path")?,
                script: r.get("script")?,
                idle,
            })
        };
        let rows = match reaches {
            Some(id) => stmt.query_map(params![id], map)?.collect::<Vec<_>>(),
            None => stmt.query_map([], map)?.collect::<Vec<_>>(),
        };
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn deploy_services(&self) -> Result<Vec<DeployServiceRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.name, d.command, p.s, d.ports, d.entry_file IS NOT NULL
               FROM deploy_services d
               LEFT JOIN files   f ON f.id = d.entry_file
               LEFT JOIN strings p ON p.id = f.path_id
              ORDER BY d.name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)? != 0,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeployStats {
    pub services: usize,
    pub with_entrypoint: usize,
    /// Containers deliberately held open with no workload.
    pub idle: usize,
    /// Built services whose command could not be resolved. Named, not counted: an
    /// unresolved entrypoint silently declares live code dead (architecture 8.4).
    pub unresolved: Vec<String>,
    /// Cron entries and runner scripts resolved to code.
    pub on_demand: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_command_shapes_that_appear() {
        assert_eq!(
            resolve_command("python3 -m domains.orders.grpc.server"),
            Some(CommandTarget::PythonModule(
                "domains.orders.grpc.server".into()
            ))
        );
        assert_eq!(
            resolve_command("/bin/grpcserver"),
            Some(CommandTarget::Binary("grpcserver".into()))
        );
        assert_eq!(
            resolve_command("celery -A proj worker"),
            Some(CommandTarget::CeleryApp("proj".into()))
        );
        assert!(matches!(
            resolve_command("python manage.py migrate"),
            Some(CommandTarget::DjangoCommand(_))
        ));
        assert_eq!(
            resolve_command("uvicorn domains.orders.mcp.server:app --host 0.0.0.0"),
            Some(CommandTarget::PythonModule(
                "domains.orders.mcp.server".into()
            ))
        );
        // A container held open on purpose is a fact, not a failure to parse.
        assert_eq!(
            resolve_command("tail -f /dev/null"),
            Some(CommandTarget::Idle)
        );
        assert_eq!(resolve_command("sleep infinity"), Some(CommandTarget::Idle));
        assert_eq!(resolve_command("some-unknown-binary --flag"), None);
    }

    #[test]
    fn follows_a_binary_back_through_a_multi_stage_build() {
        // Verbatim shape from the target repo, including the xx-go wrapper.
        let dockerfile = "\
FROM golang AS builder
RUN CGO_ENABLED=0 xx-go build -o /out/grpcserver ./domains/orders/cmd/grpcserver/server.go
FROM alpine
COPY --from=builder /out/grpcserver /bin/grpcserver
";
        assert_eq!(
            binary_to_package(dockerfile, "grpcserver").as_deref(),
            Some("domains/orders/cmd/grpcserver/server.go")
        );
    }

    #[test]
    fn merge_keys_are_resolved_or_services_come_back_empty() {
        let yaml = "\
x-base: &base
  init: true
  build:
    context: srcpy
services:
  api:
    <<: *base
    command: python3 -m domains.api.server
    ports: [\"8000:8000\"]
";
        let mut doc: Value = serde_yaml::from_str(yaml).unwrap();
        doc.apply_merge().unwrap();
        let svc = service_from_value("api", doc.get("services").unwrap().get("api").unwrap());
        assert_eq!(svc.build_context.as_deref(), Some("srcpy"));
        assert_eq!(
            svc.command.as_deref(),
            Some("python3 -m domains.api.server")
        );
        assert_eq!(svc.ports, vec!["8000:8000".to_string()]);
    }

    #[test]
    fn an_override_file_merges_rather_than_replaces() {
        let base = "\
services:
  api:
    command: python3 -m domains.api.server
    build: { context: srcpy }
";
        let overlay = "\
services:
  api:
    volumes: [\"./srcpy:/app/\"]
";
        let mut b: Value = serde_yaml::from_str(base).unwrap();
        let o: Value = serde_yaml::from_str(overlay).unwrap();
        let mut svc_b = b
            .get_mut("services")
            .unwrap()
            .get_mut("api")
            .unwrap()
            .clone();
        deep_merge(&mut svc_b, o.get("services").unwrap().get("api").unwrap());
        let svc = service_from_value("api", &svc_b);
        assert_eq!(
            svc.command.as_deref(),
            Some("python3 -m domains.api.server"),
            "an override that only adds volumes must not erase the base command"
        );
        assert_eq!(svc.mount, Some(("srcpy".into(), "/app".into())));
    }

    #[test]
    fn a_bind_mount_beats_the_image_contents() {
        let yaml = "\
services:
  api:
    volumes:
      - \"./srcpy:/app/\"
      - \".volumes:/.volumes/\"
";
        let doc: Value = serde_yaml::from_str(yaml).unwrap();
        let svc = service_from_value("api", doc.get("services").unwrap().get("api").unwrap());
        assert_eq!(
            svc.mount,
            Some(("srcpy".to_string(), "/app".to_string())),
            "the first real source mount wins; dot-prefixed caches are skipped"
        );
    }
}
