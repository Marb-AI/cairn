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
/// Two blocks, because the measurement showed the agent needs both and asks for them
/// separately: the deployed radius (`affects`, which is already intent-shaped and was the
/// one-round-trip win of the whole eval) and the call sites it would have to edit
/// (`graph --aspect callers`, which unlike `usage` does not drop test files). Both are
/// labelled with the command behind them.
pub fn change(
    store: &Store,
    symbol_id: i64,
    budget: &mut Budget,
) -> Result<(Envelope, bool)> {
    let sym = store
        .symbol(symbol_id)?
        .ok_or_else(|| anyhow::anyhow!("handle resolved to a missing symbol"))?;

    let affected = store.affects(symbol_id, 12, 40)?;
    let radius = cairn_fmt::affects(&sym, &affected, budget);

    // Depth 1: the sites that would fail to compile. Deeper is a different question and
    // asking it here would bury the answer under a transitive closure nobody edits.
    let callers = store.walk(
        symbol_id,
        cairn_store::EdgeKind::Calls,
        cairn_store::Direction::In,
        1,
        40,
        false,
    )?;
    let title = format!(
        "callers of [{}] {}   depth=1   [L1, exact]",
        sym.handle,
        sym.qualified()
    );
    let sites = cairn_fmt::walk(
        &callers,
        &title,
        cairn_fmt::View::List,
        cairn_fmt::Detail::Skeleton,
        None,
        budget,
    );

    let mut body = String::new();
    use std::fmt::Write;
    let _ = writeln!(body, "{}", sites.body.trim_end());
    let _ = writeln!(
        body,
        "  (from `cairn graph {} --aspect callers --depth 1`)\n",
        sym.handle
    );
    let _ = writeln!(body, "{}", radius.body.trim_end());
    let _ = writeln!(body, "  (from `cairn affects {}`)", sym.handle);

    let rows = sites.rows.unwrap_or(0) + radius.rows.unwrap_or(0);
    let mut env = Envelope::new(body).rows(rows);
    // Merged rather than summarised: each block's caveats belong to that block, and a
    // fused envelope that flattened them would lose the per-row provenance that made
    // agents read these lines at all.
    for note in radius.unknown.into_iter().chain(sites.unknown) {
        env = env.unknown(note);
    }
    for note in radius.suppressed.into_iter().chain(sites.suppressed) {
        env = env.suppressed(note);
    }
    Ok((env, rows > 0))
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
