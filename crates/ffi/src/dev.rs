//! C API for the development leftovers module.

use std::ffi::c_char;

use maccleaner_core::dev;

use crate::{arg, respond, respond_scan};

fn parse<T: serde::de::DeserializeOwned>(raw: Result<String, String>) -> Result<T, String> {
    serde_json::from_str(&raw?).map_err(|e| e.to_string())
}

/// `request_json`: `{"roots"?:[string]}` -> `ProjectsReport`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_dev_projects(request_json: *const c_char) -> *mut c_char {
    let raw = unsafe { arg(request_json) };
    respond_scan(move || dev::scan_projects(&parse(raw)?))
}

/// -> `ToolchainReport`.
#[unsafe(no_mangle)]
pub extern "C" fn mcl_dev_toolchains() -> *mut c_char {
    respond_scan(dev::scan_toolchains)
}

/// `request_json`: `{"paths":[string],"dry_run"?:bool}` -> `CleanReport`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_dev_clean(request_json: *const c_char) -> *mut c_char {
    let raw = unsafe { arg(request_json) };
    respond(move || dev::clean(&parse(raw)?))
}

/// `ids_json`: `["homebrew_cleanup","docker_prune"]` -> `[CommandResult]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_dev_run(ids_json: *const c_char) -> *mut c_char {
    let raw = unsafe { arg(ids_json) };
    respond(move || parse::<Vec<String>>(raw).map(|ids| dev::run_commands(&ids)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{CStr, CString};

    fn take(ptr: *mut c_char) -> serde_json::Value {
        let s = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
        unsafe { crate::mcl_string_free(ptr) };
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn clean_defaults_to_dry_run_and_rejects_bad_json() {
        let req = CString::new(r#"{"paths":[]}"#).unwrap();
        let v = take(unsafe { mcl_dev_clean(req.as_ptr()) });
        assert_eq!(v["data"]["dry_run"], true);
        let bad = CString::new("{").unwrap();
        assert_eq!(take(unsafe { mcl_dev_run(bad.as_ptr()) })["ok"], false);
    }
}
