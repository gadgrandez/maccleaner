//! Disk walking with `getattrlistbulk`.
//!
//! The walk uses `openat` relative to the parent's descriptor: no full paths are built,
//! symlinks are not followed and mount points are not crossed. Memory per walk is a
//! fixed buffer plus the names of the pending subdirectories of the current level.
//!
//! The walk is generic: one [`Visitor`] per thread receives each file with its metadata and
//! decides which subdirectories to enter. Each directory carries a tag (`Visitor::Tag`) that
//! travels with the queued jobs, so the visitor knows what to attribute its findings to.

use crate::governor;
use std::ffi::{CStr, CString};
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Bytes allocated on disk (not the logical size).
    pub bytes: u64,
    pub files: u64,
    /// Entries that could not be read (permissions, TCC, etc.).
    pub errors: u64,
}

impl Usage {
    pub fn add(&mut self, other: Usage) {
        self.bytes += other.bytes;
        self.files += other.files;
        self.errors += other.errors;
    }
}

const ATTR_BIT_MAP_COUNT: u16 = 5;
const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_DEVID: u32 = 0x0000_0002;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
const ATTR_CMN_ACCTIME: u32 = 0x0000_1000;
const ATTR_CMN_FILEID: u32 = 0x0200_0000;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;
const VREG: u32 = 1;
const VDIR: u32 = 2;

const BUF_SIZE: usize = 128 * 1024;
const MAX_DEPTH: u32 = 512;
const MAX_THREADS: usize = 8;
/// Subdirectories handed to each idle thread at once.
const SHARE_PER_IDLE: usize = 4;
/// Above this nothing is handed off (long paths would fail with ENAMETOOLONG).
const MAX_SHARED_PATH: usize = 900;

#[repr(C)]
struct AttrList {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

unsafe extern "C" {
    fn getattrlistbulk(
        dirfd: libc::c_int,
        alist: *mut libc::c_void,
        attribute_buffer: *mut libc::c_void,
        buffer_size: libc::size_t,
        options: u64,
    ) -> libc::c_int;
}

/// Metadata requested while walking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Detail {
    /// Allocated size only (the cheapest).
    Basic,
    /// Also dates, inode and logical size.
    Full,
}

/// A non-directory (file, symlink, etc.) found during the walk.
/// With `Detail::Basic` the date, inode and logical size fields are 0.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FileMeta<'a> {
    pub name: &'a [u8],
    pub regular: bool,
    pub dev: i32,
    /// Bytes allocated on disk.
    pub alloc: u64,
    /// Logical size of the contents.
    pub size: u64,
    pub modified: u64,
    pub accessed: u64,
    pub ino: u64,
}

/// Receives what a walk thread finds. Each thread has its own.
pub(crate) trait Visitor {
    type Tag: Copy + Send;
    /// Non-directory inside `dir` (path of the directory being listed).
    fn file(&mut self, tag: Self::Tag, dir: &[u8], f: &FileMeta);
    /// Subdirectory `name` of `dir`, which is at depth `depth` from the root (0).
    /// Returns the child's tag, or `None` to skip it.
    fn dir(&mut self, tag: Self::Tag, dir: &[u8], name: &[u8], depth: u32) -> Option<Self::Tag>;
    /// Unreadable entry or directory.
    fn error(&mut self, tag: Self::Tag);
    /// End of a directory listing (before descending into its children).
    fn dir_done(&mut self, _tag: Self::Tag) {}
}

struct Entry<'a> {
    meta: FileMeta<'a>,
    is_dir: bool,
    error: bool,
}

fn rd_u32(buf: &[u8], at: usize) -> Option<u32> {
    buf.get(at..at + 4).map(|b| u32::from_ne_bytes(b.try_into().unwrap()))
}

fn rd_i32(buf: &[u8], at: usize) -> Option<i32> {
    rd_u32(buf, at).map(|v| v as i32)
}

fn rd_u64(buf: &[u8], at: usize) -> Option<u64> {
    buf.get(at..at + 8).map(|b| u64::from_ne_bytes(b.try_into().unwrap()))
}

/// Seconds of a `struct timespec` (16 bytes on arm64/x86_64); negatives become 0.
fn rd_secs(buf: &[u8], at: usize) -> u64 {
    rd_u64(buf, at).map(|v| (v as i64).max(0) as u64).unwrap_or(0)
}

