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

use glass_actions::Registry;
use glass_actions::providers::{FilesProvider, MailProvider};
use glass_api::{
    AnswerApproval, ApprovalAnswer, ApprovalScope, ScopeRequest, TaskRequest, TileState,
    UndoRequest,
};
use glass_audit::{AuditLog, Durability, EventKind, Outcome};
use glass_platform::sandbox::HostSandbox;
use glass_platform::snapshot::CopySnapshotter;
use glass_policy::Policy;
use glass_rpc::Client;
use glass_rpc::mcp::{CallToolResult, ListToolsResult};
use glass_runtime::{Caller, CatalogEntry, Runtime, RuntimeConfig, RuntimeError, RuntimeParts};
use glass_types::{TaskId, Tier};
use serde_json::{Value, json};

struct Harness {
    dir: tempfile::TempDir,
    rt: Arc<Runtime>,
    me: Caller,
}

impl Harness {
    fn home(&self) -> PathBuf {
        self.me.home.clone()
    }

    fn audit_path(&self) -> PathBuf {
        self.dir.path().join("audit.log")
    }

    fn socket(&self, task: TaskId) -> PathBuf {
        self.dir
            .path()
            .join("state/sandboxes")
            .join(format!("glass-{}", task.short()))
            .join("run/glasshouse/agent.sock")
    }

    fn request(&self, read: &[&str], write: &[&str]) -> TaskRequest {
        TaskRequest {
            agent: "invoice-demo".parse().unwrap(),
            prompt: "Reconcile this month's invoices and remind vendors".into(),
            scope: ScopeRequest {
                read: read.iter().map(|p| self.home().join(p)).collect(),
                write: write.iter().map(|p| self.home().join(p)).collect(),
                restrict: Vec::new(),
            },
        }
    }

    /// Connects as the agent and completes the MCP handshake.
    async fn agent(&self, task: TaskId) -> Client {
        let socket = self.socket(task);
        eventually(|| socket.exists()).await;
        let client = Client::connect(&socket).await.unwrap();
        let init: Value = client
            .call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}))
            .await
            .unwrap();
        assert!(init["instructions"].as_str().unwrap().contains("Reconcile"));
        client
    }

    fn tile(&self, task: TaskId) -> TileState {
        self.rt.get(&self.me, task).unwrap().view.state
    }
}

async fn tool(client: &Client, name: &str, arguments: Value) -> CallToolResult {
    client
        .call(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
        .unwrap()
}

/// Polls until `check` holds, failing after five seconds.
async fn eventually(mut check: impl FnMut() -> bool) {
    for _ in 0..500 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition never became true");
}

fn harness_with(policy_extra: &str) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let meta = fs::metadata(dir.path()).unwrap();
    let account = glass_platform::users::by_uid(meta.uid()).expect("test user resolves");
    let home = dir.path().join("home");
    for folder in ["Invoices", "Reports", "Finance", "Private"] {
        fs::create_dir_all(home.join(folder)).unwrap();
    }
    for (i, amount) in ["120.00", "75.50", "310.25"].iter().enumerate() {
        fs::write(
            home.join(format!("Invoices/inv-00{}.txt", i + 1)),
            format!("Vendor {i}\nAmount: {amount}\n"),
        )
        .unwrap();
    }
    fs::write(home.join("Reports/existing.txt"), "original report\n").unwrap();
    fs::write(home.join("Finance/q3.csv"), "secret numbers\n").unwrap();

    let policy_text = format!(
        "{}\n[[label]]\npath = \"{}/Finance\"\nlabel = \"confidential\"\n{policy_extra}",
        glass_policy::DEFAULT_POLICY,
        home.display()
    );
    let policy = Policy::from_toml(&policy_text).unwrap();
    let mut registry = Registry::new();
    registry.register(Arc::new(FilesProvider)).unwrap();
    registry.register(Arc::new(MailProvider)).unwrap();

    let rt = Runtime::new(RuntimeParts {
        config: RuntimeConfig {
            state_dir: dir.path().join("state"),
            catalog: vec![CatalogEntry {
                id: "invoice-demo".parse().unwrap(),
                name: "Invoice demo".into(),
                version: "0.1".into(),
                model: String::new(),
                description: String::new(),
                mode: glass_api::AgentMode::Actions,
                program: "/bin/sleep".into(),
                args: vec!["30".into()],
                plan: vec![
                    "Read invoices".into(),
                    "Write the summary".into(),
                    "Remind vendors".into(),
                ],
            }],
            agent_dirs: Vec::new(),
            max_live_tasks_per_user: 4,
        },
        policy,
        policy_source: "test".into(),
        registry,
        sandbox: Arc::new(HostSandbox),
        snapshots: Arc::new(CopySnapshotter::new(dir.path().join("snapshots"))),
        audit: Arc::new(
            AuditLog::open(dir.path().join("audit.log"), Durability::Buffered).unwrap(),
        ),
    });
    let me = Caller {
        name: account.name,
        uid: account.uid,
        gid: account.gid,
        home,
    };
    Harness { dir, rt, me }
}

fn harness() -> Harness {
    harness_with("")
}

fn send_args(n: usize) -> Value {
    json!({ "messages": (0..n).map(|i| json!({
        "to": [format!("vendor{i}@example.com")], "subject": format!("Invoice {i} reminder"), "body": "Please pay."
    })).collect::<Vec<_>>() })
}

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

fn rebuild(h: &Harness, policy: &str) -> Arc<Runtime> {
    let mut registry = Registry::new();
    registry.register(Arc::new(FilesProvider)).unwrap();
    registry.register(Arc::new(MailProvider)).unwrap();
    Runtime::new(RuntimeParts {
        config: RuntimeConfig {
            state_dir: h.dir.path().join("state"),
            catalog: vec![CatalogEntry {
                id: "invoice-demo".parse().unwrap(),
                name: "Invoice demo".into(),
                version: "0.1".into(),
                model: String::new(),
                description: String::new(),
                mode: glass_api::AgentMode::Actions,
                program: "/bin/sleep".into(),
                args: vec!["30".into()],
                plan: Vec::new(),
            }],
            agent_dirs: Vec::new(),
            max_live_tasks_per_user: 4,
        },
        policy: Policy::from_toml(policy).unwrap(),
        policy_source: "test".into(),
        registry,
        sandbox: Arc::new(HostSandbox),
        snapshots: Arc::new(CopySnapshotter::new(h.dir.path().join("snapshots"))),
        audit: Arc::new(AuditLog::open(h.audit_path(), Durability::Buffered).unwrap()),
    })
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
