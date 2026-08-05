//! `cairn for <purpose>` — the entry point that takes what you are doing rather than
//! which mechanism to run.
//!
//! The measured problem it exists for: across sixty runs the agents' statements of
//! *purpose* were consistently right and their choice of *mechanism* was consistently
//! what cost them. One run spent eight calls across `literal`, `symbol`, `docs --about`
//! and `usage` on a question about a compose variable, which no symbol command answers.
//! Another reached for `path`, which needs both ends of a chain, when the question was
//! what the far end is — and the run that never touched `path` was the fastest of the
//! three.
//!
//! Two rules hold this together, and both come from things that measurably worked:
//!
//! 1. **Every block names the command that produced it.** Without that the answer is a
//!    slot machine: an agent that cannot see where a block came from cannot refine it,
//!    and goes back to guessing. With it, the second iteration is one copy-paste.
//! 2. **A redirect is spoken, never silent.** Asked to change something that turns out
//!    not to be a symbol, this says so and names the purpose that does fit. A router that
//!    quietly picks another strategy is the confident-and-wrong failure this tool exists
//!    to avoid.

use crate::treefind;
use anyhow::Result;
use cairn_fmt::{Budget, Envelope};
use cairn_store::Store;
use std::path::Path;

/// `for find`: search the working tree, attribute every hit.
pub fn find(
    store: &Store,
    repo: &Path,
    needle: &str,
    limit: usize,
    budget: &mut Budget,
) -> Result<(Envelope, bool)> {
    let found = treefind::search(repo, needle, limit);
    let mut lines = Vec::with_capacity(found.hits.len());
    for h in &found.hits {
        lines.push(cairn_fmt::FoundLine {
            path: h.path.clone(),
            line: h.line,
            text: h.text.clone(),
            context: store.line_context(&h.path, h.line as i64)?,
            around: h.context.clone(),
        });
    }
    let any = !lines.is_empty();
    let services = store.services_mentioning(needle)?;
    let env = cairn_fmt::found(
        needle,
        &lines,
        &services,
        found.skipped_large,
        found.truncated,
        found.files_read,
        budget,
    );
    Ok((env, any))
}

/// Does this subject look like text rather than a symbol?
///
/// Only used to *speak* a redirect, never to switch silently. The test is deliberately
/// crude — it fires on the shapes that actually appeared (an ALL_CAPS environment
/// variable, a hyphenated header name, anything with a space or a slash) and says so as a
/// suggestion the caller can ignore.
pub fn looks_like_text(subject: &str) -> bool {
    subject.contains(' ')
        || subject.contains('/')
        || subject.contains('-')
        || (subject.len() > 3
            && subject
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()))
}


