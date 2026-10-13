//! Window geometry from the compositor, for window capture. Wayland gives clients no way to see
//! other windows, so like `focused.rs` this asks Hyprland, Sway or Niri directly. A window is
//! captured as the screen area it covers (what's visible there), as Noctalia's window mode did.

use std::collections::HashMap;
use std::process::Command;

use anyhow::{bail, Context};
use serde_json::Value;

use crate::capture::OutputMeta;
use crate::geom::Rect;

#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub title: String,
    /// Global logical coordinates.
    pub rect: Rect,
    pub focused: bool,
}

fn json(cmd: &str, args: &[&str]) -> anyhow::Result<Value> {
    let out = Command::new(cmd).args(args).output().with_context(|| format!("running {cmd}"))?;
    if !out.status.success() {
        bail!("{cmd} {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    serde_json::from_slice(&out.stdout).with_context(|| format!("parsing {cmd} output"))
}

/// Windows currently on screen, topmost first, so the first one under a point is the one you see.
pub fn visible_windows(outputs: &[OutputMeta]) -> anyhow::Result<Vec<Window>> {
    let windows = if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        hyprland(&json("hyprctl", &["clients", "-j"])?, &json("hyprctl", &["monitors", "-j"])?, &json("hyprctl", &["activewindow", "-j"]).unwrap_or(Value::Null))
    } else if std::env::var_os("SWAYSOCK").is_some() {
        sway(&json("swaymsg", &["-t", "get_tree", "-r"])?)
    } else if std::env::var_os("NIRI_SOCKET").is_some() {
        let positions: HashMap<String, (f64, f64)> = outputs.iter().map(|o| (o.name.clone(), (o.logical.x, o.logical.y))).collect();
        niri(&json("niri", &["msg", "--json", "windows"])?, &json("niri", &["msg", "--json", "workspaces"])?, &positions)
    } else {
        bail!("window capture needs Hyprland, Sway or Niri (other compositors don't expose window positions)");
    };
    Ok(windows.into_iter().filter(|w| w.rect.w >= 1.0 && w.rect.h >= 1.0).collect())
}

fn pair(v: &Value) -> Option<(f64, f64)> {
    Some((v.get(0)?.as_f64()?, v.get(1)?.as_f64()?))
}

fn hyprland(clients: &Value, monitors: &Value, active: &Value) -> Vec<Window> {
    let mut shown: Vec<i64> = Vec::new();
    for m in monitors.as_array().into_iter().flatten() {
        for key in ["activeWorkspace", "specialWorkspace"] {
            if let Some(id) = m[key]["id"].as_i64().filter(|id| *id != 0) {
                shown.push(id);
            }
        }
    }
    let active_address = active["address"].as_str();
    let mut windows: Vec<(i64, bool, i64, Window)> = clients
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["mapped"].as_bool() != Some(false) && c["hidden"].as_bool() != Some(true))
        // Only the workspaces on screen. (Hyprland's own `visible` flag is true for windows on
        // hidden workspaces too, so it can't be used for this.)
        .filter(|c| c["workspace"]["id"].as_i64().is_some_and(|id| shown.contains(&id)))
        .filter_map(|c| {
            let (x, y) = pair(&c["at"])?;
            let (w, h) = pair(&c["size"])?;
            let window = Window {
                title: c["title"].as_str().unwrap_or_default().to_owned(),
                rect: Rect { x, y, w, h },
                focused: active_address.is_some() && c["address"].as_str() == active_address,
            };
            // Stacking: special workspace over normal, floating over tiled, then most recent.
            let special = c["workspace"]["id"].as_i64().is_some_and(|id| id < 0);
            Some((-(special as i64), !c["floating"].as_bool().unwrap_or(false), c["focusHistoryID"].as_i64().unwrap_or(i64::MAX), window))
        })
        .collect();
    windows.sort_by_key(|(special, tiled, recency, _)| (*special, *tiled, *recency));
    windows.into_iter().map(|(.., w)| w).collect()
}

fn sway(tree: &Value) -> Vec<Window> {
    fn walk(node: &Value, floating: bool, out: &mut Vec<(bool, Window)>) {
        let is_view = node.get("pid").is_some_and(|p| !p.is_null()) || node.get("app_id").is_some_and(|a| !a.is_null());
        if is_view && node["visible"].as_bool() == Some(true) {
            let r = &node["rect"];
            if let (Some(x), Some(y), Some(w), Some(h)) = (r["x"].as_f64(), r["y"].as_f64(), r["width"].as_f64(), r["height"].as_f64()) {
                let title = node["name"].as_str().unwrap_or_default().to_owned();
                out.push((floating, Window { title, rect: Rect { x, y, w, h }, focused: node["focused"].as_bool() == Some(true) }));
            }
        }
        for child in node["nodes"].as_array().into_iter().flatten() {
            walk(child, floating, out);
        }
        for child in node["floating_nodes"].as_array().into_iter().flatten() {
            walk(child, true, out);
        }
    }
    let mut found = Vec::new();
    walk(tree, false, &mut found);
    // Floating windows sit on top; Sway lists later floating windows above earlier ones.
    let (floating, tiled): (Vec<_>, Vec<_>) = found.into_iter().partition(|(f, _)| *f);
    floating.into_iter().rev().chain(tiled).map(|(_, w)| w).collect()
}

