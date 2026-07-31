//! Read path: symbol lookup, references, and handle assignment.

use crate::{GeneratedVia, Lang, Store};
use anyhow::Result;
use cairn_scip::{SymbolKind, ROLE_DEFINITION};
use rusqlite::params;

/// Alphabet for handles. Excludes `l`, `o`, `0`, `1` so a handle read off a terminal
/// and retyped cannot be mistaken.
const HANDLE_ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
const HANDLE_MIN_LEN: usize = 2;
const HANDLE_MAX_LEN: usize = 12;

#[derive(Debug, Clone)]
pub struct SymbolRow {
    pub id: i64,
    pub handle: String,
    pub name: String,
    pub container: Option<String>,
    pub module: Option<String>,
    pub kind: SymbolKind,
    pub lang: Lang,
    /// Definition site, if the index has one.
    pub def: Option<Occurrence>,
    /// Last line of the definition's body, when the indexer emitted an enclosing
    /// range. Absent for symbols without a body (fields, parameters, aliases).
    pub def_end_line: Option<i64>,
    pub ref_count: i64,
}

impl SymbolRow {
    /// `Class.method` when the symbol is nested in a *type*, else the bare name.
    ///
    /// The container of a module-level function is its namespace, and prefixing the
    /// full dotted module path would eat the whole line for no information — the
    /// location column already says which file it is in.
    pub fn qualified(&self) -> String {
        match &self.container {
            Some(c) if container_is_type(c) => {
                let short = last_container_segment(c);
                if short.is_empty() {
                    self.name.clone()
                } else {
                    format!("{short}.{}", self.name)
                }
            }
            _ => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Occurrence {
    pub path: String,
    pub line: i64,
    pub col_start: i64,
    pub col_end: i64,
    pub role: i32,
    pub generated: bool,
    pub gen_via: GeneratedVia,
}

impl Occurrence {
    pub fn is_definition(&self) -> bool {
        self.role & ROLE_DEFINITION != 0
    }
    /// `path:line` with the 1-based line numbering humans and editors use.
    /// SCIP lines are 0-based.
    pub fn location(&self) -> String {
        format!("{}:{}", self.path, self.line + 1)
    }
}

/// True when the innermost container is a type (`#`) rather than a namespace (`/`).
fn container_is_type(container: &str) -> bool {
    container.ends_with('#')
}

/// Trailing type/namespace segment of a container descriptor, for display.
/// `` `a.b.c`/Outer#Inner# `` -> `Inner`
fn last_container_segment(container: &str) -> &str {
    let trimmed = container.trim_end_matches(['#', '/', '.']);
    let start = trimmed
        .rfind(['#', '/'])
        .map(|i| i + 1)
        .unwrap_or(0);
    trimmed[start..].trim_matches('`')
}

impl Store {
    /// Look up or assign the persistent short handle for a symbol (architecture 6.5).
    ///
    /// The handle is the shortest prefix of the symbol's hash, rendered in the alphabet
    /// above, that is not already taken. Because assignment is persisted, a handle the
    /// agent noted in a previous session still resolves.
    pub fn handle_for(&self, symbol_id: i64) -> Result<String> {
        if let Some(h) = self.existing_handle(symbol_id)? {
            return Ok(h);
        }
        let hash: Vec<u8> = self.conn.query_row(
            "SELECT hash FROM symbols WHERE id = ?1",
            params![symbol_id],
            |r| r.get(0),
        )?;
        let full = encode(&hash, HANDLE_MAX_LEN);
        for len in HANDLE_MIN_LEN..=HANDLE_MAX_LEN {
            let candidate = &full[..len];
            let taken: Option<i64> = self
                .conn
                .query_row(
                    "SELECT symbol_id FROM handles WHERE handle = ?1",
                    params![candidate],
                    |r| r.get(0),
                )
                .ok();
            match taken {
                Some(owner) if owner == symbol_id => return Ok(candidate.to_string()),
                Some(_) => continue, // collision, lengthen
                None => {
                    self.conn.execute(
                        "INSERT INTO handles(symbol_id, handle) VALUES (?1, ?2)",
                        params![symbol_id, candidate],
                    )?;
                    return Ok(candidate.to_string());
                }
            }
        }
        anyhow::bail!("could not assign a unique handle for symbol {symbol_id}")
    }

    fn existing_handle(&self, symbol_id: i64) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT handle FROM handles WHERE symbol_id = ?1")?;
        let mut rows = stmt.query(params![symbol_id])?;
        Ok(match rows.next()? {
            Some(r) => Some(r.get(0)?),
            None => None,
        })
    }

    /// Symbols the index believes are defined in a file, as
    /// `(qualified name, start, end)`.
    ///
    /// Qualified, not bare: two classes in one file can both have `on_call_tool`, and
    /// matching on the bare name would pair one with the other and report a move that
    /// never happened.
    pub fn symbols_in_file(&self, path: &str) -> Result<Vec<(String, i64, Option<i64>)>> {
        let mut stmt = self.conn.prepare_cached(
            r#"
            SELECT n.s, s.def_line, s.def_end_line, c.s, s.kind
              FROM symbols s
              LEFT JOIN strings c ON c.id = s.container_id
              JOIN files   f ON f.id = s.def_file_id
              JOIN strings p ON p.id = f.path_id
              JOIN strings n ON n.id = s.name_id
             WHERE p.s = ?1
             ORDER BY s.def_line
            "#,
        )?;
        let rows = stmt.query_map(params![path], |r| {
            let name: String = r.get(0)?;
            let container: Option<String> = r.get(3)?;
            let kind: i64 = r.get(4)?;
            let qualified = match container.as_deref().filter(|c| c.ends_with('#')) {
                Some(c) => format!("{}.{name}", last_container_segment(c)),
                None => name,
            };
            Ok((
                qualified,
                r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                r.get(2)?,
                kind,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (q, start, end, kind) = r?;
            // Parameters and locals are not file structure; the live view does not
            // list them either, so including them would manufacture "gone" entries.
            if matches!(kind_from_i64(kind), SymbolKind::Parameter | SymbolKind::TypeParameter) {
                continue;
            }
            out.push((q, start, end));
        }
        Ok(out)
    }

    pub fn file_id_for_path(&self, path: &str) -> Result<Option<i64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.id FROM files f JOIN strings p ON p.id = f.path_id WHERE p.s = ?1",
        )?;
        let mut rows = stmt.query(params![path])?;
        Ok(match rows.next()? {
            Some(r) => Some(r.get(0)?),
            None => None,
        })
    }

    pub fn resolve_handle(&self, handle: &str) -> Result<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT symbol_id FROM handles WHERE handle = ?1")?;
        let mut rows = stmt.query(params![handle])?;
        Ok(match rows.next()? {
            Some(r) => Some(r.get(0)?),
            None => None,
        })
    }

    /// Search symbols by name.
    ///
    /// Ranking follows architecture 6.6: exact name beats prefix beats substring;
    /// generated code and tests are pushed down; more-referenced symbols win ties.
    /// The point of the ranking is that we return few results and the right ones —
    /// returning everything would spend the tokens the tool exists to save.
    pub fn find_symbols(&self, needle: &str, limit: usize) -> Result<Vec<SymbolRow>> {
        // `%` and `_` from the caller are left as SQL wildcards on purpose: the
        // command takes "name | pattern" (architecture 6.1), so `Auth%Handler` works.
        let pattern = format!("%{needle}%");
        // Two things keep this fast enough to run per keystroke-equivalent:
        //   * name matching happens on the interned `strings` table first, so the
        //     substring scan touches one narrow table rather than a four-way join;
        //   * ranking reads denormalised columns, so nothing is computed per candidate
        //     row before LIMIT applies.
        let mut stmt = self.conn.prepare_cached(
            r#"
            WITH hits AS (
                SELECT id, s FROM strings WHERE s LIKE ?1
            )
            SELECT s.id, h.s AS name, c.s AS container, m.s AS module, s.kind, s.lang,
                   s.ref_count, s.def_generated,
                   p.s AS def_path, s.def_line, s.def_col_start, s.def_col_end,
                   f.generated, f.gen_via, s.def_end_line
              FROM hits h
              JOIN symbols s ON s.name_id = h.id
              LEFT JOIN strings c ON c.id = s.container_id
              LEFT JOIN strings m ON m.id = s.module_id
              LEFT JOIN files   f ON f.id = s.def_file_id
              LEFT JOIN strings p ON p.id = f.path_id
             ORDER BY
                   (h.s = ?2) DESC,
                   (h.s LIKE ?3) DESC,
                   s.def_generated ASC,
                   (s.def_file_id IS NULL) ASC,
                   length(h.s) ASC,
                   s.ref_count DESC
             LIMIT ?4
            "#,
        )?;
        let rows = stmt.query_map(
            params![pattern, needle, format!("{needle}%"), limit as i64],
            |r| {
                let def = match r.get::<_, Option<String>>(8)? {
                    Some(path) => Some(Occurrence {
                        path,
                        line: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
                        col_start: r.get::<_, Option<i64>>(10)?.unwrap_or(0),
                        col_end: r.get::<_, Option<i64>>(11)?.unwrap_or(0),
                        role: ROLE_DEFINITION,
                        generated: r.get::<_, Option<i64>>(12)?.unwrap_or(0) != 0,
                        gen_via: gen_via_from_i64(r.get::<_, Option<i64>>(13)?.unwrap_or(0)),
                    }),
                    None => None,
                };
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    def,
                    r.get::<_, Option<i64>>(14)?,
                ))
            },
        )?;

        let mut out = Vec::new();
        for row in rows {
            let (id, name, container, module, kind, lang, refs, def, def_end_line) = row?;
            out.push(SymbolRow {
                id,
                handle: self.handle_for(id)?,
                name,
                container,
                module,
                kind: kind_from_i64(kind),
                lang: Lang::from_i64(lang),
                def,
                def_end_line,
                ref_count: refs,
            });
        }
        Ok(out)
    }

