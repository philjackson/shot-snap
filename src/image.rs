use wayland_client::protocol::{wl_output::Transform, wl_shm::Format};

use crate::geom::Rect;

/// Top-down, straight-alpha RGBA8 image.
#[derive(Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Image({}x{})", self.width, self.height)
    }
}

/// A captured output: its pixels in logical orientation plus where it sits in the desktop.
#[derive(Debug, Clone)]
pub struct OutputFrame {
    pub logical: Rect,
    pub image: Image,
}

impl OutputFrame {
    /// Physical pixels per logical pixel.
    pub fn scale(&self) -> f64 {
        f64::from(self.image.width) / self.logical.w
    }
}

/// Order of preference when the compositor offers several shm formats.
pub fn format_rank(format: Format) -> Option<u8> {
    use Format::*;
    Some(match format {
        Xrgb8888 | Argb8888 => 0,
        Xbgr8888 | Abgr8888 => 1,
        Xrgb2101010 | Argb2101010 | Xbgr2101010 | Abgr2101010 => 2,
        Bgr888 | Rgb888 => 3,
        _ => return None,
    })
}

pub fn bytes_per_pixel(format: Format) -> u32 {
    match format {
        Format::Bgr888 | Format::Rgb888 => 3,
        _ => 4,
    }
}

