//! Validated cleaning: only direct entries of a rule root are acted on.

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::rules::{Action, Rule};
use crate::{fsutil, progress};
use crate::trash;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanRequest {
    pub paths: Vec<String>,
    /// By default nothing is touched.
    #[serde(default = "default_true")]
    pub dry_run: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanOutcome {
    pub path: String,
    pub category: Option<String>,
    pub action: Option<Action>,
    pub bytes: u64,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanReport {
    pub dry_run: bool,
    /// Bytes moved to the Trash (freed when it is emptied).
    pub trashed_bytes: u64,
    /// Bytes permanently deleted.
    pub deleted_bytes: u64,
    pub failed: u64,
    pub outcomes: Vec<CleanOutcome>,
}

fn validate<'r>(rules: &'r [Rule], canon_roots: &[(usize, PathBuf)], raw: &str) -> Result<(&'r Rule, PathBuf), String> {
    let path = PathBuf::from(raw);
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return Err("invalid path".into());
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err("invalid path".into());
    };
    let canon_parent = fs::canonicalize(parent).map_err(|e| e.to_string())?;
    let mut candidates = canon_roots.iter().filter(|(_, root)| *root == canon_parent).map(|(i, _)| &rules[*i]).peekable();
    if candidates.peek().is_none() {
        return Err("outside the allowed paths".into());
    }
    fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    let mut error = "protected item";
    for rule in candidates {
        if rule.is_excluded(name) {
            continue;
        }
        // The filter is evaluated on the real path (canonicalized parent).
        if rule.passes_filter(&canon_parent.join(name)) {
            return Ok((rule, path));
        }
        error = "does not meet its category's criteria";
    }
    Err(error.into())
}

