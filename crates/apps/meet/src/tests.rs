//! Meetings between computers through an in-memory console (CF-21): chat's
//! store holds the meeting, the console's meeting routes the session.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use glass_app_chat::{ChatHandle, ChatProvider};
use glass_crypto::SecretKey;
use glass_managed::chat::testing::MemoryLink;
use glass_managed::chat::{COMPLIANCE, LinkError, new_conversation_id};
use glass_managed::directory::Applied;
use glass_managed::meet::testing::MemoryMeet;
use glass_managed::meet::{Joined, MediaKeys, MeetingDetails};
use glass_managed::{ChatRelease, DirectoryRelease, Person};

use super::*;

struct Org {
    dir: tempfile::TempDir,
    chat: Arc<MemoryLink>,
    meet: Arc<MemoryMeet>,
    it: SecretKey,
    directory: DirectoryHandle,
}

struct Computer {
    dir: PathBuf,
    chat: ChatHandle,
    roster: ChatProvider,
    meet: MeetProvider,
    /// What this computer's people asked their agent in meetings.
    asked: Arc<Asked>,
}

/// Records each "Hey Glass" this computer would start an agent on.
#[derive(Debug, Default)]
struct Asked(std::sync::Mutex<Vec<(String, String, String)>>);

impl Asked {
    fn all(&self) -> Vec<(String, String, String)> {
        self.0.lock().unwrap().clone()
    }
}

impl AskAgent for Asked {
    fn ask(
        &self,
        login: &str,
        _uid: u32,
        meeting: &str,
        _title: &str,
        request: &str,
    ) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .push((login.into(), meeting.into(), request.into()));
        Ok(())
    }
}

fn org() -> Org {
    let it = SecretKey::generate().unwrap();
    let directory = DirectoryHandle::new();
    let person = |login: &str, name: &str| Person {
        login: login.into(),
        name: name.into(),
        email: format!("{login}@acme.example"),
        title: String::new(),
        department: String::new(),
    };
    directory.set(Applied {
        version: 1,
        release: DirectoryRelease {
            people: vec![
                person("dana", "Dana Ruiz"),
                person("priya", "Priya Shah"),
                person("sam", "Sam Lee"),
            ],
        },
    });
    let chat = Arc::new(MemoryLink::new(&it.public().unwrap()));
    Org {
        dir: tempfile::tempdir().unwrap(),
        meet: Arc::new(MemoryMeet::new(chat.clone())),
        chat,
        it,
        directory,
    }
}

impl Org {
    /// A managed computer in this org, with IT's Chat release applied.
    fn computer(&self, name: &str) -> Computer {
        let dir = self.dir.path().join(name);
        let chat = ChatHandle::new(&dir);
        chat.connect("acme", name, self.chat.clone());
        let release = ChatRelease::new(&self.it.public().unwrap());
        chat.set_compliance(&release.compliance, &release.fingerprint)
            .unwrap();
        let roster = ChatProvider::new(chat.clone(), self.directory.clone());
        let handle = MeetHandle::new();
        handle.connect(self.meet.clone());
        let asked = Arc::new(Asked::default());
        handle.agents(asked.clone());
        Computer {
            meet: MeetProvider::new(Arc::new(roster.clone()), handle, self.directory.clone())
                .shared(Arc::new(roster.clone())),
            dir,
            chat,
            roster,
            asked,
        }
    }

    /// Dana's meeting with Priya, invited from Dana's computer: Priya has
    /// opened Calendar once, and her computer has synced.
    fn meeting(&self) -> (Computer, Computer, String) {
        self.meeting_with(details())
    }

    /// The same, where IT turned transcripts on for Dana (ADR 0045).
    fn transcribed_meeting(&self) -> (Computer, Computer, String) {
        self.meeting_with(MeetingDetails {
            transcribed: true,
            ..details()
        })
    }

    fn meeting_with(&self, details: MeetingDetails) -> (Computer, Computer, String) {
        let (a, b) = (self.computer("dev-a"), self.computer("dev-b"));
        b.call("priya", LIST, json!({})).unwrap();
        let id = new_conversation_id().unwrap();
        let meetings: &dyn Meetings = &a.roster;
        meetings
            .create("dana", true, &id, &["priya".into()], &details)
            .unwrap();
        b.chat.sync_once(0).unwrap();
        (a, b, id)
    }
}

fn said(text: &str) -> Value {
    json!({ "at": "2026-10-22T14:05:00Z", "text": text })
}

