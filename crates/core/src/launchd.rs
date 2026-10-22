//! Registers the monitor as a user LaunchAgent (unprivileged).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::rules::home_dir;

pub const MONITOR_LABEL: &str = "com.maccleaner.monitor";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorStatus {
    pub installed: bool,
    pub running: bool,
    pub pid: Option<i32>,
    pub program: Option<String>,
}

pub fn plist_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join("Library/LaunchAgents").join(format!("{MONITOR_LABEL}.plist")))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn plist_contents(program: &Path) -> String {
    let program = xml_escape(&program.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{MONITOR_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{program}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>60</integer>
    <key>ProcessType</key>
    <string>Background</string>
    <key>LowPriorityIO</key>
    <true/>
    <key>Nice</key>
    <integer>10</integer>
</dict>
</plist>
"#
    )
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

fn launchctl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("/bin/launchctl").args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn install(program: &Path) -> Result<(), String> {
    if !program.is_file() {
        return Err(format!("monitor executable not found: {}", program.display()));
    }
    let plist = plist_path().ok_or("HOME is not set")?;
    if let Some(dir) = plist.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    fs::write(&plist, plist_contents(program)).map_err(|e| e.to_string())?;
    let _ = launchctl(&["bootout", &format!("{}/{MONITOR_LABEL}", domain())]);
    launchctl(&["bootstrap", &domain(), &plist.to_string_lossy()]).map(|_| ())
}

pub fn uninstall() -> Result<(), String> {
    let _ = launchctl(&["bootout", &format!("{}/{MONITOR_LABEL}", domain())]);
    match plist_path() {
        Some(p) if p.exists() => fs::remove_file(p).map_err(|e| e.to_string()),
        _ => Ok(()),
    }
}

pub fn status() -> MonitorStatus {
    let installed = plist_path().is_some_and(|p| p.exists());
    let printed = launchctl(&["print", &format!("{}/{MONITOR_LABEL}", domain())]).ok();
    let field = |key: &str| {
        printed.as_deref().and_then(|out| {
            out.lines().map(str::trim).find_map(|l| l.strip_prefix(key).map(|v| v.trim().to_string()))
        })
    };
    let pid = field("pid = ").and_then(|v| v.parse().ok());
    MonitorStatus { installed, running: pid.is_some(), pid, program: field("program = ") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_escapes_program_path() {
        let p = plist_contents(Path::new("/Apps/A&B <x>.app/monitor"));
        assert!(p.contains("/Apps/A&amp;B &lt;x&gt;.app/monitor"));
        assert!(p.contains("<string>Background</string>"));
    }
}
