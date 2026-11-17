//! A project's own artwork: found in its checkout, best candidate first, and
//! rendered as a small PNG for devices that don't hold the checkout
//! (`ReadProjectIcon`). The desktop searches its own checkouts with the same
//! [`find`], so a project wears the same icon on every device.

use std::path::{Path, PathBuf};

/// Where projects typically keep their icon, best candidates first: large or
/// vector artwork, then framework conventions (Next.js `app/` and `src/app/`,
/// SvelteKit/Hugo `static/`, Angular `src/`, Expo, Electron, Tauri, Flutter),
/// then repository logos and plain favicons. Paths are relative to the
/// checkout root.
pub const ICON_PATHS: &[&str] = &[
    "public/apple-touch-icon.png",
    "apple-touch-icon.png",
    "public/favicon.svg",
    "favicon.svg",
    "public/icon.svg",
    "public/logo.svg",
    "app/icon.svg",
    "src/app/icon.svg",
    "app/apple-icon.png",
    "src/app/apple-icon.png",
    "app/icon.png",
    "src/app/icon.png",
    "public/android-chrome-512x512.png",
    "public/android-chrome-192x192.png",
    "public/icon-512.png",
    "public/icon-192.png",
    "public/favicon.png",
    "public/favicon-96x96.png",
    "public/favicon-32x32.png",
    "public/icon.png",
    "public/logo.png",
    "static/favicon.svg",
    "static/favicon.png",
    "static/logo.svg",
    "static/logo.png",
    "src/favicon.svg",
    "src/assets/icon.svg",
    "src/assets/icon.png",
    "src/assets/logo.svg",
    "src/assets/logo.png",
    "assets/icon.svg",
    "assets/icon.png",
    "assets/images/icon.png",
    "assets/logo.svg",
    "assets/logo.png",
    "resources/icon.png",
    "build/icon.png",
    "src-tauri/icons/icon.png",
    "src-tauri/icons/128x128@2x.png",
    "web/icons/Icon-512.png",
    "web/favicon.png",
    "icon.svg",
    "icon.png",
    "logo.svg",
    "logo.png",
    ".github/logo.svg",
    ".github/logo.png",
    "docs/logo.svg",
    "docs/logo.png",
    "favicon.png",
    "public/favicon.ico",
    "app/favicon.ico",
    "src/app/favicon.ico",
    "static/favicon.ico",
    "src/favicon.ico",
    "favicon.ico",
];

/// Monorepo package folders searched with [`ICON_PATHS`] when the root has
/// no icon: `apps/web/public/favicon.svg` and the like.
const MONOREPO_DIRS: &[&str] = &["apps", "packages", "sites"];

/// The largest file read as artwork.
const MAX_ICON_BYTES: u64 = zeron_proto::MAX_WORKSPACE_IMAGE_BYTES as u64;

/// The checkout root holding `folder` (a project may be a subfolder; a
/// linked worktree's `.git` is a file), canonical.
pub fn checkout_root(folder: &Path) -> Option<PathBuf> {
    let canonical = folder.canonicalize().ok()?;
    Some(
        canonical
            .ancestors()
            .find(|path| path.join(".git").exists())
            .unwrap_or(&canonical)
            .to_path_buf(),
    )
}

/// The project's artwork under the checkout holding `folder`: the first of
/// [`ICON_PATHS`] at its root, else in a monorepo package, else the largest
/// image of an Xcode app icon set. Only files inside the checkout count (a
/// symlink pointing out of it is skipped). The first match wins even when
/// it later fails to decode: a corrupt icon shows the default rather than
/// unrelated lower-priority art.
pub fn find(folder: &Path) -> Option<PathBuf> {
    let root = checkout_root(folder)?;
    first_icon_path(&root, &root)
        .or_else(|| {
            MONOREPO_DIRS
                .iter()
                .flat_map(|dir| subdirectories(&root.join(dir)))
                .find_map(|package| first_icon_path(&root, &package))
        })
        .or_else(|| xcode_app_icon(&root))
}

/// `path` resolved, when it's a regular file inside `root`.
fn inside(root: &Path, path: &Path) -> Option<PathBuf> {
    let resolved = path.canonicalize().ok()?;
    (resolved.starts_with(root) && resolved.is_file()).then_some(resolved)
}

/// The first candidate that exists under `dir`.
fn first_icon_path(root: &Path, dir: &Path) -> Option<PathBuf> {
    ICON_PATHS
        .iter()
        .find_map(|path| inside(root, &dir.join(path)))
}

