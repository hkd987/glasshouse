//! `glass-compositor`: the Wayland compositor (PRD UI-1, ADR 0009, M2b).
//!
//! It speaks Wayland to apps (`wl_compositor`, `wl_shm`, `xdg_shell`,
//! `wl_seat`, data device), keeps their windows in one space, and composes
//! them in software with pixman onto one output: no GPU, DRM, libinput or
//! X11. What owns the screen (the framebuffer on the VM, a test here) takes
//! the composed pixels and the areas that changed ([`Compositor::render`],
//! [`Compositor::pixels`]) and feeds pointer and keyboard input back in.
//!
//! What it doesn't own: the desktop around the windows (menu bar, dock,
//! tiles: the shell), the screen and input devices (`glass_platform`), and
//! deciding what an app may do (the brokers; an app reaches the system only
//! through them, UI-5).
//!
//! New windows open centered in the work area, between the menu bar and the
//! dock; a click raises a window and gives it the keyboard. Each window has
//! a title bar above it with the controls on the left, as on a Mac (DK-1):
//! the compositor stacks and hit-tests it with the window, and the desktop
//! draws it in its own style ([`Compositor::set_title_bar`]). Dragging the
//! bar moves the window; its close control asks the app to close.

use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use smithay::backend::allocator::Fourcc;
use smithay::backend::input::{Axis, AxisSource};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{AsRenderElements, Kind};
use smithay::backend::renderer::pixman::PixmanRenderer;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::backend::renderer::{Bind, Color32F, ExportMem};
use smithay::desktop::space::SpaceElement;
use smithay::desktop::{PopupManager, Space, Window, WindowSurfaceType};
use smithay::input::keyboard::{FilterResult, Keycode};
use smithay::input::pointer::{AxisFrame, ButtonEvent, CursorImageStatus, MotionEvent};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::pixman;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, Display, DisplayHandle, ListeningSocket, Resource,
};
use smithay::utils::{
    Logical, Physical, Point, Rectangle, SERIAL_COUNTER, Scale, Serial, Size, Transform,
};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface,
};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
    set_data_device_focus,
};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::{
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat, delegate_shm,
    delegate_xdg_decoration, delegate_xdg_shell,
};

pub mod client;

/// The left mouse button's evdev code (`BTN_LEFT`).
pub const BUTTON_LEFT: u32 = 0x110;

/// The title bar's height above each window, in pixels (DK-1).
pub const TITLE_BAR: i32 = 28;
/// The window controls on the title bar's left, close first, as on a Mac
/// (DK-1): their centers from the bar's left edge, and their radius.
pub const CONTROLS: [i32; 3] = [16, 36, 56];
pub const CONTROL_RADIUS: i32 = 6;

smithay::backend::renderer::element::render_elements! {
    /// What the output is composed of: apps' surfaces and their title bars.
    AppElement<=PixmanRenderer>;
    Surface=WaylandSurfaceRenderElement<PixmanRenderer>,
    Bar=MemoryRenderBufferRenderElement<PixmanRenderer>,
}

/// Where windows may go: the screen less the desktop's own bars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkArea {
    /// The menu bar's height.
    pub top: i32,
    /// The dock's height, with its margin.
    pub bottom: i32,
}

/// A window, as the desktop shows it: its app, title and place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// Stays the same while the window is open ([`Compositor::set_title_bar`]).
    pub id: u64,
    pub app_id: String,
    pub title: String,
    /// The app's part on the screen, in pixels; its title bar is the
    /// [`TITLE_BAR`] pixels above.
    pub area: Rect,
    /// It has the keyboard.
    pub focused: bool,
    /// Its minimize control hid it; [`Compositor::activate`] brings it
    /// back (DK-1).
    pub minimized: bool,
}

/// An area of the screen, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// The compositor: its Wayland display and listening socket, the windows,
/// and the output they're composed onto.
pub struct Compositor {
    display: Display<State>,
    socket: ListeningSocket,
    state: State,
    renderer: PixmanRenderer,
    target: pixman::Image<'static, 'static>,
    damage: OutputDamageTracker,
    /// The composed screen, BGRA, row after row.
    pixels: Vec<u8>,
    size: (i32, i32),
    started: Instant,
    /// The output has been composed once: it keeps what it shows, so the
    /// next frame redraws only what changed (buffer age 1).
    composed: bool,
}

