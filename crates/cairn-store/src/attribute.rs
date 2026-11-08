//! Whose line is this?
//!
//! A text search returns a line. `rg` returns the same line in the same 20 ms, so a line
//! is not the product — what the index can add to it is. Measured (scenario 7): the arm
//! that got the enclosing function with the hit answered in two round trips; the arm that
//! got the line alone spent four working out whose it was. That difference is the whole
//! reason this file exists.
//!
//! Everything here is a lookup in what was already stored for another purpose: the
//! definition spans the call graph needs, the section ranges `docs` answers from, the
//! service table the deployment commands were parsed into. Nothing is indexed twice.

use crate::Store;
use anyhow::Result;
use rusqlite::params;

/// What the index knows about one line of one file.
#[derive(Debug, Clone, Default)]
pub struct LineContext {
    /// Innermost definition whose body contains the line, with its handle so the next
    /// question costs one call instead of a name search.
    pub symbol: Option<(String, String)>,
    /// Heading trail of the markdown section holding the line, and the section's range —
    /// the range being the part that says what reading it costs.
    pub section: Option<(String, i64, i64)>,
    pub generated: bool,
    pub is_test: bool,
    /// True when the file is indexed at all. A hit in a file the index has never seen is
    /// still a hit; it just arrives without any of the above, and saying so is the
    /// difference between "no context" and "no context *because* nothing indexes SQL".
    pub indexed: bool,
    /// For a code file that is *not* indexed: was it inside a project the index was built
    /// from, or outside every one of them?
    ///
    /// `Some(true)` is a gap — the index covers that directory and this file is missing
    /// from it, so go and look. `Some(false)` is not a gap: the file is outside what was
    /// indexed, usually because the repository's own workspace excludes it, and there is
    /// nothing to find. `None` means the index does not record its roots and the question
    /// cannot be answered, which is a third thing and must not be reported as either.
    ///
    /// Measured: a run was told a deliberately excluded app's file was one "the last index
    /// run missed". Those are opposite instructions to a reader, and the line exists to
    /// carry exactly that difference.
    pub inside_indexed_project: Option<bool>,
}

/// Is `path` inside one of `roots`?
///
/// `None` when there are no roots to compare against — see `inside_indexed_project`.
/// A root of `apps/rixby` contains `apps/rixby/api/x.ts` and does not contain
/// `apps/rixby-old/x.ts`, hence the separator rather than a bare `starts_with`.
pub fn inside_any_root(roots: &[String], path: &str) -> Option<bool> {
    if roots.is_empty() {
        return None;
    }
    Some(
        roots.iter().any(|r| {
            path == r.as_str() || path.starts_with(&format!("{}/", r.trim_end_matches('/')))
        }),
    )
}

impl Store {
    /// Attribute one line. Cheap enough to call per hit: every lookup is an index seek.
    pub fn line_context(&self, path: &str, line: i64) -> Result<LineContext> {
        let mut out = LineContext::default();

        // File flags. Absent means the file is not in the index, which is normal for
        // everything that is neither Python nor Go.
        let file: Option<(i64, bool, bool)> = self
            .conn
            .query_row(
                "SELECT f.id, f.generated, f.is_test FROM files f
                   JOIN strings p ON p.id = f.path_id WHERE p.s = ?1",
                params![path],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)? != 0,
                        r.get::<_, i64>(2)? != 0,
                    ))
                },
            )
            .ok();

        if let Some((file_id, generated, is_test)) = file {
            out.indexed = true;
            out.generated = generated;
            out.is_test = is_test;
            // Innermost by span: a method inside a class inside a module should answer
            // "the method". Ordered by span width, which is what `sym_by_defspan` exists
            // for — the same ordering `weak.rs` uses to attribute a literal.
            // `symbols` stores lines zero-based and prints them one-based;
            // `doc_sections` below stores them one-based. Passing the printed number to
            // both looked right and was wrong by one: `_headers` spans 85-86 in storage,
            // the caller asked about 87, the method fell outside its own body and the
            // enclosing class answered instead. A class is a plausible-looking answer,
            // which is why this needed a query against the database to see rather than a
            // reading of the code.
            out.symbol = self
                .conn
                .query_row(
                    "SELECT n.s, h.handle FROM symbols s
                       JOIN strings n ON n.id = s.name_id
                       JOIN handles h ON h.symbol_id = s.id
                      WHERE s.def_file_id = ?1
                        AND s.def_line <= ?2 AND s.def_end_line >= ?2
                      ORDER BY (s.def_end_line - s.def_line) ASC
                      LIMIT 1",
                    params![file_id, line - 1],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .ok();
        }

        // Markdown: the smallest section that contains the line, for the same reason the
        // smallest symbol span wins.
        out.section = self
            .conn
            .query_row(
                "SELECT sec.trail, sec.heading, sec.start_line, sec.end_line
                   FROM doc_sections sec
                   JOIN doc_files d ON d.id = sec.doc_id
                  WHERE d.path = ?1 AND sec.start_line <= ?2 AND sec.end_line >= ?2
                  ORDER BY (sec.end_line - sec.start_line) ASC
                  LIMIT 1",
                params![path, line],
                |r| {
                    let trail: String = r.get(0)?;
                    let heading: String = r.get(1)?;
                    let full = if trail.is_empty() {
                        heading
                    } else {
                        format!("{trail} > {heading}")
                    };
                    Ok((full, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?))
                },
            )
            .ok();

        Ok(out)
    }

    /// Deployed services whose start command or published ports carry this text.
    ///
    /// A property of the *search term*, not of any one line — which is why it is here and
    /// not in `LineContext`. Printed per row it read as "this line belongs to
    /// assistant-mcp", which is wrong on a README line that merely mentions the variable.
    /// Per-row provenance is the point of this output; a per-query fact rendered per row
    /// is that principle applied backwards.
    pub fn services_mentioning(&self, text: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT name FROM deploy_services
              WHERE instr(lower(coalesce(command, '')), lower(?1)) > 0
                 OR instr(lower(ports), lower(?1)) > 0
              ORDER BY name",
        )?;
        let rows = stmt.query_map(params![text], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sibling_directory_is_not_inside_a_root() {
        // `apps/marb_old` next to the root `apps/marb` is the shape that matters: a bare
        // `starts_with` calls the retired app part of the indexed one, and the tag then
        // says the opposite of the truth about every hit in it.
        let roots = vec!["apps/marb".to_string()];
        assert_eq!(
            inside_any_root(&roots, "apps/marb/components/Chat.tsx"),
            Some(true)
        );
        assert_eq!(
            inside_any_root(&roots, "apps/marb_old/components/Chat.tsx"),
            Some(false)
        );
    }

    #[test]
    fn no_recorded_roots_is_not_an_answer_either_way() {
        // An index built before the set was recorded cannot say whether a missing file was
        // missed or excluded, and must not pick one.
        assert_eq!(inside_any_root(&[], "apps/marb/x.ts"), None);
    }

    #[test]
    fn every_root_counts_not_just_the_first() {
        let roots = vec!["apps/marb".to_string(), "apps/rixby".to_string()];
        assert_eq!(
            inside_any_root(&roots, "apps/rixby/hooks/use.ts"),
            Some(true)
        );
        assert_eq!(inside_any_root(&roots, "packages/ui/index.ts"), Some(false));
    }
}