/// Sorted subdirectories of `dir` (hidden ones skipped). Bounded: a huge
/// folder must not stall the icon search.
fn subdirectories(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .take(200)
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// The largest PNG of an Xcode app icon set within two levels of `root`, or
/// within the conventional `apps/<platform>/<app>` monorepo layout.
fn xcode_app_icon(root: &Path) -> Option<PathBuf> {
    let mut parents = vec![root.to_path_buf()];
    parents.extend(subdirectories(root));
    let level_two: Vec<_> = parents
        .iter()
        .skip(1)
        .flat_map(|dir| subdirectories(dir))
        .collect();
    parents.extend(level_two);
    // Keep the search bounded for ordinary repositories, but include iOS (or
    // other platform) apps living at `apps/<platform>/<app>`.
    parents.extend(
        subdirectories(&root.join("apps"))
            .into_iter()
            .flat_map(|platform| subdirectories(&platform)),
    );
    parents.into_iter().find_map(|dir| {
        let set = dir.join("Assets.xcassets/AppIcon.appiconset");
        std::fs::read_dir(&set)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|e| e == "png"))
            .filter_map(|path| inside(root, &path))
            .filter_map(|path| Some((path.metadata().ok()?.len(), path)))
            .filter(|(len, _)| *len <= MAX_ICON_BYTES)
            .max_by_key(|(len, _)| *len)
            .map(|(_, path)| path)
    })
}

/// The project's artwork under `folder`, rendered to fit `max_side` pixels
/// (never scaled up). No artwork, or artwork that won't decode, answers
/// with empty `data`.
pub fn read(folder: &Path, max_side: u32) -> zeron_proto::ProjectIconImage {
    use base64::Engine as _;
    let Some(path) = find(folder) else {
        return zeron_proto::ProjectIconImage::default();
    };
    let relative = checkout_root(folder).and_then(|root| {
        path.strip_prefix(&root)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
    });
    let mut icon = zeron_proto::ProjectIconImage {
        path: relative,
        ..Default::default()
    };
    match render_png(&path, max_side) {
        Ok((png, width, height)) => {
            icon.data = base64::engine::general_purpose::STANDARD.encode(png);
            icon.width = width;
            icon.height = height;
        }
        Err(error) => tracing::debug!(path = %path.display(), %error, "project icon unreadable"),
    }
    icon
}

/// `path` (SVG, or a PNG/ICO/JPEG/GIF/WebP raster) as a PNG at most
/// `max_side` pixels on its longer side, with its width and height.
pub fn render_png(path: &Path, max_side: u32) -> Result<(Vec<u8>, u32, u32), String> {
    use std::io::Read;
    let max_side = max_side.max(1);
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_ICON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_ICON_BYTES {
        return Err("icon is too large".into());
    }
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
    {
        render_svg(&bytes, max_side)
    } else {
        render_raster(&bytes, max_side)
    }
}

fn render_svg(bytes: &[u8], max_side: u32) -> Result<(Vec<u8>, u32, u32), String> {
    use resvg::{tiny_skia, usvg};
    // Default options load nothing from disk or the network: an SVG's
    // external references simply don't draw.
    let tree =
        usvg::Tree::from_data(bytes, &usvg::Options::default()).map_err(|e| e.to_string())?;
    let size = tree.size();
    let longest = size.width().max(size.height());
    if !(longest.is_finite() && longest > 0.0) {
        return Err("svg has no size".into());
    }
    let scale = max_side as f32 / longest;
    let width = ((size.width() * scale).round() as u32).clamp(1, max_side);
    let height = ((size.height() * scale).round() as u32).clamp(1, max_side);
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or("svg canvas failed")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let png = pixmap.encode_png().map_err(|e| e.to_string())?;
    Ok((png, width, height))
}