impl Compositor {
    /// A compositor listening on `socket` (a name in `$XDG_RUNTIME_DIR`, or
    /// an absolute path) with one output of `size` pixels.
    pub fn new(socket: &str, size: (u32, u32), work_area: WorkArea) -> anyhow::Result<Self> {
        let display = Display::<State>::new().context("creating the Wayland display")?;
        let dh = display.handle();
        let socket = ListeningSocket::bind_absolute(socket_path(socket)?)
            .with_context(|| format!("listening on {socket}"))?;
        let (width, height) = (
            i32::try_from(size.0).context("width")?,
            i32::try_from(size.1).context("height")?,
        );

        let output = Output::new(
            "glass-0".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Glasshouse".into(),
                model: "Screen".into(),
            },
        );
        let mode = Mode {
            size: (width, height).into(),
            refresh: 60_000,
        };
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            None,
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        let _global = output.create_global::<State>(&dh);
        let mut space = Space::default();
        space.map_output(&output, (0, 0));

        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(Default::default(), 500, 30)
            .map_err(|e| anyhow!("keyboard: {e:?}"))?;
        seat.add_pointer();

        let state = State {
            compositor_state: CompositorState::new::<State>(&dh),
            xdg_shell_state: XdgShellState::new::<State>(&dh),
            _decoration_state: XdgDecorationState::new::<State>(&dh),
            shm_state: ShmState::new::<State>(&dh, vec![]),
            output_manager_state: OutputManagerState::new_with_xdg_output::<State>(&dh),
            data_device_state: DataDeviceState::new::<State>(&dh),
            dh: dh.clone(),
            seat_state,
            seat,
            space,
            popups: PopupManager::default(),
            output,
            work_area,
            size: (width, height),
            unplaced: Vec::new(),
            pointer: (0.0, 0.0).into(),
            frames: Vec::new(),
            next_id: 0,
            grab: None,
            refocus: false,
            minimized: Vec::new(),
            zoomed: Vec::new(),
        };
        let target = pixman::Image::new(
            pixman::FormatCode::A8R8G8B8,
            size.0 as usize,
            size.1 as usize,
            true,
        )
        .map_err(|_| anyhow!("allocating a {}x{} output", size.0, size.1))?;
        let damage = OutputDamageTracker::from_output(&state.output);
        Ok(Self {
            display,
            socket,
            state,
            renderer: PixmanRenderer::new().context("starting the software renderer")?,
            target,
            damage,
            pixels: vec![0; size.0 as usize * size.1 as usize * 4],
            size: (width, height),
            started: Instant::now(),
            composed: false,
        })
    }

    /// Takes new clients and handles what clients sent; sends what's
    /// queued for them. Call when the socket or the display is readable,
    /// or often.
    pub fn dispatch(&mut self) -> anyhow::Result<()> {
        while let Some(stream) = self.socket.accept().context("accepting a client")? {
            self.display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))
                .context("adding a client")?;
        }
        self.display
            .dispatch_clients(&mut self.state)
            .context("dispatching clients")?;
        self.state.space.refresh();
        self.state.popups.cleanup();
        // The window with the keyboard closed: the one now in front takes
        // it, as on a Mac.
        if std::mem::take(&mut self.state.refocus) {
            let front = self.state.space.elements().last().cloned();
            self.state.focus(front, SERIAL_COUNTER.next_serial());
        }
        self.display.flush_clients().context("flushing clients")?;
        Ok(())
    }

    /// Composes the windows onto the output, over `backdrop`'s color, and
    /// returns the areas that changed since the last call (none when
    /// nothing did). Windows are told the frame is done, so they draw the
    /// next one.
    pub fn render(&mut self) -> anyhow::Result<Vec<Rect>> {
        let elements = self.elements()?;
        let changed = {
            let mut framebuffer = self
                .renderer
                .bind(&mut self.target)
                .context("binding the output")?;
            let result = self
                .damage
                .render_output(
                    &mut self.renderer,
                    &mut framebuffer,
                    usize::from(self.composed),
                    &elements,
                    Color32F::TRANSPARENT,
                )
                .map_err(|e| anyhow!("composing the output: {e:?}"))?;
            let damage: Vec<Rectangle<i32, Physical>> = result.damage.cloned().unwrap_or_default();
            let mut changed = Vec::new();
            for area in damage {
                let area = area.intersection(Rectangle::from_size(self.size.into()));
                let Some(area) = area.filter(|a| !a.is_empty()) else {
                    continue;
                };
                let region = Rectangle::new(
                    (area.loc.x, area.loc.y).into(),
                    (area.size.w, area.size.h).into(),
                );
                let mapping = self
                    .renderer
                    .copy_framebuffer(&framebuffer, region, Fourcc::Argb8888)
                    .map_err(|e| anyhow!("reading the output: {e:?}"))?;
                let copied = self
                    .renderer
                    .map_texture(&mapping)
                    .map_err(|e| anyhow!("reading the output: {e:?}"))?;
                let (w, stride) = (area.size.w as usize * 4, self.size.0 as usize * 4);
                for row in 0..area.size.h as usize {
                    let at = (area.loc.y as usize + row) * stride + area.loc.x as usize * 4;
                    self.pixels[at..at + w].copy_from_slice(&copied[row * w..row * w + w]);
                }
                changed.push(Rect {
                    x: area.loc.x,
                    y: area.loc.y,
                    width: area.size.w,
                    height: area.size.h,
                });
            }
            changed
        };
        self.composed = true;
        let elapsed = self.started.elapsed();
        let output = self.state.output.clone();
        for window in self.state.space.elements() {
            window.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
        self.display.flush_clients().context("flushing clients")?;
        Ok(changed)
    }

    /// What's on the output, front to back: each window above the ones
    /// under it, its title bar with it.
    fn elements(&mut self) -> anyhow::Result<Vec<AppElement>> {
        let mut elements = Vec::new();
        for window in self.state.space.elements().rev() {
            let Some(at) = self.state.space.element_location(window) else {
                continue;
            };
            let surfaces: Vec<AppElement> = window.render_elements(
                &mut self.renderer,
                (at - window.geometry().loc).to_physical(1),
                Scale::from(1.0),
                1.0,
            );
            elements.extend(surfaces);
            let bar = self
                .state
                .frames
                .iter()
                .find(|f| &f.window == window)
                .and_then(|f| f.bar.as_ref());
            if let Some(bar) = bar {
                let element = MemoryRenderBufferRenderElement::from_buffer(
                    &mut self.renderer,
                    (f64::from(at.x), f64::from(at.y - TITLE_BAR)),
                    bar,
                    None,
                    None,
                    None,
                    Kind::Unspecified,
                )
                .map_err(|e| anyhow!("a title bar: {e:?}"))?;
                elements.push(AppElement::Bar(element));
            }
        }
        Ok(elements)
    }

    /// Gives window `id` its title bar: `bgra` is `width` by [`TITLE_BAR`]
    /// pixels, premultiplied, drawn by the desktop in its style for the
    /// window's title, width and focus (and drawn again when one changes).
    pub fn set_title_bar(&mut self, id: u64, bgra: &[u8], width: i32) {
        if bgra.len() != (width * TITLE_BAR * 4) as usize {
            return;
        }
        if let Some(frame) = self.state.frames.iter_mut().find(|f| f.id == id) {
            frame.bar = Some(MemoryRenderBuffer::from_slice(
                bgra,
                Fourcc::Argb8888,
                (width, TITLE_BAR),
                1,
                Transform::Normal,
                None,
            ));
        }
    }

    /// The composed screen: BGRA, `width * 4` bytes a row. Where no window
    /// is, it's transparent, for the desktop behind to show through.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// What to wait on before calling [`Compositor::dispatch`], as copies
    /// for an event loop to own: the listening socket (a new client) and
    /// the display (a client sent something).
    pub fn fds(&mut self) -> std::io::Result<[OwnedFd; 2]> {
        Ok([
            self.socket.as_fd().try_clone_to_owned()?,
            self.display.backend().poll_fd().try_clone_to_owned()?,
        ])
    }

    /// Whether a window or its title bar is at `at` (pixels on the screen).
    pub fn window_at(&self, at: (f64, f64)) -> bool {
        self.state.under(at.into()).is_some()
    }

    /// Whether a window has the keyboard.
    pub fn has_focus(&self) -> bool {
        self.state
            .seat
            .get_keyboard()
            .is_some_and(|k| k.current_focus().is_some())
    }

    /// The windows, bottom to top.
    pub fn windows(&self) -> Vec<WindowInfo> {
        let focused = self
            .state
            .seat
            .get_keyboard()
            .and_then(|k| k.current_focus());
        let info = |window: &Window, at: Point<i32, Logical>, minimized: bool| {
            let toplevel = window.toplevel()?;
            let size = window.geometry().size;
            let (app_id, title) = toplevel_names(toplevel);
            let id = self.state.frames.iter().find(|f| &f.window == window)?.id;
            Some(WindowInfo {
                id,
                app_id,
                title,
                area: Rect {
                    x: at.x,
                    y: at.y,
                    width: size.w,
                    height: size.h,
                },
                focused: !minimized && focused.as_ref() == Some(toplevel.wl_surface()),
                minimized,
            })
        };
        // Minimized windows under the rest: Alt Tab offers them last.
        let hidden = self
            .state
            .minimized
            .iter()
            .filter_map(|(window, at)| info(window, *at, true));
        let shown =
            self.state.space.elements().filter_map(|window| {
                info(window, self.state.space.element_location(window)?, false)
            });
        hidden.chain(shown).collect()
    }

    /// The pointer moved to `at` (pixels on the screen).
    pub fn pointer_moved(&mut self, at: (f64, f64)) {
        self.state.pointer = at.into();
        // Dragging a title bar moves its window; the app hears nothing.
        match &self.state.grab {
            Some(Grab::Move { window, offset }) => {
                let to = self.state.pointer - *offset;
                let top = f64::from(self.state.work_area.top + TITLE_BAR);
                let window = window.clone();
                self.state
                    .space
                    .map_element(window, (to.x as i32, to.y.max(top) as i32), false);
                return;
            }
            // Dragging an edge asks the app for the size it makes; the
            // window moves with a left or top edge (DK-1).
            Some(Grab::Resize {
                window,
                edges,
                from,
                at,
                size,
            }) => {
                let moved = self.state.pointer - *from;
                let (dx, dy) = (moved.x as i32, moved.y as i32);
                let grow = |near: bool, far: bool, d: i32| match (near, far) {
                    (true, _) => -d,
                    (_, true) => d,
                    _ => 0,
                };
                let w = (size.w + grow(edges.left, edges.right, dx)).max(MIN_SIZE.0);
                let h = (size.h + grow(edges.top, edges.bottom, dy)).max(MIN_SIZE.1);
                let x = if edges.left { at.x + size.w - w } else { at.x };
                let y = if edges.top { at.y + size.h - h } else { at.y };
                let window = window.clone();
                if let Some(toplevel) = window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        state.size = Some((w, h).into());
                        state.states.unset(xdg_toplevel::State::Maximized);
                    });
                    toplevel.send_pending_configure();
                }
                self.state.zoomed.retain(|z| z.window != window);
                self.state.space.map_element(window, (x, y), false);
                return;
            }
            None => {}
        }
        let under = self.state.surface_under(self.state.pointer);
        let Some(pointer) = self.state.seat.get_pointer() else {
            return;
        };
        let event = MotionEvent {
            location: self.state.pointer,
            serial: SERIAL_COUNTER.next_serial(),
            time: self.time(),
        };
        pointer.motion(&mut self.state, under, &event);
        pointer.frame(&mut self.state);
    }

    /// A pointer button (an evdev code, such as [`BUTTON_LEFT`]) went down
    /// or up. Pressing raises the window under the pointer and gives it the
    /// keyboard.
    pub fn pointer_button(&mut self, button: u32, pressed: bool) {
        let serial = SERIAL_COUNTER.next_serial();
        if !pressed && self.state.grab.take().is_some() {
            return;
        }
        if pressed {
            let under = self.state.under(self.state.pointer);
            self.state
                .focus(under.as_ref().map(|(w, _)| w.clone()), serial);
            match under {
                Some((window, Part::Bar)) if button == BUTTON_LEFT => {
                    let at = self
                        .state
                        .space
                        .element_location(&window)
                        .unwrap_or_default();
                    self.state.grab = Some(Grab::Move {
                        offset: self.state.pointer - at.to_f64(),
                        window,
                    });
                    return;
                }
                Some((window, Part::Edge(edges))) if button == BUTTON_LEFT => {
                    let at = self
                        .state
                        .space
                        .element_location(&window)
                        .unwrap_or_default();
                    self.state.grab = Some(Grab::Resize {
                        edges,
                        from: self.state.pointer,
                        at,
                        size: window.geometry().size,
                        window,
                    });
                    return;
                }
                Some((_, Part::Edge(_))) => return,
                Some((window, Part::Minimize)) => {
                    self.state.minimize(&window);
                    return;
                }
                Some((window, Part::Close)) => {
                    if let Some(toplevel) = window.toplevel() {
                        toplevel.send_close();
                    }
                    return;
                }
                Some((window, Part::Zoom)) => {
                    self.state.zoom(&window);
                    return;
                }
                Some((_, Part::Bar)) => return,
                _ => {}
            }
        }
        let Some(pointer) = self.state.seat.get_pointer() else {
            return;
        };
        let state = if pressed {
            smithay::backend::input::ButtonState::Pressed
        } else {
            smithay::backend::input::ButtonState::Released
        };
        let event = ButtonEvent {
            button,
            state,
            serial,
            time: self.time(),
        };
        pointer.button(&mut self.state, &event);
        pointer.frame(&mut self.state);
    }

    /// Raises the window with this id (from [`Compositor::windows`]),
    /// minimized or not, and gives it the keyboard, as Alt Tab does (DK-2).
    /// An id no window has any more does nothing.
    pub fn activate(&mut self, id: u64) {
        let Some(window) = self
            .state
            .frames
            .iter()
            .find(|f| f.id == id)
            .map(|f| f.window.clone())
        else {
            return;
        };
        // A minimized window comes back where it was.
        if let Some(at) = self.state.minimized.iter().position(|(w, _)| w == &window) {
            let (window, loc) = self.state.minimized.remove(at);
            self.state.space.map_element(window, loc, true);
        }
        self.state.focus(Some(window), SERIAL_COUNTER.next_serial());
    }

    /// The wheel turned by `lines` (positive away from the person) over
    /// the window under the pointer: it scrolls (Terminal's scrollback).
    pub fn pointer_axis(&mut self, lines: f64) {
        let Some(pointer) = self.state.seat.get_pointer() else {
            return;
        };
        // Wayland scrolls down for positive values, ten units a line, and
        // 120 to a wheel notch.
        let frame = AxisFrame::new(self.time())
            .source(AxisSource::Wheel)
            .value(Axis::Vertical, -lines * 10.0)
            .v120(Axis::Vertical, (-lines * 120.0) as i32);
        pointer.axis(&mut self.state, frame);
        pointer.frame(&mut self.state);
    }

    /// A key (an evdev code) went down or up, for the focused window.
    pub fn key(&mut self, code: u32, pressed: bool) {
        let Some(keyboard) = self.state.seat.get_keyboard() else {
            return;
        };
        let state = if pressed {
            smithay::backend::input::KeyState::Pressed
        } else {
            smithay::backend::input::KeyState::Released
        };
        let time = self.time();
        // xkb key codes are evdev's plus 8.
        keyboard.input::<(), _>(
            &mut self.state,
            Keycode::new(code + 8),
            state,
            SERIAL_COUNTER.next_serial(),
            time,
            |_, _, _| FilterResult::Forward,
        );
    }

    fn time(&self) -> u32 {
        // Wayland times are milliseconds that wrap.
        self.started.elapsed().as_millis() as u32
    }
}

