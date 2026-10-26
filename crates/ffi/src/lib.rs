//! C API for the GUI. Every function returns a JSON
//! `{"ok":bool,"data":...,"error":...}` that is freed with `mcl_string_free`.

mod apps;
mod clutter;
mod dev;
mod performance;
mod privacy;

use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use maccleaner_core::{clean, governor, launchd, progress, rules, scan, space, state, system, trash};
use serde::Serialize;

#[derive(Serialize)]
struct Envelope<T> {
    ok: bool,
    data: Option<T>,
    error: Option<String>,
}

pub(crate) fn respond<T: Serialize>(f: impl FnOnce() -> Result<T, String>) -> *mut c_char {
    let result = catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Err("internal core error".into()));
    let envelope = match result {
        Ok(data) => Envelope { ok: true, data: Some(data), error: None },
        Err(error) => Envelope { ok: false, data: None, error: Some(error) },
    };
    let json = serde_json::to_string(&envelope)
        .unwrap_or_else(|e| format!(r#"{{"ok":false,"data":null,"error":"serialization: {e}"}}"#));
    CString::new(json).unwrap_or_default().into_raw()
}

/// Same as `respond`, for disk scans: does not start under critical memory pressure
/// and discards the result if the governor cut it short.
pub(crate) fn respond_scan<T: Serialize>(f: impl FnOnce() -> Result<T, String>) -> *mut c_char {
    respond(move || governor::guarded(f).and_then(|r| r))
}

pub(crate) unsafe fn arg(ptr: *const c_char) -> Result<String, String> {
    if ptr.is_null() {
        return Err("null argument".into());
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().map(str::to_owned).map_err(|_| "argument is not UTF-8".into())
}

pub(crate) fn rules_or_err() -> Result<Vec<rules::Rule>, String> {
    let r = rules::default_rules();
    if r.is_empty() { Err("HOME is not set".into()) } else { Ok(r) }
}

#[unsafe(no_mangle)]
pub extern "C" fn mcl_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

#[unsafe(no_mangle)]
pub extern "C" fn mcl_scan() -> *mut c_char {
    respond_scan(|| rules_or_err().map(|r| scan::scan(&r)))
}

/// `request_json`: `{"paths":[...],"dry_run":bool}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_clean(request_json: *const c_char) -> *mut c_char {
    let raw = unsafe { arg(request_json) };
    respond(move || {
        let req: clean::CleanRequest = serde_json::from_str(&raw?).map_err(|e| e.to_string())?;
        Ok(clean::clean(&rules_or_err()?, &req))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_space(path: *const c_char, top: u32) -> *mut c_char {
    let raw = unsafe { arg(path) };
    respond_scan(move || space::space(Path::new(&raw?), top.max(1) as usize).map_err(|e| e.to_string()))
}

/// -> `{"full_disk_access":bool}`.
#[unsafe(no_mangle)]
pub extern "C" fn mcl_access_status() -> *mut c_char {
    respond(|| Ok(maccleaner_core::access::status()))
}

/// The user's preference: cleanups delete for good instead of moving to the Trash (off until
/// the app sets it). -> the action now applied, `"trash"` or `"delete"`.
#[unsafe(no_mangle)]
pub extern "C" fn mcl_set_delete_permanently(on: i32) -> *mut c_char {
    trash::set_delete_permanently(on != 0);
    respond(|| Ok(trash::preferred_action()))
}

/// Progress of the cleanup in course (any module); polled while a cleanup call blocks.
#[unsafe(no_mangle)]
pub extern "C" fn mcl_clean_progress() -> *mut c_char {
    respond(|| Ok(progress::snapshot()))
}

#[unsafe(no_mangle)]
pub extern "C" fn mcl_system_status() -> *mut c_char {
    respond(|| Ok(system::status()))
}

#[unsafe(no_mangle)]
pub extern "C" fn mcl_monitor_status() -> *mut c_char {
    respond(|| Ok(launchd::MONITOR.status()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_monitor_install(program_path: *const c_char) -> *mut c_char {
    let raw = unsafe { arg(program_path) };
    respond(move || launchd::MONITOR.install(Path::new(&raw?)).map(|_| launchd::MONITOR.status()))
}

#[unsafe(no_mangle)]
pub extern "C" fn mcl_monitor_uninstall() -> *mut c_char {
    respond(|| launchd::MONITOR.uninstall().map(|_| launchd::MONITOR.status()))
}

#[derive(Serialize)]
struct MonitorInfo {
    state: Option<state::MonitorState>,
    config: state::MonitorConfig,
}

/// Starts the menu bar monitor (`program`: the helper's executable) at login and now; does
/// nothing if it already runs that program. -> AgentStatus
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_menu_bar_enable(program_path: *const c_char) -> *mut c_char {
    let raw = unsafe { arg(program_path) };
    respond(move || launchd::MENU_BAR.ensure(Path::new(&raw?)).map(|_| launchd::MENU_BAR.status()))
}

/// Stops the menu bar monitor and removes it from the login. -> AgentStatus
#[unsafe(no_mangle)]
pub extern "C" fn mcl_menu_bar_disable() -> *mut c_char {
    respond(|| launchd::MENU_BAR.uninstall().map(|_| launchd::MENU_BAR.status()))
}

#[unsafe(no_mangle)]
pub extern "C" fn mcl_monitor_info() -> *mut c_char {
    respond(|| {
        let read = |p: Option<std::path::PathBuf>| p.and_then(|p| std::fs::read(p).ok());
        let state = read(state::monitor_state_path()).and_then(|b| serde_json::from_slice(&b).ok());
        let config = read(state::monitor_config_path())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Ok(MonitorInfo { state, config })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take(ptr: *mut c_char) -> serde_json::Value {
        let s = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
        unsafe { mcl_string_free(ptr) };
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn clean_defaults_to_dry_run() {
        let req = CString::new(r#"{"paths":[]}"#).unwrap();
        let v = take(unsafe { mcl_clean(req.as_ptr()) });
        assert_eq!(v["ok"], true);
        assert_eq!(v["data"]["dry_run"], true);
    }

    #[test]
    fn bad_input_returns_error_envelope() {
        let req = CString::new("not json").unwrap();
        let v = take(unsafe { mcl_clean(req.as_ptr()) });
        assert_eq!(v["ok"], false);
        assert!(v["error"].is_string());
        let v = take(unsafe { mcl_space(std::ptr::null(), 5) });
        assert_eq!(v["ok"], false);
    }

    #[test]
    fn delete_preference_is_reported() {
        // Only the reply is checked: switching the preference would race with other tests.
        let v = take(mcl_set_delete_permanently(0));
        assert_eq!((v["ok"].as_bool(), v["data"].as_str()), (Some(true), Some("trash")));
    }

    #[test]
    fn system_status_is_ok() {
        let v = take(mcl_system_status());
        assert_eq!(v["ok"], true);
        assert!(v["data"]["mem_total"].as_u64().unwrap() > 0);
    }
}
