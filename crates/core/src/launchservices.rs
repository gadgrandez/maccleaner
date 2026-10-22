//! Launch Services lookups: where the apps registered for a bundle id are on disk.

use std::ffi::c_void;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

type CFTypeRef = *const c_void;

const UTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(alloc: CFTypeRef, bytes: *const u8, len: isize, encoding: u32, external: u8) -> CFTypeRef;
    fn CFArrayGetCount(array: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
    fn CFURLGetFileSystemRepresentation(url: CFTypeRef, resolve: u8, buffer: *mut u8, len: isize) -> u8;
    fn CFRelease(cf: CFTypeRef);
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn LSCopyApplicationURLsForBundleIdentifier(bundle_id: CFTypeRef, error: *mut CFTypeRef) -> CFTypeRef;
}

/// Paths of the apps Launch Services knows for `bundle_id` (they may no longer exist).
pub fn app_paths(bundle_id: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    unsafe {
        let id = CFStringCreateWithBytes(std::ptr::null(), bundle_id.as_ptr(), bundle_id.len() as isize, UTF8, 0);
        if id.is_null() {
            return out;
        }
        let urls = LSCopyApplicationURLsForBundleIdentifier(id, std::ptr::null_mut());
        CFRelease(id);
        if urls.is_null() {
            return out;
        }
        let mut buf = vec![0u8; 4096];
        for i in 0..CFArrayGetCount(urls) {
            let url = CFArrayGetValueAtIndex(urls, i);
            if CFURLGetFileSystemRepresentation(url, 1, buf.as_mut_ptr(), buf.len() as isize) != 0 {
                let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
                out.push(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len])));
            }
        }
        CFRelease(urls);
    }
    out
}

/// An app with this bundle id still exists somewhere on disk.
pub fn app_exists(bundle_id: &str) -> bool {
    app_paths(bundle_id).iter().any(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_system_apps_only() {
        assert!(app_exists("com.apple.finder"));
        assert!(!app_exists("com.maccleaner.does-not-exist"));
        assert!(app_paths("").is_empty());
    }
}
