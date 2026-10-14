# Guppy

A modern, lightweight two-pane file manager for Linux in the spirit of Norton Commander,
written in Rust with [iced](https://iced.rs). Colors follow the active Omarchy theme
("paper" look); "matte-black" and a "neon" terminal look are built in.

```
cargo build --release
target/release/guppy-app
```

Press F1 for all keys, Ctrl+P for the command palette. Settings live in
`~/.config/guppy/config.toml` (written with comments on first start; F12 edits the common ones).

## Helper programs

Guppy needs nothing else to run; zip and tar archives (also .gz, .bz2, .xz, .zst) are read by
Guppy itself. A few extras use programs when they are installed: poppler (PDF preview), ffmpeg
(video and audio), ImageMagick (HEIC, AVIF, RAW, PSD …), bsdtar (7z, rar, iso), git (status
in the list) and zoxide (jump to folders used in the shell). Settings (F12) → *Helper programs*
shows which are there; Enter on a missing one installs it in a terminal (pacman, apt, Homebrew
or winget) or opens its download page. Previews say which program they are missing.

## Windows and macOS

Guppy builds for Windows and macOS too (`cargo build --release` on that system), but it is
only tested on Linux so far. Differences there:

- **macOS:** shortcuts use Cmd instead of Ctrl (also in F1 and the palette). The clipboard
  holds file URLs like Finder; Cmd+X only moves when pasted in Guppy. *Empty trash* asks Finder
  (macOS asks once for permission); undo of trashing works for items trashed in the same session.
  User actions with `terminal = true` open Terminal.app.
- **Windows:** 7z, rar and iso archives need Windows 10 or newer (`tar.exe`). Alt+Enter only switches read-only
  (the `w` bits); there is no owner. User actions run with `cmd /C`, placeholders are quoted
  with `"`. Creating symlinks needs developer mode or admin rights.
- **Both:** helper programs as above, installed with winget or Homebrew (on macOS Homebrew’s
  folders are searched even when Guppy starts from the Dock); colors fall back to Matte Black
  (no Omarchy theme).

## Benchmark

Measured on 2026-10-13 with `GUPPY_BENCH=1 target/release/guppy-app`, which prints the time
from process start to the first frame, then memory after three idle seconds, and exits.
Release build, AMD Ryzen 9 6900HX (Radeon 680M), Arch Linux / Hyprland (Wayland),
window 1200×760, both panes on real folders (`~/guppy` and `/mnt`). Typical of six runs.

| | Guppy | Nautilus 50.3 |
|---|---|---|
| Binary size | 15 MB (one file) | — |
| First frame | 0.37 s | — |
| RSS | 97 MB | 131 MB |
| PSS (shared pages split) | 70 MB | 70 MB |
| Private dirty | 55 MB | 46 MB |

The size cache held ~70 000 folders (`cargo run --release --example du` with
`GUPPY_SAVE=1` fills it for the whole home folder).

History of this table: the first measurement (same day) was 0.49 s / 114 MB PSS. Since then:

- wgpu uses only Vulkan when a Vulkan driver is installed; before, it started Vulkan *and*
  OpenGL to choose an adapter, loading both drivers (`WGPU_BACKEND` still overrides this).
- The folder-size cache keeps 64-bit path hashes instead of paths and is stored in a compact
  binary file (1.7 MB instead of 11 MB of text), read in a few milliseconds.
- zoxide is queried in the background instead of before the first frame.

Notes:

- Most of the remaining time and memory is the renderer: Vulkan/Mesa driver state, the GPU
  font atlas, and the compositor mapping the window. Guppy's own startup work is under 10 ms.
- The software renderer (`ICED_BACKEND=tiny-skia`) saves no memory, because it keeps
  full-size frame buffers.
- Nautilus shares more pages with the GNOME stack already in memory; its time to first
  window was not measured.

Reproduce:

```
for i in 1 2 3 4 5 6; do GUPPY_BENCH=1 target/release/guppy-app; done
```
