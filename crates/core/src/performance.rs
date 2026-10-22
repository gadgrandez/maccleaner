//! Performance: startup items (launchd), maintenance tasks and the processes using the most memory.

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, c_int, c_void};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::clean::{CleanOutcome, CleanReport};
use crate::fsutil;
use crate::rules::{Action, home_dir};
use crate::trash;

// ---------------------------------------------------------------------------
// Startup items
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchScope {
    UserAgent,
    GlobalAgent,
    GlobalDaemon,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchItem {
    pub label: String,
    pub plist_path: String,
    pub program: Option<String>,
    pub scope: LaunchScope,
    pub run_at_load: bool,
    pub keep_alive: bool,
    pub enabled: bool,
    pub loaded: bool,
    pub pid: Option<i32>,
    /// Containing app inferred from `program`.
    pub app_name: Option<String>,
    pub removable: bool,
}

/// Plist directories; injectable for tests.
#[derive(Debug, Clone)]
pub struct LaunchRoots {
    pub user_agents: Option<PathBuf>,
    pub global_agents: PathBuf,
    pub global_daemons: PathBuf,
}

impl LaunchRoots {
    pub fn system() -> Self {
        Self {
            user_agents: user_agents_dir(),
            global_agents: PathBuf::from("/Library/LaunchAgents"),
            global_daemons: PathBuf::from("/Library/LaunchDaemons"),
        }
    }
}

/// launchd state queried only once per listing.
#[derive(Debug, Default, Clone)]
pub struct LaunchState {
    /// label -> disabled (gui/<uid> domain).
    pub user_disabled: HashMap<String, bool>,
    /// label -> disabled (system domain).
    pub system_disabled: HashMap<String, bool>,
    /// label -> pid (user domain, from `launchctl list`).
    pub user_loaded: HashMap<String, Option<i32>>,
    /// Root only: label -> pid in the system domain.
    pub system_loaded: Option<HashMap<String, Option<i32>>>,
}

fn user_agents_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join("Library/LaunchAgents"))
}

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

fn gui_domain() -> String {
    format!("gui/{}", uid())
}

fn launchctl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("/bin/launchctl").args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { format!("launchctl {} failed ({})", args.join(" "), out.status) } else { err })
    }
}

/// Parses `launchctl print-disabled`: `"label" => enabled|disabled` (or `true|false` in older versions,
/// where `true` means disabled).
pub fn parse_print_disabled(out: &str) -> HashMap<String, bool> {
    out.lines()
        .filter_map(|l| {
            let (label, state) = l.trim().split_once("=>")?;
            let label = label.trim().strip_prefix('"')?.strip_suffix('"')?;
            let disabled = match state.trim() {
                "disabled" | "true" => true,
                "enabled" | "false" => false,
                _ => return None,
            };
            Some((label.to_string(), disabled))
        })
        .collect()
}

/// Parses `launchctl list` (columns `PID Status Label`; PID `-` if not running).
pub fn parse_launchctl_list(out: &str) -> HashMap<String, Option<i32>> {
    out.lines()
        .filter_map(|l| {
            let mut cols = l.split('\t');
            let pid = cols.next()?.trim();
            let _status = cols.next()?;
            let label = cols.next()?.trim();
            if label.is_empty() || pid == "PID" {
                return None;
            }
            Some((label.to_string(), pid.parse::<i32>().ok().filter(|p| *p > 0)))
        })
        .collect()
}

pub fn query_state() -> LaunchState {
    let disabled = |domain: &str| launchctl(&["print-disabled", domain]).map(|o| parse_print_disabled(&o)).unwrap_or_default();
    let loaded = launchctl(&["list"]).map(|o| parse_launchctl_list(&o)).unwrap_or_default();
    let is_root = unsafe { libc::geteuid() } == 0;
    LaunchState {
        user_disabled: disabled(&gui_domain()),
        system_disabled: disabled("system"),
        // As root, `launchctl list` shows the system domain, not the user's.
        system_loaded: is_root.then(|| loaded.clone()),
        user_loaded: if is_root { HashMap::new() } else { loaded },
    }
}

/// Name (without extension) of the first `.app` containing `program`.
pub fn app_name_for(program: &str) -> Option<String> {
    Path::new(program).components().find_map(|c| match c {
        Component::Normal(name) => {
            let name = name.to_str()?;
            name.strip_suffix(".app").filter(|n| !n.is_empty()).map(str::to_string)
        }
        _ => None,
    })
}

struct PlistInfo {
    label: String,
    program: Option<String>,
    run_at_load: bool,
    keep_alive: bool,
    disabled_key: bool,
}

