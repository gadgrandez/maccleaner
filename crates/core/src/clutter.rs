//! My Clutter and Space Lens: large or old files, duplicates, the space tree for the
//! sunburst, and moving user-chosen paths to the Trash.
//!
//! Everything uses the parallel walk from `fsutil`. Paths are only built for files that
//! pass the size filter, so memory does not grow with the number of files walked.

use crate::governor;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use twox_hash::XxHash3_128;

use crate::clean::{CleanOutcome, CleanReport, CleanRequest};
use crate::fsutil::{self, Detail, FileMeta, Visitor};
use crate::rules;
use crate::{progress, trash};

const DAY: u64 = 86_400;
const LARGE_MIN: u64 = 50_000_000;
const LARGE_LIMIT: u32 = 500;
const DUP_MIN: u64 = 1_000_000;
const DUP_LIMIT: u32 = 300;
/// Leading bytes compared before reading whole files.
const HEAD: u64 = 64 * 1024;
/// Fixed per-thread buffer for the full hash.
const READ_BUF: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Video,
    Image,
    Audio,
    Archive,
    DiskImage,
    Document,
    App,
    Code,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub path: String,
    pub bytes: u64,
    pub modified: u64,
    pub accessed: u64,
    pub kind: FileKind,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LargeFilesRequest {
    pub roots: Option<Vec<String>>,
    pub min_bytes: Option<u64>,
    pub older_than_days: Option<u32>,
    pub limit: Option<u32>,
    pub include_library: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LargeFilesReport {
    pub files: Vec<FileInfo>,
    pub scanned_files: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DuplicatesRequest {
    pub roots: Option<Vec<String>>,
    pub min_bytes: Option<u64>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DupGroup {
    /// Allocated size of each copy (the largest if they differ, e.g. due to compression).
    pub bytes: u64,
    pub paths: Vec<String>,
    /// What is freed by keeping the largest copy.
    pub wasted_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicatesReport {
    pub groups: Vec<DupGroup>,
    pub total_wasted: u64,
    pub scanned_files: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceNode {
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub files: u64,
    pub is_dir: bool,
    /// The `top` largest children, from largest to smallest (empty at the last level).
    pub children: Vec<SpaceNode>,
    /// What does not fit in `children`: `bytes == Σ children.bytes + others_bytes` except at leaves.
    pub others_bytes: u64,
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn elapsed_ms(t: Instant) -> u64 {
    t.elapsed().as_millis() as u64
}

pub fn kind_of(name: &[u8]) -> FileKind {
    let ext = match name.iter().rposition(|&c| c == b'.') {
        Some(i) if i > 0 && name.len() - i <= 13 => name[i + 1..].to_ascii_lowercase(),
        _ => return FileKind::Other,
    };
    match ext.as_slice() {
        b"mp4" | b"mov" | b"m4v" | b"mkv" | b"avi" | b"wmv" | b"flv" | b"webm" | b"mpg" | b"mpeg" | b"3gp"
        | b"mts" | b"m2ts" | b"vob" | b"ogv" | b"braw" | b"r3d" => FileKind::Video,
        b"jpg" | b"jpeg" | b"png" | b"gif" | b"heic" | b"heif" | b"tif" | b"tiff" | b"bmp" | b"webp"
        | b"avif" | b"jxl" | b"raw" | b"dng" | b"cr2" | b"cr3" | b"nef" | b"arw" | b"orf" | b"rw2"
        | b"raf" | b"psd" | b"psb" | b"exr" | b"svg" | b"icns" => FileKind::Image,
        b"mp3" | b"m4a" | b"m4b" | b"aac" | b"wav" | b"aif" | b"aiff" | b"flac" | b"ogg" | b"opus"
        | b"wma" | b"caf" | b"alac" | b"mid" | b"midi" => FileKind::Audio,
        b"zip" | b"tar" | b"gz" | b"tgz" | b"bz2" | b"tbz" | b"xz" | b"txz" | b"zst" | b"7z" | b"rar"
        | b"lz" | b"lzma" | b"lz4" | b"cab" | b"xar" | b"cpio" | b"sit" | b"sitx" => FileKind::Archive,
        b"dmg" | b"iso" | b"sparseimage" | b"img" | b"toast" | b"cdr" | b"vmdk" | b"vdi" | b"qcow2"
        | b"vhd" | b"vhdx" => FileKind::DiskImage,
        b"pdf" | b"doc" | b"docx" | b"xls" | b"xlsx" | b"ppt" | b"pptx" | b"pages" | b"numbers"
        | b"key" | b"odt" | b"ods" | b"odp" | b"rtf" | b"txt" | b"md" | b"csv" | b"epub" | b"mobi" => {
            FileKind::Document
        }
        b"pkg" | b"mpkg" | b"ipa" | b"xip" | b"apk" | b"aab" | b"exe" | b"msi" | b"deb" | b"rpm"
        | b"appimage" => FileKind::App,
        b"rs" | b"c" | b"h" | b"cc" | b"cpp" | b"hpp" | b"m" | b"mm" | b"swift" | b"py" | b"js"
        | b"mjs" | b"ts" | b"tsx" | b"jsx" | b"java" | b"kt" | b"go" | b"rb" | b"php" | b"cs" | b"json"
        | b"xml" | b"yaml" | b"yml" | b"toml" | b"sql" | b"sh" | b"o" | b"a" | b"dylib" | b"so"
        | b"wasm" | b"jar" | b"class" | b"ipynb" => FileKind::Code,
        _ => FileKind::Other,
    }
}

/// `path` is `base` or is inside it.
fn within(path: &[u8], base: &[u8]) -> bool {
    path.starts_with(base) && (path.len() == base.len() || base == b"/" || path[base.len()] == b'/')
}

/// Requested roots (or `home`), without trailing slashes and without those contained in another.
fn resolve_roots(roots: Option<&[String]>, home: &Path) -> Vec<PathBuf> {
    let mut all: Vec<Vec<u8>> = match roots {
        Some(r) if !r.is_empty() => r.iter().map(|s| fsutil::trim_path(Path::new(s))).collect(),
        _ => vec![fsutil::trim_path(home)],
    };
    all.sort();
    all.dedup();
    let mut out: Vec<Vec<u8>> = Vec::new();
    for r in all {
        if !out.iter().any(|o| within(&r, o)) {
            out.push(r);
        }
    }
    out.into_iter().map(|b| PathBuf::from(OsStr::from_bytes(&b))).collect()
}

/// Which subdirectories are explored when looking for user clutter.
struct Scope {
    /// `$HOME` when `~/Library` must be skipped.
    skip_library_in: Option<Vec<u8>>,
    home: Vec<u8>,
    /// With Full Disk Access, Desktop, Documents and Downloads are entered without dialogs.
    full_disk_access: bool,
}

impl Scope {
    fn new(home: &Path, include_library: bool) -> Self {
        Self {
            skip_library_in: (!include_library).then(|| fsutil::trim_path(home)),
            home: fsutil::trim_path(home),
            full_disk_access: crate::access::full_disk_access(),
        }
    }

    fn enter(&self, dir: &[u8], name: &[u8]) -> bool {
        // Hidden ones (`.git`, `.Trash`, caches…) and packages: their inner files are not the user's.
        if name.first() == Some(&b'.') || is_package(name) {
            return false;
        }
        if crate::access::skip_protected(&self.home, dir, name, self.full_disk_access) {
            return false;
        }
        !(name == b"Library" && self.skip_library_in.as_deref() == Some(dir))
    }
}

// ---------------------------------------------------------------------------------------------
// Large and old files

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Found {
    bytes: u64,
    path: Vec<u8>,
    modified: u64,
    accessed: u64,
}

struct LargeVisitor<'a> {
    scope: &'a Scope,
    min: u64,
    /// Latest activity allowed (`u64::MAX` for no date filter).
    cutoff: u64,
    limit: usize,
    scanned: u64,
    /// Min-heap bounded to `limit`.
    heap: BinaryHeap<Reverse<Found>>,
}

impl Visitor for LargeVisitor<'_> {
    type Tag = ();

    fn file(&mut self, _: (), dir: &[u8], f: &FileMeta) {
        self.scanned += 1;
        if !f.regular || f.alloc < self.min || f.modified.max(f.accessed) > self.cutoff {
            return;
        }
        if self.heap.len() >= self.limit && self.heap.peek().is_some_and(|Reverse(m)| m.bytes >= f.alloc) {
            return;
        }
        let found = Found { bytes: f.alloc, path: fsutil::join(dir, f.name), modified: f.modified, accessed: f.accessed };
        self.heap.push(Reverse(found));
        if self.heap.len() > self.limit {
            self.heap.pop();
        }
    }

    fn dir(&mut self, _: (), dir: &[u8], name: &[u8], _depth: u32) -> Option<()> {
        self.scope.enter(dir, name).then_some(())
    }

    fn error(&mut self, _: ()) {}
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Files of at least `min_bytes` (allocated) under the roots, from largest to smallest.
pub fn large_files_in(home: &Path, req: &LargeFilesRequest) -> LargeFilesReport {
    let started = Instant::now();
    let scope = Scope::new(home, req.include_library);
    let roots: Vec<(PathBuf, ())> = resolve_roots(req.roots.as_deref(), home).into_iter().map(|p| (p, ())).collect();
    let limit = req.limit.unwrap_or(LARGE_LIMIT).max(1) as usize;
    let min = req.min_bytes.unwrap_or(LARGE_MIN);
    let cutoff = req.older_than_days.map_or(u64::MAX, |d| now_secs().saturating_sub(d as u64 * DAY));
    let visitors = fsutil::walk(&roots, Detail::Full, || LargeVisitor {
        scope: &scope,
        min,
        cutoff,
        limit,
        scanned: 0,
        heap: BinaryHeap::new(),
    });
    let mut scanned = 0;
    let mut found: Vec<Found> = Vec::new();
    for v in visitors {
        scanned += v.scanned;
        found.extend(v.heap.into_iter().map(|Reverse(f)| f));
    }
    found.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    found.truncate(limit);
    let files = found
        .into_iter()
        .map(|f| {
            let name = f.path.rsplit(|&c| c == b'/').next().unwrap_or(&[]);
            FileInfo { kind: kind_of(name), path: lossy(&f.path), bytes: f.bytes, modified: f.modified, accessed: f.accessed }
        })
        .collect();
    LargeFilesReport { files, scanned_files: scanned, elapsed_ms: elapsed_ms(started) }
}

pub fn large_files(req: &LargeFilesRequest) -> Result<LargeFilesReport, String> {
    let home = rules::home_dir().ok_or("HOME is not set")?;
    Ok(large_files_in(&home, req))
}

// ---------------------------------------------------------------------------------------------
// Duplicates
//
// Same logical size → hash of the first 64 KB → hash of the rest, streamed and only for the
// candidates still tied. Hardlinks (same device and inode) are the same file and count once.
// APFS clones (`cp -c`, Finder copies) share blocks and free no space when deleted, but
// `getattrlistbulk` cannot tell them apart from real copies: they are reported as
// duplicates.

struct Cand {
    size: u64,
    alloc: u64,
    ino: u64,
    dev: i32,
    /// Path in the arena: `arena[off..off + len]`.
    off: usize,
    len: usize,
}

struct DupVisitor<'a> {
    scope: &'a Scope,
    min: u64,
    scanned: u64,
    cands: Vec<Cand>,
    /// Candidate paths, contiguous, to avoid one allocation per file.
    arena: Vec<u8>,
}

impl Visitor for DupVisitor<'_> {
    type Tag = ();

    fn file(&mut self, _: (), dir: &[u8], f: &FileMeta) {
        self.scanned += 1;
        if !f.regular || f.alloc < self.min || f.size == 0 {
            return;
        }
        let off = self.arena.len();
        self.arena.extend_from_slice(dir);
        if dir.last() != Some(&b'/') {
            self.arena.push(b'/');
        }
        self.arena.extend_from_slice(f.name);
        self.cands.push(Cand { size: f.size, alloc: f.alloc, ino: f.ino, dev: f.dev, off, len: self.arena.len() - off });
    }

    fn dir(&mut self, _: (), dir: &[u8], name: &[u8], _depth: u32) -> Option<()> {
        self.scope.enter(dir, name).then_some(())
    }

    fn error(&mut self, _: ()) {}
}

/// Applies `f` to `0..n` in parallel with bounded threads and a fixed per-thread buffer.
/// If the governor aborts the work, unprocessed positions stay at `R::default()`
/// (no hash), so results are never paired with the wrong file.
fn par_map<R: Send + Default>(n: usize, buf_size: usize, f: impl Fn(usize, &mut [u8]) -> R + Sync) -> Vec<R> {
    if n == 0 {
        return Vec::new();
    }
    let next = AtomicUsize::new(0);
    let threads = governor::thread_budget(fsutil::worker_count()).min(n);
    let mut parts: Vec<(usize, R)> = thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    governor::lower_thread_priority();
                    let mut buf = vec![0u8; buf_size];
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= n || (i % 32 == 0 && governor::poll()) {
                            return out;
                        }
                        out.push((i, f(i, &mut buf)));
                    }
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p))).collect()
    });
    let mut out: Vec<R> = (0..n).map(|_| R::default()).collect();
    for (i, r) in parts.drain(..) {
        out[i] = r;
    }
    out
}

