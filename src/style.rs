//! "Screenshot style": the capture on a padded backdrop with rounded corners and a soft drop
//! shadow. A port of Noctalia's `applyScreenshotStyle`; the backdrop is any solid or gradient
//! fill, picked with the same colour selector as the annotations. Optionally a Hyprland-style
//! border around the capture: outside it, following its corners, its gradient blended in OkLab.

use serde::{Deserialize, Serialize};

use crate::config::Color;
use crate::geom::Rect;
use crate::image::Image;
use crate::paint::{Fill, Gradient, Stop};

/// Lengths are in image pixels, as in Noctalia.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub enabled: bool,
    pub background: bool,
    /// The backdrop's own colour, separate from the annotations'.
    pub backdrop: Fill,
    pub padding: i32,
    pub radius: i32,
    pub shadow: bool,
    pub shadow_opacity: f32,
    pub shadow_blur: i32,
    pub shadow_offset_x: i32,
    pub shadow_offset_y: i32,
    pub border: bool,
    pub border_fill: Fill,
    pub border_width: i32,
    /// Hyprland's default `decoration:shadow`: a tight dark edge just outside the border.
    pub border_shadow: bool,
}

impl Default for Style {
    /// Noctalia's defaults.
    fn default() -> Self {
        Self {
            enabled: false,
            background: true,
            backdrop: default_backdrop(),
            padding: 64,
            radius: 12,
            shadow: true,
            shadow_opacity: 0.5,
            shadow_blur: 40,
            shadow_offset_x: 0,
            shadow_offset_y: 16,
            border: false,
            border_fill: default_border(),
            border_width: 2,
            border_shadow: true,
        }
    }
}

impl Style {
    /// How far the frame reaches past the capture on each side, in image pixels.
    pub fn margin(&self) -> i32 {
        self.padding.max(0) + self.border_px()
    }

    fn border_px(&self) -> i32 {
        if self.border { self.border_width.max(0) } else { 0 }
    }
}

/// Noctalia's default style backdrop: its theme gradient, primary to tertiary, at 135°.
pub fn default_backdrop() -> Fill {
    let hex = |h: u32| Color { r: ((h >> 16) & 0xff) as f32 / 255.0, g: ((h >> 8) & 0xff) as f32 / 255.0, b: (h & 0xff) as f32 / 255.0, a: 1.0 };
    Fill::Gradient(Gradient { angle: 135.0, stops: vec![Stop { pos: 0.0, color: hex(0xfff59b) }, Stop { pos: 1.0, color: hex(0x9bfece) }] })
}

/// Hyprland's stock `col.active_border = rgba(33ccffee) rgba(00ff99ee) 45deg`. Hyprland's 45deg
/// runs top-left to bottom-right, which is 135° here.
pub fn default_border() -> Fill {
    let c = |h: u32| Color { r: ((h >> 24) & 0xff) as f32 / 255.0, g: ((h >> 16) & 0xff) as f32 / 255.0, b: ((h >> 8) & 0xff) as f32 / 255.0, a: (h & 0xff) as f32 / 255.0 };
    Fill::Gradient(Gradient { angle: 135.0, stops: vec![Stop { pos: 0.0, color: c(0x33ccffee) }, Stop { pos: 1.0, color: c(0x00ff99ee) }] })
}

const SHADOW: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };

/// Hyprland's shadow defaults: `color = rgba(1a1a1aee)`, `range = 4`, `render_power = 3`.
const EDGE_SHADOW: Color = Color { r: 26.0 / 255.0, g: 26.0 / 255.0, b: 26.0 / 255.0, a: 238.0 / 255.0 };
const EDGE_SHADOW_RANGE: f32 = 4.0;
const EDGE_SHADOW_POWER: i32 = 3;

/// Half the width of Hyprland's antialiasing ramp (its `SMOOTHING_CONSTANT`).
const SMOOTHING: f32 = std::f32::consts::PI / 5.346_658;

/// 4x4 ordered dither so wide gradients don't band at 8 bits.
const BAYER4: [[f32; 4]; 4] = [[0.0, 8.0, 2.0, 10.0], [12.0, 4.0, 14.0, 6.0], [3.0, 11.0, 1.0, 9.0], [15.0, 7.0, 13.0, 5.0]];

