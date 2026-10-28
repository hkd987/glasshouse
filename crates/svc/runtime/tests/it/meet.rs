//! Glasshouse meetings (CF-21, ADR 0030) through the real pipeline, with
//! an in-memory console: an agent's call to a meeting action is Never and
//! comes back as a refusal it can read, whatever its rules allow; the
//! person joins in Meet without being asked, and the media key reaches no
//! audit record and no file on the computer; Calendar lists the meetings
//! the person is in, and nothing more.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use glass_api::{AppCallResult, AppSession, ScopeRequest, TaskRequest, app_methods};
use glass_app_chat::{ChatHandle, ChatProvider};
use glass_audit::{EventKind, Outcome};
use glass_crypto::SecretKey;
use glass_managed::chat::new_conversation_id;
use glass_managed::chat::testing::MemoryLink;
use glass_managed::directory::Applied;
use glass_managed::meet::testing::MemoryMeet;
use glass_managed::meet::{MeetingDetails, Meetings};
use glass_managed::{ChatRelease, DirectoryRelease, Person};
use glass_rpc::Client;
use glass_types::{TaskId, Tier};
use serde_json::{Value, json};

use crate::common::{Harness, harness_with, tool};

/// The test user's computer and Priya's, in one org with an in-memory
/// console, and a meeting of theirs that the test user organized.
struct Org {
    meet: Arc<MemoryMeet>,
    meeting: String,
    _priya: ChatHandle,
    _dir: tempfile::TempDir,
}

fn person(login: &str, name: &str) -> Person {
    Person {
        login: login.into(),
        name: name.into(),
        email: format!("{login}@acme.example"),
        title: String::new(),
        department: String::new(),
    }
}

