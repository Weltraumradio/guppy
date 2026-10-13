mod icons;
mod paper;
mod watch;

use paper::Paper;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use guppy_core::fs::{human_date, human_size};
use guppy_core::ops::{self, Job, OpKind};
use guppy_core::{Cancel, DirSize, DirSizes, Entry, FileClass, Jumper};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::operation::{self, AbsoluteOffset};
use iced::widget::{
    Id, button, column, container, mouse_area, row, scrollable, svg, text,
};
use iced::{Background, Border, Color, Element, Font, Length, Subscription, Task};

const ROW_H: f32 = 26.0;
const SHEET_RADIUS: f32 = 6.0;
const JUMP_LIMIT: usize = 14;
/// Eingebettet, damit Guppy auf jedem System gleich aussieht (Lizenz: fonts/OFL.txt).
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
        // Beim Schließen erst den Größen-Cache sichern.
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
    Open(usize, usize),
    JumpTo(usize),
    Scrolled(usize, scrollable::Viewport),
    /// Ordnergrößen laufen noch: neu zeichnen.
    Tick,
    /// Bildwechsel während weichen Scrollens.
    Frame(std::time::Instant),
    Resized(iced::Size),
    /// Dateien in einem offenen Ordner oder das Theme haben sich geändert.
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
    /// Entwickler-Hilfe: `GUPPY_SCREENSHOT=datei.ppm` speichert ein Bild, sobald die
    /// Größen berechnet sind, und beendet.
    Shot(iced::window::Screenshot),
}

/// Beginnt die Eingabe mit `~` (unter Unix auch `/`), wird gesprungen statt gefiltert.
fn is_jump(query: &str) -> bool {
    query.chars().next().is_some_and(guppy_core::platform::is_jump_prefix)
}

/// Modaler Dialog über dem Fenster; solange er offen ist, gehen Tasten nur an ihn.
enum Dialog {
    Confirm { kind: OpKind, sources: Vec<PathBuf>, target: Option<PathBuf> },
    NewDir { name: String, error: Option<String> },
    Running { kind: OpKind, job: Job },
    Report { problems: Vec<String> },
    /// Kurzhilfe mit allen Tasten (F1).
    Help,
}

/// Abschnitte der Kurzhilfe: Überschrift und (Taste, Funktion).
const HELP: &[(&str, &[(&str, &str)])] = &[
    ("Navigation", &[
        ("↑ ↓", "Cursor bewegen"),
        ("Bild↑ Bild↓", "seitenweise"),
        ("Pos1 Ende", "erster / letzter Eintrag"),
        ("Enter", "öffnen / hinspringen"),
        ("Backspace", "Suche kürzen, sonst Ordner hoch"),
        ("Alt+← Alt+→", "Verlauf zurück / vor"),
        ("Tab", "Pane wechseln"),
    ]),
    ("Suche", &[
        ("a–z …", "aktives Pane filtern"),
        ("~ oder /", "Ordner springen (unscharf)"),
        ("Strg+G", "Sprungmodus öffnen"),
        ("Esc", "Suche leeren"),
    ]),
    ("Dateien", &[
        ("Einfg", "markieren und weiter"),
        ("Leertaste", "markieren (bei leerer Suche)"),
        ("F5", "kopieren ins andere Pane"),
        ("F6", "verschieben ins andere Pane"),
        ("F7", "neuer Ordner"),
        ("F8 Entf", "in den Papierkorb"),
        ("Shift+F8/Entf", "endgültig löschen"),
    ]),
    ("Ansicht", &[
        ("F1", "diese Hilfe"),
        ("Strg+H", "versteckte Dateien"),
        ("Strg+S", "Sortierung: Name, Größe, Datum"),
        ("Strg+R", "Ordnergrößen neu berechnen"),
    ]),
];

