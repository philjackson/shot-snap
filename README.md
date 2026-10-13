# shot-snap

Fast screenshots for Wayland, with a built-in annotation editor.

Works on compositors with `wlr-layer-shell` and `wlr-screencopy`: Hyprland, Sway, Niri, river and
others.

## Usage

```sh
shot-snap region             # drag to select an area
shot-snap fullscreen         # the focused monitor
shot-snap fullscreen pick    # choose a monitor
shot-snap fullscreen DP-2    # a specific monitor
shot-snap fullscreen all     # all monitors in one image
shot-snap window             # the focused window
shot-snap window pick        # click a window
```

Add `-a` (`--annotate`) to any of these to edit the screenshot before it's saved.

Screenshots are saved to your Pictures folder and copied to the clipboard. The saved path is
printed. The exit status is `0` on success, `1` on error and `2` if you cancel.

shot-snap keeps a small background process running so captures are instant. It starts
automatically; after updating shot-snap, run `shot-snap --restart-daemon` once.

### Selecting a region

Drag to select. Press Esc to cancel.

With `confirm_region` or `remember_last_region` on, the selection stays adjustable: drag its edges
or corners to resize it, drag inside to move it, or drag outside to start again. Then:

| Key | Action |
|---|---|
| Enter / Space | Capture |
| Ctrl+C | Copy only |
| Ctrl+S | Save only |

### Windows

`window pick` highlights the window under the pointer; click to capture it. Where the compositor
supports it (such as Hyprland), you get just that window even if others overlap it, without its
border or shadow. Otherwise the area of the screen it covers is captured. Window capture works on
Hyprland, Sway and Niri.

### Example key bindings (Hyprland)

```ini
bind = , Print, exec, shot-snap region
bind = SHIFT, Print, exec, shot-snap fullscreen
bind = CTRL, Print, exec, shot-snap fullscreen pick
bind = ALT, Print, exec, shot-snap region --annotate
bind = SUPER, Print, exec, shot-snap window pick
```

## Annotating

The editor opens with your screenshot under a toolbar. Hover over any button for a tip.

| Tool | Key | How |
|---|---|---|
| Rectangle | `s` | Drag |
| Arrow | `a` | Drag from tail to tip. Hold the button to put heads on the end, the start or both |
| Text | `t` | Click and type. Enter finishes, Esc discards |
| Highlight | `h` | Drag a box. Hold the button to pick its colour |
| Blur | `u` | Drag a box over anything you want hidden |
| Numbering | `n` | Click to place 1, 2, 3… |
| Crop | `c` | Drag the frame's edges or corners. Enter applies it, Esc resets it |

S / M / L set the size of the current tool; the sliders button next to them, or `[` and `]`,
gives finer control.

| Key | Action |
|---|---|
| Enter / ✓ | Save and copy |
| Ctrl+C | Copy |
| Ctrl+S | Save |
| Ctrl+Z / Ctrl+Shift+Z | Undo / redo |
| Esc / ✕ | Close without saving |

### Colours

The colour button opens a picker with two tabs:

- **Solid**: a few presets, your recent colours, and a picker for anything else.
- **Gradient**: presets, your recent gradients, and an editor. Click the bar to add a colour stop,
  drag a stop to move it, drag it off the bar (or press Delete) to remove it, and use the slider
  to set the angle.

### Screenshot style

The ✦ button frames the screenshot for sharing: a padded background with rounded corners and a
drop shadow. Click the swatch next to **Background** to choose its colour or gradient, and use
the sliders for padding, corners and the shadow. With a crop, the frame goes around the cropped
result.

Your tools, colours and style settings are remembered for next time.

## Configuration

Optional, in `~/.config/shot-snap/config.toml`:

```toml
save_to_file         = true    # save a PNG
directory            = ""      # default: your Pictures folder
filename_pattern     = ""      # strftime pattern; default: screenshot_%Y%m%d_%H%M%S
copy_to_clipboard    = true
freeze_screen        = true    # freeze the screen while selecting
confirm_region       = false   # adjust the selection before capturing
remember_last_region = false   # start with the previous selection
show_cursor          = false   # include the mouse pointer
pipe_command         = ""      # e.g. 'satty -f -'; the PNG is sent on stdin

[theme]
primary         = "#fff59b"
on_primary      = "#0e0e43"
surface         = "#070722"
on_surface      = "#f3edf7"
surface_variant = "#11112d"
outline         = "#21215f"
hover           = "#9bfece"
on_hover        = "#0e0e43"
dim_opacity     = 0.65
font            = "sans-serif:bold"
```

`pipe_command` also gets the screenshot's file path in `$SHOTSNAP_SCREENSHOT_PATH`.

## Building

```sh
cargo build --release
```

Requires wayland, libxkbcommon, mesa (EGL) and fontconfig.
