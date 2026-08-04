//! Markdown as structure, not as prose.
//!
//! The skill has always said: prose, comments, config and docs are grep's job. That still
//! holds for *finding a word*. This is a different question, and the reason it is worth
//! answering separately is that markdown in a codebase stopped being what it used to be.
//! It is largely written by agents now, which means it is genuinely structured — headings
//! that mean something, conventions in lists, rules under the heading that names them.
//! The structure is there to be read; nothing was reading it.
//!
//! What it costs today: an agent looking for a convention does not know which of forty
//! documents holds it, so it greps (and gets the word in the wrong four files) or reads
//! several whole documents to find one paragraph. Both are paid in full before anything
//! is learned.
//!
//! So the shape is the same progressive disclosure the code side uses. Not
//! handle → skeleton → body, but **document → section → line range**: a map of what
//! exists, then the sections of one document with what each would cost to read, then the
//! exact lines. Nothing here stores prose. Headings, line ranges and word counts only —
//! enough to decide what to open, which is the decision that is being made badly.
//!
//! Not a markdown renderer and not trying to be. ATX headings, aware of fenced code and
//! front matter, and that is the whole parser. Setext underlines are not handled: `---`
//! is also a thematic break and a front-matter fence, and guessing between them would put
//! invented sections in a map whose only job is to be trusted.

use crate::{Result, Store};
use rusqlite::params;
use std::path::Path;

/// Directories that hold no documentation worth mapping and cost a lot to walk.
const SKIP: &[&str] = &[
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

const MAX_DEPTH: usize = 12;

/// One heading, and the span it governs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub level: usize,
    pub heading: String,
    /// Headings above it, joined — so a section can be found by where it sits rather than
    /// only by what it is called. Half the `## Rules` headings in a repository are
    /// interchangeable; `contributing.md > Reviews > Rules` is not.
    pub trail: String,
    /// One-based, inclusive. The heading line itself.
    pub start_line: usize,
    /// One-based, inclusive. Where the next heading of this level or shallower begins,
    /// minus one — so reading `start..=end` gets the section and nothing else.
    pub end_line: usize,
    /// Words in the body, excluding the heading. What it would cost to read, stated
    /// before it is read: that is the only reason this is a map rather than a listing.
    pub words: usize,
}

/// One document.
pub struct Document {
    pub path: String,
    /// The first level-1 heading, when there is one. Not the filename: `README.md` is
    /// four hundred of them and none of them says what it is about.
    pub title: Option<String>,
    pub lines: usize,
    pub sections: Vec<Section>,
}

/// Split a markdown file into its headings.
///
/// `#` inside a fenced block is a shell comment, a Python comment, or a heading in an
/// example — never a heading of this document. Getting that wrong fills the map with
/// sections that do not exist, and a map with invented entries is worse than none.
pub fn parse(text: &str) -> (Option<String>, Vec<Section>, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let mut sections: Vec<Section> = Vec::new();
    let mut title: Option<String> = None;
    let mut fence: Option<String> = None;
    let mut trail: Vec<String> = Vec::new();
    // Front matter, when the file opens with one. Its `---` fences are not headings and
    // its keys are not prose, but a section starting at line 1 would swallow it.
    let mut start = 0usize;
    if lines.first().map(|l| l.trim_end()) == Some("---") {
        if let Some(end) = lines.iter().skip(1).position(|l| l.trim_end() == "---") {
            start = end + 2;
        }
    }

    for (i, raw) in lines.iter().enumerate().skip(start) {
        let line = raw.trim_end();
        let trimmed = line.trim_start();

        // Fences: ``` or ~~~, closed by the same character. The opening run can be longer
        // than three and the closer must be at least as long, but treating any run of the
        // same character as a close is enough here and cannot leave a fence open forever.
        if let Some(open) = &fence {
            if trimmed.starts_with(open.as_str()) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = Some(trimmed.chars().take(3).collect());
            continue;
        }

        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        // `#hashtag` is not a heading; ATX needs a space. And seven hashes is not a
        // level-seven heading, it is text.
        if hashes == 0 || hashes > 6 || !trimmed[hashes..].starts_with(' ') {
            continue;
        }
        let heading = trimmed[hashes..].trim().trim_end_matches('#').trim();
        if heading.is_empty() {
            continue;
        }
        if hashes == 1 && title.is_none() {
            title = Some(heading.to_string());
        }

        // Close every section this heading ends, and rebuild the trail to sit under it.
        for s in sections.iter_mut().rev() {
            if s.end_line == 0 && s.level >= hashes {
                s.end_line = i; // the line before this one, one-based
            }
        }
        trail.truncate(hashes.saturating_sub(1));
        while trail.len() < hashes.saturating_sub(1) {
            // A jump from `#` to `###` leaves a hole. Named rather than filled with an
            // empty string, so a trail always reads as a path someone could follow.
            trail.push("(unnamed)".to_string());
        }
        trail.push(heading.to_string());

        sections.push(Section {
            level: hashes,
            heading: heading.to_string(),
            trail: trail.join(" > "),
            start_line: i + 1,
            end_line: 0,
            words: 0,
        });
    }

    for s in sections.iter_mut() {
        if s.end_line == 0 {
            s.end_line = lines.len();
        }
    }
    // Counted after the spans are closed, over the body only. A heading's own words are
    // in the heading, and counting them twice would make short sections look expensive.
    for s in sections.iter_mut() {
        s.words = lines
            .iter()
            .skip(s.start_line) // past the heading line itself
            .take(s.end_line.saturating_sub(s.start_line))
            .map(|l| l.split_whitespace().count())
            .sum();
    }
    (title, sections, lines.len())
}

