//! A desktop notification when a screenshot lands on the clipboard, with the image as its preview.
//! Uses `notify-send` with the standard `image-path` hint, which most notification daemons show.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

/// How many preview files of clipboard-only captures to keep around.
const KEEP_PREVIEWS: usize = 5;

/// Image size from a PNG's header.
fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    let w = u32::from_be_bytes(png.get(16..20)?.try_into().ok()?);
    let h = u32::from_be_bytes(png.get(20..24)?.try_into().ok()?);
    Some((w, h))
}

fn home_relative(path: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) if path.starts_with(&home) => format!("~/{}", path.strip_prefix(&home).unwrap().display()),
        _ => path.display().to_string(),
    }
}

/// A file the notification can show: the saved screenshot, or a copy in the runtime directory
/// when it was only copied. Old copies are pruned.
fn preview_file(png: &[u8], saved: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = saved {
        return Some(path.to_path_buf());
    }
    let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("shot-snap");
    std::fs::create_dir_all(&dir).ok()?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
    let path = dir.join(format!("preview-{stamp}.png"));
    std::fs::write(&path, png).ok()?;
    let mut old: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("preview-")))
        .collect();
    old.sort();
    for stale in old.iter().rev().skip(KEEP_PREVIEWS) {
        let _ = std::fs::remove_file(stale);
    }
    Some(path)
}

/// Announces that `png` is on the clipboard. Runs off the event loop; failures are only logged.
pub fn copied(png: Arc<Vec<u8>>, saved: Option<PathBuf>) {
    std::thread::spawn(move || {
        let mut body = png_size(&png).map(|(w, h)| format!("{w} × {h}")).unwrap_or_default();
        if let Some(path) = &saved {
            body.push_str(&format!("\nSaved to {}", home_relative(path)));
        }
        let mut cmd = Command::new("notify-send");
        cmd.args(["--app-name=shot-snap", "--icon=camera-photo"]);
        if let Some(preview) = preview_file(&png, saved.as_deref()) {
            cmd.arg(format!("--hint=string:image-path:{}", preview.display()));
        }
        cmd.args(["Copied to clipboard", &body]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        match cmd.status() {
            Ok(status) if status.success() => {}
            Ok(status) => log::warn!("notify-send exited with {status}"),
            Err(e) => log::warn!("couldn't send a notification (is notify-send installed?): {e}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_png_size_from_header() {
        let img = crate::image::Image::new(37, 11);
        let png = crate::image::encode_png(&img).unwrap();
        assert_eq!(png_size(&png), Some((37, 11)));
        assert_eq!(png_size(&png[..10]), None);
    }
}
