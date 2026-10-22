//! C API for the performance module (see `maccleaner_performance.h`).

use std::ffi::{c_char, c_int};

use maccleaner_core::performance;

use crate::{arg, respond};

#[unsafe(no_mangle)]
pub extern "C" fn mcl_launch_items() -> *mut c_char {
    respond(|| Ok(performance::launch_items()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_launch_item_set_enabled(label: *const c_char, enabled: c_int) -> *mut c_char {
    let raw = unsafe { arg(label) };
    respond(move || performance::set_enabled(&raw?, enabled != 0))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_launch_item_remove(plist_path: *const c_char, dry_run: c_int) -> *mut c_char {
    let raw = unsafe { arg(plist_path) };
    respond(move || performance::remove_launch_item(&raw?, dry_run != 0))
}

#[unsafe(no_mangle)]
pub extern "C" fn mcl_maintenance_tasks() -> *mut c_char {
    respond(|| Ok(performance::maintenance_tasks()))
}

/// `ids_json`: `["flush_dns",...]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_maintenance_run(ids_json: *const c_char) -> *mut c_char {
    let raw = unsafe { arg(ids_json) };
    respond(move || {
        let ids: Vec<String> = serde_json::from_str(&raw?).map_err(|e| e.to_string())?;
        Ok(performance::run_maintenance(&ids))
    })
}

/// `limit == 0` returns all accessible processes.
#[unsafe(no_mangle)]
pub extern "C" fn mcl_top_processes(limit: u32) -> *mut c_char {
    respond(move || Ok(performance::top_processes(limit as usize)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcl_string_free;
    use std::ffi::{CStr, CString};

    fn take(ptr: *mut c_char) -> serde_json::Value {
        let s = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
        unsafe { mcl_string_free(ptr) };
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn top_processes_envelope() {
        let v = take(mcl_top_processes(0));
        assert_eq!(v["ok"], true);
        let list = v["data"].as_array().unwrap();
        let me = std::process::id() as i64;
        let mine = list.iter().find(|p| p["pid"].as_i64() == Some(me)).expect("test process");
        assert!(mine["memory"].as_u64().unwrap() > 0);
        assert_eq!(mine["user_owned"], true);
        let v = take(mcl_top_processes(2));
        assert!(v["data"].as_array().unwrap().len() <= 2);
    }

    #[test]
    fn maintenance_tasks_and_bad_input() {
        let v = take(mcl_maintenance_tasks());
        assert_eq!(v["ok"], true);
        assert_eq!(v["data"][0]["id"], "flush_dns");
        assert!(v["data"][0]["requires_admin"].is_boolean());

        let bad = CString::new("not json").unwrap();
        let v = take(unsafe { mcl_maintenance_run(bad.as_ptr()) });
        assert_eq!(v["ok"], false);
        let unknown = CString::new(r#"["nope"]"#).unwrap();
        let v = take(unsafe { mcl_maintenance_run(unknown.as_ptr()) });
        assert_eq!(v["ok"], true);
        assert_eq!(v["data"][0]["ok"], false);
        assert!(v["data"][0]["error"].is_string());
    }

    #[test]
    fn launch_items_envelope() {
        let v = take(mcl_launch_items());
        assert_eq!(v["ok"], true);
        assert!(v["data"].is_array());
        let v = take(unsafe { mcl_launch_item_set_enabled(std::ptr::null(), 1) });
        assert_eq!(v["ok"], false);
    }

    #[test]
    fn remove_rejects_outside_user_agents() {
        let p = CString::new("/Library/LaunchDaemons/x.plist").unwrap();
        let v = take(unsafe { mcl_launch_item_remove(p.as_ptr(), 0) });
        assert_eq!(v["ok"], true);
        assert_eq!(v["data"]["failed"], 1);
        assert_eq!(v["data"]["outcomes"][0]["ok"], false);
    }
}
