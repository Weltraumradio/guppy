//! Directory sizes: computed in parallel, cancellable, results shared across all panes.
//!
//! Counts nominal file size. Hard links count only once per requested
//! directory; other filesystems (e.g. `/proc`, mounts) are not entered
//! (see [`crate::platform`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use crate::platform;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirSize {
    /// Fully computed.
    Done(u64),
    /// Computation running; counted so far.
    Partial(u64),
    /// Value from the previous run; being re-checked in the background.
    Cached(u64),
}

/// A folder by its path hash (see [`key`]); `parent` lets [`DirSizes::invalidate`]
/// find everything below a folder without keeping 100 000s of paths in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Node {
    size: u64,
    parent: u64,
}

/// FNV-1a over the path bytes: stable across builds, so the cache file stays valid.
fn key(p: &Path) -> u64 {
    p.as_os_str().as_encoded_bytes().iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

fn node(dir: &Path, size: u64) -> Node {
    Node { size, parent: dir.parent().map_or(0, key) }
}

/// Header of the cache file; records follow as three little-endian u64: key, size, parent.
const MAGIC: &[u8; 8] = b"guppysz1";

#[derive(Default)]
struct Inner {
    done: Mutex<HashMap<u64, Node>>,
    /// Running computations: progress and the request's cancel signal.
    running: Mutex<HashMap<PathBuf, (Arc<AtomicU64>, Cancel)>>,
    pending: AtomicUsize,
    /// Persistent cache: loaded at startup, saved on exit.
    cached: Mutex<HashMap<u64, Node>>,
    cache_file: Option<PathBuf>,
}

/// Shareable size store; `clone()` is cheap.
#[derive(Clone, Default)]
pub struct DirSizes {
    inner: Arc<Inner>,
}

/// Cancel signal for a request, e.g. when the pane leaves the directory.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Parallel accesses per network-drive computation; set once at startup.
static NET_THREADS: AtomicUsize = AtomicUsize::new(4);

/// Sets the parallelism for network drives; only effective before the first use.
pub fn set_network_threads(n: usize) {
    NET_THREADS.store(n.max(1), Ordering::Relaxed);
}

fn net_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| rayon::ThreadPoolBuilder::new().num_threads(NET_THREADS.load(Ordering::Relaxed)).build().expect("thread pool"))
}

impl DirSizes {
    /// Default cache location, e.g. `~/.cache/guppy/sizes.bin`.
    pub fn default_cache_file() -> Option<PathBuf> {
        let dir = dirs::cache_dir()?.join("guppy");
        // The old text format, replaced by the compact one.
        let _ = std::fs::remove_file(dir.join("sizes.tsv"));
        Some(dir.join("sizes.bin"))
    }

    /// With a persistent cache in `file` (binary, see [`MAGIC`]).
    pub fn with_cache(file: PathBuf) -> Self {
        let cached = std::fs::read(&file)
            .ok()
            .filter(|b| b.starts_with(MAGIC))
            .map(|b| {
                let u = |c: &[u8]| u64::from_le_bytes(c.try_into().unwrap());
                b[MAGIC.len()..].chunks_exact(24).map(|c| (u(&c[..8]), Node { size: u(&c[8..16]), parent: u(&c[16..]) })).collect()
            })
            .unwrap_or_default();
        Self {
            inner: Arc::new(Inner { cached: Mutex::new(cached), cache_file: Some(file), ..Default::default() }),
        }
    }

