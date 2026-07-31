//! Reading source text for the `--detail` axis.
//!
//! Printing bodies is the audit workflow (architecture 18.1): a reviewer looking for
//! edge cases, missing conventions, security holes or hot loops needs the code, not a
//! list of names. It is off by default because it is the most expensive thing this tool
//! can emit — which is also why it is the setting where `--budget` matters most.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// How much of each symbol to print.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// Identity only: handle, name, kind, location. The default everywhere.
    Skeleton,
    /// The definition line, so a signature is visible without its body.
    Signature,
    /// The comment block immediately above the definition.
    Doc,
    /// The whole definition, from its first line to the end of its enclosing range.
    Body,
}

impl Detail {
    pub fn parse(s: &str) -> Option<Detail> {
        match s {
            "skeleton" => Some(Detail::Skeleton),
            "signature" => Some(Detail::Signature),
            "doc" => Some(Detail::Doc),
            "body" => Some(Detail::Body),
            _ => None,
        }
    }

    pub fn needs_source(self) -> bool {
        !matches!(self, Detail::Skeleton)
    }
}

/// Caps the size of one symbol's body.
///
/// Without this a single long function swallows the whole budget and the walk stops
/// after one node, which is the opposite of what a reviewer asked for. The cut is
/// reported per symbol, so nothing disappears silently (D8).
pub const MAX_BODY_LINES: usize = 120;

/// Reads files once and keeps them, because a walk revisits the same file repeatedly.
pub struct Source {
    root: PathBuf,
    cache: HashMap<String, Option<Vec<String>>>,
}

impl Source {
    pub fn new(root: impl Into<PathBuf>) -> Source {
        Source { root: root.into(), cache: HashMap::new() }
    }

    fn lines(&mut self, rel_path: &str) -> Option<&Vec<String>> {
        if !self.cache.contains_key(rel_path) {
            let full = self.root.join(rel_path);
            let loaded = std::fs::read_to_string(&full)
                .ok()
                .map(|t| t.lines().map(|l| l.to_string()).collect());
            self.cache.insert(rel_path.to_string(), loaded);
        }
        self.cache.get(rel_path).and_then(|v| v.as_ref())
    }

    /// Render one symbol at the requested detail.
    ///
    /// Returns the lines to print plus a note when something was cut or unavailable —
    /// the caller folds that into `unknown:`/`suppressed:` rather than dropping it.
    pub fn excerpt(
        &mut self,
        rel_path: &str,
        start_line: i64,
        end_line: Option<i64>,
        detail: Detail,
    ) -> Excerpt {
        let Some(lines) = self.lines(rel_path) else {
            return Excerpt {
                lines: Vec::new(),
                note: Some(format!("could not read {rel_path}")),
            };
        };
        let start = start_line.max(0) as usize;
        if start >= lines.len() {
            return Excerpt {
                lines: Vec::new(),
                note: Some(format!("{rel_path}:{} is past end of file", start_line + 1)),
            };
        }

        match detail {
            Detail::Skeleton => Excerpt { lines: Vec::new(), note: None },
            Detail::Signature => Excerpt {
                lines: vec![(start + 1, lines[start].clone())],
                note: None,
            },
            Detail::Doc => {
                // Walk back over the contiguous comment block above the definition.
                // Language-neutral on purpose: a leading `#`, `//`, `*` or `"""` all
                // count, and getting it slightly wrong costs a line, not a fact.
                let mut first = start;
                while first > 0 {
                    let prev = lines[first - 1].trim_start();
                    let is_comment = prev.starts_with('#')
                        || prev.starts_with("//")
                        || prev.starts_with('*')
                        || prev.starts_with("/*")
                        || prev.starts_with("\"\"\"")
                        || prev.starts_with("'''");
                    if !is_comment {
                        break;
                    }
                    first -= 1;
                }
                if first == start {
                    return Excerpt {
                        lines: Vec::new(),
                        note: Some("no comment block above the definition".into()),
                    };
                }
                Excerpt {
                    lines: (first..start).map(|i| (i + 1, lines[i].clone())).collect(),
                    note: None,
                }
            }
            Detail::Body => {
                let Some(end) = end_line else {
                    return Excerpt {
                        lines: vec![(start + 1, lines[start].clone())],
                        note: Some(
                            "indexer gave no body extent; showing the definition line only"
                                .into(),
                        ),
                    };
                };
                let end = (end as usize).min(lines.len().saturating_sub(1));
                let full_len = end.saturating_sub(start) + 1;
                let cut = full_len > MAX_BODY_LINES;
                let last = if cut { start + MAX_BODY_LINES - 1 } else { end };
                Excerpt {
                    lines: (start..=last.min(lines.len() - 1))
                        .map(|i| (i + 1, lines[i].clone()))
                        .collect(),
                    note: cut.then(|| {
                        format!(
                            "body truncated at {MAX_BODY_LINES} lines ({} more, to {rel_path}:{})",
                            full_len - MAX_BODY_LINES,
                            end + 1
                        )
                    }),
                }
            }
        }
    }
}

