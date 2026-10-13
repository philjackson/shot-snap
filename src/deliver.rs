use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use anyhow::Context;

use crate::config::Config;
use crate::image::{encode_png, Image};

pub const PATH_ENV: &str = "SHOTSNAP_SCREENSHOT_PATH";

/// What the user confirmed with: Enter/Space/release use the configured policy, Ctrl+C and
/// Ctrl+S do only that one thing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Default,
    Copy,
    Save,
}

#[derive(Debug, Clone, Copy)]
pub struct Plan {
    pub save: bool,
    pub copy: bool,
    pub pipe: bool,
}

impl Plan {
    pub fn new(config: &Config, action: Action) -> Self {
        match action {
            Action::Default => Plan {
                save: config.save_to_file,
                copy: config.copy_to_clipboard,
                pipe: !config.pipe_command.trim().is_empty(),
            },
            Action::Copy => Plan { save: false, copy: true, pipe: false },
            Action::Save => Plan { save: true, copy: false, pipe: false },
        }
    }

    pub fn is_empty(&self) -> bool {
        !(self.save || self.copy || self.pipe)
    }
}

pub struct Delivered {
    pub saved: Option<PathBuf>,
    /// Set when the PNG should go on the clipboard (the main thread owns the selection).
    pub clipboard: Option<Arc<Vec<u8>>>,
}

/// Runs off the main thread: encodes once and fans out to file, pipe and clipboard.
pub fn deliver(image: Image, config: &Config, plan: Plan) -> anyhow::Result<Delivered> {
    let png = Arc::new(encode_png(&image).context("encoding PNG")?);
    let path = unique_path(config);
    let mut saved = None;
    if plan.save {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&path, png.as_slice()).with_context(|| format!("writing {}", path.display()))?;
        log::info!("saved {}", path.display());
        saved = Some(path.clone());
    }
    if plan.pipe {
        pipe(&config.pipe_command, &path, png.clone())?;
    }
    Ok(Delivered { saved, clipboard: plan.copy.then_some(png) })
}

fn unique_path(config: &Config) -> PathBuf {
    let stem = chrono::Local::now().format(config.filename_pattern()).to_string();
    let dir = config.output_directory();
    let mut path = dir.join(format!("{stem}.png"));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{stem}_{n}.png"));
        n += 1;
    }
    path
}

/// The command gets the PNG on stdin and the would-be file path in SHOTSNAP_SCREENSHOT_PATH
/// (e.g. `satty -f - -o "$SHOTSNAP_SCREENSHOT_PATH"`). It's not awaited: editors stay open.
fn pipe(command: &str, path: &std::path::Path, png: Arc<Vec<u8>>) -> anyhow::Result<()> {
    let mut child = Command::new("/bin/sh")
        .args(["-lc", command])
        .env(PATH_ENV, path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .with_context(|| format!("running pipe_command {command:?}"))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    std::thread::spawn(move || {
        // A command that doesn't read all of stdin is fine (EPIPE is ignored: Rust ignores SIGPIPE).
        let _ = stdin.write_all(&png);
        drop(stdin);
        let _ = child.wait();
    });
    Ok(())
}
