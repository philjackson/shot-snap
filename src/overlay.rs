//! Full-screen layer surfaces (one per output) for region selection, the display picker and
//! annotation.
//! Everything is laid out in logical pixels; each canvas is scaled to the output's physical
//! resolution so frozen frames show 1:1 and lines stay crisp under fractional scaling.

use std::collections::HashMap;
use std::time::Duration;

use femtovg::{Canvas, ImageFlags, ImageId, Paint, Path};
use femtovg::renderer::OpenGl;
use smithay_client_toolkit::compositor::{CompositorState, FrameCallbackData};
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym, Modifiers};
use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface};
use smithay_client_toolkit::shell::WaylandSurface;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::QueueHandle;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::annotate::{ArrowHeads, Kind, Shape as Annotation, Tool, HIGHLIGHT_COLORS, SIZE_PRESETS};
use crate::app::App;
use crate::capture::{NoEvents, OutputMeta};
use crate::config::{Color, Theme};
use crate::deliver::Action;
use crate::geom::Rect;
use crate::gl::{Gl, GlSurface};
use crate::image::{Image, OutputFrame};
use crate::paint::Fill;
use crate::picker::Picker;
use crate::render::{self, Map};
use crate::state::AnnotateState;
use crate::ui::{card, fill_rect, text_paint, text_width};

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

#[derive(Debug, Clone)]
pub enum Outcome {
    Region(Rect, Action),
    Picked(u32),
    /// The editor's image with the annotations rendered in.
    Annotated(Image, Action),
    Failed(String),
    Cancel,
}

/// Delayed work the overlay asks the app to schedule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Timer {
    Tooltip(u64),
    LongPress(u64),
}

const TOOLTIP_DELAY: Duration = Duration::from_millis(500);
const LONG_PRESS: Duration = Duration::from_millis(400);

