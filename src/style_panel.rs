//! The screenshot style panel: switches and sliders, after Noctalia's, editing a `Style` in place.
//! Immediate mode like the colour picker: `draw` lays out and keeps the hit boxes.

use femtovg::renderer::OpenGl;
use femtovg::{Canvas, FontId, Paint, Path};

use crate::config::Theme;
use crate::geom::Rect;
use crate::picker::Target;
use crate::style::Style;
use crate::ui::{card, text_paint, text_width};

const PAD: f32 = 12.0;
const LABEL_W: f32 = 70.0;
const SLIDER_W: f32 = 150.0;
const VALUE_W: f32 = 44.0;
const GAP: f32 = 8.0;
const ROW_H: f32 = 30.0;
const CAPTION: f32 = 13.0;
const SWITCH: (f32, f32) = (34.0, 18.0);

#[derive(Debug, Clone, Copy, PartialEq)]
enum Switch {
    Enabled,
    Background,
    Shadow,
    Border,
    BorderShadow,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Slider {
    Padding,
    Radius,
    ShadowBlur,
    ShadowOpacity,
    ShadowOffsetX,
    ShadowOffsetY,
    BorderWidth,
}

enum Row {
    Switch(Switch, &'static str),
    Slider(Slider, &'static str),
}

const ROWS: [Row; 12] = [
    Row::Switch(Switch::Enabled, "Style screenshot"),
    Row::Switch(Switch::Background, "Background"),
    Row::Slider(Slider::Padding, "Padding"),
    Row::Slider(Slider::Radius, "Corners"),
    Row::Switch(Switch::Border, "Border"),
    Row::Slider(Slider::BorderWidth, "Width"),
    Row::Switch(Switch::BorderShadow, "Edge shadow"),
    Row::Switch(Switch::Shadow, "Drop shadow"),
    Row::Slider(Slider::ShadowBlur, "Blur"),
    Row::Slider(Slider::ShadowOpacity, "Opacity"),
    Row::Slider(Slider::ShadowOffsetX, "Offset X"),
    Row::Slider(Slider::ShadowOffsetY, "Offset Y"),
];

impl Slider {
    /// (min, max, step), Noctalia's ranges.
    fn range(self) -> (f64, f64, f64) {
        match self {
            Slider::Padding => (0.0, 512.0, 4.0),
            Slider::Radius => (0.0, 128.0, 1.0),
            Slider::ShadowBlur => (0.0, 200.0, 1.0),
            Slider::ShadowOpacity => (0.0, 1.0, 0.01),
            Slider::ShadowOffsetX | Slider::ShadowOffsetY => (-200.0, 200.0, 1.0),
            Slider::BorderWidth => (1.0, 32.0, 1.0),
        }
    }

    fn get(self, s: &Style) -> f64 {
        match self {
            Slider::Padding => f64::from(s.padding),
            Slider::Radius => f64::from(s.radius),
            Slider::ShadowBlur => f64::from(s.shadow_blur),
            Slider::ShadowOpacity => f64::from(s.shadow_opacity),
            Slider::ShadowOffsetX => f64::from(s.shadow_offset_x),
            Slider::ShadowOffsetY => f64::from(s.shadow_offset_y),
            Slider::BorderWidth => f64::from(s.border_width),
        }
    }

    fn set(self, s: &mut Style, v: f64) {
        let i = v.round() as i32;
        match self {
            Slider::Padding => s.padding = i,
            Slider::Radius => s.radius = i,
            Slider::ShadowBlur => s.shadow_blur = i,
            Slider::ShadowOpacity => s.shadow_opacity = v as f32,
            Slider::ShadowOffsetX => s.shadow_offset_x = i,
            Slider::ShadowOffsetY => s.shadow_offset_y = i,
            Slider::BorderWidth => s.border_width = i,
        }
    }

    fn format(self, s: &Style) -> String {
        match self {
            Slider::ShadowOpacity => format!("{}%", (s.shadow_opacity * 100.0).round() as i32),
            _ => format!("{}", self.get(s).round() as i32),
        }
    }

    /// Shadow and border sliders only matter with those on.
    fn active(self, s: &Style) -> bool {
        s.enabled
            && match self {
                Slider::Padding | Slider::Radius => true,
                Slider::BorderWidth => s.border,
                _ => s.shadow,
            }
    }
}

impl Switch {
    fn get(self, s: &Style) -> bool {
        match self {
            Switch::Enabled => s.enabled,
            Switch::Background => s.background,
            Switch::Shadow => s.shadow,
            Switch::Border => s.border,
            Switch::BorderShadow => s.border_shadow,
        }
    }

    fn toggle(self, s: &mut Style) {
        match self {
            Switch::Enabled => s.enabled ^= true,
            Switch::Background => s.background ^= true,
            Switch::Shadow => s.shadow ^= true,
            Switch::Border => s.border ^= true,
            Switch::BorderShadow => s.border_shadow ^= true,
        }
    }

    fn active(self, s: &Style) -> bool {
        match self {
            Switch::Enabled => true,
            Switch::BorderShadow => s.enabled && s.border,
            _ => s.enabled,
        }
    }
}

/// What a press on the panel did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Press {
    Outside,
    Handled,
    /// The backdrop swatch: open the colour selector on the backdrop.
    EditBackdrop,
    /// The border swatch: open the colour selector on the border.
    EditBorder,
}

#[derive(Default)]
pub struct StylePanel {
    pub open: bool,
    drag: Option<(Slider, Rect)>,
    panel: Rect,
    switches: Vec<(Rect, Switch)>,
    sliders: Vec<(Rect, Slider)>,
    backdrop_swatch: Rect,
    border_swatch: Rect,
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x: f64::from(x), y: f64::from(y), w: f64::from(w), h: f64::from(h) }
}

impl StylePanel {
    pub fn contains(&self, p: (f64, f64)) -> bool {
        self.open && self.panel.contains(p)
    }

