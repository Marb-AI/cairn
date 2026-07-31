//! What the index does *not* know, stated in numbers.
//!
//! The agent is expected to treat answers as fact and stop checking (that is the whole
//! point — architecture 0). That only works if the gaps are declared. Everything here
//! is a known-unknown: measurable, boring, and far more dangerous when left implicit
//! than when printed.
//!
//! Anything found here that a *specific answer* is affected by belongs in that answer's
//! `unknown:` section too. This command is the whole-index view of the same contract.

use crate::Store;
use anyhow::Result;
use rusqlite::params;
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Report {
    pub files: i64,
    pub symbols: i64,

    /// Symbols referenced but defined nowhere in the index. Usually third-party or
    /// stdlib: expected, but it bounds what "find the definition" can ever answer.
    pub symbols_without_definition: i64,
    /// Definitions the indexer gave no body extent for. `--detail body` degrades to a
    /// single line for these.
    pub definitions_without_body_span: i64,
    pub definitions_with_body_span: i64,

    pub references: i64,
    /// References that sit in no function body, so they have no caller. Module-level
    /// use: real code, correctly attributed to nothing.
    pub references_without_caller: i64,

    /// Same symbol defined in more than one file. Ranking picks one; the others are
    /// invisible unless asked for.
    pub ambiguous_definitions: i64,

    pub generated_files: i64,
    /// Generated files identified only by filename pattern - the weak signal.
    pub generated_by_path_only: i64,
    /// Files whose contents could not be read at index time, so generated-code
    /// detection fell back to guessing for them.
    pub files_without_content_hash: i64,

    pub weak_edges: i64,
    pub manual_edges: i64,
    /// Manual edges whose anchor file changed after the edge was recorded.
    pub manual_edges_stale: i64,
    pub concepts: i64,
    pub concept_links: i64,
    /// Concept links with no anchor: nothing can ever invalidate them.
    pub concept_links_unanchored: i64,
    pub concept_links_stale: i64,
    /// Authored links whose symbol is not in the current index: the code was renamed
    /// or deleted after the claim was made.
    pub concept_links_dangling: i64,

    /// Files whose contents differ from what was indexed.
    pub stale_files: Vec<String>,
    /// Files present in the index but gone from disk.
    pub missing_files: Vec<String>,
    /// Whether staleness could be checked at all.
    pub staleness_checked: bool,
}

impl Report {
    /// True when nothing in the report should stop an agent trusting the answers.
    pub fn is_clean(&self) -> bool {
        self.stale_files.is_empty()
            && self.missing_files.is_empty()
            && self.generated_by_path_only == 0
            && self.manual_edges_stale == 0
            && self.concept_links_stale == 0
            && self.concept_links_dangling == 0
    }
}

impl Store {
    /// Measure the known-unknowns. With `repo_root`, also compares file contents
    /// against what was indexed and names every file that has drifted.
    pub fn verify(&self, repo_root: Option<&Path>) -> Result<Report> {
        let one = |sql: &str| -> Result<i64> { Ok(self.conn.query_row(sql, [], |r| r.get(0))?) };
        let mut rep = Report {
            files: one("SELECT count(*) FROM files")?,
            symbols: one("SELECT count(*) FROM symbols")?,
            symbols_without_definition: one(
                "SELECT count(*) FROM symbols WHERE def_file_id IS NULL",
            )?,
            definitions_without_body_span: one(
                "SELECT count(*) FROM symbols WHERE def_file_id IS NOT NULL AND def_end_line IS NULL",
            )?,
            definitions_with_body_span: one(
                "SELECT count(*) FROM symbols WHERE def_end_line IS NOT NULL",
            )?,
            references: one("SELECT count(*) FROM occurrences WHERE (role & 1) = 0")?,
            ambiguous_definitions: one(
                "SELECT count(*) FROM (SELECT symbol_id FROM occurrences WHERE (role & 1) = 1
                 GROUP BY symbol_id HAVING count(DISTINCT file_id) > 1)",
            )?,
            generated_files: one("SELECT count(*) FROM files WHERE generated = 1")?,
            generated_by_path_only: one("SELECT count(*) FROM files WHERE gen_via = 3")?,
            files_without_content_hash: one(
                "SELECT count(*) FROM files WHERE content_hash IS NULL",
            )?,
            weak_edges: one("SELECT count(*) FROM edges WHERE kind = 2")?,
            manual_edges: one("SELECT count(*) FROM edges WHERE source >= 2")?,
            concepts: one("SELECT count(*) FROM k.concepts")?,
            concept_links: one("SELECT count(*) FROM k.concept_links")?,
            concept_links_unanchored: one(
                "SELECT count(*) FROM k.concept_links WHERE anchor_hash IS NULL",
            )?,
            concept_links_dangling: one(
                "SELECT count(*) FROM k.concept_links l
                  WHERE NOT EXISTS (SELECT 1 FROM symbols s WHERE s.hash = l.symbol_hash)",
            )?,
            ..Default::default()
        };
        let attributed = one("SELECT count(*) FROM edges WHERE kind = 0")?;
        rep.references_without_caller = (rep.references - attributed).max(0);