/// Every markdown file under `repo`, parsed.
pub fn scan(repo: &Path) -> Vec<Document> {
    let mut paths = Vec::new();
    walk(repo, 0, &mut paths);
    paths.sort();
    let mut out = Vec::new();
    for p in paths {
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        let rel = p
            .strip_prefix(repo)
            .unwrap_or(&p)
            .to_string_lossy()
            .to_string();
        let (title, sections, lines) = parse(&text);
        out.push(Document {
            path: rel,
            title,
            lines,
            sections,
        });
    }
    out
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<std::path::PathBuf>) {
    if depth > MAX_DEPTH || out.len() > 5000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if SKIP.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            walk(&p, depth + 1, out);
        } else if name.to_lowercase().ends_with(".md") {
            out.push(p);
        }
    }
}

/// How a section answered a search.
///
/// Two different claims, and collapsing them would throw away the one thing this can say
/// that grep cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    /// The heading, or one above it, names the thing. The section is *about* it.
    Heading,
    /// The words appear in the body this many times. It *comes up* here.
    Body(usize),
}

/// A section as it comes back from the index.
pub struct SectionRow {
    pub path: String,
    pub title: Option<String>,
    pub level: usize,
    pub heading: String,
    pub trail: String,
    pub start_line: usize,
    pub end_line: usize,
    pub words: usize,
}

/// A document as it comes back from the index.
pub struct DocumentRow {
    pub path: String,
    pub title: Option<String>,
    pub lines: usize,
    pub sections: usize,
    pub words: usize,
    /// Top-level headings, for the map. Enough to say what a document is about without
    /// opening it, which is the whole point of having a map.
    pub top: Vec<String>,
}

impl Store {
    /// Replace the document layer.
    pub fn link_docs(&mut self, repo: &Path) -> Result<(usize, usize)> {
        let docs = scan(repo);
        let tx = self.conn.transaction()?;
        tx.execute_batch("DELETE FROM doc_sections; DELETE FROM doc_files;")?;
        let mut files = 0usize;
        let mut sections = 0usize;
        {
            let mut ins_doc = tx.prepare(
                "INSERT INTO doc_files(path, title, lines) VALUES (?1, ?2, ?3) RETURNING id",
            )?;
            let mut ins_sec = tx.prepare(
                "INSERT INTO doc_sections(doc_id, level, heading, trail, start_line,
                                          end_line, words)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for d in &docs {
                let id: i64 =
                    ins_doc.query_row(params![d.path, d.title, d.lines as i64], |r| r.get(0))?;
                files += 1;
                for s in &d.sections {
                    ins_sec.execute(params![
                        id,
                        s.level as i64,
                        s.heading,
                        s.trail,
                        s.start_line as i64,
                        s.end_line as i64,
                        s.words as i64
                    ])?;
                    sections += 1;
                }
            }
        }
        tx.commit()?;
        Ok((files, sections))
    }

