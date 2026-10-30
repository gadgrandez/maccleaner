//! Homebrew packages: what is installed (formulae and casks) and its removal.
//!
//! Everything goes through `brew` itself, never by deleting its files: Homebrew keeps track of
//! what is installed and refuses to remove a package that another one needs.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::command::{first_existing, run_split};
use crate::fsutil;

const BREW_PATHS: &[&str] = &["/opt/homebrew/bin/brew", "/usr/local/bin/brew"];
const LIST_TIMEOUT: Duration = Duration::from_secs(60);
const UNINSTALL_TIMEOUT: Duration = Duration::from_secs(300);

/// The `brew` program, if Homebrew is installed.
pub fn locate() -> Option<PathBuf> {
    first_existing(BREW_PATHS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrewKind {
    /// A command-line tool or library.
    Formula,
    /// An app or a binary distribution.
    Cask,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrewPackage {
    /// Formula name or cask token: what `brew uninstall` takes.
    pub name: String,
    pub kind: BrewKind,
    /// The cask's display name; the formula's name otherwise.
    pub title: String,
    pub desc: Option<String>,
    pub version: String,
    pub bytes: u64,
    /// Installed by the user, not as a dependency of another package.
    pub requested: bool,
    /// Installed packages that need it: Homebrew won't remove it while they are there.
    pub dependents: Vec<String>,
    /// It defines a background service (`brew services`).
    pub service: bool,
    /// Apps the cask installed that are still there.
    pub apps: Vec<String>,
    /// The cask's apps were deleted without Homebrew (dragged to the Trash): it still lists them.
    pub apps_missing: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrewReport {
    /// Formulae the user installed and casks.
    pub packages: Vec<BrewPackage>,
    /// Dependencies nothing needs anymore (`brew autoremove`).
    pub unused: Vec<BrewPackage>,
}

/// Homebrew's prefix (`/opt/homebrew`) from the location of `brew`.
fn prefix(brew: &Path) -> PathBuf {
    brew.parent().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default()
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

fn strings(v: Option<&Value>) -> impl Iterator<Item = &str> {
    v.and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str)
}

/// Where the cask's apps are: the recorded target, or the Applications folder.
fn cask_apps(cask: &Value) -> Vec<String> {
    cask["artifacts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|artifact| {
            let app = artifact.get("app")?.as_array()?.first()?.as_str()?;
            Some(str_of(artifact, "target").unwrap_or_else(|| {
                format!("/Applications/{}", Path::new(app).file_name().unwrap_or_default().to_string_lossy())
            }))
        })
        .collect()
}

/// The packages to show from `brew info --json=v2 --installed` (without sizes), each with the
/// paths it takes up. `unused`: the dependencies `brew autoremove` would remove.
fn parse(json: &Value, prefix: &Path, unused: &HashSet<String>) -> Vec<(BrewPackage, Vec<PathBuf>)> {
    let formulae = json["formulae"].as_array().map(Vec::as_slice).unwrap_or_default();
    let casks = json["casks"].as_array().map(Vec::as_slice).unwrap_or_default();
    let mut dependents: HashMap<&str, Vec<String>> = HashMap::new();
    for f in formulae {
        let name = f["name"].as_str().unwrap_or_default();
        for dep in f["installed"].as_array().into_iter().flatten().flat_map(|i| i["runtime_dependencies"].as_array().into_iter().flatten()) {
            if let Some(dep) = dep["full_name"].as_str() {
                dependents.entry(dep).or_default().push(name.to_string());
            }
        }
    }
    for c in casks {
        let token = c["token"].as_str().unwrap_or_default();
        for dep in strings(c["depends_on"].get("formula")) {
            dependents.entry(dep).or_default().push(token.to_string());
        }
    }
    let mut out = Vec::new();
    for f in formulae {
        let Some(name) = str_of(f, "name") else { continue };
        let installed = f["installed"].as_array().and_then(|i| i.last());
        let requested = installed.is_some_and(|i| i["installed_on_request"].as_bool() == Some(true));
        if !requested && !unused.contains(&name) {
            continue;
        }
        let full = str_of(f, "full_name").unwrap_or_else(|| name.clone());
        let mut deps: Vec<String> = dependents.get(full.as_str()).cloned().unwrap_or_default();
        deps.sort();
        deps.dedup();
        out.push((
            BrewPackage {
                title: name.clone(),
                desc: str_of(f, "desc"),
                version: installed.and_then(|i| str_of(i, "version")).unwrap_or_default(),
                bytes: 0,
                requested,
                dependents: deps,
                service: !f["service"].is_null(),
                apps: Vec::new(),
                apps_missing: false,
                kind: BrewKind::Formula,
                name,
            },
            vec![prefix.join("Cellar").join(f["name"].as_str().unwrap_or_default())],
        ));
    }
    for c in casks {
        let Some(token) = str_of(c, "token") else { continue };
        let declared = cask_apps(c);
        let apps: Vec<String> = declared.iter().filter(|a| Path::new(a).exists()).cloned().collect();
        let caskroom = prefix.join("Caskroom").join(&token);
        let mut paths = vec![caskroom.clone()];
        for target in linked_targets(&caskroom).into_iter().chain(apps.iter().map(PathBuf::from)) {
            if !paths.contains(&target) {
                paths.push(target);
            }
        }
        out.push((
            BrewPackage {
                title: strings(c.get("name")).next().map(str::to_string).unwrap_or_else(|| token.clone()),
                desc: str_of(c, "desc"),
                version: str_of(c, "installed").or_else(|| str_of(c, "version")).unwrap_or_default(),
                bytes: 0,
                requested: true,
                dependents: Vec::new(),
                service: false,
                apps_missing: !declared.is_empty() && apps.is_empty(),
                apps,
                kind: BrewKind::Cask,
                name: token,
            },
            paths,
        ));
    }
    out
}

/// What the links in each installed version of a cask point to: Homebrew keeps the files of some
/// casks elsewhere (`share/google-cloud-sdk`, the app in Applications) and links them from there.
fn linked_targets(caskroom: &Path) -> Vec<PathBuf> {
    let entries = |dir: &Path| std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect::<Vec<_>>();
    entries(caskroom)
        .into_iter()
        .filter(|v| v.is_dir() && !v.is_symlink() && !v.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .flat_map(|version| entries(&version))
        .filter(|p| p.is_symlink())
        .filter_map(|link| std::fs::canonicalize(link).ok())
        .filter(|target| target.exists())
        .collect()
}

/// Names after the "Would autoremove…" line of `brew autoremove --dry-run`.
fn parse_autoremove(out: &str) -> HashSet<String> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("==>") && !l.contains(' '))
        .map(str::to_string)
        .collect()
}

/// The installed packages, with their sizes. Fails if Homebrew is not installed.
pub fn packages() -> Result<BrewReport, String> {
    let brew = locate().ok_or("Homebrew is not installed")?;
    let (ok, out, err) = run_split(&brew, &["info", "--json=v2", "--installed"], LIST_TIMEOUT)?;
    if !ok {
        return Err(brew_error(&err));
    }
    let json: Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    let unused = run_split(&brew, &["autoremove", "--dry-run"], LIST_TIMEOUT)
        .map(|(_, out, _)| parse_autoremove(&out))
        .unwrap_or_default();
    let parsed = parse(&json, &prefix(&brew), &unused);
    let all_paths: Vec<PathBuf> = parsed.iter().flat_map(|(_, p)| p.iter().cloned()).collect();
    let mut sizes = fsutil::usages(&all_paths).into_iter().map(|u| u.bytes);
    let mut report = BrewReport::default();
    for (mut package, paths) in parsed {
        package.bytes = paths.iter().map(|_| sizes.next().unwrap_or(0)).sum();
        if package.kind == BrewKind::Formula && unused.contains(&package.name) {
            report.unused.push(package);
        } else {
            report.packages.push(package);
        }
    }
    Ok(report)
}

/// Homebrew names: letters, digits and `@._+-`, with `/` for taps. Never an option.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || "@._+-/".contains(c))
}

/// Homebrew's own message ("Error: …"), or the last line it printed.
fn brew_error(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines
        .iter()
        .find_map(|l| l.strip_prefix("Error: "))
        .or(lines.last().copied())
        .unwrap_or("brew failed")
        .to_string()
}

fn run_brew(args: &[&str]) -> Result<(), String> {
    let brew = locate().ok_or("Homebrew is not installed")?;
    let (ok, _, err) = run_split(&brew, args, UNINSTALL_TIMEOUT)?;
    if ok { Ok(()) } else { Err(brew_error(&err)) }
}

/// `brew uninstall`. A formula's service is stopped first: Homebrew would leave it loaded.
/// Homebrew refuses if another installed package needs it.
pub fn uninstall(name: &str, kind: BrewKind) -> Result<(), String> {
    if !valid_name(name) {
        return Err("invalid package name".into());
    }
    match kind {
        BrewKind::Formula => {
            // Fails harmlessly if the service isn't running.
            let _ = run_brew(&["services", "stop", name]);
            run_brew(&["uninstall", "--formula", name])
        }
        BrewKind::Cask => run_brew(&["uninstall", "--cask", name]),
    }
}

/// `brew autoremove`: dependencies nothing needs anymore.
pub fn autoremove() -> Result<(), String> {
    run_brew(&["autoremove"])
}

/// The installed casks (`brew info`), with Homebrew's prefix. Adding `--cask` would leave out
/// casks whose app is gone, so everything installed is read.
fn installed_casks() -> Option<(Vec<Value>, PathBuf)> {
    let brew = locate()?;
    let (ok, out, _) = run_split(&brew, &["info", "--json=v2", "--installed"], LIST_TIMEOUT).ok()?;
    let mut json: Value = serde_json::from_str(&out).ok().filter(|_| ok)?;
    let casks = json["casks"].as_array_mut().map(std::mem::take)?;
    Some((casks, prefix(&brew)))
}

/// The cask that installed the app at `path`, if any.
pub fn cask_for_app(path: &Path) -> Option<String> {
    installed_casks()?.0.iter().find_map(|c| {
        cask_apps(c).iter().any(|a| Path::new(a) == path).then(|| str_of(c, "token")).flatten()
    })
}

/// A cask whose apps were deleted without Homebrew: only its record (the Caskroom) is left.
#[derive(Debug, Clone)]
pub struct DeletedCask {
    pub token: String,
    pub caskroom: PathBuf,
    /// Names its uninstall and zap stanzas mention (processes, files in `~/Library`): they give
    /// away the app's bundle id.
    pub names: Vec<String>,
}

/// Names in the cask's `uninstall` and `zap` stanzas: values, and the file name of paths.
fn stanza_names(cask: &Value) -> Vec<String> {
    fn collect(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => {
                let name = Path::new(s).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| s.clone());
                if !out.contains(&name) {
                    out.push(name);
                }
            }
            Value::Array(items) => items.iter().for_each(|i| collect(i, out)),
            Value::Object(map) => map.values().for_each(|i| collect(i, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for artifact in cask["artifacts"].as_array().into_iter().flatten() {
        for key in ["uninstall", "zap"] {
            if let Some(stanza) = artifact.get(key) {
                collect(stanza, &mut out);
            }
        }
    }
    out
}

fn deleted_in(casks: &[Value], prefix: &Path) -> Vec<DeletedCask> {
    casks
        .iter()
        .filter_map(|c| {
            let token = str_of(c, "token")?;
            let apps = cask_apps(c);
            let caskroom = prefix.join("Caskroom").join(&token);
            (!apps.is_empty() && apps.iter().all(|a| !Path::new(a).exists()) && caskroom.is_dir())
                .then(|| DeletedCask { token, caskroom, names: stanza_names(c) })
        })
        .collect()
}

/// Casks whose apps were deleted without Homebrew. Empty without Homebrew.
pub fn deleted_casks() -> Vec<DeletedCask> {
    installed_casks().map(|(casks, prefix)| deleted_in(&casks, &prefix)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Value {
        json!({
            "formulae": [
                {"name": "ffmpeg", "full_name": "ffmpeg", "desc": "Play, record, convert", "service": null,
                 "installed": [{"version": "8.0", "installed_on_request": true,
                                "runtime_dependencies": [{"full_name": "x264"}, {"full_name": "lame"}]}]},
                {"name": "x264", "full_name": "x264", "desc": "H.264 encoder", "service": null,
                 "installed": [{"version": "r3222", "installed_on_request": false, "runtime_dependencies": []}]},
                {"name": "lame", "full_name": "lame", "desc": null, "service": null,
                 "installed": [{"version": "3.100", "installed_on_request": true, "runtime_dependencies": []}]},
                {"name": "oldlib", "full_name": "oldlib", "desc": "", "service": null,
                 "installed": [{"version": "1", "installed_on_request": false, "runtime_dependencies": []}]},
                {"name": "postgresql@16", "full_name": "postgresql@16", "desc": "Database", "service": {"run": ["x"]},
                 "installed": [{"version": "16.4", "installed_on_request": true, "runtime_dependencies": []}]}
            ],
            "casks": [
                {"token": "viewer", "name": ["Viewer"], "desc": "Views things", "installed": "2.1", "version": "2.2",
                 "depends_on": {"formula": ["lame"]},
                 "artifacts": [{"uninstall": [{"quit": "com.viewer"}]}, {"app": ["Viewer.app"], "target": "/nonexistent/Viewer.app"}]},
                {"token": "tool-cli", "name": [], "desc": null, "installed": "1.0", "depends_on": {},
                 "artifacts": [{"binary": ["bin/tool"], "target": "/opt/homebrew/bin/tool"}]}
            ]
        })
    }

    #[test]
    fn lists_requested_formulae_casks_and_unused_dependencies() {
        let unused = HashSet::from(["oldlib".to_string()]);
        let parsed = parse(&fixture(), Path::new("/opt/homebrew"), &unused);
        let names: Vec<(&str, BrewKind)> = parsed.iter().map(|(p, _)| (p.name.as_str(), p.kind)).collect();
        // x264 is a dependency something still needs: not listed.
        assert_eq!(
            names,
            vec![
                ("ffmpeg", BrewKind::Formula),
                ("lame", BrewKind::Formula),
                ("oldlib", BrewKind::Formula),
                ("postgresql@16", BrewKind::Formula),
                ("viewer", BrewKind::Cask),
                ("tool-cli", BrewKind::Cask),
            ]
        );
        let by_name = |n: &str| parsed.iter().find(|(p, _)| p.name == n).unwrap();
        let (lame, lame_paths) = by_name("lame");
        assert_eq!(lame.dependents, vec!["ffmpeg", "viewer"]);
        assert_eq!(lame.desc, None);
        assert_eq!(lame_paths, &vec![PathBuf::from("/opt/homebrew/Cellar/lame")]);
        assert!(by_name("postgresql@16").0.service && !by_name("ffmpeg").0.service);
        assert!(!by_name("oldlib").0.requested && by_name("oldlib").0.desc.is_none());

        let (viewer, viewer_paths) = by_name("viewer");
        assert_eq!((viewer.title.as_str(), viewer.version.as_str()), ("Viewer", "2.1"));
        // The app is gone from disk: only the Caskroom counts, and Homebrew is out of sync.
        assert!(viewer.apps.is_empty() && viewer.apps_missing);
        assert!(!by_name("tool-cli").0.apps_missing);
        assert_eq!(viewer_paths, &vec![PathBuf::from("/opt/homebrew/Caskroom/viewer")]);
        assert_eq!(by_name("tool-cli").0.title, "tool-cli");
    }

    #[test]
    fn cask_apps_default_to_the_applications_folder() {
        let cask = json!({"artifacts": [{"app": ["Sub/Foo.app"]}, {"app": ["Bar.app"], "target": "/Applications/Baz.app"}, {"pkg": ["x.pkg"]}]});
        assert_eq!(cask_apps(&cask), vec!["/Applications/Foo.app", "/Applications/Baz.app"]);
    }

    #[test]
    fn linked_files_of_a_cask_count() {
        let tmp = crate::testutil::TempDir::new("brew-links");
        let sdk = tmp.path().join("share/sdk");
        std::fs::create_dir_all(&sdk).unwrap();
        let version = tmp.path().join("Caskroom/cli/1.0");
        std::fs::create_dir_all(&version).unwrap();
        std::os::unix::fs::symlink(&sdk, version.join("sdk")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("gone"), version.join("broken")).unwrap();
        std::os::unix::fs::symlink(&version, tmp.path().join("Caskroom/cli/latest")).unwrap();
        assert_eq!(linked_targets(&tmp.path().join("Caskroom/cli")), vec![std::fs::canonicalize(&sdk).unwrap()]);
    }

    #[test]
    fn deleted_casks_and_their_names() {
        let tmp = crate::testutil::TempDir::new("brew-deleted");
        std::fs::create_dir_all(tmp.path().join("Caskroom/viewer/2.1")).unwrap();
        std::fs::create_dir_all(tmp.path().join("Caskroom/tool-cli/1.0")).unwrap();
        let casks = fixture()["casks"].as_array().unwrap().clone();
        let deleted = deleted_in(&casks, tmp.path());
        // tool-cli has no app: it isn't a deleted app.
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].token, "viewer");
        assert_eq!(deleted[0].caskroom, tmp.path().join("Caskroom/viewer"));
        assert_eq!(deleted[0].names, vec!["com.viewer"]);

        let cask = json!({"artifacts": [
            {"uninstall": [{"signal": ["TERM", "org.foo.app"], "quit": "org.foo.app"}]},
            {"zap": [{"trash": ["~/Library/Preferences/org.foo.app.plist", "~/Library/FooData"]}]}
        ]});
        assert_eq!(stanza_names(&cask), vec!["org.foo.app", "TERM", "org.foo.app.plist", "FooData"]);
    }

    #[test]
    fn parses_autoremove_and_errors() {
        let out = "==> Would autoremove 2 unneeded formulae:\nlibfoo\nbar@2\n";
        assert_eq!(parse_autoremove(out), HashSet::from(["libfoo".to_string(), "bar@2".to_string()]));
        assert!(parse_autoremove("").is_empty());
        assert_eq!(
            brew_error("Error: Refusing to uninstall /opt/homebrew/Cellar/lame/3.100\nbecause it is required by ffmpeg.\n"),
            "Refusing to uninstall /opt/homebrew/Cellar/lame/3.100"
        );
        assert_eq!(brew_error("Warning: x\nsudo: a terminal is required\n"), "sudo: a terminal is required");
    }

    #[test]
    fn package_names_are_never_options() {
        for ok in ["ffmpeg", "postgresql@16", "homebrew/cask/foo", "gtk+3", "python3.12"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "--force", "-f", "a b", "a;rm", "$(x)"] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert!(uninstall("--ignore-dependencies", BrewKind::Formula).is_err());
    }
}
