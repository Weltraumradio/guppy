//! Zeigt Sprungziele: `cargo run --release --example jump -- <suche> [ordner]`
fn main() {
    let mut args = std::env::args().skip(1);
    let q = args.next().unwrap_or_default();
    let j = guppy_core::Jumper::load();
    if let Some(root) = args.next() {
        j.index_nearby(std::path::Path::new(&root));
        while j.indexing() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    for p in j.search(&q, 14) {
        println!("{}", j.display(&p));
    }
}
