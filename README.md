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

shot-snap region --annotate      # -a / --annotate: edit the capture before it's saved or copied
shot-snap fullscreen DP-2 -a
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

### Annotation

Add `--annotate` (or `-a`) to `region` or `fullscreen` to open the editor on the captured image
before it's delivered. The editor appears on the monitor the capture came from, with
the image centred under a toolbar at the top (1:1, scaled down if it doesn't fit). Pick a tool and
draw on the image:

- **Rectangle**: drag.
- **Arrow**: drag from tail to tip. Hold the arrow button to choose where the heads go: at the
  end, at the start, or both. The choice is remembered.
- **Highlight**: drag a box; it's filled with the highlighter's own colour at 45% opacity. Yellow
  by default; hold the highlight button to pick yellow, blue, red, green or orange. The choice is
  remembered.
- **Blur**: drag a box; what's under it is blurred (fixed strength: Noctalia's sigma of 32 px).
  Blurs always sit under the other annotations, so blurring a region never hides an arrow or label
  you've added.
- **Numbering**: click to drop a numbered marker: 1, 2, 3… in the order placed. Undo renumbers the
  rest. The size controls set the diameter (S / M / L = 16 / 32 / 64); the number is black on
  light colours and white on dark ones.
- **Text**: click to place a caret and type. Backspace deletes; Enter finishes; Esc throws away
  the text you're typing. Size and colour changes apply to the text under the caret.

Copy, Save and Done (✓) export the capture with the annotations drawn in, and the editor closes.
The export uses the same renderer as the editor, so it matches what you see exactly.

| Key | Action |
|---|---|
| Esc / ✕ | Close the colour panel, or close the editor without saving |
| Enter / ✓ | Done: use the configured outputs |
| Ctrl+C | Copy to clipboard |
| Ctrl+S | Save to file |
| Ctrl+Z / ↶ | Undo the last annotation |
| `s` / `a` / `t` / `h` / `u` / `n` | Rectangle / arrow / text / highlight / blur / numbering tool |
| `[` / `]` | Thinner / thicker by 1px |
| Delete | Remove the selected gradient stop (colour panel open) |

#### Screenshot style

The sparkles button (✦) opens the style panel, after Noctalia's. **Style screenshot** puts the
capture on a padded backdrop with rounded corners and a soft drop shadow, previewed live around the
image. The backdrop uses the colour panel's current colour or gradient (including the gradient's
angle). **Background** turns the backdrop off for a transparent frame. Padding, corner radius and
shadow blur / opacity / X and Y offset are sliders (in image pixels; defaults 64, 12, 40, 50%, 0,
16). Copy, Save and Done deliver the styled image. Settings are remembered in the state file.

#### Toolbar

Undo · Rectangle, arrow, text, highlight, blur, numbering, colour · S / M / L sizes (½×, 1×, 2× the
tool's default: 6px strokes, 24px text, 32px markers; hidden for highlight and blur) · fine size
adjustment (reveals − px +) · Copy, Save, Done · Close. Rest the pointer on a button for its
tooltip. The style button (✦) sits just before Copy. Each tool remembers its own width. The selected tool, the arrow ends and whether the fine
stepper is showing are remembered between runs (all in the state file).

#### Colours and gradients

The swatch button after the tools opens the colour panel. Rectangles, arrows, text and numbered
markers use whatever it shows. Highlight has its own colour and blur needs none, so the button is hidden for those.

- **Solid**: 5 built-in colours, your 10 most recent picks, and a saturation/value square with a
  hue bar for anything else.
- **Gradient**: 5 built-in gradients, your 10 most recent, and an editor. Click the bar to add a
  stop. Drag a stop to move it, or drag it down off the bar (or press Delete) to remove it; there
  are always at least two. Click a stop to recolour it with the square and hue bar. The angle
  slider works like CSS (0° points up, 90° right), in 5° steps. A gradient spans each rectangle's
  outer edge.

A pick counts as recent when you close the panel or the editor; the built-ins are never added.
Recents and the current paint are kept in `$XDG_STATE_HOME/shot-snap/state.toml`
(`~/.local/state/…`), not in the config.

### Hyprland example

```ini
bind = , Print, exec, shot-snap region
bind = SHIFT, Print, exec, shot-snap fullscreen
bind = CTRL, Print, exec, shot-snap fullscreen pick
bind = ALT, Print, exec, shot-snap region --annotate
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
hover           = "#9bfece"
on_hover        = "#0e0e43"
dim_opacity     = 0.65
font            = "sans-serif:bold"   # fontconfig pattern
```

`pipe_command` runs via `/bin/sh -lc` with the PNG on stdin. `SHOTSNAP_SCREENSHOT_PATH` holds the
file path the capture would be saved to, even when `save_to_file` is off.

## Building

```sh
cargo build --release   # needs wayland, libxkbcommon, EGL (mesa) and fontconfig
```
