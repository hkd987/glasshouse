//! `glass-app-bridge`: how a web app reaches the system (UI-5, ADR 0024).
//!
//! It runs as the user. For each app it serves it opens one session on
//! glassd's app socket; glassd authorizes that by this process's uid and
//! runs only what the app's manifest declares, through the pipeline,
//! attributed "by [app] for [user]". The bridge decides nothing: it serves
//! the app's files and relays its calls.
//!
//! Both modes serve loopback HTTP that the browser engine reaches, each app
//! on its own origin, `http://<app>.localhost:<port>`, so one app's page
//! can't call as another.
//!
//! - `--session`, on the device (ADR 0027): the desktop starts it for the
//!   person signed in. It listens on their own port
//!   ([`glass_api::app_bridge_port`]) and answers only connections whose
//!   socket the kernel says is theirs; anyone else's is closed unanswered.
//!   Agents can't connect at all (their jails have no network).
//! - `--dev`, for development and the tests: any process on this machine
//!   that can reach the port acts as any installed app for the user who ran
//!   the bridge.
//!
//! Either way the Host header names the app, so any of the person's own
//! processes can act as any of their apps; our own browser build will let
//! the bridge name the app from the installed copy it launched (ADR 0024).
//!
//! Artifacts (ADR 0033) are served on origins of their own,
//! `http://<id>.artifact.localhost:<port>`, read through the Artifacts
//! app's session and sandboxed by their CSP: no network, no storage, no
//! bridge, framed only by the Artifacts app.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, bail};
use clap::Parser;
use glass_api::{
    AppCallResult, AppFiles, AppLiveResult, AppManifest, AppPermission, AppSession, AppStatus,
    LiveOp, app_methods,
};
use glass_rpc::{Client, ClientError, codes};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use glass_http as http;
use glass_http::{ReadError, Request, Response};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Glasshouse app bridge: relays apps' calls to the system"
)]
struct Args {
    /// Serve the person running it, on the device: their own port, their own
    /// processes only.
    #[arg(long, conflicts_with_all = ["dev", "listen"])]
    session: bool,
    /// Serve apps over loopback HTTP for development and tests, to any local
    /// process.
    #[arg(long)]
    dev: bool,
    /// Loopback address to listen on (`--dev`).
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

/// The largest call an app sends: an artifact's page (1.5 MB at most, ADR
/// 0033) as JSON, with room for escaping.
const MAX_BODY: usize = 4 * glass_http::MAX_BODY;

/// The app that shows artifacts (ADR 0033), the only one that frames them.
const ARTIFACTS: &str = "artifacts";

/// Mail (ADR 0037), the only app that frames messages' HTML, on
/// `view.mail.localhost`.
const MAIL: &str = "mail";
const MAIL_VIEW: &str = "view.mail";

/// Added to every artifact page: where a person clicked to pin a comment.
const ARTIFACT_SHIM: &str = include_str!("artifact-shim.js");

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
    if args.dev == args.session {
        bail!("say which: --session (the device) or --dev (development and tests)");
    }
    let uid = glass_platform::users::effective_uid();
    let listen = if args.session {
        SocketAddr::from(([127, 0, 0, 1], glass_api::app_bridge_port(uid)))
    } else {
        args.listen
    };
    if !listen.ip().is_loopback() {
        bail!("the bridge serves loopback only; {listen} isn't");
    }
    let (apps, speech) = installed(&args.apps)?;
    if apps.is_empty() {
        bail!("no apps in {}", args.apps.display());
    }
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("listening on {listen}"))?;
    let port = listener.local_addr()?.port();
    if args.dev {
        tracing::warn!(
            "development bridge: any process that can reach 127.0.0.1:{port} acts as these apps for this user"
        );
    }
    for id in apps.keys() {
        tracing::info!("http://{id}.localhost:{port}/");
    }
    let bridge = Arc::new(Bridge {
        socket: args.socket,
        port,
        apps,
        speech,
        sessions: Mutex::new(BTreeMap::new()),
        own: Mutex::new(BTreeMap::new()),
    });
    let only = args.session.then_some(uid);
    // An accept that fails is tried again, and connections have a bound
    // (glass_http::Accept): the bridge ending took every open app's
    // backend with it until the desktop next started one.
    let mut accept = glass_http::Accept::new(listener);
    loop {
        let (stream, peer, place) = accept.next().await;
        let bridge = Arc::clone(&bridge);
        tokio::spawn(async move {
            let _place = place;
            if let Some(uid) = only
                && !owned_by(&stream, peer, uid).await
            {
                return;
            }
            bridge.serve(stream).await
        });
    }
}

