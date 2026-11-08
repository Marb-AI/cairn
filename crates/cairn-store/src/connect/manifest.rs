//! Reading a connector's manifest, and matching the inputs it declares.
//!
//! A manifest is the whole of what the core knows about an external connector: what to
//! run, what it reads, which kinds of fact it may contribute, and how each kind
//! propagates. Everything else — what a `domain` is, why an `rpc` `emits` an `event` —
//! stays inside the connector's own tool.
//!
//! Strict on purpose. An unknown field, an unreadable tier, an edge kind that propagates
//! in a way the core does not have a traversal for: all refused at load, with the reason
//! named. A manifest that half-loads would mean a connector whose facts are accepted
//! under rules nobody stated.

use super::{Manifest, Propagates, Tier};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where manifests live, relative to the index.
pub const DIR: &str = "connectors";

/// The YAML shape, as written. Converted into a `Manifest` once validated.
///
/// `deny_unknown_fields` is the point: a typo in `timeout_s` would otherwise be a
/// silently different timeout, and a key from a newer protocol would be ignored by an
/// older cairn rather than reported.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    connector: String,
    protocol: u32,
    command: Vec<String>,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(default = "default_timeout")]
    timeout_s: u64,
    tier: String,
    #[serde(default)]
    node_kinds: Vec<String>,
    #[serde(default)]
    edge_kinds: BTreeMap<String, EdgeKindFile>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EdgeKindFile {
    propagates: String,
}

fn default_timeout() -> u64 {
    120
}

/// The protocol version this cairn speaks.
pub const PROTOCOL: u32 = 1;

impl Manifest {
    /// Load one manifest from disk.
    pub fn load(path: &Path) -> Result<Manifest> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Manifest::parse(&text, path)
    }

    /// The same, from the text, so the validation has a test that needs no files.
    pub fn parse(text: &str, path: &Path) -> Result<Manifest> {
        let f: ManifestFile = serde_yaml::from_str(text)
            .with_context(|| format!("reading the manifest {}", path.display()))?;
        if f.protocol != PROTOCOL {
            bail!(
                "{}: protocol {} is not the {PROTOCOL} this cairn speaks",
                path.display(),
                f.protocol
            );
        }
        if f.connector.trim().is_empty() {
            bail!("{}: the connector has no name", path.display());
        }
        // The name is a namespace: it prefixes every node id the connector contributes,
        // and it names the cache file. A name with a path separator or a colon in it
        // would make both ambiguous.
        if f.connector
            .contains(|c: char| c == ':' || c == '/' || c == '\\' || c.is_whitespace())
        {
            bail!(
                "{}: connector name {:?} may not contain a colon, a path separator or \
                 whitespace - it is the namespace of every id this connector states",
                path.display(),
                f.connector
            );
        }
        if f.command.is_empty() {
            bail!("{}: no command to run", path.display());
        }
        let tier = Tier::parse(&f.tier)
            .with_context(|| format!("{}: unknown tier {:?}", path.display(), f.tier))?;
        let mut edge_kinds = BTreeMap::new();
        for (name, spec) in f.edge_kinds {
            let p = Propagates::parse(&spec.propagates).with_context(|| {
                format!(
                    "{}: edge kind {name:?} propagates {:?}, which is not \
                     dependency, containment or none",
                    path.display(),
                    spec.propagates
                )
            })?;
            edge_kinds.insert(name, p);
        }
        Ok(Manifest {
            // The manifest as written is part of the cache key: changing a command, a
            // timeout or an input glob can change the facts even when no input did.
            digest: blake3::hash(text.as_bytes()).as_bytes()[..16].to_vec(),
            name: f.connector,
            version: None,
            inputs: f.inputs,
            tier,
            node_kinds: f.node_kinds,
            edge_kinds,
            command: f.command,
            timeout_s: f.timeout_s,
        })
    }

    /// Every manifest under `<db dir>/connectors/`, in name order.
    ///
    /// A manifest that does not load is *not* skipped: the error is returned with the
    /// others so the caller can report it. Silently ignoring a broken manifest is how a
    /// connector stops contributing without anybody being told.
    pub fn load_dir(dir: &Path) -> (Vec<Manifest>, Vec<String>) {
        let mut found: Vec<(PathBuf, Result<Manifest>)> = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return (Vec::new(), Vec::new());
        };
        for e in entries.flatten() {
            let path = e.path();
            let is_yaml = path
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x == "yaml" || x == "yml")
                .unwrap_or(false);
            if !is_yaml {
                continue;
            }
            found.push((path.clone(), Manifest::load(&path)));
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        let mut manifests = Vec::new();
        let mut problems = Vec::new();
        for (path, result) in found {
            match result {
                Ok(m) => manifests.push(m),
                Err(e) => problems.push(format!(
                    "the connector manifest {} was not loaded ({e:#}), so whatever it \
                     contributes is missing from every answer here",
                    path.display()
                )),
            }
        }
        manifests.sort_by(|a, b| a.name.cmp(&b.name));
        (manifests, problems)
    }
}

