mod icons;
mod paper;

use paper::Paper;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use guppy_core::fs::human_size;
use guppy_core::{Entry, FileClass};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::operation::{self, AbsoluteOffset};
use iced::widget::{
    Id, button, column, container, mouse_area, row, scrollable, svg, text, text_input,
};
use iced::{Background, Border, Color, Element, Font, Length, Subscription, Task};

const ROW_H: f32 = 26.0;
const SHEET_RADIUS: f32 = 6.0;
const FONT: Font = Font::with_name("JetBrainsMono Nerd Font");
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
    Scrolled(usize, scrollable::Viewport),
    Filter(String),
    Back,
    Forward,
    Parent,
    Home,
    ToggleMark,
    SwitchSide,
    ToggleHidden,
    TogglePreview,
    /// Entwickler-Hilfe: `GUPPY_SCREENSHOT=datei.ppm` speichert das erste Bild und beendet.
    Shot(iced::window::Screenshot),
}

struct Pane {
    path: PathBuf,
    entries: Vec<Entry>,
    /// Index in der *sichtbaren* Liste (nach Filter/versteckten Dateien).
    cursor: usize,
    marked: HashSet<PathBuf>,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    scroll_y: f32,
    view_h: f32,
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
            scroll_y: 0.0,
            view_h: 400.0,
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
        }
    }

    fn visible(&self, filter: &str, hidden: bool) -> Vec<&Entry> {
        let f = filter.to_lowercase();
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
    filter: String,
    show_hidden: bool,
    show_preview: bool,
    preview: Option<(PathBuf, String)>,
    papers: [Paper; 3],
}

impl Guppy {
    fn new() -> (Self, Task<Message>) {
        let home = dirs_home();
        let cwd = std::env::current_dir().unwrap_or_else(|_| home.clone());
        let mut g = Self {
            theme: guppy_core::Theme::load(),
            panes: [Pane::new(cwd, "pane-left"), Pane::new(home, "pane-right")],
            active: 0,
            filter: String::new(),
            show_hidden: false,
            show_preview: true,
            preview: None,
            papers: [Paper::new(1), Paper::new(2), Paper::new(3)],
        };
        g.refresh_preview();
        let task = if std::env::var_os("GUPPY_SCREENSHOT").is_some() {
            iced::window::latest()
                .and_then(iced::window::screenshot)
                .map(Message::Shot)
        } else {
            Task::none()
        };
        (g, task)
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
        keyboard::listen().map(Message::Key)
    }

    fn current(&self) -> Option<&Entry> {
        let p = &self.panes[self.active];
        p.visible(&self.filter, self.show_hidden).get(p.cursor).copied()
    }

