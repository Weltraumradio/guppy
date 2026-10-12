mod icons;
mod paper;

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
    TogglePreview,
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
}

fn op_verb(kind: OpKind) -> &'static str {
    match kind {
        OpKind::Copy => "kopieren",
        OpKind::Move => "verschieben",
        OpKind::Trash => "in den Papierkorb legen",
        OpKind::Delete => "endgültig löschen",
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
    /// Bricht die Größenberechnung ab, wenn das Pane den Ordner verlässt.
    sizing: Cancel,
    scroll_y: f32,
    view_h: f32,
    /// Ziel des weichen Scrollens; `None` = keine Animation.
    scroll_target: Option<f32>,
    id: Id,
}

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
            sizing: Cancel::default(),
            scroll_y: 0.0,
            view_h: 400.0,
            scroll_target: None,
            id: Id::new(id),
        };
        p.load(path);
        p
    }

    fn load(&mut self, path: PathBuf) {
        // Unlesbare Ordner (Rechte) lassen die Ansicht unverändert.
        if let Ok(entries) = guppy_core::read_dir(&path) {
            self.entries = entries;
            self.path = path;
            self.cursor = 0;
            self.marked.clear();
            self.query.clear();
            // Neuer Ordner beginnt oben, ohne Animation.
            self.scroll_y = 0.0;
            self.scroll_target = Some(0.0);
        }
    }

    fn visible(&self, hidden: bool) -> Vec<&Entry> {
        let f = if is_jump(&self.query) { String::new() } else { self.query.to_lowercase() };
        self.entries
            .iter()
            .filter(|e| hidden || !e.hidden)
            .filter(|e| f.is_empty() || e.name.to_lowercase().contains(&f))
            .collect()
    }
}

