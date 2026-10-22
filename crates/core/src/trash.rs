use std::path::Path;

use objc2::rc::autoreleasepool;
use objc2_foundation::{NSFileManager, NSString, NSURL};

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Leaves a tiny file in the user's real Trash: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn moves_file_to_real_trash() {
        let tmp = crate::testutil::TempDir::new("trash");
        let f = tmp.path().join("maccleaner-trash-test.txt");
        std::fs::write(&f, b"x").unwrap();
        move_to_trash(&f).unwrap();
        assert!(!f.exists());
    }
}
