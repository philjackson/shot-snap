//! The camera-shutter sound when a screenshot is saved: the sound theme's `screen-capture` event,
//! played with PipeWire's `pw-play` (or `paplay`). Entirely best-effort: a missing sound, player
//! or audio server is logged and never affects the screenshot.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A player command and how it takes a 0..1 volume.
type Player = (&'static str, fn(f32) -> String);

/// Players to try, in order.
const PLAYERS: [Player; 2] = [
    ("pw-play", |v| format!("--volume={v}")),
    // paplay's volume is 0..65536.
    ("paplay", |v| format!("--volume={}", (v * 65536.0) as u32)),
];

/// Where XDG sound themes live, most specific first.
fn sound_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(home) => dirs.push(PathBuf::from(home).join("sounds")),
        None => {
            if let Some(home) = std::env::var_os("HOME") {
                dirs.push(PathBuf::from(home).join(".local/share/sounds"));
            }
        }
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(data_dirs.split(':').map(|d| PathBuf::from(d).join("sounds")));
    dirs
}

/// The `screen-capture` sound of `theme` under `dirs`, falling back to the freedesktop theme.
fn find_in(dirs: &[PathBuf], theme: &str) -> Option<PathBuf> {
    let themes = if theme == "freedesktop" { vec![theme] } else { vec![theme, "freedesktop"] };
    themes.into_iter().find_map(|theme| {
        dirs.iter().find_map(|dir| {
            ["stereo", ""].iter().find_map(|sub| {
                ["oga", "ogg", "wav"].iter().map(|ext| dir.join(theme).join(sub).join(format!("screen-capture.{ext}"))).find(|p| p.is_file())
            })
        })
    })
}

/// Plays `file` with the first player that works. Blocks until it finishes.
fn play(file: &Path, volume: f32, players: &[Player]) -> Result<(), String> {
    let mut problems = Vec::new();
    for (player, volume_arg) in players {
        let status = Command::new(player).arg(volume_arg(volume)).arg(file).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
        match status {
            Ok(s) if s.success() => return Ok(()),
            // Installed but failed (e.g. no PipeWire running): try the next one.
            Ok(s) => problems.push(format!("{player} exited with {s}")),
            Err(e) => problems.push(format!("{player}: {e}")),
        }
    }
    Err(problems.join("; "))
}

/// Plays the capture sound in the background. Never fails the caller: problems are only logged.
pub fn capture(theme: &str, volume: f32) {
    let Some(file) = find_in(&sound_dirs(), theme) else {
        log::debug!("no screen-capture sound in the {theme:?} sound theme; staying quiet");
        return;
    };
    let volume = volume.clamp(0.0, 1.0);
    let spawned = std::thread::Builder::new().name("capture-sound".into()).spawn(move || {
        if let Err(e) = play(&file, volume, &PLAYERS) {
            log::info!("couldn't play the capture sound ({e})");
        }
    });
    if let Err(e) = spawned {
        log::info!("couldn't start the capture sound: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shot-snap-sound-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn no_sound_theme_means_no_sound() {
        let empty = temp_dir("empty");
        assert_eq!(find_in(&[empty.clone(), PathBuf::from("/nonexistent/sounds")], "freedesktop"), None);
        assert_eq!(find_in(&[], "ocean"), None);
        let _ = std::fs::remove_dir_all(empty);
    }

    #[test]
    fn finds_the_theme_sound_then_falls_back_to_freedesktop() {
        let dir = temp_dir("themes");
        let fd = dir.join("freedesktop/stereo");
        std::fs::create_dir_all(&fd).unwrap();
        std::fs::write(fd.join("screen-capture.oga"), b"x").unwrap();
        // A theme without the sound falls back to freedesktop's.
        assert_eq!(find_in(std::slice::from_ref(&dir), "ocean"), Some(fd.join("screen-capture.oga")));
        // A theme with its own sound uses it.
        let ocean = dir.join("ocean/stereo");
        std::fs::create_dir_all(&ocean).unwrap();
        std::fs::write(ocean.join("screen-capture.wav"), b"x").unwrap();
        assert_eq!(find_in(std::slice::from_ref(&dir), "ocean"), Some(ocean.join("screen-capture.wav")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_or_failing_players_are_an_error_not_a_panic() {
        let missing: [Player; 2] = [("shot-snap-no-such-player", |_| String::new()), ("another-missing-player", |_| String::new())];
        let err = play(Path::new("/nonexistent.oga"), 0.5, &missing).unwrap_err();
        assert!(err.contains("shot-snap-no-such-player") && err.contains("another-missing-player"), "{err}");
        // A player that runs but fails moves on to the next.
        let failing: [Player; 2] = [("false", |_| String::new()), ("true", |_| String::new())];
        assert_eq!(play(Path::new("/nonexistent.oga"), 0.5, &failing), Ok(()));
    }
}