fn niri(windows: &Value, workspaces: &Value, outputs: &HashMap<String, (f64, f64)>) -> Vec<Window> {
    // Workspace id -> its output's position, for workspaces currently shown.
    let shown: HashMap<u64, (f64, f64)> = workspaces
        .as_array()
        .into_iter()
        .flatten()
        .filter(|ws| ws["is_active"].as_bool() == Some(true))
        .filter_map(|ws| Some((ws["id"].as_u64()?, *outputs.get(ws["output"].as_str()?)?)))
        .collect();
    let mut found: Vec<(bool, bool, Window)> = windows
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| {
            let origin = shown.get(&w["workspace_id"].as_u64()?)?;
            let layout = &w["layout"];
            // Off-screen columns have no position in the workspace view.
            let (tx, ty) = pair(&layout["tile_pos_in_workspace_view"])?;
            let (ox, oy) = pair(&layout["window_offset_in_tile"]).unwrap_or((0.0, 0.0));
            let (ww, wh) = pair(&layout["window_size"]).or_else(|| pair(&layout["tile_size"]))?;
            let window = Window {
                title: w["title"].as_str().unwrap_or_default().to_owned(),
                rect: Rect { x: origin.0 + tx + ox, y: origin.1 + ty + oy, w: ww, h: wh },
                focused: w["is_focused"].as_bool() == Some(true),
            };
            Some((!w["is_floating"].as_bool().unwrap_or(false), !window.focused, window))
        })
        .collect();
    found.sort_by_key(|(tiled, unfocused, _)| (*tiled, *unfocused));
    found.into_iter().map(|(.., w)| w).collect()
}

/// The window under `p`: the topmost one containing it.
pub fn at(windows: &[Window], p: (f64, f64)) -> Option<usize> {
    windows.iter().position(|w| w.rect.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hyprland_keeps_shown_workspaces_and_stacks_floating_on_top() {
        let clients = json!([
            {"address": "0xa", "mapped": true, "hidden": false, "at": [0, 0], "size": [800, 600], "workspace": {"id": 1}, "floating": false, "focusHistoryID": 1, "title": "tiled"},
            {"address": "0xb", "mapped": true, "hidden": false, "at": [100, 100], "size": [200, 150], "workspace": {"id": 1}, "floating": true, "focusHistoryID": 2, "title": "float"},
            {"address": "0xc", "mapped": true, "hidden": false, "visible": true, "at": [0, 0], "size": [800, 600], "workspace": {"id": 2}, "floating": false, "focusHistoryID": 0, "title": "elsewhere"},
            {"address": "0xd", "mapped": true, "hidden": true, "at": [0, 0], "size": [800, 600], "workspace": {"id": 1}, "floating": false, "focusHistoryID": 3, "title": "hidden"}
        ]);
        let monitors = json!([{"activeWorkspace": {"id": 1}, "specialWorkspace": {"id": 0}}]);
        let ws = hyprland(&clients, &monitors, &json!({"address": "0xa"}));
        let titles: Vec<_> = ws.iter().map(|w| w.title.as_str()).collect();
        assert_eq!(titles, ["float", "tiled"]);
        assert!(ws[1].focused && !ws[0].focused);
        assert_eq!(at(&ws, (150.0, 150.0)), Some(0), "the floating window is on top");
        assert_eq!(at(&ws, (700.0, 500.0)), Some(1));
        assert_eq!(at(&ws, (900.0, 900.0)), None);
    }

    #[test]
    fn sway_walks_the_tree_for_visible_views() {
        let tree = json!({"nodes": [{"nodes": [
            {"pid": 1, "visible": true, "focused": true, "name": "term", "rect": {"x": 0, "y": 0, "width": 960, "height": 1080}, "nodes": [], "floating_nodes": []},
            {"pid": 2, "visible": false, "name": "behind", "rect": {"x": 0, "y": 0, "width": 960, "height": 1080}, "nodes": [], "floating_nodes": []}
        ], "floating_nodes": [
            {"app_id": "calc", "visible": true, "name": "calc", "rect": {"x": 50, "y": 50, "width": 300, "height": 200}, "nodes": [], "floating_nodes": []}
        ]}]});
        let ws = sway(&tree);
        let titles: Vec<_> = ws.iter().map(|w| w.title.as_str()).collect();
        assert_eq!(titles, ["calc", "term"]);
        assert!(ws[1].focused);
    }

    #[test]
    fn niri_places_windows_on_their_output() {
        let windows = json!([
            {"title": "a", "workspace_id": 7, "is_focused": true, "is_floating": false,
             "layout": {"tile_pos_in_workspace_view": [16.0, 16.0], "window_offset_in_tile": [2.0, 2.0], "window_size": [900, 700], "tile_size": [904, 704]}},
            {"title": "offscreen", "workspace_id": 7, "is_focused": false, "is_floating": false,
             "layout": {"tile_pos_in_workspace_view": null, "window_size": [900, 700]}},
            {"title": "other ws", "workspace_id": 8, "is_focused": false, "is_floating": false,
             "layout": {"tile_pos_in_workspace_view": [0.0, 0.0], "window_size": [900, 700]}}
        ]);
        let workspaces = json!([{"id": 7, "output": "DP-2", "is_active": true}, {"id": 8, "output": "DP-2", "is_active": false}]);
        let outputs = HashMap::from([("DP-2".to_owned(), (1920.0, 0.0))]);
        let ws = niri(&windows, &workspaces, &outputs);
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].rect, Rect { x: 1938.0, y: 18.0, w: 900.0, h: 700.0 });
    }
}
