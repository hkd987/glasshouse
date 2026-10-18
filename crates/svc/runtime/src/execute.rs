//! The action pipeline: what happens to every tool call an agent makes.
//!
//! hold while paused → preview → decide (tier, labels) → ask if needed →
//! snapshot if reversible → refuse side effects if the audit log is down →
//! invoke → record (audit, feed, changes).
//!
//! Policy refusals come back to the agent as tool errors it can read and
//! react to; only malformed calls are protocol errors.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use glass_actions::{ActionDef, ActionError, ActionProvider, Preview};
use glass_api::{ActivityKind, ApprovalScope, ChangeEntry, Progress, TileState};
use glass_audit::{ApprovalRef, AuditEvent, EventKind, Outcome as AuditOutcome, PolicyDecision};
use glass_broker::{Outcome, Verdict, decide, new_view};
use glass_rpc::RpcError;
use glass_rpc::mcp::CallToolResult;
use glass_types::{SideEffect, Tier, Timestamp};
use serde::Deserialize;
use serde_json::Value;

use crate::task::{Gate, Task};
use crate::{REPORT_PROGRESS_TOOL, Runtime, labels};

impl Runtime {
    pub(crate) async fn call_tool(
        self: &Arc<Self>,
        task: &Arc<Task>,
        tool: &str,
        args: Value,
    ) -> Result<CallToolResult, RpcError> {
        if tool == REPORT_PROGRESS_TOOL {
            return self.report_progress(task, args);
        }
        let Some((def, provider)) = self.registry.by_mcp_name(tool) else {
            return Err(RpcError::invalid_params(format!("unknown tool {tool:?}")));
        };
        let (def, provider) = (def.clone(), Arc::clone(provider));

        if task.wait_runnable().await.is_err() {
            return Ok(CallToolResult::error("This task has ended."));
        }

        let preview = {
            let (provider, ctx, name, input) = (
                Arc::clone(&provider),
                task.ctx.clone(),
                def.name.clone(),
                args.clone(),
            );
            match tokio::task::spawn_blocking(move || provider.preview(&ctx, &name, &input)).await {
                Ok(result) => result,
                Err(e) => Err(ActionError::Failed(e.to_string())),
            }
        };
        let preview = match preview {
            Ok(preview) => preview,
            Err(e) => return Ok(self.refuse_invalid(task, &def, e)),
        };

        let home = task.user.home.clone();
        let label_of = |path: &std::path::Path| labels::label_of(&task.policy, &home, path);
        let paths = preview
            .reads
            .iter()
            .chain(&preview.writes)
            .map(PathBuf::as_path);
        let verdict = decide(&task.policy, &def.name, def.default_tier, paths, &label_of);

        let mut approval = None;
        match &verdict {
            Verdict::Block { reason, .. } => {
                self.record(
                    task,
                    &def,
                    &preview,
                    &verdict,
                    None,
                    None,
                    AuditOutcome::Blocked,
                );
                {
                    let mut state = task.lock();
                    state.note(
                        ActivityKind::Blocked,
                        format!("Blocked: {} — {reason}", preview.summary),
                    );
                }
                self.publish(task);
                return Ok(CallToolResult::error(format!(
                    "Blocked by policy: {reason}. This is listed under \"Can't touch\" for this task; continue without it."
                )));
            }
            Verdict::Ask { rule } if !self.desk.has_task_grant(task.id, &def.name) => {
                match self.ask(task, &def, &preview, rule, &verdict).await {
                    Ok(answer) => approval = Some(answer),
                    Err(refusal) => return Ok(refusal),
                }
                if task.wait_runnable().await.is_err() {
                    return Ok(CallToolResult::error("This task has ended."));
                }
            }
            Verdict::Ask { .. } | Verdict::Run { .. } => {}
        }

        let mut snapshot = None;
        if def.side_effect == SideEffect::LocalReversible && !preview.snapshot_roots.is_empty() {
            let needed = verdict.tier() >= Tier::Notify || !task.covered(&preview.snapshot_roots);
            if needed {
                match self
                    .take_snapshot(
                        task,
                        &preview.snapshot_roots,
                        &format!("Before: {}", preview.summary),
                    )
                    .await
                {
                    Ok(handle) => snapshot = Some(handle.id),
                    Err(e) => {
                        let failure = AuditOutcome::Failed {
                            error: format!("snapshot failed: {e}"),
                        };
                        self.record(
                            task,
                            &def,
                            &preview,
                            &verdict,
                            approval.clone(),
                            None,
                            failure,
                        );
                        return Ok(CallToolResult::error(format!(
                            "Not done: couldn't take the snapshot that makes this undoable ({e})."
                        )));
                    }
                }
            }
        }

        if def.side_effect != SideEffect::None && !self.audit_healthy() {
            return Ok(CallToolResult::error(
                "Not done: the audit log is unavailable, so the OS is refusing changes until it recovers.",
            ));
        }

        let result = {
            let (ctx, name, input) = (task.ctx.clone(), def.name.clone(), args);
            let provider: Arc<dyn ActionProvider> = provider;
            match tokio::task::spawn_blocking(move || provider.invoke(&ctx, &name, &input)).await {
                Ok(result) => result,
                Err(e) => Err(ActionError::Failed(e.to_string())),
            }
        };
        match result {
            Ok(output) => {
                self.record(
                    task,
                    &def,
                    &preview,
                    &verdict,
                    approval,
                    snapshot,
                    AuditOutcome::Completed,
                );
                {
                    let mut state = task.lock();
                    state.record_action(&preview.summary);
                    let at = Timestamp::now();
                    for change in output.changes {
                        state.changes.push(ChangeEntry {
                            at,
                            kind: change.kind,
                            target: change.target,
                            action: def.name.clone(),
                            reversible: change.reversible,
                        });
                    }
                }
                self.publish(task);
                Ok(CallToolResult::ok(output.text, output.data))
            }
            Err(e) => {
                let failure = AuditOutcome::Failed {
                    error: e.to_string(),
                };
                self.record(task, &def, &preview, &verdict, approval, snapshot, failure);
                task.lock().note(
                    ActivityKind::Notice,
                    format!("Failed: {} — {e}", preview.summary),
                );
                self.publish(task);
                Ok(CallToolResult::error(e.to_string()))
            }
        }
    }