fn details() -> MeetingDetails {
    MeetingDetails {
        title: "Northwind renewal".into(),
        start: "2026-10-20T14:00".into(),
        end: "2026-10-20T15:00".into(),
        transcribed: false,
    }
}

fn ctx(login: &str, app: &str) -> TaskContext {
    let mut ctx = glass_actions::testing::context(Path::new("/nonexistent"));
    ctx.principal.on_behalf_of.name = login.into();
    ctx.principal.agent = app.parse().unwrap();
    ctx
}

impl Computer {
    /// `login`, in Meet.
    fn call(&self, login: &str, action: &str, input: Value) -> Result<ActionOutput, ActionError> {
        self.meet
            .invoke(&ctx(login, "meet"), &action.parse().unwrap(), &input)
    }

    fn join(&self, login: &str, id: &str) -> ActionOutput {
        let offer = format!("v=0\r\ns=offer from {login}\r\n");
        self.call(login, JOIN, json!({ "meeting": id, "offer": offer }))
            .unwrap()
    }
}

/// Every file under `dir`, read whole.
fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files(&path));
        } else {
            out.push((path.clone(), std::fs::read(&path).unwrap()));
        }
    }
    out
}

#[test]
fn cf21_two_people_join_one_session_with_one_media_key_and_leave() {
    let org = org();
    let (a, b, id) = org.meeting();

    let list = a.call("dana", LIST, json!({})).unwrap();
    let row = &list.data["meetings"][0];
    assert_eq!(row["id"], id.as_str());
    assert_eq!(row["title"], "Northwind renewal");
    assert_eq!(row["start"], "2026-10-20T14:00");
    assert_eq!(row["organizer_name"], "Dana Ruiz");
    assert_eq!(row["people"].as_array().unwrap().len(), 2);
    let theirs = b.call("priya", LIST, json!({})).unwrap();
    assert_eq!(theirs.data["meetings"][0]["id"], id.as_str());

    // Dana is first: her computer makes the session's key.
    let preview = a
        .meet
        .preview(
            &ctx("dana", "meet"),
            &JOIN.parse().unwrap(),
            &json!({ "meeting": id, "offer": "v=0" }),
        )
        .unwrap();
    assert_eq!(preview.summary, "Join Northwind renewal");
    let dana = a.join("dana", &id);
    assert_eq!(dana.text, "Joined Northwind renewal as participant 1.");
    assert!(dana.data["answer"].as_str().unwrap().starts_with("v=0"));
    let key = dana.data["media_key"].as_str().unwrap().to_owned();
    assert_eq!(key.len(), 44, "32 bytes in base64");
    assert!(!dana.text.contains(&key));

    // Priya gets the same key, sealed to her computer.
    let priya = b.join("priya", &id);
    assert_eq!(priya.data["participant"], 2);
    assert_eq!(priya.data["media_key"], key.as_str());
    assert_eq!(priya.data["media_key_id"], dana.data["media_key_id"]);
    assert_eq!(org.meet.offers().len(), 2);
    assert!(org.meet.offers()[1].contains("offer from priya"));
    let keys = b.call("priya", KEYS, json!({ "meeting": id })).unwrap();
    assert_eq!(keys.data["media_keys"][0]["key"], key.as_str());
    assert_eq!(keys.text, "1 media key for Northwind renewal.");

    // Joining again keeps the seat.
    assert_eq!(a.join("dana", &id).data["participant"], 1);

    let left = a.call("dana", LEAVE, json!({ "meeting": id })).unwrap();
    assert_eq!(left.text, "Left Northwind renewal.");
    assert_eq!(org.meet.participants(&id), [("priya".to_owned(), 2)]);
    b.call("priya", LEAVE, json!({ "meeting": id })).unwrap();
    // The last one out ends the session, and the console forgets its key.
    assert!(org.meet.media(&id).is_empty());
    let next = b.join("priya", &id);
    assert_ne!(
        next.data["media_key"],
        key.as_str(),
        "a new session, a new key"
    );
}

/// Dana's meeting with Priya and Sam, everyone's computer synced.
fn three(org: &Org) -> (Computer, Computer, Computer, String) {
    let (a, b, c) = (
        org.computer("dev-a"),
        org.computer("dev-b"),
        org.computer("dev-c"),
    );
    b.call("priya", LIST, json!({})).unwrap();
    c.call("sam", LIST, json!({})).unwrap();
    let id = new_conversation_id().unwrap();
    let meetings: &dyn Meetings = &a.roster;
    meetings
        .create(
            "dana",
            true,
            &id,
            &["priya".into(), "sam".into()],
            &details(),
        )
        .unwrap();
    b.chat.sync_once(0).unwrap();
    c.chat.sync_once(0).unwrap();
    (a, b, c, id)
}

