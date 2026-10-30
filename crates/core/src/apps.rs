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
use crate::codesign;
use crate::fsutil;
use crate::performance::{self, missing_program, read_plist, sh_quote};
use crate::rules::{Action, home_dir};
use crate::{brew, progress, trash};

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
    AppScripts,
    LaunchDaemon,
    PrivilegedHelper,
    /// The record Homebrew keeps of a cask whose app was deleted without it.
    HomebrewCask,
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
            Self::AppScripts => "app_scripts",
            Self::LaunchDaemon => "launch_daemon",
            Self::PrivilegedHelper => "privileged_helper",
            Self::HomebrewCask => "homebrew_cask",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeftoverItem {
    pub path: String,
    pub kind: LeftoverKind,
    pub bytes: u64,
    /// System-wide launchd job or helper: removing it asks for an administrator password.
    #[serde(default)]
    pub requires_admin: bool,
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
    /// Audio device driver (`Audio/Plug-Ins/HAL`): virtual microphones and outputs.
    AudioDriver,
    /// Legacy camera plugin (`CoreMediaIO/Plug-Ins/DAL`).
    CameraPlugin,
    /// Camera system extension (virtual cameras).
    CameraExtension,
    /// Driver system extension (`.dext`).
    DriverExtension,
    /// Any other system extension (network filters, endpoint security...).
    SystemExtension,
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
            Self::AudioDriver => "audio_driver",
            Self::CameraPlugin => "camera_plugin",
            Self::CameraExtension => "camera_extension",
            Self::DriverExtension => "driver_extension",
            Self::SystemExtension => "system_extension",
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
    /// App that installed it (system extensions).
    #[serde(default)]
    pub origin: Option<String>,
    /// The app that installed it is gone, but the extension or driver is still loaded.
    #[serde(default)]
    pub orphan: bool,
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
    /// The item was installed by an installer package (it has a receipt).
    packaged: fn(&Path) -> bool,
    /// The Homebrew cask that installed the app, which `brew` removes so it stays in sync.
    cask_of: fn(&Path) -> Option<String>,
    uninstall_cask: fn(&str) -> Result<(), String>,
    /// Casks whose app was deleted without Homebrew: their record is a leftover of the app.
    deleted_casks: fn() -> Vec<brew::DeletedCask>,
}

