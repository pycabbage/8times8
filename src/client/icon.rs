use std::io::Cursor;

use winit::window::{Icon, WindowAttributes};

pub(super) fn apply(attrs: WindowAttributes) -> WindowAttributes {
    let icon = load();
    #[cfg(target_os = "windows")]
    let attrs = {
        use winit::platform::windows::WindowAttributesExtWindows;
        attrs.with_taskbar_icon(icon.clone())
    };
    attrs.with_window_icon(icon)
}

fn load() -> Option<Icon> {
    let bytes = include_bytes!("../../vendor/build/icon.png");
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut pixels).ok()?;
    pixels.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgb => pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 255])
            .collect(),
        png::ColorType::Rgba => pixels,
        _ => return None,
    };
    Icon::from_rgba(rgba, info.width, info.height).ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn embedded_png_loads_as_window_icon() {
        assert!(super::load().is_some());
    }
}
