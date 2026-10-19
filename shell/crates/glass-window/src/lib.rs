//! `glass-window`: a native Glasshouse app's window (PRD UI-6). One Wayland
//! toplevel whose content is an iced view drawn with the shell's renderer
//! (`glass_shell::render::Screen`) into shared memory, sent again only where
//! it changed; the pointer and keys come back as iced events, keys through
//! the compositor's own keymap (xkbcommon), repeated while held.
//!
//! First-party native apps, such as Terminal and Activity Monitor, are built
//! on it: [`App`] is what an app provides, [`run`] its loop. Other threads
//! (a terminal's output) wake the loop with a [`Waker`].
//!
//! Owed before such apps ship (UI-6): an accessibility tree (AccessKit) and
//! a manifest declaring their actions.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use glass_shell::Element;
use glass_shell::render::Screen;
use iced_core::keyboard::{self, Key, Location, Modifiers, key::Named};
use iced_core::{Event, Point, Size, mouse};
use rustix::event::{PollFd, PollFlags, poll};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm,
    wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
pub use xkbcommon::xkb;

/// A key as the app gets it, before the UI does.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyPress {
    /// What the key means in the keymap, such as `Return` or `a`.
    pub keysym: xkb::Keysym,
    /// The text it types with the modifiers held ("A" with Shift), if any.
    pub text: String,
    pub modifiers: Modifiers,
    /// A repeat while the key is held.
    pub repeat: bool,
}

/// What a native app provides.
pub trait App {
    type Message;

    fn title(&self) -> String;
    /// The app's id, as the compositor and the dock know it.
    fn app_id(&self) -> String;
    /// The window's size, in pixels.
    fn size(&self) -> (u32, u32);
    /// The window's background behind the UI: B, G, R, A.
    fn background(&self) -> [u8; 4];
    fn view(&self) -> Element<'_, Self::Message>;
    fn update(&mut self, message: Self::Message);
    /// A key going down (or repeating), before the UI sees it: true if the
    /// app took it (a terminal takes them all).
    fn key(&mut self, _press: &KeyPress) -> bool {
        false
    }
    /// The [`Waker`] was woken: true if the view changed (new output).
    fn woken(&mut self) -> bool {
        false
    }
    /// The window should close (its close control): true to let it.
    fn close(&mut self) -> bool {
        true
    }
    /// The app is finished (a terminal's shell exited): the window closes.
    fn done(&self) -> bool {
        false
    }
}

/// Wakes the window's loop from another thread, such as a terminal's
/// reader when the shell printed something.
#[derive(Debug)]
pub struct Waker(UnixStream);

impl Waker {
    pub fn wake(&self) {
        // A byte waiting is enough; a full socket means one is already there.
        let _ = (&self.0).write(&[1]);
    }

    pub fn try_clone(&self) -> std::io::Result<Waker> {
        Ok(Waker(self.0.try_clone()?))
    }
}

/// The loop's side of a [`Waker`], made before the app so the app can keep
/// the waker.
pub struct Wake {
    reader: UnixStream,
    writer: UnixStream,
}

impl Wake {
    pub fn new() -> std::io::Result<Self> {
        let (reader, writer) = UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        Ok(Self { reader, writer })
    }

    pub fn waker(&self) -> std::io::Result<Waker> {
        Ok(Waker(self.writer.try_clone()?))
    }

    /// Takes what woke the loop; true if anything had.
    fn drain(&mut self) -> bool {
        let mut buf = [0u8; 64];
        let mut woken = false;
        while let Ok(n) = self.reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            woken = true;
        }
        woken
    }
}

/// Opens `app`'s window on the compositor at `$XDG_RUNTIME_DIR/
/// $WAYLAND_DISPLAY` (`wayland-0` when unset) and runs it until it closes.
pub fn run<A: App>(app: A, wake: Wake) -> anyhow::Result<()> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR isn't set")?;
    let display = std::env::var_os("WAYLAND_DISPLAY").unwrap_or_else(|| "wayland-0".into());
    run_on(&Path::new(&dir).join(display), app, wake)
}

