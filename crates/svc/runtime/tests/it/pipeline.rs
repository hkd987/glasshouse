//! End-to-end tests of the action pipeline on development backends (host
//! sandbox, copy snapshots). The test plays the agent: it connects to the
//! task socket and makes MCP calls, while the "agent process" just sleeps.
//! Containment itself is tested on FreeBSD by the e2e suite.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use glass_api::{
    AnswerApproval, ApprovalAnswer, ApprovalScope, AskLabels, ScopeRequest, TITLE_MAX_CHARS,
    TaskRequest, TileState, UndoRequest,
};
use glass_audit::{EventKind, Outcome};
use glass_rpc::mcp::ListToolsResult;
use glass_runtime::{Caller, RuntimeError};
use glass_types::Tier;
use serde_json::{Value, json};

use crate::common::{Harness, eventually, harness, harness_with, rebuild, send_args, tool};

#[tokio::test]
async fn plan_shows_the_handoff_card_without_side_effects() {
    let h = harness();
    let card =
        h.rt.plan(&h.me, &h.request(&["Invoices"], &["Reports"]))
            .unwrap();
    assert_eq!(card.plan.len(), 3);
    assert_eq!(card.touches.len(), 2);
    assert!(card.asks_before.contains(&"Send emails".to_string()));
    assert!(card.asks_before.contains(&"Delete a file".to_string()));
    assert!(card.notifies.contains(&"Write a file".to_string()));
    assert!(card.cant_touch.iter().any(|l| l.contains("network")));
    // IT's Never rules show even though no installed app offers them.
    for rule in ["Security settings", "Vendor bank details"] {
        assert!(card.cant_touch.contains(&rule.to_string()), "{rule}");
    }
    assert!(h.rt.list(&h.me).is_empty(), "planning starts nothing");
}

#[tokio::test]
async fn scope_must_stay_inside_the_users_home() {
    let h = harness();
    // Root may grant anything, and CI may run as root, so use a plain user.
    let user = Caller {
        uid: 4242,
        ..h.me.clone()
    };
    let mut request = h.request(&[], &[]);
    request.scope.write = vec!["/etc".into()];
    assert!(matches!(
        h.rt.plan(&user, &request),
        Err(RuntimeError::PermissionDenied(_))
    ));
    request.scope.write = vec![h.home().join("Nope")];
    assert!(matches!(
        h.rt.plan(&user, &request),
        Err(RuntimeError::NotFound(_))
    ));
    request.scope.write = vec![h.home().join("Reports")];
    assert!(h.rt.plan(&user, &request).is_ok());
}

#[tokio::test]
async fn restrictions_tighten_and_never_loosen() {
    let h = harness();
    let mut request = h.request(&["Invoices"], &["Reports"]);
    request.scope.restrict = vec![("mail.send".into(), Tier::Never)];
    let card = h.rt.plan(&h.me, &request).unwrap();
    assert!(
        card.cant_touch.contains(&"Send emails".to_string()),
        "{:?}",
        card.cant_touch
    );
    request.scope.restrict = vec![("mail.send".into(), Tier::Auto)];
    assert!(matches!(
        h.rt.plan(&h.me, &request),
        Err(RuntimeError::Invalid(_))
    ));
}

#[tokio::test]
async fn reads_and_writes_flow_through_tiers_snapshots_and_audit() {
    let h = harness();
    let task =
        h.rt.start(&h.me, h.request(&["Invoices"], &["Reports"]))
            .unwrap()
            .id;
    let agent = h.agent(task).await;

    let tools: ListToolsResult = agent.call("tools/list", Value::Null).await.unwrap();
    let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_str()).collect();
    assert!(
        names.contains(&"files_read")
            && names.contains(&"mail_send")
            && names.contains(&"report_progress")
    );

    let listing = tool(&agent, "files_list", json!({"path": "Invoices"})).await;
    assert!(!listing.is_error && listing.text().contains("inv-002.txt"));
    let invoice = tool(
        &agent,
        "files_read",
        json!({"path": "Invoices/inv-003.txt"}),
    )
    .await;
    assert!(invoice.text().contains("310.25"));
    let progress = tool(
        &agent,
        "report_progress",
        json!({"caption": "Summarizing invoices", "done": 3, "total": 3}),
    )
    .await;
    assert!(!progress.is_error);
    let written = tool(
        &agent,
        "files_write",
        json!({"path": "Reports/summary.csv", "content": "total,505.75\n"}),
    )
    .await;
    assert!(!written.is_error, "{}", written.text());
    assert_eq!(
        fs::read_to_string(h.home().join("Reports/summary.csv")).unwrap(),
        "total,505.75\n"
    );
    assert_eq!(
        fs::metadata(h.home().join("Reports/summary.csv"))
            .unwrap()
            .uid(),
        h.me.uid
    );

    let detail = h.rt.get(&h.me, task).unwrap();
    assert_eq!(detail.view.state, TileState::Working);
    assert_eq!(detail.view.caption, "Summarizing invoices");
    assert_eq!(
        detail.view.feed.last().unwrap().text,
        "Write Reports/summary.csv"
    );
    assert_eq!(detail.view.feed.len(), 3);
    // One snapshot when the task started, one before the Notify-tier write.
    assert_eq!(detail.snapshots.len(), 2);
    assert_eq!(detail.changes.len(), 1);

    let records = glass_audit::read(&h.audit_path(), |r| r.event.task == Some(task)).unwrap();
    let actions: Vec<_> = records
        .iter()
        .filter(|r| r.event.kind == EventKind::Action)
        .collect();
    assert_eq!(actions.len(), 3);
    let write = actions.last().unwrap();
    assert_eq!(write.event.decision.as_ref().unwrap().tier, Tier::Notify);
    assert_eq!(write.event.outcome, Some(Outcome::Completed));
    assert!(write.event.snapshot.is_some());
    assert_eq!(write.event.user.as_deref(), Some(h.me.name.as_str()));
    // Every action says how long the OS took, for the latency budgets.
    for action in &actions {
        assert!(action.event.detail["elapsed_ms"].as_f64().unwrap() >= 0.0);
    }
    assert!(glass_audit::verify(&h.audit_path()).is_ok());
}

