//! Development leftovers: regenerable project artifacts (node_modules, target, Pods…),
//! toolchain caches and old versions, and command-based cleanups (Homebrew, Docker).
//!
//! A directory only counts as an artifact if the file that generates it sits next to it
//! (`node_modules` with `package.json`, `target` with `Cargo.toml`…); that way an arbitrary
//! `build` or `target` folder is never mistaken for an artifact.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::clean::{CleanOutcome, CleanReport};
use crate::rules::home_dir;
use crate::{fsutil, governor, progress, trash};

// MARK: Project artifacts

struct ArtifactRule {
    name: &'static str,
    ecosystem: &'static str,
    /// It is enough for one of these files to exist next to the directory.
    markers: &'static [&'static str],
    /// Alternative: some file with this extension next to the directory.
    marker_ext: Option<&'static str>,
    /// File that must exist inside the directory (e.g. `pyvenv.cfg` in a virtual environment).
    inner: Option<&'static str>,
}

const fn rule(name: &'static str, ecosystem: &'static str, markers: &'static [&'static str]) -> ArtifactRule {
    ArtifactRule { name, ecosystem, markers, marker_ext: None, inner: None }
}

const PY_MARKERS: &[&str] = &["pyproject.toml", "requirements.txt", "setup.py", "Pipfile", "uv.lock", "poetry.lock"];

const ARTIFACTS: &[ArtifactRule] = &[
    rule("node_modules", "Node.js", &["package.json"]),
    rule(".next", "Next.js", &["next.config.js", "next.config.mjs", "next.config.ts", "package.json"]),
    rule(".nuxt", "Nuxt", &["nuxt.config.js", "nuxt.config.ts", "package.json"]),
    rule(".output", "Nuxt", &["nuxt.config.js", "nuxt.config.ts"]),
    rule(".svelte-kit", "SvelteKit", &["svelte.config.js", "svelte.config.ts"]),
    rule(".turbo", "Turborepo", &["turbo.json", "package.json"]),
    rule(".parcel-cache", "Parcel", &["package.json"]),
    rule(".angular", "Angular", &["angular.json"]),
    rule(".expo", "Expo", &["app.json", "app.config.js", "app.config.ts"]),
    rule("target", "Rust", &["Cargo.toml"]),
    rule("target", "Maven", &["pom.xml"]),
    rule(".gradle", "Gradle", &["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts", "gradlew"]),
    rule("build", "Gradle", &["build.gradle", "build.gradle.kts"]),
    rule("build", "Flutter", &["pubspec.yaml"]),
    rule(".dart_tool", "Dart/Flutter", &["pubspec.yaml"]),
    rule("Pods", "CocoaPods", &["Podfile"]),
    rule(".build", "SwiftPM", &["Package.swift"]),
    ArtifactRule { name: ".venv", ecosystem: "Python", markers: PY_MARKERS, marker_ext: None, inner: Some("pyvenv.cfg") },
    ArtifactRule { name: "venv", ecosystem: "Python", markers: PY_MARKERS, marker_ext: None, inner: Some("pyvenv.cfg") },
    rule("_build", "Elixir", &["mix.exs"]),
    rule("deps", "Elixir", &["mix.exs"]),
    rule(".zig-cache", "Zig", &["build.zig"]),
    rule("zig-cache", "Zig", &["build.zig"]),
    rule("zig-out", "Zig", &["build.zig"]),
    rule(".stack-work", "Haskell", &["stack.yaml"]),
    rule("elm-stuff", "Elm", &["elm.json"]),
    ArtifactRule { name: ".terraform", ecosystem: "Terraform", markers: &[], marker_ext: Some("tf"), inner: None },
    rule("cmake-build-debug", "CMake", &["CMakeLists.txt"]),
    rule("cmake-build-release", "CMake", &["CMakeLists.txt"]),
];

