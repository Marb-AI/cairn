//! Installing the agent-facing guide into a repository.
//!
//! cairn is not much use to an agent that does not know it is there. The guide in
//! `skill/SKILL.md` is what tells it — when to reach for the tool and, more importantly,
//! when not to — and until now it was a file in this repository that somebody had to find,
//! read and copy by hand. That is a step nobody will take, so the tool takes it.
//!
//! Baked into the binary rather than fetched or read from disk: cairn ships as one file
//! with nothing beside it, and a guide that can be missing is a guide that will be.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The guide itself, as published.
const SKILL: &str = include_str!("../../../skill/SKILL.md");

/// Where an agent looks for a project's skills.
const SKILL_DIR: &str = ".claude/skills/cairn";

/// What installing did, so the caller can say something useful and nothing more.
pub enum Installed {
    Written(PathBuf),
    /// No path: nothing is printed for this case, so carrying one would be a field that
    /// exists to be ignored.
    AlreadyCurrent,
}

/// Write the guide into `repo`, replacing an older copy.
///
/// Overwrites without asking, because the file is ours and a stale one is worse than none:
/// it would describe commands that have since changed shape. Left alone when the content
/// already matches, so `cairn index` on an unchanged repository does not keep touching a
/// file someone may have in version control.
pub fn install(repo: &Path) -> Result<Installed> {
    let dir = repo.join(SKILL_DIR);
    let path = dir.join("SKILL.md");
    if std::fs::read_to_string(&path).is_ok_and(|existing| existing == SKILL) {
        return Ok(Installed::AlreadyCurrent);
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(&path, SKILL).with_context(|| format!("writing {}", path.display()))?;
    Ok(Installed::Written(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guide_is_not_empty_and_names_the_tool() {
        // A build that quietly embedded the wrong file would otherwise install nothing
        // useful and say it had succeeded.
        assert!(SKILL.len() > 500, "the embedded guide looks truncated");
        assert!(SKILL.contains("cairn"));
    }

    #[test]
    fn installing_writes_it_and_then_leaves_it_alone() {
        let repo = std::env::temp_dir().join("cairn-skill-install");
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();

        let first = install(&repo).unwrap();
        assert!(matches!(first, Installed::Written(_)));
        let path = repo.join(SKILL_DIR).join("SKILL.md");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SKILL);

        assert!(matches!(install(&repo).unwrap(), Installed::AlreadyCurrent));
    }

    #[test]
    fn an_outdated_copy_is_replaced() {
        let repo = std::env::temp_dir().join("cairn-skill-stale");
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(repo.join(SKILL_DIR)).unwrap();
        std::fs::write(repo.join(SKILL_DIR).join("SKILL.md"), "an older guide").unwrap();

        assert!(matches!(install(&repo).unwrap(), Installed::Written(_)));
        assert_eq!(
            std::fs::read_to_string(repo.join(SKILL_DIR).join("SKILL.md")).unwrap(),
            SKILL
        );
    }
}
