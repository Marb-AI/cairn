//! Claims the index makes that the index cannot check.
//!
//! Everything else in this crate is a deterministic pass: the same tree gives the same
//! rows, and a row is either derived or absent. That is the tool's strength and it has one
//! blind spot — a pass can produce output that *counts* correctly and *means* nothing. A
//! gRPC edge is recovered from a generator's naming convention rather than resolved by a
//! compiler, and two packages can carry the same service name to different processes. The
//! links are there, they are consistent, and they are wrong.
//!
//! No model is called from here, and none needs to be. cairn is a CLI an agent drives
//! (architecture D1), and the agent already is one. So this is not a checker; it is a
//! **protocol**: cairn states a claim it cannot settle, names the evidence and what would
//! falsify it, and takes a verdict back. The judgement happens where the judgement lives.
//!
//! Three rules the design does not bend on:
//!
//! * **Advisory, never a gate.** No verdict changes an exit code and none blocks a
//!   command. The moment a deterministic tool refuses to work because a non-deterministic
//!   check disagreed, the determinism that makes it trustworthy is gone.
//! * **Unverified is cheap and normal.** Most indexes will never be verified and that must
//!   read as ordinary, not as a warning. A report that nags becomes a report nobody reads.
//! * **A verdict says where it came from.** `failing` from counting is a fact; `failing`
//!   from a judgement is an opinion that may be wrong. They land in the same column, so
//!   the text has to keep them apart.

use crate::{Result, Store};
use rusqlite::params;

/// A claim put to a judgement, with everything needed to settle it.
pub struct Check {
    /// Stable across rebuilds: derived from the claim, not from any row id. A rebuilt
    /// index re-derives the same claim and finds its own verdict again.
    pub id: String,
    pub area: String,
    /// What the index asserts, in one sentence.
    pub claim: String,
    /// How to go and look. Commands and paths, not prose: the point is that the reader
    /// does not have to work out where to start.
    pub evidence: Vec<String>,
    /// What would make this false. Stated because a check with no failure mode described
    /// gets confirmed by default, which is worse than not asking.
    pub falsifier: String,
}

/// What was recorded about a check, if anything.
pub struct Verdict {
    pub check_id: String,
    pub area: String,
    pub holds: bool,
    pub note: Option<String>,
    pub commit_sha: Option<String>,
    pub recorded_at: String,
}

/// Where a check stands right now.
#[derive(Debug, PartialEq, Eq)]
pub enum Standing {
    /// Never judged.
    Open,
    /// Judged against the commit that is checked out.
    Current,
    /// Judged against a different commit, or against one that could not be determined.
    Expired,
    /// Judged, and the judgement was that the index is wrong.
    Broken,
}

/// Turn a repo-ish string into something usable as an id fragment.
fn slug(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').chars().take(60).collect()
}

