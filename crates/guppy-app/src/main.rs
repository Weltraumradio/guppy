mod clip;
mod icons;
mod paper;
mod preview;
mod watch;

use paper::{Paper, Scanlines};
use preview::ViewBody;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use guppy_core::archive;
use guppy_core::fs::{glob_match, human_date, human_size};
use std::time::SystemTime;
use guppy_core::ops::{self, Job, OpKind, Resolve};
use guppy_core::config::{Look, SortBy};
use guppy_core::{Cancel, Config, DirSize, DirSizes, Entry, FileClass, Jumper};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::operation::{self, AbsoluteOffset};
use iced::widget::{
    text_editor,
    Id, button, column, container, mouse_area, row, scrollable, svg, text,
};
use iced::{Background, Border, Color, Element, Font, Length, Subscription, Task};

const ROW_H: f32 = 26.0;
const SHEET_RADIUS: f32 = 6.0;
const JUMP_LIMIT: usize = 14;
/// Embedded so Guppy looks the same on every system (license: fonts/OFL.txt).
const FONT_REGULAR: &[u8] = include_bytes!("../fonts/JetBrainsMono-Regular.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../fonts/JetBrainsMono-Bold.ttf");
const FONT: Font = Font::with_name("JetBrains Mono");
const BOLD: Font = Font {
    weight: iced::font::Weight::Bold,
    ..FONT
};

fn main() -> iced::Result {
    iced::application(Guppy::new, Guppy::update, Guppy::view)
        .title(Guppy::title)
        .theme(Guppy::theme)
        .subscription(Guppy::subscription)
        .window_size((1200.0, 760.0))
        .font(FONT_REGULAR)
        .font(FONT_BOLD)
        .default_font(FONT)
        // On close, save the size cache first.
        .exit_on_close_request(false)
        .run()
}

/// Colors for a look: neon is a fixed palette, paper follows the Omarchy theme.
fn load_theme(look: Look) -> guppy_core::Theme {
    match look {
        Look::Paper => guppy_core::Theme::load(),
        Look::Neon => guppy_core::Theme::neon(),
    }
}