impl Image {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0; width as usize * height as usize * 4] }
    }

    /// Converts a mapped shm buffer (little-endian DRM fourcc layout) to RGBA8.
    pub fn from_shm(format: Format, width: u32, height: u32, stride: u32, src: &[u8]) -> Option<Self> {
        format_rank(format)?;
        let bpp = bytes_per_pixel(format) as usize;
        let mut out = Self::new(width, height);
        for y in 0..height as usize {
            let row = src.get(y * stride as usize..y * stride as usize + width as usize * bpp)?;
            let dst = &mut out.data[y * width as usize * 4..(y + 1) * width as usize * 4];
            for (px, d) in row.chunks_exact(bpp).zip(dst.as_chunks_mut::<4>().0) {
                let rgba = match format {
                    // Memory order B, G, R, A.
                    Format::Xrgb8888 => [px[2], px[1], px[0], 255],
                    Format::Argb8888 => [px[2], px[1], px[0], px[3]],
                    // Memory order R, G, B, A.
                    Format::Xbgr8888 => [px[0], px[1], px[2], 255],
                    Format::Abgr8888 => [px[0], px[1], px[2], px[3]],
                    // Memory order B, G, R per the fourcc, but wlroots and others put R first in
                    // practice for 24-bit (see Noctalia's screencopy_util); treat both as RGB.
                    Format::Bgr888 | Format::Rgb888 => [px[0], px[1], px[2], 255],
                    _ => {
                        let v = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                        let c = |shift: u32| ((v >> shift) & 0x3ff) as u16;
                        let to8 = |c: u16| (c >> 2) as u8;
                        let a2 = ((v >> 30) & 0x3) as u8 * 85;
                        match format {
                            Format::Xrgb2101010 => [to8(c(20)), to8(c(10)), to8(c(0)), 255],
                            Format::Argb2101010 => [to8(c(20)), to8(c(10)), to8(c(0)), a2],
                            Format::Xbgr2101010 => [to8(c(0)), to8(c(10)), to8(c(20)), 255],
                            Format::Abgr2101010 => [to8(c(0)), to8(c(10)), to8(c(20)), a2],
                            _ => unreachable!("filtered by format_rank"),
                        }
                    }
                };
                d.copy_from_slice(&rgba);
            }
        }
        Some(out)
    }

    fn px(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        self.data[i..i + 4].try_into().unwrap()
    }

    /// Builds a new image of `w`x`h` where each destination pixel reads `src(map(x, y))`.
    fn remap(&self, w: u32, h: u32, map: impl Fn(u32, u32) -> (u32, u32)) -> Image {
        let mut out = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = map(x, y);
                let i = (y as usize * w as usize + x as usize) * 4;
                out.data[i..i + 4].copy_from_slice(&self.px(sx, sy));
            }
        }
        out
    }

    pub fn flip_vertical(&self) -> Image {
        let row = self.width as usize * 4;
        let mut data = Vec::with_capacity(self.data.len());
        for chunk in self.data.chunks_exact(row).rev() {
            data.extend_from_slice(chunk);
        }
        Image { data, ..*self }
    }

    fn flip_horizontal(&self) -> Image {
        let w = self.width;
        self.remap(w, self.height, |x, y| (w - 1 - x, y))
    }

    fn rotate_cw90(&self) -> Image {
        let h = self.height;
        self.remap(self.height, self.width, |x, y| (y, h - 1 - x))
    }

    fn rotate_180(&self) -> Image {
        let (w, h) = (self.width, self.height);
        self.remap(w, h, |x, y| (w - 1 - x, h - 1 - y))
    }

    fn rotate_cw270(&self) -> Image {
        let w = self.width;
        self.remap(self.height, self.width, |x, y| (w - 1 - y, x))
    }

    /// Turns a raw screencopy buffer into logical orientation. Mirrors Noctalia's
    /// `orientCaptureToLogical`: some compositors already hand back a transformed buffer, which we
    /// detect by its size matching the logical size but not the mode size.
    pub fn orient(mut self, y_invert: bool, transform: Transform, logical: (u32, u32), mode: Option<(u32, u32)>) -> Image {
        if y_invert {
            self = self.flip_vertical();
        }
        if transform == Transform::Normal {
            return self;
        }
        let dims = (self.width, self.height);
        if dims == logical && mode != Some(dims) {
            return self;
        }
        match transform {
            Transform::_90 => self.rotate_cw90(),
            Transform::_180 => self.rotate_180(),
            Transform::_270 => self.rotate_cw270(),
            Transform::Flipped => self.flip_horizontal(),
            Transform::Flipped90 => self.flip_horizontal().rotate_cw90(),
            Transform::Flipped180 => self.flip_vertical(),
            Transform::Flipped270 => self.flip_horizontal().rotate_cw270(),
            _ => self,
        }
    }

    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> Image {
        let x = x.min(self.width);
        let y = y.min(self.height);
        let w = w.min(self.width - x);
        let h = h.min(self.height - y);
        let mut out = Image::new(w, h);
        let row = w as usize * 4;
        for r in 0..h as usize {
            let src = ((y as usize + r) * self.width as usize + x as usize) * 4;
            out.data[r * row..(r + 1) * row].copy_from_slice(&self.data[src..src + row]);
        }
        out
    }

    /// Bilinear resample. Only used when stitching outputs with different scales.
    pub fn resize(&self, w: u32, h: u32) -> Image {
        if (w, h) == (self.width, self.height) || self.width == 0 || self.height == 0 {
            return self.clone();
        }
        let mut out = Image::new(w, h);
        let sx = f64::from(self.width) / f64::from(w);
        let sy = f64::from(self.height) / f64::from(h);
        for y in 0..h {
            let fy = ((f64::from(y) + 0.5) * sy - 0.5).clamp(0.0, f64::from(self.height - 1));
            let (y0, ty) = (fy.floor() as u32, fy.fract());
            let y1 = (y0 + 1).min(self.height - 1);
            for x in 0..w {
                let fx = ((f64::from(x) + 0.5) * sx - 0.5).clamp(0.0, f64::from(self.width - 1));
                let (x0, tx) = (fx.floor() as u32, fx.fract());
                let x1 = (x0 + 1).min(self.width - 1);
                let (a, b, c, d) = (self.px(x0, y0), self.px(x1, y0), self.px(x0, y1), self.px(x1, y1));
                let i = (y as usize * w as usize + x as usize) * 4;
                for ch in 0..4 {
                    let top = f64::from(a[ch]) * (1.0 - tx) + f64::from(b[ch]) * tx;
                    let bottom = f64::from(c[ch]) * (1.0 - tx) + f64::from(d[ch]) * tx;
                    out.data[i + ch] = (top * (1.0 - ty) + bottom * ty).round() as u8;
                }
            }
        }
        out
    }

    fn blit(&mut self, src: &Image, x: u32, y: u32) {
        let w = src.width.min(self.width.saturating_sub(x)) as usize;
        for r in 0..src.height.min(self.height.saturating_sub(y)) as usize {
            let s = r * src.width as usize * 4;
            let d = ((y as usize + r) * self.width as usize + x as usize) * 4;
            self.data[d..d + w * 4].copy_from_slice(&src.data[s..s + w * 4]);
        }
    }
}

