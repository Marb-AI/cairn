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
pub use source::{Detail, Excerpt, SiteContext, Source};

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

pub fn symbols(
    rows: &[SymbolRow],
    query: &str,
    coverage: &str,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(body, "{} matches for \"{}\"", rows.len(), query);
    let defined_in: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.def.as_ref().map(|d| d.path.as_str()))
        .collect();
    if let Some(note) = concentration_note(&defined_in, "matches") {
        let _ = writeln!(body, "{note}");
    }
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

    // A miss, or a set of hits that are all generated, most often means the caller is
    // asking about code this index does not cover. Saying what *is* covered turns a
    // sequence of probes into one answer.
    let all_generated = !rows.is_empty()
        && rows
            .iter()
            .all(|r| r.def.as_ref().map(|d| d.generated).unwrap_or(false));
    if rows.is_empty() {
        env = env.unknown(format!(
            "nothing by that name. {coverage} - if the code you mean lives elsewhere, \
             it is not in this index and grep is the tool"
        ));
    } else if all_generated {
        env = env.unknown(format!(
            "every match is in generated code, which usually means the thing you meant \
             is not indexed under this name. {coverage}"
        ));
    }
    env
}

/// Reference list, optionally with the source line at each site.
///
/// A bare `path:line` is a location, not information: every measured task spent most of
/// its budget turning locations into understanding by opening files. Naming the
/// enclosing function and showing the line itself usually settles whether a site
/// matters, at a fraction of the cost of reading the file.
pub fn references_with_context(
    sym: &SymbolRow,
    refs: &[Occurrence],
    suppressed_generated: i64,
    // How many the filters matched before --limit. Without it a truncated list looked
    // complete, which is the failure the envelope exists to prevent.
    total: i64,
    source: Option<&mut Source>,
    ctx: SiteContext,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    // Leading, not trailing: a recommendation the caller reads after paying for the
    // whole answer has not saved them anything.
    if let Some(note) = routing_note(sym) {
        let _ = writeln!(body, "{note}\n");
    }
    let _ = writeln!(body, "references to [{}] {}", sym.handle, sym.qualified());
    if let Some(def) = &sym.def {
        let _ = writeln!(body, "  defined at {}", def.location());
    }
    let _ = writeln!(body, "{} references                    [L0, exact]", refs.len());

    let paths: Vec<&str> = refs.iter().map(|r| r.path.as_str()).collect();
    if let Some(note) = concentration_note(&paths, "references") {
        let _ = writeln!(body, "{note}");
    }

    let mut source = source;
    let mut shown = 0usize;
    for (i, r) in refs.iter().enumerate() {
        let inside = r
            .enclosing
            .as_deref()
            .map(|e| format!("  in {e}"))
            .unwrap_or_else(|| "  (module level)".to_string());
        let lines = source
            .as_deref_mut()
            .map(|src| src.site(&r.path, r.line, ctx))
            .unwrap_or_default();
        let inline = if lines.len() == 1 {
            format!("   |  {}", lines[0].1.trim())
        } else {
            String::new()
        };
        if !budget.push(&mut body, &format!("  {:<46}{inside}{inline}", r.location())) {
            break;
        }
        if lines.len() > 1 {
            for (n, text) in &lines {
                let marker = if *n as i64 == r.line + 1 { ">" } else { " " };
                if !budget.push(&mut body, &format!("      {marker}{n:>5} | {text}")) {
                    break;
                }
            }
        }
        shown = i + 1;
    }
    if refs.is_empty() {
        let _ = writeln!(body, "  (none outside generated code)");
    }

    let mut env = Envelope::new(body);
    let dropped = total - refs.len() as i64;
    if dropped > 0 {
        env = env.suppressed(format!(
            "{dropped} more references beyond --limit ({total} in total). Raise --limit, \
             or use `cairn usage` for the same answer grouped by file"
        ));
    }
    if shown < refs.len() {
        env = env.suppressed(budget.cut_note(refs.len() - shown, "references"));
    }
    if suppressed_generated > 0 {
        env = env.suppressed(format!(
            "{suppressed_generated} references in generated code (rerun with --include-generated)"
        ));
    }
    if let Some(note) = attribute_caveat(sym) {
        env = env.unknown(note);
    }
    env
}

