use std::{num::NonZeroU32, sync::Arc};

use _8times8::Game;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use winit::{dpi::PhysicalSize, window::Window};

use super::{Camera, Frame, Hit, Instance, NAME, Palette, board_instances};

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
    palette: Palette,
    instances: Vec<Instance>,
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
            palette: Palette::new(wgpu::TextureFormat::Bgra8Unorm),
            instances: Vec::with_capacity(128),
            warned_large: false,
        })
    }

    pub(super) fn draw(
        &mut self,
        window: &Window,
        size: PhysicalSize<u32>,
        camera: Camera,
        game: &Game,
        hover: Option<Hit>,
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
        board_instances(
            &mut self.instances,
            &self.palette,
            size,
            camera,
            game,
            hover,
        );
        let bg = self.palette.background;
        pixels.fill(pack([bg.r as f32, bg.g as f32, bg.b as f32, 1.0]));
        for instance in &self.instances {
            paint(&mut pixels, size, *instance);
        }
        window.pre_present_notify();
        pixels
            .present()
            .map_err(|err| format!("could not paint the game window: {err}"))?;
        Ok(Frame::Presented)
    }
}

fn pack([r, g, b, _]: [f32; 4]) -> u32 {
    let [r, g, b] = [r, g, b].map(|value| (value * 255.0).round() as u32);
    (r << 16) | (g << 8) | b // softbuffer expects 0x00RRGGBB.
}

fn paint(pixels: &mut [u32], size: PhysicalSize<u32>, instance: Instance) {
    let [x, y, width, height] = instance.rect.map(f64::from);
    let w = f64::from(size.width);
    let h = f64::from(size.height);
    // Convert from clip coordinates to clipped pixel ranges, sampling at pixel centers.
    let edge =
        |ndc: f64, limit: f64| ((ndc * 0.5 + 0.5) * limit - 0.5).ceil().clamp(0.0, limit) as usize;
    let (left, right) = (edge(x, w), edge(x + width, w));
    let (top, bottom) = (edge(-y, h), edge(-y - height, h));
    let color = pack(instance.color);
    for row in pixels
        .chunks_exact_mut(size.width as usize)
        .skip(top)
        .take(bottom - top)
    {
        row[left..right].fill(color);
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
            },
        );
        paint(
            &mut pixels,
            size,
            Instance {
                rect: [-0.5, 0.5, 2.0, -2.0],
                color: [0.0, 0.0, 1.0, 1.0],
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
}