/// Signed distance from the centre of pixel (px, py) to a rounded rectangle's edge, negative inside.
fn rounded_distance(px: i32, py: i32, x0: f32, y0: f32, w: f32, h: f32, radius: f32) -> f32 {
    let (hw, hh) = (w * 0.5, h * 0.5);
    let qx = (px as f32 + 0.5 - (x0 + hw)).abs() - (hw - radius);
    let qy = (py as f32 + 0.5 - (y0 + hh)).abs() - (hh - radius);
    let outside = qx.max(0.0).hypot(qy.max(0.0));
    let inside = qx.max(qy).min(0.0);
    outside + inside - radius
}

/// Antialiased coverage of a rounded rectangle at the centre of pixel (px, py).
fn rounded_coverage(px: i32, py: i32, x0: f32, y0: f32, w: f32, h: f32, radius: f32) -> f32 {
    (0.5 - rounded_distance(px, py, x0, y0, w, h, radius)).clamp(0.0, 1.0)
}

/// Coverage at signed distance `d`, with Hyprland's slightly softer smoothstep edge.
fn smooth_coverage(d: f32) -> f32 {
    let t = ((d + SMOOTHING) / (2.0 * SMOOTHING)).clamp(0.0, 1.0);
    1.0 - t * t * (3.0 - 2.0 * t)
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

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

fn to_oklab(c: Color) -> [f32; 4] {
    let (r, g, b) = (srgb_to_linear(c.r), srgb_to_linear(c.g), srgb_to_linear(c.b));
    let l = (0.412_221_5 * r + 0.536_332_55 * g + 0.051_445_99 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
        c.a,
    ]
}

fn from_oklab([l, a, b, alpha]: [f32; 4]) -> Color {
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    let r = 4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_;
    let g = -1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_;
    let b = -0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_;
    Color { r: linear_to_srgb(r.clamp(0.0, 1.0)), g: linear_to_srgb(g.clamp(0.0, 1.0)), b: linear_to_srgb(b.clamp(0.0, 1.0)), a: alpha }
}

/// The border's colour at `p`: as `Fill::color_at`, but blended in OkLab as Hyprland does, which
/// keeps a gradient's middle from going muddy.
fn border_color_at(fill: &Fill, bounds: Rect, p: (f64, f64)) -> Color {
    let Fill::Gradient(g) = fill else { return fill.color_at(bounds, p) };
    let stops = g.sorted();
    let (Some(first), Some(last)) = (stops.first(), stops.last()) else { return fill.color_at(bounds, p) };
    let (s, e) = g.line(bounds);
    let (vx, vy) = (e.0 - s.0, e.1 - s.1);
    let len2 = vx * vx + vy * vy;
    let t = if len2 > 0.0 { ((p.0 - s.0) * vx + (p.1 - s.1) * vy) / len2 } else { 0.0 } as f32;
    if t <= first.pos {
        return first.color;
    }
    if t >= last.pos {
        return last.color;
    }
    let i = stops.iter().position(|s| s.pos >= t).unwrap_or(stops.len() - 1).max(1);
    let (a, b) = (stops[i - 1], stops[i]);
    let k = if b.pos > a.pos { (t - a.pos) / (b.pos - a.pos) } else { 1.0 };
    let (la, lb) = (to_oklab(a.color), to_oklab(b.color));
    from_oklab(std::array::from_fn(|j| la[j] + (lb[j] - la[j]) * k))
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
/// at a lower resolution).
pub fn apply(source: &Image, style: &Style, k: f64) -> Image {
    compose(source, style, k, false)
}

/// Just the frame around a `width` x `height` capture, which the preview draws behind the live
/// image. The border fills in under the capture too, so the image's antialiased edge drawn over
/// it can't leave a seam.
pub fn frame(width: u32, height: u32, style: &Style, k: f64) -> Image {
    compose(&Image::new(width, height), style, k, true)
}

fn compose(source: &Image, style: &Style, k: f64, border_under: bool) -> Image {
    let fill = &style.backdrop;
    let len = |v: i32| (f64::from(v) * k).round() as i32;
    let bw = len(style.border_px());
    let pad = len(style.padding.max(0)) + bw;
    let (sw, sh) = (source.width as i32, source.height as i32);
    let (width, height) = (sw + 2 * pad, sh + 2 * pad);
    let (wu, hu) = (width as usize, height as usize);
    let (x0, y0, iw, ih) = (pad as f32, pad as f32, sw as f32, sh as f32);
    let radius = (len(style.radius) as f32).clamp(0.0, iw.min(ih) * 0.5);
    // The border's box: the capture grown by its width, with the corners grown to match so it's
    // even all the way round (square stays square, as in Hyprland).
    let b = bw as f32;
    let (ox, oy, ow, oh) = (x0 - b, y0 - b, iw + 2.0 * b, ih + 2.0 * b);
    let outer_radius = if radius > 0.0 { radius + b } else { 0.0 };
    let outer_bounds = Rect { x: f64::from(ox), y: f64::from(oy), w: f64::from(ow), h: f64::from(oh) };
    let edge_range = EDGE_SHADOW_RANGE * k as f32;
    let edge_shadow = style.border && style.border_shadow && edge_range > 0.0;

    // The shadow is cast by the capture and its border together.
    let shadow_alpha = style.shadow_opacity.clamp(0.0, 1.0);
    let shadow = (style.shadow && shadow_alpha > 0.0).then(|| {
        let mut mask = vec![0.0f32; wu * hu];
        let (sx, sy) = (ox + len(style.shadow_offset_x) as f32, oy + len(style.shadow_offset_y) as f32);
        let (minx, miny) = ((sx.floor() as i32).max(0), (sy.floor() as i32).max(0));
        let (maxx, maxy) = (((sx + ow).ceil() as i32).min(width), ((sy + oh).ceil() as i32).min(height));
        for y in miny..maxy {
            for x in minx..maxx {
                mask[y as usize * wu + x as usize] = rounded_coverage(x, y, sx, sy, ow, oh, outer_radius);
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
            // Distance outside the border's box, for its edge and Hyprland's shadow.
            let near_border = (bw > 0 || edge_shadow) && x as f32 >= ox - edge_range - 1.0 && y as f32 >= oy - edge_range - 1.0 && x as f32 <= ox + ow + edge_range && y as f32 <= oy + oh + edge_range;
            let outer = near_border.then(|| smooth_coverage(rounded_distance(x, y, ox, oy, ow, oh, outer_radius)));
            if edge_shadow && let Some(outer) = outer {
                // Hyprland's falloff, (1 - d / range)^power from the border's edge outwards, cut
                // away under the border itself.
                let d = rounded_distance(x, y, ox, oy, ow, oh, outer_radius).max(0.0);
                let a = (1.0 - d / edge_range).max(0.0).powi(EDGE_SHADOW_POWER) * EDGE_SHADOW.a * (1.0 - outer);
                if a > 0.0 {
                    px = over(px, EDGE_SHADOW.r, EDGE_SHADOW.g, EDGE_SHADOW.b, a);
                }
            }
            let (ix, iy) = (x - pad, y - pad);
            let in_capture = ix >= 0 && iy >= 0 && ix < sw && iy < sh;
            let coverage = if in_capture { rounded_coverage(x, y, x0, y0, iw, ih, radius) } else { 0.0 };
            // The border and the capture don't overlap, so they're summed (premultiplied) into
            // one layer before going over the backdrop: no seam where they meet.
            let mut layer = [0.0f32; 4];
            if bw > 0
                && let Some(outer) = outer
            {
                let ring = if border_under { outer } else { (outer - coverage).max(0.0) };
                if ring > 0.0 {
                    let c = border_color_at(&style.border_fill, outer_bounds, (f64::from(x) + 0.5, f64::from(y) + 0.5));
                    let a = ring * c.a.clamp(0.0, 1.0);
                    layer = [c.r * a, c.g * a, c.b * a, a];
                }
            }
            if coverage > 0.0 {
                let s = (iy as usize * source.width as usize + ix as usize) * 4;
                let alpha = f32::from(source.data[s + 3]) / 255.0 * coverage;
                if alpha >= 1.0 {
                    out.data[i * 4..i * 4 + 4].copy_from_slice(&source.data[s..s + 4]);
                    continue;
                }
                let c = |o: usize| f32::from(source.data[s + o]) / 255.0;
                layer = [layer[0] + c(0) * alpha, layer[1] + c(1) * alpha, layer[2] + c(2) * alpha, layer[3] + alpha];
            }
            if layer[3] > 0.0 {
                let a = layer[3].min(1.0);
                px = over(px, layer[0] / layer[3], layer[1] / layer[3], layer[2] / layer[3], a);
            }
            out.data[i * 4..i * 4 + 4].copy_from_slice(&px.map(to_byte));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let style = Style { backdrop: Fill::Solid(Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }), ..plain() };
        let out = apply(&src, &style, 1.0);
        assert_eq!((out.width, out.height), (40, 30));
        assert_eq!(px(&out, 15, 15), [10, 20, 30, 255]);
        assert_eq!(&px(&out, 2, 2)[..], &[255, 0, 0, 255]);
    }

    #[test]
    fn gradient_backdrop_runs_corner_to_corner() {
        let (red, blue) = (Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }, Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 });
        let g = Gradient { angle: 90.0, stops: vec![Stop { pos: 0.0, color: red }, Stop { pos: 1.0, color: blue }] };
        let out = apply(&solid(20, 10, [0, 0, 0, 255]), &Style { backdrop: Fill::Gradient(g), ..plain() }, 1.0);
        let (left, right) = (px(&out, 0, 15), px(&out, 39, 15));
        assert!(left[0] > 240 && left[2] < 15, "{left:?}");
        assert!(right[2] > 240 && right[0] < 15, "{right:?}");
    }

    #[test]
    fn rounds_corners_and_drops_background_when_off() {
        let style = Style { radius: 8, background: false, ..plain() };
        let out = apply(&solid(40, 40, [200, 200, 200, 255]), &style, 1.0);
        assert_eq!(px(&out, 10, 10)[3], 0, "corner pixel cut away");
        assert_eq!(px(&out, 30, 30), [200, 200, 200, 255]);
        assert_eq!(px(&out, 2, 30)[3], 0, "no backdrop");
    }

    #[test]
    fn shadow_falls_below_with_offset() {
        let style = Style { background: false, shadow: true, shadow_blur: 4, shadow_offset_y: 8, padding: 20, ..plain() };
        let out = apply(&solid(20, 20, [255, 255, 255, 255]), &style, 1.0);
        let (above, below) = (px(&out, 30, 12)[3], px(&out, 30, 45)[3]);
        assert!(below > above + 40, "above {above} below {below}");
    }

    #[test]
    fn border_goes_outside_the_capture() {
        let red = Fill::Solid(Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 });
        let style = Style { background: false, border: true, border_width: 3, border_fill: red, border_shadow: false, ..plain() };
        let out = apply(&solid(20, 10, [10, 20, 30, 255]), &style, 1.0);
        assert_eq!((out.width, out.height), (46, 36));
        assert_eq!(px(&out, 11, 15), [255, 0, 0, 255], "border");
        assert_eq!(px(&out, 13, 15), [10, 20, 30, 255], "capture untouched");
        assert_eq!(px(&out, 8, 15)[3], 0, "padding beyond the border");
    }

    #[test]
    fn rounded_border_has_no_seam() {
        let white = Fill::Solid(Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 });
        let style = Style { background: false, border: true, border_width: 4, radius: 10, border_fill: white, ..plain() };
        let out = apply(&solid(40, 40, [255, 255, 255, 255]), &style, 1.0);
        // Inside the outer edge everything's fully opaque, including where border meets capture.
        for d in 0..12 {
            let (x, y) = (14 + d, 14 + d);
            if x > 16 {
                assert_eq!(px(&out, x, y)[3], 255, "at {x},{y}");
            }
        }
    }

    #[test]
    fn edge_shadow_is_a_tight_dark_rim() {
        let white = Fill::Solid(Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 });
        let style = Style { backdrop: white.clone(), border: true, border_width: 2, border_fill: white, ..plain() };
        let out = apply(&solid(20, 10, [255, 255, 255, 255]), &style, 1.0);
        // The border spans x 10..12; just outside it is dark, and it's gone 4px out.
        let (rim, faded, beyond) = (px(&out, 9, 15)[0], px(&out, 7, 15)[0], px(&out, 5, 15)[0]);
        assert!(rim < 150, "rim {rim}");
        assert!(faded > 230, "faded {faded}");
        assert_eq!(beyond, 255);
        let off = apply(&solid(20, 10, [255, 255, 255, 255]), &Style { border_shadow: false, ..style }, 1.0);
        assert_eq!(px(&off, 9, 15)[0], 255);
    }

    #[test]
    fn oklab_round_trips_and_blends() {
        let c = Color { r: 0.2, g: 0.8, b: 1.0, a: 0.5 };
        let back = from_oklab(to_oklab(c));
        assert!((back.r - c.r).abs() < 1e-3 && (back.g - c.g).abs() < 1e-3 && (back.b - c.b).abs() < 1e-3 && back.a == c.a);
        let bounds = Rect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 };
        let mid = border_color_at(&default_border(), bounds, (5.0, 5.0));
        assert!(mid.g > 0.85 && mid.b > 0.6, "{mid:?}");
    }
}