fn attr_list(detail: Detail) -> AttrList {
    let mut common = ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_ERROR | ATTR_CMN_NAME | ATTR_CMN_DEVID | ATTR_CMN_OBJTYPE;
    let mut file = ATTR_FILE_ALLOCSIZE;
    if detail == Detail::Full {
        common |= ATTR_CMN_MODTIME | ATTR_CMN_ACCTIME | ATTR_CMN_FILEID;
        file |= ATTR_FILE_DATALENGTH;
    }
    AttrList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: common,
        volattr: 0,
        dirattr: 0,
        fileattr: file,
        forkattr: 0,
    }
}

/// Lists a directory in bulk. Returns `Err` only if the call itself fails.
///
/// Attributes arrive in bit order (except `RETURNED_ATTRS` and `ERROR`, which come
/// first): name, device, type, mtime, atime, inode; then allocated and logical size.
fn list_bulk(fd: RawFd, detail: Detail, buf: &mut [u8], mut f: impl FnMut(Entry)) -> io::Result<()> {
    let mut al = attr_list(detail);
    loop {
        let n = unsafe {
            getattrlistbulk(
                fd,
                (&mut al as *mut AttrList).cast(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                0,
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            return Ok(());
        }
        let buf: &[u8] = buf;
        let mut off = 0usize;
        for _ in 0..n {
            let Some(len) = rd_u32(buf, off) else { break };
            let base = off;
            off += len as usize;
            let mut p = base + 4;
            let (Some(common), Some(fileattr)) = (rd_u32(buf, p), rd_u32(buf, p + 12)) else {
                break;
            };
            p += 20; // attribute_set_t
            let mut e = Entry {
                meta: FileMeta { name: b"", regular: false, dev: 0, alloc: 0, size: 0, modified: 0, accessed: 0, ino: 0 },
                is_dir: false,
                error: false,
            };
            if common & ATTR_CMN_ERROR != 0 {
                e.error = rd_u32(buf, p).unwrap_or(1) != 0;
                p += 4;
            }
            if common & ATTR_CMN_NAME != 0 {
                if let (Some(rel), Some(nlen)) = (rd_i32(buf, p), rd_u32(buf, p + 4)) {
                    let start = (p as isize + rel as isize) as usize;
                    // The length includes the trailing NUL.
                    let end = start + (nlen as usize).saturating_sub(1);
                    e.meta.name = buf.get(start..end).unwrap_or(b"");
                }
                p += 8;
            }
            if common & ATTR_CMN_DEVID != 0 {
                e.meta.dev = rd_i32(buf, p).unwrap_or(0);
                p += 4;
            }
            if common & ATTR_CMN_OBJTYPE != 0 {
                let t = rd_u32(buf, p);
                e.is_dir = t == Some(VDIR);
                e.meta.regular = t == Some(VREG);
                p += 4;
            }
            if common & ATTR_CMN_MODTIME != 0 {
                e.meta.modified = rd_secs(buf, p);
                p += 16;
            }
            if common & ATTR_CMN_ACCTIME != 0 {
                e.meta.accessed = rd_secs(buf, p);
                p += 16;
            }
            if common & ATTR_CMN_FILEID != 0 {
                e.meta.ino = rd_u64(buf, p).unwrap_or(0);
                p += 8;
            }
            if fileattr & ATTR_FILE_ALLOCSIZE != 0 {
                e.meta.alloc = rd_u64(buf, p).unwrap_or(0);
                p += 8;
            }
            if fileattr & ATTR_FILE_DATALENGTH != 0 {
                e.meta.size = rd_u64(buf, p).unwrap_or(0);
            }
            f(e);
        }
    }
}

/// Fallback for file systems without `getattrlistbulk` (e.g. some network ones).
fn list_readdir(fd: RawFd, mut f: impl FnMut(Entry)) -> io::Result<()> {
    let dup = unsafe { libc::dup(fd) };
    if dup < 0 {
        return Err(io::Error::last_os_error());
    }
    let dir = unsafe { libc::fdopendir(dup) };
    if dir.is_null() {
        unsafe { libc::close(dup) };
        return Err(io::Error::last_os_error());
    }
    loop {
        let ent = unsafe { libc::readdir(dir) };
        if ent.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let mut meta = FileMeta { name: bytes, regular: false, dev: 0, alloc: 0, size: 0, modified: 0, accessed: 0, ino: 0 };
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::fstatat(fd, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
        if rc != 0 {
            f(Entry { meta, is_dir: false, error: true });
            continue;
        }
        let fmt = st.st_mode & libc::S_IFMT;
        meta.regular = fmt == libc::S_IFREG;
        meta.dev = st.st_dev;
        meta.alloc = st.st_blocks as u64 * 512;
        meta.size = st.st_size.max(0) as u64;
        meta.modified = st.st_mtime.max(0) as u64;
        meta.accessed = st.st_atime.max(0) as u64;
        meta.ino = st.st_ino;
        f(Entry { meta, is_dir: fmt == libc::S_IFDIR, error: false });
    }
    unsafe { libc::closedir(dir) };
    Ok(())
}

fn open_dir_at(parent: RawFd, name: &CStr) -> io::Result<OwnedFd> {
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

/// Directory pending a walk, with the tag the visitor assigned to it.
struct Job<T> {
    tag: T,
    dev: i32,
    depth: u32,
    path: Vec<u8>,
}

struct Queue<T> {
    jobs: Vec<Job<T>>,
    /// Queued plus in-progress jobs; the walk ends when this reaches 0.
    outstanding: usize,
}

/// Shared queue. Threads only hand off subdirectories when others are idle,
/// so the queue (and memory) stays small.
struct Pool<T> {
    queue: Mutex<Queue<T>>,
    cv: Condvar,
    idle: AtomicUsize,
}

impl<T> Pool<T> {
    fn push(&self, jobs: Vec<Job<T>>) {
        let n = jobs.len();
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.outstanding += n;
        q.jobs.extend(jobs);
        drop(q);
        self.cv.notify_all();
    }

    fn next(&self) -> Option<Job<T>> {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(job) = q.jobs.pop() {
                return Some(job);
            }
            if q.outstanding == 0 {
                return None;
            }
            self.idle.fetch_add(1, Ordering::Relaxed);
            q = self.cv.wait(q).unwrap_or_else(|e| e.into_inner());
            self.idle.fetch_sub(1, Ordering::Relaxed);
        }
    }

    fn done(&self) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.outstanding -= 1;
        if q.outstanding == 0 {
            drop(q);
            self.cv.notify_all();
        }
    }
}

struct Walker<'a, V: Visitor> {
    pool: &'a Pool<V::Tag>,
    detail: Detail,
    buf: Vec<u8>,
    /// Path of the current directory: handed to other threads and passed to the visitor.
    path: Vec<u8>,
    visitor: V,
    dev: i32,
    /// Directories read since the last memory-pressure check.
    since_poll: u32,
}

impl<V: Visitor> Walker<'_, V> {
    fn run(&mut self, job: Job<V::Tag>) {
        self.dev = job.dev;
        self.path = job.path;
        match CString::new(self.path.as_slice()).ok().and_then(|c| open_dir_at(libc::AT_FDCWD, &c).ok()) {
            Some(fd) => self.walk(&fd, job.depth, job.tag),
            None => self.visitor.error(job.tag),
        }
    }

    fn walk(&mut self, fd: &OwnedFd, depth: u32, tag: V::Tag) {
        self.since_poll += 1;
        if self.since_poll >= 64 {
            self.since_poll = 0;
            governor::poll();
        }
        if governor::aborted() {
            return;
        }
        let mut dirs: Vec<(CString, V::Tag)> = Vec::new();
        let dev = self.dev;
        let path = self.path.as_slice();
        let visitor = &mut self.visitor;
        let mut visit = |e: Entry| {
            if e.error {
                visitor.error(tag);
            } else if e.is_dir {
                // Do not cross into other volumes mounted inside the tree.
                if e.meta.dev == dev {
                    if let Some(child) = visitor.dir(tag, path, e.meta.name, depth) {
                        if let Ok(name) = CString::new(e.meta.name) {
                            dirs.push((name, child));
                        }
                    }
                }
            } else {
                visitor.file(tag, path, &e.meta);
            }
        };
        let failed = match list_bulk(fd.as_raw_fd(), self.detail, &mut self.buf, &mut visit) {
            Ok(()) => false,
            Err(err) => match err.raw_os_error() {
                Some(libc::ENOTSUP) | Some(libc::EINVAL) => list_readdir(fd.as_raw_fd(), &mut visit).is_err(),
                _ => true,
            },
        };
        if failed {
            visitor.error(tag);
        }
        visitor.dir_done(tag);
        if depth >= MAX_DEPTH {
            for (_, child) in dirs {
                self.visitor.error(child);
            }
            return;
        }

        let idle = self.pool.idle.load(Ordering::Relaxed);
        if idle > 0 && dirs.len() > 1 && self.path.len() < MAX_SHARED_PATH {
            let n = (dirs.len() - 1).min(idle * SHARE_PER_IDLE);
            let jobs = dirs
                .drain(dirs.len() - n..)
                .map(|(name, child)| Job {
                    tag: child,
                    dev: self.dev,
                    depth: depth + 1,
                    path: join(&self.path, name.as_bytes()),
                })
                .collect();
            self.pool.push(jobs);
        }

        for (name, child) in dirs {
            match open_dir_at(fd.as_raw_fd(), &name) {
                Ok(child_fd) => {
                    let len = self.path.len();
                    push_name(&mut self.path, name.as_bytes());
                    self.walk(&child_fd, depth + 1, child);
                    self.path.truncate(len);
                }
                Err(_) => self.visitor.error(child),
            }
        }
    }
}