fn open_nofollow(path: &Path) -> io::Result<File> {
    fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)
}

/// Reads until `buf` is full or the end is reached.
fn read_full(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match f.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(got)
}

/// Hash of the first `HEAD` bytes; `None` if it cannot be read or the size changed.
fn hash_head(path: &Path, size: u64, buf: &mut [u8]) -> Option<u128> {
    let want = size.min(HEAD) as usize;
    let mut f = open_nofollow(path).ok()?;
    let got = read_full(&mut f, &mut buf[..want]).ok()?;
    (got == want).then(|| XxHash3_128::oneshot(&buf[..got]))
}

/// Hash of the rest of the file (from `HEAD`), streamed without going through the cache.
fn hash_tail(path: &Path, size: u64, buf: &mut [u8]) -> Option<u128> {
    let mut f = open_nofollow(path).ok()?;
    // Reading gigabytes of duplicates must not evict what the user is using from the cache.
    unsafe { libc::fcntl(f.as_raw_fd(), libc::F_NOCACHE, 1) };
    f.seek(SeekFrom::Start(HEAD)).ok()?;
    let mut h = XxHash3_128::new();
    let mut total = 0u64;
    loop {
        let n = read_full(&mut f, buf).ok()?;
        if n == 0 {
            break;
        }
        h.write(&buf[..n]);
        total += n as u64;
        if total > size - HEAD {
            return None;
        }
    }
    (total == size - HEAD).then(|| h.finish_128())
}