    fn update(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                return self.on_key(key, modifiers);
            }
            Message::Key(_) => {}
            Message::Select(side, i) => {
                self.active = side;
                self.panes[side].cursor = i;
            }
            Message::Open(side, i) => {
                self.active = side;
                self.panes[side].cursor = i;
                self.open_current();
            }
            Message::Scrolled(side, vp) => {
                self.panes[side].scroll_y = vp.absolute_offset().y;
                self.panes[side].view_h = vp.bounds().height;
                return Task::none();
            }
            Message::Filter(f) => {
                self.filter = f;
                self.panes[self.active].cursor = 0;
            }
            Message::Back => self.history(true),
            Message::Forward => self.history(false),
            Message::Parent => self.go_parent(),
            Message::Home => self.navigate(dirs_home()),
            Message::ToggleMark => self.toggle_mark(),
            Message::SwitchSide => self.active = 1 - self.active,
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
        self.refresh_preview();
        self.scroll_into_view()
    }

    fn on_key(&mut self, key: Key, m: keyboard::Modifiers) -> Task<Message> {
        let len = {
            let p = &self.panes[self.active];
            p.visible(&self.filter, self.show_hidden).len()
        };
        let page = (self.panes[self.active].view_h / ROW_H).max(1.0) as usize;
        let cur = self.panes[self.active].cursor;
        let mut move_to = |i: usize| self.panes[self.active].cursor = i.min(len.saturating_sub(1));

        match key.as_ref() {
            Key::Named(Named::ArrowDown) => move_to(cur + 1),
            Key::Named(Named::ArrowUp) => move_to(cur.saturating_sub(1)),
            Key::Named(Named::PageDown) => move_to(cur + page),
            Key::Named(Named::PageUp) => move_to(cur.saturating_sub(page)),
            Key::Named(Named::Home) => move_to(0),
            Key::Named(Named::End) => move_to(usize::MAX),
            Key::Named(Named::Enter) => self.open_current(),
            Key::Named(Named::Backspace) => self.go_parent(),
            Key::Named(Named::Tab) => self.active = 1 - self.active,
            Key::Named(Named::Insert | Named::Space) => self.toggle_mark(),
            Key::Named(Named::Escape) => {
                self.filter.clear();
            }
            Key::Named(Named::ArrowLeft) if m.alt() => self.history(true),
            Key::Named(Named::ArrowRight) if m.alt() => self.history(false),
            Key::Character("/") => return operation::focus(Id::new("filter")),
            Key::Character("h") if m.control() => return self.update(Message::ToggleHidden),
            _ => return Task::none(),
        }
        self.refresh_preview();
        self.scroll_into_view()
    }

    fn navigate(&mut self, path: PathBuf) {
        let p = &mut self.panes[self.active];
        let old = p.path.clone();
        p.load(path);
        if p.path != old {
            p.back.push(old);
            p.forward.clear();
            self.filter.clear();
        }
    }

    fn history(&mut self, back: bool) {
        let p = &mut self.panes[self.active];
        let target = if back { p.back.pop() } else { p.forward.pop() };
        if let Some(t) = target {
            let old = p.path.clone();
            p.load(t);
            if back { p.forward.push(old) } else { p.back.push(old) }
        }
    }

    fn go_parent(&mut self) {
        let p = &self.panes[self.active];
        let Some(parent) = p.path.parent().map(Path::to_path_buf) else { return };
        let came_from = p.path.clone();
        self.navigate(parent);
        // Cursor auf den Ordner setzen, aus dem wir kamen.
        let p = &self.panes[self.active];
        if let Some(i) = p.visible(&self.filter, self.show_hidden).iter().position(|e| e.path == came_from) {
            self.panes[self.active].cursor = i;
        }
    }

    fn open_current(&mut self) {
        let Some(e) = self.current() else { return };
        if e.is_dir() {
            let path = e.path.clone();
            self.navigate(path);
        } else {
            let _ = std::process::Command::new("xdg-open").arg(&e.path).spawn();
        }
    }

    fn toggle_mark(&mut self) {
        let Some(path) = self.current().map(|e| e.path.clone()) else { return };
        let p = &mut self.panes[self.active];
        if !p.marked.remove(&path) {
            p.marked.insert(path);
        }
        p.cursor += 1;
        let len = p.visible(&self.filter, self.show_hidden).len();
        p.cursor = p.cursor.min(len.saturating_sub(1));
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

    fn scroll_into_view(&self) -> Task<Message> {
        let p = &self.panes[self.active];
        let top = p.cursor as f32 * ROW_H;
        let y = if top < p.scroll_y {
            top
        } else if top + ROW_H > p.scroll_y + p.view_h {
            top + ROW_H - p.view_h
        } else {
            return Task::none();
        };
        operation::scroll_to(p.id.clone(), AbsoluteOffset { x: None, y: Some(y) })
    }

    // ---------- Ansicht ----------

    fn view(&self) -> Element<'_, Message> {
        let t = &self.theme;
        let mut body = row![self.pane_view(0), self.pane_view(1)].spacing(22);
        if self.show_preview {
            body = body.push(self.preview_view());
        }
        let desk = container(body)
            .padding([16, 18])
            .height(Length::Fill)
            .style(move |_| container::Style {
                background: Some(rgb(t.dark_background).into()),
                ..Default::default()
            });
        column![self.menubar(), desk, self.statusbar()].into()
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
            self.icon_button(icons::NEW_DIR, "Neuer Ordner (F7) – folgt", None),
            self.icon_button(icons::COPY, "Kopieren (F5) – folgt", None),
            self.icon_button(icons::MOVE, "Verschieben (F6) – folgt", None),
            self.icon_button(icons::DELETE, "Löschen (F8) – folgt", None),
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
            .padding([6, 10])
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
        let list = p.visible(&self.filter, self.show_hidden);

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
            let size = if e.is_dir() { "DIR".to_string() } else { human_size(e.size) };
            let line = row![
                text(dot).color(dot_color).size(12).width(14),
                container(
                    text(name).color(fg).size(13).font(if e.is_dir() { BOLD } else { FONT })
                        .wrapping(iced::widget::text::Wrapping::None),
                )
                .width(Length::Fill)
                .clip(true),
                text(size).color(if on_cursor { fg } else { rgb(t.foreground) }).size(12),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center);
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

        let tone = if active { 0x4a463f } else { 0x423e38 };
        self.sheet(column![header, list].into(), &self.papers[side], tone, active.then_some(rgb(t.accent)))
            .width(Length::FillPortion(3))
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
            (Some(e), Some((_, body))) => {
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
        self.sheet(container(content).padding(16).into(), &self.papers[2], 0x46423b, None)
            .width(Length::FillPortion(2))
            .into()
    }

    fn statusbar(&self) -> Element<'_, Message> {
        let t = &self.theme;
        let p = &self.panes[self.active];
        let count = p.visible(&self.filter, self.show_hidden).len();
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
        let input = text_input("Filtern …", &self.filter)
            .id(Id::new("filter"))
            .on_input(Message::Filter)
            .size(12)
            .padding(0)
            .style(move |_, _| text_input::Style {
                background: Color::TRANSPARENT.into(),
                border: Border::default(),
                icon: dim,
                placeholder: rgb(0x555555),
                value: rgb(t.bright),
                selection: rgb(t.selection),
            });
        let search = container(row![search_icon, input, text("/").size(11).color(rgb(0x555555))].spacing(8).align_y(iced::Alignment::Center))
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
            text(cur).size(12).color(rgb(t.foreground)),
            bar_sep(),
            text(format!("{count} Einträge")).size(12).color(dim),
            bar_sep(),
            text(if marked > 0 { format!("{marked} markiert") } else { "nichts markiert".into() })
                .size(12)
                .color(if marked > 0 { rgb(t.marked) } else { dim }),
        ]
        .spacing(14)
        .align_y(iced::Alignment::Center);

        container(bar)
            .padding([6, 14])
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
