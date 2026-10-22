//! Disk access permissions (TCC).
//!
//! Without "Full Disk Access", opening Desktop, Documents or Downloads makes macOS show a
//! dialog and blocks the read until the user responds; if the dialog stays hidden, the scan
//! looks hung. With access granted there are no dialogs and everything is readable.

use std::io::ErrorKind;

use serde::{Deserialize, Serialize};

use crate::rules::home_dir;

/// `$HOME` folders that trigger a permission dialog without Full Disk Access.
pub const PROTECTED_HOME_DIRS: &[&str] = &["Desktop", "Documents", "Downloads"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessStatus {
    pub full_disk_access: bool,
}

/// Checks access by opening a file that is only readable with Full Disk Access.
/// Without the permission it fails instantly with EPERM: no dialog is triggered.
pub fn full_disk_access() -> bool {
    let Some(home) = home_dir() else { return false };
    let probes = [
        home.join("Library/Application Support/com.apple.TCC/TCC.db"),
        home.join("Library/Safari/CloudTabs.db"),
        home.join("Library/Safari/Bookmarks.plist"),
    ];
    for probe in probes {
        match std::fs::File::open(&probe) {
            Ok(_) => return true,
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(_) => return false,
        }
    }
    false
}

pub fn status() -> AccessStatus {
    AccessStatus { full_disk_access: full_disk_access() }
}

/// Whether `name`, inside `dir`, is a protected folder that must be skipped.
pub fn skip_protected(home: &[u8], dir: &[u8], name: &[u8], granted: bool) -> bool {
    !granted && dir == home && PROTECTED_HOME_DIRS.iter().any(|p| p.as_bytes() == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_only_top_level_protected_dirs_without_access() {
        assert!(skip_protected(b"/Users/a", b"/Users/a", b"Desktop", false));
        assert!(!skip_protected(b"/Users/a", b"/Users/a", b"Desktop", true));
        assert!(!skip_protected(b"/Users/a", b"/Users/a/Projects", b"Desktop", false));
        assert!(!skip_protected(b"/Users/a", b"/Users/a", b"Projects", false));
    }

    #[test]
    fn probe_never_panics() {
        let _ = full_disk_access();
    }
}
