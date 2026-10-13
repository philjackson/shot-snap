//! EGL (via glutin) + femtovg for layer surfaces. One GL context per surface keeps each output's
//! textures and canvas state independent.

use std::ffi::c_void;
use std::num::NonZeroU32;
use std::ptr::NonNull;

use anyhow::{anyhow, Context as _};
use femtovg::renderer::OpenGl;
use femtovg::{Canvas, FontId};
use glutin::config::{Api, ConfigTemplateBuilder, GlConfig};
use glutin::context::{ContextApi, ContextAttributesBuilder, PossiblyCurrentContext, Version};
use glutin::display::{Display, DisplayApiPreference, GlDisplay};
use glutin::prelude::{NotCurrentGlContext, PossiblyCurrentGlContext};
use glutin::surface::{GlSurface as _, Surface, SurfaceAttributesBuilder, SwapInterval, WindowSurface};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy};

pub struct Gl {
    display: Display,
    config: glutin::config::Config,
    font: Option<Vec<u8>>,
}

impl Gl {
    pub fn new(conn: &Connection, font_pattern: &str) -> anyhow::Result<Self> {
        let ptr = NonNull::new(conn.backend().display_ptr() as *mut c_void).ok_or_else(|| anyhow!("no wl_display"))?;
        let raw = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(ptr));
        let display = unsafe { Display::new(raw, DisplayApiPreference::Egl) }.context("creating EGL display")?;
        let template = ConfigTemplateBuilder::new().with_alpha_size(8).with_api(Api::GLES2 | Api::GLES3).build();
        let config = unsafe { display.find_configs(template) }
            .context("querying EGL configs")?
            .filter(|c| c.alpha_size() >= 8)
            .max_by_key(|c| c.api().contains(Api::GLES3))
            .ok_or_else(|| anyhow!("no EGL config with an alpha channel"))?;
        Ok(Self { display, config, font: load_font(font_pattern) })
    }

    pub fn create_surface(&self, wl_surface: &WlSurface, width: u32, height: u32) -> anyhow::Result<GlSurface> {
        let ptr = NonNull::new(wl_surface.id().as_ptr() as *mut c_void).ok_or_else(|| anyhow!("dead wl_surface"))?;
        let raw = RawWindowHandle::Wayland(WaylandWindowHandle::new(ptr));
        let (w, h) = (nz(width), nz(height));
        let attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(raw, w, h);
        let surface = unsafe { self.display.create_window_surface(&self.config, &attrs) }.context("creating EGL surface")?;

        let gles3 = ContextAttributesBuilder::new().with_context_api(ContextApi::Gles(Some(Version::new(3, 0)))).build(Some(raw));
        let gles2 = ContextAttributesBuilder::new().with_context_api(ContextApi::Gles(None)).build(Some(raw));
        let ctx = unsafe { self.display.create_context(&self.config, &gles3) }
            .or_else(|_| unsafe { self.display.create_context(&self.config, &gles2) })
            .context("creating GLES context")?
            .make_current(&surface)
            .context("making GL context current")?;
        // We pace frames ourselves with wl_surface.frame; never block in eglSwapBuffers.
        if let Err(e) = surface.set_swap_interval(&ctx, SwapInterval::DontWait) {
            log::debug!("set_swap_interval: {e}");
        }

        let renderer = unsafe { OpenGl::new_from_function_cstr(|s| self.display.get_proc_address(s).cast()) }
            .map_err(|e| anyhow!("femtovg renderer: {e:?}"))?;
        let mut canvas = Canvas::new(renderer).map_err(|e| anyhow!("femtovg canvas: {e:?}"))?;
        canvas.set_size(width, height, 1.0);
        let font = self.font.as_deref().and_then(|data| canvas.add_font_mem(data).ok());
        Ok(GlSurface { ctx, surface, canvas, font })
    }
}

pub struct GlSurface {
    ctx: PossiblyCurrentContext,
    surface: Surface<WindowSurface>,
    pub canvas: Canvas<OpenGl>,
    pub font: Option<FontId>,
}

impl GlSurface {
    pub fn make_current(&self) -> anyhow::Result<()> {
        self.ctx.make_current(&self.surface).context("make_current")
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.surface.resize(&self.ctx, nz(width), nz(height));
        self.canvas.set_size(width, height, 1.0);
    }

    pub fn swap(&mut self) -> anyhow::Result<()> {
        self.canvas.flush();
        self.surface.swap_buffers(&self.ctx).context("swap_buffers")
    }
}

fn nz(v: u32) -> NonZeroU32 {
    NonZeroU32::new(v.max(1)).unwrap()
}

/// Resolves a fontconfig pattern to a file with `fc-match` and reads it. Text is optional: without
/// a font the overlay still works, it just has no labels.
fn load_font(pattern: &str) -> Option<Vec<u8>> {
    let out = std::process::Command::new("fc-match").args(["-f", "%{file}", pattern]).output().ok()?;
    let path = String::from_utf8(out.stdout).ok()?;
    match std::fs::read(path.trim()) {
        Ok(data) => Some(data),
        Err(e) => {
            log::warn!("could not load font {pattern:?} ({path:?}): {e}");
            None
        }
    }
}
