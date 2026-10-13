//! Git status of a folder's entries, read with `git status --porcelain`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// State of one entry, most important first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mark {
    Conflict,
    Modified,
    Added,
    Untracked,
    /// Folder with changes somewhere inside.
    Inside,
    Ignored,
}

impl Mark {
    pub fn symbol(self) -> &'static str {
        match self {
            Mark::Conflict => "!",
            Mark::Modified => "M",
            Mark::Added => "A",
            Mark::Untracked => "?",
            Mark::Inside => "•",
            Mark::Ignored => "◌",
        }
    }
}

#[derive(Debug, Default)]
pub struct Status {
    pub branch: String,
    /// Uncommitted changes anywhere in the repository.
    pub dirty: bool,
    /// Commits ahead of / behind upstream.
    pub ahead: u32,
    pub behind: u32,
    /// Entry name → state, for the direct entries of the folder.
    marks: HashMap<String, Mark>,
    /// The folder itself is untracked or ignored, so is everything in it.
    all: Option<Mark>,
}

impl Status {
    pub fn mark(&self, name: &str) -> Option<Mark> {
        self.all.or_else(|| self.marks.get(name).copied())
    }
}

/// Reads the status for `dir`; `None` outside a repository or without git.
/// Slow on huge repositories, so call it off the UI thread.
pub fn status(dir: &Path) -> Option<Status> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["status", "--porcelain=v1", "-z", "--branch", "--ignored=matching", "--untracked-files=normal", "."])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let top = Command::new("git").arg("-C").arg(dir).args(["rev-parse", "--show-toplevel"]).output().ok()?;
    let root = PathBuf::from(String::from_utf8_lossy(&top.stdout).trim_end());
    let dir = dir.canonicalize().ok()?;
    let mut st = Status::default();
    let mut items = out.stdout.split(|&b| b == 0);
    while let Some(item) = items.next() {
        let item = String::from_utf8_lossy(item);
        if let Some(head) = item.strip_prefix("## ") {
            parse_branch(head, &mut st);
            continue;
        }
        if item.len() < 4 {
            continue;
        }
        let (code, rel) = item.split_at(3);
        let code = &code[..2];
        // Renames and copies carry the old path as the next item.
        if code.starts_with('R') || code.starts_with('C') {
            items.next();
        }
        let mark = match code {
            "!!" => Mark::Ignored,
            "??" => Mark::Untracked,
            "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU" => Mark::Conflict,
            c if c.starts_with('A') => Mark::Added,
            _ => Mark::Modified,
        };
        if mark != Mark::Ignored {
            st.dirty = true;
        }
        let Ok(inside) = root.join(rel.trim_end_matches('/')).strip_prefix(&dir).map(Path::to_path_buf) else { continue };
        let mut parts = inside.components();
        let Some(first) = parts.next() else {
            st.all = Some(mark);
            continue;
        };
        let deeper = parts.next().is_some();
        // Something inside a subfolder: the folder only gets a hint, ignored files don't count.
        let mark = match (deeper, mark) {
            (true, Mark::Ignored) => continue,
            (true, _) => Mark::Inside,
            (false, m) => m,
        };
        let e = st.marks.entry(first.as_os_str().to_string_lossy().into_owned()).or_insert(mark);
        *e = (*e).min(mark);
    }
    Some(st)
}

/// `main...origin/main [ahead 1, behind 2]`
fn parse_branch(head: &str, st: &mut Status) {
    let head = head.strip_prefix("No commits yet on ").unwrap_or(head);
    let (name, rest) = head.split_once(' ').unwrap_or((head, ""));
    let name = name.split("...").next().unwrap_or(name);
    st.branch = name.to_string();
    if name == "HEAD" {
        st.branch = "detached".into();
    }
    for part in rest.trim_matches(|c| c == '[' || c == ']').split(", ") {
        if let Some(n) = part.strip_prefix("ahead ") {
            st.ahead = n.parse().unwrap_or(0);
        } else if let Some(n) = part.strip_prefix("behind ") {
            st.behind = n.parse().unwrap_or(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_line() {
        let mut st = Status::default();
        parse_branch("main...origin/main [ahead 1, behind 2]", &mut st);
        assert_eq!((st.branch.as_str(), st.ahead, st.behind), ("main", 1, 2));
        let mut st = Status::default();
        parse_branch("No commits yet on dev", &mut st);
        assert_eq!(st.branch, "dev");
    }

    #[test]
    fn marks_in_repo() {
        let dir = std::env::temp_dir().join(format!("guppy-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let git = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap().status.success());
        git(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "1").unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        git(&["add", "."]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "x"]);
        std::fs::write(dir.join("a.txt"), "2").unwrap();
        std::fs::write(dir.join("sub/new.txt"), "").unwrap();
        std::fs::create_dir(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/x"), "").unwrap();
        let st = status(&dir).unwrap();
                assert_eq!(st.mark("a.txt"), Some(Mark::Modified));
        assert_eq!(st.mark("sub"), Some(Mark::Untracked));
        assert_eq!(st.mark("target"), Some(Mark::Ignored));
        assert_eq!(st.mark(".gitignore"), None);
        assert!(st.dirty);
        let sub = status(&dir.join("sub")).unwrap();
        assert_eq!(sub.mark("new.txt"), Some(Mark::Untracked));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