/// Top-level `$HOME` folders that do not contain projects.
const SKIP_TOP: &[&str] = &["Library", "Applications", "Music", "Movies", "Pictures", "Public"];
use crate::access;
const MAX_DEPTH: u32 = 8;
/// Inactivity after which artifacts are selected by default.
pub const INACTIVE_DAYS: u32 = 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub path: String,
    pub kind: String,
    pub ecosystem: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevProject {
    pub path: String,
    pub name: String,
    pub ecosystems: Vec<String>,
    /// Most recent date among the project's files (top level and `.git/index`).
    pub last_activity: u64,
    pub inactive_days: u32,
    pub git: bool,
    pub artifacts: Vec<Artifact>,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectsReport {
    pub projects: Vec<DevProject>,
    pub total_bytes: u64,
    pub scanned_dirs: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectsRequest {
    pub roots: Option<Vec<String>>,
    /// Force Desktop, Documents and Downloads even without Full Disk Access
    /// (macOS will show a permission dialog and the read will wait for the answer).
    pub include_protected: bool,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn mtime(md: &fs::Metadata) -> u64 {
    md.mtime().max(0) as u64
}

/// Rule that applies to `dir/name` given the files present in `dir`.
fn matching_rule(dir: &Path, name: &str, files: &HashSet<OsString>) -> Option<&'static ArtifactRule> {
    ARTIFACTS.iter().find(|r| {
        r.name == name
            && (r.markers.iter().any(|m| files.contains(OsStr::new(m)))
                || r.marker_ext.is_some_and(|ext| {
                    files.iter().any(|f| Path::new(f).extension().is_some_and(|e| e == ext))
                }))
            && r.inner.is_none_or(|inner| dir.join(name).join(inner).is_file())
    })
}

struct ProjectWalker {
    found: Vec<(PathBuf, Vec<(PathBuf, &'static ArtifactRule)>)>,
    scanned: u64,
    include_protected: bool,
    /// Tool caches (`~/go/pkg/mod`...): the projects inside are downloaded dependencies,
    /// cleaned as a whole from the tools list.
    skip: HashSet<PathBuf>,
}

impl ProjectWalker {
    fn walk(&mut self, dir: &Path, depth: u32) {
        self.scanned += 1;
        if self.scanned % 256 == 0 {
            governor::poll();
        }
        if governor::aborted() || depth > MAX_DEPTH {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else { return };
        let mut files: HashSet<OsString> = HashSet::new();
        let mut dirs: Vec<OsString> = Vec::new();
        for e in entries.flatten() {
            // `file_type` uses d_type: no stat and no symlink following.
            match e.file_type() {
                Ok(t) if t.is_dir() => dirs.push(e.file_name()),
                Ok(t) if t.is_file() => {
                    files.insert(e.file_name());
                }
                _ => {}
            }
        }
        let mut artifacts = Vec::new();
        let mut descend = Vec::new();
        for name in dirs {
            let Some(s) = name.to_str() else { continue };
            if let Some(rule) = matching_rule(dir, s, &files) {
                artifacts.push((dir.join(&name), rule));
            } else if !s.starts_with('.')
                && !s.ends_with(".app")
                && !self.skip.contains(&dir.join(&name))
                && !(depth == 0 && SKIP_TOP.contains(&s))
                && !(depth == 0 && !self.include_protected && access::PROTECTED_HOME_DIRS.contains(&s))
            {
                descend.push(name);
            }
        }
        // Artifacts that can't be moved out of a read-only folder are not offered.
        if !artifacts.is_empty() && fsutil::writable(dir) {
            self.found.push((dir.to_path_buf(), artifacts));
        }
        for name in descend {
            self.walk(&dir.join(name), depth + 1);
        }
    }
}

fn last_activity(dir: &Path) -> u64 {
    let mut latest = 0;
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            if let Ok(md) = e.metadata() {
                if md.is_file() {
                    latest = latest.max(mtime(&md));
                }
            }
        }
    }
    for git in [".git/index", ".git/HEAD"] {
        if let Ok(md) = fs::metadata(dir.join(git)) {
            latest = latest.max(mtime(&md));
        }
    }
    latest
}

pub fn scan_projects_in(home: &Path, req: &ProjectsRequest) -> ProjectsReport {
    let started = Instant::now();
    let include_protected = req.include_protected || access::full_disk_access();
    let skip = CACHES.iter().flat_map(|c| c.paths).map(|p| home.join(p)).collect();
    let mut walker = ProjectWalker { found: Vec::new(), scanned: 0, include_protected, skip };
    match &req.roots {
        Some(roots) => {
            for r in roots {
                walker.walk(Path::new(r), 1);
            }
        }
        None => walker.walk(home, 0),
    }

    let paths: Vec<PathBuf> = walker.found.iter().flat_map(|(_, a)| a.iter().map(|(p, _)| p.clone())).collect();
    let mut sizes = fsutil::usages(&paths).into_iter();
    let now = now_secs();
    let mut projects: Vec<DevProject> = walker
        .found
        .into_iter()
        .map(|(dir, found)| {
            let artifacts: Vec<Artifact> = found
                .into_iter()
                .map(|(p, r)| Artifact {
                    path: p.to_string_lossy().into_owned(),
                    kind: r.name.into(),
                    ecosystem: r.ecosystem.into(),
                    bytes: sizes.next().map(|u| u.bytes).unwrap_or(0),
                })
                .filter(|a| a.bytes > 0)
                .collect();
            let mut ecosystems: Vec<String> = artifacts.iter().map(|a| a.ecosystem.clone()).collect();
            ecosystems.sort();
            ecosystems.dedup();
            let last = last_activity(&dir);
            DevProject {
                name: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                path: dir.to_string_lossy().into_owned(),
                ecosystems,
                last_activity: last,
                inactive_days: (now.saturating_sub(last) / 86_400) as u32,
                git: dir.join(".git").exists(),
                bytes: artifacts.iter().map(|a| a.bytes).sum(),
                artifacts,
            }
        })
        .filter(|p| p.bytes > 0)
        .collect();
    projects.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes));

    ProjectsReport {
        total_bytes: projects.iter().map(|p| p.bytes).sum(),
        projects,
        scanned_dirs: walker.scanned,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

pub fn scan_projects(req: &ProjectsRequest) -> Result<ProjectsReport, String> {
    Ok(scan_projects_in(&home_dir().ok_or("HOME is not set")?, req))
}

/// Checks that `path` is currently a recognizable artifact inside `home`.
fn validate_artifact(home: &Path, path: &Path) -> Result<(), String> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(OsStr::to_str)) else {
        return Err("invalid path".into());
    };
    if !parent.starts_with(home) || parent == home {
        return Err("outside the home folder".into());
    }
    if !fsutil::writable(parent) {
        return Err("its folder is read-only".into());
    }
    let files: HashSet<OsString> = fs::read_dir(parent)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.file_name())
        .collect();
    matching_rule(parent, name, &files).map(|_| ()).ok_or_else(|| "not a recognizable project artifact".into())
}

