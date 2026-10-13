//! The annotation colour panel: Solid or Gradient, each with 5 built-in defaults, the 10 most
//! recent picks, and an editor (saturation/value square + hue bar; for gradients also a stop bar
//! and an angle slider). Immediate mode: `draw` lays the panel out and keeps the hit boxes that
//! the input handlers use.

use femtovg::renderer::OpenGl;
use femtovg::{Canvas, FontId, Paint, Path};

use crate::config::{Color, Theme};
use crate::geom::Rect;
use crate::paint::{default_gradients, Fill, Gradient, Hsv, Stop, DEFAULT_SOLIDS};
use crate::state::AnnotateState;
use crate::ui::{card, text_paint, text_width};

const PAD: f32 = 12.0;
const CONTENT_W: f32 = 254.0;
const SWATCH: f32 = 20.0;
const GAP: f32 = 6.0;
const TAB_H: f32 = 28.0;
const LABEL_H: f32 = 18.0;
const BAR_H: f32 = 22.0;
const HANDLE: f32 = 12.0;
const HUE_H: f32 = 14.0;
const FOOT_H: f32 = 22.0;
const CAPTION: f32 = 12.0;
const RADIUS_XL: f32 = 12.0;
const RADIUS_MD: f32 = 6.0;
const RADIUS_SM: f32 = 3.0;
/// Dragging a stop this far below the bar removes it on release.
const REMOVE_DISTANCE: f64 = 36.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tab {
    Solid,
    Gradient,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    Sv,
    Hue,
    Stop { index: usize, removing: bool },
    Angle,
}

#[derive(Debug, Clone, Default)]
struct Layout {
    panel: Rect,
    tabs: [Rect; 2],
    defaults_label: f64,
    defaults: Vec<Rect>,
    recents_label: f64,
    recents: Vec<Rect>,
    bar: Option<Rect>,
    handles: Vec<Rect>,
    sv: Rect,
    hue: Rect,
    footer: Rect,
    angle: Option<Rect>,
}

pub struct Picker {
    pub open: bool,
    tab: Tab,
    state: AnnotateState,
    /// What each tab returns to when switched back to.
    last_solid: Color,
    last_gradient: Gradient,
    /// Colour being edited: the solid, or the selected gradient stop.
    hsv: Hsv,
    stop: usize,
    drag: Option<Drag>,
    layout: Layout,
    /// Something was picked since the last time we recorded it.
    touched: bool,
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x: f64::from(x), y: f64::from(y), w: f64::from(w), h: f64::from(h) }
}

fn inflate(r: Rect, by: f64) -> Rect {
    Rect { x: r.x - by, y: r.y - by, w: r.w + 2.0 * by, h: r.h + 2.0 * by }
}

fn f(r: Rect) -> (f32, f32, f32, f32) {
    (r.x as f32, r.y as f32, r.w as f32, r.h as f32)
}

impl Picker {
    pub fn new(state: AnnotateState) -> Self {
        let last_solid = match &state.fill {
            Fill::Solid(c) => *c,
            Fill::Gradient(_) => state.recent_solids.first().copied().unwrap_or(DEFAULT_SOLIDS[0]),
        };
        let last_gradient = match &state.fill {
            Fill::Gradient(g) => g.clone(),
            Fill::Solid(_) => state.recent_gradients.first().cloned().unwrap_or_else(|| default_gradients()[0].clone()),
        };
        let tab = if matches!(state.fill, Fill::Gradient(_)) { Tab::Gradient } else { Tab::Solid };
        let mut picker = Picker {
            open: false,
            tab,
            state,
            last_solid,
            last_gradient,
            hsv: Hsv { h: 0.0, s: 0.0, v: 0.0 },
            stop: 0,
            drag: None,
            layout: Layout::default(),
            touched: false,
        };
        picker.sync_hsv();
        picker
    }

    pub fn fill(&self) -> &Fill {
        &self.state.fill
    }

