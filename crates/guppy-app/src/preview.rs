//! What the viewer (F3) and the quick view (Ctrl+Q) show for an entry.
//! Everything here runs off the UI thread; slow formats use external tools
//! (poppler, ffmpeg) when installed and fall back to a short description.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use guppy_core::archive;
use guppy_core::fs::{human_size, Entry};
use iced::widget::image::Handle;

#[derive(Debug, Clone)]
pub enum ViewBody {
    Loading,
    Text { text: String, truncated: bool },
    /// Binary file as a hex dump.
    Hex { text: String, truncated: bool },
    /// Picture file the image widget reads itself.
    Image,
    Svg,
    /// Rendered picture (PDF page, video frame, cover art) with a caption.
    Picture { image: Handle, caption: String },
    /// Folder or archive: its entries, one per line.
    Dir(String),
    /// Not displayable; a short description instead.
    Other(String),
}

/// The viewer shows at most this much of a text file.
pub const VIEW_MAX: usize = 512 * 1024;
/// Binary files: dumped up to this size (16 bytes per line).
pub const HEX_MAX: usize = 64 * 1024;
/// The quick view is redrawn while scrolling through files: keep text short.
const QUICK_MAX: usize = 48 * 1024;

const VIDEO: &[&str] = &["mp4", "mkv", "webm", "mov", "avi", "m4v", "wmv", "flv", "mpg", "mpeg", "ts", "3gp"];
const AUDIO: &[&str] = &["mp3", "flac", "ogg", "oga", "opus", "wav", "m4a", "aac", "wma", "aiff", "ape"];
/// Decoded by the image widget directly.
const IMAGE: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff"];
/// Converted with ImageMagick first.
const IMAGE_CONVERT: &[&str] = &["avif", "heic", "heif", "jxl", "psd", "xcf", "cr2", "nef", "arw", "dng"];

/// Reads what the viewer shows. `quick` keeps text shorter for the quick view.
pub fn load(path: &Path, quick: bool) -> ViewBody {
    // On a remote: the viewer downloads the file first, the quick view doesn't.
    if guppy_core::remote::is_remote(path) {
        if quick {
            return ViewBody::Other("Remote file · F3 downloads and shows it".into());
        }
        if let Ok(Some(true)) = guppy_core::remote::stat(path) {
            return match guppy_core::remote::read_dir(path) {
                Ok(list) => ViewBody::Dir(dir_lines(&list)),
                Err(e) => ViewBody::Other(e.to_string()),
            };
        }
        return match crate::temp_dir().and_then(|dir| guppy_core::remote::download(path, &dir)) {
            Ok(local) => load(&local, quick),
            Err(e) => ViewBody::Other(e.to_string()),
        };
    }
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    // Inside an archive: a folder lists, a file is read through bsdtar.
    if path.symlink_metadata().is_err()
        && let Some((arch, inner)) = archive::split(path)
    {
        if let Ok(list) = archive::read_dir(&arch, &inner) {
            return ViewBody::Dir(dir_lines(&list));
        }
        if IMAGE.contains(&ext.as_str()) || ext == "svg" {
            return ViewBody::Other("Image inside an archive · Enter opens it".into());
        }
        let max = if quick { QUICK_MAX } else { VIEW_MAX };
        return match archive::read_member(&arch, &inner, max) {
            Ok(buf) => view_bytes(buf, archive::size_of(&arch, &inner) as usize, max),
            Err(e) => ViewBody::Other(e.to_string()),
        };
    }
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return ViewBody::Other(e.to_string()),
    };
    if meta.is_dir() {
        return match guppy_core::read_dir(path) {
            Ok(list) if list.is_empty() => ViewBody::Other("Empty folder".into()),
            Ok(list) => ViewBody::Dir(dir_lines(&list)),
            Err(e) => ViewBody::Other(e.to_string()),
        };
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let e = ext.as_str();
    if IMAGE.contains(&e) {
        return ViewBody::Image;
    }
    if e == "svg" {
        return ViewBody::Svg;
    }
    if archive::is_archive_name(&name) {
        return match archive::read_dir(path, Path::new("")) {
            Ok(list) => ViewBody::Dir(dir_lines(&list)),
            Err(e) => ViewBody::Other(e.to_string()),
        };
    }
    let special = match e {
        "pdf" => Some(pdf(path)),
        _ if VIDEO.contains(&e) => Some(video(path)),
        _ if AUDIO.contains(&e) => Some(audio(path)),
        _ if IMAGE_CONVERT.contains(&e) => Some(converted(path)),
        "docx" | "odt" | "xlsx" | "ods" | "pptx" | "odp" | "epub" => Some(office(path, e)),
        _ => None,
    };
    if let Some(body) = special {
        return body;
    }
    let max = if quick { QUICK_MAX } else { VIEW_MAX };
    let mut buf = Vec::new();
    if let Err(e) = std::fs::File::open(path).and_then(|f| f.take(max as u64).read_to_end(&mut buf)) {
        return ViewBody::Other(e.to_string());
    }
    view_bytes(buf, meta.len() as usize, max)
}

