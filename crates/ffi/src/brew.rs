//! C API for Homebrew packages.

use std::ffi::c_char;

use maccleaner_core::brew;
use serde::Deserialize;

use crate::{arg, respond, respond_scan};

#[derive(Deserialize)]
struct UninstallRequest {
    name: String,
    kind: brew::BrewKind,
}

/// -> `BrewReport`.
#[unsafe(no_mangle)]
pub extern "C" fn mcl_brew_packages() -> *mut c_char {
    respond_scan(brew::packages)
}

/// `request_json`: `{"name":string,"kind":"formula"|"cask"}` -> `true`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_brew_uninstall(request_json: *const c_char) -> *mut c_char {
    let raw = unsafe { arg(request_json) };
    respond(move || {
        let req: UninstallRequest = serde_json::from_str(&raw?).map_err(|e| e.to_string())?;
        brew::uninstall(&req.name, req.kind).map(|()| true)
    })
}

/// -> `true`.
#[unsafe(no_mangle)]
pub extern "C" fn mcl_brew_autoremove() -> *mut c_char {
    respond(|| brew::autoremove().map(|()| true))
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
    fn uninstall_rejects_options_and_bad_kinds() {
        let req = CString::new(r#"{"name":"--force","kind":"formula"}"#).unwrap();
        let v = take(unsafe { mcl_brew_uninstall(req.as_ptr()) });
        assert_eq!((v["ok"].as_bool(), v["error"].as_str()), (Some(false), Some("invalid package name")));
        let req = CString::new(r#"{"name":"x","kind":"tap"}"#).unwrap();
        assert_eq!(take(unsafe { mcl_brew_uninstall(req.as_ptr()) })["ok"], false);
    }
}
