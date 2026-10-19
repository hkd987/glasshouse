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
//! dock; a click raises a window and gives it the keyboard.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::pixman::PixmanRenderer;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::backend::renderer::{Bind, Color32F, ExportMem};
use smithay::desktop::space::render_output;
use smithay::desktop::{PopupManager, Space, Window, WindowSurfaceType};
use smithay::input::keyboard::{FilterResult, Keycode};
use smithay::input::pointer::{ButtonEvent, CursorImageStatus, MotionEvent};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::pixman;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Display, ListeningSocket};
use smithay::utils::{
    Logical, Physical, Point, Rectangle, SERIAL_COUNTER, Serial, Size, Transform,
};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface,
};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::{
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat, delegate_shm,
    delegate_xdg_shell,
};

pub mod client;

/// The left mouse button's evdev code (`BTN_LEFT`).
pub const BUTTON_LEFT: u32 = 0x110;

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
    pub app_id: String,
    pub title: String,
    /// On the screen, in pixels.
    pub area: Rect,
    /// It has the keyboard.
    pub focused: bool,
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
            shm_state: ShmState::new::<State>(&dh, vec![]),
            output_manager_state: OutputManagerState::new_with_xdg_output::<State>(&dh),
            data_device_state: DataDeviceState::new::<State>(&dh),
            seat_state,
            seat,
            space,
            popups: PopupManager::default(),
            output,
            work_area,
            size: (width, height),
            unplaced: Vec::new(),
            pointer: (0.0, 0.0).into(),
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
        self.display.flush_clients().context("flushing clients")?;
        Ok(())
    }

    /// Composes the windows onto the output, over `backdrop`'s color, and
    /// returns the areas that changed since the last call (none when
    /// nothing did). Windows are told the frame is done, so they draw the
    /// next one.
    pub fn render(&mut self) -> anyhow::Result<Vec<Rect>> {
        let changed = {
            let mut framebuffer = self
                .renderer
                .bind(&mut self.target)
                .context("binding the output")?;
            let result = render_output::<_, SolidColorRenderElement, _, _>(
                &self.state.output,
                &mut self.renderer,
                &mut framebuffer,
                1.0,
                usize::from(self.composed),
                [&self.state.space],
                &[],
                &mut self.damage,
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

    /// The composed screen: BGRA, `width * 4` bytes a row. Where no window
    /// is, it's transparent, for the desktop behind to show through.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The windows, bottom to top.
    pub fn windows(&self) -> Vec<WindowInfo> {
        let focused = self
            .state
            .seat
            .get_keyboard()
            .and_then(|k| k.current_focus());
        self.state
            .space
            .elements()
            .filter_map(|window| {
                let toplevel = window.toplevel()?;
                let at = self.state.space.element_location(window)?;
                let size = window.geometry().size;
                let (app_id, title) = toplevel_names(toplevel);
                Some(WindowInfo {
                    app_id,
                    title,
                    area: Rect {
                        x: at.x,
                        y: at.y,
                        width: size.w,
                        height: size.h,
                    },
                    focused: focused.as_ref() == Some(toplevel.wl_surface()),
                })
            })
            .collect()
    }

    /// The pointer moved to `at` (pixels on the screen).
    pub fn pointer_moved(&mut self, at: (f64, f64)) {
        self.state.pointer = at.into();
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
        if pressed {
            self.state.focus_at(self.state.pointer, serial);
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
    shm_state: ShmState,
    #[allow(dead_code)] // Keeps `wl_output` and `xdg_output` advertised.
    output_manager_state: OutputManagerState,
    data_device_state: DataDeviceState,
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
}

impl State {
    /// The surface under `at` and where it sits on the screen.
    fn surface_under(&self, at: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        let (window, location) = self.space.element_under(at)?;
        window
            .surface_under(at - location.to_f64(), WindowSurfaceType::ALL)
            .map(|(surface, offset)| (surface, (offset + location).to_f64()))
    }

    /// Raises the window under `at` and gives it the keyboard; a click on
    /// no window takes the keyboard from all of them.
    fn focus_at(&mut self, at: Point<f64, Logical>, serial: Serial) {
        let window = self.space.element_under(at).map(|(w, _)| w.clone());
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
        let window = self.unplaced.remove(index);
        let area_height = self.size.1 - self.work_area.top - self.work_area.bottom;
        let x = ((self.size.0 - size.w) / 2).max(0);
        let y = self.work_area.top + ((area_height - size.h) / 2).max(0);
        self.space.map_element(window.clone(), (x, y), true);
        self.focus_window(&window);
    }

    fn focus_window(&mut self, window: &Window) {
        let at = self
            .space
            .element_location(window)
            .unwrap_or_default()
            .to_f64()
            + Point::<f64, Logical>::from((1.0, 1.0));
        self.focus_at(at, SERIAL_COUNTER.next_serial());
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
        self.unplaced.push(Window::new_wayland_window(surface));
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
        self.unplaced
            .retain(|w| w.toplevel().is_some_and(|t| t != &surface));
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
    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}
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
delegate_seat!(State);
delegate_data_device!(State);
delegate_output!(State);