/// Whether the kernel says the socket at `peer` belongs to `uid`. Anything
/// it can't say (the peer gone, another user's sockets hidden) is no.
async fn owned_by(stream: &TcpStream, peer: SocketAddr, uid: u32) -> bool {
    let Ok(local) = stream.local_addr() else {
        return false;
    };
    let owner =
        tokio::task::spawn_blocking(move || glass_platform::net::loopback_peer_uid(local, peer))
            .await
            .ok()
            .and_then(|r| r.map_err(|e| tracing::warn!("who is {peer}: {e}")).ok())
            .flatten();
    match owner {
        Some(o) if o == uid => true,
        Some(o) => {
            tracing::warn!("refused a connection from {peer}: uid {o}'s, not {uid}'s");
            false
        }
        // Gone already (the desktop checking the bridge is up), or another
        // user's where those can't be seen.
        None => {
            tracing::debug!("refused a connection from {peer}: its owner isn't visible");
            false
        }
    }
}

/// The installed apps, by id: where each one's built files are; and those
/// that turn the person's speech into text (ADR 0045 T2).
fn installed(dir: &Path) -> anyhow::Result<(BTreeMap<String, PathBuf>, BTreeSet<String>)> {
    let mut apps = BTreeMap::new();
    let mut speech = BTreeSet::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        let Ok(text) = std::fs::read_to_string(path.join("manifest.json")) else {
            continue;
        };
        // glassd checks and enforces the manifest; the bridge only needs
        // to know the app is there.
        match serde_json::from_str::<AppManifest>(&text) {
            Ok(manifest) => {
                if manifest.check().is_ok() && manifest.may(AppPermission::Speech) {
                    speech.insert(manifest.id.to_string());
                }
                apps.insert(manifest.id.to_string(), path.join("dist"));
            }
            Err(e) => tracing::warn!("{}: {e}", path.display()),
        }
    }
    Ok((apps, speech))
}

struct Bridge {
    socket: PathBuf,
    port: u16,
    apps: BTreeMap<String, PathBuf>,
    /// Installed apps whose worker may run WebAssembly, for speech (ADR
    /// 0045 T2): their own files, never an edition's or a person's.
    speech: BTreeSet<String>,
    /// One session per app, opened on first use.
    sessions: Mutex<BTreeMap<String, Arc<Open>>>,
    /// The person's own apps' and org apps' files (ADR 0032), from glassd:
    /// asked for again with each page, so a new version shows on reload.
    own: Mutex<BTreeMap<String, Arc<AppFiles>>>,
}

struct Open {
    client: Client,
    session: AppSession,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct StatusBody {
    #[serde(default)]
    wait_secs: u64,
    #[serde(default)]
    since: u64,
}

/// What a page sends `/_bridge/live` (ADR 0035 §3): an update it made to
/// a doc it opened, or a wait for others'.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveBody {
    op: LiveOp,
    doc: String,
    /// The page, made once per page load: one session serves every window.
    #[serde(default)]
    client: String,
    #[serde(default)]
    update: Option<String>,
    #[serde(default)]
    since: u64,
    #[serde(default)]
    wait_secs: u64,
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
        let (response, head_only) = match http::read_request_up_to(&mut stream, MAX_BODY).await {
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
        if let Some(artifact) = artifact_for_host(request.header("host"), self.port) {
            return self.artifact(request, &artifact).await;
        }
        if is_mail_view(request.header("host"), self.port) {
            return self.mail_view(request).await;
        }
        let Some(app) = app_for_host(request.header("host"), self.port, &self.apps) else {
            return Response::text(421, "no app is served at this address");
        };
        let app = app.as_str();
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
        let Some(root) = self.apps.get(app) else {
            return self.own_file(app, &rel).await;
        };
        // The org's edition of this app, for the people it reaches (ADR
        // 0032): glassd says, with each page; everyone else gets ours.
        if let Some(files) = self.edition(app, &rel).await {
            return match files.files.get(&rel) {
                Some(text) => page(app, self.port, &rel, text.clone().into_bytes(), false),
                None => Response::text(404, "not found"),
            };
        }
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
        page(app, self.port, &rel, body, self.speech.contains(app))
    }

