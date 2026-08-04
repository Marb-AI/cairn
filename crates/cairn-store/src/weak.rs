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

/// A literal, and whose code it sits in.
pub struct LiteralSite {
    pub text: String,
    pub path: String,
    pub line: i64,
    /// The symbol whose definition contains the line, when one does.
    pub enclosing: Option<crate::SymbolRow>,
}

#[derive(Debug, Default)]
pub struct LiteralStats {
    pub files: usize,
    pub literals: usize,
    pub attributed: usize,
}

/// Record every string literal, with the symbol whose body it sits in.
///
/// Run at index time, unlike `derive_weak_links`, which is a separate command and
/// therefore usually never runs at all. The extraction is the same pass — the weak layer
/// was already reading every literal in the repository and discarding the ones that did
/// not spell a symbol name.
///
/// Why keep them: grep finds a literal faster than this ever will and is never stale.
/// What it cannot do is say whose line it found, and that is what the caller is going to
/// ask next. Returning the enclosing symbol turns a second search into a handle.
pub fn index_literals(store: &mut Store, repo_root: &Path) -> Result<LiteralStats> {
    let mut stats = LiteralStats::default();
    let files: Vec<(i64, String)> = {
        let mut stmt = store.conn.prepare(
            "SELECT f.id, p.s FROM files f JOIN strings p ON p.id = f.path_id
              WHERE f.generated = 0",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        out
    };

    let tx = store.conn.transaction()?;
    tx.execute("DELETE FROM literals", [])?;
    {
        // Definitions with a body, per file, so a literal can be attributed without a
        // correlated subquery per row.
        let mut spans_stmt = tx.prepare(
            "SELECT id, def_line, def_end_line FROM symbols
              WHERE def_file_id = ?1 AND def_end_line IS NOT NULL",
        )?;
        let mut ins = tx.prepare(
            "INSERT OR IGNORE INTO literals(text, file_id, line, enclosing)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (file_id, rel_path) in &files {
            let Ok(text) = std::fs::read_to_string(repo_root.join(rel_path)) else {
                continue;
            };
            stats.files += 1;
            let spans: Vec<(i64, i64, i64)> = spans_stmt
                .query_map(params![file_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<std::result::Result<_, _>>()?;
            for (line, literal) in string_literals(&text, rel_path) {
                if literal.len() < MIN_LITERAL_LEN || literal.trim().is_empty() {
                    continue;
                }
                // Numbers and version strings are noise: a caller searching for one of
                // those is not asking a question this can answer better than grep.
                if literal.chars().all(|c| c.is_ascii_digit() || c == '.') {
                    continue;
                }
                // Deepest span wins. A literal inside a method is inside its class too,
                // and naming the class would hand back the larger thing to read - the
                // same mistake the documentation layer had to be corrected for.
                let enclosing = spans
                    .iter()
                    .filter(|(_, a, b)| *a <= line && line <= *b)
                    .max_by_key(|(_, a, _)| *a)
                    .map(|(id, _, _)| *id);
                if enclosing.is_some() {
                    stats.attributed += 1;
                }
                ins.execute(params![literal, file_id, line, enclosing])?;
                stats.literals += 1;
            }
        }
    }
    tx.commit()?;
    Ok(stats)
}

impl Store {
    /// String literals containing this text, with whose code each sits in.
    ///
    /// A plain case-insensitive substring, because that is the question: a caller who
    /// knows the header is spelled `X-Request-Id` is not searching, they are locating.
    /// Ordered so attributed sites come first — a literal inside a function is the one
    /// that can be followed anywhere.
    pub fn literals(&self, needle: &str, limit: usize) -> Result<Vec<LiteralSite>> {
        let mut stmt = self.conn.prepare(
            "SELECT l.text, p.s, l.line, l.enclosing
               FROM literals l
               JOIN files f   ON f.id = l.file_id
               JOIN strings p ON p.id = f.path_id
              WHERE lower(l.text) LIKE ?1
              ORDER BY (l.enclosing IS NULL) ASC, p.s, l.line
              LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![format!("%{}%", needle.to_lowercase()), limit as i64],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            },
        )?;
        let mut out = Vec::new();
        for r in rows {
            let (text, path, line, enclosing) = r?;
            let enclosing = match enclosing {
                Some(id) => self.symbol(id)?,
                None => None,
            };
            out.push(LiteralSite {
                text,
                path,
                line,
                enclosing,
            });
        }
        Ok(out)
    }

    /// How many literals were recorded, for the coverage axis.
    pub fn literal_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM literals", [], |r| r.get(0))?)
    }
}

/// Scan the repo for string literals matching symbol names and record weak edges.
///
/// `repo_root` must be the workspace root; file paths in the store are relative to it.
pub fn derive_weak_links(store: &mut Store, repo_root: &Path) -> Result<WeakStats> {
    let mut stats = WeakStats::default();

    debug_assert_eq!(
        MATCHABLE_KINDS.len(),
        2,
        "kind filter below is inlined in SQL"
    );
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
            &[
                "src_symbol",
                "dst_symbol",
                "kind",
                "source",
                "confidence",
                "file_id",
                "line",
            ],
        );
        // The weak layer is rebuilt wholesale; it is derived, never authored.
        tx.execute(
            "DELETE FROM edges WHERE source = ?1",
            params![EdgeSource::Weak as i64],
        )?;

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
                let tail = literal.rsplit(['.', '/', ':']).next().unwrap_or(&literal);
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
        string_literals(text, path)
            .into_iter()
            .map(|(_, s)| s)
            .collect()
    }

    #[test]
    fn a_literal_is_attributed_to_the_function_it_sits_in() {
        // The whole reason to keep literals at all. grep finds the line faster than this
        // ever will; what it cannot say is whose line it is, and that is the question
        // being asked next. Attribution is what turns a second search into a handle.
        let dir = std::env::temp_dir().join("cairn-literals");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("h.py"),
            "HEADER = \"X-Request-Id\"\n\ndef build_headers():\n    return {\"X-Request-Id\": 1}\n",
        )
        .unwrap();
        let mut store = Store::reset(&dir.join("index.sqlite")).unwrap();
        // One file, one function spanning lines 3-4 (zero-based 2..3).
        store
            .conn
            .execute_batch(
                "INSERT INTO strings(s) VALUES ('h.py'), ('build_headers');
                 INSERT INTO files(id, path_id, lang)
                   VALUES (1, (SELECT id FROM strings WHERE s='h.py'), 1);
                 INSERT INTO symbols(id, hash, name_id, kind, lang, def_file_id, def_line,
                                     def_end_line)
                   VALUES (1, x'01', (SELECT id FROM strings WHERE s='build_headers'),
                           3, 1, 1, 2, 3);",
            )
            .unwrap();
        let stats = index_literals(&mut store, &dir).unwrap();
        assert_eq!(stats.literals, 2, "both spellings of the header");
        assert_eq!(stats.attributed, 1, "only the one inside the function");

        let hits = store.literals("x-request-id", 10).unwrap();
        assert_eq!(hits.len(), 2);
        // Attributed first: a literal inside a function is the one that can be followed.
        assert!(hits[0].enclosing.is_some());
        assert_eq!(hits[0].enclosing.as_ref().unwrap().name, "build_headers");
        assert!(
            hits[1].enclosing.is_none(),
            "the module-level one is not invented into a function"
        );
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
