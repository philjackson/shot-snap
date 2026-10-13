//! What annotations are painted with: a solid colour or a linear gradient.

use serde::{Deserialize, Serialize};

use crate::config::Color;
use crate::geom::Rect;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fill {
    Solid(Color),
    Gradient(Gradient),
}

/// CSS-style linear gradient: `angle` in degrees, 0 = towards the top, 90 = towards the right.
/// Stops may be unsorted while being edited; everything that reads them sorts first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gradient {
    pub angle: f32,
    pub stops: Vec<Stop>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Stop {
    pub pos: f32,
    pub color: Color,
}

const fn rgb(hex: u32) -> Color {
    Color {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

/// Noctalia's first five annotation swatches (red, yellow, green, blue, orange), rounded to
/// bytes so they survive the hex round trip through the state file unchanged.
pub const DEFAULT_SOLIDS: [Color; 5] = [rgb(0xf53347), rgb(0xffe833), rgb(0x33cc66), rgb(0x338cff), rgb(0xff991a)];

/// Noctalia's fixed gradient presets: grape, sunset, sky, mint, peach.
pub fn default_gradients() -> [Gradient; 5] {
    [(0x8e2de2, 0xec6ead), (0xff7e5f, 0xfeb47b), (0x2f80ed, 0x56ccf2), (0x00b09b, 0x96c93d), (0xed4264, 0xffedbc)].map(|(a, b)| Gradient {
        angle: 135.0,
        stops: vec![Stop { pos: 0.0, color: rgb(a) }, Stop { pos: 1.0, color: rgb(b) }],
    })
}

impl Default for Fill {
    fn default() -> Self {
        Fill::Solid(DEFAULT_SOLIDS[0])
    }
}

impl Gradient {
    pub fn sorted(&self) -> Vec<Stop> {
        let mut stops = self.stops.clone();
        stops.sort_by(|a, b| a.pos.total_cmp(&b.pos));
        stops
    }

    /// Same gradient with stops in order, for storing and comparing.
    pub fn normalized(&self) -> Gradient {
        Gradient { angle: self.angle.rem_euclid(360.0), stops: self.sorted() }
    }

    pub fn sample(&self, t: f32) -> Color {
        sample(&self.sorted(), t)
    }

    /// Start and end of the CSS gradient line across `bounds`, which makes the corners reach the
    /// first and last stop exactly.
    pub fn line(&self, bounds: Rect) -> ((f64, f64), (f64, f64)) {
        let a = f64::from(self.angle).to_radians();
        let (dx, dy) = (a.sin(), -a.cos());
        let half = (bounds.w * dx.abs() + bounds.h * dy.abs()) / 2.0;
        let (cx, cy) = (bounds.x + bounds.w / 2.0, bounds.y + bounds.h / 2.0);
        ((cx - dx * half, cy - dy * half), (cx + dx * half, cy + dy * half))
    }
}

fn sample(stops: &[Stop], t: f32) -> Color {
    let Some(first) = stops.first() else { return DEFAULT_SOLIDS[0] };
    let last = stops.last().unwrap();
    if t <= first.pos {
        return first.color;
    }
    if t >= last.pos {
        return last.color;
    }
    let i = stops.iter().position(|s| s.pos >= t).unwrap_or(stops.len() - 1).max(1);
    let (a, b) = (stops[i - 1], stops[i]);
    let k = if b.pos > a.pos { (t - a.pos) / (b.pos - a.pos) } else { 1.0 };
    lerp(a.color, b.color, k)
}

pub fn lerp(a: Color, b: Color, k: f32) -> Color {
    Color { r: a.r + (b.r - a.r) * k, g: a.g + (b.g - a.g) * k, b: a.b + (b.b - a.b) * k, a: a.a + (b.a - a.a) * k }
}

impl Fill {
    /// Colour at point `p` of a shape whose painted box is `bounds` (same coordinate space): the
    /// same maths as femtovg's gradients, for CPU rendering (the screenshot style backdrop).
    pub fn color_at(&self, bounds: Rect, p: (f64, f64)) -> Color {
        match self {
            Fill::Solid(c) => *c,
            Fill::Gradient(g) => {
                let (s, e) = g.line(bounds);
                let (vx, vy) = (e.0 - s.0, e.1 - s.1);
                let len2 = vx * vx + vy * vy;
                let t = if len2 > 0.0 { ((p.0 - s.0) * vx + (p.1 - s.1) * vy) / len2 } else { 0.0 };
                g.sample(t as f32)
            }
        }
    }

    /// A femtovg paint for this fill spread over `bounds`.
    pub fn paint(&self, bounds: Rect) -> femtovg::Paint {
        match self {
            Fill::Solid(c) => femtovg::Paint::color(c.fv()),
            Fill::Gradient(g) => {
                let (s, e) = g.line(bounds);
                let stops: Vec<(f32, femtovg::Color)> = g.sorted().iter().map(|s| (s.pos.clamp(0.0, 1.0), s.color.fv())).collect();
                femtovg::Paint::linear_gradient_stops(s.0 as f32, s.1 as f32, e.0 as f32, e.1 as f32, stops)
            }
        }
    }
}

// ---- HSV, for the picker ---------------------------------------------------------------------

/// Hue in degrees [0, 360), saturation and value in [0, 1].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hsv {
    pub h: f32,
    pub s: f32,
    pub v: f32,
}

impl Hsv {
    pub fn from_color(c: Color) -> Hsv {
        let max = c.r.max(c.g).max(c.b);
        let min = c.r.min(c.g).min(c.b);
        let d = max - min;
        let h = if d == 0.0 {
            0.0
        } else if max == c.r {
            60.0 * ((c.g - c.b) / d).rem_euclid(6.0)
        } else if max == c.g {
            60.0 * ((c.b - c.r) / d + 2.0)
        } else {
            60.0 * ((c.r - c.g) / d + 4.0)
        };
        Hsv { h, s: if max > 0.0 { d / max } else { 0.0 }, v: max }
    }

    pub fn color(self) -> Color {
        let c = self.v * self.s;
        let hp = self.h.rem_euclid(360.0) / 60.0;
        let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
        let (r, g, b) = match hp as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = self.v - c;
        Color { r: r + m, g: g + m, b: b + m, a: 1.0 }.quantized()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Color, b: Color) -> bool {
        (a.r - b.r).abs() < 0.01 && (a.g - b.g).abs() < 0.01 && (a.b - b.b).abs() < 0.01
    }

    #[test]
    fn hsv_round_trip() {
        for c in DEFAULT_SOLIDS {
            assert!(close(Hsv::from_color(c).color(), c), "{c:?}");
        }
    }

    #[test]
    fn gradient_line_reaches_corners() {
        let g = &default_gradients()[0];
        let bounds = Rect { x: 0.0, y: 0.0, w: 100.0, h: 50.0 };
        let fill = Fill::Gradient(g.clone());
        // 135° runs from the top-left corner to the bottom-right corner.
        assert!(close(fill.color_at(bounds, (0.0, 0.0)), g.stops[0].color));
        assert!(close(fill.color_at(bounds, (100.0, 50.0)), g.stops[1].color));
        assert!(close(fill.color_at(bounds, (50.0, 25.0)), lerp(g.stops[0].color, g.stops[1].color, 0.5)));
    }

    #[test]
    fn unsorted_stops_sample_in_order() {
        let red = DEFAULT_SOLIDS[0];
        let blue = DEFAULT_SOLIDS[3];
        let g = Gradient { angle: 90.0, stops: vec![Stop { pos: 1.0, color: blue }, Stop { pos: 0.0, color: red }] };
        assert!(close(g.sample(0.0), red));
        assert!(close(g.sample(1.0), blue));
    }
}
