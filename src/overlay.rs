//! Full-screen layer surfaces (one per output) for region selection and the display picker.
//! Everything is laid out in logical pixels; each canvas is scaled to the output's physical
//! resolution so frozen frames show 1:1 and lines stay crisp under fractional scaling.

use std::collections::HashMap;

use femtovg::{Baseline, Canvas, ImageFlags, ImageId, Paint, Path};
use femtovg::renderer::OpenGl;
use smithay_client_toolkit::compositor::{CompositorState, FrameCallbackData};
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;
use smithay_client_toolkit::seat::keyboard::{Keysym, Modifiers};
use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface};
use smithay_client_toolkit::shell::WaylandSurface;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::QueueHandle;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::app::App;
use crate::capture::{NoEvents, OutputMeta};
use crate::config::{Color, Theme};
use crate::deliver::Action;
use crate::geom::Rect;
use crate::gl::{Gl, GlSurface};
use crate::image::OutputFrame;

const BORDER_WIDTH: f32 = 2.0;
const HANDLE_MARGIN: f64 = 15.0;
const HANDLE_RADIUS: f32 = 4.5;
const BADGE_FONT: f32 = 14.0;
const BADGE_OFFSET: (f32, f32) = (12.0, 14.0);
const BADGE_PAD: (f32, f32) = (6.0, 4.0);
const CAPTION_FONT: f32 = 13.0;
const RADIUS_SM: f32 = 3.0;
const RADIUS_MD: f32 = 6.0;
const SPACE_SM: f32 = 8.0;
const SPACE_MD: f32 = 12.0;
const BTN_LEFT: u32 = 0x110;
const MIN_SELECTION: f64 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Region,
    Picker,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Region(Rect, Action),
    Picked(u32),
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Edges {
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
}

impl Edges {
    fn any(&self) -> bool {
        self.left || self.right || self.top || self.bottom
    }

    fn cursor(&self) -> Shape {
        match (self.left, self.right, self.top, self.bottom) {
            (true, _, true, _) => Shape::NwResize,
            (_, true, true, _) => Shape::NeResize,
            (true, _, _, true) => Shape::SwResize,
            (_, true, _, true) => Shape::SeResize,
            (true, ..) => Shape::WResize,
            (_, true, ..) => Shape::EResize,
            (_, _, true, _) => Shape::NResize,
            _ => Shape::SResize,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    None,
    Select { anchor: (f64, f64) },
    Move { grab: (f64, f64) },
    Resize { edges: Edges, orig: Rect },
}

struct Surf {
    meta: OutputMeta,
    layer: LayerSurface,
    viewport: WpViewport,
    gl: Option<GlSurface>,
    texture: Option<ImageId>,
    size: (u32, u32),
    phys: (u32, u32),
    frame_pending: bool,
    dirty: bool,
    buttons: Vec<(Rect, u32)>,
}

pub struct Overlay {
    pub mode: Mode,
    surfs: Vec<Surf>,
    frozen: HashMap<u32, OutputFrame>,
    desktop: Rect,
    selection: Option<Rect>,
    awaiting: bool,
    confirm: bool,
    drag: Drag,
    pointer: Option<(f64, f64)>,
    hover_button: Option<u32>,
    theme: Theme,
    hidden: bool,
}

pub struct Init<'a> {
    pub compositor: &'a CompositorState,
    pub layer_shell: &'a LayerShell,
    pub viewporter: &'a WpViewporter,
    pub qh: &'a QueueHandle<App>,
}

impl Overlay {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        init: Init,
        mode: Mode,
        outputs: Vec<OutputMeta>,
        frozen: Vec<(u32, OutputFrame)>,
        theme: Theme,
        confirm: bool,
        preselect: Option<Rect>,
    ) -> Self {
        let desktop = outputs.iter().map(|o| o.logical).reduce(|a, b| a.union(&b)).unwrap_or_default();
        let surfs = outputs
            .into_iter()
            .map(|meta| {
                let surface = init.compositor.create_surface(init.qh);
                let viewport = init.viewporter.get_viewport(&surface, init.qh, NoEvents);
                let layer = init.layer_shell.create_layer_surface(init.qh, surface, Layer::Overlay, Some("shot-snap"), Some(&meta.wl));
                layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
                layer.set_exclusive_zone(-1);
                layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
                layer.commit();
                Surf { meta, layer, viewport, gl: None, texture: None, size: (0, 0), phys: (0, 0), frame_pending: false, dirty: true, buttons: Vec::new() }
            })
            .collect();
        let preselect = preselect.and_then(|r| r.intersect(&desktop)).filter(|r| r.w >= MIN_SELECTION && r.h >= MIN_SELECTION);
        Self {
            mode,
            surfs,
            frozen: frozen.into_iter().collect(),
            desktop,
            awaiting: preselect.is_some(),
            selection: preselect,
            confirm,
            drag: Drag::None,
            pointer: None,
            hover_button: None,
            theme,
            hidden: false,
        }
    }

