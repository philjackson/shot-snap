//! Draws annotations with femtovg, for the editor preview and for the export, so what you see is
//! exactly what gets saved.

use std::collections::HashMap;

use anyhow::anyhow;
use femtovg::renderer::OpenGl;
use femtovg::{Baseline, Canvas, FontId, ImageFilter, ImageFlags, ImageId, LineCap, LineJoin, Paint, Path, PixelFormat, RenderTarget};

use crate::annotate::{Kind, Shape, HIGHLIGHT_ALPHA};
use crate::geom::Rect;
use crate::image::Image;
use crate::ui::fill_rect;

/// Image pixels → canvas coordinates: `(origin + p / fit)`.
#[derive(Debug, Clone, Copy)]
pub struct Map {
    pub ox: f64,
    pub oy: f64,
    pub fit: f64,
}

impl Map {
    fn pt(&self, p: (f64, f64)) -> (f32, f32) {
        ((self.ox + p.0 / self.fit) as f32, (self.oy + p.1 / self.fit) as f32)
    }

    fn rect(&self, r: Rect) -> Rect {
        Rect { x: self.ox + r.x / self.fit, y: self.oy + r.y / self.fit, w: r.w / self.fit, h: r.h / self.fit }
    }
}

/// Blurred copies of the image being annotated, keyed by `blur_key(sigma)`. Each lives in the
/// canvas (one GL context), so every surface keeps its own.
pub type BlurCache = HashMap<u32, ImageId>;

fn blur_key(sigma: f64) -> u32 {
    (sigma * 2.0).round().max(1.0) as u32
}

/// femtovg caps its Gaussian at sigma 8, so a strong blur is done on a smaller copy and scaled
/// back up; at these strengths the difference doesn't show.
const MAX_FILTER_SIGMA: f64 = 6.0;

/// Makes sure every blur strength in `shapes` has a blurred copy of `texture` (an image of
/// `size` pixels). Must run outside of drawing a frame or tile: it switches render targets.
pub fn prepare_blurs(canvas: &mut Canvas<OpenGl>, cache: &mut BlurCache, texture: ImageId, size: (u32, u32), shapes: &[Shape]) {
    for shape in shapes {
        let Kind::Blur(_) = shape.kind else { continue };
        let key = blur_key(shape.width);
        if cache.contains_key(&key) {
            continue;
        }
        let sigma = f64::from(key) / 2.0;
        let k = (sigma / MAX_FILTER_SIGMA).ceil().max(1.0);
        let sw = (f64::from(size.0) / k).ceil().max(1.0) as usize;
        let sh = (f64::from(size.1) / k).ceil().max(1.0) as usize;
        let (Ok(small), Ok(blurred)) = (
            canvas.create_image_empty(sw, sh, PixelFormat::Rgba8, ImageFlags::empty()),
            canvas.create_image_empty(sw, sh, PixelFormat::Rgba8, ImageFlags::empty()),
        ) else {
            continue;
        };
        canvas.save();
        canvas.reset_transform();
        canvas.reset_scissor();
        canvas.set_render_target(RenderTarget::Image(small));
        canvas.clear_rect(0, 0, sw as u32, sh as u32, femtovg::Color::rgbaf(0.0, 0.0, 0.0, 0.0));
        fill_rect(canvas, 0.0, 0.0, sw as f32, sh as f32, Paint::image(texture, 0.0, 0.0, sw as f32, sh as f32, 0.0, 1.0));
        canvas.filter_image(blurred, ImageFilter::GaussianBlur { sigma: (sigma / k) as f32 }, small);
        canvas.set_render_target(RenderTarget::Screen);
        canvas.restore();
        cache.insert(key, blurred);
    }
}

/// Line height of annotation text relative to its size, shared with the caret.
const LINE_HEIGHT: f32 = 1.2;

fn text_paint(font: FontId, shape: &Shape, map: &Map) -> Paint {
    let mut paint = Paint::color(femtovg::Color::black()).with_font(&[font]).with_font_size((shape.width / map.fit) as f32);
    paint.set_text_baseline(Baseline::Top);
    paint
}

