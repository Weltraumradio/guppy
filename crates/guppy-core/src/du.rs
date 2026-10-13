//! Ordnergrößen: parallel berechnet, abbrechbar, Ergebnisse für alle Panes geteilt.
//!
//! Gezählt wird die nominelle Dateigröße. Hardlinks zählen pro angefragtem
//! Ordner nur einmal, andere Dateisysteme (z. B. `/proc`, Mounts) werden nicht
//! betreten (siehe [`crate::platform`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use crate::platform;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirSize {
    /// Fertig berechnet.
    Done(u64),
    /// Berechnung läuft, bisher gezählt.
    Partial(u64),
    /// Wert aus dem letzten Programmlauf; wird im Hintergrund überprüft.
    Cached(u64),
}

#[derive(Default)]
struct Inner {
    done: Mutex<HashMap<PathBuf, u64>>,
    /// Laufende Berechnungen: Zwischenstand und Abbruch-Signal der Anfrage.
    running: Mutex<HashMap<PathBuf, (Arc<AtomicU64>, Cancel)>>,
    pending: AtomicUsize,
    /// Dauerhafter Cache: beim Start geladen, beim Beenden gespeichert.
    cached: Mutex<HashMap<PathBuf, u64>>,
    cache_file: Option<PathBuf>,
}

/// Teilbarer Größen-Speicher; `clone()` ist billig.
#[derive(Clone, Default)]
pub struct DirSizes {
    inner: Arc<Inner>,
}

/// Abbruch-Signal für eine Anfrage, z. B. wenn das Pane den Ordner verlässt.
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

/// Parallele Zugriffe pro Netzlaufwerk-Berechnung.
const NET_THREADS: usize = 4;

fn net_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| rayon::ThreadPoolBuilder::new().num_threads(NET_THREADS).build().expect("Thread-Pool"))
}

impl DirSizes {
    /// Standardort des Caches, z. B. `~/.cache/guppy/sizes.tsv`.
    pub fn default_cache_file() -> Option<PathBuf> {
        dirs::cache_dir().map(|d| d.join("guppy/sizes.tsv"))
    }

    /// Mit dauerhaftem Cache in `file` (Zeilen `größe<TAB>pfad`).
    pub fn with_cache(file: PathBuf) -> Self {
        let cached = std::fs::read_to_string(&file)
            .map(|s| {
                s.lines()
                    .filter_map(|l| {
                        let (n, p) = l.split_once('\t')?;
                        Some((PathBuf::from(p), n.parse().ok()?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            inner: Arc::new(Inner { cached: Mutex::new(cached), cache_file: Some(file), ..Default::default() }),
        }
    }

    /// Schreibt alle bekannten Größen in den Cache (atomar über eine Zwischendatei).
    pub fn save(&self) -> std::io::Result<()> {
        let Some(file) = &self.inner.cache_file else { return Ok(()) };
        let mut all = self.inner.cached.lock().unwrap().clone();
        all.extend(self.inner.done.lock().unwrap().iter().map(|(p, n)| (p.clone(), *n)));
        let mut out = String::new();
        for (p, n) in &all {
            let p = p.to_string_lossy();
            // Pfade mit Zeilenumbruch passen nicht ins Zeilenformat.
            if !p.contains('\n') {
                out.push_str(&format!("{n}\t{p}\n"));
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
        if let Some(&n) = self.inner.done.lock().unwrap().get(dir) {
            return Some(DirSize::Done(n));
        }
        let cached = self.inner.cached.lock().unwrap().get(dir).copied();
        if let Some(n) = cached {
            // Schätzwert bleibt stehen, bis das neue Ergebnis da ist; kein Hochzählen.
            return Some(DirSize::Cached(n));
        }
        if let Some((acc, _)) = self.inner.running.lock().unwrap().get(dir) {
            return Some(DirSize::Partial(acc.load(Ordering::Relaxed)));
        }
        None
    }

    /// Läuft gerade mindestens eine Berechnung?
    pub fn busy(&self) -> bool {
        self.inner.pending.load(Ordering::Relaxed) > 0
    }

    /// Berechnet die Größen der Ordner im Hintergrund; bekannte werden übersprungen.
    pub fn request(&self, dirs: Vec<PathBuf>, cancel: Cancel) {
        let todo: Vec<PathBuf> = {
            let done = self.inner.done.lock().unwrap();
            let running = self.inner.running.lock().unwrap();
            // Abgebrochene, noch nicht beendete Läufe zählen nicht als laufend.
            let active = |d: &PathBuf| running.get(d).is_some_and(|(_, c)| !c.is_cancelled());
            dirs.into_iter().filter(|d| !done.contains_key(d) && !active(d)).collect()
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
                    // Nur den eigenen Eintrag entfernen, nicht den eines neueren Laufs.
                    let mut running = me.inner.running.lock().unwrap();
                    if running.get(dir).is_some_and(|(a, _)| Arc::ptr_eq(a, &acc)) {
                        running.remove(dir);
                    }
                }
                me.inner.pending.fetch_sub(1, Ordering::Relaxed);
            });
        };
        // Netzlaufwerke: eigener kleiner Pool, damit das NAS nicht mit Dutzenden
        // gleichzeitigen Anfragen überlastet wird und lokale Ordner nicht warten.
        if todo_is_network {
            net_pool().spawn(job);
        } else {
            rayon::spawn(job);
        }
    }

    /// Markiert alles unter `dir` und die übergeordneten Ordner als veraltet.
    /// Die alten Werte bleiben als Schätzwert sichtbar, bis neu gerechnet ist.
    pub fn invalidate(&self, dir: &Path) {
        let stale = |p: &PathBuf| p.starts_with(dir) || dir.starts_with(p);
        let mut done = self.inner.done.lock().unwrap();
        let mut cached = self.inner.cached.lock().unwrap();
        done.retain(|p, n| {
            if stale(p) {
                cached.insert(p.clone(), *n);
            }
            !stale(p)
        });
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
        if let Some(&n) = self.inner.done.lock().unwrap().get(dir) {
            acc.fetch_add(n, Ordering::Relaxed);
            return Some(n);
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return Some(0) };

        let mut files = 0u64;
        let mut subdirs = Vec::new();
        for e in rd.flatten() {
            // DirEntry::metadata folgt keinen Symlinks.
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                if platform::device(&m) == dev {
                    subdirs.push(e.path());
                }
            } else if platform::hardlink_key(&m).is_some_and(|k| !links.lock().unwrap().insert(k)) {
                // Hardlink schon gezählt.
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
        self.inner.done.lock().unwrap().insert(dir.to_path_buf(), total);
        self.inner.cached.lock().unwrap().remove(dir);
        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_roundtrip() {
        let file = std::env::temp_dir().join(format!("guppy-cache-{}.tsv", std::process::id()));
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
        std::fs::remove_dir_all(root).unwrap();
    }
}
