//! What each mechanism actually produced, area by area.
//!
//! `verify` reports the index's known unknowns — symbols without a definition, references
//! without a caller. This is a different question: not "what does the index know it is
//! missing" but **"did each mechanism produce anything, and does what it produced hold
//! together"**. The two failures look nothing alike. A missing definition is visible in
//! the data; a whole mechanism that ran and yielded nothing leaves no trace at all, and
//! every answer that depended on it comes back empty and confident.
//!
//! Three ways an area can be empty, and conflating them is the whole problem:
//!
//! * **not applicable** — there is no gRPC in this repository, so no links is correct.
//! * **not covered** — cairn has no indexer for this language. A boundary of the tool.
//! * **failing** — the inputs are there and the mechanism produced nothing anyway.
//!
//! The good case is called **indexed**, not verified, and the difference is a rung on a
//! ladder rather than a choice of synonym. cairn is an index: a pass that did its job has
//! indexed something, and nothing here executes anything to find out whether what it
//! indexed is true. That second question is real and this cannot answer it — a gRPC edge
//! is recovered from a generator's naming convention, so a pass can produce links that
//! count correctly and mean nothing. Confirming those is `verified`, it needs a check
//! that runs the query, and it can only ever apply to something already indexed.
//!
//! And it decays, like everything else here:
//!
//! ```text
//!   indexed -> verified -> (compose, Dockerfile, cron script or .proto changes)
//!           -> verify stale -> verified
//! ```
//!
//! The evidence a verification rests on is largely not source, so a rebuilt index does
//! not renew the claim and an edited deployment file does not disturb the index. Neither
//! side notices on its own — what the check has to leave behind is the commit it was made
//! against, so "changed since when" has a referent.

use crate::{Lang, Result, Store};

/// How much of an area can be trusted.
///
/// Two rungs, and the order is the point: **indexed → verified**. Nothing can be verified
/// that was not indexed first, because there is nothing to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// The pass ran and what it produced matches the tree it was built from.
    ///
    /// cairn is an index, so this is the word for it having done its job. It is a claim
    /// about two counts agreeing, which is all a deterministic pass can honestly assert
    /// about itself.
    Indexed,
    /// Indexed, and confirmed by a check that actually ran the query.
    ///
    /// The rung above, for the parts that being indexed does not settle. A gRPC edge is
    /// recovered from a generator's naming convention rather than resolved by a compiler,
    /// so "the pass produced links" and "those links are real" are different statements —
    /// and the second one cannot be reached by counting.
    ///
    /// Set by `cairn llm verify` recording a verdict against the commit checked out, and
    /// only ever on top of `Indexed`.
    Verified,
    /// Verified once, against inputs that have since changed.
    ///
    /// A verification is a claim with a date on it, not a stamp. The evidence it rested
    /// on is mostly *not* source — a compose file, a Dockerfile, a cron installer, a
    /// `.proto` — so nothing about a rebuilt index makes the claim true again, and
    /// nothing about editing those files makes the index look any different. Presented as
    /// current, that is the same silent staleness the envelope's `stale:` section exists
    /// to prevent, one level up.
    ///
    /// Not trouble: the area is still indexed and its answers are still usable. What
    /// lapsed is the stronger claim on top, so this reads as "worth re-running", not as
    /// "do not trust this".
    ///
    /// Needs one value recorded at verification time — the commit it was made against —
    /// and nothing more. Not a fingerprint of every input: git already holds what
    /// changed, and a diff beats a fingerprint here because a fingerprint says only
    /// *that* something moved while a diff says what, which is the difference between
    /// re-verifying one area and re-verifying all of them.
    ///
    /// The division that follows: cairn detects that the tree moved, the agent reads the
    /// diff and judges whether it matters. Whether an edited Dockerfile invalidates an
    /// entrypoint claim is exactly the judgement `llm verify` exists to make, and not one
    /// a comparison of hashes should be pretending to make on its behalf.
    VerifyStale,
    /// It produced output, and something named is missing from it.
    Partial,
    /// The inputs were present and it produced nothing. This is the one worth waking up
    /// for: every answer that rests on this area is silently empty.
    Failing,
    /// cairn can index this and did not. A toolchain that is not installed, an indexer
    /// that failed — recoverable, and nobody will recover it without being told.
    NotIndexed,
    /// cairn has no indexer for this at all. Not a fault; a limit, and grep is the tool.
    NotCovered,
    /// The question does not arise here.
    NotApplicable,
}

