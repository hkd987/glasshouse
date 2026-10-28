//! The front against a stand-in glassd and stand-ins for Microsoft (ADR
//! 0065 §13, ADR 0066): an agent asks in Teams, the person answers there,
//! and what they asked from Teams comes back there.

use std::sync::{Arc, Mutex};

use glass_api::{
    AgentMode, ApprovalState, ApprovalView, AskLabels, ComputerState, ComputerView, TaskDetail,
    TaskView, TileState,
};
use glass_rpc::{Connection, Handler, RpcError};
use glass_teams::testing::{TestKey, claims};
use glass_types::{ApprovalId, TaskId, Timestamp};
use serde_json::{Value, json};
use tokio::net::TcpListener;

use crate::glassd::Glassd;
use crate::server::Front;
use crate::teams::{EndpointsConfig, Teams, TeamsConfig};

const APP: &str = "6a1c2b3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";
const TENANT: &str = "72f988bf-86f1-41af-91ab-2d7cd011db47";
const DANA_AAD: &str = "a1b2c3d4-0000-4000-8000-00000000da0a";
const DANA: &str = "29:1dana";
const BOT: &str = "28:6a1c2b3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";
const CHAT: &str = "a:1dana-personal-chat";

/// glassd as the front sees it: one agent with a question waiting.
struct StandInGlassd {
    approval: ApprovalId,
    task: TaskId,
    asked: Mutex<Vec<Value>>,
    answered: Mutex<Option<bool>>,
}

impl StandInGlassd {
    fn computer(&self) -> ComputerView {
        ComputerView {
            id: glass_api::PERSON_COMPUTER.into(),
            name: "Your computer".into(),
            role: String::new(),
            agent: "desktop-agent".parse().unwrap(),
            state: ComputerState::Running,
            kind: "Firecracker microVM".into(),
            contained: true,
            disk_bytes: 1 << 20,
            live_tasks: vec![self.task],
            taken_over: false,
            stored: None,
            created_at: Timestamp::now(),
            personas: vec![glass_api::PersonaView {
                id: "reconciler".into(),
                name: "Reconciler".into(),
                role: "Matches invoices".into(),
                agent: "desktop-agent".parse().unwrap(),
                created_at: Timestamp::now(),
            }],
        }
    }

    fn task(&self, id: TaskId, state: TileState) -> TaskView {
        TaskView {
            id,
            agent: "desktop-agent".parse().unwrap(),
            agent_name: "Desktop agent".into(),
            user: "dana".into(),
            mode: AgentMode::Actions,
            state,
            title: "March invoices".into(),
            caption: "Matching invoices".into(),
            progress: None,
            feed: Vec::new(),
            needs: None,
            receipt: None,
            failure: None,
            started_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            driver_socket: None,
            continues: None,
            computer: Some(glass_api::PERSON_COMPUTER.into()),
            persona: Some("reconciler".into()),
        }
    }

    fn approval(&self) -> ApprovalView {
        ApprovalView {
            id: self.approval,
            task: self.task,
            agent: "desktop-agent".parse().unwrap(),
            action: "mail.send".parse().unwrap(),
            summary: "Send 3 emails, from you".into(),
            items: vec!["Harbor Paper Co.".into(), "Delta Freight".into()],
            texts: Vec::new(),
            rule: "mail.send asks".into(),
            requested_at: Timestamp::now(),
            expires_at: Timestamp::now(),
            state: match *self.answered.lock().unwrap() {
                None => ApprovalState::Pending,
                Some(true) => ApprovalState::Approved {
                    scope: glass_api::ApprovalScope::Once,
                },
                Some(false) => ApprovalState::Denied,
            },
            labels: AskLabels::new("Review", "Send 3 emails"),
        }
    }
}

