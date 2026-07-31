//! Weak links: references static analysis cannot see (architecture 18.4, layer L1-W).
//!
//! The mechanism is deliberately dumb: **every string literal in the repo that exactly
//! matches the name of a symbol is a candidate dynamic reference.** That one join
//! covers `getattr`, `importlib`, plugin registries, string-keyed routing, DI keys and
//! serialisation maps — a large share of the dynamic call sites the coverage analysis
//! found — and it needs no language knowledge, no LLM (D15) and no new index.
//!
//! Everything it produces is a *candidate*. These edges carry `EdgeSource::Weak`, are
//! reported under their own heading, and never mix with exact ones. The failure mode to
//! avoid is not false positives as such — it is producing so many that the agent learns
//! to skip the section, which would also lose the true ones. Hence the guards below.

use crate::{EdgeKind, EdgeSource, Store};
use anyhow::Result;
use rusqlite::params;
use rusqlite::types::Value;
use std::collections::HashMap;
use std::path::Path;

/// Literals shorter than this are almost always English words, keys or format
/// fragments rather than symbol references.
const MIN_LITERAL_LEN: usize = 4;

/// A name shared by more than this many distinct symbols carries no information —
/// matching it would point at everything. `handler`, `get`, `run` land here.
const MAX_SYMBOL_HOMONYMS: i64 = 8;

/// Cap per file, so one generated table of strings cannot dominate the whole layer.
const MAX_HITS_PER_FILE: usize = 50;

/// Only these kinds are worth matching, and the narrowing was driven by measurement
/// on the target repo rather than taste:
///
/// * with every kind allowed: 7,716 candidates, mostly Go import paths matching
///   *namespace* symbols. An import is not a dynamic reference and SCIP resolves it
///   exactly already.
/// * with namespaces dropped: 7,197, now dominated by *terms* - enum values and struct
///   fields appearing as serialisation keys (`NEGATIVE`, `amount_czk`). Real string
///   matches, but not dispatch, which is what this layer is for.
///
/// So: types and methods only. Serialisation-key linking is a separate, later idea
/// with a different contract; conflating the two is what made the layer unusable.
const MATCHABLE_KINDS: [i64; 2] = [
    1, // Type
    3, // Method
];