    pub fn symbol(&self, symbol_id: i64) -> Result<Option<SymbolRow>> {
        let mut stmt = self.conn.prepare_cached(
            r#"
            SELECT s.id, n.s, c.s, m.s, s.kind, s.lang, s.ref_count, s.def_end_line
              FROM symbols s
              JOIN strings n ON n.id = s.name_id
              LEFT JOIN strings c ON c.id = s.container_id
              LEFT JOIN strings m ON m.id = s.module_id
             WHERE s.id = ?1
            "#,
        )?;
        let mut rows = stmt.query(params![symbol_id])?;
        let Some(r) = rows.next()? else {
            return Ok(None);
        };
        let id: i64 = r.get(0)?;
        Ok(Some(SymbolRow {
            id,
            handle: self.handle_for(id)?,
            name: r.get(1)?,
            container: r.get(2)?,
            module: r.get(3)?,
            kind: kind_from_i64(r.get(4)?),
            lang: Lang::from_i64(r.get(5)?),
            def: self.definition(id)?,
            def_end_line: r.get(7)?,
            ref_count: r.get(6)?,
        }))
    }

    pub fn definition(&self, symbol_id: i64) -> Result<Option<Occurrence>> {
        let mut stmt = self.conn.prepare_cached(
            r#"
            SELECT p.s, s.def_line, s.def_col_start, s.def_col_end, f.generated, f.gen_via
              FROM symbols s
              JOIN files f   ON f.id = s.def_file_id
              JOIN strings p ON p.id = f.path_id
             WHERE s.id = ?1
            "#,
        )?;
        let mut rows = stmt.query(params![symbol_id])?;
        Ok(match rows.next()? {
            Some(r) => Some(Occurrence {
                path: r.get(0)?,
                line: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                col_start: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                col_end: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                role: ROLE_DEFINITION,
                generated: r.get::<_, i64>(4)? != 0,
                gen_via: gen_via_from_i64(r.get::<_, i64>(5)?),
            }),
            None => None,
        })
    }

