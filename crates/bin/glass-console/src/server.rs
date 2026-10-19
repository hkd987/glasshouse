//! The console's HTTP surface: the web app's files, the API for IT's
//! admins and support staff, and the device API devices sync through
//! (ADR 0023).
//!
//! ```text
//! GET  /                              the console web app (apps/console)
//! GET|POST|DELETE /api/session        who is signed in; sign in; sign out
//! GET  /api/policy?group=<g>          the agent policy page
//! POST /api/policy/edit               one edit to the draft
//! POST /api/policy/{preview,publish,promote,rollback,discard}
//! GET  /api/catalog                   the catalog page
//! POST /api/catalog/<id>/{approve,block,public}
//! GET  /api/fleet                     the fleet page
//! POST /api/fleet/<device>/ring       move a device to another ring
//! POST /api/fleet/<device>/kind       say what kind of computer it is
//! GET  /api/settings?group=<g>        device settings for a group (UX-1)
//! GET  /api/roles                     remote help's roles (CF-81)
//! GET  /api/support                   support staff's page (CF-80)
//! POST /device/v1/sync                a device's long poll (bearer token)
//! POST /device/v1/report              a device's report (bearer token)
//! ```
//!
//! Admins are the directory's `it-admins`; support staff are the members of
//! the groups the pilot ring's policy gives a support role (ADR 0028).
//! Every route needs an admin unless `api` names it a support route, so a
//! new route is closed to support staff until someone opens it. Sessions
//! are a random cookie,
//! HttpOnly and SameSite=Strict; every change is a JSON POST whose Origin,
//! when sent, is the console's own, so another site's page can't make one.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use glass_http::{ReadError, Request, Response};
use glass_managed::{DeviceReport, InstallMode, MAX_WAIT_SECS, Ring, SyncRequest};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use crate::access::{Access, Need};
use crate::directory::Directory;
use crate::edits::Edit;
use crate::impact::{self, Titles};
use crate::state::{Refused, State};
use crate::views;

const COOKIE: &str = "glass_console";
/// Pages carry nothing but the console's own files.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

