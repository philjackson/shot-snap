//! Annotation shapes and burning them into the exported image.

use crate::config::Color;
use crate::geom::Rect;
use crate::image::Image;

/// Noctalia's first swatch and default shape weight.
pub const DEFAULT_COLOR: Color = Color { r: 0.96, g: 0.20, b: 0.28, a: 1.0 };
pub const DEFAULT_WIDTH: f64 = 6.0;

/// A stroked rectangle. `rect` is the stroke's centre line in global logical coordinates;
/// `width` is in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    pub rect: Rect,
    pub width: f64,
    pub color: Color,
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
        let src = [shape.color.r, shape.color.g, shape.color.b].map(|v| f64::from(v) * 255.0);
        for y in y0..y1 {
            for x in x0..x1 {
                let px = Rect { x: f64::from(x), y: f64::from(y), w: 1.0, h: 1.0 };
                let area_of = |r: &Rect| r.intersect(&px).map_or(0.0, |i| i.w * i.h);
                let coverage = (area_of(&outer) - inner.as_ref().map_or(0.0, area_of)).clamp(0.0, 1.0) * f64::from(shape.color.a);
                if coverage <= 0.0 {
                    continue;
                }
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

    #[test]
    fn burns_stroke_at_scale() {
        let mut img = Image::new(40, 40);
        let area = Rect { x: 100.0, y: 100.0, w: 20.0, h: 20.0 }; // scale 2
        let shape = Shape { rect: Rect { x: 105.0, y: 105.0, w: 10.0, h: 10.0 }, width: 2.0, color: Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 } };
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
}
