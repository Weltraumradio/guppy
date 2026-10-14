//! User actions from the settings: a key that runs a shell command.

use std::path::{Path, PathBuf};

/// The command with its placeholders filled in, each path quoted for `sh`:
/// `%f` file under the cursor, `%F` marked files (or `%f`), `%d` this folder,
/// `%D` the other pane's folder, `%%` a literal `%`.
pub fn expand(command: &str, file: Option<&Path>, marked: &[PathBuf], dir: &Path, other: &Path) -> String {
    let mut out = String::new();
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('f') => out.push_str(&file.map(quote).unwrap_or_default()),
            Some('F') if marked.is_empty() => out.push_str(&file.map(quote).unwrap_or_default()),
            Some('F') => out.push_str(&marked.iter().map(|p| quote(p)).collect::<Vec<_>>().join(" ")),
            Some('d') => out.push_str(&quote(dir)),
            Some('D') => out.push_str(&quote(other)),
            Some('%') => out.push('%'),
            Some(x) => {
                out.push('%');
                out.push(x);
            }
            None => out.push('%'),
        }
    }
    out
}

/// Single quotes for `sh`; a `'` inside becomes `'\''`.
#[cfg(not(windows))]
fn quote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', r"'\''"))
}

/// Double quotes for `cmd`; `"` can't occur in Windows file names.
#[cfg(windows)]
fn quote(p: &Path) -> String {
    format!("\"{}\"", p.display())
}

/// Starts `line` with `sh -c` (Windows: `cmd /C`) in `cwd`, without waiting. In a
/// terminal the window stays open until a key is pressed, so output can be read.
pub fn run(line: &str, cwd: &Path, terminal: bool) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("cmd");
        // Passed as written: cmd has its own quoting rules.
        if terminal {
            c.raw_arg(format!("/C start \"\" cmd /K {line}"));
        } else {
            c.raw_arg(format!("/C {line}")).stdin(Stdio::null());
        }
        c
    };
    // Terminal.app opens a window for a script; it starts in the home folder.
    #[cfg(target_os = "macos")]
    let mut cmd = if terminal {
        let script = format!("cd {} && {line}", quote(cwd));
        let script = script.replace('\\', "\\\\").replace('"', "\\\"");
        let mut c = Command::new("osascript");
        c.args(["-e", &format!("tell application \"Terminal\"\nactivate\ndo script \"{script}\"\nend tell")]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", line]).stdin(Stdio::null());
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = if terminal {
        let mut c = Command::new("xdg-terminal-exec");
        c.args(["sh", "-c", &format!("{line}\nprintf '\\n[done, press Enter]'; read _")]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", line]).stdin(Stdio::null());
        c
    };
    cmd.current_dir(cwd).spawn().map(|_| ())
}

/// A key as written in the settings, e.g. `Ctrl+Alt+T`, `Shift+F9`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySpec {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Lowercase: a character (`t`) or a named key (`f9`, `enter`).
    pub key: String,
}

impl KeySpec {
    pub fn parse(s: &str) -> Option<Self> {
        let mut spec = KeySpec { ctrl: false, alt: false, shift: false, key: String::new() };
        for part in s.split('+').map(str::trim) {
            match part.to_lowercase().as_str() {
                "ctrl" | "control" => spec.ctrl = true,
                "alt" => spec.alt = true,
                "shift" => spec.shift = true,
                "" => return None,
                k if spec.key.is_empty() => spec.key = k.to_string(),
                _ => return None,
            }
        }
        (!spec.key.is_empty()).then_some(spec)
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn placeholders() {
        let (d, o) = (Path::new("/a"), Path::new("/b c"));
        let f = Path::new("/a/it's");
        let marked = [PathBuf::from("/a/x"), PathBuf::from("/a/y")];
        assert_eq!(expand("cp %f %D", Some(f), &[], d, o), r"cp '/a/it'\''s' '/b c'");
        assert_eq!(expand("tar %F", Some(f), &marked, d, o), "tar '/a/x' '/a/y'");
        assert_eq!(expand("x %F 100%% %q %d", None, &[], d, o), "x  100% %q '/a'");
        assert_eq!(KeySpec::parse("Ctrl+Alt+T"), Some(KeySpec { ctrl: true, alt: true, shift: false, key: "t".into() }));
        assert_eq!(KeySpec::parse("F9").unwrap().key, "f9");
        assert!(KeySpec::parse("Ctrl+").is_none() && KeySpec::parse("a+b").is_none());
    }
}
