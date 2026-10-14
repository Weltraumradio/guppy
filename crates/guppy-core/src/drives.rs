//! Mounted drives for the drive picker (Alt+F1 / Alt+F2): system root, home,
//! removable media, network shares, with their fill level.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Drive {
    pub path: PathBuf,
    pub label: String,
    /// Filesystem type, e.g. `btrfs`, `vfat`, `cifs`.
    pub fs: String,
    pub network: bool,
    /// Total and free bytes; not asked for network drives (could hang).
    pub space: Option<(u64, u64)>,
}

/// Mount points worth jumping to, home first.
#[cfg(target_os = "linux")]
pub fn list() -> Vec<Drive> {
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let mut out = vec![drive(home.clone(), "Home".into(), String::new(), false)];
    for line in mounts.lines() {
        let mut it = line.split(' ');
        let (Some(_), Some(mnt), Some(fs)) = (it.next(), it.next(), it.next()) else { continue };
        // Spaces are encoded as \040 in /proc/self/mounts.
        let path = PathBuf::from(mnt.replace("\\040", " "));
        let network = crate::platform::is_network(&path);
        let wanted = path == Path::new("/")
            || ["/mnt", "/media", "/run/media", "/srv"].iter().any(|p| path.starts_with(p) && path != Path::new(p))
            || (path.starts_with(&home) && path != home)
            || network;
        if !wanted || out.iter().any(|d| d.path == path) {
            continue;
        }
        let label = if path == Path::new("/") {
            "System".to_string()
        } else {
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        };
        out.push(drive(path, label, fs.to_string(), network));
    }
    out
}

/// Home, the system disk, then everything under /Volumes except the system disk's alias.
#[cfg(target_os = "macos")]
pub fn list() -> Vec<Drive> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let mut out = vec![drive(home, "Home".into(), String::new(), false), drive("/".into(), "System".into(), fs_type(Path::new("/")), false)];
    let mut volumes: Vec<PathBuf> = std::fs::read_dir("/Volumes").map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    volumes.sort();
    for path in volumes {
        if path.canonicalize().is_ok_and(|p| p == Path::new("/")) {
            continue;
        }
        let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        out.push(drive(path.clone(), label, fs_type(&path), crate::platform::is_network(&path)));
    }
    out
}

/// Filesystem type name from statfs, e.g. `apfs`, `smbfs`.
#[cfg(target_os = "macos")]
pub(crate) fn fs_type(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return String::new() };
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path, `s` a writable statfs.
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return String::new();
    }
    // SAFETY: the kernel fills f_fstypename NUL-terminated.
    unsafe { std::ffi::CStr::from_ptr(s.f_fstypename.as_ptr()) }.to_string_lossy().into_owned()
}

/// Home, then every drive letter in use (C:, D:, network drives …).
#[cfg(windows)]
pub fn list() -> Vec<Drive> {
    use windows_sys::Win32::Storage::FileSystem::{GetLogicalDrives, GetVolumeInformationW};
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("C:\\"));
    let mut out = vec![drive(home, "Home".into(), String::new(), false)];
    // SAFETY: no arguments; returns a bit mask of drive letters.
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u8 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i) as char;
        let path = PathBuf::from(format!("{letter}:\\"));
        let network = crate::platform::is_network(&path);
        let root = wide(&path);
        let (mut name, mut fs) = ([0u16; 261], [0u16; 261]);
        // SAFETY: `root` is NUL-terminated, both buffers are as long as stated.
        // A drive without a medium (empty card reader) fails and is skipped.
        let ok = network
            || unsafe {
                GetVolumeInformationW(root.as_ptr(), name.as_mut_ptr(), 261, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), fs.as_mut_ptr(), 261)
            } != 0;
        if !ok {
            continue;
        }
        let text = |b: &[u16]| String::from_utf16_lossy(&b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]);
        let label = match text(&name) {
            n if n.is_empty() => format!("{letter}:"),
            n => format!("{n} ({letter}:)"),
        };
        out.push(drive(path, label, text(&fs), network));
    }
    out
}

/// NUL-terminated UTF-16 for Windows calls.
#[cfg(windows)]
pub(crate) fn wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

fn drive(path: PathBuf, label: String, fs: String, network: bool) -> Drive {
    let space = if network { None } else { space(&path) };
    Drive { path, label, fs, network, space }
}

/// Total and free bytes of the filesystem holding `path`.
#[cfg(unix)]
pub fn space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path, `s` a writable statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let block = s.f_frsize as u64;
    Some((s.f_blocks as u64 * block, s.f_bavail as u64 * block))
}

#[cfg(windows)]
pub fn space(path: &Path) -> Option<(u64, u64)> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let p = wide(path);
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: `p` is NUL-terminated; the out pointers are valid u64s.
    let ok = unsafe { GetDiskFreeSpaceExW(p.as_ptr(), &mut free, &mut total, std::ptr::null_mut()) } != 0;
    ok.then_some((total, free))
}

#[cfg(test)]
mod tests {
    #[test]
    fn has_home_and_root() {
        let d = super::list();
        assert_eq!(d[0].label, "Home");
        assert!(d[0].space.is_some_and(|(total, free)| total >= free && total > 0));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(d.iter().any(|x| x.label == "System"));
    }
}
