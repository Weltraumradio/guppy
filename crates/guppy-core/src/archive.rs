//! Archives as read-only folders. Reading and extracting goes through `bsdtar`
//! (libarchive, part of every Arch system), which knows zip, tar.*, 7z, rar, iso.
//!
//! A path inside an archive is the archive's path joined with the member's path,
//! e.g. `/home/kc/x.zip/docs/a.txt`; [`split`] takes it apart again.

use std::collections::HashMap;
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::fs::{Entry, EntryKind};

const BSDTAR: &str = "bsdtar";

/// Does the file name look like an archive `bsdtar` can list?
/// Plain compressed files (`x.gz`) are not archives.
pub fn is_archive_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    const EXT: &[&str] = &[
        ".zip", ".jar", ".apk", ".tar", ".tgz", ".tbz", ".tbz2", ".txz", ".tzst", ".7z", ".rar", ".iso", ".cpio", ".cbz",
        ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar.lz4", ".tar.lzma",
    ];
    EXT.iter().any(|e| n.ends_with(e) && n.len() > e.len())
}

/// Splits a path into the archive file and the member path inside it.
/// `None` if no ancestor is an archive file (or the path is a real directory).
pub fn split(path: &Path) -> Option<(PathBuf, PathBuf)> {
    if path.is_dir() {
        return None;
    }
    for a in path.ancestors() {
        let name = a.file_name()?.to_string_lossy();
        if is_archive_name(&name) && a.is_file() {
            let inner = path.strip_prefix(a).ok()?.to_path_buf();
            return Some((a.to_path_buf(), inner));
        }
    }
    None
}

/// One member as `bsdtar -tv` lists it.
#[derive(Debug, Clone)]
struct Member {
    /// Normalized path without a leading `./` or `/`.
    path: PathBuf,
    kind: EntryKind,
    size: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug)]
struct Listing {
    /// What member names start with in the archive (`./`, `/` or nothing).
    prefix: String,
    members: Vec<Member>,
}

/// Listings by archive, valid while size and mtime stay the same.
type Cache = HashMap<PathBuf, (u64, Option<SystemTime>, Arc<Listing>)>;
static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

fn listing(archive: &Path) -> io::Result<Arc<Listing>> {
    let meta = std::fs::metadata(archive)?;
    let stamp = (meta.len(), meta.modified().ok());
    if let Some((len, mtime, l)) = CACHE.lock().unwrap().get_or_insert_default().get(archive)
        && (*len, *mtime) == stamp
    {
        return Ok(l.clone());
    }
    let out = Command::new(BSDTAR)
        .env("LC_ALL", "C")
        .args(["-tv", "--numeric-owner", "-f"])
        .arg(archive)
        .stdin(Stdio::null())
        .output()
        .map_err(missing)?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("not readable").to_string();
        return Err(io::Error::other(msg.trim_start_matches("bsdtar: ").to_string()));
    }
    let l = Arc::new(parse(&String::from_utf8_lossy(&out.stdout)));
    CACHE.lock().unwrap().get_or_insert_default().insert(archive.to_path_buf(), (stamp.0, stamp.1, l.clone()));
    Ok(l)
}

fn missing(e: io::Error) -> io::Error {
    if e.kind() == io::ErrorKind::NotFound {
        io::Error::other("bsdtar is not installed (package libarchive)")
    } else {
        e
    }
}

/// Parses `bsdtar -tv` output (C locale):
/// `-rw-r--r--  0 1000   1000        3 Oct 13 12:17 a/sub/f.txt`
fn parse(out: &str) -> Listing {
    let mut prefix = None;
    let mut members = Vec::new();
    for line in out.lines() {
        // Eight whitespace-separated fields, then the name (which may contain spaces).
        let mut rest = line;
        let mut fields = Vec::with_capacity(8);
        for _ in 0..8 {
            rest = rest.trim_start();
            let end = rest.find(' ').unwrap_or(rest.len());
            fields.push(&rest[..end]);
            rest = &rest[end..];
        }
        let Some(raw) = rest.strip_prefix(' ') else { continue };
        let kind = match fields[0].chars().next() {
            Some('d') => EntryKind::Dir,
            Some('l') => EntryKind::Symlink,
            _ => EntryKind::File,
        };
        let raw = match kind {
            EntryKind::Symlink => raw.split(" -> ").next().unwrap_or(raw),
            _ => raw.split(" link to ").next().unwrap_or(raw),
        };
        let p = if raw.starts_with("./") { "./" } else if raw.starts_with('/') { "/" } else { "" };
        prefix.get_or_insert(p.to_string());
        let name = raw[p.len()..].trim_end_matches('/');
        if name.is_empty() || name == "." {
            continue;
        }
        members.push(Member {
            path: PathBuf::from(name),
            kind,
            size: fields[4].parse().unwrap_or(0),
            modified: parse_date(fields[5], fields[6], fields[7]),
        });
    }
    Listing { prefix: prefix.unwrap_or_default(), members }
}