pub struct Console {
    state: Mutex<State>,
    directory: Box<dyn Directory>,
    titles: Titles,
    sessions: Mutex<HashMap<String, String>>,
    /// Bumped whenever a release changes, to wake waiting devices.
    releases: watch::Sender<u64>,
    /// The built web app (`apps/console/dist`).
    web: PathBuf,
    /// The console's address, as devices reach it.
    pub url: String,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn refused(r: Refused) -> Response {
    match r {
        Refused::Invalid(errors) => Response::json(
            400,
            &json!({ "error": errors.join("; "), "errors": errors }),
        ),
        Refused::NotFound(m) => Response::json(404, &json!({ "error": m })),
        Refused::Rule(m) => Response::json(409, &json!({ "error": m })),
    }
}

fn ok(value: Value) -> Response {
    Response::json(200, &value)
}

fn error(status: u16, message: &str) -> Response {
    Response::json(status, &json!({ "error": message }))
}

fn body<T: for<'de> Deserialize<'de>>(request: &Request) -> Result<T, Response> {
    serde_json::from_slice(&request.body).map_err(|e| error(400, &format!("bad request: {e}")))
}

fn query<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignIn {
    user: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Note {
    #[serde(default)]
    note: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Promote {
    ring: Ring,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rollback {
    to: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Approve {
    group: String,
    mode: InstallMode,
    #[serde(default)]
    pin: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetRing {
    ring: Ring,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetKind {
    kind: Option<String>,
}

impl Console {
    pub fn new(
        state: State,
        directory: Box<dyn Directory>,
        web: PathBuf,
        url: String,
    ) -> Arc<Self> {
        let (releases, _) = watch::channel(0);
        Arc::new(Self {
            state: Mutex::new(state),
            directory,
            titles: Titles::first_party(),
            sessions: Mutex::new(HashMap::new()),
            releases,
            web,
            url,
        })
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, State> {
        lock(&self.state)
    }

    fn released(&self) {
        self.releases.send_modify(|n| *n += 1);
    }

    /// Serves until the listener fails.
    pub async fn serve(self: Arc<Self>, listener: TcpListener) -> anyhow::Result<()> {
        let flusher = Arc::clone(&self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let mut state = flusher.state();
                if let Err(e) = state.save_fleet() {
                    tracing::warn!("saving the fleet: {e:#}");
                }
            }
        });
        loop {
            let (stream, _) = listener.accept().await?;
            let console = Arc::clone(&self);
            tokio::spawn(async move { console.connection(stream).await });
        }
    }

    async fn connection(&self, mut stream: TcpStream) {
        let (response, head_only) = match glass_http::read_request(&mut stream).await {
            Ok(request) => (self.handle(&request).await, request.method == "HEAD"),
            Err(ReadError::Gone) => return,
            Err(ReadError::Bad(status, why)) => (Response::text(status, why), false),
        };
        let _ = glass_http::write_response(&mut stream, response, head_only).await;
    }

    pub async fn handle(&self, request: &Request) -> Response {
        let path = request.path.as_str();
        if let Some(rest) = path.strip_prefix("/device/v1/") {
            return self.device(rest, request).await;
        }
        if let Some(rest) = path.strip_prefix("/api/") {
            if let Err(response) = check_origin(request) {
                return response;
            }
            return self.api(rest, request);
        }
        if request.method != "GET" && request.method != "HEAD" {
            return error(405, "method not allowed");
        }
        self.file(path).await
    }

    async fn file(&self, path: &str) -> Response {
        let (name, content_type) = match path {
            "/" | "/index.html" => ("index.html", "text/html; charset=utf-8"),
            "/app.js" => ("app.js", "text/javascript; charset=utf-8"),
            "/app.css" => ("app.css", "text/css; charset=utf-8"),
            _ => return Response::text(404, "not found"),
        };
        match tokio::fs::read(self.web.join(name)).await {
            Ok(bytes) => Response::new(200, content_type, bytes)
                .with("Content-Security-Policy", CSP)
                .with("Cache-Control", "no-cache"),
            Err(_) => Response::text(
                503,
                "the console's web app isn't built; run make apps-build",
            ),
        }
    }

    // ---- admins ----------------------------------------------------------

    fn signed_in(&self, request: &Request) -> Option<String> {
        let cookies = request.header("cookie")?;
        let token = cookies
            .split(';')
            .filter_map(|c| c.trim().split_once('='))
            .find(|(k, _)| *k == COOKIE)
            .map(|(_, v)| v)?;
        lock(&self.sessions).get(token).cloned()
    }

    /// What `user` may do now, by the directory and the roles in force.
    fn access(&self, user: &str) -> Access {
        let support = self.state().support_in_force();
        Access::of(user, self.directory.as_ref(), &support)
    }

    /// Development sign-in: the org's admins and support staff to pick
    /// from, never the whole directory. Our web servers sign people in with
    /// the org's SSO instead (not built).
    fn signed_out(&self) -> Response {
        let support = self.state().support_in_force();
        let mut people: Vec<&str> = self.directory.members(crate::directory::ADMINS);
        for role in &support.roles {
            for group in &role.groups {
                if group != crate::directory::EVERYONE {
                    people.extend(self.directory.members(group));
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        people.retain(|u| seen.insert(*u));
        let people: Vec<Value> = people
            .into_iter()
            .map(|u| {
                let access = Access::of(u, self.directory.as_ref(), &support);
                json!({ "user": u, "display": self.directory.display_name(u), "title": access.title() })
            })
            .collect();
        Response::json(
            401,
            &json!({ "error": "sign in", "people": people, "org": self.state().org.name }),
        )
    }

    fn session(&self, request: &Request) -> Response {
        match request.method.as_str() {
            "GET" => {
                let Some(user) = self.signed_in(request) else {
                    return self.signed_out();
                };
                let access = self.access(&user);
                // A role taken away while signed in ends the session.
                if !access.any() {
                    return self.signed_out();
                }
                let roles: Vec<Value> = access
                    .roles
                    .iter()
                    .map(|r| json!({ "id": r.id, "name": r.name, "tools": r.tools }))
                    .collect();
                let state = self.state();
                ok(json!({
                    "user": user,
                    "display": self.directory.display_name(&user),
                    "org": { "id": state.org.id, "name": state.org.name },
                    "admin": access.admin,
                    "roles": roles,
                }))
            }
            "POST" => {
                let sign_in: SignIn = match body(request) {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                if !self.access(&sign_in.user).any() {
                    return error(
                        403,
                        &format!(
                            "{} isn't an admin of this org and holds no support role.",
                            sign_in.user
                        ),
                    );
                }
                let token = match crate::state::random() {
                    Ok(bytes) => hex::encode(bytes),
                    Err(e) => return error(500, &format!("{e:#}")),
                };
                lock(&self.sessions).insert(token.clone(), sign_in.user.clone());
                tracing::info!(user = %sign_in.user, "signed in");
                ok(json!({ "user": sign_in.user })).with(
                    "Set-Cookie",
                    format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict"),
                )
            }
            "DELETE" => {
                if let Some(cookies) = request.header("cookie") {
                    for c in cookies.split(';') {
                        if let Some((COOKIE, token)) = c.trim().split_once('=') {
                            lock(&self.sessions).remove(token);
                        }
                    }
                }
                ok(json!({})).with("Set-Cookie", format!("{COOKIE}=; Path=/; Max-Age=0"))
            }
            _ => error(405, "method not allowed"),
        }
    }

    fn api(&self, path: &str, request: &Request) -> Response {
        if path == "session" {
            return self.session(request);
        }
        let Some(user) = self.signed_in(request) else {
            return error(401, "sign in");
        };
        let get = request.method == "GET";
        let post = request.method == "POST";
        if post
            && request
                .header("content-type")
                .is_none_or(|c| !c.starts_with("application/json"))
        {
            return error(415, "send JSON");
        }
        let directory = self.directory.as_ref();
        let segments: Vec<&str> = path.split('/').collect();
        // Default deny (ADR 0028): every route needs an admin unless it is
        // named here.
        let need = match (request.method.as_str(), segments.as_slice()) {
            ("GET", ["support"]) => Need::Support,
            _ => Need::Admin,
        };
        let access = self.access(&user);
        if !access.allows(need) {
            return error(
                403,
                match need {
                    Need::Admin => "Only the org's IT admins can do that.",
                    Need::Support => "Only support staff can open remote help.",
                },
            );
        }
        match (request.method.as_str(), segments.as_slice()) {
            ("GET", ["support"]) => ok(views::support(&self.state(), directory, &access)),
            ("GET", ["settings"]) => {
                let group = query(request, "group").unwrap_or(crate::directory::EVERYONE);
                if group != crate::directory::EVERYONE && directory.group(group).is_none() {
                    return error(404, &format!("no group {group}"));
                }
                match views::settings(&self.state(), directory, group) {
                    Ok(v) => ok(v),
                    Err(e) => error(500, &format!("{e:#}")),
                }
            }
            ("GET", ["roles"]) => match views::roles(&self.state(), directory) {
                Ok(v) => ok(v),
                Err(e) => error(500, &format!("{e:#}")),
            },
            ("GET", ["policy"]) => {
                let group = query(request, "group").unwrap_or(crate::directory::EVERYONE);
                let state = self.state();
                match views::policy(&state, directory, &self.titles, group) {
                    Ok(v) => ok(v),
                    Err(e) => error(500, &format!("{e:#}")),
                }
            }
            ("GET", ["mail"]) => match views::mail(&self.state()) {
                Ok(v) => ok(v),
                Err(e) => error(500, &format!("{e:#}")),
            },
            ("POST", ["policy", "edit"]) => {
                let edit: Edit = match body(request) {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                match self.state().edit_draft(edit) {
                    Ok(()) => ok(json!({})),
                    Err(r) => refused(r),
                }
            }
            ("POST", ["policy", "discard"]) => match self.state().discard_draft() {
                Ok(()) => ok(json!({})),
                Err(r) => refused(r),
            },
            ("POST", ["policy", "preview"]) => {
                let state = self.state();
                let draft = match state.draft() {
                    Ok(d) => d,
                    Err(e) => return error(500, &format!("{e:#}")),
                };
                let base = state
                    .in_force(Ring::Broad)
                    .and_then(|v| toml::from_str(&v.text).ok().map(|d| (v.version, d)));
                let impact = impact::preview(
                    base.as_ref().map(|(v, d)| (*v, d)),
                    &draft,
                    directory,
                    &self.titles,
                );
                ok(serde_json::to_value(impact).unwrap_or_default())
            }
            ("POST", ["policy", "publish"]) => {
                let note: Note = match body(request) {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                let result = self.state().publish(directory, &note.note, &user);
                match result {
                    Ok(version) => {
                        tracing::info!(version, by = %user, "policy published to the pilot ring");
                        self.released();
                        ok(json!({ "version": version }))
                    }
                    Err(r) => refused(r),
                }
            }
            ("POST", ["policy", "promote"]) => {
                let promote: Promote = match body(request) {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                let result = self.state().promote(promote.ring);
                match result {
                    Ok(version) => {
                        self.released();
                        ok(json!({ "version": version }))
                    }
                    Err(r) => refused(r),
                }
            }
            ("POST", ["policy", "rollback"]) => {
                let rollback: Rollback = match body(request) {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                let result = self.state().rollback(directory, rollback.to, &user);
                match result {
                    Ok(version) => {
                        self.released();
                        ok(json!({ "version": version }))
                    }
                    Err(r) => refused(r),
                }
            }
            ("GET", ["catalog"]) => ok(views::catalog(&self.state(), directory, &self.titles)),
            ("GET", ["catalog", id]) => {
                match views::item(&self.state(), directory, &self.titles, id) {
                    Some(v) => ok(v),
                    None => error(404, "no such item"),
                }
            }
            ("POST", ["catalog", id, "approve"]) => {
                let approve: Approve = match body(request) {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                if approve.group != crate::directory::EVERYONE
                    && directory.group(&approve.group).is_none()
                {
                    return error(400, &format!("no group {}", approve.group));
                }
                let result =
                    self.state()
                        .approve(id, &approve.group, approve.mode, approve.pin, &user);
                match result {
                    Ok(version) => {
                        self.released();
                        ok(json!({ "catalog_version": version }))
                    }
                    Err(r) => refused(r),
                }
            }
            ("POST", ["catalog", id, "block"]) => {
                let result = self.state().block(id, &user);
                match result {
                    Ok(version) => {
                        self.released();
                        ok(json!({ "catalog_version": version }))
                    }
                    Err(r) => refused(r),
                }
            }
            ("POST", ["catalog", id, "public"]) => match self.state().publish_public(id) {
                Ok(()) => ok(json!({})),
                Err(r) => refused(r),
            },
            ("GET", ["fleet"]) => ok(views::fleet(&self.state(), directory, &self.url)),
            ("POST", ["fleet", device, "ring"]) => {
                let set: SetRing = match body(request) {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                match self.state().set_ring(device, set.ring) {
                    Ok(()) => {
                        // The device may now be owed another version.
                        self.released();
                        ok(json!({}))
                    }
                    Err(r) => refused(r),
                }
            }
            ("POST", ["fleet", device, "kind"]) => {
                let set: SetKind = match body(request) {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                match self.state().set_kind(device, set.kind) {
                    Ok(()) => ok(json!({})),
                    Err(r) => refused(r),
                }
            }
            _ if get || post => error(404, "not found"),
            _ => error(405, "method not allowed"),
        }
    }

    // ---- devices ---------------------------------------------------------

    fn enrolled(&self, request: &Request) -> bool {
        let expected = format!("Bearer {}", self.state().org.enrollment_token);
        request
            .header("authorization")
            .is_some_and(|got| constant_time_eq(got.as_bytes(), expected.as_bytes()))
    }

    async fn device(&self, path: &str, request: &Request) -> Response {
        if request.method != "POST" {
            return error(405, "method not allowed");
        }
        if !self.enrolled(request) {
            return error(401, "not enrolled in this org");
        }
        match path {
            "sync" => {
                let sync: SyncRequest = match body(request) {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                if sync.device.is_empty()
                    || sync.device.len() > 128
                    || sync.device.chars().any(char::is_control)
                {
                    return error(400, "name the device");
                }
                let wait = Duration::from_secs(sync.wait_secs.min(MAX_WAIT_SECS));
                let mut changed = self.releases.subscribe();
                let first = self.state().sync(&sync);
                if first.policy.is_some()
                    || first.catalog.is_some()
                    || first.directory.is_some()
                    || wait.is_zero()
                {
                    return ok(serde_json::to_value(first).unwrap_or_default());
                }
                // Long poll: answer as soon as something is released.
                let _ = tokio::time::timeout(wait, changed.changed()).await;
                let answer = self.state().sync(&sync);
                ok(serde_json::to_value(answer).unwrap_or_default())
            }
            "report" => {
                let report: DeviceReport = match body(request) {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                self.state().report(report);
                ok(json!({}))
            }
            _ => error(404, "not found"),
        }
    }
}

/// A change must come from the console's own page: when the browser names
/// the page's origin, it must be this host's.
fn check_origin(request: &Request) -> Result<(), Response> {
    if request.method == "GET" || request.method == "HEAD" {
        return Ok(());
    }
    let Some(origin) = request.header("origin") else {
        return Ok(());
    };
    let host = request.header("host").unwrap_or_default();
    if origin == format!("http://{host}") || origin == format!("https://{host}") {
        Ok(())
    } else {
        Err(error(403, "changes come from the console's own page only"))
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use glass_managed::{CatalogRelease, DocKind, PolicyRelease, PublicKey, SyncResponse};

    use super::*;
    use crate::directory::LocalDirectory;

    struct Test {
        _dir: tempfile::TempDir,
        console: Arc<Console>,
        cookie: String,
        token: String,
        key: PublicKey,
    }

    fn request(method: &str, path: &str, body: &Value, headers: &[(&str, &str)]) -> Request {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let mut all: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        all.push(("content-type".into(), "application/json".into()));
        all.push(("host".into(), "127.0.0.1:7400".into()));
        Request {
            method: method.into(),
            path: path.into(),
            query: query.into(),
            headers: all,
            body: if body.is_null() {
                Vec::new()
            } else {
                body.to_string().into_bytes()
            },
        }
    }

    fn json_of(response: &Response) -> Value {
        serde_json::from_slice(&response.body).unwrap()
    }

    /// Signs `user` in: their session cookie, or the status it was refused with.
    async fn sign_in(console: &Console, user: &str) -> Result<String, u16> {
        let r = console
            .handle(&request(
                "POST",
                "/api/session",
                &json!({ "user": user }),
                &[],
            ))
            .await;
        if r.status != 200 {
            return Err(r.status);
        }
        let set = r
            .headers
            .iter()
            .find(|(n, _)| *n == "Set-Cookie")
            .unwrap()
            .1
            .clone();
        assert!(
            set.contains("HttpOnly") && set.contains("SameSite=Strict"),
            "{set}"
        );
        Ok(set
            .split(';')
            .next()
            .unwrap()
            .split_once('=')
            .unwrap()
            .1
            .to_owned())
    }

    impl Test {
        /// An admin's call.
        async fn call(&self, method: &str, path: &str, body: Value) -> (u16, Value) {
            self.call_as(&self.cookie, method, path, body).await
        }

        /// A call by whoever has `session`.
        async fn call_as(
            &self,
            session: &str,
            method: &str,
            path: &str,
            body: Value,
        ) -> (u16, Value) {
            let cookie = format!("{COOKIE}={session}");
            let r = self
                .console
                .handle(&request(method, path, &body, &[("cookie", &cookie)]))
                .await;
            (r.status, json_of(&r))
        }

        /// A device's sync.
        async fn sync(&self, device: &str, policy: Option<u64>, wait: u64) -> SyncResponse {
            let auth = format!("Bearer {}", self.token);
            let body = json!({ "device": device, "policy": policy, "wait_secs": wait });
            let r = self
                .console
                .handle(&request(
                    "POST",
                    "/device/v1/sync",
                    &body,
                    &[("authorization", &auth)],
                ))
                .await;
            assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
            serde_json::from_slice(&r.body).unwrap()
        }
    }

    #[tokio::test]
    async fn ad1_the_directory_is_signed_once_per_change_and_reaches_devices() {
        let dir = tempfile::tempdir().unwrap();
        let mut directory = LocalDirectory::starter("root");
        let mut state = State::open(dir.path(), "acme", "Acme Corp", &directory).unwrap();
        assert_eq!(state.release_directory(&directory).unwrap(), Some(1));
        // Nothing changed: nothing new to sign, after a restart too.
        assert_eq!(state.release_directory(&directory).unwrap(), None);
        let mut reopened = State::open(dir.path(), "acme", "Acme Corp", &directory).unwrap();
        assert_eq!(reopened.release_directory(&directory).unwrap(), None);

        let sync = |state: &mut State, have: Option<u64>| {
            state.sync(&SyncRequest {
                device: "dev-1".into(),
                policy: None,
                catalog: None,
                directory: have,
                wait_secs: 0,
            })
        };
        let first = sync(&mut reopened, None).directory.expect("the directory");
        let key: PublicKey = reopened.public_key().parse().unwrap();
        let people: glass_managed::DirectoryRelease = first
            .open(DocKind::Directory, "acme", &[key], None)
            .unwrap();
        assert_eq!(
            people.person("priya").map(|p| p.email.as_str()),
            Some("priya@acme.example")
        );
        assert!(sync(&mut reopened, Some(1)).directory.is_none());

        // A new address is a new release.
        directory.users[0].email = "dana.ruiz@acme.example".into();
        assert_eq!(reopened.release_directory(&directory).unwrap(), Some(2));
        assert_eq!(
            sync(&mut reopened, Some(1)).directory.map(|s| s.version),
            Some(2)
        );
    }

    async fn console() -> Test {
        let dir = tempfile::tempdir().unwrap();
        let directory = LocalDirectory::starter("root");
        let state = State::open(dir.path(), "acme", "Acme Corp", &directory).unwrap();
        let token = state.org.enrollment_token.clone();
        let key: PublicKey = state.public_key().parse().unwrap();
        let console = Console::new(
            state,
            Box::new(directory),
            dir.path().join("web"),
            "http://127.0.0.1:7400".into(),
        );
        let cookie = sign_in(&console, "pat").await.unwrap();
        Test {
            _dir: dir,
            console,
            cookie,
            token,
            key,
        }
    }

    #[tokio::test]
    async fn only_the_orgs_admins_sign_in_and_pages_need_it() {
        let t = console().await;
        let r = t
            .console
            .handle(&request(
                "POST",
                "/api/session",
                &json!({ "user": "dana" }),
                &[],
            ))
            .await;
        assert_eq!(r.status, 403, "dana isn't in it-admins");
        let r = t
            .console
            .handle(&request("GET", "/api/fleet", &Value::Null, &[]))
            .await;
        assert_eq!(r.status, 401);
        let (status, session) = t.call("GET", "/api/session", Value::Null).await;
        assert_eq!(status, 200);
        assert_eq!(session["display"], "Pat Okafor");
    }

    #[tokio::test]
    async fn cf80_support_staff_sign_in_and_see_their_roles_tools() {
        let t = console().await;
        // The development picker: admins and support staff, never the
        // whole directory.
        let r = t
            .console
            .handle(&request("GET", "/api/session", &Value::Null, &[]))
            .await;
        assert_eq!(r.status, 401);
        let picker = json_of(&r);
        let people: Vec<&str> = picker["people"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["user"].as_str().unwrap())
            .collect();
        assert_eq!(people, ["pat", "root", "jordan", "alex", "sasha"]);
        assert_eq!(picker["people"][2]["title"], "IT Help Desk");

        t.sync("d", None, 0).await;
        let jordan = sign_in(&t.console, "jordan").await.unwrap();
        let (status, session) = t.call_as(&jordan, "GET", "/api/session", Value::Null).await;
        assert_eq!(status, 200);
        assert_eq!(session["admin"], false);
        assert_eq!(session["roles"][0]["name"], "IT Help Desk");
        assert_eq!(session["roles"][0]["tools"], json!(["view", "control"]));

        let (status, page) = t.call_as(&jordan, "GET", "/api/support", Value::Null).await;
        assert_eq!(status, 200, "{page}");
        let tool = |id: &str| {
            page["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["id"] == id)
                .unwrap()
                .clone()
        };
        assert_eq!(tool("view")["mine"], true);
        assert_eq!(tool("install_apps")["mine"], false);
        assert_eq!(tool("install_apps")["roles"], json!(["Desktop Support"]));
        assert_eq!(tool("isolate")["roles"], json!(["Security"]));
        assert!(
            page["tools"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["ready"] == false),
            "sessions come with slice (b)"
        );
        assert_eq!(page["devices"][0]["id"], "d");

        // Pat runs the org but holds no role: no remote help (ADR 0028).
        let (status, _) = t.call("GET", "/api/support", Value::Null).await;
        assert_eq!(status, 403);
    }

    #[tokio::test]
    async fn cf80_someone_in_no_role_cant_sign_in() {
        let t = console().await;
        assert_eq!(sign_in(&t.console, "dana").await, Err(403));
        assert_eq!(sign_in(&t.console, "nobody").await, Err(403));
    }

    #[tokio::test]
    async fn cf80_support_staff_reach_nothing_but_remote_help() {
        let t = console().await;
        t.sync("d", None, 0).await;
        let before = t.console.state().policy.clone();
        let jordan = sign_in(&t.console, "jordan").await.unwrap();
        for (method, path, body) in [
            ("GET", "/api/policy", Value::Null),
            (
                "POST",
                "/api/policy/edit",
                json!({ "edit": "tier", "group": "everyone", "class": "send", "tier": "auto" }),
            ),
            ("POST", "/api/policy/preview", json!({})),
            ("POST", "/api/policy/publish", json!({})),
            ("POST", "/api/policy/promote", json!({ "ring": "early" })),
            ("POST", "/api/policy/rollback", json!({ "to": 1 })),
            ("POST", "/api/policy/discard", json!({})),
            ("GET", "/api/catalog", Value::Null),
            (
                "POST",
                "/api/catalog/x/approve",
                json!({ "group": "everyone", "mode": "available" }),
            ),
            ("POST", "/api/catalog/x/block", json!({})),
            ("POST", "/api/catalog/x/public", json!({})),
            ("GET", "/api/fleet", Value::Null),
            ("POST", "/api/fleet/d/ring", json!({ "ring": "pilot" })),
            ("POST", "/api/fleet/d/kind", json!({ "kind": "kiosk" })),
            ("GET", "/api/mail", Value::Null),
            ("GET", "/api/roles", Value::Null),
            ("GET", "/api/settings", Value::Null),
            ("GET", "/api/a-route-nobody-wrote-yet", Value::Null),
        ] {
            let (status, _) = t.call_as(&jordan, method, path, body).await;
            assert_eq!(status, 403, "{method} {path}");
        }
        assert_eq!(t.console.state().policy, before, "nothing changed");
        assert_eq!(t.console.state().fleet.devices["d"].ring, Ring::Broad);
    }

    #[tokio::test]
    async fn cf81_edit_a_roles_tools_then_publish_and_it_takes_effect() {
        let t = console().await;
        let jordan = sign_in(&t.console, "jordan").await.unwrap();
        let installs = |page: &Value| {
            page["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["id"] == "install_apps")
                .unwrap()["mine"]
                .clone()
        };

        let (status, roles) = t.call("GET", "/api/roles", Value::Null).await;
        assert_eq!(status, 200);
        assert_eq!(roles["changed"], false);
        let desk = &roles["roles"][0];
        assert_eq!(desk["holders"], json!(["Jordan Lee"]));
        let mut role = json!({});
        for field in [
            "id",
            "name",
            "groups",
            "tools",
            "people",
            "unattended",
            "max_minutes",
        ] {
            role[field] = desk[field].clone();
        }
        role["tools"]
            .as_array_mut()
            .unwrap()
            .push("install_apps".into());
        let (status, _) = t
            .call(
                "POST",
                "/api/policy/edit",
                json!({ "edit": "support_role", "id": "help-desk", "role": role }),
            )
            .await;
        assert_eq!(status, 200);
        let (_, impact) = t.call("POST", "/api/policy/preview", json!({})).await;
        assert!(
            impact["changes"]
                .to_string()
                .contains("Remote help: IT Help Desk may now install approved apps"),
            "{impact}"
        );
        // Not in force until published.
        let (_, page) = t.call_as(&jordan, "GET", "/api/support", Value::Null).await;
        assert_eq!(installs(&page), false);
        let (status, _) = t.call("POST", "/api/policy/publish", json!({})).await;
        assert_eq!(status, 200);
        let (_, page) = t.call_as(&jordan, "GET", "/api/support", Value::Null).await;
        assert_eq!(installs(&page), true);

        // A control without view is refused, as a device would.
        role["tools"] = json!(["control"]);
        let (status, _) = t
            .call(
                "POST",
                "/api/policy/edit",
                json!({ "edit": "support_role", "id": "help-desk", "role": role }),
            )
            .await;
        assert_eq!(status, 400);

        // Taking the role away ends Jordan's access once it is published.
        t.call(
            "POST",
            "/api/policy/edit",
            json!({ "edit": "support_role", "id": "help-desk", "role": null }),
        )
        .await;
        let (_, impact) = t.call("POST", "/api/policy/preview", json!({})).await;
        assert!(
            impact["changes"]
                .to_string()
                .contains("Remote help: IT Help Desk taken out"),
            "{impact}"
        );
        t.call("POST", "/api/policy/publish", json!({})).await;
        let (status, _) = t.call_as(&jordan, "GET", "/api/session", Value::Null).await;
        assert_eq!(status, 401);
        let (status, _) = t.call_as(&jordan, "GET", "/api/support", Value::Null).await;
        assert_eq!(status, 403);
    }

    #[tokio::test]
    async fn adr0028_it_sets_a_devices_kind_and_a_role_reaches_it_unattended() {
        let t = console().await;
        t.sync("kiosk-1", None, 0).await;
        let (status, _) = t
            .call(
                "POST",
                "/api/fleet/kiosk-1/kind",
                json!({ "kind": "Lobby Kiosk" }),
            )
            .await;
        assert_eq!(status, 400, "kinds are plain words");
        let (status, _) = t
            .call(
                "POST",
                "/api/fleet/kiosk-1/kind",
                json!({ "kind": "kiosk" }),
            )
            .await;
        assert_eq!(status, 200);
        let (_, fleet) = t.call("GET", "/api/fleet", Value::Null).await;
        assert_eq!(fleet["devices"][0]["kind"], "kiosk");

        let alex = sign_in(&t.console, "alex").await.unwrap();
        let unattended = |page: &Value| page["devices"][0]["unattended"].clone();
        let (_, page) = t.call_as(&alex, "GET", "/api/support", Value::Null).await;
        assert_eq!(unattended(&page), false, "no role reaches kiosks yet");

        let (_, roles) = t.call("GET", "/api/roles", Value::Null).await;
        let desktop = &roles["roles"][1];
        let mut role = json!({});
        for field in ["id", "name", "groups", "tools", "people", "max_minutes"] {
            role[field] = desktop[field].clone();
        }
        role["unattended"] = json!(["kiosk"]);
        t.call(
            "POST",
            "/api/policy/edit",
            json!({ "edit": "support_role", "id": "desktop-support", "role": role }),
        )
        .await;
        let (_, impact) = t.call("POST", "/api/policy/preview", json!({})).await;
        assert!(
            impact["warnings"]
                .to_string()
                .contains("Desktop Support may reach kiosk computers with nobody there"),
            "{impact}"
        );
        t.call("POST", "/api/policy/publish", json!({})).await;
        let (_, page) = t.call_as(&alex, "GET", "/api/support", Value::Null).await;
        assert_eq!(unattended(&page), true);
        let (_, fleet) = t.call("GET", "/api/fleet", Value::Null).await;
        assert_eq!(fleet["kinds"], json!(["kiosk"]));
    }

    #[tokio::test]
    async fn ux1_settings_set_per_group_are_previewed_published_and_resolved_per_person() {
        let t = console().await;
        let set = |group: &str, settings: Value| json!({ "edit": "settings", "group": group, "settings": settings });
        let (status, _) = t
            .call(
                "POST",
                "/api/policy/edit",
                set(
                    "ap-ops",
                    json!({ "lock.after_minutes": { "value": 5, "locked": true } }),
                ),
            )
            .await;
        assert_eq!(status, 200);
        // Legal Operations sets it differently; nobody is in both, so the
        // preview has no warning about it (impact.rs tests one).
        t.call(
            "POST",
            "/api/policy/edit",
            set(
                "legal-ops",
                json!({ "lock.after_minutes": { "value": 30 }, "clock.hour24": { "value": true } }),
            ),
        )
        .await;
        let (status, _) = t
            .call(
                "POST",
                "/api/policy/edit",
                set(
                    "ap-ops",
                    json!({ "browser.home_page": { "value": "https://intranet.acme.example" } }),
                ),
            )
            .await;
        assert_eq!(status, 400, "the browser is the whole computer's");
        t.call(
            "POST",
            "/api/policy/edit",
            set(
                "everyone",
                json!({ "browser.home_page": { "value": "https://intranet.acme.example", "locked": true } }),
            ),
        )
        .await;

        let (_, impact) = t.call("POST", "/api/policy/preview", json!({})).await;
        let changes = impact["changes"].to_string();
        assert!(
            changes.contains("AP Operations: Lock after 5 minutes (locked)"),
            "{changes}"
        );
        assert!(
            changes.contains("Everyone: Home page https://intranet.acme.example (locked)"),
            "{changes}"
        );
        assert!(
            changes.contains("Legal Operations: 24-hour clock on (people can change it)"),
            "{changes}"
        );
        assert!(!impact["warnings"].to_string().contains("in both"));

        let (_, page) = t
            .call("GET", "/api/settings?group=ap-ops", Value::Null)
            .await;
        assert_eq!(page["changed"], true);
        assert_eq!(page["order"], json!(["it-admins", "ap-ops", "legal-ops"]));
        let lock = page["settings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "lock.after_minutes")
            .unwrap()
            .clone();
        assert_eq!(lock["set"], json!({ "value": 5, "locked": true }));
        assert!(lock["pilot"].is_null());

        let (status, _) = t.call("POST", "/api/policy/publish", json!({})).await;
        assert_eq!(status, 200);
        t.sync("d", None, 0).await;
        t.call("POST", "/api/fleet/d/ring", json!({ "ring": "pilot" }))
            .await;
        let got = t.sync("d", None, 0).await.policy.unwrap();
        let release: PolicyRelease = got.open(DocKind::Policy, "acme", &[t.key], None).unwrap();
        let policy = glass_policy::Policy::from_toml(&release.policy).unwrap();
        let get = |user: &str, id: &str| {
            policy
                .settings_for(user)
                .into_iter()
                .find(|r| r.id == id)
                .unwrap()
        };
        let dana = get("dana", "lock.after_minutes");
        assert_eq!(
            (dana.value, dana.locked),
            (glass_policy::document::SettingValue::Number(5), true)
        );
        assert_eq!(
            get("priya", "lock.after_minutes").value,
            glass_policy::document::SettingValue::Number(30)
        );
        assert_eq!(
            policy
                .device_settings()
                .into_iter()
                .find(|r| r.id == "browser.home_page")
                .unwrap()
                .value,
            glass_policy::document::SettingValue::Text("https://intranet.acme.example".into())
        );
    }

    #[tokio::test]
    async fn a_change_from_another_sites_page_is_refused() {
        let t = console().await;
        let cookie = format!("{COOKIE}={}", t.cookie);
        let r = t
            .console
            .handle(&request(
                "POST",
                "/api/policy/publish",
                &json!({}),
                &[("cookie", &cookie), ("origin", "https://evil.example")],
            ))
            .await;
        assert_eq!(r.status, 403);
    }

    #[tokio::test]
    async fn ad3_edit_preview_publish_to_pilot_promote_and_roll_back() {
        let t = console().await;
        let pilot = t.sync("pilot-1", None, 0).await;
        let v1 = pilot.policy.unwrap().version;
        let (status, _) = t
            .call(
                "POST",
                "/api/fleet/pilot-1/ring",
                json!({ "ring": "pilot" }),
            )
            .await;
        assert_eq!(status, 200);
        t.sync("broad-1", None, 0).await;

        let (status, _) = t
            .call(
                "POST",
                "/api/policy/edit",
                json!({ "edit": "tier", "group": "ap-ops", "class": "send", "tier": "never" }),
            )
            .await;
        assert_eq!(status, 200);
        let (_, impact) = t.call("POST", "/api/policy/preview", json!({})).await;
        assert_eq!(impact["users"], 2, "{impact}");
        assert_eq!(impact["groups"], json!(["AP Operations"]));

        let (status, published) = t
            .call(
                "POST",
                "/api/policy/publish",
                json!({ "note": "AP sends never" }),
            )
            .await;
        assert_eq!(status, 200, "{published}");
        let v2 = published["version"].as_u64().unwrap();
        assert_eq!(v2, v1 + 1);

        // DV-1: devices verify the release against the org's key.
        let got = t
            .sync("pilot-1", Some(v1), 0)
            .await
            .policy
            .expect("pilot gets it");
        let release: PolicyRelease = got
            .open(DocKind::Policy, "acme", &[t.key], Some(v1))
            .unwrap();
        assert!(
            release.policy.contains("members = [\"dana\", \"sam\"]"),
            "members from the directory"
        );
        assert!(
            t.sync("broad-1", Some(v1), 0).await.policy.is_none(),
            "broad waits"
        );

        for ring in ["early", "broad"] {
            let (status, _) = t
                .call("POST", "/api/policy/promote", json!({ "ring": ring }))
                .await;
            assert_eq!(status, 200);
        }
        assert_eq!(
            t.sync("broad-1", Some(v1), 0).await.policy.unwrap().version,
            v2
        );

        let (status, back) = t
            .call("POST", "/api/policy/rollback", json!({ "to": v1 }))
            .await;
        assert_eq!(status, 200);
        let v3 = back["version"].as_u64().unwrap();
        let got = t.sync("broad-1", Some(v2), 0).await.policy.unwrap();
        assert_eq!(
            got.version, v3,
            "a rollback is a newer version, everywhere at once"
        );
        let release: PolicyRelease = got
            .open(DocKind::Policy, "acme", &[t.key], Some(v2))
            .unwrap();
        assert_eq!(release.rolls_back_to, Some(v1));
        assert!(!release.policy.contains("\"mail.send\" = \"never\""));

        let (_, page) = t.call("GET", "/api/policy?group=ap-ops", Value::Null).await;
        assert_eq!(page["group"]["display"], "AP Operations");
        assert_eq!(page["live"]["rollbacks_7d"], 1);
    }

    #[tokio::test]
    async fn nv3_ad2_mail_settings_are_an_edit_previewed_and_published_like_policy() {
        let t = console().await;
        let mail = json!({
            "service": "imap",
            "address_template": "{login}@acme.example",
            "incoming": { "host": "imap.acme.example", "port": 993, "security": "tls" },
            "outgoing": { "host": "smtp.acme.example", "port": 587, "security": "starttls" },
        });
        let (status, _) = t
            .call(
                "POST",
                "/api/policy/edit",
                json!({ "edit": "mail", "mail": mail }),
            )
            .await;
        assert_eq!(status, 200);
        let (_, page) = t.call("GET", "/api/mail", Value::Null).await;
        assert_eq!(page["draft"]["outgoing"]["host"], "smtp.acme.example");
        assert_eq!(page["changed"], true);
        let (_, impact) = t.call("POST", "/api/policy/preview", json!({})).await;
        let changes = impact["changes"].to_string();
        assert!(
            changes.contains("Everyone: mail through IMAP and SMTP (imap.acme.example:993, smtp.acme.example:587)"),
            "{changes}"
        );

        // What a device would refuse, the console doesn't keep.
        let (status, _) = t
            .call(
                "POST",
                "/api/policy/edit",
                json!({ "edit": "mail", "mail": { "service": "imap" } }),
            )
            .await;
        assert_eq!(status, 400);

        let (status, published) = t.call("POST", "/api/policy/publish", json!({})).await;
        assert_eq!(status, 200);
        let v = published["version"].as_u64().unwrap();
        // A pilot device gets it.
        t.sync("d", None, 0).await;
        t.call("POST", "/api/fleet/d/ring", json!({ "ring": "pilot" }))
            .await;
        let got = t.sync("d", None, 0).await.policy.unwrap();
        assert_eq!(got.version, v);
        let release: PolicyRelease = got.open(DocKind::Policy, "acme", &[t.key], None).unwrap();
        let policy = glass_policy::Policy::from_toml(&release.policy).unwrap();
        assert_eq!(
            policy.mail().unwrap().incoming.as_ref().unwrap().host,
            "imap.acme.example"
        );
        let (_, page) = t.call("GET", "/api/mail", Value::Null).await;
        assert_eq!(page["changed"], false);
        assert_eq!(page["rings"][0]["mail"]["service"], "imap");
    }

    #[tokio::test]
    async fn an_invalid_draft_is_refused_with_the_reason() {
        let t = console().await;
        let (status, body) = t
            .call(
                "POST",
                "/api/policy/edit",
                json!({ "edit": "egress", "agent": "invoice-demo", "egress": ["no port"] }),
            )
            .await;
        assert_eq!(status, 400);
        assert!(body["error"].as_str().unwrap().contains("host:port"));
    }

    #[tokio::test]
    async fn adr0023_a_waiting_device_hears_of_a_release_at_once() {
        let t = Arc::new(console().await);
        let v1 = t.sync("d", None, 0).await.policy.unwrap().version;
        t.call("POST", "/api/fleet/d/ring", json!({ "ring": "pilot" }))
            .await;
        let waiting = {
            let t = Arc::clone(&t);
            tokio::spawn(async move {
                let started = std::time::Instant::now();
                let answer = t.sync("d", Some(v1), 20).await;
                (answer, started.elapsed())
            })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        t.call(
            "POST",
            "/api/policy/edit",
            json!({ "edit": "tier", "group": "everyone", "class": "export", "tier": "never" }),
        )
        .await;
        let (status, _) = t.call("POST", "/api/policy/publish", json!({})).await;
        assert_eq!(status, 200);
        let (answer, took) = waiting.await.unwrap();
        assert_eq!(answer.policy.unwrap().version, v1 + 1);
        assert!(took < Duration::from_secs(5), "{took:?}");
    }

    #[tokio::test]
    async fn devices_need_the_orgs_enrollment_token() {
        let t = console().await;
        let r = t
            .console
            .handle(&request(
                "POST",
                "/device/v1/sync",
                &json!({ "device": "d" }),
                &[("authorization", "Bearer wrong")],
            ))
            .await;
        assert_eq!(r.status, 401);
    }

    #[tokio::test]
    async fn dv11_approve_for_a_group_and_dv13_never_to_the_public_store() {
        let t = console().await;
        {
            let mut state = t.console.state();
            state
                .submit(crate::state::ConsoleItem {
                    item: glass_managed::CatalogItem {
                        id: "contract-review".parse().unwrap(),
                        kind: glass_managed::ItemKind::Agent,
                        name: "Contract Review agent".into(),
                        version: "1.4.0".into(),
                        publisher: "Clause Labs".into(),
                        publisher_verified: true,
                        internal: false,
                        summary: "Redlines".into(),
                        keywords: Vec::new(),
                        declares: Default::default(),
                        groups: Default::default(),
                        pinned: false,
                        provenance: Default::default(),
                    },
                    review: crate::state::Review::Waiting,
                    requested_by: "Legal Operations".into(),
                    checks: Vec::new(),
                    decided_by: None,
                })
                .unwrap();
        }
        let (status, approved) = t
            .call(
                "POST",
                "/api/catalog/contract-review/approve",
                json!({ "group": "legal-ops", "mode": "available", "pin": true }),
            )
            .await;
        assert_eq!(status, 200, "{approved}");
        let got = t.sync("d", None, 0).await.catalog.unwrap();
        let release: CatalogRelease = got.open(DocKind::Catalog, "acme", &[t.key], None).unwrap();
        assert_eq!(release.items.len(), 1);
        assert_eq!(release.items[0].groups["legal-ops"], InstallMode::Available);
        assert!(release.items[0].pinned);

        let (status, refused) = t
            .call("POST", "/api/catalog/contract-review/public", json!({}))
            .await;
        assert_eq!(status, 409);
        assert!(
            refused["error"]
                .as_str()
                .unwrap()
                .contains("never go to the public store"),
            "{refused}"
        );

        let (status, _) = t
            .call("POST", "/api/catalog/contract-review/block", json!({}))
            .await;
        assert_eq!(status, 200);
        let got = t.sync("d", Some(got.version), 0).await.catalog.unwrap();
        let release: CatalogRelease = got.open(DocKind::Catalog, "acme", &[t.key], None).unwrap();
        assert!(
            release.items.is_empty(),
            "blocked items leave the catalog devices get"
        );
    }

    #[tokio::test]
    async fn ad4_the_fleet_page_counts_the_fixtures_devices() {
        let dir = tempfile::tempdir().unwrap();
        let directory = crate::fixture::directory(500, "root");
        let mut state = State::open(dir.path(), "acme", "Acme Corp", &directory).unwrap();
        crate::fixture::fill(&mut state, &directory, 500, 50).unwrap();
        let started = std::time::Instant::now();
        let fleet = views::fleet(&state, &directory, "http://x");
        let catalog = views::catalog(&state, &directory, &Titles::first_party());
        let policy = views::policy(&state, &directory, &Titles::first_party(), "ap-ops").unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(fleet["totals"]["devices"], 500);
        assert_eq!(catalog["items"].as_array().unwrap().len(), 50);
        assert!(catalog["counts"]["waiting"].as_u64().unwrap() >= 1);
        assert_eq!(
            policy["group"]["users"], 216,
            "214 people plus dana and sam"
        );
        let text = serde_json::to_string(&(fleet, catalog, policy))
            .unwrap()
            .to_lowercase();
        assert!(!text.contains(concat!("free", "bsd")), "BR-1");
    }
}
