//! Concepts: named nodes that are not symbols, and their links to code.
//!
//! This is the one node type the graph genuinely lacked. "OAuth flow", "billing
//! domain", "the retry rewrite" are real handles onto a codebase that no indexer will
//! ever emit, and without them an agent that learns something has nowhere to put it
//! except a second graph of its own — which would mean two truths, no shared
//! invalidation, and a reconciliation step at every query (architecture 18.6).
//!
//! Three constraints keep this from becoming a general graph store, which is
//! explicitly not what we are building:
//!
//! * **Anchoring.** A link to code records the file and its content hash, so when that
//!   code changes the link is flagged rather than silently trusted or silently lost.
//!   Claims that cannot be anchored are still accepted, but marked permanently
//!   unverifiable — never presented as fact.
//! * **Namespaces.** Everything is authored inside one, so a session's guesses can be
//!   filtered, scoped or dropped wholesale without touching shared knowledge.
//! * **No properties, no query language.** A concept has a name, a note and links.
//!   Anything more and this becomes a graph database, which is on the "never" list
//!   next to custom parsers and a custom storage engine.

use crate::{EdgeSource, Store};
use anyhow::Result;
use rusqlite::params;

/// Default namespace for agent-authored knowledge.
pub const DEFAULT_NS: &str = "agent";

#[derive(Debug, Clone)]
pub struct Concept {
    pub id: i64,
    pub ns: String,
    pub name: String,
    pub note: String,
    pub author: EdgeSource,
    pub link_count: i64,
}

#[derive(Debug, Clone)]
pub struct ConceptLink {
    pub symbol_id: i64,
    /// Free-text relation: `part-of`, `entry-point`, `owns`, whatever fits. Kept
    /// unconstrained on purpose — the vocabulary is the caller's, and a closed enum
    /// would just push them back towards keeping their own store.
    pub rel: String,
    pub note: String,
    pub anchor: Option<String>,
    pub needs_review: bool,
    /// False when the symbol this link names is not in the current index — the code
    /// may have been deleted or renamed since the claim was made.
    pub resolved: bool,
}

impl Store {
    /// Content hash of a symbol, which is how authored rows refer to it: rowids are
    /// assigned per ingest and would dangle after every rebuild.
    fn hash_of(&self, symbol_id: i64) -> Result<Vec<u8>> {
        Ok(self.conn.query_row(
            "SELECT hash FROM symbols WHERE id = ?1",
            params![symbol_id],
            |r| r.get(0),
        )?)
    }