pub fn references(
    sym: &SymbolRow,
    refs: &[Occurrence],
    suppressed_generated: i64,
    total: i64,
) -> Envelope {
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
    let dropped = total - refs.len() as i64;
    if dropped > 0 {
        env = env.suppressed(format!(
            "{dropped} more references beyond --limit ({total} in total). Raise --limit or \
             use `cairn usage` for the same answer grouped by file"
        ));
    }
    if suppressed_generated > 0 {
        env = env.suppressed(format!(
            "{suppressed_generated} references in generated code (rerun with --include-generated)"
        ));
    }
    if let Some(note) = attribute_caveat(sym) {
        env = env.unknown(note);
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
    // Measured (task F): a correct three-hop path was returned as skeletons, and the agent
    // then opened every hop by hand — 37 tool calls where the same command with
    // `--detail body` would have delivered the chain and its source together. The option
    // existed and was in the guide; the guide is not where an agent is looking when it has
    // an answer in front of it.
    if detail == Detail::Skeleton && hops.len() > 1 {
        env = env.unknown(
            "this is the shape of the path, not what it does. If the question is what              happens along it, re-run with `--detail body --repo <dir>`: same call, every              hop's source, and no need to open the files one at a time",
        );
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

/// The dirty overlay for one file: what the language server sees now, against what the
/// index recorded.
///
/// The comparison is the product, not the listing. An agent asking about a file it just
/// edited needs to know which symbols the index no longer describes correctly - showing
/// only the live list would leave it to diff two outputs by eye.
pub fn live_overlay(
    path: &str,
    live: &[LiveSymbolView],
    indexed: &[(String, i64, Option<i64>)],
    budget: &mut Budget,
) -> Envelope {
    use std::collections::HashSet;
    // Compare qualified names on both sides. Two classes in a file can share a method
    // name, and matching bare names pairs them and invents a move.
    let live_q: Vec<(String, &LiveSymbolView)> =
        live.iter().map(|s| (s.qualified(), s)).collect();
    let live_names: HashSet<&str> = live_q.iter().map(|(q, _)| q.as_str()).collect();
    let indexed_names: HashSet<&str> = indexed.iter().map(|(n, _, _)| n.as_str()).collect();

    let mut body = String::new();
    let _ = writeln!(
        body,
        "{path}   {} symbols live, {} in the index        [L0, live]",
        live.len(),
        indexed.len()
    );

    let mut added = 0;
    let mut moved = 0;
    for (qualified, s) in &live_q {
        let known = indexed.iter().find(|(n, _, _)| n == qualified);
        let (mark, note) = match known {
            None => {
                added += 1;
                ("+", String::new())
            }
            Some((_, line, _)) if *line != s.start_line => {
                moved += 1;
                ("~", format!("  (index says line {})", line + 1))
            }
            Some(_) => (" ", String::new()),
        };
        if !budget.push(
            &mut body,
            &format!(
                "{mark} {}:{}-{}  {qualified}{note}",
                path,
                s.start_line + 1,
                s.end_line + 1
            ),
        ) {
            break;
        }
    }

    let gone: Vec<&str> = indexed_names.difference(&live_names).copied().collect();
    for name in &gone {
        let _ = budget.push(&mut body, &format!("- {name}   (in the index, not in the file now)"));
    }

    let mut env = Envelope::new(body);
    if added + moved + gone.len() > 0 {
        env.stale.push(format!(
            "the index is behind for this file: {added} new, {moved} moved, {} gone",
            gone.len()
        ));
    }
    env = env.unknown(
        "this view is `documentSymbol` only: it shows what is defined in the file now,          not what references it. Reference answers still come from the index.",
    );
    env
}

/// Mirror of the daemon's live symbol shape, so cairn-fmt does not depend on the
/// daemon crate for one struct.
pub struct LiveSymbolView {
    pub name: String,
    pub kind: i64,
    pub start_line: i64,
    pub end_line: i64,
    pub container: Option<String>,
}

impl LiveSymbolView {
    /// `Class.method`, matching how the index qualifies its own names.
    pub fn qualified(&self) -> String {
        match &self.container {
            Some(c) if !c.is_empty() => format!("{c}.{}", self.name),
            _ => self.name.clone(),
        }
    }

    /// LSP `SymbolKind::Variable`. Locals and parameters arrive as variables nested in
    /// a function, and they are not file structure.
    pub fn is_local_variable(&self) -> bool {
        self.kind == 13 && self.container.is_some()
    }
}

/// Entry point by concept: seeds, why each one is here, and how far to trust it.

/// Say so when the seeds carry no navigational signal.
///
/// Measured (task A, lost three times): asked to list the MCP tools and their required
/// scopes, an agent opened with `context` and got back two test files and a module
/// `__init__` — no symbol that answers anything, and no indication that this was the
/// tool's way of saying "I have nothing for this question". It kept querying. The answer
/// was a dictionary literal in one file the whole time.
///
/// A seed that is a module `__init__` says only "this module exists", and a seed in a test
/// file says only "something here mentions your words". When that is all there is, the
/// question is about what a file *contains* rather than how code connects, and the honest
/// move is to name the file and stop.
fn weak_seeds_note(seeds: &[cairn_store::Seed]) -> Option<String> {
    if seeds.is_empty() {
        return None;
    }
    let useless = |s: &cairn_store::Seed| -> bool {
        let is_init = s.symbol.name == "__init__";
        let is_test = s
            .symbol
            .def
            .as_ref()
            .map(|d| d.path.contains("/tests/") || d.path.contains("test_"))
            .unwrap_or(false);
        is_init || is_test
    };
    if !seeds.iter().all(useless) {
        return None;
    }
    // The best file to point at: a non-test seed if there is one, else the first.
    let best = seeds
        .iter()
        .find(|s| {
            s.symbol
                .def
                .as_ref()
                .map(|d| !d.path.contains("/tests/") && !d.path.contains("test_"))
                .unwrap_or(false)
        })
        .or_else(|| seeds.first())?;
    let path = best.symbol.def.as_ref()?.path.clone();
    Some(format!(
        "  STOP - every seed is a module __init__ or a test file, which means nothing here \n         \x20 answers your question. This looks like a question about what a file contains, \n         \x20 not about how code connects. Read {path} and do not keep querying."
    ))
}

pub fn context(
    query: &str,
    r: &cairn_store::ContextResult,
    coverage: &str,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(body, "context for \"{query}\"   {} seeds", r.seeds.len());
    if !r.concepts_matched.is_empty() {
        let _ = writeln!(body, "matched concepts: {}", r.concepts_matched.join(", "));
    }
    for s in &r.seeds {
        let why = s
            .sources
            .iter()
            .map(|x| x.label())
            .collect::<Vec<_>>()
            .join("+");
        if !budget.push(&mut body, &format!("{}  [{why}]", symbol_line(&s.symbol))) {
            break;
        }
    }
    if r.seeds.is_empty() {
        let _ = writeln!(body, "  (nothing matched)");
    }
    if let Some(note) = weak_seeds_note(&r.seeds) {
        let _ = writeln!(body, "{note}");
    }

    let mut env = Envelope::new(body);
    if r.low_confidence {
        // Handing over weak guesses dressed as answers is how a tool teaches an agent
        // to stop trusting it. Say the seed is thin, say what is covered, name the
        // fallback.
        env = env.unknown(format!(
            "low confidence: no concept, name or docstring matched strongly. {coverage} \
             - if what you want is outside that, it is not indexed and grep is the tool"
        ));
    }
    // Not when the seeds were already declared useless: telling the caller to expand a
    // seed set that answers nothing is the advice that cost task A three losses.
    if weak_seeds_note(&r.seeds).is_none() {
        env = env.unknown(
            "seeds are a starting point, not an answer - expand with `cairn graph <handle>`",
        );
    }
    env
}

/// Symbols in a subtree that production code never calls.
pub fn unreached(
    prefix: &str,
    rows: &[cairn_store::UnreachedSymbol],
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(
        body,
        "{} symbols under {prefix} with no production caller       [L1, exact]",
        rows.len()
    );
    for r in rows {
        let why = match r.why {
            cairn_store::Unreached::TestsOnly => format!("tests only ({})", r.test_callers),
            cairn_store::Unreached::Never => "no callers".to_string(),
        };
        if !budget.push(&mut body, &format!("  {:<22} {}", why, symbol_line(&r.symbol))) {
            break;
        }
    }
    if rows.is_empty() {
        let _ = writeln!(body, "  (everything here has a production caller)");
    }
    Envelope::new(body).unknown(
        "reachability is static: a symbol invoked by name at runtime looks unreached. \
         Check `cairn weaklinks <handle>` before deleting anything",
    )
}

/// What a module contains, and how used each thing is.
pub fn outline(
    prefix: &str,
    rows: &[cairn_store::OutlineEntry],
    total: i64,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(
        body,
        "{prefix}   {} of {total} definitions",
        rows.len()
    );
    for r in rows {
        let use_note = if r.production_callers > 0 {
            format!("{} prod", r.production_callers)
        } else if r.dispatched {
            // Not "test-only" and not "unused": a service binding reaches it, and the
            // static caller count is simply not evidence either way.
            "dispatched".to_string()
        } else if r.caller_count > 0 {
            format!("{} test-only", r.caller_count)
        } else {
            "unused".to_string()
        };
        if !budget.push(&mut body, &format!("  {:<12} {}", use_note, symbol_line(&r.symbol))) {
            break;
        }
    }
    if rows.is_empty() {
        // "Nothing indexed" and "everything here is generated, and I exclude that" are
        // different facts, and the first is a lie when the second is true — a caller
        // reasonably concludes the path is outside the index and stops looking.
        let _ = writeln!(
            body,
            "  (nothing here, or everything here is generated code, which this view \
             excludes - `cairn symbol <name>` still finds generated definitions)"
        );
    }
    {
        let mut env = Envelope::new(body);
        let dropped = total - rows.len() as i64;
        if dropped > 0 {
            env = env.suppressed(format!(
                "{dropped} definitions beyond --limit. Whole files can be missing from \
                 this list; raise --limit or narrow the path"
            ));
        }
        env
    }
}

/// Where a symbol is used, grouped by file rather than listed flat.
/// When an answer turns out to be concentrated in one file, say so.
///
/// The measured losses were all tasks whose answer lived in a single file: the agent
/// paid for the skill, then for a query, then read the file anyway. The tool can see
/// this - it knows the paths - so it should say it rather than let the caller discover
/// it after paying.
///
/// Deliberately advisory and cheap: one line, and the results still print. Refusing to
/// answer would be worse than answering with a hint.
pub fn concentration_note(paths: &[&str], noun: &str) -> Option<String> {
    // Was three. Measured (task A, three losses): a symbol whose one or two use sites sit
    // in the file that defines it is the exact case SKILL.md's "the answer is in one file
    // you already know" describes, and the threshold meant the tool never said so for the
    // smallest and clearest instances of it.
    if paths.is_empty() {
        return None;
    }
    let first = paths.first()?;
    if !paths.iter().all(|p| p == first) {
        return None;
    }
    Some(format!(
        "ALL {} {noun} ARE IN ONE FILE: {first}. Reading it is probably cheaper than \
         further queries.",
        paths.len()
    ))
}

/// When the index cannot answer a question well, say so *first* and hand over the tool
/// that can.
///
/// Measured cost of not doing this: asked for the blast radius of a Django model field,
/// an agent read the partial list, correctly distrusted it because of the caveat, and
/// then did the full grep pass anyway - paying for both. Honesty buried at the bottom of
/// a long partial answer is the worst of both worlds.
///
/// So for symbols the resolver is known to under-report, the answer leads with the
/// recommendation and includes the command, and the partial list is kept short. One
/// cheap call that routes correctly beats an expensive one that has to be redone.
pub fn routing_note(sym: &SymbolRow) -> Option<String> {
    if !cairn_store::query::is_under_resolved_attribute(sym.kind, sym.container.as_deref()) {
        return None;
    }
    Some(format!(
        "USE GREP FOR THIS ONE. `{}` is an attribute on a type, and attribute access \
         resolves only where the holder's type is known - for ORM instances and \
         dicts-as-records it usually is not, so the list below is a lower bound and \
         not a blast radius. Run instead:  grep -rn '{}' <src> --include='*.py'",
        sym.qualified(),
        sym.name
    ))
}

/// Note added when the index is likely to under-report a symbol's uses.
fn attribute_caveat(sym: &SymbolRow) -> Option<String> {
    cairn_store::query::is_under_resolved_attribute(sym.kind, sym.container.as_deref()).then(|| {
        format!(
            "`{}` is an attribute on a type. Uses are only resolved where the holder's \
             type is known, which for ORM models, dicts-as-records and dynamically built \
             objects it often is not - so this list is a lower bound. Cross-check with \
             grep before treating it as the blast radius.",
            sym.qualified()
        )
    })
}

pub fn usage(sym: &SymbolRow, rows: &[(String, i64, bool)], budget: &mut Budget) -> Envelope {
    let mut body = String::new();
    if let Some(note) = routing_note(sym) {
        let _ = writeln!(body, "{note}\n");
    }
    let total: i64 = rows.iter().map(|(_, n, _)| n).sum();
    let _ = writeln!(
        body,
        "[{}] {} used at {total} sites in {} files      [L0, exact]",
        sym.handle,
        sym.qualified(),
        rows.len()
    );
    if rows.len() == 1 && !rows[0].1.eq(&0) {
        let _ = writeln!(
            body,
            "ALL USES ARE IN ONE FILE: {}. Reading it is probably cheaper than further \
             queries.",
            rows[0].0
        );
    }
    for (path, n, is_test) in rows {
        let tag = if *is_test { "  [test]" } else { "" };
        if !budget.push(&mut body, &format!("  {n:>4}x  {path}{tag}")) {
            break;
        }
    }
    if rows.is_empty() {
        let _ = writeln!(body, "  (no uses outside its own definition)");
    }
    let mut env = Envelope::new(body);
    if let Some(note) = attribute_caveat(sym) {
        env = env.unknown(note);
    }
    env
}

/// Reachability across a gRPC service boundary.
///
/// The header names the service, because the answer is only trustworthy if the caller
/// can see the route: these edges are recovered from a generator's naming convention,
/// not resolved by a compiler, and a reader who cannot check the hop should not be
/// asked to believe it.
/// Callers of one RPC, which is a stronger claim than callers of its handler: these are
/// real call sites, not a naming convention, so they are labelled exact.
pub fn rpc_reaches(
    sym: &SymbolRow,
    callers: &[cairn_store::RpcCaller],
    whole_type: bool,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(
        body,
        "[{}] {} — {} caller(s) across gRPC, by RPC        [L1, exact]",
        sym.handle,
        sym.qualified(),
        callers.len()
    );
    let _ = writeln!(
        body,
        "  {}. The package is part of the answer: two packages can carry the same \
         service name to different processes",
        if whole_type {
            "every RPC this handler serves, with the callers of each - one call, not one per method"
        } else {
            "this RPC only, not everything its handler serves"
        }
    );
    let mut shown = 0usize;
    for (i, c) in callers.iter().enumerate() {
        if !budget.push(
            &mut body,
            &format!(
                "  {}  [{}.{}.{}]",
                symbol_line(&c.symbol),
                c.pkg,
                c.service,
                c.rpc
            ),
        ) {
            break;
        }
        shown = i + 1;
    }
    let mut env = Envelope::new(body);
    if shown < callers.len() {
        env = env.suppressed(budget.cut_note(callers.len() - shown, "callers"));
    }
    env = env.unknown(
        "these are calls to the generated client. A service that reaches this some other \
         way - a hand-written transport, a REST gateway, a queue - is not here",
    );
    env
}

pub fn cross_language(
    sym: &SymbolRow,
    services: &[(String, String, cairn_store::ServiceRole)],
    links: &[cairn_store::CrossLink],
    outgoing: bool,
    via: Option<&SymbolRow>,
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let direction = if outgoing { "reached by" } else { "reaches" };
    let _ = writeln!(
        body,
        "[{}] {} — {} {} across gRPC        [L1, convention]",
        sym.handle,
        sym.qualified(),
        links.len(),
        if outgoing { "targets" } else { "callers" }
    );
    if let Some(owner) = via {
        let _ = writeln!(
            body,
            "  answered for the enclosing type [{}] {}: a service binding names the \
             handler, not each of its RPC methods",
            owner.handle,
            owner.qualified()
        );
    }
    if !services.is_empty() {
        let list: Vec<String> = services
            .iter()
            .map(|(p, n, r)| format!("{} {p}.{n}", r.label()))
            .collect();
        let _ = writeln!(body, "  via: {}", list.join(", "));
    }
    let _ = writeln!(body, "  ({direction} it, in the other language)");

    let mut shown = 0usize;
    for (i, l) in links.iter().enumerate() {
        if !budget.push(
            &mut body,
            &format!("  {}  [{}.{}]", symbol_line(&l.symbol), l.pkg, l.service),
        ) {
            break;
        }
        shown = i + 1;
    }
    if links.is_empty() {
        let _ = writeln!(body, "  (nothing on the other side of a service boundary)");
    }

    let mut env = Envelope::new(body);
    if shown < links.len() {
        env = env.suppressed(budget.cut_note(links.len() - shown, "links"));
    }
    env = env.unknown(
        "these edges come from the protobuf generator's naming convention, not from a \
         compiler. They are exact where the convention holds, and blind to any service \
         wired up by hand or reached over a transport other than the generated client.",
    );
    env
}

/// The deployment map: services, what starts each one, and where that lands in the code.
pub fn topology(
    rows: &[(String, Option<String>, Option<String>, String, bool)],
    budget: &mut Budget,
) -> Envelope {
    let mut body = String::new();
    let resolved = rows.iter().filter(|r| r.4).count();
    let _ = writeln!(
        body,
        "{} services, {resolved} with a resolved entrypoint        [L0-D, exact]",
        rows.len()
    );
    let mut unresolved = Vec::new();
    for (name, command, entry_path, ports, ok) in rows {
        let where_ = entry_path
            .clone()
            .unwrap_or_else(|| if *ok { String::new() } else { "—".into() });
        let p = if ports.is_empty() { String::new() } else { format!("  :{ports}") };
        if !*ok && command.is_some() {
            unresolved.push(name.clone());
        }
        if !budget.push(
            &mut body,
            &format!(
                "  {:<24} {:<44} {}{p}",
                name,
                truncate(command.as_deref().unwrap_or("(image default)"), 44),
                where_
            ),
        ) {
            break;
        }
    }
    let mut env = Envelope::new(body);
    if !unresolved.is_empty() {
        // An unresolved entrypoint makes everything that service runs look unreachable.
        // That silently reclassifies live code as dead, so it is stated, not counted.
        env = env.unknown(format!(
            "{} services have a start command that could not be resolved to code ({}). \
             Anything only those run will look unreachable",
            unresolved.len(),
            unresolved.join(", ")
        ));
    }
    env
}

/// Which deployed services can reach a symbol.
pub fn runs_in(
    sym: &SymbolRow,
    services: &[String],
    depth: usize,
    via: Option<&SymbolRow>,
    blind: &[String],
) -> Envelope {
    let mut body = String::new();
    let _ = writeln!(
        body,
        "[{}] {} runs in {} service(s)        [L1 + L0-D, exact]",
        sym.handle,
        sym.qualified(),
        services.len()
    );
    if let Some(owner) = via {
        let _ = writeln!(
            body,
            "  answered for the enclosing type [{}] {}: nothing calls the method \
             statically, which for a dispatched method means the caller is a table, \
             not that the code is dead",
            owner.handle,
            owner.qualified()
        );
    }
    for s in services {
        let _ = writeln!(body, "  {s}");
    }
    if services.is_empty() {
        let _ = writeln!(body, "  (no service entrypoint reaches it)");
    }
    let mut env = Envelope::new(body);
    if services.is_empty() {
        env = env.unknown(format!(
            "no service reaches this within {depth} call hops. That can mean dead code, \
             a deeper path, or an entrypoint the topology could not resolve - check \
             `cairn topology` before concluding it is unused"
        ));
    }
    if !blind.is_empty() {
        // Stated on every answer, not only on the empty one: a *non-empty* list is
        // where this misleads, because it looks complete.
        env = env.unknown(format!(
            "this list covers only what a service runs at start-up. {} service(s) start \
             nothing ({}) and run code on demand instead - cron, a management command, \
             `docker exec`. If one of those could invoke this, grep its cron and entrypoint \
             scripts; reachability cannot see them",
            blind.len(),
            blind.join(", ")
        ));
    }
    env
}

/// The whole answer to "what does changing this touch", shaped like the answer.
///
/// Laid out so it can be used as the deliverable rather than as material for one: the
/// in-process services with the command that makes them the runner, then the network
/// route hop by hop with the RPC that carries it. The `unknown:` section names what
/// reachability cannot see, so a caller can tell a complete answer from a partial one
/// without going to check — which is the whole point of answering in one call.
pub fn affects(sym: &SymbolRow, a: &cairn_store::Affects, budget: &mut Budget) -> Envelope {
    let mut body = String::new();
    let services: usize = a.in_process.len()
        + a.hops
            .iter()
            .flat_map(|h| h.from.iter())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
    let _ = writeln!(
        body,
        "[{}] {} — affects {} deployed service(s)        [L1 + L0-D]",
        sym.handle,
        sym.qualified(),
        services
    );

    let _ = writeln!(body, "\nin-process");
    for p in &a.in_process {
        // An on-demand row already carries its trigger and its command in the label, and
        // the container's own start command is not what runs this code.
        let line = if p.service.contains(" (cron ") || p.service.contains(" (on demand") {
            format!("  {}", p.service)
        } else {
            format!(
                "  {:<20} {}",
                p.service,
                p.command.as_deref().unwrap_or("(image default)")
            )
        };
        let _ = budget.push(&mut body, &line);
    }
    if a.in_process.is_empty() {
        let _ = writeln!(body, "  (no service entrypoint reaches it)");
    }

    // Grouped by route and service rather than one line per RPC. Ungrouped, a real
    // question produced 70 lines carrying maybe six distinct facts, and the whole reason
    // this command exists is that the answer should cost less than assembling it.
    if !a.hops.is_empty() {
        // Spelled out because an agent read the grouped list as "calls into that service"
        // and discarded two thirds of it as unrelated. Every row is a route to the symbol
        // that was asked about; nothing is listed for context.
        let _ = writeln!(
            body,
            "\nover the network, by hop - every route below reaches this symbol"
        );
        let mut groups: std::collections::BTreeMap<(String, String, String), (Vec<String>, String)> =
            std::collections::BTreeMap::new();
        for h in &a.hops {
            let from = if h.from.is_empty() {
                "(starts nothing)".to_string()
            } else if h.from_by_file {
                format!("{} ~", h.from.join(", "))
            } else {
                h.from.join(", ")
            };
            let to = if h.to.is_empty() { "?".to_string() } else { h.to.join(", ") };
            let site = h
                .call_site
                .def
                .as_ref()
                .map(|d| d.path.clone())
                .unwrap_or_default();
            let e = groups
                .entry((from, to, format!("{}.{}", h.pkg, h.service)))
                .or_insert_with(|| (Vec::new(), site));
            e.0.push(h.rpc.clone());
        }
        for ((from, to, service), (mut rpcs, site)) in groups {
            rpcs.sort();
            if !budget.push(
                &mut body,
                &format!(
                    "  {:<18} -> {:<16} {}  ({})\n    {} in {}",
                    from,
                    to,
                    service,
                    rpcs.len(),
                    rpcs.join(", "),
                    site
                ),
            ) {
                break;
            }
        }
    }

    if !a.outgoing.is_empty() {
        let _ = writeln!(
            body,
            "\ncalls out over the network - a change here changes what these receive"
        );
        for o in &a.outgoing {
            let who = if o.served_by.is_empty() {
                "(no deployed server resolved)".to_string()
            } else {
                o.served_by.join(", ")
            };
            if !budget.push(
                &mut body,
                &format!("  -> {:<22} {}.{}", who, o.pkg, o.service),
            ) {
                break;
            }
        }
    }

    let mut env = Envelope::new(body);
    if !a.blind.is_empty() {
        env = env.unknown(format!(
            "{} service(s) start nothing and run code on demand instead - cron, a \
             management command, `docker exec` ({}). Where one appears above it is on a \
             path; how it is triggered is not something reachability can see",
            a.blind.len(),
            a.blind.join(", ")
        ));
    }
    if a.hops.iter().any(|h| h.from_by_file) {
        env = env.unknown(
            "`~` marks a service attributed through the file the call sits in rather than \
             through a call path: a framework route handler has no static caller, so this \
             says the module is loaded there, not that the handler is reached from the \
             entrypoint",
        );
    }
    if a.truncated_hops {
        env = env.suppressed("the service chain continued past the hop limit".to_string());
    }
    env = env.unknown(
        "hops are calls through a generated gRPC client, and are exact. A service that \
         reaches this some other way - a hand-written transport, a queue, an HTTP call - \
         is not here",
    );
    env = env.unknown(
        "the in-process side follows static calls only. A module-level binding is \
         followed, so `af = wrap(f)` does not break the chain, but a task queue, a \
         registry of callables or a name resolved at run time does - `cairn weaklinks` \
         is where those candidates live",
    );
    env
}