    /// Reference occurrences, definitions excluded.
    ///
    /// `include_generated` defaults to false at the call sites: on the spike corpus
    /// 58 % of Go occurrences are generated, so including them by default would bury
    /// every answer (architecture 7.3).
    pub fn references(
        &self,
        symbol_id: i64,
        include_generated: bool,
        limit: usize,
    ) -> Result<(Vec<Occurrence>, i64)> {
        let mut stmt = self.conn.prepare_cached(
            r#"
            SELECT p.s, o.line, o.col_start, o.col_end, o.role, f.generated, f.gen_via
              FROM occurrences o
              JOIN files f   ON f.id = o.file_id
              JOIN strings p ON p.id = f.path_id
             WHERE o.symbol_id = ?1 AND (o.role & 1) = 0
               AND (?2 = 1 OR f.generated = 0)
             ORDER BY f.generated ASC, p.s ASC, o.line ASC
             LIMIT ?3
            "#,
        )?;
        let rows = stmt.query_map(
            params![symbol_id, include_generated as i64, limit as i64],
            |r| occ_from_row(r).map_err(|_| rusqlite::Error::InvalidQuery),
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }

        let suppressed: i64 = if include_generated {
            0
        } else {
            self.conn.query_row(
                r#"SELECT count(*) FROM occurrences o
                     JOIN files f ON f.id = o.file_id
                    WHERE o.symbol_id = ?1 AND (o.role & 1) = 0 AND f.generated = 1"#,
                params![symbol_id],
                |r| r.get(0),
            )?
        };
        Ok((out, suppressed))
    }
}