// MARK: Tool caches

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolAction {
    Trash,
    Command,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCache {
    pub id: String,
    pub name: String,
    pub tool: String,
    pub paths: Vec<String>,
    pub bytes: u64,
    pub default_selected: bool,
    pub note: Option<String>,
    pub action: ToolAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolchainReport {
    pub items: Vec<ToolCache>,
    pub total_bytes: u64,
    pub elapsed_ms: u64,
}

struct CacheSpec {
    id: &'static str,
    name: &'static str,
    tool: &'static str,
    paths: &'static [&'static str],
    default_selected: bool,
    note: Option<&'static str>,
}

const CACHES: &[CacheSpec] = &[
    CacheSpec { id: "npm_cache", name: "npm cache", tool: "npm", paths: &[".npm/_cacache"], default_selected: true, note: None },
    CacheSpec { id: "npm_logs", name: "npm logs", tool: "npm", paths: &[".npm/_logs"], default_selected: true, note: None },
    CacheSpec { id: "bun_cache", name: "Bun cache", tool: "Bun", paths: &[".bun/install/cache"], default_selected: true, note: None },
    CacheSpec { id: "yarn_cache", name: "Yarn cache", tool: "Yarn", paths: &[".yarn/berry/cache"], default_selected: true, note: None },
    CacheSpec {
        id: "pnpm_store",
        name: "pnpm store",
        tool: "pnpm",
        paths: &["Library/pnpm/store", ".local/share/pnpm/store", ".pnpm-store"],
        default_selected: false,
        note: Some("pnpm's node_modules link to this store; `pnpm store prune` is more selective."),
    },
    CacheSpec {
        id: "cargo_cache",
        name: "Cargo registry",
        tool: "Rust",
        paths: &[".cargo/registry/cache", ".cargo/registry/src", ".cargo/git/checkouts"],
        default_selected: true,
        note: None,
    },
    CacheSpec {
        id: "go_mod_cache",
        name: "Go modules",
        tool: "Go",
        paths: &["go/pkg/mod"],
        default_selected: false,
        note: Some("They are downloaded again on build; this may take a while without a connection."),
    },
    CacheSpec {
        id: "gradle_cache",
        name: "Gradle caches and distributions",
        tool: "Gradle",
        paths: &[".gradle/caches", ".gradle/wrapper/dists"],
        default_selected: true,
        note: None,
    },
    CacheSpec {
        id: "maven_repo",
        name: "Maven local repository",
        tool: "Maven",
        paths: &[".m2/repository"],
        default_selected: false,
        note: Some("May contain artifacts installed manually with `mvn install`."),
    },
    CacheSpec { id: "cocoapods_repos", name: "CocoaPods spec repos", tool: "CocoaPods", paths: &[".cocoapods/repos"], default_selected: false, note: None },
    CacheSpec {
        id: "android_avd",
        name: "Android emulators",
        tool: "Android",
        paths: &[".android/avd"],
        default_selected: false,
        note: Some("Deletes the virtual devices and their data."),
    },
];

/// Known caches inside `~/.cache` that regenerate themselves.
const XDG_SAFE: &[&str] =
    &["uv", "pip", "node", "node-gyp", "rubocop_cache", "pre-commit", "typescript", "prisma", "yarn", "pnpm", "go-build", "ruff", "mypy", "black", "puppeteer", "electron", "electron-builder"];
/// Tools that keep their own runtime in `~/.cache`: not listed.
const XDG_NEVER: &[&str] = &["claude", "codex-runtimes", "opencode", "gh"];

fn xdg_items(home: &Path) -> Vec<ToolCache> {
    let Ok(entries) = fs::read_dir(home.join(".cache")) else { return Vec::new() };
    let mut items: Vec<ToolCache> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok().map(|n| (n, e.path())))
        .filter(|(n, _)| !XDG_NEVER.contains(&n.as_str()))
        .map(|(n, p)| {
            let safe = XDG_SAFE.contains(&n.as_str());
            ToolCache {
                id: format!("xdg:{n}"),
                name: format!("~/.cache/{n}"),
                tool: "Tool cache".into(),
                paths: vec![p.to_string_lossy().into_owned()],
                bytes: 0,
                default_selected: safe,
                note: (!safe).then(|| {
                    if n == "huggingface" {
                        "Downloaded models: downloading them again can take a long time.".to_string()
                    } else {
                        "Unidentified cache: review it before deleting it.".to_string()
                    }
                }),
                action: ToolAction::Trash,
            }
        })
        .collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    items
}

