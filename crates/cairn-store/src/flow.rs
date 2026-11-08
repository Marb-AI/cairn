//! Control flow inside one function.
//!
//! The call graph is between functions, and that is the right shape for almost every
//! question cairn answers. Reviewing a single handler is the exception: what matters then
//! is the tree *inside* it — where it branches, where it returns early, what it calls on
//! the way, what it defers — and no amount of walking the call graph produces that.
//!
//! The shape is language-neutral and the knowledge is not. A provider per language
//! produces the nodes; this module stores them, resolves what they point at, caches them
//! and hands them on. The core never parses anything.
//!
//! What cairn adds to a provider's answer is the part a provider cannot know:
//!
//!   * **a call's target**, resolved through the occurrence at the call position, so a
//!     node carries a handle a reader can go on with rather than the text `s.accept`;
//!   * **declared annotations**, so a call into a generated mutation reads
//!     `writes w17:entity/Order (declared by w17)`;
//!   * **what could not be resolved**, because a call nobody could name is the one place
//!     this answer stops being a map.

use crate::{Store, SymbolRow};
use anyhow::Result;
use rusqlite::params;

/// What a node in the flow is.
///
/// A closed set, and language-neutral on purpose: a provider for another language has to
/// map its own grammar onto these rather than inventing a kind the renderer has never
/// heard of. `panic` is here because a panic is not a return and no caller sees it as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowKind {
    Entry,
    Branch,
    Call,
    Return,
    ReturnError,
    Loop,
    Defer,
    Panic,
}

impl FlowKind {
    pub fn label(self) -> &'static str {
        match self {
            FlowKind::Entry => "entry",
            FlowKind::Branch => "branch",
            FlowKind::Call => "call",
            FlowKind::Return => "return",
            FlowKind::ReturnError => "return!",
            FlowKind::Loop => "loop",
            FlowKind::Defer => "defer",
            FlowKind::Panic => "panic",
        }
    }
}

/// Where a provider says a call's callee name sits.
///
/// The line is 1-based, as every line a reader sees is; the column is 0-based, as SCIP
/// records it. The asymmetry is ugly and it is the convention on both sides of this
/// boundary, so it is stated rather than quietly converted.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct At {
    pub line: i64,
    pub col: i64,
}

/// One node, as the provider emits it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProviderNode {
    pub id: usize,
    pub kind: FlowKind,
    #[serde(default)]
    pub condition: Option<String>,
    /// What a call names, as written.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub at: Option<At>,
    /// `line-line`, 1-based and inclusive.
    pub span: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProviderEdge {
    pub from: usize,
    pub to: usize,
    #[serde(default)]
    pub label: Option<String>,
}

/// Exactly what a provider prints. Parsed, never trusted: a provider that emitted a kind
/// the renderer does not know would be rejected by serde here rather than halfway
/// through rendering.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProviderFlow {
    /// The provider and its version, which is part of the cache key.
    pub provider: String,
    /// What the provider called the function, for the header.
    pub function: String,
    pub file: String,
    pub nodes: Vec<ProviderNode>,
    pub edges: Vec<ProviderEdge>,
    #[serde(default)]
    pub unknown: Vec<String>,
}

/// A declared fact about what a call reaches: `writes`, `emits`, and who said so.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct Annotation {
    /// The declared edge kind, as the connector named it.
    pub kind: String,
    /// The other end: a declared node id, or a SCIP symbol string.
    pub target: String,
    pub connector: String,
}

/// One node of the answer: the provider's node, plus what cairn could resolve about it.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct FlowNode {
    pub id: usize,
    /// `entry`, `branch`, `call`, `return`, `return_error`, `loop`, `defer`, `panic`.
    pub kind: String,
    /// The condition or the returned expression, as written. Display only.
    pub condition: Option<String>,
    /// What a call names, as written.
    pub name: Option<String>,
    /// The symbol a call resolved to. `null` for a call cairn could not resolve, which is
    /// reported, and for every node that is not a call.
    pub target: Option<String>,
    /// The handle of that symbol, so the next command can be run without a lookup.
    pub handle: Option<String>,
    /// `file:line-line`.
    pub span: String,
    /// What a connector declared about the call's target. A call into a generated
    /// mutation reads `writes w17:entity/Order`, which is the sort of thing no call graph
    /// can say.
    pub declared: Vec<Annotation>,
}

#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct FlowEdge {
    pub from: usize,
    pub to: usize,
    pub label: Option<String>,
}

