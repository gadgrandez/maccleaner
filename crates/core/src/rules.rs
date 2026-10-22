//! Our own rules for what counts as junk. Each rule points to one or more root directories;
//! the cleanable items are the *direct* entries of those roots that pass its filter.
//!
//! There are deliberately NO rules to remove languages (`*.lproj`) from apps or to thin
//! universal binaries (strip the x86_64 slice): both modify the contents of signed bundles,
//! which invalidates the code signature. Gatekeeper may refuse to open the app, delta updates
//! fail, and App Store or notarized apps no longer verify.
//! The savings (a few hundred MB) are not worth that risk.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Move to the Trash (recoverable).
    Trash,
    /// Permanent deletion (only for the Trash itself).
    Delete,
}

struct RuleDef {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    /// Paths relative to `$HOME`.
    roots: &'static [&'static str],
    /// Excluded entry names; a trailing `*` means prefix.
    exclude: &'static [&'static str],
    filter: Filter,
    default_selected: bool,
    action: Action,
}

/// Optional filter on each direct entry of the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// All entries.
    All,
    /// `.plist` files that the `plist` crate cannot parse (neither binary nor XML).
    /// If they cannot be read (permissions) they are not considered broken.
    BrokenPlist,
    /// Entries with one of these extensions (case-insensitive) whose modification date
    /// is more than `days` days old.
    OldWithExtension { extensions: &'static [&'static str], days: u64 },
}

const RULES: &[RuleDef] = &[
    RuleDef {
        id: "user_caches",
        name: "User caches",
        description: "Temporary data that apps regenerate. Quit apps before cleaning.",
        roots: &["Library/Caches"],
        exclude: &[
            "com.apple.bird",
            "com.apple.CloudKit",
            "CloudKit",
            "com.apple.akd",
            "com.apple.nsurlsessiond",
            "com.apple.HomeKit",
            "FamilyCircle",
            "com.apple.ap.adprivacyd",
        ],
        filter: Filter::All,
        default_selected: true,
        action: Action::Trash,
    },
    RuleDef {
        id: "user_logs",
        name: "User logs",
        description: "App logs and old diagnostic reports.",
        roots: &["Library/Logs"],
        // Has its own category (`simulator_devices_logs`).
        exclude: &["CoreSimulator"],
        filter: Filter::All,
        default_selected: true,
        action: Action::Trash,
    },
    RuleDef {
        id: "xcode_derived_data",
        name: "Xcode · DerivedData",
        description: "Intermediate build products; Xcode regenerates them.",
        roots: &["Library/Developer/Xcode/DerivedData"],
        exclude: &[],
        filter: Filter::All,
        default_selected: true,
        action: Action::Trash,
    },
    RuleDef {
        id: "xcode_device_support",
        name: "Xcode · Device Support",
        description: "Symbols for iOS/watchOS/tvOS versions; they are copied again when the device is connected.",
        roots: &[
            "Library/Developer/Xcode/iOS DeviceSupport",
            "Library/Developer/Xcode/watchOS DeviceSupport",
            "Library/Developer/Xcode/tvOS DeviceSupport",
            "Library/Developer/Xcode/visionOS DeviceSupport",
            "Library/Developer/Xcode/macOS DeviceSupport",
        ],
        exclude: &[],
        filter: Filter::All,
        default_selected: true,
        action: Action::Trash,
    },
    RuleDef {
        id: "simulator_caches",
        name: "Simulators · caches",
        description: "CoreSimulator caches.",
        roots: &["Library/Developer/CoreSimulator/Caches"],
        exclude: &[],
        filter: Filter::All,
        default_selected: true,
        action: Action::Trash,
    },
    RuleDef {
        id: "xcode_archives",
        name: "Xcode · Archives",
        description: "Archived builds with their dSYMs. Needed to symbolicate crashes from released versions.",
        roots: &["Library/Developer/Xcode/Archives"],
        exclude: &[],
        filter: Filter::All,
        default_selected: false,
        action: Action::Trash,
    },
    RuleDef {
        id: "simulator_devices_logs",
        name: "Simulators · logs",
        description: "Logs from Xcode's simulated devices. Only useful for debugging past sessions.",
        roots: &["Library/Logs/CoreSimulator"],
        exclude: &[],
        filter: Filter::All,
        default_selected: true,
        action: Action::Trash,
    },
    RuleDef {
        id: "xcode_previews",
        name: "Xcode · Previews",
        description: "Simulators and builds for SwiftUI previews; Xcode regenerates them (the first preview will take longer).",
        roots: &["Library/Developer/Xcode/UserData/Previews"],
        exclude: &[],
        filter: Filter::All,
        default_selected: true,
        action: Action::Trash,
    },
    RuleDef {
        id: "mail_downloads",
        name: "Mail · downloaded attachments",
        description: "Copies of attachments Mail saves when you open them. The originals stay in the messages; requires Full Disk Access.",
        roots: &["Library/Containers/com.apple.mail/Data/Library/Mail Downloads"],
        exclude: &[],
        filter: Filter::All,
        default_selected: true,
        action: Action::Trash,
    },
    RuleDef {
        id: "ios_software_updates",
        name: "Downloaded iOS updates",
        description: "Firmware files (.ipsw) that Finder/iTunes downloaded to update devices; they are downloaded again if needed.",
        roots: &[
            "Library/iTunes/iPhone Software Updates",
            "Library/iTunes/iPad Software Updates",
            "Library/iTunes/iPod Software Updates",
            "Library/iTunes/Apple TV Software Updates",
        ],
        exclude: &[],
        filter: Filter::All,
        default_selected: true,
        action: Action::Trash,
    },
    RuleDef {
        id: "broken_preferences",
        name: "Broken preferences",
        description: "Preference .plist files that cannot be read. The affected app will revert to its default settings.",
        roots: &["Library/Preferences"],
        // Apple's are never touched: some (Music, TV) are TCC-protected and opening them
        // blocks the process until the user answers a permission dialog.
        exclude: &["com.apple.*", ".GlobalPreferences*", "loginwindow.plist", "ByHost"],
        filter: Filter::BrokenPlist,
        default_selected: true,
        action: Action::Trash,
    },
    RuleDef {
        id: "ios_backups",
        name: "iPhone/iPad backups",
        description: "Full local device backups. If you delete one and have no other (e.g. in iCloud), that data is lost forever. Review them one by one.",
        roots: &["Library/Application Support/MobileSync/Backup"],
        exclude: &[],
        filter: Filter::All,
        default_selected: false,
        action: Action::Trash,
    },
    RuleDef {
        id: "unused_disk_images",
        name: "Old installers in Downloads",
        description: "Disk images (.dmg) and installers (.pkg) in Downloads not modified in over 30 days. You may want to keep some.",
        roots: &["Downloads"],
        exclude: &[],
        filter: Filter::OldWithExtension { extensions: &["dmg", "pkg"], days: 30 },
        default_selected: false,
        action: Action::Trash,
    },
    RuleDef {
        id: "trash",
        name: "Trash",
        description: "Empty the Trash. This is permanent.",
        roots: &[".Trash"],
        exclude: &[],
        filter: Filter::All,
        default_selected: false,
        action: Action::Delete,
    },
];