struct Guppy {
    theme: guppy_core::Theme,
    panes: [Pane; 2],
    active: usize,
    show_hidden: bool,
    show_preview: bool,
    preview: Option<(PathBuf, String)>,
    papers: [Paper; 3],
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
    preview_due: Option<std::time::Instant>,
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
            show_preview: true,
            preview: None,
            papers: [Paper::new(1), Paper::new(2), Paper::new(3)],
            sizes: DirSizes::default(),
            jumper: Jumper::load(),
            jump: Vec::new(),
            jump_cur: 0,
            shot_wait: 0,
            jump_pending: false,
            dialog: None,
            last_frame: None,
            width: 1200.0,
            preview_due: None,
        };
        g.request_sizes(0);
        g.request_sizes(1);
        g.refresh_preview();
        // Entwickler-Hilfe: vorgegebene Eingabe, z. B. `GUPPY_QUERY=~gu`.
        if let Ok(q) = std::env::var("GUPPY_QUERY") {
            g.set_query(q);
        }
        // `GUPPY_DIALOG=copy` öffnet den Kopierdialog, `=run` bestätigt ihn auch.
        if let Ok(d) = std::env::var("GUPPY_DIALOG") {
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
        let mut keys = Subscription::batch([
            keyboard::listen().map(Message::Key),
            iced::window::resize_events().map(|(_, size)| Message::Resized(size)),
        ]);
        if self.panes.iter().any(|p| p.scroll_target.is_some()) {
            keys = Subscription::batch([keys, iced::window::frames().map(Message::Frame)]);
        }
        // Nur solange Größen berechnet werden, sonst kein Leerlauf-Takt.
        let running = matches!(self.dialog, Some(Dialog::Running { .. }));
        if self.sizes.busy() || self.jump_pending || running || self.preview_due.is_some() || std::env::var_os("GUPPY_SCREENSHOT").is_some() {
            Subscription::batch([keys, iced::time::every(Duration::from_millis(120)).map(|_| Message::Tick)])
        } else {
            keys
        }
    }

    fn current(&self) -> Option<&Entry> {
        let p = &self.panes[self.active];
        p.visible(self.show_hidden).get(p.cursor).copied()
    }

    /// Bei schmalem Fenster weicht zuerst die Vorschau, dann die Datumsspalte.
    fn preview_visible(&self) -> bool {
        self.show_preview && self.width >= 1100.0
    }

    fn show_dates(&self) -> bool {
        self.width >= if self.preview_visible() { 1300.0 } else { 760.0 }
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
            Message::Tick if self.preview_due.is_some_and(|t| std::time::Instant::now() >= t) => {
                self.preview_due = None;
                self.refresh_preview();
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
            Message::Tick if std::env::var_os("GUPPY_SCREENSHOT").is_some() && !self.sizes.busy() && !self.jump_pending && self.preview_due.is_none() => {
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
            Message::TogglePreview => self.show_preview = !self.show_preview,
            Message::Shot(shot) => {
                if let Some(path) = std::env::var_os("GUPPY_SCREENSHOT") {
                    save_ppm(&path, &shot);
                }
                return iced::exit();
            }
        }
        self.schedule_preview();
        self.scroll_into_view()
    }

    /// Tastatur-Konzept: Buchstaben filtern das aktive Pane, `~`/`/` am Anfang springt.
    fn on_key(&mut self, key: Key, m: keyboard::Modifiers, typed: Option<&str>) -> Task<Message> {
        let jumping = self.jumping();
        let query_empty = self.panes[self.active].query.is_empty();
        let len = if jumping { self.jump.len() } else { self.panes[self.active].visible(self.show_hidden).len() };
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
            Key::Named(Named::Enter) if jumping => self.jump_selected(),
            Key::Named(Named::Enter) => self.open_current(),
            Key::Named(Named::Backspace) if !query_empty => {
                let mut q = self.panes[self.active].query.clone();
                q.pop();
                self.set_query(q);
            }
            Key::Named(Named::Backspace) => self.go_parent(),
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
            Key::Character("h") if m.control() => return self.update(Message::ToggleHidden),
            Key::Character("r") if m.control() => {
                let path = self.panes[self.active].path.clone();
                self.sizes.invalidate(&path);
                self.request_sizes(self.active);
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
        self.schedule_preview();
        self.scroll_into_view()
    }

    /// Ziel der Aktion: markierte sichtbare Einträge, sonst der unter dem Cursor.
    fn selection(&self) -> Vec<PathBuf> {
        let p = &self.panes[self.active];
        let visible = p.visible(self.show_hidden);
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
                    if let Some(i) = p.visible(self.show_hidden).iter().position(|e| e.path == created) {
                        self.panes[self.active].cursor = i;
                    }
                }
                Err(e) => self.dialog = Some(Dialog::NewDir { name, error: Some(e.to_string()) }),
            },
            Some(Dialog::Running { kind, job }) => self.dialog = Some(Dialog::Running { kind, job }),
            Some(Dialog::Report { .. }) | None => {}
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

    /// Liest den Ordner neu ein und behält den Cursor so gut es geht.
    fn reload(&mut self, side: usize) {
        let p = &mut self.panes[side];
        if let Ok(entries) = guppy_core::read_dir(&p.path) {
            p.entries = entries;
            p.marked.retain(|m| m.exists());
            let len = p.visible(self.show_hidden).len();
            p.cursor = p.cursor.min(len.saturating_sub(1));
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
        let p = &mut self.panes[self.active];
        let old = p.path.clone();
        p.load(path);
        if p.path != old {
            p.back.push(old);
            p.forward.clear();
            self.jumper.visit(&self.panes[self.active].path);
            self.request_sizes(self.active);
        }
    }

    fn history(&mut self, back: bool) {
        let p = &mut self.panes[self.active];
        let target = if back { p.back.pop() } else { p.forward.pop() };
        if let Some(t) = target {
            let old = p.path.clone();
            p.load(t);
            if back { p.forward.push(old) } else { p.back.push(old) }
            self.request_sizes(self.active);
        }
    }

    fn go_parent(&mut self) {
        let p = &self.panes[self.active];
        let Some(parent) = p.path.parent().map(Path::to_path_buf) else { return };
        let came_from = p.path.clone();
        self.navigate(parent);
        // Cursor auf den Ordner setzen, aus dem wir kamen.
        let p = &self.panes[self.active];
        if let Some(i) = p.visible(self.show_hidden).iter().position(|e| e.path == came_from) {
            self.panes[self.active].cursor = i;
        }
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
        let len = p.visible(self.show_hidden).len();
        p.cursor = p.cursor.min(len.saturating_sub(1));
    }

    /// Vorschau erst laden, wenn der Cursor kurz ruht; beim Dauerscrollen kein Dateizugriff.
    fn schedule_preview(&mut self) {
        let same = match (self.current(), &self.preview) {
            (Some(e), Some((p, _))) => e.path == *p,
            _ => false,
        };
        if !same {
            self.preview_due = Some(std::time::Instant::now() + Duration::from_millis(90));
        }
    }

    fn refresh_preview(&mut self) {
        let Some(e) = self.current() else {
            self.preview = None;
            return;
        };
        if self.preview.as_ref().is_some_and(|(p, _)| *p == e.path) {
            return;
        }
        let body = if e.is_dir() {
            match guppy_core::read_dir(&e.path) {
                Ok(list) => list.iter().filter(|c| !c.hidden).take(40)
                    .map(|c| if c.is_dir() { format!("{}/", c.name) } else { c.name.clone() })
                    .collect::<Vec<_>>().join("\n"),
                Err(_) => "Kein Zugriff".into(),
            }
        } else {
            read_head(&e.path).unwrap_or_else(|| format!("{} · keine Textvorschau", human_size(e.size)))
        };
        self.preview = Some((e.path.clone(), body));
    }

    /// Hält den Cursor sichtbar, mit zwei Zeilen Abstand zum Rand; gescrollt wird weich.
    fn scroll_into_view(&mut self) -> Task<Message> {
        if self.jumping() {
            return Task::none();
        }
        const MARGIN: f32 = 2.0 * ROW_H;
        let p = &mut self.panes[self.active];
        // Gegen das laufende Ziel rechnen, sonst springt es bei Tastenwiederholung zurück.
        let base = p.scroll_target.unwrap_or(p.scroll_y);
        let top = p.cursor as f32 * ROW_H;
        let margin = MARGIN.min(((p.view_h - ROW_H) / 2.0).max(0.0));
        let y = if top - margin < base {
            (top - margin).max(0.0)
        } else if top + ROW_H + margin > base + p.view_h {
            top + ROW_H + margin - p.view_h
        } else {
            return Task::none();
        };
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
        let mut body = row![self.pane_view(0), self.pane_view(1)].spacing(22);
        if self.preview_visible() {
            body = body.push(self.preview_view());
        }
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
            .width(480)
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
            self.icon_button(icons::PREVIEW, "Vorschau ein/aus", Some(Message::TogglePreview)),
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
        let list = p.visible(self.show_hidden);

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
                (true, Some(DirSize::Partial(n))) => (format!("… {}", human_size(n)), true),
                (true, None) => ("…".to_string(), true),
            };
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
            text(p.path.display().to_string())
                .size(12)
                .color(if active { rgb(t.accent) } else { rgb(t.foreground) })
                .wrapping(iced::widget::text::Wrapping::None),
        )
        .padding([10, 10])
        .width(Length::Fill)
        .style(move |_| container::Style {
            border: Border { color: rgb(t.muted), width: 0.0, radius: 0.0.into() },
            ..Default::default()
        });

        let list = scrollable(column(rows).padding([0, 6]))
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

    fn preview_view(&self) -> Element<'_, Message> {
        let t = &self.theme;
        let content: Element<'_, Message> = match (self.current(), &self.preview) {
            (Some(e), Some((path, body))) => {
                let body = if *path == e.path { body.as_str() } else { "" };
                let kind = match e.class() {
                    FileClass::Dir => "ORDNER",
                    FileClass::Code => "CODE",
                    FileClass::Doc => "TEXT",
                    FileClass::Image => "BILD",
                    FileClass::Archive => "ARCHIV",
                    FileClass::Other => "DATEI",
                };
                column![
                    text(format!("VORSCHAU · {kind}")).size(11).color(rgb(t.foreground)),
                    text(&e.name).size(17).font(BOLD).color(rgb(t.bright)),
                    scrollable(text(body).size(12).color(rgb(t.foreground)).line_height(1.6)).height(Length::Fill),
                ]
                .spacing(12)
                .into()
            }
            _ => text("Leer").color(rgb(t.foreground)).into(),
        };
        self.sheet(container(content).padding(16).into(), &self.papers[2], 0x535047, None)
            .width(Length::FillPortion(2))
            .into()
    }

    fn statusbar(&self) -> Element<'_, Message> {
        let t = &self.theme;
        let p = &self.panes[self.active];
        let visible = p.visible(self.show_hidden);
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

/// Erste 16 KB einer Datei, falls es Text ist.
fn read_head(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut buf = vec![0; 16 * 1024];
    let n = std::fs::File::open(path).ok()?.read(&mut buf).ok()?;
    buf.truncate(n);
    if buf.contains(&0) {
        return None;
    }
    let s = match String::from_utf8(buf) {
        Ok(s) => s,
        // Am Pufferende abgeschnittenes UTF-8-Zeichen verwerfen.
        Err(e) => String::from_utf8_lossy(&e.as_bytes()[..e.utf8_error().valid_up_to()]).into_owned(),
    };
    Some(s.lines().take(200).collect::<Vec<_>>().join("\n"))
}

fn save_ppm(path: &std::ffi::OsStr, shot: &iced::window::Screenshot) {
    let mut out = format!("P6 {} {} 255\n", shot.size.width, shot.size.height).into_bytes();
    out.extend(shot.rgba.chunks_exact(4).flat_map(|px| [px[0], px[1], px[2]]));
    let _ = std::fs::write(path, out);
}