fn read_plist(path: &Path) -> Option<PlistInfo> {
    let value = plist::Value::from_file(path).ok()?;
    let dict = value.as_dictionary()?;
    let label = dict.get("Label")?.as_string()?.to_string();
    let program = dict.get("Program").and_then(|v| v.as_string()).map(str::to_string).or_else(|| {
        dict.get("ProgramArguments")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_string())
            .map(str::to_string)
    });
    let flag = |key: &str| dict.get(key).and_then(|v| v.as_boolean()).unwrap_or(false);
    let keep_alive = match dict.get("KeepAlive") {
        Some(plist::Value::Boolean(b)) => *b,
        Some(plist::Value::Dictionary(_)) => true,
        _ => false,
    };
    Some(PlistInfo { label, program, run_at_load: flag("RunAtLoad"), keep_alive, disabled_key: flag("Disabled") })
}

fn plist_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "plist")
                && !p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'))
        })
        .collect()
}

fn build_item(path: &Path, scope: LaunchScope, state: &LaunchState) -> Option<LaunchItem> {
    let info = read_plist(path)?;
    let (disabled_map, loaded_map) = match scope {
        LaunchScope::GlobalDaemon => (&state.system_disabled, state.system_loaded.as_ref()),
        _ => (&state.user_disabled, Some(&state.user_loaded)),
    };
    // launchd's overrides database wins; if there is no entry, the plist's `Disabled` key counts.
    let enabled = disabled_map.get(&info.label).map(|d| !d).unwrap_or(!info.disabled_key);
    let pid_entry = loaded_map.and_then(|m| m.get(&info.label));
    Some(LaunchItem {
        app_name: info.program.as_deref().and_then(app_name_for),
        plist_path: path.to_string_lossy().into_owned(),
        program: info.program,
        scope,
        run_at_load: info.run_at_load,
        keep_alive: info.keep_alive,
        enabled,
        loaded: pid_entry.is_some(),
        pid: pid_entry.copied().flatten(),
        removable: scope == LaunchScope::UserAgent,
        label: info.label,
    })
}

pub fn collect_launch_items(roots: &LaunchRoots, state: &LaunchState) -> Vec<LaunchItem> {
    let dirs = [
        (roots.user_agents.as_deref(), LaunchScope::UserAgent),
        (Some(roots.global_agents.as_path()), LaunchScope::GlobalAgent),
        (Some(roots.global_daemons.as_path()), LaunchScope::GlobalDaemon),
    ];
    let mut items: Vec<LaunchItem> = dirs
        .into_iter()
        .filter_map(|(dir, scope)| dir.map(|d| (d, scope)))
        .flat_map(|(dir, scope)| plist_files(dir).into_iter().filter_map(move |p| build_item(&p, scope, state)))
        .collect();
    items.sort_by(|a, b| a.scope.cmp(&b.scope).then_with(|| a.label.cmp(&b.label)).then_with(|| a.plist_path.cmp(&b.plist_path)));
    items
}

pub fn launch_items() -> Vec<LaunchItem> {
    collect_launch_items(&LaunchRoots::system(), &query_state())
}

/// Plist in `~/Library/LaunchAgents` whose `Label` matches.
fn find_user_agent(dir: &Path, label: &str) -> Option<PathBuf> {
    plist_files(dir).into_iter().find(|p| read_plist(p).is_some_and(|i| i.label == label))
}

pub fn set_enabled(label: &str, enabled: bool) -> Result<LaunchItem, String> {
    let dir = user_agents_dir().ok_or("HOME is not set")?;
    let plist = find_user_agent(&dir, label).ok_or_else(|| format!("{label} is not a user agent"))?;
    let target = format!("{}/{label}", gui_domain());
    if enabled {
        launchctl(&["enable", &target])?;
        if let Err(e) = launchctl(&["bootstrap", &gui_domain(), &plist.to_string_lossy()]) {
            // If it was already loaded, bootstrap fails but the final state is the requested one.
            if !launchctl(&["list"]).map(|o| parse_launchctl_list(&o).contains_key(label)).unwrap_or(false) {
                return Err(e);
            }
        }
    } else {
        let _ = launchctl(&["bootout", &target]);
        launchctl(&["disable", &target])?;
    }
    build_item(&plist, LaunchScope::UserAgent, &query_state()).ok_or_else(|| "could not re-read the plist".into())
}

/// Validates that `raw` is a `.plist` directly inside `agents_dir`.
fn validate_user_plist(agents_dir: &Path, raw: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(raw);
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return Err("invalid path".into());
    }
    if path.extension().is_none_or(|e| e != "plist") {
        return Err("not a plist".into());
    }
    let parent = path.parent().ok_or("invalid path")?;
    let canon_parent = fs::canonicalize(parent).map_err(|e| e.to_string())?;
    let canon_dir = fs::canonicalize(agents_dir).map_err(|e| e.to_string())?;
    if canon_parent != canon_dir {
        return Err("only agents in ~/Library/LaunchAgents can be removed".into());
    }
    let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        return Err("not a plist".into());
    }
    Ok(path)
}