/// `socket` as a path: absolute already, or in `$XDG_RUNTIME_DIR`.
fn socket_path(socket: &str) -> anyhow::Result<std::path::PathBuf> {
    if socket.starts_with('/') {
        return Ok(socket.into());
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR isn't set")?;
    Ok(std::path::Path::new(&dir).join(socket))
}

/// A toplevel's app id and title.
fn toplevel_names(toplevel: &ToplevelSurface) -> (String, String) {
    smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
        let data = states
            .data_map
            .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>();
        data.and_then(|d| d.lock().ok())
            .map(|d| {
                (
                    d.app_id.clone().unwrap_or_default(),
                    d.title.clone().unwrap_or_default(),
                )
            })
            .unwrap_or_default()
    })
}

/// What the Wayland handlers work on.
struct State {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    /// Windows are told the frame is ours (DK-1): every one gets our title
    /// bar and controls, so none draws its own.
    _decoration_state: XdgDecorationState,
    shm_state: ShmState,
    #[allow(dead_code)] // Keeps `wl_output` and `xdg_output` advertised.
    output_manager_state: OutputManagerState,
    data_device_state: DataDeviceState,
    /// For handing the clipboard to the app with the keyboard.
    dh: DisplayHandle,
    seat_state: SeatState<State>,
    seat: Seat<State>,
    space: Space<Window>,
    popups: PopupManager,
    output: Output,
    work_area: WorkArea,
    size: (i32, i32),
    /// Windows mapped but not yet placed: placed once their first buffer
    /// gives them a size.
    unplaced: Vec<Window>,
    pointer: Point<f64, Logical>,
    /// Every toplevel's id and title bar.
    frames: Vec<Frame>,
    next_id: u64,
    /// A title bar being dragged.
    grab: Option<Grab>,
    /// The window with the keyboard closed; the next one in front gets it.
    refocus: bool,
    /// Windows minimized, oldest first, and where each was.
    minimized: Vec<(Window, Point<i32, Logical>)>,
    /// Windows zoomed to the work area.
    zoomed: Vec<Zoomed>,
}

