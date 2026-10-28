//! What the window loads, from the app's own `agents:` scheme (wry's custom
//! protocol): the Agents web app, embedded; its API calls, passed on to the
//! person's Glasshouse Agents with their token, which the page never sees;
//! and the app's own pages (pages.rs), whose forms post back here.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use glass_gateway_providers::agents::{AgentsClient, Client, SignIn};
use tao::event_loop::EventLoopProxy;
use wry::RequestAsyncResponder;
use wry::http::{Request, Response};

use crate::UserEvent;
use crate::pages;
use crate::session::{self, Shared};

static INDEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/index.html"));
static SCRIPT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/app.js"));
static STYLE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/app.css"));

/// As the front serves the web app (glass-agents), the scheme as `'self'`.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/// How long a sign-in waits for the browser, as `glass login` does.
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);

/// The window's first page: the scheme's root (an `http://<scheme>.localhost`
/// origin on Windows, as WebView2 serves custom schemes).
pub const HOME: &str = if cfg!(target_os = "windows") {
    "http://agents.localhost/"
} else {
    "agents://localhost/"
};

/// Where a request goes.
#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    /// The web app's page, which its own routes all load; or, signed out,
    /// one of the app's pages.
    Page,
    Script,
    Style,
    /// `/api/v1/<path>`, its query included.
    Api(String),
    Address,
    SignIn,
    SignOut,
    Missing,
}

pub fn route(method: &str, path: &str, query: Option<&str>) -> Route {
    match (method, path) {
        ("GET", "/" | "/index.html" | "/new") => Route::Page,
        ("GET", p) if p.starts_with("/agents/") => Route::Page,
        ("GET", "/app.js") => Route::Script,
        ("GET", "/app.css") => Route::Style,
        ("POST", "/_app/address") => Route::Address,
        ("POST", "/_app/sign-in") => Route::SignIn,
        ("POST", "/_app/sign-out") => Route::SignOut,
        (_, p) if p.starts_with("/api/v1/") && p.len() > "/api/v1/".len() => {
            let rest = &p["/api/v1/".len()..];
            Route::Api(match query {
                Some(q) if !q.is_empty() => format!("{rest}?{q}"),
                _ => rest.to_owned(),
            })
        }
        _ => Route::Missing,
    }
}

/// A field of a form's body (`application/x-www-form-urlencoded`).
pub fn form_field(body: &[u8], name: &str) -> Option<String> {
    let body = String::from_utf8_lossy(body);
    body.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (decode(k) == name).then(|| decode(v))
    })
}

fn decode(text: &str) -> String {
    let hex = |b: Option<&u8>| b.and_then(|b| (*b as char).to_digit(16)).map(|d| d as u8);
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], hex(bytes.get(i + 1)), hex(bytes.get(i + 2))) {
            (b'+', _, _) => out.push(b' '),
            (b'%', Some(high), Some(low)) => {
                out.push(high * 16 + low);
                i += 2;
            }
            (b, _, _) => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn respond(
    status: u16,
    content_type: &str,
    body: impl Into<Cow<'static, [u8]>>,
) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .header("Content-Type", content_type)
        .header("Content-Security-Policy", CSP)
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "no-store")
        .body(body.into())
        .unwrap_or_else(|_| Response::new(Cow::Borrowed(&b""[..])))
}

fn html(text: String) -> Response<Cow<'static, [u8]>> {
    respond(200, "text/html; charset=utf-8", text.into_bytes())
}

fn json(status: u16, text: String) -> Response<Cow<'static, [u8]>> {
    respond(status, "application/json", text.into_bytes())
}

fn error_json(status: u16, said: &str) -> Response<Cow<'static, [u8]>> {
    json(status, serde_json::json!({ "error": said }).to_string())
}

/// Back to the app's root after a form. WebKitGTK follows neither a custom
/// scheme's redirect nor this page's refresh, so each form's handler also
/// has the window load the root itself (`UserEvent::Reload`).
fn back_home() -> Response<Cow<'static, [u8]>> {
    html(
        "<!doctype html><meta charset=\"utf-8\"><meta http-equiv=\"refresh\" content=\"0;url=/\">\
         <title>Glasshouse Agents</title>"
            .into(),
    )
}