/// Is this name distinctive enough that a literal spelling it is likely to *mean* it?
///
/// Short lowercase names collide with ordinary English and with keys that happen to
/// share a word: `json`, `post`, `context`, `financial` all matched real symbols and
/// all were noise. Anything with an underscore, internal capitals, or real length is
/// unlikely to appear by coincidence.
fn is_distinctive(name: &str) -> bool {
    if name.len() >= 12 {
        return true;
    }
    if name.contains('_') {
        return true;
    }
    let mut chars = name.chars();
    let first_upper = chars.next().map(|c| c.is_uppercase()).unwrap_or(false);
    let has_inner_upper = chars.any(|c| c.is_uppercase());
    // PascalCase or camelCase with a hump; a single leading capital is not enough
    // (`Order`, `Elevator` are as ambiguous as their lowercase forms).
    has_inner_upper || (first_upper && has_inner_upper)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct WeakStats {
    pub files_scanned: usize,
    pub literals_seen: usize,
    pub candidates: usize,
    pub skipped_common: usize,
}

/// Scan the repo for string literals matching symbol names and record weak edges.
///
/// `repo_root` must be the workspace root; file paths in the store are relative to it.
pub fn derive_weak_links(store: &mut Store, repo_root: &Path) -> Result<WeakStats> {
    let mut stats = WeakStats::default();

    debug_assert_eq!(MATCHABLE_KINDS.len(), 2, "kind filter below is inlined in SQL");
    // Name -> symbol id, for names that are distinctive enough to be worth matching.
    let mut by_name: HashMap<String, i64> = HashMap::new();
    {
        let mut stmt = store.conn.prepare(
            r#"
            SELECT n.s, min(s.id), count(*)
              FROM symbols s
              JOIN strings n ON n.id = s.name_id
             WHERE length(n.s) >= ?1
               AND s.kind IN (1, 3)
             GROUP BY n.s
            HAVING count(*) <= ?2
            "#,
        )?;
        let rows = stmt.query_map(params![MIN_LITERAL_LEN as i64, MAX_SYMBOL_HOMONYMS], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (name, id) = row?;
            if !is_distinctive(&name) {
                stats.skipped_common += 1;
                continue;
            }
            by_name.insert(name, id);
        }
    }

    // Files to scan, with the symbol that encloses each hit resolved afterwards.
    let files: Vec<(i64, String)> = {
        let mut stmt = store
            .conn
            .prepare("SELECT f.id, p.s FROM files f JOIN strings p ON p.id = f.path_id WHERE f.generated = 0")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        out
    };

    let tx = store.conn.transaction()?;
    {
        let mut batch = crate::BatchWriter::new(
            "edges",
            &["src_symbol", "dst_symbol", "kind", "source", "confidence", "file_id", "line"],
        );
        // The weak layer is rebuilt wholesale; it is derived, never authored.
        tx.execute("DELETE FROM edges WHERE source = ?1", params![EdgeSource::Weak as i64])?;

        for (file_id, rel_path) in &files {
            let full = repo_root.join(rel_path);
            let Ok(text) = std::fs::read_to_string(&full) else {
                continue;
            };
            stats.files_scanned += 1;
            let mut hits = 0usize;
            for (line, literal) in string_literals(&text, rel_path) {
                stats.literals_seen += 1;
                if literal.len() < MIN_LITERAL_LEN {
                    continue;
                }
                // Match the whole literal, or its last dotted/slashed segment, so
                // "auth.TokenValidator" and "plugins/TokenValidator" both land.
                let tail = literal
                    .rsplit(['.', '/', ':'])
                    .next()
                    .unwrap_or(&literal);
                let Some(&target) = by_name.get(&literal).or_else(|| by_name.get(tail)) else {
                    continue;
                };
                if hits >= MAX_HITS_PER_FILE {
                    break;
                }
                hits += 1;
                stats.candidates += 1;
                batch.push(
                    &tx,
                    vec![
                        Value::Null, // no known source symbol: the site is what matters
                        Value::from(target),
                        Value::from(EdgeKind::WeakRef as i64),
                        Value::from(EdgeSource::Weak as i64),
                        Value::from(0.3f64),
                        Value::from(*file_id),
                        Value::from(line),
                    ],
                )?;
            }
        }
        batch.finish(&tx)?;
    }
    tx.commit()?;
    Ok(stats)
}

/// Extract string literals with their 0-based line numbers.
///
/// A deliberately small scanner rather than a parser: it only has to find quoted runs
/// and avoid comments, and getting that slightly wrong costs a candidate, not a fact.
/// Anything that needs real parsing belongs in the tree-sitter path (architecture 4.5).
fn string_literals(text: &str, path: &str) -> Vec<(i64, String)> {
    let hash_comments = !path.ends_with(".go") && !path.ends_with(".ts") && !path.ends_with(".js");
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut line = 0i64;

    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'\n' => {
                line += 1;
                i += 1;
            }
            b'#' if hash_comments => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if !hash_comments && i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if !hash_comments && i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    if bytes[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            b'"' | b'\'' | b'`' => {
                let quote = c;
                let start_line = line;
                // Python triple quotes: treat as one literal, but their content is
                // usually prose, so only the first line is worth matching.
                let triple = (quote == b'"' || quote == b'\'')
                    && i + 2 < bytes.len()
                    && bytes[i + 1] == quote
                    && bytes[i + 2] == quote;
                let delim_len = if triple { 3 } else { 1 };
                i += delim_len;
                let start = i;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'\n' {
                        line += 1;
                        if !triple && quote != b'`' {
                            break; // unterminated single-line string
                        }
                    }
                    if bytes[i] == quote {
                        if !triple {
                            break;
                        }
                        if i + 2 < bytes.len() && bytes[i + 1] == quote && bytes[i + 2] == quote {
                            break;
                        }
                    }
                    i += 1;
                }
                if start < i && i <= bytes.len() {
                    if let Ok(s) = std::str::from_utf8(&bytes[start..i.min(bytes.len())]) {
                        let first = s.lines().next().unwrap_or("");
                        if !first.is_empty() && first.len() <= 200 {
                            out.push((start_line, first.to_string()));
                        }
                    }
                }
                i = (i + delim_len).min(bytes.len());
            }
            _ => i += 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lits(text: &str, path: &str) -> Vec<String> {
        string_literals(text, path).into_iter().map(|(_, s)| s).collect()
    }

    #[test]
    fn distinctiveness_keeps_identifiers_and_drops_words() {
        assert!(is_distinctive("TokenValidator"));
        assert!(is_distinctive("AuthServiceHandler"));
        assert!(is_distinctive("_get_client"));
        assert!(is_distinctive("order_advisor"));
        // These all matched real symbols on the target repo and were all noise.
        assert!(!is_distinctive("json"));
        assert!(!is_distinctive("post"));
        assert!(!is_distinctive("context"));
        assert!(!is_distinctive("Order"));
        assert!(!is_distinctive("Elevator"));
    }

    #[test]
    fn finds_python_literals_and_skips_comments() {
        let src = "# \"NotALiteral\"\nx = \"TokenValidator\"\ny = 'AuthHandler'\n";
        let got = lits(src, "a.py");
        assert!(got.contains(&"TokenValidator".to_string()));
        assert!(got.contains(&"AuthHandler".to_string()));
        assert!(!got.contains(&"NotALiteral".to_string()));
    }

    #[test]
    fn finds_go_literals_and_skips_comments() {
        let src = "// \"Nope\"\nv := \"OrderService\"\nw := `RawLiteral`\n";
        let got = lits(src, "a.go");
        assert!(got.contains(&"OrderService".to_string()));
        assert!(got.contains(&"RawLiteral".to_string()));
        assert!(!got.contains(&"Nope".to_string()));
    }

    #[test]
    fn records_the_right_line() {
        let hits = string_literals("a = 1\nb = \"Target\"\n", "a.py");
        assert_eq!(hits.iter().find(|(_, s)| s == "Target").unwrap().0, 1);
    }

    #[test]
    fn hash_is_not_a_comment_in_go() {
        // `#` appears inside Go strings and must not start a comment there.
        let got = lits("s := \"a#b\"\nt := \"Wanted\"\n", "a.go");
        assert!(got.contains(&"Wanted".to_string()));
    }
}
