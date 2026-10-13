//! The daemon owns the clipboard selection through data-control (ext, else wlr), which works
//! without keyboard focus and keeps serving the PNG after the overlay is gone.

use std::io::Write;
use std::os::fd::OwnedFd;
use std::sync::Arc;

use smithay_client_toolkit::dispatch2::Dispatch2;
use wayland_client::backend::ObjectData;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Proxy, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::ExtDataControlManagerV1,
    ext_data_control_offer_v1::ExtDataControlOfferV1,
    ext_data_control_source_v1::{self, ExtDataControlSourceV1},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};

use crate::app::App;
use crate::capture::NoEvents;

const MIME: &str = "image/png";

enum Manager {
    Ext(ExtDataControlManagerV1),
    Wlr(ZwlrDataControlManagerV1),
}

enum Device {
    Ext(ExtDataControlDeviceV1),
    Wlr(ZwlrDataControlDeviceV1),
}

enum Source {
    Ext(ExtDataControlSourceV1),
    Wlr(ZwlrDataControlSourceV1),
}

impl Source {
    fn destroy(&self) {
        match self {
            Source::Ext(s) => s.destroy(),
            Source::Wlr(s) => s.destroy(),
        }
    }
}

pub struct Clipboard {
    manager: Option<Manager>,
    device: Option<Device>,
    source: Option<(Source, Arc<Vec<u8>>)>,
}

pub struct DeviceData;
pub struct SourceData;
pub struct OfferData;

impl Clipboard {
    pub fn bind(globals: &GlobalList, qh: &QueueHandle<App>) -> Self {
        let manager = globals
            .bind::<ExtDataControlManagerV1, _, _>(qh, 1..=1, NoEvents)
            .map(Manager::Ext)
            .or_else(|_| globals.bind::<ZwlrDataControlManagerV1, _, _>(qh, 1..=2, NoEvents).map(Manager::Wlr))
            .ok();
        if manager.is_none() {
            log::warn!("compositor has no data-control protocol; copying to the clipboard is unavailable");
        }
        Self { manager, device: None, source: None }
    }

    pub fn available(&self) -> bool {
        self.manager.is_some()
    }

    pub fn set(&mut self, png: Arc<Vec<u8>>, seat: &WlSeat, qh: &QueueHandle<App>) -> anyhow::Result<()> {
        let manager = self.manager.as_ref().ok_or_else(|| anyhow::anyhow!("clipboard unavailable: compositor has no data-control protocol"))?;
        if self.device.is_none() {
            self.device = Some(match manager {
                Manager::Ext(m) => Device::Ext(m.get_data_device(seat, qh, DeviceData)),
                Manager::Wlr(m) => Device::Wlr(m.get_data_device(seat, qh, DeviceData)),
            });
        }
        let source = match manager {
            Manager::Ext(m) => {
                let s = m.create_data_source(qh, SourceData);
                s.offer(MIME.into());
                Source::Ext(s)
            }
            Manager::Wlr(m) => {
                let s = m.create_data_source(qh, SourceData);
                s.offer(MIME.into());
                Source::Wlr(s)
            }
        };
        match (self.device.as_ref().unwrap(), &source) {
            (Device::Ext(d), Source::Ext(s)) => d.set_selection(Some(s)),
            (Device::Wlr(d), Source::Wlr(s)) => d.set_selection(Some(s)),
            _ => unreachable!("device and source come from the same manager"),
        }
        if let Some((old, _)) = self.source.replace((source, png)) {
            old.destroy();
        }
        Ok(())
    }

    fn send(&self, source_id: &wayland_client::backend::ObjectId, mime: &str, fd: OwnedFd) {
        let Some((source, data)) = &self.source else { return };
        let current = match source {
            Source::Ext(s) => s.id(),
            Source::Wlr(s) => s.id(),
        };
        if &current != source_id || mime != MIME {
            return;
        }
        let data = data.clone();
        // Write off-thread: a slow reader must not stall the event loop.
        std::thread::spawn(move || {
            let mut file = std::fs::File::from(fd);
            if let Err(e) = file.write_all(&data) {
                log::debug!("clipboard transfer ended early: {e}");
            }
        });
    }

