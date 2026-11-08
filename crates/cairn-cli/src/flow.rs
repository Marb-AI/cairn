//! `cairn flow`: the control flow inside one function.
//!
//! The core holds the shape and the cache (`cairn_store::flow`); this is the part that
//! knows where a provider lives and how to ask it. One provider per language, run in the
//! image that already holds that language's toolchain — the same arrangement the indexers
//! have, and for the same reason: nobody should have to install Go to read a Go handler.
//!
//! Computed on demand rather than at index time. A flow is only ever wanted for the
//! handful of functions somebody is reviewing, and computing one per function at index
//! time would be thousands of parses for an answer nobody asked for.

use anyhow::{Context, Result};
use cairn_store::{Lang, ProviderFlow, Store, SymbolRow};
use std::path::Path;

use crate::docker;

/// What a provider could be asked, and what it would cost to ask.
pub enum Provider {
    /// `cairnflow`, in the indexer image.
    Go,
    /// No provider for this language. The answer says so rather than being empty.
    None(&'static str),
}

pub fn provider_for(lang: Lang) -> Provider {
    match lang {
        Lang::Go => Provider::Go,
        // Out of scope rather than missing, and the difference is worth stating: the
        // shape is language-neutral and a Python provider is a day's work with `ast`,
        // but nothing here pretends to have one.
        Lang::Python => Provider::None("flow is not available for python"),
        Lang::TypeScript => Provider::None("flow is not available for typescript"),
        Lang::Unknown => Provider::None("flow needs a language this index could not determine"),
    }
}

/// The flow of one symbol: from the cache when the file has not changed, from the
/// provider when it has.
///
/// Returns `None` with a reason when there is no provider for the language, which the
/// caller reports as `unknown` on an otherwise empty answer.
pub fn flow_of(
    store: &Store,
    sym: &SymbolRow,
    repo: Option<&Path>,
) -> Result<std::result::Result<cairn_store::Flow, String>> {
    let Some(def) = sym.def.as_ref() else {
        return Ok(Err(
            "no definition indexed for this symbol, so there is no body to describe".to_string(),
        ));
    };
    if let Provider::None(why) = provider_for(sym.lang) {
        return Ok(Err(why.to_string()));
    }
    let Some(root) = repo else {
        return Ok(Err(
            "flow reads the function's source, which needs the repository: pass --repo <dir>"
                .to_string(),
        ));
    };
    let full = root.join(&def.path);
    let bytes = std::fs::read(&full).with_context(|| format!("reading {}", full.display()))?;
    // The content of the file is the cache key. A flow is a syntax fact about one body,
    // so nothing outside this file can change it - which is why there is no dependency
    // component here (architecture 5.2 exempts exactly this case).
    let blob = blake3::hash(&bytes).as_bytes()[..16].to_vec();

    if let Some(cached) = store.cached_flow(sym.id, &blob)? {
        return Ok(Ok(store.resolve_flow(sym, &def.path, &cached, true)?));
    }

    let line = (def.line + 1).to_string();
    // The path as the container sees it. The provider is given the file and a line inside
    // the function rather than a symbol: it has no index and no notion of one.
    let inside = Path::new(docker::MOUNT).join(&def.path);
    let out = docker::exec_out(
        root,
        root,
        &[docker::FLOW_PROVIDER, &inside.to_string_lossy(), &line],
    )
    .with_context(|| format!("asking {} for the flow", docker::FLOW_PROVIDER))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Ok(Err(format!(
            "the flow provider could not describe this function: {}",
            stderr
                .lines()
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .unwrap_or("it exited without saying why")
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let parsed: ProviderFlow = serde_json::from_str(text.trim()).with_context(|| {
        format!(
            "reading what {} printed: {}",
            docker::FLOW_PROVIDER,
            text.chars().take(200).collect::<String>()
        )
    })?;
    // Cached before resolution: what is worth keeping is the provider's answer, which is
    // what cost a process. Resolution is a few queries and is redone each time, so a
    // reindex that renames a symbol cannot leave a stale handle in the cache.
    let _ = store.cache_flow(sym.id, &blob, &parsed);
    Ok(Ok(store.resolve_flow(sym, &def.path, &parsed, false)?))
}
