//! Ordnergrößen: parallel berechnet, abbrechbar, Ergebnisse für alle Panes geteilt.
//!
//! Gezählt wird die nominelle Dateigröße (`st_size`). Hardlinks zählen pro
//! angefragtem Ordner nur einmal, andere Dateisysteme (z. B. `/proc`, Mounts)
//! werden nicht betreten.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirSize {
    /// Fertig berechnet.
    Done(u64),
    /// Berechnung läuft, bisher gezählt.
    Partial(u64),
}

#[derive(Default)]
struct Inner {
    done: Mutex<HashMap<PathBuf, u64>>,
    running: Mutex<HashMap<PathBuf, Arc<AtomicU64>>>,
    pending: AtomicUsize,
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

impl DirSizes {
    pub fn get(&self, dir: &Path) -> Option<DirSize> {
        if let Some(&n) = self.inner.done.lock().unwrap().get(dir) {
            return Some(DirSize::Done(n));
        }
        let running = self.inner.running.lock().unwrap();
        running.get(dir).map(|acc| DirSize::Partial(acc.load(Ordering::Relaxed)))
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
            dirs.into_iter().filter(|d| !done.contains_key(d) && !running.contains_key(d)).collect()
        };
        if todo.is_empty() {
            return;
        }
        self.inner.pending.fetch_add(todo.len(), Ordering::Relaxed);
        let me = self.clone();
        rayon::spawn(move || {
            todo.par_iter().for_each(|dir| {
                let acc = Arc::new(AtomicU64::new(0));
                me.inner.running.lock().unwrap().insert(dir.clone(), acc.clone());
                let dev = std::fs::symlink_metadata(dir).map(|m| m.dev()).unwrap_or(0);
                let links = Mutex::new(HashSet::new());
                me.walk(dir, dev, &cancel, &links, &acc);
                me.inner.running.lock().unwrap().remove(dir);
                me.inner.pending.fetch_sub(1, Ordering::Relaxed);
            });
        });
    }

    /// Vergisst alles unter `dir` und die Summen der übergeordneten Ordner.
    pub fn invalidate(&self, dir: &Path) {
        self.inner
            .done
            .lock()
            .unwrap()
            .retain(|p, _| !p.starts_with(dir) && !dir.starts_with(p));
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
                if m.dev() == dev {
                    subdirs.push(e.path());
                }
            } else if m.nlink() > 1 && !links.lock().unwrap().insert((m.dev(), m.ino())) {
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
        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_nested() {
        let root = std::env::temp_dir().join(format!("guppy-du-{}", std::process::id()));
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/x"), [0u8; 100]).unwrap();
        std::fs::write(root.join("a/b/y"), [0u8; 50]).unwrap();
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