fn render_raster(bytes: &[u8], max_side: u32) -> Result<(Vec<u8>, u32, u32), String> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(zeron_proto::IMAGE_MAX_SIDE);
    limits.max_image_height = Some(zeron_proto::IMAGE_MAX_SIDE);
    limits.max_alloc = Some(64 * 1024 * 1024);
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    reader.limits(limits);
    let mut image = reader.decode().map_err(|e| e.to_string())?;
    if image.width() > max_side || image.height() > max_side {
        image = image.resize(max_side, max_side, image::imageops::FilterType::Lanczos3);
    }
    let (width, height) = (image.width(), image.height());
    let mut png = Vec::new();
    image
        .into_rgba8()
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok((png, width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(path: &Path, width: u32, height: u32) {
        image::RgbaImage::new(width, height).save(path).unwrap();
    }

    fn found(dir: &Path) -> Option<String> {
        let root = dir.canonicalize().unwrap();
        find(dir).map(|p| {
            p.strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
    }

    #[test]
    fn priority_subfolders_and_missing() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(found(temp.path()), None);
        png(&temp.path().join("favicon.png"), 3, 3);
        assert_eq!(found(temp.path()).as_deref(), Some("favicon.png"));
        std::fs::create_dir(temp.path().join("public")).unwrap();
        png(&temp.path().join("public/apple-touch-icon.png"), 7, 7);
        assert_eq!(
            found(temp.path()).as_deref(),
            Some("public/apple-touch-icon.png")
        );
        // A project in a subfolder of a checkout (or a linked worktree, whose
        // .git is a file) wears the checkout's icon.
        std::fs::write(temp.path().join(".git"), "gitdir: /unused-test-checkout").unwrap();
        std::fs::create_dir_all(temp.path().join("src/nested")).unwrap();
        assert_eq!(find(&temp.path().join("src/nested")), find(temp.path()));
    }

    #[test]
    fn next_src_app_monorepos_and_xcode() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("src/app")).unwrap();
        image::RgbaImage::new(16, 16)
            .save(temp.path().join("src/app/favicon.ico"))
            .unwrap();
        assert_eq!(found(temp.path()).as_deref(), Some("src/app/favicon.ico"));

        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("apps/landing/public")).unwrap();
        png(&temp.path().join("apps/landing/public/favicon.png"), 5, 5);
        assert_eq!(
            found(temp.path()).as_deref(),
            Some("apps/landing/public/favicon.png")
        );

        let temp = tempfile::tempdir().unwrap();
        let set = temp
            .path()
            .join("apps/ios/App/Assets.xcassets/AppIcon.appiconset");
        std::fs::create_dir_all(&set).unwrap();
        png(&set.join("small.png"), 2, 2);
        png(&set.join("large.png"), 512, 512);
        assert_eq!(
            found(temp.path()).as_deref(),
            Some("apps/ios/App/Assets.xcassets/AppIcon.appiconset/large.png")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_checkout_is_skipped() {
        let outside = tempfile::tempdir().unwrap();
        png(&outside.path().join("secret.png"), 4, 4);
        let temp = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.png"),
            temp.path().join("favicon.png"),
        )
        .unwrap();
        assert_eq!(found(temp.path()), None);
        // One pointing within the checkout is fine.
        png(&temp.path().join("logo.png"), 4, 4);
        std::os::unix::fs::symlink(temp.path().join("logo.png"), temp.path().join("icon.png"))
            .unwrap();
        assert_eq!(found(temp.path()).as_deref(), Some("logo.png"));
    }

    #[test]
    fn renders_raster_svg_and_ico_scaled_down_only() {
        let temp = tempfile::tempdir().unwrap();
        png(&temp.path().join("big.png"), 512, 256);
        assert_eq!(
            render_png(&temp.path().join("big.png"), 128).map(|(_, w, h)| (w, h)),
            Ok((128, 64))
        );
        png(&temp.path().join("small.png"), 20, 20);
        assert_eq!(
            render_png(&temp.path().join("small.png"), 128).map(|(_, w, h)| (w, h)),
            Ok((20, 20))
        );
        std::fs::write(
            temp.path().join("favicon.svg"),
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="12" height="8"><rect width="12" height="8" fill="#f00"/></svg>"##,
        )
        .unwrap();
        let (svg, w, h) = render_png(&temp.path().join("favicon.svg"), 120).unwrap();
        assert_eq!((w, h), (120, 80));
        let decoded = image::load_from_memory(&svg).unwrap().into_rgba8();
        assert_eq!(decoded.get_pixel(60, 40).0, [255, 0, 0, 255]);
        image::RgbaImage::new(16, 16)
            .save(temp.path().join("favicon.ico"))
            .unwrap();
        assert_eq!(
            render_png(&temp.path().join("favicon.ico"), 128).map(|(_, w, h)| (w, h)),
            Ok((16, 16))
        );
        std::fs::write(temp.path().join("broken.png"), b"invalid").unwrap();
        assert!(render_png(&temp.path().join("broken.png"), 128).is_err());
    }

    #[test]
    fn read_reports_the_source_and_empty_data_when_unreadable() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(
            read(temp.path(), 64),
            zeron_proto::ProjectIconImage::default()
        );
        std::fs::create_dir(temp.path().join("public")).unwrap();
        std::fs::write(temp.path().join("public/apple-touch-icon.png"), b"invalid").unwrap();
        let icon = read(temp.path(), 64);
        assert_eq!(icon.path.as_deref(), Some("public/apple-touch-icon.png"));
        assert!(icon.data.is_empty());
        png(&temp.path().join("public/apple-touch-icon.png"), 180, 180);
        let icon = read(temp.path(), 64);
        assert_eq!((icon.width, icon.height), (64, 64));
        assert!(!icon.data.is_empty());
    }
}