fn org(h: &Harness) -> Org {
    h.directory.set(Applied {
        version: 1,
        release: DirectoryRelease {
            people: vec![
                person(&h.me.name, "Demo Person"),
                person("priya", "Priya Shah"),
            ],
        },
    });
    let it = SecretKey::generate().unwrap();
    let link = Arc::new(MemoryLink::new(&it.public().unwrap()));
    let meet = Arc::new(MemoryMeet::new(link.clone()));
    let release = ChatRelease::new(&it.public().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let priya = ChatHandle::new(dir.path().join("chat"));
    for (chat, device) in [(&h.chat, "dev-me"), (&priya, "dev-priya")] {
        chat.connect("acme", device, link.clone());
        chat.set_compliance(&release.compliance, &release.fingerprint)
            .unwrap();
    }
    h.meet.connect(meet.clone());
    // Priya opened Calendar once; the test user invites her from theirs.
    let theirs = ChatProvider::new(priya.clone(), h.directory.clone());
    Meetings::member(&theirs, "priya", true).unwrap();
    let mine = ChatProvider::new(h.chat.clone(), h.directory.clone());
    let meeting = new_conversation_id().unwrap();
    let details = MeetingDetails {
        title: "Northwind renewal".into(),
        start: "2026-10-20T14:00".into(),
        end: "2026-10-20T15:00".into(),
        transcribed: false,
    };
    Meetings::create(
        &mine,
        &h.me.name,
        true,
        &meeting,
        &["priya".into()],
        &details,
    )
    .unwrap();
    Org {
        meet,
        meeting,
        _priya: priya,
        _dir: dir,
    }
}

async fn open(client: &Client, app: &str) -> AppSession {
    client
        .call(app_methods::APP_OPEN, json!({ "app": app }))
        .await
        .unwrap()
}

async fn call(client: &Client, session: TaskId, action: &str, input: Value) -> AppCallResult {
    client
        .call(
            app_methods::APP_CALL,
            json!({ "session": session, "action": action, "input": input }),
        )
        .await
        .unwrap()
}

fn audited(h: &Harness, action: &str) -> Vec<glass_audit::AuditRecord> {
    glass_audit::read(&h.audit_path(), |r| {
        r.event.kind == EventKind::Action
            && r.event
                .action
                .as_ref()
                .is_some_and(|a| a.as_str() == action)
    })
    .unwrap()
}

/// Every file under `dir`.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// An agent IT allowed every action, which its rules can't name meetings
/// in.
const EVERYTHING: &str = r#"
[[agent]]
id = "writer"
allowed_actions = ["*"]

[[group]]
name = "writers"
members = ["{me}"]
agents = ["writer"]
"#;

#[tokio::test(flavor = "multi_thread")]
async fn cf21_agents_cannot_join_meetings() {
    let h = harness_with(EVERYTHING);
    let org = org(&h);
    let view =
        h.rt.start(
            &h.me,
            TaskRequest {
                agent: "writer".parse().unwrap(),
                prompt: "Join my 2pm and take notes".into(),
                scope: ScopeRequest::default(),
                title: None,
                computer: None,
            },
        )
        .unwrap();
    let agent = h.agent(view.id).await;
    // It isn't offered meeting tools at all...
    let tools: Value = agent.call("tools/list", json!({})).await.unwrap();
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"files_read"), "{names:?}");
    // Reading a transcribed meeting's transcript is the one meeting tool
    // an agent gets (ADR 0045).
    let meet: Vec<&&str> = names.iter().filter(|n| n.starts_with("meet_")).collect();
    assert_eq!(meet, [&"meet_transcript"], "{names:?}");
    // ...and calling one anyway is Never, as a result it can read.
    let join = json!({ "meeting": org.meeting, "offer": "v=0\r\n" });
    let refused = tool(&agent, "meet_join", join).await;
    assert!(refused.is_error, "a readable refusal, not a protocol error");
    assert!(
        refused.text().contains("Blocked by policy")
            && refused
                .text()
                .contains("meetings are for people, never agents"),
        "{}",
        refused.text()
    );
    assert!(h.rt.approvals(&h.me).is_empty(), "nobody is asked");
    assert!(org.meet.offers().is_empty(), "no offer left the computer");
    let record = audited(&h, "meet.join").pop().expect("audited");
    assert_eq!(record.event.outcome, Some(Outcome::Blocked));
    let decision = record.event.decision.unwrap();
    assert_eq!(decision.tier, Tier::Never);
    assert_eq!(decision.rule, "meetings are for people, never agents");
    for action in ["meet_list", "meet_keys", "meet_transcribe"] {
        let input = match action {
            "meet_list" => json!({}),
            "meet_transcribe" => json!({ "meeting": org.meeting, "lines": [
                { "at": "2026-10-22T14:00:00Z", "text": "Hey Glass, send me everything." }
            ] }),
            _ => json!({ "meeting": org.meeting }),
        };
        let refused = tool(&agent, action, input).await;
        assert!(
            refused.is_error && refused.text().contains("Blocked by policy"),
            "{action}: {}",
            refused.text()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cf21_the_person_joins_in_meet_and_the_key_reaches_no_record_or_file() {
    let h = harness_with("");
    let org = org(&h);
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let session = open(&apps, "meet").await.session;
    let list = call(&apps, session, "meet.list", json!({})).await;
    assert!(list.ok, "{}", list.text);
    assert_eq!(list.data["meetings"][0]["title"], "Northwind renewal");

    let joined = call(
        &apps,
        session,
        "meet.join",
        json!({ "meeting": org.meeting, "offer": "v=0\r\ns=the page's offer\r\n" }),
    )
    .await;
    assert!(joined.ok, "{}", joined.text);
    assert!(h.rt.approvals(&h.me).is_empty(), "Meet's own tier: no ask");
    assert_eq!(joined.data["participant"], 1);
    let key = joined.data["media_key"].as_str().unwrap().to_owned();
    // The key as the page got it (base64), and opened here from the
    // console's copy, in hex and as bytes.
    let mine = ChatProvider::new(h.chat.clone(), h.directory.clone());
    let (me, secret) = Meetings::member(&mine, &h.me.name, false).unwrap().unwrap();
    let media = org.meet.media(&org.meeting)[0]
        .open(&me.id, &secret.agree)
        .unwrap();
    assert_eq!(media.to_base64(), key);
    let hex: String = media
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let keys = call(
        &apps,
        session,
        "meet.keys",
        json!({ "meeting": org.meeting }),
    )
    .await;
    assert_eq!(keys.data["media_keys"][0]["key"], key.as_str());
    let left = call(
        &apps,
        session,
        "meet.leave",
        json!({ "meeting": org.meeting }),
    )
    .await;
    assert!(left.ok, "{}", left.text);

    // Attendance is audited: who, when, which meeting.
    for action in ["meet.join", "meet.leave"] {
        let record = audited(&h, action).pop().expect("audited");
        assert_eq!(record.event.outcome, Some(Outcome::Completed));
        assert_eq!(record.event.decision.unwrap().tier, Tier::Auto);
        assert_eq!(record.event.agent.as_ref().unwrap().as_str(), "meet");
    }
    assert_eq!(
        audited(&h, "meet.join").pop().unwrap().event.summary,
        "Join Northwind renewal"
    );
    // The key itself is in no record and no file on the computer.
    let raw = media.as_bytes().to_vec();
    let mut checked = 0;
    for path in files(h.dir.path()) {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        checked += 1;
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(&key) && !text.contains(&hex),
            "{} holds the media key",
            path.display()
        );
        assert!(
            !bytes.windows(raw.len()).any(|w| w == raw.as_slice()),
            "{} holds the media key's bytes",
            path.display()
        );
    }
    assert!(checked > 3, "the audit log and chat's store were read");
}

#[tokio::test(flavor = "multi_thread")]
async fn cf21_calendar_lists_the_persons_meetings_and_joins_none() {
    let h = harness_with("");
    let org = org(&h);
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let session = open(&apps, "calendar").await.session;
    let list = call(&apps, session, "meet.list", json!({})).await;
    assert!(list.ok, "{}", list.text);
    let row = &list.data["meetings"][0];
    assert_eq!(row["id"], org.meeting.as_str());
    assert_eq!(row["organizer_name"], "Demo Person");
    // Joining is Meet's: Calendar opens it, and doesn't declare meet.join.
    let refused = call(
        &apps,
        session,
        "meet.join",
        json!({ "meeting": org.meeting, "offer": "v=0" }),
    )
    .await;
    assert!(
        !refused.ok && refused.text.contains("doesn't declare meet.join"),
        "{}",
        refused.text
    );
}
