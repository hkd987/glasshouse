//! The seat (ADR 0031): the sign-in screen says who is at the screen and
//! which `su` runs their desktop; only that desktop says the person is
//! using it; the computer sleeps after the person's idle minutes, agents
//! paused first and carrying on after.

#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glass_api::{SeatBegin, TileState, methods};
use glass_rpc::Client;
use glass_runtime::Caller;
use serde_json::{Value, json};

use crate::common::{Harness, harness_with};

fn root(h: &Harness) -> Caller {
    Caller {
        name: "root".into(),
        uid: 0,
        gid: 0,
        home: h.home(),
    }
}

/// The sign-in screen started the test user's desktop as the child of
/// `session`.
fn signed_in(h: &Harness, session: u32) {
    h.rt.seat_begin(
        &root(h),
        SeatBegin {
            user: h.me.name.clone(),
            session_pid: session,
        },
    )
    .unwrap();
}

const SLEEPY: &str = r#"
[[group]]
name = "kiosk"
members = ["{me}"]
[group.settings]
"power.sleep_after_minutes" = { value = 1, locked = true }
"#;

#[tokio::test(flavor = "multi_thread")]
async fn adr0031_only_the_desktop_the_sign_in_screen_started_says_the_person_is_there() {
    let h = harness_with("");
    let me = h.me.clone();
    // Only root says who is at the screen.
    let refused = h.rt.seat_begin(
        &me,
        SeatBegin {
            user: me.name.clone(),
            session_pid: 1,
        },
    );
    assert!(me.uid == 0 || refused.is_err());

    // This test plays the desktop: its parent is the `su` the sign-in
    // screen started.
    let desktop = Client::connect(h.serve_control()).await.unwrap();
    let before: Result<Value, _> = desktop.call(methods::SEAT_ACTIVE, json!(null)).await;
    assert!(before.is_err(), "nobody is signed in yet");
    signed_in(&h, std::os::unix::process::parent_id());
    let _: Value = desktop
        .call(methods::SEAT_ACTIVE, json!(null))
        .await
        .unwrap();

    // Another `su`: this process isn't that desktop, so it can't keep the
    // computer awake or put it to sleep.
    signed_in(&h, 1);
    let other: Result<Value, _> = desktop.call(methods::SEAT_ACTIVE, json!(null)).await;
    assert!(other.is_err());
    h.rt.seat_end(&root(&h)).unwrap();
    assert!(!h.rt.sleep_due(Instant::now() + Duration::from_secs(3600)));
}

#[tokio::test(flavor = "multi_thread")]
async fn adr0031_pm5_the_computer_sleeps_after_the_persons_idle_minutes_agents_paused_first() {
    let h = harness_with(SLEEPY);
    signed_in(&h, std::os::unix::process::parent_id());
    let now = Instant::now();
    assert!(!h.rt.sleep_due(now), "just signed in");
    assert!(!h.rt.sleep_due(now + Duration::from_secs(50)));
    assert!(
        h.rt.sleep_due(now + Duration::from_secs(61)),
        "a minute alone"
    );

    // An agent at work pauses for the sleep and carries on after it.
    let task = h.rt.start(&h.me, h.request(&["Invoices"], &[])).unwrap().id;
    let during = Arc::new(Mutex::new(None));
    let (rt, me, seen) = (Arc::clone(&h.rt), h.me.clone(), Arc::clone(&during));
    h.rt.sleep_now(move || {
        let tile = rt.get(&me, task).unwrap().view.state;
        *seen.lock().unwrap() = Some(tile);
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(*during.lock().unwrap(), Some(TileState::Paused));
    assert_ne!(h.rt.get(&h.me, task).unwrap().view.state, TileState::Paused);
    // Idle starts over from the wake.
    assert!(!h.rt.sleep_due(Instant::now()));

    // A sleep that fails still wakes the agents, and says why.
    let failed = h.rt.sleep_now(|| Err("no S3 here".into())).await;
    assert!(failed.unwrap_err().to_string().contains("no S3 here"));
    assert_ne!(h.rt.get(&h.me, task).unwrap().view.state, TileState::Paused);
}

#[tokio::test(flavor = "multi_thread")]
async fn adr0031_zero_minutes_is_never() {
    let h = harness_with(&SLEEPY.replace("value = 1,", "value = 0,"));
    signed_in(&h, std::os::unix::process::parent_id());
    assert!(
        !h.rt
            .sleep_due(Instant::now() + Duration::from_secs(24 * 3600))
    );
}
