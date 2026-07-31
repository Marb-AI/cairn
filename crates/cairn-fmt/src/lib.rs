//! Rendering.
//!
//! Deliberately split in two: **selection** happens in cairn-store (which symbols, how
//! deep, which edges), **presentation** happens here. Otherwise every new view would
//! have to be re-implemented for every query, and the number of both is going to grow.
//!
//! Two rules hold across every view (architecture 6.3, D8):
//!
//! * **Not JSON by default.** Braces, quotes and repeated keys cost several times more
//!   tokens than an aligned text table carrying the same information. When the whole
//!   pitch is cheaper context, the response format *is* the pitch.
//! * **`unknown:`, `suppressed:` and `stale:` are always printed.** A missing section
//!   reads as "this is everything", and that is the silent error the design avoids.

use cairn_store::{EdgeSource, Occurrence, PathHop, SymbolRow, Walk};
use std::fmt::Write;

pub mod budget;
pub use budget::Budget;

/// How a result set is laid out. Orthogonal to *what* was selected, so any view can
/// render any walk (architecture 18.1 — this is the fifth axis alongside detail,
/// breadth x depth, aspect and budget).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// One node per line, flat. Cheapest, good for "give me the list".
    List,
    /// Indented by depth, showing how each node was reached.
    Tree,
}

impl View {
    pub fn parse(s: &str) -> Option<View> {
        match s {
            "list" => Some(View::List),
            "tree" => Some(View::Tree),
            _ => None,
        }
    }
}

/// Every response carries the same envelope, so an agent can rely on it being there.
pub struct Envelope {
    pub body: String,
    pub unknown: Vec<String>,
    pub suppressed: Vec<String>,
    pub stale: Vec<String>,
}

impl Envelope {
    pub fn new(body: String) -> Envelope {
        Envelope { body, unknown: Vec::new(), suppressed: Vec::new(), stale: Vec::new() }
    }

    pub fn unknown(mut self, msg: impl Into<String>) -> Self {
        self.unknown.push(msg.into());
        self
    }

    pub fn suppressed(mut self, msg: impl Into<String>) -> Self {
        self.suppressed.push(msg.into());
        self
    }

    pub fn render(&self) -> String {
        let mut out = self.body.clone();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        for (label, items) in [
            ("suppressed", &self.suppressed),
            ("unknown", &self.unknown),
            ("stale", &self.stale),
        ] {
            if items.is_empty() {
                let _ = writeln!(out, "{label}: none");
            } else if items.len() == 1 {
                let _ = writeln!(out, "{label}: {}", items[0]);
            } else {
                let _ = writeln!(out, "{label} ({}):", items.len());
                for i in items {
                    let _ = writeln!(out, "  {i}");
                }
            }
        }
        out
    }
}

/// One result line: `[handle] Qualified.name  kind  lang  path:line`
pub fn symbol_line(s: &SymbolRow) -> String {
    let loc = s
        .def
        .as_ref()
        .map(|d| d.location())
        .unwrap_or_else(|| "<no definition indexed>".to_string());
    format!(
        "[{}] {:<40} {:<6} {}  {}",
        s.handle,
        truncate(&s.qualified(), 40),
        s.kind.as_str(),
        s.lang.tag(),
        loc
    )
}

pub fn symbols(rows: &[SymbolRow], query: &str, budget: &mut Budget) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(body, "{} matches for \"{}\"", rows.len(), query);
    let mut shown = 0;
    for s in rows {
        if !budget.push(&mut body, &symbol_line(s)) {
            break;
        }
        shown += 1;
    }
    if rows.is_empty() {
        let _ = writeln!(body, "(nothing matched)");
    }
    let mut env = Envelope::new(body);
    if shown < rows.len() {
        env = env.suppressed(budget.cut_note(rows.len() - shown, "matches"));
    }
    env
}

pub fn references(sym: &SymbolRow, refs: &[Occurrence], suppressed_generated: i64) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(body, "references to [{}] {}", sym.handle, sym.qualified());
    if let Some(def) = &sym.def {
        let _ = writeln!(body, "  defined at {}", def.location());
    }
    let _ = writeln!(body, "{} references                    [L0, exact]", refs.len());
    for r in refs {
        let _ = writeln!(body, "  {}", r.location());
    }
    if refs.is_empty() {
        let _ = writeln!(body, "  (none outside generated code)");
    }

    let mut env = Envelope::new(body);
    if suppressed_generated > 0 {
        env = env.suppressed(format!(
            "{suppressed_generated} references in generated code (rerun with --include-generated)"
        ));
    }
    env
}

