use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::fsutil::{self, Usage};
use crate::rules::{Action, Rule};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub path: String,
    pub bytes: u64,
    pub files: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryReport {
    pub id: String,
    pub name: String,
    pub description: String,
    pub action: Action,
    pub default_selected: bool,
    pub bytes: u64,
    pub files: u64,
    pub errors: u64,
    /// Sorted from largest to smallest.
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanReport {
    pub categories: Vec<CategoryReport>,
    pub total_bytes: u64,
    pub elapsed_ms: u64,
}

impl ScanReport {
    /// Bytes of the categories selected by default.
    pub fn default_selected_bytes(&self) -> u64 {
        self.categories.iter().filter(|c| c.default_selected).map(|c| c.bytes).sum()
    }
}

pub fn scan(rules: &[Rule]) -> ScanReport {
    let started = Instant::now();
    let mut work: Vec<(usize, PathBuf)> = Vec::new();
    let mut list_errors = vec![0u64; rules.len()];

    for (ci, rule) in rules.iter().enumerate() {
        for root in &rule.roots {
            let entries = match fs::read_dir(root) {
                Ok(rd) => rd,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => {
                    list_errors[ci] += 1;
                    continue;
                }
            };
            for entry in entries {
                match entry {
                    Ok(e) if !rule.is_excluded(&e.file_name()) && rule.passes_filter(&e.path()) => {
                        work.push((ci, e.path()))
                    }
                    Ok(_) => {}
                    Err(_) => list_errors[ci] += 1,
                }
            }
        }
    }

    let paths: Vec<PathBuf> = work.iter().map(|(_, p)| p.clone()).collect();
    let usages: Vec<Usage> = fsutil::usages(&paths);

    let mut categories: Vec<CategoryReport> = rules
        .iter()
        .zip(&list_errors)
        .map(|(r, &errors)| CategoryReport {
            id: r.id.clone(),
            name: r.name.clone(),
            description: r.description.clone(),
            action: r.action,
            default_selected: r.default_selected,
            bytes: 0,
            files: 0,
            errors,
            items: Vec::new(),
        })
        .collect();

    for ((ci, path), u) in work.into_iter().zip(usages) {
        let cat = &mut categories[ci];
        cat.errors += u.errors;
        if u.bytes == 0 {
            continue;
        }
        cat.bytes += u.bytes;
        cat.files += u.files;
        cat.items.push(Item { path: path.to_string_lossy().into_owned(), bytes: u.bytes, files: u.files });
    }
    for cat in &mut categories {
        cat.items.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes));
    }

    ScanReport {
        total_bytes: categories.iter().map(|c| c.bytes).sum(),
        categories,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::resolve;
    use crate::testutil::TempDir;

    #[test]
    fn scans_entries_and_skips_excluded_and_empty() {
        let tmp = TempDir::new("scan");
        let home = tmp.path();
        let caches = home.join("Library/Caches");
        fs::create_dir_all(caches.join("big")).unwrap();
        fs::create_dir_all(caches.join("empty")).unwrap();
        fs::create_dir_all(caches.join("CloudKit")).unwrap();
        fs::write(caches.join("big/a"), vec![0u8; 50_000]).unwrap();
        fs::write(caches.join("small"), b"hey!").unwrap();
        fs::write(caches.join("CloudKit/x"), vec![0u8; 50_000]).unwrap();

        let report = scan(&resolve(home));
        let cat = report.categories.iter().find(|c| c.id == "user_caches").unwrap();
        let names: Vec<_> = cat.items.iter().map(|i| i.path.rsplit('/').next().unwrap()).collect();
        assert_eq!(names, vec!["big", "small"]);
        assert_eq!(cat.files, 2);
        assert_eq!(report.total_bytes, cat.bytes);
    }

    #[test]
    fn new_categories_apply_their_filters() {
        let tmp = TempDir::new("scan-ext");
        let home = tmp.path();
        let prefs = home.join("Library/Preferences");
        fs::create_dir_all(&prefs).unwrap();
        fs::write(prefs.join("broken.plist"), b"<plist><dict><key>").unwrap();
        let mut good = Vec::new();
        plist::Value::Boolean(true).to_writer_xml(&mut good).unwrap();
        fs::write(prefs.join("good.plist"), &good).unwrap();
        let dl = home.join("Downloads");
        fs::create_dir_all(&dl).unwrap();
        fs::write(dl.join("old.dmg"), vec![0u8; 10_000]).unwrap();
        fs::write(dl.join("new.dmg"), vec![0u8; 10_000]).unwrap();
        fs::write(dl.join("invoice.pdf"), vec![0u8; 10_000]).unwrap();
        crate::rules::set_mtime(&dl.join("old.dmg"), 45);
        crate::rules::set_mtime(&dl.join("invoice.pdf"), 45);
        let sim = home.join("Library/Logs/CoreSimulator/ABCD");
        fs::create_dir_all(&sim).unwrap();
        fs::write(sim.join("system.log"), vec![0u8; 5_000]).unwrap();
        let backup = home.join("Library/Application Support/MobileSync/Backup/0001");
        fs::create_dir_all(&backup).unwrap();
        fs::write(backup.join("Manifest.db"), vec![0u8; 5_000]).unwrap();

        let report = scan(&resolve(home));
        let names = |id: &str| -> Vec<String> {
            let cat = report.categories.iter().find(|c| c.id == id).unwrap();
            cat.items.iter().map(|i| i.path.rsplit('/').next().unwrap().to_owned()).collect()
        };
        assert_eq!(names("broken_preferences"), vec!["broken.plist"]);
        assert_eq!(names("unused_disk_images"), vec!["old.dmg"]);
        assert_eq!(names("simulator_devices_logs"), vec!["ABCD"]);
        assert!(names("user_logs").is_empty(), "CoreSimulator must not be counted twice");
        assert_eq!(names("ios_backups"), vec!["0001"]);
        let backups = report.categories.iter().find(|c| c.id == "ios_backups").unwrap();
        assert!(!backups.default_selected);
    }
}