#[tokio::test]
async fn sends_wait_for_one_batched_approval() {
    let h = harness();
    let task =
        h.rt.start(&h.me, h.request(&["Invoices"], &["Reports"]))
            .unwrap()
            .id;
    let agent = Arc::new(h.agent(task).await);

    let pending_send = {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move { tool(&agent, "mail_send", send_args(6)).await })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    assert_eq!(ask.summary, "Send 6 emails");
    assert_eq!(
        ask.items.len(),
        6,
        "one approval with a review list (DA-12)"
    );
    // What each says, under it: the person reads what they send (ADR 0045).
    assert_eq!(ask.texts, vec![vec!["Please pay.".to_owned()]; 6]);
    assert_eq!(h.tile(task), TileState::NeedsYou);
    assert!(h.rt.list(&h.me)[0].needs.is_some());
    assert!(
        !pending_send.is_finished(),
        "the agent is held while it waits"
    );

    h.rt.answer(
        &h.me,
        AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Approve {
                scope: ApprovalScope::Once,
            },
        },
    )
    .unwrap();
    let sent = pending_send.await.unwrap();
    assert!(!sent.is_error, "{}", sent.text());
    assert_eq!(fs::read_dir(h.home().join("Mail/Sent")).unwrap().count(), 6);
    assert_eq!(h.tile(task), TileState::Working);

    // Approved once means the next send asks again; approving for the task
    // means the one after that doesn't.
    let next = {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move { tool(&agent, "mail_send", send_args(1)).await })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    h.rt.answer(
        &h.me,
        AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Approve {
                scope: ApprovalScope::Task,
            },
        },
    )
    .unwrap();
    assert!(!next.await.unwrap().is_error);
    let unasked = tool(&agent, "mail_send", send_args(1)).await;
    assert!(!unasked.is_error);
    assert!(h.rt.approvals(&h.me).is_empty());

    let records = glass_audit::read(&h.audit_path(), |r| {
        r.event.kind == EventKind::ApprovalAnswered
    })
    .unwrap();
    assert_eq!(records.len(), 2);
    // The review showed the words; the record never keeps them.
    let recorded = glass_audit::read(&h.audit_path(), |_| true).unwrap();
    assert!(
        !serde_json::to_string(&recorded)
            .unwrap()
            .contains("Please pay."),
        "an email's body is never recorded"
    );
    assert_eq!(
        records[0]
            .event
            .approval
            .as_ref()
            .unwrap()
            .answered_by
            .as_deref(),
        Some(h.me.name.as_str())
    );
}

#[tokio::test]
async fn declined_asks_come_back_as_tool_errors() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = Arc::new(h.agent(task).await);
    let call = {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move {
            tool(
                &agent,
                "files_delete",
                json!({"path": "Reports/existing.txt"}),
            )
            .await
        })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    h.rt.answer(
        &h.me,
        AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Deny,
        },
    )
    .unwrap();
    let result = call.await.unwrap();
    assert!(result.is_error && result.text().contains("declined"));
    assert!(h.home().join("Reports/existing.txt").exists());
}

#[tokio::test]
async fn scope_violations_and_labels_are_blocked_and_audited() {
    let h = harness();
    let task =
        h.rt.start(&h.me, h.request(&["Invoices", "Finance"], &[]))
            .unwrap()
            .id;
    let agent = h.agent(task).await;

    let outside = tool(&agent, "files_read", json!({"path": "Private/diary.txt"})).await;
    assert!(outside.is_error && outside.text().contains("outside this task's scope"));
    let escape = tool(
        &agent,
        "files_read",
        json!({"path": "Invoices/../../Private/x"}),
    )
    .await;
    assert!(escape.is_error);
    let labeled = tool(&agent, "files_read", json!({"path": "Finance/q3.csv"})).await;
    assert!(
        labeled.is_error && labeled.text().contains("labeled confidential"),
        "{}",
        labeled.text()
    );
    let read_only = tool(
        &agent,
        "files_write",
        json!({"path": "Invoices/x.txt", "content": "x"}),
    )
    .await;
    assert!(read_only.is_error && read_only.text().contains("read-only"));

    let blocked = glass_audit::read(&h.audit_path(), |r| {
        r.event.task == Some(task) && r.event.outcome == Some(Outcome::Blocked)
    })
    .unwrap();
    assert_eq!(blocked.len(), 4, "every refusal is on the record");
}

