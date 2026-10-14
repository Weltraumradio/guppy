//! Mounted drives for the drive picker (Alt+F1 / Alt+F2): system root, home,
//! removable media, network shares and rclone remotes, with their fill level.

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
    out.extend(remotes());
    out
}

static REMOTES: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

/// Looks up the rclone remotes in the background; call at startup.
pub fn prefetch_remotes() {
    std::thread::spawn(|| {
        let r = crate::remote::remotes();
        *REMOTES.lock().unwrap() = Some(r);
    });
}

/// rclone remotes, from the last lookup; refreshed in the background on each call
/// so a newly configured remote shows up the next time the picker opens.
fn remotes() -> Vec<Drive> {
    let names = REMOTES.lock().unwrap().clone().unwrap_or_default();
    prefetch_remotes();
    names
        .into_iter()
        .map(|r| Drive { path: PathBuf::from(&r), label: r.trim_end_matches(':').to_string(), fs: "rclone".into(), network: true, space: None })
        .collect()
}

#[cfg(not(target_os = "linux"))]
pub fn list() -> Vec<Drive> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    vec![drive(home, "Home".into(), String::new(), false)]
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

#[cfg(not(unix))]
pub fn space(_: &Path) -> Option<(u64, u64)> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn has_home_and_root() {
        let d = super::list();
        assert_eq!(d[0].label, "Home");
        assert!(d[0].space.is_some_and(|(total, free)| total >= free && total > 0));
        #[cfg(target_os = "linux")]
        assert!(d.iter().any(|x| x.label == "System"));
    }
}