#[test]
fn cf21_someone_who_left_cannot_decrypt_after_the_new_key() {
    let org = org();
    let (a, b, c, id) = three(&org);
    a.join("dana", &id);
    b.join("priya", &id);
    let sam = c.join("sam", &id);
    let held = sam.data["media_key"].as_str().unwrap().to_owned();
    c.call("sam", LEAVE, json!({ "meeting": id })).unwrap();

    // The first computer to ask makes the new key; the other gets it.
    let priya = b.call("priya", KEYS, json!({ "meeting": id })).unwrap();
    let new = priya.data["media_keys"][0]["key"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(new, held);
    assert_eq!(priya.data["due"], false);
    assert_eq!(
        priya.data["media_keys"][1]["key"],
        held.as_str(),
        "kept for frames in flight"
    );
    let dana = a.call("dana", KEYS, json!({ "meeting": id })).unwrap();
    assert_eq!(dana.data["media_keys"][0]["key"], new.as_str());
    // Sam's computer can't open it: he is in the meeting, not the session.
    let sams_key = Meetings::member(&c.roster, "sam", false)
        .unwrap()
        .unwrap()
        .0;
    assert!(
        org.meet.media(&id)[0]
            .body
            .envelopes
            .iter()
            .all(|e| e.key != sams_key.id),
        "not sealed to the one who left"
    );

    // He comes back: his computer can't open the session's key, so it
    // brings a new one for everyone present, and they switch to it. The
    // same happens for a computer whose key was registered after the
    // session's key was made.
    let back = c.join("sam", &id);
    let theirs = back.data["media_key"].as_str().unwrap().to_owned();
    assert_ne!(theirs, new);
    for (computer, login) in [(&a, "dana"), (&b, "priya")] {
        let keys = computer
            .call(login, KEYS, json!({ "meeting": id }))
            .unwrap();
        assert_eq!(keys.data["media_keys"][0]["key"], theirs.as_str());
        assert_eq!(keys.data["due"], false);
    }
}

#[test]
fn cf21_the_media_key_is_never_sealed_to_the_compliance_key() {
    let org = org();
    let (a, b, id) = org.meeting();
    let key = a.join("dana", &id).data["media_key"].clone();
    b.join("priya", &id);
    let records = org.meet.media(&id);
    assert_eq!(records.len(), 1);
    let record = &records[0];
    // Sealed to Dana's and Priya's member keys, and nothing else.
    let dana = Meetings::member(&a.roster, "dana", false)
        .unwrap()
        .unwrap()
        .0;
    let priya = Meetings::member(&b.roster, "priya", false)
        .unwrap()
        .unwrap()
        .0;
    let mut sealed_to: Vec<&str> = record
        .body
        .envelopes
        .iter()
        .map(|e| e.key.as_str())
        .collect();
    sealed_to.sort();
    let mut people = vec![dana.id.as_str(), priya.id.as_str()];
    people.sort();
    assert_eq!(sealed_to, people);
    assert!(record.body.envelopes.iter().all(|e| e.key != COMPLIANCE));
    for envelope in &record.body.envelopes {
        assert!(
            record.open(&envelope.key, &org.it).is_err(),
            "IT's key opens none"
        );
    }
    // The meeting's title, unlike its media, is IT's to read.
    let (_, epochs) = org
        .chat
        .conversations()
        .into_iter()
        .find(|(c, _)| c.id == id)
        .unwrap();
    assert!(epochs[0].open(COMPLIANCE, &org.it).is_ok());
    assert_eq!(key.as_str().map(str::len), Some(44));
}

#[test]
fn cf21_someone_not_in_the_meeting_is_refused() {
    let org = org();
    let (_a, _b, id) = org.meeting();
    let c = org.computer("dev-c");
    c.call("sam", LIST, json!({})).unwrap();
    let refused = c
        .call("sam", JOIN, json!({ "meeting": id, "offer": "v=0" }))
        .unwrap_err();
    assert!(
        matches!(&refused, ActionError::NotFound(m) if m.contains("you aren't in a meeting")),
        "{refused}"
    );
    // The console refuses him too, signed request or not.
    let (key, secret) = Meetings::member(&c.roster, "sam", false).unwrap().unwrap();
    let request = MeetProvider::request(
        &key,
        &secret,
        &id,
        MeetOp::Join {
            offer: "v=0".into(),
            media: None,
        },
        "Not joined",
    )
    .unwrap();
    assert_eq!(
        org.meet.join(&request).unwrap_err(),
        LinkError::Refused("You aren't in this meeting.".into())
    );
    assert!(org.meet.offers().is_empty(), "no offer reached a relay");
}

#[test]
fn cf21_the_media_key_stays_off_the_computers_disk() {
    let org = org();
    let (a, b, id) = org.meeting();
    let key = a.join("dana", &id).data["media_key"]
        .as_str()
        .unwrap()
        .to_owned();
    b.join("priya", &id);
    // The key as the page got it, and opened here from the console's copy.
    let (dana, secret) = Meetings::member(&a.roster, "dana", false).unwrap().unwrap();
    let media = org.meet.media(&id)[0]
        .open(&dana.id, &secret.agree)
        .unwrap();
    assert_eq!(media.to_base64(), key);
    let raw = media.as_bytes().to_vec();
    let hex = hex::encode(&raw);
    for computer in [&a, &b] {
        for (path, bytes) in files(&computer.dir) {
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains(&key) && !text.contains(&hex),
                "{} holds the key",
                path.display()
            );
            assert!(
                !bytes.windows(raw.len()).any(|w| w == raw.as_slice()),
                "{} holds the key's bytes",
                path.display()
            );
        }
    }
}

