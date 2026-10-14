# Guppy

A two-pane file manager in the tradition of Norton Commander and Total Commander, written in
Rust with [iced](https://iced.rs). It starts in about a third of a second, stays out of your
way, and is meant to be driven from the keyboard: F5 copies, F6 moves, F7 makes a folder, and
typing filters the list.

I built it for my own Arch/Hyprland setup ([Omarchy](https://omarchy.org)), so by default it
picks up the colors of the active Omarchy theme. It also builds on Windows and macOS.

![Guppy with the Matte Black look](assets/screenshots/matte-black.png)

<p>
  <img src="assets/screenshots/neon.png" width="49%" alt="Neon look">
  <img src="assets/screenshots/paper-aetheria.png" width="49%" alt="Paper look with the Aetheria theme">
</p>
<sub>Left: the <code>neon</code> look. Right: <code>paper</code> following the Omarchy theme Aetheria.</sub>

## What it does

### Folder sizes, right in the list

Every folder shows its real size, not just files. Guppy walks the folders in the background on
all CPU cores while you keep working, and the numbers fill in as they come (`… 1.2 GB` while
counting). Results are cached on disk (`~/.cache/guppy/sizes.bin`), so the next time you open
a folder the sizes are there at once (shown as `≈ 3.4 GB` until they've been checked again).
When something changes, only the affected folders are counted again.

The path line above each pane shows the size of every folder on the way, from `~` down to
where you are, and each part is a link back up.

Sort by size (Ctrl+S) and each row gets a small bar showing its share, which makes it easy to
see what is eating your disk. Hard links are counted once, other drives mounted below a folder
are left out, and network drives get fewer parallel requests so the NAS isn't flooded. Ctrl+R
counts everything again.

### Moving around

- Two panes, each with tabs (Ctrl+T, Ctrl+W, Ctrl+Tab, Ctrl+1…9) that come back at the next start, and its own history (Alt+← / Alt+→)
- Start typing to filter the list; `~` or `/` switches to fuzzy jumping across folders you've
  visited (and those zoxide knows about, if you use it)
- Drives, mounted media, network shares and your bookmarks on Alt+F1 / Alt+F2; Ctrl+D
  bookmarks the current folder
- Command palette (Ctrl+P) with every action, including your own
- The lists update by themselves when files change on disk

### Working with files

- F5 copies, F6 moves, F8 trashes, Shift+F8 deletes. Everything runs as a background job with
  progress in the status bar, so you can keep browsing
- Several jobs queue up; in the jobs window (Ctrl+J) you can pause, resume, reorder or cancel
  them. Large files are copied in chunks, so pausing takes effect right away
- When a name already exists, you choose: skip, overwrite or keep both, once or for all
- Ctrl+Z undoes the last move, rename or trash
- Drag files with the mouse onto the other pane or onto a folder (Ctrl copies)
- Mark with Insert or Space, by pattern with `+` / `-` (e.g. `*.jpg`), invert with `*`
- Ctrl+C / Ctrl+X / Ctrl+V use the system clipboard, so you can paste into Nautilus or from it
- Rename in place (F2), new folder (F7), symlinks (Ctrl+Shift+F5), permissions and owner,
  also recursively (Alt+Enter)

### Looking inside

- Quick view (Ctrl+Q) turns the other pane into a preview that follows the cursor: text and
  code, images, the first page of a PDF, a frame from a video, tags and cover art of music,
  the text of Word, Excel, PowerPoint and LibreOffice files
- F3 opens a full viewer, F4 a small built-in editor (Shift+F4 your own editor)
- Zip and tar archives (also .gz, .bz2, .xz, .zst) open like folders, and you can copy files
  out of them; 7z, rar and iso work too when bsdtar is installed
- Git repositories show the branch in the pane header and a mark next to changed, new and
  ignored files

### Making it yours

- Your own commands on keys, with placeholders for the current file, the marked files and
  both folders, optionally run in a terminal
- Three looks: `paper` follows the colors of your Omarchy theme and changes with it,
  `matte-black` keeps those colors everywhere, and `neon` is a dark terminal style
- Settings (F12) for the everyday options, the rest in a commented config file
- File names colored by type (code, documents, images, archives)

## Building

You need a recent Rust toolchain (edition 2024).

```
cargo build --release
target/release/guppy-app
```

That gives you a single binary of about 15 MB; nothing else needs to be installed next to it.

## Keys

F1 shows all of them inside the app. The ones you'll use most:

| Key | |
|---|---|
| Tab | switch pane |
| Enter / Backspace | open / go up |
| Insert or Space | mark |
| F3 / F4 | view / edit |
| F5 / F6 | copy / move to the other pane |
| F7 | new folder |
| F8 or Del | move to trash (Shift: delete for good) |
| Ctrl+Z | undo |
| Ctrl+Q | quick view |
| Ctrl+P | command palette |
| Alt+F1 / Alt+F2 | drives and bookmarks |
| F12 | settings |

On a Mac it's Cmd instead of Ctrl.

## Settings

F12 changes the common ones. Everything lives in `~/.config/guppy/config.toml`, which Guppy
writes with comments on first start. Bookmarks and your own commands go there too:

```toml
[[bookmark]]
name = "Projects"
path = "~/Work/projects"

[[action]]
name = "Open terminal here"
key = "Ctrl+Alt+T"
command = "xdg-terminal-exec"

[[action]]
name = "Pack as tar.gz"
command = "tar czf archive.tar.gz %F"   # %F = marked files, %f = file under the cursor
```

## Optional programs

Guppy runs without anything else. A few extras use other programs when they are installed:

| Program | Used for |
|---|---|
| poppler | PDF preview |
| ffmpeg | video frames, audio tags and covers |
| ImageMagick | HEIC, AVIF, RAW, PSD … previews |
| bsdtar | 7z, rar and iso archives (already part of Windows and macOS) |
| git | git status in the list |
| zoxide | jumping to folders you use in the shell |

Settings → *Helper programs* shows what's there. Press Enter on a missing one and Guppy opens
a terminal with the install command for your system (pacman, apt, Homebrew or winget), or the
download page if there is no package manager.

## Windows and macOS

Both build with `cargo build --release` on the respective system and are checked by CI on
every push, but I use Guppy on Linux and haven't run it much elsewhere. Known differences:

- **macOS:** Cmd instead of Ctrl. Copying files puts file URLs on the clipboard like Finder
  does; cut only works when you paste in Guppy. Emptying the trash goes through Finder (macOS
  asks once for permission), and undo only brings back things trashed in the same session.
- **Windows:** Alt+Enter only toggles read-only, there is no owner. User commands run with
  `cmd /C`. Symlinks need developer mode or admin rights.
- **Both:** without Omarchy the colors are Matte Black.

## License

MIT, see [LICENSE](LICENSE). The bundled JetBrains Mono font is under the SIL Open Font
License (`crates/guppy-app/fonts/OFL.txt`).