    pub fn take_frozen(&mut self) -> Vec<OutputFrame> {
        let mut frames: Vec<_> = self.frozen.drain().collect();
        frames.sort_by_key(|(id, _)| *id);
        frames.into_iter().map(|(_, f)| f).collect()
    }

    pub fn selection(&self) -> Option<Rect> {
        self.selection
    }

    /// Clears every surface to transparent so the live screen can be captured without the
    /// overlay in it. Destroying instead would let compositor fade-out animations leak in.
    pub fn hide(&mut self, qh: &QueueHandle<App>) {
        self.hidden = true;
        self.redraw_all(qh);
    }

    /// True once the compositor has shown the transparent frame on every surface.
    pub fn hidden_presented(&self) -> bool {
        self.hidden && self.surfs.iter().all(|s| s.gl.is_none() || (!s.frame_pending && !s.dirty))
    }

    pub fn owns_layer(&self, layer: &LayerSurface) -> bool {
        self.surfs.iter().any(|s| &s.layer == layer)
    }

    fn surf_index(&self, surface: &WlSurface) -> Option<usize> {
        self.surfs.iter().position(|s| s.layer.wl_surface() == surface)
    }

    pub fn configure(&mut self, layer: &LayerSurface, size: (u32, u32), gl: &Gl, qh: &QueueHandle<App>) -> anyhow::Result<()> {
        let Some(i) = self.surfs.iter().position(|s| &s.layer == layer) else { return Ok(()) };
        let s = &mut self.surfs[i];
        let size = if size.0 == 0 || size.1 == 0 { (s.meta.logical.w as u32, s.meta.logical.h as u32) } else { size };
        let phys = match self.frozen.get(&s.meta.id) {
            Some(f) => (f.image.width, f.image.height),
            None => s.meta.physical_size(),
        };
        s.viewport.set_destination(size.0 as i32, size.1 as i32);
        match &mut s.gl {
            None => {
                let mut surface = gl.create_surface(s.layer.wl_surface(), phys.0, phys.1)?;
                if let Some(frame) = self.frozen.get(&s.meta.id) {
                    let img = femtovg::imgref::Img::new(rgba_pixels(&frame.image.data), frame.image.width as usize, frame.image.height as usize);
                    s.texture = surface.canvas.create_image(img.as_ref(), ImageFlags::NEAREST).ok();
                }
                s.gl = Some(surface);
            }
            Some(gl) if (s.size, s.phys) != (size, phys) => {
                gl.make_current()?;
                gl.resize(phys.0, phys.1);
            }
            Some(_) => {}
        }
        s.size = size;
        s.phys = phys;
        s.dirty = true;
        self.draw(i, qh);
        Ok(())
    }

    pub fn frame(&mut self, surface: &WlSurface, qh: &QueueHandle<App>) {
        if let Some(i) = self.surf_index(surface) {
            self.surfs[i].frame_pending = false;
            if self.surfs[i].dirty {
                self.draw(i, qh);
            }
        }
    }

    fn redraw_all(&mut self, qh: &QueueHandle<App>) {
        for i in 0..self.surfs.len() {
            self.surfs[i].dirty = true;
            if !self.surfs[i].frame_pending {
                self.draw(i, qh);
            }
        }
    }

    // ---- input ---------------------------------------------------------------------------

    pub fn cursor(&self) -> Shape {
        match (self.mode, self.drag) {
            (Mode::Picker, _) if self.hover_button.is_some() => Shape::Pointer,
            (Mode::Picker, _) => Shape::Default,
            (_, Drag::Move { .. }) => Shape::Grabbing,
            (_, Drag::Resize { edges, .. }) => edges.cursor(),
            (_, Drag::Select { .. }) => Shape::Crosshair,
            (_, Drag::None) => match self.handle_at() {
                Some(Drag::Resize { edges, .. }) => edges.cursor(),
                Some(Drag::Move { .. }) => Shape::Move,
                _ => Shape::Crosshair,
            },
        }
    }

