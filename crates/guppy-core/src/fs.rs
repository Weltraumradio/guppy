use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
}

/// Rough classification for coloring in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileClass {
    Dir,
    Code,
    Doc,
    Image,
    Archive,
    Other,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub hidden: bool,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }

    pub fn class(&self) -> FileClass {
        if self.is_dir() {
            return FileClass::Dir;
        }
        let ext = self
            .name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "rs" | "toml" | "sh" | "json" | "yaml" | "yml" | "lua" | "py" | "js" | "ts" | "c"
            | "h" | "cpp" | "go" | "zig" | "conf" | "ini" => FileClass::Code,
            "md" | "txt" | "pdf" | "rst" | "org" => FileClass::Doc,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "avif" => FileClass::Image,
            "zip" | "tar" | "gz" | "zst" | "xz" | "bz2" | "7z" | "rar" => FileClass::Archive,
            _ => FileClass::Other,
        }
    }
}

/// Reads a directory: directories first, then alphabetical, case-insensitive.
pub fn read_dir(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|de| {
            let path = de.path();
            let name = de.file_name().to_string_lossy().into_owned();
            let ft = de.file_type().ok();
            let is_link = ft.is_some_and(|t| t.is_symlink());
            // For symlinks the target counts (links to directories should behave like directories).
            let meta = if is_link { std::fs::metadata(&path).ok() } else { de.metadata().ok() };
            let kind = match &meta {
                Some(m) if m.is_dir() => EntryKind::Dir,
                _ if is_link => EntryKind::Symlink,
                _ => EntryKind::File,
            };
            Entry {
                hidden: crate::platform::is_hidden(&name, meta.as_ref()),
                size: meta.as_ref().map_or(0, |m| m.len()),
                modified: meta.as_ref().and_then(|m| m.modified().ok()),
                name,
                path,
                kind,
            }
        })
        .collect();

    entries.sort_by(|a, b| {
        b.is_dir()
            .cmp(&a.is_dir())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Human-readable size, e.g. `4.8 KB`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Compact modification date: `today 21:17`, `Oct 12 21:17` (this year), otherwise `2024-10-12`.
pub fn human_date(t: SystemTime) -> String {
    use chrono::{DateTime, Datelike, Local};
    let t: DateTime<Local> = t.into();
    let now = Local::now();
    if t.date_naive() == now.date_naive() {
        t.format("today %H:%M").to_string()
    } else if t.year() == now.year() {
        t.format("%b %d %H:%M").to_string()
    } else {
        t.format("%Y-%m-%d").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(214), "214 B");
        assert_eq!(human_size(4915), "4.8 KB");
    }

    #[test]
    fn dirs_first() {
        let tmp = std::env::temp_dir().join(format!("guppy-test-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("zeta")).unwrap();
        std::fs::write(tmp.join("alpha.txt"), "x").unwrap();
        let e = read_dir(&tmp).unwrap();
        assert_eq!(e[0].name, "zeta");
        assert_eq!(e[1].class(), FileClass::Doc);
        std::fs::remove_dir_all(tmp).unwrap();
    }
}
