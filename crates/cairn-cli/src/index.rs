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

use anyhow::{bail, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    /// Files that mark a project root for this language. Used only to choose *where* to
    /// run the indexer once the extensions have already said the language is here.
    pub markers: &'static [&'static str],
}

pub const LANGUAGES: &[Language] = &[
    Language {
        name: "go",
        extensions: &["go"],
        indexer: "scip-go",
        markers: &["go.mod"],
    },
    Language {
        name: "python",
        extensions: &["py"],
        indexer: "scip-python",
        markers: &[
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
        ],
    },
];

/// Extensions cairn cannot index, named so a warning can say which language was skipped
/// rather than leaving someone to guess from a file count.
const UNSUPPORTED: &[(&str, &str)] = &[
    ("ts", "TypeScript"),
    ("tsx", "TypeScript"),
    ("js", "JavaScript"),
    ("jsx", "JavaScript"),
    ("rs", "Rust"),
    ("java", "Java"),
    ("kt", "Kotlin"),
    ("rb", "Ruby"),
    ("php", "PHP"),
    ("cs", "C#"),
    ("swift", "Swift"),
    ("scala", "Scala"),
    ("c", "C"),
    ("h", "C"),
    ("cc", "C++"),
    ("cpp", "C++"),
    ("hpp", "C++"),
];

/// Share of the source files below which a language is treated as incidental.
///
/// A build script in Python inside a Go repository is not a Python codebase, and indexing
/// it costs minutes to learn nothing. A floor in absolute terms as well, so a small
/// repository where everything is a small share is not silently skipped entirely.
const SHARE_THRESHOLD: f64 = 0.05;
const FILE_FLOOR: usize = 5;

/// One language found in the tree, and where its indexer should run.
pub struct Found {
    pub language: &'static Language,
    pub files: usize,
    /// Share of all source files seen, for the summary line.
    pub share: f64,
    /// Directory to run the indexer in: the shallowest project marker for this language,
    /// or the repository root when it has none.
    pub root: PathBuf,
}

/// What a walk of the tree found.
pub struct Survey {
    /// Languages worth indexing, biggest first.
    pub found: Vec<Found>,
    /// Languages present in quantity that cairn cannot index, biggest first. Named so the
    /// caller can warn about them: an index that silently covers half a repository is the
    /// same confident-incompleteness the whole design exists to avoid.
    pub unsupported: Vec<(&'static str, usize, f64)>,
    /// Every source file counted, whatever its language.
    pub total: usize,
}

/// Walk the tree once, counting files per extension.
///
/// One walk rather than one per language: on a large repository the walk is the expensive
/// part, and doing it twice to answer two questions is the kind of waste that shows up as
/// a slow first run and gets blamed on the indexer.
///
/// Counting, not just detecting, because the count is what separates a codebase from a
/// stray file. A single `setup.py` in a Go repository is not Python, and indexing it costs
/// minutes to discover nothing.
pub fn scan(repo: &Path) -> Result<Survey> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut markers: HashMap<&'static str, PathBuf> = HashMap::new();
    let mut depth_of_marker: HashMap<&'static str, usize> = HashMap::new();

    walk(repo, 0, &mut counts, &mut markers, &mut depth_of_marker)?;

    // Only extensions we can name count towards the total. Otherwise a repository full of
    // .json fixtures would drag every real language under the threshold.
    let named = |ext: &str| {
        LANGUAGES.iter().any(|l| l.extensions.contains(&ext))
            || UNSUPPORTED.iter().any(|(e, _)| *e == ext)
    };
    let total: usize = counts
        .iter()
        .filter(|(e, _)| named(e))
        .map(|(_, n)| *n)
        .sum();
    let share = |n: usize| {
        if total == 0 {
            0.0
        } else {
            n as f64 / total as f64
        }
    };
    let worth_it = |n: usize| n >= FILE_FLOOR && share(n) >= SHARE_THRESHOLD;

