//! Native file-manager actions shared by desktop file surfaces.
use gpui::App;
use std::path::{Path, PathBuf};

pub(crate) const OPEN_LABEL: &str = if cfg!(target_os = "macos") {
    "Open in Finder"
} else {
    "Open in file manager"
};

/// Remote paths must never be interpreted against the viewport's disk.
pub(crate) fn local_path(root: &str, path: &str, local: bool) -> Option<PathBuf> {
    if !local {
        return None;
    }
    let expand = |value: &str| {
        if value == "~" {
            std::env::var_os("HOME").map(PathBuf::from)
        } else if let Some(tail) = value.strip_prefix("~/") {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(tail))
        } else {
            Some(PathBuf::from(value))
        }
    };
    let path = if Path::new(path).is_absolute() || path.starts_with('~') {
        expand(path)?
    } else {
        expand(root)?.join(path)
    };
    path.is_absolute().then_some(path)
}

#[derive(Debug, PartialEq, Eq)]
enum Destination {
    File,
    Directory,
}

fn destination(path: &Path) -> std::io::Result<Destination> {
    std::fs::metadata(path).map(|metadata| {
        if metadata.is_dir() {
            Destination::Directory
        } else {
            Destination::File
        }
    })
}

/// Probe only on activation, off the UI thread. Missing paths produce a notice.
pub(crate) fn open(path: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx| {
        let probe = path.clone();
        let result = cx
            .background_executor()
            .spawn(async move { destination(&probe) })
            .await;
        cx.update(|cx| match result {
            Ok(kind) => {
                #[cfg(test)]
                {
                    let _ = cx;
                    OPENED.with(|opened| {
                        opened
                            .borrow_mut()
                            .push((path, kind == Destination::Directory))
                    });
                }
                #[cfg(not(test))]
                match kind {
                    Destination::Directory => cx.open_with_system(&path),
                    Destination::File => cx.reveal_path(&path),
                }
            }
            Err(error) => crate::toast::show(
                cx,
                crate::toast::ToastKind::Error,
                format!("Cannot open {}: {error}", path.display()),
            ),
        });
    })
    .detach();
}

#[cfg(test)]
thread_local! { static OPENED: std::cell::RefCell<Vec<(PathBuf, bool)>> = const { std::cell::RefCell::new(Vec::new()) }; }
#[cfg(test)]
pub(crate) fn take_opened() -> Vec<(PathBuf, bool)> {
    OPENED.with(|opened| opened.take())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_paths_preserve_names_expand_home_and_refuse_remote() {
        assert_eq!(
            local_path("/repo", "dir/文 件.md", true),
            Some("/repo/dir/文 件.md".into())
        );
        assert_eq!(
            local_path("/repo", "/other/a.txt", true),
            Some("/other/a.txt".into())
        );
        assert!(local_path("/repo", "a.txt", false).is_none());
        assert!(local_path("relative", "a.txt", true).is_none());
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(
                local_path("~", "a.txt", true),
                Some(PathBuf::from(home).join("a.txt"))
            );
        }
    }
    #[test]
    fn files_and_folders_have_different_native_destinations() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("文 件.md");
        std::fs::write(&file, "test").unwrap();
        assert_eq!(destination(&file).unwrap(), Destination::File);
        assert_eq!(destination(dir.path()).unwrap(), Destination::Directory);
        assert!(destination(&dir.path().join("missing")).is_err());
    }
}