    /// Opens an ask and waits for the user (DA-13: an unanswered ask holds
    /// the agent; after the policy timeout the task pauses).
    async fn ask(
        &self,
        task: &Arc<Task>,
        def: &ActionDef,
        preview: &Preview,
        rule: &str,
        verdict: &Verdict,
    ) -> Result<ApprovalRef, CallToolResult> {
        let timeout = task.policy.approval_timeout_secs();
        let items = if preview.items.is_empty() {
            preview.targets.clone()
        } else {
            preview.items.clone()
        };
        let view = new_view(
            task.id,
            task.entry.id.clone(),
            def.name.clone(),
            preview.summary.clone(),
            items,
            rule.to_owned(),
            timeout,
        );
        let id = view.id;
        let mut reply = self.desk.open(view, task.user.uid);

        let mut event = self.task_event(
            task,
            EventKind::ApprovalRequested,
            format!("Needs you: {}", preview.summary),
        );
        event.action = Some(def.name.clone());
        event.targets = preview.targets.clone();
        event.approval = Some(ApprovalRef {
            id,
            answered_by: None,
            answer: None,
            answered_at: None,
        });
        self.audit(event);
        {
            let mut state = task.lock();
            if state.tile == TileState::Working {
                state.tile = TileState::NeedsYou;
            }
            state.note(
                ActivityKind::Approval,
                format!("Needs you: {}", preview.summary),
            );
        }
        self.publish(task);

        let outcome = tokio::select! {
            answer = &mut reply => answer.unwrap_or(Outcome::Withdrawn),
            _ = tokio::time::sleep(Duration::from_secs(timeout)) => match self.desk.expire(id) {
                Some(_) => Outcome::TimedOut,
                // Answered at the last moment: take the answer.
                None => (&mut reply).await.unwrap_or(Outcome::Withdrawn),
            },
        };

        let approval = |answer: &str| ApprovalRef {
            id,
            answered_by: Some(task.user.name.clone()),
            answer: Some(answer.into()),
            answered_at: Some(Timestamp::now()),
        };
        let back_to_work = |note: String| {
            let mut state = task.lock();
            if state.tile == TileState::NeedsYou {
                state.tile = TileState::Working;
            }
            state.note(ActivityKind::Approval, note);
        };
        match outcome {
            Outcome::Approved(scope) => {
                let how = match scope {
                    ApprovalScope::Once => "once",
                    ApprovalScope::Task => "for the rest of this task",
                };
                back_to_work(format!("Approved {how}: {}", preview.summary));
                self.publish(task);
                Ok(approval(&format!("approved {how}")))
            }
            Outcome::Denied => {
                back_to_work(format!("Declined: {}", preview.summary));
                self.record(
                    task,
                    def,
                    preview,
                    verdict,
                    Some(approval("denied")),
                    None,
                    AuditOutcome::Denied,
                );
                self.publish(task);
                Err(CallToolResult::error(format!(
                    "The user declined: {}. Don't retry it; continue without it or finish.",
                    preview.summary
                )))
            }
            Outcome::TimedOut => {
                self.hold(
                    task,
                    TileState::Paused,
                    &format!("No answer in {} min; paused", timeout.div_ceil(60)),
                );
                self.record(
                    task,
                    def,
                    preview,
                    verdict,
                    None,
                    None,
                    AuditOutcome::TimedOut,
                );
                self.publish(task);
                Err(CallToolResult::error(
                    "No answer within the approval timeout, so the task is paused. If the user resumes it, ask again.",
                ))
            }
            Outcome::Withdrawn => Err(CallToolResult::error("This task has ended.")),
        }
    }

