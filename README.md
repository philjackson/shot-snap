# shot-snap

Fast Wayland screenshots with the look and feel of the [Noctalia](https://github.com/noctalia-dev) shell's
built-in capture: a frozen, dimmed overlay, drag-to-select, a size badge, and a display picker.

Needs a compositor with `wlr-layer-shell` and `wlr-screencopy` (Hyprland, Niri, Sway, river, …).
Clipboard copies need `ext-data-control` or `wlr-data-control`.

## Usage

```sh
shot-snap region                 # drag to select; release to capture
shot-snap fullscreen             # focused monitor
shot-snap fullscreen pick        # choose a monitor (immediate on single-monitor setups)
shot-snap fullscreen DP-2        # one output: connector name, or a word from its description
shot-snap fullscreen all         # every monitor in one image
shot-snap quit                   # stop the daemon
```

The first command starts a small background daemon (`shot-snap daemon`) that stays resident, so
later captures are instant and the clipboard keeps serving the image after the overlay closes.
Run `shot-snap daemon` yourself (e.g. `exec-once`) to start it early or to see its logs.

The client prints the saved path (if any) and exits `0` done, `1` error, `2` cancelled.

### Keys in the region overlay

| Key | Action |
|---|---|
| Esc | Cancel |
| Enter / Space | Capture with the configured outputs (when confirming a region) |
| Ctrl+C | Copy to clipboard only (when confirming a region) |
| Ctrl+S | Save to file only (when confirming a region) |

While confirming, drag the edges or corners to resize, drag inside to move, or drag outside to
start again.

### Hyprland example

```ini
bind = , Print, exec, shot-snap region
bind = SHIFT, Print, exec, shot-snap fullscreen
bind = CTRL, Print, exec, shot-snap fullscreen pick
```

## Configuration

`~/.config/shot-snap/config.toml` (all optional; re-read on every capture):

```toml
save_to_file         = true    # write a PNG
directory            = ""      # empty = XDG_PICTURES_DIR, else ~/Pictures
filename_pattern     = ""      # strftime; empty = screenshot_%Y%m%d_%H%M%S (".png" appended)
copy_to_clipboard    = true
freeze_screen        = true    # freeze the desktop before selecting
confirm_region       = false   # keep the region adjustable until Enter/Space/Ctrl+C/Ctrl+S
remember_last_region = false   # pre-select the previous region (implies confirm)
show_cursor          = false   # include the mouse pointer
pipe_command         = ""      # e.g. 'satty -f - -o "$SHOTSNAP_SCREENSHOT_PATH"'

[theme]                        # defaults: Noctalia's built-in palette
primary         = "#fff59b"
on_primary      = "#0e0e43"
surface         = "#070722"
on_surface      = "#f3edf7"
surface_variant = "#11112d"
outline         = "#21215f"
dim_opacity     = 0.65
font            = "sans-serif:bold"   # fontconfig pattern
```

`pipe_command` runs via `/bin/sh -lc` with the PNG on stdin. `SHOTSNAP_SCREENSHOT_PATH` holds the
file path the capture would be saved to, even when `save_to_file` is off.

## Building

```sh
cargo build --release   # needs wayland, libxkbcommon, EGL (mesa) and fontconfig
```
