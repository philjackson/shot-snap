//! Draws annotations with femtovg, for the editor preview and for the export, so what you see is
//! exactly what gets saved.

use anyhow::anyhow;
use femtovg::renderer::OpenGl;
use femtovg::{Baseline, Canvas, FontId, ImageFlags, ImageId, LineCap, LineJoin, Paint, Path, PixelFormat, RenderTarget};

use crate::annotate::{Kind, Shape};
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

/// Draws `shapes`. `caret` puts a text caret after the given text shape. `scale` is image pixels
/// per logical pixel (for arrowhead sizing).
pub fn draw_shapes(canvas: &mut Canvas<OpenGl>, font: Option<FontId>, shapes: &[Shape], map: Map, scale: f64, caret: Option<usize>) {
    for (i, shape) in shapes.iter().enumerate() {
        let line_width = (shape.width / map.fit) as f32;
        match &shape.kind {
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
pub fn export(canvas: &mut Canvas<OpenGl>, font: Option<FontId>, texture: ImageId, image: &Image, shapes: &[Shape], scale: f64) -> anyhow::Result<Image> {
    let (w, h) = (image.width as usize, image.height as usize);
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
            draw_shapes(canvas, font, shapes, Map { ox: f64::from(ox), oy: f64::from(oy), fit: 1.0 }, scale, None);
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
