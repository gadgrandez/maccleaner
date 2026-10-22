//! Progress of the cleanup in course. Cleanups report every item as they act on it, and the
//! app polls `snapshot()` while it waits for the (blocking) cleanup call to return.
//! Dry runs are not reported: they are instant and must not hide a real cleanup.

use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanProgress {
    pub active: bool,
    /// Items already handled (cleaned or failed) out of `total`.
    pub done: u64,
    pub total: u64,
    /// Bytes of the items cleaned so far.
    pub bytes: u64,
    /// Item being cleaned right now; `None` while sizes are being measured.
    pub current: Option<String>,
}

impl CleanProgress {
    fn begin(&mut self, total: usize) {
        *self = Self { active: true, total: total as u64, ..Default::default() };
    }

    fn begin_item(&mut self, path: &Path) {
        self.current = Some(path.to_string_lossy().into_owned());
    }

    fn end_item(&mut self, ok: bool, bytes: u64) {
        self.done += 1;
        if ok {
            self.bytes += bytes;
        }
    }

    fn end(&mut self) {
        self.active = false;
        self.current = None;
    }
}

static STATE: Mutex<CleanProgress> = Mutex::new(CleanProgress { active: false, done: 0, total: 0, bytes: 0, current: None });

fn update(f: impl FnOnce(&mut CleanProgress)) {
    if let Ok(mut state) = STATE.lock() {
        f(&mut state);
    }
}

/// Ends the cleanup's progress when dropped.
pub struct Tracker {
    live: bool,
}

impl Drop for Tracker {
    fn drop(&mut self) {
        if self.live {
            update(CleanProgress::end);
        }
    }
}

/// Starts reporting a cleanup of `total` items (nothing is reported for a dry run).
pub fn start(total: usize, dry_run: bool) -> Tracker {
    if !dry_run {
        update(|s| s.begin(total));
    }
    Tracker { live: !dry_run }
}

/// Runs the action on one item, showing it as the current one; its bytes count once it succeeds.
pub fn step<T, E>(path: &Path, bytes: u64, action: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    update(|s| s.begin_item(path));
    let result = action();
    update(|s| s.end_item(result.is_ok(), bytes));
    result
}

pub fn snapshot() -> CleanProgress {
    STATE.lock().map(|s| s.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // On a local value: the global one is shared with every cleanup test running in parallel.
    #[test]
    fn counts_items_and_bytes_of_successes() {
        let mut p = CleanProgress::default();
        p.begin(2);
        assert_eq!(p, CleanProgress { active: true, total: 2, ..Default::default() });
        p.begin_item(Path::new("/a"));
        assert_eq!(p.current.as_deref(), Some("/a"));
        p.end_item(true, 10);
        p.begin_item(Path::new("/b"));
        p.end_item(false, 5);
        assert_eq!((p.done, p.bytes, p.current.as_deref()), (2, 10, Some("/b")));
        p.end();
        assert!(!p.active && p.current.is_none() && p.done == 2);
        p.begin(1);
        assert_eq!((p.done, p.bytes), (0, 0));
    }
}