/// Splits `idx` into groups of 2 or more with the same key.
fn split_by<K: Ord + Copy>(mut idx: Vec<usize>, key: impl Fn(usize) -> Option<K>) -> Vec<Vec<usize>> {
    idx.retain(|&i| key(i).is_some());
    idx.sort_by_key(|&i| key(i));
    let mut out = Vec::new();
    for chunk in idx.chunk_by(|&a, &b| key(a) == key(b)) {
        if chunk.len() > 1 {
            out.push(chunk.to_vec());
        }
    }
    out
}

pub fn duplicates_in(home: &Path, req: &DuplicatesRequest) -> DuplicatesReport {
    let started = Instant::now();
    let scope = Scope::new(home, false);
    let roots: Vec<(PathBuf, ())> = resolve_roots(req.roots.as_deref(), home).into_iter().map(|p| (p, ())).collect();
    let min = req.min_bytes.unwrap_or(DUP_MIN);
    let visitors = fsutil::walk(&roots, Detail::Full, || DupVisitor {
        scope: &scope,
        min,
        scanned: 0,
        cands: Vec::new(),
        arena: Vec::new(),
    });

    let mut scanned = 0;
    let mut cands: Vec<Cand> = Vec::new();
    let mut arena: Vec<u8> = Vec::new();
    for v in visitors {
        scanned += v.scanned;
        let base = arena.len();
        arena.extend_from_slice(&v.arena);
        cands.extend(v.cands.into_iter().map(|c| Cand { off: c.off + base, ..c }));
    }
    let path_of = |c: &Cand| &arena[c.off..c.off + c.len];
    cands.sort_unstable_by(|a, b| (a.size, a.dev, a.ino, path_of(a)).cmp(&(b.size, b.dev, b.ino, path_of(b))));
    // Hardlinks: same file, the first path is kept.
    cands.dedup_by(|b, a| a.size == b.size && a.dev == b.dev && a.ino == b.ino);
    let path = |i: usize| Path::new(OsStr::from_bytes(path_of(&cands[i])));

    // 1. Same size.
    let by_size = split_by((0..cands.len()).collect(), |i| Some(cands[i].size));
    // 2. Same first 64 KB.
    let todo: Vec<usize> = by_size.into_iter().flatten().collect();
    let heads = par_map(todo.len(), HEAD as usize, |k, buf| hash_head(path(todo[k]), cands[todo[k]].size, buf));
    let mut head_of = vec![None; cands.len()];
    for (&i, h) in todo.iter().zip(heads) {
        head_of[i] = h;
    }
    let by_head = split_by(todo, |i| head_of[i].map(|h| (cands[i].size, h)));
    // 3. Rest of the contents, only if the file is larger than what was already compared.
    let (small, big): (Vec<Vec<usize>>, Vec<Vec<usize>>) = by_head.into_iter().partition(|g| cands[g[0]].size <= HEAD);
    let todo: Vec<usize> = big.into_iter().flatten().collect();
    let tails = par_map(todo.len(), READ_BUF, |k, buf| hash_tail(path(todo[k]), cands[todo[k]].size, buf));
    let mut tail_of = vec![None; cands.len()];
    for (&i, h) in todo.iter().zip(tails) {
        tail_of[i] = h;
    }
    let mut same = split_by(todo, |i| Some((cands[i].size, head_of[i]?, tail_of[i]?)));
    same.extend(small);

    let mut groups: Vec<DupGroup> = same
        .into_iter()
        .map(|g| {
            let allocs = g.iter().map(|&i| cands[i].alloc);
            let bytes = allocs.clone().max().unwrap_or(0);
            let mut paths: Vec<String> = g.iter().map(|&i| lossy(path_of(&cands[i]))).collect();
            paths.sort();
            DupGroup { bytes, paths, wasted_bytes: allocs.sum::<u64>() - bytes }
        })
        .collect();
    groups.sort_unstable_by(|a, b| {
        b.wasted_bytes.cmp(&a.wasted_bytes).then(b.bytes.cmp(&a.bytes)).then_with(|| a.paths.cmp(&b.paths))
    });
    let total_wasted = groups.iter().map(|g| g.wasted_bytes).sum();
    groups.truncate(req.limit.unwrap_or(DUP_LIMIT) as usize);
    DuplicatesReport { groups, total_wasted, scanned_files: scanned, elapsed_ms: elapsed_ms(started) }
}

