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
    /// `annotate`: open the editor on the captured image before delivering it.
    Region { annotate: bool },
    Fullscreen { target: FullscreenTarget, annotate: bool },
    /// The focused window, or (`pick`) one chosen by clicking.
    Window { pick: bool, annotate: bool },
    Ping,
    Quit,
}

impl Request {
    pub fn encode(&self) -> String {
        let flag = |annotate: bool| if annotate { " --annotate" } else { "" };
        match self {
            Request::Region { annotate } => format!("region{}", flag(*annotate)),
            Request::Fullscreen { target, annotate } => {
                let target = match target {
                    FullscreenTarget::Focused => String::new(),
                    FullscreenTarget::Pick => " pick".into(),
                    FullscreenTarget::All => " all".into(),
                    FullscreenTarget::Named(n) => format!(" {n}"),
                };
                format!("fullscreen{target}{}", flag(*annotate))
            }
            Request::Window { pick, annotate } => format!("window{}{}", if *pick { " pick" } else { "" }, flag(*annotate)),
            Request::Ping => "ping".into(),
            Request::Quit => "quit".into(),
        }
    }

    pub fn parse(args: &[&str]) -> anyhow::Result<Self> {
        let annotate = args.iter().any(|a| matches!(*a, "--annotate" | "-a"));
        let rest: Vec<&str> = args.iter().copied().filter(|a| !matches!(*a, "--annotate" | "-a")).collect();
        let fullscreen = |target| Request::Fullscreen { target, annotate };
        Ok(match rest.as_slice() {
            ["region"] => Request::Region { annotate },
            ["fullscreen"] => fullscreen(FullscreenTarget::Focused),
            ["fullscreen", "pick"] => fullscreen(FullscreenTarget::Pick),
            ["fullscreen", "all" | "*"] => fullscreen(FullscreenTarget::All),
            ["fullscreen", name] if !name.starts_with('-') => fullscreen(FullscreenTarget::Named((*name).to_owned())),
            ["window"] => Request::Window { pick: false, annotate },
            ["window", "pick"] => Request::Window { pick: true, annotate },
            ["ping"] if !annotate => Request::Ping,
            ["quit"] if !annotate => Request::Quit,
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
    // An explicit socket, e.g. to run a second daemon for testing alongside your own.
    if let Some(path) = std::env::var_os("SHOTSNAP_SOCKET").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    // Use only the socket's file name so absolute WAYLAND_DISPLAY paths still give a sane name.
    let display = display.rsplit('/').next().unwrap_or("wayland-0").to_owned();
    dir.join(format!("shot-snap-{display}.sock"))
}

fn connect() -> std::io::Result<UnixStream> {
    UnixStream::connect(socket_path())
}

/// Stops the running daemon (if any), waits for it to go, and starts a fresh one from this
/// binary, so a rebuilt shot-snap takes effect without a manual `quit`.
pub fn restart_daemon() -> anyhow::Result<()> {
    if connect().is_ok() {
        match send(&Request::Quit)? {
            Response::Done(_) => {}
            other => bail!("daemon didn't quit: {other:?}"),
        }
        // It removes its socket on the way out; wait until nothing answers any more.
        let deadline = Instant::now() + Duration::from_secs(3);
        while connect().is_ok() {
            if Instant::now() >= deadline {
                bail!("the old daemon is still running");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    drop(spawn_daemon_and_connect()?);
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotate_flag_round_trips() {
        for args in [&["region"][..], &["region", "--annotate"], &["fullscreen", "-a"], &["fullscreen", "DP-1", "--annotate"], &["fullscreen", "--annotate", "pick"], &["fullscreen", "all"], &["window"], &["window", "pick", "-a"]] {
            let req = Request::parse(args).unwrap();
            let encoded = req.encode();
            let reparsed = Request::parse(&encoded.split_whitespace().collect::<Vec<_>>()).unwrap();
            assert_eq!(req, reparsed, "{args:?}");
        }
        assert_eq!(Request::parse(&["region", "-a"]).unwrap(), Request::Region { annotate: true });
        assert_eq!(
            Request::parse(&["fullscreen", "--annotate", "pick"]).unwrap(),
            Request::Fullscreen { target: FullscreenTarget::Pick, annotate: true }
        );
        assert_eq!(Request::parse(&["window", "-a", "pick"]).unwrap(), Request::Window { pick: true, annotate: true });
        assert!(Request::parse(&["window", "DP-1"]).is_err());
        assert!(Request::parse(&["annotate"]).is_err());
        assert!(Request::parse(&["quit", "--annotate"]).is_err());
    }
}
