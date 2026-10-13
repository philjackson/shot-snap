//! "Screenshot style": the capture on a padded backdrop with rounded corners and a soft drop
//! shadow. A port of Noctalia's `applyScreenshotStyle`, except the backdrop is the colour panel's
//! current fill (solid or gradient) instead of its own gradient presets.

use serde::{Deserialize, Serialize};

use crate::config::Color;
use crate::geom::Rect;
use crate::image::Image;
use crate::paint::Fill;

/// Lengths are in image pixels, as in Noctalia.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub enabled: bool,
    pub background: bool,
    pub padding: i32,
    pub radius: i32,
    pub shadow: bool,
    pub shadow_opacity: f32,
    pub shadow_blur: i32,
    pub shadow_offset_x: i32,
    pub shadow_offset_y: i32,
}

impl Default for Style {
    /// Noctalia's defaults.
    fn default() -> Self {
        Self {
            enabled: false,
            background: true,
            padding: 64,
            radius: 12,
            shadow: true,
            shadow_opacity: 0.5,
            shadow_blur: 40,
            shadow_offset_x: 0,
            shadow_offset_y: 16,
        }
    }
}

const SHADOW: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };

/// 4x4 ordered dither so wide gradients don't band at 8 bits.
const BAYER4: [[f32; 4]; 4] = [[0.0, 8.0, 2.0, 10.0], [12.0, 4.0, 14.0, 6.0], [3.0, 11.0, 1.0, 9.0], [15.0, 7.0, 13.0, 5.0]];

/// Antialiased coverage of a rounded rectangle at the centre of pixel (px, py).
fn rounded_coverage(px: i32, py: i32, x0: f32, y0: f32, w: f32, h: f32, radius: f32) -> f32 {
    let (hw, hh) = (w * 0.5, h * 0.5);
    let qx = (px as f32 + 0.5 - (x0 + hw)).abs() - (hw - radius);
    let qy = (py as f32 + 0.5 - (y0 + hh)).abs() - (hh - radius);
    let outside = qx.max(0.0).hypot(qy.max(0.0));
    let inside = qx.max(qy).min(0.0);
    (0.5 - (outside + inside - radius)).clamp(0.0, 1.0)
}

/// One running-sum box blur pass along rows (step 1) or columns (step = width). Samples past the
/// ends read as zero.
fn box_pass(src: &[f32], dst: &mut [f32], lines: usize, length: usize, line_stride: usize, step: usize, radius: usize) {
    let scale = 1.0 / (2 * radius + 1) as f32;
    for line in 0..lines {
        let base = line * line_stride;
        let at = |i: isize| if i < 0 || i >= length as isize { 0.0 } else { src[base + i as usize * step] };
        let r = radius as isize;
        let mut sum: f32 = (-r..=r).map(at).sum();
        for i in 0..length as isize {
            dst[base + i as usize * step] = sum * scale;
            sum += at(i + r + 1) - at(i - r);
        }
    }
}

/// Three box passes per axis approximate a Gaussian with sigma = blur / 2 (Noctalia's maths).
fn gaussian_blur(mask: &mut [f32], width: usize, height: usize, blur: i32) {
    let sigma = blur as f32 * 0.5;
    if sigma < 0.5 {
        return;
    }
    const PASSES: f32 = 3.0;
    let ideal = (12.0 * sigma * sigma / PASSES + 1.0).sqrt();
    let mut lower = ideal.floor() as i32;
    if lower % 2 == 0 {
        lower -= 1;
    }
    let upper = lower + 2;
    let l = lower as f32;
    let lower_count = ((12.0 * sigma * sigma - PASSES * l * l - 4.0 * PASSES * l - 3.0 * PASSES) / (-4.0 * l - 4.0)).round() as i32;
    let mut scratch = vec![0.0; mask.len()];
    for pass in 0..3 {
        let radius = ((if pass < lower_count { lower } else { upper }) - 1) / 2;
        if radius <= 0 {
            continue;
        }
        let radius = radius as usize;
        box_pass(mask, &mut scratch, height, width, width, 1, radius);
        box_pass(&scratch, mask, width, height, 1, width, radius);
    }
}

/// Straight-alpha source-over.
fn over(dst: [f32; 4], r: f32, g: f32, b: f32, a: f32) -> [f32; 4] {
    let out_a = a + dst[3] * (1.0 - a);
    if out_a <= 0.0 {
        return [0.0; 4];
    }
    let w = dst[3] * (1.0 - a);
    [(r * a + dst[0] * w) / out_a, (g * a + dst[1] * w) / out_a, (b * a + dst[2] * w) / out_a, out_a]
}