pub fn duplicates(req: &DuplicatesRequest) -> Result<DuplicatesReport, String> {
    let home = rules::home_dir().ok_or("HOME is not set")?;
    Ok(duplicates_in(&home, req))
}

// ---------------------------------------------------------------------------------------------
// Space tree (Space Lens)
//
// A single pass: each directory in the first `depth` levels is a node with an id. The tag
// of the queued jobs is the node that everything below is attributed to; below `depth`,
// subdirectories inherit the parent's tag. Each thread sums into its own per-node vector
// and at the end these are merged and propagated up to the root.

struct TreeDir {
    parent: u32,
    name: Vec<u8>,
    /// The `top` largest direct files (only in nodes with visible children).
    files: Vec<(u64, Vec<u8>)>,
}

/// Tag: node, and whether its direct files are visible children in the tree.
type TreeTag = (u32, bool);

struct TreeVisitor<'a> {
    dirs: &'a Mutex<Vec<TreeDir>>,
    depth: u32,
    top: usize,
    /// Bytes and files attributed to each node by this thread.
    own: Vec<(u64, u64)>,
    /// Largest files of the directory being listed.
    cur: BinaryHeap<Reverse<(u64, Vec<u8>)>>,
}

impl Visitor for TreeVisitor<'_> {
    type Tag = TreeTag;

    fn file(&mut self, (node, visible): TreeTag, _dir: &[u8], f: &FileMeta) {
        let i = node as usize;
        if self.own.len() <= i {
            self.own.resize(i + 1, (0, 0));
        }
        self.own[i].0 += f.alloc;
        self.own[i].1 += 1;
        if visible {
            if self.cur.len() >= self.top && self.cur.peek().is_some_and(|Reverse(m)| m.0 >= f.alloc) {
                return;
            }
            self.cur.push(Reverse((f.alloc, f.name.to_vec())));
            if self.cur.len() > self.top {
                self.cur.pop();
            }
        }
    }

    fn dir(&mut self, (node, _): TreeTag, _dir: &[u8], name: &[u8], depth: u32) -> Option<TreeTag> {
        let level = depth + 1;
        if level > self.depth {
            return Some((node, false));
        }
        let mut dirs = self.dirs.lock().unwrap_or_else(|e| e.into_inner());
        dirs.push(TreeDir { parent: node, name: name.to_vec(), files: Vec::new() });
        Some(((dirs.len() - 1) as u32, level < self.depth))
    }

    fn error(&mut self, _: TreeTag) {}

    fn dir_done(&mut self, (node, _): TreeTag) {
        if !self.cur.is_empty() {
            let files: Vec<(u64, Vec<u8>)> = self.cur.drain().map(|Reverse(f)| f).collect();
            self.dirs.lock().unwrap_or_else(|e| e.into_inner())[node as usize].files = files;
        }
    }
}

