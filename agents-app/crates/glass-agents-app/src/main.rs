//! Glasshouse Agents' desktop app (ADR 0065 §12, ADR 0071): the Agents web
//! app in a small native window for Mac and Windows, and what a browser
//! can't do for it. It signs in as `glass login` does (ADR 0067) and keeps
//! the token in the system's keychain; the tray says who needs the person,
//! and the system's notifications tell them when an agent asks or finishes.
//!
//! `glass-agents-app [--url <address>] [--web <dir>]`: `--url` is the
//! company's Glasshouse Agents (IT sets it when it installs the app); `--web`
//! serves the web app from `apps/agents/dist` instead of the copy built in.

mod pages;
mod session;
mod watch;
mod web;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use glass_agents_api as api;
use glass_gateway_providers::agents::AgentsClient;
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};
use wry::WebViewBuilder;

use crate::session::{Shared, State};
use crate::watch::{News, Seen};
use crate::web::Ctx;

/// What the app's threads and menus tell the window.
#[derive(Debug)]
pub enum UserEvent {
    /// Signed in, out, or set up: load the root page again.
    Reload,
    /// How many questions wait on the person.
    Waiting(usize),
    /// Who the person is, once Glasshouse Agents says.
    Named(Option<String>),
    Menu(String),
    Show,
}

const OPEN: &str = "open";
const SIGN_OUT: &str = "sign-out";
const QUIT: &str = "quit";

