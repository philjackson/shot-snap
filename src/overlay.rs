//! Full-screen layer surfaces (one per output) for region selection, the display picker and
//! annotation.
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

use crate::annotate::{self, Shape as Annotation};
use crate::app::App;
use crate::capture::{NoEvents, OutputMeta};
use crate::config::{Color, Theme};
use crate::deliver::Action;
use crate::geom::Rect;
use crate::gl::{Gl, GlSurface};
use crate::image::{Image, OutputFrame};

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
const RADIUS_XL: f32 = 12.0;
const CONTROL_SM: f32 = 32.0;
const ICON: f32 = 16.0;
const TOOLBAR_FILL_ALPHA: f32 = 0.9;
const SPACE_LG: f32 = 16.0;
const SURROUND_ALPHA: f32 = 0.85;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Region,
    Picker,
    /// Editing one captured image, shown on a single output under the toolbar.
    Annotate,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Region(Rect, Action),
    Picked(u32),
    Annotated(Vec<Annotation>, Action),
    Cancel,
}

/// Something clickable drawn on a surface.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Hit {
    Output(u32),
    RectangleTool,
    Copy,
    Save,
    Done,
    Close,
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
    Draw { anchor: (f64, f64) },
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
    /// Hit boxes in surface-local coordinates, from the last draw.
    buttons: Vec<(Rect, Hit)>,
    toolbar: Option<Rect>,
}

