//! The front's HTTP side (ADR 0066): the web app's files, and the API
//! the web app, the `glass` CLI and Teams use, each call made to glassd
//! as the person the front acts for.
//!
//! Who may call ([`Mode`]):
//! - On a laptop or a development host, whoever holds the front's token,
//!   as a cookie the sign-in link sets or as `Authorization: Bearer` (the
//!   CLI reads it from the file the front writes). The front acts for the
//!   account it runs as.
//! - Behind AWS's load balancer, each request is its signed-in person's
//!   (ADR 0067): the load balancer's signed claims, or a `glass` token
//!   glassd issued. The front opens a glassd session for that person for
//!   the request and nothing else, and glassd checks the proof itself.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use glass_agents_api as api;
use glass_api::{
    AgentSummary, AnswerApproval, ApprovalAnswer, ApprovalScope, ApprovalView, ComputerRef,
    ComputerView, DisplayInput, DisplayInputRequest, FrameRequest, FrameView, PersonaNew,
    PersonaView, PointerButton, ScreenInput, ScreenRequest, ScreenView, TaskDetail, TaskRef,
    TaskRequest, TaskView, UndoReport, UndoRequest, methods,
};
use glass_http::{ReadError, Request, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use crate::glassd::{Failure, Glassd};

tokio::task_local! {
    /// Who this request is for, behind the load balancer (ADR 0067).
    static PERSON: api::Person;
}

/// Who the front may act for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The account it runs as, for whoever holds its token: a laptop or a
    /// development host. Never off this machine's loopback address.
    Local,
    /// Each request's signed-in person, proven to glassd (ADR 0067).
    LoadBalancer,
}
use crate::views::{self, Answered};

/// The cookie the sign-in link sets.
const COOKIE: &str = "glass_agents";
/// How long `GET /api/v1/events` waits for something to change.
const EVENTS_WAIT: Duration = Duration::from_secs(25);
/// Changes kept for clients that are behind; older ones refetch all.
const CHANGES_KEPT: usize = 512;
/// Tasks of an agent its conversation shows, newest.
const CONVERSATION_TASKS: usize = 20;
/// The page may run only itself.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

type Reply<T> = Result<T, Failure>;

pub struct Front {
    pub glassd: Glassd,
    pub mode: Mode,
    /// The built web app (`apps/agents/dist`).
    pub web: PathBuf,
    /// Whoever holds it may use the API as [`Self::person`].
    pub token: String,
    pub person: api::Person,
    pub org: String,
    /// Teams, when the org connected it (ADR 0065 §13).
    pub teams: Option<crate::teams::Teams>,
    /// Answers given through the front: who, when and from where.
    answered: Mutex<BTreeMap<String, Answered>>,
    changes: Mutex<Changes>,
    cursor: watch::Sender<u64>,
}

/// What changed, by cursor: an agent's id, or everything.
#[derive(Debug, Default)]
struct Changes {
    log: VecDeque<(u64, Option<String>)>,
    cursor: u64,
}

impl Front {
    pub fn new(
        glassd: Glassd,
        mode: Mode,
        web: PathBuf,
        token: String,
        person: api::Person,
        org: String,
        teams: Option<crate::teams::Teams>,
    ) -> Arc<Self> {
        let (cursor, _) = watch::channel(0);
        Arc::new(Self {
            glassd,
            mode,
            web,
            token,
            person,
            org,
            teams,
            answered: Mutex::new(BTreeMap::new()),
            changes: Mutex::new(Changes::default()),
            cursor,
        })
    }

    fn changes(&self) -> MutexGuard<'_, Changes> {
        self.changes.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Something about an agent changed, or about everything (`None`).
    pub fn changed(&self, agent: Option<String>) {
        let cursor = {
            let mut changes = self.changes();
            changes.cursor += 1;
            let cursor = changes.cursor;
            changes.log.push_back((cursor, agent));
            while changes.log.len() > CHANGES_KEPT {
                changes.log.pop_front();
            }
            cursor
        };
        self.cursor.send_replace(cursor);
    }

    fn since(&self, since: u64) -> Option<api::Events> {
        let changes = self.changes();
        if changes.cursor <= since {
            return None;
        }
        let behind = changes
            .log
            .front()
            .is_none_or(|(first, _)| *first > since + 1);
        let mut agents = BTreeSet::new();
        let mut everything = behind;
        for (_, agent) in changes.log.iter().filter(|(at, _)| *at > since) {
            match agent {
                Some(id) => {
                    agents.insert(id.clone());
                }
                None => everything = true,
            }
        }
        Some(api::Events {
            cursor: changes.cursor,
            agents: if everything {
                Vec::new()
            } else {
                agents.into_iter().collect()
            },
        })
    }