/// A window zoomed to the work area, and where and how big it was.
struct Zoomed {
    window: Window,
    at: Point<i32, Logical>,
    size: Size<i32, Logical>,
}

/// A window's id and the title bar the desktop drew for it.
struct Frame {
    window: Window,
    id: u64,
    bar: Option<MemoryRenderBuffer>,
}

/// A window moving with the pointer: where on it the pointer took hold.
enum Grab {
    /// Its title bar: where on the window the pointer took hold.
    Move {
        window: Window,
        offset: Point<f64, Logical>,
    },
    /// An edge or corner: where the pointer and the window started.
    Resize {
        window: Window,
        edges: Edges,
        from: Point<f64, Logical>,
        at: Point<i32, Logical>,
        size: Size<i32, Logical>,
    },
}

impl Grab {
    fn window(&self) -> &Window {
        match self {
            Grab::Move { window, .. } | Grab::Resize { window, .. } => window,
        }
    }
}

/// Which edges of a window the pointer holds: one, or two at a corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Edges {
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
}

/// How far outside a window its edges can be taken hold of.
const EDGE: i32 = 6;
/// The smallest a window is made by dragging an edge.
const MIN_SIZE: (i32, i32) = (160, 80);

/// What part of a window is under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Content,
    Bar,
    /// The minimize control (the middle one).
    Minimize,
    Close,
    /// The zoom control (the right one).
    Zoom,
    /// Just outside an edge or a corner: dragging resizes.
    Edge(Edges),
}

