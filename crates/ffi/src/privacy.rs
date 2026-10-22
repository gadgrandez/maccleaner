//! Privacy C API (`maccleaner_privacy.h`).

use std::ffi::c_char;

use maccleaner_core::privacy;

use crate::{arg, respond, respond_scan};

/// `-> PrivacyReport`.
#[unsafe(no_mangle)]
pub extern "C" fn mcl_privacy_scan() -> *mut c_char {
    respond_scan(privacy::scan)
}

/// `request_json`: `{"items":[{"source":..,"item":..}],"dry_run":bool}` -> `CleanReport`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_privacy_clean(request_json: *const c_char) -> *mut c_char {
    let raw = unsafe { arg(request_json) };
    respond(move || {
        let req: privacy::PrivacyCleanRequest = serde_json::from_str(&raw?).map_err(|e| e.to_string())?;
        privacy::clean(&req)
    })
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
    fn privacy_clean_defaults_to_dry_run() {
        let req = CString::new(r#"{"items":[{"source":"nothing","item":"history"}]}"#).unwrap();
        let v = take(unsafe { mcl_privacy_clean(req.as_ptr()) });
        assert_eq!(v["ok"], true);
        assert_eq!(v["data"]["dry_run"], true);
        assert_eq!(v["data"]["failed"], 1);
    }

    #[test]
    fn privacy_clean_rejects_bad_input() {
        let v = take(unsafe { mcl_privacy_clean(std::ptr::null()) });
        assert_eq!(v["ok"], false);
        let req = CString::new(r#"{"paths":["/etc"]}"#).unwrap();
        let v = take(unsafe { mcl_privacy_clean(req.as_ptr()) });
        assert_eq!(v["ok"], false);
    }

    #[test]
    fn privacy_scan_has_system_source() {
        let v = take(mcl_privacy_scan());
        assert_eq!(v["ok"], true, "{v}");
        let sources = v["data"]["sources"].as_array().unwrap();
        let system = sources.iter().find(|s| s["id"] == "system").unwrap();
        assert_eq!(system["items"][0]["id"], "recent_items");
        assert!(system["items"][0]["accessible"].is_boolean());
    }
}