    let mut found: Vec<Found> = LANGUAGES
        .iter()
        .filter_map(|lang| {
            let files: usize = lang
                .extensions
                .iter()
                .map(|e| counts.get(*e).copied().unwrap_or(0))
                .sum();
            if !worth_it(files) {
                return None;
            }
            Some(Found {
                language: lang,
                files,
                share: share(files),
                root: markers
                    .get(lang.name)
                    .cloned()
                    .unwrap_or_else(|| repo.to_path_buf()),
            })
        })
        .collect();
    found.sort_by_key(|f| std::cmp::Reverse(f.files));

    // Grouped by language rather than by extension: ".ts and .tsx" is two lines that mean
    // one thing, and the reader has to do the joining.
    let mut by_language: HashMap<&'static str, usize> = HashMap::new();
    for (ext, language) in UNSUPPORTED {
        let n = counts.get(*ext).copied().unwrap_or(0);
        if n > 0 {
            *by_language.entry(language).or_insert(0) += n;
        }
    }
    let mut unsupported: Vec<(&'static str, usize, f64)> = by_language
        .into_iter()
        .filter(|(_, n)| worth_it(*n))
        .map(|(l, n)| (l, n, share(n)))
        .collect();
    unsupported.sort_by_key(|(_, n, _)| std::cmp::Reverse(*n));

    Ok(Survey {
        found,
        unsupported,
        total,
    })
}

fn walk(
    dir: &Path,
    depth: usize,
    counts: &mut HashMap<String, usize>,
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
        }
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            *counts.entry(ext.to_string()).or_insert(0) += 1;
        }
    }
    Ok(())
}

/// What happened when one language was indexed.
pub enum Outcome {
    Indexed { scip: PathBuf, seconds: f64 },
    Failed(String),
}

