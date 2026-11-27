use std::{f64::consts::SQRT_2, num::NonZeroU32, ops::Range, sync::Arc};

use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use winit::{dpi::PhysicalSize, window::Window};

use super::{Frame, Instance, NAME};

const MAX_DIMENSION: u32 = 8192;
const MAX_PIXELS: u64 = 4096 * 4096;
const AA_RADIUS: f64 = 0.5; // Physical pixels, the same filter as the GPU.

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

#[derive(Clone, Copy)]
struct Shape {
    bounds: [f64; 4],
    radius: f64,
    diamond_half: Option<f64>,
}

impl Shape {
    fn inset(self, stroke: f64) -> Option<Self> {
        let [l, t, r, b] = self.bounds;
        let bounds = [l + stroke, t + stroke, r - stroke, b - stroke];
        (stroke > 0.0 && bounds[0] < bounds[2] && bounds[1] < bounds[3]).then_some(Self {
            bounds,
            radius: (self.radius - stroke).max(0.0),
            diamond_half: None,
        })
    }

    // Scanline of the expanded (+0.5, may contribute) or eroded (-0.5, surely
    // opaque) shape. The interval is clipped before indexing the paint buffer.
    fn span(self, y: f64, spread: f64, width: f64) -> Range<usize> {
        let [l, t, r, b] = self.bounds;
        let vertical = (y - t).min(b - y);
        if vertical + spread < 0.0 {
            return 0..0;
        }
        let rad = self.radius + spread;
        let inset = if let Some(half) = self.diamond_half {
            (half - vertical - spread * SQRT_2).max(-spread)
        } else if vertical < self.radius && self.radius > 0.0 && rad > 0.0 {
            let dy = self.radius - vertical;
            let dx = ((rad - dy).max(0.0) * (rad + dy)).sqrt();
            // rad - dx, without cancelling away a small offset on a large curve.
            dy * dy / (rad + dx) - spread
        } else {
            -spread
        };
        let start = (l + inset - AA_RADIUS).ceil().clamp(0.0, width) as usize;
        let end = ((r - inset - AA_RADIUS).floor() + 1.0).clamp(0.0, width) as usize;
        start..end.max(start)
    }

    fn coverage(self, x: f64, y: f64) -> f64 {
        let [l, t, r, b] = self.bounds;
        let overlap = |p: f64, start: f64, end: f64| {
            ((p + AA_RADIUS).min(end) - (p - AA_RADIUS).max(start)).max(0.0)
        };
        let rect = overlap(x, l, r) * overlap(y, t, b);
        if rect == 0.0 {
            return 0.0;
        }
        let nx = (x - l).min(r - x);
        let ny = (y - t).min(b - y);
        let distance = if let Some(half) = self.diamond_half {
            (half - nx - ny) / SQRT_2
        } else if nx < self.radius && ny < self.radius && self.radius > 0.0 {
            let (dx, dy) = (self.radius - nx, self.radius - ny);
            (dx * dx + dy * dy).sqrt() - self.radius
        } else {
            return rect;
        };
        rect.min((AA_RADIUS - distance).clamp(0.0, 1.0))
    }
}

fn blend(dst: u32, src: u32, coverage: f64) -> u32 {
    let alpha = (coverage * 255.0).round() as u32;
    let channel = |shift: u32| {
        let (src, dst) = ((src >> shift) & 255, (dst >> shift) & 255);
        (src * alpha + dst * (255 - alpha) + 127) / 255
    };
    pack(channel(16), channel(8), channel(0))
}

