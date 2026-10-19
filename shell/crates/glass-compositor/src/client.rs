//! A minimal Wayland client: one window filled with a color, reporting the
//! input it gets. The compositor's tests drive it, and `glass-hello` shows
//! it on a screen to check a compositor by eye.

use std::fs::File;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, anyhow};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

/// The window to open.
#[derive(Debug, Clone)]
pub struct Spec {
    pub title: String,
    pub app_id: String,
    pub size: (u32, u32),
    /// Every pixel, as `0xAARRGGBB`.
    pub color: u32,
}

/// What the window was told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The compositor configured it; its first buffer is attached.
    Shown,
    /// It has the keyboard.
    Focused,
    PointerEntered {
        x: i32,
        y: i32,
    },
    PointerLeft,
    Button {
        button: u32,
        pressed: bool,
    },
    Key {
        code: u32,
        pressed: bool,
    },
    /// The vertical axis moved by this much (positive: down).
    Scrolled(i32),
    /// The compositor asked it to close.
    CloseAsked,
    /// The compositor asked for this size (a configure with one).
    Sized {
        width: i32,
        height: i32,
    },
    /// The compositor said how big it may be (`configure_bounds`).
    Bounded {
        width: i32,
        height: i32,
    },
}

/// A window's client, running on a thread of its own.
pub struct Running {
    stop: Arc<AtomicBool>,
    pub events: Receiver<Event>,
    thread: Option<JoinHandle<anyhow::Result<()>>>,
}

impl Running {
    /// Asks the client to disconnect. It talks to the compositor on its way
    /// out, so keep the compositor running until [`Running::is_done`].
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Whether the client's thread has ended.
    pub fn is_done(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// The client's outcome, once it's done (or waiting for it).
    pub fn join(mut self) -> anyhow::Result<()> {
        match self.thread.take().map(JoinHandle::join) {
            Some(Ok(result)) => result,
            Some(Err(_)) => Err(anyhow!("the client thread panicked")),
            None => Ok(()),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Connects to the compositor at `socket` and opens `spec`'s window on a
/// thread; its events arrive on [`Running::events`].
pub fn spawn(socket: PathBuf, spec: Spec) -> Running {
    let (send, events) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let thread = std::thread::spawn(move || run(&socket, &spec, &send, Some(&stopping)));
    Running {
        stop,
        events,
        thread: Some(thread),
    }
}

/// Connects to the compositor at `socket`, opens `spec`'s window and runs
/// until `stop` is set, the window is asked to close or the compositor
/// goes; without `stop`, until one of the others.
pub fn run(
    socket: &std::path::Path,
    spec: &Spec,
    events: &Sender<Event>,
    stop: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    let stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    let connection = Connection::from_socket(stream).context("starting the connection")?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    let _registry = connection.display().get_registry(&qh, ());
    let mut app = App {
        events: Some(events.clone()),
        ..App::default()
    };
    queue.roundtrip(&mut app).context("listing globals")?;
    let (Some(compositor), Some(shm), Some(wm_base)) =
        (app.compositor.clone(), app.shm.clone(), app.wm_base.clone())
    else {
        return Err(anyhow!(
            "the compositor lacks wl_compositor, wl_shm or xdg_wm_base"
        ));
    };

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title(spec.title.clone());
    toplevel.set_app_id(spec.app_id.clone());
    surface.commit();
    while !app.configured {
        queue
            .blocking_dispatch(&mut app)
            .context("waiting to be configured")?;
    }

    let (width, height) = spec.size;
    let stride = width * 4;
    let mut file = tempfile::tempfile().context("a buffer file")?;
    write_pixels(&mut file, spec.color, (width * height) as usize)?;
    let pool = shm.create_pool(file.as_fd(), (stride * height) as i32, &qh, ());
    let buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        stride as i32,
        wl_shm::Format::Argb8888,
        &qh,
        (),
    );
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, width as i32, height as i32);
    surface.commit();
    let _ = events.send(Event::Shown);

    loop {
        if stop.is_some_and(|s| s.load(Ordering::Relaxed)) || app.close_asked {
            break;
        }
        match stop {
            // Checked again every few milliseconds.
            Some(_) => {
                queue
                    .roundtrip(&mut app)
                    .context("talking to the compositor")?;
                std::thread::sleep(Duration::from_millis(5));
            }
            None => {
                queue
                    .blocking_dispatch(&mut app)
                    .context("talking to the compositor")?;
            }
        }
    }
    toplevel.destroy();
    xdg.destroy();
    surface.destroy();
    buffer.destroy();
    pool.destroy();
    let _ = connection.flush();
    Ok(())
}

fn write_pixels(file: &mut File, color: u32, count: usize) -> anyhow::Result<()> {
    let row: Vec<u8> = std::iter::repeat_n(color.to_le_bytes(), count)
        .flatten()
        .collect();
    file.write_all(&row).context("filling the buffer")?;
    file.flush().context("filling the buffer")?;
    Ok(())
}

#[derive(Default)]
struct App {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    configured: bool,
    close_asked: bool,
    events: Option<Sender<Event>>,
}

impl App {
    fn tell(&self, event: Event) {
        if let Some(events) = &self.events {
            let _ = events.send(event);
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        app: &mut Self,
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
                    app.compositor = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_shm" => app.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => app.wm_base = Some(registry.bind(name, version.min(4), qh, ())),
                "wl_seat" => app.seat = Some(registry.bind(name, version.min(5), qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for App {
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

impl Dispatch<xdg_surface::XdgSurface, ()> for App {
    fn event(
        app: &mut Self,
        xdg: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            app.configured = true;
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for App {
    fn event(
        app: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Close => {
                app.close_asked = true;
                app.tell(Event::CloseAsked);
            }
            xdg_toplevel::Event::Configure { width, height, .. } if width > 0 && height > 0 => {
                app.tell(Event::Sized { width, height });
            }
            xdg_toplevel::Event::ConfigureBounds { width, height } => {
                app.tell(Event::Bounded { width, height });
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
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

impl Dispatch<wl_pointer::WlPointer, ()> for App {
    fn event(
        app: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => app.tell(Event::PointerEntered {
                x: surface_x as i32,
                y: surface_y as i32,
            }),
            wl_pointer::Event::Leave { .. } => app.tell(Event::PointerLeft),
            wl_pointer::Event::Button { button, state, .. } => app.tell(Event::Button {
                button,
                pressed: state == WEnum::Value(wl_pointer::ButtonState::Pressed),
            }),
            wl_pointer::Event::Axis {
                axis: WEnum::Value(wl_pointer::Axis::VerticalScroll),
                value,
                ..
            } => app.tell(Event::Scrolled(value as i32)),
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for App {
    fn event(
        app: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { .. } => app.tell(Event::Focused),
            wl_keyboard::Event::Key { key, state, .. } => app.tell(Event::Key {
                code: key,
                pressed: state == WEnum::Value(wl_keyboard::KeyState::Pressed),
            }),
            _ => {}
        }
    }
}

delegate_noop!(App: wl_compositor::WlCompositor);
delegate_noop!(App: wl_shm_pool::WlShmPool);
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore wl_buffer::WlBuffer);
delegate_noop!(App: ignore wl_surface::WlSurface);