impl State {
    /// The window (and which part of it) at `at`: the topmost whose
    /// content or title bar is there.
    fn under(&self, at: Point<f64, Logical>) -> Option<(Window, Part)> {
        for window in self.space.elements().rev() {
            let Some(loc) = self.space.element_location(window) else {
                continue;
            };
            let width = window.geometry().size.w;
            let (x, y) = (at.x - f64::from(loc.x), at.y - f64::from(loc.y));
            if (0.0..f64::from(width)).contains(&x) && (-f64::from(TITLE_BAR)..0.0).contains(&y) {
                let cy = -f64::from(TITLE_BAR) / 2.0;
                let reach = f64::from(CONTROL_RADIUS + 2);
                let on = |cx: i32| (x - f64::from(cx)).powi(2) + (y - cy).powi(2) <= reach * reach;
                let part = if on(CONTROLS[0]) {
                    Part::Close
                } else if on(CONTROLS[1]) {
                    Part::Minimize
                } else if on(CONTROLS[2]) {
                    Part::Zoom
                } else {
                    Part::Bar
                };
                return Some((window.clone(), part));
            }
            // Just outside the window and its title bar: an edge or corner.
            let height = window.geometry().size.h;
            let (left, top) = (-f64::from(EDGE), -f64::from(TITLE_BAR + EDGE));
            let (right, bottom) = (f64::from(width + EDGE), f64::from(height + EDGE));
            if (left..right).contains(&x) && (top..bottom).contains(&y) {
                let edges = Edges {
                    left: x < 0.0,
                    right: x >= f64::from(width),
                    top: y < -f64::from(TITLE_BAR),
                    bottom: y >= f64::from(height),
                };
                if edges.left || edges.right || edges.top || edges.bottom {
                    return Some((window.clone(), Part::Edge(edges)));
                }
            }
            let bbox = window.bbox();
            let inside = at - loc.to_f64() + window.geometry().loc.to_f64();
            if bbox.to_f64().contains(inside) && window.is_in_input_region(&inside) {
                return Some((window.clone(), Part::Content));
            }
        }
        None
    }