impl Roots {
    fn real() -> Result<Self, String> {
        let home = home_dir().ok_or_else(|| "HOME is not set".to_string())?;
        Ok(Self {
            home,
            applications: "/Applications".into(),
            system_applications: "/System/Applications".into(),
            library: "/Library".into(),
            packaged: has_receipt,
            cask_of: brew::cask_for_app,
            uninstall_cask: |token| brew::uninstall(token, brew::BrewKind::Cask),
            deleted_casks: brew::deleted_casks,
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
    ("Application Scripts", LeftoverKind::AppScripts, Matcher::Contains),
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
/// `shared`: app groups other installed apps are entitled to; their containers are kept.
fn find_leftovers(
    library: &Path,
    bundle_id: Option<&str>,
    names: &[String],
    others: &HashSet<String>,
    shared: &HashSet<String>,
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
            let foreign = shared.contains(&lower)
                || id.as_deref().is_some_and(|id| {
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
    let shared = app_groups(roots, Some(app_path));
    let found = find_leftovers(&roots.user_library(), bundle.info.bundle_id.as_deref(), &bundle.names, &others, &shared);
    let mut paths = vec![app_path.to_path_buf()];
    paths.extend(found.iter().map(|(p, _)| p.clone()));
    let mut sizes = fsutil::usages(&paths).into_iter().map(|u| u.bytes);
    let mut app = bundle.info;
    app.bytes = sizes.next().unwrap_or(0);
    let mut items: Vec<LeftoverItem> = found
        .into_iter()
        .zip(sizes)
        .map(|((p, kind), bytes)| LeftoverItem { path: p.to_string_lossy().into_owned(), kind, bytes, requires_admin: false })
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
    ("Group Containers", LeftoverKind::GroupContainer),
    ("Application Scripts", LeftoverKind::AppScripts),
];

/// Folders whose entries are named after the app rather than its id (`CleanMyMac_5_Setapp`).
const NAMED_ORPHAN_DIRS: &[(&str, LeftoverKind)] = &[
    ("Application Support", LeftoverKind::AppSupport),
    ("Caches", LeftoverKind::Cache),
    ("Logs", LeftoverKind::Logs),
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
    // Shortcuts (formerly Workflow, bought by Apple).
    "is.workflow.",
];

/// `tld.company.product`: at least three labels without spaces and a lowercase TLD.
fn is_reverse_dns(id: &str) -> bool {
    let labels: Vec<&str> = id.split('.').collect();
    labels.len() >= 3
        && labels.iter().all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        && (2..=10).contains(&labels[0].len())
        && labels[0].chars().all(|c| c.is_ascii_lowercase())
}

/// An app and the bundles it contains (helpers, extensions, login items...).
fn inner_bundles(app: &Path) -> Vec<PathBuf> {
    let mut out = vec![app.to_path_buf()];
    for sub in [
        "Contents/Library/LoginItems",
        "Contents/Library/LaunchServices",
        "Contents/Library/SystemExtensions",
        "Contents/PlugIns",
        "Contents/Helpers",
        "Contents/XPCServices",
        "Contents/Frameworks",
    ] {
        out.extend(entries(&app.join(sub)).into_iter().map(|(_, p)| p).filter(|p| p.extension().is_some_and(|e| e != "framework")));
    }
    out
}

/// Ids (lowercase) of an app and of the bundles it contains.
fn bundle_ids(app: &Path, out: &mut HashSet<String>) {
    for bundle in inner_bundles(app) {
        out.extend(info_plist(&bundle).and_then(|d| dict_str(&d, "CFBundleIdentifier")).map(|s| s.to_lowercase()));
    }
}

/// App groups (lowercase) that installed apps (except `skip`) and their inner bundles are entitled
/// to. Their shared containers may be named after another vendor (WhatsApp uses
/// `group.com.facebook.family`).
fn app_groups(roots: &Roots, skip: Option<&Path>) -> HashSet<String> {
    let mut groups = HashSet::new();
    for (app, _) in app_bundles(roots).into_iter().filter(|(a, _)| Some(a.as_path()) != skip) {
        for bundle in inner_bundles(&app) {
            let Some(exe) = info_plist(&bundle).and_then(|d| dict_str(&d, "CFBundleExecutable")) else { continue };
            if !exe.contains('/') {
                let groups_of = codesign::app_groups(&bundle.join("Contents/MacOS").join(exe));
                groups.extend(groups_of.into_iter().map(|g| g.to_lowercase()));
            }
        }
    }
    groups
}

/// Ids of installed apps and their inner bundles, plus active system and driver extensions
/// (they outlive the app that installed them and still use its containers), except those whose
/// app is gone.
fn app_ids(roots: &Roots) -> HashSet<String> {
    let mut ids = HashSet::new();
    for (app, _) in app_bundles(roots) {
        bundle_ids(&app, &mut ids);
    }
    let orphaned: HashSet<String> =
        system_extensions(roots, &ids).into_iter().filter(|e| e.orphan).map(|e| e.id.to_lowercase()).collect();
    for dir in real_dirs(&roots.library.join("SystemExtensions")) {
        for (name, _) in entries(&dir) {
            let lower = name.to_lowercase();
            if let Some(id) = lower.strip_suffix(".systemextension").or_else(|| lower.strip_suffix(".dext")) {
                if !orphaned.contains(id) {
                    ids.insert(id.to_owned());
                }
            }
        }
    }
    ids
}

/// A system or driver extension registered with macOS.
#[derive(Debug, Clone)]
struct SystemExtension {
    id: String,
    /// Staged copy in `/Library/SystemExtensions/<uuid>/`.
    path: PathBuf,
    kind: ExtensionKind,
    /// App the extension was activated from.
    origin: Option<PathBuf>,
    /// Its app is no longer there (nor installed elsewhere).
    orphan: bool,
}

/// Active extensions in `/Library/SystemExtensions/db.plist` (readable without privileges).
/// `app_ids`: ids of installed apps and their inner bundles, in case the app was moved.
fn system_extensions(roots: &Roots, app_ids: &HashSet<String>) -> Vec<SystemExtension> {
    let dir = roots.library.join("SystemExtensions");
    let list = plist::Value::from_file(dir.join("db.plist"))
        .ok()
        .and_then(|v| v.into_dictionary())
        .and_then(|mut d| d.remove("extensions"))
        .and_then(|v| v.into_array())
        .unwrap_or_default();
    list.into_iter()
        .filter_map(|v| {
            let d = v.into_dictionary()?;
            if !dict_str(&d, "state")?.starts_with("activated") {
                return None;
            }
            let id = dict_str(&d, "identifier")?;
            let unique = dict_str(&d, "uniqueID").filter(|u| !u.contains('/') && !u.starts_with('.'))?;
            let (_, path) = entries(&dir.join(unique)).into_iter().next()?;
            let category = |c: &str| {
                d.get("categories").and_then(|v| v.as_array()).is_some_and(|a| a.iter().any(|v| v.as_string() == Some(c)))
            };
            let kind = if category("com.apple.system_extension.cmio") {
                ExtensionKind::CameraExtension
            } else if category("com.apple.system_extension.driver_extension") {
                ExtensionKind::DriverExtension
            } else {
                ExtensionKind::SystemExtension
            };
            let origin = dict_str(&d, "originPath")
                .and_then(|p| Path::new(&p).ancestors().filter(|a| is_app(a)).last().map(Path::to_path_buf));
            let orphan = origin.as_ref().is_some_and(|a| !a.exists()) && !app_ids.contains(&id.to_lowercase());
            Some(SystemExtension { id, path, kind, origin, orphan })
        })
        .collect()
}

/// `pkgutil` knows which installer package placed `path`.
fn has_receipt(path: &Path) -> bool {
    std::process::Command::new("/usr/sbin/pkgutil")
        .arg("--file-info")
        .arg(path)
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.starts_with("pkgid:")))
}

/// Ids (lowercase) of the apps installed in the usual folders and of their inner bundles.
pub fn installed_app_ids() -> HashSet<String> {
    Roots::real().map(|r| app_ids(&r)).unwrap_or_default()
}

/// launchd domain a job is booted out of before its file is removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobDomain {
    System,
    User,
}

/// A launchd job file: an agent or daemon plist, or a privileged helper tool.
#[derive(Debug, Clone)]
struct JobFile {
    label: String,
    path: PathBuf,
    kind: LeftoverKind,
    domain: JobDomain,
    /// Outside the home folder: removing it needs administrator privileges.
    admin: bool,
    /// Its program no longer exists, or (helpers) no daemon plist launches it any more.
    dead: bool,
}

fn job_files(roots: &Roots) -> Vec<JobFile> {
    let mut jobs = Vec::new();
    // Labels and program names launched by daemon plists: the helpers they keep alive.
    let mut launched: HashSet<String> = HashSet::new();
    for (dir, kind, domain, admin) in [
        (roots.user_library().join("LaunchAgents"), LeftoverKind::LaunchAgent, JobDomain::User, false),
        (roots.library.join("LaunchAgents"), LeftoverKind::LaunchAgent, JobDomain::User, true),
        (roots.library.join("LaunchDaemons"), LeftoverKind::LaunchDaemon, JobDomain::System, true),
    ] {
        for (name, path) in entries(&dir) {
            let Some(stem) = name.strip_suffix(".plist") else { continue };
            let (label, program) = match read_plist(&path) {
                Some(info) => (info.label, info.program),
                None => (stem.to_owned(), None),
            };
            if kind == LeftoverKind::LaunchDaemon {
                launched.insert(label.to_lowercase());
                launched.extend(program.as_deref().and_then(|p| Path::new(p).file_name()).map(|n| n.to_string_lossy().to_lowercase()));
            }
            let dead = program.as_deref().is_some_and(missing_program);
            jobs.push(JobFile { label, path, kind, domain, admin, dead });
        }
    }
    for (name, path) in entries(&roots.library.join("PrivilegedHelperTools")) {
        let dead = !launched.contains(&name.to_lowercase());
        jobs.push(JobFile { label: name, path, kind: LeftoverKind::PrivilegedHelper, domain: JobDomain::System, admin: true, dead });
    }
    jobs
}

/// What counts as installed when looking for orphans.
struct Installed {
    /// Apps, their inner bundles and the launchd jobs that still work.
    ids: HashSet<String>,
    /// `tld.company` of those ids.
    vendors: HashSet<String>,
    /// App groups installed apps are entitled to: their shared containers are in use.
    groups: HashSet<String>,
    /// Dead launchd jobs whose app is gone.
    orphan_jobs: Vec<JobFile>,
}

impl Installed {
    fn scan(roots: &Roots) -> Self {
        let jobs = job_files(roots);
        let mut ids = app_ids(roots);
        ids.extend(jobs.iter().filter(|j| !j.dead).map(|j| j.label.to_lowercase()));
        let vendors = ids.iter().filter(|i| is_reverse_dns(i)).filter_map(|i| vendor(i).map(str::to_owned)).collect();
        let mut installed = Self { ids, vendors, groups: app_groups(roots, None), orphan_jobs: Vec::new() };
        installed.orphan_jobs = jobs.into_iter().filter(|j| j.dead && installed.is_orphan(&j.label)).collect();
        installed
    }

    fn is_orphan(&self, id: &str) -> bool {
        is_orphan(id, &self.ids, &self.vendors)
    }
}

/// Id inferred from an entry's name: without `.plist`, `.savedState`... and without the
/// `<TEAMID>.` and `group.` prefixes of shared containers. What is left of a group may be shorter
/// than an app id (`group.com.docker` gives `com.docker`), which never counts as an orphan.
fn entry_id(name: &str) -> &str {
    let mut id = name;
    for suffix in [".plist", ".savedstate", ".binarycookies"] {
        let cut = name.len().saturating_sub(suffix.len());
        if name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(suffix) {
            id = &name[..cut];
            break;
        }
    }
    if let Some((first, rest)) = id.split_once('.') {
        if is_team_id(first) && rest.contains('.') {
            id = rest;
        }
    }
    match id.split_once('.') {
        Some((first, rest)) if first.eq_ignore_ascii_case("group") || first.eq_ignore_ascii_case("groups") => rest,
        _ => id,
    }
}

/// Apple developer team id: ten letters and digits, at least one of them a digit.
fn is_team_id(s: &str) -> bool {
    s.len() == 10 && s.bytes().all(|b| b.is_ascii_alphanumeric()) && s.bytes().any(|b| b.is_ascii_digit())
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

/// Product name of an id, used to find folders named after the app: `com.macpaw.CleanMyMac-setapp`
/// gives `cleanmymac`. Short or generic names give none.
fn product_token(id: &str) -> Option<String> {
    let label = id.split('.').nth(2)?.to_lowercase();
    let token: String = label.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    (token.len() >= 6 && !GENERIC_NAMES.contains(&token.as_str())).then_some(token)
}

/// `cleanmymac_5_setapp` or `cleanmymac x` for the token `cleanmymac`, but not `cleanmymacs`.
fn named_after(entry: &str, token: &str) -> bool {
    entry.strip_prefix(token).is_some_and(|rest| rest.chars().next().is_none_or(|c| !c.is_ascii_alphabetic()))
}

/// Names of installed apps, lowercase and without spaces or punctuation.
fn installed_names(roots: &Roots) -> Vec<String> {
    let squash = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
    let mut names = Vec::new();
    for (app, _) in app_bundles(roots) {
        names.extend(app.file_stem().map(|s| squash(&s.to_string_lossy())));
        if let Some(d) = info_plist(&app) {
            names.extend(["CFBundleName", "CFBundleDisplayName"].iter().filter_map(|k| dict_str(&d, k)).map(|n| squash(&n)));
        }
    }
    names.retain(|n| n.len() >= 3);
    names
}

/// Maps every id to the shortest id it extends: `com.foo.app.helper` belongs to `com.foo.app`.
fn group_roots(ids: impl Iterator<Item = String>) -> HashMap<String, String> {
    let mut unique: Vec<String> = ids.collect::<HashSet<_>>().into_iter().collect();
    unique.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    let mut roots: Vec<String> = Vec::new();
    let mut out = HashMap::new();
    for id in unique {
        let root = roots.iter().find(|r| label_prefix(r, &id)).cloned().unwrap_or_else(|| {
            roots.push(id.clone());
            id.clone()
        });
        out.insert(id, root);
    }
    out
}

struct Found {
    id: String,
    path: PathBuf,
    kind: LeftoverKind,
    admin: bool,
}

fn orphans_in(roots: &Roots) -> Vec<OrphanGroup> {
    let installed = Installed::scan(roots);
    let library = roots.user_library();
    let mut found: Vec<Found> = Vec::new();
    for (dir, kind) in ORPHAN_DIRS {
        for (name, path) in entries(&library.join(dir)) {
            if installed.groups.contains(&name.to_lowercase()) {
                continue;
            }
            let id = entry_id(&name);
            if installed.is_orphan(id) {
                found.push(Found { id: id.to_owned(), path, kind: *kind, admin: false });
            }
        }
    }
    for job in &installed.orphan_jobs {
        found.push(Found { id: job.label.clone(), path: job.path.clone(), kind: job.kind, admin: job.admin });
    }
    // The cask joins its app's group through the bundle id its stanzas mention.
    for cask in (roots.deleted_casks)() {
        let ids: Vec<&str> = cask.names.iter().map(|n| entry_id(n)).filter(|id| is_reverse_dns(id)).collect();
        let id = ids
            .iter()
            .find(|id| found.iter().any(|f| f.id.eq_ignore_ascii_case(id)))
            .or(ids.first())
            .map_or(cask.token.clone(), |id| id.to_string());
        found.push(Found { id, path: cask.caskroom, kind: LeftoverKind::HomebrewCask, admin: false });
    }
    let root_of = group_roots(found.iter().map(|f| f.id.to_lowercase()));

    // Folders named after an orphaned app join its group, unless an installed app has that name.
    let names = installed_names(roots);
    let mut tokens: Vec<(String, String)> = root_of
        .values()
        .collect::<HashSet<_>>()
        .into_iter()
        .filter_map(|root| product_token(root).map(|t| (t, root.clone())))
        .filter(|(t, _)| !names.iter().any(|n| n.starts_with(t.as_str()) || t.starts_with(n.as_str())))
        .collect();
    tokens.sort();
    let taken: HashSet<PathBuf> = found.iter().map(|f| f.path.clone()).collect();
    for (dir, kind) in NAMED_ORPHAN_DIRS {
        for (name, path) in entries(&library.join(dir)) {
            let lower = name.to_lowercase();
            if taken.contains(&path) || is_reverse_dns(entry_id(&name)) || protected_name(&lower) {
                continue;
            }
            if let Some((_, root)) = tokens.iter().find(|(t, _)| named_after(&lower, t)) {
                found.push(Found { id: root.clone(), path, kind: *kind, admin: false });
            }
        }
    }

    let paths: Vec<PathBuf> = found.iter().map(|f| f.path.clone()).collect();
    let mut groups: Vec<OrphanGroup> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    // Display ids keep their original case: the first entry whose id is the group's root.
    let mut display: HashMap<String, String> = HashMap::new();
    for f in &found {
        let lower = f.id.to_lowercase();
        if root_of.get(&lower) == Some(&lower) {
            display.entry(lower).or_insert_with(|| f.id.clone());
        }
    }
    for (f, u) in found.into_iter().zip(fsutil::usages(&paths)) {
        let lower = f.id.to_lowercase();
        let root = root_of.get(&lower).cloned().unwrap_or(lower);
        let i = *index.entry(root.clone()).or_insert_with(|| {
            let id = display.get(&root).cloned().unwrap_or_else(|| root.clone());
            groups.push(OrphanGroup { id, items: Vec::new(), total_bytes: 0 });
            groups.len() - 1
        });
        let g = &mut groups[i];
        g.total_bytes += u.bytes;
        g.items.push(LeftoverItem {
            path: f.path.to_string_lossy().into_owned(),
            kind: f.kind,
            bytes: u.bytes,
            requires_admin: f.admin,
        });
    }
    for g in &mut groups {
        g.items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    }
    groups.sort_by(|a, b| b.total_bytes.cmp(&a.total_bytes).then_with(|| a.id.cmp(&b.id)));
    groups
}

// ---------------------------------------------------------------------------
// Validating and executing deletions

/// Removes an item; returns how (Trash or permanent deletion).
type Remover<'a> = &'a dyn Fn(&Path) -> Result<Action, String>;

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
    let _progress = progress::start(valid.len(), dry_run);
    let mut sizes = fsutil::usages(&valid).into_iter().map(|u| u.bytes);
    let mut report = CleanReport::new(dry_run);
    let mut gated = false;
    for (i, (raw, v)) in checked.into_iter().enumerate() {
        let outcome = match v {
            Err(error) => CleanOutcome { path: raw, category: None, action: None, bytes: 0, ok: false, error: Some(error) },
            Ok((path, category)) => {
                let bytes = sizes.next().unwrap_or(0);
                let result = if gated {
                    Err("the app was not uninstalled".to_string())
                } else if dry_run {
                    Ok(trash::preferred_action())
                } else {
                    progress::step(&path, bytes, || remove(&path))
                };
                if let Ok(action) = result {
                    report.add_removed(action, bytes);
                }
                CleanOutcome {
                    path: raw,
                    category: Some(category),
                    action: Some(*result.as_ref().unwrap_or(&Action::Trash)),
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

/// A dead system-wide launchd job, removed with administrator privileges.
#[derive(Debug, Clone)]
struct JobRemoval {
    label: String,
    path: PathBuf,
    domain: JobDomain,
}

/// What an uninstallation removes with administrator privileges.
#[derive(Debug, Clone)]
enum AdminRemoval {
    /// A dead system-wide launchd job.
    Job(JobRemoval),
    /// An app the user can't move, installed as root (Microsoft Office's installer, for one).
    App(PathBuf),
}

impl AdminRemoval {
    fn path(&self) -> &Path {
        match self {
            Self::Job(job) => &job.path,
            Self::App(path) => path,
        }
    }
}

/// Removes all the items with a single authorization dialog; one result per item.
type AdminRemover<'a> = &'a dyn Fn(&[AdminRemoval]) -> Vec<Result<Action, String>>;

/// The orphan detection reports this system-wide job right now.
fn orphan_job<'a>(installed: &'a Installed, raw: &str) -> Option<&'a JobFile> {
    let path = plain_abs(raw).ok()?;
    let parent = canon_parent(&path).ok()?;
    installed.orphan_jobs.iter().find(|job| {
        job.admin && job.path.file_name() == path.file_name() && canon_parent(&job.path).is_ok_and(|p| p == parent)
    })
}

fn uninstall_in(roots: &Roots, req: &UninstallRequest, remove: Remover, admin: AdminRemover) -> CleanReport {
    let mut checked: Vec<Checked> = Vec::new();
    let mut admin_items: Vec<AdminRemoval> = Vec::new();
    let mut app_id = None;
    // Removed with `brew uninstall --cask`: the app of a cask, or the record of one whose app is gone.
    let mut casks: HashMap<PathBuf, String> = HashMap::new();
    if let Some(raw) = &req.app_path {
        let v = validate_app(roots, raw).map(|(path, info)| {
            app_id = info.bundle_id;
            let cask = (roots.cask_of)(&path);
            // Finder asks for the password too: moving a folder needs write access to it and its parent.
            let movable = fsutil::writable(&path) && path.parent().is_some_and(fsutil::writable);
            if let Some(token) = cask {
                casks.insert(path.clone(), token);
            } else if !movable {
                admin_items.push(AdminRemoval::App(path.clone()));
            }
            (path, "app".to_string())
        });
        checked.push((raw.clone(), v));
    }
    // System-wide jobs are only removed as orphans, never as part of an app's uninstallation.
    let outside_home = req.leftover_paths.iter().any(|raw| !Path::new(raw).starts_with(&roots.home));
    let installed = (req.app_path.is_none() && outside_home).then(|| Installed::scan(roots));
    let deleted_casks = if req.app_path.is_none() && outside_home { (roots.deleted_casks)() } else { Vec::new() };
    for raw in &req.leftover_paths {
        if let Some(cask) = deleted_casks.iter().find(|c| c.caskroom.as_os_str() == raw.as_str()) {
            casks.insert(cask.caskroom.clone(), cask.token.clone());
            checked.push((raw.clone(), Ok((cask.caskroom.clone(), LeftoverKind::HomebrewCask.as_str().to_string()))));
            continue;
        }
        let v = match installed.as_ref().and_then(|i| orphan_job(i, raw)) {
            Some(job) => {
                admin_items.push(AdminRemoval::Job(JobRemoval { label: job.label.clone(), path: job.path.clone(), domain: job.domain }));
                Ok((job.path.clone(), job.kind.as_str().to_string()))
            }
            None => validate_leftover(roots, raw, app_id.as_deref()).map(|(p, k)| (p, k.as_str().to_string())),
        };
        checked.push((raw.clone(), v));
    }
    let admin_results: HashMap<PathBuf, Result<Action, String>> = if req.dry_run || admin_items.is_empty() {
        HashMap::new()
    } else {
        admin_items.iter().map(|item| item.path().to_path_buf()).zip(admin(&admin_items)).collect()
    };
    let remover = |p: &Path| match casks.get(p) {
        // Homebrew deletes it; it doesn't go to the Trash.
        Some(token) => (roots.uninstall_cask)(token).map(|()| Action::Delete),
        None => admin_results.get(p).cloned().unwrap_or_else(|| remove(p)),
    };
    // If the app cannot be removed, its data is kept.
    execute(checked, req.dry_run, req.app_path.is_some(), &remover)
}

/// Free name for `path` in the Trash; `mv -n` never overwrites anyway.
fn trash_destination(trash: &Path, path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let plain = trash.join(&name);
    if fs::symlink_metadata(&plain).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        return plain;
    }
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    trash.join(format!("{name} {secs}"))
}

/// Shell command that boots the job out of launchd and moves its file to the user's Trash
/// (`dest`), or deletes it when there is no destination.
fn job_removal_script(job: &JobRemoval, dest: Option<&Path>, uid: u32) -> String {
    let target = match job.domain {
        JobDomain::System => format!("system/{}", job.label),
        JobDomain::User => format!("gui/{uid}/{}", job.label),
    };
    format!("/bin/launchctl bootout {} 2>/dev/null; {}", sh_quote(&target), removal_command(&job.path, dest, uid))
}

/// Moves `path` to the user's Trash (`dest`), handing it over to the user, or deletes it.
fn removal_command(path: &Path, dest: Option<&Path>, uid: u32) -> String {
    let src = sh_quote(&path.to_string_lossy());
    match dest {
        Some(dest) => {
            let dest = sh_quote(&dest.to_string_lossy());
            format!("/bin/mv -n {src} {dest} && [ ! -e {src} ] && /usr/sbin/chown -R {uid} {dest}")
        }
        None => format!("/bin/rm -rf {src}"),
    }
}

fn remove_as_admin(home: &Path, items: &[AdminRemoval], action: Action) -> Vec<Result<Action, String>> {
    let uid = unsafe { libc::getuid() };
    let trash = home.join(".Trash");
    let scripts = items.iter().map(|item| {
        let dest = (action == Action::Trash).then(|| trash_destination(&trash, item.path()));
        match item {
            AdminRemoval::Job(job) => job_removal_script(job, dest.as_deref(), uid),
            AdminRemoval::App(path) => removal_command(path, dest.as_deref(), uid),
        }
    });
    run_admin_scripts(scripts, action)
}

/// System audio drivers and camera plugins. Core Audio is restarted so that a removed driver's
/// devices disappear right away; camera plugins go away when the apps using them restart.
fn remove_plugins_as_admin(home: &Path, plugins: &[(PathBuf, ExtensionKind)], action: Action) -> Vec<Result<Action, String>> {
    let uid = unsafe { libc::getuid() };
    let trash = home.join(".Trash");
    let scripts = plugins.iter().map(|(path, kind)| {
        let dest = (action == Action::Trash).then(|| trash_destination(&trash, path));
        let removal = removal_command(path, dest.as_deref(), uid);
        match kind {
            ExtensionKind::AudioDriver => format!("{removal} && {{ /usr/bin/killall coreaudiod 2>/dev/null; true; }}"),
            _ => removal,
        }
    });
    run_admin_scripts(scripts, action)
}

/// Runs the scripts with a single authorization dialog; one result per script.
fn run_admin_scripts(scripts: impl Iterator<Item = String>, action: Action) -> Vec<Result<Action, String>> {
    let tasks: Vec<(String, Vec<Vec<String>>)> = scripts
        .enumerate()
        .map(|(i, script)| (format!("item{i}"), vec![vec!["/bin/sh".to_string(), "-c".to_string(), script]]))
        .collect();
    performance::run_admin_tasks(&tasks)
        .into_iter()
        .map(|r| if r.ok { Ok(action) } else { Err(r.error.unwrap_or(r.output)) })
        .collect()
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
    ("Audio/Plug-Ins/HAL", Some("driver"), ExtensionKind::AudioDriver),
    ("CoreMediaIO/Plug-Ins/DAL", Some("plugin"), ExtensionKind::CameraPlugin),
];

/// Device drivers loaded by system services: system-wide ones are removed as administrator, and
/// those whose app is gone (and that no installer package placed) are reported as orphans.
fn is_device_plugin(kind: ExtensionKind) -> bool {
    matches!(kind, ExtensionKind::AudioDriver | ExtensionKind::CameraPlugin)
}

/// Display name of a bundle, unless it is just its id.
fn bundle_name(path: &Path) -> Option<String> {
    info_plist(path)
        .and_then(|d| dict_str(&d, "CFBundleDisplayName").or_else(|| dict_str(&d, "CFBundleName")))
        .filter(|n| !is_reverse_dns(n))
}

fn app_name(app: &Path) -> Option<String> {
    app.file_stem().map(|s| s.to_string_lossy().into_owned())
}

fn ext_matches(path: &Path, ext: Option<&str>) -> bool {
    match ext {
        None => true,
        Some(ext) => path.extension().is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case(ext)),
    }
}

fn list_extensions_in(roots: &Roots) -> Vec<ExtensionItem> {
    let ids = app_ids(roots);
    let vendors: HashSet<String> =
        ids.iter().filter(|i| is_reverse_dns(i)).filter_map(|i| vendor(i).map(str::to_owned)).collect();
    let mut items = Vec::new();
    for (library, scope) in [(roots.user_library(), ExtensionScope::User), (roots.library.clone(), ExtensionScope::System)] {
        for (dir, ext, kind) in EXTENSION_DIRS {
            for (name, path) in entries(&library.join(dir)) {
                if !ext_matches(&path, *ext) {
                    continue;
                }
                let mut orphan = false;
                if is_device_plugin(*kind) {
                    let id = info_plist(&path).and_then(|d| dict_str(&d, "CFBundleIdentifier")).unwrap_or_default();
                    // macOS's own drivers are part of the system.
                    if id.to_lowercase().starts_with("com.apple.") {
                        continue;
                    }
                    orphan = is_orphan(&id, &ids, &vendors) && !(roots.packaged)(&path);
                }
                let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(name);
                items.push(ExtensionItem {
                    name,
                    path: path.to_string_lossy().into_owned(),
                    kind: *kind,
                    scope,
                    bytes: 0,
                    removable: scope == ExtensionScope::User || is_device_plugin(*kind),
                    origin: None,
                    orphan,
                });
            }
        }
    }
    // Only macOS deactivates system extensions (Login Items & Extensions in System Settings).
    let mut app_only = HashSet::new();
    for (app, _) in app_bundles(roots) {
        bundle_ids(&app, &mut app_only);
    }
    for ext in system_extensions(roots, &app_only) {
        let origin = ext.origin.as_deref().and_then(app_name);
        items.push(ExtensionItem {
            name: bundle_name(&ext.path).or_else(|| origin.clone()).unwrap_or_else(|| ext.id.clone()),
            path: ext.path.to_string_lossy().into_owned(),
            kind: ext.kind,
            scope: ExtensionScope::System,
            bytes: 0,
            removable: false,
            origin,
            orphan: ext.orphan,
        });
    }
    let paths: Vec<PathBuf> = items.iter().map(|i| PathBuf::from(&i.path)).collect();
    for (item, u) in items.iter_mut().zip(fsutil::usages(&paths)) {
        item.bytes = u.bytes;
    }
    items.sort_by_cached_key(|i| (i.name.to_lowercase(), i.path.clone()));
    items
}

/// Deletable extension: direct child of a user extensions folder, or of a system device driver
/// folder (removed as administrator: the flag), with its file extension.
fn validate_extension(roots: &Roots, raw: &str) -> Result<(PathBuf, ExtensionKind, bool), String> {
    let path = plain_abs(raw)?;
    let parent = canon_parent(&path)?;
    let in_dir = |library: &Path, d: &str| fs::canonicalize(library.join(d)).is_ok_and(|c| c == parent);
    let user = roots.user_library();
    let (ext, kind, admin) = EXTENSION_DIRS
        .iter()
        .find_map(|(d, ext, kind)| {
            if in_dir(&user, d) {
                Some((*ext, *kind, false))
            } else if is_device_plugin(*kind) && in_dir(&roots.library, d) {
                Some((*ext, *kind, true))
            } else {
                None
            }
        })
        .ok_or_else(|| "outside the extension folders".to_string())?;
    if is_hidden(&path) || !ext_matches(&path, ext) {
        return Err("not an extension of this kind".into());
    }
    let id = info_plist(&path).and_then(|d| dict_str(&d, "CFBundleIdentifier")).unwrap_or_default();
    if admin && id.to_lowercase().starts_with("com.apple.") {
        return Err("protected system item".into());
    }
    fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    Ok((path, kind, admin))
}

/// Removes system device drivers with a single authorization dialog; one result per driver.
type PluginRemover<'a> = &'a dyn Fn(&[(PathBuf, ExtensionKind)]) -> Vec<Result<Action, String>>;

fn remove_extensions_in(roots: &Roots, req: &CleanRequest, remove: Remover, admin: PluginRemover) -> CleanReport {
    let mut plugins = Vec::new();
    let checked = req
        .paths
        .iter()
        .map(|raw| {
            let v = validate_extension(roots, raw).map(|(p, kind, as_admin)| {
                if as_admin {
                    plugins.push((p.clone(), kind));
                }
                (p, kind.as_str().to_string())
            });
            (raw.clone(), v)
        })
        .collect();
    let admin_results: HashMap<PathBuf, Result<Action, String>> = if req.dry_run || plugins.is_empty() {
        HashMap::new()
    } else {
        plugins.iter().map(|(p, _)| p.clone()).zip(admin(&plugins)).collect()
    };
    let remover = |p: &Path| admin_results.get(p).cloned().unwrap_or_else(|| remove(p));
    execute(checked, req.dry_run, false, &remover)
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
    let roots = Roots::real()?;
    let action = trash::preferred_action();
    Ok(uninstall_in(&roots, req, &|p| trash::discard(p, action), &|items| remove_as_admin(&roots.home, items, action)))
}

pub fn list_extensions() -> Result<Vec<ExtensionItem>, String> {
    Roots::real().map(|r| list_extensions_in(&r))
}

pub fn remove_extensions(req: &CleanRequest) -> Result<CleanReport, String> {
    let roots = Roots::real()?;
    let action = trash::preferred_action();
    Ok(remove_extensions_in(&roots, req, &|p| trash::discard(p, action), &|plugins| {
        remove_plugins_as_admin(&roots.home, plugins, action)
    }))
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
            packaged: |p| p.to_string_lossy().contains("Packaged"),
            cask_of: |_| None,
            uninstall_cask: |_| panic!("unexpected brew call"),
            deleted_casks: Vec::new,
        };
        for d in [&r.home, &r.applications, &r.system_applications, &r.library] {
            fs::create_dir_all(d).unwrap();
        }
        r
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    fn no_admin(_: &[AdminRemoval]) -> Vec<Result<Action, String>> {
        panic!("unexpected administrator removal")
    }

    fn no_plugins(_: &[(PathBuf, ExtensionKind)]) -> Vec<Result<Action, String>> {
        panic!("unexpected administrator removal")
    }

    fn fs_remove(p: &Path) -> Result<Action, String> {
        let md = fs::symlink_metadata(p).map_err(|e| e.to_string())?;
        if md.is_dir() { fs::remove_dir_all(p) } else { fs::remove_file(p) }.map_err(|e| e.to_string())?;
        Ok(Action::Trash)
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
        // The team-prefixed cache is named after its id once the team id is stripped.
        assert_eq!(ids, vec!["net.big.thing", "com.gone.app", "com.x.y"]);
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
            let rep = uninstall_in(&r, &req, &fs_remove, &no_admin);
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
        let rep = uninstall_in(&r, &req, &fs_remove, &no_admin);
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
        let rep = uninstall_in(&r, &req, &fs_remove, &no_admin);
        assert_eq!(rep.failed, 0, "{:?}", rep.outcomes);
        assert!(rep.dry_run && rep.trashed_bytes >= 29_000);
        assert_eq!(rep.outcomes[0].category.as_deref(), Some("app"));
        assert_eq!(rep.outcomes[1].category.as_deref(), Some("cache"));
        assert!(app.exists());

        let rep = uninstall_in(&r, &UninstallRequest { dry_run: false, ..req }, &fs_remove, &no_admin);
        assert_eq!(rep.failed, 0, "{:?}", rep.outcomes);
        assert!(!app.exists());
        assert!(!lib.join("Caches/com.foo.app").exists());
        assert!(!lib.join("Preferences/com.foo.app.plist").exists());
        assert!(lib.join("Caches").exists());
    }

    #[test]
    fn apps_the_user_cannot_move_are_removed_as_admin() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new("apps-root-owned");
        let r = roots(&tmp);
        let lib = r.user_library();
        let app = make_app(&r.applications, "Office", &[("CFBundleIdentifier", "com.office.app")], false, b"");
        touch(&lib.join("Caches/com.office.app/x"), 10);
        // Like an app installed as root: the user can't rename it out of the folder.
        fs::set_permissions(&app, fs::Permissions::from_mode(0o555)).unwrap();
        let req = UninstallRequest {
            app_path: Some(s(&app)),
            leftover_paths: vec![s(&lib.join("Caches/com.office.app"))],
            dry_run: false,
        };

        let calls = std::cell::RefCell::new(Vec::new());
        let admin = |items: &[AdminRemoval]| {
            calls.borrow_mut().extend(items.iter().map(|item| item.path().to_path_buf()));
            vec![Err("Cancelled".to_string())]
        };
        let rep = uninstall_in(&r, &req, &fs_remove, &admin);
        assert_eq!(*calls.borrow(), vec![app.clone()]);
        assert_eq!(rep.outcomes[0].error.as_deref(), Some("Cancelled"));
        // Cancelled: the app's data is kept.
        assert_eq!(rep.outcomes[1].error.as_deref(), Some("the app was not uninstalled"));
        assert!(lib.join("Caches/com.office.app").exists());

        let rep = uninstall_in(&r, &req, &fs_remove, &|items| items.iter().map(|_| Ok(Action::Trash)).collect());
        assert_eq!(rep.failed, 0, "{:?}", rep.outcomes);
        assert!(!lib.join("Caches/com.office.app").exists());
        fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn homebrew_casks_are_uninstalled_with_brew() {
        let tmp = TempDir::new("apps-cask");
        let mut r = roots(&tmp);
        let lib = r.user_library();
        let app = make_app(&r.applications, "Viewer", &[("CFBundleIdentifier", "com.viewer.app")], false, b"");
        touch(&lib.join("Caches/com.viewer.app/x"), 10);
        r.cask_of = |p| p.ends_with("Viewer.app").then(|| "viewer".to_string());
        r.uninstall_cask = |token| if token == "viewer" { Err("brew ran".into()) } else { Ok(()) };
        let req = UninstallRequest {
            app_path: Some(s(&app)),
            leftover_paths: vec![s(&lib.join("Caches/com.viewer.app"))],
            dry_run: false,
        };
        let rep = uninstall_in(&r, &req, &fs_remove, &no_admin);
        // brew removes the app, never the Trash; when it fails, the data is kept.
        assert_eq!(rep.outcomes[0].error.as_deref(), Some("brew ran"));
        assert!(app.exists() && lib.join("Caches/com.viewer.app").exists());

        r.uninstall_cask = |_| Ok(());
        let rep = uninstall_in(&r, &req, &fs_remove, &no_admin);
        assert_eq!(rep.failed, 0, "{:?}", rep.outcomes);
        assert_eq!(rep.outcomes[0].action, Some(Action::Delete));
        assert!(!lib.join("Caches/com.viewer.app").exists());
    }

    #[test]
    fn deleted_casks_join_their_app_leftovers() {
        let tmp = TempDir::new("apps-deleted-cask");
        let mut r = roots(&tmp);
        let lib = r.user_library();
        touch(&lib.join("Preferences/com.viewer.app.plist"), 10);
        let caskroom = tmp.path().join("homebrew/Caskroom/viewer");
        touch(&caskroom.join("2.1/x"), 10);
        // Function pointers can't capture the temporary folder: it goes through the environment.
        r.deleted_casks = || {
            let dir = std::env::var_os("MCL_TEST_CASKROOM").map(PathBuf::from).unwrap();
            vec![brew::DeletedCask { token: "viewer".into(), caskroom: dir, names: vec!["TERM".into(), "com.viewer.app.plist".into()] }]
        };
        unsafe { std::env::set_var("MCL_TEST_CASKROOM", &caskroom) };

        let groups = orphans_in(&r);
        let group = groups.iter().find(|g| g.id == "com.viewer.app").expect("group");
        let kinds: Vec<LeftoverKind> = group.items.iter().map(|i| i.kind).collect();
        assert!(kinds.contains(&LeftoverKind::Preferences) && kinds.contains(&LeftoverKind::HomebrewCask), "{groups:?}");

        r.uninstall_cask = |token| if token == "viewer" { Ok(()) } else { Err("wrong cask".into()) };
        let req = UninstallRequest { app_path: None, leftover_paths: vec![s(&caskroom), s(&lib.join("Preferences/com.viewer.app.plist"))], dry_run: false };
        let rep = uninstall_in(&r, &req, &fs_remove, &no_admin);
        assert_eq!(rep.failed, 0, "{:?}", rep.outcomes);
        assert_eq!(rep.outcomes[0].category.as_deref(), Some("homebrew_cask"));
        assert_eq!(rep.outcomes[0].action, Some(Action::Delete));
        // brew removed the record (stubbed here): MacCleaner never deletes it itself.
        assert!(caskroom.exists());
        assert!(!lib.join("Preferences/com.viewer.app.plist").exists());
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
        let rep = uninstall_in(&r, &req, &|_| Err("in use".into()), &no_admin);
        assert_eq!(rep.failed, 2);
        assert_eq!(rep.outcomes[1].error.as_deref(), Some("the app was not uninstalled"));

        let req = UninstallRequest { app_path: Some(s(&r.home)), ..req };
        let rep = uninstall_in(&r, &req, &fs_remove, &no_admin);
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
        let rep = remove_extensions_in(&r, &req, &fs_remove, &no_plugins);
        assert_eq!(rep.failed, 4, "{:?}", rep.outcomes);
        assert_eq!(rep.outcomes[4].category.as_deref(), Some("preference_pane"));
        assert!(!user_pane.exists() && !user_ql.exists());
        assert!(sys_md.exists() && lib.join("QuickLook/notes.txt").exists());
    }

    /// Registers a staged system extension in `db.plist`, activated from `origin`.
    fn stage_extension(r: &Roots, exts: &[(&str, &str, &str, &Path)]) {
        let dir = r.library.join("SystemExtensions");
        let mut list = Vec::new();
        for (uuid, id, category, origin) in exts {
            let bundle = dir.join(uuid).join(format!("{id}.systemextension"));
            fs::create_dir_all(bundle.join("Contents")).unwrap();
            let mut info = plist::Dictionary::new();
            info.insert("CFBundleDisplayName".into(), "Camera Extension".into());
            info.insert("CFBundleName".into(), (*id).into());
            plist::Value::Dictionary(info).to_file_xml(bundle.join("Contents/Info.plist")).unwrap();
            let mut d = plist::Dictionary::new();
            d.insert("identifier".into(), (*id).into());
            d.insert("uniqueID".into(), (*uuid).into());
            d.insert("state".into(), "activated_enabled".into());
            d.insert("categories".into(), plist::Value::Array(vec![(*category).into()]));
            let from = origin.join(format!("Contents/Library/SystemExtensions/{id}.systemextension"));
            d.insert("originPath".into(), s(&from).into());
            list.push(plist::Value::Dictionary(d));
        }
        let mut db = plist::Dictionary::new();
        db.insert("extensions".into(), plist::Value::Array(list));
        plist::Value::Dictionary(db).to_file_binary(dir.join("db.plist")).unwrap();
    }

    fn make_plugin(dir: &Path, file: &str, id: &str) -> PathBuf {
        let p = dir.join(file);
        fs::create_dir_all(p.join("Contents")).unwrap();
        let mut d = plist::Dictionary::new();
        d.insert("CFBundleIdentifier".into(), id.into());
        plist::Value::Dictionary(d).to_file_xml(p.join("Contents/Info.plist")).unwrap();
        p
    }

    #[test]
    fn extensions_and_drivers_left_by_removed_apps_are_orphans() {
        let tmp = TempDir::new("apps-sysext");
        let r = roots(&tmp);
        let kept = make_app(&r.applications, "Mi", &[("CFBundleIdentifier", "com.mi.connect")], false, b"");
        let gone = r.applications.join("Virtual Cam.app");
        stage_extension(&r, &[
            ("5DA9", "com.cam.maker.avextension", "com.apple.system_extension.cmio", &gone),
            ("35C1", "com.mi.camera", "com.apple.system_extension.cmio", &kept),
            ("0B0E", "cn.wch.serial", "com.apple.system_extension.driver_extension", &r.applications.join("Serial.app")),
        ]);
        let hal = r.library.join("Audio/Plug-Ins/HAL");
        let cam_audio = make_plugin(&hal, "CamAudio.driver", "com.cam.maker.AudioDriver");
        make_plugin(&hal, "Packaged.driver", "com.blackhole.driver");
        make_plugin(&hal, "Parrot.driver", "com.apple.audio.Parrot");
        make_plugin(&r.library.join("CoreMediaIO/Plug-Ins/DAL"), "Old.plugin", "com.mi.dal");
        touch(&r.library.join("CoreMediaIO/Plug-Ins/DAL/plugins-info.txt"), 1);
        touch(&r.user_library().join("Application Support/com.cam.maker.avextension/x"), 10);

        let items = list_extensions_in(&r);
        let summary: Vec<(&str, ExtensionKind, bool, bool, Option<&str>)> =
            items.iter().map(|i| (i.name.as_str(), i.kind, i.removable, i.orphan, i.origin.as_deref())).collect();
        assert_eq!(
            summary,
            vec![
                ("CamAudio", ExtensionKind::AudioDriver, true, true, None),
                ("Camera Extension", ExtensionKind::DriverExtension, false, true, Some("Serial")),
                ("Camera Extension", ExtensionKind::CameraExtension, false, false, Some("Mi")),
                ("Camera Extension", ExtensionKind::CameraExtension, false, true, Some("Virtual Cam")),
                ("Old", ExtensionKind::CameraPlugin, true, false, None),
                ("Packaged", ExtensionKind::AudioDriver, true, false, None),
            ]
        );
        // With its extension orphaned, the vendor's data is an orphan too.
        let ids: Vec<String> = orphans_in(&r).into_iter().map(|g| g.id).collect();
        assert_eq!(ids, vec!["com.cam.maker.avextension"]);

        let removed = std::cell::RefCell::new(Vec::new());
        let admin = |plugins: &[(PathBuf, ExtensionKind)]| {
            removed.borrow_mut().extend(plugins.iter().cloned());
            plugins.iter().map(|_| Ok(Action::Trash)).collect()
        };
        let staged = items.iter().find(|i| i.kind == ExtensionKind::DriverExtension).unwrap().path.clone();
        let req = CleanRequest {
            paths: vec![s(&cam_audio), s(&hal.join("Parrot.driver")), staged],
            dry_run: false,
        };
        let rep = remove_extensions_in(&r, &req, &fs_remove, &admin);
        assert_eq!(rep.failed, 2, "{:?}", rep.outcomes);
        assert!(rep.outcomes[0].ok);
        assert_eq!(*removed.borrow(), vec![(cam_audio, ExtensionKind::AudioDriver)]);
    }

    #[test]
    fn removal_command_quotes_the_path() {
        let script = removal_command(Path::new("/L/A b.driver"), None, 501);
        assert_eq!(script, "/bin/rm -rf '/L/A b.driver'");
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

    fn write_job(p: &Path, label: &str, program: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut d = plist::Dictionary::new();
        d.insert("Label".into(), plist::Value::String(label.into()));
        d.insert("Program".into(), plist::Value::String(program.to_string_lossy().into_owned()));
        plist::Value::Dictionary(d).to_file_xml(p).unwrap();
    }

    fn group<'a>(groups: &'a [OrphanGroup], id: &str) -> &'a OrphanGroup {
        groups.iter().find(|g| g.id == id).unwrap_or_else(|| panic!("no group {id}: {groups:?}"))
    }

    fn names_in(g: &OrphanGroup) -> Vec<String> {
        let mut v: Vec<String> = g.items.iter().map(|i| Path::new(&i.path).file_name().unwrap().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn entry_ids_drop_team_and_group_prefixes() {
        assert_eq!(entry_id("S8EX82NJP6.com.macpaw.CleanMyMac-setapp"), "com.macpaw.CleanMyMac-setapp");
        assert_eq!(entry_id("group.com.foo.app"), "com.foo.app");
        assert_eq!(entry_id("ABCDE12345.com.foo.app.plist"), "com.foo.app");
        assert_eq!(entry_id("UBF8T346G9.group.com.microsoft.shared"), "com.microsoft.shared");
        assert_eq!(entry_id("243LU875E5.groups.com.apple.podcasts"), "com.apple.podcasts");
        // Too short to be an app id once the group prefix is gone.
        assert_eq!(entry_id("group.com.docker"), "com.docker");
        assert!(!is_reverse_dns(entry_id("group.tvappservices.container")));
        // Not a team id (no digit, or nothing after it).
        assert_eq!(entry_id("HEALTHCARE.com.foo.app"), "HEALTHCARE.com.foo.app");
        assert_eq!(entry_id("UBF8T346G9.Office"), "UBF8T346G9.Office");
        assert_eq!(entry_id("com.foo.app.savedState"), "com.foo.app");
    }

    #[test]
    fn dead_launchd_jobs_are_leftovers_and_unblock_their_vendor() {
        let tmp = TempDir::new("apps-jobs");
        let r = roots(&tmp);
        let lib = r.user_library();
        let missing = tmp.path().join("gone/Gone.app/Contents/MacOS/agent");
        // A privileged helper whose daemon plist was deleted: it would otherwise hide the vendor.
        touch(&r.library.join("PrivilegedHelperTools/com.gone.app.Agent"), 50);
        // A daemon with a missing program, and a user agent with a missing program.
        write_job(&r.library.join("LaunchDaemons/com.gone.app.Updater.plist"), "com.gone.app.Updater", &missing);
        write_job(&lib.join("LaunchAgents/com.gone.app.Menu.plist"), "com.gone.app.Menu", &missing);
        // A live helper launched by its daemon plist keeps its vendor installed.
        let live = r.library.join("PrivilegedHelperTools/com.live.vendor.Helper");
        touch(&live, 50);
        write_job(&r.library.join("LaunchDaemons/com.live.vendor.Helper.plist"), "com.live.vendor.Helper", &live);
        for p in [
            "HTTPStorages/com.gone.app",
            "HTTPStorages/com.gone.app.Menu.binarycookies",
            "Group Containers/ABCDE12345.com.gone.app/x",
            "Application Scripts/com.gone.app.FinderExt",
            "Application Scripts/ABCDE12345.com.gone.app",
            "Caches/com.live.vendor.App/x",
        ] {
            touch(&lib.join(p), 10);
        }

        let groups = orphans_in(&r);
        assert_eq!(groups.len(), 1, "{groups:?}");
        let g = group(&groups, "com.gone.app");
        assert_eq!(
            names_in(g),
            vec![
                "ABCDE12345.com.gone.app",
                "ABCDE12345.com.gone.app",
                "com.gone.app",
                "com.gone.app.Agent",
                "com.gone.app.FinderExt",
                "com.gone.app.Menu.binarycookies",
                "com.gone.app.Menu.plist",
                "com.gone.app.Updater.plist",
            ]
        );
        let admin: Vec<&str> = g.items.iter().filter(|i| i.requires_admin).map(|i| i.kind.as_str()).collect();
        assert_eq!(admin.len(), 2);
        assert!(admin.contains(&"privileged_helper") && admin.contains(&"launch_daemon"));
        let menu = g.items.iter().find(|i| i.path.ends_with("com.gone.app.Menu.plist")).unwrap();
        assert!(!menu.requires_admin && menu.kind == LeftoverKind::LaunchAgent);
    }

    #[test]
    fn shared_groups_shortcuts_and_system_extensions_are_not_orphans() {
        let tmp = TempDir::new("apps-shared");
        let r = roots(&tmp);
        let lib = r.user_library();
        // A driver extension outlives the app that installed it and keeps its vendor installed.
        fs::create_dir_all(r.library.join("SystemExtensions/0B0E3BBE/com.drv.maker.dext")).unwrap();
        for p in [
            "Group Containers/group.com.docker/x",
            "Application Scripts/group.tvappservices.container",
            "Application Scripts/group.is.workflow.my.app",
            "Group Containers/243LU875E5.groups.com.apple.podcasts/x",
            "Application Scripts/com.drv.maker",
            "Group Containers/ABCDE12345.com.drv.maker.shared/x",
            "Application Scripts/com.gone.app",
        ] {
            touch(&lib.join(p), 10);
        }
        let ids: Vec<String> = orphans_in(&r).into_iter().map(|g| g.id).collect();
        assert_eq!(ids, vec!["com.gone.app"]);
    }

    #[test]
    fn app_groups_of_installed_apps_are_kept() {
        let tmp = TempDir::new("apps-groups");
        let r = roots(&tmp);
        let lib = r.user_library();
        let exe = crate::codesign::tests::entitled_to(&["group.com.facebook.family", "ABCDE12345.com.chat.shared"]);
        make_app(&r.applications, "Chat", &[("CFBundleIdentifier", "net.chat.app")], false, &exe);
        for p in [
            "Group Containers/group.com.facebook.family/x",
            "Application Scripts/group.com.facebook.family",
            "Group Containers/ABCDE12345.com.chat.shared/x",
            "Group Containers/group.com.gone.family/x",
        ] {
            touch(&lib.join(p), 10);
        }
        let ids: Vec<String> = orphans_in(&r).into_iter().map(|g| g.id).collect();
        assert_eq!(ids, vec!["com.gone.family"]);

        // Uninstalling another app never takes a group the entitled app still uses.
        let fb = make_app(&r.applications, "Facebook", &[("CFBundleIdentifier", "com.facebook.family")], false, b"");
        let items = leftovers_in(&r, &fb).unwrap().items;
        assert!(items.iter().all(|i| !i.path.contains("group.com.facebook.family")), "{items:?}");
        // The entitled app's own uninstallation does take it.
        fs::remove_dir_all(&fb).unwrap();
        let exe = crate::codesign::tests::entitled_to(&["group.net.chat.app"]);
        let chat2 = make_app(&r.applications, "Chat2", &[("CFBundleIdentifier", "net.chat.app2")], false, &exe);
        touch(&lib.join("Group Containers/group.net.chat.app2/x"), 10);
        let items = leftovers_in(&r, &chat2).unwrap().items;
        assert!(items.iter().any(|i| i.path.ends_with("group.net.chat.app2")), "{items:?}");
    }

    #[test]
    fn folders_named_after_an_orphaned_app_join_its_group() {
        let tmp = TempDir::new("apps-named");
        let r = roots(&tmp);
        let lib = r.user_library();
        make_app(&r.applications, "Keeper", &[("CFBundleIdentifier", "com.other.keeper")], false, b"");
        for p in [
            "HTTPStorages/com.vendor.GoneApp-setapp",
            "Containers/com.vendor.keepers/x",
            "Containers/com.tiny.gizmo/x",
            "Application Support/GoneApp_5_Setapp/x",
            "Application Support/GoneApp X/x",
            "Caches/goneapp/x",
            "Logs/GoneApps/x",
            "Application Support/Keeper Data/x",
            "Application Support/Gizmo/x",
        ] {
            touch(&lib.join(p), 10);
        }
        let groups = orphans_in(&r);
        assert_eq!(
            names_in(group(&groups, "com.vendor.GoneApp-setapp")),
            vec!["GoneApp X", "GoneApp_5_Setapp", "com.vendor.GoneApp-setapp", "goneapp"]
        );
        // `keepers` is also the name of an installed app; `gizmo` is too short to match by name.
        assert_eq!(names_in(group(&groups, "com.vendor.keepers")), vec!["com.vendor.keepers"]);
        assert_eq!(names_in(group(&groups, "com.tiny.gizmo")), vec!["com.tiny.gizmo"]);
    }

    #[test]
    fn orphaned_system_jobs_are_removed_with_one_admin_call() {
        let tmp = TempDir::new("apps-admin");
        let r = roots(&tmp);
        let lib = r.user_library();
        let helper = r.library.join("PrivilegedHelperTools/com.gone.app.Agent");
        touch(&helper, 50);
        let live = r.library.join("PrivilegedHelperTools/com.live.vendor.Helper");
        touch(&live, 50);
        write_job(&r.library.join("LaunchDaemons/com.live.vendor.Helper.plist"), "com.live.vendor.Helper", &live);
        let cache = lib.join("Caches/com.gone.app");
        touch(&cache.join("x"), 10);
        let req = UninstallRequest {
            app_path: None,
            leftover_paths: vec![s(&helper), s(&live), s(&cache)],
            dry_run: true,
        };

        // Dry run: nothing runs, but the helper validates and the live one does not.
        let rep = uninstall_in(&r, &req, &fs_remove, &no_admin);
        let ok: Vec<bool> = rep.outcomes.iter().map(|o| o.ok).collect();
        assert_eq!(ok, vec![true, false, true]);
        assert_eq!(rep.outcomes[0].category.as_deref(), Some("privileged_helper"));

        let calls = std::cell::RefCell::new(Vec::new());
        let admin = |items: &[AdminRemoval]| {
            let jobs = items.iter().map(|item| match item {
                AdminRemoval::Job(j) => (j.label.clone(), j.domain),
                AdminRemoval::App(_) => panic!("unexpected app"),
            });
            calls.borrow_mut().push(jobs.collect::<Vec<_>>());
            vec![Err("Cancelled".to_string())]
        };
        let rep = uninstall_in(&r, &UninstallRequest { dry_run: false, ..req.clone() }, &fs_remove, &admin);
        assert_eq!(*calls.borrow(), vec![vec![("com.gone.app.Agent".to_string(), JobDomain::System)]]);
        assert_eq!(rep.outcomes[0].error.as_deref(), Some("Cancelled"));
        assert!(!rep.outcomes[1].ok && rep.outcomes[2].ok);
        assert!(helper.exists() && live.exists() && !cache.exists());

        // Never as part of an app's uninstallation.
        let app = make_app(&r.applications, "Other", &[("CFBundleIdentifier", "com.other.app")], false, b"");
        let rep = uninstall_in(
            &r,
            &UninstallRequest { app_path: Some(s(&app)), leftover_paths: vec![s(&helper)], dry_run: true },
            &fs_remove,
            &no_admin,
        );
        assert!(rep.outcomes[0].ok && !rep.outcomes[1].ok);
    }

    #[test]
    fn job_removal_boots_out_and_moves_to_trash() {
        let job = JobRemoval { label: "com.gone.app.Agent".into(), path: "/Library/PrivilegedHelperTools/com.gone.app.Agent".into(), domain: JobDomain::System };
        let script = job_removal_script(&job, Some(Path::new("/Users/me/.Trash/com.gone.app.Agent")), 501);
        assert_eq!(
            script,
            "/bin/launchctl bootout system/com.gone.app.Agent 2>/dev/null; \
             /bin/mv -n /Library/PrivilegedHelperTools/com.gone.app.Agent /Users/me/.Trash/com.gone.app.Agent \
             && [ ! -e /Library/PrivilegedHelperTools/com.gone.app.Agent ] && /usr/sbin/chown -R 501 /Users/me/.Trash/com.gone.app.Agent"
        );
        let agent = JobRemoval { label: "a b".into(), path: "/Library/LaunchAgents/a b.plist".into(), domain: JobDomain::User };
        let script = job_removal_script(&agent, Some(Path::new("/t/a b.plist")), 501);
        assert!(script.starts_with("/bin/launchctl bootout 'gui/501/a b' 2>/dev/null; /bin/mv -n '/Library/LaunchAgents/a b.plist' '/t/a b.plist'"));
        assert_eq!(
            job_removal_script(&agent, None, 501),
            "/bin/launchctl bootout 'gui/501/a b' 2>/dev/null; /bin/rm -rf '/Library/LaunchAgents/a b.plist'"
        );

        let tmp = TempDir::new("apps-trash-dest");
        assert_eq!(trash_destination(tmp.path(), Path::new("/x/new")), tmp.path().join("new"));
        touch(&tmp.path().join("taken"), 1);
        let dest = trash_destination(tmp.path(), Path::new("/x/taken"));
        assert!(dest.file_name().unwrap().to_string_lossy().starts_with("taken "));
    }
}