    /// Follows glassd's events for as long as the front runs: a task's
    /// change is its agent's.
    pub async fn follow(self: Arc<Self>) {
        let mut wait = Duration::from_millis(250);
        loop {
            match self.glassd.subscribe().await {
                Ok(client) => {
                    wait = Duration::from_millis(250);
                    // Whatever happened while we weren't listening.
                    self.changed(None);
                    while let Some(note) = client.next_notification().await {
                        if note.method != methods::EVENT {
                            continue;
                        }
                        let event: Option<glass_api::Event> =
                            note.params.and_then(|p| serde_json::from_value(p).ok());
                        match event {
                            Some(glass_api::Event::TaskUpdated { task }) => {
                                if let Some(persona) = task.persona {
                                    self.changed(Some(persona));
                                }
                            }
                            Some(glass_api::Event::TaskRemoved { .. }) => self.changed(None),
                            _ => {}
                        }
                    }
                }
                Err(e) => tracing::warn!("glassd's events: {e}"),
            }
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(Duration::from_secs(10));
        }
    }

    /// Serves until the listener fails.
    pub async fn serve(self: Arc<Self>, listener: TcpListener) -> anyhow::Result<()> {
        // Behind the load balancer each request follows its own person's
        // events; the front itself sees no one's.
        if self.mode == Mode::Local {
            tokio::spawn(Arc::clone(&self).follow());
            tokio::spawn(Arc::clone(&self).teams_loop());
        }
        loop {
            let (stream, _) = listener.accept().await?;
            let front = Arc::clone(&self);
            tokio::spawn(async move { front.connection(stream).await });
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

    /// The person this request is for: its own, behind the load balancer.
    pub fn person(&self) -> api::Person {
        PERSON
            .try_with(Clone::clone)
            .unwrap_or_else(|_| self.person.clone())
    }

    pub async fn handle(&self, request: &Request) -> Response {
        let path = request.path.as_str();
        // Microsoft's Bot Framework: its own signed token, never ours.
        if path == "/api/teams/messages" {
            if self.mode == Mode::LoadBalancer {
                return error(
                    503,
                    "Teams isn't linked to people on this host yet (ADR 0067)",
                );
            }
            return self.teams_messages(request).await;
        }
        if self.mode == Mode::LoadBalancer {
            return self.handle_signed_in(request).await;
        }
        if let Some(rest) = path.strip_prefix(api::PREFIX) {
            if let Err(response) = check_origin(request) {
                return response;
            }
            if !self.authorized(request) {
                return error(
                    401,
                    "Sign in: open the link glass-agents printed as it started",
                );
            }
            return match self.api(rest.trim_start_matches('/'), request).await {
                Ok(response) => response,
                Err(failure) => error(failure.status, &failure.message),
            };
        }
        if request.method != "GET" && request.method != "HEAD" {
            return error(405, "method not allowed");
        }
        // The sign-in link: its token becomes a cookie, and the page opens.
        if path == "/"
            && let Some(token) = query(&request.query, "token")
        {
            if !constant_time_eq(token.as_bytes(), self.token.as_bytes()) {
                return error(401, "That sign-in link is out of date");
            }
            return Response::new(303, "text/plain; charset=utf-8", "Signed in")
                .with("Location", "/")
                .with(
                    "Set-Cookie",
                    format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict"),
                );
        }
        self.file(path).await
    }

    /// Behind the load balancer: every page and call is its signed-in
    /// person's, or a `glass` token's. Nothing is served to no one.
    async fn handle_signed_in(&self, request: &Request) -> Response {
        let path = request.path.as_str();
        if let Some(rest) = path.strip_prefix(api::PREFIX) {
            if let Err(response) = check_origin(request) {
                return response;
            }
            let rest = rest.trim_start_matches('/');
            // `glass login`'s code, traded once for a token: for no one yet.
            if rest == "tokens" && request.method == "POST" {
                return match self.exchange(request).await {
                    Ok(response) => response,
                    Err(failure) => error(failure.status, &failure.message),
                };
            }
            let work = self.api(rest, request);
            return match self.as_person(request, work).await {
                Ok(Ok(response)) => response,
                Ok(Err(failure)) | Err(failure) => error(failure.status, &failure.message),
            };
        }
        if path == "/login/cli" {
            return match self.login_cli(request).await {
                Ok(response) => response,
                Err(failure) => error(failure.status, &failure.message),
            };
        }
        if request.method != "GET" && request.method != "HEAD" {
            return error(405, "method not allowed");
        }
        // The page itself: the load balancer has signed its person in.
        if request.header("x-amzn-oidc-data").is_none() {
            return error(401, "Sign in through your organization's sign-in");
        }
        self.file(path).await
    }

    /// What proves who this request is for: the load balancer's signed
    /// claims, or a `glass` token.
    fn proof(request: &Request) -> Option<glass_api::SessionAs> {
        if let Some(assertion) = request.header("x-amzn-oidc-data") {
            return Some(glass_api::SessionAs::LoadBalancer {
                assertion: assertion.to_owned(),
            });
        }
        let bearer = request
            .header("authorization")?
            .strip_prefix("Bearer ")?
            .trim();
        bearer
            .starts_with(glass_api::TOKEN_PREFIX)
            .then(|| glass_api::SessionAs::Token {
                token: bearer.to_owned(),
            })
    }

    /// Runs `work` as this request's person, through a glassd session of
    /// its own that glassd opened only after checking the proof.
    async fn as_person<T>(
        &self,
        request: &Request,
        work: impl std::future::Future<Output = T>,
    ) -> Reply<T> {
        let proof = Self::proof(request)
            .ok_or_else(|| Failure::new(401, "Sign in through your organization's sign-in"))?;
        let (session, person) = self.glassd.session(&proof).await?;
        let person = api::Person {
            id: person.key,
            name: person.name,
            email: person.email,
        };
        Ok(PERSON.scope(person, Glassd::scoped(session, work)).await)
    }

    /// `POST /api/v1/tokens` with `Authorization: Bearer gac_…`: the code
    /// the browser handed `glass login`, traded once for a token.
    async fn exchange(&self, request: &Request) -> Reply<Response> {
        let code = request
            .header("authorization")
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|c| c.starts_with(glass_api::CODE_PREFIX))
            .ok_or_else(|| Failure::new(401, "Run glass login to sign in"))?;
        ok(&self.glassd.exchange(code).await?)
    }

    /// `glass login` (ADR 0067): its browser page, signed in at the load
    /// balancer. `GET` asks the person; their `POST` gets a one-time code
    /// from glassd and sends it to `glass` on their own computer, on its
    /// loopback address. The token itself never passes through the browser.
    async fn login_cli(&self, request: &Request) -> Reply<Response> {
        let form = if request.method == "POST" {
            if let Err(response) = check_origin(request) {
                return Ok(response);
            }
            String::from_utf8_lossy(&request.body).into_owned()
        } else {
            request.query.clone()
        };
        let port: u16 = query(&form, "port")
            .and_then(|p| p.parse().ok())
            .filter(|p| *p >= 1024)
            .ok_or_else(|| Failure::new(400, "glass login didn't say where it waits"))?;
        let state = query(&form, "state")
            .filter(|s| {
                (16..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            .ok_or_else(|| Failure::new(400, "glass login didn't say which login this is"))?;
        let host = query(&form, "host")
            .filter(|h| {
                h.len() <= 64
                    && h.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
            })
            .unwrap_or("your computer");
        // Who asks: `glass`, or the desktop app (ADR 0071). Nothing else
        // is named on the page.
        let app = query(&form, "app") == Some("desktop");
        let (who, who_starts) = if app {
            ("the Glasshouse Agents app", "The Glasshouse Agents app")
        } else {
            ("glass", "glass")
        };
        if request.method == "GET" {
            let me: api::Person = self.as_person(request, async { self.person() }).await?;
            // The referrer policy is the page's own: every response says
            // no-referrer, with which a browser's form post says its origin
            // is "null", and changes from anywhere but this page are refused
            // (check_origin). Same-origin gives this page its origin, and
            // glass on 127.0.0.1 still no referrer. Styled as the web app.
            let page = format!(
                "<!doctype html><meta charset=\"utf-8\">\
                 <meta name=\"referrer\" content=\"same-origin\">\
                 <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
                 <title>Sign in {who}</title><link rel=\"stylesheet\" href=\"/app.css\">\
                 <main class=\"page\"><h1>Let {who} on {host} use your agents?</h1>\
                 <p class=\"sub\">Signed in as {name}. {who_starts} will reach your agents and \
                 your computer as you, for 30 days or until you sign it out.</p>\
                 <form method=\"post\" action=\"/login/cli\">\
                 <input type=\"hidden\" name=\"port\" value=\"{port}\">\
                 <input type=\"hidden\" name=\"state\" value=\"{state}\">\
                 <input type=\"hidden\" name=\"host\" value=\"{host}\">{app_field}\
                 <button class=\"btn primary big\">Allow</button></form></main>",
                name = html_text(&me.name),
                app_field = if app {
                    "<input type=\"hidden\" name=\"app\" value=\"desktop\">"
                } else {
                    ""
                },
            );
            return Ok(Response::new(200, "text/html; charset=utf-8", page)
                .with(
                    "Content-Security-Policy",
                    format!(
                        "default-src 'none'; style-src 'self'; \
                         form-action 'self' http://127.0.0.1:{port}; frame-ancestors 'none'"
                    ),
                )
                .with("X-Content-Type-Options", "nosniff")
                .with("Cache-Control", "no-store"));
        }
        let code: glass_api::LoginCode = self
            .as_person(request, self.glassd.call(methods::TOKEN_CODE, ()))
            .await??;
        Ok(
            Response::new(303, "text/plain; charset=utf-8", format!("Back to {who}"))
                .with(
                    "Location",
                    format!(
                        "http://127.0.0.1:{port}/callback?code={}&state={state}",
                        code.code
                    ),
                )
                .with("Cache-Control", "no-store"),
        )
    }

    /// Live updates behind the load balancer: this request's person's
    /// events, from a subscription of their own. The cursor is a time in
    /// milliseconds; anything of theirs that changed after it answers at
    /// once.
    async fn events_signed_in(&self, since: u64) -> Reply<api::Events> {
        let session = Glassd::session_client()
            .ok_or_else(|| Failure::new(401, "Sign in through your organization's sign-in"))?;
        // Subscribe first, so nothing between the look and the wait is lost.
        let _: serde_json::Value = session
            .call(methods::EVENTS_SUBSCRIBE, serde_json::Value::Null)
            .await
            .map_err(Failure::from)?;
        let now = || u64::try_from(glass_types::Timestamp::now().unix_millis()).unwrap_or(0);
        let cursor = now();
        if since == 0 {
            return Ok(api::Events {
                cursor,
                agents: Vec::new(),
            });
        }
        let changed: Vec<String> = self
            .tasks()
            .await?
            .into_iter()
            .filter(|t| u64::try_from(t.updated_at.unix_millis()).unwrap_or(0) > since)
            .filter_map(|t| t.persona)
            .collect();
        if !changed.is_empty() {
            return Ok(api::Events {
                cursor,
                agents: changed,
            });
        }
        let wait = async {
            while let Some(note) = session.next_notification().await {
                if note.method != methods::EVENT {
                    continue;
                }
                match note.params.and_then(|p| serde_json::from_value(p).ok()) {
                    Some(glass_api::Event::TaskUpdated { task }) => {
                        if let Some(persona) = task.persona {
                            return api::Events {
                                cursor: now(),
                                agents: vec![persona],
                            };
                        }
                    }
                    Some(glass_api::Event::TaskRemoved { .. }) => {
                        return api::Events {
                            cursor: now(),
                            agents: Vec::new(),
                        };
                    }
                    _ => {}
                }
            }
            api::Events {
                cursor: now(),
                agents: Vec::new(),
            }
        };
        Ok(tokio::time::timeout(EVENTS_WAIT, wait)
            .await
            .unwrap_or(api::Events {
                cursor: since,
                agents: Vec::new(),
            }))
    }

    fn authorized(&self, request: &Request) -> bool {
        let bearer = request
            .header("authorization")
            .and_then(|h| h.strip_prefix("Bearer "));
        let cookie = request.header("cookie").and_then(|c| {
            c.split(';')
                .filter_map(|c| c.trim().split_once('='))
                .find(|(k, _)| *k == COOKIE)
                .map(|(_, v)| v)
        });
        [bearer, cookie]
            .into_iter()
            .flatten()
            .any(|t| constant_time_eq(t.trim().as_bytes(), self.token.as_bytes()))
    }

    async fn file(&self, path: &str) -> Response {
        let (name, content_type) = match path {
            "/" | "/index.html" => ("index.html", "text/html; charset=utf-8"),
            "/app.js" => ("app.js", "text/javascript; charset=utf-8"),
            "/app.css" => ("app.css", "text/css; charset=utf-8"),
            // The page's own routes: one page, its address says where.
            p if p.starts_with("/agents/") || p == "/new" => {
                ("index.html", "text/html; charset=utf-8")
            }
            _ => return Response::text(404, "not found"),
        };
        match tokio::fs::read(self.web.join(name)).await {
            Ok(bytes) => Response::new(200, content_type, bytes)
                .with("Content-Security-Policy", CSP)
                .with("X-Content-Type-Options", "nosniff")
                .with("Cache-Control", "no-cache"),
            Err(_) => Response::text(503, "the web app isn't built; run make apps-build"),
        }
    }

    async fn api(&self, rest: &str, request: &Request) -> Reply<Response> {
        let parts: Vec<&str> = rest.split('/').collect();
        match (request.method.as_str(), parts.as_slice()) {
            ("GET", ["session"]) => ok(&api::Session {
                person: self.person(),
                org: self.org.clone(),
            }),
            ("GET", ["agents"]) => ok(&self.agents().await?),
            ("POST", ["agents"]) => ok(&self.make(body(request)?).await?),
            ("GET", ["agents", id]) => ok(&self.detail(id).await?),
            ("POST", ["agents", id, "ask"]) => ok(&self.ask(id, body(request)?).await?),
            ("GET", ["agents", id, "screen"]) => {
                let since = query(&request.query, "since")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                ok(&self.screen(id, since).await?)
            }
            ("POST", ["agents", id, "take-over"]) => {
                ok(&self.computer(id, methods::COMPUTER_TAKE_OVER).await?)
            }
            ("POST", ["agents", id, "hand-back"]) => {
                ok(&self.computer(id, methods::COMPUTER_HAND_BACK).await?)
            }
            ("POST", ["agents", id, "sleep"]) => {
                ok(&self.computer(id, methods::COMPUTER_SLEEP).await?)
            }
            ("POST", ["agents", id, "wake"]) => {
                ok(&self.computer(id, methods::COMPUTER_WAKE).await?)
            }
            ("POST", ["agents", id, "input"]) => ok(&self.input(id, body(request)?).await?),
            ("GET", ["agents", id, "display"]) => {
                let since = query(&request.query, "since").map(str::to_owned);
                ok(&self.display(id, since).await?)
            }
            ("POST", ["agents", id, "display"]) => ok(&self.drive(id, body(request)?).await?),
            ("POST", ["agents", id, "undo"]) => ok(&self.undo(id).await?),
            ("GET", ["approvals"]) => ok(&self.approvals().await?),
            ("POST", ["approvals", id]) => ok(&self.answer(id, body(request)?).await?),
            ("GET", ["templates"]) => ok(&self.templates().await?),
            ("GET", ["schedules"]) => ok(&self.schedules().await?),
            ("POST", ["agents", id, "schedules"]) => ok(&self.schedule(id, body(request)?).await?),
            ("POST", ["schedules", id, on @ ("on" | "off")]) => {
                ok(&self.schedule_enable(id, *on == "on").await?)
            }
            ("DELETE", ["schedules", id]) => {
                let () = self
                    .glassd
                    .call(
                        methods::SCHEDULE_REMOVE,
                        glass_api::ScheduleRef {
                            id: (*id).to_owned(),
                        },
                    )
                    .await?;
                ok(&serde_json::json!({}))
            }
            ("GET", ["events"]) => {
                let since = query(&request.query, "since")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                if self.mode == Mode::LoadBalancer {
                    ok(&self.events_signed_in(since).await?)
                } else {
                    ok(&self.events(since).await)
                }
            }
            // Signs `glass` out everywhere: every token of the person's goes.
            ("DELETE", ["tokens"]) if self.mode == Mode::LoadBalancer => {
                let gone: u32 = self.glassd.call(methods::TOKEN_REVOKE, ()).await?;
                ok(&serde_json::json!({ "revoked": gone }))
            }
            (
                _,
                [
                    "session" | "agents" | "approvals" | "templates" | "schedules" | "events",
                    ..,
                ],
            ) => Err(Failure::new(405, "method not allowed")),
            _ => Err(Failure::new(404, "no such route")),
        }
    }

    /// The person's schedules (ADR 0069).
    async fn schedules(&self) -> Reply<Vec<api::Schedule>> {
        let all: Vec<glass_api::ScheduleView> =
            self.glassd.call(methods::SCHEDULE_LIST, ()).await?;
        Ok(all.iter().map(views::schedule).collect())
    }

    /// Directs agent `id` on a schedule: glassd checks it's one of the
    /// person's.
    async fn schedule(&self, id: &str, new: api::NewSchedule) -> Reply<api::Schedule> {
        if !api::valid_id(id) {
            return Err(Failure::new(404, "no such agent"));
        }
        let made: glass_api::ScheduleView = self
            .glassd
            .call(
                methods::SCHEDULE_MAKE,
                glass_api::ScheduleNew {
                    persona: id.to_owned(),
                    prompt: new.prompt,
                    when: new.when,
                    zone: new.zone,
                },
            )
            .await?;
        Ok(views::schedule(&made))
    }

    async fn schedule_enable(&self, id: &str, enabled: bool) -> Reply<api::Schedule> {
        let changed: glass_api::ScheduleView = self
            .glassd
            .call(
                methods::SCHEDULE_ENABLE,
                glass_api::ScheduleEnable {
                    id: id.to_owned(),
                    enabled,
                },
            )
            .await?;
        Ok(views::schedule(&changed))
    }

    async fn computers(&self) -> Reply<Vec<ComputerView>> {
        self.glassd.call(methods::COMPUTER_LIST, ()).await
    }

    async fn tasks(&self) -> Reply<Vec<TaskView>> {
        self.glassd.call(methods::TASK_LIST, ()).await
    }

    /// A persona's tasks on its computer.
    fn by_persona<'a>(tasks: &'a [TaskView], computer: &str, persona: &str) -> Vec<&'a TaskView> {
        tasks
            .iter()
            .filter(|t| {
                t.computer.as_deref() == Some(computer) && t.persona.as_deref() == Some(persona)
            })
            .collect()
    }

    /// The person's computer, the one their agents share (ADR 0068).
    fn own(computers: Vec<ComputerView>) -> Option<ComputerView> {
        computers
            .into_iter()
            .find(|c| c.id == glass_api::PERSON_COMPUTER)
    }

    pub async fn agents(&self) -> Reply<Vec<api::AgentView>> {
        let (computers, tasks) = tokio::try_join!(self.computers(), self.tasks())?;
        let Some(computer) = Self::own(computers) else {
            return Ok(Vec::new());
        };
        Ok(computer
            .personas
            .iter()
            .map(|p| views::agent(&computer, p, &Self::by_persona(&tasks, &computer.id, &p.id)))
            .collect())
    }

    /// One of the person's agents: their computer, it, and its tasks,
    /// oldest first. Another person's agent is as absent as one that
    /// doesn't exist: glassd lists only the caller's computer.
    async fn find(&self, id: &str) -> Reply<(ComputerView, PersonaView, Vec<TaskView>)> {
        if !api::valid_id(id) {
            return Err(Failure::new(404, "no such agent"));
        }
        let (computers, tasks) = tokio::try_join!(self.computers(), self.tasks())?;
        let missing = || Failure::new(404, format!("you have no agent {id}"));
        let computer = Self::own(computers).ok_or_else(missing)?;
        let persona = computer
            .personas
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(missing)?;
        let mut own: Vec<TaskView> = Self::by_persona(&tasks, &computer.id, id)
            .into_iter()
            .cloned()
            .collect();
        own.sort_by_key(|t| t.started_at.unix_seconds());
        Ok((computer, persona, own))
    }

    async fn view(&self, id: &str) -> Reply<api::AgentView> {
        let (computer, persona, tasks) = self.find(id).await?;
        Ok(views::agent(
            &computer,
            &persona,
            &tasks.iter().collect::<Vec<_>>(),
        ))
    }

    async fn make(&self, new: api::NewAgent) -> Reply<api::AgentView> {
        let templates: Vec<AgentSummary> = self.glassd.call(methods::AGENTS_LIST, ()).await?;
        let template = templates
            .iter()
            .find(|t| t.id.as_str() == new.template && !t.attended)
            .ok_or_else(|| Failure::new(404, "IT hasn't made that agent available to you"))?;
        let name = new
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(&template.name)
            .to_owned();
        let taken: Vec<String> = Self::own(self.computers().await?)
            .map(|c| c.personas.into_iter().map(|p| p.id).collect())
            .unwrap_or_default();
        let id = views::new_id(&name, &taken);
        let made: ComputerView = self
            .glassd
            .call(
                methods::PERSONA_MAKE,
                PersonaNew {
                    id: id.clone(),
                    name,
                    role: template.description.clone(),
                    agent: template.id.clone(),
                },
            )
            .await?;
        let persona = made
            .personas
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| Failure::new(500, "glassd didn't keep the new agent"))?;
        self.changed(Some(id.clone()));
        Ok(views::agent(&made, persona, &[]))
    }

    async fn detail(&self, id: &str) -> Reply<api::AgentDetail> {
        let (computer, persona, tasks) = self.find(id).await?;
        let recent: Vec<&TaskView> = tasks.iter().rev().take(CONVERSATION_TASKS).collect();
        let mut details = Vec::with_capacity(recent.len());
        for task in recent.iter().rev() {
            let detail: TaskDetail = self
                .glassd
                .call(methods::TASK_GET, TaskRef { task: task.id })
                .await?;
            details.push(detail);
        }
        let card: Option<glass_api::HandoffCard> = self
            .glassd
            .call(
                methods::TASK_PLAN,
                TaskRequest {
                    persona: None,
                    agent: persona.agent.clone(),
                    prompt: "What can you reach?".into(),
                    scope: Default::default(),
                    title: None,
                    computer: None,
                },
            )
            .await
            .ok();
        let answered = self
            .answered
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        Ok(api::AgentDetail {
            agent: views::agent(&computer, &persona, &tasks.iter().collect::<Vec<_>>()),
            reach: views::reach(&computer, card.as_ref()),
            messages: views::messages(&details, &answered),
            record: views::record(&details, 50),
        })
    }

    pub async fn ask(&self, id: &str, ask: api::Ask) -> Reply<api::Asked> {
        let text = ask.text.trim();
        if text.is_empty() {
            return Err(Failure::new(400, "Say what you'd like it to do"));
        }
        let (computer, persona, _) = self.find(id).await?;
        let started: TaskView = self
            .glassd
            .call(
                methods::TASK_START,
                TaskRequest {
                    agent: persona.agent.clone(),
                    prompt: text.to_owned(),
                    scope: Default::default(),
                    title: None,
                    computer: Some(computer.id.clone()),
                    persona: Some(persona.id.clone()),
                },
            )
            .await?;
        self.changed(Some(persona.id));
        Ok(api::Asked {
            task: started.id.to_string(),
        })
    }

    /// The screen of the computer the agent works on: its person's, shared
    /// by all their agents.
    async fn screen(&self, id: &str, since: u64) -> Reply<api::Screen> {
        let (computer, _, _) = self.find(id).await?;
        let screen: ScreenView = self
            .glassd
            .call(
                methods::COMPUTER_SCREEN,
                ScreenRequest {
                    id: computer.id,
                    offset: since,
                },
            )
            .await?;
        Ok(api::Screen {
            offset: screen.offset,
            text: screen.text,
            taken_over: screen.taken_over,
            state: views::computer_state(screen.state, false),
        })
    }

    /// Takes over, hands back, sleeps or wakes the computer the agent works
    /// on: all the person's agents share it, so all of them change.
    async fn computer(&self, id: &str, method: &str) -> Reply<api::AgentView> {
        let (computer, _, _) = self.find(id).await?;
        let _: ComputerView = self
            .glassd
            .call(method, ComputerRef { id: computer.id })
            .await
            .map_err(|f| {
                if f.status == 500 && f.message.contains("method") {
                    Failure::new(501, "this host can't do that yet")
                } else {
                    f
                }
            })?;
        self.changed(None);
        self.view(id).await
    }

    /// What the display of the computer the agent works on shows (ADR
    /// 0072): unchanged since `since`, no picture.
    async fn display(&self, id: &str, since: Option<String>) -> Reply<api::Display> {
        let (computer, _, _) = self.find(id).await?;
        let since = since.filter(|v| v.len() <= 64 && v.bytes().all(|b| b.is_ascii_hexdigit()));
        let frame: FrameView = self
            .glassd
            .call(
                methods::COMPUTER_FRAME,
                FrameRequest {
                    id: computer.id,
                    since,
                },
            )
            .await?;
        Ok(api::Display {
            version: frame.version,
            width: frame.width,
            height: frame.height,
            png: frame.png,
            taken_over: frame.taken_over,
        })
    }

    /// The person's pointer and keys on that display, while they have it.
    async fn drive(&self, id: &str, input: api::DisplayEvents) -> Reply<serde_json::Value> {
        let (computer, _, _) = self.find(id).await?;
        let events = input
            .events
            .into_iter()
            .map(|e| match e {
                api::DisplayInput::Move { x, y } => DisplayInput::Move { x, y },
                api::DisplayInput::Button { button, down } => DisplayInput::Button {
                    button: match button {
                        api::Button::Left => PointerButton::Left,
                        api::Button::Middle => PointerButton::Middle,
                        api::Button::Right => PointerButton::Right,
                    },
                    down,
                },
                api::DisplayInput::Wheel { dy } => DisplayInput::Wheel { dy },
                api::DisplayInput::Key { code, down } => DisplayInput::Key { code, down },
            })
            .collect();
        let _: serde_json::Value = self
            .glassd
            .call(
                methods::COMPUTER_DISPLAY,
                DisplayInputRequest {
                    id: computer.id,
                    events,
                },
            )
            .await?;
        Ok(serde_json::json!({}))
    }

    async fn input(&self, id: &str, input: api::Input) -> Reply<api::Screen> {
        let (computer, _, _) = self.find(id).await?;
        let _: serde_json::Value = self
            .glassd
            .call(
                methods::COMPUTER_INPUT,
                ScreenInput {
                    id: computer.id,
                    line: input.line,
                },
            )
            .await?;
        self.screen(id, 0).await
    }

    /// Takes back its latest task's changes that can be.
    async fn undo(&self, id: &str) -> Reply<api::UndoResult> {
        let (_, _, tasks) = self.find(id).await?;
        for task in tasks.iter().rev() {
            let detail: TaskDetail = self
                .glassd
                .call(methods::TASK_GET, TaskRef { task: task.id })
                .await?;
            if !detail.changes.iter().any(|c| c.reversible && !c.undone) {
                continue;
            }
            let report: UndoReport = self
                .glassd
                .call(
                    methods::TASK_UNDO,
                    UndoRequest {
                        task: task.id,
                        to: None,
                        changes: Vec::new(),
                        dry_run: false,
                    },
                )
                .await?;
            self.changed(Some(id.to_owned()));
            let back = report.restored.len() + report.removed.len();
            return Ok(api::UndoResult {
                text: match back {
                    1 => "Put 1 file back as it was".into(),
                    n => format!("Put {n} files back as they were"),
                },
                left: report.not_undone,
            });
        }
        Err(Failure::new(409, "Nothing it did can be undone"))
    }

    pub async fn approvals(&self) -> Reply<Vec<api::PendingApproval>> {
        let (asks, computers, tasks) = tokio::try_join!(
            self.glassd
                .call::<Vec<ApprovalView>>(methods::APPROVAL_LIST, ()),
            self.computers(),
            self.tasks()
        )?;
        Ok(asks
            .iter()
            .filter(|a| a.state == glass_api::ApprovalState::Pending)
            .filter_map(|a| {
                let task = tasks.iter().find(|t| t.id == a.task)?;
                let persona = computers
                    .iter()
                    .filter(|c| Some(&c.id) == task.computer.as_ref())
                    .flat_map(|c| c.personas.iter())
                    .find(|p| Some(&p.id) == task.persona.as_ref())?;
                Some(api::PendingApproval {
                    agent: persona.id.clone(),
                    agent_name: persona.name.clone(),
                    approval: views::approval(a, None),
                })
            })
            .collect())
    }

    pub async fn answer(&self, id: &str, answer: api::Answer) -> Reply<api::ApprovalView> {
        let approval = id
            .parse()
            .map_err(|_| Failure::new(404, "no such question"))?;
        let answered: ApprovalView = self
            .glassd
            .call(
                methods::APPROVAL_ANSWER,
                AnswerApproval {
                    approval,
                    answer: if answer.yes {
                        ApprovalAnswer::Approve {
                            scope: ApprovalScope::Once,
                        }
                    } else {
                        ApprovalAnswer::Deny
                    },
                },
            )
            .await?;
        let record = Answered {
            yes: answer.yes,
            by: self.person().name,
            at: views::time(&glass_types::Timestamp::now()),
            via: answer.via,
        };
        self.answered
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_owned(), record.clone());
        self.changed(None);
        Ok(views::approval(&answered, Some(&record)))
    }

