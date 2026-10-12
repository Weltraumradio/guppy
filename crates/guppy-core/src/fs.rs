use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
}

/// Grobe Einteilung für die Farbgebung in der Liste.
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

/// Liest ein Verzeichnis: Ordner zuerst, dann alphabetisch ohne Groß-/Kleinschreibung.
pub fn read_dir(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|de| {
            let path = de.path();
            let name = de.file_name().to_string_lossy().into_owned();
            let ft = de.file_type().ok();
            let is_link = ft.is_some_and(|t| t.is_symlink());
            // Für Symlinks zählt das Ziel (Ordner-Links sollen sich wie Ordner verhalten).
            let meta = if is_link { std::fs::metadata(&path).ok() } else { de.metadata().ok() };
            let kind = match &meta {
                Some(m) if m.is_dir() => EntryKind::Dir,
                _ if is_link => EntryKind::Symlink,
                _ => EntryKind::File,
            };
            Entry {
                hidden: name.starts_with('.'),
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

/// Menschenlesbare Größe im deutschen Format, z. B. `4,8 KB`.
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
        format!("{v:.1} {}", UNITS[i]).replace('.', ",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(214), "214 B");
        assert_eq!(human_size(4915), "4,8 KB");
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