/// Numeric comparison of versions like `v22.21.1`.
fn version_key(v: &str) -> Vec<u64> {
    v.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// Node version nvm uses by default (resolves aliases like `22` or `node`).
fn nvm_default(home: &Path, installed: &[String]) -> Option<String> {
    let alias = fs::read_to_string(home.join(".nvm/alias/default")).ok()?.trim().to_string();
    let best = |prefix: &str| {
        installed
            .iter()
            .filter(|v| prefix.is_empty() || v.trim_start_matches('v') == prefix || v.trim_start_matches('v').starts_with(&format!("{prefix}.")))
            .max_by_key(|v| version_key(v))
            .cloned()
    };
    match alias.as_str() {
        "node" | "stable" => best(""),
        a if a.starts_with("lts") => None,
        a => best(a.trim_start_matches('v')),
    }
}

fn old_versions(home: &Path) -> Vec<ToolCache> {
    let mut items = Vec::new();
    // nvm: every version except the default. Without a clear default nothing is proposed.
    let node_dir = home.join(".nvm/versions/node");
    if let Ok(entries) = fs::read_dir(&node_dir) {
        let installed: Vec<String> = entries.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.starts_with('v')).collect();
        if let Some(default) = nvm_default(home, &installed) {
            for v in installed.iter().filter(|v| **v != default) {
                items.push(ToolCache {
                    id: format!("nvm:{v}"),
                    name: format!("Node.js {v}"),
                    tool: "nvm".into(),
                    paths: vec![node_dir.join(v).to_string_lossy().into_owned()],
                    bytes: 0,
                    default_selected: false,
                    note: Some(format!("The default version is {default}. Make sure no project uses this one.")),
                    action: ToolAction::Trash,
                });
            }
        }
    }
    // rustup: toolchains other than the default.
    let settings = fs::read_to_string(home.join(".rustup/settings.toml")).unwrap_or_default();
    let default = settings
        .lines()
        .find_map(|l| l.trim().strip_prefix("default_toolchain").map(|v| v.trim_start_matches([' ', '=']).trim().trim_matches('"').to_string()));
    let tc_dir = home.join(".rustup/toolchains");
    if let (Some(default), Ok(entries)) = (default, fs::read_dir(&tc_dir)) {
        for name in entries.flatten().filter_map(|e| e.file_name().into_string().ok()) {
            if name != default && !name.starts_with('.') {
                items.push(ToolCache {
                    id: format!("rustup:{name}"),
                    name: format!("Rust {name}"),
                    tool: "rustup".into(),
                    paths: vec![tc_dir.join(&name).to_string_lossy().into_owned()],
                    bytes: 0,
                    default_selected: false,
                    note: Some(format!("The default toolchain is {default}.")),
                    action: ToolAction::Trash,
                });
            }
        }
    }
    items
}

