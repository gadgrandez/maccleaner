//! C API for My Clutter and Space Lens.

use std::ffi::c_char;
use std::path::Path;

use maccleaner_core::{clean, clutter};
use serde::de::DeserializeOwned;

use crate::{arg, respond, respond_scan};

fn parse<T: DeserializeOwned>(raw: Result<String, String>) -> Result<T, String> {
    serde_json::from_str(&raw?).map_err(|e| e.to_string())
}

/// `request_json`: `{roots?, min_bytes?, older_than_days?, limit?, include_library?}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_large_files(request_json: *const c_char) -> *mut c_char {
    let raw = unsafe { arg(request_json) };
    respond_scan(move || clutter::large_files(&parse(raw)?))
}

/// `request_json`: `{roots?, min_bytes?, limit?}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_duplicates(request_json: *const c_char) -> *mut c_char {
    let raw = unsafe { arg(request_json) };
    respond_scan(move || clutter::duplicates(&parse(raw)?))
}

/// `depth` is clamped to 1..=3 and `top` to a minimum of 1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_space_tree(path: *const c_char, depth: u32, top: u32) -> *mut c_char {
    let raw = unsafe { arg(path) };
    respond_scan(move || clutter::space_tree(Path::new(&raw?), depth, top as usize).map_err(|e| e.to_string()))
}

/// `request_json`: `{paths:[string], dry_run?:bool}`; `dry_run` defaults to true.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mcl_trash_paths(request_json: *const c_char) -> *mut c_char {
    let raw = unsafe { arg(request_json) };
    respond(move || clutter::trash_paths(&parse::<clean::CleanRequest>(raw)?))
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

    fn json(v: serde_json::Value) -> CString {
        CString::new(v.to_string()).unwrap()
    }

    struct Tmp(std::path::PathBuf);

    impl Tmp {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!("maccleaner-ffi-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(d.join("sub")).unwrap();
            std::fs::write(d.join("a.mov"), vec![1u8; 300_000]).unwrap();
            std::fs::write(d.join("sub/b.mov"), vec![1u8; 300_000]).unwrap();
            Self(d)
        }

        fn s(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn large_files_and_duplicates_follow_contract() {
        let t = Tmp::new("clutter");
        let req = json(serde_json::json!({ "roots": [t.s()], "min_bytes": 100_000 }));
        let v = take(unsafe { mcl_large_files(req.as_ptr()) });
        assert_eq!(v["ok"], true, "{v}");
        assert_eq!(v["data"]["files"].as_array().unwrap().len(), 2);
        assert_eq!(v["data"]["files"][0]["kind"], "video");
        assert!(v["data"]["files"][0]["modified"].is_u64());
        assert!(v["data"]["elapsed_ms"].is_u64());

        let v = take(unsafe { mcl_duplicates(req.as_ptr()) });
        assert_eq!(v["ok"], true, "{v}");
        assert_eq!(v["data"]["groups"][0]["paths"].as_array().unwrap().len(), 2);
        assert!(v["data"]["total_wasted"].as_u64().unwrap() >= 300_000);
        assert_eq!(v["data"]["scanned_files"], 2);
    }

    #[test]
    fn space_tree_clamps_and_reports_errors() {
        let t = Tmp::new("tree");
        let p = CString::new(t.s()).unwrap();
        let v = take(unsafe { mcl_space_tree(p.as_ptr(), 9, 0) });
        assert_eq!(v["ok"], true, "{v}");
        assert_eq!(v["data"]["is_dir"], true);
        assert_eq!(v["data"]["files"], 2);
        assert_eq!(v["data"]["children"].as_array().unwrap().len(), 1);
        assert!(v["data"]["others_bytes"].as_u64().unwrap() > 0);
        assert_eq!(take(unsafe { mcl_space_tree(std::ptr::null(), 2, 5) })["ok"], false);
        let missing = CString::new("/does/not/exist/maccleaner").unwrap();
        assert_eq!(take(unsafe { mcl_space_tree(missing.as_ptr(), 2, 5) })["ok"], false);
    }

    #[test]
    fn trash_paths_defaults_to_dry_run() {
        let req = json(serde_json::json!({ "paths": ["/etc/hosts"] }));
        let v = take(unsafe { mcl_trash_paths(req.as_ptr()) });
        assert_eq!(v["ok"], true, "{v}");
        assert_eq!(v["data"]["dry_run"], true);
        assert_eq!(v["data"]["failed"], 1);
        let bad = CString::new("not json").unwrap();
        assert_eq!(take(unsafe { mcl_trash_paths(bad.as_ptr()) })["ok"], false);
        assert_eq!(take(unsafe { mcl_large_files(std::ptr::null()) })["ok"], false);
    }
}