/// Rule with resolved absolute paths.
#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub description: String,
    pub roots: Vec<PathBuf>,
    pub exclude: Vec<String>,
    pub filter: Filter,
    pub default_selected: bool,
    pub action: Action,
}

impl Rule {
    pub fn is_excluded(&self, name: &OsStr) -> bool {
        let Some(name) = name.to_str() else { return true };
        self.exclude.iter().any(|pat| match pat.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => name == pat,
        })
    }

    /// Whether the entry `path` (direct child of a root) passes the rule's filter.
    pub fn passes_filter(&self, path: &Path) -> bool {
        self.passes_filter_at(path, SystemTime::now())
    }

    fn passes_filter_at(&self, path: &Path, now: SystemTime) -> bool {
        match self.filter {
            Filter::All => true,
            Filter::BrokenPlist => is_broken_plist(path),
            Filter::OldWithExtension { extensions, days } => {
                let ext_ok = path
                    .extension()
                    .and_then(OsStr::to_str)
                    .is_some_and(|e| extensions.iter().any(|x| x.eq_ignore_ascii_case(e)));
                let limit = now.checked_sub(Duration::from_secs(days * 86_400));
                ext_ok
                    && match (std::fs::symlink_metadata(path).and_then(|m| m.modified()), limit) {
                        (Ok(mtime), Some(limit)) => mtime < limit,
                        _ => false,
                    }
            }
        }
    }

    /// Exclusions and filter together: whether the entry can be listed and cleaned.
    pub fn admits(&self, path: &Path) -> bool {
        path.file_name().is_some_and(|n| !self.is_excluded(n)) && self.passes_filter(path)
    }
}

fn is_broken_plist(path: &Path) -> bool {
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("plist")) {
        return false;
    }
    match std::fs::symlink_metadata(path) {
        // A legitimate preferences plist is small; large files are not read.
        Ok(md) if md.is_file() && md.len() <= 4 << 20 => {}
        _ => return false,
    }
    match std::fs::read(path) {
        Ok(bytes) => plist::Value::from_reader(std::io::Cursor::new(bytes)).is_err(),
        Err(_) => false,
    }
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

