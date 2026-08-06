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
            // `location()`, not `path:line`. The second time this exact mistake shipped:
            // SCIP counts lines from 0, every other row on the page counts from 1, and a
            // renderer that formats the pair by hand skips the conversion. Two measured
            // agent runs caught it here by opening the file and reporting the discrepancy
            // in their answers — the arm doing the tool's checking for it.
            if budget.push(&mut body, &format!("  {}", r.location())) {
                rows += 1;
            }
        }
        let _ = writeln!(body, "  (from `cairn refs {}`)\n", node.symbol.handle);
    }

    // 2b. The lines the graph did not resolve, from the tree.
    //
    //     The index is precise and incomplete; a text search is complete and imprecise.
    //     Cairn holds both and used to hand the caller only the first, with a sentence
    //     recommending they run the second themselves. There is no reason for that: the
    //     residue is *what the tree has and the graph does not*, which is exactly the
    //     "five of six, and you lose one" the reference list cannot show.
    //
    //     Measured over 30 symbols with an unambiguous name: 99 resolved rows, 42 extra
    //     code lines, median one per symbol. Small, bounded, and precisely the part that
    //     breaks a rename — keyword arguments, proto fields, re-exports.
    //
    //     Only for a name carried by ONE symbol. With a homonym a lexical hit may belong
    //     to the other one and nothing here can say which, so the count is stated and the
    //     lines are not, rather than attributing them by guess.
    let namesakes = store.symbols_named(&sym.name)?.len();
    if let Some(root) = repo {
        // Subtract every known reference, generated included, or the residue fills up
        // with rows the answer above deliberately suppressed.
        let (all_refs, _, _) = store.references(symbol_id, true, 900)?;
        let known: std::collections::HashSet<(String, i64)> = all_refs
            .iter()
            .map(|r| (r.path.clone(), r.line + 1))
            .collect();
        let code = |p: &str| {
            p.rsplit('.')
                .next()
                .is_some_and(|e| matches!(e, "py" | "pyi" | "go" | "proto"))
        };
        if namesakes <= 1 {
            let mut found = treefind::search(root, &sym.name, 200);
            // Case-sensitive, unlike the tree search itself. `for find` is deliberately
            // case-insensitive - someone locating a header does not want to know how it
            // is capitalised - but this block claims "no other symbol has this name", and
            // `mortgage_term_months` and `MORTGAGE_TERM_MONTHS` are two symbols. Matching
            // loosely here would attribute a constant's lines to a field and say it was
            // certain.
            found.hits.retain(|h| {
                code(&h.path)
                    && h.text.contains(sym.name.as_str())
                    && !known.contains(&(h.path.clone(), h.line as i64))
            });
            if !found.hits.is_empty() {
                let _ = writeln!(
                    body,
                    "{} line(s) name `{}` that the graph did not resolve. No other symbol \
                     has this name, so each one is about this symbol - a keyword argument, \
                     a proto field, a re-export, a docstring. A rename breaks them; the \
                     list above does not contain them:",
                    found.hits.len(),
                    sym.name
                );
                for h in found.hits.iter().take(15) {
                    if budget.push(
                        &mut body,
                        &format!("  {}:{}  {}", h.path, h.line, h.text.trim()),
                    ) {
                        rows += 1;
                    }
                }
                if found.hits.len() > 15 {
                    unknown.push(format!(
                        "{} more unresolved lines than the 15 shown; `cairn for find \"{}\"` \
                         gives all of them with attribution",
                        found.hits.len() - 15,
                        sym.name
                    ));
                }
                let _ = writeln!(
                    body,
                    "  (from `cairn for find \"{}\"`, minus everything `refs` already \
                     resolved)\n",
                    sym.name
                );
            }
        } else {
            // Said rather than shown. Attributing a lexical hit to one of several
            // same-named symbols is the guess this tool exists not to make.
            unknown.push(format!(
                "{namesakes} symbols share the name `{}`, so the working tree cannot be \
                 searched for this one specifically - a lexical hit may belong to any of \
                 them. The list above is what the graph resolved; `cairn for find \"{}\"` \
                 shows every mention with the enclosing function of each",
                sym.name, sym.name
            ));
        }
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
    if !cairn_store::weak::is_built(store) {
        // The layer has never been derived for this index, so an empty result says
        // nothing at all. Printing the clean-bill sentence here was the single most
        // dangerous thing this command did: it is the sentence an agent reads before
        // deciding a rename is safe, it was printed for every symbol in the repository,
        // and it rested on a table nobody had filled in.
        let _ = writeln!(
            body,
            "the weak-link layer has NOT been built for this index, so whether a string \
             literal names this symbol is UNCHECKED - not clean. Run `cairn weak --repo \
             <dir>` and ask again before trusting a rename"
        );
        unknown.push(
            "dynamic references were not checked: the weak-link layer is missing, which is \
             not the same as finding nothing"
                .to_string(),
        );
    } else if weak.is_empty() {
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

/// `for understand`: what this calls, and where the chain lands.
///
/// The mirror of `change`. That one answers inwards — who breaks — and this one answers
/// outwards, which is the question a reader following a request through a system actually
/// has. Splitting them that way is not a taxonomy: it is the split the two measured
/// scenarios fall on, and neither block set is useful for the other's question.
///
/// The gap it closes is specific and was mis-diagnosed once. For the endpoint
/// `get_shared_object`, the first hop is *not* unresolved:
///
/// * `graph --aspect calls` shows `AppClients.share` and `ApplicationEnvironment.clients`
///   — the attribute plumbing — and not the RPC call, because its outward filter drops
///   generated code (deliberately, and for a good reason: protobuf message types crowded
///   out the real callees).
/// * `reaches --outgoing` has the hop exactly.
///
/// So the tool held the answer and the agent had to already know which of two commands
/// hides it. Both blocks are here, in that order, with the chain followed to its end
/// rather than one hop per round trip.
pub fn understand(store: &Store, symbol_id: i64, budget: &mut Budget) -> Result<(Envelope, bool)> {
    use std::fmt::Write;
    let sym = store
        .symbol(symbol_id)?
        .ok_or_else(|| anyhow::anyhow!("handle resolved to a missing symbol"))?;

    let mut body = String::new();
    let mut rows = 0usize;
    let mut unknown: Vec<String> = Vec::new();

    // 1. The chain. Depth 4 and 40 hops: the deepest chain in the corpus this was built
    //    against is 2 and terminates, so the caps exist to bound a codebase that nests
    //    deeper rather than to trim this one. Both are printed when they bite.
    let chain = store.rpc_chain(symbol_id, 4, 40)?;
    if chain.hops.is_empty() {
        let _ = writeln!(
            body,
            "[{}] {} calls nothing across a service boundary  (from `cairn reaches {} \
             --outgoing`)",
            sym.handle,
            sym.qualified(),
            sym.handle
        );
    } else {
        let depth = chain.hops.iter().map(|h| h.depth).max().unwrap_or(0);
        // Not "followed to the end" any more. Round six: three arms found a fourth hop the
        // walk missed, because the Go proxy delegates to a transformer that makes the call.
        // The walk now follows two local levels at each hop, which recovers that shape —
        // but "to the end" is a claim about a codebase, not about a bound, and the walk
        // still has bounds. Saying what it followed is the honest form of the same line.
        let _ = writeln!(
            body,
            "[{}] {} — where this lands, {} hop(s) across services, following each hop's \
             own calls two levels deep",
            sym.handle,
            sym.qualified(),
            depth
        );
        for hop in &chain.hops {
            let where_ = hop
                .to
                .symbol
                .def
                .as_ref()
                .map(|d| d.location())
                .unwrap_or_else(|| "?".to_string());
            let note = if hop.already_reached {
                "  (reached above; not followed twice)"
            } else {
                ""
            };
            // Name the function that made the call, not the one that owns the hop. A
            // reader sent to the handler for a call the handler delegates is sent to the
            // wrong file.
            let caller = match &hop.via {
                // The via symbol carries its handle, because this row is not reproducible
                // from the command the block cites — `reaches <root> --outgoing` does not
                // walk local callees, so only `reaches <via> --outgoing` produces it. The
                // stress harness caught the block citing a command that would not return
                // two of its own rows, which is rule one of this whole entry point.
                Some(v) => format!(
                    "{} via [{}] {}",
                    if hop.depth == 1 {
                        sym.qualified()
                    } else {
                        hop.from.qualified()
                    },
                    v.handle,
                    v.qualified()
                ),
                None if hop.depth == 1 => sym.qualified(),
                None => hop.from.qualified(),
            };
            let line = format!(
                "{}{} -> [{}] {}  {}  {}  [{}.{}.{}]{}",
                "  ".repeat(hop.depth),
                caller,
                hop.to.symbol.handle,
                hop.to.symbol.qualified(),
                hop.to.symbol.lang.tag(),
                where_,
                hop.to.pkg,
                hop.to.service,
                hop.to.rpc,
                note
            );
            if budget.push(&mut body, &line) {
                rows += 1;
            }
        }
        let via_rows = chain.hops.iter().any(|h| h.via.is_some());
        let _ = writeln!(
            body,
            "  (from `cairn reaches {} --outgoing`, once per hop{})\n",
            sym.handle,
            if via_rows {
                "; a row marked `via [h]` comes from `cairn reaches h --outgoing`, since \
                 the hop is made by something this code calls rather than by this code"
            } else {
                ""
            }
        );
        if chain.hops.iter().any(|h| !h.exact) {
            unknown.push(format!(
                "the first hop is from a client binding, not from a call that was seen: \
                 [{}] holds a generated client for that service. `cairn refs {}` is where \
                 the call sites are, if there are any",
                sym.handle, sym.handle
            ));
        }
        if !chain.not_followed.is_empty() {
            unknown.push(format!(
                "the walk stopped at 4 hops with {} symbol(s) still to ask - {}. Whether \
                 the chain continues past them was not checked; `cairn reaches <handle> \
                 --outgoing` on each is the next step",
                chain.not_followed.len(),
                chain
                    .not_followed
                    .iter()
                    .map(|s| format!("[{}]", s.handle))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        unknown.push(
            "the handler serving each RPC is matched by the generator's naming convention \
             (`GetFolder` <-> `get_folder`), so a hop is exact where the convention holds \
             and blind to a service reached by a hand-written transport or a queue"
                .to_string(),
        );
        // The gap round six found, stated rather than left for a reader to trip over.
        // Measured on the target repository: `shareService.GetSharedObject` calls
        // `ft.RpcToRest(...)` on a local variable, scip-go resolves no call edge for it,
        // and the hop that function makes is therefore invisible to any walk over this
        // index - including this one. Three agent runs found it by reading the file. A
        // bound that is printed can be widened; an unresolved edge cannot, so the only
        // honest thing is to say the list is a floor.
        unknown.push(
            "this list is a floor, not a ceiling. A hop made by a method called on a local \
             variable resolves to no call edge, so a handler that delegates to a helper it \
             constructs - `t := NewThing(); t.Method()` - hides whatever that helper \
             reaches. Read the hop's own file where the chain matters"
                .to_string(),
        );
    }

    // 2. The local calls: the same rows `graph --aspect calls` gives at depth 1. Kept in
    //    the same answer because the chain says which process the work moves to and this
    //    says what it does before it goes — and reading one without the other is two
    //    round trips for one question.
    let calls = store.walk(
        symbol_id,
        cairn_store::EdgeKind::Calls,
        cairn_store::Direction::Out,
        1,
        20,
        false,
    )?;
    let local: Vec<_> = calls
        .nodes
        .iter()
        .filter(|n| n.symbol.id != symbol_id)
        .collect();
    if local.is_empty() {
        let _ = writeln!(
            body,
            "calls nothing this index can follow in its own language  (from `cairn graph \
             {} --aspect calls`)",
            sym.handle
        );
    } else {
        let _ = writeln!(body, "calls, in its own language:");
        for node in &local {
            let at = node
                .symbol
                .def
                .as_ref()
                .map(|d| d.location())
                .unwrap_or_else(|| "?".to_string());
            if budget.push(
                &mut body,
                &format!(
                    "  [{}] {}  {}",
                    node.symbol.handle,
                    node.symbol.qualified(),
                    at
                ),
            ) {
                rows += 1;
            }
        }
        let _ = writeln!(
            body,
            "  (from `cairn graph {} --aspect calls`)\n",
            sym.handle
        );
    }
    if calls.truncated > 0 {
        unknown.push(format!(
            "{} more callee(s) than the 20 listed; `cairn graph {} --aspect calls` takes a \
             wider fanout",
            calls.truncated, sym.handle
        ));
    }

    // 3. Which processes this runs in. One line, and it is the frame the two blocks above
    //    are read against: "where does this land" means little without "land from where".
    let (services, via) = store.services_running_attributed(symbol_id, 12)?;
    if services.is_empty() {
        let _ = writeln!(
            body,
            "no deployed service is known to run this  (from `cairn runs {}`)",
            sym.handle
        );
    } else {
        let _ = writeln!(
            body,
            "runs in: {}  (from `cairn runs {}`)",
            services.join(", "),
            sym.handle
        );
        // How the service was attributed is part of the claim, not a footnote. A route
        // handler reached only because its module is imported is a weaker statement than
        // one on a call path, and the two look identical on the line above.
        match &via {
            cairn_store::Attribution::ViaFile => unknown.push(
                "the service was attributed through the file this code sits in rather \
                 than through a call path, so it says the module is loaded there, not \
                 that this is reached from the entrypoint"
                    .to_string(),
            ),
            cairn_store::Attribution::ViaType(t) => unknown.push(format!(
                "nothing calls this statically; the service was attributed through its \
                 enclosing type [{}] {}, which is the shape of a method reached from a \
                 dispatch table",
                t.handle,
                t.qualified()
            )),
            cairn_store::Attribution::Direct => {}
        }
    }

    let mut env = Envelope::new(body).rows(rows);
    for note in unknown {
        env = env.unknown(note);
    }
    if chain.cut_by_breadth > 0 {
        env = env.suppressed(format!(
            "{} hop(s) beyond the 40 printed",
            chain.cut_by_breadth
        ));
    }
    if !chain.unchecked.is_empty() {
        env = env.suppressed(format!(
            "no generated client in the index names the RPCs of {}, so rows for it are \
             unfiltered and may include private helpers",
            chain.unchecked.join(", ")
        ));
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