impl Handler for StandInGlassd {
    async fn call(
        &self,
        _conn: Arc<Connection>,
        method: String,
        params: Value,
    ) -> Result<Value, RpcError> {
        use glass_api::methods as m;
        let value = match method.as_str() {
            m::COMPUTER_LIST => json!([self.computer()]),
            m::TASK_LIST => json!([self.task(self.task, TileState::NeedsYou)]),
            m::APPROVAL_LIST => {
                let pending = self.answered.lock().unwrap().is_none();
                json!(if pending {
                    vec![self.approval()]
                } else {
                    vec![]
                })
            }
            m::APPROVAL_ANSWER => {
                let yes = params["answer"]["decision"] == "approve";
                *self.answered.lock().unwrap() = Some(yes);
                json!(self.approval())
            }
            m::TASK_START => {
                self.asked.lock().unwrap().push(params);
                json!(self.task(TaskId::new(), TileState::Starting))
            }
            m::TASK_GET => {
                let id: TaskId = serde_json::from_value(params["task"].clone()).unwrap();
                json!(TaskDetail {
                    view: self.task(id, TileState::Done),
                    prompt: "which invoices don't match?".into(),
                    plan: Vec::new(),
                    now: String::new(),
                    activity: Vec::new(),
                    changes: Vec::new(),
                    cant_touch: Vec::new(),
                    snapshots: Vec::new(),
                    answer: Some("Seven of 214 don't match.".into()),
                })
            }
            m::EVENTS_SUBSCRIBE => Value::Null,
            other => return Err(RpcError::new(-32601, format!("no {other}"))),
        };
        Ok(value)
    }
}

/// Microsoft's side: its keys, its sign-in, and the Bot Connector, which
/// keeps what the app sent.
#[derive(Default)]
struct Microsoft {
    sent: Mutex<Vec<(String, String, Value)>>,
}

async fn stand_in_microsoft(key: Arc<TestKey>) -> (String, Arc<Microsoft>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let microsoft = Arc::new(Microsoft::default());
    let (kept, url) = (Arc::clone(&microsoft), base.clone());
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (key, kept, url) = (Arc::clone(&key), Arc::clone(&kept), url.clone());
            tokio::spawn(async move {
                let Ok(request) = glass_http::read_request(&mut stream).await else {
                    return;
                };
                let reply = match (request.method.as_str(), request.path.as_str()) {
                    ("GET", "/openid") => json!({ "jwks_uri": format!("{url}/keys") }),
                    ("GET", "/keys") => serde_json::from_str(&key.jwks()).unwrap(),
                    ("POST", p) if p.ends_with("/oauth2/v2.0/token") => {
                        json!({"access_token": "bot-token", "expires_in": 3600, "token_type": "Bearer"})
                    }
                    (method, path) => {
                        let body = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
                        kept.sent
                            .lock()
                            .unwrap()
                            .push((method.to_owned(), path.to_owned(), body));
                        json!({ "id": format!("msg-{}", kept.sent.lock().unwrap().len()) })
                    }
                };
                let response = glass_http::Response::json(200, &reply);
                let _ = glass_http::write_response(&mut stream, response, false).await;
            });
        }
    });
    (base, microsoft)
}

struct World {
    front: Arc<Front>,
    glassd: Arc<StandInGlassd>,
    microsoft: Arc<Microsoft>,
    key: Arc<TestKey>,
    service: String,
    _dir: tempfile::TempDir,
}

async fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("control.sock");
    let glassd = Arc::new(StandInGlassd {
        approval: ApprovalId::new(),
        task: TaskId::new(),
        asked: Mutex::new(Vec::new()),
        answered: Mutex::new(None),
    });
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    tokio::spawn(glass_rpc::serve(listener, Arc::clone(&glassd)));

    let key = Arc::new(TestKey::generate("bf-key-1"));
    let (base, microsoft) = stand_in_microsoft(Arc::clone(&key)).await;
    std::fs::write(dir.path().join("secret"), "app-secret\n").unwrap();
    let teams = Teams::new(TeamsConfig {
        app_id: APP.into(),
        tenant_id: TENANT.into(),
        secret_file: dir.path().join("secret"),
        person_aad_object_id: DANA_AAD.into(),
        open_url: "https://agents.example.com".into(),
        state_file: dir.path().join("teams.json"),
        endpoints: Some(EndpointsConfig {
            openid_metadata: format!("{base}/openid"),
            login: base.clone(),
            service_hosts: Vec::new(),
            allow_loopback_http: true,
        }),
    })
    .unwrap();
    let front = Front::new(
        Glassd::new(socket),
        crate::server::Mode::Local,
        dir.path().join("web"),
        "token".into(),
        glass_agents_api::Person {
            id: "dana".into(),
            name: "Dana Alvarez".into(),
            email: String::new(),
        },
        "Acme Corp".into(),
        Some(teams),
    );
    World {
        front,
        glassd,
        microsoft,
        key,
        service: format!("{base}/"),
        _dir: dir,
    }
}

impl World {
    /// A call from Microsoft, signed as it signs them.
    async fn signed(&self, activity: &Value) -> glass_http::Response {
        let now = Timestamp::now().unix_seconds();
        let token = self.key.sign(&claims(APP, &self.service, now));
        self.post(activity, &format!("Bearer {token}")).await
    }

    async fn post(&self, activity: &Value, authorization: &str) -> glass_http::Response {
        let request = glass_http::Request {
            method: "POST".into(),
            path: "/api/teams/messages".into(),
            query: String::new(),
            headers: vec![
                ("authorization".into(), authorization.into()),
                ("content-type".into(), "application/json".into()),
            ],
            body: activity.to_string().into_bytes(),
        };
        self.front.handle(&request).await
    }

    fn sent(&self) -> Vec<(String, String, Value)> {
        self.microsoft.sent.lock().unwrap().clone()
    }

    fn activity(&self, kind: &str, from_aad: &str, extra: Value) -> Value {
        let mut activity = json!({
            "type": kind,
            "id": "1761562500456",
            "channelId": "msteams",
            "serviceUrl": self.service,
            "from": {"id": DANA, "name": "Dana Alvarez", "aadObjectId": from_aad},
            "recipient": {"id": BOT, "name": "Glasshouse Agents"},
            "conversation": {"conversationType": "personal", "tenantId": TENANT, "id": CHAT},
            "channelData": {"tenant": {"id": TENANT}},
        });
        if let (Some(activity), Value::Object(extra)) = (activity.as_object_mut(), extra) {
            activity.extend(extra);
        }
        activity
    }
}

fn body(response: &glass_http::Response) -> Value {
    serde_json::from_slice(&response.body).unwrap_or(Value::Null)
}

