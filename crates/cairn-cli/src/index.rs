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
    /// Which directories the indexer has to be run in.
    pub roots: RootRule,
    /// Files that name the members of a workspace, in the order they are consulted.
    ///
    /// Authoritative where one exists: a workspace file lists exactly the projects that
    /// build together. `go.work` includes generated modules a walk would have to guess
    /// about; `pnpm-workspace.yaml` *excludes* retired ones, and that exclusion is the
    /// repository's own statement that a change there does nothing. Ignoring it is how
    /// cairn came to index a retired app, install its stale dependency tree, fail, and
    /// then report a gap about code that was never part of the build.
    pub workspace_files: &'static [&'static str],
}

/// How many places an indexer has to run for one language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootRule {
    /// One run at the shallowest marker: the outermost project sees everything inside it.
    Shallowest,
    /// One run per marker that is not inside another marker's directory.
    ///
    /// JavaScript workspaces: `apps/a` and `apps/b` are separate compilations with
    /// separate dependency trees, and indexing the shallowest one covered a retired app
    /// while silently leaving out the live one of 827 files.
    Outermost,
    /// One run per marker, nesting included.
    ///
    /// Go's own rule: a `go.mod` inside another module's directory is *excluded* from
    /// that module, so it is a separate compilation and nothing else will index it. This
    /// was `Shallowest`, on the stated grounds that "the outermost project sees every
    /// package inside it" - true of a single-module repository and false of every
    /// workspace. Measured on a real monorepo: 33 Go modules, of which exactly one was
    /// indexed, and the other 32 were missing from every answer with nothing saying so.
    EveryModule,
    /// One run per marker that is not a declared member of another marker's workspace.
    ///
    /// Cargo's own rule, and it is neither of the other two: a `Cargo.toml` with a
    /// `[workspace]` table resolves every member it names in the *same* `cargo metadata`
    /// pass, so indexing a member separately is not a second compilation the way a nested
    /// Go module is - it is one project counted twice. But an unrelated `Cargo.toml` that
    /// nobody's workspace claims - a standalone crate, or an example nested under one by
    /// accident rather than by membership - is exactly the nested-and-silently-dropped
    /// case `EveryModule` exists to avoid, so nesting alone cannot be the exclusion either
    /// the way it is for `Outermost`. Membership, read from `[workspace] members` /
    /// `exclude`, is the only signal that actually answers "does this one project cover
    /// that directory already".
    Workspace,
}

pub const LANGUAGES: &[Language] = &[
    Language {
        name: "go",
        extensions: &["go"],
        indexer: "scip-go",
        markers: &["go.mod"],
        roots: RootRule::EveryModule,
        workspace_files: &["go.work"],
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
        // One run: a Python package tree below the outermost project is part of it.
        roots: RootRule::Shallowest,
        workspace_files: &[],
    },
    Language {
        // `.js` and `.jsx` are indexed by the same tool and land in the same graph, so
        // they are extensions of this language rather than a language of their own.
        name: "typescript",
        extensions: &["ts", "tsx", "js", "jsx"],
        indexer: "scip-typescript",
        // Only the compiler configs. `package.json` was here and it was wrong: in a pnpm
        // or yarn workspace there is one at the root that configures no TypeScript
        // project at all, so the indexer was pointed at a directory with no tsconfig and
        // refused. What marks a TypeScript project is the file the compiler reads.
        markers: &["tsconfig.json", "jsconfig.json"],
        roots: RootRule::Outermost,
        // pnpm first, because a repository that has both means the pnpm one: npm's
        // `workspaces` key is often left behind in `package.json` after a move to pnpm.
        workspace_files: &["pnpm-workspace.yaml", "package.json"],
    },
    Language {
        name: "rust",
        extensions: &["rs"],
        indexer: "rust-analyzer",
        markers: &["Cargo.toml"],
        // Not `workspace_files`: that mechanism looks for one fixed name directly at the
        // repository root (`go.work`, `pnpm-workspace.yaml`), and Cargo's workspace root
        // is just a `Cargo.toml` like any other, wherever one actually is. `RootRule::
        // Workspace` reads the `[workspace]` table out of every `Cargo.toml` it already
        // found instead.
        roots: RootRule::Workspace,
        workspace_files: &[],
    },
];

/// The project directories a workspace file names.
///
/// Three shapes, dispatched on the file name, because that is what the ecosystems wrote:
///
/// * `go.work` — `use` entries, one per line or in a parenthesised block.
/// * `pnpm-workspace.yaml` — a `packages:` list of globs, **negations included**. The
///   negation is the point: a repository that retires an app writes `!apps/old` there,
///   and that is its statement that a change in it does nothing. cairn used to index it
///   anyway - installing its stale lockfile, failing, and then reporting a gap about code
///   that was never in the build.
/// * `package.json` — the `workspaces` key, as an array or as `{ "packages": [...] }`.
///
/// A member that is not on disk is dropped rather than guessed at: that is the state of a
/// fresh checkout whose generated modules have not been produced yet, and pointing an
/// indexer at a directory that is not there produces an error instead of an answer.
fn workspace_members(repo: &Path, file: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let patterns = match name {
        "go.work" => return go_work_members(repo, &text),
        "pnpm-workspace.yaml" => yaml_packages(&text),
        "package.json" => json_workspaces(&text),
        _ => Vec::new(),
    };
    expand_members(repo, &patterns)
}

