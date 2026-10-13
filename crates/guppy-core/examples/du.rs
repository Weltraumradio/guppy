//! Benchmarks the directory size computation: `cargo run --release --example du -- <dir>`
use guppy_core::{Cancel, DirSize, DirSizes};
use std::time::Instant;

fn main() {
    let dir = std::env::args().nth(1).map(Into::into).unwrap_or_else(|| std::env::current_dir().unwrap());
    let sizes = match std::env::var_os("GUPPY_SAVE") {
        Some(_) => DirSizes::default_cache_file().map(DirSizes::with_cache).unwrap_or_default(),
        None => DirSizes::default(),
    };
    let t = Instant::now();
    sizes.request(vec![dir.clone()], Cancel::default());
    while sizes.busy() {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let Some(DirSize::Done(n)) = sizes.get(&dir) else { panic!("no size") };
    println!("{}: {} in {:?}", dir.display(), guppy_core::fs::human_size(n), t.elapsed());
    // `GUPPY_SAVE=1` fills the app's size cache, e.g. for startup measurements.
    if std::env::var_os("GUPPY_SAVE").is_some() {
        sizes.save().unwrap();
    }
}