pub struct Excerpt {
    /// `(1-based line number, text)`
    pub lines: Vec<(usize, String)>,
    pub note: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each test gets its own directory: they run in parallel, and a shared path
    // meant one test's Drop deleted the fixture another was still reading.
    fn fixture(tag: &str) -> (tempdirlike::Dir, Source) {
        let dir = tempdirlike::Dir::new(tag);
        std::fs::write(
            dir.path().join("a.py"),
            "import x\n# leading doc\n# second line\ndef f():\n    return 1\n\ndef g():\n    pass\n",
        )
        .unwrap();
        let src = Source::new(dir.path());
        (dir, src)
    }

    #[test]
    fn signature_is_one_line() {
        let (_d, mut s) = fixture("signature_is_one_line");
        let e = s.excerpt("a.py", 3, Some(4), Detail::Signature);
        assert_eq!(e.lines.len(), 1);
        assert_eq!(e.lines[0].0, 4);
        assert!(e.lines[0].1.contains("def f()"));
    }

    #[test]
    fn body_spans_the_enclosing_range() {
        let (_d, mut s) = fixture("body_spans_the_enclosing_range");
        let e = s.excerpt("a.py", 3, Some(4), Detail::Body);
        assert_eq!(e.lines.len(), 2);
        assert!(e.note.is_none());
    }

    #[test]
    fn doc_collects_the_comment_block_above() {
        let (_d, mut s) = fixture("doc_collects_the_comment_block_above");
        let e = s.excerpt("a.py", 3, Some(4), Detail::Doc);
        assert_eq!(e.lines.len(), 2);
        assert!(e.lines[0].1.contains("leading doc"));
    }

    #[test]
    fn missing_extent_says_so_rather_than_guessing() {
        let (_d, mut s) = fixture("missing_extent_says_so_rather_than_guessing");
        let e = s.excerpt("a.py", 3, None, Detail::Body);
        assert_eq!(e.lines.len(), 1);
        assert!(e.note.as_deref().unwrap().contains("no body extent"));
    }

    #[test]
    fn unreadable_file_is_reported() {
        let (_d, mut s) = fixture("unreadable_file_is_reported");
        let e = s.excerpt("nope.py", 0, Some(1), Detail::Body);
        assert!(e.lines.is_empty());
        assert!(e.note.is_some());
    }

    /// Minimal scratch directory helper - avoids a dev-dependency for four tests.
    mod tempdirlike {
        use std::path::{Path, PathBuf};
        pub struct Dir(PathBuf);
        impl Dir {
            pub fn new(tag: &str) -> Dir {
                let p = std::env::temp_dir()
                    .join(format!("cairn-src-{tag}-{}", std::process::id()));
                let _ = std::fs::create_dir_all(&p);
                Dir(p)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
