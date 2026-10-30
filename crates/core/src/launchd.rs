//! Registers MacCleaner's background processes as user LaunchAgents (unprivileged): the monitor
//! and the menu bar monitor, which also start at login.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::rules::home_dir;

/// A LaunchAgent of ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Agent {
    pub label: &'static str,
    /// Low-priority background process; otherwise an interactive one (it has UI).
    pub background: bool,
}

pub const MONITOR: Agent = Agent { label: "com.maccleaner.monitor", background: true };
/// The menu bar item: `MacCleaner Menu.app`, a background-only app.
pub const MENU_BAR: Agent = Agent { label: "com.maccleaner.menu", background: false };

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatus {
    pub installed: bool,
    pub running: bool,
    pub pid: Option<i32>,
    pub program: Option<String>,
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
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

/// Unloads a job of the user's session; launchd stops its process and no longer restarts it.
pub(crate) fn bootout(label: &str) {
    let _ = launchctl(&["bootout", &format!("{}/{label}", domain())]);
}

impl Agent {
    pub fn plist_path(&self) -> Option<PathBuf> {
        home_dir().map(|h| h.join("Library/LaunchAgents").join(format!("{}.plist", self.label)))
    }

    /// Started at login and restarted if it crashes, but not after quitting normally.
    pub fn plist_contents(&self, program: &Path) -> String {
        let program = xml_escape(&program.to_string_lossy());
        let priority = if self.background {
            "<key>ProcessType</key>\n    <string>Background</string>\n    <key>LowPriorityIO</key>\n    <true/>\n    <key>Nice</key>\n    <integer>10</integer>"
        } else {
            "<key>ProcessType</key>\n    <string>Interactive</string>\n    <key>LimitLoadToSessionType</key>\n    <string>Aqua</string>"
        };
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
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
    {priority}
</dict>
</plist>
"#,
            label = self.label
        )
    }

    /// Writes the plist and (re)starts the job.
    pub fn install(&self, program: &Path) -> Result<(), String> {
        if !program.is_file() {
            return Err(format!("executable not found: {}", program.display()));
        }
        let plist = self.plist_path().ok_or("HOME is not set")?;
        if let Some(dir) = plist.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        fs::write(&plist, self.plist_contents(program)).map_err(|e| e.to_string())?;
        bootout(self.label);
        launchctl(&["bootstrap", &domain(), &plist.to_string_lossy()]).map(|_| ())
    }

    /// Installs it unless it is already installed for `program` and running.
    pub fn ensure(&self, program: &Path) -> Result<(), String> {
        let current = self.plist_path().and_then(|p| fs::read_to_string(p).ok());
        if current.as_deref() == Some(self.plist_contents(program).as_str()) && self.status().running {
            return Ok(());
        }
        self.install(program)
    }

    pub fn uninstall(&self) -> Result<(), String> {
        bootout(self.label);
        match self.plist_path() {
            Some(p) if p.exists() => fs::remove_file(p).map_err(|e| e.to_string()),
            _ => Ok(()),
        }
    }

    pub fn status(&self) -> AgentStatus {
        let installed = self.plist_path().is_some_and(|p| p.exists());
        let printed = launchctl(&["print", &self.target()]).ok();
        let field = |key: &str| {
            printed.as_deref().and_then(|out| {
                out.lines().map(str::trim).find_map(|l| l.strip_prefix(key).map(|v| v.trim().to_string()))
            })
        };
        let pid = field("pid = ").and_then(|v| v.parse().ok());
        AgentStatus { installed, running: pid.is_some(), pid, program: field("program = ") }
    }

    fn target(&self) -> String {
        format!("{}/{}", domain(), self.label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_escapes_program_path() {
        let p = MONITOR.plist_contents(Path::new("/Apps/A&B <x>.app/monitor"));
        assert!(p.contains("/Apps/A&amp;B &lt;x&gt;.app/monitor"));
        assert!(p.contains("<string>Background</string>"));
    }

    #[test]
    fn menu_bar_agent_runs_in_the_login_session() {
        let p = MENU_BAR.plist_contents(Path::new("/A.app/Contents/Library/LoginItems/M.app/Contents/MacOS/M"));
        assert!(p.contains("<string>com.maccleaner.menu</string>"));
        assert!(p.contains("<string>Interactive</string>") && p.contains("<string>Aqua</string>"));
        assert!(!p.contains("LowPriorityIO"));
        assert!(plist::Value::from_reader_xml(p.as_bytes()).is_ok());
        assert!(plist::Value::from_reader_xml(MONITOR.plist_contents(Path::new("/m")).as_bytes()).is_ok());
    }
}