/// Cuts `area` (global logical coords) out of the captured outputs. The result uses the highest
/// scale among the outputs it touches, so a single-output crop is an exact pixel copy and
/// lower-density outputs are upscaled to match. Gaps between outputs stay transparent.
pub fn compose(area: Rect, frames: &[OutputFrame]) -> Option<Image> {
    let parts: Vec<(&OutputFrame, Rect)> =
        frames.iter().filter_map(|f| f.logical.intersect(&area).map(|part| (f, part))).collect();
    let scale = parts.iter().map(|(f, _)| f.scale()).fold(0.0, f64::max);
    if parts.is_empty() || scale <= 0.0 {
        return None;
    }
    let out_w = (area.w * scale).round().max(1.0) as u32;
    let out_h = (area.h * scale).round().max(1.0) as u32;
    let mut out = Image::new(out_w, out_h);
    for (frame, part) in parts {
        let fs = frame.scale();
        let px = |v: f64, s: f64| (v * s).round().max(0.0) as u32;
        let (sx, sy) = (px(part.x - frame.logical.x, fs), px(part.y - frame.logical.y, fs));
        let (sw, sh) = (px(part.right() - frame.logical.x, fs) - sx, px(part.bottom() - frame.logical.y, fs) - sy);
        let (dx, dy) = (px(part.x - area.x, scale), px(part.y - area.y, scale));
        let (dw, dh) = (px(part.right() - area.x, scale) - dx, px(part.bottom() - area.y, scale) - dy);
        if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
            continue;
        }
        let piece = frame.image.crop(sx, sy, sw, sh).resize(dw, dh);
        out.blit(&piece, dx, dy);
    }
    Some(out)
}

pub fn encode_png(image: &Image) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(image.data.len() / 3);
    let mut enc = png::Encoder::new(&mut buf, image.width, image.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut writer = enc.write_header()?;
    writer.write_image_data(&image.data)?;
    writer.finish()?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        for i in 0..(w * h) as usize {
            img.data[i * 4] = i as u8;
            img.data[i * 4 + 3] = 255;
        }
        img
    }

    fn reds(img: &Image) -> Vec<u8> {
        img.data.chunks_exact(4).map(|p| p[0]).collect()
    }

    #[test]
    fn rotations() {
        // 0 1 2
        // 3 4 5
        let img = numbered(3, 2);
        assert_eq!(reds(&img.rotate_cw90()), vec![3, 0, 4, 1, 5, 2]);
        assert_eq!(reds(&img.rotate_180()), vec![5, 4, 3, 2, 1, 0]);
        assert_eq!(reds(&img.rotate_cw270()), vec![2, 5, 1, 4, 0, 3]);
        assert_eq!(reds(&img.flip_horizontal()), vec![2, 1, 0, 5, 4, 3]);
        assert_eq!(reds(&img.flip_vertical()), vec![3, 4, 5, 0, 1, 2]);
    }

    #[test]
    fn shm_formats() {
        let px = [10u8, 20, 30, 40];
        let img = Image::from_shm(Format::Argb8888, 1, 1, 4, &px).unwrap();
        assert_eq!(img.data, vec![30, 20, 10, 40]);
        let img = Image::from_shm(Format::Xbgr8888, 1, 1, 4, &px).unwrap();
        assert_eq!(img.data, vec![10, 20, 30, 255]);
        let white = (0x3ffu32 << 20 | 0x3ff << 10 | 0x3ff).to_le_bytes();
        let img = Image::from_shm(Format::Xrgb2101010, 1, 1, 4, &white).unwrap();
        assert_eq!(img.data, vec![255, 255, 255, 255]);
    }

    #[test]
    fn compose_single_output_is_exact() {
        let frame = OutputFrame { logical: Rect { x: 100.0, y: 0.0, w: 3.0, h: 2.0 }, image: numbered(6, 4) };
        let out = compose(Rect { x: 101.0, y: 1.0, w: 1.0, h: 1.0 }, &[frame]).unwrap();
        assert_eq!((out.width, out.height), (2, 2));
        assert_eq!(reds(&out), vec![14, 15, 20, 21]);
    }

    #[test]
    fn compose_spans_outputs_at_max_scale() {
        let a = OutputFrame { logical: Rect { x: 0.0, y: 0.0, w: 2.0, h: 2.0 }, image: numbered(2, 2) };
        let b = OutputFrame { logical: Rect { x: 2.0, y: 0.0, w: 2.0, h: 2.0 }, image: numbered(4, 4) };
        let out = compose(Rect { x: 1.0, y: 0.0, w: 2.0, h: 2.0 }, &[a, b]).unwrap();
        assert_eq!((out.width, out.height), (4, 4));
    }
}