    /// The whole annotation state; the editor keeps tool widths in it too.
    pub fn state(&self) -> &AnnotateState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut AnnotateState {
        &mut self.state
    }

    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open = true;
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.drag = None;
        self.commit();
    }

    /// Records the current fill as a recent pick if the user picked something.
    fn commit(&mut self) {
        if std::mem::take(&mut self.touched) {
            self.state.remember_fill();
        }
    }

    /// The state to persist when the editor closes.
    pub fn finish(mut self) -> AnnotateState {
        self.commit();
        self.state
    }

    pub fn contains(&self, p: (f64, f64)) -> bool {
        self.open && self.layout.panel.contains(p)
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    // ---- model ---------------------------------------------------------------------------

    fn sync_hsv(&mut self) {
        let c = match self.tab {
            Tab::Solid => self.last_solid,
            Tab::Gradient => self.last_gradient.stops.get(self.stop).map_or(DEFAULT_SOLIDS[0], |s| s.color),
        };
        let hsv = Hsv::from_color(c);
        // Greys have no hue of their own; keep the slider where the user left it.
        self.hsv = if hsv.s == 0.0 || hsv.v == 0.0 { Hsv { h: self.hsv.h, ..hsv } } else { hsv };
    }

    fn sync_fill(&mut self) {
        self.state.fill = match self.tab {
            Tab::Solid => Fill::Solid(self.last_solid),
            Tab::Gradient => Fill::Gradient(self.last_gradient.clone()),
        };
        self.touched = true;
    }

    fn apply_hsv(&mut self) {
        let c = self.hsv.color();
        match self.tab {
            Tab::Solid => self.last_solid = c,
            Tab::Gradient => {
                if let Some(s) = self.last_gradient.stops.get_mut(self.stop) {
                    s.color = c;
                }
            }
        }
        self.sync_fill();
    }

    fn set_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.stop = 0;
            self.sync_hsv();
            self.sync_fill();
        }
    }

    fn choose_solid(&mut self, c: Color) {
        self.last_solid = c;
        self.sync_hsv();
        self.sync_fill();
    }

    fn choose_gradient(&mut self, g: Gradient) {
        self.last_gradient = g;
        self.stop = 0;
        self.sync_hsv();
        self.sync_fill();
    }

    pub fn delete_stop(&mut self) {
        if self.open && self.tab == Tab::Gradient && self.last_gradient.stops.len() > 2 {
            self.last_gradient.stops.remove(self.stop);
            self.stop = self.stop.min(self.last_gradient.stops.len() - 1);
            self.sync_hsv();
            self.sync_fill();
        }
    }

    // ---- input ---------------------------------------------------------------------------

    /// Handles a press at surface-local `p`. Returns false if it was outside the panel.
    pub fn press(&mut self, p: (f64, f64)) -> bool {
        if !self.contains(p) {
            return false;
        }
        let l = self.layout.clone();
        if let Some(i) = l.tabs.iter().position(|r| r.contains(p)) {
            self.set_tab(if i == 0 { Tab::Solid } else { Tab::Gradient });
        } else if let Some(i) = l.defaults.iter().position(|r| inflate(*r, 2.0).contains(p)) {
            match self.tab {
                Tab::Solid => self.choose_solid(DEFAULT_SOLIDS[i]),
                Tab::Gradient => self.choose_gradient(default_gradients()[i].clone()),
            }
        } else if let Some(i) = l.recents.iter().position(|r| inflate(*r, 2.0).contains(p)) {
            match self.tab {
                Tab::Solid => self.choose_solid(self.state.recent_solids[i]),
                Tab::Gradient => self.choose_gradient(self.state.recent_gradients[i].clone()),
            }
        } else if let Some(i) = l.handles.iter().position(|r| inflate(*r, 3.0).contains(p)) {
            self.stop = i;
            self.sync_hsv();
            self.drag = Some(Drag::Stop { index: i, removing: false });
        } else if let Some(bar) = l.bar.filter(|b| inflate(*b, 2.0).contains(p)) {
            // Click on the bar adds a stop there, coloured to match.
            let pos = ((p.0 - bar.x) / bar.w).clamp(0.0, 1.0) as f32;
            let color = self.last_gradient.sample(pos);
            self.last_gradient.stops.push(Stop { pos, color });
            self.stop = self.last_gradient.stops.len() - 1;
            self.sync_hsv();
            self.sync_fill();
            self.drag = Some(Drag::Stop { index: self.stop, removing: false });
        } else if l.sv.contains(p) {
            self.drag = Some(Drag::Sv);
            self.motion(p);
        } else if inflate(l.hue, 3.0).contains(p) {
            self.drag = Some(Drag::Hue);
            self.motion(p);
        } else if l.angle.is_some_and(|a| Rect { y: a.y - 8.0, h: a.h + 16.0, ..a }.contains(p)) {
            self.drag = Some(Drag::Angle);
            self.motion(p);
        }
        true
    }

    pub fn motion(&mut self, p: (f64, f64)) {
        let l = &self.layout;
        let frac = |r: Rect, v: f64, along_x: bool| {
            if along_x { ((v - r.x) / r.w).clamp(0.0, 1.0) } else { ((v - r.y) / r.h).clamp(0.0, 1.0) }
        };
        match self.drag {
            Some(Drag::Sv) => {
                self.hsv.s = frac(l.sv, p.0, true) as f32;
                self.hsv.v = 1.0 - frac(l.sv, p.1, false) as f32;
                self.apply_hsv();
            }
            Some(Drag::Hue) => {
                self.hsv.h = (frac(l.hue, p.0, true) * 360.0).min(359.9) as f32;
                self.apply_hsv();
            }
            Some(Drag::Angle) => {
                if let Some(a) = l.angle {
                    // 5° steps, like Noctalia's style slider.
                    self.last_gradient.angle = ((frac(a, p.0, true) * 360.0 / 5.0).round() * 5.0) as f32;
                    self.sync_fill();
                }
            }
            Some(Drag::Stop { index, .. }) => {
                if let Some(bar) = l.bar {
                    let removing = p.1 > bar.bottom() + REMOVE_DISTANCE && self.last_gradient.stops.len() > 2;
                    if let Some(s) = self.last_gradient.stops.get_mut(index) {
                        s.pos = frac(bar, p.0, true) as f32;
                    }
                    self.drag = Some(Drag::Stop { index, removing });
                    self.sync_fill();
                }
            }
            None => {}
        }
    }

    pub fn release(&mut self) {
        if let Some(Drag::Stop { removing: true, .. }) = self.drag {
            self.delete_stop();
        }
        self.drag = None;
    }

    // ---- drawing -------------------------------------------------------------------------

    /// Lays the panel out under `anchor` (the toolbar's colour button), clamped to `surface_w`.
    fn layout(&self, anchor: Rect, top: f32, surface_w: f32) -> Layout {
        let w = CONTENT_W + 2.0 * PAD;
        let x0 = ((anchor.x + anchor.w / 2.0) as f32 - w / 2.0).clamp(8.0, (surface_w - w - 8.0).max(8.0)).round();
        let x = x0 + PAD;
        let mut y = top + PAD;
        let mut l = Layout { tabs: [rect(x, y, CONTENT_W / 2.0, TAB_H), rect(x + CONTENT_W / 2.0, y, CONTENT_W / 2.0, TAB_H)], ..Layout::default() };
        y += TAB_H + 12.0;

        let row = |y: f32, n: usize| (0..n).map(|i| rect(x + i as f32 * (SWATCH + GAP), y, SWATCH, SWATCH)).collect::<Vec<_>>();
        l.defaults_label = f64::from(y + LABEL_H / 2.0 - 2.0);
        y += LABEL_H;
        l.defaults = row(y, 5);
        y += SWATCH + 10.0;
        l.recents_label = f64::from(y + LABEL_H / 2.0 - 2.0);
        y += LABEL_H;
        let recents = match self.tab {
            Tab::Solid => self.state.recent_solids.len(),
            Tab::Gradient => self.state.recent_gradients.len(),
        };
        l.recents = row(y, recents);
        y += SWATCH + 12.0;

        let sv_h = if self.tab == Tab::Gradient {
            let bar = rect(x, y, CONTENT_W, BAR_H);
            l.handles = self
                .last_gradient
                .stops
                .iter()
                .map(|s| rect(x + s.pos * CONTENT_W - HANDLE / 2.0, y + BAR_H + 3.0, HANDLE, HANDLE))
                .collect();
            l.bar = Some(bar);
            y += BAR_H + 3.0 + HANDLE + 10.0;
            110.0
        } else {
            140.0
        };
        l.sv = rect(x, y, CONTENT_W, sv_h);
        y += sv_h + 10.0;
        l.hue = rect(x, y, CONTENT_W, HUE_H);
        y += HUE_H + 12.0;
        l.footer = rect(x, y, CONTENT_W, FOOT_H);
        if self.tab == Tab::Gradient {
            l.angle = Some(rect(x + 52.0, y + FOOT_H / 2.0 - 2.0, CONTENT_W - 52.0 - 48.0, 4.0));
        }
        y += FOOT_H + PAD;
        l.panel = rect(x0, top, w, y - top);
        l
    }

    pub fn draw(&mut self, canvas: &mut Canvas<OpenGl>, font: Option<FontId>, t: &Theme, anchor: Rect, top: f32, surface_w: f32) {
        self.layout = self.layout(anchor, top, surface_w);
        let l = &self.layout;
        card(canvas, f(l.panel), RADIUS_XL, t.surface.with_alpha(0.94), t.outline);

        // Segmented Solid | Gradient.
        let tabs = Rect { w: l.tabs[0].w * 2.0, ..l.tabs[0] };
        card(canvas, f(tabs), RADIUS_MD, t.surface_variant, t.outline);
        let active = if self.tab == Tab::Solid { 0 } else { 1 };
        let mut path = Path::new();
        let (ax, ay, aw, ah) = f(inflate(l.tabs[active], -2.0));
        path.rounded_rect(ax, ay, aw, ah, RADIUS_MD - 1.0);
        canvas.fill_path(&path, &Paint::color(t.primary.fv()));
        if let Some(font) = font {
            for (i, label) in ["Solid", "Gradient"].into_iter().enumerate() {
                let paint = text_paint(font, 13.0, if i == active { t.on_primary } else { t.on_surface });
                let r = l.tabs[i];
                let tw = text_width(canvas, label, &paint);
                let _ = canvas.fill_text((r.x + r.w / 2.0) as f32 - tw / 2.0, (r.y + r.h / 2.0) as f32, label, &paint);
            }
            let caption = text_paint(font, CAPTION, t.on_surface.with_alpha(0.7));
            let _ = canvas.fill_text(l.panel.x as f32 + PAD, l.defaults_label as f32, "Defaults", &caption);
            let _ = canvas.fill_text(l.panel.x as f32 + PAD, l.recents_label as f32, "Recent", &caption);
            if l.recents.is_empty() {
                let y = l.recents_label as f32 + LABEL_H + SWATCH / 2.0 - 2.0;
                let _ = canvas.fill_text(l.panel.x as f32 + PAD, y, "Nothing yet", &text_paint(font, CAPTION, t.on_surface.with_alpha(0.45)));
            }
        }

        // Defaults and recents.
        let current = self.state.fill.clone();
        match self.tab {
            Tab::Solid => {
                let colors = DEFAULT_SOLIDS.iter().zip(&l.defaults).chain(self.state.recent_solids.iter().zip(&l.recents));
                for (c, r) in colors {
                    swatch(canvas, *r, &Fill::Solid(*c), current == Fill::Solid(*c), t, true);
                }
            }
            Tab::Gradient => {
                let current = match &current {
                    Fill::Gradient(g) => Some(g.normalized()),
                    Fill::Solid(_) => None,
                };
                let defaults = default_gradients();
                for (g, r) in defaults.iter().zip(&l.defaults).chain(self.state.recent_gradients.iter().zip(&l.recents)) {
                    swatch(canvas, *r, &Fill::Gradient(g.clone()), current.as_ref() == Some(&g.normalized()), t, false);
                }
            }
        }

        // Gradient stop bar and handles.
        if let Some(bar) = l.bar {
            let flat = Gradient { angle: 90.0, ..self.last_gradient.clone() };
            let mut path = Path::new();
            let (bx, by, bw, bh) = f(bar);
            path.rounded_rect(bx, by, bw, bh, RADIUS_SM);
            canvas.fill_path(&path, &Fill::Gradient(flat).paint(bar));
            canvas.stroke_path(&path, &Paint::color(t.outline.fv()).with_line_width(1.0));
            for (i, (h, s)) in l.handles.iter().zip(&self.last_gradient.stops).enumerate() {
                let removing = matches!(self.drag, Some(Drag::Stop { index, removing: true }) if index == i);
                let selected = i == self.stop;
                let (cx, cy) = ((h.x + h.w / 2.0) as f32, (h.y + h.h / 2.0) as f32);
                // Tick from the bar to the handle.
                let mut tick = Path::new();
                tick.move_to(cx, by + bh);
                tick.line_to(cx, cy - HANDLE / 2.0);
                canvas.stroke_path(&tick, &Paint::color(t.on_surface.with_alpha(0.6).fv()).with_line_width(1.0));
                let mut dot = Path::new();
                dot.circle(cx, cy, HANDLE / 2.0);
                let alpha = if removing { 0.35 } else { 1.0 };
                canvas.fill_path(&dot, &Paint::color(s.color.with_alpha(alpha).fv()));
                let ring = if selected { t.primary } else { t.on_surface };
                canvas.stroke_path(&dot, &Paint::color(ring.with_alpha(alpha).fv()).with_line_width(if selected { 2.5 } else { 1.5 }));
            }
        }

        // Saturation / value square for the current hue.
        let (sx, sy, sw, sh) = f(l.sv);
        let mut path = Path::new();
        path.rounded_rect(sx, sy, sw, sh, RADIUS_MD);
        let pure = Hsv { h: self.hsv.h, s: 1.0, v: 1.0 }.color();
        canvas.fill_path(&path, &Paint::color(pure.fv()));
        canvas.fill_path(&path, &Paint::linear_gradient(sx, sy, sx + sw, sy, femtovg::Color::rgbaf(1.0, 1.0, 1.0, 1.0), femtovg::Color::rgbaf(1.0, 1.0, 1.0, 0.0)));
        canvas.fill_path(&path, &Paint::linear_gradient(sx, sy, sx, sy + sh, femtovg::Color::rgbaf(0.0, 0.0, 0.0, 0.0), femtovg::Color::rgbaf(0.0, 0.0, 0.0, 1.0)));
        canvas.stroke_path(&path, &Paint::color(t.outline.fv()).with_line_width(1.0));
        let (mx, my) = (sx + self.hsv.s * sw, sy + (1.0 - self.hsv.v) * sh);
        let mut marker = Path::new();
        marker.circle(mx, my, 6.0);
        canvas.stroke_path(&marker, &Paint::color(femtovg::Color::rgbaf(0.0, 0.0, 0.0, 0.6)).with_line_width(3.5));
        canvas.stroke_path(&marker, &Paint::color(femtovg::Color::white()).with_line_width(2.0));

        // Hue bar.
        let (hx, hy, hw, hh) = f(l.hue);
        let mut path = Path::new();
        path.rounded_rect(hx, hy, hw, hh, hh / 2.0);
        let stops = (0..=6).map(|i| (i as f32 / 6.0, Hsv { h: i as f32 * 60.0 % 360.0, s: 1.0, v: 1.0 }.color().fv()));
        canvas.fill_path(&path, &Paint::linear_gradient_stops(hx, hy, hx + hw, hy, stops));
        let kx = hx + self.hsv.h / 360.0 * hw;
        let mut knob = Path::new();
        knob.rounded_rect(kx - 3.0, hy - 2.0, 6.0, hh + 4.0, 3.0);
        canvas.stroke_path(&knob, &Paint::color(femtovg::Color::rgbaf(0.0, 0.0, 0.0, 0.6)).with_line_width(3.5));
        canvas.stroke_path(&knob, &Paint::color(femtovg::Color::white()).with_line_width(2.0));

        // Footer: the colour being edited, or the gradient angle.
        let (fx, fy, fw, fh) = f(l.footer);
        let cy = fy + fh / 2.0;
        match (self.tab, l.angle) {
            (Tab::Gradient, Some(track)) => {
                let (tx, ty, tw, th) = f(track);
                let frac = self.last_gradient.angle.rem_euclid(360.0) / 360.0;
                let mut path = Path::new();
                path.rounded_rect(tx, ty, tw, th, th / 2.0);
                canvas.fill_path(&path, &Paint::color(t.surface_variant.fv()));
                canvas.stroke_path(&path, &Paint::color(t.outline.fv()).with_line_width(1.0));
                let mut filled = Path::new();
                filled.rounded_rect(tx, ty, tw * frac, th, th / 2.0);
                canvas.fill_path(&filled, &Paint::color(t.primary.fv()));
                let mut knob = Path::new();
                knob.circle(tx + tw * frac, ty + th / 2.0, 7.0);
                canvas.fill_path(&knob, &Paint::color(t.primary.fv()));
                canvas.stroke_path(&knob, &Paint::color(t.surface.fv()).with_line_width(1.5));
                if let Some(font) = font {
                    let paint = text_paint(font, 13.0, t.on_surface);
                    let _ = canvas.fill_text(fx, cy, "Angle", &paint);
                    let value = format!("{}°", self.last_gradient.angle.round() as i32);
                    let vw = text_width(canvas, &value, &paint);
                    let _ = canvas.fill_text(fx + fw - vw, cy, &value, &paint);
                }
            }
            _ => {
                let mut dot = Path::new();
                dot.circle(fx + fh / 2.0, cy, fh / 2.0);
                canvas.fill_path(&dot, &Paint::color(self.last_solid.fv()));
                canvas.stroke_path(&dot, &Paint::color(t.outline.fv()).with_line_width(1.0));
                if let Some(font) = font {
                    let _ = canvas.fill_text(fx + fh + 10.0, cy, self.last_solid.to_hex(), &text_paint(font, 13.0, t.on_surface));
                }
            }
        }
    }
}

/// A solid (circle) or gradient (rounded square) chip; the current pick gets a heavier ring.
pub fn swatch(canvas: &mut Canvas<OpenGl>, r: Rect, fill: &Fill, selected: bool, t: &Theme, round: bool) {
    let (x, y, w, h) = f(r);
    let mut path = Path::new();
    if round {
        path.circle(x + w / 2.0, y + h / 2.0, w / 2.0);
    } else {
        path.rounded_rect(x, y, w, h, RADIUS_SM);
    }
    canvas.fill_path(&path, &fill.paint(r));
    let (ring, width) = if selected { (t.on_surface, 2.0) } else { (t.outline, 1.0) };
    canvas.stroke_path(&path, &Paint::color(ring.fv()).with_line_width(width));
}