fn paint(pixels: &mut [u32], size: PhysicalSize<u32>, instance: Instance) {
    let [l, t, r, b] = instance.rect.map(f64::from);
    let [w, h] = [f64::from(size.width), f64::from(size.height)];
    let bounds = [
        (0.5 + l * 0.5) * w,
        (0.5 - t * 0.5) * h,
        (0.5 + r * 0.5) * w,
        (0.5 - b * 0.5) * h,
    ];
    let top = bounds[1].floor().clamp(0.0, h) as usize;
    let bottom = bounds[3].ceil().clamp(0.0, h) as usize;
    let [r, g, b, _] = instance.color.map(|n| (n * 255.0).round() as u32);
    let color = pack(r, g, b);
    let [sw, _, radius, stroke] = instance.shape.map(f64::from);
    let outer = Shape {
        bounds,
        radius,
        diamond_half: (stroke < 0.0).then_some(sw * 0.5),
    };
    let inner = outer.inset(stroke);
    for (iy, row) in pixels
        .chunks_exact_mut(size.width as usize)
        .enumerate()
        .take(bottom)
        .skip(top)
    {
        let y = iy as f64 + 0.5;
        let visible = outer.span(y, AA_RADIUS, w);
        if visible.is_empty() {
            continue;
        }
        let solid = outer.span(y, -AA_RADIUS, w);
        let (hole_edge, hole) = inner.map_or((0..0, 0..0), |shape| {
            (shape.span(y, AA_RADIUS, w), shape.span(y, -AA_RADIUS, w))
        });
        let paint_edge = |left: usize, pixels: &mut [u32]| {
            for (i, pixel) in pixels.iter_mut().enumerate() {
                if *pixel == color {
                    continue;
                }
                let x = (left + i) as f64 + 0.5;
                let coverage = outer.coverage(x, y) - inner.map_or(0.0, |s| s.coverage(x, y));
                if coverage > 0.0 {
                    *pixel = blend(*pixel, color, coverage);
                }
            }
        };
        // Fast bulk writes for opaque interiors, and skip fully empty holes.
        // Only the bands between them require per-pixel coverage.
        let bands = [
            (solid.start..solid.end.min(hole_edge.start), true),
            (hole, false),
            (solid.start.max(hole_edge.end)..solid.end, true),
        ];
        let mut cursor = visible.start;
        for (band, opaque) in bands {
            let a = band.start.max(cursor);
            let b = band.end.min(visible.end);
            if a < b {
                paint_edge(cursor, &mut row[cursor..a]);
                if opaque {
                    row[a..b].fill(color);
                }
                cursor = b;
            }
        }
        paint_edge(cursor, &mut row[cursor..visible.end]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Painter;

    fn draw(pixels: &mut [u32], size: PhysicalSize<u32>, shapes: impl FnOnce(&mut Painter<'_>)) {
        let mut instances = Vec::new();
        shapes(&mut Painter {
            output: &mut instances,
            size,
            limit: 8,
        });
        for instance in instances {
            paint(pixels, size, instance);
        }
    }

    #[test]
    fn software_rectangles_clip_and_paint_in_scene_order() {
        let size = PhysicalSize::new(4, 4);
        let mut pixels = [0; 16];
        draw(&mut pixels, size, |p| {
            p.rect(-2.0, 0.0, 4.0, 2.0, [255, 0, 0]);
            p.rect(1.0, 1.0, 4.0, 4.0, [0, 0, 255]);
        });
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
        let size = PhysicalSize::new(8, 8);
        let mut pixels = [0; 64];
        draw(&mut pixels, size, |p| {
            p.outline(0.5, 0.5, 7.0, 7.0, 2.5, 1.0, [251, 251, 249]);
        });
        assert_eq!(pixels[0], 0);
        assert_eq!(pixels[4], 0xfbfbf9);
        assert_eq!(pixels[4 * 8 + 4], 0);
        assert_eq!(pixels[7 * 8 + 4], 0xfbfbf9);
        assert!((1..0xfbfbf9).contains(&pixels[1]));
        assert!((1..0xfbfbf9).contains(&pixels[8 + 1]));
    }

    #[test]
    fn fractional_rects_and_thin_ghost_strokes_blend_against_the_scene() {
        let size = PhysicalSize::new(8, 8);
        let mut pixels = [0x202020; 64];
        draw(&mut pixels, size, |p| {
            p.rect(0.25, 7.0, 1.75, 1.0, [255; 3]);
            p.outline(1.25, 1.0, 5.0, 5.0, 0.0, 0.25, [199, 199, 198]);
        });
        assert_eq!(pixels[7 * 8], 0xc7c7c7); // 75% white over the prior layer.
        assert_eq!(pixels[7 * 8 + 1], 0xffffff);
        assert_eq!(pixels[3 * 8 + 1], 0x4a4a4a); // 1/4px centered on a pixel.
        assert_eq!(pixels[3], 0x353535); // Half of the same stroke on either row.
        assert_eq!(pixels[8 + 3], 0x353535);
        assert_eq!(pixels[3 * 8 + 3], 0x202020);
    }

    #[test]
    fn diamond_has_intermediate_edges_and_exact_body_color() {
        let size = PhysicalSize::new(8, 8);
        let mut pixels = [0; 64];
        draw(&mut pixels, size, |p| {
            p.diamond([4.0, 4.0], 3.0, [235, 237, 241]);
        });
        assert_eq!(pixels[0], 0);
        assert!((1..0xebedf1).contains(&pixels[2 * 8 + 2]));
        assert_eq!(pixels[3 * 8 + 3], 0xebedf1);
    }

    #[test]
    fn very_large_offscreen_rect_keeps_its_visible_fractional_edge() {
        let size = PhysicalSize::new(8192, 2);
        let mut pixels = vec![0; 8192 * 2];
        let offset = (1_u64 << 34) as f64;
        draw(&mut pixels, size, |p| {
            p.rect(-offset, -offset, offset + 3.25, offset + 20.0, [255; 3]);
        });
        for row in pixels.chunks(8192) {
            assert_eq!(&row[..5], &[0xffffff, 0xffffff, 0xffffff, 0x404040, 0]);
            assert_eq!(row.last(), Some(&0));
        }
    }
}