/// Bounds of a text shape in image pixels, for gradients and hit testing.
pub fn text_bounds(canvas: &Canvas<OpenGl>, font: FontId, at: (f64, f64), text: &str, size: f64) -> Rect {
    let paint = text_paint(font, &Shape { kind: Kind::Text { at, text: String::new() }, width: size, fill: Default::default() }, &Map { ox: 0.0, oy: 0.0, fit: 1.0 });
    let w = canvas.measure_text(0.0, 0.0, text, &paint).map(|m| f64::from(m.width())).unwrap_or(0.0);
    Rect { x: at.0, y: at.1, w: w.max(1.0), h: size * f64::from(LINE_HEIGHT) }
}

/// What the shapes are drawn over: the image's size (for mapping blurred copies) and its blurs.
pub struct Backdrop<'a> {
    pub size: (u32, u32),
    pub blurs: &'a BlurCache,
}

/// Draws `shapes`. `caret` puts a text caret after the given text shape. `scale` is image pixels
/// per logical pixel (for arrowhead sizing). Blurs go first, underneath everything else, so a
/// later blur never wipes out an earlier arrow or label.
pub fn draw_shapes(canvas: &mut Canvas<OpenGl>, font: Option<FontId>, shapes: &[Shape], map: Map, scale: f64, caret: Option<usize>, backdrop: &Backdrop) {
    let (ox, oy) = map.pt((0.0, 0.0));
    let (iw, ih) = ((f64::from(backdrop.size.0) / map.fit) as f32, (f64::from(backdrop.size.1) / map.fit) as f32);
    for shape in shapes {
        if let Kind::Blur(r) = shape.kind
            && let Some(&blurred) = backdrop.blurs.get(&blur_key(shape.width))
        {
            let r = map.rect(r);
            fill_rect(canvas, r.x as f32, r.y as f32, r.w as f32, r.h as f32, Paint::image(blurred, ox, oy, iw, ih, 0.0, 1.0));
        }
    }
    let mut number = 0;
    for (i, shape) in shapes.iter().enumerate() {
        let line_width = (shape.width / map.fit) as f32;
        match &shape.kind {
            Kind::Blur(_) => {}
            Kind::Number { at } => {
                number += 1;
                let (cx, cy) = map.pt(*at);
                let r = line_width / 2.0;
                let mut circle = Path::new();
                circle.circle(cx, cy, r);
                let bounds = map.rect(shape.stroke_bounds(scale).unwrap_or_default());
                canvas.fill_path(&circle, &shape.fill.paint(bounds));
                if let Some(font) = font {
                    // Noctalia: 60% of the diameter, black on light fills and white on dark ones.
                    let base = match &shape.fill {
                        crate::paint::Fill::Solid(c) => *c,
                        crate::paint::Fill::Gradient(g) => g.sample(0.5),
                    };
                    let luminance = 0.299 * base.r + 0.587 * base.g + 0.114 * base.b;
                    let ink = if luminance > 0.5 { femtovg::Color::black() } else { femtovg::Color::white() };
                    let mut paint = Paint::color(ink).with_font(&[font]).with_font_size(line_width * 0.6);
                    paint.set_text_baseline(Baseline::Middle);
                    let label = number.to_string();
                    let tw = canvas.measure_text(0.0, 0.0, &label, &paint).map(|m| m.width()).unwrap_or(0.0);
                    let _ = canvas.fill_text(cx - tw / 2.0, cy, &label, &paint);
                }
            }
            Kind::Highlight(r) => {
                let r = map.rect(*r);
                canvas.save();
                canvas.set_global_alpha(HIGHLIGHT_ALPHA);
                fill_rect(canvas, r.x as f32, r.y as f32, r.w as f32, r.h as f32, shape.fill.paint(r));
                canvas.restore();
            }
            Kind::Rectangle(r) => {
                let r = map.rect(*r);
                let mut path = Path::new();
                path.rect(r.x as f32, r.y as f32, r.w as f32, r.h as f32);
                let bounds = map.rect(shape.stroke_bounds(scale).unwrap_or(r));
                canvas.stroke_path(&path, &shape.fill.paint(bounds).with_line_width(line_width).with_line_join(LineJoin::Miter));
            }
            Kind::Arrow { from, to, heads } => {
                let mut path = Path::new();
                for (a, b) in Shape::arrow_segments(*from, *to, *heads, shape.width, scale) {
                    let (a, b) = (map.pt(a), map.pt(b));
                    path.move_to(a.0, a.1);
                    path.line_to(b.0, b.1);
                }
                let bounds = map.rect(shape.stroke_bounds(scale).unwrap_or_default());
                let paint = shape.fill.paint(bounds).with_line_width(line_width).with_line_cap(LineCap::Round).with_line_join(LineJoin::Round);
                canvas.stroke_path(&path, &paint);
            }
            Kind::Text { at, text } => {
                let Some(font) = font else { continue };
                let bounds = text_bounds(canvas, font, *at, text, shape.width);
                let (x, y) = map.pt(*at);
                if !text.is_empty() {
                    let mut paint = shape.fill.paint(map.rect(bounds));
                    paint.set_font(&[font]);
                    paint.set_font_size(line_width);
                    paint.set_text_baseline(Baseline::Top);
                    let _ = canvas.fill_text(x, y, text, &paint);
                }
                if caret == Some(i) {
                    let cx = if text.is_empty() { x } else { (map.ox + (bounds.right()) / map.fit) as f32 + 1.0 };
                    let color = match &shape.fill {
                        crate::paint::Fill::Solid(c) => *c,
                        crate::paint::Fill::Gradient(g) => g.sample(1.0),
                    };
                    fill_rect(canvas, cx, y, 2.0, line_width * LINE_HEIGHT, Paint::color(color.fv()));
                }
            }
        }
    }
}

