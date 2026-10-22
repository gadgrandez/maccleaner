//! Applications: app inventory, leftovers in `~/Library`, orphaned leftovers,
//! validated uninstallation and extensions (panes, plugins, screen savers...).
//!
//! Everything is resolved against injectable roots (`Roots`); the public functions use
//! the real ones. Destructive operations only act on direct entries of known folders.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::clean::{CleanOutcome, CleanReport, CleanRequest};
use crate::fsutil;
use crate::rules::{Action, home_dir};
use crate::trash;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppSource {
    AppStore,
    Setapp,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: String,
    pub bundle_id: Option<String>,
    pub version: Option<String>,
    pub path: String,
    pub bytes: u64,
    pub is_apple: bool,
    pub source: AppSource,
    pub sparkle_feed_url: Option<String>,
    /// Architectures of the main executable, e.g. `["arm64","x86_64"]`.
    pub archs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeftoverKind {
    Cache,
    Preferences,
    AppSupport,
    Container,
    GroupContainer,
    SavedState,
    Logs,
    LaunchAgent,
    Cookies,
    Webkit,
    HttpStorage,
    Other,
}

impl LeftoverKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cache => "cache",
            Self::Preferences => "preferences",
            Self::AppSupport => "app_support",
            Self::Container => "container",
            Self::GroupContainer => "group_container",
            Self::SavedState => "saved_state",
            Self::Logs => "logs",
            Self::LaunchAgent => "launch_agent",
            Self::Cookies => "cookies",
            Self::Webkit => "webkit",
            Self::HttpStorage => "http_storage",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeftoverItem {
    pub path: String,
    pub kind: LeftoverKind,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppLeftovers {
    pub app: AppInfo,
    pub items: Vec<LeftoverItem>,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanGroup {
    /// Bundle id inferred from the entries' names.
    pub id: String,
    pub items: Vec<LeftoverItem>,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionKind {
    PreferencePane,
    Quicklook,
    Spotlight,
    InternetPlugin,
    ScreenSaver,
    Services,
    MailBundle,
}

impl ExtensionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreferencePane => "preference_pane",
            Self::Quicklook => "quicklook",
            Self::Spotlight => "spotlight",
            Self::InternetPlugin => "internet_plugin",
            Self::ScreenSaver => "screen_saver",
            Self::Services => "services",
            Self::MailBundle => "mail_bundle",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionScope {
    User,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionItem {
    pub name: String,
    pub path: String,
    pub kind: ExtensionKind,
    pub scope: ExtensionScope,
    pub bytes: u64,
    pub removable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UninstallRequest {
    #[serde(default)]
    pub app_path: Option<String>,
    #[serde(default)]
    pub leftover_paths: Vec<String>,
    /// By default nothing is touched.
    #[serde(default = "default_true")]
    pub dry_run: bool,
}

fn default_true() -> bool {
    true
}

/// File system roots; in tests they point to a temporary directory.
#[derive(Debug, Clone)]
struct Roots {
    home: PathBuf,
    applications: PathBuf,
    system_applications: PathBuf,
    /// `/Library` (system-wide extensions, installed agents and helpers).
    library: PathBuf,
}

impl Roots {
    fn real() -> Result<Self, String> {
        let home = home_dir().ok_or_else(|| "HOME is not set".to_string())?;
        Ok(Self {
            home,
            applications: "/Applications".into(),
            system_applications: "/System/Applications".into(),
            library: "/Library".into(),
        })
    }

    fn user_applications(&self) -> PathBuf {
        self.home.join("Applications")
    }

    fn user_library(&self) -> PathBuf {
        self.home.join("Library")
    }
}

// ---------------------------------------------------------------------------
// App inventory

struct Bundle {
    info: AppInfo,
    /// Candidate names to search for leftovers (display name, CFBundleName and file name).
    names: Vec<String>,
}

fn is_app(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("app"))
}

fn is_hidden(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_none_or(|n| n.starts_with('.'))
}

/// Real subdirectories (no symlinks or hidden ones) of `dir`.
fn real_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| !is_hidden(p))
        .collect()
}

/// `.app`s in a folder; with `nested`, also those in its subfolders (Utilities, Setapp...).
fn apps_in(dir: &Path, nested: bool, out: &mut Vec<PathBuf>) {
    for entry in real_dirs(dir) {
        if is_app(&entry) {
            out.push(entry);
        } else if nested {
            out.extend(real_dirs(&entry).into_iter().filter(|p| is_app(p)));
        }
    }
}

/// Paths of all apps with their system flag.
fn app_bundles(roots: &Roots) -> Vec<(PathBuf, bool)> {
    let mut out = Vec::new();
    for (dir, nested, system) in [
        (roots.applications.clone(), true, false),
        (roots.system_applications.clone(), true, true),
        (roots.user_applications(), false, false),
    ] {
        let mut found = Vec::new();
        apps_in(&dir, nested, &mut found);
        out.extend(found.into_iter().map(|p| (p, system)));
    }
    out
}

fn info_plist(app: &Path) -> Option<plist::Dictionary> {
    plist::Value::from_file(app.join("Contents/Info.plist")).ok()?.into_dictionary()
}

/// Whitespace and invisible direction marks (some apps put them in their name).
fn is_invisible(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

fn dict_str(d: &plist::Dictionary, key: &str) -> Option<String> {
    d.get(key).and_then(|v| v.as_string()).map(|s| s.trim_matches(is_invisible)).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn cpu_name(cputype: u32) -> Option<&'static str> {
    match cputype {
        0x0100_000c => Some("arm64"),
        0x0100_0007 => Some("x86_64"),
        0x0000_0007 => Some("i386"),
        _ => None,
    }
}

/// Architectures from the Mach-O header (universal or thin).
fn macho_archs(head: &[u8]) -> Vec<String> {
    let be = |at: usize| head.get(at..at + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap()));
    let le = |at: usize| head.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let mut types = Vec::new();
    match (be(0), le(0)) {
        (Some(magic @ (0xcafe_babe | 0xcafe_babf)), _) => {
            let stride = if magic == 0xcafe_babf { 32 } else { 20 };
            // Low limit: 0xcafebabe is also the magic of Java class files.
            let n = be(4).unwrap_or(0).min(16) as usize;
            types.extend((0..n).filter_map(|i| be(8 + i * stride)));
        }
        (_, Some(0xfeed_facf | 0xfeed_face)) => types.extend(le(4)),
        (Some(0xfeed_facf | 0xfeed_face), _) => types.extend(be(4)),
        _ => {}
    }
    let mut out: Vec<String> = Vec::new();
    for name in types.into_iter().filter_map(cpu_name) {
        if !out.iter().any(|o| o == name) {
            out.push(name.to_owned());
        }
    }
    out
}

fn exe_archs(exe: &Path) -> Vec<String> {
    let mut head = Vec::with_capacity(1024);
    match fs::File::open(exe).and_then(|f| f.take(1024).read_to_end(&mut head)) {
        Ok(_) => macho_archs(&head),
        Err(_) => Vec::new(),
    }
}

fn read_bundle(path: &Path, system: bool, roots: &Roots) -> Bundle {
    let dict = info_plist(path).unwrap_or_default();
    let get = |k: &str| dict_str(&dict, k);
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let bundle_name = get("CFBundleName");
    let name = get("CFBundleDisplayName").or_else(|| bundle_name.clone()).unwrap_or_else(|| stem.clone());
    let bundle_id = get("CFBundleIdentifier");
    let source = if path.join("Contents/_MASReceipt/receipt").exists() {
        AppSource::AppStore
    } else if path.starts_with(roots.applications.join("Setapp")) {
        AppSource::Setapp
    } else {
        AppSource::Other
    };
    // Apple apps outside /System without an App Store receipt are part of the system.
    let is_apple = system
        || (source != AppSource::AppStore && bundle_id.as_deref().is_some_and(|id| id.starts_with("com.apple.")));
    let exe = get("CFBundleExecutable").unwrap_or_else(|| stem.clone());
    let archs = if exe.contains('/') { Vec::new() } else { exe_archs(&path.join("Contents/MacOS").join(&exe)) };
    let mut names: Vec<String> = Vec::new();
    for n in [Some(name.clone()), bundle_name, Some(stem)].into_iter().flatten() {
        if !names.iter().any(|o| o.eq_ignore_ascii_case(&n)) {
            names.push(n);
        }
    }
    Bundle {
        info: AppInfo {
            name,
            bundle_id,
            version: get("CFBundleShortVersionString"),
            path: path.to_string_lossy().into_owned(),
            bytes: 0,
            is_apple,
            source,
            sparkle_feed_url: get("SUFeedURL"),
            archs,
        },
        names,
    }
}

fn list_apps_in(roots: &Roots) -> Vec<AppInfo> {
    let bundles = app_bundles(roots);
    let paths: Vec<PathBuf> = bundles.iter().map(|(p, _)| p.clone()).collect();
    let mut apps: Vec<AppInfo> = bundles.iter().map(|(p, sys)| read_bundle(p, *sys, roots).info).collect();
    // A single call: all sizes are computed in parallel.
    for (app, u) in apps.iter_mut().zip(fsutil::usages(&paths)) {
        app.bytes = u.bytes;
    }
    apps.sort_by_cached_key(|a| (a.name.to_lowercase(), a.path.clone()));
    apps
}

// ---------------------------------------------------------------------------
// An app's leftovers

/// How a leftover is recognized inside each folder (lowercase comparison).
#[derive(Clone, Copy)]
enum Matcher {
    /// `<id>`, `<id>.*` (if the id is long) or `<name>`.
    IdOrName,
    /// `<id>` or `<id>.*`.
    Id,
    /// `<id>.plist` or `<id>.*.plist`.
    Plist,
    /// `<id>.*` (ByHost files: `<id>.<UUID>.plist`).
    ByHost,
    /// The id appears delimited by dots: `group.<id>`, `<TEAM>.<id>`...
    Contains,
    /// Like `Contains`, but with a `.plist` extension.
    ContainsPlist,
    /// Exactly `<id><suffix>`.
    Suffix(&'static str),
}

/// Leftover folders under `~/Library`. They are also the only ones leftovers are deleted from.
const LEFTOVER_DIRS: &[(&str, LeftoverKind, Matcher)] = &[
    ("Caches", LeftoverKind::Cache, Matcher::IdOrName),
    ("Preferences", LeftoverKind::Preferences, Matcher::Plist),
    ("Preferences/ByHost", LeftoverKind::Preferences, Matcher::ByHost),
    ("Application Support", LeftoverKind::AppSupport, Matcher::IdOrName),
    ("Containers", LeftoverKind::Container, Matcher::Id),
    ("Group Containers", LeftoverKind::GroupContainer, Matcher::Contains),
    ("Saved Application State", LeftoverKind::SavedState, Matcher::Suffix(".savedstate")),
    ("Logs", LeftoverKind::Logs, Matcher::IdOrName),
    ("LaunchAgents", LeftoverKind::LaunchAgent, Matcher::ContainsPlist),
    ("Cookies", LeftoverKind::Cookies, Matcher::Suffix(".binarycookies")),
    ("WebKit", LeftoverKind::Webkit, Matcher::Id),
    ("HTTPStorages", LeftoverKind::HttpStorage, Matcher::Id),
];

/// Names too generic or shared to search for leftovers by name.
const GENERIC_NAMES: &[&str] = &[
    "app", "apps", "application", "applications", "apple", "google", "microsoft", "adobe", "mozilla",
    "jetbrains", "helper", "helpers", "update", "updater", "updates", "library", "caches", "logs",
    "preferences", "data", "default", "common", "shared", "system", "temp", "tmp", "cloudkit",
    "icloud", "clouddocs", "mobilesync", "addressbook", "callhistorydb", "knowledge", "fileprovider",
    "crashreporter", "diagnosticreports", "safari", "finder", "mail", "music", "photos",
];

/// `<id>` or `<id>.<something>`; the prefix only applies to ids with at least three labels.
fn id_or_child(e: &str, id: &str, child: bool) -> bool {
    e == id || (child && e.len() > id.len() && e.starts_with(id) && e.as_bytes()[id.len()] == b'.')
}

/// `id` appears in `e` delimited by start/end or dots.
fn bounded_contains(e: &str, id: &str) -> bool {
    e.match_indices(id).any(|(i, _)| {
        (i == 0 || e.as_bytes()[i - 1] == b'.') && matches!(e.as_bytes().get(i + id.len()), None | Some(b'.'))
    })
}

fn leftover_match(m: Matcher, e: &str, id: Option<&str>, names: &[String]) -> bool {
    let child = id.is_some_and(|id| id.split('.').count() >= 3);
    let by_id = |f: &dyn Fn(&str) -> bool| id.is_some_and(f);
    match m {
        Matcher::IdOrName => by_id(&|id| id_or_child(e, id, child)) || names.iter().any(|n| n == e),
        Matcher::Id => by_id(&|id| id_or_child(e, id, child)),
        Matcher::Plist => by_id(&|id| {
            e.strip_suffix(".plist").is_some_and(|s| s == id || (child && id_or_child(s, id, true)))
        }),
        Matcher::ByHost => by_id(&|id| id_or_child(e, id, true) && e != id),
        Matcher::Contains => child && by_id(&|id| bounded_contains(e, id)),
        Matcher::ContainsPlist => child && by_id(&|id| e.strip_suffix(".plist").is_some_and(|s| bounded_contains(s, id))),
        Matcher::Suffix(suffix) => by_id(&|id| e.strip_suffix(suffix) == Some(id)),
    }
}

fn valid_bundle_id(id: &str) -> bool {
    id.len() >= 5 && id.contains('.') && !id.starts_with('.') && !id.ends_with('.') && !id.contains("..")
}

/// Non-hidden direct entries of a directory, with their UTF-8 name.
fn entries(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok().map(|n| (n, e.path())))
        .filter(|(n, _)| !n.starts_with('.'))
        .collect()
}

/// `others`: installed ids not belonging to the app; an `<id>.*` that belongs to another installed
/// app with a more specific id (e.g. `<id>.for.testing`) is not taken as a leftover.
fn find_leftovers(
    library: &Path,
    bundle_id: Option<&str>,
    names: &[String],
    others: &HashSet<String>,
) -> Vec<(PathBuf, LeftoverKind)> {
    let id = bundle_id.filter(|id| valid_bundle_id(id)).map(str::to_lowercase);
    let names: Vec<String> = names
        .iter()
        .map(|n| n.trim().to_lowercase())
        .filter(|n| n.chars().count() >= 3 && !GENERIC_NAMES.contains(&n.as_str()) && !n.starts_with("com.apple"))
        .collect();
    let mut out = Vec::new();
    for (dir, kind, matcher) in LEFTOVER_DIRS {
        for (name, path) in entries(&library.join(dir)) {
            let lower = name.to_lowercase();
            let foreign = id.as_deref().is_some_and(|id| {
                let eid = entry_id(&lower);
                others.iter().any(|o| o.len() > id.len() && label_prefix(id, o) && label_prefix(o, eid))
            });
            if !foreign && leftover_match(*matcher, &lower, id.as_deref(), &names) {
                out.push((path, *kind));
            }
        }
    }
    out
}

fn leftovers_in(roots: &Roots, app_path: &Path) -> Result<AppLeftovers, String> {
    if !app_path.is_absolute() || !is_app(app_path) || !app_path.is_dir() {
        return Err("not an app (.app)".into());
    }
    let system = app_path.starts_with(&roots.system_applications);
    let bundle = read_bundle(app_path, system, roots);
    let mut own = HashSet::new();
    bundle_ids(app_path, &mut own);
    let others: HashSet<String> = app_ids(roots).difference(&own).cloned().collect();
    let found = find_leftovers(&roots.user_library(), bundle.info.bundle_id.as_deref(), &bundle.names, &others);
    let mut paths = vec![app_path.to_path_buf()];
    paths.extend(found.iter().map(|(p, _)| p.clone()));
    let mut sizes = fsutil::usages(&paths).into_iter().map(|u| u.bytes);
    let mut app = bundle.info;
    app.bytes = sizes.next().unwrap_or(0);
    let mut items: Vec<LeftoverItem> = found
        .into_iter()
        .zip(sizes)
        .map(|((p, kind), bytes)| LeftoverItem { path: p.to_string_lossy().into_owned(), kind, bytes })
        .collect();
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    let total_bytes = items.iter().map(|i| i.bytes).sum();
    Ok(AppLeftovers { app, items, total_bytes })
}

// ---------------------------------------------------------------------------
// Orphaned leftovers

const ORPHAN_DIRS: &[(&str, LeftoverKind)] = &[
    ("Containers", LeftoverKind::Container),
    ("Preferences", LeftoverKind::Preferences),
    ("Application Support", LeftoverKind::AppSupport),
    ("Caches", LeftoverKind::Cache),
    ("Saved Application State", LeftoverKind::SavedState),
    ("HTTPStorages", LeftoverKind::HttpStorage),
    ("WebKit", LeftoverKind::Webkit),
];

/// Prefixes (lowercase) of system ids or shared components.
const SYSTEM_ID_PREFIXES: &[&str] = &[
    "com.apple.",
    "group.com.apple.",
    "systemgroup.",
    "org.cups.",
    "org.sparkle-project.",
    "com.crashlytics",
    "io.fabric.",
    "org.swift.",
    "org.llvm.",
    "org.python.",
    "com.github.electron",
];

/// `tld.company.product`: at least three labels without spaces and a lowercase TLD.
fn is_reverse_dns(id: &str) -> bool {
    let labels: Vec<&str> = id.split('.').collect();
    labels.len() >= 3
        && labels.iter().all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        && (2..=10).contains(&labels[0].len())
        && labels[0].chars().all(|c| c.is_ascii_lowercase())
}

/// Ids (lowercase) of an app and of the bundles it contains (helpers, extensions...).
fn bundle_ids(app: &Path, out: &mut HashSet<String>) {
    let id_of = |p: &Path| info_plist(p).and_then(|d| dict_str(&d, "CFBundleIdentifier")).map(|s| s.to_lowercase());
    out.extend(id_of(app));
    for sub in [
        "Contents/Library/LoginItems",
        "Contents/Library/LaunchServices",
        "Contents/Library/SystemExtensions",
        "Contents/PlugIns",
        "Contents/Helpers",
        "Contents/XPCServices",
        "Contents/Frameworks",
    ] {
        for (_, p) in entries(&app.join(sub)) {
            if p.extension().is_some_and(|e| e != "framework") {
                out.extend(id_of(&p));
            }
        }
    }
}

/// Ids of installed apps and their inner bundles.
fn app_ids(roots: &Roots) -> HashSet<String> {
    let mut ids = HashSet::new();
    for (app, _) in app_bundles(roots) {
        bundle_ids(&app, &mut ids);
    }
    ids
}

/// Ids that count as installed: apps, their inner bundles and registered agents/helpers.
fn installed_ids(roots: &Roots) -> HashSet<String> {
    let mut ids = app_ids(roots);
    for dir in [
        roots.user_library().join("LaunchAgents"),
        roots.library.join("LaunchAgents"),
        roots.library.join("LaunchDaemons"),
        roots.library.join("PrivilegedHelperTools"),
    ] {
        for (name, _) in entries(&dir) {
            let lower = name.to_lowercase();
            ids.insert(lower.strip_suffix(".plist").unwrap_or(&lower).to_owned());
        }
    }
    ids
}

/// Id inferred from an entry's name (without `.plist`, `.savedState`...).
fn entry_id(name: &str) -> &str {
    for suffix in [".plist", ".savedstate", ".binarycookies"] {
        let cut = name.len().saturating_sub(suffix.len());
        if name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(suffix) {
            return &name[..cut];
        }
    }
    name
}

/// Prefix with a label separator: `com.foo.app` covers `com.foo.app.helper`.
fn label_prefix(prefix: &str, id: &str) -> bool {
    id_or_child(id, prefix, true)
}

/// `tld.company` of a lowercase id.
fn vendor(id: &str) -> Option<&str> {
    let mut dots = id.match_indices('.').map(|(i, _)| i);
    dots.next()?;
    Some(dots.next().map_or(id, |i| &id[..i]))
}

/// Conservative: anything from the same vendor as an installed app is also discarded
/// (updaters and shared components such as `com.google.Keystone`).
fn is_orphan(id: &str, installed: &HashSet<String>, vendors: &HashSet<String>) -> bool {
    let lower = id.to_lowercase();
    is_reverse_dns(id)
        && !SYSTEM_ID_PREFIXES.iter().any(|p| lower.starts_with(p))
        && !vendor(&lower).is_some_and(|v| vendors.contains(v))
        && !installed.iter().any(|i| label_prefix(i, &lower) || label_prefix(&lower, i))
}

fn orphans_in(roots: &Roots) -> Vec<OrphanGroup> {
    let installed = installed_ids(roots);
    let vendors: HashSet<String> =
        installed.iter().filter(|i| is_reverse_dns(i)).filter_map(|i| vendor(i).map(str::to_owned)).collect();
    let library = roots.user_library();
    let mut found: Vec<(String, PathBuf, LeftoverKind)> = Vec::new();
    for (dir, kind) in ORPHAN_DIRS {
        for (name, path) in entries(&library.join(dir)) {
            let id = entry_id(&name);
            if is_orphan(id, &installed, &vendors) {
                found.push((id.to_owned(), path, *kind));
            }
        }
    }
    let paths: Vec<PathBuf> = found.iter().map(|(_, p, _)| p.clone()).collect();
    let mut groups: Vec<OrphanGroup> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for ((id, path, kind), u) in found.into_iter().zip(fsutil::usages(&paths)) {
        let i = *index.entry(id.to_lowercase()).or_insert_with(|| {
            groups.push(OrphanGroup { id: id.clone(), items: Vec::new(), total_bytes: 0 });
            groups.len() - 1
        });
        let g = &mut groups[i];
        g.total_bytes += u.bytes;
        g.items.push(LeftoverItem { path: path.to_string_lossy().into_owned(), kind, bytes: u.bytes });
    }
    for g in &mut groups {
        g.items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    }
    groups.sort_by(|a, b| b.total_bytes.cmp(&a.total_bytes).then_with(|| a.id.cmp(&b.id)));
    groups
}

// ---------------------------------------------------------------------------
// Validating and executing deletions

type Remover<'a> = &'a dyn Fn(&Path) -> Result<(), String>;

/// Absolute path without `.` or `..`, with a parent and a name.
fn plain_abs(raw: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(raw);
    if !path.is_absolute()
        || path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        || path.parent().is_none()
        || path.file_name().is_none()
    {
        return Err("invalid path".into());
    }
    Ok(path)
}

fn canon_parent(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path.parent().unwrap_or(path)).map_err(|e| e.to_string())
}

fn validate_app(roots: &Roots, raw: &str) -> Result<(PathBuf, AppInfo), String> {
    let path = plain_abs(raw)?;
    if !is_app(&path) {
        return Err("not an app (.app)".into());
    }
    let md = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !md.is_dir() {
        return Err("not an app bundle".into());
    }
    let parent = canon_parent(&path)?;
    let canon = |p: &Path| fs::canonicalize(p).ok();
    let in_system = parent.starts_with("/System")
        || canon(&roots.system_applications).is_some_and(|s| parent.starts_with(s));
    if in_system {
        return Err("system apps cannot be uninstalled".into());
    }
    let apps = canon(&roots.applications);
    let allowed = Some(&parent) == apps.as_ref()
        || Some(&parent) == canon(&roots.user_applications()).as_ref()
        || (parent.parent() == apps.as_deref() && !is_app(&parent) && !is_hidden(&parent));
    if !allowed {
        return Err("outside the Applications folders".into());
    }
    let info = read_bundle(&path, false, roots).info;
    if info.is_apple {
        return Err("protected Apple app".into());
    }
    Ok((path, info))
}

fn protected_name(lower: &str) -> bool {
    ["com.apple.", "group.com.apple.", "systemgroup.", ".globalpreferences", "apple"].iter().any(|p| lower.starts_with(p))
}

/// Valid leftover: a direct entry (not the folder itself) of one of `LEFTOVER_DIRS`.
fn validate_leftover(roots: &Roots, raw: &str, app_id: Option<&str>) -> Result<(PathBuf, LeftoverKind), String> {
    let path = plain_abs(raw)?;
    let parent = canon_parent(&path)?;
    let library = roots.user_library();
    let canon_dirs: Vec<(PathBuf, LeftoverKind)> = LEFTOVER_DIRS
        .iter()
        .filter_map(|(d, k, _)| fs::canonicalize(library.join(d)).ok().map(|c| (c, *k)))
        .collect();
    let kind = canon_dirs
        .iter()
        .find(|(dir, _)| *dir == parent)
        .map(|(_, k)| *k)
        .ok_or_else(|| "outside the leftover folders".to_string())?;
    let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| "invalid path".to_string())?;
    let target = parent.join(name);
    if canon_dirs.iter().any(|(dir, _)| *dir == target) {
        return Err("a leftover folder cannot be deleted".into());
    }
    let lower = name.to_lowercase();
    let own = app_id.is_some_and(|id| valid_bundle_id(id) && lower.starts_with(&id.to_lowercase()));
    if protected_name(&lower) && !own {
        return Err("protected system item".into());
    }
    fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    Ok((path, kind))
}

type Checked = (String, Result<(PathBuf, String), String>);

/// Executes (or simulates) the deletions. With `gate_first`, if the first fails the rest are left untouched.
fn execute(checked: Vec<Checked>, dry_run: bool, gate_first: bool, remove: Remover) -> CleanReport {
    let valid: Vec<PathBuf> = checked.iter().filter_map(|(_, v)| v.as_ref().ok().map(|(p, _)| p.clone())).collect();
    let mut sizes = fsutil::usages(&valid).into_iter().map(|u| u.bytes);
    let mut report = CleanReport { dry_run, trashed_bytes: 0, deleted_bytes: 0, failed: 0, outcomes: Vec::new() };
    let mut gated = false;
    for (i, (raw, v)) in checked.into_iter().enumerate() {
        let outcome = match v {
            Err(error) => CleanOutcome { path: raw, category: None, action: None, bytes: 0, ok: false, error: Some(error) },
            Ok((path, category)) => {
                let bytes = sizes.next().unwrap_or(0);
                let result = if gated {
                    Err("the app was not uninstalled".to_string())
                } else if dry_run {
                    Ok(())
                } else {
                    remove(&path)
                };
                if result.is_ok() {
                    report.trashed_bytes += bytes;
                }
                CleanOutcome {
                    path: raw,
                    category: Some(category),
                    action: Some(Action::Trash),
                    bytes,
                    ok: result.is_ok(),
                    error: result.err(),
                }
            }
        };
        if !outcome.ok {
            report.failed += 1;
            gated |= gate_first && i == 0;
        }
        report.outcomes.push(outcome);
    }
    report
}

fn uninstall_in(roots: &Roots, req: &UninstallRequest, remove: Remover) -> CleanReport {
    let mut checked: Vec<Checked> = Vec::new();
    let mut app_id = None;
    if let Some(raw) = &req.app_path {
        let v = validate_app(roots, raw).map(|(path, info)| {
            app_id = info.bundle_id;
            (path, "app".to_string())
        });
        checked.push((raw.clone(), v));
    }
    for raw in &req.leftover_paths {
        let v = validate_leftover(roots, raw, app_id.as_deref()).map(|(p, k)| (p, k.as_str().to_string()));
        checked.push((raw.clone(), v));
    }
    // If the app cannot be removed, its data is kept.
    execute(checked, req.dry_run, req.app_path.is_some(), remove)
}

// ---------------------------------------------------------------------------
// Extensions

/// Folder relative to `Library`, required extension (or any) and kind.
const EXTENSION_DIRS: &[(&str, Option<&str>, ExtensionKind)] = &[
    ("PreferencePanes", Some("prefPane"), ExtensionKind::PreferencePane),
    ("QuickLook", Some("qlgenerator"), ExtensionKind::Quicklook),
    ("Spotlight", Some("mdimporter"), ExtensionKind::Spotlight),
    ("Internet Plug-Ins", None, ExtensionKind::InternetPlugin),
    ("Screen Savers", Some("saver"), ExtensionKind::ScreenSaver),
    ("Services", Some("workflow"), ExtensionKind::Services),
    ("Mail/Bundles", Some("mailbundle"), ExtensionKind::MailBundle),
];

fn ext_matches(path: &Path, ext: Option<&str>) -> bool {
    match ext {
        None => true,
        Some(ext) => path.extension().is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case(ext)),
    }
}

