//! Concept entry point: turning "the OAuth stuff" into a set of symbols.
//!
//! Every other command needs a symbol name or a handle. An agent dropped into an
//! unfamiliar repo has neither, so without this the first move of every session is
//! still a grep — the exact loop the tool exists to remove (architecture 6.4).
//!
//! The seed is gathered cheaply and in a fixed order, and **which source matched is
//! reported**, because "this came from a concept someone recorded" and "this came from
//! a fuzzy name match" deserve very different amounts of trust:
//!
//!   1. concepts     — someone already said what this means (18.6). Strongest.
//!   2. names/paths  — `*Auth*`, `/auth/`. Primitive, and right surprisingly often.
//!   3. documentation— the bridge for terms that appear in no identifier. SCIP
//!      carries it for 77.7 % of Python symbols on the target repo.
//!   4. test names   — the best description of a feature a project usually has.
//!
//! What deliberately does *not* happen here is an LLM call. The caller is one
//! (architecture D15, 6.4): if the seed is weak, saying so and handing over candidates
//! beats guessing badly on their behalf.

use crate::{Store, SymbolRow};
use anyhow::Result;
use rusqlite::params;
use std::collections::HashMap;

/// Where a seed came from. Ordered by how much it should be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SeedSource {
    Concept,
    Name,
    Doc,
    Test,
}

