//! Privacy: browsing traces (history, cookies, caches…) and recent items.
//!
//! The scan locates each browser's files from its data folders.
//! Cleaning only receives `{source, item}` pairs: paths are recomputed with the same
//! scan, so the client can never ask for an arbitrary path to be moved.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::clean::{CleanOutcome, CleanReport};
use crate::fsutil;
use crate::rules::{Action, home_dir};
use crate::{progress, trash};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemId {
    History,
    Cookies,
    Cache,
    Downloads,
    Autofill,
    LocalStorage,
    Sessions,
    RecentItems,
}

impl ItemId {
    fn as_str(self) -> &'static str {
        match self {
            Self::History => "history",
            Self::Cookies => "cookies",
            Self::Cache => "cache",
            Self::Downloads => "downloads",
            Self::Autofill => "autofill",
            Self::LocalStorage => "local_storage",
            Self::Sessions => "sessions",
            Self::RecentItems => "recent_items",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::History => "Browsing history",
            Self::Cookies => "Cookies (signs you out of websites)",
            Self::Cache => "Cache",
            Self::Downloads => "Download history",
            Self::Autofill => "Form autofill",
            Self::LocalStorage => "Website storage",
            Self::Sessions => "Saved sessions and tabs",
            Self::RecentItems => "Recent items",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceId {
    Safari,
    Chrome,
    Firefox,
    Edge,
    Brave,
    Arc,
    Opera,
    Vivaldi,
    System,
}

impl SourceId {
    fn as_str(self) -> &'static str {
        match self {
            Self::Safari => "safari",
            Self::Chrome => "chrome",
            Self::Firefox => "firefox",
            Self::Edge => "edge",
            Self::Brave => "brave",
            Self::Arc => "arc",
            Self::Opera => "opera",
            Self::Vivaldi => "vivaldi",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyItem {
    pub id: ItemId,
    pub name: String,
    pub paths: Vec<String>,
    pub bytes: u64,
    /// `false` if some folder could not be read (e.g. TCC without Full Disk Access).
    pub accessible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacySource {
    pub id: SourceId,
    pub name: String,
    pub running: bool,
    pub items: Vec<PrivacyItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyReport {
    pub sources: Vec<PrivacySource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemRef {
    pub source: String,
    pub item: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyCleanRequest {
    pub items: Vec<ItemRef>,
    /// By default nothing is touched.
    #[serde(default = "default_true")]
    pub dry_run: bool,
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Search patterns

/// How entries of a base directory are chosen.
#[derive(Clone, Copy)]
enum Pat {
    /// Exact name plus its `-journal`, `-wal` and `-shm` files.
    Exact(&'static str),
    /// Names starting with the prefix (`""` = all entries).
    Prefix(&'static str),
    /// Extension starting with the prefix (`sfl` → `.sfl2`, `.sfl3`…).
    ExtPrefix(&'static str),
}

impl Pat {
    fn matches(self, name: &str) -> bool {
        match self {
            Pat::Exact(p) => {
                name == p || name.strip_prefix(p).is_some_and(|rest| matches!(rest, "-journal" | "-wal" | "-shm"))
            }
            Pat::Prefix(p) => name.starts_with(p),
            Pat::ExtPrefix(p) => name.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && ext.starts_with(p)),
        }
    }
}

/// A probe: entries of `base/<subdir>` that match `pat`.
struct Probe {
    dir: PathBuf,
    pat: Pat,
}

/// `rel` may contain subdirectories (`Network/Cookies`): the last component is the pattern.
fn exact(base: &Path, rel: &'static str) -> Probe {
    match rel.rsplit_once('/') {
        Some((dir, name)) => Probe { dir: base.join(dir), pat: Pat::Exact(name) },
        None => Probe { dir: base.to_path_buf(), pat: Pat::Exact(rel) },
    }
}

fn prefix(base: &Path, p: &'static str) -> Probe {
    Probe { dir: base.to_path_buf(), pat: Pat::Prefix(p) }
}

fn children(dir: PathBuf) -> Probe {
    Probe { dir, pat: Pat::Prefix("") }
}

/// Directory reads cached during a scan.
#[derive(Default)]
struct Lister {
    cache: HashMap<PathBuf, Result<Vec<OsString>, ()>>,
}

impl Lister {
    /// `Ok(empty)` if it does not exist; `Err` if it exists but cannot be read.
    fn list(&mut self, dir: &Path) -> Result<&[OsString], ()> {
        let entry = self.cache.entry(dir.to_path_buf()).or_insert_with(|| match fs::read_dir(dir) {
            Ok(rd) => {
                let mut v: Vec<OsString> = rd.filter_map(|e| e.ok().map(|e| e.file_name())).collect();
                v.sort();
                Ok(v)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(_) => Err(()),
        });
        entry.as_ref().map(Vec::as_slice).map_err(|_| ())
    }

    /// Resolves an item's probes: paths found and whether everything was readable.
    fn resolve(&mut self, probes: &[Probe]) -> (Vec<PathBuf>, bool) {
        let mut paths = Vec::new();
        let mut accessible = true;
        for probe in probes {
            match self.list(&probe.dir) {
                Ok(names) => paths.extend(
                    names.iter().filter(|n| n.to_str().is_some_and(|s| probe.pat.matches(s))).map(|n| probe.dir.join(n)),
                ),
                Err(()) => accessible = false,
            }
        }
        (dedupe_nested(paths), accessible)
    }
}

/// Removes duplicates and paths contained in another one in the list.
fn dedupe_nested(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();
    let mut out: Vec<PathBuf> = Vec::with_capacity(paths.len());
    for p in paths {
        if !out.iter().any(|kept| p.starts_with(kept)) {
            out.push(p);
        }
    }
    out
}

fn exists(path: &Path) -> bool {
    // A permission error also means the folder exists.
    !matches!(fs::symlink_metadata(path), Err(e) if e.kind() == io::ErrorKind::NotFound)
}

// ---------------------------------------------------------------------------
// Browsers

struct Chromium {
    id: SourceId,
    name: &'static str,
    /// Data folder relative to `~/Library/Application Support`.
    data: &'static str,
    /// Folders relative to `~/Library/Caches`.
    caches: &'static [&'static str],
    processes: &'static [&'static str],
}

const CHROMIUM: &[Chromium] = &[
    Chromium {
        id: SourceId::Chrome,
        name: "Google Chrome",
        data: "Google/Chrome",
        caches: &["Google/Chrome"],
        processes: &["Google Chrome"],
    },
    Chromium {
        id: SourceId::Edge,
        name: "Microsoft Edge",
        data: "Microsoft Edge",
        caches: &["Microsoft Edge"],
        processes: &["Microsoft Edge"],
    },
    Chromium {
        id: SourceId::Brave,
        name: "Brave",
        data: "BraveSoftware/Brave-Browser",
        caches: &["BraveSoftware/Brave-Browser"],
        processes: &["Brave Browser"],
    },
    Chromium {
        id: SourceId::Arc,
        name: "Arc",
        data: "Arc/User Data",
        caches: &["Arc", "company.thebrowser.Browser"],
        processes: &["Arc"],
    },
    Chromium {
        id: SourceId::Opera,
        name: "Opera",
        data: "com.operasoftware.Opera",
        caches: &["com.operasoftware.Opera"],
        processes: &["Opera"],
    },
    Chromium {
        id: SourceId::Vivaldi,
        name: "Vivaldi",
        data: "Vivaldi",
        caches: &["Vivaldi"],
        processes: &["Vivaldi"],
    },
];

const SAFARI_PROCESSES: &[&str] = &["Safari"];
const FIREFOX_PROCESSES: &[&str] = &["firefox", "Firefox"];

/// Item without sizes yet: candidate paths and readability.
struct Draft {
    id: ItemId,
    paths: Vec<PathBuf>,
    accessible: bool,
}

struct SourceDraft {
    id: SourceId,
    name: &'static str,
    processes: &'static [&'static str],
    items: Vec<Draft>,
}

fn draft(lister: &mut Lister, id: ItemId, probes: &[Probe]) -> Draft {
    let (paths, accessible) = lister.resolve(probes);
    Draft { id, paths, accessible }
}

/// Chromium profiles: `Default`, `Profile N` and the folder itself if it is a profile (Opera).
fn chromium_profiles(lister: &mut Lister, data: &Path) -> (Vec<PathBuf>, bool) {
    let Ok(names) = lister.list(data) else { return (Vec::new(), false) };
    let mut profiles: Vec<PathBuf> = names
        .iter()
        .filter_map(|n| n.to_str())
        .filter(|n| *n == "Default" || n.starts_with("Profile "))
        .map(|n| data.join(n))
        .collect();
    if names.iter().any(|n| n == "History" || n == "Cookies" || n == "Preferences") {
        profiles.insert(0, data.to_path_buf());
    }
    (profiles, true)
}

fn chromium_source(lister: &mut Lister, home: &Path, b: &Chromium) -> Option<SourceDraft> {
    let data = home.join("Library/Application Support").join(b.data);
    if !exists(&data) {
        return None;
    }
    let (profiles, readable) = chromium_profiles(lister, &data);
    let per_profile = |f: &dyn Fn(&Path) -> Vec<Probe>| -> Vec<Probe> { profiles.iter().flat_map(|p| f(p)).collect() };

    let history = per_profile(&|p| {
        vec![prefix(p, "History"), exact(p, "Visited Links"), exact(p, "Top Sites"), exact(p, "Shortcuts")]
    });
    let cookies = per_profile(&|p| vec![exact(p, "Cookies"), exact(p, "Network/Cookies")]);
    let mut cache = per_profile(&|p| vec![exact(p, "Cache"), exact(p, "Code Cache"), exact(p, "GPUCache")]);
    let caches_root = home.join("Library/Caches");
    cache.extend(b.caches.iter().map(|c| exact(&caches_root, c)));
    let autofill = per_profile(&|p| vec![exact(p, "Web Data")]);
    let storage = per_profile(&|p| {
        vec![exact(p, "Local Storage"), exact(p, "IndexedDB"), exact(p, "Session Storage"), exact(p, "databases")]
    });
    let sessions = per_profile(&|p| {
        vec![
            exact(p, "Sessions"),
            exact(p, "Current Session"),
            exact(p, "Current Tabs"),
            exact(p, "Last Session"),
            exact(p, "Last Tabs"),
        ]
    });

    // Downloads live inside `History`: there is no separate `downloads` item.
    let mut items = vec![
        draft(lister, ItemId::History, &history),
        draft(lister, ItemId::Cookies, &cookies),
        draft(lister, ItemId::Cache, &cache),
        draft(lister, ItemId::Autofill, &autofill),
        draft(lister, ItemId::LocalStorage, &storage),
        draft(lister, ItemId::Sessions, &sessions),
    ];
    if !readable {
        for it in &mut items {
            if it.id != ItemId::Cache {
                it.accessible = false;
            }
        }
    }
    Some(SourceDraft { id: b.id, name: b.name, processes: b.processes, items })
}

fn safari_source(lister: &mut Lister, home: &Path) -> Option<SourceDraft> {
    let lib = home.join("Library");
    let safari = lib.join("Safari");
    let container = lib.join("Containers/com.apple.Safari/Data/Library");
    if !exists(&safari) && !exists(&container) {
        return None;
    }
    let items = vec![
        draft(lister, ItemId::History, &[exact(&safari, "History.db"), exact(&safari, "History.plist")]),
        draft(
            lister,
            ItemId::Cookies,
            &[exact(&container, "Cookies/Cookies.binarycookies"), exact(&lib, "Cookies/Cookies.binarycookies")],
        ),
        draft(
            lister,
            ItemId::Cache,
            &[
                children(container.join("Caches")),
                exact(&lib, "Caches/com.apple.Safari"),
                exact(&lib, "Caches/com.apple.WebKit.Networking"),
            ],
        ),
        draft(lister, ItemId::Downloads, &[exact(&safari, "Downloads.plist")]),
        draft(lister, ItemId::Autofill, &[exact(&safari, "Form Values")]),
        draft(
            lister,
            ItemId::LocalStorage,
            &[
                exact(&safari, "LocalStorage"),
                exact(&safari, "Databases"),
                exact(&container, "WebKit/WebsiteData"),
                exact(&lib, "WebKit/com.apple.Safari"),
            ],
        ),
        draft(
            lister,
            ItemId::Sessions,
            &[
                exact(&safari, "LastSession.plist"),
                exact(&container, "Saved Application State/com.apple.Safari.savedState"),
            ],
        ),
    ];
    Some(SourceDraft { id: SourceId::Safari, name: "Safari", processes: SAFARI_PROCESSES, items })
}

fn firefox_source(lister: &mut Lister, home: &Path) -> Option<SourceDraft> {
    let root = home.join("Library/Application Support/Firefox");
    if !exists(&root) {
        return None;
    }
    let profiles_dir = root.join("Profiles");
    let (profiles, readable) = match lister.list(&profiles_dir) {
        Ok(names) => (names.iter().map(|n| profiles_dir.join(n)).filter(|p| p.is_dir()).collect::<Vec<_>>(), true),
        Err(()) => (Vec::new(), false),
    };
    let per_profile = |f: &dyn Fn(&Path) -> Vec<Probe>| -> Vec<Probe> { profiles.iter().flat_map(|p| f(p)).collect() };

    // `places.sqlite` also stores bookmarks; Firefox restores them on its own from
    // `bookmarkbackups/` when the file is missing.
    let history = per_profile(&|p| vec![exact(p, "places.sqlite"), exact(p, "favicons.sqlite")]);
    let cookies = per_profile(&|p| vec![exact(p, "cookies.sqlite")]);
    let mut cache = per_profile(&|p| vec![exact(p, "startupCache")]);
    cache.push(exact(&home.join("Library/Caches"), "Firefox"));
    let downloads = per_profile(&|p| vec![exact(p, "downloads.sqlite")]);
    let autofill = per_profile(&|p| vec![exact(p, "formhistory.sqlite")]);
    let storage = per_profile(&|p| vec![exact(p, "storage"), exact(p, "webappsstore.sqlite")]);
    let sessions = per_profile(&|p| vec![prefix(p, "sessionstore")]);

    let mut items = vec![
        draft(lister, ItemId::History, &history),
        draft(lister, ItemId::Cookies, &cookies),
        draft(lister, ItemId::Cache, &cache),
        draft(lister, ItemId::Downloads, &downloads),
        draft(lister, ItemId::Autofill, &autofill),
        draft(lister, ItemId::LocalStorage, &storage),
        draft(lister, ItemId::Sessions, &sessions),
    ];
    // Since Firefox 26 downloads live inside `places.sqlite`: there is only an item if the
    // old separate file exists.
    items.retain(|it| it.id != ItemId::Downloads || !it.paths.is_empty());
    if !readable {
        for it in &mut items {
            if it.id != ItemId::Cache {
                it.accessible = false;
            }
        }
    }
    Some(SourceDraft { id: SourceId::Firefox, name: "Firefox", processes: FIREFOX_PROCESSES, items })
}

fn system_source(lister: &mut Lister, home: &Path) -> SourceDraft {
    let sfl = home.join("Library/Application Support/com.apple.sharedfilelist");
    // Only "Recent*" lists: `FavoriteItems`, `FavoriteVolumes`, etc. are the Finder sidebar
    // and must not be touched.
    let probes = [
        prefix(&sfl, "com.apple.LSSharedFileList.Recent"),
        Probe { dir: sfl.join("com.apple.LSSharedFileList.ApplicationRecentDocuments"), pat: Pat::ExtPrefix("sfl") },
    ];
    let mut item = draft(lister, ItemId::RecentItems, &probes);
    item.paths.retain(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.starts_with("sfl")));
    SourceDraft { id: SourceId::System, name: "System", processes: &[], items: vec![item] }
}

// ---------------------------------------------------------------------------
// Processes

/// Names (`p_comm`) of running processes.
pub fn running_process_names() -> HashSet<String> {
    let mut names = HashSet::new();
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return names;
    }
    let mut pids = vec![0 as libc::c_int; n as usize + 64];
    let size = (pids.len() * std::mem::size_of::<libc::c_int>()) as libc::c_int;
    let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), size) };
    if n <= 0 {
        return names;
    }
    let mut buf = [0u8; 256];
    for &pid in &pids[..(n as usize).min(pids.len())] {
        let len = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        if len > 0
            && let Ok(s) = std::str::from_utf8(&buf[..len as usize])
        {
            names.insert(s.to_owned());
        }
    }
    names
}

// ---------------------------------------------------------------------------
// Scanning and cleaning

/// Scan with `$HOME` and real processes.
pub fn scan() -> Result<PrivacyReport, String> {
    let home = home_dir().ok_or("HOME is not set")?;
    Ok(scan_at(&home, &running_process_names()))
}

/// Scan of a given `home`; `running` holds the names of active processes.
pub fn scan_at(home: &Path, running: &HashSet<String>) -> PrivacyReport {
    let mut lister = Lister::default();
    let mut drafts = Vec::new();
    drafts.extend(safari_source(&mut lister, home));
    let chromium = |id| CHROMIUM.iter().find(|b| b.id == id).unwrap();
    drafts.extend(chromium_source(&mut lister, home, chromium(SourceId::Chrome)));
    drafts.extend(firefox_source(&mut lister, home));
    for id in [SourceId::Edge, SourceId::Brave, SourceId::Arc, SourceId::Opera, SourceId::Vivaldi] {
        drafts.extend(chromium_source(&mut lister, home, chromium(id)));
    }
    drafts.push(system_source(&mut lister, home));

    // A single parallel size computation for all paths.
    let all: Vec<PathBuf> = drafts.iter().flat_map(|s| &s.items).flat_map(|i| i.paths.iter().cloned()).collect();
    let mut sizes = fsutil::usages(&all).into_iter();

    let sources = drafts
        .into_iter()
        .map(|s| PrivacySource {
            id: s.id,
            name: s.name.into(),
            running: s.processes.iter().any(|p| running.contains(*p)),
            items: s
                .items
                .into_iter()
                .map(|d| PrivacyItem {
                    id: d.id,
                    name: d.id.name().into(),
                    bytes: sizes.by_ref().take(d.paths.len()).map(|u| u.bytes).sum(),
                    paths: d.paths.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
                    accessible: d.accessible,
                })
                .collect(),
        })
        .collect();
    PrivacyReport { sources }
}

/// Cleaning with `$HOME` and real processes.
pub fn clean(req: &PrivacyCleanRequest) -> Result<CleanReport, String> {
    let home = home_dir().ok_or("HOME is not set")?;
    Ok(clean_at(&home, &running_process_names(), req))
}

fn failure(path: String, category: Option<String>, error: String) -> CleanOutcome {
    CleanOutcome { path, category, action: None, bytes: 0, ok: false, error: Some(error) }
}

pub fn clean_at(home: &Path, running: &HashSet<String>, req: &PrivacyCleanRequest) -> CleanReport {
    let report = scan_at(home, running);
    // The number of files is only known item by item: the progress shows what is being removed.
    let _progress = progress::start(0, req.dry_run);
    let mut out = CleanReport { dry_run: req.dry_run, trashed_bytes: 0, deleted_bytes: 0, failed: 0, outcomes: Vec::new() };
    let mut seen = HashSet::new();

    for r in &req.items {
        if !seen.insert((r.source.as_str(), r.item.as_str())) {
            continue;
        }
        let label = format!("{}.{}", r.source, r.item);
        let source = report.sources.iter().find(|s| s.id.as_str() == r.source);
        let item = source.and_then(|s| s.items.iter().find(|i| i.id.as_str() == r.item));
        let (Some(source), Some(item)) = (source, item) else {
            out.outcomes.push(failure(label, None, "unknown or missing item".into()));
            continue;
        };
        if source.running {
            out.outcomes.push(failure(label.clone(), Some(label), format!("Quit {} before cleaning", source.name)));
            continue;
        }
        if !item.accessible && item.paths.is_empty() {
            out.outcomes.push(failure(label.clone(), Some(label), "no access: requires Full Disk Access".into()));
            continue;
        }

        let paths: Vec<PathBuf> = item.paths.iter().map(PathBuf::from).collect();
        let sizes = fsutil::usages(&paths);
        for (path, usage) in paths.iter().zip(sizes) {
            let raw = path.to_string_lossy().into_owned();
            // All paths come from the scan; this is just a safeguard.
            if !path.starts_with(home) {
                out.outcomes.push(failure(raw, Some(label.clone()), "outside the allowed paths".into()));
                continue;
            }
            let result = if req.dry_run { Ok(()) } else { progress::step(path, usage.bytes, || trash::move_to_trash(path)) };
            if result.is_ok() {
                out.trashed_bytes += usage.bytes;
            }
            out.outcomes.push(CleanOutcome {
                path: raw,
                category: Some(label.clone()),
                action: Some(Action::Trash),
                bytes: usage.bytes,
                ok: result.is_ok(),
                error: result.err(),
            });
        }
    }
    out.failed = out.outcomes.iter().filter(|o| !o.ok).count() as u64;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn touch(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![1u8; bytes]).unwrap();
    }

    fn fake_home() -> TempDir {
        let tmp = TempDir::new("privacy");
        let h = tmp.path();
        let chrome = h.join("Library/Application Support/Google/Chrome");
        for f in [
            "Default/History",
            "Default/History-journal",
            "Default/Cookies",
            "Default/Network/Cookies",
            "Default/Network/Cookies-journal",
            "Default/Web Data",
            "Default/Login Data",
            "Default/Bookmarks",
            "Default/Cache/Cache_Data/f_000001",
            "Default/Local Storage/leveldb/000003.log",
            "Default/Sessions/Session_1",
            "Profile 1/History",
            "Profile 1/Cookies-wal",
            "System Profile/History",
            "Local State",
        ] {
            touch(&chrome.join(f), 4096);
        }
        touch(&h.join("Library/Caches/Google/Chrome/Default/Cache/x"), 8192);

        let ff = h.join("Library/Application Support/Firefox/Profiles/abcd.default-release");
        for f in [
            "places.sqlite",
            "places.sqlite-wal",
            "cookies.sqlite",
            "cookies.sqlite-shm",
            "formhistory.sqlite",
            "key4.db",
            "logins.json",
            "sessionstore.jsonlz4",
            "sessionstore-backups/recovery.jsonlz4",
            "storage/default/x",
        ] {
            touch(&ff.join(f), 4096);
        }
        touch(&h.join("Library/Caches/Firefox/Profiles/abcd.default-release/cache2/entries/x"), 4096);

        let sfl = h.join("Library/Application Support/com.apple.sharedfilelist");
        touch(&sfl.join("com.apple.LSSharedFileList.RecentDocuments.sfl3"), 100);
        touch(&sfl.join("com.apple.LSSharedFileList.FavoriteItems.sfl3"), 100);
        touch(&sfl.join("com.apple.LSSharedFileList.ApplicationRecentDocuments/com.apple.textedit.sfl3"), 100);
        tmp
    }

    fn names(item: &PrivacyItem, base: &Path) -> Vec<String> {
        item.paths.iter().map(|p| Path::new(p).strip_prefix(base).unwrap().to_string_lossy().into_owned()).collect()
    }

    fn find<'a>(r: &'a PrivacyReport, s: SourceId, i: ItemId) -> Option<&'a PrivacyItem> {
        r.sources.iter().find(|x| x.id == s)?.items.iter().find(|x| x.id == i)
    }

    #[test]
    fn detects_chromium_profile() {
        let tmp = fake_home();
        let h = tmp.path();
        let r = scan_at(h, &HashSet::new());
        let ids: Vec<_> = r.sources.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![SourceId::Chrome, SourceId::Firefox, SourceId::System]);

        let base = h.join("Library/Application Support/Google/Chrome");
        let history = find(&r, SourceId::Chrome, ItemId::History).unwrap();
        assert_eq!(names(history, &base), vec!["Default/History", "Default/History-journal", "Profile 1/History"]);
        assert!(history.bytes >= 3 * 4096);
        assert!(history.accessible);
        let cookies = find(&r, SourceId::Chrome, ItemId::Cookies).unwrap();
        assert_eq!(
            names(cookies, &base),
            vec!["Default/Cookies", "Default/Network/Cookies", "Default/Network/Cookies-journal", "Profile 1/Cookies-wal"]
        );
        assert_eq!(names(find(&r, SourceId::Chrome, ItemId::Autofill).unwrap(), &base), vec!["Default/Web Data"]);
        let cache = find(&r, SourceId::Chrome, ItemId::Cache).unwrap();
        assert_eq!(cache.paths.len(), 2);
        assert!(cache.paths.iter().any(|p| p.ends_with("Library/Caches/Google/Chrome")));
        assert!(find(&r, SourceId::Chrome, ItemId::Downloads).is_none(), "in Chromium it lives inside History");
        // Passwords and bookmarks never appear.
        let all: Vec<&String> = r.sources.iter().flat_map(|s| &s.items).flat_map(|i| &i.paths).collect();
        assert!(!all.iter().any(|p| p.contains("Login Data") || p.contains("Bookmarks") || p.contains("logins.json")));
    }