    /// What pressing at the current pointer position would grab, while awaiting confirmation.
    fn handle_at(&self) -> Option<Drag> {
        let (sel, p) = (self.selection?, self.pointer?);
        if !self.awaiting {
            return None;
        }
        let m = HANDLE_MARGIN;
        let within_x = p.0 >= sel.x - m && p.0 <= sel.right() + m;
        let within_y = p.1 >= sel.y - m && p.1 <= sel.bottom() + m;
        let (dl, dr, dt, db) = ((p.0 - sel.x).abs(), (p.0 - sel.right()).abs(), (p.1 - sel.y).abs(), (p.1 - sel.bottom()).abs());
        let edges = Edges {
            left: within_y && dl <= m && dl <= dr,
            right: within_y && dr <= m && dr < dl,
            top: within_x && dt <= m && dt <= db,
            bottom: within_x && db <= m && db < dt,
        };
        if edges.any() {
            Some(Drag::Resize { edges, orig: sel })
        } else if sel.contains(p) {
            Some(Drag::Move { grab: (p.0 - sel.x, p.1 - sel.y) })
        } else {
            None
        }
    }

    fn clamp_point(&self, p: (f64, f64)) -> (f64, f64) {
        (p.0.clamp(self.desktop.x, self.desktop.right()), p.1.clamp(self.desktop.y, self.desktop.bottom()))
    }

    pub fn pointer_motion(&mut self, surface: &WlSurface, pos: (f64, f64), qh: &QueueHandle<App>) {
        if self.hidden {
            return;
        }
        let Some(i) = self.surf_index(surface) else { return };
        let o = self.surfs[i].meta.logical;
        let p = (o.x + pos.0, o.y + pos.1);
        self.pointer = Some(p);
        match self.drag {
            Drag::Select { anchor } => self.selection = Some(Rect::from_points(anchor, self.clamp_point(p))),
            Drag::Move { grab } => {
                if let Some(sel) = self.selection {
                    let d = self.desktop;
                    let x = (p.0 - grab.0).clamp(d.x, (d.right() - sel.w).max(d.x));
                    let y = (p.1 - grab.1).clamp(d.y, (d.bottom() - sel.h).max(d.y));
                    self.selection = Some(Rect { x, y, ..sel });
                }
            }
            Drag::Resize { edges, orig } => {
                let p = self.clamp_point(p);
                let mut a = (orig.x, orig.y);
                let mut b = (orig.right(), orig.bottom());
                if edges.left {
                    a.0 = p.0;
                }
                if edges.right {
                    b.0 = p.0;
                }
                if edges.top {
                    a.1 = p.1;
                }
                if edges.bottom {
                    b.1 = p.1;
                }
                self.selection = Some(Rect::from_points(a, b));
            }
            Drag::None => {}
        }
        if self.mode == Mode::Picker {
            let local = (pos.0, pos.1);
            self.hover_button = self.surfs[i].buttons.iter().find(|(r, _)| r.contains(local)).map(|(_, id)| *id);
        }
        self.redraw_all(qh);
    }

    pub fn pointer_button(&mut self, button: u32, pressed: bool, qh: &QueueHandle<App>) -> Option<Outcome> {
        if button != BTN_LEFT || self.hidden {
            return None;
        }
        let p = self.pointer?;
        let outcome = match (self.mode, pressed) {
            (Mode::Picker, true) => {
                let id = self.hover_button.or_else(|| self.surfs.iter().find(|s| s.meta.logical.contains(p)).map(|s| s.meta.id))?;
                return Some(Outcome::Picked(id));
            }
            (Mode::Picker, false) => None,
            (Mode::Region, true) => {
                self.drag = self.handle_at().unwrap_or(Drag::Select { anchor: self.clamp_point(p) });
                if let Drag::Select { anchor } = self.drag {
                    self.selection = Some(Rect { x: anchor.0, y: anchor.1, w: 0.0, h: 0.0 });
                    self.awaiting = false;
                }
                None
            }
            (Mode::Region, false) => {
                let drag = std::mem::replace(&mut self.drag, Drag::None);
                let sel = self.selection.map(|r| r.rounded()).filter(|r| r.w >= MIN_SELECTION && r.h >= MIN_SELECTION);
                self.selection = sel;
                match (drag, sel) {
                    (Drag::None, _) | (_, None) => None,
                    (Drag::Select { .. }, Some(sel)) if !self.confirm => Some(Outcome::Region(sel, Action::Default)),
                    _ => {
                        self.awaiting = true;
                        None
                    }
                }
            }
        };
        self.redraw_all(qh);
        outcome
    }