#[tokio::test]
async fn undo_all_restores_removes_and_lists_what_left_the_machine() {
    let h = harness();
    let task =
        h.rt.start(&h.me, h.request(&["Invoices"], &["Reports"]))
            .unwrap()
            .id;
    let agent = Arc::new(h.agent(task).await);
    assert!(
        !tool(
            &agent,
            "files_write",
            json!({"path": "Reports/new.csv", "content": "a"})
        )
        .await
        .is_error
    );
    assert!(
        !tool(
            &agent,
            "files_write",
            json!({"path": "Reports/existing.txt", "content": "clobbered"})
        )
        .await
        .is_error
    );
    assert!(
        !tool(
            &agent,
            "mail_draft",
            json!({"to": ["a@example.com"], "subject": "Draft", "body": "x"})
        )
        .await
        .is_error
    );
    let send = {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move { tool(&agent, "mail_send", send_args(1)).await })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    h.rt.answer(
        &h.me,
        AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Approve {
                scope: ApprovalScope::Once,
            },
        },
    )
    .unwrap();
    assert!(!send.await.unwrap().is_error);

    let preview =
        h.rt.undo(
            &h.me,
            UndoRequest {
                task,
                to: None,
                changes: vec![],
                dry_run: true,
            },
        )
        .await
        .unwrap();
    assert!(preview.dry_run);
    assert_eq!(preview.restored, ["~/Reports/existing.txt"]);
    assert_eq!(preview.removed.len(), 2, "{:?}", preview.removed);
    assert_eq!(preview.not_undone.len(), 1);
    assert_eq!(
        fs::read_to_string(h.home().join("Reports/existing.txt")).unwrap(),
        "clobbered",
        "dry run changes nothing"
    );

    let report =
        h.rt.undo(
            &h.me,
            UndoRequest {
                task,
                to: None,
                changes: vec![],
                dry_run: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(report.restored, preview.restored);
    assert_eq!(
        fs::read_to_string(h.home().join("Reports/existing.txt")).unwrap(),
        "original report\n"
    );
    assert!(!h.home().join("Reports/new.csv").exists());
    assert_eq!(
        fs::read_dir(h.home().join("Mail/Drafts")).unwrap().count(),
        0
    );
    assert_eq!(
        fs::read_dir(h.home().join("Mail/Sent")).unwrap().count(),
        1,
        "sent mail can't be recalled"
    );
    assert_eq!(
        h.tile(task),
        TileState::Paused,
        "rollback holds the agent (AW-22)"
    );
}

#[tokio::test]
async fn rolling_back_to_a_marker_keeps_earlier_work() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(task).await;
    assert!(
        !tool(
            &agent,
            "files_write",
            json!({"path": "Reports/one.txt", "content": "1"})
        )
        .await
        .is_error
    );
    assert!(
        !tool(
            &agent,
            "files_write",
            json!({"path": "Reports/two.txt", "content": "2"})
        )
        .await
        .is_error
    );
    let markers = h.rt.get(&h.me, task).unwrap().snapshots;
    // [task started, before one.txt, before two.txt]
    assert_eq!(markers.len(), 3);
    let report =
        h.rt.undo(
            &h.me,
            UndoRequest {
                task,
                to: Some(markers[2].id),
                changes: vec![],
                dry_run: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(report.removed, ["~/Reports/two.txt"]);
    assert!(h.home().join("Reports/one.txt").exists());
}

#[tokio::test]
async fn pause_holds_calls_at_a_safe_point_until_resume() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&["Invoices"], &[])).unwrap().id;
    let agent = Arc::new(h.agent(task).await);
    h.rt.pause(&h.me, task).unwrap();
    let call = {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move {
            tool(
                &agent,
                "files_read",
                json!({"path": "Invoices/inv-001.txt"}),
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(!call.is_finished());
    h.rt.resume(&h.me, task).unwrap();
    assert!(!call.await.unwrap().is_error);
}

#[tokio::test]
async fn take_over_and_hand_back_summarize_the_users_changes() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let _agent = h.agent(task).await;
    h.rt.take_over(&h.me, task).await.unwrap();
    assert_eq!(h.tile(task), TileState::YouAreDriving);
    fs::write(h.home().join("Reports/by-hand.txt"), "typed by the user").unwrap();
    let view = h.rt.hand_back(&h.me, task).await.unwrap();
    assert_eq!(view.state, TileState::Working);
    assert!(
        view.caption.contains("changed 1 file") && view.caption.contains("by-hand.txt"),
        "{}",
        view.caption
    );
}

#[tokio::test]
async fn approvals_time_out_into_a_paused_task() {
    let h = harness_with("");
    // Rebuild with a one-second timeout.
    let text = glass_policy::DEFAULT_POLICY
        .replace("approval_timeout_secs = 900", "approval_timeout_secs = 1");
    let h = Harness {
        rt: rebuild(&h, &text),
        ..h
    };
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(task).await;
    let result = tool(
        &agent,
        "files_delete",
        json!({"path": "Reports/existing.txt"}),
    )
    .await;
    assert!(result.is_error && result.text().contains("paused"));
    assert_eq!(h.tile(task), TileState::Paused);
    let timed_out = glass_audit::read(&h.audit_path(), |r| {
        r.event.outcome == Some(Outcome::TimedOut)
    })
    .unwrap();
    assert_eq!(timed_out.len(), 1);
}

#[tokio::test]
async fn stopping_tears_down_the_sandbox_and_withdraws_asks() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = Arc::new(h.agent(task).await);
    let call = {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move {
            tool(
                &agent,
                "files_delete",
                json!({"path": "Reports/existing.txt"}),
            )
            .await
        })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    h.rt.stop(&h.me, task).unwrap();
    let result = call.await.unwrap();
    assert!(result.is_error);
    assert!(
        h.rt.approvals(&h.me).is_empty(),
        "asks are withdrawn with the task"
    );
    let root = h
        .socket(task)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    eventually(|| !root.exists()).await;
    assert_eq!(h.tile(task), TileState::Stopped);
    eventually(|| {
        glass_audit::read(&h.audit_path(), |r| {
            r.event.kind == EventKind::TaskFinished && r.event.task == Some(task)
        })
        .map(|r| r.len() == 1)
        .unwrap_or(false)
    })
    .await;
}

/// The Containment model's look before the agent starts: a task stopped
/// while it is being set up never starts its agent, and says only that it
/// stopped (AW-11, ADR 0053).
#[tokio::test]
async fn aw11_a_task_stopped_as_it_starts_never_starts_its_agent() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    // On this test's one thread the supervisor hasn't run yet.
    h.rt.stop(&h.me, task).unwrap();
    eventually(|| {
        glass_audit::read(&h.audit_path(), |r| {
            r.event.kind == EventKind::TaskFinished && r.event.task == Some(task)
        })
        .is_ok_and(|r| r.len() == 1)
    })
    .await;
    assert_eq!(h.tile(task), TileState::Stopped);
    let log = h.dir.path().join(format!("state/tasks/{task}/agent.log"));
    assert!(!log.exists(), "the agent was started");
    let activity = h.rt.get(&h.me, task).unwrap().activity;
    assert!(
        !activity.iter().any(|a| a.text.starts_with("Agent running")),
        "{activity:?}"
    );
}

/// The person's live-task limit holds however many starts land at once:
/// they're counted and inserted under one lock (ADR 0053). A burst of
/// starts, again and again: the race it closed was a few microseconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_live_task_limit_holds_for_starts_at_once() {
    let h = harness();
    for round in 0..20 {
        let starts = 16;
        let barrier = Arc::new(std::sync::Barrier::new(starts));
        let calls: Vec<_> = (0..starts)
            .map(|_| {
                let (rt, me) = (Arc::clone(&h.rt), h.me.clone());
                let request = h.request(&[], &["Reports"]);
                let barrier = Arc::clone(&barrier);
                tokio::task::spawn_blocking(move || {
                    barrier.wait();
                    rt.start(&me, request).map(|view| view.id)
                })
            })
            .collect();
        let mut started = Vec::new();
        for call in calls {
            if let Ok(id) = call.await.unwrap() {
                started.push(id);
            }
        }
        for id in &started {
            let _ = h.rt.stop(&h.me, *id);
        }
        assert_eq!(started.len(), 4, "round {round}: the limit is 4 live tasks");
    }
}

/// Once glassd is shutting down, no task starts: one landing after its
/// stops would run on after glassd had gone (ADR 0053).
#[tokio::test]
async fn pa2_no_task_starts_once_shutting_down() {
    let h = harness();
    h.rt.shutdown().await;
    let refused = h.rt.start(&h.me, h.request(&[], &["Reports"]));
    assert!(
        matches!(&refused, Err(RuntimeError::Conflict(m)) if m.contains("shutting down")),
        "{refused:?}"
    );
}

