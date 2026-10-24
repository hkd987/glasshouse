//! Undo puts back what the task changed and nothing else (ADR 0017): not the
//! user's own work in the same folders, not another task's, and not a file
//! someone changed after the task, which it leaves and names in the report.
//! The test plays the agent on the task socket, as in `pipeline.rs`.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use glass_actions::{
    ActionDef, ActionError, ActionOutput, ActionProvider, Change, Preview, Registry, TaskContext,
};
use glass_api::{ChangeKind, ScopeRequest, TaskRequest, UndoReport, UndoRequest};
use glass_app_files::FilesProvider;
use glass_platform::fs::ConfinedDir;
use glass_policy::{DEFAULT_POLICY, Policy};
use glass_rpc::Client;
use glass_types::{ActionName, SideEffect, TaskId, Tier};
use serde_json::{Value, json};

use crate::common::{Harness, eventually, harness, tool};

const CHANGED_SINCE: &str = "changed since this task, so it was left as it is";

async fn write(agent: &Client, path: &str, content: &str) {
    let done = tool(
        agent,
        "files_write",
        json!({"path": path, "content": content}),
    )
    .await;
    assert!(!done.is_error, "{}", done.text());
}

async fn draft(agent: &Client, subject: &str) {
    let args = json!({"to": ["a@example.com"], "subject": subject, "body": "x"});
    assert!(!tool(agent, "mail_draft", args).await.is_error);
}

async fn undo(h: &Harness, task: TaskId, changes: Vec<u32>, dry_run: bool) -> UndoReport {
    let request = UndoRequest {
        task,
        to: None,
        changes,
        dry_run,
    };
    h.rt.undo(&h.me, request).await.unwrap()
}

fn read(h: &Harness, path: &str) -> String {
    fs::read_to_string(h.home().join(path)).unwrap()
}

fn undone(h: &Harness, task: TaskId) -> Vec<bool> {
    let changes = h.rt.get(&h.me, task).unwrap().changes;
    changes.iter().map(|c| c.undone).collect()
}

#[tokio::test]
async fn undo_all_leaves_the_users_own_work_in_the_same_folder_alone() {
    // AU-21
    let h = harness();
    fs::write(h.home().join("Reports/notes.txt"), "my notes\n").unwrap();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(task).await;
    write(&agent, "Reports/new.csv", "a").await;
    write(&agent, "Reports/existing.txt", "clobbered").await;
    // The user carries on in the same folder while the task runs.
    fs::write(h.home().join("Reports/mine.txt"), "mine\n").unwrap();
    fs::write(h.home().join("Reports/notes.txt"), "my notes, edited\n").unwrap();

    let report = undo(&h, task, vec![], false).await;
    assert_eq!(report.restored, ["~/Reports/existing.txt"]);
    assert_eq!(report.removed, ["~/Reports/new.csv"]);
    assert!(report.not_undone.is_empty(), "{:?}", report.not_undone);
    assert_eq!(read(&h, "Reports/existing.txt"), "original report\n");
    assert_eq!(read(&h, "Reports/mine.txt"), "mine\n");
    assert_eq!(read(&h, "Reports/notes.txt"), "my notes, edited\n");
}

#[tokio::test]
async fn undoing_one_task_leaves_another_tasks_work_alone() {
    // AU-21
    let h = harness();
    let a = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let b = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let (agent_a, agent_b) = (h.agent(a).await, h.agent(b).await);
    draft(&agent_a, "From A").await;
    draft(&agent_b, "From B").await;
    write(&agent_a, "Reports/shared.csv", "a\n").await;
    write(&agent_b, "Reports/shared.csv", "b\n").await;
    let drafts = || fs::read_dir(h.home().join("Mail/Drafts")).unwrap().count();
    let draft_of = |task| h.rt.get(&h.me, task).unwrap().changes[0].paths.clone();

    // A's draft goes; B's stays, and so does B's version of the file A made.
    let report = undo(&h, a, vec![], false).await;
    assert_eq!(report.removed, draft_of(a));
    assert_eq!(drafts(), 1);
    assert_eq!(
        report.not_undone,
        [format!("~/Reports/shared.csv — {CHANGED_SINCE}")]
    );
    assert_eq!(read(&h, "Reports/shared.csv"), "b\n");
    assert_eq!(
        undone(&h, a),
        [true, false],
        "the file's change stays undoable"
    );

    // Undoing B puts A's version back, which makes it A's to undo again.
    let report = undo(&h, b, vec![], false).await;
    assert_eq!(report.restored, ["~/Reports/shared.csv"]);
    assert_eq!(report.removed, draft_of(b));
    assert_eq!(read(&h, "Reports/shared.csv"), "a\n");
    let report = undo(&h, a, vec![], false).await;
    assert_eq!(report.removed, ["~/Reports/shared.csv"]);
    assert!(!h.home().join("Reports/shared.csv").exists());
    assert_eq!(drafts(), 0);
}

