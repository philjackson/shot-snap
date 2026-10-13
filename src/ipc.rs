//! Line-based protocol over a Unix socket. The client sends one request line, the daemon answers
//! with one line once the capture has finished, been cancelled or failed.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};

#[derive(Debug, Clone, PartialEq)]
pub enum FullscreenTarget {
    Focused,
    Pick,
    Named(String),
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Region,
    Fullscreen(FullscreenTarget),
    Ping,
    Quit,
}

impl Request {
    pub fn encode(&self) -> String {
        match self {
            Request::Region => "region".into(),
            Request::Fullscreen(FullscreenTarget::Focused) => "fullscreen".into(),
            Request::Fullscreen(FullscreenTarget::Pick) => "fullscreen pick".into(),
            Request::Fullscreen(FullscreenTarget::All) => "fullscreen all".into(),
            Request::Fullscreen(FullscreenTarget::Named(n)) => format!("fullscreen {n}"),
            Request::Ping => "ping".into(),
            Request::Quit => "quit".into(),
        }
    }

    pub fn parse(args: &[&str]) -> anyhow::Result<Self> {
        Ok(match args {
            ["region"] => Request::Region,
            ["fullscreen"] => Request::Fullscreen(FullscreenTarget::Focused),
            ["fullscreen", "pick"] => Request::Fullscreen(FullscreenTarget::Pick),
            ["fullscreen", "all" | "*"] => Request::Fullscreen(FullscreenTarget::All),
            ["fullscreen", name] => Request::Fullscreen(FullscreenTarget::Named((*name).to_owned())),
            ["ping"] => Request::Ping,
            ["quit"] => Request::Quit,
            _ => bail!("unknown command: {}", args.join(" ")),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    /// Path of the saved file, if one was written.
    Done(Option<PathBuf>),
    Cancelled,
    Error(String),
}

impl Response {
    pub fn encode(&self) -> String {
        match self {
            Response::Done(Some(p)) => format!("ok {}\n", p.display()),
            Response::Done(None) => "ok\n".into(),
            Response::Cancelled => "cancelled\n".into(),
            Response::Error(e) => format!("error {}\n", e.replace('\n', " ")),
        }
    }

    fn decode(line: &str) -> Response {
        let line = line.trim_end_matches('\n');
        match line.split_once(' ') {
            Some(("ok", path)) => Response::Done(Some(PathBuf::from(path))),
            Some(("error", msg)) => Response::Error(msg.to_owned()),
            _ if line == "ok" => Response::Done(None),
            _ if line == "cancelled" => Response::Cancelled,
            _ => Response::Error(format!("unexpected reply from daemon: {line:?}")),
        }
    }
}

pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    // Use only the socket's file name so absolute WAYLAND_DISPLAY paths still give a sane name.
    let display = display.rsplit('/').next().unwrap_or("wayland-0").to_owned();
    dir.join(format!("shot-snap-{display}.sock"))
}

fn connect() -> std::io::Result<UnixStream> {
    UnixStream::connect(socket_path())
}

/// Sends a request, starting the daemon first if nothing is listening.
pub fn send(request: &Request) -> anyhow::Result<Response> {
    let stream = match connect() {
        Ok(s) => s,
        Err(_) if *request == Request::Quit => return Ok(Response::Done(None)),
        Err(_) => spawn_daemon_and_connect()?,
    };
    let mut writer = stream.try_clone()?;
    writer.write_all(format!("{}\n", request.encode()).as_bytes())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).context("reading reply from daemon")?;
    if line.is_empty() {
        bail!("daemon closed the connection without replying");
    }
    Ok(Response::decode(&line))
}

fn spawn_daemon_and_connect() -> anyhow::Result<UnixStream> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().context("locating shot-snap executable")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    // New session so the daemon outlives the terminal/keybind that started it.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().context("starting shot-snap daemon")?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match connect() {
            Ok(s) => return Ok(s),
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => bail!("daemon did not start: {e}"),
        }
    }
}
