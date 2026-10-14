//! Helper programs Guppy can use when they are installed, and how to get them:
//! a package manager command run in a terminal, or a download page.

use std::path::{Path, PathBuf};

/// One helper program.
pub struct Tool {
    /// Program name as called (without `.exe`).
    pub bin: &'static str,
    /// Name shown in the settings.
    pub name: &'static str,
    /// What Guppy does with it.
    pub purpose: &'static str,
    /// Package names: Arch, Debian/Ubuntu, Homebrew, winget. `None` where there
    /// is no package or the system brings it along.
    pub pacman: Option<&'static str>,
    pub apt: Option<&'static str>,
    pub brew: Option<&'static str>,
    pub winget: Option<&'static str>,
    pub url: &'static str,
    /// Part of the system here (no install offered).
    pub builtin: bool,
}

pub const TOOLS: &[Tool] = &[
    Tool {
        bin: "pdftoppm",
        name: "poppler",
        purpose: "PDF preview",
        pacman: Some("poppler"),
        apt: Some("poppler-utils"),
        brew: Some("poppler"),
        winget: Some("oschwartz10612.Poppler"),
        url: "https://poppler.freedesktop.org",
        builtin: false,
    },
    Tool {
        bin: "ffmpeg",
        name: "ffmpeg",
        purpose: "video and audio preview",
        pacman: Some("ffmpeg"),
        apt: Some("ffmpeg"),
        brew: Some("ffmpeg"),
        winget: Some("Gyan.FFmpeg"),
        url: "https://ffmpeg.org/download.html",
        builtin: false,
    },
    Tool {
        bin: "magick",
        name: "ImageMagick",
        purpose: "preview of HEIC, AVIF, RAW, PSD …",
        pacman: Some("imagemagick"),
        apt: Some("imagemagick"),
        brew: Some("imagemagick"),
        winget: Some("ImageMagick.ImageMagick"),
        url: "https://imagemagick.org/script/download.php",
        builtin: false,
    },
    Tool {
        bin: if cfg!(windows) { "tar" } else { "bsdtar" },
        name: if cfg!(windows) { "tar" } else { "bsdtar" },
        purpose: "7z, rar and iso archives (zip and tar need nothing)",
        pacman: Some("libarchive"),
        apt: Some("libarchive-tools"),
        brew: None,
        winget: None,
        url: "https://www.libarchive.org",
        builtin: cfg!(any(windows, target_os = "macos")),
    },
    Tool {
        bin: "git",
        name: "git",
        purpose: "git status in the file list",
        pacman: Some("git"),
        apt: Some("git"),
        brew: Some("git"),
        winget: Some("Git.Git"),
        url: "https://git-scm.com/downloads",
        builtin: false,
    },
    Tool {
        bin: "zoxide",
        name: "zoxide",
        purpose: "jump to folders you use in the shell",
        pacman: Some("zoxide"),
        apt: Some("zoxide"),
        brew: Some("zoxide"),
        winget: Some("ajeetdsouza.zoxide"),
        url: "https://github.com/ajeetdsouza/zoxide",
        builtin: false,
    },
];

/// Where `bin` is in `PATH`, if anywhere.
pub fn find(bin: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) { format!("{bin}.exe") } else { bin.to_string() };
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(&file)).find(|p| p.is_file())
}

pub fn tool(bin: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.bin == bin)
}

/// How to install it here, as one shell command; `None` means "download page".
pub fn install_command(t: &Tool) -> Option<String> {
    if t.builtin {
        return None;
    }
    if cfg!(windows) {
        return t.winget.filter(|_| find("winget").is_some()).map(|id| format!("winget install --id {id} -e"));
    }
    if cfg!(target_os = "macos") {
        return t.brew.filter(|_| find("brew").is_some()).map(|p| format!("brew install {p}"));
    }
    if let Some(p) = t.pacman.filter(|_| find("pacman").is_some()) {
        return Some(format!("sudo pacman -S --needed {p}"));
    }
    t.apt.filter(|_| find("apt").is_some()).map(|p| format!("sudo apt install {p}"))
}

/// Message for a feature that needs a missing tool.
pub fn missing_hint(bin: &str) -> String {
    let what = tool(bin).map_or(bin, |t| t.name);
    format!("needs {what} · Settings (F12) → Helper programs installs it")
}

/// Installs `t` in a terminal window (so a password prompt works and the
/// output stays visible), or opens its download page.
pub fn install(t: &Tool) -> std::io::Result<()> {
    match install_command(t) {
        Some(cmd) => crate::actions::run(&cmd, &dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")), true),
        None => crate::platform::open(Path::new(t.url)),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_programs() {
        assert!(super::find(if cfg!(windows) { "cmd" } else { "sh" }).is_some());
        assert!(super::find("surely-not-a-program-xyz").is_none());
    }
}