/// A folder's entries, one per line.
fn dir_lines(list: &[Entry]) -> String {
    list.iter()
        .map(|c| if c.is_dir() { format!("{}/", c.name) } else { format!("{:<40} {:>10}", c.name, human_size(c.size)) })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Text or hex dump of the first bytes of a file of `len` bytes.
fn view_bytes(mut buf: Vec<u8>, len: usize, max: usize) -> ViewBody {
    // NUL bytes in the first few KB: treat as binary.
    if buf.iter().take(8192).any(|&b| b == 0) {
        let hex_max = HEX_MAX.min(max);
        buf.truncate(hex_max);
        return ViewBody::Hex { text: hex_dump(&buf), truncated: len > hex_max };
    }
    let text = match String::from_utf8(buf) {
        Ok(s) => s,
        // Cut-off multi-byte character at the end of the buffer.
        Err(e) => String::from_utf8_lossy(&e.as_bytes()[..e.utf8_error().valid_up_to().max(e.as_bytes().len().saturating_sub(4))]).into_owned(),
    };
    ViewBody::Text { text: text.replace('\t', "    "), truncated: len > max }
}

/// Offset, 16 bytes in hex (split in two groups), then printable ASCII.
pub fn hex_dump(buf: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(buf.len() / 16 * 80 + 80);
    for (n, chunk) in buf.chunks(16).enumerate() {
        let _ = write!(out, "{:08x}  ", n * 16);
        for i in 0..16 {
            match chunk.get(i) {
                Some(b) => {
                    let _ = write!(out, "{b:02x} ");
                }
                None => out.push_str("   "),
            }
            if i == 7 {
                out.push(' ');
            }
        }
        out.push(' ');
        out.extend(chunk.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '·' }));
        out.push('\n');
    }
    out
}

/// Runs a tool and returns its stdout if it succeeded with output.
fn run(cmd: &mut Command) -> Option<Vec<u8>> {
    let out = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    (out.status.success() && !out.stdout.is_empty()).then_some(out.stdout)
}

fn picture(png: Vec<u8>, caption: String) -> ViewBody {
    ViewBody::Picture { image: Handle::from_bytes(png), caption }
}

/// First page as a picture (poppler), else its text.
fn pdf(path: &Path) -> ViewBody {
    let pages = run(Command::new("pdfinfo").arg(path))
        .and_then(|o| String::from_utf8_lossy(&o).lines().find_map(|l| l.strip_prefix("Pages:").map(|n| n.trim().to_string())))
        .map(|n| format!("{n} pages"))
        .unwrap_or_default();
    // pdftoppm can't write to stdout: render into a temporary file.
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!("guppy-pdf-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let rendered = Command::new("pdftoppm")
        .args(["-png", "-r", "90", "-f", "1", "-l", "1", "-singlefile"])
        .arg(path)
        .arg(&root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let png = root.with_extension("png");
    let image = rendered.then(|| std::fs::read(&png).ok()).flatten();
    let _ = std::fs::remove_file(&png);
    if let Some(png) = image {
        return picture(png, format!("Page 1 · {pages}"));
    }
    match run(Command::new("pdftotext").args(["-l", "3", "-layout"]).arg(path).arg("-")) {
        Some(t) if !String::from_utf8_lossy(&t).trim().is_empty() => ViewBody::Text { text: String::from_utf8_lossy(&t).replace('\u{c}', "\n"), truncated: false },
        _ => ViewBody::Other("PDF · install poppler for a preview".into()),
    }
}

/// Duration, resolution and codecs as one line, from ffprobe.
fn media_info(path: &Path) -> Option<(String, Vec<(String, String)>)> {
    let out = run(Command::new("ffprobe").args(["-v", "quiet", "-show_entries", "format=duration,bit_rate:format_tags:stream=codec_type,codec_name,width,height,sample_rate,channels", "-of", "default=nw=1"]).arg(path))?;
    let text = String::from_utf8_lossy(&out).into_owned();
    let mut parts = Vec::new();
    let mut tags = Vec::new();
    let (mut w, mut h) = (None, None);
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k {
            "duration" => {
                if let Ok(s) = v.parse::<f64>() {
                    let s = s as u64;
                    parts.push(if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) });
                }
            }
            "width" => w = Some(v.to_string()),
            "height" => h = Some(v.to_string()),
            "codec_name" => parts.push(v.to_string()),
            "sample_rate" => parts.push(format!("{} kHz", v.parse::<f64>().map(|r| r / 1000.0).unwrap_or(0.0))),
            "bit_rate" => {
                if let Ok(b) = v.parse::<u64>() {
                    parts.push(format!("{} kb/s", b / 1000));
                }
            }
            k if k.starts_with("TAG:") => {
                let key = k[4..].to_lowercase();
                if matches!(key.as_str(), "title" | "artist" | "album" | "date" | "genre" | "track" | "album_artist") {
                    tags.push((key, v.to_string()));
                }
            }
            _ => {}
        }
    }
    if let (Some(w), Some(h)) = (w, h) {
        parts.insert(0, format!("{w}×{h}"));
    }
    parts.dedup();
    Some((parts.join(" · "), tags))
}