    pub fn key(&mut self, keysym: Keysym, mods: Modifiers) -> Option<Outcome> {
        if self.hidden {
            return None;
        }
        if keysym == Keysym::Escape {
            return Some(Outcome::Cancel);
        }
        let sel = self.selection.filter(|_| self.mode == Mode::Region && self.awaiting && self.drag == Drag::None)?;
        match keysym {
            Keysym::Return | Keysym::KP_Enter | Keysym::space if !mods.ctrl => Some(Outcome::Region(sel, Action::Default)),
            Keysym::c | Keysym::C if mods.ctrl => Some(Outcome::Region(sel, Action::Copy)),
            Keysym::s | Keysym::S if mods.ctrl => Some(Outcome::Region(sel, Action::Save)),
            _ => None,
        }
    }

    /// Image size the current selection would produce, for the size badge.
    fn selection_pixels(&self, sel: Rect) -> (u32, u32) {
        let scale = self
            .surfs
            .iter()
            .filter(|s| s.meta.logical.intersect(&sel).is_some())
            .map(|s| f64::from(s.phys.0) / s.meta.logical.w)
            .fold(1.0, f64::max);
        ((sel.w * scale).round() as u32, (sel.h * scale).round() as u32)
    }

    // ---- drawing -------------------------------------------------------------------------

    fn draw(&mut self, i: usize, qh: &QueueHandle<App>) {
        if let Err(e) = self.try_draw(i, qh) {
            log::error!("drawing overlay on {}: {e:#}", self.surfs[i].meta.name);
        }
    }

    fn try_draw(&mut self, i: usize, qh: &QueueHandle<App>) -> anyhow::Result<()> {
        let badge = match (self.drag, self.selection) {
            (Drag::None, _) | (_, None) => None,
            (_, Some(sel)) => Some(self.selection_pixels(sel)),
        };
        let picker_outputs: Vec<(u32, String)> = self.surfs.iter().map(|s| (s.meta.id, picker_label(&s.meta))).collect();
        let hovered_output = self.pointer.and_then(|p| self.surfs.iter().find(|s| s.meta.logical.contains(p)).map(|s| s.meta.id));

        let s = &mut self.surfs[i];
        let Some(gl) = s.gl.as_mut() else { return Ok(()) };
        gl.make_current()?;
        let (w, h) = (s.size.0 as f32, s.size.1 as f32);
        let font = gl.font;
        let canvas = &mut gl.canvas;
        canvas.reset();
        canvas.clear_rect(0, 0, s.phys.0, s.phys.1, femtovg::Color::rgbaf(0.0, 0.0, 0.0, 0.0));
        if self.hidden {
            return present(s, qh);
        }
        canvas.scale(s.phys.0 as f32 / w, s.phys.1 as f32 / h);

        if let Some(tex) = s.texture {
            fill_rect(canvas, 0.0, 0.0, w, h, Paint::image(tex, 0.0, 0.0, w, h, 0.0, 1.0));
        }
        let origin = s.meta.logical;
        let local = |r: Rect| Rect { x: r.x - origin.x, y: r.y - origin.y, ..r };
        let t = &self.theme;
        let dim = Paint::color(femtovg::Color::rgbaf(0.0, 0.0, 0.0, t.dim_opacity));

        match self.mode {
            Mode::Region => {
                match self.selection.map(local) {
                    Some(sel) => {
                        let (x0, y0, x1, y1) = (sel.x as f32, sel.y as f32, sel.right() as f32, sel.bottom() as f32);
                        // Four strips so the selection itself is never tinted.
                        fill_rect(canvas, 0.0, 0.0, w, y0.clamp(0.0, h), dim.clone());
                        fill_rect(canvas, 0.0, y1.clamp(0.0, h), w, h - y1.clamp(0.0, h), dim.clone());
                        let (sy0, sy1) = (y0.clamp(0.0, h), y1.clamp(0.0, h));
                        fill_rect(canvas, 0.0, sy0, x0.clamp(0.0, w), sy1 - sy0, dim.clone());
                        fill_rect(canvas, x1.clamp(0.0, w), sy0, w - x1.clamp(0.0, w), sy1 - sy0, dim);
                        if sel.w > 0.0 && sel.h > 0.0 {
                            let mut path = Path::new();
                            let inset = BORDER_WIDTH / 2.0;
                            path.rect(x0 + inset, y0 + inset, (x1 - x0 - BORDER_WIDTH).max(0.0), (y1 - y0 - BORDER_WIDTH).max(0.0));
                            canvas.stroke_path(&path, &Paint::color(t.primary.fv()).with_line_width(BORDER_WIDTH));
                        }
                        if self.awaiting && self.drag == Drag::None {
                            let (mx, my) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
                            for (hx, hy) in [(x0, y0), (mx, y0), (x1, y0), (x1, my), (x1, y1), (mx, y1), (x0, y1), (x0, my)] {
                                let mut path = Path::new();
                                path.circle(hx, hy, HANDLE_RADIUS);
                                canvas.fill_path(&path, &Paint::color(t.primary.fv()));
                                canvas.stroke_path(&path, &Paint::color(t.surface.fv()).with_line_width(1.5));
                            }
                        }
                    }
                    None => fill_rect(canvas, 0.0, 0.0, w, h, dim),
                }
                if let (Some(font), Some((pw, ph)), Some(p)) = (font, badge, self.pointer) {
                    let (px, py) = ((p.0 - origin.x) as f32, (p.1 - origin.y) as f32);
                    if px >= 0.0 && py >= 0.0 && px < w && py < h {
                        draw_badge(canvas, font, t, &format!("{pw} × {ph}"), (px, py), (w, h));
                    }
                }
                if self.awaiting && self.drag == Drag::None
                    && let Some(font) = font {
                        let hint = "Ctrl+C to copy · Ctrl+S to save · Enter or Space for defaults · Esc to cancel · Drag to reselect";
                        draw_hint(canvas, font, t, hint, w);
                    }
            }
            Mode::Picker => {
                let lit = hovered_output == Some(s.meta.id);
                fill_rect(canvas, 0.0, 0.0, w, h, Paint::color(femtovg::Color::rgbaf(0.0, 0.0, 0.0, if lit { t.dim_opacity * 0.4 } else { t.dim_opacity })));
                if lit {
                    let mut path = Path::new();
                    path.rect(BORDER_WIDTH / 2.0, BORDER_WIDTH / 2.0, w - BORDER_WIDTH, h - BORDER_WIDTH);
                    canvas.stroke_path(&path, &Paint::color(t.primary.fv()).with_line_width(BORDER_WIDTH));
                }
                s.buttons = match font {
                    Some(font) => draw_picker(canvas, font, t, &picker_outputs, self.hover_button, w),
                    None => Vec::new(),
                };
            }
        }

        present(s, qh)
    }
}

