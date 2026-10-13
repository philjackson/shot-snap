//! Capturing one window's own contents, so windows stacked above it aren't included. Uses the
//! standard `ext-foreign-toplevel-list` + `ext-image-capture-source` + `ext-image-copy-capture`
//! protocols; on Hyprland, `hyprland-toplevel-mapping` ties each toplevel to the window address
//! hyprctl reports, so the picked window is matched exactly. Elsewhere windows are matched by
//! title and app id.

use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::shm::raw::RawPool;
use smithay_client_toolkit::shm::Shm;
use wayland_client::backend::{ObjectData, ObjectId};
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::Transform;
use wayland_client::protocol::wl_shm::Format;
use wayland_client::{Connection, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};

use crate::app::App;
use crate::capture::{BufferData, NoEvents};
use crate::hyprland_proto::mapping::{
    hyprland_toplevel_mapping_manager_v1::HyprlandToplevelMappingManagerV1,
    hyprland_toplevel_window_mapping_handle_v1::{self, HyprlandToplevelWindowMappingHandleV1},
};
use crate::image::{bytes_per_pixel, format_rank, Image};
use crate::windows::Window;

struct Toplevel {
    handle: ExtForeignToplevelHandleV1,
    title: String,
    app_id: String,
    /// Title / app id arrive before `done`, which applies them.
    pending: (Option<String>, Option<String>),
    /// Hyprland window address, once the mapping answers.
    address: Option<u64>,
    mapped: bool,
}

/// The compositor's open windows, kept up to date for as long as the daemon runs.
pub struct Toplevels {
    list: Option<ExtForeignToplevelListV1>,
    mapping: Option<HyprlandToplevelMappingManagerV1>,
    sources: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,
    copy: Option<ExtImageCopyCaptureManagerV1>,
    items: Vec<Toplevel>,
}

pub struct ListData;
pub struct HandleData;
pub struct MappingData(ObjectId);

impl Toplevels {
    pub fn bind(globals: &GlobalList, qh: &QueueHandle<App>) -> Self {
        let sources = globals.bind(qh, 1..=1, NoEvents).ok();
        let copy = globals.bind(qh, 1..=1, NoEvents).ok();
        // Only worth listing windows if they can be captured.
        let list = (sources.is_some() && copy.is_some()).then(|| globals.bind(qh, 1..=1, ListData).ok()).flatten();
        let mapping = list.as_ref().and_then(|_| globals.bind(qh, 1..=1, NoEvents).ok());
        if list.is_none() {
            log::info!("compositor can't capture single windows; window capture will grab the window's screen area");
        }
        Self { list, mapping, sources, copy, items: Vec::new() }
    }

    /// The toplevel for a window found through the compositor's IPC, if it can be told apart.
    fn find(&self, window: &Window) -> Option<&ExtForeignToplevelHandleV1> {
        self.list.as_ref()?;
        if let Some(address) = window.address
            && self.mapping.is_some()
        {
            return self.items.iter().find(|t| t.address == Some(address)).map(|t| &t.handle);
        }
        // No exact mapping: title and app id, but only if exactly one window matches.
        let mut matches = self.items.iter().filter(|t| t.title == window.title && (window.app_id.is_empty() || t.app_id == window.app_id));
        match (matches.next(), matches.next()) {
            (Some(t), None) => Some(&t.handle),
            _ => None,
        }
    }

    fn item(&mut self, handle: &ExtForeignToplevelHandleV1) -> Option<&mut Toplevel> {
        self.items.iter_mut().find(|t| &t.handle == handle)
    }
}

/// A one-shot capture of a single window.
pub struct WindowCapture {
    pub id: u64,
    source: ExtImageCaptureSourceV1,
    session: ExtImageCopyCaptureSessionV1,
    size: Option<(u32, u32)>,
    formats: Vec<Format>,
    frame: Option<(ExtImageCopyCaptureFrameV1, Format, u32, RawPool, WlBuffer)>,
    transform: Transform,
}

pub struct SessionData(u64);
pub struct FrameData(u64);

