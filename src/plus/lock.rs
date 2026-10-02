//! Advisory file locks under the herdr+ directory (`<dir>/<name>.lock`):
//! the watcher singleton, the registry read-modify-write and the message log
//! rotation. `File::lock` locks are per open file, so two handles in one
//! process exclude each other just like two processes do.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

/// A held lock; released when dropped (the file handle closes).
pub struct DirLock(#[allow(dead_code)] File); // the handle is held only to keep the lock

fn lock_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.lock"))
}

fn open(dir: &Path, name: &str) -> io::Result<File> {
    std::fs::create_dir_all(dir)?;
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path(dir, name))
}

/// Block until the lock is held.
pub fn exclusive(dir: &Path, name: &str) -> io::Result<DirLock> {
    let file = open(dir, name)?;
    file.lock()?;
    Ok(DirLock(file))
}

/// Take the lock if nobody holds it; `None` when it is held elsewhere.
pub fn try_exclusive(dir: &Path, name: &str) -> io::Result<Option<DirLock>> {
    let file = open(dir, name)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(DirLock(file))),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_excludes_others_until_dropped() {
        let dir = super::super::test_dir("lock");
        let held = exclusive(&dir, "watcher").unwrap();
        assert!(lock_path(&dir, "watcher").exists());
        assert!(try_exclusive(&dir, "watcher").unwrap().is_none());
        assert!(
            try_exclusive(&dir, "registry").unwrap().is_some(),
            "names are independent"
        );
        drop(held);
        let again = try_exclusive(&dir, "watcher").unwrap();
        assert!(again.is_some());
        drop(again);
        drop(exclusive(&dir, "watcher").unwrap());
        // The directory is created on demand.
        assert!(try_exclusive(&dir.join("sub"), "x").unwrap().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
