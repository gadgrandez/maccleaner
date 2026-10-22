//! Background monitor. Designed to stay resident at ~1–2 MB:
//! - Only cheap syscalls per cycle (statfs, sysctl, host_statistics64).
//! - Background priority (throttled CPU and I/O) on top of `ProcessType=Background` in launchd.
//! - The junk scan runs in a child process (`--scan-junk`) that frees everything on exit.
//! - Notifications are posted by the app in ephemeral mode (`MacCleaner --notify`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use maccleaner_core::state::{self, MonitorConfig, MonitorState};
use maccleaner_core::system::{self, Pressure};
use maccleaner_core::{governor, rules, scan};
use serde::{Deserialize, Serialize};

const PRIO_DARWIN_PROCESS: libc::c_int = 4;
const PRIO_DARWIN_BG: libc::c_int = 0x1000;

const HOUR: u64 = 3600;
const DISK_ALERT_EVERY: u64 = 6 * HOUR;
const PRESSURE_ALERT_EVERY: u64 = HOUR;
const SWAP_ALERT_EVERY: u64 = 6 * HOUR;
const JUNK_ALERT_EVERY: u64 = 24 * HOUR;
const HEARTBEAT_EVERY: u64 = HOUR;

#[derive(Serialize, Deserialize)]
struct JunkSummary {
    total_bytes: u64,
    selected_bytes: u64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: Option<PathBuf>) -> Option<T> {
    serde_json::from_slice(&fs::read(path?).ok()?).ok()
}

fn save_state(st: &MonitorState) {
    let Some(path) = state::monitor_state_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if let Ok(json) = serde_json::to_vec_pretty(st) {
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }
}

fn config_mtime() -> Option<SystemTime> {
    fs::metadata(state::monitor_config_path()?).ok()?.modified().ok()
}

fn applescript_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Alerts the monitor can raise. The app turns them into text in the device language, so the
/// monitor only sends a kind and its values (`MacCleaner --notify <kind> <values…>`).
enum Alert {
    DiskLow { free: u64, percent: f64 },
    MemoryPressure,
    SwapHigh { used: u64 },
    JunkReady { bytes: u64 },
}

impl Alert {
    fn kind(&self) -> &'static str {
        match self {
            Alert::DiskLow { .. } => "disk_low",
            Alert::MemoryPressure => "memory_pressure",
            Alert::SwapHigh { .. } => "swap_high",
            Alert::JunkReady { .. } => "junk_ready",
        }
    }

    fn values(&self) -> Vec<String> {
        match self {
            Alert::DiskLow { free, percent } => vec![gib(*free), format!("{percent:.0}")],
            Alert::MemoryPressure => Vec::new(),
            Alert::SwapHigh { used } => vec![gib(*used)],
            Alert::JunkReady { bytes } => vec![gib(*bytes)],
        }
    }

    /// English text for the `osascript` fallback, when the app cannot post the notification.
    fn fallback(&self) -> (String, String) {
        match self {
            Alert::DiskLow { free, percent } => ("Low disk space".into(), format!("{} free ({percent:.0}%).", gib(*free))),
            Alert::MemoryPressure => (
                "High memory pressure".into(),
                "The system is running out of memory. Quit apps you're not using.".into(),
            ),
            Alert::SwapHigh { used } => ("High swap usage".into(), format!("{} in swap.", gib(*used))),
            Alert::JunkReady { bytes } => (
                "Junk ready to clean".into(),
                format!("{} can be reclaimed. Open MacCleaner to review it.", gib(*bytes)),
            ),
        }
    }
}

fn notify(alert: Alert) {
    // Inside MacCleaner.app/Contents/MacOS the app is the sibling executable.
    let gui = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.join("MacCleaner")));
    if let Some(gui) = gui.filter(|p| p.is_file()) {
        let ok = Command::new(&gui)
            .arg("--notify")
            .arg(alert.kind())
            .args(alert.values())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return;
        }
    }
    let (title, body) = alert.fallback();
    let script = format!("display notification {} with title {}", applescript_quote(&body), applescript_quote(&title));
    let _ = Command::new("/usr/bin/osascript").args(["-e", &script]).stdout(Stdio::null()).status();
}