impl WindowCapture {
    /// Starts capturing `window`, or returns None if this compositor can't (or the window can't
    /// be matched), in which case the caller captures its screen area instead.
    pub fn start(id: u64, toplevels: &Toplevels, window: &Window, paint_cursor: bool, qh: &QueueHandle<App>) -> Option<Self> {
        let (sources, copy) = (toplevels.sources.as_ref()?, toplevels.copy.as_ref()?);
        let handle = toplevels.find(window)?;
        let source = sources.create_source(handle, qh, NoEvents);
        let options = if paint_cursor { Options::PaintCursors } else { Options::empty() };
        let session = copy.create_session(&source, options, qh, SessionData(id));
        Some(Self { id, source, session, size: None, formats: Vec::new(), frame: None, transform: Transform::Normal })
    }

    /// Abandons the capture (job finished another way).
    pub fn cancel(self) {
        self.destroy();
    }

    fn destroy(self) {
        if let Some((frame, _, _, _, buffer)) = self.frame {
            frame.destroy();
            buffer.destroy();
        }
        self.session.destroy();
        self.source.destroy();
    }

    /// Session constraints are known: allocate a buffer and ask for one frame.
    fn request_frame(&mut self, shm: &Shm, qh: &QueueHandle<App>) -> Result<(), String> {
        if self.frame.is_some() {
            return Ok(());
        }
        let (w, h) = self.size.ok_or("no buffer size")?;
        let format = self.formats.iter().copied().filter(|f| format_rank(*f).is_some()).min_by_key(|f| format_rank(*f)).ok_or("no supported shm format")?;
        let stride = w * bytes_per_pixel(format);
        let mut pool = RawPool::new((stride * h) as usize, shm).map_err(|e| e.to_string())?;
        let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, format, BufferData, qh);
        let frame = self.session.create_frame(qh, FrameData(self.id));
        frame.attach_buffer(&buffer);
        frame.damage_buffer(0, 0, w as i32, h as i32);
        frame.capture();
        self.frame = Some((frame, format, stride, pool, buffer));
        Ok(())
    }

    fn read(mut self) -> Result<Image, String> {
        let (w, h) = self.size.ok_or("no buffer size")?;
        let (_, format, stride, pool, _) = self.frame.as_mut().ok_or("no frame")?;
        let image = Image::from_shm(*format, w, h, *stride, &pool.mmap()[..]).ok_or("could not decode the window")?;
        // Force the transform (if any): the buffer is the window's, not an output's.
        let image = image.orient(false, self.transform, (u32::MAX, u32::MAX), Some((w, h)));
        self.destroy();
        Ok(image)
    }
}

impl App {
    fn window_capture_event(&mut self, id: u64, result: Option<Result<(), String>>) {
        match result {
            Some(Ok(())) => {
                if let Some(capture) = self.window_capture.take_if(|c| c.id == id) {
                    let result = capture.read();
                    self.window_captured(result);
                }
            }
            Some(Err(e)) => {
                if let Some(capture) = self.window_capture.take_if(|c| c.id == id) {
                    capture.destroy();
                    self.window_captured(Err(e));
                }
            }
            None => {}
        }
    }
}

// ---- Dispatch -----------------------------------------------------------------------------------

impl Dispatch2<ExtForeignToplevelListV1, App> for ListData {
    fn event(&self, app: &mut App, _: &ExtForeignToplevelListV1, event: ext_foreign_toplevel_list_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            app.toplevels.items.push(Toplevel { handle: toplevel, title: String::new(), app_id: String::new(), pending: (None, None), address: None, mapped: false });
        }
    }

    fn event_created_child(opcode: u16, qh: &QueueHandle<App>) -> std::sync::Arc<dyn ObjectData> {
        assert_eq!(opcode, ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE);
        qh.make_data::<ExtForeignToplevelHandleV1, _>(HandleData)
    }
}

