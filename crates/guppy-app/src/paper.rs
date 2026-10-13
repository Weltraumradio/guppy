//! Pane textures, drawn once and cached: paper grain, or neon scanlines.

use iced::widget::canvas::{self, Cache, Frame, Geometry};
use iced::{Color, Point, Rectangle, Renderer, Size, Theme, mouse};

pub struct Paper {
    cache: Cache,
    seed: u32,
}

impl Paper {
    pub fn new(seed: u32) -> Self {
        Self { cache: Cache::new(), seed }
    }
}

impl<Message> canvas::Program<Message> for Paper {
    type State = ();

    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        // The cache only redraws when the size changes.
        vec![self.cache.draw(renderer, bounds.size(), |frame: &mut Frame| grain(frame, self.seed))]
    }
}

/// Random light and dark fibers, deterministic per sheet.
fn grain(frame: &mut Frame, seed: u32) {
    let mut s = seed.wrapping_mul(2654435761).max(1);
    let mut rnd = || {
        // xorshift32
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        s as f32 / u32::MAX as f32
    };
    let Size { width, height } = frame.size();
    let count = (width * height / 6.0) as usize;
    for _ in 0..count {
        let (x, y, a, light, long) = (rnd() * width, rnd() * height, rnd(), rnd() > 0.5, rnd() > 0.985);
        let color = if light {
            Color { a: 0.012 + a * 0.025, ..Color::WHITE }
        } else {
            Color { a: 0.02 + a * 0.045, ..Color::BLACK }
        };
        // Occasional longer horizontal fibers.
        let w = if long { 2.0 + a * 2.0 } else { 1.0 };
        frame.fill_rectangle(Point::new(x, y), Size::new(w, 1.0), color);
    }
}

/// Neon look: faint horizontal scanlines, like an old CRT terminal.
#[derive(Default)]
pub struct Scanlines {
    cache: Cache,
}

impl<Message> canvas::Program<Message> for Scanlines {
    type State = ();

    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        vec![self.cache.draw(renderer, bounds.size(), |frame: &mut Frame| {
            let Size { width, height } = frame.size();
            let line = Color { r: 0.0, g: 0.94, b: 1.0, a: 0.025 };
            let mut y = 0.0;
            while y < height {
                frame.fill_rectangle(Point::new(0.0, y), Size::new(width, 1.0), line);
                y += 3.0;
            }
        })]
    }
}