fn occ_from_row(r: &rusqlite::Row) -> Result<Occurrence> {
    Ok(Occurrence {
        path: r.get(0)?,
        line: r.get(1)?,
        col_start: r.get(2)?,
        col_end: r.get(3)?,
        role: r.get::<_, i64>(4)? as i32,
        generated: r.get::<_, i64>(5)? != 0,
        gen_via: gen_via_from_i64(r.get::<_, i64>(6)?),
    })
}

fn gen_via_from_i64(v: i64) -> GeneratedVia {
    match v {
        1 => GeneratedVia::HeaderMarker,
        2 => GeneratedVia::GitAttributes,
        3 => GeneratedVia::PathPattern,
        _ => GeneratedVia::No,
    }
}

fn kind_from_i64(v: i64) -> SymbolKind {
    match v {
        0 => SymbolKind::Namespace,
        1 => SymbolKind::Type,
        2 => SymbolKind::Term,
        3 => SymbolKind::Method,
        4 => SymbolKind::TypeParameter,
        5 => SymbolKind::Parameter,
        6 => SymbolKind::Meta,
        7 => SymbolKind::Local,
        _ => SymbolKind::Unknown,
    }
}

fn encode(bytes: &[u8], max_chars: usize) -> String {
    let mut out = String::with_capacity(max_chars);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for &b in bytes {
        acc = (acc << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = ((acc >> bits) & 0x1f) as usize;
            out.push(HANDLE_ALPHABET[idx] as char);
            if out.len() == max_chars {
                return out;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_types_qualify_the_name() {
        assert!(container_is_type("`a.b`/Klass#"));
        assert!(!container_is_type("`a.b.c`/"));
    }

    #[test]
    fn container_segment() {
        assert_eq!(last_container_segment("`a.b.c`/Outer#Inner#"), "Inner");
        assert_eq!(last_container_segment("`a.b`/Klass#"), "Klass");
        assert_eq!(last_container_segment("`a.b`/"), "a.b");
    }

    #[test]
    fn handles_are_deterministic_and_short() {
        let h1 = encode(&crate::symbol_hash("scip-python python p v `m`/A#"), 12);
        let h2 = encode(&crate::symbol_hash("scip-python python p v `m`/A#"), 12);
        assert_eq!(h1, h2);
        assert_ne!(
            h1,
            encode(&crate::symbol_hash("scip-python python p v `m`/B#"), 12)
        );
        assert!(h1.chars().all(|c| HANDLE_ALPHABET.contains(&(c as u8))));
    }

    #[test]
    fn line_numbers_are_one_based_on_output() {
        let occ = Occurrence {
            path: "a/b.py".into(),
            line: 41,
            col_start: 0,
            col_end: 3,
            role: 0,
            generated: false,
            gen_via: GeneratedVia::No,
        };
        assert_eq!(occ.location(), "a/b.py:42");
    }
}