    #[test]
    fn detects_firefox_profile_and_recent_items() {
        let tmp = fake_home();
        let h = tmp.path();
        let r = scan_at(h, &HashSet::new());
        let base = h.join("Library/Application Support/Firefox/Profiles/abcd.default-release");
        let n = |i| names(find(&r, SourceId::Firefox, i).unwrap(), &base);
        assert_eq!(n(ItemId::History), vec!["places.sqlite", "places.sqlite-wal"]);
        assert_eq!(n(ItemId::Cookies), vec!["cookies.sqlite", "cookies.sqlite-shm"]);
        assert_eq!(n(ItemId::Autofill), vec!["formhistory.sqlite"]);
        assert_eq!(n(ItemId::LocalStorage), vec!["storage"]);
        assert_eq!(n(ItemId::Sessions), vec!["sessionstore-backups", "sessionstore.jsonlz4"]);
        assert!(find(&r, SourceId::Firefox, ItemId::Downloads).is_none());
        let cache = find(&r, SourceId::Firefox, ItemId::Cache).unwrap();
        assert!(cache.paths[0].ends_with("Library/Caches/Firefox"));

        let recent = find(&r, SourceId::System, ItemId::RecentItems).unwrap();
        let sfl = h.join("Library/Application Support/com.apple.sharedfilelist");
        assert_eq!(
            names(recent, &sfl),
            vec![
                "com.apple.LSSharedFileList.ApplicationRecentDocuments/com.apple.textedit.sfl3",
                "com.apple.LSSharedFileList.RecentDocuments.sfl3"
            ]
        );
    }