fn present(s: &mut Surf, qh: &QueueHandle<App>) -> anyhow::Result<()> {
    s.layer.wl_surface().frame(qh, FrameCallbackData(s.layer.wl_surface().clone()));
    s.gl.as_mut().expect("drawn surfaces have GL").swap()?;
    s.frame_pending = true;
    s.dirty = false;
    Ok(())
}

impl Drop for Overlay {
    fn drop(&mut self) {
        for s in &mut self.surfs {
            // GL objects must be freed with their own context current.
            if let Some(gl) = s.gl.take()
                && gl.make_current().is_ok() {
                    drop(gl);
                }
            s.viewport.destroy();
        }
    }
}

fn rgba_pixels(data: &[u8]) -> Vec<femtovg::rgb::RGBA8> {
    data.as_chunks::<4>().0.iter().map(|p| femtovg::rgb::RGBA8::new(p[0], p[1], p[2], p[3])).collect()
}

fn picker_label(meta: &OutputMeta) -> String {
    if !meta.name.is_empty() {
        meta.name.clone()
    } else if !meta.description.is_empty() {
        meta.description.clone()
    } else {
        "Display".into()
    }
}

fn fill_rect(canvas: &mut Canvas<OpenGl>, x: f32, y: f32, w: f32, h: f32, paint: Paint) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let mut path = Path::new();
    path.rect(x, y, w, h);
    canvas.fill_path(&path, &paint);
}

fn text_paint(font: femtovg::FontId, size: f32, color: Color) -> Paint {
    let mut paint = Paint::color(color.fv()).with_font(&[font]).with_font_size(size);
    paint.set_text_baseline(Baseline::Middle);
    paint
}

fn text_width(canvas: &Canvas<OpenGl>, text: &str, paint: &Paint) -> f32 {
    canvas.measure_text(0.0, 0.0, text, paint).map(|m| m.width()).unwrap_or(0.0)
}

