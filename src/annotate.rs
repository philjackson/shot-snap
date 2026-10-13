//! Annotation shapes and burning them into the exported image.

use crate::geom::Rect;
use crate::image::Image;
use crate::paint::Fill;

/// Noctalia's default shape weight.
pub const DEFAULT_WIDTH: f64 = 6.0;
pub const MIN_WIDTH: f64 = 1.0;
pub const MAX_WIDTH: f64 = 100.0;
/// S / M / L multiply the tool's default width, as in Noctalia.
pub const SIZE_PRESETS: [f64; 3] = [0.5, 1.0, 2.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Rectangle,
}

impl Tool {
    /// Key in the state file.
    pub fn name(self) -> &'static str {
        match self {
            Tool::Rectangle => "rectangle",
        }
    }

    pub fn default_width(self) -> f64 {
        match self {
            Tool::Rectangle => DEFAULT_WIDTH,
        }
    }
}

/// A stroked rectangle. `rect` is the stroke's centre line and `width` its weight, both in the
/// coordinates of the image being annotated. Gradients span the stroke's outer bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    pub rect: Rect,
    pub width: f64,
    pub fill: Fill,
}

impl Shape {
    /// Outer edge of the stroke: what a gradient is spread over.
    pub fn bounds(&self) -> Rect {
        let h = self.width / 2.0;
        Rect { x: self.rect.x - h, y: self.rect.y - h, w: self.rect.w + self.width, h: self.rect.h + self.width }
    }
}

/// Draws `shapes` onto `image`, which shows `area` of the desktop. Coverage is computed exactly per
/// pixel (axis-aligned outer box minus inner box), so edges are antialiased and corners mitred, as
/// in the overlay preview.
pub fn burn(image: &mut Image, area: Rect, shapes: &[Shape]) {
    let scale = f64::from(image.width) / area.w;
    let to_px = |r: Rect| Rect { x: (r.x - area.x) * scale, y: (r.y - area.y) * scale, w: r.w * scale, h: r.h * scale };
    for shape in shapes {
        let half = shape.width * scale / 2.0;
        let c = to_px(shape.rect);
        let outer = Rect { x: c.x - half, y: c.y - half, w: c.w + 2.0 * half, h: c.h + 2.0 * half };
        let inner = (c.w > 2.0 * half && c.h > 2.0 * half).then_some(Rect { x: c.x + half, y: c.y + half, w: c.w - 2.0 * half, h: c.h - 2.0 * half });
        let x0 = outer.x.floor().max(0.0) as u32;
        let y0 = outer.y.floor().max(0.0) as u32;
        let x1 = (outer.right().ceil().max(0.0) as u32).min(image.width);
        let y1 = (outer.bottom().ceil().max(0.0) as u32).min(image.height);
        for y in y0..y1 {
            for x in x0..x1 {
                let px = Rect { x: f64::from(x), y: f64::from(y), w: 1.0, h: 1.0 };
                let area_of = |r: &Rect| r.intersect(&px).map_or(0.0, |i| i.w * i.h);
                let coverage = (area_of(&outer) - inner.as_ref().map_or(0.0, area_of)).clamp(0.0, 1.0);
                if coverage <= 0.0 {
                    continue;
                }
                let color = shape.fill.color_at(outer, (f64::from(x) + 0.5, f64::from(y) + 0.5));
                let coverage = coverage * f64::from(color.a);
                let src = [color.r, color.g, color.b].map(|v| f64::from(v) * 255.0);
                let i = (y as usize * image.width as usize + x as usize) * 4;
                for (d, s) in image.data[i..i + 3].iter_mut().zip(src) {
                    *d = (f64::from(*d) + (s - f64::from(*d)) * coverage).round() as u8;
                }
                let a = f64::from(image.data[i + 3]);
                image.data[i + 3] = (a + (255.0 - a) * coverage).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Color;
    use crate::paint::{Gradient, Stop};

    #[test]
    fn burns_stroke_at_scale() {
        let mut img = Image::new(40, 40);
        let area = Rect { x: 100.0, y: 100.0, w: 20.0, h: 20.0 }; // scale 2
        let shape = Shape { rect: Rect { x: 105.0, y: 105.0, w: 10.0, h: 10.0 }, width: 2.0, fill: Fill::Solid(Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }) };
        burn(&mut img, area, &[shape]);
        let px = |x: usize, y: usize| &img.data[(y * 40 + x) * 4..(y * 40 + x) * 4 + 4];
        // Stroke spans 8..12 px on the left edge (centre 10 ± 2), full coverage.
        assert_eq!(px(9, 20), &[255, 0, 0, 255]);
        assert_eq!(px(11, 20), &[255, 0, 0, 255]);
        // Interior and exterior untouched.
        assert_eq!(px(20, 20), &[0, 0, 0, 0]);
        assert_eq!(px(5, 20), &[0, 0, 0, 0]);
        // Mitred corner is filled.
        assert_eq!(px(8, 8), &[255, 0, 0, 255]);
    }

    #[test]
    fn burns_gradient_across_stroke_bounds() {
        let mut img = Image::new(20, 10);
        let (red, blue) = (Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }, Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 });
        let gradient = Gradient { angle: 90.0, stops: vec![Stop { pos: 0.0, color: red }, Stop { pos: 1.0, color: blue }] };
        // Stroke 2 wide around (1,1)-(19,9): outer bounds are the whole image.
        let shape = Shape { rect: Rect { x: 1.0, y: 1.0, w: 18.0, h: 8.0 }, width: 2.0, fill: Fill::Gradient(gradient) };
        burn(&mut img, Rect { x: 0.0, y: 0.0, w: 20.0, h: 10.0 }, &[shape]);
        let px = |x: usize, y: usize| img.data[(y * 20 + x) * 4..(y * 20 + x) * 4 + 3].to_vec();
        assert!(px(0, 5)[0] > 230 && px(0, 5)[2] < 25, "left edge is red: {:?}", px(0, 5));
        assert!(px(19, 5)[2] > 230 && px(19, 5)[0] < 25, "right edge is blue: {:?}", px(19, 5));
        assert_eq!(px(10, 5), vec![0, 0, 0], "interior untouched");
    }
}
