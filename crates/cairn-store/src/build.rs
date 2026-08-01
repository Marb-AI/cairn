//! Rebuilding an index without taking it away from readers.
//!
//! Measured: while `cairn index` ran, twelve of twelve concurrent reads failed. Not with a
//! lock error — the rebuild is not one transaction, so a reader saw a database whose schema
//! version had already been wiped and whose tables were half filled. Honest (it says the
//! index is incomplete) but unavailable, for the thirty-six seconds a rebuild takes. A tool
//! an agent is supposed to reach for reflexively cannot be unavailable for half a minute
//! whenever the code changes.
//!
//! So a rebuild writes to `<db>.building` and, only when it has finished, renames it over
//! the live file. `rename(2)` is atomic: a reader either opens the old index or the new
//! one, never a mixture. Readers that already hold the old file keep it until they close —
//! POSIX keeps the inode alive — so a query in flight finishes against consistent data.
//!
//! A lock file beside the index stops two rebuilds racing, and carries the pid of the
//! process holding it. A build that dies leaves the lock behind; the next one sees that the
//! recorded pid is gone and takes over, rather than refusing forever and needing a human to
//! delete a file they have to know about.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Where a rebuild assembles the new index before it replaces the old one.
pub fn building_path(db: &Path) -> PathBuf {
    with_suffix(db, "building")
}

fn lock_path(db: &Path) -> PathBuf {
    with_suffix(db, "lock")
}

fn with_suffix(db: &Path, suffix: &str) -> PathBuf {
    let mut s = db.as_os_str().to_os_string();
    s.push(".");
    s.push(suffix);
    PathBuf::from(s)
}

/// Held for the duration of a rebuild; releases on drop, including on unwind.
pub struct BuildLock {
    path: PathBuf,
}

impl BuildLock {
    /// Take the lock, or explain who holds it.
    ///
    /// A stale lock — one naming a process that no longer exists — is taken over rather
    /// than reported. The alternative is a tool that stays broken after a crash until
    /// someone finds out which file to delete.
    pub fn acquire(db: &Path) -> Result<BuildLock> {
        let path = lock_path(db);
        if let Ok(text) = std::fs::read_to_string(&path) {
            let holder: Option<u32> = text
                .lines()
                .find_map(|l| l.strip_prefix("pid="))
                .and_then(|v| v.trim().parse().ok());
            match holder {
                Some(pid) if pid_alive(pid) => anyhow::bail!(
                    "another `cairn index` is running (pid {pid}). Wait for it, or remove \
                     {} if you are sure it is not",
                    path.display()
                ),
                Some(pid) => {
                    eprintln!(
                        "cairn: taking over a lock left by pid {pid}, which is gone"
                    );
                }
                None => {}
            }
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        std::fs::write(&path, format!("pid={}\n", std::process::id()))
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(BuildLock { path })
    }
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Is a process with this id still around?
///
/// Signal 0 performs the permission and existence checks without delivering anything,
/// which is the portable way to ask. A pid we may not signal is still a live process, so
/// `EPERM` counts as alive.
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(pid as i32, 0) };
        if rc == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
}

/// Put the freshly built index in place of the live one, atomically.
///
/// The WAL and shared-memory sidecars are the reason this is not a bare rename: they belong
/// to the file they were written beside, and leaving the old ones next to the new database
/// is how a "successful" rebuild produces a corrupt index.
pub fn promote(building: &Path, db: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm"] {
        let stale = PathBuf::from(format!("{}{suffix}", db.display()));
        let _ = std::fs::remove_file(stale);
        let from = PathBuf::from(format!("{}{suffix}", building.display()));
        let _ = std::fs::remove_file(from);
    }
    std::fs::rename(building, db).with_context(|| {
        format!("promoting {} to {}", building.display(), db.display())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_is_released_when_it_goes_out_of_scope() {
        let dir = std::env::temp_dir().join("cairn-build-lock-test");
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("index.sqlite");
        {
            let _held = BuildLock::acquire(&db).unwrap();
            assert!(lock_path(&db).exists());
            assert!(BuildLock::acquire(&db).is_err(), "a live lock must not be taken twice");
        }
        assert!(!lock_path(&db).exists());
    }

    #[test]
    fn a_lock_left_by_a_dead_process_is_taken_over() {
        let dir = std::env::temp_dir().join("cairn-build-lock-stale");
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("index.sqlite");
        // A pid that cannot be running: the kernel would have to have wrapped all the way
        // around, and the point is that a crash must not need a human with a shovel.
        std::fs::write(lock_path(&db), "pid=4294967290\n").unwrap();
        let held = BuildLock::acquire(&db);
        assert!(held.is_ok(), "a stale lock should be taken over, not refused");
    }

    #[test]
    fn the_current_process_is_alive_and_pid_zero_is_not() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(0));
    }
}