/// `use` entries of a `go.work`, as directories that hold a module.
fn go_work_members(repo: &Path, text: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut in_block = false;
    for raw in text.lines() {
        let line = raw.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if in_block {
            if line == ")" {
                in_block = false;
                continue;
            }
            push_member(repo, line, "go.mod", &mut out);
            continue;
        }
        if let Some(rest) = line.strip_prefix("use") {
            let rest = rest.trim();
            if rest == "(" {
                in_block = true;
            } else if !rest.is_empty() {
                push_member(repo, rest, "go.mod", &mut out);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn push_member(repo: &Path, entry: &str, marker: &str, out: &mut Vec<PathBuf>) {
    let cleaned = entry.trim().trim_matches('"').trim_start_matches("./");
    if cleaned.is_empty() || cleaned == "." {
        out.push(repo.to_path_buf());
        return;
    }
    let path = repo.join(cleaned);
    // Only what is there, and only if it is the kind of project the file claims.
    if path.join(marker).exists() {
        out.push(path);
    }
}

/// The `packages:` list of a `pnpm-workspace.yaml`, as written.
///
/// A deliberately small reader: a `packages:` key followed by `- "glob"` items. Enough
/// for every workspace file this has to understand, and it keeps a YAML dependency out of
/// a decision this important - what it cannot parse, it says nothing about, and the walk
/// takes over.
fn yaml_packages(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_packages = false;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("");
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !line.starts_with(char::is_whitespace) && trimmed.starts_with("packages:") {
            in_packages = true;
            continue;
        }
        if in_packages {
            match trimmed.strip_prefix("- ") {
                Some(item) => out.push(item.trim().trim_matches(['"', '\'']).to_string()),
                // A key at any indentation ends the list.
                None if trimmed.ends_with(':') => in_packages = false,
                None => {}
            }
        }
    }
    out
}

/// The `workspaces` key of a `package.json`, in both shapes npm accepts.
fn json_workspaces(text: &str) -> Vec<String> {
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let value = &doc["workspaces"];
    let list = if value.is_array() {
        value.as_array()
    } else {
        value["packages"].as_array()
    };
    list.map(|items| {
        items
            .iter()
            .filter_map(|i| i.as_str())
            .map(String::from)
            .collect()
    })
    .unwrap_or_default()
}

/// The directories a `Cargo.toml`'s `[workspace]` table claims, or `None` if it has no
/// such table - a plain crate, standalone or itself a member some ancestor already
/// claims. `members` and `exclude` are read the way `pnpm-workspace.yaml`'s negation is
/// (`expand_members` applies both), because an excluded member is the workspace's own
/// statement that a directory under it does not build, same lesson as there.
fn cargo_workspace_members(dir: &Path) -> Option<Vec<PathBuf>> {
    let text = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    let section = workspace_section(&text)?;
    let mut patterns = toml_string_array(section, "members");
    patterns.extend(
        toml_string_array(section, "exclude")
            .into_iter()
            .map(|e| format!("!{e}")),
    );
    Some(expand_members(dir, &patterns))
}

/// The text of a `Cargo.toml`'s own `[workspace]` table, not `[workspace.package]` or
/// anything else that starts the same way and not the `[package]` table above it.
///
/// A mixed manifest - `[package]` and `[workspace]` in the same file, which is how
/// ripgrep's own root `Cargo.toml` is written - has an `exclude` key that belongs to
/// `[package]` (what `cargo publish` leaves out) and means nothing about workspace
/// membership at all. Searching the whole file for a key named `exclude` would read
/// that one by accident; TOML tables run until the next `[...]` header, so slicing to
/// the lines between `[workspace]` and whatever heading comes after it is what actually
/// answers "is this key in this table".
fn workspace_section(text: &str) -> Option<&str> {
    let mut in_section = false;
    let mut start = None;
    let mut end = text.len();
    let mut pos = 0;
    for line in text.lines() {
        let line_start = pos;
        pos += line.len() + 1; // +1 for the newline this split consumed
        let trimmed = line.trim();
        if in_section {
            if trimmed.starts_with('[') {
                end = line_start;
                break;
            }
            continue;
        }
        if trimmed == "[workspace]" {
            in_section = true;
            start = Some(pos);
        }
    }
    Some(&text[start?..end])
}

/// A TOML array of strings written as `key = [...]`, on one line or several. Only what
/// `Cargo.toml`'s `members`/`exclude` need: quoted strings, comma-separated.
fn toml_string_array(text: &str, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_array = false;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if !in_array {
            let Some(rest) = line.strip_prefix(key) else {
                continue;
            };
            let Some(rest) = rest.trim_start().strip_prefix('=') else {
                continue;
            };
            let Some(rest) = rest.trim_start().strip_prefix('[') else {
                continue;
            };
            in_array = true;
            match rest.split_once(']') {
                Some((inside, _)) => {
                    push_toml_strings(inside, &mut out);
                    break;
                }
                None => push_toml_strings(rest, &mut out),
            }
            continue;
        }
        match line.split_once(']') {
            Some((inside, _)) => {
                push_toml_strings(inside, &mut out);
                break;
            }
            None => push_toml_strings(line, &mut out),
        }
    }
    out
}

fn push_toml_strings(s: &str, out: &mut Vec<String>) {
    for part in s.split(',') {
        let p = part.trim().trim_matches(['"', '\'']);
        if !p.is_empty() {
            out.push(p.to_string());
        }
    }
}

/// Turn workspace globs into the directories they actually name.
///
/// Only the one wildcard shape these files use in practice - `apps/*`, `packages/*` - and
/// a leading `!` removes whatever the positive patterns matched. A pattern this cannot
/// read matches nothing, which falls back to the walk rather than inventing a member.
fn expand_members(repo: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let mut included: Vec<PathBuf> = Vec::new();
    let mut excluded: Vec<PathBuf> = Vec::new();
    for pattern in patterns {
        let (negated, glob) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest.trim()),
            None => (false, pattern.as_str()),
        };
        let matched = match glob.split_once('*') {
            None => {
                let path = repo.join(glob.trim_start_matches("./"));
                if path.is_dir() {
                    vec![path]
                } else {
                    Vec::new()
                }
            }
            Some((prefix, suffix)) => {
                // `apps/*` and `apps/*/thing`: the children of the prefix, filtered by
                // whatever follows the star.
                let base = repo.join(prefix.trim_end_matches('/').trim_start_matches("./"));
                let mut found = Vec::new();
                if let Ok(entries) = std::fs::read_dir(&base) {
                    for e in entries.flatten() {
                        let p = e.path();
                        if !p.is_dir() {
                            continue;
                        }
                        let tail = suffix.trim_start_matches('/');
                        let candidate = if tail.is_empty() { p } else { p.join(tail) };
                        if candidate.is_dir() {
                            found.push(candidate);
                        }
                    }
                }
                found
            }
        };
        if negated {
            excluded.extend(matched);
        } else {
            included.extend(matched);
        }
    }
    included.retain(|p| !excluded.iter().any(|x| p == x));
    included.sort();
    included.dedup();
    included
}