#[tokio::test]
async fn rolling_back_to_a_marker_removes_drafts_saved_after_it() {
    // AW-15: the second draft takes no snapshot of its own (Auto, and the
    // drafts folder is already covered); the earlier one still holds it.
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(task).await;
    write(&agent, "Reports/one.txt", "1").await;
    draft(&agent, "First").await;
    write(&agent, "Reports/two.txt", "2").await;
    draft(&agent, "Second").await;
    let detail = h.rt.get(&h.me, task).unwrap();
    // [task started, before one.txt, before the first draft, before two.txt]
    assert_eq!(detail.snapshots.len(), 4);
    let (first, second) = (&detail.changes[1].paths[0], &detail.changes[3].paths[0]);

    let request = UndoRequest {
        task,
        to: Some(detail.snapshots[3].id),
        changes: vec![],
        dry_run: false,
    };
    let report = h.rt.undo(&h.me, request).await.unwrap();
    assert_eq!(report.removed, [second.as_str(), "~/Reports/two.txt"]);
    assert!(h.home().join(first.trim_start_matches("~/")).exists());
    assert!(h.home().join("Reports/one.txt").exists());
    assert_eq!(undone(&h, task), [false, false, true, true]);
}

#[tokio::test]
async fn edits_made_while_driving_are_changes_you_can_undo() {
    // AW-21, AU-21
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(task).await;
    write(&agent, "Reports/agent.txt", "by the agent").await;
    h.rt.take_over(&h.me, task).await.unwrap();
    fs::write(h.home().join("Reports/by-hand.txt"), "typed by the user").unwrap();
    fs::write(
        h.home().join("Reports/existing.txt"),
        "edited by the user\n",
    )
    .unwrap();
    let view = h.rt.hand_back(&h.me, task).await.unwrap();
    assert!(view.caption.contains("changed 2 files"), "{}", view.caption);

    let changes = h.rt.get(&h.me, task).unwrap().changes;
    let yours: Vec<_> = changes
        .iter()
        .filter(|c| c.action.as_str() == "you.edit")
        .collect();
    assert_eq!(yours.len(), 2, "{changes:?}");
    assert!(
        yours
            .iter()
            .all(|c| c.reversible && c.target.ends_with("(by you)"))
    );
    let by_hand = yours
        .iter()
        .find(|c| c.paths == ["~/Reports/by-hand.txt"])
        .unwrap();
    assert_eq!(by_hand.kind, ChangeKind::Created);

    let report = undo(&h, task, vec![by_hand.id], false).await;
    assert_eq!(report.removed, ["~/Reports/by-hand.txt"]);
    assert_eq!(read(&h, "Reports/existing.txt"), "edited by the user\n");

    let report = undo(&h, task, vec![], false).await;
    assert_eq!(report.restored, ["~/Reports/existing.txt"]);
    assert_eq!(report.removed, ["~/Reports/agent.txt"]);
    assert_eq!(read(&h, "Reports/existing.txt"), "original report\n");
    assert!(undone(&h, task).iter().all(|u| *u));
}