    /// The map: every document, with what it is called and what it covers.
    pub fn documents(&self) -> Result<Vec<DocumentRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.id, f.path, f.title, f.lines,
                    (SELECT count(*) FROM doc_sections s WHERE s.doc_id = f.id),
                    (SELECT coalesce(sum(words), 0) FROM doc_sections s WHERE s.doc_id = f.id)
               FROM doc_files f ORDER BY f.path",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)? as usize,
                r.get::<_, i64>(4)? as usize,
                r.get::<_, i64>(5)? as usize,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (id, path, title, lines, sections, words) = r?;
            let mut top_stmt = self.conn.prepare_cached(
                "SELECT heading FROM doc_sections
                  WHERE doc_id = ?1 AND level = (SELECT min(level) FROM doc_sections
                                                  WHERE doc_id = ?1 AND level > 1)
                  ORDER BY start_line",
            )?;
            let top: Vec<String> = top_stmt
                .query_map(params![id], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<_, _>>()?;
            out.push(DocumentRow {
                path,
                title,
                lines,
                sections,
                words,
                top,
            });
        }
        Ok(out)
    }

    /// Every section of one document.
    pub fn document_sections(&self, path: &str) -> Result<Vec<SectionRow>> {
        self.sections_where("f.path = ?1", params![path])
    }

    /// Sections whose heading or trail names these words.
    pub fn sections_about(&self, query: &str) -> Result<Vec<SectionRow>> {
        let like = format!("%{}%", query.trim().to_lowercase());
        self.sections_where(
            "lower(s.trail) LIKE ?1 OR lower(s.heading) LIKE ?1",
            params![like],
        )
    }

    /// Sections that name this, and sections that merely mention it.
    ///
    /// The distinction is the product. A heading match says the section is *about* the
    /// thing; a body match says it comes up there. Grep can find neither, because grep
    /// does not know where a section begins or ends — it returns a line and leaves the
    /// reader to guess how much around it to take, which is how one word costs four
    /// whole files.
    ///
    /// Bodies are read from disk at query time rather than stored. Ten documents are a
    /// few hundred kilobytes and reading them costs milliseconds; keeping a copy of the
    /// prose in the index would be a second, staler copy of something already on disk,
    /// and this layer's whole claim is that it holds structure and not text.
    pub fn sections_matching(&self, repo: &Path, query: &str) -> Result<Vec<(SectionRow, Hit)>> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let named: Vec<SectionRow> = self.sections_about(query)?;
        let named_keys: std::collections::HashSet<(String, usize)> = named
            .iter()
            .map(|s| (s.path.clone(), s.start_line))
            .collect();

        let mut out: Vec<(SectionRow, Hit)> =
            named.into_iter().map(|s| (s, Hit::Heading)).collect();

        // One pass per document, then each hit is attributed to the deepest section whose
        // span contains it — the smallest range that answers, rather than the chapter.
        let all = self.sections_where("1 = 1", params![])?;
        let mut by_path: std::collections::BTreeMap<String, Vec<SectionRow>> =
            std::collections::BTreeMap::new();
        for s in all {
            by_path.entry(s.path.clone()).or_default().push(s);
        }
        for (path, sections) in by_path {
            let Ok(text) = std::fs::read_to_string(repo.join(&path)) else {
                continue;
            };
            let mut counts: std::collections::HashMap<usize, usize> =
                std::collections::HashMap::new();
            for (i, line) in text.lines().enumerate() {
                if !line.to_lowercase().contains(&needle) {
                    continue;
                }
                let n = i + 1;
                if let Some(best) = sections
                    .iter()
                    .filter(|s| s.start_line <= n && n <= s.end_line)
                    .max_by_key(|s| s.level)
                {
                    *counts.entry(best.start_line).or_insert(0) += 1;
                }
            }
            for s in sections {
                let Some(n) = counts.get(&s.start_line) else {
                    continue;
                };
                // Already reported as a heading match, and the stronger claim wins.
                if named_keys.contains(&(s.path.clone(), s.start_line)) {
                    continue;
                }
                out.push((s, Hit::Body(*n)));
            }
        }
        // Named first: "this section is about it" outranks "it comes up here", and a
        // reader working down the list should meet the strong answers before the weak.
        //
        // Mentions rank by density, not by count. Raw count prefers whatever is longest,
        // because longer text contains more of everything — a 1900-word preamble with
        // three mentions beat a 94-word section with one, and the range handed back was
        // the whole file. That is the thing this command exists to stop being the answer.
        // Ties go to the shorter range: between two that both mention it, cheaper wins.
        let density = |s: &SectionRow, n: usize| n as f64 / (s.words.max(1) as f64);
        out.sort_by(|a, b| match (&a.1, &b.1) {
            (Hit::Heading, Hit::Body(_)) => std::cmp::Ordering::Less,
            (Hit::Body(_), Hit::Heading) => std::cmp::Ordering::Greater,
            (Hit::Body(x), Hit::Body(y)) => density(&b.0, *y)
                .total_cmp(&density(&a.0, *x))
                .then(a.0.words.cmp(&b.0.words)),
            _ => (&a.0.path, a.0.start_line).cmp(&(&b.0.path, b.0.start_line)),
        });
        Ok(out)
    }

    fn sections_where(&self, cond: &str, p: impl rusqlite::Params) -> Result<Vec<SectionRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT f.path, f.title, s.level, s.heading, s.trail, s.start_line, s.end_line,
                    s.words
               FROM doc_sections s JOIN doc_files f ON f.id = s.doc_id
              WHERE {cond}
              ORDER BY f.path, s.start_line"
        ))?;
        let rows = stmt.query_map(p, |r| {
            Ok(SectionRow {
                path: r.get(0)?,
                title: r.get(1)?,
                level: r.get::<_, i64>(2)? as usize,
                heading: r.get(3)?,
                trail: r.get(4)?,
                start_line: r.get::<_, i64>(5)? as usize,
                end_line: r.get::<_, i64>(6)? as usize,
                words: r.get::<_, i64>(7)? as usize,
            })
        })?;
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
    fn a_hash_inside_a_fence_is_not_a_heading() {
        // The bug this parser exists to not have. Every second document in a codebase has
        // a shell block in it, and every one of those lines starts with `#`.
        let (_, sections, _) =
            parse("# Real\n\ntext\n\n```sh\n# not a heading\n## nor this\n```\n\n## Also real\n");
        let names: Vec<&str> = sections.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(names, ["Real", "Also real"]);
    }

    #[test]
    fn a_section_ends_where_the_next_one_of_its_level_begins() {
        // The line range is the product: it is what turns "this document mentions it"
        // into "read these thirty lines".
        let text = "# Top\nintro\n\n## A\naaa\nbbb\n\n## B\nccc\n";
        let (_, s, lines) = parse(text);
        assert_eq!(lines, 9);
        let a = s.iter().find(|s| s.heading == "A").unwrap();
        assert_eq!((a.start_line, a.end_line), (4, 7));
        let b = s.iter().find(|s| s.heading == "B").unwrap();
        assert_eq!((b.start_line, b.end_line), (8, 9));
        // `# Top` runs to the end of the file: a level-1 section is not closed by a
        // level-2 one.
        let top = s.iter().find(|s| s.heading == "Top").unwrap();
        assert_eq!(top.end_line, 9);
    }

    #[test]
    fn the_trail_says_where_a_section_sits() {
        // Half the `## Rules` headings in a repository are interchangeable. Where one
        // sits is what tells them apart, and it is what a reader is actually searching by.
        let (_, s, _) = parse("# Guide\n## Reviews\n### Rules\n## Setup\n### Rules\n");
        let trails: Vec<&str> = s
            .iter()
            .filter(|s| s.heading == "Rules")
            .map(|s| s.trail.as_str())
            .collect();
        assert_eq!(trails, ["Guide > Reviews > Rules", "Guide > Setup > Rules"]);
    }

    #[test]
    fn front_matter_is_not_content() {
        let (title, s, _) = parse("---\ntitle: x\ntags: [a]\n---\n\n# Real\nbody\n");
        assert_eq!(title.as_deref(), Some("Real"));
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].start_line, 6);
    }

    #[test]
    fn what_is_not_a_heading_stays_out_of_the_map() {
        let (_, s, _) = parse("#hashtag\n####### seven\n#\n# Real\n");
        let names: Vec<&str> = s.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(
            names,
            ["Real"],
            "invented sections make a map worth less than none"
        );
    }

    #[test]
    fn being_about_something_outranks_mentioning_it() {
        // The distinction grep cannot draw, and the reason this is not just grep with
        // extra steps. One of these sections is the answer; the other says the words.
        let dir = std::env::temp_dir().join("cairn-docs-search");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("guide.md"),
            "# Guide\n\n## Exit codes\nThe contract.\n\n## Testing\nCheck the exit codes \
             here and the exit codes there.\n",
        )
        .unwrap();
        let mut store = Store::reset(&dir.join("index.sqlite")).unwrap();
        store.link_docs(&dir).unwrap();

        let hits = store.sections_matching(&dir, "exit codes").unwrap();
        assert_eq!(hits[0].0.heading, "Exit codes");
        assert_eq!(hits[0].1, Hit::Heading, "a heading match comes first");
        let testing = hits.iter().find(|(s, _)| s.heading == "Testing").unwrap();
        assert_eq!(
            testing.1,
            Hit::Body(1),
            "one line mentions it twice, and the unit is the line"
        );
        // The section that names it is not also reported as mentioning it: the stronger
        // claim wins rather than the row appearing twice under two labels.
        assert_eq!(
            hits.iter()
                .filter(|(s, _)| s.heading == "Exit codes")
                .count(),
            1
        );
    }

    #[test]
    fn a_long_section_does_not_win_by_being_long() {
        // Measured, and it cost the comparison a query: ranking mentions by raw count
        // handed back a whole README, because a 1900-word preamble mentioning something
        // three times outranked a short section mentioning it once. Longer text contains
        // more of everything, so counting alone always ends at the largest range - which
        // is the answer this command exists to stop giving.
        let dir = std::env::temp_dir().join("cairn-docs-density");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let filler = "padding words here and there. ".repeat(80);
        std::fs::write(
            dir.join("g.md"),
            format!("# Long\nwidget {filler} widget {filler} widget\n\n## Short\nwidget here\n"),
        )
        .unwrap();
        let mut store = Store::reset(&dir.join("index.sqlite")).unwrap();
        store.link_docs(&dir).unwrap();
        let hits = store.sections_matching(&dir, "widget").unwrap();
        assert_eq!(
            hits[0].0.heading, "Short",
            "the dense short section is the cheaper answer and must come first"
        );
        assert!(hits[0].0.words < hits[1].0.words);
    }

    #[test]
    fn a_mention_is_attributed_to_the_smallest_section_that_holds_it() {
        // Every hit is inside `# Guide` too. Reporting the chapter would hand back four
        // hundred lines to answer a question that lives in twelve, which is the cost this
        // whole layer exists to remove.
        let dir = std::env::temp_dir().join("cairn-docs-innermost");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("g.md"),
            "# Guide\n\n## Setup\n\n### Docker\nuse the widget here\n\n## Other\nnothing\n",
        )
        .unwrap();
        let mut store = Store::reset(&dir.join("index.sqlite")).unwrap();
        store.link_docs(&dir).unwrap();
        let hits = store.sections_matching(&dir, "widget").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.heading, "Docker");
        assert_eq!(hits[0].0.trail, "Guide > Setup > Docker");
    }

    #[test]
    fn a_section_says_what_it_would_cost_to_read() {
        // Stated before it is read, which is the entire reason this is a map and not a
        // listing: the decision being made badly today is what to open.
        let (_, s, _) = parse("# T\n## A\none two three\n## B\n");
        let a = s.iter().find(|s| s.heading == "A").unwrap();
        assert_eq!(a.words, 3);
        let b = s.iter().find(|s| s.heading == "B").unwrap();
        assert_eq!(b.words, 0, "an empty section is worth knowing is empty");
    }
}