fn op_verb(kind: OpKind) -> &'static str {
    match kind {
        OpKind::Copy => "kopieren",
        OpKind::Move => "verschieben",
        OpKind::Trash => "in den Papierkorb legen",
        OpKind::Delete => "endgültig löschen",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort {
    /// Ordner zuerst, dann alphabetisch.
    Name,
    /// Größte zuerst, Ordner und Dateien gemischt.
    Size,
    /// Neueste zuerst.
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
            Sort::Size => "Größe ↓",
            Sort::Date => "Datum ↓",
        }
    }
}

/// Größe eines Eintrags für Anzeige und Sortierung; Ordner aus der Größenberechnung.
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
    /// Index in der *sichtbaren* Liste (nach Filter/versteckten Dateien).
    cursor: usize,
    marked: HashSet<PathBuf>,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    /// Getippte Eingabe: Filter, oder Sprungziel wenn sie mit `~`/`/` beginnt.
    query: String,
    sort: Sort,
    /// Bricht die Größenberechnung ab, wenn das Pane den Ordner verlässt.
    sizing: Cancel,
    scroll_y: f32,
    view_h: f32,
    /// Ziel des weichen Scrollens; `None` = keine Animation.
    scroll_target: Option<f32>,
    id: Id,
    /// Liegt auf einem Netzlaufwerk.
    network: bool,
}

/// Was nach dem Einlesen eines Ordners passiert.
#[derive(Clone)]
enum After {
    /// Startordner beim Programmstart.
    Start,
    Nav,
    Back,
    Forward,
    /// Hoch, Cursor auf den Ordner, aus dem wir kamen.
    Parent(PathBuf),
    /// Gleicher Ordner, neu eingelesen; Cursor bleibt auf seinem Eintrag.
    Reload,
}

/// Ordner, der gerade im Hintergrund eingelesen wird.
struct Loading {
    path: PathBuf,
    rx: std::sync::mpsc::Receiver<std::io::Result<Vec<Entry>>>,
    since: std::time::Instant,
    after: After,
}

/// So lange wird synchron auf das Einlesen gewartet; lokale Ordner sind fast immer
/// schneller und erscheinen damit ohne Zwischenzustand.
const LOAD_WAIT: Duration = Duration::from_millis(120);
/// Ab dann gilt ein Ordner als „antwortet nicht“.
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

    /// Zeigt einen frisch eingelesenen Ordner an.
    fn enter(&mut self, path: PathBuf, entries: Vec<Entry>) {
        self.network = guppy_core::platform::is_network(&path);
        self.entries = entries;
        self.path = path;
        self.cursor = 0;
        self.marked.clear();
        self.query.clear();
        // Neuer Ordner beginnt oben, ohne Animation.
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
        // `entries` ist bereits nach Name sortiert; sort_by ist stabil.
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
    /// Treffer im Sprungmodus und die gewählte Zeile.
    jump: Vec<PathBuf>,
    jump_cur: usize,
    shot_wait: u8,
    /// Sprungtreffer müssen nach dem Einlesen noch aufgefrischt werden.
    jump_pending: bool,
    dialog: Option<Dialog>,
    last_frame: Option<std::time::Instant>,
    /// Fensterbreite, für das anpassungsfähige Layout.
    width: f32,
    /// Zeitpunkt, ab dem die Vorschau geladen wird.
    /// Letzte Cursorbewegung, um Tastenwiederholung zu erkennen.
    last_move: Option<std::time::Instant>,
    /// Ordner, die pro Seite gerade eingelesen werden.
    loads: [Option<Loading>; 2],
}