        if let Some(root) = repo_root {
            rep.staleness_checked = true;
            let mut stmt = self.conn.prepare(
                "SELECT p.s, f.content_hash FROM files f JOIN strings p ON p.id = f.path_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<Vec<u8>>>(1)?))
            })?;
            for row in rows {
                let (rel, indexed) = row?;
                let full = root.join(&rel);
                match std::fs::read(&full) {
                    Err(_) => rep.missing_files.push(rel),
                    Ok(bytes) => {
                        let now = blake3::hash(&bytes).as_bytes()[..16].to_vec();
                        // No recorded hash means the file was indexed without repo
                        // access; that is counted separately, not called stale.
                        if let Some(then) = indexed {
                            if then != now {
                                rep.stale_files.push(rel);
                            }
                        }
                    }
                }
            }
            rep.stale_files.sort();
            rep.missing_files.sort();

            rep.manual_edges_stale = self.count_stale_manual_edges(root)?;
            rep.concept_links_stale = self.conn.query_row(
                "SELECT count(*) FROM k.concept_links WHERE needs_review = 1",
                [],
                |r| r.get(0),
            )?;
        }
        Ok(rep)
    }

    /// Manual edges anchored in files that have changed since the edge was recorded.
    ///
    /// A hand-authored link points at a place in the code. When that place moves or is
    /// rewritten, the link may still be right or may be nonsense — and we cannot tell.
    /// Reporting it is the only honest option: silently keeping it would let a stale
    /// claim masquerade as a fact, and silently dropping it would throw away work the
    /// static pass could not do (architecture 18.3).
    fn count_stale_manual_edges(&self, root: &Path) -> Result<i64> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT p.s, f.content_hash
               FROM edges e
               JOIN files f   ON f.id = e.file_id
               JOIN strings p ON p.id = f.path_id
              WHERE e.source >= 2",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<Vec<u8>>>(1)?))
        })?;
        let mut stale = 0;
        for row in rows {
            let (rel, indexed) = row?;
            let Some(then) = indexed else { continue };
            match std::fs::read(root.join(&rel)) {
                Ok(bytes) if blake3::hash(&bytes).as_bytes()[..16] != then[..] => stale += 1,
                Err(_) => stale += 1,
                _ => {}
            }
        }
        Ok(stale)
    }

    /// Mark manual edges as needing review because their anchor moved.
    pub fn flag_stale_manual_edges(&self, root: &Path) -> Result<i64> {
        let mut to_flag: Vec<i64> = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT f.id, p.s, f.content_hash
                   FROM files f JOIN strings p ON p.id = f.path_id
                  WHERE f.id IN (SELECT DISTINCT file_id FROM edges WHERE source >= 2)",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                ))
            })?;
            for row in rows {
                let (id, rel, indexed) = row?;
                let Some(then) = indexed else { continue };
                let changed = match std::fs::read(root.join(&rel)) {
                    Ok(bytes) => blake3::hash(&bytes).as_bytes()[..16] != then[..],
                    Err(_) => true,
                };
                if changed {
                    to_flag.push(id);
                }
            }
        }
        let mut n = 0;
        for file_id in to_flag {
            n += self.conn.execute(
                "UPDATE edges SET needs_review = 1 WHERE file_id = ?1 AND source >= 2",
                params![file_id],
            )? as i64;
        }
        Ok(n)
    }
}

impl Store {
    /// Record a hand-authored link between two symbols.
    ///
    /// This is the escape hatch for everything the static pass cannot see: a dispatch
    /// through configuration, a contract honoured by convention, a dependency that
    /// only exists at runtime. It is stored with its own provenance and never mixed
    /// into exact results.
    pub fn add_link(
        &self,
        src: i64,
        dst: i64,
        source: crate::EdgeSource,
        note: &str,
        anchor: Option<(i64, i64)>,
    ) -> Result<()> {
        let (file_id, line) = match anchor {
            Some((f, l)) => (Some(f), Some(l)),
            None => (None, None),
        };
        self.conn.execute(
            "INSERT INTO edges(src_symbol, dst_symbol, kind, source, confidence,
                               file_id, line, note, needs_review)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
            params![
                src,
                dst,
                crate::EdgeKind::Asserted as i64,
                source as i64,
                0.9f64,
                file_id,
                line,
                note
            ],
        )?;
        Ok(())
    }

    /// Hand-authored links touching a symbol, in either direction.
    pub fn asserted_links(&self, symbol_id: i64) -> Result<Vec<AssertedLink>> {
        let mut stmt = self.conn.prepare_cached(
            r#"
            SELECT e.src_symbol, e.dst_symbol, e.source, coalesce(e.note, ''),
                   e.needs_review, coalesce(p.s, ''), e.line
              FROM edges e
              LEFT JOIN files   f ON f.id = e.file_id
              LEFT JOIN strings p ON p.id = f.path_id
             WHERE e.kind = 3 AND (e.src_symbol = ?1 OR e.dst_symbol = ?1)
             ORDER BY e.needs_review DESC
            "#,
        )?;
        let rows = stmt.query_map(params![symbol_id], |r| {
            Ok(AssertedLink {
                src: r.get(0)?,
                dst: r.get(1)?,
                source: match r.get::<_, i64>(2)? {
                    3 => crate::EdgeSource::Human,
                    _ => crate::EdgeSource::Agent,
                },
                note: r.get(3)?,
                needs_review: r.get::<_, i64>(4)? != 0,
                anchor: {
                    let p: String = r.get(5)?;
                    let l: Option<i64> = r.get(6)?;
                    match (p.is_empty(), l) {
                        (false, Some(l)) => Some(format!("{p}:{}", l + 1)),
                        _ => None,
                    }
                },
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

#[derive(Debug, Clone)]
pub struct AssertedLink {
    pub src: i64,
    pub dst: i64,
    pub source: crate::EdgeSource,
    pub note: String,
    pub needs_review: bool,
    pub anchor: Option<String>,
}