struct Tree {
    dirs: Vec<TreeDir>,
    totals: Vec<(u64, u64)>,
    kids: Vec<Vec<u32>>,
    depth: u32,
    top: usize,
}

enum Pick {
    Dir(u32),
    File(u64, Vec<u8>),
}

impl Tree {
    fn build(&mut self, id: u32, name: String, path: Vec<u8>, level: u32) -> SpaceNode {
        let (bytes, files) = self.totals[id as usize];
        let mut node = SpaceNode { name, path: lossy(&path), bytes, files, is_dir: true, children: Vec::new(), others_bytes: 0 };
        if level >= self.depth {
            return node;
        }
        let mut picks: Vec<(u64, Pick)> = self.kids[id as usize].iter().map(|&k| (self.totals[k as usize].0, Pick::Dir(k))).collect();
        picks.extend(std::mem::take(&mut self.dirs[id as usize].files).into_iter().map(|(b, n)| (b, Pick::File(b, n))));
        let name_of = |p: &Pick, dirs: &[TreeDir]| match p {
            Pick::Dir(k) => dirs[*k as usize].name.clone(),
            Pick::File(_, n) => n.clone(),
        };
        picks.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| name_of(&a.1, &self.dirs).cmp(&name_of(&b.1, &self.dirs))));
        picks.truncate(self.top);
        for (_, pick) in picks {
            let child = match pick {
                Pick::Dir(k) => {
                    let n = std::mem::take(&mut self.dirs[k as usize].name);
                    let p = fsutil::join(&path, &n);
                    self.build(k, lossy(&n), p, level + 1)
                }
                Pick::File(b, n) => SpaceNode {
                    name: lossy(&n),
                    path: lossy(&fsutil::join(&path, &n)),
                    bytes: b,
                    files: 1,
                    is_dir: false,
                    children: Vec::new(),
                    others_bytes: 0,
                },
            };
            node.children.push(child);
        }
        node.others_bytes = bytes - node.children.iter().map(|c| c.bytes).sum::<u64>();
        node
    }
}

/// Usage tree of `path` down to `depth` levels (1..=3), with the `top` largest children per node.
pub fn space_tree(path: &Path, depth: u32, top: usize) -> io::Result<SpaceNode> {
    let depth = depth.clamp(1, 3);
    let top = top.max(1);
    let md = fs::symlink_metadata(path)?;
    let root = fsutil::trim_path(path);
    let name = match root.rsplit(|&c| c == b'/').next() {
        Some(n) if !n.is_empty() => lossy(n),
        _ => lossy(&root),
    };
    if !md.is_dir() {
        let bytes = md.blocks() * 512;
        return Ok(SpaceNode { name, path: lossy(&root), bytes, files: 1, is_dir: false, children: Vec::new(), others_bytes: 0 });
    }
    let dirs = Mutex::new(vec![TreeDir { parent: 0, name: Vec::new(), files: Vec::new() }]);
    let owns: Vec<Vec<(u64, u64)>> = fsutil::walk(&[(path.to_path_buf(), (0, true))], Detail::Basic, || TreeVisitor {
        dirs: &dirs,
        depth,
        top,
        own: Vec::new(),
        cur: BinaryHeap::new(),
    })
    .into_iter()
    .map(|v| v.own)
    .collect();
    let dirs = dirs.into_inner().unwrap_or_else(|e| e.into_inner());
    let mut totals = vec![(0u64, 0u64); dirs.len()];
    for own in owns {
        for (t, (b, f)) in totals.iter_mut().zip(own) {
            t.0 += b;
            t.1 += f;
        }
    }
    // Children always have a higher id than their parent.
    let mut kids = vec![Vec::new(); dirs.len()];
    for i in (1..dirs.len()).rev() {
        let p = dirs[i].parent as usize;
        totals[p].0 += totals[i].0;
        totals[p].1 += totals[i].1;
        kids[p].push(i as u32);
    }
    let mut tree = Tree { dirs, totals, kids, depth, top };
    Ok(tree.build(0, name, root, 0))
}

// ---------------------------------------------------------------------------------------------
// Trash

/// Package extensions: folders macOS presents as a single document or app. Deleting a file
/// inside one (e.g. the Photos library's `Photos.sqlite`) corrupts the whole library.
const PACKAGE_EXTENSIONS: &[&str] = &[
    "app", "photoslibrary", "photolibrary", "migratedphotolibrary", "musiclibrary", "tvlibrary", "imovielibrary",
    "fcpbundle", "logicx", "band", "lrlibrary", "aplibrary", "bundle", "framework", "plugin", "kext", "xcarchive",
    "xcodeproj", "xcworkspace", "playground", "pages", "numbers", "key", "rtfd", "sparsebundle", "photosmigrator",
];

