//! Rendering a screen without a display: lay out the view, draw it with the
//! software renderer, and read the pixels back. Used for previews and
//! snapshot tests now, and the same path draws into the compositor's
//! buffers later.

use std::io::BufWriter;
use std::path::Path;

use iced_core::renderer::{Headless, Style};
use iced_core::{Color, Event, Pixels, Point, Size, clipboard, mouse, window};
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
    Screen::new(size, scale).draw(view, None)
}

/// A screen whose UI keeps its state between frames: what has the keyboard
/// focus, which button is held down. The live shell feeds it input and
/// draws it; [`frame`] draws a view once.
pub struct Screen {
    renderer: iced_tiny_skia::Renderer,
    cache: Cache,
    size: Size,
    scale: f32,
}

impl Screen {
    pub fn new(size: Size, scale: f32) -> Self {
        fonts::load();
        Self {
            renderer: iced_tiny_skia::Renderer::new(fonts::SANS, Pixels(14.0)),
            cache: Cache::default(),
            size,
            scale,
        }
    }

    /// Feeds input to `view`, with the pointer at `cursor`, and returns the
    /// messages it produced.
    pub fn update<Message>(
        &mut self,
        view: Element<'_, Message>,
        events: &[Event],
        cursor: Option<Point>,
    ) -> Vec<Message> {
        let mut messages = Vec::new();
        self.with_ui(view, |ui, renderer| {
            let _ = ui.update(
                events,
                pointer(cursor),
                renderer,
                &mut clipboard::Null,
                &mut messages,
            );
        });
        messages
    }

    /// Gives the keyboard focus to the widget with this id, such as the
    /// command bar's input.
    pub fn focus<Message>(&mut self, view: Element<'_, Message>, id: &'static str) {
        self.with_ui(view, |ui, renderer| {
            let mut focus = iced_core::widget::operation::focusable::focus::<()>(
                iced_core::widget::Id::new(id),
            );
            ui.operate(renderer, &mut focus);
        });
    }

    /// Draws `view` with the pointer at `cursor`.
    pub fn draw<Message>(&mut self, view: Element<'_, Message>, cursor: Option<Point>) -> Frame {
        let cursor = pointer(cursor);
        self.with_ui(view, |ui, renderer| {
            // One redraw, as a window gets for each frame.
            let _ = ui.update(
                &[Event::Window(window::Event::RedrawRequested(
                    iced_core::time::Instant::now(),
                ))],
                cursor,
                renderer,
                &mut clipboard::Null,
                &mut Vec::new(),
            );
            ui.draw(
                renderer,
                &iced_widget::Theme::Light,
                &Style {
                    text_color: Color::WHITE,
                },
                cursor,
            );
        });
        let width = (self.size.width * self.scale).round() as u32;
        let height = (self.size.height * self.scale).round() as u32;
        let rgba = self
            .renderer
            .screenshot(Size::new(width, height), self.scale, Color::BLACK);
        Frame {
            width,
            height,
            rgba,
        }
    }

    fn with_ui<Message>(
        &mut self,
        view: Element<'_, Message>,
        run: impl FnOnce(
            &mut UserInterface<'_, Message, iced_widget::Theme, iced_widget::Renderer>,
            &mut iced_widget::Renderer,
        ),
    ) {
        let cache = std::mem::take(&mut self.cache);
        let mut ui = UserInterface::build(view, self.size, cache, &mut self.renderer);
        run(&mut ui, &mut self.renderer);
        self.cache = ui.into_cache();
    }
}

fn pointer(at: Option<Point>) -> mouse::Cursor {
    at.map_or(mouse::Cursor::Unavailable, mouse::Cursor::Available)
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