#[tokio::test]
async fn undo_leaves_a_file_changed_after_the_agent_and_says_so() {
    // AU-21: someone else's later version wins over the snapshot.
    let h = harness();
    fs::write(h.home().join("Reports/plan.txt"), "the plan\n").unwrap();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(task).await;
    write(&agent, "Reports/existing.txt", "clobbered").await;
    write(&agent, "Reports/new.csv", "a,b\n").await;
    write(&agent, "Reports/plan.txt", "the agent's plan\n").await;
    // After the agent: the user edits one file, deletes another, and saves
    // the third again unchanged.
    fs::write(h.home().join("Reports/existing.txt"), "fixed by hand\n").unwrap();
    fs::remove_file(h.home().join("Reports/plan.txt")).unwrap();
    fs::write(h.home().join("Reports/new.csv"), "a,b\n").unwrap();

    let preview = undo(&h, task, vec![], true).await;
    assert_eq!(preview.removed, ["~/Reports/new.csv"]);
    assert!(preview.restored.is_empty(), "{:?}", preview.restored);
    assert_eq!(
        preview.not_undone,
        [
            format!("~/Reports/existing.txt — {CHANGED_SINCE}"),
            format!("~/Reports/plan.txt — {CHANGED_SINCE}"),
        ]
    );
    let report = undo(&h, task, vec![], false).await;
    assert_eq!(
        report,
        UndoReport {
            dry_run: false,
            ..preview
        }
    );
    assert_eq!(read(&h, "Reports/existing.txt"), "fixed by hand\n");
    assert!(!h.home().join("Reports/plan.txt").exists());
    assert!(!h.home().join("Reports/new.csv").exists());
    assert_eq!(undone(&h, task), [false, true, false]);
}

#[tokio::test]
async fn undoing_a_change_leaves_a_file_a_later_kept_change_also_wrote() {
    // AU-21: undoing change 0 alone would throw away change 1.
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(task).await;
    write(&agent, "Reports/one.txt", "1").await;
    write(&agent, "Reports/one.txt", "2").await;

    let report = undo(&h, task, vec![0], false).await;
    assert!(report.removed.is_empty() && report.restored.is_empty());
    assert_eq!(
        report.not_undone,
        ["~/Reports/one.txt — also changed by a step this undo keeps, so it was left as it is"]
    );
    assert_eq!(read(&h, "Reports/one.txt"), "2");
    assert_eq!(undone(&h, task), [false, false]);

    // Newest first works, each back to just before it.
    let report = undo(&h, task, vec![1], false).await;
    assert_eq!(report.restored, ["~/Reports/one.txt"]);
    assert_eq!(read(&h, "Reports/one.txt"), "1");
    let report = undo(&h, task, vec![0], false).await;
    assert_eq!(report.removed, ["~/Reports/one.txt"]);
    assert_eq!(undone(&h, task), [true, true]);
}

#[tokio::test]
async fn a_cache_cleanup_comes_back_whole_and_new_cache_files_stay() {
    // AE-4, AU-21: a folder change is undone file by file.
    let h = harness();
    let cache = h.home().join(".cache");
    fs::create_dir_all(cache.join("thumbs")).unwrap();
    fs::write(cache.join("thumbs/a.png"), vec![0u8; 4096]).unwrap();
    fs::write(cache.join("b.bin"), "old b").unwrap();
    let request = TaskRequest {
        agent: "it-agent".parse().unwrap(),
        prompt: "My disk is full".into(),
        scope: ScopeRequest::default(),
        title: None,
    };
    let task = h.rt.start(&h.me, request).unwrap().id;
    let agent = h.agent(task).await;
    let cleaned = tool(&agent, "system_storage_clean_cache", json!({})).await;
    assert!(!cleaned.is_error, "{}", cleaned.text());
    assert!(!cache.join("thumbs").exists());
    // Apps fill the cache again before the user undoes.
    fs::write(cache.join("fresh.tmp"), "new").unwrap();
    fs::write(cache.join("b.bin"), "new b").unwrap();

    let report = undo(&h, task, vec![], false).await;
    assert_eq!(report.restored, ["~/.cache/thumbs/a.png"]);
    assert!(report.removed.is_empty(), "{:?}", report.removed);
    assert_eq!(
        report.not_undone,
        [format!("~/.cache/b.bin — {CHANGED_SINCE}")]
    );
    assert_eq!(
        fs::read(cache.join("thumbs/a.png")).unwrap(),
        vec![0u8; 4096]
    );
    assert_eq!(read(&h, ".cache/fresh.tmp"), "new");
    assert_eq!(read(&h, ".cache/b.bin"), "new b");
}

