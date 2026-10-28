//! The front's HTTP side (ADR 0066): the web app's files, and the API
//! the web app, the `glass` CLI and Teams use, each call made to glassd
//! as the person the front acts for.
//!
//! Who may call: here, whoever holds the front's token, as a cookie the
//! sign-in link sets or as `Authorization: Bearer` (the CLI reads it from
//! the file the front writes). Behind AWS's load balancer, its signed-in
//! person (ADR 0066, "Identity on a host").

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use glass_agents_api as api;
use glass_api::{
    AgentSummary, AnswerApproval, ApprovalAnswer, ApprovalScope, ApprovalView, ComputerRef,
    ComputerView, PersonaNew, PersonaView, ScreenInput, ScreenRequest, ScreenView, TaskDetail,
    TaskRef, TaskRequest, TaskView, UndoReport, UndoRequest, methods,
};
use glass_http::{ReadError, Request, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use crate::glassd::{Failure, Glassd};
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
        web: PathBuf,
        token: String,
        person: api::Person,
        org: String,
        teams: Option<crate::teams::Teams>,
    ) -> Arc<Self> {
        let (cursor, _) = watch::channel(0);
        Arc::new(Self {
            glassd,
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
        tokio::spawn(Arc::clone(&self).follow());
        tokio::spawn(Arc::clone(&self).teams_loop());
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

    pub async fn handle(&self, request: &Request) -> Response {
        let path = request.path.as_str();
        // Microsoft's Bot Framework: its own signed token, never ours.
        if path == "/api/teams/messages" {
            return self.teams_messages(request).await;
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
                person: self.person.clone(),
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
            ("POST", ["agents", id, "undo"]) => ok(&self.undo(id).await?),
            ("GET", ["approvals"]) => ok(&self.approvals().await?),
            ("POST", ["approvals", id]) => ok(&self.answer(id, body(request)?).await?),
            ("GET", ["templates"]) => ok(&self.templates().await?),
            ("GET", ["events"]) => {
                let since = query(&request.query, "since")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                ok(&self.events(since).await)
            }
            (
                _,
                [
                    "session" | "agents" | "approvals" | "templates" | "events",
                    ..,
                ],
            ) => Err(Failure::new(405, "method not allowed")),
            _ => Err(Failure::new(404, "no such route")),
        }
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
            by: self.person.name.clone(),
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