    /// Where the panel was last drawn.
    pub fn rect(&self) -> Rect {
        self.panel
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn close(&mut self) {
        self.open = false;
        self.drag = None;
    }

    /// A press at surface-local `p`.
    pub fn press(&mut self, p: (f64, f64), style: &mut Style) -> Press {
        if !self.contains(p) {
            return Press::Outside;
        }
        if let Some((_, switch)) = self.switches.iter().find(|(r, _)| r.contains(p)) {
            switch.toggle(style);
            return Press::Handled;
        }
        // Only offered while there's a backdrop to see.
        if style.enabled && style.background && self.backdrop_swatch.contains(p) {
            return Press::EditBackdrop;
        }
        if style.enabled && style.border && self.border_swatch.contains(p) {
            return Press::EditBorder;
        }
        if let Some(&(track, slider)) = self.sliders.iter().find(|(r, _)| Rect { y: r.y - 8.0, h: r.h + 16.0, ..*r }.contains(p)) {
            self.drag = Some((slider, track));
            self.motion(p, style);
        }
        Press::Handled
    }

    pub fn motion(&mut self, p: (f64, f64), style: &mut Style) {
        if let Some((slider, track)) = self.drag {
            let (min, max, step) = slider.range();
            let frac = ((p.0 - track.x) / track.w).clamp(0.0, 1.0);
            slider.set(style, ((min + frac * (max - min)) / step).round() * step);
        }
    }

    pub fn release(&mut self) {
        self.drag = None;
    }

    /// Lays out and draws the panel under `anchor`, kept within `surface_w`.
    #[allow(clippy::too_many_arguments)]
    /// `editing` rings the backdrop or border swatch while the colour selector is on it.
    pub fn draw(&mut self, canvas: &mut Canvas<OpenGl>, font: Option<FontId>, t: &Theme, style: &Style, editing: Option<Target>, anchor: Rect, top: f32, surface_w: f32) {
        let w = PAD * 2.0 + LABEL_W + GAP + SLIDER_W + GAP + VALUE_W;
        let h = PAD * 2.0 + ROW_H * ROWS.len() as f32;
        let x0 = ((anchor.x + anchor.w / 2.0) as f32 - w / 2.0).clamp(8.0, (surface_w - w - 8.0).max(8.0)).round();
        self.panel = rect(x0, top, w, h);
        self.switches.clear();
        self.sliders.clear();
        self.backdrop_swatch = Rect::default();
        self.border_swatch = Rect::default();
        card(canvas, (x0, top, w, h), 12.0, t.surface.with_alpha(0.94), t.outline);

        let x = x0 + PAD;
        for (i, row) in ROWS.iter().enumerate() {
            let cy = top + PAD + ROW_H * (i as f32 + 0.5);
            let (label, active) = match row {
                Row::Switch(s, label) => (*label, s.active(style)),
                Row::Slider(s, label) => (*label, s.active(style)),
            };
            let ink = if active { t.on_surface } else { t.on_surface.with_alpha(0.45) };
            if let Some(font) = font {
                let _ = canvas.fill_text(x, cy, label, &text_paint(font, CAPTION, ink));
            }
            match row {
                Row::Switch(switch, _) => {
                    let on = switch.get(style);
                    let (sw, sh) = SWITCH;
                    let sx = x + w - PAD * 2.0 - sw;
                    let mut pill = Path::new();
                    pill.rounded_rect(sx, cy - sh / 2.0, sw, sh, sh / 2.0);
                    let alpha = if active { 1.0 } else { 0.45 };
                    let (bg, knob) = if on { (t.primary, t.on_primary) } else { (t.surface_variant, t.on_surface) };
                    canvas.fill_path(&pill, &Paint::color(bg.with_alpha(alpha).fv()));
                    canvas.stroke_path(&pill, &Paint::color(if on { t.primary } else { t.outline }.with_alpha(alpha).fv()).with_line_width(1.0));
                    let mut dot = Path::new();
                    dot.circle(if on { sx + sw - sh / 2.0 } else { sx + sh / 2.0 }, cy, sh / 2.0 - 3.0);
                    canvas.fill_path(&dot, &Paint::color(knob.with_alpha(alpha).fv()));
                    // The backdrop's or border's own colour; click it to change it. Only while it's shown.
                    let swatch = match switch {
                        Switch::Background if style.enabled && style.background => Some((&style.backdrop, Target::Backdrop)),
                        Switch::Border if style.enabled && style.border => Some((&style.border_fill, Target::Border)),
                        _ => None,
                    };
                    if let Some((fill, target)) = swatch {
                        let r = rect(sx - GAP - 18.0, cy - 9.0, 18.0, 18.0);
                        crate::picker::swatch(canvas, r, fill, editing == Some(target), t, false);
                        let hit = Rect { x: r.x - 2.0, y: r.y - 2.0, w: r.w + 4.0, h: r.h + 4.0 };
                        if target == Target::Backdrop {
                            self.backdrop_swatch = hit;
                        } else {
                            self.border_swatch = hit;
                        }
                    }
                    self.switches.push((rect(sx - 4.0, cy - ROW_H / 2.0, sw + 8.0, ROW_H), *switch));
                }
                Row::Slider(slider, _) => {
                    let (min, max, _) = slider.range();
                    let frac = ((slider.get(style) - min) / (max - min)).clamp(0.0, 1.0) as f32;
                    let tx = x + LABEL_W + GAP;
                    let track = rect(tx, cy - 2.0, SLIDER_W, 4.0);
                    let alpha = if active { 1.0 } else { 0.45 };
                    let mut path = Path::new();
                    path.rounded_rect(tx, cy - 2.0, SLIDER_W, 4.0, 2.0);
                    canvas.fill_path(&path, &Paint::color(t.surface_variant.with_alpha(alpha).fv()));
                    canvas.stroke_path(&path, &Paint::color(t.outline.with_alpha(alpha).fv()).with_line_width(1.0));
                    let mut filled = Path::new();
                    filled.rounded_rect(tx, cy - 2.0, SLIDER_W * frac, 4.0, 2.0);
                    canvas.fill_path(&filled, &Paint::color(t.primary.with_alpha(alpha).fv()));
                    let mut knob = Path::new();
                    knob.circle(tx + SLIDER_W * frac, cy, 7.0);
                    canvas.fill_path(&knob, &Paint::color(t.primary.with_alpha(alpha).fv()));
                    canvas.stroke_path(&knob, &Paint::color(t.surface.fv()).with_line_width(1.5));
                    if let Some(font) = font {
                        let value = slider.format(style);
                        let paint = text_paint(font, CAPTION, ink);
                        let vw = text_width(canvas, &value, &paint);
                        let _ = canvas.fill_text(x0 + w - PAD - vw, cy, &value, &paint);
                    }
                    self.sliders.push((track, *slider));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sliders_snap_to_step_and_clamp() {
        let mut s = Style::default();
        Slider::Padding.set(&mut s, ((63.0f64 / 4.0).round()) * 4.0);
        assert_eq!(s.padding, 64);
        let mut panel = StylePanel { open: true, ..Default::default() };
        panel.drag = Some((Slider::ShadowOffsetY, Rect { x: 0.0, y: 0.0, w: 100.0, h: 4.0 }));
        panel.motion((-50.0, 0.0), &mut s);
        assert_eq!(s.shadow_offset_y, -200);
        panel.motion((75.0, 0.0), &mut s);
        assert_eq!(s.shadow_offset_y, 100);
        panel.drag = Some((Slider::ShadowOpacity, Rect { x: 0.0, y: 0.0, w: 100.0, h: 4.0 }));
        panel.motion((33.3, 0.0), &mut s);
        assert!((s.shadow_opacity - 0.33).abs() < 1e-6);
    }
}