/// Extensions cairn cannot index, named so a warning can say which language was skipped
/// rather than leaving someone to guess from a file count.
const UNSUPPORTED: &[(&str, &str)] = &[
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
    /// Directories to run the indexer in, sorted so a run is reproducible.
    ///
    /// One entry for a language whose outermost project contains the rest; one per
    /// workspace member where it does not. Never empty: with no marker at all it is the
    /// repository root.
    pub roots: Vec<PathBuf>,
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
    /// `.proto` files in the tree.
    ///
    /// Not a language here — nothing indexes protobuf as source. It is counted because it
    /// is the only way to tell "this repository has no gRPC" from "the cross-language
    /// pass found nothing", and those two readings of an empty answer are opposites.
    pub protos: usize,
    /// Every directory holding a `package.json`, relative to the repository root, sorted.
    ///
    /// Not a marker for anything — a workspace root's `package.json` configures no
    /// TypeScript project. It is collected because these are exactly the directories a
    /// package manager creates a `node_modules` in, and cairn has to cover each of them
    /// with a volume of its own **before** any install runs. One that is missed is not a
    /// smaller failure: the tree lands in the user's working copy instead, which is the
    /// thing managing dependencies here exists to prevent. A workspace install writes to
    /// every member, not only the project being indexed, so the list is the whole tree's.
    pub package_dirs: Vec<PathBuf>,
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
    let mut all_markers: HashMap<&'static str, Vec<PathBuf>> = HashMap::new();
    let mut package_dirs: Vec<PathBuf> = Vec::new();

    walk(
        repo,
        0,
        &mut counts,
        &mut markers,
        &mut depth_of_marker,
        &mut all_markers,
        &mut package_dirs,
    )?;
    // Relative to the repository, because that is how they are used: a path inside the
    // container and a key for the volume that covers it.
    let mut package_dirs: Vec<PathBuf> = package_dirs
        .iter()
        .filter_map(|d| d.strip_prefix(repo).ok().map(|p| p.to_path_buf()))
        .collect();
    package_dirs.sort();

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
            let shallowest = markers
                .get(lang.name)
                .cloned()
                .unwrap_or_else(|| repo.to_path_buf());
            let mut roots = vec![shallowest];
            // The workspace file first, where the language has one and the repository
            // wrote one: it names the projects that build together, which is a stronger
            // answer than anything a walk can conclude - and the only place an
            // *exclusion* is written down at all.
            let members: Vec<PathBuf> = lang
                .workspace_files
                .iter()
                .map(|f| repo.join(f))
                .find(|p| p.exists())
                .map(|p| workspace_members(repo, &p))
                .unwrap_or_default();
            if !members.is_empty() {
                // The language's own markers, kept to the ones a member contains. For Go
                // the marker *is* the member (`go.mod` in each `use` entry); for a
                // JavaScript workspace a member may hold one tsconfig or none.
                let inside = |p: &PathBuf| members.iter().any(|m| p.starts_with(m));
                let mut chosen: Vec<PathBuf> = all_markers
                    .get(lang.name)
                    .map(|all| all.iter().filter(|p| inside(p)).cloned().collect())
                    .unwrap_or_default();
                chosen.sort();
                chosen.dedup();
                if !chosen.is_empty() {
                    roots = chosen;
                }
            } else if let Some(all) = all_markers.get(lang.name) {
                let mut chosen: Vec<PathBuf> = match lang.roots {
                    RootRule::Shallowest => Vec::new(),
                    // A marker below another one belongs to that compilation.
                    RootRule::Outermost => all
                        .iter()
                        .filter(|p| !all.iter().any(|q| q != *p && p.starts_with(q)))
                        .cloned()
                        .collect(),
                    // Every one of them, nesting included: see `RootRule::EveryModule`.
                    RootRule::EveryModule => all.clone(),
                    // Every one of them that is not a member some other one already
                    // claims: see `RootRule::Workspace`.
                    RootRule::Workspace => {
                        let mut claimed: Vec<PathBuf> = Vec::new();
                        for dir in all {
                            if let Some(members) = cargo_workspace_members(dir) {
                                claimed.extend(members);
                            }
                        }
                        all.iter()
                            .filter(|p| !claimed.contains(p))
                            .cloned()
                            .collect()
                    }
                };
                chosen.sort();
                chosen.dedup();
                // Sorted, so which project is indexed first does not depend on the
                // order the filesystem handed back its entries. It did: the first run
                // on a three-app workspace picked the retired one.
                if !chosen.is_empty() {
                    roots = chosen;
                }
            }
            Some(Found {
                language: lang,
                files,
                share: share(files),
                roots,
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
        protos: counts.get("proto").copied().unwrap_or(0),
        package_dirs,
    })
}

#[allow(clippy::too_many_arguments)]
fn walk(
    dir: &Path,
    depth: usize,
    counts: &mut HashMap<String, usize>,
    markers: &mut HashMap<&'static str, PathBuf>,
    depth_of_marker: &mut HashMap<&'static str, usize>,
    all_markers: &mut HashMap<&'static str, Vec<PathBuf>>,
    package_dirs: &mut Vec<PathBuf>,
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
            walk(
                &path,
                depth + 1,
                counts,
                markers,
                depth_of_marker,
                all_markers,
                package_dirs,
            )?;
            continue;
        }

        if name == "package.json" {
            package_dirs.push(dir.to_path_buf());
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
                let seen = all_markers.entry(lang.name).or_default();
                if !seen.contains(&dir.to_path_buf()) {
                    seen.push(dir.to_path_buf());
                }
            }
        }
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            *counts.entry(ext.to_string()).or_insert(0) += 1;
        }
    }
    Ok(())
}

/// How a project's dependencies get installed, decided by the lockfile it commits.
pub enum Install {
    /// The manager and the flags that install exactly what the lockfile pins.
    Frozen {
        lock: &'static str,
        argv: Vec<&'static str>,
    },
    /// A lockfile cairn recognises and cannot install from, with the reason to say so.
    Unsupported {
        lock: &'static str,
        why: &'static str,
    },
}