fn to_byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// `source` on its styled backdrop. `k` scales every length (1 for export; the preview renders
/// at a lower resolution). A fully transparent source yields just the frame, which is what the
/// preview draws behind the live image.
pub fn apply(source: &Image, style: &Style, fill: &Fill, k: f64) -> Image {
    let len = |v: i32| (f64::from(v) * k).round() as i32;
    let pad = len(style.padding.max(0));
    let (sw, sh) = (source.width as i32, source.height as i32);
    let (width, height) = (sw + 2 * pad, sh + 2 * pad);
    let (wu, hu) = (width as usize, height as usize);
    let (x0, y0, iw, ih) = (pad as f32, pad as f32, sw as f32, sh as f32);
    let radius = (len(style.radius) as f32).clamp(0.0, iw.min(ih) * 0.5);

    let shadow_alpha = style.shadow_opacity.clamp(0.0, 1.0);
    let shadow = (style.shadow && shadow_alpha > 0.0).then(|| {
        let mut mask = vec![0.0f32; wu * hu];
        let (sx, sy) = (x0 + len(style.shadow_offset_x) as f32, y0 + len(style.shadow_offset_y) as f32);
        let (minx, miny) = ((sx.floor() as i32).max(0), (sy.floor() as i32).max(0));
        let (maxx, maxy) = (((sx + iw).ceil() as i32).min(width), ((sy + ih).ceil() as i32).min(height));
        for y in miny..maxy {
            for x in minx..maxx {
                mask[y as usize * wu + x as usize] = rounded_coverage(x, y, sx, sy, iw, ih, radius);
            }
        }
        gaussian_blur(&mut mask, wu, hu, len(style.shadow_blur.max(0)));
        mask
    });

    let canvas = Rect { x: 0.0, y: 0.0, w: f64::from(width), h: f64::from(height) };
    let mut out = Image::new(width as u32, height as u32);
    for y in 0..height {
        for x in 0..width {
            let i = y as usize * wu + x as usize;
            let mut px = [0.0f32; 4];
            if style.background {
                let c = fill.color_at(canvas, (f64::from(x) + 0.5, f64::from(y) + 0.5));
                let dither = ((BAYER4[(y & 3) as usize][(x & 3) as usize] + 0.5) / 16.0 - 0.5) / 255.0;
                px = over(px, c.r + dither, c.g + dither, c.b + dither, c.a.clamp(0.0, 1.0));
            }
            if let Some(mask) = &shadow
                && mask[i] > 0.0
            {
                px = over(px, SHADOW.r, SHADOW.g, SHADOW.b, mask[i] * shadow_alpha);
            }
            let (ix, iy) = (x - pad, y - pad);
            if ix >= 0 && iy >= 0 && ix < sw && iy < sh {
                let coverage = rounded_coverage(x, y, x0, y0, iw, ih, radius);
                if coverage > 0.0 {
                    let s = (iy as usize * source.width as usize + ix as usize) * 4;
                    let alpha = f32::from(source.data[s + 3]) / 255.0 * coverage;
                    if alpha >= 1.0 {
                        out.data[i * 4..i * 4 + 4].copy_from_slice(&source.data[s..s + 4]);
                        continue;
                    }
                    let c = |o: usize| f32::from(source.data[s + o]) / 255.0;
                    px = over(px, c(0), c(1), c(2), alpha);
                }
            }
            out.data[i * 4..i * 4 + 4].copy_from_slice(&px.map(to_byte));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paint::{Gradient, Stop};

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Image {
        Image { width: w, height: h, data: rgba.repeat((w * h) as usize) }
    }

    fn px(img: &Image, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * img.width + x) * 4) as usize;
        img.data[i..i + 4].try_into().unwrap()
    }

    fn plain() -> Style {
        Style { enabled: true, background: true, padding: 10, radius: 0, shadow: false, ..Style::default() }
    }

    #[test]
    fn pads_and_keeps_the_capture() {
        let src = solid(20, 10, [10, 20, 30, 255]);
        let out = apply(&src, &plain(), &Fill::Solid(Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }), 1.0);
        assert_eq!((out.width, out.height), (40, 30));
        assert_eq!(px(&out, 15, 15), [10, 20, 30, 255]);
        assert_eq!(&px(&out, 2, 2)[..], &[255, 0, 0, 255]);
    }

    #[test]
    fn gradient_backdrop_runs_corner_to_corner() {
        let (red, blue) = (Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }, Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 });
        let g = Gradient { angle: 90.0, stops: vec![Stop { pos: 0.0, color: red }, Stop { pos: 1.0, color: blue }] };
        let out = apply(&solid(20, 10, [0, 0, 0, 255]), &plain(), &Fill::Gradient(g), 1.0);
        let (left, right) = (px(&out, 0, 15), px(&out, 39, 15));
        assert!(left[0] > 240 && left[2] < 15, "{left:?}");
        assert!(right[2] > 240 && right[0] < 15, "{right:?}");
    }

    #[test]
    fn rounds_corners_and_drops_background_when_off() {
        let style = Style { radius: 8, background: false, ..plain() };
        let out = apply(&solid(40, 40, [200, 200, 200, 255]), &style, &Fill::default(), 1.0);
        assert_eq!(px(&out, 10, 10)[3], 0, "corner pixel cut away");
        assert_eq!(px(&out, 30, 30), [200, 200, 200, 255]);
        assert_eq!(px(&out, 2, 30)[3], 0, "no backdrop");
    }

    #[test]
    fn shadow_falls_below_with_offset() {
        let style = Style { background: false, shadow: true, shadow_blur: 4, shadow_offset_y: 8, padding: 20, ..plain() };
        let out = apply(&solid(20, 20, [255, 255, 255, 255]), &style, &Fill::default(), 1.0);
        let (above, below) = (px(&out, 30, 12)[3], px(&out, 30, 45)[3]);
        assert!(below > above + 40, "above {above} below {below}");
    }
}
