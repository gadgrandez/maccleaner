//! State and configuration shared between the monitor and the app.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::rules::home_dir;

pub fn support_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join("Library/Application Support/MacCleaner"))
}

pub fn monitor_state_path() -> Option<PathBuf> {
    support_dir().map(|d| d.join("monitor-state.json"))
}

pub fn monitor_config_path() -> Option<PathBuf> {
    support_dir().map(|d| d.join("monitor-config.json"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorConfig {
    pub check_interval_secs: u64,
    /// Alert when free space drops below this percentage.
    pub disk_free_min_percent: f64,
    pub junk_scan_interval_hours: u64,
    /// Alert when the junk selected by default exceeds this size.
    pub junk_alert_bytes: u64,
    pub swap_alert_bytes: u64,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            check_interval_secs: 60,
            disk_free_min_percent: 10.0,
            junk_scan_interval_hours: 24,
            junk_alert_bytes: 5 << 30,
            swap_alert_bytes: 4 << 30,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorState {
    pub started_at: u64,
    pub last_check: u64,
    pub last_junk_scan: u64,
    pub last_junk_bytes: u64,
    pub last_alert_disk: u64,
    pub last_alert_pressure: u64,
    pub last_alert_swap: u64,
    pub last_alert_junk: u64,
}
