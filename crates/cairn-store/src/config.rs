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
    /// Resident memory above which a finished command says so. Defaults to a quarter of
    /// the machine's RAM.
    ///
    /// A **report, not a limit**: nothing is enforced and nothing is aborted. The check
    /// runs once the command has already finished, compares its peak against this number,
    /// and prints a line when it was higher. Indexing is the only path that can grow
    /// without bound, so it is the one this is worth knowing about.
    ///
    /// Said plainly because the doc here used to claim the build was aborted, which was
    /// never true of any version of this code.
    pub memory_limit_mb: Option<u64>,
    /// Default ceiling on an answer, in tokens, when `--budget` is not given.
    pub default_budget: Option<usize>,
    /// Refuse to answer at all beyond this, whatever `--budget` says. A guard for a
    /// shared install, where one caller's generous budget is everyone's memory.
    pub max_budget: Option<usize>,
}

/// Settings that came with the installation, if there are any.
///
/// Beside the binary, which is where a machine-wide default belongs: an administrator can
/// drop one there and every user gets it. Read-only as far as anyone but them is
/// concerned — see `config_path` for why writes do not go here.
fn system_config_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // Resolve symlinks: a binary on the PATH is usually a link into a versioned
    // directory, and the settings belong with the real thing.
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    Some(exe.parent()?.join("cairn.yaml"))
}

/// Where *this user's* settings live, and where `cairn config` writes.
///
/// Not beside the binary. That was the original design, on the reasoning that one binary
/// serves every repository on the machine — true, but it assumed the binary sits somewhere
/// its user can write. Installed system-wide it does not, and `cairn config tracking=on`
/// failed with a permission error on a perfectly ordinary install.
pub fn config_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("CAIRN_CONFIG") {
        return Some(PathBuf::from(explicit));
    }
    if let Some(home) = std::env::var_os("CAIRN_HOME") {
        return Some(PathBuf::from(home).join("cairn.yaml"));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    Some(home.join(".cairn").join("cairn.yaml"))
}