/// Deletable items (without sizes): used both for the report and to validate cleaning.
fn trash_items(home: &Path) -> Vec<ToolCache> {
    let mut items: Vec<ToolCache> = CACHES
        .iter()
        .filter_map(|c| {
            let paths: Vec<String> =
                c.paths.iter().map(|p| home.join(p)).filter(|p| p.is_dir()).map(|p| p.to_string_lossy().into_owned()).collect();
            (!paths.is_empty()).then(|| ToolCache {
                id: c.id.into(),
                name: c.name.into(),
                tool: c.tool.into(),
                paths,
                bytes: 0,
                default_selected: c.default_selected,
                note: c.note.map(Into::into),
                action: ToolAction::Trash,
            })
        })
        .collect();
    items.extend(xdg_items(home));
    items.extend(old_versions(home));
    items
}

// MARK: Commands (Homebrew, Docker)

const BREW_PATHS: &[&str] = &["/opt/homebrew/bin/brew", "/usr/local/bin/brew"];
const DOCKER_PATHS: &[&str] = &["/usr/local/bin/docker", "/opt/homebrew/bin/docker", "/Applications/Docker.app/Contents/Resources/bin/docker"];
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const ESTIMATE_TIMEOUT: Duration = Duration::from_secs(20);

fn first_existing(paths: &[&str]) -> Option<PathBuf> {
    paths.iter().map(PathBuf::from).find(|p| p.is_file())
}

/// Runs with a time limit; returns (success, stdout+stderr).
fn run(program: &Path, args: &[&str], timeout: Duration) -> Result<(bool, String), String> {
    let mut child = Command::new(program)
        .args(args)
        .env("HOMEBREW_NO_AUTO_UPDATE", "1")
        .env("HOMEBREW_NO_ENV_HINTS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err("timed out".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut out = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut out);
    }
    Ok((status.success(), out))
}

/// "1.2GB", "512.3 MB", "0B", "830kB" → bytes (decimal, as brew and docker print them).
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mult = match unit.trim().to_ascii_uppercase().as_str() {
        "B" => 1.0,
        "KB" | "K" => 1e3,
        "MB" | "M" => 1e6,
        "GB" | "G" => 1e9,
        "TB" | "T" => 1e12,
        _ => return None,
    };
    Some((n * mult) as u64)
}

fn brew_item() -> Option<ToolCache> {
    let brew = first_existing(BREW_PATHS)?;
    let (_, out) = run(&brew, &["cleanup", "-n"], ESTIMATE_TIMEOUT).ok()?;
    // "This operation would free approximately 1.2GB of disk space."
    let bytes = out.split("approximately").nth(1).and_then(|r| r.split_whitespace().next()).and_then(parse_size).unwrap_or(0);
    Some(ToolCache {
        id: "homebrew_cleanup".into(),
        name: "Homebrew: old versions and downloads".into(),
        tool: "Homebrew".into(),
        paths: Vec::new(),
        bytes,
        default_selected: false,
        note: Some("Runs `brew cleanup`.".into()),
        action: ToolAction::Command,
    })
}