fn run_junk_scan_child() -> Option<JunkSummary> {
    let exe = std::env::current_exe().ok()?;
    let out = Command::new(exe).arg("--scan-junk").stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn scan_junk_and_print() {
    // Under critical memory pressure no scan is done; the monitor retries on the next cycle.
    let report = match governor::guarded(|| scan::scan(&rules::default_rules())) {
        Ok(r) => r,
        Err(_) => std::process::exit(3),
    };
    let summary = JunkSummary { total_bytes: report.total_bytes, selected_bytes: report.default_selected_bytes() };
    println!("{}", serde_json::to_string(&summary).unwrap_or_default());
}

fn due(last: u64, every: u64, t: u64) -> bool {
    t.saturating_sub(last) >= every
}

struct Monitor {
    cfg: MonitorConfig,
    cfg_mtime: Option<SystemTime>,
    st: MonitorState,
    critical_samples: u32,
}

impl Monitor {
    fn tick(&mut self) {
        let t = now();
        let mut dirty = false;

        let mtime = config_mtime();
        if mtime != self.cfg_mtime {
            self.cfg = read_json(state::monitor_config_path()).unwrap_or_default();
            self.cfg_mtime = mtime;
        }

        let s = system::status();
        if s.disk_total > 0 {
            let free_pct = s.disk_available as f64 * 100.0 / s.disk_total as f64;
            if free_pct < self.cfg.disk_free_min_percent && due(self.st.last_alert_disk, DISK_ALERT_EVERY, t) {
                notify(Alert::DiskLow { free: s.disk_available, percent: free_pct });
                self.st.last_alert_disk = t;
                dirty = true;
            }
        }

        // Requiring two consecutive samples avoids alerting on momentary spikes.
        self.critical_samples = if s.pressure == Pressure::Critical { self.critical_samples + 1 } else { 0 };
        if self.critical_samples >= 2 && due(self.st.last_alert_pressure, PRESSURE_ALERT_EVERY, t) {
            notify(Alert::MemoryPressure);
            self.st.last_alert_pressure = t;
            dirty = true;
        }

        if s.swap_used > self.cfg.swap_alert_bytes && due(self.st.last_alert_swap, SWAP_ALERT_EVERY, t) {
            notify(Alert::SwapHigh { used: s.swap_used });
            self.st.last_alert_swap = t;
            dirty = true;
        }

        if due(self.st.last_junk_scan, self.cfg.junk_scan_interval_hours.max(1) * HOUR, t) {
            if let Some(j) = run_junk_scan_child() {
                self.st.last_junk_scan = t;
                self.st.last_junk_bytes = j.selected_bytes;
                dirty = true;
                if j.selected_bytes > self.cfg.junk_alert_bytes && due(self.st.last_alert_junk, JUNK_ALERT_EVERY, t) {
                    notify(Alert::JunkReady { bytes: j.selected_bytes });
                    self.st.last_alert_junk = t;
                }
            }
        }

        if dirty || due(self.st.last_check, HEARTBEAT_EVERY, t) {
            self.st.last_check = t;
            save_state(&self.st);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--scan-junk") => return scan_junk_and_print(),
        Some("--once") => {
            println!("{}", serde_json::to_string_pretty(&system::status()).unwrap_or_default());
            return;
        }
        Some(other) => {
            eprintln!("usage: {} [--once | --scan-junk]", Path::new(&args[0]).display());
            eprintln!("unknown argument: {other}");
            std::process::exit(2);
        }
        None => {}
    }

    unsafe { libc::setpriority(PRIO_DARWIN_PROCESS, 0, PRIO_DARWIN_BG) };

    let mut monitor = Monitor {
        cfg: read_json(state::monitor_config_path()).unwrap_or_default(),
        cfg_mtime: config_mtime(),
        st: read_json(state::monitor_state_path()).unwrap_or_default(),
        critical_samples: 0,
    };
    monitor.st.started_at = now();
    monitor.st.last_check = 0;
    loop {
        monitor.tick();
        thread::sleep(Duration::from_secs(monitor.cfg.check_interval_secs.clamp(10, 3600)));
    }
}