fn rgb(c: u32) -> Color {
    Color::from_rgb8((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

#[derive(Debug, Clone)]
enum Message {
    Key(keyboard::Event),
    Select(usize, usize),
    /// Mouse entered a pane: it becomes the active one.
    HoverPane(usize),
    Open(usize, usize),
    JumpTo(usize),
    Scrolled(usize, scrollable::Viewport),
    /// Folder sizes still running: redraw.
    Tick,
    /// Frame tick during smooth scrolling.
    Frame(std::time::Instant),
    Resized(iced::Size),
    /// Files in an open folder or the theme changed.
    FsChanged(Vec<PathBuf>),
    CloseRequested(iced::window::Id),
    Back,
    Forward,
    Parent,
    Home,
    ToggleMark,
    SwitchSide,
    Op(OpKind),
    NewDir,
    DialogConfirm,
    DialogCancel,
    ToggleHidden,
    OpenSettings,
    Viewed(PathBuf, ViewBody),
    /// Built-in editor (F4): file read, typing, save, close.
    EditLoaded(PathBuf, Result<String, String>),
    EditAction(text_editor::Action),
    EditSave,
    EditClose,
    /// Clipboard text for the text field that has the keyboard.
    Paste(Option<String>),
    /// Files from the clipboard, and whether they were cut.
    FilesPasted(Option<(Vec<PathBuf>, bool)>),
    /// Click on a settings row: select it and change it one step.
    SettingClick(usize),
    /// Checkbox on the startup help: show it at every start.
    ToggleStartHelp,
    /// Ctrl+Q: show the entry under the cursor in the other pane.
    ToggleQuick,
    /// The cursor rested long enough: load the quick view (if still current).
    QuickDue(u64),
    QuickLoaded(PathBuf, ViewBody),
    /// Click on a palette row: run that command.
    PaletteRun(usize),
    /// Click on a drive in the drive picker.
    DrivePick(usize),
    /// Answer to "target already exists".
    Resolve(Resolve),
    /// Conflict dialog: the answer holds for all later conflicts of the job.
    ToggleAll,
    /// Open the job list (also Ctrl+J).
    OpenJobs,
    /// Cancel the running job, or with `true` also everything queued.
    CancelJobs(bool),
    /// A file from an archive was extracted for opening (or failed).
    Opened(Result<(), String>),
    /// Developer aid: `GUPPY_SCREENSHOT=file.ppm` saves an image once sizes are
    /// computed, then exits.
    Shot(iced::window::Screenshot),
}

/// Input starting with `~` (on Unix also `/`) jumps instead of filtering.
fn is_jump(query: &str) -> bool {
    query.chars().next().is_some_and(guppy_core::platform::is_jump_prefix)
}

/// Modal dialog over the window; while open, it receives all keys.
enum Dialog {
    Confirm { kind: OpKind, sources: Vec<PathBuf>, target: Option<PathBuf> },
    NewDir { name: String, error: Option<String> },
    /// Built-in editor (F4); `content` is `None` while the file loads.
    Edit {
        path: PathBuf,
        content: Option<text_editor::Content>,
        dirty: bool,
        /// Esc with unsaved changes asks once; a second Esc discards.
        closing: bool,
        error: Option<String>,
    },
    /// Viewer (F3) for the file under the cursor.
    View { path: PathBuf, body: ViewBody },
    /// The running job hit an existing target; `all` = apply the answer to the rest.
    Conflict { conflict: ops::Conflict, all: bool },
    /// Running and queued jobs.
    Jobs,
    /// Command palette (Ctrl+P): typed query and selected hit.
    Palette { query: String, cur: usize },
    /// Drive picker (Alt+F1 / Alt+F2) for the pane `side`.
    Drives { side: usize, list: Vec<guppy_core::drives::Drive>, cur: usize },
    /// Mark (`+`) or unmark (`-`) by wildcard pattern.
    Mark { add: bool, pattern: String },
    Report { title: &'static str, problems: Vec<String> },
    /// Quick help listing all keys (F1); at startup with a "show again" checkbox.
    Help { startup: bool },
    /// Settings (F12): selected row, and the text being typed for path rows.
    Settings { row: usize, edit: Option<String> },
}

/// F2: the name being edited in place, in the cursor row of `side`.
struct Rename {
    side: usize,
    from: PathBuf,
    name: String,
    /// Caret position in chars.
    caret: usize,
    /// Other end of the selection (in chars), if text is selected.
    anchor: Option<usize>,
    error: Option<String>,
}

impl Rename {
    fn byte(&self, i: usize) -> usize {
        self.name.char_indices().nth(i).map_or(self.name.len(), |(b, _)| b)
    }

    /// Selected range in chars, if not empty.
    fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        (a != self.caret).then(|| (a.min(self.caret), a.max(self.caret)))
    }

    fn selected(&self) -> Option<&str> {
        self.selection().map(|(lo, hi)| &self.name[self.byte(lo)..self.byte(hi)])
    }

    /// Removes the selection; false if nothing was selected.
    fn delete_selection(&mut self) -> bool {
        let Some((lo, hi)) = self.selection() else { return false };
        let range = self.byte(lo)..self.byte(hi);
        self.name.replace_range(range, "");
        self.caret = lo;
        self.anchor = None;
        true
    }

    /// Types `t` at the caret, replacing the selection.
    fn insert(&mut self, t: &str) {
        self.delete_selection();
        let b = self.byte(self.caret);
        self.name.insert_str(b, t);
        self.caret += t.chars().count();
        self.anchor = None;
        self.error = None;
    }

    /// Moves the caret; with Shift the selection grows, without it ends.
    fn move_to(&mut self, to: usize, select: bool) {
        if select {
            self.anchor.get_or_insert(self.caret);
        } else {
            self.anchor = None;
        }
        self.caret = to;
    }
}

/// Ctrl+C / Ctrl+X / Ctrl+V and Shift+Insert, as 'c', 'x', 'v'.
fn clip_key(key: &Key, m: keyboard::Modifiers) -> Option<char> {
    match key.as_ref() {
        Key::Character(c) if m.command() && !m.shift() && !m.alt() => match c.to_lowercase().as_str() {
            "c" => Some('c'),
            "x" => Some('x'),
            "v" => Some('v'),
            _ => None,
        },
        Key::Named(Named::Insert) if m.shift() => Some('v'),
        _ => None,
    }
}

fn editor_id() -> Id {
    Id::new("editor")
}

/// Reads a text file for the editor; refuses binary and very large files.
fn load_edit(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() as usize > 4 * preview::VIEW_MAX {
        return Err(format!("Too large for the editor · {}", human_size(meta.len())));
    }
    let buf = std::fs::read(path).map_err(|e| e.to_string())?;
    if buf.iter().take(8192).any(|&b| b == 0) {
        return Err("Binary file, not editable here".into());
    }
    String::from_utf8(buf).map_err(|_| "Not UTF-8 text, not editable here".to_string())
}

fn quick_id() -> Id {
    Id::new("quick")
}

/// Size and date of a file on disk, for viewer headers.
fn file_info(path: &Path) -> String {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => "folder".to_string(),
        Ok(m) => {
            let date = m.modified().ok().map(|t: SystemTime| human_date(t)).unwrap_or_default();
            format!("{} · {date}", human_size(m.len()))
        }
        Err(_) => String::new(),
    }
}

fn view_id() -> Id {
    Id::new("viewer")
}

/// Extracts a file from an archive into a temporary folder and opens it there.
fn open_from_archive(path: &Path) -> Result<(), String> {
    let (archive, inner) = archive::split(path).ok_or("not inside an archive")?;
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("guppy-{}/{n}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    archive::extract(&archive, std::slice::from_ref(&inner), &dir, |_, _| true).map_err(|e| e.to_string())?;
    guppy_core::platform::open(&dir.join(inner)).map_err(|e| e.to_string())
}

/// Rows of the settings overlay, in display order: (section, label).
const SETTINGS: &[(&str, &str)] = &[
    ("View", "Show hidden files"),
    ("View", "Default sort"),
    ("View", "Folder sizes automatically"),
    ("Ask before", "Copy"),
    ("Ask before", "Move"),
    ("Ask before", "Move to trash"),
    ("Ask before", "Delete permanently"),
    ("Start folders", "Left pane"),
    ("Start folders", "Right pane"),
    ("Network drives", "Parallel accesses"),
    ("Startup", "Show key help"),
    ("Appearance", "Look"),
];

/// Rows 7 and 8 hold paths and are edited as text.
fn setting_is_text(row: usize) -> bool {
    matches!(row, 7 | 8)
}

fn setting_value(c: &Config, row: usize) -> String {
    let on = |b: bool| if b { "on" } else { "off" }.to_string();
    let path = |p: &str| if p.trim().is_empty() { "startup folder".to_string() } else { p.to_string() };
    match row {
        0 => on(c.view.show_hidden),
        1 => sort_from(c.view.sort).label().trim_end_matches(" ↓").to_string(),
        2 => on(c.view.dir_sizes),
        3 => on(c.confirm.copy),
        4 => on(c.confirm.move_),
        5 => on(c.confirm.trash),
        6 => on(c.confirm.delete),
        7 => path(&c.start.left),
        8 => path(&c.start.right),
        9 => c.network.threads.to_string(),
        10 => on(c.start.help),
        _ => c.view.look.name().to_string(),
    }
}

/// Changes a non-text setting one step; `dir` is -1 or +1.
fn setting_step(c: &mut Config, row: usize, dir: i32) {
    let flip = |b: &mut bool| *b = !*b;
    match row {
        0 => flip(&mut c.view.show_hidden),
        1 => {
            let order = [SortBy::Name, SortBy::Size, SortBy::Date];
            let i = order.iter().position(|s| *s == c.view.sort).unwrap_or(0) as i32;
            c.view.sort = order[(i + dir).rem_euclid(3) as usize];
        }
        2 => flip(&mut c.view.dir_sizes),
        3 => flip(&mut c.confirm.copy),
        4 => flip(&mut c.confirm.move_),
        5 => flip(&mut c.confirm.trash),
        6 => flip(&mut c.confirm.delete),
        9 => c.network.threads = (c.network.threads as i32 + dir).clamp(1, 32) as usize,
        10 => flip(&mut c.start.help),
        11 => c.view.look = if c.view.look == Look::Paper { Look::Neon } else { Look::Paper },
        _ => {}
    }
}

/// Everything the command palette offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cmd {
    Copy,
    Move,
    Trash,
    Delete,
    NewDir,
    Rename,
    View,
    Edit,
    EditExternal,
    Quick,
    Mark,
    Unmark,
    Invert,
    Hidden,
    Sort,
    Sizes,
    Jump,
    Back,
    Forward,
    Parent,
    Home,
    Switch,
    DrivesLeft,
    DrivesRight,
    Jobs,
    Look,
    Settings,
    Help,
}

/// Palette entries: (label, key, command).
const COMMANDS: &[(&str, &str, Cmd)] = &[
    ("Copy to other pane", "F5", Cmd::Copy),
    ("Move to other pane", "F6", Cmd::Move),
    ("Move to trash", "F8", Cmd::Trash),
    ("Delete permanently", "Shift+F8", Cmd::Delete),
    ("New folder", "F7", Cmd::NewDir),
    ("Rename", "F2", Cmd::Rename),
    ("View file", "F3", Cmd::View),
    ("Edit file", "F4", Cmd::Edit),
    ("Edit in external editor", "Shift+F4", Cmd::EditExternal),
    ("Quick view in other pane", "Ctrl+Q", Cmd::Quick),
    ("Mark by pattern", "+", Cmd::Mark),
    ("Unmark by pattern", "-", Cmd::Unmark),
    ("Invert marks", "*", Cmd::Invert),
    ("Show / hide hidden files", "Ctrl+H", Cmd::Hidden),
    ("Sort: name, size, date", "Ctrl+S", Cmd::Sort),
    ("Recompute folder sizes", "Ctrl+R", Cmd::Sizes),
    ("Jump to folder", "Ctrl+G", Cmd::Jump),
    ("Back", "Alt+←", Cmd::Back),
    ("Forward", "Alt+→", Cmd::Forward),
    ("Parent folder", "Backspace", Cmd::Parent),
    ("Home folder", "", Cmd::Home),
    ("Switch pane", "Tab", Cmd::Switch),
    ("Drives: left pane", "Alt+F1", Cmd::DrivesLeft),
    ("Drives: right pane", "Alt+F2", Cmd::DrivesRight),
    ("Running jobs", "Ctrl+J", Cmd::Jobs),
    ("Switch look: paper / neon", "", Cmd::Look),
    ("Settings", "F12", Cmd::Settings),
    ("Key help", "F1", Cmd::Help),
];

/// Fuzzy score: all query characters in order; consecutive and word-start hits
/// count more. `None` if they don't all appear.
fn fuzzy(query: &str, label: &str) -> Option<i32> {
    let label: Vec<char> = label.to_lowercase().chars().collect();
    let mut score = 0;
    let mut pos = 0;
    let mut last: Option<usize> = None;
    for q in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let i = pos + label[pos..].iter().position(|&c| c == q)?;
        score += 1;
        if last.is_some_and(|l| l + 1 == i) {
            score += 3;
        }
        if i == 0 || !label[i - 1].is_alphanumeric() {
            score += 5;
        }
        last = Some(i);
        pos = i + 1;
    }
    // Earlier and shorter matches first.
    Some(score * 10 - last.unwrap_or(0) as i32 - label.len() as i32 / 4)
}

/// Palette hits for `query`, best first (all commands when empty).
fn palette_hits(query: &str) -> Vec<usize> {
    let mut hits: Vec<(i32, usize)> = COMMANDS.iter().enumerate().filter_map(|(i, (label, _, _))| fuzzy(query, label).map(|s| (s, i))).collect();
    if !query.trim().is_empty() {
        hits.sort_by_key(|(s, i)| (std::cmp::Reverse(*s), *i));
    }
    hits.into_iter().map(|(_, i)| i).collect()
}

/// Quick help sections: heading and (key, action).
const HELP: &[(&str, &[(&str, &str)])] = &[
    ("Navigation", &[
        ("↑ ↓", "move cursor"),
        ("PgUp PgDn", "page up / down"),
        ("Home End", "first / last item"),
        ("Enter", "open folder, archive, file"),
        ("Backspace", "shorten search, else folder up"),
        ("Alt+← Alt+→", "history back / forward"),
        ("Tab", "switch pane"),
    ]),
    ("Search", &[
        ("a–z …", "filter active pane"),
        ("~ or /", "jump to folder (fuzzy)"),
        ("Ctrl+G", "open jump mode"),
        ("Esc", "clear search"),
    ]),
    ("Files", &[
        ("Insert", "mark and advance"),
        ("Space", "mark (when search is empty)"),
        ("+ / -", "mark / unmark by pattern"),
        ("*", "invert marks"),
        ("F2", "rename"),
        ("F3", "view"),
        ("F4", "edit"),
        ("Shift+F4", "external editor"),
        ("F5", "copy to other pane"),
        ("F6", "move to other pane"),
        ("F7", "new folder"),
        ("F8 Del", "move to trash"),
        ("Shift+F8/Del", "delete permanently"),
        ("Ctrl+C / X", "copy / cut files to clipboard"),
        ("Ctrl+V", "paste files here"),
        ("Ctrl+J", "running jobs"),
        ("Ctrl+Q", "quick view in other pane"),
        ("Ctrl+P", "command palette"),
        ("Alt+F1 / F2", "drives left / right"),
    ]),
    ("View", &[
        ("F1", "this help"),
        ("F12", "settings (also Ctrl+Comma)"),
        ("Ctrl+H", "hidden files"),
        ("Ctrl+S", "sort: name, size, date"),
        ("Ctrl+R", "recompute folder sizes"),
    ]),
];

fn op_verb(kind: OpKind) -> &'static str {
    match kind {
        OpKind::Copy => "copy",
        OpKind::Move => "move",
        OpKind::Trash => "trash",
        OpKind::Delete => "permanently delete",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort {
    /// Folders first, then alphabetical.
    Name,
    /// Largest first, folders and files mixed.
    Size,
    /// Newest first.
    Date,
}

impl Sort {
    fn next(self) -> Self {
        match self {
            Sort::Name => Sort::Size,
            Sort::Size => Sort::Date,
            Sort::Date => Sort::Name,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Sort::Name => "Name",
            Sort::Size => "Size ↓",
            Sort::Date => "Date ↓",
        }
    }
}

/// Size of an entry for display and sorting; folders come from the size computation.
fn entry_size(e: &Entry, sizes: &DirSizes) -> Option<u64> {
    if !e.is_dir() || e.in_archive {
        return Some(e.size);
    }
    match sizes.get(&e.path) {
        Some(DirSize::Done(n) | DirSize::Cached(n)) => Some(n),
        _ => None,
    }
}

struct Pane {
    path: PathBuf,
    entries: Vec<Entry>,
    /// Index into the *visible* list (after filter/hidden files).
    cursor: usize,
    marked: HashSet<PathBuf>,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    /// Typed input: a filter, or a jump target if it starts with `~`/`/`.
    query: String,
    sort: Sort,
    /// Cancels size computation when the pane leaves the folder.
    sizing: Cancel,
    scroll_y: f32,
    view_h: f32,
    /// Smooth scroll target; `None` = no animation.
    scroll_target: Option<f32>,
    id: Id,
    /// Located on a network drive.
    network: bool,
    /// Showing the inside of an archive (read-only).
    archive: bool,
}

/// What happens after a folder has been read.
#[derive(Clone)]
enum After {
    /// Initial folder at startup.
    Start,
    Nav,
    Back,
    Forward,
    /// Up, with the cursor on the folder we came from.
    Parent(PathBuf),
    /// Same folder, re-read; cursor stays on its entry.
    Reload,
}

/// Folder currently being read in the background.
struct Loading {
    path: PathBuf,
    rx: std::sync::mpsc::Receiver<std::io::Result<Vec<Entry>>>,
    since: std::time::Instant,
    after: After,
}

/// File operations run one after another in the background.
#[derive(Default)]
struct Jobs {
    running: Option<(OpKind, Job, String)>,
    queue: std::collections::VecDeque<(OpKind, Vec<PathBuf>, Option<PathBuf>, String)>,
    /// Notes collected from finished jobs, shown once the queue is empty.
    problems: Vec<String>,
    /// Jobs started since the queue was last empty, for "2 of 3".
    started: usize,
}

impl Jobs {
    fn busy(&self) -> bool {
        self.running.is_some() || !self.queue.is_empty()
    }
}

/// How long to wait synchronously for a read; local folders are almost always
/// faster and thus appear without an intermediate state.
const LOAD_WAIT: Duration = Duration::from_millis(120);
/// After this, a folder counts as "not responding".
const LOAD_SLOW: Duration = Duration::from_secs(3);

impl Pane {
    fn new(path: PathBuf, id: &'static str) -> Self {
        let mut p = Self {
            path: PathBuf::new(),
            entries: Vec::new(),
            cursor: 0,
            marked: HashSet::new(),
            back: Vec::new(),
            forward: Vec::new(),
            query: String::new(),
            sort: Sort::Name,
            sizing: Cancel::default(),
            scroll_y: 0.0,
            view_h: 400.0,
            scroll_target: None,
            id: Id::new(id),
            network: false,
            archive: false,
        };
        p.network = guppy_core::platform::is_network(&path);
        p.path = path;
        p
    }

    /// Shows a freshly read folder.
    fn enter(&mut self, path: PathBuf, entries: Vec<Entry>) {
        self.network = guppy_core::platform::is_network(&path);
        self.archive = !path.is_dir();
        self.entries = entries;
        self.path = path;
        self.cursor = 0;
        self.marked.clear();
        self.query.clear();
        // A new folder starts at the top, without animation.
        self.scroll_y = 0.0;
        self.scroll_target = Some(0.0);
    }

    fn visible(&self, hidden: bool, sizes: &DirSizes) -> Vec<&Entry> {
        let f = if is_jump(&self.query) { String::new() } else { self.query.to_lowercase() };
        let mut v: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| hidden || !e.hidden)
            .filter(|e| f.is_empty() || e.name.to_lowercase().contains(&f))
            .collect();
        // `entries` is already sorted by name; sort_by is stable.
        match self.sort {
            Sort::Name => {}
            Sort::Size => v.sort_by_key(|e| std::cmp::Reverse(entry_size(e, sizes).unwrap_or(0))),
            Sort::Date => v.sort_by_key(|e| std::cmp::Reverse(e.modified)),
        }
        v
    }
}

struct Guppy {
    theme: guppy_core::Theme,
    panes: [Pane; 2],
    active: usize,
    show_hidden: bool,
    papers: [Paper; 2],
    scanlines: [Scanlines; 2],
    sizes: DirSizes,
    jumper: Jumper,
    /// Jump mode hits and the selected row.
    jump: Vec<PathBuf>,
    jump_cur: usize,
    shot_wait: u8,
    /// Jump hits still need refreshing after indexing.
    jump_pending: bool,
    dialog: Option<Dialog>,
    rename: Option<Rename>,
    last_frame: Option<std::time::Instant>,
    /// Window width, for the responsive layout.
    width: f32,
    /// Last cursor move, to detect key repeat.
    last_move: Option<std::time::Instant>,
    /// Folders currently being read, per side.
    loads: [Option<Loading>; 2],
    config: Config,
    jobs: Jobs,
    /// Quick view on: the other pane shows the entry under the cursor.
    quick: bool,
    /// Entry the quick view should show, and what it shows now.
    quick_want: Option<PathBuf>,
    quick_view: Option<(PathBuf, ViewBody)>,
    /// Counts cursor changes, so only the latest one loads.
    quick_gen: u64,
    /// Last pattern used with `+` / `-`.
    last_pattern: String,
    /// Window close was requested while jobs run; the next request quits.
    close_armed: bool,
}

fn sort_from(s: SortBy) -> Sort {
    match s {
        SortBy::Name => Sort::Name,
        SortBy::Size => Sort::Size,
        SortBy::Date => Sort::Date,
    }
}

impl Guppy {
    fn new() -> (Self, Task<Message>) {
        let home = dirs_home();
        let cwd = std::env::current_dir().unwrap_or_else(|_| home.clone());
        let (config, config_error) = Config::load();
        guppy_core::du::set_network_threads(config.network.threads);
        let mut g = Self {
            theme: load_theme(config.view.look),
            panes: [
                Pane::new(Config::resolve(&config.start.left, &cwd), "pane-left"),
                Pane::new(Config::resolve(&config.start.right, &cwd), "pane-right"),
            ],
            active: 0,
            show_hidden: config.view.show_hidden,
            papers: [Paper::new(1), Paper::new(2)],
            scanlines: [Scanlines::default(), Scanlines::default()],
            sizes: DirSizes::default_cache_file().map(DirSizes::with_cache).unwrap_or_default(),
            jumper: Jumper::load(),
            jump: Vec::new(),
            jump_cur: 0,
            shot_wait: 0,
            jump_pending: false,
            dialog: None,
            rename: None,
            last_frame: None,
            width: 1200.0,
            last_move: None,
            loads: [None, None],
            config,
            jobs: Jobs::default(),
            quick: false,
            quick_want: None,
            quick_view: None,
            quick_gen: 0,
            last_pattern: "*".into(),
            close_armed: false,
        };
        let sort = sort_from(g.config.view.sort);
        g.panes.iter_mut().for_each(|p| p.sort = sort);
        if let Some(e) = config_error {
            g.dialog = Some(Dialog::Report { title: "Settings not loaded", problems: vec![e] });
        } else if g.config.start.help && std::env::var_os("GUPPY_SCREENSHOT").is_none() {
            g.dialog = Some(Dialog::Help { startup: true });
        }
        for side in 0..2 {
            let path = g.panes[side].path.clone();
            g.load(side, path, After::Start);
        }
        // Developer aid: preset input, e.g. `GUPPY_QUERY=~gu`.
        if let Ok(q) = std::env::var("GUPPY_QUERY") {
            g.set_query(q);
        }
        // `GUPPY_QUICK=1` starts with the quick view open.
        g.quick = std::env::var_os("GUPPY_QUICK").is_some();
        if std::env::var("GUPPY_SORT").as_deref() == Ok("size") {
            g.panes.iter_mut().for_each(|p| p.sort = Sort::Size);
        }
        // `GUPPY_DIALOG=copy` opens the copy dialog, `=run` also confirms it.
        if let Ok(d) = std::env::var("GUPPY_DIALOG") {
            if d == "view" {
                let task = g.open_view();
                return (g, task);
            }
            if d == "rename" {
                g.start_rename();
                return (g, Task::none());
            }
            if d == "settings" {
                g.dialog = Some(Dialog::Settings { row: 2, edit: None });
                return (g, Task::none());
            }
            if d == "splash" {
                g.dialog = Some(Dialog::Help { startup: true });
                return (g, Task::none());
            }
            if let Some(q) = d.strip_prefix("palette") {
                g.dialog = Some(Dialog::Palette { query: q.trim_start_matches(':').to_string(), cur: 0 });
                return (g, Task::none());
            }
            if d == "drives" {
                g.open_drives(0);
                return (g, Task::none());
            }
            if d == "help" {
                g.dialog = Some(Dialog::Help { startup: false });
                return (g, Task::none());
            }
            g.ask(OpKind::Copy);
            if d == "run" {
                g.confirm_dialog();
            }
        }
        (g, Task::none())
    }

    fn title(&self) -> String {
        format!("Guppy — {}", self.panes[self.active].path.display())
    }

    fn theme(&self) -> iced::Theme {
        let t = &self.theme;
        iced::Theme::custom(
            "Guppy",
            iced::theme::Palette {
                background: rgb(t.background),
                text: rgb(t.foreground),
                primary: rgb(t.accent),
                success: rgb(t.marked),
                warning: rgb(t.accent),
                danger: rgb(t.red),
            },
        )
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut dirs: Vec<PathBuf> = self.panes.iter().map(|p| p.path.clone()).collect();
        if let Some(theme_dir) = guppy_core::Theme::omarchy_path().and_then(|p| p.parent().map(Path::to_path_buf)) {
            // The theme directory and its parent (Omarchy replaces it on switch).
            dirs.extend(theme_dir.parent().map(Path::to_path_buf));
            dirs.push(theme_dir);
        }
        dirs.extend(Config::path().and_then(|p| p.parent().map(Path::to_path_buf)));
        let mut keys = Subscription::batch([
            keyboard::listen().map(Message::Key),
            iced::window::resize_events().map(|(_, size)| Message::Resized(size)),
            iced::window::close_requests().map(Message::CloseRequested),
            watch::changes(watch::Targets { dirs }).map(Message::FsChanged),
        ]);
        if self.panes.iter().any(|p| p.scroll_target.is_some()) {
            keys = Subscription::batch([keys, iced::window::frames().map(Message::Frame)]);
        }
        // Only while sizes are computed; no idle ticking otherwise.
        let running = self.jobs.busy() || !self.jobs.problems.is_empty();
        let loading = self.loads.iter().any(Option::is_some);
        if self.sizes.busy() || self.jump_pending || running || loading || std::env::var_os("GUPPY_SCREENSHOT").is_some() {
            Subscription::batch([keys, iced::time::every(Duration::from_millis(120)).map(|_| Message::Tick)])
        } else {
            keys
        }
    }

    fn current(&self) -> Option<&Entry> {
        let p = &self.panes[self.active];
        p.visible(self.show_hidden, &self.sizes).get(p.cursor).copied()
    }

    /// In a narrow window the date column is dropped.
    fn show_dates(&self) -> bool {
        self.width >= 760.0
    }

    fn jumping(&self) -> bool {
        is_jump(&self.panes[self.active].query)
    }

    fn update(&mut self, msg: Message) -> Task<Message> {
        let task = self.handle(msg);
        Task::batch([task, self.sync_quick()])
    }

    /// Follows the cursor: after a short rest, loads the entry for the quick view.
    fn sync_quick(&mut self) -> Task<Message> {
        if !self.quick {
            return Task::none();
        }
        let want = if self.jumping() { None } else { self.current().map(|e| e.path.clone()) };
        if want == self.quick_want {
            return Task::none();
        }
        self.quick_want = want;
        self.quick_gen += 1;
        let generation = self.quick_gen;
        Task::perform(smol::Timer::after(Duration::from_millis(90)), move |_| Message::QuickDue(generation))
    }

    fn handle(&mut self, msg: Message) -> Task<Message> {
        if matches!(msg, Message::Tick) {
            self.poll_jobs();
        }
        match msg {
            // On the file list (empty search): files to / from the clipboard.
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. })
                if self.dialog.is_none()
                    && self.rename.is_none()
                    && self.panes[self.active].query.is_empty()
                    && clip_key(&key, modifiers).is_some() =>
            {
                if clip_key(&key, modifiers) == Some('v') {
                    return Task::perform(smol::unblock(clip::get), Message::FilesPasted);
                }
                if self.read_only() {
                    return Task::none();
                }
                let paths = self.selection();
                if !paths.is_empty()
                    && let Err(e) = clip::put(&paths, clip_key(&key, modifiers) == Some('x'))
                {
                    self.dialog = Some(Dialog::Report { title: "Clipboard", problems: vec![e] });
                }
                return Task::none();
            }
            Message::FilesPasted(None) => {}
            Message::FilesPasted(Some((sources, cut))) => {
                let target = self.panes[self.active].path.clone();
                if self.read_only() {
                } else if sources.iter().all(|s| s.parent() == Some(target.as_path())) {
                    self.dialog = Some(Dialog::Report { title: "Not possible", problems: vec!["The files are already in this folder.".into()] });
                } else {
                    self.ask_with(if cut { OpKind::Move } else { OpKind::Copy }, sources, Some(target));
                }
            }
            // Copy / cut / paste in every one-line text field.
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. })
                if clip_key(&key, modifiers).is_some() && self.field_mut().is_some() =>
            {
                let op = clip_key(&key, modifiers).expect("checked above");
                if op == 'v' {
                    return iced::clipboard::read().map(Message::Paste);
                }
                // Rename with a selection: only the selected part.
                let partial = self.rename.as_mut().and_then(|r| {
                    let part = r.selected()?.to_string();
                    if op == 'x' {
                        r.delete_selection();
                        r.error = None;
                    }
                    Some(part)
                });
                let copied = match partial {
                    Some(p) => p,
                    None => {
                        let field = self.field_mut().expect("checked above");
                        let all = if op == 'x' { std::mem::take(field) } else { field.clone() };
                        if let Some(r) = &mut self.rename {
                            r.caret = r.caret.min(r.name.chars().count());
                            r.anchor = None;
                        }
                        all
                    }
                };
                // Empty field: leave the clipboard alone.
                if copied.is_empty() {
                    return Task::none();
                }
                return iced::clipboard::write(copied);
            }
            Message::Paste(Some(clip)) => {
                // One-line fields: newlines become spaces.
                let clip = clip.trim_end_matches(['\n', '\r']).replace(['\n', '\r'], " ");
                if let Some(r) = &mut self.rename {
                    r.insert(&clip);
                } else if self.dialog.is_none() {
                    let q = format!("{}{clip}", self.panes[self.active].query);
                    self.set_query(q);
                } else if let Some(field) = self.field_mut() {
                    field.push_str(&clip);
                }
            }
            Message::Paste(None) => {}
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, text, .. }) if self.rename.is_some() => {
                self.on_rename_key(key, modifiers, text.as_deref());
                return Task::none();
            }
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. }) if matches!(self.dialog, Some(Dialog::Edit { .. })) => {
                // Only reaches here when the editor itself is not focused.
                return match key.as_ref() {
                    Key::Named(Named::Escape) => self.update(Message::EditClose),
                    Key::Character("s") if modifiers.command() => self.update(Message::EditSave),
                    _ => operation::focus(editor_id()),
                };
            }
            Message::Key(keyboard::Event::KeyPressed { key, text, .. }) if self.dialog.is_some() => {
                if matches!(self.dialog, Some(Dialog::View { .. })) {
                    return self.on_view_key(key);
                }
                if matches!(self.dialog, Some(Dialog::Palette { .. })) {
                    return self.on_palette_key(key, text.as_deref());
                }
                if matches!(self.dialog, Some(Dialog::Drives { .. })) {
                    return self.on_drives_key(key, text.as_deref());
                }
                self.on_dialog_key(key, text.as_deref());
                return Task::none();
            }
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, text, .. }) => {
                return self.on_key(key, modifiers, text.as_deref());
            }
            Message::Tick if self.jump_pending => {
                // Subfolder index is growing: refresh hits, keep selection.
                // One last time after indexing ends.
                self.jump_pending = self.jumper.indexing();
                if self.jumping() {
                    let cur = self.jump_cur;
                    self.update_jump();
                    self.jump_cur = cur.min(self.jump.len().saturating_sub(1));
                }
                return Task::none();
            }
            Message::Tick if self.loads.iter().any(Option::is_some) => {
                self.poll_loads();
                return Task::none();
            }
            Message::Tick if std::env::var_os("GUPPY_SCREENSHOT").is_some() && !self.sizes.busy() && !self.jump_pending => {
                // Wait a few ticks so the final state is drawn.
                self.shot_wait += 1;
                if self.shot_wait < 4 {
                    return Task::none();
                }
                return iced::window::latest().and_then(iced::window::screenshot).map(Message::Shot);
            }
            Message::Key(_) | Message::Tick => return Task::none(),
            // Not while typing a name or with a dialog open (the mouse may just pass by).
            Message::HoverPane(side) if self.rename.is_none() && self.dialog.is_none() && !self.quick => self.active = side,
            Message::HoverPane(_) => {}
            Message::Select(side, i) => {
                self.rename = None;
                self.active = side;
                self.panes[side].cursor = i;
            }
            Message::Open(side, i) => {
                self.rename = None;
                self.active = side;
                self.panes[side].cursor = i;
                return self.open_current();
            }
            Message::JumpTo(i) => {
                self.jump_cur = i;
                self.jump_selected();
            }
            Message::Frame(now) => return self.animate_scroll(now),
            Message::CloseRequested(id) => {
                // Running jobs: the first request shows them, the second quits.
                if self.jobs.busy() && !self.close_armed {
                    self.close_armed = true;
                    self.dialog = Some(Dialog::Jobs);
                    return Task::none();
                }
                let _ = self.sizes.save();
                return iced::window::close(id);
            }
            Message::FsChanged(paths) => {
                self.on_fs_changed(&paths);
                return Task::none();
            }
            Message::Resized(size) => {
                self.width = size.width;
                return Task::none();
            }
            Message::Scrolled(side, vp) => {
                let p = &mut self.panes[side];
                p.view_h = vp.bounds().height;
                // During animation the list reports the position of an earlier frame;
                // our own value is authoritative then.
                if p.scroll_target.is_none() {
                    p.scroll_y = vp.absolute_offset().y;
                }
                return Task::none();
            }
            Message::Back => self.history(true),
            Message::Forward => self.history(false),
            Message::Parent => self.go_parent(),
            Message::Home => self.navigate(dirs_home()),
            Message::ToggleMark => self.toggle_mark(),
            Message::SwitchSide => self.active = 1 - self.active,
            Message::Op(kind) => self.ask(kind),
            Message::NewDir => self.new_dir(),
            Message::DialogConfirm => self.confirm_dialog(),
            Message::DialogCancel => self.cancel_dialog(),
            Message::OpenSettings => self.dialog = Some(Dialog::Settings { row: 0, edit: None }),
            Message::Viewed(path, body) => {
                if let Some(Dialog::View { path: p, body: b }) = &mut self.dialog
                    && *p == path
                {
                    *b = body;
                }
            }
            Message::EditLoaded(path, result) => {
                if let Some(Dialog::Edit { path: p, content, .. }) = &mut self.dialog
                    && *p == path
                {
                    match result {
                        Ok(s) => {
                            *content = Some(text_editor::Content::with_text(&s));
                            return operation::focus(editor_id());
                        }
                        Err(e) => self.dialog = Some(Dialog::Report { title: "Not editable", problems: vec![e] }),
                    }
                }
            }
            Message::EditAction(action) => {
                if let Some(Dialog::Edit { content: Some(c), dirty, closing, error, .. }) = &mut self.dialog {
                    if action.is_edit() {
                        *dirty = true;
                        *closing = false;
                        *error = None;
                    }
                    c.perform(action);
                }
            }
            Message::EditSave => {
                if let Some(Dialog::Edit { path, content: Some(c), dirty, closing, error }) = &mut self.dialog {
                    match std::fs::write(&*path, c.text()) {
                        Ok(()) => {
                            *dirty = false;
                            *closing = false;
                            *error = None;
                        }
                        Err(e) => *error = Some(e.to_string()),
                    }
                }
            }
            Message::EditClose => {
                if let Some(Dialog::Edit { dirty, closing, .. }) = &mut self.dialog {
                    if *dirty && !*closing {
                        *closing = true;
                    } else {
                        self.dialog = None;
                        self.reload(self.active);
                    }
                }
            }
            Message::Resolve(r) => {
                if let Some(Dialog::Conflict { all, .. }) = self.dialog.take()
                    && let Some((_, job, _)) = &self.jobs.running
                {
                    job.answer(r, all);
                }
            }
            Message::ToggleAll => {
                if let Some(Dialog::Conflict { all, .. }) = &mut self.dialog {
                    *all = !*all;
                }
            }
            Message::OpenJobs => self.dialog = Some(Dialog::Jobs),
            Message::PaletteRun(i) => {
                self.dialog = None;
                if let Some((_, _, cmd)) = COMMANDS.get(i) {
                    return self.run_cmd(*cmd);
                }
            }
            Message::DrivePick(i) => {
                if let Some(Dialog::Drives { cur, .. }) = &mut self.dialog {
                    *cur = i;
                }
                self.pick_drive();
            }
            Message::ToggleQuick => {
                self.quick = !self.quick;
                self.quick_want = None;
                self.quick_view = None;
            }
            Message::QuickDue(generation) => {
                if generation == self.quick_gen
                    && let Some(path) = self.quick_want.clone()
                {
                    return Task::perform(
                        async move {
                            let p = path.clone();
                            (path, smol::unblock(move || preview::load(&p, true)).await)
                        },
                        |(p, b)| Message::QuickLoaded(p, b),
                    );
                }
            }
            Message::QuickLoaded(path, body) => {
                if self.quick_want.as_ref() == Some(&path) {
                    self.quick_view = Some((path, body));
                    return operation::scroll_to(quick_id(), AbsoluteOffset { x: None, y: Some(0.0) });
                }
            }
            Message::CancelJobs(everything) => {
                if everything {
                    self.jobs.queue.clear();
                }
                if let Some((_, job, _)) = &self.jobs.running {
                    job.cancel();
                }
                if matches!(self.dialog, Some(Dialog::Conflict { .. })) {
                    self.dialog = None;
                }
            }
            Message::Opened(Ok(())) => {}
            Message::Opened(Err(e)) => self.dialog = Some(Dialog::Report { title: "Not opened", problems: vec![e] }),
            Message::ToggleStartHelp => {
                let mut new = self.config.clone();
                new.start.help = !new.start.help;
                self.set_config(new);
            }
            Message::SettingClick(row) => {
                if let Some(Dialog::Settings { row: sel, edit }) = &mut self.dialog {
                    *sel = row;
                    *edit = None;
                }
                self.settings_activate(1);
            }
            Message::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                self.panes.iter_mut().for_each(|p| p.cursor = 0);
            }
            Message::Shot(shot) => {
                if let Some(path) = std::env::var_os("GUPPY_SCREENSHOT") {
                    save_ppm(&path, &shot);
                }
                return iced::exit();
            }
        }
        self.scroll_into_view()
    }

    /// Keyboard model: letters filter the active pane, a leading `~`/`/` jumps.
    fn on_key(&mut self, key: Key, m: keyboard::Modifiers, typed: Option<&str>) -> Task<Message> {
        let jumping = self.jumping();
        let query_empty = self.panes[self.active].query.is_empty();
        let len = if jumping { self.jump.len() } else { self.panes[self.active].visible(self.show_hidden, &self.sizes).len() };
        let page = (self.panes[self.active].view_h / ROW_H).max(1.0) as usize;
        let cur = if jumping { self.jump_cur } else { self.panes[self.active].cursor };
        let mut move_to = |i: usize| {
            let i = i.min(len.saturating_sub(1));
            if jumping { self.jump_cur = i } else { self.panes[self.active].cursor = i }
        };

        match key.as_ref() {
            Key::Named(Named::ArrowDown) => move_to(cur + 1),
            Key::Named(Named::ArrowUp) => move_to(cur.saturating_sub(1)),
            Key::Named(Named::PageDown) => move_to(cur + page),
            Key::Named(Named::PageUp) => move_to(cur.saturating_sub(page)),
            Key::Named(Named::Home) => move_to(0),
            Key::Named(Named::End) => move_to(usize::MAX),
            Key::Named(Named::F1) if m.alt() => self.open_drives(0),
            Key::Named(Named::F2) if m.alt() => self.open_drives(1),
            Key::Named(Named::F1) => self.dialog = Some(Dialog::Help { startup: false }),
            Key::Named(Named::F2) => {
                if !self.read_only() {
                    self.start_rename();
                }
            }
            Key::Named(Named::F3) => return self.open_view(),
            Key::Named(Named::F4) if m.shift() => self.edit_external(),
            Key::Named(Named::F4) => return self.edit_current(),
            Key::Named(Named::F12) => return self.update(Message::OpenSettings),
            Key::Character(",") if m.control() => return self.update(Message::OpenSettings),
            Key::Named(Named::Enter) if jumping => self.jump_selected(),
            Key::Named(Named::Enter) => return self.open_current(),
            Key::Named(Named::Backspace) if !query_empty => {
                let mut q = self.panes[self.active].query.clone();
                q.pop();
                self.set_query(q);
            }
            Key::Named(Named::Backspace) => self.go_parent(),
            // Esc first cancels a pending folder change, then the search.
            Key::Named(Named::Escape) if self.loads[self.active].is_some() => self.loads[self.active] = None,
            Key::Named(Named::Escape) => self.set_query(String::new()),
            Key::Named(Named::Tab) => self.active = 1 - self.active,
            Key::Named(Named::Insert) => self.toggle_mark(),
            Key::Named(Named::Space) if query_empty => self.toggle_mark(),
            Key::Named(Named::ArrowLeft) if m.alt() => self.history(true),
            Key::Named(Named::ArrowRight) if m.alt() => self.history(false),
            Key::Named(Named::F5) => self.ask(OpKind::Copy),
            Key::Named(Named::F6) => self.ask(OpKind::Move),
            Key::Named(Named::F7) => self.new_dir(),
            Key::Named(Named::F8 | Named::Delete) if m.shift() => self.ask(OpKind::Delete),
            Key::Named(Named::F8 | Named::Delete) => self.ask(OpKind::Trash),
            Key::Character("g") if m.control() => self.set_query("~".into()),
            // The cursor keeps its row; the entries reorder beneath it.
            Key::Character("s") if m.control() => self.cycle_sort(),
            Key::Character("h") if m.control() => return self.update(Message::ToggleHidden),
            Key::Character("j") if m.control() => self.dialog = Some(Dialog::Jobs),
            Key::Character("q") if m.control() => return self.update(Message::ToggleQuick),
            Key::Character("p") if m.control() => self.dialog = Some(Dialog::Palette { query: String::new(), cur: 0 }),
            // On the file list (empty search): `+` / `-` mark by pattern, `*` inverts.
            _ if query_empty && !m.control() && !m.alt() && matches!(typed, Some("+" | "-")) => {
                self.dialog = Some(Dialog::Mark { add: typed == Some("+"), pattern: self.last_pattern.clone() });
            }
            _ if query_empty && !m.control() && !m.alt() && typed == Some("*") => self.invert_marks(),
            Key::Character("r") if m.control() => self.recompute_sizes(),
            // Anything printable without Ctrl/Alt goes into the input.
            _ => match typed {
                Some(t) if !m.control() && !m.alt() && !m.logo() && t.chars().all(|c| !c.is_control()) => {
                    let q = format!("{}{t}", self.panes[self.active].query);
                    self.set_query(q);
                }
                _ => return Task::none(),
            },
        }
        self.scroll_into_view()
    }

    /// Action target: marked visible entries, otherwise the one under the cursor.
    fn selection(&self) -> Vec<PathBuf> {
        let p = &self.panes[self.active];
        let visible = p.visible(self.show_hidden, &self.sizes);
        let marked: Vec<PathBuf> = visible.iter().filter(|e| p.marked.contains(&e.path)).map(|e| e.path.clone()).collect();
        if !marked.is_empty() {
            return marked;
        }
        visible.get(p.cursor).map(|e| vec![e.path.clone()]).unwrap_or_default()
    }

    /// Takes over a changed config file. View settings only apply when they
    /// changed in the file, so toggles made in the app are not overwritten.
    fn apply_config(&mut self) {
        let (new, error) = Config::load();
        if let Some(e) = error {
            self.dialog = Some(Dialog::Report { title: "Settings not loaded", problems: vec![e] });
            return;
        }
        self.use_config(new);
    }

    /// Saves settings changed in the overlay and applies them right away.
    fn set_config(&mut self, new: Config) {
        if let Err(e) = new.save() {
            self.dialog = Some(Dialog::Report { title: "Settings not saved", problems: vec![e.to_string()] });
        }
        self.use_config(new);
    }

    /// Enter/Space/←/→ on the selected settings row.
    fn settings_activate(&mut self, dir: i32) {
        let Some(Dialog::Settings { row, edit }) = &mut self.dialog else { return };
        let row = *row;
        if setting_is_text(row) {
            let c = &self.config.start;
            *edit = Some(if row == 7 { c.left.clone() } else { c.right.clone() });
            return;
        }
        let mut new = self.config.clone();
        setting_step(&mut new, row, dir);
        self.set_config(new);
    }

    fn on_settings_key(&mut self, key: Key, typed: Option<&str>) {
        let Some(Dialog::Settings { row, edit }) = &mut self.dialog else { return };
        // Typing a start folder.
        if let Some(text) = edit {
            match key.as_ref() {
                Key::Named(Named::Enter) => {
                    let value = std::mem::take(text);
                    let row = *row;
                    *edit = None;
                    let mut new = self.config.clone();
                    if row == 7 { new.start.left = value } else { new.start.right = value }
                    self.set_config(new);
                }
                Key::Named(Named::Escape) => *edit = None,
                Key::Named(Named::Backspace) => {
                    text.pop();
                }
                _ => {
                    if let Some(t) = typed.filter(|t| t.chars().all(|c| !c.is_control())) {
                        text.push_str(t);
                    }
                }
            }
            return;
        }
        match key.as_ref() {
            Key::Named(Named::ArrowDown) => *row = (*row + 1).min(SETTINGS.len() - 1),
            Key::Named(Named::ArrowUp) => *row = row.saturating_sub(1),
            Key::Named(Named::Home) => *row = 0,
            Key::Named(Named::End) => *row = SETTINGS.len() - 1,
            Key::Named(Named::Enter | Named::Space) | Key::Named(Named::ArrowRight) => self.settings_activate(1),
            Key::Named(Named::ArrowLeft) => self.settings_activate(-1),
            Key::Named(Named::Escape | Named::F12) => self.dialog = None,
            // On a start-folder row, "." takes the folder currently shown in that pane.
            Key::Character(".") if setting_is_text(*row) => {
                let row = *row;
                let mut new = self.config.clone();
                let here = self.jumper.display(&self.panes[row - 7].path);
                if row == 7 { new.start.left = here } else { new.start.right = here }
                self.set_config(new);
            }
            _ => {}
        }
    }

    /// Applies new settings. View settings only change what differs from the
    /// previous settings, so toggles made in the app are not overwritten.
    fn use_config(&mut self, new: Config) {
        let old = std::mem::replace(&mut self.config, new);
        let v = &self.config.view;
        if v.show_hidden != old.view.show_hidden {
            self.show_hidden = v.show_hidden;
        }
        if v.sort != old.view.sort {
            let sort = sort_from(v.sort);
            self.panes.iter_mut().for_each(|p| p.sort = sort);
        }
        if v.look != old.view.look {
            self.theme = load_theme(v.look);
        }
        if v.dir_sizes && !old.view.dir_sizes {
            self.request_sizes(0);
            self.request_sizes(1);
        }
    }

    fn ask(&mut self, kind: OpKind) {
        let sources = self.selection();
        if sources.is_empty() {
            return;
        }
        // Archives are read-only: copying out of one extracts, nothing goes in.
        if self.panes[self.active].archive && kind != OpKind::Copy {
            self.read_only();
            return;
        }
        if self.panes[1 - self.active].archive && matches!(kind, OpKind::Copy | OpKind::Move) {
            self.dialog = Some(Dialog::Report { title: "Archive is read-only", problems: vec!["Files can't be copied or moved into an archive.".into()] });
            return;
        }
        let target = matches!(kind, OpKind::Copy | OpKind::Move).then(|| self.panes[1 - self.active].path.clone());
        if target.as_deref() == Some(self.panes[self.active].path.as_path()) {
            self.dialog = Some(Dialog::Report { title: "Not possible", problems: vec!["Source and target are the same folder.".into()] });
            return;
        }
        self.ask_with(kind, sources, target);
    }

    /// Asks for confirmation (if configured), then runs the operation.
    fn ask_with(&mut self, kind: OpKind, sources: Vec<PathBuf>, target: Option<PathBuf>) {
        let c = &self.config.confirm;
        let confirm = match kind {
            OpKind::Copy => c.copy,
            OpKind::Move => c.move_,
            OpKind::Trash => c.trash,
            OpKind::Delete => c.delete,
        };
        self.dialog = Some(Dialog::Confirm { kind, sources, target });
        if !confirm {
            self.confirm_dialog();
        }
    }

    fn confirm_dialog(&mut self) {
        match self.dialog.take() {
            Some(Dialog::Confirm { kind, sources, target }) => self.enqueue(kind, sources, target),
            Some(Dialog::Mark { add, pattern }) => {
                self.mark_pattern(add, &pattern);
                self.last_pattern = pattern;
            }
            // Enter on a conflict: the safe answer.
            Some(Dialog::Conflict { all, .. }) => {
                if let Some((_, job, _)) = &self.jobs.running {
                    job.answer(Resolve::Skip, all);
                }
            }
            Some(Dialog::NewDir { name, .. }) => match ops::make_dir(&self.panes[self.active].path, name.trim()) {
                Ok(created) => {
                    self.reload(self.active);
                    let p = &self.panes[self.active];
                    if let Some(i) = p.visible(self.show_hidden, &self.sizes).iter().position(|e| e.path == created) {
                        self.panes[self.active].cursor = i;
                    }
                }
                Err(e) => self.dialog = Some(Dialog::NewDir { name, error: Some(e.to_string()) }),
            },
            Some(d @ Dialog::Settings { .. }) => self.dialog = Some(d),
            Some(d @ (Dialog::View { .. } | Dialog::Edit { .. })) => self.dialog = Some(d),
            Some(d @ (Dialog::Palette { .. } | Dialog::Drives { .. })) => self.dialog = Some(d),
            Some(Dialog::Report { .. } | Dialog::Help { .. } | Dialog::Jobs) | None => {}
        }
    }

    fn cancel_dialog(&mut self) {
        match &self.dialog {
            // Esc on a conflict stops the whole job.
            Some(Dialog::Conflict { .. }) => {
                let _ = self.update(Message::CancelJobs(false));
            }
            _ => self.dialog = None,
        }
    }

    fn on_dialog_key(&mut self, key: Key, typed: Option<&str>) {
        if matches!(self.dialog, Some(Dialog::Settings { .. })) {
            self.on_settings_key(key, typed);
            return;
        }
        match key.as_ref() {
            Key::Named(Named::Enter) => self.confirm_dialog(),
            Key::Named(Named::Escape) => self.cancel_dialog(),
            Key::Named(Named::F1) if matches!(self.dialog, Some(Dialog::Help { .. })) => self.dialog = None,
            Key::Named(Named::Delete) if matches!(self.dialog, Some(Dialog::Jobs)) => {
                let _ = self.update(Message::CancelJobs(false));
            }
            Key::Character("j") if matches!(self.dialog, Some(Dialog::Jobs)) => self.dialog = None,
            Key::Character(c) if matches!(self.dialog, Some(Dialog::Conflict { .. })) => {
                let msg = match c {
                    "o" => Message::Resolve(Resolve::Overwrite),
                    "s" => Message::Resolve(Resolve::Skip),
                    "k" => Message::Resolve(Resolve::KeepBoth),
                    "a" => Message::ToggleAll,
                    _ => return,
                };
                let _ = self.update(msg);
            }
            Key::Named(Named::Space) if matches!(self.dialog, Some(Dialog::Help { startup: true })) => {
                let _ = self.update(Message::ToggleStartHelp);
            }
            _ => {
                let (name, error) = match &mut self.dialog {
                    Some(Dialog::NewDir { name, error }) => (name, Some(error)),
                    Some(Dialog::Mark { pattern, .. }) => (pattern, None),
                    _ => return,
                };
                match (key.as_ref(), typed) {
                    (Key::Named(Named::Backspace), _) => {
                        name.pop();
                    }
                    (_, Some(t)) if t.chars().all(|c| !c.is_control()) => name.push_str(t),
                    _ => return,
                }
                if let Some(e) = error {
                    *e = None;
                }
            }
        }
    }

    /// Starts editing the name in place with the name (without extension) selected.
    fn start_rename(&mut self) {
        let Some(e) = self.current() else { return };
        let stem = match e.name.rfind('.') {
            Some(dot) if dot > 0 && !e.is_dir() => e.name[..dot].chars().count(),
            _ => e.name.chars().count(),
        };
        self.rename = Some(Rename { side: self.active, from: e.path.clone(), name: e.name.clone(), caret: stem, anchor: Some(0), error: None });
    }

    /// The one-line text field that currently has the keyboard, if any.
    fn field_mut(&mut self) -> Option<&mut String> {
        if let Some(r) = &mut self.rename {
            return Some(&mut r.name);
        }
        match &mut self.dialog {
            None => Some(&mut self.panes[self.active].query),
            Some(Dialog::NewDir { name, .. } | Dialog::Mark { pattern: name, .. } | Dialog::Palette { query: name, .. }) => Some(name),
            Some(Dialog::Settings { edit: Some(text), .. }) => Some(text),
            _ => None,
        }
    }

    fn on_rename_key(&mut self, key: Key, m: keyboard::Modifiers, typed: Option<&str>) {
        let Some(r) = &mut self.rename else { return };
        let len = r.name.chars().count();
        let sel = m.shift();
        match key.as_ref() {
            Key::Named(Named::Escape) => self.rename = None,
            Key::Named(Named::Enter) => self.finish_rename(),
            Key::Character("a") if m.command() => {
                r.anchor = Some(0);
                r.caret = len;
            }
            // Left/Right without Shift on a selection: go to its edge.
            Key::Named(Named::ArrowLeft) if !sel && r.selection().is_some() => {
                let lo = r.selection().map_or(0, |s| s.0);
                r.move_to(lo, false);
            }
            Key::Named(Named::ArrowRight) if !sel && r.selection().is_some() => {
                let hi = r.selection().map_or(0, |s| s.1);
                r.move_to(hi, false);
            }
            Key::Named(Named::ArrowLeft) => r.move_to(r.caret.saturating_sub(1), sel),
            Key::Named(Named::ArrowRight) => r.move_to((r.caret + 1).min(len), sel),
            Key::Named(Named::Home) => r.move_to(0, sel),
            Key::Named(Named::End) => r.move_to(len, sel),
            Key::Named(Named::Backspace | Named::Delete) if r.delete_selection() => r.error = None,
            Key::Named(Named::Backspace) if r.caret > 0 => {
                r.caret -= 1;
                let b = r.byte(r.caret);
                r.name.remove(b);
                r.anchor = None;
                r.error = None;
            }
            Key::Named(Named::Delete) if r.caret < len => {
                let b = r.byte(r.caret);
                r.name.remove(b);
                r.anchor = None;
                r.error = None;
            }
            _ => {
                if let Some(t) = typed.filter(|t| !m.command() && t.chars().all(|c| !c.is_control())) {
                    r.insert(t);
                }
            }
        }
    }

    fn finish_rename(&mut self) {
        let Some(r) = &mut self.rename else { return };
        match ops::rename(&r.from, r.name.trim()) {
            Ok(to) => {
                let side = r.side;
                self.rename = None;
                self.reload(side);
                let p = &self.panes[side];
                if let Some(i) = p.visible(self.show_hidden, &self.sizes).iter().position(|e| e.path == to) {
                    self.panes[side].cursor = i;
                }
            }
            Err(e) => r.error = Some(e.to_string()),
        }
    }

    /// Opens the built-in editor for the file under the cursor.
    fn edit_current(&mut self) -> Task<Message> {
        if self.read_only() {
            return Task::none();
        }
        let Some(e) = self.current() else { return Task::none() };
        if e.is_dir() {
            return Task::none();
        }
        let path = e.path.clone();
        self.dialog = Some(Dialog::Edit { path: path.clone(), content: None, dirty: false, closing: false, error: None });
        Task::perform(
            async move {
                let p = path.clone();
                (path, smol::unblock(move || load_edit(&p)).await)
            },
            |(p, r)| Message::EditLoaded(p, r),
        )
    }

    /// Shift+F4: the configured external editor.
    fn edit_external(&mut self) {
        if self.read_only() {
            return;
        }
        let Some(e) = self.current() else { return };
        if e.is_dir() {
            return;
        }
        if let Err(err) = guppy_core::platform::edit(&e.path, &self.config.edit.command) {
            self.dialog = Some(Dialog::Report { title: "Editor not started", problems: vec![err.to_string()] });
        }
    }

    /// Opens the viewer for the entry under the cursor and loads it in the background.
    fn open_view(&mut self) -> Task<Message> {
        let Some(path) = self.current().map(|e| e.path.clone()) else { return Task::none() };
        self.dialog = Some(Dialog::View { path: path.clone(), body: ViewBody::Loading });
        Task::batch([
            operation::scroll_to(view_id(), AbsoluteOffset { x: None, y: Some(0.0) }),
            Task::perform(
                async move {
                    let p = path.clone();
                    (path, smol::unblock(move || preview::load(&p, false)).await)
                },
                |(p, b)| Message::Viewed(p, b),
            ),
        ])
    }

    fn on_view_key(&mut self, key: Key) -> Task<Message> {
        let line = 18.0;
        let page = self.panes[self.active].view_h.max(200.0);
        let by = |dy: f32| operation::scroll_by(view_id(), AbsoluteOffset { x: 0.0, y: dy });
        match key.as_ref() {
            Key::Named(Named::Escape | Named::F3 | Named::Enter) => self.dialog = None,
            Key::Named(Named::ArrowDown) => return by(3.0 * line),
            Key::Named(Named::ArrowUp) => return by(-3.0 * line),
            Key::Named(Named::PageDown | Named::Space) => return by(page),
            Key::Named(Named::PageUp) => return by(-page),
            Key::Named(Named::Home) => return operation::scroll_to(view_id(), AbsoluteOffset { x: None, y: Some(0.0) }),
            Key::Named(Named::End) => return operation::scroll_to(view_id(), AbsoluteOffset { x: None, y: Some(f32::MAX) }),
            // Previous / next entry of the pane, like flipping through a folder.
            Key::Named(dir @ (Named::ArrowLeft | Named::ArrowRight)) => {
                let len = self.panes[self.active].visible(self.show_hidden, &self.sizes).len();
                let p = &mut self.panes[self.active];
                p.cursor = if dir == Named::ArrowRight { (p.cursor + 1).min(len.saturating_sub(1)) } else { p.cursor.saturating_sub(1) };
                let scroll = self.scroll_into_view();
                return Task::batch([self.open_view(), scroll]);
            }
            Key::Named(Named::F4) => return self.edit_current(),
            _ => {}
        }
        Task::none()
    }

    /// Queues an operation; it starts right away if nothing else runs.
    fn enqueue(&mut self, kind: OpKind, sources: Vec<PathBuf>, target: Option<PathBuf>) {
        let what = if sources.len() == 1 {
            sources[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        } else {
            format!("{} items", sources.len())
        };
        let label = match &target {
            Some(t) => format!("{} {what} to {}", capitalize(op_verb(kind)), self.jumper.display(t)),
            None => format!("{} {what}", capitalize(op_verb(kind))),
        };
        self.jobs.queue.push_back((kind, sources, target, label));
        self.start_next_job();
    }

    fn start_next_job(&mut self) {
        if self.jobs.running.is_some() {
            return;
        }
        if let Some((kind, sources, target, label)) = self.jobs.queue.pop_front() {
            self.jobs.started += 1;
            self.jobs.running = Some((kind, ops::start(kind, sources, target), label));
        }
    }

    /// On every tick: shows conflicts, collects finished jobs, starts the next one.
    fn poll_jobs(&mut self) {
        if let Some((_, job, _)) = &self.jobs.running {
            let progress = job.progress();
            match job.conflict() {
                Some(c) if self.dialog.is_none() || matches!(self.dialog, Some(Dialog::Jobs)) => {
                    self.dialog = Some(Dialog::Conflict { conflict: c, all: false });
                }
                None if matches!(self.dialog, Some(Dialog::Conflict { .. })) => self.dialog = None,
                _ => {}
            }
            if progress.finished {
                self.jobs.running = None;
                self.jobs.problems.extend(progress.problems);
                for side in 0..2 {
                    let path = self.panes[side].path.clone();
                    self.sizes.invalidate(&path);
                    self.reload(side);
                }
                self.start_next_job();
            }
        }
        if !self.jobs.busy() {
            self.jobs.started = 0;
            self.close_armed = false;
            if matches!(self.dialog, Some(Dialog::Jobs)) {
                self.dialog = None;
            }
            if !self.jobs.problems.is_empty() && self.dialog.is_none() {
                self.dialog = Some(Dialog::Report { title: "Done, with notes", problems: std::mem::take(&mut self.jobs.problems) });
            }
        }
    }

    fn cycle_sort(&mut self) {
        let p = &mut self.panes[self.active];
        p.sort = p.sort.next();
    }

    fn recompute_sizes(&mut self) {
        let path = self.panes[self.active].path.clone();
        self.sizes.invalidate(&path);
        self.request_sizes(self.active);
        self.reload(self.active);
    }

    fn on_palette_key(&mut self, key: Key, typed: Option<&str>) -> Task<Message> {
        let Some(Dialog::Palette { query, cur }) = &mut self.dialog else { return Task::none() };
        let len = palette_hits(query).len();
        match key.as_ref() {
            Key::Named(Named::Escape) => self.dialog = None,
            Key::Named(Named::ArrowDown) => *cur = (*cur + 1).min(len.saturating_sub(1)),
            Key::Named(Named::ArrowUp) => *cur = cur.saturating_sub(1),
            Key::Named(Named::Enter) => {
                let hit = palette_hits(query).get(*cur).copied();
                self.dialog = None;
                if let Some(i) = hit {
                    return self.run_cmd(COMMANDS[i].2);
                }
            }
            Key::Named(Named::Backspace) => {
                query.pop();
                *cur = 0;
            }
            _ => {
                if let Some(t) = typed.filter(|t| t.chars().all(|c| !c.is_control())) {
                    query.push_str(t);
                    *cur = 0;
                }
            }
        }
        Task::none()
    }

    /// Runs a palette command as if its key had been pressed.
    fn run_cmd(&mut self, cmd: Cmd) -> Task<Message> {
        match cmd {
            Cmd::Copy => self.ask(OpKind::Copy),
            Cmd::Move => self.ask(OpKind::Move),
            Cmd::Trash => self.ask(OpKind::Trash),
            Cmd::Delete => self.ask(OpKind::Delete),
            Cmd::NewDir => self.new_dir(),
            Cmd::Rename => {
                if !self.read_only() {
                    self.start_rename();
                }
            }
            Cmd::View => return self.open_view(),
            Cmd::Edit => return self.edit_current(),
            Cmd::EditExternal => self.edit_external(),
            Cmd::Quick => return self.update(Message::ToggleQuick),
            Cmd::Mark | Cmd::Unmark => self.dialog = Some(Dialog::Mark { add: cmd == Cmd::Mark, pattern: self.last_pattern.clone() }),
            Cmd::Invert => self.invert_marks(),
            Cmd::Hidden => return self.update(Message::ToggleHidden),
            Cmd::Sort => self.cycle_sort(),
            Cmd::Sizes => self.recompute_sizes(),
            Cmd::Jump => self.set_query("~".into()),
            Cmd::Back => self.history(true),
            Cmd::Forward => self.history(false),
            Cmd::Parent => self.go_parent(),
            Cmd::Home => self.navigate(dirs_home()),
            Cmd::Switch => self.active = 1 - self.active,
            Cmd::DrivesLeft => self.open_drives(0),
            Cmd::DrivesRight => self.open_drives(1),
            Cmd::Jobs => self.dialog = Some(Dialog::Jobs),
            Cmd::Look => {
                let mut new = self.config.clone();
                new.view.look = if new.view.look == Look::Paper { Look::Neon } else { Look::Paper };
                self.set_config(new);
            }
            Cmd::Settings => self.dialog = Some(Dialog::Settings { row: 0, edit: None }),
            Cmd::Help => self.dialog = Some(Dialog::Help { startup: false }),
        }
        self.scroll_into_view()
    }

    /// Opens the drive picker for `side`, with the drive holding its folder selected.
    fn open_drives(&mut self, side: usize) {
        let list = guppy_core::drives::list();
        let here = &self.panes[side].path;
        let cur = list
            .iter()
            .enumerate()
            .filter(|(_, d)| here.starts_with(&d.path))
            .max_by_key(|(_, d)| d.path.as_os_str().len())
            .map_or(0, |(i, _)| i);
        self.dialog = Some(Dialog::Drives { side, list, cur });
    }

    fn on_drives_key(&mut self, key: Key, typed: Option<&str>) -> Task<Message> {
        let Some(Dialog::Drives { list, cur, .. }) = &mut self.dialog else { return Task::none() };
        match key.as_ref() {
            Key::Named(Named::Escape) => self.dialog = None,
            Key::Named(Named::ArrowDown) => *cur = (*cur + 1).min(list.len().saturating_sub(1)),
            Key::Named(Named::ArrowUp) => *cur = cur.saturating_sub(1),
            Key::Named(Named::Home) => *cur = 0,
            Key::Named(Named::End) => *cur = list.len().saturating_sub(1),
            Key::Named(Named::Enter) => self.pick_drive(),
            // 1–9 pick directly.
            _ => {
                if let Some(n) = typed.and_then(|t| t.parse::<usize>().ok()).filter(|n| (1..=list.len()).contains(n)) {
                    *cur = n - 1;
                    self.pick_drive();
                }
            }
        }
        self.scroll_into_view()
    }

    fn pick_drive(&mut self) {
        let Some(Dialog::Drives { side, list, cur }) = self.dialog.take() else { return };
        if let Some(d) = list.get(cur) {
            self.active = side;
            self.navigate(d.path.clone());
        }
    }

    /// Inside an archive nothing can change; says so and returns true.
    fn read_only(&mut self) -> bool {
        if !self.panes[self.active].archive {
            return false;
        }
        self.dialog = Some(Dialog::Report {
            title: "Archive is read-only",
            problems: vec!["Copy (F5) extracts files to the other pane; Enter opens a file.".into()],
        });
        true
    }

    fn new_dir(&mut self) {
        if !self.read_only() {
            self.dialog = Some(Dialog::NewDir { name: String::new(), error: None });
        }
    }

    /// `+` / `-`: marks or unmarks visible files matching any of the patterns
    /// (separated by spaces, commas or semicolons); a pattern ending in `/` matches folders.
    fn mark_pattern(&mut self, add: bool, pattern: &str) {
        let pats: Vec<&str> = pattern.split([' ', ',', ';']).filter(|s| !s.is_empty()).collect();
        let p = &self.panes[self.active];
        let hits: Vec<PathBuf> = p
            .visible(self.show_hidden, &self.sizes)
            .iter()
            .filter(|e| {
                pats.iter().any(|pat| match pat.strip_suffix('/') {
                    Some(dir) => e.is_dir() && glob_match(dir, &e.name),
                    None => !e.is_dir() && glob_match(pat, &e.name),
                })
            })
            .map(|e| e.path.clone())
            .collect();
        let p = &mut self.panes[self.active];
        for h in hits {
            if add {
                p.marked.insert(h);
            } else {
                p.marked.remove(&h);
            }
        }
    }

    /// `*`: inverts the marks of the visible files.
    fn invert_marks(&mut self) {
        let p = &self.panes[self.active];
        let files: Vec<PathBuf> = p.visible(self.show_hidden, &self.sizes).iter().filter(|e| !e.is_dir()).map(|e| e.path.clone()).collect();
        let p = &mut self.panes[self.active];
        for f in files {
            if !p.marked.remove(&f) {
                p.marked.insert(f);
            }
        }
    }

    fn on_fs_changed(&mut self, paths: &[PathBuf]) {
        let theme_dir = guppy_core::Theme::omarchy_path().and_then(|p| p.parent().map(Path::to_path_buf));
        if theme_dir.is_some_and(|t| paths.iter().any(|p| p.starts_with(&t) || t.starts_with(p))) {
            self.theme = load_theme(self.config.view.look);
        }
        if let Some(cfg) = Config::path()
            && paths.contains(&cfg)
        {
            self.apply_config();
        }
        for side in 0..2 {
            let dir = self.panes[side].path.clone();
            // Events on the folder itself (e.g. timestamps) don't change sizes.
            let touched: Vec<&PathBuf> = paths.iter().filter(|p| p.parent() == Some(dir.as_path())).collect();
            if touched.is_empty() {
                continue;
            }
            for p in touched {
                self.sizes.invalidate(p);
            }
            self.reload(side);
        }
    }

    /// Re-reads the folder and keeps the cursor on its entry.
    fn reload(&mut self, side: usize) {
        let path = self.panes[side].path.clone();
        self.load(side, path, After::Reload);
    }

    /// Reads a folder in the background. If it arrives quickly (almost always locally),
    /// it is shown immediately; otherwise the UI stays usable until it arrives.
    fn load(&mut self, side: usize, path: PathBuf, after: After) {
        // A reload never replaces a pending folder change.
        if matches!(after, After::Reload) && self.loads[side].is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let p = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(guppy_core::archive::read_path(&p));
        });
        match rx.recv_timeout(LOAD_WAIT) {
            Ok(res) => {
                self.loads[side] = None;
                self.finish_load(side, path, res, after);
            }
            Err(_) => self.loads[side] = Some(Loading { path, rx, since: std::time::Instant::now(), after }),
        }
    }

    fn poll_loads(&mut self) {
        for side in 0..2 {
            let Some(l) = &self.loads[side] else { continue };
            if let Ok(res) = l.rx.try_recv() {
                let l = self.loads[side].take().unwrap();
                self.finish_load(side, l.path, res, l.after);
            }
        }
    }

    fn finish_load(&mut self, side: usize, path: PathBuf, res: std::io::Result<Vec<Entry>>, after: After) {
        let entries = match res {
            Ok(entries) => entries,
            Err(e) => {
                // Unreadable (permissions, NAS gone): view stays as it was.
                if !matches!(after, After::Reload | After::Start) {
                    self.dialog = Some(Dialog::Report { title: "Folder not readable", problems: vec![format!("{}: {e}", self.jumper.display(&path))] });
                }
                return;
            }
        };
        if let After::Reload = after {
            let keep = self.panes[side].visible(self.show_hidden, &self.sizes).get(self.panes[side].cursor).map(|e| e.path.clone());
            let p = &mut self.panes[side];
            if p.path != path {
                return;
            }
            p.entries = entries;
            let names: HashSet<&PathBuf> = p.entries.iter().map(|e| &e.path).collect();
            p.marked.retain(|m| names.contains(m));
            let visible = p.visible(self.show_hidden, &self.sizes);
            let pos = keep.and_then(|k| visible.iter().position(|e| e.path == k));
            let len = visible.len();
            p.cursor = pos.unwrap_or(p.cursor).min(len.saturating_sub(1));
            // Let running computations continue, request only what's missing.
            if !self.config.view.dir_sizes || self.panes[side].archive {
                return;
            }
            let p = &self.panes[side];
            let dirs = p.entries.iter().filter(|e| e.is_dir()).map(|e| e.path.clone()).collect();
            self.sizes.request(dirs, p.sizing.clone());
            return;
        }
        let p = &mut self.panes[side];
        let old = std::mem::take(&mut p.path);
        p.enter(path.clone(), entries);
        let changed = old != path && !old.as_os_str().is_empty();
        match &after {
            After::Nav | After::Parent(_) if changed => {
                p.back.push(old);
                p.forward.clear();
            }
            After::Back => p.forward.push(old),
            After::Forward => p.back.push(old),
            _ => {}
        }
        if let After::Parent(came_from) = &after {
            let pos = self.panes[side].visible(self.show_hidden, &self.sizes).iter().position(|e| e.path == *came_from);
            self.panes[side].cursor = pos.unwrap_or(0);
        }
        if matches!(after, After::Nav | After::Parent(_)) && !self.panes[side].archive {
            self.jumper.visit(&path);
        }
        self.request_sizes(side);
    }

    fn set_query(&mut self, q: String) {
        let entering_jump = is_jump(&q) && !self.jumping();
        let p = &mut self.panes[self.active];
        p.query = q;
        p.cursor = 0;
        if entering_jump {
            self.jumper.index_nearby(&self.panes[self.active].path);
            self.jump_pending = true;
        }
        if self.jumping() {
            self.update_jump();
        }
    }

    fn update_jump(&mut self) {
        let q = &self.panes[self.active].query;
        let pattern = &q[1..];
        let mut hits = self.jumper.search(pattern, JUMP_LIMIT);
        // A typed path that exists goes to the top.
        let typed = match q.strip_prefix('~') {
            Some(rest) => dirs_home().join(rest.trim_start_matches('/')),
            None => PathBuf::from(q),
        };
        if q.len() > 1 && typed.is_dir() {
            hits.retain(|h| *h != typed);
            hits.insert(0, typed);
            hits.truncate(JUMP_LIMIT);
        }
        self.jump = hits;
        self.jump_cur = 0;
    }

    fn jump_selected(&mut self) {
        if let Some(target) = self.jump.get(self.jump_cur).cloned() {
            self.navigate(target);
        }
        self.jump.clear();
    }

    fn request_sizes(&mut self, side: usize) {
        let p = &mut self.panes[side];
        p.sizing.cancel();
        p.sizing = Cancel::default();
        if !self.config.view.dir_sizes || p.archive {
            return;
        }
        let dirs = p.entries.iter().filter(|e| e.is_dir()).map(|e| e.path.clone()).collect();
        self.sizes.request(dirs, p.sizing.clone());
    }

    fn navigate(&mut self, path: PathBuf) {
        if path != self.panes[self.active].path {
            self.load(self.active, path, After::Nav);
        }
    }

    fn history(&mut self, back: bool) {
        let p = &mut self.panes[self.active];
        let target = if back { p.back.pop() } else { p.forward.pop() };
        if let Some(t) = target {
            self.load(self.active, t, if back { After::Back } else { After::Forward });
        }
    }

    fn go_parent(&mut self) {
        let p = &self.panes[self.active];
        let Some(parent) = p.path.parent().map(Path::to_path_buf) else { return };
        let came_from = p.path.clone();
        self.load(self.active, parent, After::Parent(came_from));
    }

    /// Enter: folders and archives are entered, files open in their application;
    /// a file inside an archive is extracted to a temporary folder first.
    fn open_current(&mut self) -> Task<Message> {
        let Some(e) = self.current() else { return Task::none() };
        let path = e.path.clone();
        if e.is_dir() || (!e.in_archive && guppy_core::archive::is_archive_name(&e.name)) {
            self.navigate(path);
        } else if e.in_archive {
            return Task::perform(smol::unblock(move || open_from_archive(&path)), Message::Opened);
        } else {
            let _ = guppy_core::platform::open(&path);
        }
        Task::none()
    }

    fn toggle_mark(&mut self) {
        let Some(path) = self.current().map(|e| e.path.clone()) else { return };
        let p = &mut self.panes[self.active];
        if !p.marked.remove(&path) {
            p.marked.insert(path);
        }
        p.cursor += 1;
        let len = p.visible(self.show_hidden, &self.sizes).len();
        p.cursor = p.cursor.min(len.saturating_sub(1));
    }

    /// Keeps the cursor visible with two rows of margin; scrolls smoothly.
    fn scroll_into_view(&mut self) -> Task<Message> {
        if self.jumping() {
            return Task::none();
        }
        const MARGIN: f32 = 2.0 * ROW_H;
        let now = std::time::Instant::now();
        let p = &mut self.panes[self.active];
        // Compute against the running target, else key repeat makes it jump back.
        let base = p.scroll_target.unwrap_or(p.scroll_y);
        let top = p.cursor as f32 * ROW_H;
        let margin = MARGIN.min(((p.view_h - ROW_H) / 2.0).max(0.0));
        // Round targets to whole rows so rows don't jump a few pixels
        // (window height modulo row height) when scrolling starts.
        let y = if top - margin < base {
            ((top - margin) / ROW_H).floor().max(0.0) * ROW_H
        } else if top + ROW_H + margin > base + p.view_h {
            ((top + ROW_H + margin - p.view_h) / ROW_H).ceil() * ROW_H
        } else {
            self.last_move = Some(now);
            return Task::none();
        };
        // Key repeat: the list moves in step with the keys, the cursor stays put.
        // Animating here causes sawtooth jitter (cursor jumps, list catches up).
        let repeating = self.last_move.is_some_and(|t| now.duration_since(t) < Duration::from_millis(90));
        self.last_move = Some(now);
        if repeating {
            p.scroll_target = None;
            p.scroll_y = y;
            return operation::scroll_to(p.id.clone(), AbsoluteOffset { x: None, y: Some(y) });
        }
        p.scroll_target = Some(y);
        self.last_frame = None;
        Task::none()
    }

    /// One animation step: approaches the target exponentially, independent of frame rate.
    fn animate_scroll(&mut self, now: std::time::Instant) -> Task<Message> {
        const TAU: f32 = 0.03; // seconds; smaller = faster
        let dt = self.last_frame.map_or(1.0 / 60.0, |t| now.duration_since(t).as_secs_f32()).min(0.1);
        self.last_frame = Some(now);
        let k = 1.0 - (-dt / TAU).exp();
        let mut tasks = Vec::new();
        for p in &mut self.panes {
            let Some(target) = p.scroll_target else { continue };
            let mut y = p.scroll_y + (target - p.scroll_y) * k;
            // Never lag behind the cursor: its row stays fully visible.
            let top = p.cursor as f32 * ROW_H;
            y = y.clamp((top + ROW_H - p.view_h).min(top), top);
            if (target - y).abs() < 0.5 {
                y = target;
                p.scroll_target = None;
            }
            p.scroll_y = y;
            tasks.push(operation::scroll_to(p.id.clone(), AbsoluteOffset { x: None, y: Some(y) }));
        }
        Task::batch(tasks)
    }

    // ---------- View ----------

    fn view(&self) -> Element<'_, Message> {
        let t = &self.theme;
        let side_view = |side: usize| if self.quick && side != self.active { self.quick_pane(side) } else { self.pane_view(side) };
        let body = row![side_view(0), side_view(1)].spacing(22);
        let desk = container(body)
            .padding([16, 18])
            .height(Length::Fill)
            .style(move |_| container::Style {
                background: Some(rgb(t.dark_background).into()),
                ..Default::default()
            });
        let main = column![self.menubar(), desk, self.statusbar()];
        match self.dialog_view() {
            Some(d) => iced::widget::stack![main, iced::widget::opaque(d)].into(),
            None => main.into(),
        }
    }

    fn dialog_view(&self) -> Option<Element<'_, Message>> {
        let t = &self.theme;
        let dialog = self.dialog.as_ref()?;
        let fg = rgb(t.foreground);
        let title = |s: String| text(s).size(15).font(BOLD).color(rgb(t.bright));
        let small = |s: String| text(s).size(12).color(fg);
        let btn = |label: &'static str, msg: Message, primary: bool| {
            button(text(label).size(13))
                .padding([8, 16])
                .on_press(msg)
                .style(move |_, status| button::Style {
                    background: Some(if primary { rgb(t.accent) } else if matches!(status, button::Status::Hovered) {
                        Color { a: 0.08, ..Color::WHITE }
                    } else {
                        Color::TRANSPARENT
                    }.into()),
                    text_color: if primary { rgb(t.background) } else { fg },
                    border: Border { color: if primary { rgb(t.accent) } else { Color { a: 0.2, ..Color::WHITE } }, width: 1.0, radius: 6.0.into() },
                    ..Default::default()
                })
        };
        let buttons = |ok: &'static str| {
            row![iced::widget::Space::new().width(Length::Fill), btn("Cancel", Message::DialogCancel, false), btn(ok, Message::DialogConfirm, true)]
                .spacing(8)
        };
        let progress_bar = |frac: f32| {
            iced::widget::progress_bar(0.0..=1.0, frac).girth(6).style(move |_| iced::widget::progress_bar::Style {
                background: Color { a: 0.25, ..Color::BLACK }.into(),
                bar: rgb(t.accent).into(),
                border: Border::default().rounded(3),
            })
        };
        let names = |paths: &[PathBuf]| {
            let mut lines: Vec<String> = paths.iter().take(6).map(|p| {
                let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if p.is_dir() { format!("· {n}/") } else { format!("· {n}") }
            }).collect();
            if paths.len() > 6 {
                lines.push(format!("  and {} more", paths.len() - 6));
            }
            small(lines.join("\n"))
        };

        let content: Element<'_, Message> = match dialog {
            Dialog::Confirm { kind, sources, target } => {
                let what = if sources.len() == 1 { "1 item".to_string() } else { format!("{} items", sources.len()) };
                let mut c = column![title(format!("{} {what}", capitalize(op_verb(*kind))))].spacing(14);
                if let Some(target) = target {
                    c = c.push(small(format!("to {}", self.jumper.display(target))));
                }
                c = c.push(names(sources));
                if *kind == OpKind::Delete {
                    c = c.push(text("This cannot be undone.").size(12).color(rgb(t.red)));
                }
                let ok = match kind {
                    OpKind::Copy => "Copy",
                    OpKind::Move => "Move",
                    OpKind::Trash => "Trash",
                    OpKind::Delete => "Delete",
                };
                c.push(buttons(ok)).into()
            }
            Dialog::View { path, body } => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
                column![
                    row![title(name), iced::widget::Space::new().width(Length::Fill), small(file_info(path))].align_y(iced::Alignment::Center).spacing(12),
                    self.body_view(path, body, view_id()),
                    small("↑↓ PgUp PgDn scroll · ←→ previous / next · F4 edit · Esc closes".into()),
                ]
                .spacing(14)
                .into()
            }
            Dialog::Edit { path, content, dirty, closing, error } => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let body: Element<'_, Message> = match content {
                    None => small("loading …".into()).into(),
                    Some(c) => text_editor(c)
                        .id(editor_id())
                        .on_action(Message::EditAction)
                        .key_binding(|k| {
                            let ctrl_s = k.modifiers.command() && matches!(k.key.as_ref(), Key::Character("s"));
                            match k.key.as_ref() {
                                Key::Named(Named::Escape) => Some(text_editor::Binding::Custom(Message::EditClose)),
                                _ if ctrl_s => Some(text_editor::Binding::Custom(Message::EditSave)),
                                Key::Named(Named::Tab) if matches!(k.status, text_editor::Status::Focused { .. }) => {
                                    Some(text_editor::Binding::Sequence(vec![text_editor::Binding::Insert(' '); 4]))
                                }
                                _ => text_editor::Binding::from_key_press(k),
                            }
                        })
                        .font(FONT)
                        .size(13)
                        .height(Length::Fill)
                        .padding(10)
                        .style(move |_, _| text_editor::Style {
                            background: Color { a: 0.25, ..Color::BLACK }.into(),
                            border: Border::default().rounded(4),
                            placeholder: rgb(t.foreground),
                            value: rgb(t.bright),
                            selection: Color { a: 0.35, ..rgb(t.accent) },
                        })
                        .into(),
                };
                let (status, status_color) = match (error, closing, dirty) {
                    (Some(e), _, _) => (e.clone(), rgb(t.red)),
                    (None, true, _) => ("Unsaved changes · Esc again discards · Ctrl+S saves".to_string(), rgb(t.red)),
                    (None, false, true) => ("modified".to_string(), rgb(t.accent)),
                    (None, false, false) => (String::new(), fg),
                };
                let pos = content.as_ref().map(|c| {
                    let p = c.cursor().position;
                    format!("{}:{}", p.line + 1, p.column + 1)
                }).unwrap_or_default();
                column![
                    row![title(name), iced::widget::Space::new().width(Length::Fill), text(status).size(12).color(status_color), small(pos)]
                        .align_y(iced::Alignment::Center)
                        .spacing(12),
                    body,
                    small("Ctrl+S saves · Esc closes".into()),
                ]
                .spacing(14)
                .into()
            }
            Dialog::NewDir { name, error } => {
                let field = container(text(format!("{name}▏")).size(13).color(rgb(t.bright)))
                    .padding([8, 10])
                    .width(Length::Fill)
                    .style(move |_| container::Style {
                        background: Some(Color { a: 0.25, ..Color::BLACK }.into()),
                        border: Border::default().rounded(4),
                        ..Default::default()
                    });
                let mut c = column![
                    title("New folder".into()),
                    small(format!("in {}", self.jumper.display(&self.panes[self.active].path))),
                    field,
                ]
                .spacing(14);
                if let Some(e) = error {
                    c = c.push(text(e.clone()).size(12).color(rgb(t.red)));
                }
                c.push(buttons("Create")).into()
            }
            Dialog::Jobs => {
                let mut c = column![title("Jobs".into())].spacing(14);
                match &self.jobs.running {
                    Some((kind, job, label)) => {
                        let p = job.progress();
                        let frac = if p.total == 0 { 0.0 } else { p.done as f32 / p.total as f32 };
                        let amount = if matches!(kind, OpKind::Copy | OpKind::Move) {
                            format!("{} of {}", human_size(p.done), human_size(p.total))
                        } else {
                            format!("{} of {}", p.done, p.total)
                        };
                        c = c.push(text(label.clone()).size(13).color(rgb(t.bright)));
                        c = c.push(progress_bar(frac));
                        c = c.push(row![small(p.current), iced::widget::Space::new().width(Length::Fill), small(amount)]);
                    }
                    None => c = c.push(small("Nothing running.".into())),
                }
                if !self.jobs.queue.is_empty() {
                    let waiting: Vec<String> = self.jobs.queue.iter().map(|(.., label)| format!("· {label}")).collect();
                    c = c.push(column![text("WAITING").size(11).font(BOLD).color(rgb(t.accent)), small(waiting.join("\n"))].spacing(6));
                }
                if self.close_armed {
                    c = c.push(text("Jobs are still running. Close the window again to quit and cancel them.").size(12).color(rgb(t.red)));
                }
                let mut buttons = row![small("Del cancels · Esc closes".into()), iced::widget::Space::new().width(Length::Fill)].spacing(8).align_y(iced::Alignment::Center);
                if !self.jobs.queue.is_empty() {
                    buttons = buttons.push(btn("Cancel all", Message::CancelJobs(true), false));
                }
                if self.jobs.running.is_some() {
                    buttons = buttons.push(btn("Cancel job", Message::CancelJobs(false), false));
                }
                c.push(buttons.push(btn("Close", Message::DialogCancel, true))).into()
            }
            Dialog::Conflict { conflict, all } => {
                let describe = |p: &Path| match std::fs::symlink_metadata(p) {
                    Ok(m) if m.is_dir() => "folder".to_string(),
                    Ok(m) => format!("{} · {}", human_size(m.len()), m.modified().ok().map(human_date).unwrap_or_default()),
                    Err(_) => String::new(),
                };
                let name = conflict.dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let dir = conflict.dst.parent().map(|d| self.jumper.display(d)).unwrap_or_default();
                let line = |label: &'static str, what: String| row![text(label).size(12).color(fg).width(80), text(what).size(12).color(rgb(t.bright))];
                let on = *all;
                let tick = container(text(if on { "✓" } else { "" }).size(11).color(rgb(t.background)))
                    .center_x(15)
                    .center_y(15)
                    .style(move |_| container::Style {
                        background: on.then(|| rgb(t.accent).into()),
                        border: Border { color: rgb(t.accent), width: 1.0, radius: 3.0.into() },
                        ..Default::default()
                    });
                let toggle = mouse_area(row![tick, small("Same answer for the rest · A".into())].spacing(8).align_y(iced::Alignment::Center))
                    .on_press(Message::ToggleAll)
                    .interaction(iced::mouse::Interaction::Pointer);
                column![
                    title(format!("“{name}” already exists")),
                    small(format!("in {dir}")),
                    column![line("New", describe(&conflict.src)), line("Existing", describe(&conflict.dst))].spacing(4),
                    toggle,
                    row![
                        small("Esc cancels the job".into()),
                        iced::widget::Space::new().width(Length::Fill),
                        btn("Overwrite · O", Message::Resolve(Resolve::Overwrite), false),
                        btn("Keep both · K", Message::Resolve(Resolve::KeepBoth), false),
                        btn("Skip · S", Message::Resolve(Resolve::Skip), true),
                    ]
                    .spacing(8)
                    .align_y(iced::Alignment::Center),
                ]
                .spacing(14)
                .into()
            }
            Dialog::Palette { query, cur } => {
                let hits = palette_hits(query);
                let field = container(text(if query.is_empty() { "type a command …".to_string() } else { format!("{query}▏") }).size(13).color(if query.is_empty() { fg } else { rgb(t.bright) }))
                    .padding([8, 10])
                    .width(Length::Fill)
                    .style(move |_| container::Style {
                        background: Some(Color { a: 0.25, ..Color::BLACK }.into()),
                        border: Border::default().rounded(4),
                        ..Default::default()
                    });
                // A window of rows that keeps the selection visible.
                const SHOWN: usize = 12;
                let first = cur.saturating_sub(SHOWN - 1);
                let mut list = column![].spacing(2);
                for (n, &i) in hits.iter().enumerate().skip(first).take(SHOWN) {
                    let (label, keys, _) = COMMANDS[i];
                    let on = n == *cur;
                    let color = if on { rgb(t.background) } else { rgb(t.bright) };
                    list = list.push(
                        button(row![text(label).size(13).color(color).width(Length::Fill), text(keys).size(12).color(if on { color } else { fg })].spacing(12))
                            .padding([5, 10])
                            .width(Length::Fill)
                            .on_press(Message::PaletteRun(i))
                            .style(move |_, status| button::Style {
                                background: Some(if on {
                                    rgb(t.accent)
                                } else if matches!(status, button::Status::Hovered) {
                                    Color { a: 0.06, ..Color::WHITE }
                                } else {
                                    Color::TRANSPARENT
                                }
                                .into()),
                                border: Border::default().rounded(if self.neon() { 0 } else { 4 }),
                                ..Default::default()
                            }),
                    );
                }
                if hits.is_empty() {
                    list = list.push(small("No matching command".into()));
                }
                column![field, list, small("↑↓ choose · Enter runs · Esc closes".into())].spacing(12).into()
            }
            Dialog::Drives { side, list, cur } => {
                let mut rows = column![].spacing(4);
                for (i, d) in list.iter().enumerate() {
                    let on = i == *cur;
                    let main = if on { rgb(t.background) } else { rgb(t.bright) };
                    let dim = if on { rgb(t.background) } else { fg };
                    let (frac, space) = match d.space {
                        Some((total, free)) if total > 0 => (
                            (total - free) as f32 / total as f32,
                            format!("{} free of {}", human_size(free), human_size(total)),
                        ),
                        _ => (0.0, if d.network { "network".to_string() } else { String::new() }),
                    };
                    let bar = iced::widget::progress_bar(0.0..=1.0, frac).girth(4).length(120).style(move |_| iced::widget::progress_bar::Style {
                        background: Color { a: 0.25, ..Color::BLACK }.into(),
                        bar: if on { rgb(t.background) } else if frac > 0.9 { rgb(t.red) } else { rgb(t.accent) }.into(),
                        border: Border::default().rounded(2),
                    });
                    let info = column![
                        row![
                            text(format!("{}  {}", i + 1, d.label)).size(13).font(BOLD).color(main),
                            text(d.fs.clone()).size(11).color(dim),
                        ]
                        .spacing(10)
                        .align_y(iced::Alignment::Center),
                        text(self.jumper.display(&d.path)).size(11).color(dim).wrapping(iced::widget::text::Wrapping::None),
                    ]
                    .spacing(2)
                    .width(Length::Fill);
                    rows = rows.push(
                        button(
                            row![info, column![bar, text(space).size(11).color(dim)].spacing(4).align_x(iced::Alignment::End)]
                                .spacing(12)
                                .align_y(iced::Alignment::Center),
                        )
                        .padding([6, 10])
                        .width(Length::Fill)
                        .on_press(Message::DrivePick(i))
                        .style(move |_, status| button::Style {
                            background: Some(if on {
                                rgb(t.accent)
                            } else if matches!(status, button::Status::Hovered) {
                                Color { a: 0.06, ..Color::WHITE }
                            } else {
                                Color::TRANSPARENT
                            }
                            .into()),
                            border: Border::default().rounded(if self.neon() { 0 } else { 4 }),
                            ..Default::default()
                        }),
                    );
                }
                column![
                    title(format!("Drives · {} pane", if *side == 0 { "left" } else { "right" })),
                    scrollable(rows).height(Length::Shrink),
                    small("↑↓ choose · Enter or 1–9 opens · Esc closes".into()),
                ]
                .spacing(14)
                .into()
            }
            Dialog::Mark { add, pattern } => {
                let field = container(text(format!("{pattern}▏")).size(13).color(rgb(t.bright)))
                    .padding([8, 10])
                    .width(Length::Fill)
                    .style(move |_| container::Style {
                        background: Some(Color { a: 0.25, ..Color::BLACK }.into()),
                        border: Border::default().rounded(4),
                        ..Default::default()
                    });
                column![
                    title(if *add { "Mark files" } else { "Unmark files" }.into()),
                    small("Wildcards * and ?, several patterns separated by spaces. End a pattern with / for folders.".into()),
                    field,
                    buttons(if *add { "Mark" } else { "Unmark" }),
                ]
                .spacing(14)
                .into()
            }
            Dialog::Settings { row: sel, edit } => {
                let mut list = column![].spacing(2);
                let mut section = "";
                for (i, (head, label)) in SETTINGS.iter().enumerate() {
                    if *head != section {
                        section = head;
                        let gap = if i == 0 { 0 } else { 12 };
                        list = list.push(
                            container(text(*head).size(11).font(BOLD).color(rgb(t.accent))).padding(iced::Padding { top: gap as f32, bottom: 4.0, ..Default::default() }),
                        );
                    }
                    let selected = i == *sel;
                    let value = match edit {
                        Some(e) if selected => format!("{e}▏"),
                        _ => setting_value(&self.config, i),
                    };
                    let on = !matches!(value.as_str(), "off");
                    let value_color = if selected { rgb(t.background) } else if on { rgb(t.bright) } else { fg };
                    let label_color = if selected { rgb(t.background) } else { fg };
                    list = list.push(
                        button(
                            row![
                                text(*label).size(13).color(label_color).width(Length::Fill),
                                text(value).size(13).font(BOLD).color(value_color).wrapping(iced::widget::text::Wrapping::None),
                            ]
                            .spacing(12),
                        )
                        .padding([5, 10])
                        .width(Length::Fill)
                        .on_press(Message::SettingClick(i))
                        .style(move |_, status| button::Style {
                            background: Some(if selected {
                                rgb(t.accent)
                            } else if matches!(status, button::Status::Hovered) {
                                Color { a: 0.06, ..Color::WHITE }
                            } else {
                                Color::TRANSPARENT
                            }.into()),
                            border: Border::default().rounded(4),
                            ..Default::default()
                        }),
                    );
                }
                let hint = match edit {
                    Some(_) => "type a path · empty = startup folder · Enter saves · Esc cancels",
                    None if setting_is_text(*sel) => "Enter edits · . takes this pane's folder · Esc closes",
                    None => "↑↓ choose · Space/←→ change · Esc closes",
                };
                let file = Config::path().map(|p| self.jumper.display(&p)).unwrap_or_default();
                column![
                    title("Settings".into()),
                    list,
                    column![small(hint.into()), text(format!("Saved to {file}")).size(11).color(Color { a: 0.6, ..fg })].spacing(4),
                ]
                .spacing(16)
                .into()
            }
            Dialog::Help { startup } => {
                let section = |(head, keys): &(&'static str, &'static [(&'static str, &'static str)])| {
                    let mut c = column![text(*head).size(11).font(BOLD).color(rgb(t.accent))].spacing(5);
                    for (k, what) in keys.iter() {
                        c = c.push(row![
                            text(*k).size(12).font(BOLD).color(rgb(t.bright)).width(118),
                            text(*what).size(12).color(rgb(t.foreground)),
                        ]);
                    }
                    c
                };
                column![
                    title("Guppy · Keys".into()),
                    small("Everything works from the keyboard. Typing filters the active pane, ~ or / jumps to a folder. Without marks, file operations act on the entry under the cursor; Enter confirms, Esc cancels.".into()),
                    row![
                        column![section(&HELP[0]), section(&HELP[1])].spacing(18).width(Length::Fill),
                        column![section(&HELP[2]), section(&HELP[3])].spacing(18).width(Length::Fill),
                    ]
                    .spacing(24),
                    row![
                        if *startup {
                            // Flat checkbox: small bordered box, filled when on.
                            let on = self.config.start.help;
                            let tick = container(text(if on { "✓" } else { "" }).size(11).color(rgb(t.background)))
                                .center_x(15)
                                .center_y(15)
                                .style(move |_| container::Style {
                                    background: on.then(|| rgb(t.accent).into()),
                                    border: Border { color: rgb(t.accent), width: 1.0, radius: 3.0.into() },
                                    ..Default::default()
                                });
                            Element::from(
                                mouse_area(row![tick, small("Show at every start · Space".into())].spacing(8).align_y(iced::Alignment::Center))
                                    .on_press(Message::ToggleStartHelp)
                                    .interaction(iced::mouse::Interaction::Pointer),
                            )
                        } else {
                            small("F1 or Esc closes".into()).into()
                        },
                        iced::widget::Space::new().width(Length::Fill),
                        btn("OK", Message::DialogCancel, true)
                    ]
                        .align_y(iced::Alignment::Center),
                ]
                .spacing(18)
                .into()
            }
            Dialog::Report { title: heading, problems } => {
                let mut lines: Vec<String> = problems.iter().take(8).cloned().collect();
                if problems.len() > 8 {
                    lines.push(format!("and {} more", problems.len() - 8));
                }
                column![
                    title(heading.to_string()),
                    small(lines.join("\n")),
                    row![iced::widget::Space::new().width(Length::Fill), btn("OK", Message::DialogCancel, true)],
                ]
                .spacing(14)
                .into()
            }
        };

        let viewer = matches!(dialog, Dialog::View { .. } | Dialog::Edit { .. });
        let card = container(content)
            .padding(22)
            .width(match dialog {
                Dialog::View { .. } | Dialog::Edit { .. } => Length::Fill,
                Dialog::Help { .. } => Length::Fixed(760.0),
                Dialog::Settings { .. } | Dialog::Jobs | Dialog::Conflict { .. } | Dialog::Palette { .. } | Dialog::Drives { .. } => Length::Fixed(560.0),
                _ => Length::Fixed(480.0),
            })
            .height(if viewer { Length::Fill } else { Length::Shrink })
            .max_width(1400)
        .style(move |_| container::Style {
            background: Some(rgb(t.sheet).into()),
            border: Border { color: rgb(t.accent), width: 1.5, radius: self.radius().into() },
            ..Default::default()
        });
        Some(
            container(card)
                .center(Length::Fill)
                .padding(if viewer { 32 } else { 0 })
                .style(|_| container::Style {
                    background: Some(Color { a: 0.6, ..Color::BLACK }.into()),
                    ..Default::default()
                })
                .into(),
        )
    }

    /// The content of a viewer: pictures fit the area, text scrolls.
    fn body_view<'a>(&'a self, path: &'a Path, body: &'a ViewBody, id: Id) -> Element<'a, Message> {
        let t = &self.theme;
        let small = |s: String| text(s).size(12).color(rgb(t.foreground));
        let mono = |s: &str| text(s.to_string()).size(12).font(FONT).color(rgb(t.bright));
        let scroll = |c: Element<'a, Message>| scrollable(c).id(id.clone()).height(Length::Fill).width(Length::Fill).into();
        let fit = |c: Element<'a, Message>| container(c).center(Length::Fill).into();
        match body {
            ViewBody::Loading => small("loading …".into()).into(),
            ViewBody::Text { text: s, truncated } | ViewBody::Hex { text: s, truncated } => {
                let mut c = column![mono(s)];
                if *truncated {
                    c = c.push(small("… the rest is not shown · F3 shows more".into()));
                }
                scroll(c.into())
            }
            ViewBody::Dir(s) => scroll(mono(s).into()),
            ViewBody::Image => fit(iced::widget::image(iced::widget::image::Handle::from_path(path)).content_fit(iced::ContentFit::ScaleDown).into()),
            ViewBody::Svg => fit(svg(svg::Handle::from_path(path)).content_fit(iced::ContentFit::ScaleDown).into()),
            ViewBody::Picture { image, caption } => column![
                container(iced::widget::image(image.clone()).content_fit(iced::ContentFit::ScaleDown)).center(Length::Fill),
                small(caption.clone()),
            ]
            .spacing(10)
            .into(),
            ViewBody::Other(s) => fit(small(s.clone()).into()),
        }
    }

    /// Quick view: the other pane's sheet showing the entry under the cursor.
    fn quick_pane(&self, side: usize) -> Element<'_, Message> {
        let t = &self.theme;
        let (name, info) = match &self.quick_view {
            Some((p, _)) => (p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), file_info(p)),
            None => (String::new(), String::new()),
        };
        let header = container(
            row![
                text(if self.neon() { "» PREVIEW" } else { "PREVIEW" }).size(11).font(BOLD).color(rgb(t.accent)),
                container(text(name).size(12).color(rgb(t.bright)).wrapping(iced::widget::text::Wrapping::None)).width(Length::Fill).clip(true),
                text(info).size(11).color(rgb(t.foreground)),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        )
        .padding([10, 10]);
        let body: Element<'_, Message> = match &self.quick_view {
            Some((p, b)) => container(self.body_view(p, b, quick_id())).padding([4, 14]).height(Length::Fill).into(),
            None => container(text("Ctrl+Q closes the preview").size(12).color(rgb(t.foreground))).center(Length::Fill).into(),
        };
        let outline = self.neon().then(|| rgb(t.muted));
        self.sheet(column![header, body].into(), side, t.sheet_dim, outline).width(Length::FillPortion(3)).into()
    }

    fn icon_button(&self, data: &'static str, label: &'static str, msg: Option<Message>) -> Element<'_, Message> {
        let t = &self.theme;
        let enabled = msg.is_some();
        let icon = svg(svg::Handle::from_memory(data.as_bytes()))
            .width(18)
            .height(18)
            .style(move |_, status| svg::Style {
                color: Some(match (enabled, status) {
                    (false, _) => rgb(t.muted),
                    (true, svg::Status::Hovered) => rgb(t.bright),
                    _ => rgb(t.light_foreground),
                }),
            });
        let b = button(container(icon).center(34))
            .padding(0)
            .on_press_maybe(msg)
            .style(move |_, status| button::Style {
                background: matches!(status, button::Status::Hovered)
                    .then(|| rgb(t.lighter_background).into()),
                border: Border::default().rounded(6),
                ..Default::default()
            });
        iced::widget::tooltip(b, text(label).size(12), iced::widget::tooltip::Position::Bottom)
            .style(move |_| container::Style {
                background: Some(rgb(t.lighter_background).into()),
                text_color: Some(rgb(t.foreground)),
                border: Border::default().rounded(4),
                ..Default::default()
            })
            .padding(6)
            .into()
    }

    fn menubar(&self) -> Element<'_, Message> {
        let t = &self.theme;
        let sep = || {
            let line = container(iced::widget::Space::new().width(1).height(18)).style(move |_| container::Style {
                background: Some(rgb(t.selection).into()),
                ..Default::default()
            });
            container(line).padding([0, 6])
        };
        let p = &self.panes[self.active];
        let bar = row![
            text(if self.neon() { ">_" } else { "◆" }).color(rgb(t.accent)).font(BOLD).size(13),
            iced::widget::Space::new().width(10),
            self.icon_button(icons::BACK, "Back (Alt ←)", (!p.back.is_empty()).then_some(Message::Back)),
            self.icon_button(icons::FORWARD, "Forward (Alt →)", (!p.forward.is_empty()).then_some(Message::Forward)),
            self.icon_button(icons::UP, "Parent folder (⌫)", Some(Message::Parent)),
            self.icon_button(icons::HOME, "Home", Some(Message::Home)),
            sep(),
            self.icon_button(icons::NEW_DIR, "New folder (F7)", Some(Message::NewDir)),
            self.icon_button(icons::COPY, "Copy (F5)", Some(Message::Op(OpKind::Copy))),
            self.icon_button(icons::MOVE, "Move (F6)", Some(Message::Op(OpKind::Move))),
            self.icon_button(icons::DELETE, "Trash (F8 · Del), permanently with Shift", Some(Message::Op(OpKind::Trash))),
            sep(),
            self.icon_button(icons::MARK, "Mark (Ins)", Some(Message::ToggleMark)),
            self.icon_button(icons::SWAP, "Switch side (Tab)", Some(Message::SwitchSide)),
            iced::widget::Space::new().width(Length::Fill),
            self.icon_button(icons::HIDDEN, "Hidden files (Ctrl H)", Some(Message::ToggleHidden)),
            self.icon_button(icons::SETTINGS, "Settings (F12)", Some(Message::OpenSettings)),
        ]
        .spacing(2)
        .align_y(iced::Alignment::Center);
        container(bar)
            .padding([0, 10])
            .height(46)
            .center_y(46)
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(rgb(t.dark_background).into()),
                ..Default::default()
            })
            .into()
    }

    fn neon(&self) -> bool {
        self.config.view.look == Look::Neon
    }

    fn radius(&self) -> f32 {
        if self.neon() { 0.0 } else { SHEET_RADIUS }
    }

    fn class_color(&self, c: FileClass) -> Color {
        let t = &self.theme;
        match c {
            FileClass::Dir => rgb(t.bright),
            FileClass::Code => rgb(t.code),
            FileClass::Doc | FileClass::Other => rgb(t.foreground),
            FileClass::Image => rgb(t.image),
            FileClass::Archive => rgb(t.red),
        }
    }

    fn pane_view(&self, side: usize) -> Element<'_, Message> {
        let t = &self.theme;
        let p = &self.panes[side];
        let active = side == self.active;
        let list = p.visible(self.show_hidden, &self.sizes);
        let max_size = list.iter().filter_map(|e| entry_size(e, &self.sizes)).max().unwrap_or(0);

        let row_radius = if self.neon() { 0 } else { 3 };
        let rows = list.iter().enumerate().map(|(i, e)| {
            let marked = p.marked.contains(&e.path);
            let is_cur = i == p.cursor;
            let on_cursor = is_cur && active;
            let fg = if on_cursor {
                rgb(t.background)
            } else if marked {
                rgb(t.marked)
            } else {
                self.class_color(e.class())
            };
            let dot = if marked { "●" } else { " " };
            let dot_color = if on_cursor { fg } else if marked { rgb(t.marked) } else { self.class_color(e.class()) };
            let name = if e.is_dir() { format!("/{}", e.name) } else { e.name.clone() };
            let (size, pending) = match (e.is_dir() && !e.in_archive, self.sizes.get(&e.path)) {
                (false, _) => (human_size(e.size), false),
                (true, Some(DirSize::Done(n))) => (human_size(n), false),
                // From the last run's cache, currently being verified.
                (true, Some(DirSize::Cached(n))) => (format!("≈ {}", human_size(n)), true),
                (true, Some(DirSize::Partial(n))) => (format!("… {}", human_size(n)), true),
                (true, None) => ("…".to_string(), true),
            };
            let share = (p.sort == Sort::Size).then(|| {
                let frac = entry_size(e, &self.sizes).unwrap_or(0) as f32 / max_size.max(1) as f32;
                let fill = (frac * 1000.0).round() as u16;
                let bar_color = if on_cursor { Color { a: 0.55, ..rgb(t.background) } } else { Color { a: 0.55, ..rgb(t.accent) } };
                let track = Color { a: if on_cursor { 0.15 } else { 0.12 }, ..Color::BLACK };
                container(row![
                    container(iced::widget::Space::new().height(4)).width(Length::FillPortion(fill.max(1))).style(move |_| container::Style {
                        background: Some(if fill == 0 { track } else { bar_color }.into()),
                        border: Border::default().rounded(2),
                        ..Default::default()
                    }),
                    container(iced::widget::Space::new().height(4)).width(Length::FillPortion(1000 - fill.min(999))),
                ])
                .width(56)
                .style(move |_| container::Style {
                    background: Some(track.into()),
                    border: Border::default().rounded(2),
                    ..Default::default()
                })
            });
            let font = if e.is_dir() { BOLD } else { FONT };
            let editing = self.rename.as_ref().filter(|r| r.side == side && is_cur && r.from == e.path);
            let name: Element<'_, Message> = match editing {
                // Name being edited: plain field with a caret, inside the row.
                Some(r) => {
                    let part = |s: &str| text(s.to_string()).color(rgb(t.bright)).size(13).font(font).wrapping(iced::widget::text::Wrapping::None);
                    let caret = || container(iced::widget::Space::new().width(2).height(15)).style(move |_| container::Style {
                        background: Some(rgb(t.accent).into()),
                        ..Default::default()
                    });
                    let (lo, hi) = r.selection().unwrap_or((r.caret, r.caret));
                    let (lo_b, hi_b) = (r.byte(lo), r.byte(hi));
                    let marked = container(part(&r.name[lo_b..hi_b])).style(move |_| container::Style {
                        background: Some(Color { a: 0.45, ..rgb(t.accent) }.into()),
                        ..Default::default()
                    });
                    let mut field = row![part(&r.name[..lo_b])];
                    if r.caret == lo {
                        field = field.push(caret());
                    }
                    if hi > lo {
                        field = field.push(marked);
                    }
                    if r.caret == hi && hi > lo {
                        field = field.push(caret());
                    }
                    field = field.push(part(&r.name[hi_b..]));
                    container(field.align_y(iced::Alignment::Center))
                        .padding([2, 6])
                        .style(move |_| container::Style {
                            background: Some(rgb(t.dark_background).into()),
                            border: Border::default().rounded(3),
                            ..Default::default()
                        })
                        .into()
                }
                None => text(name).color(fg).size(13).font(font).wrapping(iced::widget::text::Wrapping::None).into(),
            };
            let (size, size_color) = match editing.and_then(|r| r.error.clone()) {
                Some(err) => (err, rgb(t.red)),
                None => (size, if on_cursor { fg } else if pending { rgb(t.light_foreground) } else { rgb(t.foreground) }),
            };
            let mut line = row![
                text(dot).color(dot_color).size(12).width(14),
                container(name)
                .width(Length::Fill)
                .clip(true),
                text(size)
                    .color(size_color)
                    .size(12)
                    .width(if editing.is_some_and(|r| r.error.is_some()) { Length::Shrink } else { Length::Fixed(84.0) })
                    .align_x(iced::alignment::Horizontal::Right),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center);
            if let Some(bar) = share {
                line = line.push(bar);
            }
            if self.show_dates() {
                line = line.push(
                text(e.modified.map(human_date).unwrap_or_default())
                    .color(if on_cursor { fg } else { rgb(t.foreground) })
                    .size(12)
                    .width(100)
                    .align_x(iced::alignment::Horizontal::Right)

                );
            }
            let bg = if on_cursor {
                Some(rgb(t.accent))
            } else if is_cur {
                Some(Color { a: 0.09, ..Color::WHITE })
            } else {
                None
            };
            mouse_area(
                container(line)
                    .height(ROW_H)
                    .padding([0, 10])
                    .center_y(ROW_H)
                    .style(move |_| container::Style {
                        background: bg.map(Background::Color),
                        border: Border::default().rounded(row_radius),
                        ..Default::default()
                    }),
            )
            .on_press(Message::Select(side, i))
            .on_double_click(Message::Open(side, i))
            .into()
        });

        let header = container(
            row![
                container(
                    text(match &self.loads[side] {
                        // Folder change still waiting on the filesystem.
                        Some(l) if !matches!(l.after, After::Reload) => {
                            let state = if l.since.elapsed() >= LOAD_SLOW { "not responding · Esc cancels" } else { "loading …" };
                            format!("{} · {state}", l.path.display())
                        }
                        _ if self.neon() => format!("» {}", p.path.display()),
                        _ => p.path.display().to_string(),
                    })
                        .size(12)
                        .color(if active { rgb(t.accent) } else { rgb(t.foreground) })
                        .wrapping(iced::widget::text::Wrapping::None),
                )
                .width(Length::Fill)
                .clip(true),
                text(if p.archive { "ARCHIVE · " } else if p.network { "NET · " } else { "" }).size(11).font(BOLD).color(rgb(t.foreground)),
                text(p.sort.label()).size(11).color(rgb(t.foreground)),
            ]
            .spacing(10),
        )
        .padding([10, 10])
        .width(Length::Fill)
        .style(move |_| container::Style {
            border: Border { color: rgb(t.muted), width: 0.0, radius: 0.0.into() },
            ..Default::default()
        });

        let list = scrollable(column(rows).padding([0, 6]))
            .direction(scrollable::Direction::Vertical(scrollable::Scrollbar::new().width(4).scroller_width(4).margin(3)))
            .style(move |_, status| {
                let hot = matches!(status, scrollable::Status::Hovered { .. } | scrollable::Status::Dragged { .. });
                let rail = scrollable::Rail {
                    background: None,
                    border: Border::default(),
                    scroller: scrollable::Scroller {
                        background: Color { a: if hot { 0.45 } else { 0.22 }, ..Color::WHITE }.into(),
                        border: Border::default().rounded(2),
                    },
                };
                scrollable::Style {
                    container: container::Style::default(),
                    vertical_rail: rail,
                    horizontal_rail: rail,
                    gap: None,
                    auto_scroll: scrollable::AutoScroll {
                        background: rgb(t.lighter_background).into(),
                        border: Border::default().rounded(4),
                        shadow: iced::Shadow::default(),
                        icon: rgb(t.foreground),
                    },
                }
            })
            .id(p.id.clone())
            .on_scroll(move |vp| Message::Scrolled(side, vp))
            .height(Length::Fill);

        let body: Element<'_, Message> = if active && self.jumping() { self.jump_view() } else { list.into() };
        let tone = if active { t.sheet } else { t.sheet_dim };
        // Neon frames both panes; paper outlines only the active one.
        let outline = if active { Some(rgb(t.accent)) } else { self.neon().then(|| rgb(t.muted)) };
        let sheet = self.sheet(column![header, body].into(), side, tone, outline)
            .width(Length::FillPortion(3));
        mouse_area(sheet).on_enter(Message::HoverPane(side)).into()
    }

    /// Jump mode hit list, in place of the active pane's file list.
    fn jump_view(&self) -> Element<'_, Message> {
        let t = &self.theme;
        let rows = self.jump.iter().enumerate().map(|(i, path)| {
            let on = i == self.jump_cur;
            let fg = if on { rgb(t.background) } else { rgb(t.bright) };
            mouse_area(
                container(text(self.jumper.display(path)).color(fg).size(13).wrapping(iced::widget::text::Wrapping::None))
                    .clip(true)
                    .width(Length::Fill)
                    .height(ROW_H)
                    .padding([0, 10])
                    .center_y(ROW_H)
                    .style(move |_| container::Style {
                        background: on.then(|| rgb(t.accent).into()),
                        border: Border::default().rounded(3),
                        ..Default::default()
                    }),
            )
            .on_press(Message::JumpTo(i))
            .into()
        });
        let hint = if self.jump.is_empty() { "No matches" } else { "↑↓ select · Enter jump · Esc back" };
        column![
            container(text("JUMP TO").size(11).color(rgb(t.foreground))).padding([4, 16]),
            column(rows).padding([0, 6]),
            container(text(hint).size(11).color(rgb(t.foreground))).padding([8, 16]),
        ]
        .into()
    }

    /// A sheet of paper: grain texture under the content, optionally with a colored border.
    fn sheet<'a>(&'a self, content: Element<'a, Message>, side: usize, tone: u32, outline: Option<Color>) -> container::Container<'a, Message> {
        let texture: Element<'a, Message> = if self.neon() {
            iced::widget::canvas(&self.scanlines[side]).width(Length::Fill).height(Length::Fill).into()
        } else {
            iced::widget::canvas(&self.papers[side]).width(Length::Fill).height(Length::Fill).into()
        };
        container(iced::widget::stack![texture, content])
        .clip(true)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(rgb(tone).into()),
            border: Border {
                color: outline.unwrap_or(Color::TRANSPARENT),
                width: if self.neon() { 1.0 } else { 1.5 },
                radius: self.radius().into(),
            },
            ..Default::default()
        })
    }

    fn statusbar(&self) -> Element<'_, Message> {
        let t = &self.theme;
        let p = &self.panes[self.active];
        let visible = p.visible(self.show_hidden, &self.sizes);
        let count = visible.len();
        let hidden_marked = p.marked.iter().filter(|m| !visible.iter().any(|e| &e.path == *m)).count();
        // Total of visible entries, once all folder sizes are known.
        let total = visible.iter().try_fold(0u64, |acc, e| match (e.is_dir() && !e.in_archive, self.sizes.get(&e.path)) {
            (false, _) => Some(acc + e.size),
            (true, Some(DirSize::Done(n))) => Some(acc + n),
            _ => None,
        });
        let cur = self.current().map(|e| {
            if e.is_dir() { e.name.clone() } else { format!("{} · {}", e.name, human_size(e.size)) }
        }).unwrap_or_default();
        let marked = p.marked.len();
        let dim = rgb(t.light_foreground);
        let bar_sep = || text("│").color(rgb(t.muted)).size(12);

        let search_icon = svg(svg::Handle::from_memory(icons::SEARCH.as_bytes()))
            .width(15)
            .height(15)
            .style(move |_, _| svg::Style { color: Some(dim) });
        let jumping = self.jumping();
        let (mode, mode_color) = if jumping { ("JUMP", rgb(t.accent)) } else { ("FILTER", dim) };
        let query: Element<'_, Message> = if p.query.is_empty() {
            text("type to filter · ~ to jump").size(12).color(if self.neon() { rgb(t.light_foreground) } else { rgb(0x555555) }).wrapping(iced::widget::text::Wrapping::None).into()
        } else {
            text(format!("{}▏", p.query)).size(12).color(rgb(t.bright)).into()
        };
        let input = row![text(mode).size(10).font(BOLD).color(mode_color), query]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .width(Length::Fill);
        let search = container(row![search_icon, input].spacing(8).align_y(iced::Alignment::Center))
            .width(300)
            .padding([6, 10])
            .style(move |_| container::Style {
                background: Some(if self.neon() { rgb(t.sheet) } else { rgb(0x1a1a1a) }.into()),
                border: if self.neon() { Border { color: rgb(t.muted), width: 1.0, radius: 0.0.into() } } else { Border::default().rounded(6) },
                ..Default::default()
            });

        // Running jobs: a small clickable progress readout.
        let jobs: Element<'_, Message> = match &self.jobs.running {
            Some((kind, job, _)) => {
                let p = job.progress();
                let pct = (p.done * 100).checked_div(p.total).unwrap_or(0).min(100);
                let verb = match kind {
                    OpKind::Copy => "Copying",
                    OpKind::Move => "Moving",
                    OpKind::Trash => "Trashing",
                    OpKind::Delete => "Deleting",
                };
                let count = self.jobs.started + self.jobs.queue.len();
                let of = if count > 1 { format!(" · {} of {count}", self.jobs.started) } else { String::new() };
                let waiting = if job.conflict().is_some() { " · waiting for you" } else { "" };
                mouse_area(text(format!("{verb} {pct}%{of}{waiting}")).size(12).color(rgb(t.accent)).wrapping(iced::widget::text::Wrapping::None))
                    .on_press(Message::OpenJobs)
                    .interaction(iced::mouse::Interaction::Pointer)
                    .into()
            }
            None => iced::widget::Space::new().width(0).into(),
        };
        let bar = row![
            search,
            jobs,
            iced::widget::Space::new().width(Length::Fill),
            text(cur).size(12).color(rgb(t.foreground)).wrapping(iced::widget::text::Wrapping::None),
            bar_sep(),
            text(format!("{count} items")).size(12).color(dim),
            bar_sep(),
            text(match total {
                Some(n) => format!("Σ {}", human_size(n)),
                None => "Σ …".into(),
            })
            .size(12)
            .color(dim),
            bar_sep(),
            text(match (marked, hidden_marked) {
                (0, _) => "nothing marked".into(),
                (m, 0) => format!("{m} marked"),
                (m, h) => format!("{m} marked ({h} hidden)"),
            })
                .size(12)
                .color(if marked > 0 { rgb(t.marked) } else { dim }),
        ]
        .spacing(14)
        .align_y(iced::Alignment::Center);

        container(bar)
            .padding([0, 14])
            .height(42)
            .center_y(42)
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(rgb(t.dark_background).into()),
                ..Default::default()
            })
            .into()
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn save_ppm(path: &std::ffi::OsStr, shot: &iced::window::Screenshot) {
    let mut out = format!("P6 {} {} 255\n", shot.size.width, shot.size.height).into_bytes();
    out.extend(shot.rgba.chunks_exact(4).flat_map(|px| [px[0], px[1], px[2]]));
    let _ = std::fs::write(path, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_finds_commands() {
        let label = |q: &str| COMMANDS[palette_hits(q)[0]].0;
        assert_eq!(label("copy"), "Copy to other pane");
        assert_eq!(label("trash"), "Move to trash");
        assert_eq!(label("hid"), "Show / hide hidden files");
        assert_eq!(label("drv r"), "Drives: right pane");
        assert!(palette_hits("zzzq").is_empty());
        assert_eq!(palette_hits("").len(), COMMANDS.len());
    }
}