#[tokio::test(flavor = "multi_thread")]
async fn au20_old_snapshots_go_but_never_while_their_task_can_still_undo() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(task).await;
    write(&agent, "Reports/existing.txt", "clobbered").await;

    // However old, a snapshot its task still holds stays, so undo works.
    assert_eq!(h.rt.prune_snapshots(std::time::Duration::ZERO).unwrap(), 0);
    let report = undo(&h, task, vec![], true).await;
    assert_eq!(report.restored, ["~/Reports/existing.txt"]);

    // Stopped and dismissed, nothing can undo from them: they go, on the
    // record.
    h.rt.stop(&h.me, task).unwrap();
    eventually(|| !h.rt.get(&h.me, task).unwrap().view.state.is_live()).await;
    h.rt.dismiss(&h.me, task).unwrap();
    assert!(h.rt.prune_snapshots(std::time::Duration::ZERO).unwrap() >= 1);
    let records = glass_audit::read(&h.audit_path(), |r| {
        r.event.kind == glass_audit::EventKind::SnapshotsRemoved
    })
    .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        fs::read_to_string(h.home().join("Reports/existing.txt")).unwrap(),
        "clobbered"
    );
}

// ---- What the TLA+ model of undo found (specs/tla/Undo.tla, ADR 0051) ----

/// A `files.*` action that can stop partway, to put another task's step in
/// the gap: before it writes (`hold: "before"`), or after it wrote and
/// before glassd reads how it left the file (`"after"`). `files.gated_note`
/// is the same at Auto, as a draft is. Its revert hook counts its calls.
#[derive(Debug, Default)]
struct Gated {
    gate: Mutex<GateState>,
    turned: Condvar,
    reverts: AtomicUsize,
}

#[derive(Debug, Default)]
struct GateState {
    waiting: bool,
    open: bool,
}

impl Gated {
    fn hold(&self) {
        let mut gate = self.gate.lock().unwrap();
        gate.waiting = true;
        self.turned.notify_all();
        while !gate.open {
            gate = self.turned.wait(gate).unwrap();
        }
        *gate = GateState::default();
    }

    async fn reached(&self) {
        eventually(|| self.gate.lock().unwrap().waiting).await;
    }

    fn release(&self) {
        self.gate.lock().unwrap().open = true;
        self.turned.notify_all();
    }

    fn reverts(&self) -> usize {
        self.reverts.load(Ordering::SeqCst)
    }
}

impl ActionProvider for Gated {
    fn actions(&self) -> Vec<ActionDef> {
        let def = |name: &str, tier| ActionDef {
            name: name.parse().unwrap(),
            title: "Write a file, gated".into(),
            description: "Test only.".into(),
            input_schema: json!({ "type": "object" }),
            side_effect: SideEffect::LocalReversible,
            default_tier: tier,
            ask: Default::default(),
        };
        vec![
            def("files.gated_write", Tier::Notify),
            def("files.gated_note", Tier::Auto),
        ]
    }

    fn preview(
        &self,
        ctx: &TaskContext,
        _action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        let r = ctx.resolve_writable(input["path"].as_str().unwrap_or_default())?;
        Ok(Preview {
            summary: format!("Write {}", r.display),
            targets: vec![r.display.clone()],
            writes: vec![r.host.clone()],
            snapshot_roots: vec![r.grant.root.clone()],
            ..Preview::default()
        })
    }

    fn invoke(
        &self,
        ctx: &TaskContext,
        _action: &ActionName,
        input: &Value,
    ) -> Result<ActionOutput, ActionError> {
        let r = ctx.resolve_writable(input["path"].as_str().unwrap_or_default())?;
        if input["hold"] == "before" {
            self.hold();
        }
        let dir = ConfinedDir::open(&r.grant.root).map_err(|e| ActionError::io(&r.display, e))?;
        let content = input["content"].as_str().unwrap_or_default();
        dir.write(&r.rel, content.as_bytes(), Some(ctx.owner))
            .map_err(|e| ActionError::io(&r.display, e))?;
        if input["hold"] == "after" {
            self.hold();
        }
        Ok(ActionOutput {
            text: format!("Wrote {}", r.display),
            data: json!({}),
            changes: vec![Change {
                kind: ChangeKind::Modified,
                target: r.display,
                reversible: true,
                paths: vec![r.host],
            }],
        })
    }