impl State {
    /// One register for the whole vocabulary.
    ///
    /// These are a closed set of names in one column, not sentences in prose. cairn does
    /// shout elsewhere — `STOP -`, `USE GREP FOR THIS ONE` — but those are capitals inside
    /// a paragraph, where they have lowercase to stand out against. In a column there is
    /// nothing to stand out against, and mixing the case only asks the reader to decode
    /// severity and identity from the same token. Severity is `is_trouble`, and it gets
    /// its own column.
    pub fn label(&self) -> &'static str {
        match self {
            State::Indexed => "indexed",
            State::Verified => "verified",
            State::VerifyStale => "verify stale",
            State::Partial => "partial",
            State::Failing => "failing",
            State::NotIndexed => "not indexed",
            State::NotCovered => "not covered",
            State::NotApplicable => "n/a",
        }
    }

    /// Whether an agent should treat answers from this area as usable.
    pub fn is_trouble(&self) -> bool {
        matches!(self, State::Partial | State::Failing | State::NotIndexed)
    }
}

/// One name for a language, whichever side it came from.
///
/// The tree walk names languages the way a person would (`python`, `TypeScript`) and the
/// index tags them the way SCIP does (`py`, `ts`). Joined raw, `python` never meets `py`
/// and the report shows the same language twice — once as fully indexed and once as
/// missing entirely, which is worse than either row alone.
fn normalise_lang(name: &str) -> String {
    match name.to_lowercase().as_str() {
        "py" | "python" => "python".to_string(),
        "ts" | "typescript" => "typescript".to_string(),
        "js" | "javascript" => "javascript".to_string(),
        "rs" | "rust" => "rust".to_string(),
        other => other.to_string(),
    }
}

/// One mechanism or language, and what became of it.
pub struct Area {
    pub name: String,
    pub state: State,
    /// The evidence, in one line. Counts rather than adjectives: a reader has to be able
    /// to disagree with the verdict, and cannot if the verdict is all they are given.
    pub detail: String,
}

/// A language the tree held when the index was built.
#[derive(Debug, Clone)]
pub struct TreeLanguage {
    pub name: String,
    pub files: i64,
    /// False when cairn has no indexer for it — the difference between "should be in
    /// here and is not" and "was never going to be".
    pub indexable: bool,
}

impl Store {
    /// Record what the tree held, so `status` can compare against it later.
    ///
    /// Persisted rather than re-walked: the question is whether the index covers the tree
    /// *it was built from*. Walking the tree again at query time would answer a different
    /// question and would quietly call an untouched index stale the moment someone added
    /// a file.
    pub fn set_tree_survey(&self, langs: &[TreeLanguage], protos: i64) -> Result<()> {
        for l in langs {
            let kind = if l.indexable { "found" } else { "unsupported" };
            self.set_meta(&format!("tree.{kind}.{}", l.name), &l.files.to_string())?;
        }
        self.set_meta("tree.protos", &protos.to_string())?;
        Ok(())
    }

    /// Record the projects an indexer could not build, so later answers can say so.
    ///
    /// `cairn index` printed which project failed and the fact went no further. Every
    /// command afterwards read an index missing a fifth of the repository and reported it
    /// as an index of the repository — the failure was known, once, by the only process
    /// that never gets asked a question. An index has to carry the shape of its own gaps
    /// or nothing downstream can.
    ///
    /// Cleared first, so a run that fixes a project stops reporting it as broken.
    pub fn set_failed_projects(&self, failed: &[(String, String)]) -> Result<()> {
        self.conn
            .execute("DELETE FROM meta WHERE key LIKE 'index.failed.%'", [])?;
        let mut by_lang: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for (lang, project) in failed {
            by_lang.entry(lang).or_default().push(project);
        }
        for (lang, mut projects) in by_lang {
            projects.sort();
            self.set_meta(&format!("index.failed.{lang}"), &projects.join("\n"))?;
        }
        Ok(())
    }