/// A console that answers with a media key of its choosing.
#[derive(Debug)]
struct Swapping(MediaKeyRecord, MemberKey);

impl MeetLink for Swapping {
    fn join(&self, _: &MeetRequest) -> Result<Joined, LinkError> {
        Ok(Joined {
            answer: "v=0\r\n".into(),
            participant: 1,
            media: vec![self.0.clone()],
            keys: vec![self.1.clone()],
        })
    }
    fn keys(&self, _: &MeetRequest) -> Result<MediaKeys, LinkError> {
        Ok(MediaKeys {
            media: vec![self.0.clone()],
            keys: vec![self.1.clone()],
            ..MediaKeys::default()
        })
    }
    fn rekey(&self, request: &MeetRequest) -> Result<MediaKeys, LinkError> {
        self.keys(request)
    }
    fn leave(&self, _: &MeetRequest) -> Result<(), LinkError> {
        Ok(())
    }
}

#[test]
fn cf21_a_media_key_made_outside_the_meeting_is_refused() {
    let org = org();
    let (a, _b, id) = org.meeting();
    let dana = Meetings::member(&a.roster, "dana", false)
        .unwrap()
        .unwrap()
        .0;
    // Sam's key isn't in the meeting: a console holding it could seal a
    // key of its own to everyone in it.
    let c = org.computer("dev-c");
    c.call("sam", LIST, json!({})).unwrap();
    let (sam, sam_secret) = Meetings::member(&c.roster, "sam", false).unwrap().unwrap();
    let forged = MediaKeyRecord::make(
        &id,
        &MediaKey::generate().unwrap(),
        &[dana],
        (&sam, &sam_secret),
    )
    .unwrap();
    let handle = MeetHandle::new();
    handle.connect(Arc::new(Swapping(forged, sam)));
    let meet = MeetProvider::new(Arc::new(a.roster.clone()), handle, org.directory.clone());
    let refused = meet
        .invoke(
            &ctx("dana", "meet"),
            &JOIN.parse().unwrap(),
            &json!({ "meeting": id, "offer": "v=0" }),
        )
        .unwrap_err()
        .to_string();
    assert!(refused.starts_with("Not joined:"), "{refused}");
    assert!(
        refused.contains("isn't signed by anyone in the meeting"),
        "{refused}"
    );
}

#[test]
fn cf21_without_a_console_there_are_no_meetings_and_calendar_still_reads() {
    let org = org();
    let unmanaged = ChatProvider::new(
        ChatHandle::new(org.dir.path().join("x")),
        org.directory.clone(),
    );
    let meet = MeetProvider::new(
        Arc::new(unmanaged),
        MeetHandle::new(),
        org.directory.clone(),
    );
    let list = meet
        .invoke(&ctx("dana", "calendar"), &LIST.parse().unwrap(), &json!({}))
        .unwrap();
    assert_eq!(list.data["available"], false);
    assert!(
        list.text.contains("isn't managed by an organization"),
        "{}",
        list.text
    );
    let refused = meet
        .invoke(
            &ctx("dana", "meet"),
            &JOIN.parse().unwrap(),
            &json!({ "meeting": new_conversation_id().unwrap(), "offer": "v=0" }),
        )
        .unwrap_err()
        .to_string();
    assert!(
        refused.starts_with("Not joined: this computer can't reach meetings"),
        "{refused}"
    );
    // A bad id or an empty offer is refused before anything.
    for input in [
        json!({ "meeting": "cv-../../etc", "offer": "v=0" }),
        json!({ "meeting": new_conversation_id().unwrap(), "offer": " " }),
        json!({ "meeting": new_conversation_id().unwrap(), "offer": "v=0", "key": "x" }),
    ] {
        assert!(matches!(
            meet.preview(&ctx("dana", "meet"), &JOIN.parse().unwrap(), &input),
            Err(ActionError::InvalidInput(_))
        ));
    }
}