/// ADR 0064 (EP-1): no agent starts while glassd checks the system's
/// files at its start, nor on a system that differs from its image; a
/// development build, with nothing to check against, runs agents. Each
/// check is on the audit record.
#[tokio::test]
async fn ep1_no_agent_starts_on_a_system_that_differs_or_while_it_is_checked() {
    let h = harness();
    let root = tempfile::tempdir().unwrap();
    h.rt.begin_system_check();
    let refused = h.rt.start(&h.me, h.request(&[], &["Reports"]));
    assert!(
        matches!(&refused, Err(RuntimeError::Conflict(m)) if m.contains("checking its system files")),
        "{refused:?}"
    );
    // An image built for release with no signed list: changed.
    let check = h.rt.check_system(root.path(), true);
    assert_eq!(check.state, glass_api::SystemCheckState::Changed);
    let refused = h.rt.start(&h.me, h.request(&[], &["Reports"]));
    assert!(
        matches!(&refused, Err(RuntimeError::Conflict(m)) if m.contains("has changed since it was signed")),
        "{refused:?}"
    );
    assert_eq!(
        h.rt.status().system_files.map(|c| c.state),
        Some(glass_api::SystemCheckState::Changed)
    );
    // A development build: unchecked, and agents run.
    let check = h.rt.check_system(root.path(), false);
    assert_eq!(check.state, glass_api::SystemCheckState::Unchecked);
    h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap();
    let checks: Vec<String> = glass_audit::read(&h.audit_path(), |r| {
        r.event.kind == EventKind::SystemChecked
    })
    .unwrap()
    .into_iter()
    .map(|r| r.event.summary)
    .collect();
    assert_eq!(checks.len(), 2, "{checks:?}");
    assert!(checks[0].contains("No agent starts"), "{checks:?}");
}

#[tokio::test]
async fn other_users_cannot_see_drive_or_answer_a_task() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = Arc::new(h.agent(task).await);
    let stranger = Caller {
        name: "mallory".into(),
        uid: 4242,
        gid: 4242,
        home: PathBuf::from("/home/mallory"),
    };
    assert!(h.rt.list(&stranger).is_empty());
    assert!(matches!(
        h.rt.get(&stranger, task),
        Err(RuntimeError::NotFound(_))
    ));
    assert!(matches!(
        h.rt.stop(&stranger, task),
        Err(RuntimeError::NotFound(_))
    ));

    let call = {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move {
            tool(
                &agent,
                "files_delete",
                json!({"path": "Reports/existing.txt"}),
            )
            .await
        })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    assert!(h.rt.approvals(&stranger).is_empty());
    let ask = h.rt.approvals(&h.me).remove(0);
    let attempt = h.rt.answer(
        &stranger,
        AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Approve {
                scope: ApprovalScope::Task,
            },
        },
    );
    assert!(matches!(attempt, Err(RuntimeError::NotFound(_))));
    h.rt.answer(
        &h.me,
        AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Deny,
        },
    )
    .unwrap();
    assert!(call.await.unwrap().is_error);
}