    #[test]
    fn unreadable_folder_is_reported_not_fatal() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = fake_home();
        let h = tmp.path();
        let safari = h.join("Library/Safari");
        touch(&safari.join("History.db"), 4096);
        fs::set_permissions(&safari, fs::Permissions::from_mode(0o000)).unwrap();
        let r = scan_at(h, &HashSet::new());
        fs::set_permissions(&safari, fs::Permissions::from_mode(0o755)).unwrap();
        let history = find(&r, SourceId::Safari, ItemId::History).unwrap();
        assert!(!history.accessible);
        assert!(history.paths.is_empty());
        assert!(find(&r, SourceId::Safari, ItemId::Downloads).is_some());
    }

    #[test]
    fn clean_rejects_running_and_unknown() {
        let tmp = fake_home();
        let h = tmp.path();
        let item = |source: &str, item: &str| ItemRef { source: source.into(), item: item.into() };
        let req = PrivacyCleanRequest {
            items: vec![
                item("chrome", "history"),
                item("firefox", "cookies"),
                item("firefox", "cookies"),
                item("edge", "history"),
                item("chrome", "/etc"),
            ],
            dry_run: true,
        };
        let running: HashSet<String> = ["Google Chrome".to_string()].into();
        let r = clean_at(h, &running, &req);
        assert_eq!(r.outcomes[0].error.as_deref(), Some("Quit Google Chrome before cleaning"));
        assert!(!r.outcomes[0].ok);
        let ff: Vec<_> = r.outcomes.iter().filter(|o| o.category.as_deref() == Some("firefox.cookies")).collect();
        assert_eq!(ff.len(), 2, "no duplicates: {:?}", r.outcomes);
        assert!(ff.iter().all(|o| o.ok && o.action == Some(Action::Trash)));
        assert_eq!(r.failed, 3);
        assert_eq!(r.outcomes.len(), 5);
        assert!(r.trashed_bytes >= 2 * 4096);
        assert!(h.join("Library/Application Support/Firefox/Profiles/abcd.default-release/cookies.sqlite").exists());
    }

    #[test]
    fn pattern_matching() {
        assert!(Pat::Exact("Cookies").matches("Cookies-journal"));
        assert!(!Pat::Exact("Cookies").matches("Cookies.old"));
        assert!(Pat::ExtPrefix("sfl").matches("a.sfl2"));
        assert!(!Pat::ExtPrefix("sfl").matches(".sfl"));
        let v = dedupe_nested(vec!["/a/b".into(), "/a".into(), "/ab".into(), "/a".into()]);
        assert_eq!(v, vec![PathBuf::from("/a"), PathBuf::from("/ab")]);
    }

    #[test]
    fn lists_some_processes() {
        assert!(!running_process_names().is_empty());
    }
}