#[tokio::test(flavor = "multi_thread")]
async fn adr0065_an_agent_asks_in_teams_and_the_answer_comes_back() {
    let w = world().await;

    // Only Microsoft's signed calls get in.
    let unsigned = w
        .post(&w.activity("message", DANA_AAD, json!({})), "Bearer x.y.z")
        .await;
    assert_eq!(unsigned.status, 401);
    assert!(unsigned.body.is_empty());

    // Installed for Dana: the chat is kept, and she's greeted.
    let installed = w
        .signed(&w.activity("installationUpdate", DANA_AAD, json!({"action": "add"})))
        .await;
    assert_eq!(installed.status, 200);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let sent = w.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(
        sent[0].1,
        "/v3/conversations/a%3A1dana-personal-chat/activities"
    );
    assert!(
        sent[0].2["text"]
            .as_str()
            .unwrap()
            .starts_with("Hi Dana Alvarez")
    );

    // The agent's question reaches her chat as a card.
    w.front.teams_look_once().await.unwrap();
    let sent = w.sent();
    assert_eq!(sent.len(), 2, "{sent:?}");
    let card = sent[1].2.to_string();
    assert!(card.contains("Reconciler needs you"), "{card}");
    assert!(card.contains("Send 3 emails, from you"), "{card}");
    // Once.
    w.front.teams_look_once().await.unwrap();
    assert_eq!(w.sent().len(), 2);

    let approval = w.glassd.approval.to_string();
    let press = |aad: &str| {
        w.activity(
            "invoke",
            aad,
            json!({
                "name": "adaptiveCard/action",
                "replyToId": "msg-2",
                "value": {"action": {"type": "Action.Execute", "verb": "approve", "data": {"approval": approval}}},
            }),
        )
    };
    // Someone else pressing it is told no, and nothing is answered.
    let other = w
        .signed(&press("ffffffff-0000-4000-8000-000000000000"))
        .await;
    assert!(
        body(&other)
            .to_string()
            .contains("Only Dana Alvarez can answer this")
    );
    assert_eq!(*w.glassd.answered.lock().unwrap(), None);

    // Dana approves: glassd hears it, and the card shows who answered.
    let answered = w.signed(&press(DANA_AAD)).await;
    assert_eq!(*w.glassd.answered.lock().unwrap(), Some(true));
    let answered = body(&answered);
    assert_eq!(answered["statusCode"], 200);
    let text = answered.to_string();
    assert!(text.contains("Dana Alvarez"), "{text}");
    assert!(
        !text.contains("Action.Execute"),
        "an answered card has no buttons: {text}"
    );

    // She asks an agent by name; it answers in the same chat when done.
    let asked = w
        .signed(&w.activity(
            "message",
            DANA_AAD,
            json!({"text": "Reconciler, which invoices don't match?"}),
        ))
        .await;
    assert_eq!(asked.status, 200);
    let started = w.glassd.asked.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["prompt"], "which invoices don't match?");
    assert_eq!(started[0]["computer"], glass_api::PERSON_COMPUTER);
    assert_eq!(started[0]["persona"], "reconciler");
    w.front.teams_look_once().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let texts: Vec<String> = w
        .sent()
        .iter()
        .filter_map(|(_, _, a)| a["text"].as_str().map(str::to_owned))
        .collect();
    assert!(
        texts
            .iter()
            .any(|t| t == "Reconciler is on it. It'll answer here."),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t == "Reconciler: Seven of 214 don't match."),
        "{texts:?}"
    );
}

/// glassd on an agent host, as the front sees it (ADR 0067): a connection
/// acts for whoever its proof names, and sees only that person's computer.
#[derive(Default)]
struct PeopleGlassd {
    sessions: Mutex<std::collections::HashMap<u64, String>>,
    codes: Mutex<Vec<String>>,
}

