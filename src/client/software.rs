use std::{num::NonZeroU32, sync::Arc};

use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use winit::{dpi::PhysicalSize, window::Window};

use super::{Frame, Instance, NAME};

const MAX_DIMENSION: u32 = 8192;
const MAX_PIXELS: u64 = 4096 * 4096;

pub(super) fn is_wine() -> bool {
    // SAFETY: Both names are NUL-terminated ASCII. ntdll is already loaded for
    // this process, and the optional address is tested only, never called.
    unsafe {
        let ntdll = GetModuleHandleA(c"ntdll.dll".as_ptr().cast());
        !ntdll.is_null() && GetProcAddress(ntdll, c"wine_get_version".as_ptr().cast()).is_some()
    }
}

pub(super) struct Software {
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    warned_large: bool,
}

impl Software {
    pub(super) fn new(window: Arc<Window>) -> Result<Self, String> {
        let context = softbuffer::Context::new(window.clone())
            .map_err(|err| format!("could not open the window's paint context: {err}"))?;
        let surface = softbuffer::Surface::new(&context, window)
            .map_err(|err| format!("could not create the window's paint surface: {err}"))?;
        Ok(Self {
            surface,
            warned_large: false,
        })
    }

    pub(super) fn draw(
        &mut self,
        window: &Window,
        size: PhysicalSize<u32>,
        [r, g, b]: [u8; 3],
        instances: &[Instance],
    ) -> Result<Frame, String> {
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return Ok(Frame::Hidden);
        };
        if size.width > MAX_DIMENSION
            || size.height > MAX_DIMENSION
            || u64::from(size.width) * u64::from(size.height) > MAX_PIXELS
        {
            if !self.warned_large {
                eprintln!("{NAME}: window is too large to paint; resize to resume drawing");
                self.warned_large = true;
            }
            return Ok(Frame::Hidden);
        }
        self.warned_large = false;
        self.surface
            .resize(width, height)
            .map_err(|err| format!("could not resize the window's paint surface: {err}"))?;
        let mut pixels = self
            .surface
            .buffer_mut()
            .map_err(|err| format!("could not obtain the window's paint buffer: {err}"))?;
        pixels.fill(pack(u32::from(r), u32::from(g), u32::from(b)));
        for instance in instances {
            paint(&mut pixels, size, *instance);
        }
        window.pre_present_notify();
        pixels
            .present()
            .map_err(|err| format!("could not paint the game window: {err}"))?;
        Ok(Frame::Presented)
    }
}

fn pack(r: u32, g: u32, b: u32) -> u32 {
    (r << 16) | (g << 8) | b // softbuffer expects 0x00RRGGBB.
}

fn paint(pixels: &mut [u32], size: PhysicalSize<u32>, instance: Instance) {
    let [x, y, width, height] = instance.rect.map(f64::from);
    let w = f64::from(size.width);
    let h = f64::from(size.height);
    let edge = |pixel: f64, limit: f64| (pixel - 0.5).ceil().clamp(0.0, limit) as usize;
    let end = |pixel: f64| ((pixel - 0.5).floor() + 1.0).clamp(0.0, w) as usize;
    let left_px = (x * 0.5 + 0.5) * w;
    let top_px = (0.5 - y * 0.5) * h;
    let right_px = ((x + width) * 0.5 + 0.5) * w;
    let bottom_px = (0.5 - (y + height) * 0.5) * h;
    let (left, right) = (edge(left_px, w), edge(right_px, w));
    let (top, bottom) = (edge(top_px, h), edge(bottom_px, h));
    let [r, g, b, _] = instance.color.map(|n| (n * 255.0).round() as u32);
    let color = pack(r, g, b);
    let [sw, sh, radius, stroke] = instance.shape.map(f64::from);
    let cx = (left_px + right_px) * 0.5;
    let cy = (top_px + bottom_px) * 0.5;
    let (hx, hy) = (sw * 0.5, sh * 0.5);
    let round_span = |y: f64, hx: f64, hy: f64, radius: f64| {
        let corner = (y - (hy - radius)).max(0.0);
        hx - radius + (radius * radius - corner * corner).max(0.0).sqrt()
    };
    for (iy, row) in pixels
        .chunks_exact_mut(size.width as usize)
        .enumerate()
        .skip(top)
        .take(bottom - top)
    {
        let y = (iy as f64 + 0.5 - cy).abs();
        let (a, b) = if stroke < 0.0 || radius > 0.0 {
            let span = if stroke < 0.0 {
                hx * (1.0 - y / hy)
            } else {
                round_span(y, hx, hy, radius)
            };
            (left.max(edge(cx - span, w)), right.min(end(cx + span)))
        } else {
            (left, right)
        };
        if a >= b {
            continue;
        }
        if stroke > 0.0 && y < hy - stroke && hx > stroke {
            let hole = round_span(y, hx - stroke, hy - stroke, (radius - stroke).max(0.0));
            let c = end(cx - hole).clamp(a, b);
            let d = edge(cx + hole, w).clamp(c, b);
            row[a..c].fill(color);
            row[d..b].fill(color);
        } else {
            row[a..b].fill(color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_rectangles_clip_and_paint_in_scene_order() {
        let size = PhysicalSize::new(4, 4);
        let mut pixels = [0; 16];
        paint(
            &mut pixels,
            size,
            Instance {
                rect: [-2.0, 1.0, 2.0, -1.0],
                color: [1.0, 0.0, 0.0, 1.0],
                shape: [4.0, 2.0, 0.0, 0.0],
            },
        );
        paint(
            &mut pixels,
            size,
            Instance {
                rect: [-0.5, 0.5, 2.0, -2.0],
                color: [0.0, 0.0, 1.0, 1.0],
                shape: [4.0, 4.0, 0.0, 0.0],
            },
        );
        assert_eq!(
            pixels.as_chunks::<4>().0,
            &[
                [0xff0000, 0xff0000, 0, 0],
                [0xff0000, 0x0000ff, 0x0000ff, 0x0000ff],
                [0, 0x0000ff, 0x0000ff, 0x0000ff],
                [0, 0x0000ff, 0x0000ff, 0x0000ff],
            ]
        );
    }

    #[test]
    fn rounded_outline_leaves_corners_and_middle_untouched() {
        let size = PhysicalSize::new(6, 6);
        let mut pixels = [0; 36];
        paint(
            &mut pixels,
            size,
            Instance {
                rect: [-1.0, 1.0, 2.0, -2.0],
                color: [1.0; 4],
                shape: [6.0, 6.0, 2.0, 1.0],
            },
        );
        assert_eq!(pixels[0], 0);
        assert_eq!(pixels[3], 0xffffff);
        assert_eq!(pixels[3 * 6 + 3], 0);
        assert_eq!(pixels[5 * 6 + 3], 0xffffff);
    }
}
