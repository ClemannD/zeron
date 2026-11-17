//! Finds the local images an agent's reply names by path, so the host can
//! store copies in uploads and append them to the reply as image parts.
//!
//! Agents often end with "the screenshot is at `/tmp/…/shot.png`" instead of
//! calling `show_image`, and a path outside the chat's folder can't be drawn
//! as a Markdown image (`read_chat_image_chunk` serves only that folder).
//! Image parts are read from uploads, so remote clients can load them and
//! they survive the source file being deleted.
//!
//! A path counts when it is a Markdown image or link target, an inline code
//! span, or a bare word, and is absolute, `~/`-relative, or relative to the
//! chat's folder. Images over [`zeron_proto::IMAGE_MAX_SIDE`] on a side or
//! [`zeron_proto::IMAGE_MAX_BYTES`] long are downscaled, not skipped.

use std::path::{Path, PathBuf};

/// At most this many images per turn: a reply listing a folder of icons
/// shows the first few, not all of them.
pub(crate) const MAX_PER_TURN: usize = 8;

/// The largest source file read at all. Bigger ones are downscaled to fit
/// [`zeron_proto::IMAGE_MAX_BYTES`], so this only bounds the decode.
const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

/// One path the reply names that looks like an image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mention {
    /// As written, minus `file://` and surrounding punctuation.
    pub source: String,
    /// Written as a Markdown image (`![alt](source)`): clients may already
    /// draw it in place.
    pub markdown_image: bool,
}

const EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];

fn has_image_extension(source: &str) -> bool {
    let name = source.rsplit(['/', '\\']).next().unwrap_or(source);
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
    })
}