fn main() -> anyhow::Result<()> {
    let mut url = std::env::var("GLASS_AGENTS_URL").ok();
    let mut web = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--url" => url = Some(args.next().context("--url takes the address")?),
            "--web" => web = Some(PathBuf::from(args.next().context("--web takes a folder")?)),
            other => {
                anyhow::bail!("{other}? usage: glass-agents-app [--url <address>] [--web <dir>]")
            }
        }
    }
    session::use_system_keychain()?;
    let mut state = State::default();
    match url.map(|u| session::tidy_address(&u)).transpose() {
        Ok(Some(address)) => state.address = Some(address),
        Ok(None) => state.address = session::saved_address(),
        Err(said) => state.note = Some(said),
    }
    if let Some(address) = &state.address {
        state.token = session::token_for(address)?;
        eprintln!(
            "glass-agents-app: {address}: {}",
            if state.token.is_some() {
                "signed in, from this computer's keychain"
            } else {
                "not signed in"
            }
        );
    }
    let shared = Shared::new(state);

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let ctx = Ctx::new(shared.clone(), event_loop.create_proxy(), web);
    {
        let ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| ctx.send(UserEvent::Menu(e.id.0))));
    }
    {
        let ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if matches!(
                e,
                TrayIconEvent::Click { .. } | TrayIconEvent::DoubleClick { .. }
            ) {
                ctx.send(UserEvent::Show);
            }
        }));
    }

    let window = WindowBuilder::new()
        .with_title("Glasshouse Agents")
        .with_inner_size(LogicalSize::new(1200.0, 820.0))
        .with_min_inner_size(LogicalSize::new(420.0, 520.0))
        .with_window_icon(tao::window::Icon::from_rgba(mark(64, false), 64, 64).ok())
        .build(&event_loop)?;

    // The window's menu: Linux and Mac (Windows keeps to the tray's).
    let who = MenuItem::with_id("who", "Not signed in", false, None);
    let window_menu = Menu::new();
    let account = Submenu::new("Glasshouse Agents", true);
    account.append_items(&[
        &who,
        &MenuItem::with_id(SIGN_OUT, "Sign out", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(QUIT, "Quit Glasshouse Agents", true, None),
    ])?;
    window_menu.append(&account)?;
    #[cfg(target_os = "macos")]
    {
        // Typing in the page needs the Edit menu's shortcuts on a Mac.
        let edit = Submenu::new("Edit", true);
        edit.append_items(&[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::select_all(None),
        ])?;
        window_menu.append(&edit)?;
        window_menu.init_for_nsapp();
    }
    #[cfg(target_os = "linux")]
    {
        use tao::platform::unix::WindowExtUnix;
        window_menu.init_for_gtk_window(window.gtk_window(), window.default_vbox())?;
    }

    let builder = {
        let ctx = ctx.clone();
        WebViewBuilder::new()
            .with_asynchronous_custom_protocol("agents".into(), move |_id, request, responder| {
                ctx.serve(request, responder)
            })
            .with_url(web::HOME)
            // Anything that isn't the app's own opens in the browser.
            .with_navigation_handler(|url: String| {
                let ours = url.starts_with(web::HOME) || url == "about:blank";
                if !ours {
                    web::open_in_browser(&url);
                }
                ours
            })
            .with_new_window_req_handler(|url: String, _features| {
                web::open_in_browser(&url);
                wry::NewWindowResponse::Deny
            })
    };
    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder.build_gtk(
            window
                .default_vbox()
                .context("the window has no box for the page")?,
        )?
    };
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window)?;

    {
        let ctx = ctx.clone();
        std::thread::spawn(move || follow(&ctx));
    }

    // The tray's menu: what waits, open, sign out, quit.
    let waiting = MenuItem::with_id("waiting", watch::waiting_words(0), false, None);
    let tray_menu = Menu::new();
    tray_menu.append_items(&[
        &MenuItem::with_id(OPEN, "Open Glasshouse Agents", true, None),
        &waiting,
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(format!("tray-{SIGN_OUT}"), "Sign out", true, None),
        &MenuItem::with_id(format!("tray-{QUIT}"), "Quit", true, None),
    ])?;
    let mut tray: Option<TrayIcon> = None;
    let mut tray_menu = Some(tray_menu);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            // A tray is made once the event loop runs (a Mac's rule).
            Event::NewEvents(StartCause::Init) => {
                let mut builder = TrayIconBuilder::new().with_tooltip("Glasshouse Agents");
                if let Ok(icon) = tray_icon::Icon::from_rgba(mark(32, false), 32, 32) {
                    builder = builder.with_icon(icon);
                }
                if let Some(menu) = tray_menu.take() {
                    builder = builder.with_menu(Box::new(menu));
                }
                match builder.build() {
                    Ok(t) => tray = Some(t),
                    Err(e) => eprintln!("glass-agents-app: no tray on this desktop ({e})"),
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                // With a tray, closing hides the window, as menu-bar apps do.
                // Linux desktops don't all show trays: there it quits.
                if cfg!(target_os = "linux") || tray.is_none() {
                    *control_flow = ControlFlow::Exit;
                } else {
                    window.set_visible(false);
                }
            }
            Event::UserEvent(UserEvent::Reload) => {
                if let Err(e) = webview.load_url(web::HOME) {
                    eprintln!("glass-agents-app: {e}");
                }
            }
            Event::UserEvent(UserEvent::Waiting(n)) => {
                waiting.set_text(watch::waiting_words(n));
                if let Some(tray) = &tray {
                    let _ = tray.set_icon(tray_icon::Icon::from_rgba(mark(32, n > 0), 32, 32).ok());
                    let _ = tray.set_tooltip(Some(format!(
                        "Glasshouse Agents: {}",
                        watch::waiting_words(n)
                    )));
                    tray.set_title((n > 0).then(|| n.to_string()));
                }
            }
            Event::UserEvent(UserEvent::Named(name)) => who.set_text(match name {
                Some(name) => format!("Signed in as {name}"),
                None => "Not signed in".into(),
            }),
            Event::UserEvent(UserEvent::Show) => {
                window.set_visible(true);
                window.set_focus();
            }
            Event::UserEvent(UserEvent::Menu(id)) => match id.trim_start_matches("tray-") {
                OPEN => {
                    window.set_visible(true);
                    window.set_focus();
                }
                SIGN_OUT => {
                    let ctx = ctx.clone();
                    std::thread::spawn(move || ctx.sign_out());
                }
                QUIT => *control_flow = ControlFlow::Exit,
                _ => {}
            },
            _ => {}
        }
    })
}