/// `cairn flow` as data.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct Flow {
    /// The symbol this is the flow of, as a handle.
    pub handle: String,
    /// Its full SCIP symbol string.
    pub symbol: Option<String>,
    pub function: String,
    pub file: String,
    pub provider: String,
    /// True when this came from the cache rather than from a fresh run.
    pub cached: bool,
    pub nodes: Vec<FlowNode>,
    pub edges: Vec<FlowEdge>,
    /// What the provider could not determine, and what cairn could not resolve.
    pub unknown: Vec<String>,
}

impl Store {
    /// The symbol an occurrence at this exact position names.
    ///
    /// Position rather than name: `s.accept` and `other.accept` are different symbols
    /// with the same text, and a flow that resolved by name would hand a reader the wrong
    /// handle while looking right. The column is where the *name* starts, which is what
    /// SCIP records.
    pub fn symbol_at(&self, path: &str, line: i64, col: i64) -> Result<Option<SymbolRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT o.symbol_id FROM occurrences o
               JOIN files f   ON f.id = o.file_id
               JOIN strings p ON p.id = f.path_id
              WHERE p.s = ?1 AND o.line = ?2
                AND o.col_start <= ?3 AND o.col_end > ?3
              ORDER BY (o.col_end - o.col_start) ASC
              LIMIT 1",
        )?;
        let mut rows = stmt.query(params![path, line, col])?;
        let Some(r) = rows.next()? else {
            return Ok(None);
        };
        self.symbol(r.get::<_, i64>(0)?)
    }

    /// Turn a provider's answer into one of cairn's, resolving what it could not.
    pub fn resolve_flow(
        &self,
        sym: &SymbolRow,
        repo_relative_file: &str,
        provider: &ProviderFlow,
        cached: bool,
    ) -> Result<Flow> {
        let mut unknown = provider.unknown.clone();
        let mut unresolved: Vec<String> = Vec::new();
        let mut nodes = Vec::new();
        for n in &provider.nodes {
            let resolved = match (n.kind, n.at) {
                (FlowKind::Call | FlowKind::Defer | FlowKind::Panic, Some(at)) => {
                    self.symbol_at(repo_relative_file, at.line - 1, at.col)?
                }
                _ => None,
            };
            if matches!(n.kind, FlowKind::Call) && resolved.is_none() {
                if let Some(name) = &n.name {
                    unresolved.push(format!("{name} at {}", n.span));
                }
            }
            let declared = match &resolved {
                Some(row) => self
                    .declared_annotations(row.id)?
                    .into_iter()
                    .map(|(kind, target, connector)| Annotation {
                        kind,
                        target,
                        connector,
                    })
                    .collect(),
                None => Vec::new(),
            };
            nodes.push(FlowNode {
                id: n.id,
                kind: serde_json::to_value(n.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| n.kind.label().to_string()),
                condition: n.condition.clone(),
                name: n.name.clone(),
                target: resolved.as_ref().and_then(|r| r.scip.clone()),
                handle: resolved.as_ref().map(|r| r.handle.clone()),
                span: format!("{repo_relative_file}:{}", n.span),
                declared,
            });
        }
        if !unresolved.is_empty() {
            // Named rather than counted: a reader deciding whether the map is complete
            // needs to know *which* call went unresolved. `len(…)` being a builtin is
            // fine; a handler call that did not resolve is a hole in the answer.
            unknown.push(format!(
                "{} call(s) could not be resolved to a symbol ({}{}) - a builtin, a \
                 third-party function, or code this index does not cover",
                unresolved.len(),
                unresolved
                    .iter()
                    .take(4)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                if unresolved.len() > 4 { ", ..." } else { "" }
            ));
        }
        Ok(Flow {
            handle: sym.handle.clone(),
            symbol: sym.scip.clone(),
            function: provider.function.clone(),
            file: repo_relative_file.to_string(),
            provider: provider.provider.clone(),
            cached,
            nodes,
            edges: provider
                .edges
                .iter()
                .map(|e| FlowEdge {
                    from: e.from,
                    to: e.to,
                    label: e.label.clone().filter(|l| !l.is_empty()),
                })
                .collect(),
            unknown,
        })
    }

    /// A cached provider answer, if one was computed for this exact file content.
    ///
    /// Keyed on the content of the file rather than on a timestamp, and on the provider's
    /// version so a better provider invalidates its own output. There is no dependency
    /// component to the key — the flow *inside* a function is a syntax fact about one
    /// file, so a change anywhere else cannot alter it. That is the one case architecture
    /// 5.2 explicitly exempts from `deps_api_hash`.
    pub fn cached_flow(&self, symbol_id: i64, blob: &[u8]) -> Result<Option<ProviderFlow>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT json FROM flows WHERE symbol_id = ?1 AND blob = ?2")?;
        let mut rows = stmt.query(params![symbol_id, blob])?;
        let Some(r) = rows.next()? else {
            return Ok(None);
        };
        let text: String = r.get(0)?;
        // A cached answer that no longer parses is a cache miss, not a failure: the
        // shape may have changed under it, and recomputing is cheap.
        Ok(serde_json::from_str(&text).ok())
    }

    pub fn cache_flow(&self, symbol_id: i64, blob: &[u8], flow: &ProviderFlow) -> Result<()> {
        let json = serde_json::to_string(flow)?;
        self.conn.execute(
            "INSERT INTO flows(symbol_id, blob, provider, json) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(symbol_id) DO UPDATE SET blob = excluded.blob,
                                                  provider = excluded.provider,
                                                  json = excluded.json",
            params![symbol_id, blob, flow.provider, json],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "provider": "cairnflow 1",
      "function": "Service.UploadReadings",
      "file": "/repo/srcgo/internal/ingest/service.go",
      "nodes": [
        {"id": 0, "kind": "entry", "name": "Service.UploadReadings", "span": "38-56"},
        {"id": 1, "kind": "branch", "condition": "batch == nil", "span": "39-41"},
        {"id": 2, "kind": "return_error", "condition": "nil, fmt.Errorf(…)", "span": "40-40"},
        {"id": 3, "kind": "call", "name": "s.accept", "at": {"line": 47, "col": 8}, "span": "47-47"}
      ],
      "edges": [{"from": 0, "to": 1}, {"from": 1, "to": 2}, {"from": 2, "to": 3}],
      "unknown": []
    }"#;

    #[test]
    fn a_provider_answer_parses_into_the_shape_the_core_renders() {
        let parsed: ProviderFlow = serde_json::from_str(SAMPLE).expect("the sample parses");
        assert_eq!(parsed.nodes.len(), 4);
        assert_eq!(parsed.nodes[2].kind, FlowKind::ReturnError);
        assert_eq!(parsed.nodes[3].at.map(|a| a.col), Some(8));
    }

    #[test]
    fn a_kind_the_renderer_does_not_know_is_refused_rather_than_rendered() {
        // The closed set is the contract with a provider. A provider inventing `maybe`
        // has to be told, not half-rendered.
        let text = SAMPLE.replace("\"kind\": \"branch\"", "\"kind\": \"maybe\"");
        assert!(serde_json::from_str::<ProviderFlow>(&text).is_err());
    }

    #[test]
    fn an_unresolved_call_is_named_in_the_answer() {
        // Nothing is indexed here, so the call cannot resolve - which is exactly the
        // case that has to be reported rather than left as an empty field.
        let store = Store::open_in_memory().unwrap();
        let sym = SymbolRow {
            id: 1,
            handle: "ab".into(),
            scip: Some("scip-go gomod x . `x`/Service#UploadReadings().".into()),
            name: "UploadReadings".into(),
            container: None,
            module: None,
            kind: cairn_scip::SymbolKind::Method,
            lang: crate::Lang::Go,
            def: None,
            def_end_line: None,
            ref_count: 0,
        };
        let provider: ProviderFlow = serde_json::from_str(SAMPLE).unwrap();
        let flow = store
            .resolve_flow(&sym, "srcgo/internal/ingest/service.go", &provider, false)
            .unwrap();
        assert_eq!(flow.nodes.len(), 4);
        assert!(flow.nodes[3].target.is_none());
        assert_eq!(flow.unknown.len(), 1, "{:#?}", flow.unknown);
        assert!(flow.unknown[0].contains("s.accept at 47-47"));
        // The span carries the file, so a reader can open it without guessing.
        assert_eq!(flow.nodes[0].span, "srcgo/internal/ingest/service.go:38-56");
    }

    #[test]
    fn a_cached_flow_is_keyed_on_the_content_of_the_file() {
        let store = Store::open_in_memory().unwrap();
        let name_id = store.intern("f").unwrap();
        store
            .conn
            .execute(
                "INSERT INTO symbols(hash, name_id, kind, lang, ref_count)
                 VALUES (x'01', ?1, 3, 2, 0)",
                params![name_id],
            )
            .unwrap();
        let id = store.conn.last_insert_rowid();
        let provider: ProviderFlow = serde_json::from_str(SAMPLE).unwrap();
        store.cache_flow(id, b"blob-one", &provider).unwrap();

        assert!(store.cached_flow(id, b"blob-one").unwrap().is_some());
        // The file changed, so the flow inside it may have: a miss, not a stale hit.
        assert!(store.cached_flow(id, b"blob-two").unwrap().is_none());
    }
}