impl Handler for PeopleGlassd {
    async fn call(
        &self,
        conn: Arc<Connection>,
        method: String,
        params: Value,
    ) -> Result<Value, RpcError> {
        use glass_api::methods as m;
        let who = self.sessions.lock().unwrap().get(&conn.id()).cloned();
        match method.as_str() {
            m::SESSION_AS => {
                // The real glassd checks ES256 against AWS's key; here the
                // stand-in load balancer's claims are just a name.
                let person = match (
                    params["proof"].as_str(),
                    params["assertion"].as_str(),
                    params["token"].as_str(),
                ) {
                    (Some("load_balancer"), Some(a), _) if a.starts_with("signed-for-") => {
                        a["signed-for-".len()..].to_owned()
                    }
                    (Some("token"), _, Some("gat_sams-token")) => "sam".into(),
                    _ => return Err(RpcError::permission_denied("not acting for anyone: forged")),
                };
                self.sessions
                    .lock()
                    .unwrap()
                    .insert(conn.id(), person.clone());
                Ok(
                    json!({"key": person, "name": person, "email": format!("{person}@example.com"), "proven_by": "load_balancer"}),
                )
            }
            m::TOKEN_EXCHANGE => {
                let code = params["code"].as_str().unwrap_or_default().to_owned();
                let mut codes = self.codes.lock().unwrap();
                match codes.iter().position(|c| *c == code) {
                    Some(at) => {
                        codes.remove(at);
                        Ok(json!({"token": "gat_issued", "expires_at": Timestamp::now()}))
                    }
                    None => Err(RpcError::permission_denied(
                        "that sign-in code isn't one this host gave, or it was used",
                    )),
                }
            }
            _ if who.is_none() => Err(RpcError::permission_denied(
                "say who you act for first (session.as)",
            )),
            m::TOKEN_CODE => {
                let code = format!("gac_for-{}", who.unwrap());
                self.codes.lock().unwrap().push(code.clone());
                Ok(json!({"code": code, "expires_at": Timestamp::now()}))
            }
            m::COMPUTER_LIST => {
                let who = who.unwrap();
                Ok(json!([ComputerView {
                    id: glass_api::PERSON_COMPUTER.into(),
                    name: format!("{who}'s computer"),
                    role: String::new(),
                    agent: "desktop-agent".parse().unwrap(),
                    state: ComputerState::Ready,
                    kind: "Firecracker microVM".into(),
                    contained: true,
                    disk_bytes: 0,
                    live_tasks: Vec::new(),
                    taken_over: false,
                    stored: None,
                    created_at: Timestamp::now(),
                    personas: vec![glass_api::PersonaView {
                        id: format!("{who}-helper"),
                        name: format!("{who}'s helper"),
                        role: String::new(),
                        agent: "desktop-agent".parse().unwrap(),
                        created_at: Timestamp::now(),
                    }],
                }]))
            }
            m::TASK_LIST => Ok(json!([])),
            other => Err(RpcError::method_not_found(other)),
        }
    }

    fn disconnected(&self, conn: &Connection) {
        self.sessions.lock().unwrap().remove(&conn.id());
    }
}

fn get(path: &str, headers: &[(&str, &str)]) -> glass_http::Request {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    glass_http::Request {
        method: "GET".into(),
        path: path.into(),
        query: query.into(),
        headers: headers
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect(),
        body: Vec::new(),
    }
}

fn json_of(response: &glass_http::Response) -> Value {
    serde_json::from_slice(&response.body).unwrap()
}

