//! One fetch child at a time per cache file, across the sessions that all
//! find one cache overdue on the same tick.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

/// An exclusive advisory lock on a file, held for as long as the value
/// lives and released explicitly on drop. Closing the descriptor alone is
/// not enough: a child that another thread forked while the lock was held
/// carries a copy of the descriptor until its exec, and a flock lock lives
/// on the shared open file description, so the close would leave the lock
/// held for that window. A killed holder leaves nothing to take over: the
/// OS releases the lock at process exit. No holder ever unlinks a lock
/// path, so one holder can never remove another's lock; the proxy sweep
/// removes a session's lock only once it is a day old, and a hold lasts
/// seconds.
pub struct Lock {
    file: File,
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// The file is created once and never removed; `try_lock` is the one
/// atomic step. It is touched on every acquisition so the proxy sweep,
/// which removes session files by age, reads a held lock as fresh. A
/// filesystem that cannot lock at all (`Error`, not `WouldBlock`) gets the
/// unserialized fetch it had before, rather than none.
pub fn try_acquire(path: &Path) -> Option<Lock> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .ok()?;
    match file.try_lock() {
        Ok(()) | Err(TryLockError::Error(_)) => {
            let _ = file.set_modified(std::time::SystemTime::now());
            Some(Lock { file })
        }
        Err(TryLockError::WouldBlock) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_refuses_a_second_holder_until_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.lock");
        let first = try_acquire(&path).expect("free lock");
        assert!(try_acquire(&path).is_none());
        drop(first);
        assert!(try_acquire(&path).is_some());
        assert!(
            path.exists(),
            "the file stays; the lock lives on the open descriptor"
        );
    }

    #[test]
    fn a_losing_contender_leaves_the_holder_locked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.lock");
        let _first = try_acquire(&path).expect("free lock");
        // Each loser opens and closes the file; closing a losing descriptor
        // must not release the winner (flock semantics, not fcntl's).
        for _ in 0..3 {
            assert!(try_acquire(&path).is_none());
        }
    }

    #[test]
    fn a_missing_parent_directory_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep").join("usage.lock");
        assert!(try_acquire(&path).is_some());
    }

    #[test]
    fn acquiring_touches_the_file() {
        // The proxy sweep removes session files a day old by mtime, so a
        // held lock has to read as fresh.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.lock");
        std::fs::write(&path, "").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let _lock = try_acquire(&path).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(modified > old + std::time::Duration::from_secs(3600));
    }
}