impl Store {
    /// Every claim worth putting to a judgement.
    ///
    /// Deliberately not "everything that could be checked". A plan long enough to skim is
    /// a plan nobody works through, and the areas here are the two where being indexed
    /// genuinely does not settle the question: what a deployment actually starts, and
    /// whether a convention-recovered service boundary is real.
    pub fn verification_checks(&self) -> Result<Vec<Check>> {
        let mut out = Vec::new();

        // An entrypoint is a chain of parses - compose command, Dockerfile hop, cron line,
        // runner script - each of which is right until a repository does it slightly
        // differently. The file it lands in is the one thing a reader can check in one
        // look, so that is what the claim is about.
        for e in self.entrypoints(None)? {
            let Some(path) = &e.entry_path else {
                continue;
            };
            let command = e.command.as_deref().unwrap_or("(image default)");
            out.push(Check {
                id: format!(
                    "entrypoints/{}-{}",
                    slug(&e.service),
                    slug(&e.trigger.label())
                ),
                area: "entrypoints".into(),
                claim: format!(
                    "`{}` runs `{command}` ({}), and that lands in {path}",
                    e.service,
                    e.trigger.label()
                ),
                evidence: vec![
                    format!("cairn outline {path}"),
                    match &e.script {
                        Some(s) => format!("read {s} - the runner this was recovered from"),
                        None => "read the compose file's command for this service".into(),
                    },
                ],
                falsifier: format!(
                    "{path} is not what `{command}` executes - a different module, a \
                     wrapper that hands off elsewhere, or an entrypoint overridden at \
                     deploy time"
                ),
            });
        }

        // The convention-recovered edge. Both sides are real code; whether they are two
        // ends of one boundary is a naming argument, and naming arguments are exactly what
        // a compiler is not doing here.
        for (pkg, service, serves, calls) in self.proto_service_summary()? {
            out.push(Check {
                id: format!("cross-api/{}", slug(&format!("{pkg}.{service}"))),
                area: "cross-api".into(),
                claim: format!(
                    "{pkg}.{service} is one service boundary: {serves} symbol(s) serve it \
                     and {calls} call it, in different languages"
                ),
                evidence: vec![
                    format!("cairn symbol {service} - both sides, if they are both there"),
                    "read the .proto that declares it, and the generated client the callers use"
                        .into(),
                ],
                falsifier: format!(
                    "the two sides are different services that share a name. The package \
                     is the thing to check: two packages can carry `{service}` to \
                     different processes, and this edge would look identical"
                ),
            });
        }

        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// gRPC services with how many symbols sit on each side of them.
    fn proto_service_summary(&self) -> Result<Vec<(String, String, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT ps.pkg, ps.name,
                    sum(CASE WHEN l.role = 0 THEN 1 ELSE 0 END),
                    sum(CASE WHEN l.role = 1 THEN 1 ELSE 0 END)
               FROM proto_services ps
               LEFT JOIN service_links l ON l.service_id = ps.id
              GROUP BY ps.id
              ORDER BY 1, 2",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                r.get::<_, Option<i64>>(3)?.unwrap_or(0),
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Record a judgement. Overwrites any earlier one for the same check.
    ///
    /// Overwrites rather than accumulates: the question is what is believed now, and a
    /// history of superseded opinions is a thing to maintain rather than a thing to read.
    pub fn record_verdict(
        &self,
        check_id: &str,
        holds: bool,
        note: Option<&str>,
        commit_sha: Option<&str>,
        area: &str,
        now: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO k.verifications(check_id, area, holds, note, commit_sha, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(check_id) DO UPDATE SET
                 area = excluded.area, holds = excluded.holds, note = excluded.note,
                 commit_sha = excluded.commit_sha, recorded_at = excluded.recorded_at",
            params![check_id, area, holds as i64, note, commit_sha, now],
        )?;
        Ok(())
    }

    pub fn verdicts(&self) -> Result<Vec<Verdict>> {
        let mut stmt = self.conn.prepare(
            "SELECT check_id, area, holds, note, commit_sha, recorded_at
               FROM k.verifications ORDER BY check_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Verdict {
                check_id: r.get(0)?,
                area: r.get(1)?,
                holds: r.get::<_, i64>(2)? != 0,
                note: r.get(3)?,
                commit_sha: r.get(4)?,
                recorded_at: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Where a check stands against the commit that is checked out.
    ///
    /// `head` of `None` means the commit could not be determined, and then nothing can be
    /// current: an unknown difference and no difference look identical, and treating the
    /// first as the second is the silent staleness the whole envelope exists to prevent.
    pub fn standing(&self, verdict: Option<&Verdict>, head: Option<&str>) -> Standing {
        match verdict {
            None => Standing::Open,
            Some(v) if !v.holds => Standing::Broken,
            Some(v) => match (v.commit_sha.as_deref(), head) {
                (Some(a), Some(b)) if a == b => Standing::Current,
                _ => Standing::Expired,
            },
        }
    }
}

/// The commit that is checked out, when that can be established.
///
/// Shelled out rather than parsed: `.git/HEAD` can be a ref, a packed ref, a worktree
/// pointer or a detached hash, and re-implementing that is a lot of surface for a value
/// that is allowed to be absent. Absent is a supported answer here — it costs a verdict
/// its currency, and nothing else.
pub fn head_commit(repo: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// True when the working tree has changes git has not been told about.
///
/// A verdict made over a dirty tree is anchored to a commit that does not describe what
/// was looked at, so it is recorded and immediately treated as expired rather than
/// refused: refusing would make the check unusable in exactly the situation - mid-change -
/// where someone most wants to run it.
pub fn tree_is_dirty(repo: &std::path::Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["status", "--porcelain"])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(holds: bool, sha: Option<&str>) -> Verdict {
        Verdict {
            check_id: "x".into(),
            area: "entrypoints".into(),
            holds,
            note: None,
            commit_sha: sha.map(|s| s.to_string()),
            recorded_at: "2026-08-04T00:00:00Z".into(),
        }
    }

    #[test]
    fn a_verdict_expires_when_the_tree_moves_under_it() {
        let store = Store::open_in_memory().unwrap();
        let v = verdict(true, Some("aaa"));
        assert_eq!(store.standing(Some(&v), Some("aaa")), Standing::Current);
        assert_eq!(store.standing(Some(&v), Some("bbb")), Standing::Expired);
    }

    #[test]
    fn a_commit_that_cannot_be_determined_is_never_current() {
        // An unknown difference and no difference look the same. Calling the first one
        // current is how a verification quietly outlives what it was about.
        let store = Store::open_in_memory().unwrap();
        assert_eq!(
            store.standing(Some(&verdict(true, Some("aaa"))), None),
            Standing::Expired
        );
        assert_eq!(
            store.standing(Some(&verdict(true, None)), Some("aaa")),
            Standing::Expired
        );
        assert_eq!(store.standing(None, Some("aaa")), Standing::Open);
    }

    #[test]
    fn a_broken_verdict_outranks_whether_it_is_current() {
        // "This is wrong" does not stop being worth saying because the tree has moved.
        // Ageing it into an ordinary expiry would lose the one verdict that carries news.
        let store = Store::open_in_memory().unwrap();
        let v = verdict(false, Some("aaa"));
        assert_eq!(store.standing(Some(&v), Some("bbb")), Standing::Broken);
        assert_eq!(store.standing(Some(&v), None), Standing::Broken);
    }

    #[test]
    fn a_verdict_is_keyed_by_the_claim_so_it_survives_a_rebuild() {
        let store = Store::open_in_memory().unwrap();
        store
            .record_verdict(
                "entrypoints/alert-worker-start",
                true,
                Some("worker.py really is the loop"),
                Some("aaa"),
                "entrypoints",
                "2026-08-04T00:00:00Z",
            )
            .unwrap();
        // Same claim judged again: one row, the later opinion.
        store
            .record_verdict(
                "entrypoints/alert-worker-start",
                false,
                Some("it hands off to a supervisor"),
                Some("bbb"),
                "entrypoints",
                "2026-08-05T00:00:00Z",
            )
            .unwrap();
        let all = store.verdicts().unwrap();
        assert_eq!(all.len(), 1, "a check holds one verdict, the current one");
        assert!(!all[0].holds);
        assert_eq!(all[0].commit_sha.as_deref(), Some("bbb"));
    }

    #[test]
    fn what_is_written_to_the_authored_side_is_still_there_next_time() {
        // Not a test of this module so much as of the ground it stands on. The sidecar
        // could not be created by ATTACH - the read path opens the projection without
        // `CREATE` and an attached database inherits that - so it fell back to a memory
        // database, and every note, link, concept and verdict ever written was accepted,
        // acknowledged and dropped. Nothing covered it because every test that touched
        // authored knowledge used one connection and never reopened.
        let dir = std::env::temp_dir().join("cairn-knowledge-durable");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("index.sqlite");
        {
            let store = Store::reset(&db).unwrap();
            store
                .set_meta("schema_version", &crate::schema::SCHEMA_VERSION.to_string())
                .unwrap();
        }
        {
            let store = Store::open(&db).unwrap();
            assert!(
                store.knowledge_is_durable(),
                "a writable directory must give a real sidecar, not a memory stand-in"
            );
            store
                .record_verdict("a/b", true, Some("looked"), Some("aaa"), "a", "t")
                .unwrap();
        }
        // The part that was broken: a second process.
        let store = Store::open(&db).unwrap();
        let all = store.verdicts().unwrap();
        assert_eq!(all.len(), 1, "the verdict did not survive reopening");
        assert_eq!(all[0].note.as_deref(), Some("looked"));
    }

    #[test]
    fn an_id_survives_being_built_from_whatever_a_repository_calls_things() {
        assert_eq!(slug("alert-worker"), "alert-worker");
        assert_eq!(slug("cron 30 3 * * *"), "cron-30-3");
        assert_eq!(slug("telemetry.Collector"), "telemetry.collector");
        assert!(slug(&"x".repeat(200)).len() <= 60);
    }
}
