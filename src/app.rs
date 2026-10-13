use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

use anyhow::{bail, Context};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::channel::{self, Sender};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{EventLoop, Interest, LoopHandle, Mode as PollMode, PostAction};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers};
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{delegate_dispatch2, delegate_registry, registry_handlers};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface};
use wayland_client::{Connection, Proxy, QueueHandle};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

use crate::capture::{Batch, NoEvents, OutputMeta};
use crate::clipboard::Clipboard;
use crate::config::{self, Config};
use crate::deliver::{self, Action, Delivered, Plan};
use crate::geom::Rect;
use crate::gl::Gl;
use crate::image::{compose, Image, OutputFrame};
use crate::ipc::{FullscreenTarget, Request, Response};
use crate::overlay::{self, Outcome, Overlay};

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(2);
const HIDE_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy)]
enum Area {
    Rect(Rect),
    Output(u32),
    All,
}

#[derive(Debug, Clone, Copy)]
enum Stage {
    /// Capturing every output to show under the overlay.
    Freeze(overlay::Mode),
    Overlay,
    /// Waiting for the transparent overlay frame to be on screen before a live capture.
    Hiding(Area, Action),
    /// Capturing for the final image (no freeze, or a direct fullscreen request).
    Final(Area, Action),
    Delivering,
}

struct Job {
    reply: Option<UnixStream>,
    stage: Stage,
}

pub struct App {
    conn: Connection,
    qh: QueueHandle<App>,
    handle: LoopHandle<'static, App>,
    registry: RegistryState,
    outputs: OutputState,
    seats: SeatState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    pub shm: Shm,
    viewporter: WpViewporter,
    cursor_shapes: Option<CursorShapeManager>,
    screencopy: Option<ZwlrScreencopyManagerV1>,
    pub clipboard: Clipboard,
    gl: Gl,
    config: Config,
    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    cursor_device: Option<WpCursorShapeDeviceV1>,
    pointer_serial: Option<u32>,
    cursor_shape: Option<Shape>,
    modifiers: Modifiers,
    job: Option<Job>,
    pub capture: Option<Batch>,
    batch_counter: u64,
    overlay: Option<Overlay>,
    delivered: Sender<anyhow::Result<Delivered>>,
    exit: bool,
}