#[tokio::test]
async fn adr0067_behind_the_load_balancer_each_request_is_its_own_persons() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("control.sock");
    let glassd = Arc::new(PeopleGlassd::default());
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    tokio::spawn(glass_rpc::serve(listener, Arc::clone(&glassd)));
    let front = Front::new(
        Glassd::new(socket),
        crate::server::Mode::LoadBalancer,
        dir.path().join("web"),
        "never-used".into(),
        glass_agents_api::Person {
            id: "front".into(),
            name: "The front".into(),
            email: String::new(),
        },
        "Acme Corp".into(),
        None,
    );

    // Each person sees only their own agents.
    let dana = front
        .handle(&get(
            "/api/v1/agents",
            &[("x-amzn-oidc-data", "signed-for-dana")],
        ))
        .await;
    assert_eq!(dana.status, 200);
    assert_eq!(json_of(&dana)[0]["id"], "dana-helper");
    let sam = front
        .handle(&get(
            "/api/v1/agents",
            &[("x-amzn-oidc-data", "signed-for-sam")],
        ))
        .await;
    assert_eq!(json_of(&sam)[0]["id"], "sam-helper");
    let session = front
        .handle(&get(
            "/api/v1/session",
            &[("x-amzn-oidc-data", "signed-for-sam")],
        ))
        .await;
    assert_eq!(json_of(&session)["person"]["name"], "sam");
    // Sam's token is Sam's; a forged assertion, a made-up token or none
    // gets nothing, and neither does the front's own token.
    let token = front
        .handle(&get(
            "/api/v1/agents",
            &[("authorization", "Bearer gat_sams-token")],
        ))
        .await;
    assert_eq!(json_of(&token)[0]["id"], "sam-helper");
    for headers in [
        vec![("x-amzn-oidc-data", "forged-for-dana")],
        vec![("authorization", "Bearer gat_made-up")],
        vec![("authorization", "Bearer never-used")],
        vec![("cookie", "glass_agents=never-used")],
        vec![],
    ] {
        let refused = front.handle(&get("/api/v1/agents", &headers)).await;
        assert_eq!(refused.status, 401, "{headers:?}");
    }
    // The page too: nothing for no one.
    assert_eq!(front.handle(&get("/", &[])).await.status, 401);
    // Teams isn't tied to people yet: refused, not served as the front.
    let teams = front
        .handle(&glass_http::Request {
            method: "POST".into(),
            ..get("/api/teams/messages", &[])
        })
        .await;
    assert_eq!(teams.status, 503);

    // glass login: the page asks, the POST sends a one-time code to glass
    // on the person's own computer, and the code buys a token once.
    let ask = front
        .handle(&get(
            "/login/cli?port=53111&state=0123456789abcdef&host=dana-laptop",
            &[("x-amzn-oidc-data", "signed-for-dana")],
        ))
        .await;
    assert_eq!(ask.status, 200);
    let page = String::from_utf8_lossy(&ask.body);
    assert!(
        page.contains("Let glass on dana-laptop use your agents?"),
        "{page}"
    );
    assert!(
        ask.headers
            .iter()
            .any(|(k, v)| *k == "Content-Security-Policy" && v.contains("http://127.0.0.1:53111"))
    );
    // A browser posts the form with its origin, not "null" (which every
    // response's no-referrer would give it, and check_origin refuses).
    assert!(
        page.contains(r#"<meta name="referrer" content="same-origin">"#),
        "{page}"
    );
    // As a browser posts it: from the front's own origin.
    let allow = front
        .handle(&glass_http::Request {
            method: "POST".into(),
            body: b"port=53111&state=0123456789abcdef&host=dana-laptop".to_vec(),
            ..get(
                "/login/cli",
                &[
                    ("x-amzn-oidc-data", "signed-for-dana"),
                    ("origin", "http://agents.acme.example"),
                    ("host", "agents.acme.example"),
                ],
            )
        })
        .await;
    assert_eq!(allow.status, 303);
    let location = allow
        .headers
        .iter()
        .find(|(k, _)| *k == "Location")
        .map(|(_, v)| v.clone())
        .unwrap();
    assert_eq!(
        location,
        "http://127.0.0.1:53111/callback?code=gac_for-dana&state=0123456789abcdef"
    );
    let exchange = |code: &str| glass_http::Request {
        method: "POST".into(),
        ..get(
            "/api/v1/tokens",
            &[("authorization", &format!("Bearer {code}"))],
        )
    };
    let token = front.handle(&exchange("gac_for-dana")).await;
    assert_eq!(json_of(&token)["token"], "gat_issued");
    assert_eq!(
        front.handle(&exchange("gac_for-dana")).await.status,
        401,
        "once"
    );
    assert_eq!(front.handle(&exchange("gat_sams-token")).await.status, 401);
    // Login's own checks: a port below 1024, or no state, is refused.
    for bad in [
        "port=80&state=0123456789abcdef",
        "port=53111&state=short",
        "port=53111",
    ] {
        let refused = front
            .handle(&get(
                &format!("/login/cli?{bad}"),
                &[("x-amzn-oidc-data", "signed-for-dana")],
            ))
            .await;
        assert_eq!(refused.status, 400, "{bad}");
    }
    // Not signed in, no page.
    let anonymous = front
        .handle(&get("/login/cli?port=53111&state=0123456789abcdef", &[]))
        .await;
    assert_eq!(anonymous.status, 401);
}