/// Largest offscreen tile: comfortably below every GL implementation's texture limit.
const TILE: usize = 4096;

/// Renders `image` (already uploaded as `texture`) with `shapes` burned in, at full resolution.
/// Draws into offscreen tiles and reads them back.
pub fn export(canvas: &mut Canvas<OpenGl>, font: Option<FontId>, texture: ImageId, image: &Image, shapes: &[Shape], scale: f64, blurs: &mut BlurCache) -> anyhow::Result<Image> {
    let (w, h) = (image.width as usize, image.height as usize);
    prepare_blurs(canvas, blurs, texture, (image.width, image.height), shapes);
    let backdrop = Backdrop { size: (image.width, image.height), blurs };
    let mut out = Image::new(image.width, image.height);
    for ty in (0..h).step_by(TILE) {
        for tx in (0..w).step_by(TILE) {
            let (tw, th) = (TILE.min(w - tx), TILE.min(h - ty));
            let target = canvas.create_image_empty(tw, th, PixelFormat::Rgba8, ImageFlags::empty()).map_err(|e| anyhow!("offscreen image: {e:?}"))?;
            canvas.set_render_target(RenderTarget::Image(target));
            canvas.reset();
            canvas.clear_rect(0, 0, tw as u32, th as u32, femtovg::Color::rgbaf(0.0, 0.0, 0.0, 0.0));
            let (ox, oy) = (-(tx as f32), -(ty as f32));
            fill_rect(canvas, ox, oy, w as f32, h as f32, Paint::image(texture, ox, oy, w as f32, h as f32, 0.0, 1.0));
            draw_shapes(canvas, font, shapes, Map { ox: f64::from(ox), oy: f64::from(oy), fit: 1.0 }, scale, None, &backdrop);
            canvas.flush();
            let shot = canvas.screenshot();
            canvas.set_render_target(RenderTarget::Screen);
            canvas.delete_image(target);
            let shot = shot.map_err(|e| anyhow!("reading back the export: {e:?}"))?;
            for (row, line) in shot.rows().enumerate() {
                let dst = ((ty + row) * w + tx) * 4;
                for (i, px) in line.iter().enumerate() {
                    // The framebuffer holds premultiplied alpha.
                    let a = px.a;
                    let un = |c: u8| if a == 0 { 0 } else { ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8 };
                    out.data[dst + i * 4..dst + i * 4 + 4].copy_from_slice(&[un(px.r), un(px.g), un(px.b), a]);
                }
            }
        }
    }
    Ok(out)
}