/// A frame from about 10 % in, plus duration and resolution.
fn video(path: &Path) -> ViewBody {
    let info = media_info(path).map(|(i, _)| i).unwrap_or_default();
    let frame = run(Command::new("ffmpegthumbnailer").args(["-s", "640", "-c", "png", "-t", "10%", "-i"]).arg(path).args(["-o", "-"]))
        .or_else(|| run(Command::new("ffmpeg").args(["-v", "quiet", "-ss", "3", "-i"]).arg(path).args(["-frames:v", "1", "-vf", "scale=640:-1", "-f", "image2pipe", "-c:v", "png", "-"])));
    match frame {
        Some(png) => picture(png, info),
        None if info.is_empty() => ViewBody::Other("Video · install ffmpeg for a preview".into()),
        None => ViewBody::Text { text: info, truncated: false },
    }
}

/// Tags and format, with the cover art if there is one.
fn audio(path: &Path) -> ViewBody {
    let Some((info, tags)) = media_info(path) else {
        return ViewBody::Other("Audio · install ffmpeg for details".into());
    };
    let mut lines: Vec<String> = tags.iter().map(|(k, v)| format!("{k:<13} {v}")).collect();
    lines.push(String::new());
    lines.push(info.clone());
    let cover = run(Command::new("ffmpeg").args(["-v", "quiet", "-i"]).arg(path).args(["-an", "-frames:v", "1", "-vf", "scale=480:-1", "-f", "image2pipe", "-c:v", "png", "-"]));
    match cover {
        Some(png) => picture(png, lines.into_iter().filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")),
        None => ViewBody::Text { text: lines.join("\n"), truncated: false },
    }
}

/// Formats the image widget can't read, converted with ImageMagick.
fn converted(path: &Path) -> ViewBody {
    let mut src = path.as_os_str().to_owned();
    src.push("[0]");
    match run(Command::new("magick").arg(src).args(["-resize", "1600x1600>", "png:-"])) {
        Some(png) => picture(png, String::new()),
        None => ViewBody::Other("Image · install ImageMagick for a preview".into()),
    }
}

/// Office documents are zip files with XML inside: show their text.
fn office(path: &Path, ext: &str) -> ViewBody {
    let members: &[&str] = match ext {
        "docx" => &["word/document.xml"],
        "xlsx" => &["xl/sharedStrings.xml"],
        "pptx" => &["ppt/slides/slide1.xml", "ppt/slides/slide2.xml", "ppt/slides/slide3.xml"],
        "epub" => &["OEBPS/content.opf"],
        _ => &["content.xml"],
    };
    let mut text = String::new();
    for m in members {
        if let Ok(xml) = archive::read_member(path, Path::new(m), 2 * 1024 * 1024) {
            text.push_str(&xml_text(&String::from_utf8_lossy(&xml)));
            text.push('\n');
        }
    }
    let text = text.trim().to_string();
    if text.is_empty() {
        return ViewBody::Other("Document · no text preview".into());
    }
    let truncated = text.len() > QUICK_MAX;
    let mut cut = QUICK_MAX.min(text.len());
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    ViewBody::Text { text: text[..cut].to_string(), truncated }
}

/// Strips tags; paragraph, row and line-break ends become newlines.
fn xml_text(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let Some(gt) = rest[lt..].find('>') else { break };
        let tag = &rest[lt + 1..lt + gt];
        let name = tag.trim_start_matches('/').split([' ', '/']).next().unwrap_or("");
        let local = name.rsplit(':').next().unwrap_or(name);
        if (tag.starts_with('/') && matches!(local, "p" | "h" | "si" | "row" | "title")) || matches!(local, "br" | "tab" | "line-break") {
            out.push(if local == "tab" { '\t' } else { '\n' });
        }
        rest = &rest[lt + gt + 1..];
    }
    out.push_str(rest);
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace('\t', "    ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_to_text() {
        let t = xml_text("<w:p><w:r><w:t>Hallo</w:t></w:r></w:p><w:p><w:t>A &amp; B</w:t></w:p>");
        assert_eq!(t, "Hallo\nA & B\n");
    }

    #[test]
    fn text_and_binary() {
        assert!(matches!(view_bytes(b"hi\tthere".to_vec(), 8, 100), ViewBody::Text { ref text, truncated: false } if text == "hi    there"));
        assert!(matches!(view_bytes(vec![0, 1, 2], 3, 100), ViewBody::Hex { .. }));
    }
}