    fn cancelled(&mut self, source_id: &wayland_client::backend::ObjectId) {
        let is_current = matches!(&self.source, Some((Source::Ext(s), _)) if &s.id() == source_id)
            || matches!(&self.source, Some((Source::Wlr(s), _)) if &s.id() == source_id);
        if is_current
            && let Some((s, _)) = self.source.take() {
                s.destroy();
            }
    }
}

impl Dispatch2<ExtDataControlManagerV1, App> for NoEvents {
    fn event(&self, _: &mut App, _: &ExtDataControlManagerV1, _: <ExtDataControlManagerV1 as Proxy>::Event, _: &Connection, _: &QueueHandle<App>) {}
}

impl Dispatch2<ZwlrDataControlManagerV1, App> for NoEvents {
    fn event(&self, _: &mut App, _: &ZwlrDataControlManagerV1, _: <ZwlrDataControlManagerV1 as Proxy>::Event, _: &Connection, _: &QueueHandle<App>) {}
}

// Other clients' selections arrive as offers; we never read them, so drop them as soon as
// they're replaced.
impl Dispatch2<ExtDataControlDeviceV1, App> for DeviceData {
    fn event(&self, _: &mut App, _: &ExtDataControlDeviceV1, event: ext_data_control_device_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        use ext_data_control_device_v1::Event;
        match event {
            Event::Selection { id: Some(offer) } | Event::PrimarySelection { id: Some(offer) } => offer.destroy(),
            _ => {}
        }
    }

    fn event_created_child(opcode: u16, qh: &QueueHandle<App>) -> Arc<dyn ObjectData> {
        assert_eq!(opcode, ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE);
        qh.make_data::<ExtDataControlOfferV1, _>(OfferData)
    }
}

impl Dispatch2<ZwlrDataControlDeviceV1, App> for DeviceData {
    fn event(&self, _: &mut App, _: &ZwlrDataControlDeviceV1, event: zwlr_data_control_device_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        use zwlr_data_control_device_v1::Event;
        match event {
            Event::Selection { id: Some(offer) } | Event::PrimarySelection { id: Some(offer) } => offer.destroy(),
            _ => {}
        }
    }

    fn event_created_child(opcode: u16, qh: &QueueHandle<App>) -> Arc<dyn ObjectData> {
        assert_eq!(opcode, zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE);
        qh.make_data::<ZwlrDataControlOfferV1, _>(OfferData)
    }
}

impl Dispatch2<ExtDataControlOfferV1, App> for OfferData {
    fn event(&self, _: &mut App, _: &ExtDataControlOfferV1, _: <ExtDataControlOfferV1 as Proxy>::Event, _: &Connection, _: &QueueHandle<App>) {}
}

impl Dispatch2<ZwlrDataControlOfferV1, App> for OfferData {
    fn event(&self, _: &mut App, _: &ZwlrDataControlOfferV1, _: <ZwlrDataControlOfferV1 as Proxy>::Event, _: &Connection, _: &QueueHandle<App>) {}
}

impl Dispatch2<ExtDataControlSourceV1, App> for SourceData {
    fn event(&self, app: &mut App, source: &ExtDataControlSourceV1, event: ext_data_control_source_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        use ext_data_control_source_v1::Event;
        match event {
            Event::Send { mime_type, fd } => app.clipboard.send(&source.id(), &mime_type, fd),
            Event::Cancelled => app.clipboard.cancelled(&source.id()),
            _ => {}
        }
    }
}

impl Dispatch2<ZwlrDataControlSourceV1, App> for SourceData {
    fn event(&self, app: &mut App, source: &ZwlrDataControlSourceV1, event: zwlr_data_control_source_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        use zwlr_data_control_source_v1::Event;
        match event {
            Event::Send { mime_type, fd } => app.clipboard.send(&source.id(), &mime_type, fd),
            Event::Cancelled => app.clipboard.cancelled(&source.id()),
            _ => {}
        }
    }
}
