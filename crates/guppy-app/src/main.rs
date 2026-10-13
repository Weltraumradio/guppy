mod clip;
mod icons;
mod paper;
mod watch;

use paper::Paper;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use guppy_core::fs::{human_date, human_size};
use guppy_core::ops::{self, Job, OpKind};
use guppy_core::config::SortBy;
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
    Running { kind: OpKind, job: Job },
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

/// What the viewer shows.
#[derive(Debug, Clone)]
enum ViewBody {
    Loading,
    Text { text: String, truncated: bool },
    /// Binary file as a hex dump.
    Hex { text: String, truncated: bool },
    Image,
    Svg,
    /// Folder: its entries, one per line.
    Dir(String),
    /// Not displayable; a short description instead.
    Other(String),
}

/// The viewer shows at most this much of a text file.
const VIEW_MAX: usize = 512 * 1024;
/// Binary files: dumped up to this size (16 bytes per line).
const HEX_MAX: usize = 64 * 1024;

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
    if meta.len() as usize > 4 * VIEW_MAX {
        return Err(format!("Too large for the editor · {}", human_size(meta.len())));
    }
    let buf = std::fs::read(path).map_err(|e| e.to_string())?;
    if buf.iter().take(8192).any(|&b| b == 0) {
        return Err("Binary file, not editable here".into());
    }
    String::from_utf8(buf).map_err(|_| "Not UTF-8 text, not editable here".to_string())
}

fn view_id() -> Id {
    Id::new("viewer")
}

/// Reads what the viewer shows; runs off the UI thread (slow drives, big files).
fn load_view(path: &Path) -> ViewBody {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return ViewBody::Other(e.to_string()),
    };
    if meta.is_dir() {
        return match guppy_core::read_dir(path) {
            Ok(list) => ViewBody::Dir(
                list.iter().map(|c| if c.is_dir() { format!("{}/", c.name) } else { format!("{:<40} {:>10}", c.name, human_size(c.size)) }).collect::<Vec<_>>().join("\n"),
            ),
            Err(e) => ViewBody::Other(e.to_string()),
        };
    }
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" => return ViewBody::Image,
        "svg" => return ViewBody::Svg,
        _ => {}
    }
    use std::io::Read;
    let mut buf = Vec::new();
    let read = std::fs::File::open(path).and_then(|f| f.take(VIEW_MAX as u64).read_to_end(&mut buf));
    if let Err(e) = read {
        return ViewBody::Other(e.to_string());
    }
    // NUL bytes in the first few KB: treat as binary.
    if buf.iter().take(8192).any(|&b| b == 0) {
        buf.truncate(HEX_MAX);
        return ViewBody::Hex { text: hex_dump(&buf), truncated: meta.len() as usize > HEX_MAX };
    }
    let text = match String::from_utf8(buf) {
        Ok(s) => s,
        // Cut-off multi-byte character at the end of the buffer.
        Err(e) => String::from_utf8_lossy(&e.as_bytes()[..e.utf8_error().valid_up_to().max(e.as_bytes().len().saturating_sub(4))]).into_owned(),
    };
    ViewBody::Text { text: text.replace('\t', "    "), truncated: meta.len() as usize > VIEW_MAX }
}

/// Offset, 16 bytes in hex (split in two groups), then printable ASCII.
fn hex_dump(buf: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(buf.len() / 16 * 80 + 80);
    for (n, chunk) in buf.chunks(16).enumerate() {
        let _ = write!(out, "{:08x}  ", n * 16);
        for i in 0..16 {
            match chunk.get(i) {
                Some(b) => { let _ = write!(out, "{b:02x} "); }
                None => out.push_str("   "),
            }
            if i == 7 {
                out.push(' ');
            }
        }
        out.push(' ');
        out.extend(chunk.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '·' }));
        out.push('\n');
    }
    out
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
        _ => on(c.start.help),
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
        _ => {}
    }
}

