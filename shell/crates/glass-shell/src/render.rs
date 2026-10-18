//! Rendering a screen without a display: lay out the view, draw it with the
//! software renderer, and read the pixels back. Used for previews and
//! snapshot tests now, and the same path draws into the compositor's
//! buffers later.

use std::io::BufWriter;
use std::path::Path;

use iced_core::renderer::{Headless, Style};
use iced_core::{Color, Event, Pixels, Size, clipboard, mouse, window};
use iced_runtime::user_interface::{Cache, UserInterface};

use crate::{Element, fonts};

/// A rendered frame: straight RGBA rows, top to bottom.
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Draws `view` on a `size` (logical pixels) screen at `scale` (2.0 for a
/// high-density display).
pub fn frame<Message>(view: Element<'_, Message>, size: Size, scale: f32) -> Frame {
    fonts::load();
    let mut renderer = iced_tiny_skia::Renderer::new(fonts::SANS, Pixels(14.0));
    let mut ui = UserInterface::build(view, size, Cache::default(), &mut renderer);
    // One redraw, as a window would get on its first frame.
    let mut messages = Vec::new();
    let _ = ui.update(
        &[Event::Window(window::Event::RedrawRequested(
            iced_core::time::Instant::now(),
        ))],
        mouse::Cursor::Unavailable,
        &mut renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    ui.draw(
        &mut renderer,
        &iced_widget::Theme::Light,
        &Style {
            text_color: Color::WHITE,
        },
        mouse::Cursor::Unavailable,
    );
    let width = (size.width * scale).round() as u32;
    let height = (size.height * scale).round() as u32;
    let rgba = renderer.screenshot(Size::new(width, height), scale, Color::BLACK);
    Frame {
        width,
        height,
        rgba,
    }
}

impl Frame {
    pub fn write_png(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = BufWriter::new(std::fs::File::create(path)?);
        let mut encoder = png::Encoder::new(file, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(std::io::Error::other)?;
        writer
            .write_image_data(&self.rgba)
            .map_err(std::io::Error::other)?;
        writer.finish().map_err(std::io::Error::other)
    }

    /// The pixel at `(x, y)`, for tests.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }
}
