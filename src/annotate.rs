//! Annotation tools and shapes. Shapes live in the pixel coordinates of the image being annotated
//! and are drawn by `render`, for both the editor preview and the export.

use serde::{Deserialize, Serialize};

use crate::geom::Rect;
use crate::paint::Fill;

/// S / M / L multiply the tool's default width, as in Noctalia.
pub const SIZE_PRESETS: [f64; 3] = [0.5, 1.0, 2.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    #[default]
    Rectangle,
    Arrow,
    Text,
}

impl Tool {
    /// Key in the state file.
    pub fn name(self) -> &'static str {
        match self {
            Tool::Rectangle => "rectangle",
            Tool::Arrow => "arrow",
            Tool::Text => "text",
        }
    }

    /// Stroke width in logical pixels; for text, the font size.
    pub fn default_width(self) -> f64 {
        match self {
            Tool::Rectangle | Tool::Arrow => 6.0,
            Tool::Text => 24.0,
        }
    }

    pub fn width_range(self) -> (f64, f64) {
        match self {
            Tool::Text => (8.0, 200.0),
            _ => (1.0, 100.0),
        }
    }
}

/// Which ends of an arrow get a head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArrowHeads {
    #[default]
    End,
    Start,
    Both,
}

impl ArrowHeads {
    pub const ALL: [ArrowHeads; 3] = [ArrowHeads::End, ArrowHeads::Start, ArrowHeads::Both];

    pub fn label(self) -> &'static str {
        match self {
            ArrowHeads::End => "Arrow at end",
            ArrowHeads::Start => "Arrow at start",
            ArrowHeads::Both => "Arrows at both ends",
        }
    }
}

pub type Point = (f64, f64);

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// Centre line of the stroke.
    Rectangle(Rect),
    Arrow { from: Point, to: Point, heads: ArrowHeads },
    /// `at` is the top-left of the text.
    Text { at: Point, text: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    pub kind: Kind,
    /// Stroke width, or font size for text, in image pixels.
    pub width: f64,
    pub fill: Fill,
}

/// Noctalia's head spread either side of the shaft.
const HEAD_SPREAD: f64 = std::f64::consts::PI / 6.0;

impl Shape {
    /// An arrow as line segments: the shaft, then each head as (tip, wing) pairs. `scale` is image
    /// pixels per logical pixel, so heads keep Noctalia's 12 logical px minimum.
    pub fn arrow_segments(from: Point, to: Point, heads: ArrowHeads, width: f64, scale: f64) -> Vec<(Point, Point)> {
        let mut segments = vec![(from, to)];
        let len = (12.0 * scale).max(width * 3.0);
        let mut head = |tip: Point, tail: Point| {
            let angle = (tip.1 - tail.1).atan2(tip.0 - tail.0);
            for a in [angle - HEAD_SPREAD, angle + HEAD_SPREAD] {
                segments.push((tip, (tip.0 - len * a.cos(), tip.1 - len * a.sin())));
            }
        };
        if matches!(heads, ArrowHeads::End | ArrowHeads::Both) {
            head(to, from);
        }
        if matches!(heads, ArrowHeads::Start | ArrowHeads::Both) {
            head(from, to);
        }
        segments
    }

    /// Outer bounds of a stroked shape, which gradients span. Text bounds depend on the font, so
    /// the renderer measures those.
    pub fn stroke_bounds(&self, scale: f64) -> Option<Rect> {
        let h = self.width / 2.0;
        match &self.kind {
            Kind::Rectangle(r) => Some(Rect { x: r.x - h, y: r.y - h, w: r.w + self.width, h: r.h + self.width }),
            Kind::Arrow { from, to, heads } => {
                let points = Self::arrow_segments(*from, *to, *heads, self.width, scale).into_iter().flat_map(|(a, b)| [a, b]);
                let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                for (x, y) in points {
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                }
                Some(Rect { x: x0 - h, y: y0 - h, w: x1 - x0 + self.width, h: y1 - y0 + self.width })
            }
            Kind::Text { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: Point, b: Point) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    #[test]
    fn arrow_heads_point_back_along_the_shaft() {
        // Horizontal arrow to the right, width 2: heads use the 12 px minimum.
        let end = Shape::arrow_segments((0.0, 0.0), (100.0, 0.0), ArrowHeads::End, 2.0, 1.0);
        assert_eq!(end.len(), 3);
        let wing = (100.0 - 12.0 * HEAD_SPREAD.cos(), 12.0 * HEAD_SPREAD.sin());
        assert!(end[1..].iter().any(|(_, w)| near(*w, wing)), "{end:?}");
        assert!(end[1..].iter().all(|(tip, w)| near(*tip, (100.0, 0.0)) && w.0 < 100.0));

        let start = Shape::arrow_segments((0.0, 0.0), (100.0, 0.0), ArrowHeads::Start, 2.0, 1.0);
        assert!(start[1..].iter().all(|(tip, w)| near(*tip, (0.0, 0.0)) && w.0 > 0.0));
        assert_eq!(Shape::arrow_segments((0.0, 0.0), (100.0, 0.0), ArrowHeads::Both, 2.0, 1.0).len(), 5);
    }

    #[test]
    fn heads_scale_with_width_and_bounds_include_them() {
        let segs = Shape::arrow_segments((0.0, 0.0), (100.0, 0.0), ArrowHeads::End, 10.0, 1.0);
        let (tip, wing) = segs[1];
        assert!(((tip.0 - wing.0).hypot(tip.1 - wing.1) - 30.0).abs() < 1e-9);

        let shape = Shape { kind: Kind::Arrow { from: (0.0, 0.0), to: (100.0, 0.0), heads: ArrowHeads::End }, width: 10.0, fill: Fill::default() };
        let b = shape.stroke_bounds(1.0).unwrap();
        // Wings reach 15 px either side of the shaft, plus half the stroke.
        assert!((b.y + 20.0).abs() < 1e-9 && (b.bottom() - 20.0).abs() < 1e-9, "{b:?}");
        assert!((b.x + 5.0).abs() < 1e-9 && (b.right() - 105.0).abs() < 1e-9, "{b:?}");
    }
}