/// [`run`] on the compositor socket at `socket`.
pub fn run_on<A: App>(socket: &Path, mut app: A, mut wake: Wake) -> anyhow::Result<()> {
    let stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    let connection = Connection::from_socket(stream).context("starting the connection")?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    let _registry = connection.display().get_registry(&qh, ());
    let mut state = State::default();
    queue.roundtrip(&mut state).context("listing globals")?;
    let (Some(compositor), Some(shm), Some(wm_base)) = (
        state.compositor.clone(),
        state.shm.clone(),
        state.wm_base.clone(),
    ) else {
        return Err(anyhow!(
            "the compositor lacks wl_compositor, wl_shm or xdg_wm_base"
        ));
    };

    let (width, height) = app.size();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title(app.title());
    toplevel.set_app_id(app.app_id());
    surface.commit();
    while !state.configured {
        queue
            .blocking_dispatch(&mut state)
            .context("waiting to be configured")?;
    }

    let mut buffers = Buffers::new(&shm, &qh, (width, height))?;
    let mut screen = Screen::new(Size::new(width as f32, height as f32), 1.0);
    screen.set_backdrop(app.background().repeat(width as usize * height as usize));
    let mut title = app.title();
    let mut dirty = true;
    let mut frame_pending = false;

    loop {
        queue
            .dispatch_pending(&mut state)
            .context("talking to the compositor")?;
        for index in state.released.drain(..) {
            buffers.busy[index] = false;
        }
        // Input, as the UI takes it, and the keys the app takes first.
        let events = std::mem::take(&mut state.events);
        let mut ui_events = Vec::with_capacity(events.len());
        for event in events {
            match event {
                Input::Key(press) if app.key(&press) => dirty = true,
                Input::Key(press) => ui_events.extend(press_events(&press)),
                Input::Ui(event) => ui_events.push(event),
            }
        }
        if !ui_events.is_empty() {
            let updated = screen.update(app.view(), &ui_events, state.pointer);
            dirty |= updated.redraw || !updated.messages.is_empty();
            dirty |= ui_events
                .iter()
                .any(|e| !matches!(e, Event::Mouse(mouse::Event::CursorMoved { .. })));
            for message in updated.messages {
                app.update(message);
            }
        }
        if state.close_asked {
            if app.close() {
                break;
            }
            state.close_asked = false;
        }
        if app.done() {
            break;
        }
        if state.frame_done {
            state.frame_done = false;
            frame_pending = false;
        }
        if app.title() != title {
            title = app.title();
            toplevel.set_title(title.clone());
        }
        if dirty
            && !frame_pending
            && let Some(index) = buffers.free()
        {
            let damage = screen.render(app.view(), state.pointer);
            buffers.write(index, screen.bgra())?;
            surface.attach(Some(&buffers.wl[index]), 0, 0);
            for d in &damage {
                surface.damage_buffer(d.x as i32, d.y as i32, d.width as i32, d.height as i32);
            }
            surface.frame(&qh, ());
            surface.commit();
            buffers.busy[index] = true;
            frame_pending = true;
            dirty = false;
        }
        connection.flush().context("talking to the compositor")?;

        // Wait for the compositor, a wake-up, a key to repeat or a caret.
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let next = [state.repeat_at(), screen.next_redraw()]
            .into_iter()
            .flatten()
            .min();
        let timeout = next.map(|at| {
            let wait = at.saturating_duration_since(Instant::now());
            rustix::event::Timespec {
                tv_sec: wait.as_secs() as _,
                tv_nsec: wait.subsec_nanos() as _,
            }
        });
        let ready = {
            let connection_fd = guard.connection_fd();
            let mut fds = [
                PollFd::new(&connection_fd, PollFlags::IN),
                PollFd::new(&wake.reader, PollFlags::IN),
            ];
            match poll(&mut fds, timeout.as_ref()) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(anyhow!("waiting: {e}")),
            }
            (!fds[0].revents().is_empty(), !fds[1].revents().is_empty())
        };
        if ready.0 {
            guard.read().context("reading from the compositor")?;
        } else {
            drop(guard);
        }
        if ready.1 && wake.drain() && app.woken() {
            dirty = true;
        }
        if let Some(press) = state.repeat_due() {
            state.events.push(Input::Key(press));
        }
        if screen.next_redraw().is_some_and(|at| at <= Instant::now()) {
            dirty = true;
        }
    }
    toplevel.destroy();
    xdg.destroy();
    surface.destroy();
    let _ = connection.flush();
    Ok(())
}

