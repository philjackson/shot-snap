mod annotate;
mod app;
mod capture;
mod clipboard;
mod config;
mod deliver;
mod focused;
mod geom;
mod gl;
mod hyprland_proto;
mod image;
mod ipc;
mod notify;
mod overlay;
mod paint;
mod picker;
mod render;
mod sound;
mod state;
mod style;
mod style_panel;
mod windows;
mod toplevel;
mod ui;

use std::process::ExitCode;

use ipc::{Request, Response};

const USAGE: &str = "\
shot-snap: Wayland screenshots

USAGE:
    shot-snap region                   Drag to select a region
    shot-snap fullscreen               Capture the focused monitor
    shot-snap fullscreen pick          Choose a monitor (multi-monitor setups)
    shot-snap fullscreen <OUTPUT>      Capture one output (connector name, or a word from its description)
    shot-snap fullscreen all           One image of every monitor
    shot-snap window                   Capture the focused window
    shot-snap window pick              Click a window to capture it
    shot-snap daemon                   Run the daemon in the foreground (started automatically otherwise)
    shot-snap quit                     Stop the daemon

OPTIONS (region, fullscreen and window):
    -a, --annotate                     Open the editor on the capture before saving or copying

    --restart-daemon                   Restart the daemon first (e.g. after rebuilding); works
                                       alone or with any command

Prints the saved file path, if any. Exit status: 0 done, 1 error, 2 cancelled.
Config: ~/.config/shot-snap/config.toml";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["daemon"] => {
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
            match app::run_daemon() {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    log::error!("{e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        [] | ["-h" | "--help" | "help"] => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            // --restart-daemon can go anywhere, alone or with a command.
            let restart = args.contains(&"--restart-daemon");
            let args: Vec<&str> = args.into_iter().filter(|a| *a != "--restart-daemon").collect();
            if restart {
                if let Err(e) = ipc::restart_daemon() {
                    eprintln!("shot-snap: restarting the daemon: {e:#}");
                    return ExitCode::FAILURE;
                }
                if args.is_empty() {
                    return ExitCode::SUCCESS;
                }
            }
            let request = match Request::parse(&args) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("shot-snap: {e}\n\n{USAGE}");
                    return ExitCode::FAILURE;
                }
            };
            match ipc::send(&request) {
                Ok(Response::Done(path)) => {
                    if let Some(path) = path {
                        println!("{}", path.display());
                    }
                    ExitCode::SUCCESS
                }
                Ok(Response::Cancelled) => ExitCode::from(2),
                Ok(Response::Error(e)) => {
                    eprintln!("shot-snap: {e}");
                    ExitCode::FAILURE
                }
                Err(e) => {
                    eprintln!("shot-snap: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}