fn list_extensions_in(roots: &Roots) -> Vec<ExtensionItem> {
    let mut items = Vec::new();
    for (library, scope) in [(roots.user_library(), ExtensionScope::User), (roots.library.clone(), ExtensionScope::System)] {
        for (dir, ext, kind) in EXTENSION_DIRS {
            for (name, path) in entries(&library.join(dir)) {
                if !ext_matches(&path, *ext) {
                    continue;
                }
                let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(name);
                items.push(ExtensionItem {
                    name,
                    path: path.to_string_lossy().into_owned(),
                    kind: *kind,
                    scope,
                    bytes: 0,
                    removable: scope == ExtensionScope::User,
                });
            }
        }
    }
    let paths: Vec<PathBuf> = items.iter().map(|i| PathBuf::from(&i.path)).collect();
    for (item, u) in items.iter_mut().zip(fsutil::usages(&paths)) {
        item.bytes = u.bytes;
    }
    items.sort_by_cached_key(|i| (i.name.to_lowercase(), i.path.clone()));
    items
}

/// Deletable extension: direct child of a user extensions folder, with its file extension.
fn validate_extension(roots: &Roots, raw: &str) -> Result<(PathBuf, ExtensionKind), String> {
    let path = plain_abs(raw)?;
    let parent = canon_parent(&path)?;
    let library = roots.user_library();
    let (ext, kind) = EXTENSION_DIRS
        .iter()
        .find(|(d, _, _)| fs::canonicalize(library.join(d)).is_ok_and(|c| c == parent))
        .map(|(_, ext, kind)| (*ext, *kind))
        .ok_or_else(|| "outside the user extension folders".to_string())?;
    if is_hidden(&path) || !ext_matches(&path, ext) {
        return Err("not an extension of this kind".into());
    }
    fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    Ok((path, kind))
}

