//! `glass-app-bridge`: how a web app reaches the system (UI-5, ADR 0024).
//!
//! It runs as the user. For each app it serves it opens one session on
//! glassd's app socket; glassd authorizes that by this process's uid and
//! runs only what the app's manifest declares, through the pipeline,
//! attributed "by [app] for [user]". The bridge decides nothing: it serves
//! the app's files and relays its calls.
//!
//! This first slice has one mode, `--dev`: loopback HTTP that a browser
//! (Playwright's Chromium in the tests) can reach, each app on its own
//! origin, `http://<app>.localhost:<port>`, so one app's page can't call as
//! another.
//!
//! What dev mode trusts that the OS won't: any process on this machine that
//! can reach the loopback port can act as any installed app for the user
//! who ran the bridge, and the Host header names the app. In the OS the app
//! runtime reaches the bridge over the user's own Unix socket and the bridge
//! names the app from the installed copy it launched.

mod http;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, bail};
use clap::Parser;
use glass_api::{AppCallResult, AppManifest, AppSession, AppStatus, app_methods};
use glass_rpc::{Client, ClientError, codes};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use crate::http::{ReadError, Request, Response};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Glasshouse app bridge: relays apps' calls to the system"
)]
struct Args {
    /// Serve apps over loopback HTTP for development and tests. Required:
    /// it is the only mode in this build.
    #[arg(long)]
    dev: bool,
    /// Loopback address to listen on.
    #[arg(long, default_value = "127.0.0.1:7300")]
    listen: SocketAddr,
    /// Installed apps: one folder per app, with manifest.json and dist/.
    #[arg(long, default_value = "/usr/local/share/glasshouse/apps")]
    apps: PathBuf,
    /// glassd's app socket.
    #[arg(long, env = glass_api::APP_SOCKET_ENV, default_value = glass_api::DEFAULT_APP_SOCKET)]
    socket: PathBuf,
}

/// The Content-Security-Policy every page is served with: the app's own
/// files and its own bridge, nothing from anywhere else (UI-5: apps reach
/// the system only through the brokers).
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
     img-src 'self' data:; connect-src 'self'; worker-src 'self'; font-src 'self'; \
     object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

fn main() -> ExitCode {
    tracing_subscriber::fmt().with_target(false).init();
    let args = Args::parse();
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")
        .and_then(|rt| rt.block_on(run(args)));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> anyhow::Result<()> {
    if !args.dev {
        bail!(
            "only --dev is available in this build: apps reach the system through the \
             app runtime, which isn't part of it yet"
        );
    }
    if !args.listen.ip().is_loopback() {
        bail!("--dev serves loopback only; {} isn't", args.listen);
    }
    let apps = installed(&args.apps)?;
    if apps.is_empty() {
        bail!("no apps in {}", args.apps.display());
    }
    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("listening on {}", args.listen))?;
    let port = listener.local_addr()?.port();
    tracing::warn!(
        "development bridge: any process that can reach 127.0.0.1:{port} acts as these apps for this user"
    );
    for id in apps.keys() {
        tracing::info!("http://{id}.localhost:{port}/");
    }
    let bridge = Arc::new(Bridge {
        socket: args.socket,
        port,
        apps,
        sessions: Mutex::new(BTreeMap::new()),
    });
    loop {
        let (stream, _) = listener.accept().await?;
        let bridge = Arc::clone(&bridge);
        tokio::spawn(async move { bridge.serve(stream).await });
    }
}

/// The installed apps, by id: where each one's built files are.
fn installed(dir: &Path) -> anyhow::Result<BTreeMap<String, PathBuf>> {
    let mut apps = BTreeMap::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        let Ok(text) = std::fs::read_to_string(path.join("manifest.json")) else {
            continue;
        };
        // glassd checks and enforces the manifest; the bridge only needs
        // to know the app is there.
        match serde_json::from_str::<AppManifest>(&text) {
            Ok(manifest) => {
                apps.insert(manifest.id.to_string(), path.join("dist"));
            }
            Err(e) => tracing::warn!("{}: {e}", path.display()),
        }
    }
    Ok(apps)
}

struct Bridge {
    socket: PathBuf,
    port: u16,
    apps: BTreeMap<String, PathBuf>,
    /// One session per app, opened on first use.
    sessions: Mutex<BTreeMap<String, Arc<Open>>>,
}

struct Open {
    client: Client,
    session: AppSession,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CallBody {
    action: String,
    #[serde(default)]
    input: Value,
}

/// Why the bridge couldn't relay a call.
enum Relay {
    /// glassd isn't answering: the app shows what it has loaded.
    Down,
    /// glassd refused the request itself (not a refusal of an action).
    Refused(String),
}

impl Bridge {
    async fn serve(&self, mut stream: TcpStream) {
        let (response, head_only) = match http::read_request(&mut stream).await {
            Ok(request) => {
                let head_only = request.method == "HEAD";
                (self.handle(&request).await, head_only)
            }
            Err(ReadError::Bad(status, why)) => (Response::text(status, why), false),
            Err(ReadError::Gone) => return,
        };
        let _ = http::write_response(&mut stream, response, head_only).await;
    }

