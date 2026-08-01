//! Turning a directory into an index, without the caller having heard of SCIP.
//!
//! `cairn index` used to take a list of `.scip` files and a `--repo` flag, which meant the
//! first thing a new user had to learn was a file format and two indexers they had never
//! run. That is a build step wearing a tool's clothes. Now the command takes nothing: the
//! directory you are standing in is the repository, and what to run against it is
//! something to work out rather than ask.
//!
//! The languages are decided by **what is actually in the tree**, not by a marker file.
//! A repository with a hundred `.go` files under `srcgo/` and no `go.mod` at the root is
//! still a Go repository, and a `pyproject.toml` next to no Python is not a Python one.
//! So the whole tree is walked, extensions are counted, and the count decides.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Directories never worth walking into. Cheap to list and expensive to get wrong: a
/// `node_modules` in a large repository is most of the file count and none of the code.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    "target",
    "dist",
    "build",
    "__pycache__",
    "site-packages",
    ".venv",
    "venv",
];

/// Depth bound, so a symlink loop or a pathological tree cannot hang the command.
const MAX_DEPTH: usize = 24;

/// A language cairn can index, and what it takes to do it.
pub struct Language {
    pub name: &'static str,
    /// File extensions that mean this language is present.
    pub extensions: &'static [&'static str],
    /// The indexer binary, as it is spelled on PATH.
    pub indexer: &'static str,
    /// What to tell someone who has not got it.
    pub install: &'static str,
    /// Files that mark a project root for this language. Used only to choose *where* to
    /// run the indexer once the extensions have already said the language is here.
    pub markers: &'static [&'static str],
}

pub const LANGUAGES: &[Language] = &[
    Language {
        name: "go",
        extensions: &["go"],
        indexer: "scip-go",
        install: "go install github.com/scip-code/scip-go/cmd/scip-go@latest",
        markers: &["go.mod"],
    },
    Language {
        name: "python",
        extensions: &["py"],
        indexer: "scip-python",
        install: "npm install -g @sourcegraph/scip-python",
        markers: &[
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
        ],
    },
];

/// One language found in the tree, and where its indexer should run.
pub struct Found {
    pub language: &'static Language,
    pub files: usize,
    /// Directory to run the indexer in: the shallowest project marker for this language,
    /// or the repository root when it has none.
    pub root: PathBuf,
}

/// Walk the tree once, counting source files per extension and noting project markers.
///
/// One walk rather than one per language: on a large repository the walk is the expensive
/// part, and doing it twice to answer two questions is the kind of waste that shows up as
/// a slow first run and gets blamed on the indexer.
pub fn scan(repo: &Path) -> Result<Vec<Found>> {
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    let mut markers: HashMap<&'static str, PathBuf> = HashMap::new();
    let mut depth_of_marker: HashMap<&'static str, usize> = HashMap::new();

    walk(repo, 0, &mut counts, &mut markers, &mut depth_of_marker)?;

    let mut found: Vec<Found> = LANGUAGES
        .iter()
        .filter_map(|lang| {
            let files = counts.get(lang.name).copied().unwrap_or(0);
            if files == 0 {
                return None;
            }
            Some(Found {
                language: lang,
                files,
                root: markers
                    .get(lang.name)
                    .cloned()
                    .unwrap_or_else(|| repo.to_path_buf()),
            })
        })
        .collect();
    // Biggest first, so the slowest language starts while attention is still on the
    // command, and so the summary reads in order of what matters.
    found.sort_by_key(|f| std::cmp::Reverse(f.files));
    Ok(found)
}

fn walk(
    dir: &Path,
    depth: usize,
    counts: &mut HashMap<&'static str, usize>,
    markers: &mut HashMap<&'static str, PathBuf>,
    depth_of_marker: &mut HashMap<&'static str, usize>,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        // An unreadable directory is not a reason to fail the whole command; it is a
        // reason to index what can be read.
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(kind) = entry.file_type() else {
            continue;
        };

        if kind.is_dir() {
            // Symlinked directories are not followed: a link pointing at a parent turns
            // the walk into a loop, and a link pointing outside the repo indexes code the
            // repository does not own.
            if kind.is_symlink() || name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk(&path, depth + 1, counts, markers, depth_of_marker)?;
            continue;
        }

        for lang in LANGUAGES {
            if lang.markers.contains(&name.as_str()) {
                // Shallowest wins: in a monorepo the root module is the one that sees
                // every package, and a deeper one would index a fraction of the tree.
                let previous = depth_of_marker.get(lang.name).copied();
                if previous.is_none_or(|d| depth < d) {
                    depth_of_marker.insert(lang.name, depth);
                    markers.insert(lang.name, dir.to_path_buf());
                }
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if lang.extensions.contains(&ext) {
                *counts.entry(lang.name).or_insert(0) += 1;
            }
        }
    }
    Ok(())
}