fn docker_item() -> Option<ToolCache> {
    let docker = first_existing(DOCKER_PATHS)?;
    // Fails within seconds if the daemon is not running: then it is not offered.
    let (ok, out) = run(&docker, &["system", "df", "--format", "{{.Type}}\t{{.Reclaimable}}"], ESTIMATE_TIMEOUT).ok()?;
    if !ok {
        return None;
    }
    let bytes: u64 = out
        .lines()
        .filter(|l| l.starts_with("Images") || l.starts_with("Build Cache"))
        .filter_map(|l| l.split('\t').nth(1)?.split_whitespace().next().and_then(parse_size))
        .sum();
    Some(ToolCache {
        id: "docker_prune".into(),
        name: "Docker: unused images and build cache".into(),
        tool: "Docker".into(),
        paths: Vec::new(),
        bytes,
        default_selected: false,
        note: Some("Runs `docker image prune -a -f` and `docker builder prune -f`. Does not touch containers or volumes.".into()),
        action: ToolAction::Command,
    })
}

pub fn scan_toolchains_in(home: &Path) -> ToolchainReport {
    let started = Instant::now();
    let mut items = trash_items(home);
    let paths: Vec<PathBuf> = items.iter().flat_map(|i| i.paths.iter().map(PathBuf::from)).collect();
    let mut sizes = fsutil::usages(&paths).into_iter();
    for item in &mut items {
        item.bytes = item.paths.iter().map(|_| sizes.next().map(|u| u.bytes).unwrap_or(0)).sum();
    }
    items.retain(|i| i.bytes > 0);
    // Commands are only offered if they would free something.
    items.extend(brew_item().into_iter().chain(docker_item()).filter(|i| i.bytes > 0));
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    ToolchainReport {
        total_bytes: items.iter().map(|i| i.bytes).sum(),
        items,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

pub fn scan_toolchains() -> Result<ToolchainReport, String> {
    Ok(scan_toolchains_in(&home_dir().ok_or("HOME is not set")?))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub id: String,
    pub ok: bool,
    pub output: String,
    pub error: Option<String>,
}

/// Runs command-based cleanups. Known ids only; arguments are fixed.
pub fn run_commands(ids: &[String]) -> Vec<CommandResult> {
    let _progress = progress::start(ids.len(), false);
    ids.iter()
        .map(|id| {
            let result = progress::step(Path::new(id), 0, || match id.as_str() {
                "homebrew_cleanup" => first_existing(BREW_PATHS)
                    .ok_or_else(|| "Homebrew is not installed".to_string())
                    .and_then(|brew| run(&brew, &["cleanup"], COMMAND_TIMEOUT)),
                "docker_prune" => first_existing(DOCKER_PATHS).ok_or_else(|| "Docker is not installed".to_string()).and_then(|d| {
                    let (ok1, out1) = run(&d, &["image", "prune", "-a", "-f"], COMMAND_TIMEOUT)?;
                    let (ok2, out2) = run(&d, &["builder", "prune", "-f"], COMMAND_TIMEOUT)?;
                    Ok((ok1 && ok2, format!("{out1}\n{out2}")))
                }),
                _ => Err("unknown task".to_string()),
            });
            match result {
                Ok((ok, output)) => CommandResult {
                    id: id.clone(),
                    ok,
                    error: (!ok).then(|| output.lines().last().unwrap_or("failed").to_string()),
                    output: output.chars().rev().take(4000).collect::<Vec<_>>().into_iter().rev().collect(),
                },
                Err(e) => CommandResult { id: id.clone(), ok: false, output: String::new(), error: Some(e) },
            }
        })
        .collect()
}

// MARK: Cleaning

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevCleanRequest {
    pub paths: Vec<String>,
    #[serde(default = "yes")]
    pub dry_run: bool,
}

fn yes() -> bool {
    true
}

/// Moves project artifacts or known caches to the Trash, revalidating each path.
pub fn clean_in(home: &Path, req: &DevCleanRequest) -> CleanReport {
    let allowed_caches: HashSet<String> = trash_items(home).into_iter().flat_map(|i| i.paths).collect();
    let checked: Vec<(String, Result<(PathBuf, &'static str), String>)> = req
        .paths
        .iter()
        .map(|raw| {
            let path = PathBuf::from(raw);
            let check = (|| {
                if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
                    return Err("invalid path".to_string());
                }
                let md = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
                if !md.is_dir() {
                    return Err("not a directory".into());
                }
                if allowed_caches.contains(raw) {
                    return Ok((path.clone(), "dev_toolchain"));
                }
                validate_artifact(home, &path).map(|_| (path.clone(), "dev_artifact"))
            })();
            (raw.clone(), check)
        })
        .collect();

    let valid: Vec<PathBuf> = checked.iter().filter_map(|(_, c)| c.as_ref().ok().map(|(p, _)| p.clone())).collect();
    let _progress = progress::start(valid.len(), req.dry_run);
    let mut sizes = fsutil::usages(&valid).into_iter();
    let mut report = CleanReport::new(req.dry_run);
    let action = trash::preferred_action();
    for (raw, check) in checked {
        let outcome = match check {
            Err(error) => CleanOutcome { path: raw, category: None, action: None, bytes: 0, ok: false, error: Some(error) },
            Ok((path, category)) => {
                let bytes = sizes.next().map(|u| u.bytes).unwrap_or(0);
                let result = if req.dry_run { Ok(action) } else { progress::step(&path, bytes, || trash::discard(&path, action)) };
                if result.is_ok() {
                    report.add_removed(action, bytes);
                }
                CleanOutcome {
                    path: raw,
                    category: Some(category.into()),
                    action: Some(action),
                    bytes,
                    ok: result.is_ok(),
                    error: result.err(),
                }
            }
        };
        if !outcome.ok {
            report.failed += 1;
        }
        report.outcomes.push(outcome);
    }
    report
}

pub fn clean(req: &DevCleanRequest) -> Result<CleanReport, String> {
    Ok(clean_in(&home_dir().ok_or("HOME is not set")?, req))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::os::unix::fs::PermissionsExt;

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    fn fill(p: &Path) {
        fs::create_dir_all(p).unwrap();
        fs::write(p.join("blob"), vec![0u8; 10_000]).unwrap();
    }

    #[test]
    fn detects_artifacts_only_next_to_their_marker() {
        let tmp = TempDir::new("dev-projects");
        let h = tmp.path();
        // Node project with valid artifacts.
        touch(&h.join("Projects/web/package.json"));
        fill(&h.join("Projects/web/node_modules/dep"));
        fill(&h.join("Projects/web/.next"));
        // node_modules nested inside another artifact: not walked or counted separately.
        touch(&h.join("Projects/web/node_modules/dep/package.json"));
        fill(&h.join("Projects/web/node_modules/dep/node_modules/x"));
        // Rust, and a `target` folder without Cargo.toml (not an artifact).
        touch(&h.join("Projects/cli/Cargo.toml"));
        fill(&h.join("Projects/cli/target/debug"));
        fill(&h.join("Docs/target"));
        // venv without pyvenv.cfg: doesn't count; with pyvenv.cfg: does.
        touch(&h.join("Projects/py/pyproject.toml"));
        fill(&h.join("Projects/py/venv"));
        fill(&h.join("Projects/py2/.venv"));
        touch(&h.join("Projects/py2/.venv/pyvenv.cfg"));
        touch(&h.join("Projects/py2/requirements.txt"));
        // Dependencies downloaded into a tool cache, and a read-only folder (Go's module cache
        // is read-only): neither is offered.
        touch(&h.join("go/pkg/mod/github.com/x/y@v1/plugin/build.gradle"));
        fill(&h.join("go/pkg/mod/github.com/x/y@v1/plugin/build"));
        touch(&h.join("Projects/locked/Cargo.toml"));
        fill(&h.join("Projects/locked/target"));
        let locked = h.join("Projects/locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        // Library is not walked; Desktop only if requested.
        touch(&h.join("Library/x/package.json"));
        fill(&h.join("Library/x/node_modules"));
        touch(&h.join("Desktop/demo/package.json"));
        fill(&h.join("Desktop/demo/node_modules"));

        let r = scan_projects_in(h, &ProjectsRequest::default());
        let mut found: Vec<String> = r
            .projects
            .iter()
            .flat_map(|p| p.artifacts.iter().map(|a| a.path.strip_prefix(&*h.to_string_lossy()).unwrap().to_string()))
            // Desktop depends on whether this process has Full Disk Access; checked below.
            .filter(|p| !p.starts_with("/Desktop"))
            .collect();
        found.sort();
        assert_eq!(found, ["/Projects/cli/target", "/Projects/py2/.venv", "/Projects/web/.next", "/Projects/web/node_modules"]);
        let rep = clean_in(h, &DevCleanRequest { paths: vec![locked.join("target").to_string_lossy().into_owned()], dry_run: true });
        assert_eq!(rep.outcomes[0].error.as_deref(), Some("its folder is read-only"));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let with_protected = scan_projects_in(h, &ProjectsRequest { include_protected: true, ..Default::default() });
        assert!(with_protected.projects.iter().any(|p| p.name == "demo"));
        let web = r.projects.iter().find(|p| p.name == "web").unwrap();
        assert_eq!(web.ecosystems, ["Next.js", "Node.js"]);
        assert_eq!(web.inactive_days, 0);
    }

    #[test]
    fn clean_only_accepts_recognised_artifacts_and_caches() {
        let tmp = TempDir::new("dev-clean");
        let h = tmp.path();
        touch(&h.join("p/Cargo.toml"));
        fill(&h.join("p/target"));
        fill(&h.join("p/src"));
        fill(&h.join("other/target"));
        fill(&h.join(".npm/_cacache"));
        let s = |p: &str| h.join(p).to_string_lossy().into_owned();
        let req = DevCleanRequest {
            paths: vec![s("p/target"), s(".npm/_cacache"), s("p/src"), s("other/target"), s("p/target/../src"), "/usr/lib".into()],
            dry_run: true,
        };
        let r = clean_in(h, &req);
        let ok: Vec<bool> = r.outcomes.iter().map(|o| o.ok).collect();
        assert_eq!(ok, [true, true, false, false, false, false]);
        assert!(h.join("p/target").exists(), "dry_run must not touch anything");
    }

    #[test]
    fn nvm_keeps_default_and_resolves_partial_alias() {
        let tmp = TempDir::new("dev-nvm");
        let h = tmp.path();
        for v in ["v18.20.8", "v22.1.0", "v22.21.1"] {
            fill(&h.join(".nvm/versions/node").join(v));
        }
        touch(&h.join(".nvm/alias/default"));
        fs::write(h.join(".nvm/alias/default"), "22\n").unwrap();
        let mut ids: Vec<String> = old_versions(h).into_iter().map(|i| i.id).collect();
        ids.sort();
        assert_eq!(ids, ["nvm:v18.20.8", "nvm:v22.1.0"]);
        fs::write(h.join(".nvm/alias/default"), "lts/*\n").unwrap();
        assert!(old_versions(h).is_empty(), "without a clear default nothing is proposed");
    }

    #[test]
    fn xdg_skips_tool_runtimes_and_flags_unknown() {
        let tmp = TempDir::new("dev-xdg");
        let h = tmp.path();
        for n in ["uv", "claude", "huggingface", "mystery"] {
            fill(&h.join(".cache").join(n));
        }
        let items = xdg_items(h);
        let get = |id: &str| items.iter().find(|i| i.id == id);
        assert!(get("xdg:claude").is_none());
        assert!(get("xdg:uv").unwrap().default_selected);
        assert!(!get("xdg:huggingface").unwrap().default_selected);
        assert!(!get("xdg:mystery").unwrap().default_selected);
    }

    #[test]
    fn parses_sizes_from_brew_and_docker() {
        assert_eq!(parse_size("1.2GB"), Some(1_200_000_000));
        assert_eq!(parse_size("830kB"), Some(830_000));
        assert_eq!(parse_size("0B"), Some(0));
        assert_eq!(parse_size("12 MB"), Some(12_000_000));
        assert_eq!(parse_size("abc"), None);
    }

    #[test]
    fn unknown_command_is_rejected() {
        let r = run_commands(&["rm -rf /".into()]);
        assert!(!r[0].ok);
    }
}