    async fn handle(&self, request: &Request) -> Response {
        let Some(app) = app_for_host(request.header("host"), self.port, &self.apps) else {
            return Response::text(421, "no app is served at this address");
        };
        if let Some(endpoint) = request.path.strip_prefix("/_bridge/") {
            if let Err((status, why)) = check_call(request, app, self.port) {
                return Response::text(status, why);
            }
            return match self.api(app, endpoint, &request.body).await {
                Ok(value) => Response::json(200, &value),
                Err(Relay::Down) => Response::json(
                    503,
                    &json!({ "error": "The system isn't answering right now." }),
                ),
                Err(Relay::Refused(why)) => Response::json(502, &json!({ "error": why })),
            };
        }
        if request.method != "GET" && request.method != "HEAD" {
            return Response::text(405, "apps' files are read-only");
        }
        self.file(app, &request.path).await
    }

    async fn file(&self, app: &str, path: &str) -> Response {
        let Some(rel) = static_path(path) else {
            return Response::text(404, "not found");
        };
        let root = &self.apps[app];
        let file = root.join(&rel);
        // Built files only: nothing a link inside dist/ points at elsewhere.
        let inside = match (
            tokio::fs::canonicalize(root).await,
            tokio::fs::canonicalize(&file).await,
        ) {
            (Ok(root), Ok(file)) => file.starts_with(root),
            _ => false,
        };
        let body = match inside {
            true => tokio::fs::read(&file).await.ok(),
            false => None,
        };
        let Some(body) = body else {
            return Response::text(404, "not found (build the apps with make apps-build)");
        };
        let mut response =
            Response::new(200, content_type(&rel), body).with("Cache-Control", "no-cache");
        if rel.ends_with(".html") {
            response = response
                .with("Content-Security-Policy", CSP)
                .with("Cross-Origin-Opener-Policy", "same-origin");
        }
        response
    }

    async fn api(&self, app: &str, endpoint: &str, body: &[u8]) -> Result<Value, Relay> {
        match endpoint {
            "session" => {
                let open = self.session(app).await?;
                let mut value = serde_json::to_value(&open.session).unwrap_or_default();
                // The session id stays in the bridge.
                if let Some(map) = value.as_object_mut() {
                    map.remove("session");
                }
                Ok(value)
            }
            "call" => {
                let call: CallBody = serde_json::from_slice(body)
                    .map_err(|e| Relay::Refused(format!("bad call: {e}")))?;
                let result: AppCallResult = self
                    .relay(app, app_methods::APP_CALL, |session| {
                        json!({ "session": session, "action": call.action, "input": call.input })
                    })
                    .await?;
                Ok(serde_json::to_value(result).unwrap_or_default())
            }
            "status" => {
                let status: AppStatus = self
                    .relay(
                        app,
                        app_methods::APP_STATUS,
                        |session| json!({ "session": session }),
                    )
                    .await?;
                Ok(serde_json::to_value(status).unwrap_or_default())
            }
            _ => Err(Relay::Refused(format!("no bridge endpoint {endpoint:?}"))),
        }
    }

    /// The app's session, opening one if it has none.
    async fn session(&self, app: &str) -> Result<Arc<Open>, Relay> {
        let mut sessions = self.sessions.lock().await;
        if let Some(open) = sessions.get(app) {
            return Ok(Arc::clone(open));
        }
        let client = Client::connect(&self.socket)
            .await
            .map_err(|_| Relay::Down)?;
        let session: AppSession = client
            .call(app_methods::APP_OPEN, json!({ "app": app }))
            .await
            .map_err(relay_error)?;
        let open = Arc::new(Open { client, session });
        sessions.insert(app.to_owned(), Arc::clone(&open));
        Ok(open)
    }

    /// One call through the app's session. A session glassd no longer has
    /// (closed from the desktop, glassd restarted) is opened again, once.
    async fn relay<R: serde::de::DeserializeOwned>(
        &self,
        app: &str,
        method: &str,
        params: impl Fn(&glass_types::TaskId) -> Value,
    ) -> Result<R, Relay> {
        for attempt in 0..2 {
            let open = self.session(app).await?;
            match open
                .client
                .call(method, params(&open.session.session))
                .await
            {
                Ok(result) => return Ok(result),
                Err(ClientError::Rpc(e)) if e.code == codes::NOT_FOUND && attempt == 0 => {}
                Err(ClientError::Closed) if attempt == 0 => {}
                Err(e) => return Err(relay_error(e)),
            }
            self.sessions.lock().await.remove(app);
        }
        Err(Relay::Down)
    }
}

fn relay_error(e: ClientError) -> Relay {
    match e {
        ClientError::Rpc(e) => Relay::Refused(e.message),
        _ => Relay::Down,
    }
}

/// The app this request is for: its own origin, `<app>.localhost:<port>`,
/// exactly. Any other Host (another name pointed at loopback, a bare IP)
/// is refused, which also keeps DNS rebinding out.
fn app_for_host<'a>(
    host: Option<&str>,
    port: u16,
    apps: &'a BTreeMap<String, PathBuf>,
) -> Option<&'a str> {
    let host = host?.to_ascii_lowercase();
    let name = host.strip_suffix(&format!(".localhost:{port}"))?;
    apps.get_key_value(name).map(|(id, _)| id.as_str())
}

