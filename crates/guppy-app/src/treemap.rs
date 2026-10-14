//! Space map: the current folder as tiles sized by what they hold (squarified treemap).

use std::path::PathBuf;

use iced::widget::canvas::{self, Frame, Geometry, Text};
use iced::{Color, Point, Rectangle, Renderer, Size, Theme, mouse};

use crate::Message;

pub struct Tile {
    /// `None` for the tile that sums up the smallest entries.
    pub path: Option<PathBuf>,
    pub name: String,
    pub size: u64,
    pub color: Color,
}

pub struct Map {
    /// Largest first.
    pub tiles: Vec<Tile>,
    /// Tile of the active pane's cursor.
    pub cursor: Option<PathBuf>,
    pub text: Color,
    pub gap: Color,
}

/// Rectangles for `values` (largest first) filling `r`, as square as possible
/// (Bruls, Huizing, van Wijk: "Squarified Treemaps").
pub fn squarify(values: &[f64], r: Rectangle) -> Vec<Rectangle> {
    let total: f64 = values.iter().sum();
    if total <= 0.0 || r.width <= 0.0 || r.height <= 0.0 {
        return vec![Rectangle::default(); values.len()];
    }
    let scale = (r.width * r.height) as f64 / total;
    let areas: Vec<f64> = values.iter().map(|v| v * scale).collect();
    let worst = |row: &[f64], side: f64| {
        let s: f64 = row.iter().sum();
        let (max, min) = row.iter().fold((0.0f64, f64::MAX), |(a, b), &v| (a.max(v), b.min(v)));
        if min <= 0.0 || s <= 0.0 {
            return f64::MAX;
        }
        (side * side * max / (s * s)).max(s * s / (side * side * min))
    };
    let mut out = Vec::with_capacity(values.len());
    let (mut x, mut y, mut w, mut h) = (r.x as f64, r.y as f64, r.width as f64, r.height as f64);
    let mut i = 0;
    while i < areas.len() {
        let side = w.min(h);
        let mut j = i + 1;
        while j < areas.len() && worst(&areas[i..=j], side) <= worst(&areas[i..j], side) {
            j += 1;
        }
        let row = &areas[i..j];
        let s: f64 = row.iter().sum();
        if w >= h {
            // A column on the left.
            let cw = if h > 0.0 { s / h } else { 0.0 };
            let mut cy = y;
            for a in row {
                let ch = if cw > 0.0 { a / cw } else { 0.0 };
                out.push(Rectangle::new(Point::new(x as f32, cy as f32), Size::new(cw as f32, ch as f32)));
                cy += ch;
            }
            x += cw;
            w -= cw;
        } else {
            // A row on top.
            let rh = if w > 0.0 { s / w } else { 0.0 };
            let mut cx = x;
            for a in row {
                let rw = if rh > 0.0 { a / rh } else { 0.0 };
                out.push(Rectangle::new(Point::new(cx as f32, y as f32), Size::new(rw as f32, rh as f32)));
                cx += rw;
            }
            y += rh;
            h -= rh;
        }
        i = j;
    }
    out
}

impl Map {
    fn rects(&self, bounds: Rectangle) -> Vec<Rectangle> {
        let values: Vec<f64> = self.tiles.iter().map(|t| t.size as f64).collect();
        squarify(&values, Rectangle::new(Point::ORIGIN, bounds.size()))
    }

    fn hit(&self, bounds: Rectangle, at: Point) -> Option<usize> {
        self.rects(bounds).iter().position(|r| r.contains(at))
    }
}

/// Text cut to what fits in `width` (monospace, about 0.6 em per character).
fn fit(s: &str, width: f32, size: f32) -> Option<String> {
    let max = (width / (size * 0.6)) as usize;
    let n = s.chars().count();
    match max {
        0..=2 => None,
        _ if n <= max => Some(s.to_string()),
        _ => Some(s.chars().take(max - 1).chain(std::iter::once('…')).collect()),
    }
}

impl canvas::Program<Message> for Map {
    /// Tile under the pointer.
    type State = Option<usize>;

    fn update(&self, hover: &mut Option<usize>, event: &canvas::Event, bounds: Rectangle, cursor: mouse::Cursor) -> Option<canvas::Action<Message>> {
        let at = cursor.position_in(bounds);
        match event {
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. } | mouse::Event::CursorLeft) => {
                let now = at.and_then(|p| self.hit(bounds, p));
                (now != *hover).then(|| {
                    *hover = now;
                    canvas::Action::request_redraw()
                })
            }
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let path = at.and_then(|p| self.hit(bounds, p)).and_then(|i| self.tiles[i].path.clone())?;
                Some(canvas::Action::publish(Message::MapClick(path)).and_capture())
            }
            _ => None,
        }
    }

    fn draw(&self, hover: &Option<usize>, renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        for (i, (t, r)) in self.tiles.iter().zip(self.rects(bounds)).enumerate() {
            if r.width < 1.0 || r.height < 1.0 {
                continue;
            }
            let on = *hover == Some(i);
            let current = t.path.is_some() && t.path == self.cursor;
            let fill = Color { a: if on { 0.55 } else if current { 0.45 } else { 0.28 }, ..t.color };
            // A 2 px gap between tiles in the background color.
            let inner = Rectangle::new(Point::new(r.x + 1.0, r.y + 1.0), Size::new((r.width - 2.0).max(0.0), (r.height - 2.0).max(0.0)));
            frame.fill_rectangle(r.position(), r.size(), self.gap);
            frame.fill_rectangle(inner.position(), inner.size(), fill);
            if current {
                frame.stroke_rectangle(inner.position(), inner.size(), canvas::Stroke::default().with_color(self.text).with_width(1.5));
            }
            if inner.height >= 18.0
                && let Some(name) = fit(&t.name, inner.width - 10.0, 12.0)
            {
                frame.fill_text(Text { content: name, position: Point::new(inner.x + 5.0, inner.y + 4.0), color: self.text, size: 12.0.into(), font: crate::BOLD, ..Text::default() });
                if inner.height >= 34.0
                    && let Some(size) = fit(&guppy_core::fs::human_size(t.size), inner.width - 10.0, 11.0)
                {
                    frame.fill_text(Text { content: size, position: Point::new(inner.x + 5.0, inner.y + 19.0), color: Color { a: 0.75, ..self.text }, size: 11.0.into(), font: crate::FONT, ..Text::default() });
                }
            }
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(&self, hover: &Option<usize>, _: Rectangle, _: mouse::Cursor) -> mouse::Interaction {
        match hover {
            Some(i) if self.tiles[*i].path.is_some() => mouse::Interaction::Pointer,
            _ => mouse::Interaction::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_fill_the_area() {
        let r = Rectangle::new(Point::ORIGIN, Size::new(600.0, 400.0));
        let rects = squarify(&[6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0], r);
        let area: f32 = rects.iter().map(|r| r.width * r.height).sum();
        assert!((area - 240_000.0).abs() < 1.0);
        // Proportional: the first tile holds 6/24 of the area.
        assert!((rects[0].width * rects[0].height - 60_000.0).abs() < 1.0);
        assert!(rects.iter().all(|t| t.x >= 0.0 && t.y >= 0.0 && t.x + t.width <= 600.01 && t.y + t.height <= 400.01));
    }
}
