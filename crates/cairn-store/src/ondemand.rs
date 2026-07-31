//! Code a service runs after it has started: cron entries and runner scripts.
//!
//! The compose `command:` is only half of what a deployment runs. The other half arrives
//! later — a crontab line, a management command, a `docker exec` — and it is invisible to
//! every mechanism the rest of this crate uses, because nothing in the source tree calls
//! it. Measured (eval/RESULTS.md, task E): a container whose command is `tail -f
//! /dev/null` was reported as running nothing, while a nightly job inside it reached a
//! repository function under audit. The tool-less baseline found it by reading a shell
//! script, which is the one thing an index cannot do by not looking.
//!
//! What is recovered here, and each step is a parse rather than a guess:
//!
//! ```text
//!   30 3 * * * docker exec "$(docker ps -q -f name=orders-cli …)" /app/foo.sh
//!     -> service   orders-cli          from the container filter
//!     -> runner    /app/foo.sh            resolved to srcpy/foo.sh by basename
//!     -> command   python manage.py foo   the script's own exec line
//!     -> entry file, and from there the call graph, exactly as for a start command
//! ```
//!
//! Shell is not parsed, only read. Simple `VAR=value` assignments are expanded because
//! without them the schedule lines in a real installer resolve to nothing — the service
//! name lives in a variable defined thirty lines earlier. Anything cleverer than that is
//! out of scope and stays unreported rather than guessed at.

use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

/// One way a service runs code that its start command does not.
#[derive(Debug, Clone)]
pub struct OnDemand {
    pub service: String,
    /// The cron expression, when the trigger is a schedule.
    pub schedule: Option<String>,
    /// Repo-relative path of the runner script — the evidence for the whole chain.
    pub script: String,
    /// What that script executes, in the form the command resolver understands.
    pub command: String,
}

/// Directories that never hold deployment scripts and cost a lot to walk.
const SKIP: &[&str] = &[
    ".git", "node_modules", ".venv", "venv", "__pycache__", "target", "dist", "build",
    ".mypy_cache", ".ruff_cache", ".pytest_cache",
];

/// Find cron-triggered runners and the code they lead to.
pub fn scan(repo: &Path) -> Result<Vec<OnDemand>> {
    let mut scripts: Vec<std::path::PathBuf> = Vec::new();
    collect_scripts(repo, repo, 0, &mut scripts);

    // Runner scripts are looked up by basename: a crontab names a path inside the
    // container (`/app/foo.sh`) and the repo holds it somewhere else entirely
    // (`srcpy/foo.sh`). The container path is a deployment detail; the basename survives
    // it.
    let mut by_name: HashMap<String, std::path::PathBuf> = HashMap::new();
    for p in &scripts {
        if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
            by_name.entry(n.to_string()).or_insert_with(|| p.clone());
        }
    }

    let mut out: Vec<OnDemand> = Vec::new();
    for path in &scripts {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let vars = assignments(&text);
        for line in text.lines() {
            let line = expand(line.trim(), &vars);
            let Some((schedule, rest)) = split_cron(&line) else { continue };
            let Some(service) = container_filter(&rest) else { continue };
            let Some(runner) = runner_script(&rest) else { continue };
            let Some(runner_path) = by_name.get(&runner) else { continue };
            let Ok(runner_text) = std::fs::read_to_string(runner_path) else { continue };
            let Some(command) = last_command(&runner_text) else { continue };
            let rel = runner_path
                .strip_prefix(repo)
                .unwrap_or(runner_path)
                .to_string_lossy()
                .to_string();
            out.push(OnDemand {
                service,
                schedule: Some(schedule),
                script: rel,
                command,
            });
        }
    }
    out.sort_by(|a, b| (&a.service, &a.script).cmp(&(&b.service, &b.script)));
    out.dedup_by(|a, b| a.service == b.service && a.script == b.script && a.command == b.command);
    Ok(out)
}

fn collect_scripts(root: &Path, dir: &Path, depth: usize, out: &mut Vec<std::path::PathBuf>) {
    if depth > 8 || out.len() > 5000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if SKIP.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            collect_scripts(root, &p, depth + 1, out);
        } else if name.ends_with(".sh") {
            out.push(p);
        }
    }
}

/// Simple `VAR=value` assignments, quoted or not.
fn assignments(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else { continue };
        let name = name.trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
            || name.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('\'')
            .and_then(|v| v.strip_suffix('\''))
            .or_else(|| value.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
            .unwrap_or(value);
        out.insert(name.to_string(), value.to_string());
    }
    out
}

