//! C API for the apps module (contract in `docs/ffi-contracts.md`).

use std::ffi::c_char;
use std::path::Path;

use maccleaner_core::{apps, clean};

use crate::{arg, respond, respond_scan};

/// -> `[AppInfo]` sorted by name.
#[unsafe(no_mangle)]
pub extern "C" fn mcl_apps_list() -> *mut c_char {
    respond_scan(apps::list_apps)
}

/// -> `AppLeftovers`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_apps_leftovers(app_path: *const c_char) -> *mut c_char {
    let raw = unsafe { arg(app_path) };
    respond_scan(move || apps::leftovers(Path::new(&raw?)))
}

/// -> `[OrphanGroup]` by descending size.
#[unsafe(no_mangle)]
pub extern "C" fn mcl_apps_orphans() -> *mut c_char {
    respond_scan(apps::orphans)
}

/// `request_json`: `{"app_path"?:string,"leftover_paths":[...],"dry_run"?:bool}` -> `CleanReport`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_apps_uninstall(request_json: *const c_char) -> *mut c_char {
    let raw = unsafe { arg(request_json) };
    respond(move || {
        let req: apps::UninstallRequest = serde_json::from_str(&raw?).map_err(|e| e.to_string())?;
        apps::uninstall(&req)
    })
}

/// -> `[ExtensionItem]`.
#[unsafe(no_mangle)]
pub extern "C" fn mcl_extensions_list() -> *mut c_char {
    respond(apps::list_extensions)
}

/// `request_json`: `{"paths":[...],"dry_run"?:bool}` -> `CleanReport`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_extensions_remove(request_json: *const c_char) -> *mut c_char {
    let raw = unsafe { arg(request_json) };
    respond(move || {
        let req: clean::CleanRequest = serde_json::from_str(&raw?).map_err(|e| e.to_string())?;
        apps::remove_extensions(&req)
    })
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
    fn uninstall_defaults_to_dry_run() {
        let req = CString::new(r#"{"leftover_paths":[]}"#).unwrap();
        let v = take(unsafe { mcl_apps_uninstall(req.as_ptr()) });
        assert_eq!(v["ok"], true);
        assert_eq!(v["data"]["dry_run"], true);
        assert_eq!(v["data"]["failed"], 0);
    }

    #[test]
    fn uninstall_rejects_system_app() {
        let req = CString::new(r#"{"app_path":"/System/Applications/Calculator.app","leftover_paths":[],"dry_run":false}"#).unwrap();
        let v = take(unsafe { mcl_apps_uninstall(req.as_ptr()) });
        assert_eq!(v["ok"], true);
        assert_eq!(v["data"]["failed"], 1);
        assert_eq!(v["data"]["outcomes"][0]["ok"], false);
    }

    #[test]
    fn extensions_remove_defaults_to_dry_run() {
        let req = CString::new(r#"{"paths":[]}"#).unwrap();
        let v = take(unsafe { mcl_extensions_remove(req.as_ptr()) });
        assert_eq!(v["ok"], true);
        assert_eq!(v["data"]["dry_run"], true);
    }

    #[test]
    fn bad_input_returns_error_envelope() {
        let bad = CString::new("not json").unwrap();
        for v in [
            take(unsafe { mcl_apps_uninstall(bad.as_ptr()) }),
            take(unsafe { mcl_extensions_remove(bad.as_ptr()) }),
            take(unsafe { mcl_apps_leftovers(std::ptr::null()) }),
            take(unsafe { mcl_apps_leftovers(bad.as_ptr()) }),
        ] {
            assert_eq!(v["ok"], false);
            assert!(v["error"].is_string());
        }
    }
}