pub fn run_daemon() -> anyhow::Result<()> {
    let socket = crate::ipc::socket_path();
    if UnixStream::connect(&socket).is_ok() {
        bail!("a shot-snap daemon is already running ({})", socket.display());
    }
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;
    listener.set_nonblocking(true)?;

    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, queue) = registry_queue_init::<App>(&conn).context("listing Wayland globals")?;
    let qh = queue.handle();
    let mut event_loop: EventLoop<'static, App> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), queue).insert(handle.clone()).map_err(|e| anyhow::anyhow!("{e}"))?;

    let (config, config_error) = Config::load();
    if let Some(e) = config_error {
        log::error!("config error, using defaults: {e}");
    }

    let (delivered, delivered_rx) = channel::channel();
    handle
        .insert_source(delivered_rx, |event, _, app: &mut App| {
            if let channel::Event::Msg(result) = event {
                app.delivery_finished(result);
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    handle
        .insert_source(Generic::new(listener, Interest::READ, PollMode::Level), |_, listener, app: &mut App| {
            while let Ok((stream, _)) = listener.accept() {
                app.accept(stream);
            }
            Ok(PostAction::Continue)
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let screencopy = globals.bind::<ZwlrScreencopyManagerV1, _, _>(&qh, 1..=3, NoEvents).ok();
    if screencopy.is_none() {
        log::warn!("compositor has no wlr-screencopy; captures will fail");
    }
    let mut app = App {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        seats: SeatState::new(&globals, &qh),
        compositor: CompositorState::bind(&globals, &qh).context("wl_compositor missing")?,
        layer_shell: LayerShell::bind(&globals, &qh).context("compositor has no wlr-layer-shell")?,
        shm: Shm::bind(&globals, &qh).context("wl_shm missing")?,
        viewporter: globals.bind(&qh, 1..=1, NoEvents).context("compositor has no wp_viewporter")?,
        cursor_shapes: CursorShapeManager::bind(&globals, &qh).ok(),
        clipboard: Clipboard::bind(&globals, &qh),
        gl: Gl::new(&conn, &config.theme.font)?,
        screencopy,
        config,
        conn,
        qh,
        handle,
        seat: None,
        keyboard: None,
        pointer: None,
        cursor_device: None,
        pointer_serial: None,
        cursor_shape: None,
        modifiers: Modifiers::default(),
        job: None,
        capture: None,
        batch_counter: 0,
        overlay: None,
        delivered,
        exit: false,
    };

    log::info!("listening on {}", socket.display());
    while !app.exit {
        event_loop.dispatch(None, &mut app)?;
    }
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

impl App {
    // ---- requests ------------------------------------------------------------------------

    fn accept(&mut self, mut stream: UnixStream) {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            return;
        }
        let args: Vec<&str> = line.split_whitespace().collect();
        let request = match Request::parse(&args) {
            Ok(r) => r,
            Err(e) => return reply(&mut stream, Response::Error(e.to_string())),
        };
        log::debug!("request: {request:?}");
        match request {
            Request::Ping => reply(&mut stream, Response::Done(None)),
            Request::Quit => {
                reply(&mut stream, Response::Done(None));
                self.exit = true;
            }
            _ if self.job.is_some() => reply(&mut stream, Response::Error("a capture is already in progress".into())),
            request => {
                // Pick up config edits without restarting the daemon.
                let (config, err) = Config::load();
                if let Some(e) = err {
                    return reply(&mut stream, Response::Error(format!("config error: {e}")));
                }
                self.config = config;
                self.job = Some(Job { reply: Some(stream), stage: Stage::Delivering });
                if let Err(e) = self.start(request) {
                    self.finish(Response::Error(format!("{e:#}")));
                }
            }
        }
    }

    fn start(&mut self, request: Request) -> anyhow::Result<()> {
        if self.screencopy.is_none() {
            bail!("Screen capture is not available on this compositor");
        }
        if Plan::new(&self.config, Action::Default).is_empty() {
            bail!("No screenshot output enabled");
        }
        let outputs = self.output_metas();
        if outputs.is_empty() {
            bail!("No outputs available");
        }
        match request {
            Request::Region => self.begin_overlay(overlay::Mode::Region),
            Request::Fullscreen(FullscreenTarget::Pick) if outputs.len() > 1 => self.begin_overlay(overlay::Mode::Picker),
            Request::Fullscreen(FullscreenTarget::Focused | FullscreenTarget::Pick) => {
                let focused = crate::focused::focused_output_name();
                let output = focused
                    .and_then(|name| outputs.iter().find(|o| o.name == name))
                    .unwrap_or(&outputs[0]);
                self.capture_final(Area::Output(output.id), Action::Default, vec![output.clone()]);
                Ok(())
            }
            Request::Fullscreen(FullscreenTarget::Named(selector)) => {
                let output = find_output(&outputs, &selector).ok_or_else(|| {
                    let names: Vec<_> = outputs.iter().map(|o| o.name.as_str()).collect();
                    anyhow::anyhow!("unknown output {selector:?}; available: {}", names.join(", "))
                })?;
                self.capture_final(Area::Output(output.id), Action::Default, vec![output.clone()]);
                Ok(())
            }
            Request::Fullscreen(FullscreenTarget::All) => {
                self.capture_final(Area::All, Action::Default, outputs);
                Ok(())
            }
            Request::Ping | Request::Quit => unreachable!(),
        }
    }

    fn begin_overlay(&mut self, mode: overlay::Mode) -> anyhow::Result<()> {
        if self.config.freeze_screen {
            self.set_stage(Stage::Freeze(mode));
            self.start_batch(self.output_metas());
        } else {
            self.open_overlay(mode, Vec::new());
        }
        Ok(())
    }

    fn open_overlay(&mut self, mode: overlay::Mode, frozen: Vec<OutputFrame>) {
        let outputs = self.output_metas();
        // Frames come back in output-id order, matching output_metas().
        let frozen: Vec<(u32, OutputFrame)> = frozen
            .into_iter()
            .filter_map(|f| outputs.iter().find(|o| o.logical == f.logical).map(|o| (o.id, f)))
            .collect();
        let remember = self.config.remember_last_region && mode == overlay::Mode::Region;
        let preselect = if remember { config::load_last_region() } else { None };
        let init = overlay::Init { compositor: &self.compositor, layer_shell: &self.layer_shell, viewporter: &self.viewporter, qh: &self.qh };
        let confirm = self.config.confirm_region || self.config.remember_last_region;
        self.overlay = Some(Overlay::new(init, mode, outputs, frozen, self.config.theme.clone(), confirm, preselect));
        self.cursor_shape = None;
        self.set_stage(Stage::Overlay);
    }

    fn overlay_outcome(&mut self, outcome: Outcome) {
        let Some(overlay) = &mut self.overlay else { return };
        if overlay.mode == overlay::Mode::Region && self.config.remember_last_region
            && let Some(sel) = overlay.selection() {
                config::save_last_region(sel);
            }
        let frozen = overlay.take_frozen();
        let (area, action) = match outcome {
            Outcome::Cancel => return self.finish(Response::Cancelled),
            Outcome::Region(rect, action) => (Area::Rect(rect), action),
            Outcome::Picked(id) => (Area::Output(id), Action::Default),
        };
        if !frozen.is_empty() {
            self.overlay = None;
            let _ = self.conn.flush();
            return self.compose_and_deliver(area, action, &frozen);
        }
        // Live screen: clear the overlay and capture once that's on screen. Frame callbacks
        // never arrive for outputs that are off, hence the fallback timer.
        overlay.hide(&self.qh);
        self.set_stage(Stage::Hiding(area, action));
        let _ = self.handle.insert_source(Timer::from_duration(HIDE_TIMEOUT), |_, _, app: &mut App| {
            app.capture_after_hide();
            TimeoutAction::Drop
        });
    }

    fn capture_after_hide(&mut self) {
        let Some(Stage::Hiding(area, action)) = self.job.as_ref().map(|j| j.stage) else { return };
        let outputs: Vec<OutputMeta> = self
            .output_metas()
            .into_iter()
            .filter(|o| match area {
                Area::Rect(r) => o.logical.intersect(&r).is_some(),
                Area::Output(id) => o.id == id,
                Area::All => true,
            })
            .collect();
        self.capture_final(area, action, outputs);
    }

    fn capture_final(&mut self, area: Area, action: Action, outputs: Vec<OutputMeta>) {
        self.set_stage(Stage::Final(area, action));
        self.start_batch(outputs);
    }

    fn start_batch(&mut self, outputs: Vec<OutputMeta>) {
        let Some(manager) = &self.screencopy else { return };
        self.batch_counter += 1;
        let id = self.batch_counter;
        self.capture = Some(Batch::start(id, manager, outputs, self.config.show_cursor, &self.qh));
        let _ = self.handle.insert_source(Timer::from_duration(CAPTURE_TIMEOUT), move |_, _, app: &mut App| {
            if app.capture.as_ref().is_some_and(|b| b.id == id) {
                app.capture_finished();
            }
            TimeoutAction::Drop
        });
    }

    /// Called when every output in the batch answered, or on timeout.
    pub fn capture_finished(&mut self) {
        let Some(batch) = self.capture.take() else { return };
        let (frames, failed) = batch.finish();
        if frames.is_empty() {
            let detail = if failed.is_empty() { String::new() } else { format!(" ({})", failed.join("; ")) };
            return self.finish(Response::Error(format!("screen capture failed{detail}")));
        }
        match self.job.as_ref().map(|j| j.stage) {
            Some(Stage::Freeze(mode)) => self.open_overlay(mode, frames),
            Some(Stage::Final(area, action)) => self.compose_and_deliver(area, action, &frames),
            _ => {}
        }
    }

    fn compose_and_deliver(&mut self, area: Area, action: Action, frames: &[OutputFrame]) {
        let metas = self.output_metas();
        let image = match area {
            Area::Rect(r) => compose(r, frames),
            Area::Output(id) => {
                let logical = metas.iter().find(|o| o.id == id).map(|o| o.logical);
                frames.iter().find(|f| Some(f.logical) == logical).or(frames.first()).map(|f| f.image.clone())
            }
            Area::All => frames.iter().map(|f| f.logical).reduce(|a, b| a.union(&b)).and_then(|r| compose(r, frames)),
        };
        match image {
            Some(image) => self.deliver(image, action),
            None => self.finish(Response::Error("selection is outside every output".into())),
        }
    }

    fn deliver(&mut self, image: Image, action: Action) {
        let plan = Plan::new(&self.config, action);
        if plan.copy && !self.clipboard.available() && !plan.save && !plan.pipe {
            return self.finish(Response::Error("clipboard unavailable: compositor has no data-control protocol".into()));
        }
        self.set_stage(Stage::Delivering);
        let config = self.config.clone();
        let tx = self.delivered.clone();
        std::thread::spawn(move || {
            let _ = tx.send(deliver::deliver(image, &config, plan));
        });
    }

    fn delivery_finished(&mut self, result: anyhow::Result<Delivered>) {
        let response = match result {
            Ok(delivered) => {
                let clip = match (delivered.clipboard, self.seat.clone()) {
                    (Some(png), Some(seat)) => self.clipboard.set(png, &seat, &self.qh),
                    (Some(_), None) => Err(anyhow::anyhow!("no seat for the clipboard")),
                    (None, _) => Ok(()),
                };
                match clip {
                    Err(e) if delivered.saved.is_none() => Response::Error(format!("{e:#}")),
                    Err(e) => {
                        log::warn!("{e:#}");
                        Response::Done(delivered.saved)
                    }
                    Ok(()) => Response::Done(delivered.saved),
                }
            }
            Err(e) => Response::Error(format!("{e:#}")),
        };
        self.finish(response);
    }

    fn finish(&mut self, response: Response) {
        self.overlay = None;
        self.capture = None;
        if let Response::Error(e) = &response {
            log::error!("{e}");
        }
        if let Some(mut stream) = self.job.take().and_then(|j| j.reply) {
            reply(&mut stream, response);
        }
        let _ = self.conn.flush();
    }

    fn set_stage(&mut self, stage: Stage) {
        if let Some(job) = &mut self.job {
            job.stage = stage;
        }
    }

    pub fn output_metas(&self) -> Vec<OutputMeta> {
        let mut metas: Vec<OutputMeta> = self
            .outputs
            .outputs()
            .filter_map(|wl| {
                let info = self.outputs.info(&wl)?;
                let mode = info.modes.iter().find(|m| m.current).map(|m| (m.dimensions.0 as u32, m.dimensions.1 as u32));
                let (x, y) = info.logical_position.unwrap_or(info.location);
                let (w, h) = info.logical_size.or_else(|| {
                    let (mw, mh) = mode?;
                    Some((mw as i32 / info.scale_factor.max(1), mh as i32 / info.scale_factor.max(1)))
                })?;
                Some(OutputMeta {
                    id: info.id,
                    wl,
                    name: info.name.clone().unwrap_or_default(),
                    description: info.description.clone().unwrap_or_default(),
                    logical: Rect { x: f64::from(x), y: f64::from(y), w: f64::from(w), h: f64::from(h) },
                    transform: info.transform,
                    mode,
                })
            })
            .filter(|o| o.logical.w > 0.0 && o.logical.h > 0.0)
            .collect();
        metas.sort_by_key(|o| o.id);
        metas
    }

    fn update_cursor(&mut self) {
        let (Some(overlay), Some(device), Some(serial)) = (&self.overlay, &self.cursor_device, self.pointer_serial) else { return };
        let shape = overlay.cursor();
        if self.cursor_shape != Some(shape) {
            device.set_shape(serial, shape);
            self.cursor_shape = Some(shape);
        }
    }
}

/// Exact connector name, or a word-boundary match in the description ("Dell" matches
/// "Dell Inc. U2720Q").
fn find_output<'a>(outputs: &'a [OutputMeta], selector: &str) -> Option<&'a OutputMeta> {
    outputs.iter().find(|o| o.name == selector).or_else(|| {
        let needle = selector.to_lowercase();
        outputs.iter().find(|o| {
            let hay = o.description.to_lowercase();
            hay.match_indices(&needle).any(|(i, m)| {
                let before = hay[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
                let after = hay[i + m.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric());
                before && after
            })
        })
    })
}

fn reply(stream: &mut UnixStream, response: Response) {
    let _ = stream.write_all(response.encode().as_bytes());
}

// ---- Wayland handlers ----------------------------------------------------------------------

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}

    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _: u32) {
        if let Some(overlay) = &mut self.overlay {
            overlay.frame(surface, qh);
            if overlay.hidden_presented() {
                self.capture_after_hide();
            }
        }
    }

    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if self.overlay.as_ref().is_some_and(|o| o.owns_layer(layer)) {
            self.overlay_outcome(Outcome::Cancel);
        }
    }

    fn configure(&mut self, _: &Connection, qh: &QueueHandle<Self>, layer: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
        let Some(overlay) = &mut self.overlay else { return };
        if let Err(e) = overlay.configure(layer, configure.new_size, &self.gl, qh) {
            self.finish(Response::Error(format!("overlay: {e:#}")));
        }
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.seat.get_or_insert(seat);
    }

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        self.seat.get_or_insert_with(|| seat.clone());
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seats.get_keyboard(qh, &seat, None).ok();
        }
        if capability == Capability::Pointer && self.pointer.is_none()
            && let Ok(pointer) = self.seats.get_pointer(qh, &seat) {
                self.cursor_device = self.cursor_shapes.as_ref().map(|m| m.get_shape_device(&pointer, qh));
                self.pointer = Some(pointer);
            }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Keyboard
            && let Some(k) = self.keyboard.take() {
                k.release();
            }
        if capability == Capability::Pointer {
            if let Some(d) = self.cursor_device.take() {
                d.destroy();
            }
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        let Some(overlay) = &mut self.overlay else { return };
        if let Some(outcome) = overlay.key(event.keysym, self.modifiers) {
            self.overlay_outcome(outcome);
        }
    }

    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}

    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, modifiers: Modifiers, _: RawModifiers, _: u32) {
        self.modifiers = modifiers;
    }
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for event in events {
            let Some(overlay) = &mut self.overlay else { return };
            let outcome = match event.kind {
                PointerEventKind::Enter { serial } => {
                    self.pointer_serial = Some(serial);
                    self.cursor_shape = None;
                    overlay.pointer_motion(&event.surface, event.position, qh);
                    None
                }
                PointerEventKind::Motion { .. } => {
                    overlay.pointer_motion(&event.surface, event.position, qh);
                    None
                }
                PointerEventKind::Press { button, .. } => overlay.pointer_button(button, true, qh),
                PointerEventKind::Release { button, .. } => overlay.pointer_button(button, false, qh),
                _ => None,
            };
            match outcome {
                Some(outcome) => self.overlay_outcome(outcome),
                None => self.update_cursor(),
            }
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

impl Dispatch2<WpViewporter, App> for NoEvents {
    fn event(&self, _: &mut App, _: &WpViewporter, _: <WpViewporter as Proxy>::Event, _: &Connection, _: &QueueHandle<App>) {}
}

impl Dispatch2<WpViewport, App> for NoEvents {
    fn event(&self, _: &mut App, _: &WpViewport, _: <WpViewport as Proxy>::Event, _: &Connection, _: &QueueHandle<App>) {}
}

delegate_registry!(App);
delegate_dispatch2!(App);
