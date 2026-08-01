//! Settings for the installation, not for a repository.
//!
//! These belong to whoever installed cairn: whether it records sessions, how big an answer
//! may get. They live beside the binary rather than in a checkout, because one binary
//! serves every repository on the machine and the answer to "is tracking on" should not
//! depend on which directory you happen to be standing in.
//!
//! The distinction matters the other way too. The *index* is per repository and is found
//! from the working directory — run cairn inside `repo1` and you get `repo1`'s index; run
//! it somewhere with no index above it and it says so. Conventions (`rules.yaml`) are per
//! repository as well, because they describe that codebase. Only this file is global.
//!
//! Absent is the normal state and means the defaults, so a fresh install needs nothing.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// Append a line per command to `<index dir>/sessions/<id>.jsonl`.
    ///
    /// Off unless switched on, including for internal builds: a tool that starts recording
    /// what you searched for without being asked is one people stop trusting.
    pub tracking: bool,
    /// Report peak resident memory on stderr when a command finishes.
    pub memory_peak: bool,
    /// Ceiling on resident memory, in megabytes. Defaults to a quarter of the machine's
    /// RAM.
    ///
    /// Indexing is the only path that can grow without bound — a repository ten times the
    /// size of the one this was built against would try. A quarter leaves the machine
    /// usable, and exceeding it aborts the build rather than letting the OS decide which
    /// process dies. Aborting is safe: a build assembles a staging file and the live index
    /// is untouched until it is promoted.
    pub memory_limit_mb: Option<u64>,
    /// Default ceiling on an answer, in tokens, when `--budget` is not given.
    pub default_budget: Option<usize>,
    /// Refuse to answer at all beyond this, whatever `--budget` says. A guard for a
    /// shared install, where one caller's generous budget is everyone's memory.
    pub max_budget: Option<usize>,
}

/// Where the installation's settings live: beside the binary, unless told otherwise.
pub fn config_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("CAIRN_CONFIG") {
        return Some(PathBuf::from(explicit));
    }
    let exe = std::env::current_exe().ok()?;
    // Resolve symlinks: a binary on the PATH is usually a link into a versioned
    // directory, and the settings belong with the real thing.
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    Some(exe.parent()?.join("cairn.yaml"))
}

impl Config {
    /// The ceiling in bytes: what was configured, or a quarter of the machine's RAM.
    pub fn memory_limit_bytes(&self) -> Option<u64> {
        if let Some(mb) = self.memory_limit_mb {
            return Some(mb * 1024 * 1024);
        }
        total_ram_bytes().map(|total| total / 4)
    }

    pub fn load() -> Result<Config> {
        let Some(path) = config_path() else { return Ok(Config::default()) };
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }
}

/// Physical memory, where the platform will say. `None` means no default ceiling, which
/// is the honest answer rather than a guessed number.
fn total_ram_bytes() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb: u64 = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(kb * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_file_means_defaults_rather_than_an_error() {
        // The normal state of a fresh install.
        std::env::set_var("CAIRN_CONFIG", "/nonexistent/cairn.yaml");
        let c = Config::load().expect("a missing config is not an error");
        assert!(!c.tracking, "tracking must be off unless someone asked for it");
        assert!(!c.memory_peak);
        assert!(c.default_budget.is_none());
        // No explicit limit still yields one, from the machine.
        if let Some(limit) = c.memory_limit_bytes() {
            assert!(limit > 64 * 1024 * 1024, "a quarter of RAM should not be tiny");
        }
        std::env::remove_var("CAIRN_CONFIG");
    }

    #[test]
    fn a_partial_file_leaves_the_rest_at_its_default() {
        let dir = std::env::temp_dir().join("cairn-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cairn.yaml");
        std::fs::write(&path, "tracking: true\n").unwrap();
        std::env::set_var("CAIRN_CONFIG", &path);
        let c = Config::load().expect("parsing");
        assert!(c.tracking);
        assert!(!c.memory_peak, "an unmentioned setting keeps its default");
        std::env::remove_var("CAIRN_CONFIG");
    }
}
