//! Wayland has no standard "focused output", so ask the compositor directly, like Noctalia does.

use std::process::Command;

use serde_json::Value;

fn json(cmd: &str, args: &[&str]) -> Option<Value> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status.success().then_some(())?;
    serde_json::from_slice(&out.stdout).ok()
}

pub fn focused_output_name() -> Option<String> {
    let name = if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        json("hyprctl", &["monitors", "-j"])?
            .as_array()?
            .iter()
            .find(|m| m["focused"].as_bool() == Some(true))?["name"]
            .as_str()
            .map(str::to_owned)
    } else if std::env::var_os("NIRI_SOCKET").is_some() {
        json("niri", &["msg", "--json", "focused-output"])?["name"].as_str().map(str::to_owned)
    } else if std::env::var_os("SWAYSOCK").is_some() {
        json("swaymsg", &["-t", "get_outputs", "-r"])?
            .as_array()?
            .iter()
            .find(|m| m["focused"].as_bool() == Some(true))?["name"]
            .as_str()
            .map(str::to_owned)
    } else {
        None
    };
    log::debug!("focused output: {name:?}");
    name
}