    fn id_of_hash(&self, hash: &[u8]) -> Result<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id FROM symbols WHERE hash = ?1")?;
        let mut rows = stmt.query(params![hash])?;
        Ok(match rows.next()? {
            Some(r) => Some(r.get(0)?),
            None => None,
        })
    }

    pub fn concept_upsert(
        &self,
        ns: &str,
        name: &str,
        note: &str,
        author: EdgeSource,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO k.concepts(ns, name, note, author) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(ns, name) DO UPDATE SET note = excluded.note",
            params![ns, name, note, author as i64],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM k.concepts WHERE ns = ?1 AND name = ?2",
            params![ns, name],
            |r| r.get(0),
        )?)
    }

    pub fn concept_find(&self, ns: &str, name: &str) -> Result<Option<Concept>> {
        let mut stmt = self.conn.prepare_cached(
            r#"
            SELECT c.id, c.ns, c.name, coalesce(c.note, ''), c.author,
                   (SELECT count(*) FROM k.concept_links l WHERE l.concept_id = c.id)
              FROM k.concepts c
             WHERE c.ns = ?1 AND c.name = ?2
            "#,
        )?;
        let mut rows = stmt.query(params![ns, name])?;
        Ok(match rows.next()? {
            Some(r) => Some(Concept {
                id: r.get(0)?,
                ns: r.get(1)?,
                name: r.get(2)?,
                note: r.get(3)?,
                author: author_from_i64(r.get(4)?),
                link_count: r.get(5)?,
            }),
            None => None,
        })
    }

    pub fn concept_list(&self, ns: Option<&str>) -> Result<Vec<Concept>> {
        let mut stmt = self.conn.prepare_cached(
            r#"
            SELECT c.id, c.ns, c.name, coalesce(c.note, ''), c.author,
                   (SELECT count(*) FROM k.concept_links l WHERE l.concept_id = c.id)
              FROM k.concepts c
             WHERE (?1 IS NULL OR c.ns = ?1)
             ORDER BY c.ns, c.name
            "#,
        )?;
        let rows = stmt.query_map(params![ns], |r| {
            Ok(Concept {
                id: r.get(0)?,
                ns: r.get(1)?,
                name: r.get(2)?,
                note: r.get(3)?,
                author: author_from_i64(r.get(4)?),
                link_count: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Attach a symbol to a concept, anchored to that symbol's definition site.
    pub fn concept_link(
        &self,
        concept_id: i64,
        symbol_id: i64,
        rel: &str,
        note: &str,
    ) -> Result<bool> {
        let hash = self.hash_of(symbol_id)?;
        let anchor = self.anchor_for(symbol_id)?;
        self.conn.execute(
            "INSERT INTO k.concept_links(concept_id, symbol_hash, rel, note,
                                         anchor_path, anchor_line, anchor_hash, needs_review)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)
             ON CONFLICT(concept_id, symbol_hash, rel) DO UPDATE SET
                 note = excluded.note, needs_review = 0,
                 anchor_path = excluded.anchor_path,
                 anchor_line = excluded.anchor_line,
                 anchor_hash = excluded.anchor_hash",
            params![
                concept_id,
                hash,
                rel,
                note,
                anchor.as_ref().map(|a| &a.0),
                anchor.as_ref().map(|a| a.1),
                anchor.as_ref().map(|a| &a.2)
            ],
        )?;
        Ok(anchor.is_some())
    }

    /// Definition site of a symbol plus the file's content hash, so a later change is
    /// detectable. Returns None when the symbol has no definition in the index.
    fn anchor_for(&self, symbol_id: i64) -> Result<Option<(String, i64, Vec<u8>)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT p.s, s.def_line, f.content_hash
               FROM symbols s
               JOIN files f   ON f.id = s.def_file_id
               JOIN strings p ON p.id = f.path_id
              WHERE s.id = ?1",
        )?;
        let mut rows = stmt.query(params![symbol_id])?;
        Ok(match rows.next()? {
            Some(r) => {
                let path: String = r.get(0)?;
                let line: Option<i64> = r.get(1)?;
                let hash: Option<Vec<u8>> = r.get(2)?;
                match (line, hash) {
                    (Some(l), Some(h)) => Some((path, l, h)),
                    _ => None,
                }
            }
            None => None,
        })
    }

    pub fn concept_links(&self, concept_id: i64) -> Result<Vec<ConceptLink>> {
        let mut stmt = self.conn.prepare_cached(
            r#"
            SELECT l.symbol_hash, l.rel, coalesce(l.note, ''),
                   coalesce(l.anchor_path, ''), l.anchor_line, l.needs_review
              FROM k.concept_links l
             WHERE l.concept_id = ?1
             ORDER BY l.needs_review DESC, l.rel, l.anchor_path
            "#,
        )?;
        let rows = stmt.query_map(params![concept_id], |r| {
            let path: String = r.get(3)?;
            let line: Option<i64> = r.get(4)?;
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                ConceptLink {
                    symbol_id: 0,
                    rel: r.get(1)?,
                    note: r.get(2)?,
                    anchor: match (path.is_empty(), line) {
                        (false, Some(l)) => Some(format!("{path}:{}", l + 1)),
                        _ => None,
                    },
                    needs_review: r.get::<_, i64>(5)? != 0,
                    resolved: false,
                },
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (hash, mut link) = r?;
            // A link whose symbol is gone from the current index is not an error: the
            // code may have been deleted or renamed. It is reported, not dropped.
            if let Some(id) = self.id_of_hash(&hash)? {
                link.symbol_id = id;
                link.resolved = true;
            }
            out.push(link);
        }
        Ok(out)
    }

    /// Concepts a symbol belongs to — the reverse lookup, which is what makes concepts
    /// useful during navigation rather than only when asked for by name.
    pub fn concepts_of_symbol(&self, symbol_id: i64) -> Result<Vec<(String, String, String)>> {
        let hash = self.hash_of(symbol_id)?;
        let mut stmt = self.conn.prepare_cached(
            r#"
            SELECT c.ns, c.name, l.rel
              FROM k.concept_links l
              JOIN k.concepts c ON c.id = l.concept_id
             WHERE l.symbol_hash = ?1
             ORDER BY c.ns, c.name
            "#,
        )?;
        let rows = stmt.query_map(params![hash], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Discard a whole namespace. The reason namespaces exist: a session's guesses
    /// must be removable in one move, without touching anything shared.
    pub fn concept_drop_ns(&self, ns: &str) -> Result<(usize, usize)> {
        let links = self.conn.execute(
            "DELETE FROM k.concept_links WHERE concept_id IN
               (SELECT id FROM k.concepts WHERE ns = ?1)",
            params![ns],
        )?;
        let concepts = self
            .conn
            .execute("DELETE FROM k.concepts WHERE ns = ?1", params![ns])?;
        Ok((concepts, links))
    }

    /// Flag concept links whose anchor file has changed since they were recorded.
    ///
    /// Comparison is against the hash stored *with the claim*, not against the current
    /// index, so this stays correct even across a full rebuild.
    pub fn flag_stale_concept_links(&self, root: &std::path::Path) -> Result<i64> {
        let mut stale: Vec<(String, Vec<u8>)> = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT DISTINCT anchor_path, anchor_hash FROM k.concept_links
                  WHERE anchor_path IS NOT NULL AND anchor_hash IS NOT NULL",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?;
            for row in rows {
                let (path, then) = row?;
                let changed = match std::fs::read(root.join(&path)) {
                    Ok(bytes) => blake3::hash(&bytes).as_bytes()[..16] != then[..],
                    Err(_) => true,
                };
                if changed {
                    stale.push((path, then));
                }
            }
        }
        let mut n = 0;
        for (path, then) in stale {
            n += self.conn.execute(
                "UPDATE k.concept_links SET needs_review = 1
                  WHERE anchor_path = ?1 AND anchor_hash = ?2",
                params![path, then],
            )? as i64;
        }
        Ok(n)
    }

}

fn author_from_i64(v: i64) -> EdgeSource {
    match v {
        3 => EdgeSource::Human,
        _ => EdgeSource::Agent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let s = Store::open_in_memory().unwrap();
        crate::schema::create_indexes(&s.conn).unwrap();
        s
    }

    #[test]
    fn upsert_is_idempotent_and_updates_the_note() {
        let s = store();
        let a = s.concept_upsert("agent", "oauth-flow", "first", EdgeSource::Agent).unwrap();
        let b = s.concept_upsert("agent", "oauth-flow", "second", EdgeSource::Agent).unwrap();
        assert_eq!(a, b);
        assert_eq!(s.concept_find("agent", "oauth-flow").unwrap().unwrap().note, "second");
    }

    #[test]
    fn namespaces_are_separate() {
        let s = store();
        s.concept_upsert("agent", "billing", "", EdgeSource::Agent).unwrap();
        s.concept_upsert("team", "billing", "", EdgeSource::Human).unwrap();
        assert_eq!(s.concept_list(None).unwrap().len(), 2);
        assert_eq!(s.concept_list(Some("team")).unwrap().len(), 1);
    }

    #[test]
    fn dropping_a_namespace_leaves_the_others_alone() {
        let s = store();
        s.concept_upsert("session-1", "guess", "", EdgeSource::Agent).unwrap();
        s.concept_upsert("team", "keep", "", EdgeSource::Human).unwrap();
        let (concepts, _) = s.concept_drop_ns("session-1").unwrap();
        assert_eq!(concepts, 1);
        assert_eq!(s.concept_list(None).unwrap().len(), 1);
        assert!(s.concept_find("team", "keep").unwrap().is_some());
    }
}