impl Dispatch2<ExtForeignToplevelHandleV1, App> for HandleData {
    fn event(&self, app: &mut App, handle: &ExtForeignToplevelHandleV1, event: ext_foreign_toplevel_handle_v1::Event, _: &Connection, qh: &QueueHandle<App>) {
        use ext_foreign_toplevel_handle_v1::Event;
        let mapping = app.toplevels.mapping.clone();
        let Some(t) = app.toplevels.item(handle) else { return };
        match event {
            Event::Title { title } => t.pending.0 = Some(title),
            Event::AppId { app_id } => t.pending.1 = Some(app_id),
            Event::Done => {
                if let Some(title) = t.pending.0.take() {
                    t.title = title;
                }
                if let Some(app_id) = t.pending.1.take() {
                    t.app_id = app_id;
                }
                if let (Some(mapping), false) = (mapping, t.mapped) {
                    t.mapped = true;
                    mapping.get_window_for_toplevel(handle, qh, MappingData(handle.id()));
                }
            }
            Event::Closed => {
                app.toplevels.items.retain(|t| &t.handle != handle);
                handle.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch2<HyprlandToplevelWindowMappingHandleV1, App> for MappingData {
    fn event(&self, app: &mut App, mapping: &HyprlandToplevelWindowMappingHandleV1, event: hyprland_toplevel_window_mapping_handle_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        use hyprland_toplevel_window_mapping_handle_v1::Event;
        if let Event::WindowAddress { address_hi, address } = event
            && let Some(t) = app.toplevels.items.iter_mut().find(|t| t.handle.id() == self.0)
        {
            t.address = Some((u64::from(address_hi) << 32) | u64::from(address));
        }
        mapping.destroy();
    }
}

impl Dispatch2<ExtImageCopyCaptureSessionV1, App> for SessionData {
    fn event(&self, app: &mut App, _: &ExtImageCopyCaptureSessionV1, event: ext_image_copy_capture_session_v1::Event, _: &Connection, qh: &QueueHandle<App>) {
        use ext_image_copy_capture_session_v1::Event;
        let result = {
            let shm = &app.shm;
            let Some(capture) = app.window_capture.as_mut().filter(|c| c.id == self.0) else { return };
            match event {
                Event::BufferSize { width, height } => {
                    capture.size = Some((width, height));
                    None
                }
                Event::ShmFormat { format: WEnum::Value(format) } => {
                    capture.formats.push(format);
                    None
                }
                Event::Done => capture.request_frame(shm, qh).err().map(Err),
                Event::Stopped => Some(Err("the window went away".into())),
                _ => None,
            }
        };
        app.window_capture_event(self.0, result);
    }
}

impl Dispatch2<ExtImageCopyCaptureFrameV1, App> for FrameData {
    fn event(&self, app: &mut App, _: &ExtImageCopyCaptureFrameV1, event: ext_image_copy_capture_frame_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        use ext_image_copy_capture_frame_v1::Event;
        let result = match event {
            Event::Transform { transform: WEnum::Value(t) } => {
                if let Some(capture) = app.window_capture.as_mut().filter(|c| c.id == self.0) {
                    capture.transform = t;
                }
                None
            }
            Event::Ready => Some(Ok(())),
            Event::Failed { reason } => Some(Err(format!("compositor refused the window capture ({reason:?})"))),
            _ => None,
        };
        app.window_capture_event(self.0, result);
    }
}

impl Dispatch2<ExtForeignToplevelImageCaptureSourceManagerV1, App> for NoEvents {
    fn event(&self, _: &mut App, _: &ExtForeignToplevelImageCaptureSourceManagerV1, _: <ExtForeignToplevelImageCaptureSourceManagerV1 as Proxy>::Event, _: &Connection, _: &QueueHandle<App>) {}
}

impl Dispatch2<ExtImageCopyCaptureManagerV1, App> for NoEvents {
    fn event(&self, _: &mut App, _: &ExtImageCopyCaptureManagerV1, _: <ExtImageCopyCaptureManagerV1 as Proxy>::Event, _: &Connection, _: &QueueHandle<App>) {}
}

impl Dispatch2<ExtImageCaptureSourceV1, App> for NoEvents {
    fn event(&self, _: &mut App, _: &ExtImageCaptureSourceV1, _: <ExtImageCaptureSourceV1 as Proxy>::Event, _: &Connection, _: &QueueHandle<App>) {}
}

impl Dispatch2<HyprlandToplevelMappingManagerV1, App> for NoEvents {
    fn event(&self, _: &mut App, _: &HyprlandToplevelMappingManagerV1, _: <HyprlandToplevelMappingManagerV1 as Proxy>::Event, _: &Connection, _: &QueueHandle<App>) {}
}