/// Substitute `${VAR}` and `$VAR`. One pass over the longest names first, so `$FOO_BAR`
/// is not eaten by a `$FOO` that also exists.
fn expand(line: &str, vars: &HashMap<String, String>) -> String {
    let mut names: Vec<&String> = vars.keys().collect();
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    let mut out = line.to_string();
    for n in names {
        let v = &vars[n];
        out = out.replace(&format!("${{{n}}}"), v).replace(&format!("${n}"), v);
    }
    out
}

/// Split a crontab line into its schedule and the command it runs.
///
/// Five fields of digits, `*`, and the usual separators. Deliberately strict: a line that
/// merely starts with a number is not a schedule, and treating it as one would attribute
/// code to a service on no evidence.
fn split_cron(line: &str) -> Option<(String, String)> {
    if line.starts_with('#') {
        return None;
    }
    let mut it = line.split_whitespace();
    let mut fields = Vec::new();
    for _ in 0..5 {
        let f = it.next()?;
        if f.is_empty()
            || !f
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '*' | '/' | ',' | '-'))
        {
            return None;
        }
        fields.push(f);
    }
    let rest: Vec<&str> = it.collect();
    if rest.is_empty() {
        return None;
    }
    Some((fields.join(" "), rest.join(" ")))
}

/// The service a `docker exec` line targets, from the container name filter.
fn container_filter(cmd: &str) -> Option<String> {
    let i = cmd.find("name=")?;
    let tail = &cmd[i + 5..];
    let name: String = tail
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// The runner the line invokes inside the container, by basename.
fn runner_script(cmd: &str) -> Option<String> {
    cmd.split_whitespace()
        .filter(|w| w.ends_with(".sh"))
        .filter_map(|w| w.rsplit('/').next())
        .map(|s| s.to_string())
        .next_back()
        .or_else(|| {
            cmd.split_whitespace()
                .find(|w| w.ends_with(".sh"))
                .and_then(|w| w.rsplit('/').next())
                .map(|s| s.to_string())
        })
}

/// The command a runner script actually executes.
///
/// The last `exec` line, or failing that the last line that looks like an interpreter
/// invocation. Runner scripts are short and end with the thing they run; anything that
/// needs more than this is not something to guess at.
fn last_command(text: &str) -> Option<String> {
    let mut fallback = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("exec ") {
            // `"$@"` forwards the runner's own arguments and says nothing about what
            // runs; it only makes the command unreadable wherever it is quoted back.
            return Some(rest.trim().trim_end_matches("\"$@\"").trim().to_string());
        }
        if line.starts_with("python") || line.starts_with("./") || line.contains("manage.py") {
            fallback = Some(line.to_string());
        }
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_schedule_and_its_command() {
        let (sched, cmd) = split_cron("30 3 * * * docker exec foo /app/run.sh").unwrap();
        assert_eq!(sched, "30 3 * * *");
        assert!(cmd.starts_with("docker exec"));
        assert!(split_cron("# 30 3 * * * commented out").is_none());
        // A version string is not a schedule.
        assert!(split_cron("1.2.3 build something here now").is_none());
    }

    #[test]
    fn finds_the_service_and_the_runner() {
        let line = r#"docker exec "$(docker ps -q -f name=orders-cli | head -1)" /app/daily_pricing_sync.sh >> /var/log/x.log 2>&1"#;
        assert_eq!(container_filter(line).as_deref(), Some("orders-cli"));
        assert_eq!(runner_script(line).as_deref(), Some("daily_pricing_sync.sh"));
    }

    #[test]
    fn expands_the_variable_that_holds_the_service_name() {
        // The real installer defines the whole `docker exec` prefix once and reuses it;
        // without expansion every schedule line resolves to no service at all.
        let text = "CLI='docker exec \"$(docker ps -q -f name=orders-cli)\"'\n\
                    30 3 * * * ${CLI} /app/run.sh\n";
        let vars = assignments(text);
        let line = expand("30 3 * * * ${CLI} /app/run.sh", &vars);
        assert_eq!(container_filter(&line).as_deref(), Some("orders-cli"));
    }

    #[test]
    fn takes_the_exec_line_from_a_runner() {
        let script = "#!/bin/sh\n# a comment\nexec python manage.py daily_pricing_sync \"$@\"\n";
        assert_eq!(
            last_command(script).as_deref(),
            Some("python manage.py daily_pricing_sync")
        );
    }
}