/// The iced events for a key press the app left to the UI.
fn press_events(press: &KeyPress) -> Vec<Event> {
    let key = iced_key(press.keysym, &press.text);
    let text = (!press.text.is_empty() && !press.text.chars().any(char::is_control))
        .then(|| press.text.clone().into());
    vec![Event::Keyboard(keyboard::Event::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        location: Location::Standard,
        modifiers: press.modifiers,
        text,
        repeat: press.repeat,
    })]
}

/// iced's key for a keysym, or the text it types.
pub fn iced_key(keysym: xkb::Keysym, text: &str) -> Key {
    let named = match keysym {
        xkb::Keysym::Return | xkb::Keysym::KP_Enter => Named::Enter,
        xkb::Keysym::BackSpace => Named::Backspace,
        xkb::Keysym::Tab | xkb::Keysym::ISO_Left_Tab => Named::Tab,
        xkb::Keysym::Escape => Named::Escape,
        xkb::Keysym::Delete => Named::Delete,
        xkb::Keysym::Up => Named::ArrowUp,
        xkb::Keysym::Down => Named::ArrowDown,
        xkb::Keysym::Left => Named::ArrowLeft,
        xkb::Keysym::Right => Named::ArrowRight,
        xkb::Keysym::Home => Named::Home,
        xkb::Keysym::End => Named::End,
        xkb::Keysym::Page_Up => Named::PageUp,
        xkb::Keysym::Page_Down => Named::PageDown,
        xkb::Keysym::space => Named::Space,
        _ => return Key::Character(text.to_lowercase().into()),
    };
    Key::Named(named)
}

/// The window's two shared-memory buffers: one can be drawn while the
/// compositor still reads the other.
struct Buffers {
    files: Vec<File>,
    wl: Vec<wl_buffer::WlBuffer>,
    busy: Vec<bool>,
}

impl Buffers {
    fn new(
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<State>,
        (w, h): (u32, u32),
    ) -> anyhow::Result<Self> {
        let size = (w * h * 4) as usize;
        let (mut files, mut wl) = (Vec::new(), Vec::new());
        for index in 0..2 {
            let file = tempfile::tempfile().context("a window buffer")?;
            file.set_len(size as u64).context("a window buffer")?;
            let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
            wl.push(pool.create_buffer(
                0,
                w as i32,
                h as i32,
                (w * 4) as i32,
                wl_shm::Format::Argb8888,
                qh,
                index,
            ));
            pool.destroy();
            files.push(file);
        }
        Ok(Self {
            files,
            wl,
            busy: vec![false; 2],
        })
    }

    fn free(&self) -> Option<usize> {
        self.busy.iter().position(|busy| !busy)
    }

    fn write(&self, index: usize, pixels: &[u8]) -> anyhow::Result<()> {
        self.files[index]
            .write_all_at(pixels, 0)
            .context("filling a window buffer")
    }
}

enum Input {
    Ui(Event),
    Key(KeyPress),
}

#[derive(Default)]
struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    configured: bool,
    close_asked: bool,
    frame_done: bool,
    /// Buffers the compositor gave back.
    released: Vec<usize>,
    pointer: Option<Point>,
    events: Vec<Input>,
    xkb: Option<(xkb::Keymap, xkb::State)>,
    /// The key held down, its press, and when it next repeats.
    held: Option<(u32, KeyPress, Instant)>,
    /// Keys per second and the wait before the first repeat.
    repeat: Option<(i32, Duration)>,
}