    /// The surface under `at` and where it sits on the screen.
    fn surface_under(&self, at: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        let (window, location) = self.space.element_under(at)?;
        window
            .surface_under(at - location.to_f64(), WindowSurfaceType::ALL)
            .map(|(surface, offset)| (surface, (offset + location).to_f64()))
    }

    /// Fills the work area with `window`, or puts a zoomed one back where
    /// and as big as it was (DK-1). The app draws at the size it's asked
    /// for, if it can; one that can't stays as it is, moved.
    fn zoom(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let (at, size, maximized) =
            if let Some(index) = self.zoomed.iter().position(|z| &z.window == window) {
                let was = self.zoomed.remove(index);
                (was.at, was.size, false)
            } else {
                let Some(at) = self.space.element_location(window) else {
                    return;
                };
                self.zoomed.push(Zoomed {
                    window: window.clone(),
                    at,
                    size: window.geometry().size,
                });
                let top = self.work_area.top + TITLE_BAR;
                let size = (self.size.0, self.size.1 - top - self.work_area.bottom).into();
                ((0, top).into(), size, true)
            };
        toplevel.with_pending_state(|state| {
            state.size = Some(size);
            if maximized {
                state.states.set(xdg_toplevel::State::Maximized);
            } else {
                state.states.unset(xdg_toplevel::State::Maximized);
            }
        });
        toplevel.send_pending_configure();
        self.space.map_element(window.clone(), at, true);
        self.focus(Some(window.clone()), SERIAL_COUNTER.next_serial());
    }

