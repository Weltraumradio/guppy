//! Remote folders (Google Drive, SFTP, S3, …) through `rclone`.
//!
//! A remote path is the rclone remote followed by the path inside it:
//! `gdrive:` is the root, `gdrive:/Photos/2024` a folder in it. The remotes
//! themselves are set up with `rclone config`; Guppy never sees credentials.

use std::ffi::OsString;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use serde::Deserialize;

use crate::fs::{Entry, EntryKind};

/// The rclone binary; `GUPPY_RCLONE` overrides it (tests, non-standard installs).
fn rclone() -> Command {
    let mut c = Command::new(std::env::var_os("GUPPY_RCLONE").unwrap_or_else(|| OsString::from("rclone")));
    c.stdin(Stdio::null());
    c
}

fn missing(e: io::Error) -> io::Error {
    if e.kind() == io::ErrorKind::NotFound {
        io::Error::new(io::ErrorKind::NotFound, "rclone is not installed")
    } else {
        e
    }
}

/// Is this a remote path (`name:` or `name:/…`)?
pub fn is_remote(path: &Path) -> bool {
    split(path).is_some()
}

/// Splits into the remote (`gdrive:`) and the path inside it, without a leading `/`.
pub fn split(path: &Path) -> Option<(&str, &str)> {
    let s = path.to_str()?;
    let colon = s.find(':')?;
    let (remote, rest) = s.split_at(colon + 1);
    // `/x:y` is a local path; remote names don't contain `/`.
    if colon == 0 || remote.contains('/') {
        return None;
    }
    Some((remote, rest.trim_start_matches('/')))
}

/// The path as rclone wants it: `gdrive:Photos/2024`.
fn spec(path: &Path) -> io::Result<String> {
    let (remote, inner) = split(path).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a remote path"))?;
    Ok(format!("{remote}{inner}"))
}

/// rclone argument for a local or remote path.
fn arg(path: &Path) -> io::Result<OsString> {
    if is_remote(path) { spec(path).map(OsString::from) } else { Ok(path.as_os_str().to_owned()) }
}

/// Configured remotes, e.g. `["gdrive:"]`; empty without rclone.
pub fn remotes() -> Vec<String> {
    let Ok(out) = rclone().arg("listremotes").output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout).lines().map(str::trim).filter(|l| l.ends_with(':')).map(String::from).collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Item {
    name: String,
    size: i64,
    mod_time: Option<String>,
    is_dir: bool,
}

/// Runs rclone and returns stdout, or its last error line as the error.
fn run(cmd: &mut Command) -> io::Result<Vec<u8>> {
    let out = cmd.output().map_err(missing)?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let line = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("rclone failed");
    // Strip rclone's "2026/10/13 12:00:00 ERROR : " prefix.
    let msg = line.split_once(" : ").map_or(line, |(_, m)| m).trim();
    let kind = if msg.contains("not found") { io::ErrorKind::NotFound } else { io::ErrorKind::Other };
    Err(io::Error::new(kind, msg.to_string()))
}

/// Lists a remote folder.
pub fn read_dir(dir: &Path) -> io::Result<Vec<Entry>> {
    let out = run(rclone().args(["lsjson", "--max-depth", "1", "--no-mimetype"]).arg(spec(dir)?))?;
    let items: Vec<Item> = serde_json::from_slice(&out).map_err(io::Error::other)?;
    let mut entries: Vec<Entry> = items
        .into_iter()
        .map(|i| Entry {
            path: dir.join(&i.name),
            hidden: i.name.starts_with('.'),
            kind: if i.is_dir { EntryKind::Dir } else { EntryKind::File },
            size: if i.is_dir { 0 } else { i.size.max(0) as u64 },
            modified: i.mod_time.as_deref().and_then(parse_time),
            name: i.name,
            in_archive: false,
            remote: true,
        })
        .collect();
    crate::fs::sort_entries(&mut entries);
    Ok(entries)
}

fn parse_time(s: &str) -> Option<SystemTime> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(SystemTime::from)
}

