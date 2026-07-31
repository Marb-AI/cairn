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
pub mod source;
pub use budget::Budget;
pub use source::{Detail, Excerpt, Source};

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

    /// Mark the answer stale where it touches files that have changed since indexing.
    ///
    /// `dirty` is what the daemon has observed; `None` means no daemon was running, and
    /// that is reported as *unknown* rather than as clean. An empty dirty set and an
    /// unknown one look identical in an answer, and treating the second as the first is
    /// exactly the silent staleness the contract forbids (D8).
    pub fn mark_stale(mut self, dirty: Option<&[String]>, mentioned: &[String]) -> Self {
        let Some(dirty) = dirty else {
            self.stale.push(
                "not tracked - no daemon running, so changes since indexing are invisible \
                 (`cairn daemon --repo <dir>`)"
                    .to_string(),
            );
            return self;
        };
        let mut hit: Vec<&String> = mentioned
            .iter()
            .filter(|p| dirty.iter().any(|d| d == *p))
            .collect();
        hit.sort();
        hit.dedup();
        if !hit.is_empty() {
            self.stale.push(format!(
                "{} of the files in this answer changed since indexing: {}",
                hit.len(),
                hit.iter()
                    .take(5)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
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

/// One result line: `[handle] Qualified.name  kind  lang  path:start-end`
///
/// The line range, not just the start, is deliberate: an agent that wants to audit on
/// its own terms - or build its own graph rather than trusting ours - needs to know
/// where the symbol ends without opening the file to find out.
pub fn symbol_line(s: &SymbolRow) -> String {
    let loc = match (s.def.as_ref(), s.def_end_line) {
        (Some(d), Some(end)) if end > d.line => format!("{}-{}", d.location(), end + 1),
        (Some(d), _) => d.location(),
        (None, _) => "<no definition indexed>".to_string(),
    };
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
///
/// `detail` decides how much of each node is printed. Anything above `Skeleton` needs
/// `source`, and is the audit shape: walk the callers and show me their code.
pub fn walk(
    w: &Walk,
    title: &str,
    view: View,
    detail: Detail,
    source: Option<&mut Source>,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(body, "{title}");

    let mut notes: Vec<String> = Vec::new();
    let mut source = source;
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

        if detail.needs_source() {
            let indent = if view == View::Tree { "  ".repeat(node.depth + 1) } else { "  ".into() };
            if !emit_excerpt(
                &mut body,
                &indent,
                &node.symbol,
                detail,
                source.as_deref_mut(),
                budget,
                &mut notes,
            ) {
                break;
            }
        }
    }

    let mut env = Envelope::new(body);
    for n in notes {
        env = env.suppressed(n);
    }
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
pub fn path(
    hops: &[PathHop],
    detail: Detail,
    source: Option<&mut Source>,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(
        body,
        "call path, {} hops                              [L1, exact]",
        hops.len().saturating_sub(1)
    );
    let mut notes: Vec<String> = Vec::new();
    let mut source = source;
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
        if detail.needs_source()
            && !emit_excerpt(
                &mut body,
                "   ",
                &hop.symbol,
                detail,
                source.as_deref_mut(),
                budget,
                &mut notes,
            )
        {
            break;
        }
    }
    let mut env = Envelope::new(body);
    for n in notes {
        env = env.suppressed(n);
    }
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

/// Print one symbol's source at the requested detail. Returns false when the budget
/// ran out mid-excerpt, so the caller stops the whole walk rather than emitting
/// half-symbols to the end of the list.
fn emit_excerpt(
    out: &mut String,
    indent: &str,
    sym: &SymbolRow,
    detail: Detail,
    source: Option<&mut Source>,
    budget: &mut Budget,
    notes: &mut Vec<String>,
) -> bool {
    let (Some(src), Some(def)) = (source, sym.def.as_ref()) else {
        return true;
    };
    let ex = src.excerpt(&def.path, def.line, sym.def_end_line, detail);
    if let Some(note) = ex.note {
        notes.push(format!("[{}] {note}", sym.handle));
    }
    for (n, text) in ex.lines {
        if !budget.push(out, &format!("{indent}{n:>5} | {text}")) {
            notes.push(format!("[{}] excerpt cut by the budget", sym.handle));
            return false;
        }
    }
    true
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
/// The known-unknowns report.
///
/// Written to be read by an agent deciding whether to trust the rest: the shape is
/// "here is what is solid, here is what is not, here is what to do about it".
pub fn verify(r: &cairn_store::Report) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(body, "index coverage");
    let _ = writeln!(body, "  files                    {:>8}", r.files);
    let _ = writeln!(body, "  symbols                  {:>8}", r.symbols);
    let _ = writeln!(
        body,
        "  references               {:>8}   {} with a caller",
        r.references,
        r.references - r.references_without_caller
    );

    let mut env = Envelope::new(body);

    // Everything below is a limit on what answers can mean. It is stated even when
    // benign, because an agent cannot infer a gap it was never told about.
    if r.symbols_without_definition > 0 {
        env = env.unknown(format!(
            "{} symbols are referenced but defined nowhere in the index (third-party \
             or unresolved); `expand` cannot show their source",
            r.symbols_without_definition
        ));
    }
    if r.definitions_without_body_span > 0 {
        env = env.unknown(format!(
            "{} definitions have no body extent from the indexer; `--detail body` \
             shows only their definition line",
            r.definitions_without_body_span
        ));
    }
    if r.references_without_caller > 0 {
        env = env.unknown(format!(
            "{} references sit outside any function body (imports, module-level use) \
             and therefore have no caller in the call graph",
            r.references_without_caller
        ));
    }
    if r.ambiguous_definitions > 0 {
        env = env.unknown(format!(
            "{} symbols are defined in more than one file; ranking shows one",
            r.ambiguous_definitions
        ));
    }
    if r.generated_by_path_only > 0 {
        env = env.unknown(format!(
            "{} files were called generated on a filename pattern alone, which is the \
             unreliable signal - reindex with --repo so headers can be read",
            r.generated_by_path_only
        ));
    }
    if r.files_without_content_hash > 0 {
        env = env.unknown(format!(
            "{} files have no recorded content hash, so their staleness cannot be checked",
            r.files_without_content_hash
        ));
    }
    if r.weak_edges > 0 {
        env = env.unknown(format!(
            "{} weak links are candidates, not facts; confirm before relying on one",
            r.weak_edges
        ));
    }

    if !r.staleness_checked {
        env = env.unknown(
            "staleness was not checked: pass --repo to compare the index against the \
             working tree",
        );
    } else {
        if !r.stale_files.is_empty() {
            let sample: Vec<&str> = r.stale_files.iter().take(5).map(|s| s.as_str()).collect();
            env.stale.push(format!(
                "{} files changed since indexing: {}{}",
                r.stale_files.len(),
                sample.join(", "),
                if r.stale_files.len() > 5 { ", ..." } else { "" }
            ));
        }
        if !r.missing_files.is_empty() {
            env.stale.push(format!(
                "{} indexed files no longer exist on disk",
                r.missing_files.len()
            ));
        }
    }
    if r.concept_links_dangling > 0 {
        env = env.unknown(format!(
            "{} authored links point at symbols no longer in the index; the code was              renamed or removed after the claim was made",
            r.concept_links_dangling
        ));
    }
    if r.concept_links_stale > 0 {
        env = env.unknown(format!(
            "{} authored links are anchored in code that has since changed and need a              fresh judgement (`cairn concept show`)",
            r.concept_links_stale
        ));
    }
    if r.manual_edges_stale > 0 {
        env = env.unknown(format!(
            "{} hand-authored links are anchored in code that has since changed. The \
             static pass cannot re-derive them - they need a fresh judgement (rerun \
             `cairn verify --flag-stale`, then review with `cairn links`)",
            r.manual_edges_stale
        ));
    }
    env
}

/// Hand-authored links touching a symbol.
pub fn asserted(
    store: &cairn_store::Store,
    sym: &SymbolRow,
    links: &[cairn_store::verify::AssertedLink],
    budget: &mut Budget,
) -> anyhow::Result<Envelope> {
    let mut body = String::new();
    let _ = writeln!(
        body,
        "asserted links for [{}] {}",
        sym.handle,
        sym.qualified()
    );
    let mut needs_review = 0;
    for l in links {
        let other = if l.src == sym.id { l.dst } else { l.src };
        let other_name = store
            .symbol(other)?
            .map(|s| format!("[{}] {}", s.handle, s.qualified()))
            .unwrap_or_else(|| format!("symbol {other}"));
        let arrow = if l.src == sym.id { "->" } else { "<-" };
        let flag = if l.needs_review {
            needs_review += 1;
            "  !! anchor changed, needs review"
        } else {
            ""
        };
        let anchor = l.anchor.as_deref().unwrap_or("<no anchor>");
        let line = format!(
            "  {arrow} {other_name}   [{}]  {anchor}{flag}\n     why: {}",
            l.source.label(),
            l.note
        );
        if !budget.push(&mut body, &line) {
            break;
        }
    }
    if links.is_empty() {
        let _ = writeln!(body, "  (none)");
    }
    let mut env = Envelope::new(body);
    if needs_review > 0 {
        env = env.unknown(format!(
            "{needs_review} of these are anchored in code that changed after they were \
             recorded; treat them as claims to re-check, not as facts"
        ));
    }
    Ok(env)
}

/// A concept and everything attached to it.
pub fn concept(
    store: &cairn_store::Store,
    c: &cairn_store::Concept,
    links: &[cairn_store::ConceptLink],
    budget: &mut Budget,
) -> anyhow::Result<Envelope> {
    let mut body = String::new();
    let _ = writeln!(body, "{}/{}   [{}]", c.ns, c.name, c.author.label());
    if !c.note.is_empty() {
        let _ = writeln!(body, "  {}", c.note);
    }
    let _ = writeln!(body, "{} linked symbols", links.len());

    let mut needs_review = 0;
    let mut unanchored = 0;
    for l in links {
        let label = if l.resolved {
            store
                .symbol(l.symbol_id)?
                .map(|s| symbol_line(&s))
                .unwrap_or_else(|| "<gone from index>".to_string())
        } else {
            "<symbol no longer in the index>".to_string()
        };
        if l.needs_review {
            needs_review += 1;
        }
        if l.anchor.is_none() {
            unanchored += 1;
        }
        let flag = if l.needs_review {
            "  !! anchor changed"
        } else if !l.resolved {
            "  !! symbol gone"
        } else {
            ""
        };
        let note = if l.note.is_empty() {
            String::new()
        } else {
            format!("  — {}", l.note)
        };
        if !budget.push(&mut body, &format!("  {:<12} {label}{note}{flag}", l.rel)) {
            break;
        }
    }
    if links.is_empty() {
        let _ = writeln!(body, "  (nothing linked yet)");
    }

    let mut env = Envelope::new(body);
    if needs_review > 0 {
        env = env.unknown(format!(
            "{needs_review} links are anchored in code that changed after they were \
             recorded; the static pass cannot re-derive them, so they need a fresh look"
        ));
    }
    if unanchored > 0 {
        env = env.unknown(format!(
            "{unanchored} links have no anchor and can never be checked against the code"
        ));
    }
    env = env.unknown("concepts are asserted knowledge, not derived facts");
    Ok(env)
}

pub fn concept_list(list: &[cairn_store::Concept], budget: &mut Budget) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(body, "{} concepts", list.len());
    for c in list {
        let note = if c.note.is_empty() { String::new() } else { format!("  — {}", c.note) };
        if !budget.push(
            &mut body,
            &format!("  {:<28} {:>3} links{note}", format!("{}/{}", c.ns, c.name), c.link_count),
        ) {
            break;
        }
    }
    if list.is_empty() {
        let _ = writeln!(body, "  (none)");
    }
    Envelope::new(body)
}