/// Every file that contributes, nearest last: the system default first, then this user's,
/// so a personal setting wins over one the machine came with.
fn config_sources() -> Vec<PathBuf> {
    let mut out = Vec::new();
    // Skipped when the caller named a file: an explicit path means "use this one".
    if std::env::var_os("CAIRN_CONFIG").is_none() {
        if let Some(p) = system_config_path() {
            out.push(p);
        }
    }
    if let Some(p) = config_path() {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// One setting, as the CLI sees it: a name, what it means, and how to render it.
///
/// A table rather than a match in the command handler, so `cairn config` can list what
/// exists without a second list to keep in step with this one.
pub const SETTINGS: &[(&str, &str)] = &[
    (
        "tracking",
        "record one line per command, for reading a session back afterwards",
    ),
    (
        "memory_peak",
        "print peak memory use on stderr when a command finishes",
    ),
    (
        "memory_limit_mb",
        "say so when a command used more than this; default is a quarter of RAM",
    ),
    (
        "default_budget",
        "ceiling on an answer in tokens when --budget is not given",
    ),
    ("max_budget", "refuse to exceed this whatever --budget says"),
];

impl Config {
    /// Render as the YAML that `save` writes, so `config show` and the file agree.
    pub fn get(&self, key: &str) -> Option<String> {
        let unset = "(unset)".to_string();
        Some(match key {
            "tracking" => self.tracking.to_string(),
            "memory_peak" => self.memory_peak.to_string(),
            "memory_limit_mb" => self.memory_limit_mb.map_or(unset, |v| v.to_string()),
            "default_budget" => self.default_budget.map_or(unset, |v| v.to_string()),
            "max_budget" => self.max_budget.map_or(unset, |v| v.to_string()),
            _ => return None,
        })
    }

    /// Apply one `key=value` edit, parsing per the field's type.
    ///
    /// `unset` restores the default rather than requiring someone to know that deleting
    /// the line is how you do it — the file is an implementation detail now.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let as_bool = || match value {
            "true" | "on" | "yes" | "1" => Ok(true),
            "false" | "off" | "no" | "0" => Ok(false),
            other => Err(anyhow::anyhow!("{key} is on or off, not {other:?}")),
        };
        let as_num = || -> Result<Option<u64>> {
            if value == "unset" || value == "default" {
                return Ok(None);
            }
            value.parse::<u64>().map(Some).map_err(|_| {
                anyhow::anyhow!("{key} takes a whole number or `unset`, not {value:?}")
            })
        };
        match key {
            "tracking" => self.tracking = as_bool()?,
            "memory_peak" => self.memory_peak = as_bool()?,
            "memory_limit_mb" => self.memory_limit_mb = as_num()?,
            "default_budget" => self.default_budget = as_num()?.map(|v| v as usize),
            "max_budget" => self.max_budget = as_num()?.map(|v| v as usize),
            other => {
                let known: Vec<&str> = SETTINGS.iter().map(|(k, _)| *k).collect();
                anyhow::bail!(
                    "no setting called {other:?}. There is: {}",
                    known.join(", ")
                );
            }
        }
        Ok(())
    }

    /// Write the settings back, creating the file if it is not there yet.
    ///
    /// Every field is written, including the ones left at their defaults: a file that
    /// shows only what was changed reads as though the rest is unknowable.
    pub fn save(&self) -> Result<PathBuf> {
        let path = config_path().context(
            "cannot work out where settings belong (the binary's own location is unreadable)",
        )?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let mut out = String::from(
            "# cairn settings. Written by `cairn config <key>=<value>`; editing by hand\n\
             # works too, but the command is what the documentation points at.\n",
        );
        for (key, description) in SETTINGS {
            let value = self.get(key).unwrap_or_default();
            out.push_str(&format!("\n# {description}\n"));
            if value == "(unset)" {
                out.push_str(&format!("#{key}:\n"));
            } else {
                out.push_str(&format!("{key}: {value}\n"));
            }
        }
        std::fs::write(&path, out).with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }

    /// The ceiling in bytes: what was configured, or a quarter of the machine's RAM.
    pub fn memory_limit_bytes(&self) -> Option<u64> {
        if let Some(mb) = self.memory_limit_mb {
            return Some(mb * 1024 * 1024);
        }
        total_ram_bytes().map(|total| total / 4)
    }

    /// Read the settings in effect: the machine's, then this user's on top.
    ///
    /// A later file replaces an earlier one wholesale rather than merging field by field.
    /// Merging would mean a user could not turn off something the machine switched on
    /// without knowing which file said it, and `cairn config` writes every field, so the
    /// user's file is complete whenever it exists.
    pub fn load() -> Result<Config> {
        let mut cfg = Config::default();
        for path in config_sources() {
            if !path.exists() {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            cfg = serde_yaml::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
        }
        Ok(cfg)
    }

    /// The file `load` last took its values from, for saying where a setting came from.
    pub fn source() -> Option<PathBuf> {
        config_sources().into_iter().rev().find(|p| p.exists())
    }
}

/// Physical memory, where the platform will say. `None` means no default ceiling, which
/// is the honest answer rather than a guessed number.
///
/// Asked per platform rather than through one portable call, because there isn't one.
/// Getting this wrong is not cosmetic: the answer sets the default figure a finished
/// command is reported against, so a silent `None` on macOS or Windows means nobody is
/// ever told that indexing used more memory than the machine could comfortably spare.
#[cfg(target_os = "linux")]
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

#[cfg(target_os = "macos")]
fn total_ram_bytes() -> Option<u64> {
    let mut bytes: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    // `hw.memsize` is the physical total in bytes; `hw.physmem` is a 32-bit legacy
    // sibling that saturates on any machine we care about.
    let rc = unsafe {
        libc::sysctlbyname(
            b"hw.memsize\0".as_ptr() as *const libc::c_char,
            &mut bytes as *mut u64 as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && bytes > 0).then_some(bytes)
}

#[cfg(windows)]
fn total_ram_bytes() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    // The call rejects a struct that has not been told its own size.
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    (ok != 0 && status.ullTotalPhys > 0).then_some(status.ullTotalPhys)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn total_ram_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_file_means_defaults_rather_than_an_error() {
        // The normal state of a fresh install.
        std::env::set_var("CAIRN_CONFIG", "/nonexistent/cairn.yaml");
        let c = Config::load().expect("a missing config is not an error");
        assert!(
            !c.tracking,
            "tracking must be off unless someone asked for it"
        );
        assert!(!c.memory_peak);
        assert!(c.default_budget.is_none());
        // No explicit limit still yields one, from the machine.
        if let Some(limit) = c.memory_limit_bytes() {
            assert!(
                limit > 64 * 1024 * 1024,
                "a quarter of RAM should not be tiny"
            );
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