impl State {
    fn modifiers(&self) -> Modifiers {
        let Some((_, xkb)) = &self.xkb else {
            return Modifiers::empty();
        };
        let on = |name| xkb.mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE);
        let mut modifiers = Modifiers::empty();
        modifiers.set(Modifiers::SHIFT, on(xkb::MOD_NAME_SHIFT));
        modifiers.set(Modifiers::CTRL, on(xkb::MOD_NAME_CTRL));
        modifiers.set(Modifiers::ALT, on(xkb::MOD_NAME_ALT));
        modifiers.set(Modifiers::LOGO, on(xkb::MOD_NAME_LOGO));
        modifiers
    }

    fn repeat_at(&self) -> Option<Instant> {
        self.held.as_ref().map(|(_, _, at)| *at)
    }

    /// The held key's next repeat, if it's due.
    fn repeat_due(&mut self) -> Option<KeyPress> {
        let (rate, _) = self.repeat?;
        let (_, press, at) = self.held.as_mut()?;
        if *at > Instant::now() || rate <= 0 {
            return None;
        }
        *at = Instant::now() + Duration::from_millis(1000 / rate as u64);
        Some(KeyPress {
            repeat: true,
            ..press.clone()
        })
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => state.wm_base = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => state.seat = Some(registry.bind(name, version.min(5), qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        xdg: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            state.configured = true;
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for State {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Close = event {
            state.close_asked = true;
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.frame_done = true;
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize> for State {
    fn event(
        state: &mut Self,
        _: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            state.released.push(*index);
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
            if capabilities.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mouse_event = match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            }
            | wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                let at = Point::new(surface_x as f32, surface_y as f32);
                state.pointer = Some(at);
                mouse::Event::CursorMoved { position: at }
            }
            wl_pointer::Event::Leave { .. } => {
                state.pointer = None;
                mouse::Event::CursorLeft
            }
            wl_pointer::Event::Button {
                button,
                state: pressed,
                ..
            } => {
                let button = match button {
                    0x110 => mouse::Button::Left,
                    0x111 => mouse::Button::Right,
                    0x112 => mouse::Button::Middle,
                    _ => return,
                };
                if pressed == WEnum::Value(wl_pointer::ButtonState::Pressed) {
                    mouse::Event::ButtonPressed(button)
                } else {
                    mouse::Event::ButtonReleased(button)
                }
            }
            wl_pointer::Event::Axis {
                axis: WEnum::Value(wl_pointer::Axis::VerticalScroll),
                value,
                ..
            } => mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines {
                    x: 0.0,
                    y: -(value as f32) / 10.0,
                },
            },
            _ => return,
        };
        state.events.push(Input::Ui(Event::Mouse(mouse_event)));
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                if format == WEnum::Value(wl_keyboard::KeymapFormat::XkbV1) {
                    state.xkb = keymap(fd, size as usize);
                }
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some((_, xkb)) = &mut state.xkb {
                    xkb.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                state.repeat = Some((rate, Duration::from_millis(delay.max(0) as u64)));
            }
            wl_keyboard::Event::Key {
                key,
                state: pressed,
                ..
            } => {
                let down = pressed == WEnum::Value(wl_keyboard::KeyState::Pressed);
                if !down {
                    if state.held.as_ref().is_some_and(|(held, _, _)| *held == key) {
                        state.held = None;
                    }
                    return;
                }
                let Some((keymap, xkb)) = &state.xkb else {
                    return;
                };
                // xkb key codes are evdev's plus 8.
                let code = xkb::Keycode::new(key + 8);
                let keysym = xkb.key_get_one_sym(code);
                // Shift and the like only change other keys (`modifiers`).
                if keysym.is_modifier_key() {
                    return;
                }
                let press = KeyPress {
                    keysym,
                    text: xkb.key_get_utf8(code),
                    modifiers: state.modifiers(),
                    repeat: false,
                };
                let delay = state.repeat.map_or(Duration::from_millis(500), |(_, d)| d);
                state.held = keymap
                    .key_repeats(code)
                    .then(|| (key, press.clone(), Instant::now() + delay));
                state.events.push(Input::Key(press));
            }
            wl_keyboard::Event::Leave { .. } => state.held = None,
            _ => {}
        }
    }
}

/// The compositor's keymap, read from the descriptor it sent (as text: no
/// mapping of the file needed).
fn keymap(fd: OwnedFd, size: usize) -> Option<(xkb::Keymap, xkb::State)> {
    // From the start, whatever the shared descriptor's offset.
    let mut bytes = vec![0; size];
    File::from(fd).read_exact_at(&mut bytes, 0).ok()?;
    let text = String::from_utf8_lossy(&bytes)
        .trim_end_matches('\0')
        .to_owned();
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_string(
        &context,
        text,
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )?;
    let state = xkb::State::new(&keymap);
    Some((keymap, state))
}

delegate_noop!(State: wl_compositor::WlCompositor);
delegate_noop!(State: wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_surface::WlSurface);