    fn revert(
        &self,
        _ctx: &TaskContext,
        _action: &ActionName,
        _kind: ChangeKind,
        _paths: &[PathBuf],
    ) -> Result<(), ActionError> {
        self.reverts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Policy's actions are the files actions and `Gated`'s.
fn gated(h: &Harness) -> Arc<Gated> {
    let gated = Arc::new(Gated::default());
    let mut registry = Registry::new();
    registry.register(Arc::new(FilesProvider)).unwrap();
    registry
        .register(gated.clone() as Arc<dyn ActionProvider>)
        .unwrap();
    h.rt.apply_policy(Policy::from_toml(DEFAULT_POLICY).unwrap(), registry, "test");
    gated
}

async fn gated_write(agent: &Client, tool_name: &str, path: &str, content: &str, hold: &str) {
    let args = json!({ "path": path, "content": content, "hold": hold });
    let done = tool(agent, tool_name, args).await;
    assert!(!done.is_error, "{}", done.text());
}

/// Long enough for another task's whole step, had it not waited its turn.
const A_STEP: std::time::Duration = std::time::Duration::from_millis(300);

#[tokio::test]
async fn adr0051_undoing_an_earlier_change_never_takes_back_another_tasks_later_one() {
    // AU-21. A writes, B writes, A writes again. Undoing A's last change
    // puts B's version back; that version is B's, so undoing A's first
    // change leaves it and says so.
    let h = harness();
    let a = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let b = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let (agent_a, agent_b) = (h.agent(a).await, h.agent(b).await);
    write(&agent_a, "Reports/shared.csv", "a1\n").await;
    write(&agent_b, "Reports/shared.csv", "b\n").await;
    write(&agent_a, "Reports/shared.csv", "a2\n").await;

    undo(&h, a, vec![1], false).await;
    assert_eq!(read(&h, "Reports/shared.csv"), "b\n");
    let report = undo(&h, a, vec![0], false).await;
    assert_eq!(read(&h, "Reports/shared.csv"), "b\n");
    assert_eq!(
        report.not_undone,
        [format!("~/Reports/shared.csv — {CHANGED_SINCE}")]
    );
    assert_eq!(undone(&h, a), [false, true]);
}

#[tokio::test]
async fn adr0051_undoing_an_undone_change_runs_nothing_again() {
    // AU-21: once undone, a change's revert hook never runs again, asked
    // again or by two undos at once.
    let h = harness();
    let gated = gated(&h);
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(task).await;
    gated_write(&agent, "files_gated_write", "Reports/x.txt", "x", "").await;
    undo(&h, task, vec![], false).await;
    assert_eq!(gated.reverts(), 1);
    let again = undo(&h, task, vec![0], false).await;
    assert_eq!(gated.reverts(), 1);
    assert!(
        again.restored.is_empty() && again.removed.is_empty() && again.not_undone.is_empty(),
        "{again:?}"
    );

    // Undo held the task (AW-22); the person carries on.
    h.rt.resume(&h.me, task).unwrap();
    gated_write(&agent, "files_gated_write", "Reports/y.txt", "y", "").await;
    let (one, all) = tokio::join!(
        undo(&h, task, vec![1], false),
        undo(&h, task, vec![], false)
    );
    assert_eq!(gated.reverts(), 2, "{one:?} {all:?}");
    assert_eq!(undone(&h, task), [true, true]);
}

#[tokio::test]
async fn adr0051_another_tasks_write_just_after_a_step_is_never_taken_for_the_steps() {
    // ADR 0017: glassd reads how a step left its files right after it; a
    // write of another task's lands after that, not in between, so undoing
    // the first step leaves the second's version.
    let h = harness();
    let gated = gated(&h);
    let a = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let b = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let (agent_a, agent_b) = (h.agent(a).await, h.agent(b).await);
    let first = tokio::spawn(async move {
        gated_write(
            &agent_a,
            "files_gated_write",
            "Reports/shared.csv",
            "a\n",
            "after",
        )
        .await;
    });
    gated.reached().await;
    let second = tokio::spawn(async move {
        write(&agent_b, "Reports/shared.csv", "b\n").await;
    });
    tokio::time::sleep(A_STEP).await;
    gated.release();
    first.await.unwrap();
    second.await.unwrap();

    let report = undo(&h, a, vec![], false).await;
    assert_eq!(read(&h, "Reports/shared.csv"), "b\n");
    assert_eq!(
        report.not_undone,
        [format!("~/Reports/shared.csv — {CHANGED_SINCE}")]
    );
}

#[tokio::test]
async fn adr0051_undo_never_takes_back_a_write_made_between_a_steps_snapshot_and_it() {
    // AU-21: undo puts back the file as it was just before the step. Another
    // task's write comes before the snapshot or after the step, never in
    // between, where the snapshot wouldn't hold it.
    let h = harness();
    let gated = gated(&h);
    let a = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let b = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let (agent_a, agent_b) = (h.agent(a).await, h.agent(b).await);
    let first = tokio::spawn(async move {
        gated_write(
            &agent_a,
            "files_gated_write",
            "Reports/shared.csv",
            "a\n",
            "before",
        )
        .await;
    });
    gated.reached().await;
    let second = tokio::spawn(async move {
        write(&agent_b, "Reports/shared.csv", "b\n").await;
    });
    tokio::time::sleep(A_STEP).await;
    gated.release();
    first.await.unwrap();
    second.await.unwrap();

    undo(&h, a, vec![], false).await;
    assert_eq!(read(&h, "Reports/shared.csv"), "b\n");
}

#[tokio::test]
async fn adr0051_an_undone_change_never_comes_back_with_another_tasks_undo() {
    // AU-21: an undo waits for another task's step under way, so that
    // step's snapshot never holds what the undo took away.
    let h = harness();
    let gated = gated(&h);
    let a = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let b = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let (agent_a, agent_b) = (h.agent(a).await, h.agent(b).await);
    write(&agent_a, "Reports/shared.csv", "a\n").await;
    let step = tokio::spawn(async move {
        gated_write(
            &agent_b,
            "files_gated_write",
            "Reports/shared.csv",
            "b\n",
            "before",
        )
        .await;
    });
    gated.reached().await;
    let rt = h.rt.clone();
    let me = h.me.clone();
    let undoing = tokio::spawn(async move {
        let request = UndoRequest {
            task: a,
            to: None,
            changes: vec![],
            dry_run: false,
        };
        rt.undo(&me, request).await.unwrap()
    });
    tokio::time::sleep(A_STEP).await;
    gated.release();
    step.await.unwrap();
    undoing.await.unwrap();
    undo(&h, b, vec![], false).await;

    let content = fs::read_to_string(h.home().join("Reports/shared.csv")).ok();
    assert!(
        !(undone(&h, a) == [true] && content.as_deref() == Some("a\n")),
        "A's change is undone, yet its version is back: {content:?}"
    );
}

#[tokio::test]
async fn adr0051_an_auto_step_after_another_tasks_change_snapshots_afresh() {
    // AU-20, AU-21: an Auto step needs no snapshot of its own while the
    // task's snapshot still holds its files as they are; once another task
    // changed one, undoing the step would put back the older version.
    let h = harness();
    let _gated = gated(&h);
    let a = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let b = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let (agent_a, agent_b) = (h.agent(a).await, h.agent(b).await);
    gated_write(
        &agent_a,
        "files_gated_write",
        "Reports/other.txt",
        "o\n",
        "",
    )
    .await;
    write(&agent_b, "Reports/shared.csv", "b\n").await;
    gated_write(
        &agent_a,
        "files_gated_note",
        "Reports/shared.csv",
        "a\n",
        "",
    )
    .await;

    undo(&h, a, vec![1], false).await;
    assert_eq!(read(&h, "Reports/shared.csv"), "b\n");
}
