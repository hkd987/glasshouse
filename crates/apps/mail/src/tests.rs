#![allow(clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;

use glass_actions::testing::context;
use glass_managed::mail::testing::MemoryLink;
use glass_managed::mail::{MailLink, Server};
use glass_policy::document::{MailSecurity, MailServer, MailService, MailSettings};

use super::*;

fn setup() -> (tempfile::TempDir, TaskContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = context(dir.path());
    let meta = fs::metadata(dir.path()).unwrap();
    ctx.owner = glass_platform::fs::Owner {
        uid: meta.uid(),
        gid: meta.gid(),
    };
    MailProvider::default().prepare(&ctx).unwrap();
    (dir, ctx)
}

fn action(name: &str) -> ActionName {
    name.parse().unwrap()
}

#[test]
fn pa3_mail_folders_are_never_made_through_a_symlink_out_of_home() {
    let outside = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join(MAIL_DIR)).unwrap();
    let ctx = context(dir.path());
    assert!(MailProvider::default().prepare(&ctx).is_err());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn batched_sends_are_one_preview_with_a_review_list() {
    let (_dir, ctx) = setup();
    let input = json!({ "messages": (0..6).map(|i| json!({
        "to": [format!("vendor{i}@example.com")], "subject": "Invoice reminder", "body": "Please pay."
    })).collect::<Vec<_>>() });
    let preview = MailProvider::default()
        .preview(&ctx, &action("mail.send"), &input)
        .unwrap();
    assert_eq!(preview.summary, "Send 6 emails");
    assert_eq!(preview.items.len(), 6);
    assert!(preview.snapshot_roots.is_empty(), "sends are not undoable");
}