    /// How an approval answered through the front was answered.
    pub fn answered_state(&self, id: &str) -> Option<api::ApprovalState> {
        let answered = self.answered.lock().unwrap_or_else(|e| e.into_inner());
        let a = answered.get(id)?;
        Some(if a.yes {
            api::ApprovalState::Approved {
                by: a.by.clone(),
                at: a.at.clone(),
                via: a.via,
            }
        } else {
            api::ApprovalState::Denied {
                by: a.by.clone(),
                at: a.at.clone(),
                via: a.via,
            }
        })
    }

    /// What a task ended with, for people, once it has.
    pub async fn task_answer(&self, task: &str) -> Option<String> {
        let task: glass_types::TaskId = task.parse().ok()?;
        let detail: TaskDetail = self
            .glassd
            .call(methods::TASK_GET, TaskRef { task })
            .await
            .ok()?;
        let view = &detail.view;
        match view.state {
            glass_api::TileState::Done => Some(
                detail
                    .answer
                    .filter(|a| !a.trim().is_empty())
                    .unwrap_or_else(|| view.caption.clone()),
            ),
            glass_api::TileState::Failed => Some(format!(
                "I couldn't finish: {}",
                view.failure.as_deref().unwrap_or("it stopped")
            )),
            glass_api::TileState::Stopped => Some("Stopped.".into()),
            _ => None,
        }
    }