pub(crate) fn is_package(name: &[u8]) -> bool {
    let Some(dot) = name.iter().rposition(|&c| c == b'.') else { return false };
    let ext = &name[dot + 1..];
    dot > 0 && PACKAGE_EXTENSIONS.iter().any(|p| p.as_bytes().eq_ignore_ascii_case(ext))
}

/// Standard top-level `$HOME` folders that are never sent to the Trash.
const PROTECTED: [&str; 10] =
    ["Desktop", "Documents", "Downloads", "Movies", "Music", "Pictures", "Public", "Applications", "Library", ".Trash"];

fn same_name(c: &OsStr, n: &str) -> bool {
    // APFS is usually case-insensitive: `~/desktop` is `~/Desktop`.
    c.as_bytes().eq_ignore_ascii_case(n.as_bytes())
}

fn validate_user_path(home: Option<&Path>, raw: &str) -> Result<PathBuf, String> {
    let home = home.ok_or("HOME is not available")?;
    if !raw.starts_with('/') || raw.split('/').any(|c| c == "." || c == "..") {
        return Err("invalid path".into());
    }
    let path = Path::new(raw);
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err("invalid path".into());
    };
    fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    // The parent is resolved (not the item: a symlink is moved itself, not its target).
    let canon = fs::canonicalize(parent).map_err(|e| e.to_string())?.join(name);
    let rel = canon.strip_prefix(home).map_err(|_| "outside the home directory".to_string())?;
    let parts: Vec<&OsStr> = rel.iter().collect();
    let protected = match parts.as_slice() {
        [] => true,
        [first] => PROTECTED.iter().any(|p| same_name(first, p)),
        [first, _] => same_name(first, "Library"),
        _ => false,
    };
    if protected {
        return Err("protected folder".into());
    }
    if parts.iter().rev().skip(1).any(|c| is_package(c.as_bytes())) {
        return Err("it is inside a library or app: delete it from the app that uses it".into());
    }
    Ok(canon)
}

/// Moves user-chosen paths inside `home` to the Trash.
pub fn trash_paths_in(home: &Path, req: &CleanRequest) -> CleanReport {
    let home = fs::canonicalize(home).ok();
    let validated: Vec<Result<PathBuf, String>> =
        req.paths.iter().map(|p| validate_user_path(home.as_deref(), p)).collect();
    let valid: Vec<PathBuf> = validated.iter().filter_map(|v| v.as_ref().ok().cloned()).collect();
    let _progress = progress::start(valid.len(), req.dry_run);
    let mut sizes = fsutil::usages(&valid).into_iter().map(|u| u.bytes);

    let mut report = CleanReport::new(req.dry_run);
    let action = trash::preferred_action();
    for (raw, v) in req.paths.iter().zip(validated) {
        let outcome = match v {
            Err(error) => CleanOutcome { path: raw.clone(), category: None, action: None, bytes: 0, ok: false, error: Some(error) },
            Ok(path) => {
                let bytes = sizes.next().unwrap_or(0);
                let result = if req.dry_run { Ok(action) } else { progress::step(&path, bytes, || trash::discard(&path, action)) };
                if result.is_ok() {
                    report.add_removed(action, bytes);
                }
                CleanOutcome {
                    path: raw.clone(),
                    category: Some("clutter".into()),
                    action: Some(action),
                    bytes,
                    ok: result.is_ok(),
                    error: result.err(),
                }
            }
        };
        if !outcome.ok {
            report.failed += 1;
        }
        report.outcomes.push(outcome);
    }
    report
}