fn remove_in(agents_dir: &Path, raw: &str, dry_run: bool) -> CleanReport {
    let category = Some("launch_agent".to_string());
    let (bytes, result) = match validate_user_plist(agents_dir, raw) {
        Err(e) => (0, Err(e)),
        Ok(path) => {
            let bytes = fsutil::usage(&path).bytes;
            let result = if dry_run {
                Ok(())
            } else {
                if let Some(info) = read_plist(&path) {
                    let _ = launchctl(&["bootout", &format!("{}/{}", gui_domain(), info.label)]);
                }
                trash::move_to_trash(&path)
            };
            (bytes, result)
        }
    };
    let ok = result.is_ok();
    CleanReport {
        dry_run,
        trashed_bytes: if ok { bytes } else { 0 },
        deleted_bytes: 0,
        failed: u64::from(!ok),
        outcomes: vec![CleanOutcome {
            path: raw.to_string(),
            category,
            action: Some(Action::Trash),
            bytes,
            ok,
            error: result.err(),
        }],
    }
}

pub fn remove_launch_item(plist_path: &str, dry_run: bool) -> Result<CleanReport, String> {
    let dir = user_agents_dir().ok_or("HOME is not set")?;
    Ok(remove_in(&dir, plist_path, dry_run))
}

// ---------------------------------------------------------------------------
// Maintenance
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenanceTask {
    pub id: String,
    pub name: String,
    pub description: String,
    pub requires_admin: bool,
    /// SF Symbol.
    pub icon: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResult {
    pub id: String,
    pub ok: bool,
    pub output: String,
    pub error: Option<String>,
}

impl TaskResult {
    fn fail(id: &str, error: impl Into<String>) -> Self {
        Self { id: id.to_string(), ok: false, output: String::new(), error: Some(error.into()) }
    }
}

struct TaskDef {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    requires_admin: bool,
    icon: &'static str,
    /// Fixed commands (absolute executable + arguments); never contain user input.
    cmds: &'static [&'static [&'static str]],
}

const LSREGISTER: &str =
    "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

const TASKS: &[TaskDef] = &[
    TaskDef {
        id: "flush_dns",
        name: "Flush DNS cache",
        description: "Clears the name resolution cache. Useful if some websites don't load or point to an old address.",
        requires_admin: true,
        icon: "network",
        cmds: &[&["/usr/bin/dscacheutil", "-flushcache"], &["/usr/bin/killall", "-HUP", "mDNSResponder"]],
    },
    TaskDef {
        id: "free_ram",
        name: "Free up RAM",
        description: "Purges disk caches held in inactive memory to leave more RAM free.",
        requires_admin: true,
        icon: "memorychip",
        cmds: &[&["/usr/sbin/purge"]],
    },
    TaskDef {
        id: "reindex_spotlight",
        name: "Reindex Spotlight",
        description: "Erases and rebuilds the Spotlight index of the startup disk. It may take a while to complete.",
        requires_admin: true,
        icon: "magnifyingglass",
        cmds: &[&["/usr/bin/mdutil", "-E", "/"]],
    },
    TaskDef {
        id: "thin_snapshots",
        name: "Thin Time Machine snapshots",
        description: "Removes local Time Machine snapshots to reclaim disk space.",
        requires_admin: true,
        icon: "clock.arrow.circlepath",
        cmds: &[&["/usr/bin/tmutil", "thinlocalsnapshots", "/", "999999999999", "4"]],
    },
    TaskDef {
        id: "rebuild_launch_services",
        name: "Rebuild Launch Services",
        description: "Regenerates the database that associates files with apps. Fixes duplicates in “Open With”.",
        requires_admin: false,
        icon: "arrow.triangle.2.circlepath",
        cmds: &[&[LSREGISTER, "-kill", "-r", "-domain", "local", "-domain", "user"]],
    },
    TaskDef {
        id: "clear_font_caches",
        name: "Clear font caches",
        description: "Removes the user's font caches. Fixes fonts that look wrong or don't appear.",
        requires_admin: false,
        icon: "textformat",
        cmds: &[&["/usr/bin/atsutil", "databases", "-removeUser"]],
    },
    TaskDef {
        id: "clear_quicklook",
        name: "Reset Quick Look",
        description: "Clears the Quick Look thumbnail cache and reloads its generators.",
        requires_admin: false,
        icon: "eye",
        cmds: &[&["/usr/bin/qlmanage", "-r", "cache"]],
    },
];

pub fn maintenance_tasks() -> Vec<MaintenanceTask> {
    TASKS
        .iter()
        .map(|t| MaintenanceTask {
            id: t.id.into(),
            name: t.name.into(),
            description: t.description.into(),
            requires_admin: t.requires_admin,
            icon: t.icon.into(),
        })
        .collect()
}

/// Execution plan: kept separate from execution so it can be tested without launching anything.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MaintenancePlan {
    /// Ids in the requested order (no duplicates).
    pub order: Vec<String>,
    /// Results already decided (unknown id, missing binary).
    pub failed: Vec<TaskResult>,
    /// User tasks: (id, commands).
    pub user: Vec<(String, Vec<Vec<String>>)>,
    /// Admin tasks, run together: (id, commands).
    pub admin: Vec<(String, Vec<Vec<String>>)>,
}

pub fn plan_maintenance(ids: &[String], exists: impl Fn(&Path) -> bool) -> MaintenancePlan {
    let mut plan = MaintenancePlan::default();
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(id.as_str()) {
            continue;
        }
        plan.order.push(id.clone());
        let Some(def) = TASKS.iter().find(|t| t.id == id) else {
            plan.failed.push(TaskResult::fail(id, format!("unknown task: {id}")));
            continue;
        };
        if let Some(missing) = def.cmds.iter().map(|c| c[0]).find(|bin| !exists(Path::new(bin))) {
            plan.failed.push(TaskResult::fail(id, format!("not available on this macOS: {missing} is missing")));
            continue;
        }
        let cmds = def.cmds.iter().map(|c| c.iter().map(|s| s.to_string()).collect()).collect();
        if def.requires_admin { &mut plan.admin } else { &mut plan.user }.push((id.clone(), cmds));
    }
    plan
}

