//! User settings from `~/.config/guppy/config.toml`.
//!
//! Every key is optional; missing keys keep their default. On first start a
//! commented template is written so the options are easy to discover.

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub start: Start,
    pub view: View,
    pub confirm: Confirm,
    pub network: Network,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Start {
    /// Left pane; empty = the directory Guppy was started in.
    pub left: String,
    /// Right pane; empty = the directory Guppy was started in.
    pub right: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortBy {
    Name,
    Size,
    Date,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct View {
    pub show_hidden: bool,
    pub sort: SortBy,
    /// Compute folder sizes automatically; otherwise only on Ctrl+R.
    pub dir_sizes: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Confirm {
    pub copy: bool,
    #[serde(rename = "move")]
    pub move_: bool,
    pub trash: bool,
    pub delete: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Network {
    /// Parallel accesses per folder-size computation on network drives.
    pub threads: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self { start: Start::default(), view: View::default(), confirm: Confirm::default(), network: Network::default() }
    }
}

impl Default for Start {
    fn default() -> Self {
        Self { left: String::new(), right: "~".into() }
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
"#;

impl Config {
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
    }
}