fn remove_extensions_in(roots: &Roots, req: &CleanRequest, remove: Remover) -> CleanReport {
    let checked = req
        .paths
        .iter()
        .map(|raw| (raw.clone(), validate_extension(roots, raw).map(|(p, k)| (p, k.as_str().to_string()))))
        .collect();
    execute(checked, req.dry_run, false, remove)
}

// ---------------------------------------------------------------------------
// Public API (real roots)

pub fn list_apps() -> Result<Vec<AppInfo>, String> {
    Roots::real().map(|r| list_apps_in(&r))
}

pub fn leftovers(app_path: &Path) -> Result<AppLeftovers, String> {
    leftovers_in(&Roots::real()?, app_path)
}

pub fn orphans() -> Result<Vec<OrphanGroup>, String> {
    Roots::real().map(|r| orphans_in(&r))
}

pub fn uninstall(req: &UninstallRequest) -> Result<CleanReport, String> {
    Ok(uninstall_in(&Roots::real()?, req, &trash::move_to_trash))
}

pub fn list_extensions() -> Result<Vec<ExtensionItem>, String> {
    Roots::real().map(|r| list_extensions_in(&r))
}

pub fn remove_extensions(req: &CleanRequest) -> Result<CleanReport, String> {
    Ok(remove_extensions_in(&Roots::real()?, req, &trash::move_to_trash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn roots(tmp: &TempDir) -> Roots {
        let base = tmp.path();
        let r = Roots {
            home: base.join("home"),
            applications: base.join("Applications"),
            system_applications: base.join("System/Applications"),
            library: base.join("Library"),
        };
        for d in [&r.home, &r.applications, &r.system_applications, &r.library] {
            fs::create_dir_all(d).unwrap();
        }
        r
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    fn fs_remove(p: &Path) -> Result<(), String> {
        let md = fs::symlink_metadata(p).map_err(|e| e.to_string())?;
        if md.is_dir() { fs::remove_dir_all(p) } else { fs::remove_file(p) }.map_err(|e| e.to_string())
    }

    fn fat_header(cputypes: &[u32]) -> Vec<u8> {
        let mut h = 0xcafe_babe_u32.to_be_bytes().to_vec();
        h.extend((cputypes.len() as u32).to_be_bytes());
        for t in cputypes {
            h.extend(t.to_be_bytes());
            h.extend([0u8; 16]);
        }
        h
    }

    fn thin_header(cputype: u32) -> Vec<u8> {
        let mut h = 0xfeed_facf_u32.to_le_bytes().to_vec();
        h.extend(cputype.to_le_bytes());
        h.extend([0u8; 24]);
        h
    }

    /// Creates `<dir>/<file>.app` with an Info.plist (binary or XML) and an executable.
    fn make_app(dir: &Path, file: &str, keys: &[(&str, &str)], binary: bool, exe: &[u8]) -> PathBuf {
        let app = dir.join(format!("{file}.app"));
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        let mut d = plist::Dictionary::new();
        d.insert("CFBundleExecutable".into(), plist::Value::String(file.into()));
        for (k, v) in keys {
            d.insert((*k).into(), plist::Value::String((*v).into()));
        }
        let v = plist::Value::Dictionary(d);
        let info = app.join("Contents/Info.plist");
        if binary { v.to_file_binary(&info).unwrap() } else { v.to_file_xml(&info).unwrap() }
        fs::write(app.join("Contents/MacOS").join(file), exe).unwrap();
        app
    }

    #[test]
    fn parses_macho_headers() {
        assert_eq!(macho_archs(&fat_header(&[0x0100_0007, 0x0100_000c])), vec!["x86_64", "arm64"]);
        assert_eq!(macho_archs(&thin_header(0x0100_000c)), vec!["arm64"]);
        let mut fat64 = 0xcafe_babf_u32.to_be_bytes().to_vec();
        fat64.extend(1u32.to_be_bytes());
        fat64.extend(0x0100_000c_u32.to_be_bytes());
        assert_eq!(macho_archs(&fat64), vec!["arm64"]);
        assert!(macho_archs(b"#!/bin/sh\n").is_empty());
        assert!(macho_archs(&[]).is_empty());
    }

    #[test]
    fn lists_apps_with_info_plist() {
        let tmp = TempDir::new("apps-list");
        let r = roots(&tmp);
        let foo = make_app(
            &r.applications,
            "Foo",
            &[
                ("CFBundleIdentifier", "com.foo.app"),
                ("CFBundleName", "Foo"),
                ("CFBundleDisplayName", "Foo Pro"),
                ("CFBundleShortVersionString", "2.1"),
                ("SUFeedURL", "https://foo.example/appcast.xml"),
            ],
            true,
            &fat_header(&[0x0100_000c, 0x0100_0007]),
        );
        let store = make_app(&r.applications, "Store", &[("CFBundleIdentifier", "com.store.app")], false, &thin_header(0x0100_000c));
        fs::create_dir_all(store.join("Contents/_MASReceipt")).unwrap();
        fs::write(store.join("Contents/_MASReceipt/receipt"), b"r").unwrap();
        fs::create_dir_all(r.applications.join("Setapp")).unwrap();
        make_app(&r.applications.join("Setapp"), "Bar", &[("CFBundleIdentifier", "com.bar.app")], false, b"");
        fs::create_dir_all(r.applications.join("Utilities")).unwrap();
        make_app(&r.applications.join("Utilities"), "Util", &[], false, b"");
        make_app(&r.system_applications, "Notes", &[("CFBundleIdentifier", "com.apple.Notes")], true, b"");
        make_app(&r.applications, "Browser", &[("CFBundleIdentifier", "com.apple.Safari")], true, b"");
        fs::create_dir_all(r.user_applications()).unwrap();
        make_app(&r.user_applications(), "Mine", &[("CFBundleIdentifier", "org.me.mine"), ("CFBundleDisplayName", "\u{200e}Mine ")], false, b"");
        std::os::unix::fs::symlink(&foo, r.applications.join("Alias.app")).unwrap();

        let apps = list_apps_in(&r);
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["Bar", "Browser", "Foo Pro", "Mine", "Notes", "Store", "Util"]);
        let get = |n: &str| apps.iter().find(|a| a.name == n).unwrap();
        let foo = get("Foo Pro");
        assert_eq!(foo.bundle_id.as_deref(), Some("com.foo.app"));
        assert_eq!(foo.version.as_deref(), Some("2.1"));
        assert_eq!(foo.sparkle_feed_url.as_deref(), Some("https://foo.example/appcast.xml"));
        assert_eq!(foo.archs, vec!["arm64", "x86_64"]);
        assert_eq!(foo.source, AppSource::Other);
        assert!(!foo.is_apple && foo.bytes > 0);
        assert_eq!(get("Store").source, AppSource::AppStore);
        assert_eq!(get("Store").archs, vec!["arm64"]);
        assert_eq!(get("Bar").source, AppSource::Setapp);
        assert!(get("Notes").is_apple);
        assert!(get("Browser").is_apple);
        assert!(!get("Mine").is_apple);
        assert!(get("Util").bundle_id.is_none());
    }

    fn touch(p: &Path, bytes: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![0u8; bytes]).unwrap();
    }

    #[test]
    fn finds_leftovers_by_id_and_name() {
        let tmp = TempDir::new("apps-leftovers");
        let r = roots(&tmp);
        let app = make_app(&r.applications, "Foo", &[("CFBundleIdentifier", "com.foo.App"), ("CFBundleName", "Foo")], false, b"");
        make_app(&r.applications, "Foo Test", &[("CFBundleIdentifier", "com.foo.App.for.testing")], false, b"");
        let lib = r.user_library();
        let expected = [
            ("Caches/com.foo.app/x", LeftoverKind::Cache),
            ("Caches/com.foo.App.ShipIt/x", LeftoverKind::Cache),
            ("Caches/Foo/x", LeftoverKind::Cache),
            ("Preferences/com.foo.App.plist", LeftoverKind::Preferences),
            ("Preferences/com.foo.App.helper.plist", LeftoverKind::Preferences),
            ("Preferences/ByHost/com.foo.App.0000-1111.plist", LeftoverKind::Preferences),
            ("Application Support/Foo/x", LeftoverKind::AppSupport),
            ("Application Support/com.foo.App/x", LeftoverKind::AppSupport),
            ("Containers/com.foo.App/x", LeftoverKind::Container),
            ("Group Containers/ABCDE12345.com.foo.App/x", LeftoverKind::GroupContainer),
            ("Group Containers/group.com.foo.App/x", LeftoverKind::GroupContainer),
            ("Saved Application State/com.foo.App.savedState/x", LeftoverKind::SavedState),
            ("Logs/Foo/x", LeftoverKind::Logs),
            ("LaunchAgents/com.foo.App.agent.plist", LeftoverKind::LaunchAgent),
            ("Cookies/com.foo.App.binarycookies", LeftoverKind::Cookies),
            ("WebKit/com.foo.App/x", LeftoverKind::Webkit),
            ("HTTPStorages/com.foo.App/x", LeftoverKind::HttpStorage),
            ("HTTPStorages/com.foo.App.binarycookies", LeftoverKind::HttpStorage),
        ];
        for (p, _) in &expected {
            touch(&lib.join(p), 5000);
        }
        for p in [
            "Caches/com.foo.Application/x",
            "Caches/com.foo/x",
            "Caches/Foobar/x",
            "Preferences/com.foo.Apps.plist",
            "Group Containers/group.com.foo.Application/x",
            "Containers/com.foo.Appx/x",
            "Documents/com.foo.App/x",
            "Application Support/Google/x",
            "Preferences/com.foo.App.for.testing.plist",
            "Caches/com.foo.app.for.testing/x",
        ] {
            touch(&lib.join(p), 10);
        }
        let got = leftovers_in(&r, &app).unwrap();
        assert_eq!(got.app.bundle_id.as_deref(), Some("com.foo.App"));
        let mut found: Vec<(String, LeftoverKind)> = got.items.iter().map(|i| (i.path.clone(), i.kind)).collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        let mut want: Vec<(String, LeftoverKind)> = expected
            .iter()
            .map(|(p, k)| {
                let top = p.strip_suffix("/x").unwrap_or(p);
                (s(&lib.join(top)), *k)
            })
            .collect();
        want.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(found, want);
        assert_eq!(got.total_bytes, got.items.iter().map(|i| i.bytes).sum::<u64>());
        assert!(got.total_bytes > 0);
    }

    #[test]
    fn short_or_generic_names_never_match() {
        let tmp = TempDir::new("apps-short");
        let r = roots(&tmp);
        let app = make_app(&r.applications, "Go", &[("CFBundleName", "Go")], false, b"");
        let lib = r.user_library();
        touch(&lib.join("Caches/Go/x"), 10);
        touch(&lib.join("Application Support/Google/x"), 10);
        assert!(leftovers_in(&r, &app).unwrap().items.is_empty());
        let app = make_app(&r.applications, "Google", &[], false, b"");
        assert!(leftovers_in(&r, &app).unwrap().items.is_empty());
        assert!(leftovers_in(&r, &r.applications.join("Nope.app")).is_err());
    }

    #[test]
    fn orphans_are_conservative_and_grouped() {
        let tmp = TempDir::new("apps-orphans");
        let r = roots(&tmp);
        let app = make_app(&r.applications, "Foo", &[("CFBundleIdentifier", "com.foo.app")], false, b"");
        make_app(&app.join("Contents/Library/LoginItems"), "Launcher", &[("CFBundleIdentifier", "com.other.launcher")], false, b"");
        let lib = r.user_library();
        touch(&r.library.join("LaunchDaemons/org.daemon.tool.plist"), 1);
        for (p, n) in [
            ("Containers/com.foo.app/x", 10),
            ("Containers/com.foo.app.extension/x", 10),
            ("Caches/com.other.launcher/x", 10),
            ("Caches/com.apple.Safari/x", 10),
            ("Preferences/group.com.apple.notes.plist", 10),
            ("Caches/Homebrew/x", 10),
            ("Caches/com.foo/x", 10),
            ("Application Support/org.daemon.tool/x", 10),
            ("Caches/ABCDE12345.com.x.y/x", 10),
            ("Caches/com.foo.Updater/x", 10),
            ("Preferences/com.other.shared.plist", 10),
            ("Caches/com.gone.app/x", 100),
            ("Preferences/com.gone.app.plist", 100),
            ("Saved Application State/com.gone.app.savedState/x", 100),
            ("Containers/net.big.thing/x", 900_000),
        ] {
            touch(&lib.join(p), n);
        }
        let groups = orphans_in(&r);
        let ids: Vec<&str> = groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, vec!["net.big.thing", "com.gone.app"]);
        assert_eq!(groups[1].items.len(), 3);
        assert_eq!(groups[1].total_bytes, groups[1].items.iter().map(|i| i.bytes).sum::<u64>());
        assert!(groups[0].total_bytes > groups[1].total_bytes);
    }

    #[test]
    fn uninstall_rejects_dangerous_paths() {
        let tmp = TempDir::new("apps-reject");
        let r = roots(&tmp);
        let lib = r.user_library();
        let sys = make_app(&r.system_applications, "Notes", &[("CFBundleIdentifier", "com.foo.notes")], false, b"");
        let apple = make_app(&r.applications, "Safari", &[("CFBundleIdentifier", "com.apple.Safari")], false, b"");
        let outside = make_app(&r.home.join("Downloads"), "Foo", &[], false, b"");
        let deep = make_app(&r.applications.join("A/B"), "Deep", &[], false, b"");
        let inner = make_app(&r.applications.join("Host.app/Contents"), "Inner", &[], false, b"");
        let real = make_app(&r.applications, "Real", &[], false, b"");
        std::os::unix::fs::symlink(&real, r.applications.join("Link.app")).unwrap();
        touch(&r.home.join("document.txt"), 10);
        touch(&lib.join("Caches/com.foo.app/x"), 10);
        touch(&lib.join("Caches/com.foo.app/deep/y"), 10);
        touch(&lib.join("Preferences/ByHost/com.foo.app.1.plist"), 10);
        touch(&lib.join("Preferences/com.apple.finder.plist"), 10);
        touch(&lib.join("Preferences/.GlobalPreferences.plist"), 10);
        touch(&lib.join("Mail/V10/x"), 10);

        for app in [
            s(&sys),
            s(&apple),
            s(&outside),
            s(&deep),
            s(&inner),
            s(&r.applications.join("Link.app")),
            s(&r.applications),
            format!("{}/../Applications/Real.app", s(&r.applications)),
            "Real.app".into(),
            s(&r.applications.join("Missing.app")),
        ] {
            let req = UninstallRequest { app_path: Some(app.clone()), leftover_paths: vec![], dry_run: false };
            let rep = uninstall_in(&r, &req, &fs_remove);
            assert_eq!(rep.failed, 1, "should reject {app}");
        }
        let req = UninstallRequest {
            app_path: None,
            leftover_paths: vec![
                s(&lib.join("Caches")),
                s(&lib.join("Preferences/ByHost")),
                s(&lib),
                s(&r.home.join("document.txt")),
                s(&lib.join("Caches/com.foo.app/deep")),
                format!("{}/../../document.txt", s(&lib.join("Caches"))),
                s(&lib.join("Preferences/com.apple.finder.plist")),
                s(&lib.join("Preferences/.GlobalPreferences.plist")),
                s(&lib.join("Mail/V10")),
                "relative".into(),
            ],
            dry_run: false,
        };
        let rep = uninstall_in(&r, &req, &fs_remove);
        assert_eq!(rep.failed, 10, "{:?}", rep.outcomes);
        for p in [&sys, &apple, &outside, &deep, &inner, &real] {
            assert!(p.exists());
        }
        assert!(r.home.join("document.txt").exists());
        assert!(lib.join("Caches/com.foo.app/deep/y").exists());
        assert!(lib.join("Preferences/ByHost/com.foo.app.1.plist").exists());
        assert!(lib.join("Preferences/com.apple.finder.plist").exists());
    }

    #[test]
    fn uninstall_dry_run_then_real() {
        let tmp = TempDir::new("apps-uninstall");
        let r = roots(&tmp);
        let lib = r.user_library();
        let app = make_app(&r.applications.join("Utilities"), "Foo", &[("CFBundleIdentifier", "com.foo.app")], false, &[0u8; 9000]);
        touch(&lib.join("Caches/com.foo.app/x"), 20_000);
        touch(&lib.join("Preferences/com.foo.app.plist"), 100);
        let leftovers = vec![s(&lib.join("Caches/com.foo.app")), s(&lib.join("Preferences/com.foo.app.plist"))];

        let req = UninstallRequest { app_path: Some(s(&app)), leftover_paths: leftovers, dry_run: default_true() };
        assert!(req.dry_run);
        let rep = uninstall_in(&r, &req, &fs_remove);
        assert_eq!(rep.failed, 0, "{:?}", rep.outcomes);
        assert!(rep.dry_run && rep.trashed_bytes >= 29_000);
        assert_eq!(rep.outcomes[0].category.as_deref(), Some("app"));
        assert_eq!(rep.outcomes[1].category.as_deref(), Some("cache"));
        assert!(app.exists());

        let rep = uninstall_in(&r, &UninstallRequest { dry_run: false, ..req }, &fs_remove);
        assert_eq!(rep.failed, 0, "{:?}", rep.outcomes);
        assert!(!app.exists());
        assert!(!lib.join("Caches/com.foo.app").exists());
        assert!(!lib.join("Preferences/com.foo.app.plist").exists());
        assert!(lib.join("Caches").exists());
    }

    #[test]
    fn failed_app_keeps_leftovers() {
        let tmp = TempDir::new("apps-gate");
        let r = roots(&tmp);
        let lib = r.user_library();
        let app = make_app(&r.applications, "Foo", &[("CFBundleIdentifier", "com.foo.app")], false, b"");
        touch(&lib.join("Caches/com.foo.app/x"), 10);
        let req = UninstallRequest {
            app_path: Some(s(&app)),
            leftover_paths: vec![s(&lib.join("Caches/com.foo.app"))],
            dry_run: false,
        };
        let rep = uninstall_in(&r, &req, &|_| Err("in use".into()));
        assert_eq!(rep.failed, 2);
        assert_eq!(rep.outcomes[1].error.as_deref(), Some("the app was not uninstalled"));

        let req = UninstallRequest { app_path: Some(s(&r.home)), ..req };
        let rep = uninstall_in(&r, &req, &fs_remove);
        assert_eq!(rep.failed, 2);
        assert!(lib.join("Caches/com.foo.app").exists());
    }

    #[test]
    fn extensions_list_and_remove() {
        let tmp = TempDir::new("apps-ext");
        let r = roots(&tmp);
        let lib = r.user_library();
        let user_ql = lib.join("QuickLook/Foo.qlgenerator");
        touch(&user_ql.join("Contents/Info.plist"), 100);
        touch(&lib.join("QuickLook/notes.txt"), 1);
        let user_pane = lib.join("PreferencePanes/Pane.prefPane");
        touch(&user_pane.join("x"), 100);
        let sys_md = r.library.join("Spotlight/Bar.mdimporter");
        touch(&sys_md.join("x"), 100);
        touch(&r.library.join("Internet Plug-Ins/Flash.plugin/x"), 100);
        touch(&lib.join("Mail/Bundles/M.mailbundle/x"), 10);

        let items = list_extensions_in(&r);
        let summary: Vec<(&str, ExtensionKind, ExtensionScope, bool)> =
            items.iter().map(|i| (i.name.as_str(), i.kind, i.scope, i.removable)).collect();
        assert_eq!(
            summary,
            vec![
                ("Bar", ExtensionKind::Spotlight, ExtensionScope::System, false),
                ("Flash", ExtensionKind::InternetPlugin, ExtensionScope::System, false),
                ("Foo", ExtensionKind::Quicklook, ExtensionScope::User, true),
                ("M", ExtensionKind::MailBundle, ExtensionScope::User, true),
                ("Pane", ExtensionKind::PreferencePane, ExtensionScope::User, true),
            ]
        );
        assert!(items.iter().all(|i| i.bytes > 0));

        let req = CleanRequest {
            paths: vec![
                s(&sys_md),
                s(&lib.join("QuickLook")),
                s(&lib.join("QuickLook/notes.txt")),
                s(&user_ql.join("Contents")),
                s(&user_pane),
                s(&user_ql),
            ],
            dry_run: false,
        };
        let rep = remove_extensions_in(&r, &req, &fs_remove);
        assert_eq!(rep.failed, 4, "{:?}", rep.outcomes);
        assert_eq!(rep.outcomes[4].category.as_deref(), Some("preference_pane"));
        assert!(!user_pane.exists() && !user_ql.exists());
        assert!(sys_md.exists() && lib.join("QuickLook/notes.txt").exists());
    }

    #[test]
    fn reverse_dns_shape() {
        assert!(is_reverse_dns("com.foo.app"));
        assert!(is_reverse_dns("io.some-thing.App_2"));
        assert!(!is_reverse_dns("com.foo"));
        assert!(!is_reverse_dns("ABCDE12345.com.foo"));
        assert!(!is_reverse_dns("Google Chrome"));
        assert!(!is_reverse_dns("com..foo"));
        assert!(bounded_contains("group.com.foo.app", "com.foo.app"));
        assert!(!bounded_contains("com.foo.application", "com.foo.app"));
    }
}