    /// Writes all known sizes to the cache (atomically via a temp file).
    pub fn save(&self) -> std::io::Result<()> {
        let Some(file) = &self.inner.cache_file else { return Ok(()) };
        let mut all = self.inner.cached.lock().unwrap().clone();
        all.extend(self.inner.done.lock().unwrap().iter().map(|(k, n)| (*k, *n)));
        let mut out = Vec::with_capacity(MAGIC.len() + 24 * all.len());
        out.extend_from_slice(MAGIC);
        for (k, n) in &all {
            for v in [*k, n.size, n.parent] {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = file.with_extension("tmp");
        std::fs::write(&tmp, out)?;
        std::fs::rename(tmp, file)
    }

    pub fn get(&self, dir: &Path) -> Option<DirSize> {
        let k = key(dir);
        if let Some(n) = self.inner.done.lock().unwrap().get(&k) {
            return Some(DirSize::Done(n.size));
        }
        let cached = self.inner.cached.lock().unwrap().get(&k).map(|n| n.size);
        if let Some(n) = cached {
            // Keep the estimate until the new result is in; no counting up.
            return Some(DirSize::Cached(n));
        }
        if let Some((acc, _)) = self.inner.running.lock().unwrap().get(dir) {
            return Some(DirSize::Partial(acc.load(Ordering::Relaxed)));
        }
        None
    }

    /// Is at least one computation running?
    pub fn busy(&self) -> bool {
        self.inner.pending.load(Ordering::Relaxed) > 0
    }

    /// Computes directory sizes in the background; known ones are skipped.
    pub fn request(&self, dirs: Vec<PathBuf>, cancel: Cancel) {
        let todo: Vec<PathBuf> = {
            let done = self.inner.done.lock().unwrap();
            let running = self.inner.running.lock().unwrap();
            // Cancelled runs that haven't finished yet don't count as running.
            let active = |d: &PathBuf| running.get(d).is_some_and(|(_, c)| !c.is_cancelled());
            dirs.into_iter().filter(|d| !done.contains_key(&key(d)) && !active(d)).collect()
        };
        if todo.is_empty() {
            return;
        }
        let todo_is_network = platform::is_network(&todo[0]);
        self.inner.pending.fetch_add(todo.len(), Ordering::Relaxed);
        let me = self.clone();
        let job = move || {
            todo.par_iter().for_each(|dir| {
                let acc = Arc::new(AtomicU64::new(0));
                me.inner.running.lock().unwrap().insert(dir.clone(), (acc.clone(), cancel.clone()));
                let dev = std::fs::symlink_metadata(dir).map(|m| platform::device(&m)).unwrap_or(0);
                let links = Mutex::new(HashSet::new());
                me.walk(dir, dev, &cancel, &links, &acc);
                {
                    // Remove only our own entry, not that of a newer run.
                    let mut running = me.inner.running.lock().unwrap();
                    if running.get(dir).is_some_and(|(a, _)| Arc::ptr_eq(a, &acc)) {
                        running.remove(dir);
                    }
                }
                me.inner.pending.fetch_sub(1, Ordering::Relaxed);
            });
        };
        // Network drives: separate small pool so the NAS isn't flooded with dozens
        // of concurrent requests and local directories don't have to wait.
        if todo_is_network {
            net_pool().spawn(job);
        } else {
            rayon::spawn(job);
        }
    }

    /// Marks everything under `dir` and its parent directories as stale.
    /// Old values stay visible as estimates until recomputed.
    pub fn invalidate(&self, dir: &Path) {
        let target = key(dir);
        let ancestors: HashSet<u64> = dir.ancestors().map(key).collect();
        let mut done = self.inner.done.lock().unwrap();
        let mut cached = self.inner.cached.lock().unwrap();
        // Below `dir`: following the parents leads to it. Each answer is remembered.
        let mut below: HashMap<u64, bool> = HashMap::new();
        let stale: Vec<u64> = done
            .keys()
            .copied()
            .filter(|&k| {
                if ancestors.contains(&k) {
                    return true;
                }
                let mut chain = Vec::new();
                let mut cur = k;
                let hit = loop {
                    if cur == target {
                        break true;
                    }
                    if let Some(&b) = below.get(&cur) {
                        break b;
                    }
                    chain.push(cur);
                    match done.get(&cur).or_else(|| cached.get(&cur)) {
                        Some(n) if n.parent != 0 => cur = n.parent,
                        _ => break false,
                    }
                };
                below.extend(chain.into_iter().map(|c| (c, hit)));
                hit
            })
            .collect();
        for k in stale {
            if let Some(n) = done.remove(&k) {
                cached.insert(k, n);
            }
        }
    }

    fn walk(
        &self,
        dir: &Path,
        dev: u64,
        cancel: &Cancel,
        links: &Mutex<HashSet<(u64, u64)>>,
        acc: &AtomicU64,
    ) -> Option<u64> {
        if cancel.is_cancelled() {
            return None;
        }
        let k = key(dir);
        if let Some(n) = self.inner.done.lock().unwrap().get(&k) {
            acc.fetch_add(n.size, Ordering::Relaxed);
            return Some(n.size);
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return Some(0) };

        let mut files = 0u64;
        let mut subdirs = Vec::new();
        for e in rd.flatten() {
            // DirEntry::metadata does not follow symlinks.
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                if platform::device(&m) == dev {
                    subdirs.push(e.path());
                }
            } else if platform::hardlink_key(&m).is_some_and(|k| !links.lock().unwrap().insert(k)) {
                // Hard link already counted.
            } else {
                files += m.len();
            }
        }
        acc.fetch_add(files, Ordering::Relaxed);

        let sub: Option<u64> = subdirs
            .par_iter()
            .map(|d| self.walk(d, dev, cancel, links, acc))
            .sum();
        let total = files + sub?;
        self.inner.done.lock().unwrap().insert(k, node(dir, total));
        self.inner.cached.lock().unwrap().remove(&k);
        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_roundtrip() {
        let file = std::env::temp_dir().join(format!("guppy-cache-{}.bin", std::process::id()));
        let root = std::env::temp_dir().join(format!("guppy-cdir-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("x"), [0u8; 42]).unwrap();

        let sizes = DirSizes::with_cache(file.clone());
        sizes.request(vec![root.clone()], Cancel::default());
        while sizes.busy() {
            std::thread::yield_now();
        }
        sizes.save().unwrap();
        assert_eq!(DirSizes::with_cache(file.clone()).get(&root), Some(DirSize::Cached(42)));
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn sums_nested() {
        let root = std::env::temp_dir().join(format!("guppy-du-{}", std::process::id()));
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/x"), [0u8; 100]).unwrap();
        std::fs::write(root.join("a/b/y"), [0u8; 50]).unwrap();
        #[cfg(unix)]
        std::fs::hard_link(root.join("a/b/y"), root.join("a/b/y2")).unwrap();

        let sizes = DirSizes::default();
        sizes.request(vec![root.join("a")], Cancel::default());
        while sizes.busy() {
            std::thread::yield_now();
        }
        assert_eq!(sizes.get(&root.join("a")), Some(DirSize::Done(150)));
        assert_eq!(sizes.get(&root.join("a/b")), Some(DirSize::Done(50)));
        // Below and above the changed folder become estimates; a sibling stays.
        std::fs::create_dir_all(root.join("a/c")).unwrap();
        sizes.request(vec![root.join("a/c")], Cancel::default());
        while sizes.busy() {
            std::thread::yield_now();
        }
        sizes.invalidate(&root.join("a/b"));
        assert_eq!(sizes.get(&root.join("a")), Some(DirSize::Cached(150)));
        assert_eq!(sizes.get(&root.join("a/b")), Some(DirSize::Cached(50)));
        assert_eq!(sizes.get(&root.join("a/c")), Some(DirSize::Done(0)));
        sizes.invalidate(&root);
        assert_eq!(sizes.get(&root.join("a/c")), Some(DirSize::Cached(0)));
        std::fs::remove_dir_all(root).unwrap();
    }
}