    /// Projects that failed to index, by language.
    pub fn failed_projects(&self) -> Result<std::collections::HashMap<String, Vec<String>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM meta WHERE key LIKE 'index.failed.%'")?;
        let rows = stmt.query_map([], |r| {
            let key: String = r.get(0)?;
            let value: String = r.get(1)?;
            Ok((
                key.trim_start_matches("index.failed.").to_string(),
                value.lines().map(str::to_string).collect::<Vec<String>>(),
            ))
        })?;
        let mut out = std::collections::HashMap::new();
        for r in rows {
            let (k, v) = r?;
            out.insert(k, v);
        }
        Ok(out)
    }

    /// Files in the index, per language tag.
    fn indexed_files_by_lang(&self) -> Result<std::collections::HashMap<String, i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT lang, count(*) FROM files GROUP BY lang")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                crate::Lang::from_i64(r.get::<_, i64>(0)?).tag().to_string(),
                r.get::<_, i64>(1)?,
            ))
        })?;
        let mut out = std::collections::HashMap::new();
        for r in rows {
            let (k, v) = r?;
            *out.entry(normalise_lang(&k)).or_insert(0) += v;
        }
        Ok(out)
    }

    /// What the tree held, or an empty list when nothing was recorded.
    ///
    /// Empty is normal: `cairn index <file.scip>` ingests a SCIP file somebody else
    /// produced and never walks a tree. It means "unknown", not "nothing", and the report
    /// says so rather than reporting an absence it did not observe.
    pub fn tree_survey(&self) -> Result<Vec<TreeLanguage>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM meta WHERE key LIKE 'tree.found.%' OR key LIKE 'tree.unsupported.%'")?;
        let rows = stmt.query_map([], |r| {
            let key: String = r.get(0)?;
            let value: String = r.get(1)?;
            let indexable = key.starts_with("tree.found.");
            let name = key
                .trim_start_matches("tree.found.")
                .trim_start_matches("tree.unsupported.")
                .to_string();
            Ok(TreeLanguage {
                name,
                files: value.parse().unwrap_or(0),
                indexable,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        out.sort_by_key(|l| std::cmp::Reverse(l.files));
        Ok(out)
    }

    fn tree_protos(&self) -> Result<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM meta WHERE key = 'tree.protos'")?;
        let mut rows = stmt.query([])?;
        Ok(match rows.next()? {
            Some(r) => r.get::<_, String>(0)?.parse().ok(),
            None => None,
        })
    }

    /// Indexed symbols per language.
    pub fn symbols_by_language(&self) -> Result<Vec<(Lang, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT lang, count(*) FROM symbols GROUP BY lang ORDER BY 2 DESC")?;
        let rows = stmt.query_map([], |r| Ok((Lang::from_i64(r.get(0)?), r.get::<_, i64>(1)?)))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Every area, in the order a reader should care about them.
    ///
    /// `head` is the commit that is checked out, used to age recorded verdicts. `None`
    /// means it could not be established, and then nothing counts as verified — an
    /// unknown difference and no difference look identical from here.
    pub fn coverage(&self, head: Option<&str>) -> Result<Vec<Area>> {
        let mut out = Vec::new();
        out.push(self.entrypoint_coverage()?);
        out.push(self.cross_api_coverage()?);
        out.extend(self.language_coverage()?);
        self.apply_verdicts(&mut out, head)?;
        Ok(out)
    }

    /// Lift areas onto the rung above, where something has judged them.
    ///
    /// Only ever applied to an area that is already `Indexed`. A pass that produced
    /// nothing cannot be verified into having produced something, and letting a judgement
    /// override a count would put an opinion where a fact belongs — the deterministic
    /// verdict is the one that has to survive disagreement.
    fn apply_verdicts(&self, areas: &mut [Area], head: Option<&str>) -> Result<()> {
        use crate::llmverify::Standing;
        let verdicts = self.verdicts()?;
        let checks = self.verification_checks()?;
        for area in areas.iter_mut() {
            if area.state != State::Indexed {
                continue;
            }
            let mine: Vec<&crate::llmverify::Check> =
                checks.iter().filter(|c| c.area == area.name).collect();
            if mine.is_empty() {
                continue;
            }
            let standings: Vec<Standing> = mine
                .iter()
                .map(|c| {
                    let v = verdicts.iter().find(|v| v.check_id == c.id);
                    self.standing(v, head)
                })
                .collect();

            // A single "this is wrong" is the news, whatever the rest say.
            if let Some(i) = standings.iter().position(|s| *s == Standing::Broken) {
                let note = verdicts
                    .iter()
                    .find(|v| v.check_id == mine[i].id)
                    .and_then(|v| v.note.clone())
                    .unwrap_or_else(|| "no reason recorded".into());
                // Said out loud, because this lands in the same column as a failure the
                // tool derived and the two are not the same kind of thing.
                area.state = State::Failing;
                area.detail = format!(
                    "{}; a recorded judgement says this is wrong: {note} (a judgement, not \
                     a derivation - re-check it before acting)",
                    area.detail
                );
                continue;
            }
            // Silence is the normal case and must not read as a warning.
            if standings.iter().all(|s| *s == Standing::Open) {
                continue;
            }
            let current = standings
                .iter()
                .filter(|s| **s == Standing::Current)
                .count();
            if current == standings.len() {
                area.state = State::Verified;
                area.detail = format!("{}; {current} check(s) confirmed", area.detail);
            } else {
                area.state = State::VerifyStale;
                area.detail = format!(
                    "{}; {current} of {} checks confirmed against the commit checked out, \
                     the rest were judged earlier or not at all - `cairn llm verify`",
                    area.detail,
                    standings.len()
                );
            }
        }
        Ok(())
    }

    fn entrypoint_coverage(&self) -> Result<Area> {
        let rows = self.entrypoints(None)?;
        if rows.is_empty() {
            return Ok(Area {
                name: "entrypoints".into(),
                state: State::NotApplicable,
                detail: "no compose file resolved, so nothing declares what starts what".into(),
            });
        }
        let resolved = rows.iter().filter(|e| e.entry_path.is_some()).count();
        let idle = rows.iter().filter(|e| e.idle).count();
        // An image this repository does not build has no code here to resolve to, so it is
        // an answer rather than a gap - the same distinction `Entrypoint::idle` already
        // makes, one case further. Counted and named, not folded into `unresolved`:
        // measured on a real monorepo, 25 of 31 "unresolved" services were postgres, nats
        // and other registry images, which made a working mechanism read as a broken one.
        // Not `idle && external` twice: the four groups must partition the rows, or the
        // detail line sums to more entrypoints than there are - the fixture's one idle
        // sidecar is also an unbuilt image, and the first version of this counted it in
        // both, printing "4 entrypoints, 3 resolve to code, 1 idle, 1 from images..."
        let external = rows
            .iter()
            .filter(|e| e.external && e.entry_path.is_none() && !e.idle)
            .count();
        let unresolved: Vec<&str> = rows
            .iter()
            .filter(|e| e.entry_path.is_none() && !e.idle && !e.external)
            .map(|e| e.service.as_str())
            .collect();
        let base = format!(
            "{} entrypoints, {resolved} resolve to code{}{}",
            rows.len(),
            if idle > 0 {
                format!(", {idle} idle")
            } else {
                String::new()
            },
            if external > 0 {
                format!(", {external} from images this repository does not build")
            } else {
                String::new()
            }
        );
        Ok(if unresolved.is_empty() {
            Area {
                name: "entrypoints".into(),
                state: State::Indexed,
                detail: base,
            }
        } else {
            // An unresolved entrypoint declares everything only it runs to be dead code,
            // which is why this is partial rather than a footnote on a pass.
            Area {
                name: "entrypoints".into(),
                state: State::Partial,
                detail: format!("{base}; unresolved: {}", unresolved.join(", ")),
            }
        })
    }

    fn cross_api_coverage(&self) -> Result<Area> {
        let (services, serves, calls) = self.link_counts()?;
        let protos = self.tree_protos()?;
        let name = "cross-api".to_string();
        // No protobuf in the tree means no answer is owed. Reporting that as a failure is
        // how a single-language repository gets told its index is broken.
        if protos == Some(0) {
            return Ok(Area {
                name,
                state: State::NotApplicable,
                detail: "no .proto files in the tree; `cairn reaches` has nothing to answer".into(),
            });
        }
        if services == 0 {
            return Ok(match protos {
                // Protos are there and none parsed: the mechanism ran and produced
                // nothing, which is the case `reaches` answers emptily and confidently.
                Some(n) => Area {
                    name,
                    state: State::Failing,
                    detail: format!(
                        "{n} .proto files in the tree, 0 services parsed - `cairn reaches` \
                         will find nothing, and that is this mechanism failing rather than \
                         an answer"
                    ),
                },
                // No survey to compare against, so neither reading can be ruled out.
                None => Area {
                    name,
                    state: State::NotApplicable,
                    detail: "no services parsed, and no record of whether this repository \
                             has any protobuf"
                        .into(),
                },
            });
        }
        // One side only. Serving without callers is a real state for an edge service, but
        // it is also exactly what a naming convention that stopped matching looks like.
        Ok(if calls == 0 {
            Area {
                name,
                state: State::Partial,
                detail: format!(
                    "{services} services, {serves} serve links, 0 call links - nothing was \
                     found calling a generated client, so `reaches` answers in one direction"
                ),
            }
        } else {
            Area {
                name,
                state: State::Indexed,
                detail: format!("{services} gRPC services, {serves} serve, {calls} call links"),
            }
        })
    }

    fn language_coverage(&self) -> Result<Vec<Area>> {
        use std::collections::BTreeMap;
        let mut indexed: BTreeMap<String, i64> = BTreeMap::new();
        for (lang, n) in self.symbols_by_language()? {
            if lang != Lang::Unknown {
                *indexed.entry(normalise_lang(lang.tag())).or_insert(0) += n;
            }
        }
        let tree = self.tree_survey()?;
        let failed = self.failed_projects()?;
        let indexed_files = self.indexed_files_by_lang()?;
        let mut out = Vec::new();

        for l in &tree {
            let symbols = indexed.remove(&normalise_lang(&l.name)).unwrap_or(0);
            out.push(match (l.indexable, symbols) {
                (false, _) => Area {
                    name: l.name.clone(),
                    state: State::NotCovered,
                    detail: format!(
                        "{} files in the tree, no indexer exists - grep is the tool for this code",
                        l.files
                    ),
                },
                // The recoverable one, and it looks identical to "this language is not
                // here" from every other command's output.
                (true, 0) => Area {
                    name: l.name.clone(),
                    state: State::NotIndexed,
                    detail: format!(
                        "{} files in the tree, 0 symbols in the index - the indexer did \
                         not run or produced nothing; re-run `cairn index` and read what \
                         it prints",
                        l.files
                    ),
                },
                // `Indexed` is documented as two counts agreeing, and nothing compared
                // them. A workspace whose retired app fails on a broken lockfile read
                // `indexed  25053 symbols from 1079 files` — the file count being the
                // tree's, so the sentence claimed the symbols came from files the index
                // had never opened, and the state claimed a match that had not been
                // checked. `Partial` already existed for exactly this and was unreachable.
                //
                // Which projects failed is recorded by the index run rather than inferred
                // from a shortfall: counts differ a little for honest reasons — 15 of the
                // backend's 1184 Python files hold no symbol — and a threshold over that
                // noise would be a number nobody could defend.
                (true, n) => {
                    let held = indexed_files
                        .get(&normalise_lang(&l.name))
                        .copied()
                        .unwrap_or(0);
                    match failed.get(&normalise_lang(&l.name)) {
                        Some(projects) if !projects.is_empty() => Area {
                            name: l.name.clone(),
                            state: State::Partial,
                            detail: format!(
                                "{n} symbols from {held} of {} files - {} did not index, so \
                                 answers about that code are absent rather than empty; \
                                 re-run `cairn index` and read what it prints",
                                l.files,
                                projects.join(", ")
                            ),
                        },
                        _ => Area {
                            name: l.name.clone(),
                            state: State::Indexed,
                            detail: format!("{n} symbols from {held} files"),
                        },
                    }
                }
            });
        }

        // Indexed without a survey behind it: a SCIP file ingested directly. Real, and
        // there is nothing to check it against, so it is reported as what it is.
        //
        // A recorded failure still counts here. It was dropped: this branch reported
        // `indexed` for a language whose project had failed, because the *comparison*
        // was unavailable — but "I could not compare the counts" and "nothing failed" are
        // different statements, and the second one was being made from the absence of the
        // first. The failure is a fact the index run wrote down; it does not need a tree
        // walk to be true.
        for (name, n) in indexed {
            let key = normalise_lang(&name);
            match failed.get(&key) {
                Some(projects) if !projects.is_empty() => out.push(Area {
                    name,
                    state: State::Partial,
                    detail: format!(
                        "{n} symbols, ingested from SCIP with no tree walk to compare - and \
                         {} did not index, so answers about that code are absent rather \
                         than empty",
                        projects.join(", ")
                    ),
                }),
                _ => out.push(Area {
                    name,
                    state: State::Indexed,
                    detail: format!("{n} symbols, ingested from SCIP with no tree walk to compare"),
                }),
            }
        }

        if tree.is_empty() && out.is_empty() {
            out.push(Area {
                name: "languages".into(),
                state: State::Failing,
                detail: "no symbols in any language - this index is empty".into(),
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_ways_of_being_empty_are_not_the_same_state() {
        // The distinction this whole module exists for: an agent that reads "no gRPC
        // links" needs to know whether that is the repository, the tool's limits, or a
        // broken mechanism, because the right next move differs in all three.
        assert!(!State::NotApplicable.is_trouble());
        assert!(!State::NotCovered.is_trouble());
        assert!(State::Failing.is_trouble());
        assert!(State::NotIndexed.is_trouble());
        assert!(!State::Indexed.is_trouble());
        assert!(!State::Verified.is_trouble());
    }

    #[test]
    fn the_vocabulary_is_one_register() {
        // A closed set of names in one column. Spelling severity into some of them asks
        // the reader to decode two things from one token, and severity already has a
        // column of its own.
        for s in [
            State::Indexed,
            State::Verified,
            State::VerifyStale,
            State::Partial,
            State::Failing,
            State::NotIndexed,
            State::NotCovered,
            State::NotApplicable,
        ] {
            let l = s.label();
            assert_eq!(l, l.to_lowercase(), "{l} breaks the register");
        }
    }

    #[test]
    fn a_lapsed_verification_does_not_make_the_index_untrustworthy() {
        // The area is still indexed and still answers. Marking it as trouble would put
        // it beside a pass that produced nothing, and the two call for opposite moves:
        // one wants `llm verify` run again, the other wants the index rebuilt.
        assert!(!State::VerifyStale.is_trouble());
        assert!(State::Failing.is_trouble());
    }

    #[test]
    fn nothing_is_verified_without_being_indexed_first() {
        // The ladder is the model: an area that produced nothing cannot be judged into
        // having produced something. Letting a verdict lift anything below `indexed`
        // would put an opinion where a derivation belongs, and the derivation is the one
        // that has to survive disagreement.
        let dir = std::env::temp_dir().join("cairn-coverage-ladder");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::reset(&dir.join("index.sqlite")).unwrap();
        store.set_tree_survey(&[], 0).unwrap();
        // Nothing is indexed here, so this verdict answers a claim the index never makes.
        store
            .record_verdict(
                "entrypoints/whatever",
                true,
                None,
                Some("aaa"),
                "entrypoints",
                "2026-08-04T00:00:00Z",
            )
            .unwrap();
        assert!(
            store
                .coverage(Some("aaa"))
                .unwrap()
                .iter()
                .all(|a| a.state != State::Verified && a.state != State::VerifyStale),
            "a verdict against a claim this index does not make must not lift anything"
        );
    }

    #[test]
    fn a_survey_round_trips_and_keeps_which_side_it_was_on() {
        let dir = std::env::temp_dir().join("cairn-coverage-survey");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::reset(&dir.join("index.sqlite")).unwrap();
        store
            .set_tree_survey(
                &[
                    TreeLanguage {
                        name: "python".into(),
                        files: 120,
                        indexable: true,
                    },
                    TreeLanguage {
                        name: "TypeScript".into(),
                        files: 41,
                        indexable: false,
                    },
                ],
                7,
            )
            .unwrap();
        let back = store.tree_survey().unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].name, "python");
        assert_eq!(back[0].files, 120);
        assert!(back[0].indexable);
        assert_eq!(back[1].name, "TypeScript");
        assert!(
            !back[1].indexable,
            "a language cairn cannot index must not come back as one it can, or the \
             report turns a boundary of the tool into a fault to go and fix"
        );
        assert_eq!(store.tree_protos().unwrap(), Some(7));
    }

    #[test]
    fn the_tree_and_the_index_spell_a_language_differently_and_still_meet() {
        // The tree walk names languages the way a person would, SCIP tags them its own
        // way. Joined raw, the same language appears twice - fully indexed under one name
        // and missing entirely under the other - and the second row sends someone to fix
        // an indexer that is working.
        assert_eq!(normalise_lang("python"), normalise_lang("py"));
        assert_eq!(normalise_lang("TypeScript"), normalise_lang("ts"));
        assert_eq!(normalise_lang("go"), "go");
        assert_ne!(normalise_lang("python"), normalise_lang("go"));
        // Caught on a real indexed repository (ripgrep), not a fixture: `rust` (the
        // tree walk's name) and `rs` (Lang::tag's) never met, so `status`/`verify`
        // printed Rust twice - "not indexed" under one spelling, "indexed, 8602
        // symbols" under the other - which is exactly the false "mechanism did not
        // run" this function exists to prevent.
        assert_eq!(normalise_lang("rust"), normalise_lang("rs"));
    }

    #[test]
    fn no_protobuf_in_the_tree_is_not_a_broken_cross_language_pass() {
        let dir = std::env::temp_dir().join("cairn-coverage-noproto");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::reset(&dir.join("index.sqlite")).unwrap();
        store.set_tree_survey(&[], 0).unwrap();
        let area = store.cross_api_coverage().unwrap();
        assert_eq!(area.state, State::NotApplicable);

        // Same zero links, protos present: now it is a failure, and the difference is
        // the only thing that tells a reader which one they are looking at.
        store.set_meta("tree.protos", "3").unwrap();
        let area = store.cross_api_coverage().unwrap();
        assert_eq!(area.state, State::Failing);
        assert!(area.detail.contains("3 .proto files"));
    }
}

#[cfg(test)]
mod partial_index {
    use super::*;

    #[test]
    fn a_project_that_failed_to_index_makes_the_language_partial_not_indexed() {
        // `Indexed` is documented as "the pass ran and what it produced matches the tree
        // it was built from", and nothing compared the two. A workspace whose retired app
        // fails on a broken lockfile reported `indexed  25053 symbols from 1079 files`
        // while the index held 841 - the state asserting a match nobody checked, and the
        // sentence quoting the tree's file count as though the symbols came from it.
        let store = Store::open_in_memory().unwrap();
        store
            .set_tree_survey(
                &[TreeLanguage {
                    name: "typescript".into(),
                    files: 1079,
                    indexable: true,
                }],
                0,
            )
            .unwrap();
        store.set_meta("tree.protos", "0").unwrap();
        seed_files(&store, 841);

        // Clean first: nothing recorded as failed.
        let areas = store.coverage(None).unwrap();
        let ts = areas.iter().find(|a| a.name == "typescript").unwrap();
        assert_eq!(ts.state, State::Indexed);

        store
            .set_failed_projects(&[("typescript".into(), "apps/web_legacy".into())])
            .unwrap();
        let areas = store.coverage(None).unwrap();
        let ts = areas.iter().find(|a| a.name == "typescript").unwrap();
        assert_eq!(ts.state, State::Partial, "{}", ts.detail);
        assert!(ts.state.is_trouble());
        assert!(ts.detail.contains("apps/web_legacy"), "{}", ts.detail);
        assert!(
            ts.detail.contains("841 of 1079"),
            "both counts, so the gap is visible: {}",
            ts.detail
        );
    }

    #[test]
    fn a_run_that_fixes_the_project_stops_reporting_it() {
        // The record has to be cleared, or an index rebuilt successfully goes on
        // advertising a failure that is over - which teaches the reader to ignore it.
        let store = Store::open_in_memory().unwrap();
        store
            .set_failed_projects(&[("typescript".into(), "apps/web_legacy".into())])
            .unwrap();
        assert!(!store.failed_projects().unwrap().is_empty());
        store.set_failed_projects(&[]).unwrap();
        assert!(store.failed_projects().unwrap().is_empty());
    }

    fn seed_files(store: &Store, n: i64) {
        let c = &store.conn;
        for i in 0..n {
            c.execute(
                "INSERT OR IGNORE INTO strings(s) VALUES (?1)",
                rusqlite::params![format!("apps/web/f{i}.ts")],
            )
            .unwrap();
            let sid: i64 = c
                .query_row(
                    "SELECT id FROM strings WHERE s = ?1",
                    rusqlite::params![format!("apps/web/f{i}.ts")],
                    |r| r.get(0),
                )
                .unwrap();
            c.execute(
                "INSERT INTO files(path_id, lang, is_test, generated) VALUES (?1, 3, 0, 0)",
                rusqlite::params![sid],
            )
            .unwrap();
        }
        // One symbol, so the language counts as produced at all.
        c.execute(
            "INSERT INTO symbols(hash, name_id, kind, lang, ref_count, def_file_id)
             VALUES (?1, 1, 2, 3, 0, 1)",
            rusqlite::params![vec![7u8; 16]],
        )
        .unwrap();
    }
}

#[cfg(test)]
mod entrypoint_classification_tests {
    use crate::{State, Store};

    /// Four groups that must partition the entrypoints, and the one that was missing.
    ///
    /// Before this, any service without a resolved entry path counted as unresolved, which
    /// put the mechanism in `Partial` and told every answer that rested on it to doubt
    /// itself. On a real monorepo that meant 46 entrypoints, 15 resolving, and 25 of the 31
    /// "unresolved" being postgres, nats and other registry images — services with no code
    /// in this repository to resolve to, so the mechanism had answered them completely. A
    /// working layer read as a broken one, which is the same defect as reporting `n/a` as a
    /// failure: a gap note that fires where there is no gap teaches a reader to skim the
    /// one line the whole design puts the honesty in.
    fn store_with(services: &[(&str, Option<&str>, Option<&str>, &str)]) -> Store {
        let s = Store::open_in_memory().expect("in-memory store");
        for (name, build_context, image, command) in services {
            s.conn
                .execute(
                    "INSERT INTO deploy_services(name, build_context, image, command, ports)
                     VALUES (?1, ?2, ?3, ?4, '')",
                    rusqlite::params![name, build_context, image, command],
                )
                .expect("insert service");
        }
        s
    }

    #[test]
    fn an_image_this_repo_does_not_build_is_an_answer_not_a_gap() {
        let s = store_with(&[(
            "postgres",
            None,
            Some("postgres:16"),
            "postgres -c max_connections=200",
        )]);
        let area = s.entrypoint_coverage().expect("coverage");
        assert_eq!(area.state, State::Indexed, "detail was: {}", area.detail);
        assert!(
            area.detail
                .contains("1 from images this repository does not build"),
            "{}",
            area.detail
        );
        assert!(!area.state.is_trouble());
    }

    #[test]
    fn a_service_this_repo_does_build_and_cannot_resolve_is_still_a_gap() {
        // The case the change must not swallow: cairn builds this one, so an unresolved
        // command means everything only it runs looks like dead code.
        let s = store_with(&[("api", Some("."), None, "./some-binary serve")]);
        let area = s.entrypoint_coverage().expect("coverage");
        assert_eq!(area.state, State::Partial, "detail was: {}", area.detail);
        assert!(area.detail.contains("api"), "{}", area.detail);
        assert!(area.state.is_trouble());
    }

    #[test]
    fn the_groups_partition_so_the_counts_never_exceed_the_total() {
        // The first version counted the fixture's idle sidecar as both idle and external,
        // printing "4 entrypoints, 3 resolve to code, 1 idle, 1 from images..." - five
        // things out of four. The golden gate caught it.
        let s = store_with(&[
            ("db", None, Some("postgres:16"), "postgres"),
            ("sidecar", None, Some("busybox"), "sleep infinity"),
            ("api", Some("."), None, "./bin/api"),
        ]);
        let area = s.entrypoint_coverage().expect("coverage");
        let n = |word: &str| -> usize {
            area.detail
                .split(", ")
                .find(|p| p.contains(word))
                .and_then(|p| p.split_whitespace().next())
                .and_then(|d| d.parse().ok())
                .unwrap_or(0)
        };
        assert_eq!(n("entrypoints"), 3, "{}", area.detail);
        assert!(
            n("resolve to code") + n("idle") + n("images this repository") <= 3,
            "groups overlap: {}",
            area.detail
        );
    }
}