fn push_name(path: &mut Vec<u8>, name: &[u8]) {
    if path.last() != Some(&b'/') {
        path.push(b'/');
    }
    path.extend_from_slice(name);
}

/// `base/name` without doubling the slash (the root `/` already has it).
pub(crate) fn join(base: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(base.len() + 1 + name.len());
    p.extend_from_slice(base);
    push_name(&mut p, name);
    p
}

/// Path without trailing slashes (except `/`).
pub(crate) fn trim_path(path: &Path) -> Vec<u8> {
    let mut b = path.as_os_str().as_bytes().to_vec();
    while b.len() > 1 && b.last() == Some(&b'/') {
        b.pop();
    }
    b
}

pub(crate) fn worker_count() -> usize {
    thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(MAX_THREADS)
}

/// Walks each root (file, symlink or directory) in parallel, without following links.
///
/// All threads share a queue: when one goes idle, the others hand it subdirectories,
/// so a single huge folder is also walked in parallel.
/// Returns the visitors of all threads so the caller can merge them.
pub(crate) fn walk<V, F>(roots: &[(PathBuf, V::Tag)], detail: Detail, make: F) -> Vec<V>
where
    V: Visitor + Send,
    F: Fn() -> V + Sync,
{
    let mut first = make();
    let mut jobs = Vec::new();
    for (path, tag) in roots {
        let tag = *tag;
        let bytes = trim_path(path);
        match fs::symlink_metadata(path) {
            Err(_) => first.error(tag),
            Ok(md) if md.is_dir() => jobs.push(Job { tag, dev: md.dev() as i32, depth: 0, path: bytes }),
            Ok(md) => {
                let (dir, name) = match bytes.iter().rposition(|&c| c == b'/') {
                    Some(i) => (&bytes[..i.max(1)], &bytes[i + 1..]),
                    None => (&b""[..], &bytes[..]),
                };
                let meta = FileMeta {
                    name,
                    regular: md.is_file(),
                    dev: md.dev() as i32,
                    alloc: md.blocks() * 512,
                    size: md.len(),
                    modified: md.mtime().max(0) as u64,
                    accessed: md.atime().max(0) as u64,
                    ino: md.ino(),
                };
                first.file(tag, dir, &meta);
            }
        }
    }
    let mut out = vec![first];
    if jobs.is_empty() {
        return out;
    }
    // Even with a single job, the other threads receive handed-off subdirectories.
    let threads = governor::thread_budget(worker_count());
    jobs.reverse(); // so `pop` takes the first elements first.
    let pool = Pool { queue: Mutex::new(Queue { outstanding: jobs.len(), jobs }), cv: Condvar::new(), idle: AtomicUsize::new(0) };
    thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    governor::lower_thread_priority();
                    let mut walker = Walker {
                        pool: &pool,
                        detail,
                        buf: vec![0u8; BUF_SIZE],
                        path: Vec::new(),
                        visitor: make(),
                        dev: 0,
                        since_poll: 0,
                    };
                    while let Some(job) = pool.next() {
                        walker.run(job);
                        pool.done();
                    }
                    walker.visitor
                })
            })
            .collect();
        for h in handles {
            match h.join() {
                Ok(v) => out.push(v),
                Err(p) => std::panic::resume_unwind(p),
            }
        }
    });
    out
}

