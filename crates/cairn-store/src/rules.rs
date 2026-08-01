//! Conventions, as data rather than as Rust.
//!
//! Architecture D16 says the core knows no language: what a start command looks like, how a
//! protobuf generator names things, where tests live, what marks a file as generated — all
//! of that is convention, and conventions belong in a pack that can be edited without
//! touching the crate.
//!
//! For a while they were not. They were `if` chains in `deploy.rs`, `protolink.rs`,
//! `ingest.rs` and `conventions.rs`, which made cairn a Python/Go/Docker tool wearing a
//! general one's documentation. A generality audit (docs/generality-audit.md) recorded that
//! as debt; this is it being paid.
//!
//! The defaults are embedded, so a repository that follows the usual conventions needs no
//! file at all and behaves exactly as before. A repository that does not can drop a
//! `rules.yaml` next to its index and change any of it — and, more to the point, a new
//! language is a pack rather than a patch.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

/// The built-in pack. Everything cairn assumed implicitly, written down.
const DEFAULT: &str = include_str!("rules/default.yaml");

#[derive(Debug, Clone, Deserialize)]
pub struct Rules {
    pub generated: GeneratedRules,
    pub tests: TestRules,
    /// Ordered: the first rule that matches a command wins, so `Register`-style prefixes
    /// and more specific shapes must come before the general ones.
    pub commands: Vec<CommandRule>,
    pub proto: ProtoRules,
    pub on_demand: OnDemandRules,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeneratedRules {
    /// Matched case-insensitively, and only within the leading comment block — a marker
    /// belongs above the code, and matching anywhere in the head classified a hand-written
    /// module as generated because line 40 mentioned "auto-generated" in prose.
    pub markers: Vec<String>,
    /// Last resort, when the file cannot be read.
    pub path_suffixes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TestRules {
    pub path_contains: Vec<String>,
    pub path_prefixes: Vec<String>,
    pub file_prefixes: Vec<String>,
    pub file_suffixes: Vec<String>,
}

/// How to recognise a start command, and what it points at.
#[derive(Debug, Clone, Deserialize)]
pub struct CommandRule {
    /// For diagnostics and for a pack author to see what they are overriding.
    pub name: String,
    #[serde(default)]
    pub argv0_starts_with: Vec<String>,
    #[serde(default)]
    pub argv0_ends_with: Vec<String>,
    /// Whole command starts with one of these — for `tail -f`, `sleep infinity`.
    #[serde(default)]
    pub command_starts_with: Vec<String>,
    /// A word anywhere in the command ends with this — `manage.py`.
    #[serde(default)]
    pub word_ends_with: Option<String>,
    pub target: TargetRule,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TargetRule {
    /// The word after `flag` is a module path: `python -m pkg.mod`.
    ModuleAfterFlag { flag: String },
    /// The part of `spec` before `:` is a module: `uvicorn pkg.mod:app`.
    ModuleBeforeColon,
    /// The word after the matched word names a management command.
    SubcommandAfterMatch,
    /// The word after `flag` names an application: `celery -A proj`.
    AppAfterFlag { flag: String },
    /// A path to a built binary.
    Binary,
    /// Runs nothing: a container held open.
    Idle,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProtoRules {
    /// Checked in order. `Register<Svc>Server` must precede the bare `Server` suffix.
    pub bindings: Vec<BindingRule>,
    /// Stripped from a stem before canonicalising, longest-first by convention.
    pub strip_prefixes: Vec<String>,
    /// A stem must contain this to be a service artefact at all. Without it, every name
    /// ending in `Client` becomes a service and the graph fills with invented edges.
    pub service_marker: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BindingRule {
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub suffix: Option<String>,
    /// `serves` or `calls`.
    pub role: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OnDemandRules {
    /// How a scheduled line names the container it runs in.
    pub service_marker: String,
    /// Extensions a runner script can have.
    pub runner_suffixes: Vec<String>,
    /// Directories never worth walking for deployment scripts.
    pub skip_dirs: Vec<String>,
}

impl Default for Rules {
    fn default() -> Self {
        serde_yaml::from_str(DEFAULT).expect("the embedded rule pack must parse")
    }
}

impl Rules {
    /// Load a pack from disk, falling back to the built-in one when there is no file.
    ///
    /// Deliberately all-or-nothing rather than a merge: a half-overridden convention set is
    /// harder to reason about than a whole one, and a pack author who wants the defaults
    /// can start from the embedded file, which is printed by `cairn rules --print`.
    pub fn load(path: Option<&Path>) -> Result<Rules> {
        let Some(path) = path else {
            return Ok(Rules::default());
        };
        if !path.exists() {
            return Ok(Rules::default());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading rule pack {}", path.display()))?;
        serde_yaml::from_str(&text).with_context(|| format!("parsing rule pack {}", path.display()))
    }

    /// The embedded default, verbatim, so a pack can be started from it.
    pub fn builtin_text() -> &'static str {
        DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_pack_parses_and_covers_the_shapes_that_were_hardcoded() {
        let r = Rules::default();
        assert!(r
            .commands
            .iter()
            .any(|c| matches!(c.target, TargetRule::ModuleAfterFlag { .. })));
        assert!(r
            .commands
            .iter()
            .any(|c| matches!(c.target, TargetRule::Idle)));
        assert!(r.proto.bindings.iter().any(|b| b.role == "serves"));
        assert!(r.proto.bindings.iter().any(|b| b.role == "calls"));
        assert!(!r.generated.markers.is_empty());
        assert!(!r.tests.path_contains.is_empty());
    }

    #[test]
    fn a_missing_pack_is_the_default_not_an_error() {
        let r = Rules::load(Some(Path::new("/nonexistent/rules.yaml"))).unwrap();
        assert_eq!(
            r.proto.service_marker,
            Rules::default().proto.service_marker
        );
    }

    #[test]
    fn register_style_bindings_come_before_the_bare_suffix() {
        // `RegisterAuthServiceServer` must not be read as the `Server` suffix rule, or the
        // stem keeps the prefix and canonicalisation invents a service.
        let r = Rules::default();
        let reg = r
            .proto
            .bindings
            .iter()
            .position(|b| b.prefix.as_deref() == Some("Register"))
            .expect("a Register rule");
        let bare = r
            .proto
            .bindings
            .iter()
            .position(|b| b.prefix.is_none() && b.suffix.as_deref() == Some("Server"))
            .expect("a bare Server rule");
        assert!(reg < bare, "Register must be checked first");
    }
}