pub fn trash_paths(req: &CleanRequest) -> Result<CleanReport, String> {
    let home = rules::home_dir().ok_or("HOME is not set")?;
    Ok(trash_paths_in(&home, req))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::ffi::CString;

    fn write(p: &Path, len: usize, fill: u8) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![fill; len]).unwrap();
    }

    fn set_times(p: &Path, secs: u64) {
        let c = CString::new(p.as_os_str().as_bytes()).unwrap();
        let tv = [libc::timeval { tv_sec: secs as libc::time_t, tv_usec: 0 }; 2];
        assert_eq!(unsafe { libc::utimes(c.as_ptr(), tv.as_ptr()) }, 0);
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    fn names(r: &LargeFilesReport, home: &Path) -> Vec<String> {
        r.files.iter().map(|f| f.path.strip_prefix(&s(home)).unwrap().to_owned()).collect()
    }

    #[test]
    fn large_files_filters_size_scope_and_age() {
        let tmp = TempDir::new("large");
        let h = tmp.path();
        write(&h.join("old.mov"), 3_000_000, 1);
        write(&h.join("new.zip"), 2_000_000, 2);
        write(&h.join("small.txt"), 10_000, 3);
        write(&h.join("sub/deep/disk.dmg"), 4_000_000, 4);
        write(&h.join(".hidden/huge.bin"), 5_000_000, 5);
        write(&h.join("Foo.app/Contents/big"), 5_000_000, 6);
        write(&h.join("Library/big.bin"), 5_000_000, 7);
        let old = now_secs() - 400 * DAY;
        set_times(&h.join("old.mov"), old);

        let req = LargeFilesRequest { min_bytes: Some(1_000_000), ..Default::default() };
        let r = large_files_in(h, &req);
        assert_eq!(names(&r, h), ["/sub/deep/disk.dmg", "/old.mov", "/new.zip"]);
        assert_eq!(r.files.iter().map(|f| f.kind).collect::<Vec<_>>(), [FileKind::DiskImage, FileKind::Video, FileKind::Archive]);
        assert!(r.files[0].bytes >= 4_000_000);
        assert_eq!(r.files[1].modified, old);
        assert_eq!(r.files[1].accessed, old);
        assert_eq!(r.scanned_files, 4);

        let r = large_files_in(h, &LargeFilesRequest { include_library: true, ..req.clone() });
        assert_eq!(names(&r, h)[0], "/Library/big.bin");
        assert_eq!(r.files.len(), 4);

        let r = large_files_in(h, &LargeFilesRequest { older_than_days: Some(30), ..req.clone() });
        assert_eq!(names(&r, h), ["/old.mov"]);

        let r = large_files_in(h, &LargeFilesRequest { limit: Some(2), ..req.clone() });
        assert_eq!(names(&r, h), ["/sub/deep/disk.dmg", "/old.mov"]);

        // Overlapping explicit roots do not duplicate results; the default filter is 50 MB.
        let roots = Some(vec![s(h), s(&h.join("sub")), format!("{}/", s(h))]);
        let r = large_files_in(h, &LargeFilesRequest { roots: roots.clone(), min_bytes: Some(1_000_000), ..Default::default() });
        assert_eq!(r.files.len(), 3);
        assert!(large_files_in(h, &LargeFilesRequest { roots, ..Default::default() }).files.is_empty());
    }

    #[test]
    fn kinds_by_extension() {
        assert_eq!(kind_of(b"a.MKV"), FileKind::Video);
        assert_eq!(kind_of(b"b.heic"), FileKind::Image);
        assert_eq!(kind_of(b"c.flac"), FileKind::Audio);
        assert_eq!(kind_of(b"d.tar.gz"), FileKind::Archive);
        assert_eq!(kind_of(b"e.sparseimage"), FileKind::DiskImage);
        assert_eq!(kind_of(b"f.pdf"), FileKind::Document);
        assert_eq!(kind_of(b"g.pkg"), FileKind::App);
        assert_eq!(kind_of(b"h.rs"), FileKind::Code);
        assert_eq!(kind_of(b".zip"), FileKind::Other);
        assert_eq!(kind_of(b"no_ext"), FileKind::Other);
    }

    #[test]
    fn duplicates_by_content_not_size_nor_inode() {
        let tmp = TempDir::new("dups");
        let h = tmp.path();
        write(&h.join("a1.bin"), 2_000_000, 0xaa);
        write(&h.join("sub/a2.bin"), 2_000_000, 0xaa);
        fs::hard_link(h.join("a1.bin"), h.join("a3.bin")).unwrap();
        // Same size and same first 64 KB, different ending.
        let mut b = vec![0xaau8; 2_000_000];
        b[1_999_999] = 0;
        fs::write(h.join("b.bin"), b).unwrap();
        write(&h.join("c.bin"), 100_000, 0xcc);
        write(&h.join("sub/c2.bin"), 100_000, 0xcc);
        // Smaller than the initial block: the first hash is enough.
        write(&h.join("d.bin"), 30_000, 0xdd);
        write(&h.join("d2.bin"), 30_000, 0xdd);
        write(&h.join("e.bin"), 30_000, 0xee);
        // Hardlinks with no other copy: not duplicates.
        write(&h.join("f.bin"), 40_000, 0xff);
        fs::hard_link(h.join("f.bin"), h.join("f2.bin")).unwrap();
        write(&h.join(".hidden/a4.bin"), 2_000_000, 0xaa);

        let r = duplicates_in(h, &DuplicatesRequest { min_bytes: Some(10_000), ..Default::default() });
        let got: Vec<Vec<String>> =
            r.groups.iter().map(|g| g.paths.iter().map(|p| p.strip_prefix(&s(h)).unwrap().to_owned()).collect()).collect();
        assert_eq!(got, [vec!["/a1.bin", "/sub/a2.bin"], vec!["/c.bin", "/sub/c2.bin"], vec!["/d.bin", "/d2.bin"]]);
        assert!(r.groups[0].bytes >= 2_000_000);
        assert_eq!(r.groups[0].wasted_bytes, r.groups[0].bytes);
        assert_eq!(r.total_wasted, r.groups.iter().map(|g| g.wasted_bytes).sum::<u64>());
        assert_eq!(r.scanned_files, 11);

        let r = duplicates_in(h, &DuplicatesRequest { min_bytes: Some(10_000), limit: Some(1), ..Default::default() });
        assert_eq!(r.groups.len(), 1);
        assert!(duplicates_in(h, &DuplicatesRequest::default()).groups.len() == 1);
    }

    fn check_sums(n: &SpaceNode) {
        if n.is_dir {
            assert_eq!(n.bytes, fsutil::usage(Path::new(&n.path)).bytes, "{}", n.path);
            assert_eq!(n.files, fsutil::usage(Path::new(&n.path)).files, "{}", n.path);
        }
        if !n.children.is_empty() {
            assert_eq!(n.bytes, n.children.iter().map(|c| c.bytes).sum::<u64>() + n.others_bytes, "{}", n.path);
            assert!(n.children.windows(2).all(|w| w[0].bytes >= w[1].bytes));
        }
        n.children.iter().for_each(check_sums);
    }

    #[test]
    fn space_tree_keeps_top_and_matches_usage() {
        let tmp = TempDir::new("tree");
        let root = tmp.path().join("root");
        write(&root.join("a/x.bin"), 100_000, 1);
        write(&root.join("a/y/z.bin"), 50_000, 1);
        write(&root.join("a/y/w/v/u.bin"), 70_000, 1);
        write(&root.join("a/t.bin"), 1_000, 1);
        write(&root.join("b.bin"), 400_000, 1);
        for i in 0..30 {
            write(&root.join(format!("c/f{i}")), 3_000, 1);
        }
        write(&root.join("d.bin"), 10_000, 1);
        let total = fsutil::usage(&root);

        let t = space_tree(&root, 2, 2).unwrap();
        check_sums(&t);
        assert_eq!((t.bytes, t.files), (total.bytes, total.files));
        assert_eq!(t.name, "root");
        assert_eq!(t.children.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["b.bin", "a"]);
        assert!(!t.children[0].is_dir && t.children[0].files == 1);
        let a = &t.children[1];
        assert_eq!(a.children.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["y", "x.bin"]);
        assert_eq!(a.others_bytes, fsutil::usage(&root.join("a/t.bin")).bytes);
        // Last level: no children, with everything below.
        assert!(a.children[0].children.is_empty() && a.children[0].files == 2);

        let t = space_tree(&root, 3, 50).unwrap();
        check_sums(&t);
        assert_eq!(t.others_bytes, 0);
        assert_eq!(t.children.len(), 4);
        let c = t.children.iter().find(|c| c.name == "c").unwrap();
        assert_eq!(c.children.len(), 30);

        let t = space_tree(&root, 1, 1).unwrap();
        check_sums(&t);
        assert_eq!(t.children.len(), 1);
        assert!(t.children[0].children.is_empty());

        let f = space_tree(&root.join("b.bin"), 2, 2).unwrap();
        assert!(!f.is_dir && f.bytes >= 400_000);
        assert!(space_tree(&root.join("no"), 2, 2).is_err());
    }

    #[test]
    fn trash_paths_validates_strictly() {
        let tmp = TempDir::new("trashpaths");
        let h = tmp.path();
        for d in ["Desktop", "Documents", "Downloads", "Library/Caches/app", "sub"] {
            fs::create_dir_all(h.join(d)).unwrap();
        }
        write(&h.join("notes.txt"), 5_000, 1);
        write(&h.join("sub/f.bin"), 20_000, 1);
        write(&h.join("Library/Caches/app/x"), 1_000, 1);
        std::os::unix::fs::symlink("/", h.join("escape")).unwrap();

        let bad = [
            s(h),
            format!("{}/", s(h)),
            s(&h.join("Desktop")),
            s(&h.join("desktop")),
            s(&h.join("Downloads")),
            s(&h.join("Library")),
            s(&h.join("Library/Caches")),
            s(&h.join("sub/../notes.txt")),
            s(&h.join("sub/./f.bin")),
            s(&h.join("missing")),
            s(&h.join("escape/etc")),
            "/etc/hosts".into(),
            "relative/x".into(),
        ];
        let good = [s(&h.join("notes.txt")), s(&h.join("sub/f.bin")), s(&h.join("sub")), s(&h.join("Library/Caches/app")), s(&h.join("escape"))];
        let req = CleanRequest { paths: bad.iter().chain(&good).cloned().collect(), dry_run: true };
        let r = trash_paths_in(h, &req);
        let oks: Vec<bool> = r.outcomes.iter().map(|o| o.ok).collect();
        assert_eq!(oks, [vec![false; bad.len()], vec![true; good.len()]].concat(), "{:?}", r.outcomes);
        assert_eq!(r.failed, bad.len() as u64);
        assert!(r.trashed_bytes >= 25_000);
        assert_eq!(r.outcomes[bad.len()].category.as_deref(), Some("clutter"));
        assert!(h.join("notes.txt").exists() && h.join("sub/f.bin").exists());
    }

    #[test]
    fn never_lists_or_trashes_files_inside_packages() {
        let tmp = TempDir::new("clutter-packages");
        let home = tmp.path();
        let lib = home.join("Pictures/Photos Library.photoslibrary/database");
        fs::create_dir_all(&lib).unwrap();
        fs::write(lib.join("Photos.sqlite"), vec![0u8; 3_000_000]).unwrap();
        let free = home.join("Videos");
        fs::create_dir_all(&free).unwrap();
        fs::write(free.join("big.mov"), vec![0u8; 3_000_000]).unwrap();

        let r = large_files_in(home, &LargeFilesRequest { min_bytes: Some(1_000_000), ..Default::default() });
        let names: Vec<_> = r.files.iter().map(|f| f.path.rsplit('/').next().unwrap().to_string()).collect();
        assert_eq!(names, ["big.mov"]);

        let inside = lib.join("Photos.sqlite").to_string_lossy().into_owned();
        let report = trash_paths_in(home, &CleanRequest { paths: vec![inside], dry_run: true });
        assert!(!report.outcomes[0].ok, "{:?}", report.outcomes[0].error);
        assert!(is_package(b"Photos Library.photoslibrary") && !is_package(b"photos") && !is_package(b".app"));
    }

}