    /// A call the provider refused before anything ran: bad input, or a path
    /// outside the task's grants. Scope violations are security-relevant and
    /// always audited.
    fn refuse_invalid(&self, task: &Task, def: &ActionDef, error: ActionError) -> CallToolResult {
        if let ActionError::OutOfScope(detail) = &error {
            let mut event = self.task_event(
                task,
                EventKind::Action,
                format!("Out of scope: {}", def.name),
            );
            event.action = Some(def.name.clone());
            event.outcome = Some(AuditOutcome::Blocked);
            event.decision = Some(PolicyDecision {
                tier: Tier::Never,
                rule: "task scope".into(),
            });
            event.detail = serde_json::json!({ "reason": detail });
            self.audit(event);
            task.lock().note(
                ActivityKind::Blocked,
                format!("Blocked: {} — {detail}", def.title),
            );
            self.publish(task);
        }
        CallToolResult::error(error.to_string())
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        task: &Task,
        def: &ActionDef,
        preview: &Preview,
        verdict: &Verdict,
        approval: Option<ApprovalRef>,
        snapshot: Option<glass_types::SnapshotId>,
        outcome: AuditOutcome,
    ) {
        let mut event: AuditEvent =
            self.task_event(task, EventKind::Action, preview.summary.clone());
        event.action = Some(def.name.clone());
        event.targets = preview.targets.clone();
        event.decision = Some(PolicyDecision {
            tier: verdict.tier(),
            rule: verdict.rule().to_owned(),
        });
        event.approval = approval;
        event.snapshot = snapshot;
        event.outcome = Some(outcome);
        self.audit(event);
    }

    fn report_progress(&self, task: &Task, args: Value) -> Result<CallToolResult, RpcError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            caption: String,
            #[serde(default)]
            done: Option<u32>,
            #[serde(default)]
            total: Option<u32>,
        }
        let args: Args = serde_json::from_value(args).map_err(RpcError::invalid_params)?;
        let caption: String = args.caption.chars().take(120).collect();
        {
            let mut state = task.lock();
            if *task.gate.borrow() == Gate::Ended {
                return Ok(CallToolResult::error("This task has ended."));
            }
            if state.caption != caption {
                state.note(ActivityKind::Notice, caption.clone());
            }
            state.caption = caption;
            state.progress = match (args.done, args.total) {
                (Some(done), Some(total)) if total > 0 => Some(Progress {
                    done: done.min(total),
                    total,
                }),
                _ => None,
            };
            state.updated_at = Timestamp::now();
        }
        self.publish(task);
        Ok(CallToolResult::ok("ok", Value::Null))
    }
}