#[test]
fn drafts_carry_attribution_headers() {
    let (dir, ctx) = setup();
    let input = json!({ "to": ["ap@example.com"], "subject": "Q3 summary", "body": "Attached." });
    let out = MailProvider::default()
        .invoke(&ctx, &action("mail.draft"), &input)
        .unwrap();
    assert!(out.changes[0].reversible);
    let drafts: Vec<_> = fs::read_dir(dir.path().join("Mail/Drafts"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(drafts.len(), 1);
    let text = fs::read_to_string(drafts[0].path()).unwrap();
    assert!(text.contains("X-Glasshouse-Attribution: by invoice-demo for demo"));
}

#[test]
fn same_subject_in_the_same_second_makes_two_drafts() {
    // Two tasks drafting at once must not overwrite each other.
    let (dir, ctx) = setup();
    let input = json!({ "to": ["ap@example.com"], "subject": "Same", "body": "x" });
    let draft = action("mail.draft");
    let first = MailProvider::default()
        .invoke(&ctx, &draft, &input)
        .unwrap();
    let second = MailProvider::default()
        .invoke(&ctx, &draft, &input)
        .unwrap();
    assert_ne!(first.changes[0].paths, second.changes[0].paths);
    let count = fs::read_dir(dir.path().join("Mail/Drafts"))
        .unwrap()
        .count();
    assert_eq!(count, 2);
}

#[test]
fn au10_listings_and_reads_say_who_wrote_each_message() {
    let (dir, ctx) = setup();
    fs::write(
        dir.path().join("Mail/Inbox/1792400000-0-northwind.eml"),
        "From: Dana Ruiz <dana@northwind.example>\nTo: demo@corp.example\n\
         Subject: Renewal terms\nDate: 2026-10-19T08:00:00Z\n\nHi,\n\nCan we talk\n terms?\n",
    )
    .unwrap();
    let input =
        json!({ "to": ["dana@northwind.example"], "subject": "Re: Renewal terms", "body": "Yes." });
    MailProvider::default()
        .invoke(&ctx, &action("mail.draft"), &input)
        .unwrap();

    let list = action("mail.list");
    let preview = MailProvider::default()
        .preview(&ctx, &list, &json!({ "folder": "inbox" }))
        .unwrap();
    assert_eq!(preview.reads, vec![dir.path().join("Mail/Inbox")]);
    assert!(preview.writes.is_empty());
    let inbox = MailProvider::default()
        .invoke(&ctx, &list, &json!({ "folder": "inbox" }))
        .unwrap();
    let message = &inbox.data["messages"][0];
    assert_eq!(message["subject"], "Renewal terms");
    assert_eq!(message["snippet"], "Hi, Can we talk terms?");
    assert!(message["attribution"].is_null(), "a person wrote it");

    let drafts = MailProvider::default()
        .invoke(&ctx, &list, &json!({ "folder": "drafts" }))
        .unwrap();
    let draft = &drafts.data["messages"][0];
    assert_eq!(draft["attribution"]["by"], "invoice-demo");
    assert_eq!(draft["attribution"]["for"], "demo");

    let read = MailProvider::default()
        .invoke(&ctx, &action("mail.read"), &json!({ "id": draft["id"] }))
        .unwrap();
    assert_eq!(
        read.data["message"]["body"]
            .as_str()
            .unwrap()
            .replace("\r\n", "\n"),
        "Yes.\n"
    );
    assert_eq!(
        read.data["message"]["attribution"]["text"],
        "by invoice-demo for demo"
    );
}

#[test]
fn message_ids_stay_inside_their_mailbox() {
    let (_dir, ctx) = setup();
    let read = action("mail.read");
    for id in [
        "inbox/../../etc/passwd.eml",
        "inbox/.hidden.eml",
        "outbox/x.eml",
        "inbox/x.txt",
        "inbox",
    ] {
        assert!(
            matches!(
                MailProvider::default().preview(&ctx, &read, &json!({ "id": id })),
                Err(ActionError::InvalidInput(_))
            ),
            "{id}"
        );
    }
    assert!(
        MailProvider::default()
            .preview(&ctx, &action("mail.list"), &json!({ "folder": "outbox" }))
            .is_err()
    );
}

#[test]
fn rejects_bad_addresses_and_header_injection() {
    let (_dir, ctx) = setup();
    let draft = action("mail.draft");
    let bad_addr = json!({ "to": ["not an address"], "subject": "x", "body": "y" });
    assert!(
        MailProvider::default()
            .preview(&ctx, &draft, &bad_addr)
            .is_err()
    );
    let injected = json!({ "to": ["a@b.co"], "subject": "x\nBcc: all@corp.com", "body": "y" });
    assert!(
        MailProvider::default()
            .preview(&ctx, &draft, &injected)
            .is_err()
    );
    let smuggled = json!({ "to": ["a@b.co>\r\nRCPT TO:<c@d.co"], "subject": "x", "body": "y" });
    assert!(
        MailProvider::default()
            .preview(&ctx, &draft, &smuggled)
            .is_err()
    );
}

// ---- With a mail service (ADR 0037, EM0), on an in-memory mailbox ------

const PASSWORD: &str = "correct-horse-canary-5521";

struct Service {
    _state: tempfile::TempDir,
    link: Arc<MemoryLink>,
    mail: MailHandle,
    provider: MailProvider,
}

fn settings() -> MailSettings {
    let server = |port| MailServer {
        host: "mail.corp.example".into(),
        port,
        security: MailSecurity::Tls,
    };
    MailSettings {
        service: MailService::Imap,
        address_template: Some("{login}@corp.example".into()),
        tenant: None,
        incoming: Some(server(993)),
        outgoing: Some(server(465)),
    }
}

fn service() -> Service {
    let state = tempfile::tempdir().unwrap();
    let link = Arc::new(MemoryLink::new(PASSWORD));
    let mail = MailHandle::new(state.path().join("mail"));
    let server = |port| Server {
        host: "mail.corp.example".into(),
        port,
        implicit_tls: true,
    };
    mail.connect(Remote {
        link: link.clone(),
        incoming: server(993),
        outgoing: server(465),
    });
    let provider = MailProvider::new(DirectoryHandle::new())
        .settings(Some(settings()))
        .mail(mail.clone());
    Service {
        _state: state,
        link,
        mail,
        provider,
    }
}

impl Service {
    fn connect(&self, ctx: &TaskContext) {
        let mut app = ctx.clone();
        app.principal.agent = APP.parse().unwrap();
        self.provider
            .invoke(
                &app,
                &action("mail.connect"),
                &json!({ "password": PASSWORD }),
            )
            .unwrap();
    }
}

fn arriving(subject: &str) -> Vec<u8> {
    format!(
        "From: Dana Ruiz <dana@northwind.example>\r\nTo: demo@corp.example\r\n\
         Subject: {subject}\r\nDate: Mon, 19 Oct 2026 08:00:00 +0000\r\n\
         Message-ID: <{subject}@northwind.example>\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/html; charset=utf-8\r\n\r\n<p>About <b>{subject}</b></p>\r\n"
    )
    .into_bytes()
}

#[test]
fn em0_connecting_checks_the_password_and_keeps_it_root_only_never_in_a_result() {
    let (_home, ctx) = setup();
    let s = service();
    let connect = action("mail.connect");
    let wrong = s
        .provider
        .invoke(&ctx, &connect, &json!({ "password": "wrong-canary" }))
        .unwrap_err();
    assert!(wrong.to_string().contains("didn't accept"), "{wrong}");
    assert!(!s.mail.connected("demo"));

    let preview = s
        .provider
        .preview(&ctx, &connect, &json!({ "password": PASSWORD }))
        .unwrap();
    assert_eq!(preview.summary, "Connect mail as demo@corp.example");
    let out = s
        .provider
        .invoke(&ctx, &connect, &json!({ "password": PASSWORD }))
        .unwrap();
    let shown = format!("{preview:?} {} {}", out.text, out.data);
    assert!(!shown.contains(PASSWORD), "{shown}");
    assert!(s.mail.connected("demo"));
    let kept = s._state.path().join("mail/demo/account.json");
    assert_eq!(
        fs::metadata(&kept).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(kept.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    // And the account says so.
    let account = s
        .provider
        .invoke(&ctx, &action("mail.account"), &json!({}))
        .unwrap();
    assert_eq!(account.data["connected"], true);
    assert!(!account.text.contains(PASSWORD));
}

#[test]
fn em0_the_inbox_syncs_into_the_cache_by_uid_and_agents_read_text_only() {
    let (home, ctx) = setup();
    let s = service();
    let uid = s.link.deliver("INBOX", &arriving("Renewal"));
    s.connect(&ctx);
    let list = s
        .provider
        .invoke(&ctx, &action("mail.list"), &json!({ "folder": "inbox" }))
        .unwrap();
    let message = &list.data["messages"][0];
    let id = message["id"].as_str().unwrap().to_owned();
    assert_eq!(id, format!("inbox/{}", cache::synced_name(1000, uid)));
    assert_eq!(message["subject"], "Renewal");
    assert_eq!(message["date"], "2026-10-19T08:00:00Z");
    assert_eq!(
        fs::read(
            home.path()
                .join("Mail/Inbox")
                .join(cache::synced_name(1000, uid))
        )
        .unwrap(),
        arriving("Renewal"),
        "kept byte for byte"
    );

    let read = action("mail.read");
    let by_agent = s
        .provider
        .invoke(&ctx, &read, &json!({ "id": id }))
        .unwrap();
    assert!(
        by_agent.data["message"]["html"].is_null(),
        "agents read text"
    );
    assert!(
        by_agent.data["message"]["body"]
            .as_str()
            .unwrap()
            .contains("About Renewal")
    );
    let mut app = ctx.clone();
    app.principal.agent = APP.parse().unwrap();
    let in_mail = s
        .provider
        .invoke(&app, &read, &json!({ "id": id }))
        .unwrap();
    assert!(
        in_mail.data["message"]["html"]
            .as_str()
            .unwrap()
            .contains("<b>Renewal</b>")
    );

    // Mail moved away elsewhere leaves the cache with the next sync.
    s.link
        .move_to(&s.mail.account("demo").unwrap().1, "INBOX", uid, "Archive")
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5100));
    let again = s
        .provider
        .invoke(&ctx, &action("mail.list"), &json!({ "folder": "inbox" }))
        .unwrap();
    assert_eq!(again.data["messages"].as_array().unwrap().len(), 0);
}

#[test]
fn em0_a_send_is_built_here_submitted_filed_in_sent_and_audited_by_hash() {
    let (home, ctx) = setup();
    let s = service();
    s.connect(&ctx);
    let input = json!({ "messages": [{ "to": ["dana@northwind.example"], "subject": "Renewal", "body": "Friday works." }] });
    let out = s
        .provider
        .invoke(&ctx, &action("mail.send"), &input)
        .unwrap();
    let submitted = s.link.submitted();
    assert_eq!(submitted.len(), 1);
    let (from, to, raw) = &submitted[0];
    assert_eq!(from, "demo@corp.example");
    assert_eq!(to, &["dana@northwind.example".to_owned()]);
    let text = String::from_utf8(raw.clone()).unwrap();
    assert!(text.contains("X-Glasshouse-Attribution: by invoice-demo for demo\r\n"));
    let audit = &out.data[AUDIT_DETAIL][0];
    assert_eq!(audit["sha256"], glass_managed::sha256_hex(raw));
    assert_eq!(audit["size"], raw.len());
    assert_eq!(audit["recipients"], 1);
    assert!(audit["reply"].as_str().unwrap().starts_with("250"));
    assert!(!audit.to_string().contains("Friday"), "never the body");
    // Filed in Sent on the server and here, byte for byte what left.
    let sent = s.link.messages("Sent");
    assert_eq!(&sent[0].1, raw);
    let change = &out.changes[0];
    assert_eq!(change.kind, ChangeKind::Sent);
    assert!(!change.reversible);
    assert_eq!(&fs::read(&change.paths[0]).unwrap(), raw);
    assert!(change.paths[0].starts_with(home.path().join("Mail/Sent")));
}

#[test]
fn dp30_offline_lists_say_so_and_a_send_waits_in_drafts() {
    let (_home, ctx) = setup();
    let s = service();
    s.link.deliver("INBOX", &arriving("Kept"));
    s.connect(&ctx);
    s.link.offline(true);
    std::thread::sleep(std::time::Duration::from_millis(5100));
    let list = s
        .provider
        .invoke(&ctx, &action("mail.list"), &json!({ "folder": "inbox" }))
        .unwrap();
    assert_eq!(list.data["messages"][0]["subject"], "Kept");
    assert!(list.text.contains("offline"), "{}", list.text);

    let input = json!({ "messages": [{ "to": ["dana@northwind.example"], "subject": "Waiting", "body": "x" }] });
    let err = s
        .provider
        .invoke(&ctx, &action("mail.send"), &input)
        .unwrap_err();
    let err = err.to_string();
    assert!(err.contains("Not sent") && err.contains("offline"), "{err}");
    assert!(err.contains("drafts/"), "{err}");
    assert!(s.link.submitted().is_empty());
    let drafts = s
        .provider
        .invoke(&ctx, &action("mail.list"), &json!({ "folder": "drafts" }))
        .unwrap();
    assert_eq!(drafts.data["messages"][0]["subject"], "Waiting");
}

#[test]
fn em0_mail_moves_on_the_server_and_undo_moves_it_back() {
    let (home, ctx) = setup();
    let s = service();
    let uid = s.link.deliver("INBOX", &arriving("Spammy"));
    s.connect(&ctx);
    let id = format!("inbox/{}", cache::synced_name(1000, uid));
    let input = json!({ "ids": [id], "to": "junk" });
    let preview = s
        .provider
        .preview(&ctx, &action("mail.move"), &input)
        .unwrap();
    assert_eq!(
        preview.summary,
        "Move an email to Junk: from Dana Ruiz <dana@northwind.example>: Spammy"
    );
    assert!(
        preview
            .snapshot_roots
            .contains(&home.path().join("Mail/Inbox"))
    );
    assert!(
        preview
            .snapshot_roots
            .contains(&home.path().join("Mail/Junk"))
    );
    let out = s
        .provider
        .invoke(&ctx, &action("mail.move"), &input)
        .unwrap();
    assert!(s.link.messages("INBOX").is_empty());
    assert_eq!(s.link.messages("Junk").len(), 1);
    let change = &out.changes[0];
    assert_eq!(change.kind, ChangeKind::Moved);
    assert!(change.reversible);
    assert!(!change.paths[0].exists() && change.paths[1].exists());

    // Undo: the snapshot puts the files back, then the hook moves it back.
    fs::rename(&change.paths[1], &change.paths[0]).unwrap();
    s.provider
        .revert(&ctx, &action("mail.move"), &change.paths)
        .unwrap();
    assert!(s.link.messages("Junk").is_empty());
    let back = s.link.messages("INBOX");
    assert_eq!(back.len(), 1);
    let named = home
        .path()
        .join("Mail/Inbox")
        .join(cache::synced_name(1000, back[0].0));
    assert!(named.exists(), "the restored file names its new UID");
}

#[test]
fn em0_without_a_service_moves_and_sends_stay_on_this_computer() {
    let (home, ctx) = setup();
    fs::write(
        home.path().join("Mail/Inbox/1792400000-0-hello.eml"),
        "From: a@b.example\nSubject: Hello\nDate: 2026-10-19T08:00:00Z\n\nHi\n",
    )
    .unwrap();
    let out = MailProvider::default()
        .invoke(
            &ctx,
            &action("mail.move"),
            &json!({ "ids": ["inbox/1792400000-0-hello.eml"], "to": "archive" }),
        )
        .unwrap();
    assert!(
        home.path()
            .join("Mail/Archive/1792400000-0-hello.eml")
            .exists()
    );
    assert!(
        MailProvider::default()
            .revert(&ctx, &action("mail.move"), &out.changes[0].paths)
            .is_ok()
    );
    for bad in [
        json!({ "ids": ["drafts/1792400000-0-x.eml"], "to": "archive" }),
        json!({ "ids": ["archive/1792400000-0-hello.eml"], "to": "archive" }),
        json!({ "ids": ["archive/1792400000-0-hello.eml"], "to": "sent" }),
    ] {
        assert!(
            matches!(
                MailProvider::default().preview(&ctx, &action("mail.move"), &bad),
                Err(ActionError::InvalidInput(_))
            ),
            "{bad}"
        );
    }
}

#[test]
fn a_message_too_large_to_keep_keeps_its_header_and_says_so() {
    let raw = message::header_only(
        b"From: a@b.example\r\nSubject: Big\r\nContent-Type: multipart/mixed;\r\n boundary=x\r\n\r\n",
    );
    let parsed = message::parse(&raw);
    assert_eq!(parsed.subject, "Big");
    assert!(parsed.body.contains("too large"), "{}", parsed.body);
    let _ = Path::new("");
}
