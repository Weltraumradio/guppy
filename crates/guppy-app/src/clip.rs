//! Files on the Wayland clipboard, the way other file managers do it:
//! `x-special/gnome-copied-files` (copy or cut), `text/uri-list` and the plain paths.

use std::io::Read;
use std::path::{Path, PathBuf};

use wl_clipboard_rs::copy::{self, MimeSource, Source};
use wl_clipboard_rs::paste::{self, ClipboardType, Seat};

const GNOME: &str = "x-special/gnome-copied-files";
const URI_LIST: &str = "text/uri-list";

/// Puts files on the clipboard; `cut` marks them to be moved on paste.
pub fn put(paths: &[PathBuf], cut: bool) -> Result<(), String> {
    let uris: Vec<String> = paths.iter().map(|p| uri(p)).collect();
    let text = |s: String| Source::Bytes(s.into_bytes().into_boxed_slice());
    // Plain paths first: `wl-paste` without a type (used by terminal apps such
    // as Claude Code) takes the first text format offered.
    let sources = vec![
        MimeSource {
            source: text(paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n")),
            mime_type: copy::MimeType::Text,
        },
        MimeSource { source: text(uris.iter().map(|u| format!("{u}\r\n")).collect()), mime_type: copy::MimeType::Specific(URI_LIST.into()) },
        MimeSource {
            source: text(format!("{}\n{}", if cut { "cut" } else { "copy" }, uris.join("\n"))),
            mime_type: copy::MimeType::Specific(GNOME.into()),
        },
    ];

    copy::Options::new().copy_multi(sources).map_err(|e| e.to_string())
}

/// Files on the clipboard and whether they were cut; `None` if it holds no files.
pub fn get() -> Option<(Vec<PathBuf>, bool)> {
    let read = |mime: &str| {
        let (mut pipe, _) = paste::get_contents(ClipboardType::Regular, Seat::Unspecified, paste::MimeType::Specific(mime)).ok()?;
        let mut s = String::new();
        pipe.read_to_string(&mut s).ok()?;
        Some(s)
    };
    let (cut, list) = match read(GNOME) {
        Some(s) => {
            let mut lines = s.lines();
            (lines.next() == Some("cut"), lines.map(str::to_string).collect::<Vec<_>>())
        }
        None => (false, read(URI_LIST)?.lines().map(str::to_string).collect()),
    };
    let paths: Vec<PathBuf> = list.iter().filter(|l| !l.starts_with('#')).filter_map(|l| from_uri(l.trim())).collect();
    (!paths.is_empty()).then_some((paths, cut))
}

fn uri(p: &Path) -> String {
    let mut out = String::from("file://");
    for b in p.as_os_str().as_encoded_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn from_uri(u: &str) -> Option<PathBuf> {
    let rest = u.strip_prefix("file://")?;
    // Skip an optional host part ("file://host/path").
    let rest = &rest[rest.find('/')?..];
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
    }
    #[cfg(not(unix))]
    Some(PathBuf::from(String::from_utf8_lossy(&out).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_round_trip() {
        for p in ["/home/a b/ä.jpg", "/x/100%.txt", "/plain"] {
            assert_eq!(from_uri(&uri(Path::new(p))), Some(PathBuf::from(p)));
        }
        assert_eq!(from_uri("file://host/tmp/x"), Some(PathBuf::from("/tmp/x")));
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Needs a Wayland session: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn put_then_get() {
        let p = vec![PathBuf::from("/tmp/a b.jpg"), PathBuf::from("/etc/hosts")];
        put(&p, true).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let out = std::process::Command::new("wl-paste").args(["--list-types"]).output().unwrap();
        eprintln!("{}", String::from_utf8_lossy(&out.stdout));
        for args in [vec![], vec!["--no-newline"], vec!["--type", "text/plain"], vec!["--type", "text"]] {
            let out = std::process::Command::new("wl-paste").args(&args).output().unwrap();
            eprintln!("{args:?} -> {:?}", String::from_utf8_lossy(&out.stdout));
        }
        assert_eq!(get(), Some((p, true)));
    }
}