    /// Hides `window` until it's activated again; the one in front of the
    /// rest takes the keyboard if it had it (DK-1).
    fn minimize(&mut self, window: &Window) {
        let Some(at) = self.space.element_location(window) else {
            return;
        };
        let had_keyboard = window.toplevel().is_some_and(|t| {
            self.seat
                .get_keyboard()
                .and_then(|k| k.current_focus())
                .as_ref()
                == Some(t.wl_surface())
        });
        self.space.unmap_elem(window);
        self.minimized.push((window.clone(), at));
        if had_keyboard {
            let front = self.space.elements().last().cloned();
            self.focus(front, SERIAL_COUNTER.next_serial());
        }
    }

    /// Raises `window` and gives it the keyboard; none takes the keyboard
    /// from all of them.
    fn focus(&mut self, window: Option<Window>, serial: Serial) {
        if let Some(window) = &window {
            self.space.raise_element(window, true);
        }
        for each in self.space.elements() {
            let active = Some(each) == window.as_ref();
            if let Some(toplevel) = each.toplevel() {
                toplevel.with_pending_state(|state| {
                    if active {
                        state.states.set(xdg_toplevel::State::Activated);
                    } else {
                        state.states.unset(xdg_toplevel::State::Activated);
                    }
                });
                toplevel.send_pending_configure();
            }
        }
        let focus = window.and_then(|w| w.toplevel().map(|t| t.wl_surface().clone()));
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, focus, serial);
        }
    }

    /// The room a window has: the work area under its title bar.
    fn work_size(&self) -> Size<i32, Logical> {
        let height = self.size.1 - self.work_area.top - self.work_area.bottom - TITLE_BAR;
        Size::from((self.size.0, height.max(1)))
    }

    /// Places a window centered in the work area once it has a size.
    fn place(&mut self, surface: &WlSurface) {
        let Some(index) = self
            .unplaced
            .iter()
            .position(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
        else {
            return;
        };
        let size: Size<i32, Logical> = self.unplaced[index].geometry().size;
        if size.w <= 0 || size.h <= 0 {
            return;
        }
        // One bigger than the work area is asked to fit it; it's placed at
        // the size it has now and takes the new one when it next commits.
        let fits = self.work_size();
        let size = Size::from((size.w.min(fits.w), size.h.min(fits.h)));
        if let Some(toplevel) = self.unplaced[index].toplevel()
            && toplevel
                .current_state()
                .size
                .is_none_or(|s| s.w > fits.w || s.h > fits.h)
            && self.unplaced[index].geometry().size != size
        {
            toplevel.with_pending_state(|state| state.size = Some(size));
            toplevel.send_pending_configure();
        }
        let window = self.unplaced.remove(index);
        // The window with its title bar, centered in the work area.
        let area_height = self.size.1 - self.work_area.top - self.work_area.bottom;
        let x = ((self.size.0 - size.w) / 2).max(0);
        let y = self.work_area.top + TITLE_BAR + ((area_height - TITLE_BAR - size.h) / 2).max(0);
        self.space.map_element(window.clone(), (x, y), true);
        self.focus(Some(window), SERIAL_COUNTER.next_serial());
    }
}

