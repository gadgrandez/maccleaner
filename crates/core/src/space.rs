//! Space view: size of each direct child of a folder.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::fsutil::{self, Usage};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub bytes: u64,
    pub files: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceReport {
    pub path: String,
    pub total_bytes: u64,
    pub total_files: u64,
    pub errors: u64,
    /// The `top` largest, from largest to smallest.
    pub entries: Vec<SpaceEntry>,
    pub others_bytes: u64,
    pub others_count: u64,
    pub elapsed_ms: u64,
}

pub fn space(path: &Path, top: usize) -> io::Result<SpaceReport> {
    let started = Instant::now();
    let mut children: Vec<(PathBuf, bool)> = Vec::new();
    let mut errors = 0u64;
    for entry in fs::read_dir(path)? {
        match entry {
            Ok(e) => {
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                children.push((e.path(), is_dir));
            }
            Err(_) => errors += 1,
        }
    }
    let paths: Vec<PathBuf> = children.iter().map(|(p, _)| p.clone()).collect();
    let usages: Vec<Usage> = fsutil::usages(&paths);

    let mut entries: Vec<SpaceEntry> = Vec::with_capacity(children.len());
    let mut total = Usage { errors, ..Usage::default() };
    for ((p, is_dir), u) in children.into_iter().zip(usages) {
        total.add(u);
        entries.push(SpaceEntry {
            name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            path: p.to_string_lossy().into_owned(),
            is_dir,
            bytes: u.bytes,
            files: u.files,
        });
    }
    entries.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes));
    let rest = if entries.len() > top { entries.split_off(top) } else { Vec::new() };

    Ok(SpaceReport {
        path: path.to_string_lossy().into_owned(),
        total_bytes: total.bytes,
        total_files: total.files,
        errors: total.errors,
        entries,
        others_bytes: rest.iter().map(|e| e.bytes).sum(),
        others_count: rest.len() as u64,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn keeps_top_and_groups_rest() {
        let tmp = TempDir::new("space");
        for (i, size) in [100_000usize, 50_000, 10, 20].iter().enumerate() {
            fs::write(tmp.path().join(format!("f{i}")), vec![0u8; *size]).unwrap();
        }
        let r = space(tmp.path(), 2).unwrap();
        assert_eq!(r.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["f0", "f1"]);
        assert_eq!(r.others_count, 2);
        assert_eq!(r.total_files, 4);
        assert_eq!(r.total_bytes, r.entries.iter().map(|e| e.bytes).sum::<u64>() + r.others_bytes);
    }
}
