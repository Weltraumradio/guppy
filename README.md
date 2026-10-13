# Guppy

A modern, lightweight two-pane file manager for Linux in the spirit of Norton Commander,
written in Rust with [iced](https://iced.rs). Colors follow the active Omarchy theme
("paper" look); a "neon" terminal look is built in.

```
cargo build --release
target/release/guppy-app
```

Press F1 for all keys, Ctrl+P for the command palette. Settings live in
`~/.config/guppy/config.toml` (written with comments on first start; F12 edits the common ones).

## Benchmark

Measured on 2026-10-13 with `GUPPY_BENCH=1 target/release/guppy-app`, which prints the time
from process start to the first frame, then memory after three idle seconds, and exits.
Release build, AMD Ryzen 9 6900HX (Radeon 680M), Arch Linux / Hyprland (Wayland),
window 1200×760, both panes on real folders (`~/guppy` and `/mnt`). Median of six runs.

| | Guppy (wgpu, default) | Guppy (`ICED_BACKEND=tiny-skia`) | Nautilus 50.3 |
|---|---|---|---|
| Binary size | 15 MB (one static file) | — | — |
| First frame | 0.49 s | 0.46 s | — |
| RSS | 182 MB | 184 MB | 131 MB |
| PSS (shared pages split) | 114 MB | 176 MB | 70 MB |
| Private dirty | 104 MB | 162 MB | 46 MB |

Notes:

- Most of the memory is the renderer, not Guppy's data: with an empty home folder
  (no jump index, no size cache) PSS is still ~100 MB. Vulkan/Mesa driver state and the
  GPU font atlas dominate.
- The software renderer (tiny-skia) saves no memory, because it keeps full-size frame buffers.
- Nautilus shares more pages with the rest of the GNOME stack already in memory, which lowers
  its PSS; its time to first window was not measured.

Reproduce:

```
for i in 1 2 3 4 5 6; do GUPPY_BENCH=1 target/release/guppy-app; done
```