/// Opens `url` in the person's browser: the company's sign-in happens
/// there, never in the app. `GLASS_AGENTS_APP_BROWSER` names another
/// program to hand it to (development and tests).
pub fn open_in_browser(url: &str) {
    let spawned = if let Ok(program) = std::env::var("GLASS_AGENTS_APP_BROWSER") {
        std::process::Command::new(program).arg(url).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if let Err(e) = spawned {
        eprintln!("glass-agents-app: no browser opened ({e}); the window shows the address");
    }
}

/// What the scheme's handler shares with the app.
#[derive(Clone)]
pub struct Ctx {
    pub shared: Shared,
    events: Arc<Mutex<EventLoopProxy<UserEvent>>>,
    /// Serves the web app from this folder instead (development).
    web: Option<PathBuf>,
}

impl Ctx {
    pub fn new(shared: Shared, events: EventLoopProxy<UserEvent>, web: Option<PathBuf>) -> Self {
        Self {
            shared,
            events: Arc::new(Mutex::new(events)),
            web,
        }
    }

    pub fn send(&self, event: UserEvent) {
        let events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        let _ = events.send_event(event);
    }

    /// Answers one request from the window. API calls go on a thread of
    /// their own: the events long poll waits up to 25 seconds.
    pub fn serve(&self, request: Request<Vec<u8>>, responder: RequestAsyncResponder) {
        let method = request.method().as_str().to_owned();
        let path = request.uri().path().to_owned();
        let query = request.uri().query().map(str::to_owned);
        match route(&method, &path, query.as_deref()) {
            Route::Page => responder.respond(self.page()),
            Route::Script => {
                responder.respond(self.asset(SCRIPT, "app.js", "text/javascript; charset=utf-8"))
            }
            Route::Style => {
                responder.respond(self.asset(STYLE, "app.css", "text/css; charset=utf-8"))
            }
            Route::Api(rest) => {
                let ctx = self.clone();
                std::thread::spawn(move || {
                    responder.respond(ctx.api(&method, &rest, request.body()))
                });
            }
            Route::Address => responder.respond(self.set_address(request.body())),
            Route::SignIn => responder.respond(self.sign_in()),
            Route::SignOut => {
                let ctx = self.clone();
                std::thread::spawn(move || ctx.sign_out());
                responder.respond(back_home());
            }
            Route::Missing => {
                responder.respond(respond(404, "text/plain; charset=utf-8", &b"not found"[..]))
            }
        }
    }

    fn asset(
        &self,
        embedded: &'static [u8],
        name: &str,
        content_type: &str,
    ) -> Response<Cow<'static, [u8]>> {
        match &self.web {
            Some(dir) => match std::fs::read(dir.join(name)) {
                Ok(bytes) => respond(200, content_type, bytes),
                Err(_) => respond(404, "text/plain; charset=utf-8", &b"not built"[..]),
            },
            None => respond(200, content_type, embedded),
        }
    }

    /// Signed in, the web app; else the page for where the person is.
    fn page(&self) -> Response<Cow<'static, [u8]>> {
        let state = self.shared.lock();
        let Some(address) = &state.address else {
            return html(pages::address(state.note.as_deref()));
        };
        if state.token.is_none() {
            return html(match &state.waiting {
                Some(link) => pages::waiting(link),
                None => pages::sign_in(address, state.note.as_deref()),
            });
        }
        drop(state);
        if self.web.is_none() && (INDEX.is_empty() || SCRIPT.is_empty()) {
            return html(pages::unbuilt());
        }
        self.asset(INDEX, "../index.html", "text/html; charset=utf-8")
    }

    /// The web app's call, made with the person's token. Their answer goes
    /// back as it is, so the page shows Glasshouse Agents' own words.
    fn api(&self, method: &str, rest: &str, body: &[u8]) -> Response<Cow<'static, [u8]>> {
        let Some((address, token)) = self.shared.signed_in() else {
            self.send(UserEvent::Reload);
            return error_json(401, "You're signed out: sign in again.");
        };
        let client = match AgentsClient::new(&address, &token) {
            Ok(client) => client,
            Err(e) => return error_json(502, &e.to_string()),
        };
        match client.send(method, rest, body) {
            Ok(answer) => {
                let signed_out_here =
                    method == "DELETE" && rest == "tokens" && (200..300).contains(&answer.status);
                if answer.status == 401 || signed_out_here {
                    self.shared.sign_out(
                        Some(&token),
                        Some(if signed_out_here {
                            "You signed out: every sign-in of yours stopped working, this one too."
                                .into()
                        } else {
                            "Your sign-in ran out, or you signed out somewhere else. Sign in again."
                                .into()
                        }),
                    );
                    self.send(UserEvent::Reload);
                }
                json(answer.status, answer.body)
            }
            Err(e) => error_json(502, &format!("Glasshouse Agents {e}")),
        }
    }

    fn set_address(&self, body: &[u8]) -> Response<Cow<'static, [u8]>> {
        let typed = form_field(body, "url").unwrap_or_default();
        let mut state = self.shared.lock();
        if typed.trim().is_empty() {
            // "Use another address": the old one's sign-in stays in the
            // keychain, for when the person comes back to it.
            state.address = None;
            state.token = None;
            state.note = None;
            state.waiting = None;
            if let Err(e) = session::save_address(None) {
                state.note = Some(format!("{e:#}"));
            }
            return back_home();
        }
        match session::tidy_address(&typed) {
            Ok(address) => {
                state.note = session::save_address(Some(&address))
                    .err()
                    .map(|e| format!("{e:#}"));
                state.token = session::token_for(&address).unwrap_or_else(|e| {
                    state.note = Some(format!("{e:#}"));
                    None
                });
                state.address = Some(address);
            }
            Err(said) => state.note = Some(said),
        }
        drop(state);
        self.send(UserEvent::Reload);
        back_home()
    }

    /// Signs in as `glass login` does (ADR 0067): the browser, then the
    /// code back to this computer, traded for a token kept in the keychain.
    fn sign_in(&self) -> Response<Cow<'static, [u8]>> {
        let Some(address) = self.shared.lock().address.clone() else {
            return back_home();
        };
        match SignIn::start(&address, Client::App) {
            Err(e) => {
                self.shared.lock().note = Some(e.to_string());
                self.send(UserEvent::Reload);
            }
            Ok(sign_in) => {
                let link = sign_in.page().to_owned();
                eprintln!("glass-agents-app: sign in in your browser: {link}");
                {
                    let mut state = self.shared.lock();
                    state.waiting = Some(link.clone());
                    state.note = None;
                }
                open_in_browser(&link);
                self.send(UserEvent::Reload);
                let ctx = self.clone();
                std::thread::spawn(move || {
                    let done = sign_in.finish(SIGN_IN_WAIT);
                    let mut state = ctx.shared.lock();
                    if state.waiting.as_deref() == Some(link.as_str()) {
                        state.waiting = None;
                    }
                    match done.map_err(|e| e.to_string()).and_then(|token| {
                        session::keep(&address, &token).map_err(|e| format!("{e:#}"))?;
                        Ok(token)
                    }) {
                        Ok(token) if state.address.as_deref() == Some(address.as_str()) => {
                            eprintln!(
                                "glass-agents-app: signed in; the token is in this computer's keychain"
                            );
                            state.token = Some(token);
                            state.note = None;
                        }
                        Ok(_) => {}
                        Err(said) => state.note = Some(said),
                    }
                    drop(state);
                    ctx.send(UserEvent::Reload);
                });
            }
        }
        back_home()
    }

    /// Signs out as `glass logout` does: every token of the person's stops
    /// working, then this one leaves the keychain.
    pub fn sign_out(&self) {
        if let Some((address, token)) = self.shared.signed_in() {
            let revoked = AgentsClient::new(&address, &token)
                .and_then(|client| client.delete("tokens"))
                .map(|answer| (200..300).contains(&answer.status) || answer.status == 401);
            let note = if matches!(revoked, Ok(true)) {
                "You signed out: every sign-in of yours stopped working, this one too."
            } else {
                "You signed out on this computer. Glasshouse Agents didn't answer, so your \
                 sign-ins elsewhere work until they run out."
            };
            self.shared.sign_out(Some(&token), Some(note.into()));
        }
        self.send(UserEvent::Reload);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr0071_the_web_apps_routes_load_its_page_and_its_calls_go_on() {
        assert_eq!(route("GET", "/", None), Route::Page);
        assert_eq!(
            route("GET", "/agents/reconciler/computer", None),
            Route::Page
        );
        assert_eq!(route("GET", "/new", None), Route::Page);
        assert_eq!(route("GET", "/app.js", None), Route::Script);
        assert_eq!(
            route("GET", "/api/v1/events", Some("since=12")),
            Route::Api("events?since=12".into())
        );
        assert_eq!(
            route("POST", "/api/v1/agents/reconciler/ask", None),
            Route::Api("agents/reconciler/ask".into())
        );
        assert_eq!(route("GET", "/api/v1/", None), Route::Missing);
        assert_eq!(route("POST", "/_app/sign-in", None), Route::SignIn);
        // The app's forms only take posts; nothing else is served.
        assert_eq!(route("GET", "/_app/sign-in", None), Route::Missing);
        assert_eq!(route("GET", "/../etc/passwd", None), Route::Missing);
        assert_eq!(route("GET", "/sw.js", None), Route::Missing);
    }

    #[test]
    fn adr0071_a_forms_fields_are_read_as_browsers_send_them() {
        let body = b"url=https%3A%2F%2Fagents.acme.example%2F&x=a+b&bad=%zz&end=%4";
        assert_eq!(
            form_field(body, "url").as_deref(),
            Some("https://agents.acme.example/")
        );
        assert_eq!(form_field(body, "x").as_deref(), Some("a b"));
        assert_eq!(form_field(body, "bad").as_deref(), Some("%zz"));
        assert_eq!(form_field(body, "end").as_deref(), Some("%4"));
        assert_eq!(form_field(b"url=", "url").as_deref(), Some(""));
        assert_eq!(form_field(b"other=1", "url"), None);
    }
}