/// `for change`: what a modification to this symbol reaches, assembled.
///
/// Four blocks, and each one is here because a measured run asked for it separately after
/// the first version answered. The arm's trace was identical in all three runs of round
/// three: `for change` gave callers and services, then `refs` for the source at those
/// sites, then `refs` again on the async wrapper, then `weaklinks` to rule out dynamic
/// dispatch. Those are not four questions. They are one question the answer was missing
/// three quarters of.
pub fn change(
    store: &Store,
    repo: Option<&Path>,
    symbol_id: i64,
    budget: &mut Budget,
) -> Result<(Envelope, bool)> {
    use std::fmt::Write;
    let sym = store
        .symbol(symbol_id)?
        .ok_or_else(|| anyhow::anyhow!("handle resolved to a missing symbol"))?;

    let mut body = String::new();
    let mut unknown: Vec<String> = Vec::new();
    let mut rows = 0usize;

    // 1. The sites to edit, with the source at each. `refs` rather than the call graph:
    //    a signature change is edited at occurrences, and reading them is what the arm
    //    did next every single time.
    let (refs, suppressed_generated, total) = store.references(symbol_id, false, 40)?;
    let ctx = cairn_fmt::SiteContext::auto(None, refs.len());
    let mut source = repo.map(|r| cairn_fmt::Source::new(r.to_path_buf()));
    let sites = cairn_fmt::references_with_context(
        &sym,
        &refs,
        suppressed_generated,
        total,
        source.as_mut(),
        ctx,
        budget,
    );
    let _ = writeln!(body, "{}", sites.body.trim_end());
    let _ = writeln!(
        body,
        "  (from `cairn refs {} --context auto`)\n",
        sym.handle
    );
    rows += sites.rows.unwrap_or(0);
    unknown.extend(sites.unknown);

    // 2. The hop the arm always had to make by hand. A repository function is reached
    //    through `af = db_async(f)`, a module-level binding: it shows up as a caller, and
    //    its own callers are the ones that actually break. Followed one step, which is
    //    where this codebase's wrappers stop.
    let callers = store.walk(
        symbol_id,
        cairn_store::EdgeKind::Calls,
        cairn_store::Direction::In,
        1,
        40,
        false,
    )?;
    for node in callers.nodes.iter().filter(|n| n.symbol.id != symbol_id) {
        if !matches!(node.symbol.kind, cairn_scip::SymbolKind::Term) {
            continue;
        }
        let (through, _, _) = store.references(node.symbol.id, false, 12)?;
        if through.is_empty() {
            continue;
        }
        let _ = writeln!(
            body,
            "reached through the binding [{}] {} — these break too:",
            node.symbol.handle,
            node.symbol.qualified()
        );
        for r in &through {
            if budget.push(&mut body, &format!("  {}:{}", r.path, r.line)) {
                rows += 1;
            }
        }
        let _ = writeln!(body, "  (from `cairn refs {}`)\n", node.symbol.handle);
    }

    // 3. The deployed radius.
    let affected = store.affects(symbol_id, 12, 40)?;
    let radius = cairn_fmt::affects(&sym, &affected, budget);
    let _ = writeln!(body, "{}", radius.body.trim_end());
    let _ = writeln!(body, "  (from `cairn affects {}`)\n", sym.handle);
    rows += radius.rows.unwrap_or(0);
    unknown.extend(radius.unknown);

    // 4. The dynamic-dispatch check, stated rather than left to be asked. One line when
    //    it is clean, which is the common case and the whole point: the arm ran
    //    `weaklinks` to be told nothing.
    let weak = store.weak_sites(symbol_id, 10)?;
    if weak.is_empty() {
        let _ = writeln!(
            body,
            "no string literal anywhere names this symbol, so nothing reaches it by a \
             name resolved at run time  (from `cairn weaklinks {}`)",
            sym.handle
        );
    } else {
        let _ = writeln!(
            body,
            "{} string literal(s) name this symbol — candidates for dynamic dispatch, \
             check them before trusting the list above  (`cairn weaklinks {}`)",
            weak.len(),
            sym.handle
        );
        for (site, score) in weak.iter().take(6) {
            if budget.push(&mut body, &format!("  {site}  ({score:.2})")) {
                rows += 1;
            }
        }
    }

    let mut env = Envelope::new(body).rows(rows);
    for note in unknown {
        env = env.unknown(note);
    }
    for note in radius.suppressed.into_iter().chain(sites.suppressed) {
        env = env.suppressed(note);
    }
    Ok((env, rows > 0))
}

/// The candidates a `change` question can plausibly mean.
///
/// Generated definitions are dropped, and that is a judgement the *intent* licenses rather
/// than a guess about which symbol: nobody hand-edits a protobuf stub, so it cannot be
/// what "I am going to modify this" refers to. For `get_quota_status` that removes four of
/// seven candidates in one honest step.
pub fn change_candidates(store: &Store, name: &str) -> Result<Vec<cairn_store::SymbolRow>> {
    let mut out = Vec::new();
    for s in store.symbols_named(name)? {
        if s.def.as_ref().is_some_and(|d| d.generated) {
            continue;
        }
        out.push(s);
    }
    // Most-referenced first. A real signal, not a tie-break invented for the occasion —
    // and the choice it makes is printed with the alternatives, so a reader who disagrees
    // pays one copy-paste rather than one round trip.
    out.sort_by_key(|s| std::cmp::Reverse(s.ref_count));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::looks_like_text;

    #[test]
    fn the_shapes_that_cost_a_run_are_the_ones_it_catches() {
        // Scenario 8: eight calls into symbol commands for a compose variable.
        assert!(looks_like_text("MCP_SERVER_PORT"));
        // Scenario 7: a header name.
        assert!(looks_like_text("X-Api-Key"));
        assert!(looks_like_text("tools/sql/geoplatform"));
        assert!(looks_like_text("the folder client"));
    }

    #[test]
    fn an_ordinary_symbol_name_is_left_alone() {
        // A false positive here would send a real symbol question to a text search, which
        // is the redirect being wrong in the expensive direction.
        assert!(!looks_like_text("FolderServiceHandler"));
        assert!(!looks_like_text("get_quota_status"));
        assert!(!looks_like_text("d95"));
        assert!(!looks_like_text("NewFolderService"));
    }
}