impl SeedSource {
    pub fn label(self) -> &'static str {
        match self {
            SeedSource::Concept => "concept",
            SeedSource::Name => "name",
            SeedSource::Doc => "doc",
            SeedSource::Test => "test",
        }
    }

    /// Weight used to rank a seed. Concepts are asserted knowledge, a name match is
    /// evidence, prose is weaker evidence, and a test name is a hint.
    fn weight(self) -> f64 {
        match self {
            SeedSource::Concept => 10.0,
            SeedSource::Name => 4.0,
            SeedSource::Doc => 2.0,
            SeedSource::Test => 1.5,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Seed {
    pub symbol: SymbolRow,
    pub score: f64,
    /// Every source that produced this symbol, so an answer can say why it is here.
    pub sources: Vec<SeedSource>,
}

#[derive(Debug, Clone, Default)]
pub struct ContextResult {
    pub seeds: Vec<Seed>,
    /// True when nothing scored above the confidence floor. The caller is told to fall
    /// back rather than being handed weak guesses dressed as answers.
    pub low_confidence: bool,
    pub concepts_matched: Vec<String>,
}

/// Coarse shape of a symbol, for ranking. Deliberately not the full kind list: the
/// only distinction that matters here is whether a symbol could plausibly *be* the
/// thing someone asked about.
enum Shape {
    Type,
    Callable,
    Module,
    Value,
}

fn kind_of(kind: i64) -> Shape {
    match kind {
        0 => Shape::Module,
        1 => Shape::Type,
        3 => Shape::Callable,
        _ => Shape::Value,
    }
}

/// Below this the seed is noise and saying so is more useful than returning it.
const CONFIDENCE_FLOOR: f64 = 3.0;

impl Store {
    pub fn context(&self, query: &str, limit: usize) -> Result<ContextResult> {
        let mut scores: HashMap<i64, (f64, Vec<SeedSource>)> = HashMap::new();
        let mut result = ContextResult::default();

        let mut add = |id: i64, src: SeedSource, boost: f64| {
            let e = scores.entry(id).or_insert((0.0, Vec::new()));
            e.0 += src.weight() * boost;
            if !e.1.contains(&src) {
                e.1.push(src);
            }
        };

        // 1. Concepts: someone already named this. Both the concept's own name and its
        // note are matched, since "billing" may only appear in the description.
        {
            let mut stmt = self.conn.prepare(
                "SELECT c.ns, c.name, l.symbol_hash
                   FROM k.concepts c
                   JOIN k.concept_links l ON l.concept_id = c.id
                  WHERE lower(c.name) LIKE ?1 OR lower(coalesce(c.note,'')) LIKE ?1",
            )?;
            let pattern = format!("%{}%", query.to_lowercase());
            let rows = stmt.query_map(params![pattern], |r| {
                Ok((
                    format!("{}/{}", r.get::<_, String>(0)?, r.get::<_, String>(1)?),
                    r.get::<_, Vec<u8>>(2)?,
                ))
            })?;
            for row in rows {
                let (name, hash) = row?;
                if !result.concepts_matched.contains(&name) {
                    result.concepts_matched.push(name);
                }
                if let Some(id) = self.id_for_hash(&hash)? {
                    add(id, SeedSource::Concept, 1.0);
                }
            }
        }

        // 2-4. Names, documentation and tests, in one pass over the symbol table.
        // LIKE rather than FTS: the corpus is small enough (43k interned strings) that
        // a scan is well inside the latency budget, and it avoids an index that would
        // have to be kept in step with every write.
        let pattern = format!("%{}%", query.to_lowercase());
        let mut stmt = self.conn.prepare(
            r#"
            SELECT s.id,
                   lower(n.s) LIKE ?1                       AS name_hit,
                   lower(coalesce(m.s,'')) LIKE ?1          AS module_hit,
                   lower(coalesce(s.doc,'')) LIKE ?1        AS doc_hit,
                   coalesce(f.is_test, 0)                   AS is_test,
                   lower(n.s) = ?2                          AS exact,
                   s.ref_count,
                   coalesce(f.generated, 0)                 AS generated,
                   s.kind
              FROM symbols s
              JOIN strings n ON n.id = s.name_id
              LEFT JOIN strings m ON m.id = s.module_id
              LEFT JOIN files   f ON f.id = s.def_file_id
             WHERE s.def_file_id IS NOT NULL
               AND (lower(n.s) LIKE ?1
                    OR lower(coalesce(m.s,'')) LIKE ?1
                    OR lower(coalesce(s.doc,'')) LIKE ?1)
            "#,
        )?;
        let rows = stmt.query_map(params![pattern, query.to_lowercase()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)? != 0,
                r.get::<_, i64>(2)? != 0,
                r.get::<_, i64>(3)? != 0,
                r.get::<_, i64>(4)? != 0,
                r.get::<_, i64>(5)? != 0,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)? != 0,
                r.get::<_, i64>(8)?,
            ))
        })?;
        for row in rows {
            let (id, name_hit, module_hit, doc_hit, is_test, exact, refs, generated, kind) = row?;
            // Centrality as a mild tie-breaker: a symbol nothing references is rarely
            // the answer to "show me the X part of this system".
            let central = 1.0 + (refs as f64).min(50.0) / 100.0;
            // Generated code is where a term appears most often and means least. Asked
            // about "quota", the first version returned protobuf message fields named
            // `quota` and buried `QuotaModule`, whose own documentation says it is the
            // quota client. Suppress hard rather than exclude: a term that only exists
            // in generated code should still surface something.
            let gen_penalty = if generated { 0.05 } else { 1.0 };
            // What the symbol *is* matters as much as where the term appeared. A type
            // or a function can be "the X part of the system"; a field cannot.
            let kind_weight = match kind_of(kind) {
                Shape::Type => 1.6,
                Shape::Callable => 1.4,
                Shape::Module => 1.2,
                Shape::Value => 0.35,
            };
            let base = central * gen_penalty * kind_weight;
            if name_hit || module_hit {
                add(id, SeedSource::Name, if exact { 2.5 } else { 1.0 } * base);
            }
            if doc_hit {
                // Prose is where a concept is actually described, so a documentation
                // hit on a type is worth more than a name collision on a field.
                add(id, SeedSource::Doc, base * 1.5);
            }
            if is_test && (name_hit || doc_hit) {
                // A test is a hint about where a feature lives, not the feature.
                add(id, SeedSource::Test, 0.6);
            }
        }

        let mut ranked: Vec<(i64, f64, Vec<SeedSource>)> = scores
            .into_iter()
            .map(|(id, (score, srcs))| (id, score, srcs))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let best = ranked.first().map(|r| r.1).unwrap_or(0.0);
        result.low_confidence = best < CONFIDENCE_FLOOR;

        for (id, score, mut sources) in ranked.into_iter().take(limit) {
            let Some(symbol) = self.symbol(id)? else {
                continue;
            };
            sources.sort();
            result.seeds.push(Seed {
                symbol,
                score,
                sources,
            });
        }
        Ok(result)
    }

    fn id_for_hash(&self, hash: &[u8]) -> Result<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id FROM symbols WHERE hash = ?1")?;
        let mut rows = stmt.query(params![hash])?;
        Ok(match rows.next()? {
            Some(r) => Some(r.get(0)?),
            None => None,
        })
    }
}