#[test]
fn adr0045_each_computer_adds_its_own_persons_words_and_everyone_reads_all_of_them() {
    let org = org();
    let (a, b, id) = org.transcribed_meeting();
    let listed = a.call("dana", LIST, json!({})).unwrap();
    assert_eq!(listed.data["meetings"][0]["transcribed"], true);

    // What the preview (and so the audit log) says: counts, never words.
    let input = json!({ "meeting": id, "lines": [said("Let's ship on the 14th."), said("Ana books the room.")] });
    let preview = a
        .meet
        .preview(&ctx("dana", "meet"), &TRANSCRIBE.parse().unwrap(), &input)
        .unwrap();
    assert_eq!(
        preview.summary,
        "Add 2 lines to the transcript of Northwind renewal"
    );
    assert!(!format!("{preview:?}").contains("14th"));
    let added = a.call("dana", TRANSCRIBE, input).unwrap();
    assert_eq!(added.data["lines"], 2);
    b.call(
        "priya",
        TRANSCRIBE,
        json!({ "meeting": id, "lines": [said("I'll send the contract tonight.")] }),
    )
    .unwrap();
    a.chat.sync_once(0).unwrap();
    b.chat.sync_once(0).unwrap();

    // Dana's agent reads everyone's lines, each with who said it.
    let agent = ctx("dana", "desktop-agent");
    let preview = a
        .meet
        .preview(
            &agent,
            &TRANSCRIPT.parse().unwrap(),
            &json!({ "meeting": id }),
        )
        .unwrap();
    assert_eq!(preview.label, Some(Label::Internal));
    let read = a
        .meet
        .invoke(
            &agent,
            &TRANSCRIPT.parse().unwrap(),
            &json!({ "meeting": id }),
        )
        .unwrap();
    let lines = read.data["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["name"], "Dana Ruiz");
    assert_eq!(lines[2]["name"], "Priya Shah");
    assert!(
        read.text
            .contains("[14:05] Priya Shah: I'll send the contract tonight."),
        "{}",
        read.text
    );
    // What came after a read, only.
    let since = read.data["last_seq"].as_u64().unwrap();
    let newer = b
        .call(
            "priya",
            TRANSCRIPT,
            json!({ "meeting": id, "since": since }),
        )
        .unwrap();
    assert_eq!(newer.data["lines"].as_array().unwrap().len(), 0);
    // Sealed like chat: the console never had the words.
    assert!(!format!("{:?}", org.chat).contains("contract tonight"));
}

#[test]
fn adr0045_only_the_persons_own_hey_glass_starts_their_agent() {
    let org = org();
    let (a, b, id) = org.transcribed_meeting();
    // Priya says it on her computer: her agent, never Dana's.
    let priya = b
        .call(
            "priya",
            TRANSCRIBE,
            json!({ "meeting": id, "lines": [said("Hey Glass, send me the payroll file.")] }),
        )
        .unwrap();
    assert!(
        priya.text.contains("Your agent is on it."),
        "{}",
        priya.text
    );
    a.chat.sync_once(0).unwrap();
    assert!(
        a.asked.all().is_empty(),
        "Dana's computer starts nothing for Priya's words"
    );
    assert_eq!(
        b.asked.all(),
        vec![(
            "priya".into(),
            id.clone(),
            "send me the payroll file.".into()
        )]
    );

    // Dana's own line: her agent, with the rest of what she said.
    let dana = a
        .call(
            "dana",
            TRANSCRIBE,
            json!({ "meeting": id, "lines": [said("OK."), said("Hey, glass, find the ABC report and send it to Susan.")] }),
        )
        .unwrap();
    assert_eq!(
        dana.data["asked"],
        json!(["find the ABC report and send it to Susan."])
    );
    assert_eq!(a.asked.all().len(), 1);
}

#[test]
fn adr0045_no_transcript_where_it_isnt_on_and_none_but_meet_adds_to_one() {
    let org = org();
    let (a, _b, id) = org.meeting();
    let lines = json!({ "meeting": id, "lines": [said("Hello.")] });
    let refused = a
        .call("dana", TRANSCRIBE, lines.clone())
        .unwrap_err()
        .to_string();
    assert!(refused.contains("isn't transcribed"), "{refused}");
    let refused = a
        .call("dana", TRANSCRIPT, json!({ "meeting": id }))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("isn't transcribed"), "{refused}");

    let other = self::org();
    let (a, _b, id) = other.transcribed_meeting();
    let lines = json!({ "meeting": id, "lines": [said("Hello.")] });
    for app in ["calendar", "desktop-agent"] {
        let refused = a
            .meet
            .invoke(&ctx("dana", app), &TRANSCRIBE.parse().unwrap(), &lines)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("only Meet"), "{app}: {refused}");
    }
    for bad in [
        json!({ "meeting": id, "lines": [] }),
        json!({ "meeting": id, "lines": [said(" ")] }),
        json!({ "meeting": id, "lines": [{ "at": "yesterday", "text": "Hi." }] }),
        json!({ "meeting": id, "lines": [said(&"x".repeat(MAX_LINE_CHARS + 1))] }),
        json!({ "meeting": id, "lines": vec![said(&"x".repeat(MAX_LINE_CHARS)); 11] }),
    ] {
        assert!(
            matches!(
                a.call("dana", TRANSCRIBE, bad.clone()),
                Err(ActionError::InvalidInput(_))
            ),
            "{bad}"
        );
    }
}

