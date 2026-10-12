//! Misst die Ordnergrößen-Berechnung: `cargo run --release --example du -- <ordner>`
use guppy_core::{Cancel, DirSize, DirSizes};
use std::time::Instant;

fn main() {
    let dir = std::env::args().nth(1).map(Into::into).unwrap_or_else(|| std::env::current_dir().unwrap());
    let sizes = DirSizes::default();
    let t = Instant::now();
    sizes.request(vec![dir.clone()], Cancel::default());
    while sizes.busy() {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let Some(DirSize::Done(n)) = sizes.get(&dir) else { panic!("keine Größe") };
    println!("{}: {} in {:?}", dir.display(), guppy_core::fs::human_size(n), t.elapsed());
}
