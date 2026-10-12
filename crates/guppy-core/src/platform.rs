//! Alles Betriebssystem-Spezifische an einer Stelle.
//!
//! Linux und macOS teilen sich den Unix-Zweig. Windows ist vorbereitet, aber
//! noch nicht getestet; Hardlinks und Laufwerksgrenzen werden dort noch nicht erkannt.

use std::fs::Metadata;
use std::io;
use std::path::Path;

/// Kennung eines Dateisystems; Ordnergrößen bleiben auf einem Gerät.
#[cfg(unix)]
pub fn device(m: &Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::dev(m)
}

#[cfg(not(unix))]
pub fn device(_: &Metadata) -> u64 {
    0
}

/// Schlüssel für Dateien mit mehreren Hardlinks, damit sie nur einmal zählen.
#[cfg(unix)]
pub fn hardlink_key(m: &Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    (m.nlink() > 1).then(|| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
pub fn hardlink_key(_: &Metadata) -> Option<(u64, u64)> {
    None
}

/// Versteckt: Punkt am Namensanfang (Unix) bzw. Datei-Attribut (Windows).
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

/// Öffnet eine Datei mit dem Standardprogramm des Systems.
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

/// Zeichen, mit denen eine Eingabe in den Sprungmodus wechselt.
/// Unter Windows nur `~`, weil `/` dort kein Pfadanfang ist.
pub fn is_jump_prefix(c: char) -> bool {
    c == '~' || (cfg!(unix) && c == '/')
}