/// Run one language's indexer in the shared image, writing SCIP into the index directory.
///
/// The output path is expressed inside the container, not on the host: the repository is
/// mounted at a fixed place, so the paths the indexer records do not depend on where the
/// repository happens to live on this machine.
pub fn run_indexer(found: &Found, repo: &Path, out_rel: &Path) -> Outcome {
    let started = std::time::Instant::now();
    let out_in_container = Path::new("/repo")
        .join(out_rel)
        .join(format!("{}.scip", found.language.name));
    let out = out_in_container.to_string_lossy().to_string();

    let project;
    let args: Vec<&str> = match found.language.name {
        "go" => vec![found.language.indexer, "--output", &out],
        "python" => {
            // `--project-name` is required and ends up inside every symbol string, so it
            // has to be stable across runs: the directory name is, a timestamp is not.
            project = found
                .root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "project".to_string());
            vec![
                "scip-python",
                "index",
                ".",
                "--project-name",
                &project,
                "--project-version",
                "cairn",
                "--output",
                &out,
            ]
        }
        other => return Outcome::Failed(format!("no invocation known for {other}")),
    };

    let result = crate::docker::exec(repo, &found.root, &args);
    let on_host = repo
        .join(out_rel)
        .join(format!("{}.scip", found.language.name));

    match result {
        Ok(o) if o.status.success() && on_host.exists() => Outcome::Indexed {
            scip: on_host,
            seconds: started.elapsed().as_secs_f64(),
        },
        Ok(o) => {
            // Not the last line: a crashing Node prints its own version last, so taking
            // the tail reports "Node.js v22" as though that were the complaint. The first
            // line that looks like an error is the one worth showing, and the rest is kept
            // in a file so nothing is actually lost.
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
            let log = on_host.with_extension("log");
            let _ = std::fs::write(&log, err.as_bytes());
            Outcome::Failed(if complaint.is_empty() {
                format!("exited with {} (output in {})", o.status, log.display())
            } else {
                format!("{complaint} (full output in {})", log.display())
            })
        }
        Err(e) => Outcome::Failed(format!("{e:#}")),
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

    /// Enough files to clear both the absolute floor and the share threshold.
    fn many(prefix: &str, ext: &str, n: usize) -> Vec<(String, String)> {
        (0..n)
            .map(|i| (format!("{prefix}/f{i}.{ext}"), String::new()))
            .collect()
    }

    fn refs(v: &[(String, String)]) -> Vec<(&str, &str)> {
        v.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect()
    }

    #[test]
    fn extensions_decide_the_language_not_the_marker_file() {
        // No go.mod and no pyproject.toml anywhere: the code is still what it is.
        let mut files = many("srcgo", "go", 10);
        files.extend(many("srcpy", "py", 6));
        let dir = tree("extensions", &refs(&files));
        let survey = scan(&dir).unwrap();
        let names: Vec<_> = survey.found.iter().map(|f| f.language.name).collect();
        assert_eq!(names, vec!["go", "python"], "biggest first");
        assert_eq!(survey.found[0].files, 10);
        assert_eq!(survey.total, 16);
    }

    #[test]
    fn a_marker_with_no_code_is_not_a_language() {
        let mut files = vec![("pyproject.toml".to_string(), String::new())];
        files.extend(many(".", "go", 10));
        let dir = tree("marker-only", &refs(&files));
        let survey = scan(&dir).unwrap();
        assert_eq!(survey.found.len(), 1);
        assert_eq!(survey.found[0].language.name, "go");
    }

    #[test]
    fn a_stray_script_is_not_a_language() {
        // The case the threshold exists for: one helper script should not cost minutes of
        // indexing and a second toolchain.
        let mut files = many("cmd", "go", 40);
        files.push(("scripts/helper.py".to_string(), String::new()));
        let dir = tree("stray", &refs(&files));
        let survey = scan(&dir).unwrap();
        let names: Vec<_> = survey.found.iter().map(|f| f.language.name).collect();
        assert_eq!(
            names,
            vec!["go"],
            "one .py in forty .go files is not Python"
        );
    }

    #[test]
    fn a_language_we_cannot_read_is_named_rather_than_ignored() {
        let mut files = many("srcgo", "go", 20);
        files.extend(many("web", "ts", 20));
        let dir = tree("unsupported", &refs(&files));
        let survey = scan(&dir).unwrap();
        assert_eq!(survey.found.len(), 1, "only Go can be indexed");
        assert_eq!(survey.unsupported.len(), 1);
        assert_eq!(survey.unsupported[0].0, "TypeScript");
        assert_eq!(survey.unsupported[0].1, 20);
    }

    #[test]
    fn extensions_of_one_language_are_counted_together() {
        let mut files = many("srcgo", "go", 20);
        files.extend(many("web", "ts", 11));
        files.extend(many("web", "tsx", 9));
        let dir = tree("grouped", &refs(&files));
        let survey = scan(&dir).unwrap();
        assert_eq!(survey.unsupported[0].0, "TypeScript");
        assert_eq!(survey.unsupported[0].1, 20, ".ts and .tsx are one language");
    }

    #[test]
    fn the_indexer_runs_at_the_shallowest_marker() {
        let mut files = vec![
            ("go.mod".to_string(), "module x".to_string()),
            ("services/inner/go.mod".to_string(), "module y".to_string()),
        ];
        files.extend(many("services/inner", "go", 10));
        let dir = tree("shallowest", &refs(&files));
        let survey = scan(&dir).unwrap();
        assert_eq!(
            survey.found[0].root, dir,
            "the root module sees every package"
        );
    }

    #[test]
    fn without_a_marker_the_indexer_runs_at_the_repository_root() {
        let dir = tree("no-marker", &refs(&many("pkg", "go", 10)));
        assert_eq!(scan(&dir).unwrap().found[0].root, dir);
    }

    #[test]
    fn noise_directories_are_not_walked() {
        let mut files = many(".", "py", 10);
        files.extend(many("node_modules/pkg", "py", 200));
        files.extend(many(".venv/lib", "py", 200));
        let dir = tree("noise", &refs(&files));
        let survey = scan(&dir).unwrap();
        assert_eq!(
            survey.found[0].files, 10,
            "only the project's own files count"
        );
        assert_eq!(survey.total, 10);
    }

    #[test]
    fn a_tree_with_nothing_indexable_finds_nothing() {
        let dir = tree("nothing", &[("README.md", ""), ("Makefile", "")]);
        let survey = scan(&dir).unwrap();
        assert!(survey.found.is_empty());
        assert!(survey.unsupported.is_empty());
    }
}