/// The package manager a project declares, and the command that installs from its lockfile.
///
/// Read from the lockfile rather than from `packageManager` in package.json or from what
/// happens to be on PATH: the lockfile is the file the project actually resolves against,
/// and installing with the wrong manager produces a different tree from the one the app
/// runs on.
///
/// `--frozen-lockfile` and its equivalents throughout: cairn installs what the repository
/// declares and never resolves a new version. An index is a projection of the code as it
/// is, and a dependency tree it invented would make it a projection of something else.
pub fn install_command(root: &Path) -> Option<Install> {
    let frozen = |lock, argv: &[&'static str]| {
        Some(Install::Frozen {
            lock,
            argv: argv.to_vec(),
        })
    };

    if root.join("pnpm-lock.yaml").is_file() {
        return frozen(
            "pnpm-lock.yaml",
            &["pnpm", "install", "--frozen-lockfile", "--ignore-scripts"],
        );
    }
    if root.join("yarn.lock").is_file() {
        // Yarn 1 and Yarn 2+ are different programs reading different formats out of the
        // same filename, and Yarn 1 does not say when it is handed the wrong one. Given
        // `--immutable`, which only Yarn 2+ knows, it ignores the unknown flag, resolves
        // fresh versions and writes the lockfile back — measured on an empty project,
        // where it reported "success Saved lockfile". Modifying the working tree is the
        // one thing indexing must never do, so the format is read rather than assumed.
        let berry = std::fs::read_to_string(root.join("yarn.lock"))
            .map(|s| s.contains("__metadata:"))
            .unwrap_or(false);
        return if berry {
            Some(Install::Unsupported {
                lock: "yarn.lock",
                why: "this is a Yarn 2+ (Berry) lockfile and the indexer image ships Yarn 1, \
                      which would resolve its own versions rather than install what the file \
                      pins. Install this project's dependencies yourself and run `cairn index` \
                      again",
            })
        } else {
            frozen(
                "yarn.lock",
                &["yarn", "install", "--frozen-lockfile", "--ignore-scripts"],
            )
        };
    }
    for lock in ["bun.lock", "bun.lockb"] {
        if root.join(lock).is_file() {
            return frozen(
                lock,
                &["bun", "install", "--frozen-lockfile", "--ignore-scripts"],
            );
        }
    }
    if root.join("package-lock.json").is_file() {
        return frozen("package-lock.json", &["npm", "ci", "--ignore-scripts"]);
    }
    // A workspace member usually has no lockfile of its own; the root holds it.
    None
}

/// What to say when a package manager refuses to install.
///
/// One failure here is neither cairn's fault nor a broken machine, and is common enough to
/// be worth naming: the lockfile no longer agrees with `package.json`. cairn installs only
/// what the lockfile pins, so it cannot resolve its way past it, and the fix is one command
/// in the project. Every manager reports it in its own words and buries the sentence that
/// matters — npm's first line is `npm error code EUSAGE`, which names nothing — so the
/// diagnosis is written here rather than quoted from them.
fn install_complaint(lock: &str, dir: &Path, stderr: &str, log: &Path) -> String {
    let where_ = if dir.as_os_str().is_empty() {
        "the repository root".to_string()
    } else {
        dir.display().to_string()
    };
    // Each manager's own wording for one condition, taken from a run of each rather than
    // from its documentation.
    let out_of_sync = [
        "ERR_PNPM_OUTDATED_LOCKFILE",
        "can only install packages when your package.json",
        "lockfile needs to be updated",
        "lockfile had changes, but lockfile is frozen",
    ]
    .iter()
    .any(|m| stderr.contains(m));

    if out_of_sync {
        let refresh = match lock {
            "pnpm-lock.yaml" => "pnpm install",
            "yarn.lock" => "yarn install",
            "bun.lock" | "bun.lockb" => "bun install",
            _ => "npm install",
        };
        return format!(
            "{lock} in {where_} is out of sync with its package.json, so installing from it \
             was refused. cairn installs only what a lockfile pins and never resolves a \
             version of its own. Run `{refresh}` there to bring the lockfile up to date, then \
             `cairn index` again (full output in {})",
            log.display()
        );
    }

    // Anything else: the first line that reads like a complaint, with the rest kept.
    let lines: Vec<&str> = stderr
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
    if complaint.is_empty() {
        format!(
            "installing from {lock} in {where_} failed (output in {})",
            log.display()
        )
    } else {
        format!(
            "installing from {lock} in {where_} failed: {complaint} (full output in {})",
            log.display()
        )
    }
}

/// The nearest directory at or above `root`, and never above `repo`, that commits a
/// lockfile.
///
/// Node resolution walks up and workspaces hoist to the repository root, so a member
/// legitimately has no lockfile of its own. The install belongs wherever the lockfile is,
/// which is the same place the package manager would put the tree.
pub fn lock_root(root: &Path, repo: &Path) -> Option<PathBuf> {
    root.ancestors()
        .take_while(|d| d.starts_with(repo) || *d == repo)
        .find(|d| install_command(d).is_some())
        .map(|d| d.to_path_buf())
}

/// Install one lockfile's dependencies inside the container, into the volume cairn owns.
///
/// Returns how long it took, or what to tell the user about why it did not happen. The
/// whole output is kept in a file either way: a diagnosis worth printing is one sentence,
/// and the hundred lines behind it are worth keeping but not worth showing.
pub fn install_dependencies(repo: &Path, lock_root: &Path, out_rel: &Path) -> Result<f64, String> {
    let rel = lock_root.strip_prefix(repo).unwrap_or(Path::new(""));
    let (lock, argv) = match install_command(lock_root).ok_or("no lockfile here")? {
        Install::Frozen { lock, argv } => (lock, argv),
        Install::Unsupported { lock, why } => {
            return Err(format!("{lock} in {}: {why}", rel.display()))
        }
    };
    let started = std::time::Instant::now();
    match crate::docker::exec(repo, lock_root, &argv) {
        Ok(o) if o.status.success() => Ok(started.elapsed().as_secs_f64()),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            let slug = rel.to_string_lossy().replace(['/', '\\'], "-");
            let name = if slug.is_empty() {
                "install.log".to_string()
            } else {
                format!("install-{slug}.log")
            };
            let log = repo.join(out_rel).join(name);
            let _ = std::fs::write(&log, err.as_bytes());
            Err(install_complaint(lock, rel, &err, &log))
        }
        Err(e) => Err(format!("installing dependencies: {e:#}")),
    }
}