/// `Oct 13 12:17` (within the last half year) or `Oct 13  2023`.
fn parse_date(mon: &str, day: &str, time_or_year: &str) -> Option<SystemTime> {
    use chrono::{Datelike, Local, NaiveDate, TimeZone};
    let m = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|x| *x == mon)? as u32 + 1;
    let d: u32 = day.parse().ok()?;
    let now = Local::now();
    let (year, h, min) = match time_or_year.split_once(':') {
        Some((h, mi)) => {
            let y = now.year();
            // A date "in the future" belongs to last year.
            let y = if (m, d) > (now.month(), now.day()) { y - 1 } else { y };
            (y, h.parse().ok()?, mi.parse().ok()?)
        }
        None => (time_or_year.parse().ok()?, 0, 0),
    };
    let naive = NaiveDate::from_ymd_opt(year, m, d)?.and_hms_opt(h, min, 0)?;
    Local.from_local_datetime(&naive).earliest().map(SystemTime::from)
}

/// The entries directly inside `inner` (empty = archive root), sorted like folders on disk.
/// Folders only implied by member paths are listed too; their size is the sum of their contents.
pub fn read_dir(archive: &Path, inner: &Path) -> io::Result<Vec<Entry>> {
    let l = listing(archive)?;
    let base = archive.join(inner);
    let mut found: HashMap<String, Entry> = HashMap::new();
    let mut any = inner.as_os_str().is_empty();
    for m in &l.members {
        let Ok(rel) = m.path.strip_prefix(inner) else { continue };
        let mut parts = rel.components();
        let Some(first) = parts.next() else {
            any |= m.kind == EntryKind::Dir;
            continue;
        };
        any = true;
        let name = first.as_os_str().to_string_lossy().into_owned();
        let direct = parts.next().is_none();
        let e = found.entry(name.clone()).or_insert_with(|| Entry {
            path: base.join(&name),
            hidden: name.starts_with('.'),
            name,
            kind: EntryKind::Dir,
            size: 0,
            modified: None,
            in_archive: true,
        });
        if direct {
            e.kind = m.kind;
            e.modified = m.modified;
        }
        if m.kind != EntryKind::Dir {
            e.size += m.size;
        }
    }
    if !any {
        return Err(io::Error::new(io::ErrorKind::NotFound, "not in the archive"));
    }
    let mut entries: Vec<Entry> = found.into_values().collect();
    crate::fs::sort_entries(&mut entries);
    Ok(entries)
}

/// Total size of the files at or below `inner`.
pub fn size_of(archive: &Path, inner: &Path) -> u64 {
    listing(archive).map_or(0, |l| {
        l.members.iter().filter(|m| m.kind != EntryKind::Dir && m.path.starts_with(inner)).map(|m| m.size).sum()
    })
}

/// A member name as `bsdtar` wants it on the command line: with the archive's prefix
/// and wildcards escaped (arguments are patterns).
fn pattern(l: &Listing, inner: &Path) -> String {
    let mut s = l.prefix.clone();
    for c in inner.to_string_lossy().chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s
}