/// A call must come from the app's own page: a JSON POST (which a page on
/// any other origin can't send without a preflight the bridge never
/// answers) whose Origin is the app's.
fn check_call(request: &Request, app: &str, port: u16) -> Result<(), (u16, &'static str)> {
    if request.method != "POST" {
        return Err((405, "bridge calls are POSTs"));
    }
    let json = request
        .header("content-type")
        .and_then(|v| v.split(';').next())
        .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json"));
    if !json {
        return Err((415, "bridge calls are JSON"));
    }
    if request.header("origin") != Some(format!("http://{app}.localhost:{port}").as_str()) {
        return Err((403, "only the app's own page may call its bridge"));
    }
    if request
        .header("sec-fetch-site")
        .is_some_and(|site| site != "same-origin")
    {
        return Err((403, "only the app's own page may call its bridge"));
    }
    Ok(())
}

/// The file a GET names, relative to the app's dist/: plain names only, no
/// dot files, no `..`, no escapes; a folder means its index.html.
fn static_path(path: &str) -> Option<String> {
    let rel = path.strip_prefix('/')?;
    let plain = rel
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'));
    if !plain || rel.len() > 200 {
        return None;
    }
    let mut parts: Vec<&str> = rel.split('/').collect();
    if parts.last() == Some(&"") {
        parts.pop();
        parts.push("index.html");
    }
    if parts.is_empty() {
        parts.push("index.html");
    }
    if parts.iter().any(|p| p.is_empty() || p.starts_with('.')) {
        return None;
    }
    Some(parts.join("/"))
}

fn content_type(rel: &str) -> &'static str {
    match rel.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apps() -> BTreeMap<String, PathBuf> {
        ["mail", "calendar"]
            .into_iter()
            .map(|id| (id.to_owned(), PathBuf::from(id)))
            .collect()
    }

    fn post(origin: Option<&str>, content_type: &str) -> Request {
        let mut headers = vec![("content-type".to_owned(), content_type.to_owned())];
        if let Some(origin) = origin {
            headers.push(("origin".to_owned(), origin.to_owned()));
        }
        Request {
            method: "POST".into(),
            path: "/_bridge/call".into(),
            headers,
            body: Vec::new(),
        }
    }

    #[test]
    fn ui5_each_app_is_its_own_origin() {
        let apps = apps();
        assert_eq!(
            app_for_host(Some("mail.localhost:7300"), 7300, &apps),
            Some("mail")
        );
        assert_eq!(
            app_for_host(Some("Calendar.LOCALHOST:7300"), 7300, &apps),
            Some("calendar")
        );
        for host in [
            "mail.localhost:7301",
            "127.0.0.1:7300",
            "localhost:7300",
            "notes.localhost:7300",
            "mail.localhost.evil.example:7300",
            "evil.example:7300",
        ] {
            assert_eq!(app_for_host(Some(host), 7300, &apps), None, "{host}");
        }
        assert_eq!(app_for_host(None, 7300, &apps), None);
    }

    #[test]
    fn ui5_calls_come_only_from_the_apps_own_page() {
        let own = "http://mail.localhost:7300";
        assert!(check_call(&post(Some(own), "application/json"), "mail", 7300).is_ok());
        assert!(
            check_call(
                &post(Some(own), "application/json; charset=utf-8"),
                "mail",
                7300
            )
            .is_ok()
        );
        // Calendar's page calling Mail's bridge, a page elsewhere, no origin.
        for origin in [
            Some("http://calendar.localhost:7300"),
            Some("http://evil.example"),
            None,
        ] {
            assert_eq!(
                check_call(&post(origin, "application/json"), "mail", 7300)
                    .unwrap_err()
                    .0,
                403,
                "{origin:?}"
            );
        }
        // A form post needs no preflight, so it never gets through.
        assert_eq!(
            check_call(&post(Some(own), "text/plain"), "mail", 7300)
                .unwrap_err()
                .0,
            415
        );
        let mut get = post(Some(own), "application/json");
        get.method = "GET".into();
        assert_eq!(check_call(&get, "mail", 7300).unwrap_err().0, 405);
        let mut cross = post(Some(own), "application/json");
        cross
            .headers
            .push(("sec-fetch-site".into(), "cross-site".into()));
        assert_eq!(check_call(&cross, "mail", 7300).unwrap_err().0, 403);
    }

    #[test]
    fn files_stay_inside_the_apps_build() {
        assert_eq!(static_path("/").as_deref(), Some("index.html"));
        assert_eq!(static_path("/app.js").as_deref(), Some("app.js"));
        assert_eq!(
            static_path("/assets/").as_deref(),
            Some("assets/index.html")
        );
        for bad in [
            "/../manifest.json",
            "/%2e%2e/x",
            "/.env",
            "/a//b",
            "/a/../b",
            "relative",
            "/a\\b",
        ] {
            assert_eq!(static_path(bad), None, "{bad}");
        }
    }
}