    /// An artifact's page (ADR 0033): the version the path names, as
    /// `artifact.read` opens it for the person through the Artifacts app's
    /// session, so the read is decided and audited like any.
    async fn artifact(&self, request: &Request, id: &str) -> Response {
        if request.method != "GET" && request.method != "HEAD" {
            return Response::text(405, "artifacts are read-only");
        }
        let Some(version) = artifact_version(&request.path) else {
            return Response::text(404, "not found");
        };
        if !self.apps.contains_key(ARTIFACTS) {
            return Response::text(404, "Artifacts isn't installed");
        }
        let input = match version {
            Some(v) => json!({ "artifact": id, "version": v }),
            None => json!({ "artifact": id }),
        };
        let read: Result<AppCallResult, Relay> = self
            .relay(
                ARTIFACTS,
                app_methods::APP_CALL,
                |session| json!({ "session": session, "action": "artifact.read", "input": input }),
            )
            .await;
        match read {
            Ok(result) if result.ok => match result.data["html"].as_str() {
                Some(html) => artifact_page(html, self.port),
                None => Response::text(404, "not found"),
            },
            Ok(result) => Response::text(404, &result.text),
            Err(Relay::Down) => Response::text(503, "The system isn't answering right now."),
            Err(Relay::Refused(why)) => Response::text(502, &why),
        }
    }

    /// A message's HTML (ADR 0037): as `mail.read` hands it to Mail, through
    /// Mail's session, so the read is decided and audited like any; served
    /// on its own origin under a CSP with no scripts and no network, framed
    /// only by Mail.
    async fn mail_view(&self, request: &Request) -> Response {
        if request.method != "GET" && request.method != "HEAD" {
            return Response::text(405, "mail is read-only here");
        }
        // Only into a frame: a browser says what it loads a response
        // into, so an image, script or fetch on another page can't make a
        // read happen.
        if request
            .header("sec-fetch-dest")
            .is_some_and(|dest| !dest.eq_ignore_ascii_case("iframe"))
        {
            return Response::text(403, "a message is only shown in Mail");
        }
        let Some(id) = mail_view_id(&request.path) else {
            return Response::text(404, "not found");
        };
        if !self.apps.contains_key(MAIL) {
            return Response::text(404, "Mail isn't installed");
        }
        let read: Result<AppCallResult, Relay> = self
            .relay(MAIL, app_methods::APP_CALL, |session| {
                json!({ "session": session, "action": "mail.read", "input": { "id": id } })
            })
            .await;
        match read {
            Ok(result) if result.ok => match result.data["message"]["html"].as_str() {
                Some(html) => mail_page(html, self.port),
                None => Response::text(404, "this message has no HTML"),
            },
            Ok(result) => Response::text(404, &result.text),
            Err(Relay::Down) => Response::text(503, "The system isn't answering right now."),
            Err(Relay::Refused(why)) => Response::text(502, &why),
        }
    }

    /// A file of one of the person's own apps (ADR 0032): what glassd built
    /// from the version they chose, never a folder they could change.
    async fn own_file(&self, app: &str, rel: &str) -> Response {
        let cached = self.own.lock().await.get(app).cloned();
        let files = match cached {
            Some(files) if rel != "index.html" => files,
            _ => match self.own_files(app).await {
                Ok(files) => files,
                Err(Relay::Down) => {
                    return Response::text(503, "the system isn't answering right now");
                }
                Err(Relay::Refused(_)) => {
                    return Response::text(421, "no app is served at this address");
                }
            },
        };
        match files.files.get(rel) {
            Some(text) => page(app, self.port, rel, text.clone().into_bytes(), false),
            None => Response::text(404, "not found"),
        }
    }