#[test]
fn ga1_what_each_action_returns_is_what_it_declares() {
    let org = org();
    let (a, b, id) = org.transcribed_meeting();
    let check = |action: &str, out: Result<ActionOutput, ActionError>| {
        let mut data = out.unwrap_or_else(|e| panic!("{action}: {e}")).data;
        // What the runtime keeps for the record never reaches the caller.
        if let Some(fields) = data.as_object_mut() {
            fields.remove(glass_actions::AUDIT_DETAIL);
        }
        let shape = a.meet.returns(&action.parse().unwrap()).unwrap();
        glass_actions::shape::conforms(&data, &shape).unwrap_or_else(|e| panic!("{action}: {e}"));
        data
    };
    let listed = check(LIST, a.call("dana", LIST, json!({})));
    assert_eq!(listed["meetings"].as_array().unwrap().len(), 1);
    // Where meetings can't be reached, the list says why.
    let unmanaged = MeetProvider::new(
        Arc::new(ChatProvider::new(
            ChatHandle::new(org.dir.path().join("x")),
            org.directory.clone(),
        )),
        MeetHandle::new(),
        org.directory.clone(),
    );
    let none = check(
        LIST,
        unmanaged.invoke(&ctx("dana", "calendar"), &LIST.parse().unwrap(), &json!({})),
    );
    assert_eq!(none["available"], false);

    check(
        JOIN,
        a.call(
            "dana",
            JOIN,
            json!({ "meeting": id, "offer": "v=0\r\ns=offer from dana\r\n" }),
        ),
    );
    check(
        JOIN,
        b.call(
            "priya",
            JOIN,
            json!({ "meeting": id, "offer": "v=0\r\ns=offer from priya\r\n" }),
        ),
    );
    let keys = check(KEYS, b.call("priya", KEYS, json!({ "meeting": id })));
    assert_eq!(keys["media_keys"].as_array().unwrap().len(), 1);
    let added = check(
        TRANSCRIBE,
        a.call(
            "dana",
            TRANSCRIBE,
            json!({ "meeting": id, "lines": [said("Let's ship on the 14th."), said("Hey Glass, book the room.")] }),
        ),
    );
    assert_eq!(added["asked"], json!(["book the room."]));
    let read = check(
        TRANSCRIPT,
        a.meet.invoke(
            &ctx("dana", "desktop-agent"),
            &TRANSCRIPT.parse().unwrap(),
            &json!({ "meeting": id }),
        ),
    );
    assert_eq!(read["lines"].as_array().unwrap().len(), 2);
    check(LEAVE, a.call("dana", LEAVE, json!({ "meeting": id })));
    // Every action the provider declares has a shape.
    for def in a.meet.actions() {
        assert!(a.meet.returns(&def.name).is_some(), "{}", def.name);
    }
}
