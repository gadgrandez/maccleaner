//! How cleaned items are removed: moved to the Trash (the default) or, when the user turns it
//! on, deleted for good.

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::rc::autoreleasepool;
use objc2_foundation::{NSFileManager, NSString, NSURL};

use crate::rules::Action;

static DELETE_PERMANENTLY: AtomicBool = AtomicBool::new(false);

/// The user's choice for the cleanups of this process: delete instead of moving to the Trash.
pub fn set_delete_permanently(on: bool) {
    DELETE_PERMANENTLY.store(on, Ordering::Relaxed);
}

/// How cleanups that would move items to the Trash remove them right now.
pub fn preferred_action() -> Action {
    if DELETE_PERMANENTLY.load(Ordering::Relaxed) { Action::Delete } else { Action::Trash }
}

/// Removes `path` with `action`; returns the action done.
pub fn discard(path: &Path, action: Action) -> Result<Action, String> {
    match action {
        Action::Trash => move_to_trash(path),
        Action::Delete => remove(path).map_err(|e| e.to_string()),
    }
    .map(|()| action)
}

/// Moves to the Trash via `NSFileManager` (supports "Put Back" from Finder).
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    let s = path.to_str().ok_or_else(|| "path is not UTF-8".to_string())?;
    autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(s));
        let fm = NSFileManager::defaultManager();
        fm.trashItemAtURL_resultingItemURL_error(&url, None)
            .map_err(|e| e.localizedDescription().to_string())
    })
}

/// Deletes `path` for good. Entries can't be removed from read-only folders (Go's module cache
/// is read-only on purpose), so on a permission error the owner's write permission is
/// restored on every folder inside and the deletion is retried, as Finder does.
pub fn remove(path: &Path) -> std::io::Result<()> {
    if !fs::symlink_metadata(path)?.is_dir() {
        return fs::remove_file(path);
    }
    match fs::remove_dir_all(path) {
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            make_writable(path);
            fs::remove_dir_all(path)
        }
        result => result,
    }
}

/// Adds the owner's rwx to `dir` and the folders below it (symlinks are not followed).
fn make_writable(dir: &Path) {
    let Ok(md) = fs::symlink_metadata(dir) else { return };
    if !md.is_dir() {
        return;
    }
    let mode = md.permissions().mode();
    if mode & 0o700 != 0o700 {
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(mode | 0o700));
    }
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            make_writable(&entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn trash_is_the_default() {
        assert_eq!(preferred_action(), Action::Trash);
    }

    /// Leaves a tiny file in the user's real Trash: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn moves_file_to_real_trash() {
        let tmp = TempDir::new("trash");
        let f = tmp.path().join("maccleaner-trash-test.txt");
        fs::write(&f, b"x").unwrap();
        move_to_trash(&f).unwrap();
        assert!(!f.exists());
    }
}
