//! Background file operations: copy, move, trash, delete, create directory.
//! Progress and cancellation go through [`Job`].
//!
//! When a target already exists, the job pauses and asks (see [`Job::conflict`]);
//! the answer can hold for the rest of the job. Copying out of an archive
//! extracts it.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Copy,
    Move,
    /// Move to trash.
    Trash,
    /// Delete permanently.
    Delete,
}

/// What to do when the target already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolve {
    /// Files are replaced, folders merged.
    Overwrite,
    Skip,
    /// The new entry gets a free name, e.g. `a (2).txt`.
    KeepBoth,
}

/// A target that already exists, waiting for an answer.
#[derive(Debug, Clone)]
pub struct Conflict {
    pub src: PathBuf,
    pub dst: PathBuf,
}

#[derive(Default)]
struct State {
    total: AtomicU64,
    done: AtomicU64,
    finished: AtomicBool,
    cancel: AtomicBool,
    current: Mutex<String>,
    problems: Mutex<Vec<String>>,
    /// Open question and its answer; `wake` signals both answers and cancelling.
    asking: Mutex<Option<Conflict>>,
    answer: Mutex<Option<Resolve>>,
    /// Answer given "for all".
    policy: Mutex<Option<Resolve>>,
    wake: Condvar,
    /// What was done, for [`undo`].
    steps: Mutex<Vec<Step>>,
}

/// One completed change that [`undo`] can reverse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Moved or renamed.
    Moved { from: PathBuf, to: PathBuf },
    /// Moved to the trash from this path.
    Trashed(PathBuf),
}

/// Running operation; `clone()` is cheap.
#[derive(Clone, Default)]
pub struct Job(Arc<State>);

/// Snapshot for display.
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

    /// The changes made so far (moves and trashing; copies are not undone).
    pub fn steps(&self) -> Vec<Step> {
        self.0.steps.lock().unwrap().clone()
    }

    fn step(&self, s: Step) {
        self.0.steps.lock().unwrap().push(s);
    }

    pub fn cancel(&self) {
        self.0.cancel.store(true, Ordering::Relaxed);
        let _guard = self.0.answer.lock().unwrap();
        self.0.wake.notify_all();
    }

    /// The question the job is waiting on, if any.
    pub fn conflict(&self) -> Option<Conflict> {
        self.0.asking.lock().unwrap().clone()
    }

    /// Answers the open question; with `all`, the same answer holds for every later one.
    pub fn answer(&self, r: Resolve, all: bool) {
        if all {
            *self.0.policy.lock().unwrap() = Some(r);
        }
        let mut answer = self.0.answer.lock().unwrap();
        // Cleared here, so the question is gone as soon as it is answered.
        *self.0.asking.lock().unwrap() = None;
        *answer = Some(r);
        self.0.wake.notify_all();
    }

    fn cancelled(&self) -> bool {
        self.0.cancel.load(Ordering::Relaxed)
    }

    fn problem(&self, path: &Path, what: impl std::fmt::Display) {
        self.0.problems.lock().unwrap().push(format!("{}: {what}", path.display()));
    }

    fn problem_count(&self) -> usize {
        self.0.problems.lock().unwrap().len()
    }

    fn set_current(&self, p: &Path) {
        *self.0.current.lock().unwrap() = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    }

    fn add_done(&self, n: u64) {
        self.0.done.fetch_add(n, Ordering::Relaxed);
    }

    /// `dst` exists: asks (or uses the "for all" answer). Cancelling counts as skip.
    fn resolve(&self, src: &Path, dst: &Path) -> Resolve {
        if let Some(r) = *self.0.policy.lock().unwrap() {
            return r;
        }
        let mut answer = self.0.answer.lock().unwrap();
        *answer = None;
        *self.0.asking.lock().unwrap() = Some(Conflict { src: src.to_path_buf(), dst: dst.to_path_buf() });
        while answer.is_none() && !self.cancelled() {
            answer = self.0.wake.wait(answer).unwrap();
        }
        *self.0.asking.lock().unwrap() = None;
        answer.take().unwrap_or(Resolve::Skip)
    }
}

