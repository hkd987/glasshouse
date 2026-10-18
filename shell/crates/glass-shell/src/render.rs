//! Rendering a screen with the software renderer: lay out the view, draw
//! it, and read the pixels back. Previews and snapshot tests draw a view
//! once; a live screen ([`Screen`]) keeps its pixels and redraws only what
//! changed since the last frame.

use std::io::BufWriter;
use std::path::Path;

use iced_core::renderer::Style;
use iced_core::{Color, Event, Pixels, Point, Rectangle, Size, clipboard, mouse, window};
use iced_runtime::user_interface::{self as ui, Cache, UserInterface};
use iced_tiny_skia::Layer;
use iced_tiny_skia::graphics::{Viewport, damage};

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
    let mut screen = Screen::new(size, scale);
    screen.render(view, None);
    screen.frame()
}

/// A screen whose UI keeps its state between frames (what has the keyboard
/// focus, which button is held down) and whose pixels stay drawn: each
/// frame redraws only the parts whose content changed, such as a tile or
/// the text being typed. The live shell feeds it input and draws it;
/// [`frame`] draws a view once.
pub struct Screen {
    renderer: iced_tiny_skia::Renderer,
    cache: Cache,
    size: Size,
    scale: f32,
    viewport: Viewport,
    /// The screen's pixels as the renderer draws them: B, G, R, A bytes.
    pixels: Vec<u8>,
    /// Where each frame is drawn before its damaged areas are copied over.
    scratch: Vec<u8>,
    clip: tiny_skia::Mask,
    /// What `pixels` shows, to diff the next frame against.
    shown: Option<Vec<Layer>>,
    /// What's behind the UI, such as the wallpaper, as B, G, R, A bytes;
    /// black when there's none.
    backdrop: Option<Vec<u8>>,
    /// Where the pointer was at the last update or frame.
    cursor: mouse::Cursor,
    /// When the last frame asked to be drawn again, such as a text field's
    /// caret blinking.
    wake: Option<iced_core::time::Instant>,
}

/// What the UI made of some input.
#[derive(Debug)]
pub struct Updated<Message> {
    pub messages: Vec<Message>,
    /// It looks different now without a message, such as a button the
    /// pointer went onto.
    pub redraw: bool,
}

