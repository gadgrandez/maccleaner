//! Coexisting with the rest of the system during disk scans.
//!
//! - Scan threads lower their CPU priority (QoS utility) and I/O priority (utility tier),
//!   so they yield to the app the user is working in.
//! - Under "warning" memory pressure fewer threads are used; under "critical" pressure the scan
//!   does not start or is cut short: walking millions of files fills the kernel's vnode cache
//!   and pushes macOS to kill processes to reclaim memory.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::system::{self, Pressure};

pub const ABORT_MESSAGE: &str =
    "Scan stopped: system memory is under critical pressure. Close apps or tabs you're not using and try again.";

/// Scan threads under "warning" memory pressure.
const THREADS_UNDER_WARNING: usize = 2;

/// Memory-pressure abort state shared by the scans in progress.
struct Gate {
    aborted: AtomicBool,
    /// Scans in progress. The abort is only reset when none are left: that way a new scan
    /// does not clear the abort of another one still running (which would return incomplete results).
    active: AtomicUsize,
}

impl Gate {
    const fn new() -> Self {
        Self { aborted: AtomicBool::new(false), active: AtomicUsize::new(0) }
    }

    fn begin(&self, critical_now: bool) -> Result<(), String> {
        if self.active.fetch_add(1, Ordering::AcqRel) == 0 {
            self.aborted.store(false, Ordering::Relaxed);
        }
        if critical_now {
            self.aborted.store(true, Ordering::Relaxed);
        }
        if self.aborted.load(Ordering::Relaxed) {
            self.finish();
            return Err(ABORT_MESSAGE.into());
        }
        Ok(())
    }

    /// Returns whether an abort happened while the scan was running.
    fn finish(&self) -> bool {
        let aborted = self.aborted.load(Ordering::Relaxed);
        self.active.fetch_sub(1, Ordering::AcqRel);
        aborted
    }
}

static GATE: Gate = Gate::new();

const QOS_CLASS_UTILITY: u32 = 0x11;
const IOPOL_TYPE_DISK: i32 = 0;
const IOPOL_SCOPE_THREAD: i32 = 1;
const IOPOL_UTILITY: i32 = 4;

unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
    fn setiopolicy_np(iotype: i32, scope: i32, policy: i32) -> i32;
}

/// Lowers the current thread's CPU and disk priority. Call at the start of each scan thread.
pub fn lower_thread_priority() {
    unsafe {
        pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0);
        setiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_THREAD, IOPOL_UTILITY);
    }
}

/// Threads to use given the current memory pressure.
pub fn thread_budget(max: usize) -> usize {
    match system::pressure() {
        Pressure::Warning => max.min(THREADS_UNDER_WARNING),
        Pressure::Critical => 1,
        Pressure::Normal | Pressure::Unknown => max,
    }
}

/// Reads the pressure (one `sysctl` call) and flags the abort if it is critical.
pub fn poll() -> bool {
    if system::pressure() == Pressure::Critical {
        GATE.aborted.store(true, Ordering::Relaxed);
    }
    aborted()
}

/// Cheap check for hot loops.
pub fn aborted() -> bool {
    GATE.aborted.load(Ordering::Relaxed)
}

/// Runs a full scan under the governor; errors if it could not start or was cut short.
pub fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    GATE.begin(system::pressure() == Pressure::Critical)?;
    let result = f();
    if GATE.finish() { Err(ABORT_MESSAGE.into()) } else { Ok(result) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_never_exceeds_max_or_hits_zero() {
        for max in [1, 2, 8] {
            let b = thread_budget(max);
            assert!(b >= 1 && b <= max);
        }
    }

    #[test]
    fn concurrent_scans_keep_each_others_abort() {
        // Own instance: does not touch the global state used by other tests in parallel.
        let gate = Gate::new();
        assert!(gate.begin(false).is_ok());
        gate.aborted.store(true, Ordering::Relaxed); // the first one is aborted…
        assert!(gate.begin(false).is_err(), "…and a new one neither clears it nor starts");
        assert!(gate.finish(), "the first one sees its abort");
        assert_eq!(gate.active.load(Ordering::Relaxed), 0);
        assert!(gate.begin(false).is_ok(), "with no active scans, the abort is reset");
        assert!(!gate.finish());
        assert!(gate.begin(true).is_err(), "under critical pressure it does not start");
    }

    #[test]
    fn lowering_priority_is_harmless() {
        std::thread::spawn(lower_thread_priority).join().unwrap();
    }
}