#[tokio::test]
async fn agents_pause_for_sleep_and_carry_on_after_wake() {
    let h = harness();
    let system = Caller {
        uid: 0,
        ..h.me.clone()
    };
    let user = Caller {
        uid: 4242,
        ..h.me.clone()
    };
    let task = h.rt.start(&h.me, h.request(&["Invoices"], &[])).unwrap().id;
    let agent = Arc::new(h.agent(task).await);
    tool(
        &agent,
        "report_progress",
        json!({"caption": "Reading invoices"}),
    )
    .await;

    assert!(matches!(
        h.rt.suspend(&user).await,
        Err(RuntimeError::PermissionDenied(_))
    ));
    assert_eq!(h.rt.suspend(&system).await.unwrap().tasks, 1);
    let detail = h.rt.get(&h.me, task).unwrap();
    assert_eq!(detail.view.state, TileState::Paused);
    assert_eq!(detail.view.caption, "Paused for sleep");
    assert!(detail.now.starts_with("Paused for sleep"));

    let call = {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move {
            tool(
                &agent,
                "files_read",
                json!({"path": "Invoices/inv-001.txt"}),
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !call.is_finished(),
        "agents stay held while the machine sleeps"
    );

    assert_eq!(h.rt.wake(&system).unwrap().tasks, 1);
    assert!(!call.await.unwrap().is_error);
    let view = h.rt.get(&h.me, task).unwrap().view;
    assert_eq!(view.state, TileState::Working);
    assert_eq!(view.caption, "Reading invoices", "the caption comes back");
}

#[tokio::test]
async fn sleep_leaves_tasks_the_user_paused_alone() {
    let h = harness();
    let system = Caller {
        uid: 0,
        ..h.me.clone()
    };
    let task = h.rt.start(&h.me, h.request(&["Invoices"], &[])).unwrap().id;
    let _agent = h.agent(task).await;
    h.rt.pause(&h.me, task).unwrap();
    assert_eq!(h.rt.suspend(&system).await.unwrap().tasks, 0);
    assert_eq!(h.rt.wake(&system).unwrap().tasks, 0);
    assert_eq!(
        h.tile(task),
        TileState::Paused,
        "the user's pause outlives the wake"
    );
}

/// PM-5, found by the Approvals model (specs/tla): a task the person stops
/// while it's paused for sleep stays stopped when the machine wakes.
// Audit 3.6: an OS update's image downloaded with the lock on the system's
// state held, up to 16 GiB of it, and waking from sleep takes that lock: a
// computer woken meanwhile kept its agents paused until the download was
// done. (What the download runs in is driven here by its test hook; that
// an update's check and staging are outside it is osupdate's tests.)
#[tokio::test]
async fn pm5_waking_from_sleep_doesnt_wait_for_an_os_update_to_download() {
    let h = harness();
    let system = Caller {
        uid: 0,
        ..h.me.clone()
    };
    let rt = Arc::clone(&h.rt);
    let mut woke = None;
    let mut waker = None;
    h.rt.system_state_let_go_for_tests(|| {
        // The image is on its way. The computer wakes.
        let (tell, told) = std::sync::mpsc::channel();
        waker = Some(std::thread::spawn(move || {
            let _ = tell.send(rt.wake(&system).is_ok());
        }));
        woke = told.recv_timeout(Duration::from_secs(5)).ok();
    });
    // Joined out here: had the wake waited, it waits no longer.
    waker.unwrap().join().unwrap();
    assert_eq!(woke, Some(true), "waking waited for the download to end");
}

#[tokio::test]
async fn pm5_a_task_stopped_while_paused_for_sleep_stays_stopped_after_wake() {
    let h = harness();
    let system = Caller {
        uid: 0,
        ..h.me.clone()
    };
    let task = h.rt.start(&h.me, h.request(&["Invoices"], &[])).unwrap().id;
    let _agent = h.agent(task).await;
    assert_eq!(h.rt.suspend(&system).await.unwrap().tasks, 1);
    h.rt.stop(&h.me, task).unwrap();
    assert_eq!(h.rt.wake(&system).unwrap().tasks, 0, "nothing to wake");
    assert_eq!(h.tile(task), TileState::Stopped);
}

const REPEATER: &str = r#"
[[group]]
name = "repeaters"
members = ["{me}"]
agents = ["repeater"]

[[agent]]
id = "repeater"
allowed_actions = ["files.*"]
"#;

/// PM-5, found by the Approvals model: an agent that finishes while its
/// task is paused for sleep leaves it Done; the wake doesn't start it again.
#[tokio::test]
async fn pm5_a_task_that_finished_while_paused_for_sleep_stays_done_after_wake() {
    let h = harness_with(REPEATER);
    let system = Caller {
        uid: 0,
        ..h.me.clone()
    };
    let mut request = h.request(&["Invoices"], &[]);
    request.agent = "repeater".parse().unwrap();
    let task = h.rt.start(&h.me, request).unwrap().id;
    let _agent = h.agent(task).await;
    assert_eq!(h.rt.suspend(&system).await.unwrap().tasks, 1);
    // Its agent finishes on its own, three seconds in.
    eventually(|| h.tile(task) == TileState::Done).await;
    assert_eq!(h.rt.wake(&system).unwrap().tasks, 0, "nothing to wake");
    assert_eq!(h.tile(task), TileState::Done);
}

/// PM-5, found by the Approvals model: a resume that lands while the
/// machine is going to sleep carries on after the wake, not before it.
#[tokio::test]
async fn pm5_a_resume_while_going_to_sleep_waits_for_the_wake() {
    let h = harness();
    let system = Caller {
        uid: 0,
        ..h.me.clone()
    };
    let task = h.rt.start(&h.me, h.request(&["Invoices"], &[])).unwrap().id;
    let agent = Arc::new(h.agent(task).await);
    h.rt.pause(&h.me, task).unwrap();
    assert_eq!(h.rt.suspend(&system).await.unwrap().tasks, 0);
    h.rt.resume(&h.me, task).unwrap();
    assert_eq!(h.tile(task), TileState::Paused);
    let call = {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move {
            tool(
                &agent,
                "files_read",
                json!({"path": "Invoices/inv-001.txt"}),
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(!call.is_finished(), "nothing runs until the wake");
    assert_eq!(h.rt.wake(&system).unwrap().tasks, 1);
    assert!(!call.await.unwrap().is_error);
    assert_eq!(h.tile(task), TileState::Working);
}

/// AW-12, found by the Approvals model: while one of a task's asks is still
/// open, answering another leaves the tile at Needs you.
#[tokio::test]
async fn aw12_a_task_still_needs_you_while_another_ask_is_open() {
    let h = harness();
    fs::write(h.home().join("Reports/other.txt"), "another report\n").unwrap();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = Arc::new(h.agent(task).await);
    let delete = |path: &'static str| {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move { tool(&agent, "files_delete", json!({ "path": path })).await })
    };
    let first = delete("Reports/existing.txt");
    eventually(|| h.rt.approvals(&h.me).len() == 1).await;
    let second = delete("Reports/other.txt");
    eventually(|| h.rt.approvals(&h.me).len() == 2).await;
    assert_eq!(h.tile(task), TileState::NeedsYou);
    let asks = h.rt.approvals(&h.me);
    let answer = |id, answer| {
        h.rt.answer(
            &h.me,
            AnswerApproval {
                approval: id,
                answer,
            },
        )
        .unwrap()
    };
    answer(asks[0].id, ApprovalAnswer::Deny);
    assert!(first.await.unwrap().is_error);
    assert_eq!(
        h.tile(task),
        TileState::NeedsYou,
        "the other ask is still open"
    );
    answer(
        asks[1].id,
        ApprovalAnswer::Approve {
            scope: ApprovalScope::Once,
        },
    );
    assert!(!second.await.unwrap().is_error);
    assert_eq!(h.tile(task), TileState::Working);
}

#[tokio::test]
async fn feed_lines_carry_verb_and_target() {
    let h = harness();
    let task =
        h.rt.start(&h.me, h.request(&["Invoices"], &["Reports"]))
            .unwrap()
            .id;
    let agent = h.agent(task).await;
    tool(
        &agent,
        "files_write",
        json!({"path": "Reports/a.csv", "content": "x"}),
    )
    .await;
    let view = h.rt.get(&h.me, task).unwrap().view;
    let line = view.feed.last().unwrap();
    assert_eq!(
        (line.verb.as_str(), line.target.as_str()),
        ("write", "Reports/a.csv")
    );
    let card = h.rt.plan(&h.me, &h.request(&["Invoices"], &[])).unwrap();
    assert_eq!(card.agent.version, "0.1");
    assert_eq!(card.max_label, Some(glass_types::Label::Internal));
    assert_eq!(card.model, "No model (scripted agent)");
}

/// A task's title names it in lists like the command bar's Recent (DA-1):
/// given with the request, or made from the prompt. Retry keeps it.
#[tokio::test]
async fn da1_tasks_are_titled_from_the_request_or_the_prompt() {
    let h = harness();
    let made = h.rt.start(&h.me, h.request(&["Invoices"], &[])).unwrap();
    assert_eq!(made.title, "Reconcile this month's invoices");

    let mut request = h.request(&["Invoices"], &[]);
    request.title = Some("  October close ".into());
    let given = h.rt.start(&h.me, request.clone()).unwrap();
    assert_eq!(given.title, "October close");
    assert_eq!(
        h.rt.get(&h.me, given.id).unwrap().view.title,
        "October close"
    );
    let started = glass_audit::read(&h.audit_path(), |r| {
        r.event.kind == EventKind::TaskStarted && r.event.task == Some(given.id)
    })
    .unwrap();
    assert_eq!(started[0].event.detail["title"], "October close");

    for bad in ["October\nclose", "", &"x".repeat(TITLE_MAX_CHARS + 1)] {
        request.title = Some(bad.into());
        assert!(
            matches!(h.rt.plan(&h.me, &request), Err(RuntimeError::Invalid(_))),
            "{bad:?}"
        );
    }

    h.rt.stop(&h.me, made.id).unwrap();
    eventually(|| !h.rt.get(&h.me, made.id).unwrap().view.state.is_live()).await;
    assert_eq!(h.rt.retry(&h.me, made.id).unwrap().title, made.title);
}

/// The agent may rename its task once, with `report_progress`, unless the
/// user named it; a refused rename changes nothing, and a rename is logged
/// (DA-1).
#[tokio::test]
async fn da1_agents_may_rename_their_task_once() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&["Invoices"], &[])).unwrap().id;
    let agent = h.agent(task).await;
    let progress = |caption: &str, title: &str| json!({ "caption": caption, "title": title });

    let bad = tool(&agent, "report_progress", progress("Reading", "Two\nlines")).await;
    assert!(
        bad.is_error && bad.text().contains("Nothing changed"),
        "{}",
        bad.text()
    );
    assert_ne!(h.rt.get(&h.me, task).unwrap().view.caption, "Reading");

    let renamed = tool(
        &agent,
        "report_progress",
        progress("Reading invoices", "October invoices"),
    )
    .await;
    assert!(!renamed.is_error, "{}", renamed.text());
    let detail = h.rt.get(&h.me, task).unwrap();
    assert_eq!(detail.view.title, "October invoices");
    assert_eq!(detail.view.caption, "Reading invoices");
    assert!(
        detail
            .activity
            .iter()
            .any(|a| a.text == "Renamed \"October invoices\"")
    );
    let logged = glass_audit::read(&h.audit_path(), |r| {
        r.event.kind == EventKind::TaskStateChanged && r.event.task == Some(task)
    })
    .unwrap();
    let rename = &logged.last().unwrap().event.detail;
    assert_eq!(rename["title"], "October invoices");
    assert_eq!(rename["was"], "Reconcile this month's invoices");

    let again = tool(&agent, "report_progress", progress("Matching", "Mine now")).await;
    assert!(
        again.is_error && again.text().contains("can't change again"),
        "{}",
        again.text()
    );
    let view = h.rt.get(&h.me, task).unwrap().view;
    assert_eq!(
        (view.title.as_str(), view.caption.as_str()),
        ("October invoices", "Reading invoices")
    );
    let plain = tool(&agent, "report_progress", json!({"caption": "Matching"})).await;
    assert!(!plain.is_error);

    let mut request = h.request(&["Invoices"], &[]);
    request.title = Some("October close".into());
    let named = h.rt.start(&h.me, request).unwrap().id;
    let agent = h.agent(named).await;
    let refused = tool(&agent, "report_progress", progress("Reading", "Mine now")).await;
    assert!(refused.is_error);
    assert_eq!(h.rt.get(&h.me, named).unwrap().view.title, "October close");
}

/// An ask reads as what the action does: its own answer labels, or Review
/// and Approve when it declares none (DA-12).
#[tokio::test]
async fn da12_asks_carry_the_actions_answer_labels() {
    let h = harness();
    let mut request = h.request(&["Invoices"], &["Reports"]);
    request
        .scope
        .restrict
        .push(("files.read".into(), Tier::Ask));
    let task = h.rt.start(&h.me, request).unwrap().id;
    let agent = Arc::new(h.agent(task).await);

    for (tool_name, args, labels) in [
        (
            "mail_send",
            send_args(2),
            AskLabels::new("Review drafts", "Approve and send"),
        ),
        (
            "files_read",
            json!({"path": "Invoices/inv-003.txt"}),
            AskLabels::default(),
        ),
    ] {
        let call = {
            let agent = Arc::clone(&agent);
            tokio::spawn(async move { tool(&agent, tool_name, args).await })
        };
        eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
        let ask = h.rt.approvals(&h.me).remove(0);
        assert_eq!(ask.labels, labels, "{tool_name}");
        let tile = h.rt.list(&h.me).remove(0);
        assert_eq!(tile.needs.unwrap().labels, labels);
        h.rt.answer(
            &h.me,
            AnswerApproval {
                approval: ask.id,
                answer: ApprovalAnswer::Deny,
            },
        )
        .unwrap();
        assert!(call.await.unwrap().is_error);
    }
}

/// What an agent writes reaches tiles, the expanded view, asks and undo
/// reports as one line of plain text (AW-3, AW-4, AW-13, DA-12): its
/// caption, its file names and its mail can't clear the user's terminal,
/// hide a line or reorder what they read (PA-3). The audit log keeps what
/// the agent sent as it was.
#[tokio::test]
async fn aw4_agent_text_reaches_every_view_as_plain_lines() {
    const CLEAR: &str = "\x1b[2J";
    const OVERRIDE: char = '\u{202E}';
    // Serialized, ESC is "\u001b" and CR is "\r"; the override stays raw.
    let assert_plain = |what: &str, json: String| {
        assert!(
            !json.contains("\\u001b") && !json.contains("\\r") && !json.contains(OVERRIDE),
            "{what}: {json}"
        );
    };
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = Arc::new(h.agent(task).await);

    let name = format!("Reports/report{CLEAR}{OVERRIDE}vsc.txt");
    let written = tool(&agent, "files_write", json!({"path": name, "content": "x"})).await;
    assert!(!written.is_error, "{}", written.text());
    let caption = format!("Summarizing{CLEAR}\rAll approved{OVERRIDE}");
    let progress = tool(&agent, "report_progress", json!({ "caption": caption })).await;
    assert!(!progress.is_error);

    let send = {
        let agent = Arc::clone(&agent);
        let mail = json!({ "messages": [{
            "to": [format!("ap{CLEAR}@example.com")],
            "subject": format!("Paid\x1b]8;;https://evil.example\x1b\\{OVERRIDE}"),
            "body": "Done."
        }]});
        tokio::spawn(async move { tool(&agent, "mail_send", mail).await })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    assert_eq!(
        ask.summary,
        "Send email: Paid\u{FFFD}]8;;https://evil.example\u{FFFD}\\\u{FFFD}"
    );
    assert_plain("ask", serde_json::to_string(&ask).unwrap());
    let tile = h.rt.list(&h.me).remove(0);
    assert_eq!(tile.caption, "Summarizing\u{FFFD}[2J All approved\u{FFFD}");
    assert_eq!(
        tile.feed[0].target,
        "Reports/report\u{FFFD}[2J\u{FFFD}vsc.txt"
    );
    assert!(tile.needs.is_some());
    assert_plain("tile", serde_json::to_string(&tile).unwrap());
    h.rt.answer(
        &h.me,
        AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Deny,
        },
    )
    .unwrap();
    assert!(send.await.unwrap().is_error);

    let detail = h.rt.get(&h.me, task).unwrap();
    assert_eq!(
        detail.changes[0].target,
        "Reports/report\u{FFFD}[2J\u{FFFD}vsc.txt"
    );
    assert!(
        detail
            .activity
            .iter()
            .any(|a| a.text.starts_with("Declined: Send email: Paid"))
    );
    assert_plain("detail", serde_json::to_string(&detail).unwrap());
    let report =
        h.rt.undo(
            &h.me,
            UndoRequest {
                task,
                to: None,
                changes: Vec::new(),
                dry_run: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        report.removed,
        ["~/Reports/report\u{FFFD}[2J\u{FFFD}vsc.txt"]
    );
    assert_plain("undo", serde_json::to_string(&report).unwrap());

    // The record is history: it keeps exactly what the agent sent.
    let wrote = glass_audit::read(&h.audit_path(), |r| {
        r.event.task == Some(task)
            && r.event
                .action
                .as_ref()
                .is_some_and(|a| a.as_str() == "files.write")
    })
    .unwrap();
    assert_eq!(
        wrote[0].event.targets,
        [format!("Reports/report{CLEAR}{OVERRIDE}vsc.txt")]
    );
}

#[tokio::test]
async fn undoing_single_changes_leaves_the_rest() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(task).await;
    for (path, content) in [
        ("Reports/one.txt", "1"),
        ("Reports/two.txt", "2"),
        ("Reports/existing.txt", "clobbered"),
    ] {
        let args = json!({"path": path, "content": content});
        assert!(!tool(&agent, "files_write", args).await.is_error);
    }
    let changes = h.rt.get(&h.me, task).unwrap().changes;
    assert_eq!(changes.iter().map(|c| c.id).collect::<Vec<_>>(), [0, 1, 2]);
    assert_eq!(changes[2].paths, ["~/Reports/existing.txt"]);

    let undo = |changes: Vec<u32>, dry_run| UndoRequest {
        task,
        to: None,
        changes,
        dry_run,
    };
    let preview = h.rt.undo(&h.me, undo(vec![1, 2], true)).await.unwrap();
    assert_eq!(preview.removed, ["~/Reports/two.txt"]);
    assert_eq!(preview.restored, ["~/Reports/existing.txt"]);
    let report = h.rt.undo(&h.me, undo(vec![1, 2], false)).await.unwrap();
    assert_eq!(report.removed, preview.removed);
    assert!(h.home().join("Reports/one.txt").exists(), "the rest stays");
    assert!(!h.home().join("Reports/two.txt").exists());
    assert_eq!(
        fs::read_to_string(h.home().join("Reports/existing.txt")).unwrap(),
        "original report\n"
    );
    let undone: Vec<bool> =
        h.rt.get(&h.me, task)
            .unwrap()
            .changes
            .iter()
            .map(|c| c.undone)
            .collect();
    assert_eq!(undone, [false, true, true]);
    assert!(matches!(
        h.rt.undo(&h.me, undo(vec![9], true)).await,
        Err(RuntimeError::NotFound(_))
    ));
}

#[tokio::test]
async fn an_ended_task_says_what_is_safe_and_can_be_retried() {
    let h = harness();
    let request = h.request(&["Invoices"], &["Reports"]);
    let first = h.rt.start(&h.me, request.clone()).unwrap().id;
    assert!(
        matches!(h.rt.retry(&h.me, first), Err(RuntimeError::Conflict(_))),
        "a running task can't be retried"
    );
    let agent = h.agent(first).await;
    let args = json!({"path": "Reports/one.txt", "content": "1"});
    assert!(!tool(&agent, "files_write", args).await.is_error);
    h.rt.stop(&h.me, first).unwrap();
    eventually(|| h.rt.get(&h.me, first).unwrap().view.receipt.is_some()).await;

    // The Failed and Stopped tiles say what's safe (Calm mockup).
    let receipt = h.rt.get(&h.me, first).unwrap().view.receipt.unwrap();
    assert_eq!((receipt.undoable, receipt.left_machine), (1, 0));
    assert_eq!(
        receipt.outcome(),
        "Nothing left the machine. 1 change can be undone."
    );

    let stranger = Caller {
        name: "mallory".into(),
        uid: 4242,
        gid: 4242,
        home: PathBuf::from("/home/mallory"),
    };
    assert!(matches!(
        h.rt.retry(&stranger, first),
        Err(RuntimeError::NotFound(_))
    ));

    let second = h.rt.retry(&h.me, first).unwrap().id;
    assert_ne!(second, first);
    let detail = h.rt.get(&h.me, second).unwrap();
    assert_eq!(detail.prompt, request.prompt);
    let note = format!("retry of {}", first.short());
    assert!(detail.activity.iter().any(|a| a.text.contains(&note)));
    // Same scope as the first run: Reports is writable again.
    let agent = h.agent(second).await;
    let args = json!({"path": "Reports/two.txt", "content": "2"});
    assert!(!tool(&agent, "files_write", args).await.is_error);
    let started = glass_audit::read(&h.audit_path(), |r| {
        r.event.kind == EventKind::TaskStarted && r.event.task == Some(second)
    })
    .unwrap();
    assert_eq!(started[0].event.detail["retry_of"], json!(first));
}

#[tokio::test]
async fn only_the_it_agent_reaches_the_system_and_its_fixes_stay_undoable() {
    // AE-4
    let h = harness();
    let other = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(other).await;
    let tools: ListToolsResult = agent.call("tools/list", json!({})).await.unwrap();
    assert!(!tools.tools.iter().any(|t| t.name.starts_with("system_")));
    assert!(
        tool(&agent, "system_storage_clean_cache", json!({}))
            .await
            .is_error
    );

    fs::create_dir_all(h.home().join(".cache/thumbs")).unwrap();
    fs::write(h.home().join(".cache/thumbs/a.png"), vec![0u8; 4096]).unwrap();
    let request = TaskRequest {
        persona: None,
        agent: "it-agent".parse().unwrap(),
        prompt: "My disk is full".into(),
        scope: ScopeRequest::default(),
        title: None,
        computer: None,
    };
    let task = h.rt.start(&h.me, request).unwrap().id;
    let agent = Arc::new(h.agent(task).await);
    let checked = tool(&agent, "system_storage_check", json!({})).await;
    assert!(checked.text().contains("95% full"), "{}", checked.text());

    // Notify: runs without asking, snapshotted first.
    let cleaned = tool(&agent, "system_storage_clean_cache", json!({})).await;
    assert!(!cleaned.is_error, "{}", cleaned.text());
    assert!(h.rt.approvals(&h.me).is_empty());
    assert!(!h.home().join(".cache/thumbs").exists());
    let change = h.rt.get(&h.me, task).unwrap().changes.remove(0);
    assert_eq!(change.paths, ["~/.cache"]);
    h.rt.undo(
        &h.me,
        UndoRequest {
            task,
            to: None,
            changes: vec![change.id],
            dry_run: false,
        },
    )
    .await
    .unwrap();
    assert!(
        h.home().join(".cache/thumbs/a.png").exists(),
        "undo puts the cache back"
    );

    // A restart from IT's list asks first.
    h.rt.resume(&h.me, task).unwrap();
    let restart = {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move {
            tool(&agent, "system_service_restart", json!({"service": "ntpd"})).await
        })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    assert_eq!(ask.summary, "Restart the ntpd service");
    h.rt.answer(
        &h.me,
        AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Approve {
                scope: ApprovalScope::Once,
            },
        },
    )
    .unwrap();
    assert!(!restart.await.unwrap().is_error);
    assert_eq!(*h.machine.restarted.lock().unwrap(), ["ntpd"]);
}