/// Can anything be resolved from this project, seen from where the indexer stands?
///
/// Asked inside the container on purpose. Node walks `node_modules` upwards from the
/// project, and the directories cairn covers are volumes — so the host sees empty mount
/// points where the container sees the tree, and a check on the host would answer about a
/// filesystem no indexer reads.
fn dependencies_present(repo: &Path, root: &Path) -> bool {
    // Emptiness matters as much as existence: an empty `node_modules` is what a skipped
    // install leaves behind, and it is indistinguishable from a real one to a test for the
    // directory alone.
    let script = "p=$PWD; while [ \"$p\" != / ]; do \
                  if [ -n \"$(ls -A \"$p/node_modules\" 2>/dev/null | head -1)\" ]; then exit 0; fi; \
                  p=$(dirname \"$p\"); done; exit 1";
    crate::docker::exec(repo, root, &["sh", "-c", script])
        .map(|o| o.status.success())
        .unwrap_or(false)
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
pub fn run_indexer(found: &Found, root: &Path, tag: &str, repo: &Path, out_rel: &Path) -> Outcome {
    let started = std::time::Instant::now();
    let out_in_container = Path::new("/repo").join(out_rel).join(format!("{tag}.scip"));
    let out = out_in_container.to_string_lossy().to_string();

    let project;
    let args: Vec<&str> = match found.language.name {
        "go" => vec![found.language.indexer, "--output", &out],
        "python" => {
            // `--project-name` is required and ends up inside every symbol string, so it
            // has to be stable across runs: the directory name is, a timestamp is not.
            project = root
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
        "typescript" => {
            // Unlike the other two, this indexer cannot do anything at all without the
            // project's own dependencies on disk. A tsconfig that says
            // `extends: "expo/tsconfig.base"` is unreadable without them, and what comes
            // out is not a smaller index but `no files got indexed` — an empty index that
            // every later answer would report as a fact about the code. Refusing here,
            // naming the directory and the command, is the whole difference.
            // The install has already run, once per lockfile, before any of this. What is
            // left is to check it arrived — and to check it where the indexer will look,
            // which is inside the container. On the host the answer is now always yes and
            // always meaningless: every directory cairn covers has an empty `node_modules`
            // mount point sitting in it, and a test for one would pass on the exact tree
            // that produces `no files got indexed`.
            if !dependencies_present(repo, root) {
                return Outcome::Failed(format!(
                    "no dependencies are resolvable from {}, and scip-typescript resolves \
                     nothing without them - a tsconfig that extends a package cannot even be \
                     read, and the index would come out empty rather than partial. There is \
                     no lockfile at or above it for cairn to install from; commit one, or \
                     install the dependencies there, and run `cairn index` again",
                    root.display()
                ));
            }
            vec!["scip-typescript", "index", "--output", &out]
        }
        // Unlike the other three, this is rust-analyzer itself, not a wrapper built for
        // SCIP: there is no separate indexer binary to install or version apart from the
        // language server already in the image. `.` because the workdir the exec runs in
        // is already the chosen root (see `docker::exec`'s `--workdir`).
        "rust" => vec!["rust-analyzer", "scip", ".", "--output", &out],
        other => return Outcome::Failed(format!("no invocation known for {other}")),
    };

    let result = crate::docker::exec(repo, root, &args);
    let on_host = repo.join(out_rel).join(format!("{tag}.scip"));

    match result {
        Ok(o) if o.status.success() && on_host.exists() => {
            // Kept even on success, which it was not before. An indexer that exits 0 and
            // writes an index describing nothing is a case this code already knows about
            // and reports - and the reason was being thrown away with the output, because
            // only a failing run got a log. Measured: two Go modules of 102 files came out
            // empty on a real repository, and the sentence that explained it,
            // `go: updating go.sum: open /repo/go.work.sum: permission denied`, had been
            // discarded. That turned a two-minute diagnosis into twenty.
            //
            // Costs one small file per project per index, written where the failure path
            // already writes one.
            let log = on_host.with_extension("log");
            let _ = std::fs::write(&log, &o.stderr);
            Outcome::Indexed {
                scip: on_host,
                seconds: started.elapsed().as_secs_f64(),
            }
        }
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
            // cairn's own explanation wins when there is one. An indexer killed under the
            // memory cap complains about a heap limit; the fact the reader can act on is
            // the cap, and it is the one line here that cairn wrote itself.
            let complaint = lines
                .iter()
                .find(|l| l.starts_with(crate::docker::NOTE_PREFIX))
                .or_else(|| {
                    lines.iter().find(|l| {
                        let l = l.to_ascii_lowercase();
                        l.contains("error") || l.contains("failed") || l.contains("cannot")
                    })
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
        // Ruby rather than TypeScript or Rust: both of those used to be the example here
        // and both then became languages cairn indexes, each time turning the test from
        // checking the warning into checking nothing. The property is about *any*
        // unreadable language, so it needs an example that stays unreadable.
        let mut files = many("srcgo", "go", 20);
        files.extend(many("engine", "rb", 20));
        let dir = tree("unsupported", &refs(&files));
        let survey = scan(&dir).unwrap();
        assert_eq!(survey.found.len(), 1, "only Go can be indexed");
        assert_eq!(survey.unsupported.len(), 1);
        assert_eq!(survey.unsupported[0].0, "Ruby");
        assert_eq!(survey.unsupported[0].1, 20);
    }

    #[test]
    fn extensions_of_one_language_are_counted_together() {
        let mut files = many("srcgo", "go", 20);
        files.extend(many("engine", "c", 11));
        files.extend(many("engine", "h", 9));
        let dir = tree("grouped", &refs(&files));
        let survey = scan(&dir).unwrap();
        assert_eq!(survey.unsupported[0].0, "C");
        assert_eq!(survey.unsupported[0].1, 20, ".c and .h are one language");
    }

    #[test]
    fn a_typescript_tree_is_a_language_now_and_its_four_extensions_are_one() {
        // The counterpart of the two above, and the reason they had to change: .ts, .tsx,
        // .js and .jsx are one graph produced by one indexer, so they are one language
        // here rather than four rows a reader has to add up.
        let mut files = vec![("tsconfig.json".to_string(), "{}".to_string())];
        files.extend(many("app", "ts", 10));
        files.extend(many("app", "tsx", 8));
        files.extend(many("app", "js", 2));
        let dir = tree("ts-tree", &refs(&files));
        let survey = scan(&dir).unwrap();
        assert_eq!(survey.found.len(), 1);
        assert_eq!(survey.found[0].language.name, "typescript");
        assert_eq!(survey.found[0].files, 20);
        assert!(
            survey.unsupported.is_empty(),
            "TypeScript should no longer be reported as unreadable"
        );
    }

    #[test]
    fn every_go_module_is_its_own_root_nesting_included() {
        // This test asserted the opposite, on the grounds that "the root module sees
        // every package". Go's own rule is the reverse: a `go.mod` inside another
        // module's directory is *excluded* from it, so `services/inner` is a separate
        // compilation and indexing only the root leaves its ten files out of the index
        // with nothing saying so. Measured on a real monorepo, where it was 32 modules.
        let mut files = vec![
            ("go.mod".to_string(), "module x".to_string()),
            ("services/inner/go.mod".to_string(), "module y".to_string()),
        ];
        files.extend(many("services/inner", "go", 10));
        let dir = tree("shallowest", &refs(&files));
        let survey = scan(&dir).unwrap();
        assert_eq!(
            survey.found[0].roots,
            vec![dir.clone(), dir.join("services/inner")],
            "both modules have to be indexed, and in a fixed order"
        );
    }

    #[test]
    fn a_pnpm_workspace_excludes_what_it_says_it_excludes() {
        // The exclusion is the repository's own statement that a change there does
        // nothing. cairn used to index the retired app anyway: it installed its stale
        // lockfile, failed, warned that answers about that code were incomplete - about
        // code that was never in the build - and in an earlier measured round it put a
        // retired app's symbols in front of an agent whose task penalised naming them.
        let mut files = vec![
            (
                "pnpm-workspace.yaml".to_string(),
                "packages:\n  - \"apps/*\"\n  # retired\n  - \"!apps/old\"\n".to_string(),
            ),
            ("apps/live/tsconfig.json".to_string(), "{}".to_string()),
            ("apps/old/tsconfig.json".to_string(), "{}".to_string()),
        ];
        files.extend(many("apps/live/src", "ts", 12));
        files.extend(many("apps/old/src", "ts", 12));
        let dir = tree("pnpm-workspace", &refs(&files));
        let survey = scan(&dir).unwrap();
        assert_eq!(
            survey.found[0].roots,
            vec![dir.join("apps/live")],
            "the excluded app is not a project to index"
        );
    }

    #[test]
    fn npm_workspaces_in_package_json_are_read_too() {
        let mut files = vec![
            (
                "package.json".to_string(),
                "{\"workspaces\": [\"packages/*\"]}".to_string(),
            ),
            ("packages/a/tsconfig.json".to_string(), "{}".to_string()),
            ("tools/scripts/tsconfig.json".to_string(), "{}".to_string()),
        ];
        files.extend(many("packages/a/src", "ts", 10));
        files.extend(many("tools/scripts", "ts", 6));
        let dir = tree("npm-workspaces", &refs(&files));
        let survey = scan(&dir).unwrap();
        assert_eq!(
            survey.found[0].roots,
            vec![dir.join("packages/a")],
            "a tsconfig outside every workspace member is not a project of this workspace"
        );
    }

    #[test]
    fn a_go_workspace_names_its_own_modules() {
        // `go.work` is authoritative where it exists: it lists the modules that build
        // together, including generated ones a walk would have to guess about. The
        // module it names but that is not on disk is dropped - that is the state of a
        // fresh checkout before codegen has run, and pointing an indexer at a directory
        // that is not there produces an error instead of an answer.
        let mut files = vec![
            (
                "go.work".to_string(),
                "go 1.26\n\nuse (\n\t./src\n\t./gen\n\t./not-generated-yet\n)\n".to_string(),
            ),
            ("src/go.mod".to_string(), "module x".to_string()),
            ("gen/go.mod".to_string(), "module y".to_string()),
        ];
        files.extend(many("src", "go", 8));
        files.extend(many("gen", "go", 4));
        let dir = tree("go-workspace", &refs(&files));
        let survey = scan(&dir).unwrap();
        assert_eq!(
            survey.found[0].roots,
            vec![dir.join("gen"), dir.join("src")],
            "the workspace's own list, minus what is not on disk"
        );
    }

    #[test]
    fn a_cargo_workspace_absorbs_its_own_members() {
        // Unlike Go, a Cargo workspace resolves every member it names in the one
        // `cargo metadata` pass the indexer makes from the workspace root - indexing a
        // member separately would be the same project counted twice, not a second one.
        let mut files = vec![
            (
                "Cargo.toml".to_string(),
                "[workspace]\nmembers = [\"crates/*\"]\n".to_string(),
            ),
            ("crates/a/Cargo.toml".to_string(), "[package]\n".to_string()),
            ("crates/b/Cargo.toml".to_string(), "[package]\n".to_string()),
        ];
        files.extend(many("crates/a/src", "rs", 10));
        files.extend(many("crates/b/src", "rs", 10));
        let dir = tree("cargo-workspace", &refs(&files));
        let roots = &scan(&dir).unwrap().found[0].roots;
        assert_eq!(
            roots,
            &vec![dir.clone()],
            "one run at the workspace root, not one per member Cargo.toml too"
        );
    }

    #[test]
    fn a_cargo_crate_nobody_claims_is_its_own_root() {
        // No workspace at all: two unrelated crates, each indexed on its own.
        let mut files = vec![
            (
                "tools/cli/Cargo.toml".to_string(),
                "[package]\n".to_string(),
            ),
            ("engine/Cargo.toml".to_string(), "[package]\n".to_string()),
        ];
        files.extend(many("tools/cli/src", "rs", 10));
        files.extend(many("engine/src", "rs", 10));
        let dir = tree("cargo-standalone", &refs(&files));
        let roots = &scan(&dir).unwrap().found[0].roots;
        assert_eq!(
            roots,
            &vec![dir.join("engine"), dir.join("tools/cli")],
            "both, in a fixed order"
        );
    }

    #[test]
    fn a_crate_the_workspace_does_not_list_is_still_indexed() {
        // The Go lesson (33 modules, one indexed) read onto Cargo: a crate nested under
        // the workspace root that the workspace's own `members` does not name - an
        // example checked in by hand, say - is not swallowed by the root just because it
        // is inside it.
        let mut files = vec![
            (
                "Cargo.toml".to_string(),
                "[workspace]\nmembers = [\"crates/a\"]\n".to_string(),
            ),
            (
                "crates/not-a-member/Cargo.toml".to_string(),
                "[package]\n".to_string(),
            ),
        ];
        files.extend(many("crates/a/src", "rs", 10));
        files.extend(many("crates/not-a-member/src", "rs", 10));
        let dir = tree("cargo-unclaimed", &refs(&files));
        let roots = &scan(&dir).unwrap().found[0].roots;
        assert_eq!(
            roots,
            &vec![dir.clone(), dir.join("crates/not-a-member")],
            "the workspace root, and the crate it never claimed"
        );
    }

    #[test]
    fn cargo_exclude_drops_a_member_the_way_pnpm_does() {
        let mut files = vec![
            (
                "Cargo.toml".to_string(),
                "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/experimental\"]\n"
                    .to_string(),
            ),
            ("crates/a/Cargo.toml".to_string(), "[package]\n".to_string()),
            (
                "crates/experimental/Cargo.toml".to_string(),
                "[package]\n".to_string(),
            ),
        ];
        files.extend(many("crates/a/src", "rs", 10));
        files.extend(many("crates/experimental/src", "rs", 10));
        let dir = tree("cargo-exclude", &refs(&files));
        let roots = &scan(&dir).unwrap().found[0].roots;
        assert_eq!(
            roots,
            &vec![dir.clone(), dir.join("crates/experimental")],
            "excluded from the workspace, so it is not absorbed - it is still its own crate"
        );
    }

    #[test]
    fn a_mixed_manifests_own_publish_exclude_is_not_workspace_exclude() {
        // Modelled on ripgrep's own root `Cargo.toml`: `[package]` and `[workspace]` in
        // the same file, and `[package]`'s own `exclude` is `cargo publish`'s list of
        // what not to ship - nothing to do with workspace membership, and it happens to
        // name a directory ("grep") that genuinely *is* a declared member here.
        // Reading the whole file for a key named `exclude` without checking which table
        // it is in would read this one by mistake and silently drop a real member.
        //
        // No leading `/` on the path, unlike ripgrep's own ("/crates/fuzz"): with one,
        // `expand_members` would join it onto the repo root as an absolute path, find
        // nothing there, and mask the bug by accident - the point here is the table, not
        // that coincidence.
        let mut files = vec![(
            "Cargo.toml".to_string(),
            "[package]\n\
             name = \"ripgrep\"\n\
             exclude = [\n  \"scripts/\",\n  \"crates/grep\",\n]\n\
             \n\
             [workspace]\n\
             members = [\"crates/grep\", \"crates/fuzz\"]\n\
             \n\
             [workspace.package]\n\
             edition = \"2021\"\n"
                .to_string(),
        )];
        files.push((
            "crates/fuzz/Cargo.toml".to_string(),
            "[package]\n".to_string(),
        ));
        files.push((
            "crates/grep/Cargo.toml".to_string(),
            "[package]\n".to_string(),
        ));
        files.extend(many("crates/fuzz/src", "rs", 10));
        files.extend(many("crates/grep/src", "rs", 10));
        let dir = tree("mixed-manifest", &refs(&files));
        let roots = &scan(&dir).unwrap().found[0].roots;
        assert_eq!(
            roots,
            &vec![dir.clone()],
            "crates/grep is a declared member - `[package] exclude` naming the same path \
             for an unrelated reason (cargo publish) must not pull it back out"
        );
    }

    #[test]
    fn without_a_marker_the_indexer_runs_at_the_repository_root() {
        let dir = tree("no-marker", &refs(&many("pkg", "go", 10)));
        assert_eq!(scan(&dir).unwrap().found[0].roots, vec![dir]);
    }

    #[test]
    fn every_workspace_member_is_a_root_of_its_own() {
        // Go and Python nest: the outer project compiles the inner one. A JavaScript
        // workspace does not, and taking the shallowest marker there indexed one app of
        // three - picked by directory order, so a second run could pick another - while
        // reporting an index of the repository.
        let mut files = vec![
            ("package.json".to_string(), "{}".to_string()),
            ("apps/live/tsconfig.json".to_string(), "{}".to_string()),
            ("apps/retired/tsconfig.json".to_string(), "{}".to_string()),
        ];
        files.extend(many("apps/live", "ts", 20));
        files.extend(many("apps/retired", "ts", 10));
        let dir = tree("workspace", &refs(&files));
        let roots = &scan(&dir).unwrap().found[0].roots;
        assert_eq!(
            roots,
            &vec![dir.join("apps/live"), dir.join("apps/retired")],
            "both members, in a fixed order"
        );
    }

    #[test]
    fn a_nested_tsconfig_is_part_of_its_project_not_a_second_one() {
        let mut files = vec![
            ("tsconfig.json".to_string(), "{}".to_string()),
            ("src/feature/tsconfig.json".to_string(), "{}".to_string()),
        ];
        files.extend(many("src", "ts", 20));
        let dir = tree("nested-ts", &refs(&files));
        assert_eq!(scan(&dir).unwrap().found[0].roots, vec![dir]);
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

    #[test]
    fn every_package_json_directory_is_collected_not_only_the_projects() {
        // The workspace root has no tsconfig and is indexed as nothing, but a workspace
        // install writes a `node_modules` there all the same. Missing it put 1.3 GB into a
        // working copy, so the root has to appear here even though no indexer runs in it.
        let mut files = vec![
            ("package.json".to_string(), "{}".to_string()),
            ("apps/live/package.json".to_string(), "{}".to_string()),
            ("apps/live/tsconfig.json".to_string(), "{}".to_string()),
            ("packages/ui/package.json".to_string(), "{}".to_string()),
        ];
        files.extend(many("apps/live", "ts", 20));
        let dir = tree("package-dirs", &refs(&files));
        let survey = scan(&dir).unwrap();
        assert_eq!(
            survey.package_dirs,
            vec![
                PathBuf::from(""),
                PathBuf::from("apps/live"),
                PathBuf::from("packages/ui"),
            ],
            "the root and every member, relative and sorted"
        );
    }

    #[test]
    fn a_lockfile_inside_node_modules_is_not_a_project() {
        let mut files = many(".", "ts", 10);
        files.push(("tsconfig.json".to_string(), "{}".to_string()));
        files.push(("package.json".to_string(), "{}".to_string()));
        files.push((
            "node_modules/dep/package.json".to_string(),
            "{}".to_string(),
        ));
        let dir = tree("package-dirs-noise", &refs(&files));
        assert_eq!(scan(&dir).unwrap().package_dirs, vec![PathBuf::from("")]);
    }

    #[test]
    fn the_lockfile_picks_the_manager_and_the_flags_freeze_it() {
        let dir = tree("lock-pnpm", &[("pnpm-lock.yaml", "lockfileVersion: '9.0'")]);
        let Some(Install::Frozen { lock, argv }) = install_command(&dir) else {
            panic!("pnpm's lockfile should be installable");
        };
        assert_eq!(lock, "pnpm-lock.yaml");
        assert_eq!(argv[0], "pnpm");
        assert!(argv.contains(&"--frozen-lockfile"));

        let dir = tree("lock-npm", &[("package-lock.json", "{}")]);
        let Some(Install::Frozen { lock, argv }) = install_command(&dir) else {
            panic!("npm's lockfile should be installable");
        };
        assert_eq!(lock, "package-lock.json");
        assert_eq!(argv[..2], ["npm", "ci"], "not `npm install`");

        let dir = tree("lock-bun", &[("bun.lock", "{}")]);
        let Some(Install::Frozen { argv, .. }) = install_command(&dir) else {
            panic!("bun's lockfile should be installable");
        };
        assert_eq!(argv[0], "bun");

        // Nothing to install from is not a failure: a workspace member's dependencies are
        // resolved from the lockfile at the root, which the caller looks for next.
        let dir = tree("lock-none", &[("package.json", "{}")]);
        assert!(install_command(&dir).is_none());
    }

    #[test]
    fn a_berry_lockfile_is_refused_rather_than_handed_to_yarn_1() {
        // Yarn 1 does not reject a Yarn 2+ lockfile, and does not reject `--immutable`
        // either: it ignores the flag it does not know, resolves fresh versions and writes
        // the lockfile back. Measured. Refusing is the only behaviour here that leaves the
        // working tree as it was found.
        let berry =
            "# This file is generated by running \"yarn install\"\n\n__metadata:\n  version: 6\n";
        let dir = tree("lock-berry", &[("yarn.lock", berry)]);
        let Some(Install::Unsupported { lock, why }) = install_command(&dir) else {
            panic!("a Berry lockfile must not be installed by Yarn 1");
        };
        assert_eq!(lock, "yarn.lock");
        assert!(why.contains("Yarn 2+"), "the reason has to name the format");

        let classic = "# THIS IS AN AUTOGENERATED FILE. DO NOT EDIT THIS FILE DIRECTLY.\n# yarn lockfile v1\n";
        let dir = tree("lock-classic", &[("yarn.lock", classic)]);
        let Some(Install::Frozen { argv, .. }) = install_command(&dir) else {
            panic!("a Yarn 1 lockfile is installable");
        };
        assert!(
            argv.contains(&"--frozen-lockfile") && !argv.contains(&"--immutable"),
            "the flag Yarn 1 actually honours, or it saves the lockfile: {argv:?}"
        );
    }

    #[test]
    fn a_lockfile_out_of_sync_is_named_as_that_and_not_as_an_error_code() {
        // One line of each manager's own output, from a run of each.
        let cases = [
            (
                "package-lock.json",
                "npm error code EUSAGE\nnpm error `npm ci` can only install packages when your \
                 package.json and package-lock.json are in sync.",
                "npm install",
            ),
            (
                "pnpm-lock.yaml",
                "[ERR_PNPM_OUTDATED_LOCKFILE] Cannot install with \"frozen-lockfile\"",
                "pnpm install",
            ),
            (
                "yarn.lock",
                "error Your lockfile needs to be updated, but yarn was run with \
                 `--frozen-lockfile`.",
                "yarn install",
            ),
            (
                "bun.lock",
                "error: lockfile had changes, but lockfile is frozen",
                "bun install",
            ),
        ];
        for (lock, stderr, refresh) in cases {
            let said = install_complaint(lock, Path::new("apps/legacy"), stderr, Path::new("/l"));
            assert!(said.contains("out of sync"), "{lock}: {said}");
            assert!(said.contains("apps/legacy"), "which project: {said}");
            assert!(said.contains(refresh), "what to run about it: {said}");
            assert!(
                !said.contains("EUSAGE") && !said.contains("ERR_PNPM"),
                "the code is in the log, not in the sentence: {said}"
            );
        }
    }

    #[test]
    fn an_install_failure_that_is_not_the_lockfile_still_says_something() {
        let said = install_complaint(
            "package-lock.json",
            Path::new(""),
            "npm warn deprecated x\nnpm error network ETIMEDOUT\nnpm error at Socket",
            Path::new("/tmp/i.log"),
        );
        assert!(said.contains("ETIMEDOUT"), "the complaint itself: {said}");
        assert!(said.contains("the repository root"), "where: {said}");
        assert!(said.contains("/tmp/i.log"), "and the rest is kept: {said}");
    }
}