    /// The org's edition of our app `app` for this person, if it reaches
    /// them: asked again with each page, kept for the page's other files.
    async fn edition(&self, app: &str, rel: &str) -> Option<Arc<AppFiles>> {
        if rel != "index.html" {
            return self.own.lock().await.get(app).cloned();
        }
        match self.own_files(app).await {
            Ok(files) => Some(files),
            Err(_) => {
                // Ours, then; and not an edition's other files with it.
                self.own.lock().await.remove(app);
                None
            }
        }
    }

    async fn own_files(&self, app: &str) -> Result<Arc<AppFiles>, Relay> {
        let client = Client::connect(&self.socket)
            .await
            .map_err(|_| Relay::Down)?;
        let files: AppFiles = client
            .call(app_methods::APP_FILES, json!({ "app": app }))
            .await
            .map_err(relay_error)?;
        let files = Arc::new(files);
        self.own
            .lock()
            .await
            .insert(app.to_owned(), Arc::clone(&files));
        Ok(files)
    }

    async fn api(&self, app: &str, endpoint: &str, body: &[u8]) -> Result<Value, Relay> {
        match endpoint {
            "session" => {
                // Tiers as they are now: IT may have changed policy since
                // the app opened.
                let now: AppStatus = self
                    .relay(
                        app,
                        app_methods::APP_STATUS,
                        |session| json!({ "session": session }),
                    )
                    .await?;
                let open = self.session(app).await?;
                let mut session = open.session.clone();
                session.actions = now.actions;
                let mut value = serde_json::to_value(&session).unwrap_or_default();
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
                // A wait for the app's data to change (ADR 0030): how long
                // and since when; glassd caps the wait.
                let wait: StatusBody = serde_json::from_slice(body).unwrap_or_default();
                let status: AppStatus = self
                    .relay(app, app_methods::APP_STATUS, |session| {
                        json!({ "session": session, "wait_secs": wait.wait_secs, "since": wait.since })
                    })
                    .await?;
                Ok(serde_json::to_value(status).unwrap_or_default())
            }
            "live" => {
                let live: LiveBody = serde_json::from_slice(body)
                    .map_err(|e| Relay::Refused(format!("bad live request: {e}")))?;
                let result: AppLiveResult = self
                    .relay(app, app_methods::APP_LIVE, |session| {
                        json!({
                            "session": session,
                            "op": live.op,
                            "item": live.doc,
                            "client": live.client,
                            "update": live.update,
                            "since": live.since,
                            "wait_secs": live.wait_secs,
                        })
                    })
                    .await?;
                Ok(serde_json::to_value(result).unwrap_or_default())
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

/// A file as the bridge serves it: pages with the CSP that keeps them to
/// their own files and bridge.
fn page(app: &str, port: u16, rel: &str, body: Vec<u8>, wasm: bool) -> Response {
    let mut response =
        Response::new(200, content_type(rel), body).with("Cache-Control", "no-cache");
    if rel.ends_with(".html") {
        response = response
            .with("Content-Security-Policy", app_csp(app, port, wasm))
            .with("Cross-Origin-Opener-Policy", "same-origin");
    } else if rel.ends_with(".js") || rel.ends_with(".mjs") {
        // A worker's policy is its own script's: without one, a worker
        // could reach anywhere. A page's scripts ignore it.
        response = response.with("Content-Security-Policy", app_csp(app, port, wasm));
    }
    response
}

/// An app's CSP: [`CSP`], and for Artifacts, frames of artifacts' pages;
/// for Mail, frames of messages' HTML; with `wasm`, WebAssembly, for the
/// speech an app turns into text on the computer (ADR 0045 T2).
fn app_csp(app: &str, port: u16, wasm: bool) -> String {
    let csp = if wasm {
        CSP.replacen(
            "script-src 'self'",
            "script-src 'self' 'wasm-unsafe-eval'",
            1,
        )
    } else {
        CSP.to_owned()
    };
    if app == ARTIFACTS {
        format!("{csp}; frame-src http://*.artifact.localhost:{port}")
    } else if app == MAIL {
        format!("{csp}; frame-src http://{MAIL_VIEW}.localhost:{port}")
    } else {
        csp
    }
}

/// What a message's HTML may do: show itself, with its own inline styles
/// and inline (`data:`) images. `sandbox` with no allowances: no scripts,
/// no forms, no popups, an origin that is no one's. Nothing it names
/// loads, so no remote image or stylesheet tells the sender it was read;
/// only Mail may frame it (ADR 0037).
fn mail_view_csp(port: u16) -> String {
    format!(
        "sandbox; default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; \
         img-src data:; font-src data:; media-src 'none'; connect-src 'none'; \
         frame-src 'none'; worker-src 'none'; object-src 'none'; base-uri 'none'; \
         form-action 'none'; frame-ancestors http://{MAIL}.localhost:{port}"
    )
}

/// A message's HTML under its CSP. Links open nowhere: a new window is
/// what they'd target, and the sandbox allows none.
fn mail_page(html: &str, port: u16) -> Response {
    let mut body = String::with_capacity(html.len() + 64);
    body.push_str("<!doctype html><base target=\"_blank\">");
    body.push_str(html);
    Response::new(200, "text/html; charset=utf-8", body)
        .with("Content-Security-Policy", mail_view_csp(port))
        .with("Cache-Control", "no-store")
        .with("Referrer-Policy", "no-referrer")
        .with("X-Content-Type-Options", "nosniff")
}

/// Whether a Host is Mail's message view, `view.mail.localhost:<port>`
/// exactly: a name no app's origin can have.
fn is_mail_view(host: Option<&str>, port: u16) -> bool {
    host.is_some_and(|h| h.eq_ignore_ascii_case(&format!("{MAIL_VIEW}.localhost:{port}")))
}

/// The message a view's path names: `/<folder>/<file>.eml`, plain names
/// only, as `mail.read` takes them.
fn mail_view_id(path: &str) -> Option<String> {
    let (folder, file) = path.strip_prefix('/')?.split_once('/')?;
    let folder_ok = !folder.is_empty() && folder.bytes().all(|b| b.is_ascii_lowercase());
    let file_ok = file.len() <= 200
        && file.ends_with(".eml")
        && !file.starts_with('.')
        && file
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    (folder_ok && file_ok).then(|| format!("{folder}/{file}"))
}

/// What an artifact's page may do: run its own inline scripts and styles
/// and show its own inline images and fonts. `sandbox` without
/// `allow-same-origin` gives it an origin that is no one's, framed or not
/// (no cookies, no storage, no bridge); nothing it loads or sends leaves
/// the machine; only the Artifacts app may frame it. Navigating itself
/// away isn't something CSP governs; the viewer reloads it (ADR 0033).
fn artifact_csp(port: u16) -> String {
    format!(
        "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval'; \
         style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:; \
         connect-src 'none'; frame-src 'none'; worker-src 'none'; object-src 'none'; \
         base-uri 'none'; form-action 'none'; \
         frame-ancestors http://{ARTIFACTS}.localhost:{port}"
    )
}

/// An artifact's HTML with the pin script after it, under its CSP.
fn artifact_page(html: &str, port: u16) -> Response {
    let mut body = String::with_capacity(html.len() + ARTIFACT_SHIM.len() + 20);
    body.push_str(html);
    body.push_str("\n<script>");
    body.push_str(ARTIFACT_SHIM);
    body.push_str("</script>\n");
    Response::new(200, "text/html; charset=utf-8", body)
        .with("Content-Security-Policy", artifact_csp(port))
        .with("Cache-Control", "no-store")
        .with("Referrer-Policy", "no-referrer")
        .with("X-Content-Type-Options", "nosniff")
}

/// The artifact a Host names: `<32 hex>.artifact.localhost:<port>`
/// exactly, as its conversation id `cv-<hex>`.
fn artifact_for_host(host: Option<&str>, port: u16) -> Option<String> {
    let host = host?.to_ascii_lowercase();
    let hex = host.strip_suffix(&format!(".artifact.localhost:{port}"))?;
    let ok = hex.len() == 32
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    ok.then(|| format!("cv-{hex}"))
}

/// The version an artifact's path names: `/` the latest, `/v<n>` the nth.
fn artifact_version(path: &str) -> Option<Option<u32>> {
    if path == "/" {
        return Some(None);
    }
    let n: u32 = path.strip_prefix("/v")?.parse().ok()?;
    (n > 0 && !path.starts_with("/v+")).then_some(Some(n))
}

fn relay_error(e: ClientError) -> Relay {
    match e {
        ClientError::Rpc(e) => Relay::Refused(e.message),
        _ => Relay::Down,
    }
}

/// The app this request is for: its own origin, `<app>.localhost:<port>`,
/// exactly. An installed app's files come from its folder; any other app
/// id is one of the person's own or an org app (ADR 0032), whose files
/// glassd gives only if it is theirs to open. Any other Host (another name
/// pointed at loopback, a bare IP) is refused, which also keeps DNS
/// rebinding out.
fn app_for_host(host: Option<&str>, port: u16, apps: &BTreeMap<String, PathBuf>) -> Option<String> {
    let host = host?.to_ascii_lowercase();
    let name = host.strip_suffix(&format!(".localhost:{port}"))?;
    let id = name.parse::<glass_types::AgentId>().is_ok();
    (apps.contains_key(name) || id).then(|| name.to_owned())
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
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        // The speech model's runtime (ADR 0045 T2); compiled as it streams.
        "wasm" => "application/wasm",
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
            query: String::new(),
            headers,
            body: Vec::new(),
        }
    }

    #[test]
    fn ui5_each_app_is_its_own_origin() {
        let apps = apps();
        let named = |host| app_for_host(Some(host), 7300, &apps);
        assert_eq!(named("mail.localhost:7300").as_deref(), Some("mail"));
        assert_eq!(
            named("Calendar.LOCALHOST:7300").as_deref(),
            Some("calendar")
        );
        // A person's own app or an org app: glassd says whether it's
        // theirs (ADR 0032).
        assert_eq!(named("my-mail.localhost:7300").as_deref(), Some("my-mail"));
        assert_eq!(named("notes.localhost:7300").as_deref(), Some("notes"));
        for host in [
            "mail.localhost:7301",
            "127.0.0.1:7300",
            "localhost:7300",
            "my-.localhost:7300",
            "my_mail.localhost:7300",
            "a.my-mail.localhost:7300",
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
    fn nv3_a_messages_html_is_its_own_origin_with_no_scripts_or_network() {
        assert!(is_mail_view(Some("view.mail.localhost:7300"), 7300));
        assert!(is_mail_view(Some("VIEW.MAIL.localhost:7300"), 7300));
        for host in [
            "view.mail.localhost:7301",
            "mail.localhost:7300",
            "x.view.mail.localhost:7300",
            "view.mail.localhost.evil.example:7300",
        ] {
            assert!(!is_mail_view(Some(host), 7300), "{host}");
        }
        assert_eq!(
            mail_view_id("/inbox/0896246782-0000000042.eml").as_deref(),
            Some("inbox/0896246782-0000000042.eml")
        );
        for path in [
            "/inbox/../x.eml",
            "/inbox/.x.eml",
            "/inbox/x.txt",
            "/Inbox/x.eml",
            "/inbox/a/b.eml",
            "/x.eml",
        ] {
            assert_eq!(mail_view_id(path), None, "{path}");
        }
        let csp = mail_view_csp(7300);
        for part in [
            "sandbox;",
            "default-src 'none'",
            "script-src 'none'",
            "img-src data:;",
            "connect-src 'none'",
            "form-action 'none'",
            "frame-ancestors http://mail.localhost:7300",
        ] {
            assert!(csp.contains(part), "{part}: {csp}");
        }
        assert!(!csp.contains("allow-"), "no allowances: {csp}");
        assert!(
            app_csp("mail", 7300, false).ends_with("frame-src http://view.mail.localhost:7300")
        );
        assert!(!app_csp("calendar", 7300, false).contains("frame-src"));
        let page = mail_page("<p>Hi</p>", 7300);
        assert!(
            page.headers
                .iter()
                .any(|(n, v)| *n == "Content-Security-Policy" && *v == csp)
        );
        assert!(String::from_utf8_lossy(&page.body).ends_with("<p>Hi</p>"));
    }

    #[test]
    fn ar1_each_artifact_is_its_own_origin_and_no_app() {
        let hex = "0123456789abcdef0123456789abcdef";
        let named = |host: &str| artifact_for_host(Some(host), 7300);
        assert_eq!(
            named(&format!("{hex}.artifact.localhost:7300")),
            Some(format!("cv-{hex}"))
        );
        assert_eq!(
            named(&format!("{}.artifact.localhost:7300", hex.to_uppercase())),
            Some(format!("cv-{hex}"))
        );
        for bad in [
            format!("{hex}.artifact.localhost:7301"),
            format!("{hex}0.artifact.localhost:7300"),
            format!("x.{hex}.artifact.localhost:7300"),
            format!("{}g.artifact.localhost:7300", &hex[..31]),
            format!("{hex}.artifact.localhost.evil.example:7300"),
            "artifact.localhost:7300".into(),
            "artifacts.localhost:7300".into(),
        ] {
            assert_eq!(named(&bad), None, "{bad}");
        }
        // No artifact's host names an app, so its page can't call a bridge.
        assert_eq!(
            app_for_host(
                Some(&format!("{hex}.artifact.localhost:7300")),
                7300,
                &apps()
            ),
            None
        );
        assert_eq!(artifact_version("/"), Some(None));
        assert_eq!(artifact_version("/v2"), Some(Some(2)));
        for bad in ["/v0", "/v+2", "/v-1", "/v", "/index.html", "/v2/x", ""] {
            assert_eq!(artifact_version(bad), None, "{bad}");
        }
    }

    #[test]
    fn adr0045_workers_keep_to_their_app_and_only_speech_runs_webassembly() {
        let csp = |response: &Response| {
            response
                .headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case("content-security-policy"))
                .map(|(_, v)| v.clone())
        };
        // A worker's script carries the app's policy, as its page does.
        let worker = page("meet", 7300, "speech.js", b"0".to_vec(), true);
        let policy = csp(&worker).unwrap();
        assert!(policy.contains("connect-src 'self'"), "{policy}");
        assert!(
            policy.contains("script-src 'self' 'wasm-unsafe-eval';"),
            "{policy}"
        );
        // Without speech, no WebAssembly; other files carry none.
        assert!(!app_csp("chat", 7300, false).contains("wasm"));
        assert!(
            csp(&page("chat", 7300, "app.js", b"0".to_vec(), false))
                .is_some_and(|p| !p.contains("wasm"))
        );
        assert_eq!(
            csp(&page("meet", 7300, "speech/x.onnx", b"0".to_vec(), true)),
            None
        );
    }

    #[test]
    fn ar1_an_artifact_runs_sandboxed_with_no_network() {
        let response = artifact_page("<h1>Q3</h1>", 7300);
        let header = |name: &str| {
            response
                .headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        let csp = header("content-security-policy");
        assert!(csp.starts_with("sandbox allow-scripts;"), "{csp}");
        assert!(!csp.contains("allow-same-origin"), "{csp}");
        for directive in [
            "default-src 'none'",
            "connect-src 'none'",
            "frame-src 'none'",
            "form-action 'none'",
            "base-uri 'none'",
            "frame-ancestors http://artifacts.localhost:7300",
        ] {
            assert!(csp.contains(directive), "{directive}: {csp}");
        }
        // The only place it names is who may frame it.
        assert_eq!(csp.matches("http").count(), 1, "{csp}");
        assert_eq!(header("referrer-policy"), "no-referrer");
        assert_eq!(header("cache-control"), "no-store");
        let body = String::from_utf8(response.body.clone()).unwrap();
        assert!(body.starts_with("<h1>Q3</h1>"));
        assert!(body.trim_end().ends_with("</script>"));
        assert!(body.contains("glass: TAG"));
        // Only the Artifacts app frames them.
        assert!(
            app_csp("artifacts", 7300, false)
                .ends_with("frame-src http://*.artifact.localhost:7300")
        );
        assert_eq!(app_csp("calendar", 7300, false), CSP);
        // Mail frames messages' HTML (ADR 0037), never an artifact.
        assert!(!app_csp("mail", 7300, false).contains("artifact"));
        assert!(CSP.contains("frame-ancestors 'none'"));
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