/// A part of the screen that changed, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Damage {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Screen {
    pub fn new(size: Size, scale: f32) -> Self {
        fonts::load();
        let viewport = Viewport::with_physical_size(
            Size::new(
                (size.width * scale).round() as u32,
                (size.height * scale).round() as u32,
            ),
            scale,
        );
        let physical = viewport.physical_size();
        Self {
            renderer: iced_tiny_skia::Renderer::new(fonts::SANS, Pixels(14.0)),
            cache: Cache::default(),
            size,
            scale,
            viewport,
            pixels: vec![0; physical.width as usize * physical.height as usize * 4],
            scratch: vec![0; physical.width as usize * physical.height as usize * 4],
            clip: tiny_skia::Mask::new(physical.width.max(1), physical.height.max(1))
                .expect("a screen has a size"),
            shown: None,
            backdrop: None,
            cursor: mouse::Cursor::Unavailable,
            wake: None,
        }
    }

    /// Keeps `bgra` (the screen's size, B, G, R, A bytes, opaque) behind
    /// the UI, and draws it again only where the UI changes. A large image
    /// as the view's bottom layer would instead be scaled and drawn again
    /// for every changed area; see `wallpaper::backdrop`.
    pub fn set_backdrop(&mut self, bgra: Vec<u8>) {
        if bgra.len() == self.pixels.len() {
            self.backdrop = Some(bgra);
            self.shown = None;
        }
    }

    /// The screen's size in physical pixels.
    pub fn physical_size(&self) -> (u32, u32) {
        let size = self.viewport.physical_size();
        (size.width, size.height)
    }

    /// Feeds input to `view`, with the pointer at `cursor`, and returns the
    /// messages it produced.
    pub fn update<Message>(
        &mut self,
        view: Element<'_, Message>,
        events: &[Event],
        cursor: Option<Point>,
    ) -> Updated<Message> {
        let mut messages = Vec::new();
        let mut redraw = false;
        let (before, cursor) = (self.cursor, pointer(cursor));
        self.with_ui(view, |ui, renderer| {
            // Widgets are built afresh for every update, and a button only
            // asks for a redraw when its look changes from the one it was
            // last drawn with; a redraw at the pointer's last place tells it
            // that look first.
            let _ = ui.update(
                &[Event::Window(window::Event::RedrawRequested(
                    iced_core::time::Instant::now(),
                ))],
                before,
                renderer,
                &mut clipboard::Null,
                &mut Vec::new(),
            );
            let (state, _) = ui.update(
                events,
                cursor,
                renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            // A widget asks for the next frame when it looks different (a
            // button the pointer went onto). A blinking caret asks for a
            // later one, which isn't a reason to draw now.
            redraw = matches!(
                state,
                ui::State::Updated {
                    redraw_request: window::RedrawRequest::NextFrame,
                    ..
                }
            );
        });
        self.cursor = cursor;
        Updated { messages, redraw }
    }

    /// When the last frame wants drawing again even if nothing else
    /// changes: a focused text field's caret blinks.
    pub fn next_redraw(&self) -> Option<std::time::Instant> {
        self.wake
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

    /// Draws `view` with the pointer at `cursor`, redrawing only what
    /// changed since the last frame, and returns those parts: everything
    /// the first time, nothing when the frame is the same.
    pub fn render<Message>(
        &mut self,
        view: Element<'_, Message>,
        cursor: Option<Point>,
    ) -> Vec<Damage> {
        let cursor = pointer(cursor);
        self.cursor = cursor;
        let mut wake = None;
        self.with_ui(view, |ui, renderer| {
            // One redraw, as a window gets for each frame.
            let (state, _) = ui.update(
                &[Event::Window(window::Event::RedrawRequested(
                    iced_core::time::Instant::now(),
                ))],
                cursor,
                renderer,
                &mut clipboard::Null,
                &mut Vec::new(),
            );
            wake = match state {
                ui::State::Updated {
                    redraw_request: window::RedrawRequest::At(at),
                    ..
                } => Some(at),
                _ => None,
            };
            ui.draw(
                renderer,
                &iced_widget::Theme::Light,
                &Style {
                    text_color: Color::WHITE,
                },
                cursor,
            );
        });
        self.wake = wake;
        let screen = Rectangle::with_size(self.viewport.logical_size());
        let layers = self.renderer.layers().to_vec();
        let changed = match &self.shown {
            Some(shown) => damage::diff(shown, &layers, |l| vec![l.bounds], layer_damage),
            None => vec![screen],
        };
        let changed = merge_overlapping(damage::group(changed, screen));
        // The renderer fills a damaged area and redraws everything over it,
        // but a quad inside the area casts its shadow past it unclipped. So
        // each area is drawn into scratch on its own and only it is kept.
        let (width, height) = self.physical_size();
        let stride = width as usize * 4;
        let mut damaged = Vec::with_capacity(changed.len());
        for area in changed {
            let Some(d) = self.physical(area) else {
                continue;
            };
            // Whole pixels, so every pixel copied below was drawn.
            let area = Rectangle {
                x: d.x as f32 / self.scale,
                y: d.y as f32 / self.scale,
                width: d.width as f32 / self.scale,
                height: d.height as f32 / self.scale,
            };
            let mut scratch = tiny_skia::PixmapMut::from_bytes(&mut self.scratch, width, height)
                .expect("the buffer fits the screen");
            let background = match self.backdrop {
                Some(_) => Color::TRANSPARENT,
                None => Color::BLACK,
            };
            self.renderer.draw(
                &mut scratch,
                &mut self.clip,
                &self.viewport,
                &[area],
                background,
            );
            let rows = d.y as usize..(d.y + d.height) as usize;
            let columns = d.x as usize * 4..(d.x + d.width) as usize * 4;
            for y in rows.clone() {
                let row = y * stride + columns.start..y * stride + columns.end;
                let from = self.backdrop.as_deref().unwrap_or(&self.scratch);
                self.pixels[row.clone()].copy_from_slice(&from[row]);
            }
            if self.backdrop.is_some() {
                self.composite(d);
            }
            damaged.push(d);
        }
        self.shown = Some(layers);
        damaged
    }

    /// Lays the UI drawn into scratch over the backdrop already copied into
    /// `pixels`, in area `d`.
    fn composite(&mut self, d: Damage) {
        let (width, height) = self.physical_size();
        let Some(ui) =
            tiny_skia::PixmapRef::from_bytes(&self.scratch, width, height).and_then(|all| {
                all.clone_rect(tiny_skia::IntRect::from_xywh(
                    d.x as i32, d.y as i32, d.width, d.height,
                )?)
            })
        else {
            return;
        };
        if let Some(mut screen) = tiny_skia::PixmapMut::from_bytes(&mut self.pixels, width, height)
        {
            screen.draw_pixmap(
                d.x as i32,
                d.y as i32,
                ui.as_ref(),
                &tiny_skia::PixmapPaint::default(),
                tiny_skia::Transform::identity(),
                None,
            );
        }
    }

    /// The screen's pixels: B, G, R, A bytes, rows top to bottom.
    pub fn bgra(&self) -> &[u8] {
        &self.pixels
    }

    /// The screen's pixels as a straight RGBA frame.
    pub fn frame(&self) -> Frame {
        let (width, height) = self.physical_size();
        let mut rgba = self.pixels.clone();
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        Frame {
            width,
            height,
            rgba,
        }
    }

    /// Draws `view` and returns the whole frame, for callers that want
    /// pixels rather than what changed.
    pub fn draw<Message>(&mut self, view: Element<'_, Message>, cursor: Option<Point>) -> Frame {
        self.render(view, cursor);
        self.frame()
    }

    /// A logical rectangle as whole physical pixels on the screen.
    fn physical(&self, r: Rectangle) -> Option<Damage> {
        let (width, height) = self.physical_size();
        let x0 = ((r.x * self.scale).floor().max(0.0) as u32).min(width);
        let y0 = ((r.y * self.scale).floor().max(0.0) as u32).min(height);
        let x1 = (((r.x + r.width) * self.scale).ceil().max(0.0) as u32).min(width);
        let y1 = (((r.y + r.height) * self.scale).ceil().max(0.0) as u32).min(height);
        (x1 > x0 && y1 > y0).then_some(Damage {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        })
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

/// Joins areas that overlap where one area costs no more to draw than the
/// two: each is drawn on its own, so an overlap would be drawn twice.
/// (`damage::group` only joins areas next to each other in its order.)
fn merge_overlapping(mut areas: Vec<Rectangle>) -> Vec<Rectangle> {
    'again: loop {
        for i in 0..areas.len() {
            for j in i + 1..areas.len() {
                let (a, b) = (areas[i], areas[j]);
                let union = a.union(&b);
                if a.intersects(&b) && union.area() <= a.area() + b.area() {
                    areas[i] = union;
                    areas.swap_remove(j);
                    continue 'again;
                }
            }
        }
        return areas;
    }
}

/// What changed between two layers. iced's own diff covers a changed
/// quad's bounds but not its drop shadow, which tiles and panels cast well
/// outside them, so a quad's damage here reaches as far as its shadow.
fn layer_damage(previous: &Layer, current: &Layer) -> Vec<Rectangle> {
    let mut changed = Layer::damage(previous, current);
    if previous.bounds == current.bounds {
        let reach = |(quad, _): &(iced_core::renderer::Quad, iced_core::Background)| {
            let shadow = quad.shadow;
            let blur = shadow.blur_radius;
            let cast = Rectangle {
                x: quad.bounds.x + shadow.offset.x - blur,
                y: quad.bounds.y + shadow.offset.y - blur,
                width: quad.bounds.width + blur * 2.0,
                height: quad.bounds.height + blur * 2.0,
            };
            vec![quad.bounds.union(&cast).expand(1.0)]
        };
        changed.extend(damage::list(
            &previous.quads,
            &current.quads,
            reach,
            |a, b| a == b,
        ));
    }
    changed
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