/// Quick help sections: heading and (key, action).
const HELP: &[(&str, &[(&str, &str)])] = &[
    ("Navigation", &[
        ("↑ ↓", "move cursor"),
        ("PgUp PgDn", "page up / down"),
        ("Home End", "first / last item"),
        ("Enter", "open / jump there"),
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
    if !e.is_dir() {
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
        };
        p.network = guppy_core::platform::is_network(&path);
        p.path = path;
        p
    }

    /// Shows a freshly read folder.
    fn enter(&mut self, path: PathBuf, entries: Vec<Entry>) {
        self.network = guppy_core::platform::is_network(&path);
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
            theme: guppy_core::Theme::load(),
            panes: [
                Pane::new(Config::resolve(&config.start.left, &cwd), "pane-left"),
                Pane::new(Config::resolve(&config.start.right, &cwd), "pane-right"),
            ],
            active: 0,
            show_hidden: config.view.show_hidden,
            papers: [Paper::new(1), Paper::new(2)],
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
        let running = matches!(self.dialog, Some(Dialog::Running { .. }));
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
                if sources.iter().all(|s| s.parent() == Some(target.as_path())) {
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
                self.on_dialog_key(key, text.as_deref());
                return Task::none();
            }
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, text, .. }) => {
                return self.on_key(key, modifiers, text.as_deref());
            }
            Message::Tick if matches!(self.dialog, Some(Dialog::Running { .. })) => {
                self.poll_job();
                return Task::none();
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
            Message::HoverPane(side) if self.rename.is_none() && self.dialog.is_none() => self.active = side,
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
                self.open_current();
            }
            Message::JumpTo(i) => {
                self.jump_cur = i;
                self.jump_selected();
            }
            Message::Frame(now) => return self.animate_scroll(now),
            Message::CloseRequested(id) => {
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
            Message::NewDir => self.dialog = Some(Dialog::NewDir { name: String::new(), error: None }),
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
            Key::Named(Named::F1) => self.dialog = Some(Dialog::Help { startup: false }),
            Key::Named(Named::F2) => self.start_rename(),
            Key::Named(Named::F3) => return self.open_view(),
            Key::Named(Named::F4) if m.shift() => self.edit_external(),
            Key::Named(Named::F4) => return self.edit_current(),
            Key::Named(Named::F12) => return self.update(Message::OpenSettings),
            Key::Character(",") if m.control() => return self.update(Message::OpenSettings),
            Key::Named(Named::Enter) if jumping => self.jump_selected(),
            Key::Named(Named::Enter) => self.open_current(),
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
            Key::Named(Named::F7) => self.dialog = Some(Dialog::NewDir { name: String::new(), error: None }),
            Key::Named(Named::F8 | Named::Delete) if m.shift() => self.ask(OpKind::Delete),
            Key::Named(Named::F8 | Named::Delete) => self.ask(OpKind::Trash),
            Key::Character("g") if m.control() => self.set_query("~".into()),
            // The cursor keeps its row; the entries reorder beneath it.
            Key::Character("s") if m.control() => {
                let p = &mut self.panes[self.active];
                p.sort = p.sort.next();
            }
            Key::Character("h") if m.control() => return self.update(Message::ToggleHidden),
            Key::Character("r") if m.control() => {
                let path = self.panes[self.active].path.clone();
                self.sizes.invalidate(&path);
                self.request_sizes(self.active);
                self.reload(self.active);
            }
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
            Some(Dialog::Confirm { kind, sources, target }) => {
                self.dialog = Some(Dialog::Running { kind, job: ops::start(kind, sources, target) });
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
            Some(Dialog::Running { kind, job }) => self.dialog = Some(Dialog::Running { kind, job }),
            Some(d @ Dialog::Settings { .. }) => self.dialog = Some(d),
            Some(d @ (Dialog::View { .. } | Dialog::Edit { .. })) => self.dialog = Some(d),
            Some(Dialog::Report { .. } | Dialog::Help { .. }) | None => {}
        }
    }

    fn cancel_dialog(&mut self) {
        match &self.dialog {
            // Cancel the running operation; the dialog closes once it stops.
            Some(Dialog::Running { job, .. }) => job.cancel(),
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
            Key::Named(Named::Space) if matches!(self.dialog, Some(Dialog::Help { startup: true })) => {
                let _ = self.update(Message::ToggleStartHelp);
            }
            _ => {
                if let Some(Dialog::NewDir { name, error }) = &mut self.dialog {
                    match (key.as_ref(), typed) {
                        (Key::Named(Named::Backspace), _) => {
                            name.pop();
                        }
                        (_, Some(t)) if t.chars().all(|c| !c.is_control()) => name.push_str(t),
                        _ => return,
                    }
                    *error = None;
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
            Some(Dialog::NewDir { name, .. }) => Some(name),
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
                    (path, smol::unblock(move || load_view(&p)).await)
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

    fn poll_job(&mut self) {
        let Some(Dialog::Running { job, .. }) = &self.dialog else { return };
        let progress = job.progress();
        if !progress.finished {
            return;
        }
        self.dialog = (!progress.problems.is_empty()).then_some(Dialog::Report { title: "Done, with notes", problems: progress.problems });
        for side in 0..2 {
            let path = self.panes[side].path.clone();
            self.sizes.invalidate(&path);
            self.reload(side);
        }
    }

    fn on_fs_changed(&mut self, paths: &[PathBuf]) {
        let theme_dir = guppy_core::Theme::omarchy_path().and_then(|p| p.parent().map(Path::to_path_buf));
        if theme_dir.is_some_and(|t| paths.iter().any(|p| p.starts_with(&t) || t.starts_with(p))) {
            self.theme = guppy_core::Theme::load();
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
            let _ = tx.send(guppy_core::read_dir(&p));
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
            if !self.config.view.dir_sizes {
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
        if matches!(after, After::Nav | After::Parent(_)) {
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
        if !self.config.view.dir_sizes {
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

    fn open_current(&mut self) {
        let Some(e) = self.current() else { return };
        if e.is_dir() {
            let path = e.path.clone();
            self.navigate(path);
        } else {
            let _ = guppy_core::platform::open(&e.path);
        }
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
        let body = row![self.pane_view(0), self.pane_view(1)].spacing(22);
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
                let meta = std::fs::symlink_metadata(path).ok();
                let info = match &meta {
                    Some(m) if m.is_dir() => "folder".to_string(),
                    Some(m) => {
                        let date = m.modified().ok().map(|t| human_date(t.into())).unwrap_or_default();
                        format!("{} · {date}", human_size(m.len()))
                    }
                    None => String::new(),
                };
                let mono = |s: String| text(s).size(12).font(FONT).color(rgb(t.bright));
                let content: Element<'_, Message> = match body {
                    ViewBody::Loading => small("loading …".into()).into(),
                    ViewBody::Text { text: s, truncated } => {
                        let mut c = column![mono(s.clone())];
                        if *truncated {
                            c = c.push(small(format!("… only the first {} are shown", human_size(VIEW_MAX as u64))));
                        }
                        c.into()
                    }
                    ViewBody::Hex { text: s, truncated } => {
                        let mut c = column![mono(s.clone())];
                        if *truncated {
                            c = c.push(small(format!("… only the first {} are shown", human_size(HEX_MAX as u64))));
                        }
                        c.into()
                    }
                    ViewBody::Dir(s) => mono(s.clone()).into(),
                    ViewBody::Image => container(iced::widget::image(iced::widget::image::Handle::from_path(path)).content_fit(iced::ContentFit::ScaleDown))
                        .center_x(Length::Fill)
                        .into(),
                    ViewBody::Svg => container(svg(svg::Handle::from_path(path)).width(Length::Shrink).height(Length::Shrink))
                        .center_x(Length::Fill)
                        .into(),
                    ViewBody::Other(s) => small(s.clone()).into(),
                };
                column![
                    row![title(name), iced::widget::Space::new().width(Length::Fill), small(info)].align_y(iced::Alignment::Center).spacing(12),
                    scrollable(content).id(view_id()).height(Length::Fill).width(Length::Fill),
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
            Dialog::Running { kind, job } => {
                let p = job.progress();
                let frac = if p.total == 0 { 0.0 } else { p.done as f32 / p.total as f32 };
                let amount = if matches!(kind, OpKind::Copy | OpKind::Move) {
                    format!("{} of {}", human_size(p.done), human_size(p.total))
                } else {
                    format!("{} of {}", p.done, p.total)
                };
                column![
                    title(format!("{} …", match kind {
                        OpKind::Copy => "Copying",
                        OpKind::Move => "Moving",
                        OpKind::Trash => "Moving to trash",
                        OpKind::Delete => "Deleting",
                    })),
                    iced::widget::progress_bar(0.0..=1.0, frac).girth(6).style(move |_| iced::widget::progress_bar::Style {
                        background: Color { a: 0.25, ..Color::BLACK }.into(),
                        bar: rgb(t.accent).into(),
                        border: Border::default().rounded(3),
                    }),
                    row![small(p.current), iced::widget::Space::new().width(Length::Fill), small(amount)],
                    row![iced::widget::Space::new().width(Length::Fill), btn("Cancel", Message::DialogCancel, false)],
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
                Dialog::Settings { .. } => Length::Fixed(560.0),
                _ => Length::Fixed(480.0),
            })
            .height(if viewer { Length::Fill } else { Length::Shrink })
            .max_width(1400)
        .style(move |_| container::Style {
            background: Some(rgb(0x57534a).into()),
            border: Border { color: rgb(t.accent), width: 1.5, radius: SHEET_RADIUS.into() },
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
            text("◆").color(rgb(t.accent)).font(BOLD).size(13),
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

    fn class_color(&self, c: FileClass) -> Color {
        let t = &self.theme;
        match c {
            FileClass::Dir => rgb(t.bright),
            FileClass::Code => rgb(0xe5a45a),
            FileClass::Doc | FileClass::Other => rgb(t.foreground),
            FileClass::Image => rgb(0xb69be0),
            FileClass::Archive => rgb(t.red),
        }
    }

    fn pane_view(&self, side: usize) -> Element<'_, Message> {
        let t = &self.theme;
        let p = &self.panes[side];
        let active = side == self.active;
        let list = p.visible(self.show_hidden, &self.sizes);
        let max_size = list.iter().filter_map(|e| entry_size(e, &self.sizes)).max().unwrap_or(0);

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
            let (size, pending) = match (e.is_dir(), self.sizes.get(&e.path)) {
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
                        border: Border::default().rounded(3),
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
                        _ => p.path.display().to_string(),
                    })
                        .size(12)
                        .color(if active { rgb(t.accent) } else { rgb(t.foreground) })
                        .wrapping(iced::widget::text::Wrapping::None),
                )
                .width(Length::Fill)
                .clip(true),
                text(if p.network { "NET · " } else { "" }).size(11).font(BOLD).color(rgb(t.foreground)),
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
        let tone = if active { 0x57534a } else { 0x4f4b43 };
        let sheet = self.sheet(column![header, body].into(), &self.papers[side], tone, active.then_some(rgb(t.accent)))
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
    fn sheet<'a>(
        &'a self,
        content: Element<'a, Message>,
        paper: &'a Paper,
        tone: u32,
        outline: Option<Color>,
    ) -> container::Container<'a, Message> {
        container(iced::widget::stack![
            iced::widget::canvas(paper).width(Length::Fill).height(Length::Fill),
            content,
        ])
        .clip(true)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(rgb(tone).into()),
            border: Border {
                color: outline.unwrap_or(Color::TRANSPARENT),
                width: 1.5,
                radius: SHEET_RADIUS.into(),
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
        let total = visible.iter().try_fold(0u64, |acc, e| match (e.is_dir(), self.sizes.get(&e.path)) {
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
            text("type to filter · ~ to jump").size(12).color(rgb(0x555555)).wrapping(iced::widget::text::Wrapping::None).into()
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
                background: Some(rgb(0x1a1a1a).into()),
                border: Border::default().rounded(6),
                ..Default::default()
            });

        let bar = row![
            search,
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