/// `http:`, `data:`, `mailto:`…, but not a drive letter (`C:\`).
fn has_scheme(source: &str) -> bool {
    source.split_once(':').is_some_and(|(scheme, _)| {
        scheme.len() > 1
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

/// A candidate cleaned up, or `None` if it isn't a local image path.
fn clean(raw: &str) -> Option<String> {
    let trimmed = raw
        .trim()
        .trim_start_matches(['(', '<', '"', '\'', '*', '_', '[']);
    let trimmed = trimmed.trim_end_matches([
        ')', '>', '"', '\'', '*', '_', ',', '.', ';', ':', '!', '?', ']',
    ]);
    let source = trimmed.strip_prefix("file://").unwrap_or(trimmed);
    if source.is_empty() || has_scheme(source) || source.contains('\n') {
        return None;
    }
    // UNC and device paths (`//host/share`, `\\host\share`) are never read.
    if source.starts_with("//") || source.starts_with("\\\\") {
        return None;
    }
    has_image_extension(source).then(|| source.to_owned())
}

/// Every image path `text` names, in order, each once.
pub(crate) fn mentions(text: &str) -> Vec<Mention> {
    let mut found: Vec<Mention> = Vec::new();
    let mut push = |source: Option<String>, markdown_image: bool| {
        let Some(source) = source else { return };
        if let Some(existing) = found.iter_mut().find(|m| m.source == source) {
            existing.markdown_image &= markdown_image;
        } else {
            found.push(Mention {
                source,
                markdown_image,
            });
        }
    };
    let mut fence: Option<&str> = None;
    for line in text.lines() {
        let opener = line.trim_start();
        let marker = ["```", "~~~"].into_iter().find(|m| opener.starts_with(m));
        match (fence, marker) {
            (None, Some(marker)) => {
                fence = Some(marker);
                continue;
            }
            (Some(open), Some(marker)) if open == marker => {
                fence = None;
                continue;
            }
            _ => {}
        }
        if fence.is_some() {
            // Inside a code block only a line that is nothing but a path
            // counts: a script that writes `out.png` is not a mention.
            let only = opener.trim_end();
            if !only.contains(char::is_whitespace) {
                push(clean(only), false);
            }
            continue;
        }
        scan_line(line, &mut push);
    }
    found
}

/// One line outside code blocks: Markdown targets, then code spans, then the
/// bare words left over.
fn scan_line(line: &str, push: &mut impl FnMut(Option<String>, bool)) {
    let mut rest = line.to_owned();

    // `[label](target)` and `![alt](target)`, target optionally `<…>` and
    // followed by a title.
    let mut from = 0;
    while let Some(at) = rest[from..].find("](").map(|i| i + from) {
        let target_start = at + 2;
        let Some(len) = rest[target_start..].find(')') else {
            break;
        };
        let target_end = target_start + len;
        let target = rest[target_start..target_end].trim();
        let target = if let Some(inner) = target.strip_prefix('<') {
            inner.split('>').next().unwrap_or_default()
        } else {
            target.split_whitespace().next().unwrap_or_default()
        };
        let image = rest[..at]
            .rfind('[')
            .is_some_and(|open| open > 0 && rest.as_bytes()[open - 1] == b'!');
        push(clean(target), image);
        // Blank the target so the bare-word pass doesn't see it again.
        let blank = " ".repeat(target_end - target_start);
        rest.replace_range(target_start..target_end, &blank);
        from = target_end;
    }

    // Code spans: the whole content (a path with spaces), then its words
    // (`open /tmp/a.png`).
    let mut outside = String::with_capacity(rest.len());
    let mut chars = rest.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c != '`' {
            outside.push(c);
            continue;
        }
        let mut ticks = 1;
        while chars.peek().is_some_and(|(_, c)| *c == '`') {
            chars.next();
            ticks += 1;
        }
        let open_end = i + ticks;
        let closer = "`".repeat(ticks);
        match rest[open_end..].find(&closer) {
            Some(len) => {
                let content = &rest[open_end..open_end + len];
                // With spaces, the whole span is a path only when its first
                // word already is one (`assets/my icon.png`, not `open a.png`).
                let first_word = content.split_whitespace().next().unwrap_or_default();
                if first_word == content.trim() || first_word.contains(['/', '\\']) {
                    push(clean(content), false);
                }
                for word in content.split_whitespace() {
                    push(clean(word), false);
                }
                outside.push(' ');
                let close_end = open_end + len + ticks;
                while chars.peek().is_some_and(|(j, _)| *j < close_end) {
                    chars.next();
                }
            }
            None => outside.push_str(&closer),
        }
    }

    for word in outside.split_whitespace() {
        push(clean(word), false);
    }
}

/// `source` as an absolute path on this host: `~/` joins `home`, a relative
/// path joins `cwd`. `None` when that can't be done.
pub(crate) fn resolve(source: &str, cwd: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    let path = if let Some(rest) = source.strip_prefix("~/") {
        home?.join(rest)
    } else if source == "~" || source.starts_with('~') {
        return None;
    } else {
        let path = Path::new(source);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd?.join(path)
        }
    };
    path.is_absolute().then_some(path)
}

/// An image ready to store: bytes in one of the four formats, at most
/// [`zeron_proto::IMAGE_MAX_SIDE`] on a side and
/// [`zeron_proto::IMAGE_MAX_BYTES`] long, and the name it shows under.
#[derive(Debug)]
pub(crate) struct Loaded {
    pub bytes: Vec<u8>,
    pub name: String,
}

/// Read `path` if it is a regular file holding a PNG, JPEG, WebP or GIF,
/// downscaling it when it's too big to show as it is.
pub(crate) fn load(path: &Path) -> Result<Loaded, String> {
    use zeron_proto::{IMAGE_MAX_BYTES, IMAGE_MAX_SIDE};
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a file".into());
    }
    if meta.len() > MAX_SOURCE_BYTES {
        return Err("too large to read".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let format = match image::guess_format(&bytes) {
        Ok(
            f @ (image::ImageFormat::Png
            | image::ImageFormat::Jpeg
            | image::ImageFormat::WebP
            | image::ImageFormat::Gif),
        ) => f,
        _ => return Err("not a PNG, JPEG, WebP, or GIF image".into()),
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".into());
    let (width, height) = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format)
        .into_dimensions()
        .map_err(|e| e.to_string())?;
    if width.max(height) <= IMAGE_MAX_SIDE && bytes.len() <= IMAGE_MAX_BYTES {
        return Ok(Loaded { bytes, name });
    }
    let decoded = image::load_from_memory_with_format(&bytes, format).map_err(|e| e.to_string())?;
    let scaled = if width.max(height) > IMAGE_MAX_SIDE {
        decoded.resize(
            IMAGE_MAX_SIDE,
            IMAGE_MAX_SIDE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        decoded
    };
    // PNG keeps a screenshot's text crisp; JPEG when that is still too big.
    let mut png = Vec::new();
    scaled
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let (bytes, ext) = if png.len() <= IMAGE_MAX_BYTES {
        (png, "png")
    } else {
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(scaled.to_rgb8())
            .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut std::io::Cursor::new(&mut jpeg),
                85,
            ))
            .map_err(|e| e.to_string())?;
        if jpeg.len() > IMAGE_MAX_BYTES {
            return Err("too large even downscaled".into());
        }
        (jpeg, "jpg")
    };
    let stem = Path::new(&name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".into());
    Ok(Loaded {
        bytes,
        name: format!("{stem}.{ext}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(text: &str) -> Vec<(String, bool)> {
        mentions(text)
            .into_iter()
            .map(|m| (m.source, m.markdown_image))
            .collect()
    }

    #[test]
    fn finds_paths_however_the_reply_writes_them() {
        let text = "Here are the files:\n\n\
            - **Desktop**: `/private/tmp/shots/desktop-all.png`\n\
            - Mobile: /tmp/shots/mobile.PNG.\n\
            See ![chart](out/chart.webp \"Chart\") and [the icon](<assets/my icon.jpg>).\n\
            Ran `open ~/Pictures/a.jpeg` too, and (relative.gif)";
        assert_eq!(
            sources(text),
            [
                ("/private/tmp/shots/desktop-all.png".to_owned(), false),
                ("/tmp/shots/mobile.PNG".to_owned(), false),
                ("out/chart.webp".to_owned(), true),
                ("assets/my icon.jpg".to_owned(), false),
                ("~/Pictures/a.jpeg".to_owned(), false),
                ("relative.gif".to_owned(), false),
            ]
        );
    }

    #[test]
    fn leaves_urls_other_files_and_code_alone() {
        let text = "https://example.com/a.png and data:image/png;base64,xx \
            `//host/share/b.png` notes.md `src/main.rs`\n\
            ```sh\nconvert in.png -resize 50% out.png\n/abs/only-path.png\n```\n\
            file:///tmp/c.png";
        assert_eq!(
            sources(text),
            [
                ("/abs/only-path.png".to_owned(), false),
                ("/tmp/c.png".to_owned(), false),
            ]
        );
    }

    #[test]
    fn a_path_named_twice_is_one_mention_and_any_plain_use_wins() {
        let text = "![a](/tmp/a.png)\n\nAlso at `/tmp/a.png`.";
        assert_eq!(sources(text), [("/tmp/a.png".to_owned(), false)]);
        assert_eq!(
            sources("![a](/tmp/a.png)"),
            [("/tmp/a.png".to_owned(), true)]
        );
    }

    #[test]
    fn resolves_against_the_chat_folder_and_home() {
        let cwd = Path::new("/work/app");
        let home = Path::new("/Users/me");
        assert_eq!(
            resolve("out/a.png", Some(cwd), Some(home)),
            Some(PathBuf::from("/work/app/out/a.png"))
        );
        assert_eq!(
            resolve("~/a.png", Some(cwd), Some(home)),
            Some(PathBuf::from("/Users/me/a.png"))
        );
        assert_eq!(resolve("~other/a.png", Some(cwd), Some(home)), None);
        assert_eq!(resolve("a.png", None, Some(home)), None);
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            width,
            height,
            image::Rgba([10, 20, 30, 255]),
        ))
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
        out
    }

    #[test]
    fn loads_images_and_downscales_tall_screenshots() {
        let dir = tempfile::tempdir().unwrap();
        let small = dir.path().join("small.png");
        std::fs::write(&small, png(40, 20)).unwrap();
        let loaded = load(&small).unwrap();
        assert_eq!(loaded.name, "small.png");
        assert_eq!(loaded.bytes, std::fs::read(&small).unwrap());

        let tall = dir.path().join("page.png");
        std::fs::write(&tall, png(400, 9000)).unwrap();
        let loaded = load(&tall).unwrap();
        let (w, h) = image::ImageReader::new(std::io::Cursor::new(&loaded.bytes))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert_eq!(h, zeron_proto::IMAGE_MAX_SIDE);
        assert!(w < 400 && w > 150, "{w}");

        let fake = dir.path().join("fake.png");
        std::fs::write(&fake, b"not an image").unwrap();
        assert!(load(&fake).is_err());
        assert!(load(dir.path()).is_err());
    }
}