/// Where a program lives on PATH, if it does.
///
/// Resolved rather than handed to `Command`, so "is the indexer installed" can be answered
/// and reported before anything is run, instead of surfacing as a spawn failure halfway
/// through.
pub fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    // Windows needs the extension: CreateProcess only ever appends `.exe`, and an npm
    // shim is a `.cmd`.
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_string())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let candidate = dir.join(format!("{program}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// What happened when one language was indexed.
pub enum Outcome {
    Indexed { scip: PathBuf, seconds: f64 },
    NoIndexer,
    Failed(String),
}

/// Run the indexer for one language, writing its SCIP output into the index directory.
pub fn run_indexer(found: &Found, out_dir: &Path) -> Outcome {
    let Some(indexer) = on_path(found.language.indexer) else {
        return Outcome::NoIndexer;
    };
    // Absolute, because the indexer is spawned with its own working directory: a relative
    // `.cairn/go.scip` would resolve against the module root and fail there instead of
    // landing beside the index.
    let out = match std::path::absolute(out_dir.join(format!("{}.scip", found.language.name))) {
        Ok(p) => p,
        Err(e) => return Outcome::Failed(format!("resolving the output path: {e}")),
    };
    let started = std::time::Instant::now();

    let mut cmd = Command::new(indexer);
    match found.language.name {
        "go" => {
            cmd.arg("--output").arg(&out);
        }
        "python" => {
            // `--project-name` is required and ends up inside every symbol string, so it
            // has to be stable across runs: the directory name is, a timestamp is not.
            let project = found
                .root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "project".to_string());
            cmd.arg("index")
                .arg(".")
                .arg("--project-name")
                .arg(project)
                .arg("--project-version")
                .arg("cairn")
                .arg("--output")
                .arg(&out);
        }
        other => return Outcome::Failed(format!("no invocation known for {other}")),
    }

    let result = cmd
        .current_dir(&found.root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();

    match result {
        Ok(o) if o.status.success() && out.exists() => Outcome::Indexed {
            scip: out,
            seconds: started.elapsed().as_secs_f64(),
        },
        Ok(o) => {
            // Not the last line: a crashing Node prints its own version last, so taking
            // the tail reports "Node.js v22" as though that were the complaint. The first
            // line that looks like an error is the one worth showing, and the rest is kept
            // for the file so nothing is actually lost.
            let err = String::from_utf8_lossy(&o.stderr);
            let lines: Vec<&str> = err
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            let complaint = lines
                .iter()
                .find(|l| {
                    let l = l.to_ascii_lowercase();
                    l.contains("error") || l.contains("failed") || l.contains("cannot")
                })
                .copied()
                .or_else(|| lines.first().copied())
                .unwrap_or("");
            let log = out.with_extension("log");
            let _ = std::fs::write(&log, err.as_bytes());
            Outcome::Failed(if complaint.is_empty() {
                format!("exited with {} (output in {})", o.status, log.display())
            } else {
                format!("{complaint} (full output in {})", log.display())
            })
        }
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// Refuse to index the directory the binary lives in.
///
/// Not a guess about intent — everywhere else, standing in a directory *is* the intent, and
/// there is deliberately no check for `.git`. This one case is different because it is
/// never what anyone meant: `~/.cairn/bin` holds cairn, not the code being asked about, and
/// indexing it produces a large index of nothing.
pub fn refuse_own_directory(repo: &Path) -> Result<()> {
    let Ok(exe) = std::env::current_exe() else {
        return Ok(());
    };
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let Some(exe_dir) = exe.parent() else {
        return Ok(());
    };
    let here = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    if here == exe_dir {
        bail!(
            "this is where cairn itself is installed ({}), not a repository to index.\n\
             cd to the root of the codebase you want indexed and run `cairn index` there.",
            here.display()
        );
    }
    Ok(())
}

/// The directory SCIP output and the index share.
pub fn index_dir(db: &Path) -> Result<PathBuf> {
    db.parent()
        .map(|d| d.to_path_buf())
        .context("the index path has no parent directory")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a throwaway tree. `name` is the calling test's, not a description of the
    /// contents: cargo runs these in parallel, and keying the directory on anything two
    /// tests can share — the file count, say — means one test deletes another's tree
    /// halfway through and the failure lands wherever the scheduler put it.
    fn tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cairn-scan-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        for (path, body) in files {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, body).unwrap();
        }
        dir
    }

    #[test]
    fn extensions_decide_the_language_not_the_marker_file() {
        // No go.mod and no pyproject.toml anywhere: the code is still what it is.
        let dir = tree(
            "extensions",
            &[("srcgo/a.go", ""), ("srcgo/b.go", ""), ("srcpy/c.py", "")],
        );
        let found = scan(&dir).unwrap();
        let names: Vec<_> = found.iter().map(|f| f.language.name).collect();
        assert_eq!(names, vec!["go", "python"], "biggest first");
        assert_eq!(found[0].files, 2);
    }

    #[test]
    fn a_marker_with_no_code_is_not_a_language() {
        let dir = tree("marker-only", &[("pyproject.toml", ""), ("main.go", "")]);
        let found = scan(&dir).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].language.name, "go");
    }

    #[test]
    fn the_indexer_runs_at_the_shallowest_marker() {
        let dir = tree(
            "shallowest",
            &[
                ("go.mod", "module x"),
                ("services/inner/go.mod", "module y"),
                ("services/inner/a.go", ""),
            ],
        );
        let found = scan(&dir).unwrap();
        assert_eq!(found[0].root, dir, "the root module sees every package");
    }

    #[test]
    fn without_a_marker_the_indexer_runs_at_the_repository_root() {
        let dir = tree("no-marker", &[("pkg/a.go", "")]);
        let found = scan(&dir).unwrap();
        assert_eq!(found[0].root, dir);
    }

    #[test]
    fn noise_directories_are_not_walked() {
        let dir = tree(
            "noise",
            &[
                ("app.py", ""),
                ("node_modules/pkg/vendored.py", ""),
                (".venv/lib/thing.py", ""),
                (".git/hooks/sample.py", ""),
            ],
        );
        let found = scan(&dir).unwrap();
        assert_eq!(found[0].files, 1, "only the project's own file counts");
    }

    #[test]
    fn a_tree_with_nothing_indexable_finds_nothing() {
        let dir = tree("nothing", &[("README.md", ""), ("Makefile", "")]);
        assert!(scan(&dir).unwrap().is_empty());
    }
}
