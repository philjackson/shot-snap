//! Output capture through wlr-screencopy into shm buffers.

use std::collections::HashMap;

use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::shm::raw::RawPool;
use smithay_client_toolkit::shm::Shm;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::{Transform, WlOutput};
use wayland_client::protocol::wl_shm::Format;
use wayland_client::{Connection, QueueHandle, WEnum};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::{self, Flags, ZwlrScreencopyFrameV1};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

use crate::app::App;
use crate::geom::Rect;
use crate::image::{format_rank, Image, OutputFrame};

/// What we need to know about an output to orient and place its capture.
#[derive(Debug, Clone)]
pub struct OutputMeta {
    pub id: u32,
    pub wl: WlOutput,
    pub name: String,
    pub description: String,
    pub logical: Rect,
    pub transform: Transform,
    /// Current mode size, untransformed.
    pub mode: Option<(u32, u32)>,
}

impl OutputMeta {
    /// Physical pixel size in logical orientation.
    pub fn physical_size(&self) -> (u32, u32) {
        match self.mode {
            Some((w, h)) if matches!(self.transform, Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270) => (h, w),
            Some(m) => m,
            None => (self.logical.w as u32, self.logical.h as u32),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BufferSpec {
    format: Format,
    width: u32,
    height: u32,
    stride: u32,
}

struct Pending {
    meta: OutputMeta,
    frame: ZwlrScreencopyFrameV1,
    offers: Vec<BufferSpec>,
    copying: Option<(BufferSpec, RawPool, WlBuffer)>,
    y_invert: bool,
}

pub struct Batch {
    pub id: u64,
    pending: HashMap<u32, Pending>,
    done: Vec<(u32, OutputFrame)>,
    failed: Vec<String>,
}

pub struct FrameData {
    batch: u64,
    output: u32,
}

pub struct BufferData;
pub struct NoEvents;

impl Batch {
    pub fn start(
        id: u64,
        manager: &ZwlrScreencopyManagerV1,
        outputs: Vec<OutputMeta>,
        overlay_cursor: bool,
        qh: &QueueHandle<App>,
    ) -> Self {
        let pending = outputs
            .into_iter()
            .map(|meta| {
                let frame = manager.capture_output(overlay_cursor as i32, &meta.wl, qh, FrameData { batch: id, output: meta.id });
                (meta.id, Pending { meta, frame, offers: Vec::new(), copying: None, y_invert: false })
            })
            .collect();
        Self { id, pending, done: Vec::new(), failed: Vec::new() }
    }

    pub fn is_complete(&self) -> bool {
        self.pending.is_empty()
    }

    /// Gives up on outputs that haven't answered. Returns the frames in the order requested.
    pub fn finish(mut self) -> (Vec<OutputFrame>, Vec<String>) {
        for (_, p) in self.pending.drain() {
            log::warn!("output {} did not respond to screencopy, skipping it", p.meta.name);
            self.failed.push(format!("{}: timed out", p.meta.name));
            p.frame.destroy();
            if let Some((_, _, buf)) = p.copying {
                buf.destroy();
            }
        }
        self.done.sort_by_key(|(id, _)| *id);
        (self.done.into_iter().map(|(_, f)| f).collect(), self.failed)
    }

    fn event(&mut self, output: u32, event: zwlr_screencopy_frame_v1::Event, version: u32, qh: &QueueHandle<App>, shm: &Shm) {
        use zwlr_screencopy_frame_v1::Event;
        let Some(p) = self.pending.get_mut(&output) else { return };
        match event {
            Event::Buffer { format: WEnum::Value(format), width, height, stride } => {
                p.offers.push(BufferSpec { format, width, height, stride });
                // Before v3 there's no buffer_done and only one shm offer.
                if version < 3 {
                    Self::copy(p, qh, shm);
                }
            }
            Event::BufferDone => Self::copy(p, qh, shm),
            Event::Flags { flags: WEnum::Value(flags) } => p.y_invert = flags.contains(Flags::YInvert),
            Event::Ready { .. } => {
                let p = self.pending.remove(&output).unwrap();
                p.frame.destroy();
                match Self::read(p) {
                    Ok(frame) => self.done.push((output, frame)),
                    Err(e) => self.failed.push(e),
                }
            }
            Event::Failed => {
                let p = self.pending.remove(&output).unwrap();
                p.frame.destroy();
                if let Some((_, _, buf)) = p.copying {
                    buf.destroy();
                }
                self.failed.push(format!("{}: compositor refused the capture", p.meta.name));
            }
            _ => {}
        }
    }

    fn copy(p: &mut Pending, qh: &QueueHandle<App>, shm: &Shm) {
        if p.copying.is_some() {
            return;
        }
        let Some(spec) = p.offers.iter().filter(|s| format_rank(s.format).is_some()).min_by_key(|s| format_rank(s.format)).copied() else {
            log::warn!("{}: no supported shm format offered ({:?})", p.meta.name, p.offers);
            p.frame.destroy();
            return;
        };
        let size = spec.stride as usize * spec.height as usize;
        let mut pool = match RawPool::new(size, shm) {
            Ok(pool) => pool,
            Err(e) => {
                log::error!("shm pool for capture: {e}");
                return;
            }
        };
        let buffer = pool.create_buffer(0, spec.width as i32, spec.height as i32, spec.stride as i32, spec.format, BufferData, qh);
        p.frame.copy(&buffer);
        p.copying = Some((spec, pool, buffer));
    }

    fn read(p: Pending) -> Result<OutputFrame, String> {
        let name = p.meta.name.clone();
        let (spec, mut pool, buffer) = p.copying.ok_or_else(|| format!("{name}: ready without a buffer"))?;
        let image = Image::from_shm(spec.format, spec.width, spec.height, spec.stride, &pool.mmap()[..])
            .ok_or_else(|| format!("{name}: could not decode {:?}", spec.format))?;
        buffer.destroy();
        let logical = (p.meta.logical.w as u32, p.meta.logical.h as u32);
        let image = image.orient(p.y_invert, p.meta.transform, logical, p.meta.mode);
        Ok(OutputFrame { logical: p.meta.logical, image })
    }
}

impl Dispatch2<ZwlrScreencopyFrameV1, App> for FrameData {
    fn event(&self, app: &mut App, frame: &ZwlrScreencopyFrameV1, event: zwlr_screencopy_frame_v1::Event, _: &Connection, qh: &QueueHandle<App>) {
        use wayland_client::Proxy;
        let version = frame.version();
        if let Some(batch) = app.capture.as_mut().filter(|b| b.id == self.batch) {
            batch.event(self.output, event, version, qh, &app.shm);
            if batch.is_complete() {
                app.capture_finished();
            }
        }
    }
}

impl Dispatch2<WlBuffer, App> for BufferData {
    fn event(&self, _: &mut App, _: &WlBuffer, _: wayland_client::protocol::wl_buffer::Event, _: &Connection, _: &QueueHandle<App>) {}
}

impl Dispatch2<ZwlrScreencopyManagerV1, App> for NoEvents {
    fn event(&self, _: &mut App, _: &ZwlrScreencopyManagerV1, _: <ZwlrScreencopyManagerV1 as wayland_client::Proxy>::Event, _: &Connection, _: &QueueHandle<App>) {}
}