pub fn resolve(home: &Path) -> Vec<Rule> {
    RULES
        .iter()
        .map(|r| Rule {
            id: r.id.into(),
            name: r.name.into(),
            description: r.description.into(),
            roots: r.roots.iter().map(|p| home.join(p)).collect(),
            exclude: r.exclude.iter().map(|s| (*s).into()).collect(),
            filter: r.filter,
            default_selected: r.default_selected,
            action: r.action,
        })
        .collect()
}

pub fn default_rules() -> Vec<Rule> {
    home_dir().map(|h| resolve(&h)).unwrap_or_default()
}

/// Sets the modification date of `path` to `days_ago` days ago (for tests).
#[cfg(test)]
pub(crate) fn set_mtime(path: &Path, days_ago: u64) {
    use std::os::unix::ffi::OsStrExt;
    let secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() - days_ago * 86_400;
    let tv = libc::timeval { tv_sec: secs as libc::time_t, tv_usec: 0 };
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::utimes(c.as_ptr(), [tv, tv].as_ptr()) }, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<_> = RULES.iter().map(|r| r.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), RULES.len());
    }

    #[test]
    fn broken_preferences_never_touch_apple_plists() {
        let tmp = TempDir::new("prefs");
        let rule = resolve(tmp.path()).into_iter().find(|r| r.id == "broken_preferences").unwrap();
        let dir = tmp.path().join("Library/Preferences");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["com.apple.Music.plist", "com.example.app.plist"] {
            std::fs::write(dir.join(name), b"not a plist").unwrap();
        }
        assert!(!rule.admits(&dir.join("com.apple.Music.plist")));
        assert!(rule.admits(&dir.join("com.example.app.plist")));
    }

    #[test]
    fn exclusion_supports_prefix() {
        let mut rule = resolve(Path::new("/h")).remove(0);
        rule.exclude = vec!["exact".into(), "pre*".into()];
        assert!(rule.is_excluded(OsStr::new("exact")));
        assert!(rule.is_excluded(OsStr::new("prefix.x")));
        assert!(!rule.is_excluded(OsStr::new("exactly")));
        assert_eq!(rule.roots, vec![PathBuf::from("/h/Library/Caches")]);
    }

    fn rule(id: &str) -> Rule {
        resolve(Path::new("/h")).into_iter().find(|r| r.id == id).unwrap()
    }

    #[test]
    fn roots_do_not_overlap() {
        // An entry cannot belong to two categories (it would be counted twice).
        let rules = resolve(Path::new("/h"));
        for a in &rules {
            for b in &rules {
                for ra in &a.roots {
                    for rb in &b.roots {
                        if ra != rb && rb.starts_with(ra) {
                            let child = rb.strip_prefix(ra).unwrap().components().next().unwrap();
                            assert!(a.is_excluded(child.as_os_str()), "{} overlaps with {}", a.id, b.id);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn broken_plist_filter() {
        let tmp = TempDir::new("rules-plist");
        let d = tmp.path();
        let mut good = Vec::new();
        plist::Value::Dictionary(plist::Dictionary::new()).to_writer_binary(&mut good).unwrap();
        std::fs::write(d.join("good.plist"), &good).unwrap();
        std::fs::write(d.join("xml.plist"), br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>a</key><integer>1</integer></dict></plist>"#).unwrap();
        std::fs::write(d.join("broken.plist"), b"bplist00\x01\x02garbage").unwrap();
        std::fs::write(d.join("empty.plist"), b"").unwrap();
        std::fs::write(d.join("notes.txt"), b"not plist").unwrap();
        std::fs::create_dir(d.join("dir.plist")).unwrap();
        let r = rule("broken_preferences");
        let ok: Vec<_> = ["good.plist", "xml.plist", "broken.plist", "empty.plist", "notes.txt", "dir.plist"]
            .iter()
            .filter(|n| r.passes_filter(&d.join(n)))
            .copied()
            .collect();
        assert_eq!(ok, vec!["broken.plist", "empty.plist"]);
    }

    #[test]
    fn old_disk_image_filter() {
        let tmp = TempDir::new("rules-dmg");
        let d = tmp.path();
        for n in ["old.dmg", "OLD2.PKG", "new.dmg", "old.zip"] {
            std::fs::write(d.join(n), b"x").unwrap();
        }
        for n in ["old.dmg", "OLD2.PKG", "old.zip"] {
            set_mtime(&d.join(n), 60);
        }
        let r = rule("unused_disk_images");
        assert!(!r.default_selected);
        let ok: Vec<_> =
            ["old.dmg", "OLD2.PKG", "new.dmg", "old.zip"].iter().filter(|n| r.admits(&d.join(n))).copied().collect();
        assert_eq!(ok, vec!["old.dmg", "OLD2.PKG"]);
        assert!(!r.admits(&d.join("missing.dmg")));
    }
}