/// Starts an operation on `sources`; `target` is the destination directory (copy/move).
/// Sources inside an archive (see [`crate::archive`]) can only be copied: they are extracted.
pub fn start(kind: OpKind, sources: Vec<PathBuf>, target: Option<PathBuf>) -> Job {
    let job = Job::default();
    let j = job.clone();
    std::thread::spawn(move || {
        match kind {
            OpKind::Copy | OpKind::Move => {
                let target = target.expect("target directory missing");
                let archived = sources.first().filter(|s| s.symlink_metadata().is_err()).and_then(|s| crate::archive::split(s));
                if let Some((archive, _)) = archived {
                    extract(&j, &archive, &sources, &target);
                } else {
                    // Progress in bytes: count up front.
                    let total = sources.iter().map(|s| tree_size(s)).sum();
                    j.0.total.store(total, Ordering::Relaxed);
                    for src in &sources {
                        if j.cancelled() {
                            break;
                        }
                        let Some(name) = src.file_name() else { continue };
                        let dst = target.join(name);
                        if dst != *src && dst.starts_with(src) {
                            j.problem(src, "cannot be copied into itself");
                            continue;
                        }
                        let res = if kind == OpKind::Move { move_entry(&j, src, &dst) } else { copy_entry(&j, src, &dst) };
                        if let Err(e) = res {
                            j.problem(src, e);
                        }
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
                    match res {
                        Ok(()) if kind == OpKind::Trash => j.step(Step::Trashed(src.clone())),
                        Ok(()) => {}
                        Err(e) => j.problem(src, e),
                    }
                    j.add_done(1);
                }
            }
        }
        j.0.finished.store(true, Ordering::Relaxed);
    });
    job
}

/// Reverses `steps`, last first: moves entries back and restores trashed ones.
/// A move back asks like any other move if the old place is taken again.
pub fn undo(steps: Vec<Step>) -> Job {
    let job = Job::default();
    let j = job.clone();
    std::thread::spawn(move || {
        j.0.total.store(steps.iter().map(|s| if let Step::Moved { to, .. } = s { tree_size(to) } else { 0 }).sum(), Ordering::Relaxed);
        let mut trashed = Vec::new();
        for s in steps.into_iter().rev() {
            if j.cancelled() {
                break;
            }
            match s {
                Step::Moved { from, to } => {
                    // A merged folder was removed after its contents moved out.
                    if let Some(parent) = from.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Err(e) = move_entry(&j, &to, &from) {
                        j.problem(&to, e);
                    }
                }
                Step::Trashed(p) => trashed.push(p),
            }
        }
        if !trashed.is_empty() && !j.cancelled() {
            restore(&j, &trashed);
        }
        j.0.finished.store(true, Ordering::Relaxed);
    });
    job
}

/// Takes the most recently trashed item for each path back out of the trash.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn restore(job: &Job, paths: &[PathBuf]) {
    let items = match trash::os_limited::list() {
        Ok(items) => items,
        Err(e) => return job.problem(Path::new("trash"), e),
    };
    let mut pick = Vec::new();
    for p in paths {
        match items.iter().filter(|i| i.original_path() == *p).max_by_key(|i| i.time_deleted) {
            Some(_) if p.symlink_metadata().is_ok() => job.problem(p, "something with that name exists again, left in the trash"),
            Some(i) => pick.push(i.clone()),
            None => job.problem(p, "no longer in the trash"),
        }
    }
    if let Err(e) = trash::os_limited::restore_all(pick) {
        job.problem(Path::new("trash"), e);
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn restore(job: &Job, paths: &[PathBuf]) {
    for p in paths {
        job.problem(p, "restoring from the trash is not supported here");
    }
}

/// Extracts archive members into a hidden folder inside `target` (same filesystem),
/// then moves them into place, which brings the usual conflict handling.
fn extract(job: &Job, archive: &Path, sources: &[PathBuf], target: &Path) {
    let members: Vec<PathBuf> = sources.iter().filter_map(|s| s.strip_prefix(archive).ok().map(Path::to_path_buf)).collect();
    // Bytes count twice: once unpacked, once moved into place.
    let total: u64 = members.iter().map(|m| crate::archive::size_of(archive, m)).sum();
    job.0.total.store(2 * total, Ordering::Relaxed);
    let tmp = target.join(format!(".guppy-extract-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir(&tmp) {
        job.problem(target, e);
        return;
    }
    let res = crate::archive::extract(archive, &members, &tmp, |name, size| {
        *job.0.current.lock().unwrap() = name.rsplit('/').next().unwrap_or(name).to_string();
        job.add_done(size);
        !job.cancelled()
    });
    match res {
        Err(e) => job.problem(archive, e),
        Ok(()) if job.cancelled() => {}
        Ok(()) => {
            for m in &members {
                let Some(name) = m.file_name() else { continue };
                if let Err(e) = move_entry(job, &tmp.join(m), &target.join(name)) {
                    job.problem(&target.join(name), e);
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Renames `from` within its folder; refuses to overwrite an existing entry.
pub fn rename(from: &Path, name: &str) -> io::Result<PathBuf> {
    if name.is_empty() || name.contains(std::path::MAIN_SEPARATOR) || name == "." || name == ".." {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid name"));
    }
    let to = from.with_file_name(name);
    if to == from {
        return Ok(to);
    }
    // Only a change of case may hit "itself" on case-insensitive file systems.
    let same_entry = to.to_string_lossy().to_lowercase() == from.to_string_lossy().to_lowercase();
    if to.symlink_metadata().is_ok() && !same_entry {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "a file with that name already exists"));
    }
    std::fs::rename(from, &to)?;
    Ok(to)
}

/// Creates a new directory; errors (e.g. already exists) go to the caller.
pub fn make_dir(parent: &Path, name: &str) -> io::Result<PathBuf> {
    if name.is_empty() || name.contains(std::path::MAIN_SEPARATOR) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid name"));
    }
    let p = parent.join(name);
    std::fs::create_dir(&p)?;
    Ok(p)
}

/// First free name next to `p`: `a (2).txt`, `a (3).txt`, … (folders without extension split).
pub fn free_name(p: &Path) -> PathBuf {
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 && !p.is_dir() => (&name[..dot], &name[dot..]),
        _ => (name.as_str(), ""),
    };
    (2..)
        .map(|n| p.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|c| c.symlink_metadata().is_err())
        .expect("some name is free")
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

/// Where an entry goes once a possible conflict is settled.
enum Place {
    /// Write to this path (free, or the old entry was removed).
    New(PathBuf),
    /// Both are folders: merge into the existing one.
    Merge,
    Skip,
}

/// Settles a conflict at `dst`. Copying onto itself always keeps both.
fn place(job: &Job, src: &Path, dst: &Path, src_dir: bool) -> io::Result<Place> {
    let Ok(dm) = dst.symlink_metadata() else { return Ok(Place::New(dst.to_path_buf())) };
    if src == dst {
        return Ok(Place::New(free_name(dst)));
    }
    match job.resolve(src, dst) {
        Resolve::Skip => Ok(Place::Skip),
        Resolve::KeepBoth => Ok(Place::New(free_name(dst))),
        Resolve::Overwrite if src_dir && dm.is_dir() => Ok(Place::Merge),
        Resolve::Overwrite if src_dir || dm.is_dir() => {
            Err(io::Error::other("a file and a folder have the same name, not overwritten"))
        }
        Resolve::Overwrite => {
            std::fs::remove_file(dst)?;
            Ok(Place::New(dst.to_path_buf()))
        }
    }
}

fn move_entry(job: &Job, src: &Path, dst: &Path) -> io::Result<()> {
    if job.cancelled() {
        return Ok(());
    }
    job.set_current(src);
    let is_dir = src.symlink_metadata()?.is_dir();
    let dst = match place(job, src, dst, is_dir)? {
        Place::Skip => {
            job.add_done(tree_size(src));
            return Ok(());
        }
        // Move the contents one by one; what is skipped stays in the source.
        Place::Merge => {
            for e in std::fs::read_dir(src)?.flatten() {
                if let Err(err) = move_entry(job, &e.path(), &dst.join(e.file_name())) {
                    job.problem(&e.path(), err);
                }
            }
            let _ = std::fs::remove_dir(src);
            return Ok(());
        }
        Place::New(p) => p,
    };
    // Same filesystem: instant. Otherwise copy and remove the source.
    match std::fs::rename(src, &dst) {
        Ok(()) => job.add_done(tree_size(&dst)),
        Err(_) => {
            let before = job.problem_count();
            copy_entry(job, src, &dst)?;
            // Keep the source if anything inside did not make it.
            if job.cancelled() || job.problem_count() > before {
                return Ok(());
            }
            remove(src)?;
        }
    }
    job.step(Step::Moved { from: src.to_path_buf(), to: dst });
    Ok(())
}

fn copy_entry(job: &Job, src: &Path, dst: &Path) -> io::Result<()> {
    if job.cancelled() {
        return Ok(());
    }
    let m = src.symlink_metadata()?;
    job.set_current(src);
    let (dst, merge) = match place(job, src, dst, m.is_dir())? {
        Place::Skip => {
            job.add_done(tree_size(src));
            return Ok(());
        }
        Place::Merge => (dst.to_path_buf(), true),
        Place::New(p) => (p, false),
    };
    if m.file_type().is_symlink() {
        #[cfg(unix)]
        return std::os::unix::fs::symlink(std::fs::read_link(src)?, &dst);
        #[cfg(not(unix))]
        return std::fs::copy(src, &dst).map(|_| ());
    }
    if m.is_dir() {
        if !merge {
            std::fs::create_dir(&dst)?;
        }
        for e in std::fs::read_dir(src)?.flatten() {
            if let Err(err) = copy_entry(job, &e.path(), &dst.join(e.file_name())) {
                job.problem(&e.path(), err);
            }
        }
        std::fs::set_permissions(&dst, m.permissions())?;
        return Ok(());
    }
    // On Linux std::fs::copy uses copy_file_range (a reflink on btrfs/XFS).
    std::fs::copy(src, &dst)?;
    job.add_done(m.len());
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

    /// Waits for the next question and answers it.
    fn answer(job: &Job, r: Resolve, all: bool) -> Conflict {
        loop {
            if let Some(c) = job.conflict() {
                job.answer(r, all);
                return c;
            }
            std::thread::yield_now();
        }
    }

    #[test]
    fn copy_move_delete() {
        let root = std::env::temp_dir().join(format!("guppy-ops-{}", std::process::id()));
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(a.join("sub")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("sub/f"), "hello").unwrap();

        let p = wait(&start(OpKind::Copy, vec![a.join("sub")], Some(b.clone())));
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        assert_eq!(std::fs::read_to_string(b.join("sub/f")).unwrap(), "hello");
        assert_eq!((p.done, p.total), (5, 5));

        // Second copy: the target exists, the job asks; skip leaves it alone.
        let job = start(OpKind::Copy, vec![a.join("sub")], Some(b.clone()));
        assert_eq!(answer(&job, Resolve::Skip, false).dst, b.join("sub"));
        assert!(wait(&job).problems.is_empty());

        // Keep both: a second folder next to it.
        let job = start(OpKind::Copy, vec![a.join("sub")], Some(b.clone()));
        answer(&job, Resolve::KeepBoth, false);
        wait(&job);
        assert!(b.join("sub (2)/f").exists());

        // Overwrite merges folders and asks again for the file inside.
        std::fs::write(a.join("sub/f"), "new").unwrap();
        std::fs::write(a.join("sub/g"), "g").unwrap();
        let job = start(OpKind::Copy, vec![a.join("sub")], Some(b.clone()));
        answer(&job, Resolve::Overwrite, false);
        assert_eq!(answer(&job, Resolve::Overwrite, false).dst, b.join("sub/f"));
        assert!(wait(&job).problems.is_empty());
        assert_eq!(std::fs::read_to_string(b.join("sub/f")).unwrap(), "new");
        assert!(b.join("sub/g").exists());

        // Move with "skip for all": nothing moves, the source stays complete.
        let job = start(OpKind::Move, vec![a.join("sub")], Some(b.clone()));
        answer(&job, Resolve::Overwrite, false);
        answer(&job, Resolve::Skip, true);
        wait(&job);
        assert!(a.join("sub/f").exists() && a.join("sub/g").exists());

        std::fs::remove_dir_all(b.join("sub")).unwrap();
        wait(&start(OpKind::Move, vec![a.join("sub")], Some(b.clone())));
        assert!(!a.join("sub").exists() && b.join("sub/f").exists());

        // Pasting into the same folder duplicates.
        wait(&start(OpKind::Copy, vec![b.join("sub/f")], Some(b.join("sub"))));
        assert!(b.join("sub/f (2)").exists());

        // Cancelling while asked stops the job.
        std::fs::create_dir(a.join("sub")).unwrap();
        let job = start(OpKind::Copy, vec![b.join("sub")], Some(a.clone()));
        while job.conflict().is_none() {
            std::thread::yield_now();
        }
        job.cancel();
        assert!(wait(&job).finished);
        std::fs::remove_dir(a.join("sub")).unwrap();

        assert!(make_dir(&a, "new").is_ok());
        assert!(make_dir(&a, "new").is_err());

        let renamed = rename(&a.join("new"), "renamed").unwrap();
        assert!(renamed.is_dir() && !a.join("new").exists());
        std::fs::create_dir(a.join("new")).unwrap();
        assert!(rename(&a.join("new"), "renamed").is_err());
        std::fs::remove_dir(&renamed).unwrap();

        wait(&start(OpKind::Delete, vec![b.join("sub"), a.join("new")], None));
        assert!(!b.join("sub").exists() && !a.join("new").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn undo_moves() {
        let root = std::env::temp_dir().join(format!("guppy-ops-u-{}", std::process::id()));
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(a.join("d")).unwrap();
        std::fs::create_dir_all(b.join("d")).unwrap();
        std::fs::write(a.join("d/f"), "f").unwrap();
        std::fs::write(a.join("x"), "x").unwrap();

        // `d` merges into the existing folder, `x` moves plainly.
        let job = start(OpKind::Move, vec![a.join("d"), a.join("x")], Some(b.clone()));
        answer(&job, Resolve::Overwrite, true);
        wait(&job);
        assert!(!a.join("d").exists() && b.join("d/f").exists());
        let steps = job.steps();
        assert_eq!(steps.len(), 2, "{steps:?}");

        let p = wait(&undo(steps));
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        assert_eq!(std::fs::read_to_string(a.join("d/f")).unwrap(), "f");
        assert!(a.join("x").exists() && !b.join("x").exists() && !b.join("d/f").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn extract_from_archive() {
        if std::process::Command::new("bsdtar").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!("guppy-ops-x-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src/d")).unwrap();
        std::fs::write(root.join("src/d/f"), "abc").unwrap();
        let tgz = root.join("t.tar.gz");
        std::process::Command::new("bsdtar").arg("-czf").arg(&tgz).arg("-C").arg(root.join("src")).arg(".").status().unwrap();
        let out = root.join("out");
        std::fs::create_dir_all(out.join("d")).unwrap();
        std::fs::write(out.join("d/f"), "old").unwrap();

        let job = start(OpKind::Copy, vec![tgz.join("d")], Some(out.clone()));
        answer(&job, Resolve::Overwrite, true);
        let p = wait(&job);
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        assert_eq!(std::fs::read_to_string(out.join("d/f")).unwrap(), "abc");
        assert_eq!(p.done, p.total);
        // The temporary folder is gone.
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
}
