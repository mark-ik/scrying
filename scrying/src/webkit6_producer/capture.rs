//! CPU snapshot via `webkit_web_view_get_snapshot` →
//! [`gdk::Texture::download`].
//!
//! GTK 4's snapshot pipeline returns a `gdk::Texture` (vs cairo's
//! `ImageSurface` on GTK 3). `Texture::download(buf, stride)` fills
//! a buffer with Cairo-ARGB32-format pixels (BGRA premultiplied on
//! little-endian) — same as the GTK 3 producer's surface bytes —
//! so the un-premultiplied RGBA conversion is identical.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use dpi::PhysicalSize;
use webkit6::gdk;
use webkit6::gdk::prelude::*;
use webkit6::gio;
use webkit6::prelude::*;
use webkit6::{SnapshotOptions, SnapshotRegion};

use crate::{WebSurfaceError, WebSurfaceFrame};

use super::helpers::pump_until;
use super::producer::WebKit6Producer;

impl WebKit6Producer {
    pub fn capture_cpu_snapshot(&self) -> Result<WebSurfaceFrame, WebSurfaceError> {
        let timeout = std::time::Duration::from_secs(2);
        let result: Rc<RefCell<Option<Result<gdk::Texture, String>>>> = Rc::new(RefCell::new(None));
        let r = result.clone();
        self.webview.snapshot(
            SnapshotRegion::Visible,
            SnapshotOptions::empty(),
            gio::Cancellable::NONE,
            move |res| {
                *r.borrow_mut() = Some(res.map_err(|e| e.to_string()));
            },
        );

        let deadline = Instant::now() + timeout;
        pump_until(deadline, || result.borrow().is_some())?;
        let texture = result
            .borrow_mut()
            .take()
            .ok_or(WebSurfaceError::NotReady(
                "WebKitGTK 6 snapshot did not deliver in time",
            ))?
            .map_err(|e| WebSurfaceError::Platform(format!("snapshot failed: {e}")))?;

        let width = texture.width().max(0) as u32;
        let height = texture.height().max(0) as u32;
        let stride = (width as usize) * 4;
        let mut buf = vec![0u8; stride * (height as usize)];
        // `Texture::download` writes Cairo-ARGB32 format — BGRA
        // premultiplied on little-endian, identical to the GTK 3
        // producer's `ImageSurface::data()`.
        texture.download(&mut buf, stride);

        let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);
        for y in 0..height as usize {
            let row_start = y * stride;
            for x in 0..width as usize {
                let px = row_start + x * 4;
                let b = buf[px] as u32;
                let g = buf[px + 1] as u32;
                let r = buf[px + 2] as u32;
                let a = buf[px + 3] as u32;
                let (r8, g8, b8) = if a == 0 {
                    (0u8, 0u8, 0u8)
                } else if a == 255 {
                    (r as u8, g as u8, b as u8)
                } else {
                    (
                        unpremultiply(r, a),
                        unpremultiply(g, a),
                        unpremultiply(b, a),
                    )
                };
                rgba.extend_from_slice(&[r8, g8, b8, a as u8]);
            }
        }

        let pixels = image::RgbaImage::from_raw(width, height, rgba).ok_or_else(|| {
            WebSurfaceError::Platform("failed to construct RgbaImage from snapshot bytes".into())
        })?;
        Ok(WebSurfaceFrame::CpuRgba {
            size: PhysicalSize::new(width, height),
            pixels,
            generation: self.next_generation(),
        })
    }

    /// Take a snapshot and encode it as a PNG.
    pub fn capture_snapshot_png(&self) -> Result<Vec<u8>, WebSurfaceError> {
        let frame = self.capture_cpu_snapshot()?;
        let pixels = match frame {
            WebSurfaceFrame::CpuRgba { pixels, .. } => pixels,
            _ => {
                return Err(WebSurfaceError::Platform(
                    "capture_cpu_snapshot returned an unexpected frame variant".into(),
                ));
            }
        };
        let mut buf: Vec<u8> = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut buf);
        pixels
            .write_to(&mut cursor, image::ImageFormat::Png)
            .map_err(|e| WebSurfaceError::Platform(format!("PNG encode failed: {e}")))?;
        Ok(buf)
    }
}

#[inline]
fn unpremultiply(channel: u32, alpha: u32) -> u8 {
    (((channel * 255) + (alpha / 2)) / alpha).min(255) as u8
}