/// Accumulates usage per root.
struct UsageVisitor(Vec<Usage>);

impl Visitor for UsageVisitor {
    type Tag = usize;

    fn file(&mut self, tag: usize, _dir: &[u8], f: &FileMeta) {
        let u = &mut self.0[tag];
        u.bytes += f.alloc;
        u.files += 1;
    }

    fn dir(&mut self, tag: usize, _dir: &[u8], _name: &[u8], _depth: u32) -> Option<usize> {
        Some(tag)
    }

    fn error(&mut self, tag: usize) {
        self.0[tag].errors += 1;
    }
}

/// Disk usage of each path (file, symlink or directory), without following links.
pub fn usages(paths: &[PathBuf]) -> Vec<Usage> {
    let roots: Vec<(PathBuf, usize)> = paths.iter().cloned().zip(0..).collect();
    let mut total = vec![Usage::default(); paths.len()];
    for v in walk(&roots, Detail::Basic, || UsageVisitor(vec![Usage::default(); paths.len()])) {
        for (t, u) in total.iter_mut().zip(v.0) {
            t.add(u);
        }
    }
    total
}

/// Disk usage of a single path (also walked in parallel).
pub fn usage(path: &Path) -> Usage {
    usages(&[path.to_path_buf()]).pop().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn std_usage(path: &Path) -> Usage {
        let md = fs::symlink_metadata(path).unwrap();
        if !md.is_dir() {
            return Usage { bytes: md.blocks() * 512, files: 1, errors: 0 };
        }
        let mut total = Usage::default();
        for e in fs::read_dir(path).unwrap() {
            total.add(std_usage(&e.unwrap().path()));
        }
        total
    }

    #[test]
    fn matches_std_walk() {
        let tmp = TempDir::new("usage");
        let root = tmp.path();
        fs::create_dir_all(root.join("a/b/c")).unwrap();
        fs::write(root.join("a/x.bin"), vec![1u8; 10_000]).unwrap();
        fs::write(root.join("a/b/y.bin"), vec![2u8; 200_000]).unwrap();
        fs::write(root.join("a/b/c/z.bin"), vec![3u8; 5]).unwrap();
        std::os::unix::fs::symlink("/usr", root.join("a/link")).unwrap();

        let got = usage(&root.join("a"));
        let want = std_usage(&root.join("a"));
        assert_eq!(got.files, 4);
        assert_eq!(got.errors, 0);
        assert_eq!(got.bytes, want.bytes);
        assert!(got.bytes >= 210_005);
    }

    #[test]
    fn full_detail_matches_metadata() {
        let tmp = TempDir::new("detail");
        let root = tmp.path();
        fs::write(root.join("a.bin"), vec![7u8; 123_457]).unwrap();
        fs::write(root.join("b.txt"), b"hey!").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        // Must be resolved with `getattrlistbulk`, without falling back to the slow path.
        let fd = open_dir_at(libc::AT_FDCWD, &CString::new(root.as_os_str().as_bytes()).unwrap()).unwrap();
        let mut buf = vec![0u8; BUF_SIZE];
        let mut seen = 0;
        list_bulk(fd.as_raw_fd(), Detail::Full, &mut buf, |e| {
            let md = fs::symlink_metadata(root.join(std::ffi::OsStr::from_bytes(e.meta.name))).unwrap();
            assert_eq!(e.is_dir, md.is_dir());
            if !e.is_dir {
                assert!(e.meta.regular);
                assert_eq!(e.meta.alloc, md.blocks() * 512);
                assert_eq!(e.meta.size, md.len());
                assert_eq!(e.meta.modified, md.mtime() as u64);
                assert_eq!(e.meta.accessed, md.atime() as u64);
                assert_eq!(e.meta.ino, md.ino());
            }
            seen += 1;
        })
        .unwrap();
        assert_eq!(seen, 3);
    }

    #[test]
    fn missing_path_is_error() {
        assert_eq!(usage(Path::new("/does/not/exist/maccleaner")).errors, 1);
    }

    #[test]
    fn single_huge_dir_matches_std_walk() {
        let tmp = TempDir::new("wide");
        let root = tmp.path().join("big");
        for i in 0..40 {
            let d = root.join(format!("d{i}/sub/deeper"));
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("f"), vec![0u8; 5000 + i]).unwrap();
            fs::write(root.join(format!("d{i}/g")), b"x").unwrap();
        }
        let want = std_usage(&root);
        for _ in 0..20 {
            let got = usage(&root);
            assert_eq!((got.bytes, got.files, got.errors), (want.bytes, want.files, 0));
        }
    }

    #[test]
    fn usages_keep_order_and_attribution() {
        let tmp = TempDir::new("multi");
        let a = tmp.path().join("a");
        fs::create_dir_all(a.join("x/y")).unwrap();
        fs::write(a.join("x/y/f"), vec![0u8; 100_000]).unwrap();
        let b = tmp.path().join("b.bin");
        fs::write(&b, vec![0u8; 10]).unwrap();
        let missing = tmp.path().join("no");
        let got = usages(&[b.clone(), missing, a.clone()]);
        assert_eq!(got[0], std_usage(&b));
        assert_eq!(got[1].errors, 1);
        assert_eq!(got[2], std_usage(&a));
    }
}