/// Does this repo-relative path match one of the globs?
///
/// `*` matches within a path segment, `**` crosses segments, `?` is one character.
/// Enough for `**/*.proto` and `compose*.y*ml`, which is what a manifest's `inputs` are
/// for, and small enough to read — a glob crate for two patterns would be a dependency
/// nobody could justify at review.
pub fn matches_any(path: &str, globs: &[String]) -> bool {
    globs.iter().any(|g| matches(path, g))
}

pub fn matches(path: &str, glob: &str) -> bool {
    glob_match(path.as_bytes(), glob.as_bytes())
}

/// Backtracking matcher over bytes. Paths are `/`-separated and repo-relative.
fn glob_match(path: &[u8], glob: &[u8]) -> bool {
    // `**/` at the start of a pattern also matches nothing, so `**/*.proto` matches a
    // file at the root. Without this, `inputs: ["**/*.proto"]` would quietly miss
    // `orders.proto` beside the manifest.
    if let Some(rest) = glob.strip_prefix(b"**/") {
        if glob_match(path, rest) {
            return true;
        }
    }
    match (path.first(), glob.first()) {
        (None, None) => true,
        (_, Some(b'*')) if glob.get(1) == Some(&b'*') => {
            // `**` consumes any number of characters, separators included.
            let rest = &glob[2..];
            let rest = rest.strip_prefix(b"/").unwrap_or(rest);
            (0..=path.len()).any(|i| glob_match(&path[i..], rest))
        }
        (_, Some(b'*')) => {
            // `*` stops at a separator.
            let rest = &glob[1..];
            let limit = path.iter().position(|c| *c == b'/').unwrap_or(path.len());
            (0..=limit).any(|i| glob_match(&path[i..], rest))
        }
        (Some(_), Some(b'?')) => glob_match(&path[1..], &glob[1..]),
        (Some(p), Some(g)) if p == g => glob_match(&path[1..], &glob[1..]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
connector: w17
protocol: 1
command: ["w17ctl", "cairn", "facts", "--jsonl"]
inputs: ["**/*.proto"]
timeout_s: 30
tier: declared
node_kinds: [domain, rpc]
edge_kinds:
  implemented_by: { propagates: dependency }
  part_of:        { propagates: containment }
"#;

    #[test]
    fn a_manifest_round_trips_into_what_the_core_reads() {
        let m = Manifest::parse(GOOD, Path::new("w17.yaml")).unwrap();
        assert_eq!(m.name, "w17");
        assert_eq!(m.timeout_s, 30);
        assert_eq!(m.command[0], "w17ctl");
        assert_eq!(m.node_kinds, vec!["domain", "rpc"]);
        assert_eq!(m.edge_kinds["implemented_by"], Propagates::Dependency);
        assert_eq!(m.edge_kinds["part_of"], Propagates::Containment);
    }

    #[test]
    fn every_way_a_manifest_can_be_wrong_is_refused_with_a_reason() {
        // Each of these used to be a plausible silent default. A manifest that loads
        // halfway is a connector running under rules nobody stated.
        let cases = [
            ("protocol: 1", "protocol: 2", "protocol"),
            ("tier: declared", "tier: guessed", "tier"),
            ("connector: w17", "connector: \"w 17\"", "whitespace"),
            (
                "propagates: dependency",
                "propagates: sideways",
                "dependency",
            ),
            (
                r#"command: ["w17ctl", "cairn", "facts", "--jsonl"]"#,
                "command: []",
                "no command",
            ),
        ];
        for (from, to, expected) in cases {
            let text = GOOD.replace(from, to);
            let err = Manifest::parse(&text, Path::new("w17.yaml"))
                .expect_err(&format!("{to} should not load"))
                .to_string()
                + &format!(
                    "{:#}",
                    Manifest::parse(&text, Path::new("w17.yaml")).unwrap_err()
                );
            assert!(
                err.contains(expected),
                "loading {to} failed for the wrong reason: {err}"
            );
        }
    }

    #[test]
    fn an_unknown_field_is_an_error_rather_than_a_different_setting() {
        let text = GOOD.replace("timeout_s: 30", "timeout_seconds: 30");
        assert!(Manifest::parse(&text, Path::new("w17.yaml")).is_err());
    }

    #[test]
    fn the_globs_a_manifest_can_write_match_what_a_reader_expects() {
        let cases: &[(&str, &str, bool)] = &[
            ("proto/orders.proto", "**/*.proto", true),
            ("orders.proto", "**/*.proto", true),
            ("proto/v1/orders.proto", "**/*.proto", true),
            ("proto/orders.go", "**/*.proto", false),
            ("compose.yaml", "compose*.y*ml", true),
            ("compose.override.yml", "compose*.y*ml", true),
            ("srcgo/Dockerfile", "**/Dockerfile", true),
            ("Dockerfile", "**/Dockerfile", true),
            ("srcgo/Dockerfile.dev", "**/Dockerfile", false),
            ("a/b/c.ts", "a/*/c.ts", true),
            ("a/b/d/c.ts", "a/*/c.ts", false),
            ("a/b/d/c.ts", "a/**/c.ts", true),
            ("x.py", "?.py", true),
            ("xy.py", "?.py", false),
        ];
        for (path, glob, expected) in cases {
            assert_eq!(
                matches(path, glob),
                *expected,
                "{path} against {glob} should be {expected}"
            );
        }
    }
}