fn card(canvas: &mut Canvas<OpenGl>, r: (f32, f32, f32, f32), radius: f32, fill: Color, border: Color) {
    let mut path = Path::new();
    path.rounded_rect(r.0, r.1, r.2, r.3, radius);
    canvas.fill_path(&path, &Paint::color(fill.fv()));
    let mut path = Path::new();
    path.rounded_rect(r.0 + 0.5, r.1 + 0.5, r.2 - 1.0, r.3 - 1.0, radius);
    canvas.stroke_path(&path, &Paint::color(border.fv()).with_line_width(1.0));
}

/// Size badge that follows the pointer, flipped to stay on screen.
fn draw_badge(canvas: &mut Canvas<OpenGl>, font: femtovg::FontId, t: &Theme, text: &str, p: (f32, f32), bounds: (f32, f32)) {
    let paint = text_paint(font, BADGE_FONT, t.primary);
    let bw = text_width(canvas, text, &paint) + BADGE_PAD.0 * 2.0;
    let bh = BADGE_FONT * 1.25 + BADGE_PAD.1 * 2.0;
    let mut x = p.0 + BADGE_OFFSET.0;
    let mut y = p.1 + BADGE_OFFSET.1;
    if x + bw > bounds.0 {
        x = p.0 - BADGE_OFFSET.0 - bw;
    }
    if y + bh > bounds.1 {
        y = p.1 - BADGE_OFFSET.1 - bh;
    }
    let (x, y) = (x.round(), y.round());
    card(canvas, (x, y, bw, bh), RADIUS_SM, t.surface.with_alpha(0.94), t.primary);
    let _ = canvas.fill_text(x + BADGE_PAD.0, y + bh / 2.0, text, &paint);
}

fn draw_hint(canvas: &mut Canvas<OpenGl>, font: femtovg::FontId, t: &Theme, text: &str, w: f32) {
    let paint = text_paint(font, CAPTION_FONT, t.on_surface);
    let tw = text_width(canvas, text, &paint);
    let (cw, ch) = (tw + SPACE_MD * 2.0, CAPTION_FONT * 1.3 + SPACE_SM * 2.0);
    let x = ((w - cw) / 2.0).round();
    card(canvas, (x, SPACE_MD, cw, ch), RADIUS_MD, t.surface.with_alpha(0.94), t.outline);
    let _ = canvas.fill_text(x + SPACE_MD, SPACE_MD + ch / 2.0, text, &paint);
}

/// "Choose display" bar with one button per output. Returns the buttons' hit boxes.
fn draw_picker(canvas: &mut Canvas<OpenGl>, font: femtovg::FontId, t: &Theme, outputs: &[(u32, String)], hover: Option<u32>, w: f32) -> Vec<(Rect, u32)> {
    let label = "Choose display";
    let label_paint = text_paint(font, CAPTION_FONT, t.on_surface);
    let label_w = text_width(canvas, label, &label_paint);
    let btn_h = CAPTION_FONT * 1.3 + SPACE_SM * 2.0;
    let widths: Vec<f32> = outputs.iter().map(|(_, n)| text_width(canvas, n, &label_paint) + SPACE_MD * 2.0).collect();
    let card_w = SPACE_MD * 2.0 + label_w + widths.iter().map(|bw| bw + SPACE_SM).sum::<f32>();
    let card_h = btn_h + SPACE_SM * 2.0;
    let x0 = ((w - card_w) / 2.0).round();
    let y0 = SPACE_MD;
    card(canvas, (x0, y0, card_w, card_h), RADIUS_MD, t.surface.with_alpha(0.94), t.outline);
    let _ = canvas.fill_text(x0 + SPACE_MD, y0 + card_h / 2.0, label, &label_paint);

    let mut buttons = Vec::new();
    let mut x = x0 + SPACE_MD + label_w + SPACE_SM;
    for ((id, name), bw) in outputs.iter().zip(widths) {
        let hovered = hover == Some(*id);
        let (fill, border, text) = if hovered { (t.primary, t.primary, t.on_primary) } else { (t.surface_variant, t.outline, t.on_surface) };
        let r = (x, y0 + SPACE_SM, bw, btn_h);
        card(canvas, r, RADIUS_MD, fill, border);
        let _ = canvas.fill_text(x + SPACE_MD, r.1 + btn_h / 2.0, name, &text_paint(font, CAPTION_FONT, text));
        buttons.push((Rect { x: f64::from(r.0), y: f64::from(r.1), w: f64::from(r.2), h: f64::from(r.3) }, *id));
        x += bw + SPACE_SM;
    }
    buttons
}