/// The image being annotated. Annotations are kept in image pixels.
struct Editor {
    image: Image,
    /// Image pixels per desktop logical pixel, so strokes keep their logical weight.
    scale: f64,
    /// Where the image was last drawn (surface-local logical) and image pixels per logical pixel there.
    canvas: Rect,
    fit: f64,
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
    hover_button: Option<Hit>,
    /// The pointer is over a toolbar card, so presses don't draw.
    over_toolbar: bool,
    annotations: Vec<Annotation>,
    editor: Option<Editor>,
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
                Surf { meta, layer, viewport, gl: None, texture: None, size: (0, 0), phys: (0, 0), frame_pending: false, dirty: true, buttons: Vec::new(), toolbar: None }
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
            over_toolbar: false,
            annotations: Vec::new(),
            editor: None,
            theme,
            hidden: false,
        }
    }

    /// The annotation editor for an already-captured image, on `output` only.
    pub fn new_editor(init: Init, output: OutputMeta, image: Image, scale: f64, theme: Theme) -> Self {
        let mut overlay = Self::new(init, Mode::Annotate, vec![output], Vec::new(), theme, false, None);
        overlay.editor = Some(Editor { image, scale, canvas: Rect::default(), fit: 1.0 });
        overlay
    }

    pub fn take_image(&mut self) -> Option<Image> {
        self.editor.take().map(|e| e.image)
    }

    /// Global pointer position to image pixels (unclamped).
    fn to_image(&self, p: (f64, f64)) -> Option<(f64, f64)> {
        let (e, s) = (self.editor.as_ref()?, self.surfs.first()?);
        Some(((p.0 - s.meta.logical.x - e.canvas.x) * e.fit, (p.1 - s.meta.logical.y - e.canvas.y) * e.fit))
    }

    fn clamp_image(&self, p: (f64, f64)) -> (f64, f64) {
        match &self.editor {
            Some(e) => (p.0.clamp(0.0, f64::from(e.image.width)), p.1.clamp(0.0, f64::from(e.image.height))),
            None => p,
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
                // Frozen frames show 1:1; the editor image may be scaled down to fit.
                let source = match (&self.editor, self.frozen.get(&s.meta.id)) {
                    (Some(e), _) => Some((&e.image, ImageFlags::GENERATE_MIPMAPS)),
                    (None, Some(frame)) => Some((&frame.image, ImageFlags::NEAREST)),
                    (None, None) => None,
                };
                if let Some((image, flags)) = source {
                    let img = femtovg::imgref::Img::new(rgba_pixels(&image.data), image.width as usize, image.height as usize);
                    s.texture = surface.canvas.create_image(img.as_ref(), flags).ok();
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
            (Mode::Annotate, Drag::None) if self.hover_button.is_some() => Shape::Pointer,
            (Mode::Annotate, Drag::None) if self.over_toolbar => Shape::Default,
            (_, Drag::Draw { .. }) => Shape::Crosshair,
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
            Drag::None | Drag::Draw { .. } => {}
        }
        if self.drag == Drag::None {
            let s = &self.surfs[i];
            self.hover_button = s.buttons.iter().find(|(r, _)| r.contains(pos)).map(|(_, hit)| *hit);
            self.over_toolbar = s.toolbar.is_some_and(|r| r.contains(pos));
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
                let id = match self.hover_button {
                    Some(Hit::Output(id)) => id,
                    _ => self.surfs.iter().find(|s| s.meta.logical.contains(p)).map(|s| s.meta.id)?,
                };
                return Some(Outcome::Picked(id));
            }
            (Mode::Picker, false) => None,
            (Mode::Annotate, true) => match self.hover_button {
                Some(Hit::Copy) => return Some(self.annotated(Action::Copy)),
                Some(Hit::Save) => return Some(self.annotated(Action::Save)),
                Some(Hit::Done) => return Some(self.annotated(Action::Default)),
                Some(Hit::Close) => return Some(Outcome::Cancel),
                _ if self.over_toolbar => None,
                _ => {
                    // Drawing starts on the image; the drag may then leave it and is clamped.
                    if let Some(ip) = self.to_image(p).filter(|&ip| self.clamp_image(ip) == ip) {
                        self.drag = Drag::Draw { anchor: ip };
                    }
                    None
                }
            },
            (Mode::Annotate, false) => {
                if let (Drag::Draw { anchor }, Some(ip)) = (std::mem::replace(&mut self.drag, Drag::None), self.to_image(p)) {
                    let rect = Rect::from_points(anchor, self.clamp_image(ip));
                    let scale = self.editor.as_ref().map_or(1.0, |e| e.scale);
                    // Ignore clicks: the rectangle must be at least a couple of logical pixels.
                    if rect.w >= MIN_SELECTION * scale && rect.h >= MIN_SELECTION * scale {
                        self.annotations.push(Annotation { rect, width: annotate::DEFAULT_WIDTH * scale, color: annotate::DEFAULT_COLOR });
                    }
                }
                None
            }
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

    pub fn key(&mut self, keysym: Keysym, mods: Modifiers, qh: &QueueHandle<App>) -> Option<Outcome> {
        if self.hidden {
            return None;
        }
        if keysym == Keysym::Escape {
            return Some(Outcome::Cancel);
        }
        if self.mode == Mode::Annotate {
            return match keysym {
                _ if self.drag != Drag::None => None,
                Keysym::Return | Keysym::KP_Enter if !mods.ctrl => Some(self.annotated(Action::Default)),
                Keysym::c | Keysym::C if mods.ctrl => Some(self.annotated(Action::Copy)),
                Keysym::s | Keysym::S if mods.ctrl => Some(self.annotated(Action::Save)),
                Keysym::z | Keysym::Z if mods.ctrl => {
                    self.annotations.pop();
                    self.redraw_all(qh);
                    None
                }
                _ => None,
            };
        }
        let sel = self.selection.filter(|_| self.mode == Mode::Region && self.awaiting && self.drag == Drag::None)?;
        match keysym {
            Keysym::Return | Keysym::KP_Enter | Keysym::space if !mods.ctrl => Some(Outcome::Region(sel, Action::Default)),
            Keysym::c | Keysym::C if mods.ctrl => Some(Outcome::Region(sel, Action::Copy)),
            Keysym::s | Keysym::S if mods.ctrl => Some(Outcome::Region(sel, Action::Save)),
            _ => None,
        }
    }

    /// Noctalia's image placement: centred below the toolbar at 1:1 device pixels, scaled down
    /// only when it doesn't fit.
    fn layout_editor(&mut self, i: usize) {
        let s = &self.surfs[i];
        let Some(e) = self.editor.as_mut() else { return };
        let (w, h) = (f64::from(s.size.0), f64::from(s.size.1));
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let device = f64::from(s.phys.0) / w;
        let top = f64::from(SPACE_MD + CONTROL_SM + SPACE_SM * 2.0 + SPACE_LG);
        let (avail_w, avail_h) = ((w - 2.0 * f64::from(SPACE_LG)).max(1.0), (h - top - f64::from(SPACE_LG)).max(1.0));
        let (iw, ih) = (f64::from(e.image.width), f64::from(e.image.height));
        let fit = device.max(iw / avail_w).max(ih / avail_h).max(0.1);
        let (dw, dh) = (iw / fit, ih / fit);
        // Snap the origin to device pixels so a 1:1 image isn't resampled.
        let snap = |v: f64| (v * device).round() / device;
        e.canvas = Rect { x: snap((w - dw) / 2.0), y: snap(top + (avail_h - dh) / 2.0), w: dw, h: dh };
        e.fit = fit;
    }

    fn annotated(&self, action: Action) -> Outcome {
        Outcome::Annotated(self.annotations.clone(), action)
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
        let drawing = match (self.drag, self.pointer.and_then(|p| self.to_image(p))) {
            (Drag::Draw { anchor }, Some(ip)) => Some(Rect::from_points(anchor, self.clamp_image(ip))),
            _ => None,
        };
        if self.mode == Mode::Annotate {
            self.layout_editor(i);
        }
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
                (s.buttons, s.toolbar) = match font {
                    Some(font) => draw_picker(canvas, font, t, &picker_outputs, self.hover_button, w),
                    None => (Vec::new(), None),
                };
            }
            Mode::Annotate => {
                fill_rect(canvas, 0.0, 0.0, w, h, Paint::color(t.surface.with_alpha(SURROUND_ALPHA).fv()));
                if let Some(e) = &self.editor {
                    let c = e.canvas;
                    let (cx, cy, cw, ch) = (c.x as f32, c.y as f32, c.w as f32, c.h as f32);
                    if let Some(tex) = s.texture {
                        fill_rect(canvas, cx, cy, cw, ch, Paint::image(tex, cx, cy, cw, ch, 0.0, 1.0));
                    }
                    // Strokes are clipped to the image, as they are when burned in.
                    canvas.save();
                    canvas.scissor(cx, cy, cw, ch);
                    let preview = drawing.map(|rect| Annotation { rect, width: annotate::DEFAULT_WIDTH * e.scale, color: annotate::DEFAULT_COLOR });
                    for shape in self.annotations.iter().copied().chain(preview) {
                        let r = shape.rect;
                        let fit = e.fit as f32;
                        let mut path = Path::new();
                        path.rect(cx + r.x as f32 / fit, cy + r.y as f32 / fit, r.w as f32 / fit, r.h as f32 / fit);
                        canvas.stroke_path(&path, &Paint::color(shape.color.fv()).with_line_width(shape.width as f32 / fit));
                    }
                    canvas.restore();
                    // Inset outline separating the image from the surround; never exported.
                    let mut path = Path::new();
                    path.rect(cx + 0.5, cy + 0.5, cw - 1.0, ch - 1.0);
                    canvas.stroke_path(&path, &Paint::color(t.outline.fv()).with_line_width(1.0));
                }
                (s.buttons, s.toolbar) = draw_annotate_toolbar(canvas, t, self.hover_button, w);
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
fn draw_picker(canvas: &mut Canvas<OpenGl>, font: femtovg::FontId, t: &Theme, outputs: &[(u32, String)], hover: Option<Hit>, w: f32) -> (Vec<(Rect, Hit)>, Option<Rect>) {
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
        let hovered = hover == Some(Hit::Output(*id));
        let (fill, border, text) = if hovered { (t.primary, t.primary, t.on_primary) } else { (t.surface_variant, t.outline, t.on_surface) };
        let r = (x, y0 + SPACE_SM, bw, btn_h);
        card(canvas, r, RADIUS_MD, fill, border);
        let _ = canvas.fill_text(x + SPACE_MD, r.1 + btn_h / 2.0, name, &text_paint(font, CAPTION_FONT, text));
        buttons.push((to_rect(r), Hit::Output(*id)));
        x += bw + SPACE_SM;
    }
    (buttons, Some(to_rect((x0, y0, card_w, card_h))))
}

fn to_rect(r: (f32, f32, f32, f32)) -> Rect {
    Rect { x: f64::from(r.0), y: f64::from(r.1), w: f64::from(r.2), h: f64::from(r.3) }
}

enum Item {
    Button(Hit),
    Separator,
}

/// Noctalia's annotation toolbar: a rounded card centred at the top of each output.
fn draw_annotate_toolbar(canvas: &mut Canvas<OpenGl>, t: &Theme, hover: Option<Hit>, w: f32) -> (Vec<(Rect, Hit)>, Option<Rect>) {
    use Item::*;
    let items = [Button(Hit::RectangleTool), Separator, Button(Hit::Copy), Button(Hit::Save), Button(Hit::Done), Button(Hit::Close)];
    let item_w = |item: &Item| match item {
        Button(_) => CONTROL_SM,
        Separator => 1.0,
    };
    let content_w: f32 = items.iter().map(item_w).sum::<f32>() + SPACE_SM * (items.len() - 1) as f32;
    let (card_w, card_h) = (content_w + SPACE_MD * 2.0, CONTROL_SM + SPACE_SM * 2.0);
    let x0 = ((w - card_w) / 2.0).round();
    let y0 = SPACE_MD;
    card(canvas, (x0, y0, card_w, card_h), RADIUS_XL, t.surface.with_alpha(TOOLBAR_FILL_ALPHA), t.outline);

    let mut buttons = Vec::new();
    let mut x = x0 + SPACE_MD;
    let y = y0 + SPACE_SM;
    for item in &items {
        match *item {
            Separator => {
                let h = CONTROL_SM * 0.6;
                fill_rect(canvas, x, y + (CONTROL_SM - h) / 2.0, 1.0, h, Paint::color(t.outline.fv()));
            }
            Button(hit) => {
                // The only tool is always selected; Done is the primary action.
                let (fill, fg) = match (hit, hover == Some(hit)) {
                    (Hit::RectangleTool, _) => (Some(t.primary), t.on_primary),
                    (_, true) => (Some(t.hover), t.on_hover),
                    (Hit::Done, false) => (Some(t.primary), t.on_primary),
                    _ => (None, t.on_surface),
                };
                if let Some(fill) = fill {
                    let mut path = Path::new();
                    path.rounded_rect(x, y, CONTROL_SM, CONTROL_SM, RADIUS_MD);
                    canvas.fill_path(&path, &Paint::color(fill.fv()));
                }
                let pad = (CONTROL_SM - ICON) / 2.0;
                draw_icon(canvas, hit, x + pad, y + pad, fg);
                buttons.push((to_rect((x, y, CONTROL_SM, CONTROL_SM)), hit));
            }
        }
        x += item_w(item) + SPACE_SM;
    }
    (buttons, Some(to_rect((x0, y0, card_w, card_h))))
}

/// Line icons in a 16×16 box, after the Tabler glyphs Noctalia uses.
fn draw_icon(canvas: &mut Canvas<OpenGl>, hit: Hit, x: f32, y: f32, color: Color) {
    let mut path = Path::new();
    match hit {
        Hit::RectangleTool => path.rounded_rect(x + 2.0, y + 2.0, 12.0, 12.0, 2.0),
        Hit::Copy => {
            path.rounded_rect(x + 5.5, y + 5.5, 9.0, 9.0, 1.5);
            path.move_to(x + 10.5, y + 5.5);
            path.line_to(x + 10.5, y + 2.5);
            path.line_to(x + 1.5, y + 2.5);
            path.line_to(x + 1.5, y + 11.5);
            path.line_to(x + 5.5, y + 11.5);
        }
        Hit::Save => {
            path.move_to(x + 2.0, y + 2.0);
            path.line_to(x + 11.5, y + 2.0);
            path.line_to(x + 14.0, y + 4.5);
            path.line_to(x + 14.0, y + 14.0);
            path.line_to(x + 2.0, y + 14.0);
            path.close();
            path.rect(x + 5.0, y + 2.0, 5.0, 3.5);
            path.circle(x + 8.0, y + 10.0, 1.75);
        }
        Hit::Done => {
            path.move_to(x + 3.0, y + 8.5);
            path.line_to(x + 6.5, y + 12.0);
            path.line_to(x + 13.0, y + 4.5);
        }
        Hit::Close => {
            path.move_to(x + 4.0, y + 4.0);
            path.line_to(x + 12.0, y + 12.0);
            path.move_to(x + 12.0, y + 4.0);
            path.line_to(x + 4.0, y + 12.0);
        }
        Hit::Output(_) => return,
    }
    let paint = Paint::color(color.fv())
        .with_line_width(1.75)
        .with_line_cap(femtovg::LineCap::Round)
        .with_line_join(femtovg::LineJoin::Round);
    canvas.stroke_path(&path, &paint);
}