/// Something clickable drawn on a surface.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Hit {
    Output(u32),
    Undo,
    Tool(Tool),
    ArrowHeads(ArrowHeads),
    /// Index into `HIGHLIGHT_COLORS`.
    HighlightColor(usize),
    Color,
    SizePreset(usize),
    SizeAdvanced,
    SizeDown,
    SizeUp,
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
    /// The arrow-ends menu, when open.
    menu: Option<Rect>,
    blurs: render::BlurCache,
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
    picker: Option<Picker>,
    tool: Tool,
    /// Toolbar button whose tooltip is showing, and a counter that invalidates pending tooltip
    /// timers whenever the hovered button changes.
    tooltip: Option<Hit>,
    hover_gen: u64,
    timers: Vec<(Duration, Timer)>,
    /// The arrow button is held: the generation its long-press timer belongs to.
    long_press: Option<(Tool, u64)>,
    /// The tool whose long-press menu is open.
    menu: Option<Tool>,
    /// Index of the text annotation being typed.
    editing: Option<usize>,
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
                Surf { meta, layer, viewport, gl: None, texture: None, size: (0, 0), phys: (0, 0), frame_pending: false, dirty: true, buttons: Vec::new(), toolbar: None, menu: None, blurs: Default::default() }
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
            picker: None,
            tool: Tool::Rectangle,
            tooltip: None,
            hover_gen: 0,
            timers: Vec::new(),
            long_press: None,
            menu: None,
            editing: None,
            theme,
            hidden: false,
        }
    }

    /// The annotation editor for an already-captured image, on `output` only.
    pub fn new_editor(init: Init, output: OutputMeta, image: Image, scale: f64, theme: Theme, annotate: AnnotateState) -> Self {
        let mut overlay = Self::new(init, Mode::Annotate, vec![output], Vec::new(), theme, false, None);
        overlay.editor = Some(Editor { image, scale, canvas: Rect::default(), fit: 1.0 });
        overlay.tool = annotate.tool;
        overlay.picker = Some(Picker::new(annotate));
        overlay
    }

    /// The colour panel's state to persist, with the current pick recorded.
    pub fn take_annotate_state(&mut self) -> Option<AnnotateState> {
        self.picker.take().map(Picker::finish)
    }

    /// Timers to schedule, as (delay, what to report back through `timer`).
    pub fn take_timers(&mut self) -> Vec<(Duration, Timer)> {
        std::mem::take(&mut self.timers)
    }

    pub fn timer(&mut self, timer: Timer, qh: &QueueHandle<App>) {
        match timer {
            // Show the tooltip if the pointer is still on the same button.
            Timer::Tooltip(generation) => {
                if generation == self.hover_gen && self.drag == Drag::None && self.hover_button.is_some_and(|h| tooltip_text(h).is_some()) {
                    self.tooltip = self.hover_button;
                }
            }
            // Still holding the tool's button: open its menu.
            Timer::LongPress(generation) => {
                if let Some((tool, g)) = self.long_press
                    && g == generation
                {
                    self.menu = Some(tool);
                }
            }
        }
        self.redraw_all(qh);
    }

    fn arrow_heads(&self) -> ArrowHeads {
        self.picker.as_ref().map(|p| p.state().arrow_heads).unwrap_or_default()
    }

    fn set_tool(&mut self, tool: Tool) {
        self.commit_text();
        self.tool = tool;
        if let Some(p) = &mut self.picker {
            p.state_mut().tool = tool;
            // Its button is hidden for tools that don't use it.
            if !tool.uses_fill() && p.open {
                p.close();
            }
        }
    }

    fn set_arrow_heads(&mut self, heads: ArrowHeads) {
        if let Some(p) = &mut self.picker {
            p.state_mut().arrow_heads = heads;
        }
        self.set_tool(Tool::Arrow);
        self.menu = None;
    }

    fn highlight_color(&self) -> Color {
        self.picker.as_ref().map_or(HIGHLIGHT_COLORS[0].1, |p| p.state().highlight_color)
    }

    fn set_highlight_color(&mut self, i: usize) {
        if let Some(p) = &mut self.picker {
            p.state_mut().highlight_color = HIGHLIGHT_COLORS[i].1;
        }
        self.set_tool(Tool::Highlight);
        self.menu = None;
    }

    /// Picks a menu item. Returns false if `hit` isn't one.
    fn choose_menu_item(&mut self, hit: Option<Hit>) -> bool {
        match hit {
            Some(Hit::ArrowHeads(heads)) => self.set_arrow_heads(heads),
            Some(Hit::HighlightColor(i)) => self.set_highlight_color(i),
            _ => return false,
        }
        true
    }

    /// What the current tool paints with: the highlighter has its own colour.
    fn current_fill(&self) -> crate::paint::Fill {
        match self.tool {
            Tool::Highlight => crate::paint::Fill::Solid(self.highlight_color()),
            _ => self.picker.as_ref().map(|p| p.fill().clone()).unwrap_or_default(),
        }
    }

    fn image_scale(&self) -> f64 {
        self.editor.as_ref().map_or(1.0, |e| e.scale)
    }

    /// Places a text caret at `at` (image pixels).
    fn start_text(&mut self, at: (f64, f64)) {
        self.commit_text();
        let shape = Annotation { kind: Kind::Text { at, text: String::new() }, width: self.tool_width() * self.image_scale(), fill: self.current_fill() };
        self.annotations.push(shape);
        self.editing = Some(self.annotations.len() - 1);
    }

    /// Ends text entry, dropping it if nothing was typed.
    fn commit_text(&mut self) {
        if let Some(i) = self.editing.take()
            && matches!(&self.annotations.get(i), Some(Annotation { kind: Kind::Text { text, .. }, .. }) if text.is_empty())
        {
            self.annotations.remove(i);
        }
    }

    fn cancel_text(&mut self) {
        if let Some(i) = self.editing.take() {
            self.annotations.remove(i);
        }
    }

    fn editing_text(&mut self) -> Option<&mut String> {
        match &mut self.annotations.get_mut(self.editing?)?.kind {
            Kind::Text { text, .. } => Some(text),
            _ => None,
        }
    }

    fn tool_width(&self) -> f64 {
        self.picker.as_ref().map_or(self.tool.default_width(), |p| p.state().width(self.tool))
    }

    fn set_tool_width(&mut self, width: f64) {
        let tool = self.tool;
        if let Some(p) = &mut self.picker {
            p.state_mut().set_width(tool, width);
        }
    }

    fn advanced_size(&self) -> bool {
        self.picker.as_ref().is_some_and(|p| p.state().advanced_size)
    }

    fn undo(&mut self) {
        self.commit_text();
        self.annotations.pop();
    }

    fn picker_open(&self) -> bool {
        self.picker.as_ref().is_some_and(|p| p.open)
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
            (Mode::Annotate, _) if self.picker.as_ref().is_some_and(|p| p.dragging()) => Shape::Default,
            (Mode::Annotate, Drag::None) if self.hover_button.is_some() => Shape::Pointer,
            (Mode::Annotate, Drag::None) if self.over_toolbar => Shape::Default,
            (Mode::Annotate, Drag::None) if self.tool == Tool::Text => Shape::Text,
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
        if let Some(picker) = self.picker.as_mut().filter(|p| p.dragging()) {
            picker.motion(pos);
            self.redraw_all(qh);
            return;
        }
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
            // Leaving the arrow button before the long press fires cancels it.
            if let Some((tool, _)) = self.long_press
                && self.menu.is_none()
                && !s.buttons.iter().any(|(r, h)| *h == Hit::Tool(tool) && r.contains(pos))
            {
                self.long_press = None;
            }
            let hover = s.buttons.iter().find(|(r, _)| r.contains(pos)).map(|(_, hit)| *hit);
            if hover != self.hover_button {
                self.hover_gen += 1;
                self.tooltip = None;
                if hover.and_then(tooltip_text).is_some() {
                    self.timers.push((TOOLTIP_DELAY, Timer::Tooltip(self.hover_gen)));
                }
            }
            self.hover_button = hover;
            // Panels and menus count as toolbar: they're chrome, not canvas.
            self.over_toolbar = s.toolbar.is_some_and(|r| r.contains(pos))
                || s.menu.is_some_and(|r| r.contains(pos))
                || self.picker.as_ref().is_some_and(|pk| pk.contains(pos));
            if self.over_toolbar && self.picker.as_ref().is_some_and(|pk| pk.contains(pos)) {
                self.hover_button = None;
            }
        }
        self.redraw_all(qh);
    }

    pub fn pointer_button(&mut self, button: u32, pressed: bool, qh: &QueueHandle<App>) -> Option<Outcome> {
        if button != BTN_LEFT || self.hidden {
            return None;
        }
        let p = self.pointer?;
        if pressed {
            self.tooltip = None;
            self.hover_gen += 1;
        }
        let outcome = match (self.mode, pressed) {
            (Mode::Picker, true) => {
                let id = match self.hover_button {
                    Some(Hit::Output(id)) => id,
                    _ => self.surfs.iter().find(|s| s.meta.logical.contains(p)).map(|s| s.meta.id)?,
                };
                return Some(Outcome::Picked(id));
            }
            (Mode::Picker, false) => None,
            (Mode::Annotate, true) if self.press_picker(p) => None,
            (Mode::Annotate, true) if self.menu.is_some() => {
                // A tool menu is open: pick from it, or any other click just closes it.
                if !self.choose_menu_item(self.hover_button) {
                    self.menu = None;
                }
                None
            }
            (Mode::Annotate, true) => match self.hover_button {
                Some(Hit::Tool(tool)) if tool.has_menu() => {
                    // Decided on release: a short click picks the tool, a long press opens the menu.
                    self.long_press = Some((tool, self.hover_gen));
                    self.timers.push((LONG_PRESS, Timer::LongPress(self.hover_gen)));
                    None
                }
                Some(Hit::Tool(tool)) => {
                    self.set_tool(tool);
                    None
                }
                Some(Hit::Color) => {
                    if let Some(picker) = &mut self.picker {
                        picker.toggle();
                    }
                    None
                }
                Some(Hit::Undo) => {
                    self.undo();
                    None
                }
                Some(Hit::SizePreset(i)) => {
                    self.set_tool_width(self.tool.default_width() * SIZE_PRESETS[i]);
                    None
                }
                Some(Hit::SizeAdvanced) => {
                    if let Some(p) = &mut self.picker {
                        p.state_mut().advanced_size ^= true;
                    }
                    None
                }
                Some(Hit::SizeDown) => {
                    self.set_tool_width(self.tool_width() - 1.0);
                    None
                }
                Some(Hit::SizeUp) => {
                    self.set_tool_width(self.tool_width() + 1.0);
                    None
                }
                Some(Hit::Copy) => return Some(self.annotated(Action::Copy)),
                Some(Hit::Save) => return Some(self.annotated(Action::Save)),
                Some(Hit::Done) => return Some(self.annotated(Action::Default)),
                Some(Hit::Close) => return Some(Outcome::Cancel),
                _ if self.over_toolbar => None,
                // A click outside the open panel just closes it.
                _ if self.picker_open() => {
                    if let Some(picker) = &mut self.picker {
                        picker.close();
                    }
                    None
                }
                _ => {
                    // Drawing starts on the image; the drag may then leave it and is clamped.
                    match self.to_image(p).filter(|&ip| self.clamp_image(ip) == ip) {
                        Some(ip) if self.tool == Tool::Text => self.start_text(ip),
                        Some(ip) if self.tool == Tool::Number => {
                            self.commit_text();
                            let shape = Annotation { kind: Kind::Number { at: ip }, width: self.tool_width() * self.image_scale(), fill: self.current_fill() };
                            self.annotations.push(shape);
                        }
                        Some(ip) => {
                            self.commit_text();
                            self.drag = Drag::Draw { anchor: ip };
                        }
                        None => self.commit_text(),
                    }
                    None
                }
            },
            (Mode::Annotate, false) => {
                if let Some(picker) = self.picker.as_mut().filter(|p| p.dragging()) {
                    picker.release();
                }
                if let Some((tool, _)) = self.long_press.take() {
                    match self.hover_button {
                        // Released on the button before the menu opened: a plain click.
                        Some(Hit::Tool(t)) if t == tool && self.menu.is_none() => self.set_tool(tool),
                        // Press, slide onto an item, release.
                        hit => {
                            self.choose_menu_item(hit);
                        }
                    }
                }
                if let (Drag::Draw { anchor }, Some(ip)) = (std::mem::replace(&mut self.drag, Drag::None), self.to_image(p)) {
                    let end = self.clamp_image(ip);
                    if let Some(kind) = self.shape_for(anchor, end) {
                        let shape = Annotation { kind, width: self.tool_width() * self.image_scale(), fill: self.current_fill() };
                        self.annotations.push(shape);
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

    /// The shape the current tool makes for a drag from `a` to `b` (image pixels), or None
    /// for a click-sized drag.
    fn shape_for(&self, a: (f64, f64), b: (f64, f64)) -> Option<Kind> {
        let min = MIN_SELECTION * self.image_scale();
        match self.tool {
            Tool::Rectangle => {
                let rect = Rect::from_points(a, b);
                (rect.w >= min && rect.h >= min).then_some(Kind::Rectangle(rect))
            }
            Tool::Arrow => ((b.0 - a.0).hypot(b.1 - a.1) >= min).then_some(Kind::Arrow { from: a, to: b, heads: self.arrow_heads() }),
            Tool::Highlight | Tool::Blur => {
                let rect = Rect::from_points(a, b);
                (rect.w >= min && rect.h >= min).then_some(if self.tool == Tool::Blur { Kind::Blur(rect) } else { Kind::Highlight(rect) })
            }
            Tool::Text | Tool::Number => None,
        }
    }

    /// Typing into the text being edited. Returns false when no text is being edited.
    fn text_key(&mut self, event: &KeyEvent, mods: Modifiers) -> bool {
        if self.editing.is_none() {
            return false;
        }
        match event.keysym {
            Keysym::Return | Keysym::KP_Enter => self.commit_text(),
            Keysym::Escape => self.cancel_text(),
            Keysym::BackSpace => {
                if let Some(text) = self.editing_text() {
                    text.pop();
                }
            }
            // Every other key is swallowed so shortcuts can't fire mid-word.
            _ if mods.ctrl || mods.alt || mods.logo => {}
            _ => {
                let typed: String = event.utf8.as_deref().unwrap_or("").chars().filter(|c| !c.is_control()).collect();
                if let Some(text) = self.editing_text() {
                    text.push_str(&typed);
                }
            }
        }
        true
    }

    /// Held keys: only typing and size steps repeat.
    pub fn key_repeat(&mut self, event: &KeyEvent, mods: Modifiers, qh: &QueueHandle<App>) {
        let repeatable = match event.keysym {
            Keysym::Return | Keysym::KP_Enter | Keysym::Escape => false,
            Keysym::bracketleft | Keysym::bracketright => self.editing.is_none(),
            _ => self.editing.is_some(),
        };
        if repeatable {
            let _ = self.key(event, mods, qh);
        }
    }

    pub fn key(&mut self, event: &KeyEvent, mods: Modifiers, qh: &QueueHandle<App>) -> Option<Outcome> {
        let keysym = event.keysym;
        if self.hidden {
            return None;
        }
        if self.text_key(event, mods) {
            self.redraw_all(qh);
            return None;
        }
        if keysym == Keysym::Escape && self.menu.is_some() {
            self.menu = None;
            self.redraw_all(qh);
            return None;
        }
        // Esc closes the colour panel before it closes the editor.
        if keysym == Keysym::Escape && self.picker_open() {
            if let Some(picker) = &mut self.picker {
                picker.close();
            }
            self.redraw_all(qh);
            return None;
        }
        if keysym == Keysym::Escape {
            return Some(Outcome::Cancel);
        }
        if self.mode == Mode::Annotate {
            return match keysym {
                _ if self.drag != Drag::None => None,
                Keysym::Delete | Keysym::BackSpace if self.picker_open() => {
                    if let Some(picker) = &mut self.picker {
                        picker.delete_stop();
                    }
                    self.redraw_all(qh);
                    None
                }
                Keysym::Return | Keysym::KP_Enter if !mods.ctrl => Some(self.annotated(Action::Default)),
                Keysym::c | Keysym::C if mods.ctrl => Some(self.annotated(Action::Copy)),
                Keysym::s | Keysym::S if mods.ctrl => Some(self.annotated(Action::Save)),
                Keysym::z | Keysym::Z if mods.ctrl => {
                    self.undo();
                    self.redraw_all(qh);
                    None
                }
                Keysym::bracketleft | Keysym::bracketright if !mods.ctrl => {
                    let step = if keysym == Keysym::bracketleft { -1.0 } else { 1.0 };
                    self.set_tool_width(self.tool_width() + step);
                    self.redraw_all(qh);
                    None
                }
                Keysym::s | Keysym::a | Keysym::t | Keysym::h | Keysym::u | Keysym::n if !mods.ctrl && !mods.alt && !mods.logo => {
                    self.set_tool(match keysym {
                        Keysym::s => Tool::Rectangle,
                        Keysym::a => Tool::Arrow,
                        Keysym::h => Tool::Highlight,
                        Keysym::u => Tool::Blur,
                        Keysym::n => Tool::Number,
                        _ => Tool::Text,
                    });
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

    /// Routes a press to the open colour panel; true if the panel took it.
    fn press_picker(&mut self, p: (f64, f64)) -> bool {
        let Some(s) = self.surfs.first() else { return false };
        let local = (p.0 - s.meta.logical.x, p.1 - s.meta.logical.y);
        self.picker.as_mut().is_some_and(|picker| picker.open && picker.press(local))
    }

    /// Renders the export with the same renderer as the preview, then hands it over.
    fn annotated(&mut self, action: Action) -> Outcome {
        self.commit_text();
        let scale = self.image_scale();
        let (Some(editor), Some(s)) = (&self.editor, self.surfs.first_mut()) else {
            return Outcome::Failed("annotation editor has no image".into());
        };
        let (Some(gl), Some(texture)) = (s.gl.as_mut(), s.texture) else {
            return Outcome::Failed("annotation editor isn't ready yet".into());
        };
        if let Err(e) = gl.make_current() {
            return Outcome::Failed(format!("{e:#}"));
        }
        let font = gl.font;
        match render::export(&mut gl.canvas, font, texture, &editor.image, &self.annotations, scale, &mut s.blurs) {
            Ok(image) => Outcome::Annotated(image, action),
            Err(e) => Outcome::Failed(format!("rendering the annotated image: {e:#}")),
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
        let drawing = match (self.drag, self.pointer.and_then(|p| self.to_image(p))) {
            (Drag::Draw { anchor }, Some(ip)) => self.shape_for(anchor, self.clamp_image(ip)),
            _ => None,
        };
        if self.mode == Mode::Annotate {
            self.layout_editor(i);
            // Size and colour changes restyle the text under the caret, as in Noctalia.
            let (width, fill) = (self.tool_width() * self.image_scale(), self.current_fill());
            if let Some(shape) = self.editing.and_then(|i| self.annotations.get_mut(i)) {
                shape.width = width;
                shape.fill = fill;
            }
        }
        let picker_open = self.picker_open();
        let tool_width = self.tool_width();
        let toolbar_state = ToolbarState {
            hover: self.hover_button,
            picker_open,
            can_undo: !self.annotations.is_empty(),
            tool: self.tool,
            width: tool_width,
            advanced: self.advanced_size(),
            arrow_heads: self.arrow_heads(),
            menu: self.menu,
            highlight_color: self.highlight_color(),
        };
        let tooltip = self.tooltip.and_then(tooltip_text);
        let hovered_output = self.pointer.and_then(|p| self.surfs.iter().find(|s| s.meta.logical.contains(p)).map(|s| s.meta.id));

        let fill = self.picker.as_ref().map(|p| p.fill().clone()).unwrap_or_default();
        let shapes: Vec<Annotation> = match &self.editor {
            Some(e) => {
                let mut shapes = self.annotations.clone();
                shapes.extend(drawing.map(|kind| Annotation { kind, width: tool_width * e.scale, fill: fill.clone() }));
                shapes
            }
            None => Vec::new(),
        };
        let image_size = self.editor.as_ref().map(|e| (e.image.width, e.image.height));

        let s = &mut self.surfs[i];
        let Some(gl) = s.gl.as_mut() else { return Ok(()) };
        gl.make_current()?;
        let (w, h) = (s.size.0 as f32, s.size.1 as f32);
        let font = gl.font;
        let canvas = &mut gl.canvas;
        if let (Some(size), Some(tex)) = (image_size, s.texture) {
            render::prepare_blurs(canvas, &mut s.blurs, tex, size, &shapes);
        }
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
                    // Annotations are clipped to the image, as they are in the export.
                    canvas.save();
                    canvas.scissor(cx, cy, cw, ch);
                    let backdrop = render::Backdrop { size: (e.image.width, e.image.height), blurs: &s.blurs };
                    render::draw_shapes(canvas, font, &shapes, Map { ox: c.x, oy: c.y, fit: e.fit }, e.scale, self.editing, &backdrop);
                    canvas.restore();
                    // Inset outline separating the image from the surround; never exported.
                    let mut path = Path::new();
                    path.rect(cx + 0.5, cy + 0.5, cw - 1.0, ch - 1.0);
                    canvas.stroke_path(&path, &Paint::color(t.outline.fv()).with_line_width(1.0));
                }
                (s.buttons, s.toolbar) = draw_annotate_toolbar(canvas, t, font, w, &fill, &toolbar_state);
                let anchor = s.buttons.iter().find(|(_, hit)| *hit == Hit::Color).map(|(r, _)| *r);
                if let (Some(picker), Some(anchor), Some(bar)) = (self.picker.as_mut().filter(|p| p.open), anchor, s.toolbar) {
                    picker.draw(canvas, font, t, anchor, (bar.bottom() as f32) + SPACE_SM, w);
                }
                s.menu = None;
                let menu_tool = self.menu;
                let anchor = menu_tool.and_then(|tool| s.buttons.iter().find(|(_, hit)| *hit == Hit::Tool(tool))).map(|(r, _)| *r);
                if let (Some(tool), Some(anchor), Some(bar)) = (menu_tool, anchor, s.toolbar) {
                    let (items, menu) = draw_tool_menu(canvas, font, t, tool, anchor, bar.bottom() as f32 + SPACE_SM, w, &toolbar_state);
                    s.buttons.extend(items);
                    s.menu = Some(menu);
                }
                let target = self.tooltip.and_then(|hit| s.buttons.iter().find(|(_, h)| *h == hit)).map(|(r, _)| *r);
                if let (Some(text), Some(target), Some(bar), Some(font)) = (tooltip, target, s.toolbar, font) {
                    draw_tooltip(canvas, font, t, text, target, bar.bottom() as f32 + 6.0, w);
                }
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
    /// S / M / L as one segmented control.
    Presets,
    SizeLabel,
}

struct ToolbarState {
    hover: Option<Hit>,
    picker_open: bool,
    can_undo: bool,
    tool: Tool,
    width: f64,
    advanced: bool,
    arrow_heads: ArrowHeads,
    menu: Option<Tool>,
    highlight_color: Color,
}

const PRESET_W: f32 = 26.0;
const SIZE_LABEL_W: f32 = 30.0;
const DISABLED_ALPHA: f32 = 0.55;

/// Noctalia's wording; shortcuts in brackets.
fn tooltip_text(hit: Hit) -> Option<&'static str> {
    Some(match hit {
        Hit::Undo => "Undo (Ctrl+Z)",
        Hit::Tool(Tool::Rectangle) => "Rectangle (s)",
        Hit::Tool(Tool::Arrow) => "Arrow (a) · hold to choose the ends",
        Hit::Tool(Tool::Text) => "Text (t) · click to type, Enter to finish",
        Hit::Tool(Tool::Highlight) => "Highlight (h) · hold to choose the colour",
        Hit::Tool(Tool::Blur) => "Blur (u)",
        Hit::Tool(Tool::Number) => "Numbering (n) · click to place the next number",
        Hit::Color => "Colour or gradient",
        Hit::SizePreset(0) => "Small",
        Hit::SizePreset(1) => "Medium",
        Hit::SizePreset(_) => "Large",
        Hit::SizeAdvanced => "Fine size adjustment",
        Hit::SizeDown => "Thinner ([)",
        Hit::SizeUp => "Thicker (])",
        Hit::Copy => "Copy the annotated image (Ctrl+C)",
        Hit::Save => "Save the annotated image (Ctrl+S)",
        Hit::Done => "Apply the screenshot settings (Enter)",
        Hit::Close => "Close without delivering (Esc)",
        Hit::Output(_) | Hit::ArrowHeads(_) | Hit::HighlightColor(_) => return None,
    })
}

/// Noctalia's annotation toolbar: a rounded card centred at the top of each output.
fn draw_annotate_toolbar(canvas: &mut Canvas<OpenGl>, t: &Theme, font: Option<femtovg::FontId>, w: f32, current: &Fill, st: &ToolbarState) -> (Vec<(Rect, Hit)>, Option<Rect>) {
    use Item::*;
    let mut items = vec![
        Button(Hit::Undo),
        Separator,
        Button(Hit::Tool(Tool::Rectangle)),
        Button(Hit::Tool(Tool::Arrow)),
        Button(Hit::Tool(Tool::Text)),
        Button(Hit::Tool(Tool::Highlight)),
        Button(Hit::Tool(Tool::Blur)),
        Button(Hit::Tool(Tool::Number)),
    ];
    // The colour panel only matters for tools that paint with it.
    if st.tool.uses_fill() {
        items.push(Button(Hit::Color));
    }
    if st.tool.has_size() {
        items.extend([Separator, Presets, Button(Hit::SizeAdvanced)]);
        if st.advanced {
            items.extend([Button(Hit::SizeDown), SizeLabel, Button(Hit::SizeUp)]);
        }
    }
    items.extend([Separator, Button(Hit::Copy), Button(Hit::Save), Button(Hit::Done), Button(Hit::Close)]);
    let item_w = |item: &Item| match item {
        Button(_) => CONTROL_SM,
        Separator => 1.0,
        Presets => PRESET_W * 3.0,
        SizeLabel => SIZE_LABEL_W,
    };
    let content_w: f32 = items.iter().map(item_w).sum::<f32>() + SPACE_SM * (items.len() - 1) as f32;
    let (card_w, card_h) = (content_w + SPACE_MD * 2.0, CONTROL_SM + SPACE_SM * 2.0);
    let x0 = ((w - card_w) / 2.0).round();
    let y0 = SPACE_MD;
    card(canvas, (x0, y0, card_w, card_h), RADIUS_XL, t.surface.with_alpha(TOOLBAR_FILL_ALPHA), t.outline);

    let preset = SIZE_PRESETS.iter().position(|m| (st.tool.default_width() * m - st.width).abs() < 1e-6);
    let mut buttons = Vec::new();
    let mut x = x0 + SPACE_MD;
    let y = y0 + SPACE_SM;
    for item in &items {
        match *item {
            Separator => {
                let h = CONTROL_SM * 0.6;
                fill_rect(canvas, x, y + (CONTROL_SM - h) / 2.0, 1.0, h, Paint::color(t.outline.fv()));
            }
            Presets => {
                let (sy, sh) = (y + 2.0, CONTROL_SM - 4.0);
                card(canvas, (x, sy, PRESET_W * 3.0, sh), RADIUS_MD, t.surface_variant, t.outline);
                for (i, label) in ["S", "M", "L"].into_iter().enumerate() {
                    let hit = Hit::SizePreset(i);
                    let bx = x + PRESET_W * i as f32;
                    let (fill, fg) = match (preset == Some(i), st.hover == Some(hit)) {
                        (true, _) => (Some(t.primary), t.on_primary),
                        (false, true) => (Some(t.hover), t.on_hover),
                        _ => (None, t.on_surface),
                    };
                    if let Some(fill) = fill {
                        let mut path = Path::new();
                        path.rounded_rect(bx + 2.0, sy + 2.0, PRESET_W - 4.0, sh - 4.0, RADIUS_MD - 2.0);
                        canvas.fill_path(&path, &Paint::color(fill.fv()));
                    }
                    if let Some(font) = font {
                        let paint = text_paint(font, CAPTION_FONT, fg);
                        let tw = text_width(canvas, label, &paint);
                        let _ = canvas.fill_text(bx + (PRESET_W - tw) / 2.0, sy + sh / 2.0, label, &paint);
                    }
                    buttons.push((to_rect((bx, sy, PRESET_W, sh)), hit));
                }
            }
            SizeLabel => {
                if let Some(font) = font {
                    let paint = text_paint(font, CAPTION_FONT, t.on_surface);
                    let label = format!("{}", st.width.round() as i64);
                    let tw = text_width(canvas, &label, &paint);
                    let _ = canvas.fill_text(x + (SIZE_LABEL_W - tw) / 2.0, y + CONTROL_SM / 2.0, &label, &paint);
                }
            }
            Button(hit) => {
                let enabled = hit != Hit::Undo || st.can_undo;
                // The selected tool and Done (the primary action) are filled.
                let (fill, fg) = match (hit, st.hover == Some(hit) && enabled) {
                    (Hit::Tool(tool), _) if tool == st.tool => (Some(t.primary), t.on_primary),
                    (Hit::Tool(tool), false) if st.menu == Some(tool) => (Some(t.surface_variant), t.on_surface),
                    (_, true) => (Some(t.hover), t.on_hover),
                    (Hit::Color, false) if st.picker_open => (Some(t.surface_variant), t.on_surface),
                    (Hit::SizeAdvanced, false) if st.advanced => (Some(t.surface_variant), t.on_surface),
                    (Hit::Done, false) => (Some(t.primary), t.on_primary),
                    _ if !enabled => (None, t.on_surface.with_alpha(DISABLED_ALPHA)),
                    _ => (None, t.on_surface),
                };
                if let Some(fill) = fill {
                    let mut path = Path::new();
                    path.rounded_rect(x, y, CONTROL_SM, CONTROL_SM, RADIUS_MD);
                    canvas.fill_path(&path, &Paint::color(fill.fv()));
                }
                let pad = (CONTROL_SM - ICON) / 2.0;
                if hit == Hit::Color {
                    // The button shows the current paint.
                    let r = to_rect((x + pad, y + pad, ICON, ICON));
                    crate::picker::swatch(canvas, r, current, false, t, matches!(current, Fill::Solid(_)));
                } else if let Hit::Tool(tool) = hit
                    && tool.has_menu()
                {
                    match tool {
                        Tool::Arrow => draw_arrow_icon(canvas, st.arrow_heads, x + pad, y + pad, fg),
                        _ => draw_highlight_icon(canvas, st.highlight_color, x + pad, y + pad, fg),
                    }
                    // Corner mark: this button has a menu (long press).
                    let mut mark = Path::new();
                    mark.move_to(x + CONTROL_SM - 4.0, y + CONTROL_SM - 9.0);
                    mark.line_to(x + CONTROL_SM - 4.0, y + CONTROL_SM - 4.0);
                    mark.line_to(x + CONTROL_SM - 9.0, y + CONTROL_SM - 4.0);
                    mark.close();
                    canvas.fill_path(&mark, &Paint::color(fg.with_alpha(0.8).fv()));
                } else {
                    draw_icon(canvas, hit, x + pad, y + pad, fg);
                }
                buttons.push((to_rect((x, y, CONTROL_SM, CONTROL_SM)), hit));
            }
        }
        x += item_w(item) + SPACE_SM;
    }
    (buttons, Some(to_rect((x0, y0, card_w, card_h))))
}

const MENU_W: f32 = 200.0;

/// A tool's long-press menu under its button: arrow ends or highlight colours. Returns its items
/// and its card.
#[allow(clippy::too_many_arguments)]
fn draw_tool_menu(canvas: &mut Canvas<OpenGl>, font: Option<femtovg::FontId>, t: &Theme, tool: Tool, anchor: Rect, top: f32, w: f32, st: &ToolbarState) -> (Vec<(Rect, Hit)>, Rect) {
    let entries: Vec<(Hit, &str, bool)> = match tool {
        Tool::Highlight => HIGHLIGHT_COLORS.iter().enumerate().map(|(i, (name, c))| (Hit::HighlightColor(i), *name, *c == st.highlight_color)).collect(),
        _ => ArrowHeads::ALL.iter().map(|h| (Hit::ArrowHeads(*h), h.label(), *h == st.arrow_heads)).collect(),
    };
    let pad = SPACE_SM / 2.0;
    let cw = if tool == Tool::Highlight { MENU_W * 0.7 } else { MENU_W };
    let ch = CONTROL_SM * entries.len() as f32 + pad * 2.0;
    let x0 = (anchor.x as f32).clamp(4.0, (w - cw - 4.0).max(4.0)).round();
    card(canvas, (x0, top, cw, ch), RADIUS_MD + 2.0, t.surface.with_alpha(0.96), t.outline);
    let mut items = Vec::new();
    for (i, (hit, label, selected)) in entries.into_iter().enumerate() {
        let r = (x0 + pad, top + pad + CONTROL_SM * i as f32, cw - pad * 2.0, CONTROL_SM);
        let (fill, fg) = match (selected, st.hover == Some(hit)) {
            (_, true) => (Some(t.hover), t.on_hover),
            (true, false) => (Some(t.primary), t.on_primary),
            _ => (None, t.on_surface),
        };
        if let Some(fill) = fill {
            let mut path = Path::new();
            path.rounded_rect(r.0, r.1, r.2, r.3, RADIUS_MD);
            canvas.fill_path(&path, &Paint::color(fill.fv()));
        }
        let ipad = (CONTROL_SM - ICON) / 2.0;
        match hit {
            Hit::ArrowHeads(heads) => draw_arrow_icon(canvas, heads, r.0 + ipad, r.1 + ipad, fg),
            Hit::HighlightColor(i) => draw_highlight_icon(canvas, HIGHLIGHT_COLORS[i].1, r.0 + ipad, r.1 + ipad, fg),
            _ => {}
        }
        if let Some(font) = font {
            let _ = canvas.fill_text(r.0 + CONTROL_SM + 2.0, r.1 + CONTROL_SM / 2.0, label, &text_paint(font, CAPTION_FONT, fg));
        }
        items.push((to_rect(r), hit));
    }
    (items, to_rect((x0, top, cw, ch)))
}

/// A marked box in the highlighter's colour.
fn draw_highlight_icon(canvas: &mut Canvas<OpenGl>, color: Color, x: f32, y: f32, outline: Color) {
    let mut path = Path::new();
    path.rounded_rect(x + 1.5, y + 4.0, 13.0, 8.0, 1.5);
    canvas.fill_path(&path, &Paint::color(color.with_alpha(0.85).fv()));
    canvas.stroke_path(&path, &Paint::color(outline.fv()).with_line_width(1.5));
}

/// A diagonal arrow with heads where `heads` says.
fn draw_arrow_icon(canvas: &mut Canvas<OpenGl>, heads: ArrowHeads, x: f32, y: f32, color: Color) {
    let (a, b) = ((x + 3.0, y + 13.0), (x + 13.0, y + 3.0));
    let mut path = Path::new();
    path.move_to(a.0, a.1);
    path.line_to(b.0, b.1);
    if matches!(heads, ArrowHeads::End | ArrowHeads::Both) {
        path.move_to(b.0 - 6.0, b.1);
        path.line_to(b.0, b.1);
        path.line_to(b.0, b.1 + 6.0);
    }
    if matches!(heads, ArrowHeads::Start | ArrowHeads::Both) {
        path.move_to(a.0 + 6.0, a.1);
        path.line_to(a.0, a.1);
        path.line_to(a.0, a.1 - 6.0);
    }
    let paint = Paint::color(color.fv()).with_line_width(1.75).with_line_cap(femtovg::LineCap::Round).with_line_join(femtovg::LineJoin::Round);
    canvas.stroke_path(&path, &paint);
}

/// A caption card under the hovered toolbar button, kept on screen.
fn draw_tooltip(canvas: &mut Canvas<OpenGl>, font: femtovg::FontId, t: &Theme, text: &str, target: Rect, top: f32, w: f32) {
    let paint = text_paint(font, CAPTION_FONT, t.on_surface);
    let tw = text_width(canvas, text, &paint);
    let (cw, ch) = (tw + SPACE_MD * 2.0, CAPTION_FONT * 1.3 + SPACE_SM * 2.0);
    let x = ((target.x + target.w / 2.0) as f32 - cw / 2.0).clamp(4.0, (w - cw - 4.0).max(4.0)).round();
    card(canvas, (x, top, cw, ch), RADIUS_MD, t.surface.with_alpha(0.96), t.outline);
    let _ = canvas.fill_text(x + SPACE_MD, top + ch / 2.0, text, &paint);
}

/// Line icons in a 16×16 box, after the Tabler glyphs Noctalia uses.
fn draw_icon(canvas: &mut Canvas<OpenGl>, hit: Hit, x: f32, y: f32, color: Color) {
    let mut path = Path::new();
    match hit {
        Hit::Tool(Tool::Rectangle) => path.rounded_rect(x + 2.0, y + 2.0, 12.0, 12.0, 2.0),
        Hit::Tool(Tool::Number) => {
            // A circled 1.
            path.circle(x + 8.0, y + 8.0, 6.5);
            path.move_to(x + 6.5, y + 6.0);
            path.line_to(x + 8.25, y + 4.75);
            path.line_to(x + 8.25, y + 11.25);
        }
        Hit::Tool(Tool::Blur) => {
            // Tabler blur: a circle fading into lines.
            path.circle(x + 8.0, y + 8.0, 6.0);
            for ly in [5.0, 8.0, 11.0] {
                path.move_to(x + 8.0, y + ly);
                path.line_to(x + 13.5 - (ly - 8.0f32).abs() * 0.4, y + ly);
            }
        }
        Hit::Tool(Tool::Text) => {
            // Tabler typography-ish: a T.
            path.move_to(x + 3.0, y + 4.0);
            path.line_to(x + 3.0, y + 2.5);
            path.line_to(x + 13.0, y + 2.5);
            path.line_to(x + 13.0, y + 4.0);
            path.move_to(x + 8.0, y + 2.5);
            path.line_to(x + 8.0, y + 13.5);
            path.move_to(x + 6.0, y + 13.5);
            path.line_to(x + 10.0, y + 13.5);
        }
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
        Hit::Undo => {
            // Tabler arrow-back-up.
            path.move_to(x + 6.0, y + 9.5);
            path.line_to(x + 3.0, y + 6.5);
            path.line_to(x + 6.0, y + 3.5);
            path.move_to(x + 3.0, y + 6.5);
            path.line_to(x + 10.5, y + 6.5);
            path.bezier_to(x + 14.5, y + 6.5, x + 14.5, y + 12.5, x + 10.5, y + 12.5);
            path.line_to(x + 9.5, y + 12.5);
        }
        Hit::SizeAdvanced => {
            // Tabler adjustments-horizontal: three sliders.
            for (ly, kx) in [(4.0, 10.0), (8.0, 5.0), (12.0, 11.0)] {
                path.move_to(x + 2.0, y + ly);
                path.line_to(x + kx - 2.0, y + ly);
                path.move_to(x + kx + 2.0, y + ly);
                path.line_to(x + 14.0, y + ly);
                path.circle(x + kx, y + ly, 2.0);
            }
        }
        Hit::SizeDown => {
            path.move_to(x + 4.0, y + 8.0);
            path.line_to(x + 12.0, y + 8.0);
        }
        Hit::SizeUp => {
            path.move_to(x + 4.0, y + 8.0);
            path.line_to(x + 12.0, y + 8.0);
            path.move_to(x + 8.0, y + 4.0);
            path.line_to(x + 8.0, y + 12.0);
        }
        Hit::Output(_) | Hit::Color | Hit::SizePreset(_) | Hit::Tool(Tool::Arrow | Tool::Highlight) | Hit::ArrowHeads(_) | Hit::HighlightColor(_) => return,
    }
    let paint = Paint::color(color.fv())
        .with_line_width(1.75)
        .with_line_cap(femtovg::LineCap::Round)
        .with_line_join(femtovg::LineJoin::Round);
    canvas.stroke_path(&path, &paint);
}