/// Extracts `members` (paths inside the archive) with their full path into `dir`.
/// `on_file` gets each extracted member's size; returning `false` aborts.
pub fn extract(archive: &Path, members: &[PathBuf], dir: &Path, mut on_file: impl FnMut(&str, u64) -> bool) -> io::Result<()> {
    let l = listing(archive)?;
    let sizes: HashMap<String, u64> = l.members.iter().map(|m| (m.path.to_string_lossy().into_owned(), m.size)).collect();
    let mut child = Command::new(BSDTAR)
        .env("LC_ALL", "C")
        .args(["-xv", "--no-same-owner", "-f"])
        .arg(archive)
        .arg("-C")
        .arg(dir)
        .arg("--")
        .args(members.iter().map(|m| pattern(&l, m)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(missing)?;
    // `-v` reports each member on stderr as `x name`; errors come on the same stream.
    let mut errors = Vec::new();
    let mut aborted = false;
    for line in io::BufReader::new(child.stderr.take().expect("piped")).lines().map_while(Result::ok) {
        match line.strip_prefix("x ") {
            Some(name) => {
                let name = name.trim_start_matches("./").trim_start_matches('/').trim_end_matches('/');
                if !on_file(name, sizes.get(name).copied().unwrap_or(0)) {
                    let _ = child.kill();
                    aborted = true;
                    break;
                }
            }
            None => errors.push(line.trim_start_matches("bsdtar: ").to_string()),
        }
    }
    let status = child.wait()?;
    if !aborted && !status.success() {
        return Err(io::Error::other(errors.into_iter().next().unwrap_or_else(|| "extraction failed".into())));
    }
    Ok(())
}

/// Reads up to `max` bytes of one member, for the viewer.
pub fn read_member(archive: &Path, inner: &Path, max: usize) -> io::Result<Vec<u8>> {
    let l = listing(archive)?;
    let mut child = Command::new(BSDTAR)
        .args(["-xOf"])
        .arg(archive)
        .arg("--")
        .arg(pattern(&l, inner))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(missing)?;
    let mut buf = Vec::new();
    child.stdout.take().expect("piped").take(max as u64).read_to_end(&mut buf)?;
    let _ = child.kill();
    let _ = child.wait();
    Ok(buf)
}

/// Reads a folder on disk or inside an archive.
pub fn read_path(path: &Path) -> io::Result<Vec<Entry>> {
    if path.is_dir() {
        return crate::fs::read_dir(path);
    }
    match split(path) {
        Some((archive, inner)) => read_dir(&archive, &inner),
        None => crate::fs::read_dir(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(is_archive_name("a.ZIP") && is_archive_name("x.tar.gz") && is_archive_name("b.tgz"));
        assert!(!is_archive_name("x.gz") && !is_archive_name(".zip") && !is_archive_name("notes.txt"));
    }

    #[test]
    fn parses_listing() {
        let l = parse(
            "drwxr-xr-x  0 1000   1000        0 Oct 13 12:17 ./a/\n\
             lrwxrwxrwx  0 1000   1000        0 Oct 13 12:17 ./a/l -> f.txt\n\
             -rw-r--r--  0 1000   1000        3 Jan  2  2023 ./a/my file.txt\n",
        );
        assert_eq!(l.prefix, "./");
        assert_eq!(l.members.len(), 3);
        assert_eq!(l.members[1].path, PathBuf::from("a/l"));
        assert_eq!(l.members[1].kind, EntryKind::Symlink);
        assert_eq!(l.members[2].path, PathBuf::from("a/my file.txt"));
        assert_eq!(l.members[2].size, 3);
        assert!(l.members[2].modified.is_some());
        assert_eq!(pattern(&l, Path::new("a/[x]*")), "./a/\\[x\\]\\*");
    }

    #[test]
    fn browse_and_extract() {
        if Command::new(BSDTAR).arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!("guppy-arch-{}", std::process::id()));
        let src = root.join("src");
        std::fs::create_dir_all(src.join("d/e")).unwrap();
        std::fs::write(src.join("d/e/f.txt"), "hello").unwrap();
        std::fs::write(src.join("top.txt"), "x").unwrap();
        let zip = root.join("t.zip");
        // No explicit folder entries: folders are implied by member paths.
        let ok = Command::new(BSDTAR).args(["--format", "zip", "-cf"]).arg(&zip).arg("-C").arg(&src).args(["d/e/f.txt", "top.txt"]).status().unwrap();
        assert!(ok.success());

        let rootlist = read_path(&zip).unwrap();
        assert_eq!(rootlist.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["d", "top.txt"]);
        assert!(rootlist[0].is_dir() && rootlist[0].size == 5 && rootlist[0].in_archive);
        let inner = read_path(&zip.join("d/e")).unwrap();
        assert_eq!(inner[0].path, zip.join("d/e/f.txt"));
        assert_eq!(split(&zip.join("d/e/f.txt")), Some((zip.clone(), PathBuf::from("d/e/f.txt"))));
        assert!(read_path(&zip.join("nope")).is_err());
        assert_eq!(read_member(&zip, Path::new("d/e/f.txt"), 3).unwrap(), b"hel");
        assert_eq!(size_of(&zip, Path::new("d")), 5);

        let out = root.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let mut seen = 0;
        extract(&zip, &[PathBuf::from("d")], &out, |_, n| {
            seen += n;
            true
        })
        .unwrap();
        assert_eq!(std::fs::read_to_string(out.join("d/e/f.txt")).unwrap(), "hello");
        assert!(!out.join("top.txt").exists());
        assert_eq!(seen, 5);
        std::fs::remove_dir_all(root).unwrap();
    }
}