/// Does the remote path exist, and is it a folder?
pub fn stat(path: &Path) -> io::Result<Option<bool>> {
    if split(path).is_some_and(|(_, inner)| inner.is_empty()) {
        return Ok(Some(true));
    }
    match run(rclone().args(["lsjson", "--stat", "--no-mimetype", "--no-modtime"]).arg(spec(path)?)) {
        Ok(out) => Ok(serde_json::from_slice::<Item>(&out).ok().map(|i| i.is_dir)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Total size of a local or remote file or folder.
pub fn size(path: &Path) -> u64 {
    #[derive(Deserialize)]
    struct Size {
        bytes: i64,
    }
    let Ok(a) = arg(path) else { return 0 };
    run(rclone().args(["size", "--json"]).arg(a))
        .ok()
        .and_then(|out| serde_json::from_slice::<Size>(&out).ok())
        .map_or(0, |s| s.bytes.max(0) as u64)
}

pub fn make_dir(path: &Path) -> io::Result<()> {
    run(rclone().arg("mkdir").arg(spec(path)?)).map(drop)
}

/// Deletes a file or a whole folder. On Google Drive this goes to Drive's trash.
pub fn delete(path: &Path, is_dir: bool) -> io::Result<()> {
    run(rclone().arg(if is_dir { "purge" } else { "deletefile" }).arg(spec(path)?)).map(drop)
}

/// Copies or moves `src` to exactly `dst` (either may be local or remote; folders
/// are merged). `on_bytes` gets the bytes transferred so far and returns false to cancel.
pub fn transfer(src: &Path, dst: &Path, mv: bool, mut on_bytes: impl FnMut(u64) -> bool) -> io::Result<()> {
    let mut child = rclone()
        .arg(if mv { "moveto" } else { "copyto" })
        .arg(arg(src)?)
        .arg(arg(dst)?)
        .args(["--use-json-log", "--stats", "500ms", "--stats-log-level", "NOTICE"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(missing)?;
    let mut last_error = None;
    for line in io::BufReader::new(child.stderr.take().expect("piped")).lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if v["level"] == "error" {
            last_error = v["msg"].as_str().map(|m| m.trim().to_string());
        }
        if let Some(b) = v["stats"]["bytes"].as_u64()
            && !on_bytes(b)
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
    }
    if child.wait()?.success() {
        Ok(())
    } else {
        Err(io::Error::other(last_error.unwrap_or_else(|| "rclone failed".into())))
    }
}

/// Downloads a remote file into `dir` and returns the local path.
pub fn download(path: &Path, dir: &Path) -> io::Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let local = dir.join(name);
    transfer(path, &local, false, |_| true)?;
    Ok(local)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(split(Path::new("gdrive:")), Some(("gdrive:", "")));
        assert_eq!(split(Path::new("gdrive:/a/b")), Some(("gdrive:", "a/b")));
        assert_eq!(split(&Path::new("gdrive:").join("x")), Some(("gdrive:", "x")));
        assert_eq!(split(Path::new("/home/a:b")), None);
        assert_eq!(split(Path::new("relative")), None);
        assert_eq!(spec(Path::new("gdrive:/a b/c")).unwrap(), "gdrive:a b/c");
        assert_eq!(Path::new("gdrive:/a").parent(), Some(Path::new("gdrive:")));
    }

    /// Runs against a local folder posing as remote `t:`; needs rclone (or `GUPPY_RCLONE`).
    #[test]
    fn with_rclone() {
        if rclone().arg("version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!("guppy-rclone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src/d")).unwrap();
        std::fs::write(root.join("src/d/f.txt"), "hello").unwrap();
        std::fs::create_dir_all(root.join("remote")).unwrap();
        // SAFETY: tests in this module are the only users of these variables.
        unsafe {
            std::env::set_var("RCLONE_CONFIG_T_TYPE", "alias");
            std::env::set_var("RCLONE_CONFIG_T_REMOTE", root.join("remote"));
        }
        let r = PathBuf::from("t:");
        assert!(is_remote(&r));
        assert!(remotes().contains(&"t:".to_string()));

        transfer(&root.join("src/d"), &r.join("d"), false, |_| true).unwrap();
        let list = read_dir(&r).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].is_dir() && list[0].remote && list[0].path == r.join("d"));
        let inner = read_dir(&r.join("d")).unwrap();
        assert_eq!((inner[0].name.as_str(), inner[0].size), ("f.txt", 5));
        assert!(inner[0].modified.is_some());
        assert_eq!(stat(&r.join("d")).unwrap(), Some(true));
        assert_eq!(stat(&r.join("d/f.txt")).unwrap(), Some(false));
        assert_eq!(stat(&r.join("nope")).unwrap(), None);
        assert_eq!(size(&r.join("d")), 5);

        make_dir(&r.join("new")).unwrap();
        transfer(&r.join("d/f.txt"), &r.join("new/g.txt"), true, |_| true).unwrap();
        assert_eq!(stat(&r.join("d/f.txt")).unwrap(), None);
        let local = download(&r.join("new/g.txt"), &root).unwrap();
        assert_eq!(std::fs::read_to_string(local).unwrap(), "hello");
        delete(&r.join("new"), true).unwrap();
        assert_eq!(stat(&r.join("new")).unwrap(), None);
        assert!(read_dir(&r.join("gone")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
