//! Walking a working tree, once, in one place.
//!
//! Three passes already read the tree for different reasons — the markdown map, the
//! literal scan, and now a connector's declared inputs — and each had its own idea of
//! what to skip. That is the shape of defect this project keeps finding: a list written
//! when there was one caller, used as though it were the only one. One walker, one skip
//! list, and the limits as parameters so a caller that wants a shallow pass can have one
//! without forking the traversal.

use std::path::{Path, PathBuf};

/// Directories that hold nothing worth reading and cost a lot to walk.
///
/// Dot-directories are skipped wholesale besides these, which is why `.cairn` itself
/// never appears in a walk: an index is not an input to anything.
pub const SKIP: &[&str] = &[
    ".git",
    "node_modules",
    ".venv",
    "venv",
    "target",
    "dist",
    "build",
    "__pycache__",
    "site-packages",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
];

/// How far a walk may go. Both limits exist so a pathological tree cannot turn a query
/// into a filesystem crawl.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_depth: usize,
    pub max_files: usize,
}

/// Every file under `root` the filter keeps, as repo-relative paths with `/` separators.
///
/// Relative and slash-separated because that is how the index stores a path, and a
/// caller that had to normalise one would eventually normalise it differently from the
/// ingest — which is how a staleness check compares two spellings of the same file and
/// reports a change nobody made.
pub fn walk(root: &Path, limits: Limits, keep: &mut dyn FnMut(&str) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_in(root, root, 0, limits, keep, &mut out);
    out.sort();
    out
}

fn walk_in(
    root: &Path,
    dir: &Path,
    depth: usize,
    limits: Limits,
    keep: &mut dyn FnMut(&str) -> bool,
    out: &mut Vec<PathBuf>,
) {
    if depth > limits.max_depth || out.len() >= limits.max_files {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    // Sorted, so two walks of the same tree produce the same order. `read_dir` is in
    // whatever order the filesystem offers, and every number derived from a walk — a
    // cache key, a count in an answer — has to be the same twice.
    let mut names: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    names.sort();
    for p in names {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if p.is_dir() {
            if SKIP.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            walk_in(root, &p, depth + 1, limits, keep, out);
        } else {
            let Ok(rel) = p.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if keep(&rel) {
                out.push(PathBuf::from(rel));
            }
        }
        if out.len() >= limits.max_files {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_walk_is_relative_sorted_and_skips_what_it_says_it_skips() {
        let dir = std::env::temp_dir().join("cairn-tree-walk");
        let _ = std::fs::remove_dir_all(&dir);
        for (path, text) in [
            ("b.proto", "b"),
            ("a.proto", "a"),
            ("sub/c.proto", "c"),
            (".git/d.proto", "d"),
            ("node_modules/e.proto", "e"),
        ] {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, text).unwrap();
        }
        let found = walk(
            &dir,
            Limits {
                max_depth: 8,
                max_files: 100,
            },
            &mut |p| p.ends_with(".proto"),
        );
        let found: Vec<String> = found
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert_eq!(found, vec!["a.proto", "b.proto", "sub/c.proto"]);
    }
}