impl Guppy {
    fn new() -> (Self, Task<Message>) {
        let home = dirs_home();
        let cwd = std::env::current_dir().unwrap_or_else(|_| home.clone());
        let mut g = Self {
            theme: guppy_core::Theme::load(),
            panes: [Pane::new(cwd, "pane-left"), Pane::new(home, "pane-right")],
            active: 0,
            show_hidden: false,
            papers: [Paper::new(1), Paper::new(2)],
            sizes: DirSizes::default_cache_file().map(DirSizes::with_cache).unwrap_or_default(),
            jumper: Jumper::load(),
            jump: Vec::new(),
            jump_cur: 0,
            shot_wait: 0,
            jump_pending: false,
            dialog: None,
            last_frame: None,
            width: 1200.0,
            last_move: None,
            loads: [None, None],
        };
        for side in 0..2 {
            let path = g.panes[side].path.clone();
            g.load(side, path, After::Start);
        }
        // Entwickler-Hilfe: vorgegebene Eingabe, z. B. `GUPPY_QUERY=~gu`.
        if let Ok(q) = std::env::var("GUPPY_QUERY") {
            g.set_query(q);
        }
        if std::env::var("GUPPY_SORT").as_deref() == Ok("size") {
            g.panes.iter_mut().for_each(|p| p.sort = Sort::Size);
        }
        // `GUPPY_DIALOG=copy` öffnet den Kopierdialog, `=run` bestätigt ihn auch.
        if let Ok(d) = std::env::var("GUPPY_DIALOG") {
            if d == "help" {
                g.dialog = Some(Dialog::Help);
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
            // Das Theme-Verzeichnis selbst und sein Elternordner (Omarchy tauscht es beim Wechsel aus).
            dirs.extend(theme_dir.parent().map(Path::to_path_buf));
            dirs.push(theme_dir);
        }
        let mut keys = Subscription::batch([
            keyboard::listen().map(Message::Key),
            iced::window::resize_events().map(|(_, size)| Message::Resized(size)),
            iced::window::close_requests().map(Message::CloseRequested),
            watch::changes(watch::Targets { dirs }).map(Message::FsChanged),
        ]);
        if self.panes.iter().any(|p| p.scroll_target.is_some()) {
            keys = Subscription::batch([keys, iced::window::frames().map(Message::Frame)]);
        }
        // Nur solange Größen berechnet werden, sonst kein Leerlauf-Takt.
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

    /// Bei schmalem Fenster weicht die Datumsspalte.
    fn show_dates(&self) -> bool {
        self.width >= 760.0
    }

    fn jumping(&self) -> bool {
        is_jump(&self.panes[self.active].query)
    }

    fn update(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::Key(keyboard::Event::KeyPressed { key, text, .. }) if self.dialog.is_some() => {
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
                // Unterordner-Index wächst: Treffer auffrischen, Auswahl behalten.
                // Nach dem Ende des Einlesens ein letztes Mal.
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
                // Ein paar Takte warten, damit der letzte Stand gezeichnet ist.
                self.shot_wait += 1;
                if self.shot_wait < 4 {
                    return Task::none();
                }
                return iced::window::latest().and_then(iced::window::screenshot).map(Message::Shot);
            }
            Message::Key(_) | Message::Tick => return Task::none(),
            Message::Select(side, i) => {
                self.active = side;
                self.panes[side].cursor = i;
            }
            Message::Open(side, i) => {
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
                // Während der Animation meldet die Liste die Position eines früheren Bildes;
                // maßgeblich ist dann die eigene.
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

    /// Tastatur-Konzept: Buchstaben filtern das aktive Pane, `~`/`/` am Anfang springt.
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
            Key::Named(Named::F1) => self.dialog = Some(Dialog::Help),
            Key::Named(Named::Enter) if jumping => self.jump_selected(),
            Key::Named(Named::Enter) => self.open_current(),
            Key::Named(Named::Backspace) if !query_empty => {
                let mut q = self.panes[self.active].query.clone();
                q.pop();
                self.set_query(q);
            }
            Key::Named(Named::Backspace) => self.go_parent(),
            // Esc bricht zuerst einen hängenden Ordnerwechsel ab, dann die Suche.
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
            Key::Character("s") if m.control() => {
                // Cursor bleibt auf demselben Eintrag.
                let path = self.current().map(|e| e.path.clone());
                let p = &mut self.panes[self.active];
                p.sort = p.sort.next();
                let pos = self.panes[self.active].visible(self.show_hidden, &self.sizes).iter().position(|e| Some(&e.path) == path.as_ref());
                self.panes[self.active].cursor = pos.unwrap_or(0);
            }
            Key::Character("h") if m.control() => return self.update(Message::ToggleHidden),
            Key::Character("r") if m.control() => {
                let path = self.panes[self.active].path.clone();
                self.sizes.invalidate(&path);
                self.request_sizes(self.active);
                self.reload(self.active);
            }
            // Alles Druckbare ohne Strg/Alt landet in der Eingabe.
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

    /// Ziel der Aktion: markierte sichtbare Einträge, sonst der unter dem Cursor.
    fn selection(&self) -> Vec<PathBuf> {
        let p = &self.panes[self.active];
        let visible = p.visible(self.show_hidden, &self.sizes);
        let marked: Vec<PathBuf> = visible.iter().filter(|e| p.marked.contains(&e.path)).map(|e| e.path.clone()).collect();
        if !marked.is_empty() {
            return marked;
        }
        visible.get(p.cursor).map(|e| vec![e.path.clone()]).unwrap_or_default()
    }

    fn ask(&mut self, kind: OpKind) {
        let sources = self.selection();
        if sources.is_empty() {
            return;
        }
        let target = matches!(kind, OpKind::Copy | OpKind::Move).then(|| self.panes[1 - self.active].path.clone());
        if target.as_deref() == Some(self.panes[self.active].path.as_path()) {
            self.dialog = Some(Dialog::Report { problems: vec!["Quelle und Ziel sind derselbe Ordner.".into()] });
            return;
        }
        self.dialog = Some(Dialog::Confirm { kind, sources, target });
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
            Some(Dialog::Report { .. } | Dialog::Help) | None => {}
        }
    }

    fn cancel_dialog(&mut self) {
        match &self.dialog {
            // Laufende Operation abbrechen; der Dialog schließt, sobald sie steht.
            Some(Dialog::Running { job, .. }) => job.cancel(),
            _ => self.dialog = None,
        }
    }

    fn on_dialog_key(&mut self, key: Key, typed: Option<&str>) {
        match key.as_ref() {
            Key::Named(Named::Enter) => self.confirm_dialog(),
            Key::Named(Named::Escape) => self.cancel_dialog(),
            Key::Named(Named::F1) if matches!(self.dialog, Some(Dialog::Help)) => self.dialog = None,
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

    fn poll_job(&mut self) {
        let Some(Dialog::Running { job, .. }) = &self.dialog else { return };
        let progress = job.progress();
        if !progress.finished {
            return;
        }
        self.dialog = (!progress.problems.is_empty()).then_some(Dialog::Report { problems: progress.problems });
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
        for side in 0..2 {
            let dir = self.panes[side].path.clone();
            // Ereignisse am Ordner selbst (z. B. Zeitstempel) ändern keine Größen.
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

    /// Liest den Ordner neu ein und behält den Cursor auf seinem Eintrag.
    fn reload(&mut self, side: usize) {
        let path = self.panes[side].path.clone();
        self.load(side, path, After::Reload);
    }

    /// Liest einen Ordner im Hintergrund ein. Ist er schnell da (lokal fast immer),
    /// wird er sofort angezeigt; sonst bleibt die Oberfläche bedienbar, bis er kommt.
    fn load(&mut self, side: usize, path: PathBuf, after: After) {
        // Ein Neueinlesen ersetzt keinen laufenden Ordnerwechsel.
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
                // Unlesbar (Rechte, NAS weg): Ansicht bleibt, wie sie war.
                if !matches!(after, After::Reload | After::Start) {
                    self.dialog = Some(Dialog::Report { problems: vec![format!("{}: {e}", self.jumper.display(&path))] });
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
            // Laufende Berechnungen weiterlaufen lassen, nur Fehlendes nachfordern.
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
        // Eingetippter Pfad, der existiert, steht ganz oben.
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

    /// Hält den Cursor sichtbar, mit zwei Zeilen Abstand zum Rand; gescrollt wird weich.
    fn scroll_into_view(&mut self) -> Task<Message> {
        if self.jumping() {
            return Task::none();
        }
        const MARGIN: f32 = 2.0 * ROW_H;
        let now = std::time::Instant::now();
        let p = &mut self.panes[self.active];
        // Gegen das laufende Ziel rechnen, sonst springt es bei Tastenwiederholung zurück.
        let base = p.scroll_target.unwrap_or(p.scroll_y);
        let top = p.cursor as f32 * ROW_H;
        let margin = MARGIN.min(((p.view_h - ROW_H) / 2.0).max(0.0));
        // Ziele auf ganze Zeilen runden, damit die Zeilen beim Scrollbeginn nicht
        // um ein paar Pixel (Fensterhöhe modulo Zeilenhöhe) springen.
        let y = if top - margin < base {
            ((top - margin) / ROW_H).floor().max(0.0) * ROW_H
        } else if top + ROW_H + margin > base + p.view_h {
            ((top + ROW_H + margin - p.view_h) / ROW_H).ceil() * ROW_H
        } else {
            self.last_move = Some(now);
            return Task::none();
        };
        // Tastenwiederholung: Liste läuft im Takt der Tasten mit, der Cursor steht still.
        // Animiert nachzuziehen erzeugt hier ein Sägezahn-Zittern (Cursor springt, Liste holt auf).
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

    /// Ein Animationsschritt: nähert sich dem Ziel exponentiell, unabhängig von der Bildrate.
    fn animate_scroll(&mut self, now: std::time::Instant) -> Task<Message> {
        const TAU: f32 = 0.03; // Sekunden; kleiner = schneller
        let dt = self.last_frame.map_or(1.0 / 60.0, |t| now.duration_since(t).as_secs_f32()).min(0.1);
        self.last_frame = Some(now);
        let k = 1.0 - (-dt / TAU).exp();
        let mut tasks = Vec::new();
        for p in &mut self.panes {
            let Some(target) = p.scroll_target else { continue };
            let mut y = p.scroll_y + (target - p.scroll_y) * k;
            // Nie hinter dem Cursor herhinken: seine Zeile bleibt vollständig im Bild.
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

    // ---------- Ansicht ----------

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
            row![iced::widget::Space::new().width(Length::Fill), btn("Abbrechen", Message::DialogCancel, false), btn(ok, Message::DialogConfirm, true)]
                .spacing(8)
        };
        let names = |paths: &[PathBuf]| {
            let mut lines: Vec<String> = paths.iter().take(6).map(|p| {
                let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if p.is_dir() { format!("· {n}/") } else { format!("· {n}") }
            }).collect();
            if paths.len() > 6 {
                lines.push(format!("  und {} weitere", paths.len() - 6));
            }
            small(lines.join("\n"))
        };

        let content: Element<'_, Message> = match dialog {
            Dialog::Confirm { kind, sources, target } => {
                let what = if sources.len() == 1 { "1 Eintrag".to_string() } else { format!("{} Einträge", sources.len()) };
                let mut c = column![title(format!("{what} {}", op_verb(*kind)))].spacing(14);
                if let Some(target) = target {
                    c = c.push(small(format!("nach {}", self.jumper.display(target))));
                }
                c = c.push(names(sources));
                if *kind == OpKind::Delete {
                    c = c.push(text("Kann nicht rückgängig gemacht werden.").size(12).color(rgb(t.red)));
                }
                let ok = match kind {
                    OpKind::Copy => "Kopieren",
                    OpKind::Move => "Verschieben",
                    OpKind::Trash => "Papierkorb",
                    OpKind::Delete => "Löschen",
                };
                c.push(buttons(ok)).into()
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
                    title("Neuer Ordner".into()),
                    small(format!("in {}", self.jumper.display(&self.panes[self.active].path))),
                    field,
                ]
                .spacing(14);
                if let Some(e) = error {
                    c = c.push(text(e.clone()).size(12).color(rgb(t.red)));
                }
                c.push(buttons("Anlegen")).into()
            }
            Dialog::Running { kind, job } => {
                let p = job.progress();
                let frac = if p.total == 0 { 0.0 } else { p.done as f32 / p.total as f32 };
                let amount = if matches!(kind, OpKind::Copy | OpKind::Move) {
                    format!("{} von {}", human_size(p.done), human_size(p.total))
                } else {
                    format!("{} von {}", p.done, p.total)
                };
                column![
                    title(format!("Wird {}", match kind {
                        OpKind::Copy => "kopiert",
                        OpKind::Move => "verschoben",
                        OpKind::Trash => "in den Papierkorb gelegt",
                        OpKind::Delete => "gelöscht",
                    })),
                    iced::widget::progress_bar(0.0..=1.0, frac).girth(6).style(move |_| iced::widget::progress_bar::Style {
                        background: Color { a: 0.25, ..Color::BLACK }.into(),
                        bar: rgb(t.accent).into(),
                        border: Border::default().rounded(3),
                    }),
                    row![small(p.current), iced::widget::Space::new().width(Length::Fill), small(amount)],
                    row![iced::widget::Space::new().width(Length::Fill), btn("Abbrechen", Message::DialogCancel, false)],
                ]
                .spacing(14)
                .into()
            }
            Dialog::Help => {
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
                    title("Guppy · Tasten".into()),
                    small("Alles geht per Tastatur. Tippen filtert das aktive Pane, ~ oder / springt zu einem Ordner. Ohne Markierung wirken Dateioperationen auf den Eintrag unter dem Cursor; Enter bestätigt, Esc bricht ab.".into()),
                    row![
                        column![section(&HELP[0]), section(&HELP[1])].spacing(18).width(Length::Fill),
                        column![section(&HELP[2]), section(&HELP[3])].spacing(18).width(Length::Fill),
                    ]
                    .spacing(24),
                    row![small("F1 oder Esc schließt".into()), iced::widget::Space::new().width(Length::Fill), btn("OK", Message::DialogCancel, true)]
                        .align_y(iced::Alignment::Center),
                ]
                .spacing(18)
                .into()
            }
            Dialog::Report { problems } => {
                let mut lines: Vec<String> = problems.iter().take(8).cloned().collect();
                if problems.len() > 8 {
                    lines.push(format!("und {} weitere", problems.len() - 8));
                }
                column![
                    title("Erledigt, mit Hinweisen".into()),
                    small(lines.join("\n")),
                    row![iced::widget::Space::new().width(Length::Fill), btn("OK", Message::DialogCancel, true)],
                ]
                .spacing(14)
                .into()
            }
        };

        let card = container(content)
            .padding(22)
            .width(if matches!(dialog, Dialog::Help) { 760 } else { 480 })
        .style(move |_| container::Style {
            background: Some(rgb(0x57534a).into()),
            border: Border { color: rgb(t.accent), width: 1.5, radius: SHEET_RADIUS.into() },
            ..Default::default()
        });
        Some(
            container(card)
                .center(Length::Fill)
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
            self.icon_button(icons::BACK, "Zurück (Alt ←)", (!p.back.is_empty()).then_some(Message::Back)),
            self.icon_button(icons::FORWARD, "Vor (Alt →)", (!p.forward.is_empty()).then_some(Message::Forward)),
            self.icon_button(icons::UP, "Übergeordneter Ordner (⌫)", Some(Message::Parent)),
            self.icon_button(icons::HOME, "Home", Some(Message::Home)),
            sep(),
            self.icon_button(icons::NEW_DIR, "Neuer Ordner (F7)", Some(Message::NewDir)),
            self.icon_button(icons::COPY, "Kopieren (F5)", Some(Message::Op(OpKind::Copy))),
            self.icon_button(icons::MOVE, "Verschieben (F6)", Some(Message::Op(OpKind::Move))),
            self.icon_button(icons::DELETE, "Papierkorb (F8 · Entf), endgültig mit Umschalt", Some(Message::Op(OpKind::Trash))),
            sep(),
            self.icon_button(icons::MARK, "Markieren (Ins)", Some(Message::ToggleMark)),
            self.icon_button(icons::SWAP, "Seite wechseln (Tab)", Some(Message::SwitchSide)),
            iced::widget::Space::new().width(Length::Fill),
            self.icon_button(icons::HIDDEN, "Versteckte Dateien (Strg H)", Some(Message::ToggleHidden)),
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
            let dot = if marked { "●" } else if e.is_dir() { " " } else { "▪" };
            let dot_color = if on_cursor { fg } else if marked { rgb(t.marked) } else { self.class_color(e.class()) };
            let name = if e.is_dir() { format!("/{}", e.name) } else { e.name.clone() };
            let (size, pending) = match (e.is_dir(), self.sizes.get(&e.path)) {
                (false, _) => (human_size(e.size), false),
                (true, Some(DirSize::Done(n))) => (human_size(n), false),
                // Aus dem Cache des letzten Laufs, wird gerade überprüft.
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
            let mut line = row![
                text(dot).color(dot_color).size(12).width(14),
                container(
                    text(name).color(fg).size(13).font(if e.is_dir() { BOLD } else { FONT })
                        .wrapping(iced::widget::text::Wrapping::None),
                )
                .width(Length::Fill)
                .clip(true),
                text(size)
                    .color(if on_cursor { fg } else if pending { rgb(t.light_foreground) } else { rgb(t.foreground) })
                    .size(12)
                    .width(84)
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
                        // Ordnerwechsel, der noch auf das Dateisystem wartet.
                        Some(l) if !matches!(l.after, After::Reload) => {
                            let state = if l.since.elapsed() >= LOAD_SLOW { "antwortet nicht · Esc bricht ab" } else { "lädt …" };
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
                text(if p.network { "NETZ · " } else { "" }).size(11).font(BOLD).color(rgb(t.foreground)),
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
        self.sheet(column![header, body].into(), &self.papers[side], tone, active.then_some(rgb(t.accent)))
            .width(Length::FillPortion(3))
            .into()
    }

    /// Trefferliste im Sprungmodus, an Stelle der Dateiliste des aktiven Panes.
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
        let hint = if self.jump.is_empty() { "Keine Treffer" } else { "↑↓ wählen · Enter springen · Esc zurück" };
        column![
            container(text("SPRINGEN NACH").size(11).color(rgb(t.foreground))).padding([4, 16]),
            column(rows).padding([0, 6]),
            container(text(hint).size(11).color(rgb(t.foreground))).padding([8, 16]),
        ]
        .into()
    }

    /// Ein Blatt Papier: Korn-Struktur unter dem Inhalt, optional mit farbigem Rand.
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
        // Summe der sichtbaren Einträge, sobald alle Ordnergrößen feststehen.
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
        let (mode, mode_color) = if jumping { ("SPRINGEN", rgb(t.accent)) } else { ("FILTER", dim) };
        let query: Element<'_, Message> = if p.query.is_empty() {
            text("tippen filtert · ~ springt").size(12).color(rgb(0x555555)).wrapping(iced::widget::text::Wrapping::None).into()
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
            text(format!("{count} Einträge")).size(12).color(dim),
            bar_sep(),
            text(match total {
                Some(n) => format!("Σ {}", human_size(n)),
                None => "Σ …".into(),
            })
            .size(12)
            .color(dim),
            bar_sep(),
            text(match (marked, hidden_marked) {
                (0, _) => "nichts markiert".into(),
                (m, 0) => format!("{m} markiert"),
                (m, h) => format!("{m} markiert ({h} ausgeblendet)"),
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

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn save_ppm(path: &std::ffi::OsStr, shot: &iced::window::Screenshot) {
    let mut out = format!("P6 {} {} 255\n", shot.size.width, shot.size.height).into_bytes();
    out.extend(shot.rgba.chunks_exact(4).flat_map(|px| [px[0], px[1], px[2]]));
    let _ = std::fs::write(path, out);
}
