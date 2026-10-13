//! Permissions, owner and symbolic links (Unix).

use std::io;
use std::path::{Path, PathBuf};

/// Permission bits (`0o755`) and `user:group` of `path`, as shown for editing.
#[cfg(unix)]
pub fn read(path: &Path) -> io::Result<(u32, String)> {
    use std::os::unix::fs::MetadataExt;
    let m = path.symlink_metadata()?;
    let user = user_name(m.uid()).unwrap_or_else(|| m.uid().to_string());
    let group = group_name(m.gid()).unwrap_or_else(|| m.gid().to_string());
    Ok((m.mode() & 0o7777, format!("{user}:{group}")))
}

#[cfg(not(unix))]
pub fn read(_: &Path) -> io::Result<(u32, String)> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "not supported on this system"))
}

/// `rwxr-xr-x` for `0o755`.
pub fn symbolic(mode: u32) -> String {
    (0..9).map(|i| if mode & (0o400 >> i) != 0 { ['r', 'w', 'x'][i % 3] } else { '-' }).collect()
}

/// Octal permissions like `755` or `0644`.
pub fn parse_mode(s: &str) -> Option<u32> {
    let s = s.trim();
    (!s.is_empty() && s.len() <= 4).then(|| u32::from_str_radix(s, 8).ok()).flatten()
}

/// What [`apply`] changes; `None` leaves it as it is.
#[derive(Debug, Clone, Default)]
pub struct Change {
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    /// Also everything inside folders. Files there only keep execute bits they already had.
    pub recursive: bool,
}

/// Parses `user`, `user:group`, `:group` (names or numbers) into ids.
#[cfg(unix)]
pub fn parse_owner(s: &str) -> Result<(Option<u32>, Option<u32>), String> {
    let (u, g) = s.trim().split_once(':').unwrap_or((s.trim(), ""));
    let id = |name: &str, lookup: fn(&str) -> Option<u32>, what: &str| -> Result<Option<u32>, String> {
        if name.is_empty() {
            return Ok(None);
        }
        name.parse().ok().or_else(|| lookup(name)).map(Some).ok_or(format!("unknown {what} \"{name}\""))
    };
    Ok((id(u, user_id, "user")?, id(g, group_id, "group")?))
}

#[cfg(not(unix))]
pub fn parse_owner(_: &str) -> Result<(Option<u32>, Option<u32>), String> {
    Err("not supported on this system".into())
}

/// Applies `change` to every path; returns the problems.
pub fn apply(paths: &[PathBuf], change: &Change) -> Vec<String> {
    let mut problems = Vec::new();
    for p in paths {
        apply_one(p, change, true, &mut problems);
    }
    problems
}

#[cfg(unix)]
fn apply_one(p: &Path, c: &Change, top: bool, problems: &mut Vec<String>) {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let Ok(m) = p.symlink_metadata() else { return problems.push(format!("{}: not found", p.display())) };
    let mut fail = |e: io::Error| problems.push(format!("{}: {e}", p.display()));
    if c.uid.is_some() || c.gid.is_some() {
        if let Err(e) = std::os::unix::fs::lchown(p, c.uid, c.gid) {
            fail(e);
        }
    }
    // Links have no permissions of their own.
    if let Some(mode) = c.mode.filter(|_| !m.file_type().is_symlink()) {
        let mode = if top || m.is_dir() || m.mode() & 0o111 != 0 { mode } else { mode & !0o111 };
        if let Err(e) = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)) {
            fail(e);
        }
    }
    if c.recursive && m.is_dir() {
        match std::fs::read_dir(p) {
            Ok(rd) => rd.flatten().for_each(|e| apply_one(&e.path(), c, false, problems)),
            Err(e) => problems.push(format!("{}: {e}", p.display())),
        }
    }
}

#[cfg(not(unix))]
fn apply_one(p: &Path, _: &Change, _: bool, problems: &mut Vec<String>) {
    problems.push(format!("{}: not supported on this system", p.display()));
}

/// Creates a link in `dir` pointing to `target`, named like it.
pub fn symlink(target: &Path, dir: &Path) -> io::Result<PathBuf> {
    let name = target.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no name"))?;
    let link = dir.join(name);
    if link.symlink_metadata().is_ok() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "something with that name already exists"));
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, &link)?;
    #[cfg(windows)]
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, &link)?;
    } else {
        std::os::windows::fs::symlink_file(target, &link)?;
    }
    Ok(link)
}

#[cfg(unix)]
fn user_name(uid: u32) -> Option<String> {
    let pw = unsafe { libc::getpwuid(uid) };
    (!pw.is_null()).then(|| unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) }.to_string_lossy().into_owned())
}

#[cfg(unix)]
fn group_name(gid: u32) -> Option<String> {
    let gr = unsafe { libc::getgrgid(gid) };
    (!gr.is_null()).then(|| unsafe { std::ffi::CStr::from_ptr((*gr).gr_name) }.to_string_lossy().into_owned())
}

#[cfg(unix)]
fn user_id(name: &str) -> Option<u32> {
    let c = std::ffi::CString::new(name).ok()?;
    let pw = unsafe { libc::getpwnam(c.as_ptr()) };
    (!pw.is_null()).then(|| unsafe { (*pw).pw_uid })
}

#[cfg(unix)]
fn group_id(name: &str) -> Option<u32> {
    let c = std::ffi::CString::new(name).ok()?;
    let gr = unsafe { libc::getgrnam(c.as_ptr()) };
    (!gr.is_null()).then(|| unsafe { (*gr).gr_gid })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn modes_owner_links() {
        assert_eq!(symbolic(0o754), "rwxr-xr--");
        assert_eq!(parse_mode("0644"), Some(0o644));
        assert!(parse_mode("9").is_none() && parse_mode("").is_none() && parse_mode("77777").is_none());
        assert_eq!(parse_owner("0:0"), Ok((Some(0), Some(0))));
        assert_eq!(parse_owner(":root"), Ok((None, Some(0))));
        assert!(parse_owner("no-such-user-xyz").is_err());

        let root = std::env::temp_dir().join(format!("guppy-attrs-{}", std::process::id()));
        std::fs::create_dir_all(root.join("d")).unwrap();
        std::fs::write(root.join("d/f"), "").unwrap();
        let change = Change { mode: Some(0o750), recursive: true, ..Default::default() };
        assert!(apply(&[root.join("d")], &change).is_empty());
        let mode = |p: &str| root.join(p).metadata().unwrap().permissions().mode() & 0o777;
        // The file inside does not become executable.
        assert_eq!((mode("d"), mode("d/f")), (0o750, 0o640));
        assert_eq!(read(&root.join("d")).unwrap().0, 0o750);

        let link = symlink(&root.join("d/f"), &root).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), root.join("d/f"));
        assert!(symlink(&root.join("d/f"), &root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
