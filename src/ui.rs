//! Small drawing helpers shared by the overlay surfaces and the colour picker.

use femtovg::renderer::OpenGl;
use femtovg::{Baseline, Canvas, Paint, Path};

use crate::config::Color;

pub fn fill_rect(canvas: &mut Canvas<OpenGl>, x: f32, y: f32, w: f32, h: f32, paint: Paint) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let mut path = Path::new();
    path.rect(x, y, w, h);
    canvas.fill_path(&path, &paint);
}

pub fn text_paint(font: femtovg::FontId, size: f32, color: Color) -> Paint {
    let mut paint = Paint::color(color.fv()).with_font(&[font]).with_font_size(size);
    paint.set_text_baseline(Baseline::Middle);
    paint
}

pub fn text_width(canvas: &Canvas<OpenGl>, text: &str, paint: &Paint) -> f32 {
    canvas.measure_text(0.0, 0.0, text, paint).map(|m| m.width()).unwrap_or(0.0)
}

pub fn card(canvas: &mut Canvas<OpenGl>, r: (f32, f32, f32, f32), radius: f32, fill: Color, border: Color) {
    let mut path = Path::new();
    path.rounded_rect(r.0, r.1, r.2, r.3, radius);
    canvas.fill_path(&path, &Paint::color(fill.fv()));
    let mut path = Path::new();
    path.rounded_rect(r.0 + 0.5, r.1 + 0.5, r.2 - 1.0, r.3 - 1.0, radius);
    canvas.stroke_path(&path, &Paint::color(border.fv()).with_line_width(1.0));
}