/// Deletes `path` for good. Entries can't be removed from read-only folders (Go's module cache
/// is read-only on purpose), so on a permission error the owner's write permission is
/// restored on every folder inside and the deletion is retried, as Finder does.
fn remove(path: &Path) -> std::io::Result<()> {
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

pub fn clean(rules: &[Rule], req: &CleanRequest) -> CleanReport {
    let canon_roots: Vec<(usize, PathBuf)> = rules
        .iter()
        .enumerate()
        .flat_map(|(i, r)| r.roots.iter().filter_map(move |root| fs::canonicalize(root).ok().map(|c| (i, c))))
        .collect();

    let validated: Vec<Result<(&Rule, PathBuf), String>> =
        req.paths.iter().map(|p| validate(rules, &canon_roots, p)).collect();
    let valid_paths: Vec<PathBuf> = validated.iter().filter_map(|v| v.as_ref().ok().map(|(_, p)| p.clone())).collect();
    let _progress = progress::start(valid_paths.len(), req.dry_run);
    let mut valid_sizes = fsutil::usages(&valid_paths).into_iter().map(|u| u.bytes);
    let sizes: Vec<u64> = validated.iter().map(|v| if v.is_ok() { valid_sizes.next().unwrap_or(0) } else { 0 }).collect();

    let mut report = CleanReport { dry_run: req.dry_run, trashed_bytes: 0, deleted_bytes: 0, failed: 0, outcomes: Vec::new() };
    for ((raw, v), bytes) in req.paths.iter().zip(validated).zip(sizes) {
        let outcome = match v {
            Err(error) => CleanOutcome { path: raw.clone(), category: None, action: None, bytes: 0, ok: false, error: Some(error) },
            Ok((rule, path)) => {
                let result = if req.dry_run {
                    Ok(())
                } else {
                    progress::step(&path, bytes, || match rule.action {
                        Action::Trash => trash::move_to_trash(&path),
                        Action::Delete => remove(&path).map_err(|e| e.to_string()),
                    })
                };
                if result.is_ok() {
                    match rule.action {
                        Action::Trash => report.trashed_bytes += bytes,
                        Action::Delete => report.deleted_bytes += bytes,
                    }
                }
                CleanOutcome {
                    path: raw.clone(),
                    category: Some(rule.id.clone()),
                    action: Some(rule.action),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::resolve;
    use crate::testutil::TempDir;

    fn setup() -> (TempDir, Vec<Rule>) {
        let tmp = TempDir::new("clean");
        let home = tmp.path().to_path_buf();
        fs::create_dir_all(home.join("Library/Caches/app")).unwrap();
        fs::write(home.join("Library/Caches/app/data"), vec![0u8; 20_000]).unwrap();
        fs::create_dir_all(home.join("Library/Caches/CloudKit")).unwrap();
        fs::create_dir_all(home.join(".Trash/old")).unwrap();
        fs::write(home.join(".Trash/old/f"), b"x").unwrap();
        fs::write(home.join("document.txt"), b"important").unwrap();
        fs::create_dir_all(home.join("Library/Preferences")).unwrap();
        fs::write(home.join("Library/Preferences/broken.plist"), b"garbage").unwrap();
        fs::write(home.join("Library/Preferences/healthy.plist"), br#"<plist version="1.0"><true/></plist>"#).unwrap();
        fs::create_dir_all(home.join("Downloads")).unwrap();
        fs::write(home.join("Downloads/old.dmg"), b"x").unwrap();
        fs::write(home.join("Downloads/new.dmg"), b"x").unwrap();
        fs::write(home.join("Downloads/old.pdf"), b"x").unwrap();
        crate::rules::set_mtime(&home.join("Downloads/old.dmg"), 40);
        crate::rules::set_mtime(&home.join("Downloads/old.pdf"), 40);
        let rules = resolve(&home);
        (tmp, rules)
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn rejects_paths_outside_rules() {
        let (tmp, rules) = setup();
        let h = tmp.path();
        let req = CleanRequest {
            paths: vec![
                s(&h.join("document.txt")),
                s(&h.join("Library/Caches")),
                s(&h.join("Library/Caches/app/data")),
                s(&h.join("Library/Caches/CloudKit")),
                s(&h.join("Library/Caches/../../document.txt")),
                "relative".into(),
            ],
            dry_run: false,
        };
        let r = clean(&rules, &req);
        assert_eq!(r.failed, 6);
        assert!(h.join("document.txt").exists());
        assert!(h.join("Library/Caches/app/data").exists());
    }

    #[test]
    fn respects_rule_filters() {
        let (tmp, rules) = setup();
        let h = tmp.path();
        let paths = ["Library/Preferences/broken.plist", "Library/Preferences/healthy.plist", "Downloads/old.dmg", "Downloads/new.dmg", "Downloads/old.pdf"];
        let req = CleanRequest { paths: paths.iter().map(|p| s(&h.join(p))).collect(), dry_run: true };
        let r = clean(&rules, &req);
        let ok: Vec<_> = r.outcomes.iter().map(|o| (o.ok, o.category.clone())).collect();
        assert_eq!(
            ok,
            vec![
                (true, Some("broken_preferences".into())),
                (false, None),
                (true, Some("unused_disk_images".into())),
                (false, None),
                (false, None),
            ]
        );
        assert_eq!(r.outcomes[1].error.as_deref(), Some("does not meet its category's criteria"));
    }

    #[test]
    fn dry_run_touches_nothing() {
        let (tmp, rules) = setup();
        let target = tmp.path().join("Library/Caches/app");
        let r = clean(&rules, &CleanRequest { paths: vec![s(&target)], dry_run: true });
        assert_eq!(r.failed, 0);
        assert!(r.trashed_bytes >= 20_000);
        assert!(target.exists());
    }

    #[test]
    fn delete_action_removes_from_trash_dir() {
        let (tmp, rules) = setup();
        let target = tmp.path().join(".Trash/old");
        let r = clean(&rules, &CleanRequest { paths: vec![s(&target)], dry_run: false });
        assert_eq!(r.failed, 0, "{:?}", r.outcomes);
        assert!(r.deleted_bytes > 0);
        assert!(!target.exists());
    }

    #[test]
    fn delete_handles_read_only_folders() {
        let (tmp, rules) = setup();
        // Like Go's module cache once in the Trash: read-only folders with files inside.
        let target = tmp.path().join(".Trash/mod");
        let deep = target.join("github.com/x/y@v1");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("go.mod"), b"module y").unwrap();
        for dir in [&deep, &deep.parent().unwrap().to_path_buf(), &target.join("github.com")] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
        }
        let r = clean(&rules, &CleanRequest { paths: vec![s(&target)], dry_run: false });
        assert_eq!(r.failed, 0, "{:?}", r.outcomes);
        assert!(!target.exists());
    }
}