/// Render a bounded walk over the call graph.
pub fn walk(w: &Walk, title: &str, view: View, budget: &mut Budget) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(body, "{title}");

    let mut shown = 0usize;
    for (i, node) in w.nodes.iter().enumerate() {
        let line = match view {
            View::Tree => {
                let indent = "  ".repeat(node.depth);
                let site = node
                    .site
                    .as_deref()
                    .map(|s| format!("  @ {s}"))
                    .unwrap_or_default();
                let tag = if node.source == EdgeSource::Scip {
                    String::new()
                } else {
                    format!("  [{}]", node.source.label())
                };
                format!("{indent}{}{site}{tag}", symbol_line(&node.symbol))
            }
            View::List => symbol_line(&node.symbol),
        };
        if !budget.push(&mut body, &line) {
            break;
        }
        shown = i + 1;
    }

    let mut env = Envelope::new(body);
    if shown < w.nodes.len() {
        env = env.suppressed(budget.cut_note(w.nodes.len() - shown, "nodes"));
    }
    if w.truncated > 0 {
        env = env.suppressed(format!(
            "{} neighbours beyond --fanout",
            w.truncated
        ));
    }
    if w.revisited > 0 {
        env = env.suppressed(format!(
            "{} nodes reachable by more than one path, shown once",
            w.revisited
        ));
    }
    env
}

/// A resolved call chain, one hop per line with the call site that leads onward.
pub fn path(hops: &[PathHop], budget: &mut Budget) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(
        body,
        "call path, {} hops                              [L1, exact]",
        hops.len().saturating_sub(1)
    );
    let mut shown = 0usize;
    for (i, hop) in hops.iter().enumerate() {
        let arrow = if i == 0 { "   " } else { "-> " };
        let site = hop
            .site
            .as_deref()
            .map(|s| format!("  @ {s}"))
            .unwrap_or_default();
        let line = format!("{arrow}{}{site}", symbol_line(&hop.symbol));
        if !budget.push(&mut body, &line) {
            break;
        }
        shown = i + 1;
    }
    let mut env = Envelope::new(body);
    if shown < hops.len() {
        env = env.suppressed(budget.cut_note(hops.len() - shown, "hops"));
    }
    env
}

/// Tests that reach a symbol through the call graph.
pub fn tests(sym: &SymbolRow, rows: &[SymbolRow], budget: &mut Budget) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(
        body,
        "tests reaching [{}] {}                          [L1, exact]",
        sym.handle,
        sym.qualified()
    );
    let mut shown = 0usize;
    for (i, t) in rows.iter().enumerate() {
        if !budget.push(&mut body, &format!("  {}", symbol_line(t))) {
            break;
        }
        shown = i + 1;
    }
    if rows.is_empty() {
        let _ = writeln!(body, "  (no test reaches this symbol through the call graph)");
    }
    let mut env = Envelope::new(body);
    if shown < rows.len() {
        env = env.suppressed(budget.cut_note(rows.len() - shown, "tests"));
    }
    env
}

/// Candidate dynamic references. Always labelled, never merged with exact results.
pub fn weak_links(sym: &SymbolRow, sites: &[(String, f64)], budget: &mut Budget) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(
        body,
        "string literals naming [{}] {}          [L1-W, unverified]",
        sym.handle,
        sym.qualified()
    );
    let mut shown = 0usize;
    for (i, (site, conf)) in sites.iter().enumerate() {
        if !budget.push(&mut body, &format!("  {site}   confidence {conf:.1}")) {
            break;
        }
        shown = i + 1;
    }
    if sites.is_empty() {
        let _ = writeln!(body, "  (no literal in the repo spells this name)");
    }
    let mut env = Envelope::new(body);
    if shown < sites.len() {
        env = env.suppressed(budget.cut_note(sites.len() - shown, "sites"));
    }
    if !sites.is_empty() {
        env = env.unknown(
            "these are lexical matches, not resolved references - read the site to \
             confirm before relying on it",
        );
    }
    env
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{keep}~")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_always_has_all_sections() {
        let out = Envelope::new("body".into()).render();
        assert!(out.contains("suppressed: none"));
        assert!(out.contains("unknown: none"));
        assert!(out.contains("stale: none"));
    }

    #[test]
    fn truncation_marks_itself() {
        assert_eq!(truncate("abcdef", 4), "abc~");
        assert_eq!(truncate("abc", 4), "abc");
    }
}