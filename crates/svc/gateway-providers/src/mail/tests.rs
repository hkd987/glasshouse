//! `ImapLink` against a real server, `mox localserve` (skipped, saying so,
//! when it isn't built: `tools/dev/mox.sh build`).

#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use glass_managed::mail::{
    Account, LinkError, MailLink, Outgoing, Role, Secret, Server, SyncLimits,
};

use super::ImapLink;
use super::testing::{self, Mox};

const LIMITS: SyncLimits = SyncLimits {
    newest: 500,
    max_bytes: 1024 * 1024,
};

fn message(subject: &str) -> Vec<u8> {
    format!(
        "From: Dana Ruiz <dana@northwind.example>\r\nTo: mox@localhost\r\nSubject: {subject}\r\n\
         Date: Mon, 19 Oct 2026 08:00:00 +0000\r\nMessage-ID: <{subject}@northwind.example>\r\n\r\n\
         Hello.\r\n.A line that starts with a dot.\r\n"
    )
    .into_bytes()
}

#[test]
fn em0_imap_link_lists_syncs_fetches_and_moves_on_a_real_server() {
    let Some(mox) = Mox::start() else { return };
    let link = mox.link();
    let me = mox.account("dana");

    let folders = link.folders(&me).unwrap();
    for role in Role::ALL {
        assert!(
            folders.iter().any(|f| f.role == Some(role)),
            "{role:?} in {folders:?}"
        );
    }
    let name = |role| {
        folders
            .iter()
            .find(|f| f.role == Some(role))
            .unwrap()
            .name
            .clone()
    };

    let first = mox.deliver(&message("first"));
    let second = mox.deliver(&message("second"));
    let synced = link.sync(&me, "Inbox", None, &[], LIMITS).unwrap();
    assert_eq!(
        synced.new.iter().map(|m| m.uid).collect::<Vec<_>>(),
        [first, second]
    );
    assert_eq!(synced.new[0].raw, message("first"), "byte for byte");
    assert!(synced.state.uid_validity > 0 && synced.state.exists == 2);

    // Nothing changed: nothing comes.
    let again = link
        .sync(&me, "Inbox", Some(&synced.state), &[first, second], LIMITS)
        .unwrap();
    assert!(again.new.is_empty() && again.vanished.is_empty() && !again.reset);

    let third = mox.deliver(&message("third"));
    let moved = link
        .move_to(&me, "Inbox", first, &name(Role::Archive))
        .unwrap();
    assert!(moved.is_some(), "mox says where it went (UIDPLUS)");
    let next = link
        .sync(&me, "Inbox", Some(&synced.state), &[first, second], LIMITS)
        .unwrap();
    assert_eq!(next.new.iter().map(|m| m.uid).collect::<Vec<_>>(), [third]);
    assert_eq!(next.vanished, [first]);

    // Without a modseq, the same answer comes from comparing every UID.
    let mut plain = synced.state;
    plain.highest_modseq = None;
    let compared = link
        .sync(&me, "Inbox", Some(&plain), &[first, second], LIMITS)
        .unwrap();
    assert_eq!(compared.vanished, [first]);

    let archived = link
        .fetch(&me, &name(Role::Archive), moved.unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(archived, message("first"));
    assert_eq!(link.fetch(&me, "Inbox", first).unwrap(), None);

    // A message over the cap comes as its header.
    let small = SyncLimits {
        newest: 1,
        max_bytes: 10,
    };
    let capped = link.sync(&me, "Inbox", None, &[], small).unwrap();
    assert_eq!(capped.new.len(), 1, "only the newest");
    assert!(capped.new[0].truncated);
    assert!(capped.new[0].raw.starts_with(b"From: Dana"));
    assert!(!capped.new[0].raw.windows(6).any(|w| w == b"Hello."));
}

#[test]
fn em0_imap_link_submits_and_files_in_sent() {
    let Some(mox) = Mox::start() else { return };
    let link = mox.link();
    let me = mox.account("dana");
    // mox, like most servers, sends only as the signed-in person.
    let sent = String::from_utf8(message("outgoing"))
        .unwrap()
        .replace("Dana Ruiz <dana@northwind.example>", testing::USER)
        .into_bytes();
    let to = vec!["priya@elsewhere.example".to_owned()];
    let submitted = link
        .submit(
            &me,
            &Outgoing {
                from: testing::USER,
                to: &to,
                message: &sent,
            },
        )
        .unwrap();
    assert!(submitted.reply.starts_with("250"), "{}", submitted.reply);
    // It waits in mox's queue (whose delivery back to itself its untrained
    // junk filter may refuse).
    let queued = mox.queue();
    assert!(
        queued
            .iter()
            .any(|m| m.contains("to:priya@elsewhere.example")),
        "{queued:?}"
    );

    let uid = link.append(&me, "Sent", &sent).unwrap().unwrap();
    assert_eq!(link.fetch(&me, "Sent", uid).unwrap().unwrap(), sent);

    // A message from someone else is refused, with the server's reason.
    let refused = link.submit(
        &me,
        &Outgoing {
            from: testing::USER,
            to: &to,
            message: &message("not-mine"),
        },
    );
    assert!(
        matches!(&refused, Err(LinkError::Refused(_))),
        "{refused:?}\n{}",
        mox.log()
    );
}

#[test]
fn em0_a_wrong_password_is_a_sign_in_error_and_never_echoed() {
    let Some(mox) = Mox::start() else { return };
    let mut me = mox.account("dana");
    me.secret = Secret::new("wrong-canary-8812");
    let err = mox.link().check(&me).unwrap_err();
    assert!(matches!(err, LinkError::SignIn(_)), "{err:?}");
    assert!(!err.to_string().contains("canary"), "{err}");
    assert!(mox.link().check(&mox.account("dana")).is_ok());
}

#[test]
fn em0_starttls_ports_work_and_an_untrusted_server_is_refused() {
    let Some(mox) = Mox::start() else { return };
    let mut me = mox.account("dana");
    me.incoming.port = testing::IMAP;
    me.incoming.implicit_tls = false;
    me.outgoing.port = testing::SUBMISSION;
    me.outgoing.implicit_tls = false;
    mox.link().check(&me).unwrap();

    // The system's trust store doesn't know mox's certificate.
    let stranger = ImapLink::for_test_server(b"", "localhost", mox.ip());
    let err = stranger.check(&mox.account("dana")).unwrap_err();
    assert!(err.is_offline(), "{err:?}");
}

#[test]
fn em0_nothing_listening_is_offline() {
    let link = ImapLink::new();
    let server = |port| Server {
        host: "127.0.0.1".into(),
        port,
        implicit_tls: true,
    };
    let account = Account {
        login: "dana".into(),
        incoming: server(9),
        outgoing: server(9),
        user: "dana".into(),
        secret: Secret::new("x"),
    };
    assert!(link.check(&account).unwrap_err().is_offline());
}

#[test]
fn nv3_new_mail_ends_an_idle_wait_at_once() {
    let Some(mox) = Mox::start() else { return };
    let link = mox.link();
    let me = mox.account("dana");
    let waiting = {
        let (link, me) = (link.clone(), me.clone());
        std::thread::spawn(move || {
            let started = Instant::now();
            let changed = link.wait(&me, "Inbox", Duration::from_secs(20)).unwrap();
            (changed, started.elapsed())
        })
    };
    std::thread::sleep(Duration::from_millis(500));
    let delivered = Instant::now();
    mox.deliver(&message("ping"));
    let (changed, took) = waiting.join().unwrap();
    assert!(changed);
    assert!(
        took < Duration::from_secs(10) && delivered.elapsed() < Duration::from_secs(2),
        "{took:?}"
    );
    // With nothing new, the wait ends when its time is up.
    let quiet = link.wait(&me, "Inbox", Duration::from_millis(300)).unwrap();
    assert!(!quiet);
}