    async fn templates(&self) -> Reply<Vec<api::Template>> {
        let agents: Vec<AgentSummary> = self.glassd.call(methods::AGENTS_LIST, ()).await?;
        Ok(agents
            .iter()
            .filter(|a| !a.attended)
            .map(views::template)
            .collect())
    }

    async fn events(&self, since: u64) -> api::Events {
        let mut cursor = self.cursor.subscribe();
        let wait = async {
            loop {
                if let Some(events) = self.since(since) {
                    return events;
                }
                if cursor.changed().await.is_err() {
                    break;
                }
            }
            api::Events {
                cursor: since,
                agents: Vec::new(),
            }
        };
        match tokio::time::timeout(EVENTS_WAIT, wait).await {
            Ok(events) => events,
            Err(_) => api::Events {
                cursor: since.min(self.changes().cursor),
                agents: Vec::new(),
            },
        }
    }
}

fn ok(value: &impl Serialize) -> Reply<Response> {
    let value = serde_json::to_value(value)
        .map_err(|e| Failure::new(500, format!("an answer we couldn't write: {e}")))?;
    Ok(Response::json(200, &value))
}

/// Text for an HTML page, escaped.
fn html_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn error(status: u16, message: &str) -> Response {
    Response::json(status, &serde_json::json!({ "error": message }))
}

fn body<T: DeserializeOwned>(request: &Request) -> Reply<T> {
    let json = request
        .header("content-type")
        .is_some_and(|c| c.starts_with("application/json"));
    if !json {
        return Err(Failure::new(415, "send JSON"));
    }
    serde_json::from_slice(&request.body)
        .map_err(|e| Failure::new(400, format!("that request isn't what this takes: {e}")))
}

/// One parameter of a query string, as sent (ids and numbers only).
fn query<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

/// Changes come from the page itself, or from tools that send no Origin.
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
        Err(error(
            403,
            "changes come from Glasshouse Agents' own page only",
        ))
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