/// Single-quoting for `sh` (commands are fixed, but that is not relied upon).
pub fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-@:=+,".contains(&b)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// AppleScript string literal.
pub fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', r"\\").replace('"', "\\\""))
}

const MARK_BEGIN: &str = "@@MCL_BEGIN:";
const MARK_END: &str = "@@MCL_END:";

/// `sh` script that runs all admin tasks and marks each one's output and exit code.
pub fn admin_script(tasks: &[(String, Vec<Vec<String>>)]) -> String {
    tasks
        .iter()
        .map(|(id, cmds)| {
            let body = cmds.iter().map(|c| c.iter().map(|a| sh_quote(a)).collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join(" && ");
            let id = sh_quote(id);
            format!("echo {MARK_BEGIN}{id}; ( {body} ) 2>&1; echo {MARK_END}{id}:$?")
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn admin_applescript(tasks: &[(String, Vec<Vec<String>>)]) -> String {
    format!(
        "do shell script {} with administrator privileges without altering line endings",
        applescript_string(&admin_script(tasks))
    )
}

/// Splits the admin script's marked output into one result per task.
pub fn parse_admin_output(ids: &[String], out: &str) -> Vec<TaskResult> {
    let mut found: HashMap<String, TaskResult> = HashMap::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    for line in out.lines() {
        if let Some(id) = line.strip_prefix(MARK_BEGIN) {
            current = Some((id.to_string(), Vec::new()));
        } else if let Some(rest) = line.strip_prefix(MARK_END) {
            let Some((id, code)) = rest.rsplit_once(':') else { continue };
            let Some((cur_id, lines)) = current.take() else { continue };
            if cur_id != id {
                continue;
            }
            let output = lines.join("\n").trim().to_string();
            let ok = code.trim() == "0";
            let error = (!ok).then(|| if output.is_empty() { format!("exit code {}", code.trim()) } else { output.clone() });
            found.insert(id.to_string(), TaskResult { id: id.to_string(), ok, output, error });
        } else if let Some((_, lines)) = current.as_mut() {
            lines.push(line);
        }
    }
    ids.iter().map(|id| found.remove(id).unwrap_or_else(|| TaskResult::fail(id, "no result"))).collect()
}

fn run_user_task(id: &str, cmds: &[Vec<String>]) -> TaskResult {
    let mut output = String::new();
    for cmd in cmds {
        let out = match Command::new(&cmd[0]).args(&cmd[1..]).output() {
            Ok(o) => o,
            Err(e) => return TaskResult { id: id.into(), ok: false, output, error: Some(e.to_string()) },
        };
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        output.push_str(text.trim());
        if !out.status.success() {
            let error = if text.trim().is_empty() { format!("exit code {}", out.status) } else { text.trim().to_string() };
            return TaskResult { id: id.into(), ok: false, output: output.trim().to_string(), error: Some(error) };
        }
        output.push('\n');
    }
    TaskResult { id: id.into(), ok: true, output: output.trim().to_string(), error: None }
}

/// Runs the admin tasks in a single `osascript` invocation (a single dialog).
fn run_admin_tasks(tasks: &[(String, Vec<Vec<String>>)]) -> Vec<TaskResult> {
    let ids: Vec<String> = tasks.iter().map(|(id, _)| id.clone()).collect();
    let out = match Command::new("/usr/bin/osascript").arg("-e").arg(admin_applescript(tasks)).output() {
        Ok(o) => o,
        Err(e) => return ids.iter().map(|id| TaskResult::fail(id, e.to_string())).collect(),
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let error = if stderr.contains("-128") { "Cancelled".to_string() } else { stderr };
        return ids.iter().map(|id| TaskResult::fail(id, error.clone())).collect();
    }
    parse_admin_output(&ids, &String::from_utf8_lossy(&out.stdout))
}

pub fn run_maintenance(ids: &[String]) -> Vec<TaskResult> {
    let plan = plan_maintenance(ids, |p| p.exists());
    let mut results: HashMap<String, TaskResult> = plan.failed.into_iter().map(|r| (r.id.clone(), r)).collect();
    if !plan.admin.is_empty() {
        results.extend(run_admin_tasks(&plan.admin).into_iter().map(|r| (r.id.clone(), r)));
    }
    for (id, cmds) in &plan.user {
        results.insert(id.clone(), run_user_task(id, cmds));
    }
    plan.order.iter().filter_map(|id| results.remove(id)).collect()
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: i32,
    pub name: String,
    pub path: Option<String>,
    /// `phys_footprint` in bytes.
    pub memory: u64,
    pub cpu_time_ns: u64,
    pub user_owned: bool,
}

/// `struct rusage_info_v2` from `<sys/resource.h>`.
#[repr(C)]
#[derive(Default)]
struct RusageInfoV2 {
    ri_uuid: [u8; 16],
    ri_user_time: u64,
    ri_system_time: u64,
    ri_pkg_idle_wkups: u64,
    ri_interrupt_wkups: u64,
    ri_pageins: u64,
    ri_wired_size: u64,
    ri_resident_size: u64,
    ri_phys_footprint: u64,
    ri_proc_start_abstime: u64,
    ri_proc_exit_abstime: u64,
    ri_child_user_time: u64,
    ri_child_system_time: u64,
    ri_child_pkg_idle_wkups: u64,
    ri_child_interrupt_wkups: u64,
    ri_child_pageins: u64,
    ri_child_elapsed_abstime: u64,
    ri_diskio_bytesread: u64,
    ri_diskio_byteswritten: u64,
}

/// `struct proc_bsdinfo` from `<sys/proc_info.h>`.
#[repr(C)]
#[derive(Default)]
struct ProcBsdInfo {
    pbi_flags: u32,
    pbi_status: u32,
    pbi_xstatus: u32,
    pbi_pid: u32,
    pbi_ppid: u32,
    pbi_uid: u32,
    pbi_gid: u32,
    pbi_ruid: u32,
    pbi_rgid: u32,
    pbi_svuid: u32,
    pbi_svgid: u32,
    rfu_1: u32,
    pbi_comm: [u8; 16],
    pbi_name: [u8; 32],
    pbi_nfiles: u32,
    pbi_pgid: u32,
    pbi_pjobc: u32,
    e_tdev: u32,
    e_tpgid: u32,
    pbi_nice: i32,
    pbi_start_tvsec: u64,
    pbi_start_tvusec: u64,
}

#[repr(C)]
#[derive(Default)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

const RUSAGE_INFO_V2: c_int = 2;
const PROC_PIDTBSDINFO: c_int = 3;
const PROC_PIDPATHINFO_MAXSIZE: usize = 4096;

unsafe extern "C" {
    fn proc_listallpids(buffer: *mut c_void, buffersize: c_int) -> c_int;
    fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut c_void) -> c_int;
    fn proc_pidinfo(pid: c_int, flavor: c_int, arg: u64, buffer: *mut c_void, buffersize: c_int) -> c_int;
    fn proc_name(pid: c_int, buffer: *mut c_void, buffersize: u32) -> c_int;
    fn proc_pidpath(pid: c_int, buffer: *mut c_void, buffersize: u32) -> c_int;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> c_int;
}

fn timebase() -> (u64, u64) {
    static TB: OnceLock<(u64, u64)> = OnceLock::new();
    *TB.get_or_init(|| {
        let mut tb = MachTimebaseInfo::default();
        if unsafe { mach_timebase_info(&mut tb) } == 0 && tb.denom != 0 { (tb.numer as u64, tb.denom as u64) } else { (1, 1) }
    })
}

fn mach_to_ns(ticks: u64) -> u64 {
    let (numer, denom) = timebase();
    (ticks as u128 * numer as u128 / denom as u128) as u64
}

fn all_pids() -> Vec<i32> {
    let n = unsafe { proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Vec::new();
    }
    // Headroom for new processes between the two calls.
    let mut pids = vec![0i32; n as usize + 64];
    let bytes = (pids.len() * size_of::<i32>()) as c_int;
    let got = unsafe { proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(got.max(0) as usize);
    pids
}

fn c_buf_to_string(buf: &[u8]) -> String {
    CStr::from_bytes_until_nul(buf).map(|c| c.to_string_lossy().into_owned()).unwrap_or_default()
}

fn process_info(pid: i32, my_uid: u32) -> Option<ProcessInfo> {
    let mut ru = RusageInfoV2::default();
    if unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V2, (&mut ru as *mut RusageInfoV2).cast()) } != 0 {
        return None;
    }
    let mut bsd = ProcBsdInfo::default();
    let size = size_of::<ProcBsdInfo>() as c_int;
    let got = unsafe { proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, (&mut bsd as *mut ProcBsdInfo).cast(), size) };
    if got != size {
        return None;
    }
    let mut path_buf = vec![0u8; PROC_PIDPATHINFO_MAXSIZE];
    let n = unsafe { proc_pidpath(pid, path_buf.as_mut_ptr().cast(), path_buf.len() as u32) };
    let path = (n > 0).then(|| String::from_utf8_lossy(&path_buf[..n as usize]).into_owned());
    let mut name_buf = [0u8; 256];
    let n = unsafe { proc_name(pid, name_buf.as_mut_ptr().cast(), name_buf.len() as u32) };
    let short = if n > 0 { String::from_utf8_lossy(&name_buf[..n as usize]).into_owned() } else { String::new() };
    let file_name = path.as_deref().and_then(|p| Path::new(p).file_name()).map(|f| f.to_string_lossy().into_owned());
    // `proc_name` truncates to 32 bytes: if it is a prefix of the executable's name, the full one is used.
    let name = match file_name {
        Some(f) if short.is_empty() || (f.len() > short.len() && f.starts_with(&short)) => f,
        _ if !short.is_empty() => short,
        _ => c_buf_to_string(&bsd.pbi_comm),
    };
    Some(ProcessInfo {
        pid,
        name,
        path,
        memory: ru.ri_phys_footprint,
        cpu_time_ns: mach_to_ns(ru.ri_user_time.saturating_add(ru.ri_system_time)),
        user_owned: bsd.pbi_uid == my_uid,
    })
}

/// Accessible processes sorted by descending memory; `limit == 0` returns all of them.
pub fn top_processes(limit: usize) -> Vec<ProcessInfo> {
    let my_uid = uid();
    let mut procs: Vec<ProcessInfo> = all_pids().into_iter().filter(|p| *p > 0).filter_map(|p| process_info(p, my_uid)).collect();
    procs.sort_by(|a, b| b.memory.cmp(&a.memory).then(a.pid.cmp(&b.pid)));
    if limit > 0 {
        procs.truncate(limit);
    }
    procs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn struct_sizes_match_kernel() {
        assert_eq!(size_of::<RusageInfoV2>(), 160);
        assert_eq!(std::mem::offset_of!(RusageInfoV2, ri_phys_footprint), 72);
        assert_eq!(size_of::<ProcBsdInfo>(), 136);
        assert_eq!(std::mem::offset_of!(ProcBsdInfo, pbi_uid), 20);
        assert_eq!(size_of::<MachTimebaseInfo>(), 8);
    }

    fn write_plist(dir: &Path, file: &str, body: &str) -> PathBuf {
        let p = dir.join(file);
        fs::write(
            &p,
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>{body}</dict></plist>"#
            ),
        )
        .unwrap();
        p
    }

    fn roots(tmp: &TempDir) -> LaunchRoots {
        let r = LaunchRoots {
            user_agents: Some(tmp.path().join("user")),
            global_agents: tmp.path().join("agents"),
            global_daemons: tmp.path().join("daemons"),
        };
        for d in [r.user_agents.as_ref().unwrap(), &r.global_agents, &r.global_daemons] {
            fs::create_dir_all(d).unwrap();
        }
        r
    }

    #[test]
    fn collects_items_from_injected_roots() {
        let tmp = TempDir::new("launch");
        let r = roots(&tmp);
        let user = r.user_agents.clone().unwrap();
        write_plist(
            &user,
            "com.b.helper.plist",
            "<key>Label</key><string>com.b.helper</string>\
             <key>ProgramArguments</key><array><string>/Applications/Foo.app/Contents/Library/LoginItems/H.app/Contents/MacOS/h</string><string>--x</string></array>\
             <key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>",
        );
        write_plist(&user, "com.a.tool.plist", "<key>Label</key><string>com.a.tool</string><key>Program</key><string>/usr/local/bin/tool</string><key>Disabled</key><true/>");
        fs::write(user.join("broken.plist"), b"not a plist").unwrap();
        write_plist(&user, "no-label.plist", "<key>Program</key><string>/x</string>");
        fs::write(user.join("notes.txt"), b"x").unwrap();
        write_plist(&r.global_agents, "g.plist", "<key>Label</key><string>com.g</string><key>KeepAlive</key><true/>");
        write_plist(&r.global_daemons, "d.plist", "<key>Label</key><string>com.d</string><key>Program</key><string>/Library/D/d</string>");

        let mut state = LaunchState::default();
        state.user_disabled.insert("com.b.helper".into(), true);
        state.user_loaded.insert("com.b.helper".into(), Some(42));
        state.user_loaded.insert("com.g".into(), None);
        state.user_loaded.insert("com.d".into(), Some(7)); // Must not apply to daemons.
        let items = collect_launch_items(&r, &state);

        let labels: Vec<_> = items.iter().map(|i| (i.scope, i.label.as_str())).collect();
        assert_eq!(
            labels,
            vec![
                (LaunchScope::UserAgent, "com.a.tool"),
                (LaunchScope::UserAgent, "com.b.helper"),
                (LaunchScope::GlobalAgent, "com.g"),
                (LaunchScope::GlobalDaemon, "com.d"),
            ]
        );
        let a = &items[0];
        assert!(!a.enabled && !a.loaded && a.pid.is_none() && a.removable && a.app_name.is_none());
        let b = &items[1];
        assert!(!b.enabled && b.loaded && b.run_at_load && b.keep_alive && b.removable);
        assert_eq!(b.pid, Some(42));
        assert_eq!(b.app_name.as_deref(), Some("Foo"));
        assert!(b.program.as_deref().unwrap().ends_with("/MacOS/h"));
        let g = &items[2];
        assert!(g.enabled && g.loaded && g.pid.is_none() && g.keep_alive && !g.removable && g.program.is_none());
        let d = &items[3];
        assert!(d.enabled && !d.loaded && d.pid.is_none() && !d.removable);
    }

    #[test]
    fn parses_launchctl_outputs() {
        let disabled = parse_print_disabled(
            "\tdisabled services = {\n\t\t\"com.x\" => enabled\n\t\t\"com.y\" => disabled\n\t\t\"com.z\" => true\n\t}\n\tlogin item associations = {\n\t}\n",
        );
        assert_eq!(disabled.get("com.x"), Some(&false));
        assert_eq!(disabled.get("com.y"), Some(&true));
        assert_eq!(disabled.get("com.z"), Some(&true));
        let list = parse_launchctl_list("PID\tStatus\tLabel\n-\t0\tcom.idle\n31554\t-9\tcom.run\n");
        assert_eq!(list.len(), 2);
        assert_eq!(list.get("com.idle"), Some(&None));
        assert_eq!(list.get("com.run"), Some(&Some(31554)));
    }

    #[test]
    fn app_name_from_program() {
        assert_eq!(app_name_for("/Applications/Setapp/Foo Bar.app/Contents/MacOS/x").as_deref(), Some("Foo Bar"));
        assert_eq!(app_name_for("/usr/local/bin/tool"), None);
        assert_eq!(app_name_for("/opt/.app/x"), None);
    }

    #[test]
    fn remove_validates_paths() {
        let tmp = TempDir::new("launch-rm");
        let r = roots(&tmp);
        let user = r.user_agents.clone().unwrap();
        let ok = write_plist(&user, "com.a.plist", "<key>Label</key><string>com.a</string>");
        let outside = write_plist(&r.global_agents, "com.g.plist", "<key>Label</key><string>com.g</string>");
        fs::create_dir_all(user.join("sub")).unwrap();
        let nested = write_plist(&user.join("sub"), "n.plist", "<key>Label</key><string>n</string>");
        fs::write(user.join("x.txt"), b"x").unwrap();

        for bad in [
            outside.to_string_lossy().into_owned(),
            nested.to_string_lossy().into_owned(),
            user.join("x.txt").to_string_lossy().into_owned(),
            user.join("sub/../com.a.plist").to_string_lossy().into_owned(),
            user.join("missing.plist").to_string_lossy().into_owned(),
            "com.a.plist".into(),
        ] {
            let rep = remove_in(&user, &bad, false);
            assert_eq!(rep.failed, 1, "{bad}");
            assert!(!rep.outcomes[0].ok && rep.outcomes[0].error.is_some());
        }
        assert!(outside.exists() && nested.exists() && ok.exists());

        let rep = remove_in(&user, &ok.to_string_lossy(), true);
        assert_eq!(rep.failed, 0, "{:?}", rep.outcomes);
        assert!(rep.dry_run && rep.trashed_bytes > 0);
        assert_eq!(rep.outcomes[0].category.as_deref(), Some("launch_agent"));
        assert!(ok.exists());
    }

    #[test]
    fn task_catalog_is_consistent() {
        let tasks = maintenance_tasks();
        assert_eq!(tasks.len(), 7);
        let admin: Vec<_> = tasks.iter().filter(|t| t.requires_admin).map(|t| t.id.as_str()).collect();
        assert_eq!(admin, ["flush_dns", "free_ram", "reindex_spotlight", "thin_snapshots"]);
        assert!(tasks.iter().all(|t| !t.name.is_empty() && !t.description.is_empty() && !t.icon.is_empty()));
        // All binaries exist on the development macOS.
        let plan = plan_maintenance(&tasks.iter().map(|t| t.id.clone()).collect::<Vec<_>>(), |p| p.exists());
        assert!(plan.failed.is_empty(), "{:?}", plan.failed);
    }

    #[test]
    fn plan_splits_and_validates() {
        let ids: Vec<String> =
            ["clear_quicklook", "flush_dns", "nope", "free_ram", "flush_dns", "clear_font_caches"].map(String::from).to_vec();
        let plan = plan_maintenance(&ids, |p| p != Path::new("/usr/bin/atsutil"));
        assert_eq!(plan.order, ["clear_quicklook", "flush_dns", "nope", "free_ram", "clear_font_caches"]);
        assert_eq!(plan.admin.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["flush_dns", "free_ram"]);
        assert_eq!(plan.user.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["clear_quicklook"]);
        assert_eq!(plan.failed.len(), 2);
        assert_eq!(plan.failed[0].id, "nope");
        assert!(plan.failed[0].error.as_deref().unwrap().contains("unknown"));
        assert_eq!(plan.failed[1].id, "clear_font_caches");
        assert!(plan.failed[1].error.as_deref().unwrap().contains("/usr/bin/atsutil"));
    }

    #[test]
    fn escaping() {
        assert_eq!(sh_quote("/usr/bin/tmutil"), "/usr/bin/tmutil");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's; rm -rf /"), r"'it'\''s; rm -rf /'");
        assert_eq!(sh_quote(""), "''");
        assert_eq!(applescript_string(r#"a "b" \c"#), r#""a \"b\" \\c""#);
    }

    #[test]
    fn admin_script_is_single_invocation() {
        let ids: Vec<String> = ["flush_dns", "thin_snapshots"].map(String::from).to_vec();
        let plan = plan_maintenance(&ids, |_| true);
        let script = admin_script(&plan.admin);
        assert_eq!(
            script,
            "echo @@MCL_BEGIN:flush_dns; ( /usr/bin/dscacheutil -flushcache && /usr/bin/killall -HUP mDNSResponder ) 2>&1; \
             echo @@MCL_END:flush_dns:$?; \
             echo @@MCL_BEGIN:thin_snapshots; ( /usr/bin/tmutil thinlocalsnapshots / 999999999999 4 ) 2>&1; \
             echo @@MCL_END:thin_snapshots:$?"
        );
        let apple = admin_applescript(&plan.admin);
        assert!(apple.starts_with("do shell script \"echo @@MCL_BEGIN:flush_dns;"));
        assert!(apple.ends_with("\" with administrator privileges without altering line endings"));
        assert_eq!(apple.matches("do shell script").count(), 1);
    }

    #[test]
    fn parses_admin_output() {
        let ids: Vec<String> = ["flush_dns", "free_ram", "thin_snapshots"].map(String::from).to_vec();
        let out = "@@MCL_BEGIN:flush_dns\n@@MCL_END:flush_dns:0\n@@MCL_BEGIN:free_ram\nUnable to purge\n@@MCL_END:free_ram:1\n";
        let r = parse_admin_output(&ids, out);
        assert_eq!(r[0], TaskResult { id: "flush_dns".into(), ok: true, output: String::new(), error: None });
        assert!(!r[1].ok);
        assert_eq!(r[1].output, "Unable to purge");
        assert_eq!(r[1].error.as_deref(), Some("Unable to purge"));
        assert_eq!(r[2].error.as_deref(), Some("no result"));
    }

    #[test]
    fn run_rejects_unknown_without_executing() {
        let r = run_maintenance(&["nope".to_string()]);
        assert_eq!(r.len(), 1);
        assert!(!r[0].ok);
    }

    #[test]
    fn top_processes_includes_self() {
        let me = std::process::id() as i32;
        let all = top_processes(0);
        let mine = all.iter().find(|p| p.pid == me).expect("the test process must appear");
        assert!(mine.memory > 0 && mine.user_owned && !mine.name.is_empty());
        assert!(mine.path.is_some());
        assert!(all.windows(2).all(|w| w[0].memory >= w[1].memory));
        assert_eq!(top_processes(3).len(), 3.min(all.len()));
    }

    #[test]
    fn mach_conversion_is_sane() {
        let (n, d) = timebase();
        assert!(n > 0 && d > 0);
        assert_eq!(mach_to_ns(0), 0);
    }
}