#[derive(Default)]
struct ClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // Every client is inserted with a `ClientState` (`dispatch`).
        static FALLBACK: std::sync::OnceLock<CompositorClientState> = std::sync::OnceLock::new();
        client
            .get_data::<ClientState>()
            .map(|c| &c.compositor_state)
            .unwrap_or_else(|| FALLBACK.get_or_init(CompositorClientState::default))
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self
                .space
                .elements()
                .chain(self.unplaced.iter())
                .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root))
            {
                window.on_commit();
            }
        }
        // The first commit asks for a configure before any buffer.
        if let Some(window) = self
            .unplaced
            .iter()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
            && let Some(toplevel) = window.toplevel()
            && !toplevel.is_initial_configure_sent()
        {
            toplevel.send_configure();
        }
        self.place(surface);
        self.popups.commit(surface);
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // As big as it may be: the work area under its title bar
        // (`xdg_toplevel.configure_bounds`); `place` holds it to that too.
        let bounds = self.work_size();
        surface.with_pending_state(|state| state.bounds = Some(bounds));
        let window = Window::new_wayland_window(surface);
        self.next_id += 1;
        self.frames.push(Frame {
            window: window.clone(),
            id: self.next_id,
            bar: None,
        });
        self.unplaced.push(window);
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        let _ = self
            .popups
            .track_popup(smithay::desktop::PopupKind::Xdg(surface));
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {}

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let focused = self.seat.get_keyboard().and_then(|k| k.current_focus());
        self.refocus |= focused.as_ref() == Some(surface.wl_surface());
        let gone = |w: &Window| w.toplevel().is_some_and(|t| t == &surface);
        self.unplaced.retain(|w| !gone(w));
        self.minimized.retain(|(w, _)| !gone(w));
        self.zoomed.retain(|z| !gone(&z.window));
        self.frames.retain(|f| !gone(&f.window));
        if self.grab.as_ref().is_some_and(|g| gone(g.window())) {
            self.grab = None;
        }
    }
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<State> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}
    /// The app with the keyboard gets the clipboard: its data device
    /// hears of the selection, and may set it (copy and paste, DK-2).
    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|surface| self.dh.get_client(surface.id()).ok());
        set_data_device_focus(&self.dh, seat, client);
    }
}

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for State {}
impl ServerDndGrabHandler for State {}

impl OutputHandler for State {}

delegate_compositor!(State);
delegate_shm!(State);
delegate_xdg_shell!(State);
delegate_xdg_decoration!(State);

impl XdgDecorationHandler for State {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        server_side(&toplevel);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        server_side(&toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        server_side(&toplevel);
    }
}

/// Whatever a window asks, its frame is ours.
fn server_side(toplevel: &ToplevelSurface) {
    toplevel.with_pending_state(|state| {
        state.decoration_mode = Some(DecorationMode::ServerSide);
    });
    if toplevel.is_initial_configure_sent() {
        toplevel.send_pending_configure();
    }
}
delegate_seat!(State);
delegate_data_device!(State);
delegate_output!(State);