/// The user's own Claude Code (attended, ADR 0015), connected and
/// initialized, on a task with no folders: calendar actions need none.
async fn harness_agent(h: &Harness) -> (glass_types::TaskId, Arc<glass_rpc::Client>) {
    let mut request = h.request(&[], &[]);
    request.agent = "claude-code".parse().unwrap();
    let view = h.rt.start(&h.me, request).unwrap();
    let socket = view.driver_socket.unwrap();
    eventually(|| socket.exists()).await;
    let client = glass_rpc::Client::connect(&socket).await.unwrap();
    let _: Value = client
        .call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}))
        .await
        .unwrap();
    (view.id, Arc::new(client))
}

#[tokio::test]
async fn nv3_calendar_holds_notify_invitations_ask_and_undo_takes_the_hold_back() {
    let h = harness();
    let (task, agent) = harness_agent(&h).await;

    // A hold runs without asking and shows in the feed (Notify), after a
    // snapshot that makes it undoable.
    let held = tool(
        &agent,
        "calendar_hold",
        json!({ "title": "Prep for Northwind", "start": "2026-10-20T14:00", "end": "2026-10-20T15:00" }),
    )
    .await;
    assert!(!held.is_error, "{}", held.text());
    let id = held.structured_content.as_ref().unwrap()["event"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let file = h.home().join(format!("Calendar/Events/{id}.json"));
    assert!(file.exists());
    assert!(h.rt.approvals(&h.me).is_empty());
    let detail = h.rt.get(&h.me, task).unwrap();
    assert_eq!(detail.view.feed.last().unwrap().verb, "hold");
    let records = glass_audit::read(&h.audit_path(), |r| {
        r.event.kind == glass_audit::EventKind::Action
            && r.event.action.as_ref().map(|a| a.as_str()) == Some("calendar.hold")
    })
    .unwrap();
    let decision = records[0].event.decision.as_ref().unwrap();
    assert_eq!(decision.tier, Tier::Notify);
    assert!(records[0].event.snapshot.is_some());

    // Invitations leave the machine: one ask with its own words, and a
    // decline sends nothing.
    let invite = {
        let agent = Arc::clone(&agent);
        let input = json!({ "id": id, "to": ["dana@northwind.example"] });
        tokio::spawn(async move { tool(&agent, "calendar_invite", input).await })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    assert_eq!(ask.summary, "Send 1 invitation: Prep for Northwind");
    assert_eq!(ask.labels.approve, "Approve and send");
    h.rt.answer(
        &h.me,
        AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Deny,
        },
    )
    .unwrap();
    assert!(invite.await.unwrap().is_error);
    assert_eq!(
        fs::read_dir(h.home().join("Calendar/Sent"))
            .unwrap()
            .count(),
        0
    );

    // Undo takes the hold back off the calendar (AU-21).
    let report =
        h.rt.undo(
            &h.me,
            UndoRequest {
                task,
                to: None,
                changes: vec![],
                dry_run: false,
            },
        )
        .await
        .unwrap();
    assert!(!file.exists(), "{report:?}");
}

/// The CalendarInvite model (ADR 0052): what leaves is what the person
/// approved. An event moved while they decide on its invitation is not
/// sent at the new time: the call is refused, in words, and nothing leaves.
#[tokio::test(flavor = "multi_thread")]
async fn da11_an_invitation_whose_event_changed_while_the_person_decided_is_not_sent() {
    let h = harness();
    let (_task, agent) = harness_agent(&h).await;
    let held = tool(
        &agent,
        "calendar_hold",
        json!({ "title": "Prep for Northwind", "start": "2026-10-20T14:00", "end": "2026-10-20T15:00" }),
    )
    .await;
    let id = held.structured_content.as_ref().unwrap()["event"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let invite = {
        let agent = Arc::clone(&agent);
        let input = json!({ "id": id, "to": ["dana@northwind.example"] });
        tokio::spawn(async move { tool(&agent, "calendar_invite", input).await })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    assert!(
        ask.items.iter().any(|i| i.contains("14:00")),
        "{:?}",
        ask.items
    );
    // Moved to the next morning while the ask waits (Notify: no ask).
    let moved = tool(
        &agent,
        "calendar_update",
        json!({ "id": id, "start": "2026-10-21T09:00", "end": "2026-10-21T10:00" }),
    )
    .await;
    assert!(!moved.is_error, "{}", moved.text());
    h.rt.answer(
        &h.me,
        AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Approve {
                scope: ApprovalScope::Once,
            },
        },
    )
    .unwrap();
    let result = invite.await.unwrap();
    assert!(result.is_error, "{}", result.text());
    assert!(result.text().contains("changed while"), "{}", result.text());
    assert_eq!(
        fs::read_dir(h.home().join("Calendar/Sent"))
            .unwrap()
            .count(),
        0
    );

    // Asked again, as it is now, it goes.
    let again = {
        let agent = Arc::clone(&agent);
        let input = json!({ "id": id, "to": ["dana@northwind.example"] });
        tokio::spawn(async move { tool(&agent, "calendar_invite", input).await })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    h.rt.answer(
        &h.me,
        AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Approve {
                scope: ApprovalScope::Once,
            },
        },
    )
    .unwrap();
    let sent = again.await.unwrap();
    assert!(!sent.is_error, "{}", sent.text());
    let ics = fs::read_dir(h.home().join("Calendar/Sent"))
        .unwrap()
        .map(|e| fs::read_to_string(e.unwrap().path()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ics.len(), 1);
    assert!(ics[0].contains("DTSTART:20261021T090000"), "{}", ics[0]);
}
