//! Everything OS-specific in one place.
//!
//! Linux and macOS share the Unix branch. Windows is prepared but not yet
//! tested; hard links and drive boundaries aren't detected there yet.

use std::fs::Metadata;
use std::io;
use std::path::Path;

/// Filesystem identifier; directory sizes stay on one device.
#[cfg(unix)]
pub fn device(m: &Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::dev(m)
}

#[cfg(not(unix))]
pub fn device(_: &Metadata) -> u64 {
    0
}

/// Key for files with multiple hard links, so they count only once.
#[cfg(unix)]
pub fn hardlink_key(m: &Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    (m.nlink() > 1).then(|| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
pub fn hardlink_key(_: &Metadata) -> Option<(u64, u64)> {
    None
}

/// Hidden: leading dot in the name (Unix) or file attribute (Windows).
#[cfg(not(windows))]
pub fn is_hidden(name: &str, _: Option<&Metadata>) -> bool {
    name.starts_with('.')
}

#[cfg(windows)]
pub fn is_hidden(name: &str, m: Option<&Metadata>) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    name.starts_with('.') || m.is_some_and(|m| m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
}

/// Opens a file with the system's default application.
pub fn open(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(path).spawn().map(|_| ())
}

/// Opens a file for editing. `command` (from the settings) wins; otherwise
/// Omarchy's editor launcher, `$VISUAL`, `$EDITOR` in a terminal, then the
/// system default application.
pub fn edit(path: &Path, command: &str) -> io::Result<()> {
    use std::process::Command;
    let spawn = |line: &str| -> io::Result<()> {
        let mut parts = line.split_whitespace();
        let program = parts.next().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty editor command"))?;
        Command::new(program).args(parts).arg(path).spawn().map(|_| ())
    };
    if !command.trim().is_empty() {
        return spawn(command);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if spawn("omarchy-launch-editor").is_ok() {
            return Ok(());
        }
        if let Ok(v) = std::env::var("VISUAL")
            && spawn(&v).is_ok()
        {
            return Ok(());
        }
        // A terminal editor needs a terminal window around it.
        if let Ok(e) = std::env::var("EDITOR")
            && spawn(&format!("xdg-terminal-exec {e}")).is_ok()
        {
            return Ok(());
        }
    }
    #[cfg(target_os = "macos")]
    if Command::new("open").arg("-t").arg(path).spawn().is_ok() {
        return Ok(());
    }
    open(path)
}

/// Characters that switch input into jump mode.
/// Only `~` on Windows, since `/` doesn't start a path there.
pub fn is_jump_prefix(c: char) -> bool {
    c == '~' || (cfg!(unix) && c == '/')
}

/// Is `path` on a network drive (NFS, SMB, SSHFS, WebDAV …)?
/// Work there is gentler: fewer parallel accesses, shallower search.
#[cfg(target_os = "linux")]
pub fn is_network(path: &Path) -> bool {
    const NET: &[&str] = &["nfs", "nfs4", "cifs", "smb3", "smbfs", "ncpfs", "afs", "9p", "davfs", "fuse.sshfs", "fuse.rclone", "fuse.davfs2", "fuse.smbnetfs", "fuse.gvfsd-fuse"];
    let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") else { return false };
    // The longest matching mount point determines the filesystem.
    mounts
        .lines()
        .filter_map(|l| {
            let mut it = l.split(' ');
            let (_, mnt, fs) = (it.next()?, it.next()?, it.next()?);
            // Spaces are encoded as \040 in /proc/self/mounts.
            let mnt = mnt.replace("\\040", " ");
            path.starts_with(&mnt).then(|| (mnt.len(), NET.contains(&fs)))
        })
        .max_by_key(|(len, _)| *len)
        .is_some_and(|(_, net)| net)
}

#[cfg(not(target_os = "linux"))]
pub fn is_network(_: &Path) -> bool {
    false
}
