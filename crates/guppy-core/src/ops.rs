//! Dateioperationen im Hintergrund: Kopieren, Verschieben, Papierkorb, Löschen,
//! Ordner anlegen. Fortschritt und Abbruch laufen über [`Job`].
//!
//! Bestehende Ziele werden nie überschrieben, sondern übersprungen und gemeldet.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Copy,
    Move,
    /// In den Papierkorb.
    Trash,
    /// Endgültig löschen.
    Delete,
}

#[derive(Default)]
struct State {
    total: AtomicU64,
    done: AtomicU64,
    finished: AtomicBool,
    cancel: AtomicBool,
    current: Mutex<String>,
    problems: Mutex<Vec<String>>,
}

/// Laufende Operation; `clone()` ist billig.
#[derive(Clone, Default)]
pub struct Job(Arc<State>);

/// Momentaufnahme für die Anzeige.
#[derive(Debug, Clone)]
pub struct Progress {
    pub total: u64,
    pub done: u64,
    pub current: String,
    pub finished: bool,
    pub problems: Vec<String>,
}

impl Job {
    pub fn progress(&self) -> Progress {
        let s = &self.0;
        Progress {
            total: s.total.load(Ordering::Relaxed),
            done: s.done.load(Ordering::Relaxed),
            current: s.current.lock().unwrap().clone(),
            finished: s.finished.load(Ordering::Relaxed),
            problems: s.problems.lock().unwrap().clone(),
        }
    }

    pub fn cancel(&self) {
        self.0.cancel.store(true, Ordering::Relaxed);
    }

    fn cancelled(&self) -> bool {
        self.0.cancel.load(Ordering::Relaxed)
    }

    fn problem(&self, path: &Path, what: impl std::fmt::Display) {
        self.0.problems.lock().unwrap().push(format!("{}: {what}", path.display()));
    }

    fn set_current(&self, p: &Path) {
        *self.0.current.lock().unwrap() = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    }
}

/// Startet eine Operation auf `sources`; `target` ist der Zielordner (Kopieren/Verschieben).
pub fn start(kind: OpKind, sources: Vec<PathBuf>, target: Option<PathBuf>) -> Job {
    let job = Job::default();
    let j = job.clone();
    std::thread::spawn(move || {
        match kind {
            OpKind::Copy | OpKind::Move => {
                let target = target.expect("Zielordner fehlt");
                // Fortschritt in Bytes: vorher zählen.
                let total = sources.iter().map(|s| tree_size(s)).sum();
                j.0.total.store(total, Ordering::Relaxed);
                for src in &sources {
                    if j.cancelled() {
                        break;
                    }
                    let Some(name) = src.file_name() else { continue };
                    let dst = target.join(name);
                    if dst.exists() || dst.symlink_metadata().is_ok() {
                        j.problem(&dst, "existiert bereits, übersprungen");
                        j.0.done.fetch_add(tree_size(src), Ordering::Relaxed);
                        continue;
                    }
                    if dst.starts_with(src) {
                        j.problem(src, "kann nicht in sich selbst kopiert werden");
                        continue;
                    }
                    let res = if kind == OpKind::Move { move_entry(&j, src, &dst) } else { copy_entry(&j, src, &dst) };
                    if let Err(e) = res {
                        j.problem(src, e);
                    }
                }
            }
            OpKind::Trash | OpKind::Delete => {
                j.0.total.store(sources.len() as u64, Ordering::Relaxed);
                for src in &sources {
                    if j.cancelled() {
                        break;
                    }
                    j.set_current(src);
                    let res = if kind == OpKind::Trash {
                        trash::delete(src).map_err(|e| e.to_string())
                    } else {
                        remove(src).map_err(|e| e.to_string())
                    };
                    if let Err(e) = res {
                        j.problem(src, e);
                    }
                    j.0.done.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        j.0.finished.store(true, Ordering::Relaxed);
    });
    job
}

/// Legt einen neuen Ordner an; Fehler (z. B. existiert schon) gehen an den Aufrufer.
pub fn make_dir(parent: &Path, name: &str) -> io::Result<PathBuf> {
    if name.is_empty() || name.contains(std::path::MAIN_SEPARATOR) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "ungültiger Name"));
    }
    let p = parent.join(name);
    std::fs::create_dir(&p)?;
    Ok(p)
}

fn tree_size(p: &Path) -> u64 {
    let Ok(m) = p.symlink_metadata() else { return 0 };
    if !m.is_dir() {
        return m.len();
    }
    std::fs::read_dir(p)
        .map(|rd| rd.flatten().map(|e| tree_size(&e.path())).sum())
        .unwrap_or(0)
}

fn remove(p: &Path) -> io::Result<()> {
    if p.symlink_metadata()?.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) }
}

fn move_entry(job: &Job, src: &Path, dst: &Path) -> io::Result<()> {
    job.set_current(src);
    // Gleiches Dateisystem: sofort. Sonst kopieren und Quelle entfernen.
    match std::fs::rename(src, dst) {
        Ok(()) => {
            job.0.done.fetch_add(tree_size(dst), Ordering::Relaxed);
            Ok(())
        }
        Err(_) => {
            copy_entry(job, src, dst)?;
            if job.cancelled() {
                return Ok(());
            }
            remove(src)
        }
    }
}

fn copy_entry(job: &Job, src: &Path, dst: &Path) -> io::Result<()> {
    if job.cancelled() {
        return Ok(());
    }
    let m = src.symlink_metadata()?;
    job.set_current(src);
    if m.file_type().is_symlink() {
        #[cfg(unix)]
        return std::os::unix::fs::symlink(std::fs::read_link(src)?, dst);
        #[cfg(not(unix))]
        return std::fs::copy(src, dst).map(|_| ());
    }
    if m.is_dir() {
        std::fs::create_dir(dst)?;
        for e in std::fs::read_dir(src)?.flatten() {
            if let Err(err) = copy_entry(job, &e.path(), &dst.join(e.file_name())) {
                job.problem(&e.path(), err);
            }
        }
        std::fs::set_permissions(dst, m.permissions())?;
        return Ok(());
    }
    // std::fs::copy nutzt unter Linux copy_file_range (auf btrfs/XFS als Reflink).
    std::fs::copy(src, dst)?;
    job.0.done.fetch_add(m.len(), Ordering::Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(job: &Job) -> Progress {
        while !job.progress().finished {
            std::thread::yield_now();
        }
        job.progress()
    }

    #[test]
    fn copy_move_delete() {
        let root = std::env::temp_dir().join(format!("guppy-ops-{}", std::process::id()));
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(a.join("sub")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("sub/f"), "hallo").unwrap();

        let p = wait(&start(OpKind::Copy, vec![a.join("sub")], Some(b.clone())));
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        assert_eq!(std::fs::read_to_string(b.join("sub/f")).unwrap(), "hallo");
        assert_eq!((p.done, p.total), (5, 5));

        // Zweites Kopieren: Ziel existiert, wird übersprungen statt überschrieben.
        let p = wait(&start(OpKind::Copy, vec![a.join("sub")], Some(b.clone())));
        assert_eq!(p.problems.len(), 1);

        std::fs::remove_dir_all(b.join("sub")).unwrap();
        wait(&start(OpKind::Move, vec![a.join("sub")], Some(b.clone())));
        assert!(!a.join("sub").exists() && b.join("sub/f").exists());

        assert!(make_dir(&a, "neu").is_ok());
        assert!(make_dir(&a, "neu").is_err());

        wait(&start(OpKind::Delete, vec![b.join("sub"), a.join("neu")], None));
        assert!(!b.join("sub").exists() && !a.join("neu").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