/// The Glasshouse mark, drawn: a blue tile with a window's four panes,
/// and an orange dot when something waits on the person. RGBA, `size`
/// pixels square.
fn mark(size: u32, waiting: bool) -> Vec<u8> {
    let s = size as f32;
    let radius = s * 0.22;
    let bar = (s / 16.0).max(1.5);
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Inside the rounded tile?
            let cx = fx.clamp(radius, s - radius);
            let cy = fy.clamp(radius, s - radius);
            let inside = (fx - cx).powi(2) + (fy - cy).powi(2) <= radius * radius;
            let pane = (fx - s / 2.0).abs() < bar / 2.0 || (fy - s / 2.0).abs() < bar / 2.0;
            let frame = fx < s * 0.2 + bar
                || fx > s * 0.8 - bar
                || fy < s * 0.2 + bar
                || fy > s * 0.8 - bar;
            let in_window = fx > s * 0.2 && fx < s * 0.8 && fy > s * 0.2 && fy < s * 0.8;
            let dot =
                waiting && (fx - s * 0.8).powi(2) + (fy - s * 0.2).powi(2) <= (s * 0.2).powi(2);
            let pixel: [u8; 4] = if dot {
                [245, 138, 31, 255]
            } else if !inside {
                [0, 0, 0, 0]
            } else if in_window && (pane || frame) {
                [255, 255, 255, 255]
            } else {
                [47, 84, 235, 255]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    rgba
}

/// Follows the person's events as the web app does, and after each change
/// looks at what waits on them: the tray's count and the notifications.
fn follow(ctx: &Ctx) {
    let mut seen = Seen::default();
    let mut cursor = 0u64;
    let mut following: Option<String> = None;
    loop {
        let Some((address, token)) = ctx.shared.signed_in() else {
            if following.take().is_some() {
                ctx.send(UserEvent::Waiting(0));
                ctx.send(UserEvent::Named(None));
            }
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };
        let Ok(client) = AgentsClient::new(&address, &token) else {
            std::thread::sleep(Duration::from_secs(5));
            continue;
        };
        if following.as_deref() != Some(token.as_str()) {
            seen = Seen::default();
            cursor = 0;
            following = Some(token.clone());
            if let Some(session) = get::<api::Session>(&client, "session", ctx, &token) {
                ctx.shared.lock().name = Some(session.person.name.clone());
                ctx.send(UserEvent::Named(Some(session.person.name)));
            }
        }
        let asks = get::<Vec<api::PendingApproval>>(&client, "approvals", ctx, &token);
        let agents = get::<Vec<api::AgentView>>(&client, "agents", ctx, &token);
        if let (Some(asks), Some(agents)) = (asks, agents) {
            for news in seen.look(&asks, &agents) {
                tell(&news);
            }
            ctx.send(UserEvent::Waiting(asks.len()));
        }
        // Until something changes: the front answers within 25 seconds.
        match get::<api::Events>(&client, &format!("events?since={cursor}"), ctx, &token) {
            Some(events) => cursor = events.cursor,
            None => std::thread::sleep(Duration::from_secs(5)),
        }
    }
}

/// `GET /api/v1/<path>`: a refused token signs the app out.
fn get<T: serde::de::DeserializeOwned>(
    client: &AgentsClient,
    path: &str,
    ctx: &Ctx,
    token: &str,
) -> Option<T> {
    let answer = client.get(path).ok()?;
    if answer.status == 401 {
        ctx.shared.sign_out(
            Some(token),
            Some("Your sign-in ran out, or you signed out somewhere else. Sign in again.".into()),
        );
        ctx.send(UserEvent::Reload);
        return None;
    }
    if !(200..300).contains(&answer.status) {
        return None;
    }
    serde_json::from_str(&answer.body).ok()
}

/// A notification from the system, and a line in the app's log.
fn tell(news: &News) {
    let (title, body) = news.words();
    eprintln!("glass-agents-app: notifying: {title}: {body}");
    if let Err(e) = notify_rust::Notification::new()
        .appname("Glasshouse Agents")
        .summary(&title)
        .body(&body)
        .show()
    {
        eprintln!("glass-agents-app: no notification shown ({e})");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr0071_the_mark_shows_a_dot_only_when_something_waits() {
        let quiet = mark(32, false);
        let waiting = mark(32, true);
        assert_eq!(quiet.len(), 32 * 32 * 4);
        let at =
            |rgba: &[u8], x: usize, y: usize| rgba[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4].to_vec();
        // The corner outside the rounded tile is clear; the middle is a pane bar.
        assert_eq!(at(&quiet, 0, 0), [0, 0, 0, 0]);
        assert_eq!(at(&quiet, 16, 16), [255, 255, 255, 255]);
        assert_eq!(at(&waiting, 25, 6), [245, 138, 31, 255]);
        assert_ne!(at(&quiet, 25, 6), [245, 138, 31, 255]);
    }
}
