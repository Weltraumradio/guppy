//! User settings from `~/.config/guppy/config.toml`.
//!
//! Every key is optional; missing keys keep their default. On first start a
//! commented template is written so the options are easy to discover.

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub start: Start,
    pub view: View,
    pub confirm: Confirm,
    pub network: Network,
    pub edit: Edit,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Edit {
    /// Editor command for F4; empty = automatic.
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Start {
    /// Left pane; empty = the directory Guppy was started in.
    pub left: String,
    /// Right pane; empty = the directory Guppy was started in.
    pub right: String,
    /// Show the key help when Guppy starts.
    pub help: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortBy {
    Name,
    Size,
    Date,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct View {
    pub show_hidden: bool,
    pub sort: SortBy,
    /// Compute folder sizes automatically; otherwise only on Ctrl+R.
    pub dir_sizes: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Confirm {
    pub copy: bool,
    #[serde(rename = "move")]
    pub move_: bool,
    pub trash: bool,
    pub delete: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Network {
    /// Parallel accesses per folder-size computation on network drives.
    pub threads: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self { start: Start::default(), view: View::default(), confirm: Confirm::default(), network: Network::default(), edit: Edit::default() }
    }
}

impl Default for Start {
    fn default() -> Self {
        Self { left: String::new(), right: "~".into(), help: true }
    }
}

impl Default for View {
    fn default() -> Self {
        Self { show_hidden: false, sort: SortBy::Name, dir_sizes: true }
    }
}

impl Default for Confirm {
    fn default() -> Self {
        Self { copy: true, move_: true, trash: true, delete: true }
    }
}

impl Default for Network {
    fn default() -> Self {
        Self { threads: 4 }
    }
}

const TEMPLATE: &str = r#"# Guppy settings. Every key is optional; delete a line to use the default.
# Changes take effect immediately, except [start] and [network] (used at startup).

[start]
left = ""          # left pane: path, "~/…", or "" for the directory Guppy was started in
right = "~"        # right pane
help = true        # show the key help at startup

[view]
show_hidden = false
sort = "name"      # "name", "size" or "date"
dir_sizes = true   # compute folder sizes automatically (otherwise only on Ctrl+R)

[confirm]
# Ask before these operations. Progress is shown either way.
copy = true
move = true
trash = true
delete = true      # permanent delete; turning this off is not recommended

[network]
threads = 4        # parallel accesses per folder-size computation on network drives

[edit]
command = ""       # editor for F4, e.g. "code" or "kitty -e nvim"; "" = automatic
"#;

impl SortBy {
    pub fn name(self) -> &'static str {
        match self {
            SortBy::Name => "name",
            SortBy::Size => "size",
            SortBy::Date => "date",
        }
    }
}

impl Config {
    /// The file contents for these settings, in the commented template layout.
    pub fn to_toml(&self) -> String {
        let q = |s: &str| toml::Value::String(s.to_string()).to_string();
        let mut out = String::new();
        // Walk the template and substitute each value, keeping all comments.
        let mut section = "";
        for line in TEMPLATE.lines() {
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = match name {
                    "start" => "start",
                    "view" => "view",
                    "confirm" => "confirm",
                    "edit" => "edit",
                    _ => "network",
                };
                out.push_str(line);
                out.push('\n');
                continue;
            }
            let Some((key, rest)) = line.split_once(" = ") else {
                out.push_str(line);
                out.push('\n');
                continue;
            };
            let key = key.trim();
            let value = match (section, key) {
                ("start", "left") => q(&self.start.left),
                ("start", "right") => q(&self.start.right),
                ("start", "help") => self.start.help.to_string(),
                ("view", "show_hidden") => self.view.show_hidden.to_string(),
                ("view", "sort") => q(self.view.sort.name()),
                ("view", "dir_sizes") => self.view.dir_sizes.to_string(),
                ("confirm", "copy") => self.confirm.copy.to_string(),
                ("confirm", "move") => self.confirm.move_.to_string(),
                ("confirm", "trash") => self.confirm.trash.to_string(),
                ("confirm", "delete") => self.confirm.delete.to_string(),
                ("network", "threads") => self.network.threads.to_string(),
                ("edit", "command") => q(&self.edit.command),
                _ => rest.split('#').next().unwrap_or("").trim().to_string(),
            };
            let comment = rest.find('#').map(|i| &rest[i..]);
            let head = format!("{key} = {value}");
            match comment {
                Some(c) => out.push_str(&format!("{head:<18} {c}\n")),
                None => {
                    out.push_str(&head);
                    out.push('\n');
                }
            }
        }
        out
    }

    /// Writes the settings (atomically via a temporary file).
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::path() else { return Ok(()) };
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/")))?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, self.to_toml())?;
        std::fs::rename(tmp, path)
    }

    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("guppy/config.toml"))
    }

    /// Loads the settings; writes the template if there is no file yet.
    /// A broken file yields the defaults plus an error message for the user.
    pub fn load() -> (Self, Option<String>) {
        let Some(path) = Self::path() else { return (Self::default(), None) };
        match std::fs::read_to_string(&path) {
            Ok(s) => match toml::from_str(&s) {
                Ok(c) => (c, None),
                Err(e) => (Self::default(), Some(format!("{}: {}", path.display(), e.message()))),
            },
            Err(_) => {
                let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/")));
                let _ = std::fs::write(&path, TEMPLATE);
                (Self::default(), None)
            }
        }
    }

    /// Resolves a start path; `~` means home, empty means `cwd`.
    pub fn resolve(p: &str, cwd: &Path) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        match p.trim() {
            "" => cwd.to_path_buf(),
            "~" => home,
            p => match p.strip_prefix("~/") {
                Some(rest) => home.join(rest),
                None => PathBuf::from(p),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_to_defaults() {
        let c: Config = toml::from_str(TEMPLATE).unwrap();
        assert!(c.confirm.delete && c.view.dir_sizes);
        assert_eq!(c.view.sort, SortBy::Name);
        assert_eq!(c.network.threads, 4);
        assert!(toml::from_str::<Config>("[view]\nbogus = 1").is_err());
        // Round trip through the commented layout.
        let mut c = Config::default();
        c.start.left = "~/a \"b\"".into();
        c.view.sort = SortBy::Date;
        c.confirm.trash = false;
        c.network.threads = 7;
        c.edit.command = "kitty -e nvim".into();
        assert_eq!(toml::from_str::<Config>(&c.to_toml()).unwrap(), c);
    }
}
