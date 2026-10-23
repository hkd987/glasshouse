//! The action pipeline: what happens to every tool call an agent makes.
//!
//! hold while paused → preview → decide (tier, labels) → ask if needed →
//! snapshot if reversible → refuse side effects if the audit log is down →
//! invoke → record (audit, feed, changes).
//!
//! A model call (`model.complete`) takes the same path. After the tier
//! check, the most sensitive label of what the task has read decides where
//! it may go, and "invoke" sends it through the model gateway (ADR 0018).
//!
//! Policy refusals come back to the agent as tool errors it can read and
//! react to; only malformed calls are protocol errors.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use glass_actions::{ActionDef, ActionError, ActionProvider, Preview};
use glass_api::{ActivityKind, ApprovalScope, ChangeEntry, Progress, TileState};
use glass_audit::{ApprovalRef, AuditEvent, EventKind, Outcome as AuditOutcome, PolicyDecision};
use glass_broker::{Outcome, Verdict, decide, new_view};
use glass_gateway::MODEL_COMPLETE;
use glass_rpc::RpcError;
use glass_rpc::mcp::CallToolResult;
use glass_types::{SideEffect, Tier, Timestamp};
use serde::Deserialize;
use serde_json::Value;

use crate::runtime::display_path;
use crate::task::{Gate, Task};
use crate::written::{self, Seen, Touched};
use crate::{REPORT_PROGRESS_TOOL, Runtime, labels, model, title};

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
        let registry = self.registry();
        let Some((def, provider)) = registry.by_mcp_name(tool) else {
            return Err(RpcError::invalid_params(format!("unknown tool {tool:?}")));
        };
        let (def, provider) = (def.clone(), Arc::clone(provider));
        Ok(self.run_action(task, def, provider, args).await)
    }

    /// Runs one action for the task's principal, an agent or an app,
    /// through every step of the pipeline. Refusals are results.
    pub(crate) async fn run_action(
        self: &Arc<Self>,
        task: &Arc<Task>,
        def: ActionDef,
        provider: Arc<dyn ActionProvider>,
        args: Value,
    ) -> CallToolResult {
        match self.pipeline(task, def, provider, args).await {
            Ok(result) | Err(result) => result,
        }
    }

    async fn pipeline(
        self: &Arc<Self>,
        task: &Arc<Task>,
        def: ActionDef,
        provider: Arc<dyn ActionProvider>,
        args: Value,
    ) -> Result<CallToolResult, CallToolResult> {
        let started = Instant::now();

        if task.wait_runnable().await.is_err() {
            return Ok(CallToolResult::error("This task has ended."));
        }
        // A routine saved from what this task did: this computer writes its
        // script from the task's calls, so what the person approves is that
        // code, the same every time (ADR 0046).
        let args = match task_script(task, &def, args) {
            Ok(args) => args,
            Err(why) => {
                return Ok(self.refuse_invalid(
                    task,
                    &def,
                    ActionError::InvalidInput(format!("this task can't be replayed: {why}")),
                ));
            }
        };
        // What the call did goes in the task's trace, for replay code: not
        // model calls, an app's, or saving routines.
        let recorded = (!task.is_app()
            && def.name.as_str() != MODEL_COMPLETE
            && !def.name.as_str().starts_with("routines."))
        .then(|| args.clone());

        // What only people take (a meeting's, the person's own) is Never
        // for an agent whatever it names: refused before the provider sees
        // its input at all (ADR 0047).
        let people_only = matches!(
            task.policy().tier_for(&def.name, def.default_tier).source,
            glass_policy::RuleSource::PeopleOnly | glass_policy::RuleSource::PersonsOwn
        );
        let preview = if people_only {
            Ok(Preview {
                summary: def.title.clone(),
                ..Preview::default()
            })
        } else {
            let (provider, ctx, name, input) = (
                Arc::clone(&provider),
                task.call_ctx(),
                def.name.clone(),
                args.clone(),
            );
            match tokio::task::spawn_blocking(move || provider.preview(&ctx, &name, &input)).await {
                Ok(result) => result,
                Err(e) => Err(ActionError::Failed(e.to_string())),
            }
        };
        let mut preview = match preview {
            Ok(preview) => preview,
            Err(e) => return Ok(self.refuse_invalid(task, &def, e)),
        };

        let home = task.user.home.clone();
        let policy = task.policy();
        let label_of = |path: &std::path::Path| labels::label_of(&policy, &home, path);
        let paths = preview
            .reads
            .iter()
            .chain(&preview.writes)
            .map(PathBuf::as_path);
        let verdict = decide(&policy, &def.name, def.default_tier, paths, &label_of);
        // An item of the shared space is held to its label like a file
        // (ADR 0034 §5).
        let verdict = match (verdict, preview.label) {
            (verdict @ Verdict::Block { .. }, _) | (verdict, None) => verdict,
            (_, Some(label)) if !policy.may_open(label) => Verdict::Block {
                reason: format!(
                    "{} is labeled {label}; this agent may only open data up to {}",
                    preview.targets.first().map_or("It", String::as_str),
                    policy.max_label()
                ),
                rule: format!("data label {label}"),
            },
            (verdict, _) => verdict,
        };
        // What the call reads, so a successful one raises the task's label
        // (DP-11).
        let read_label = preview
            .reads
            .iter()
            .filter_map(|path| label_of(path).ok().flatten())
            .chain(preview.label)
            .max();

        // A model call carries the task's data off the machine: the most
        // sensitive label it has read decides where it may go (DP-2).
        let mut routed = None;
        let mut refused_model = None;
        let verdict = match verdict {
            Verdict::Block { .. } => verdict,
            _ if def.name.as_str() == MODEL_COMPLETE => match self.route_model(task, &args) {
                Ok(call) => {
                    (preview.summary, preview.targets) = model::summary(&call);
                    routed = Some(call);
                    verdict
                }
                Err(refusal) => {
                    // Refused for the data it carries, not for its input.
                    let hint = if refusal.rule.starts_with("models.") {
                        ""
                    } else {
                        " Policy decides which models may see the data this task has read; \
                         continue without a model for this step."
                    };
                    refused_model = Some((model::refusal_detail(task, &args), hint));
                    Verdict::Block {
                        reason: refusal.reason,
                        rule: refusal.rule,
                    }
                }
            },
            _ => verdict,
        };

        let mut approval = None;
        match &verdict {
            Verdict::Block { reason, .. } => {
                let hint = refused_model.as_ref().map(|(_, hint)| *hint);
                self.record(
                    task,
                    &def,
                    &preview,
                    &verdict,
                    None,
                    None,
                    AuditOutcome::Blocked,
                    started,
                    refused_model.map(|(detail, _)| detail),
                );
                {
                    let mut state = task.lock();
                    state.note(
                        ActivityKind::Blocked,
                        format!("Blocked: {} — {reason}", preview.summary),
                    );
                }
                self.publish(task);
                return Ok(CallToolResult::error(if let Some(hint) = hint {
                    format!("Not sent: {reason}.{hint}")
                } else if task.is_app() {
                    format!("Blocked by policy: {reason}.")
                } else {
                    format!(
                        "Blocked by policy: {reason}. This is listed under \"Can't touch\" for this task; continue without it."
                    )
                }));
            }
            Verdict::Ask { rule } if !self.desk.has_task_grant(task.id, &def.name) => {
                match self
                    .ask(task, &def, &preview, rule, &verdict, started)
                    .await
                {
                    Ok(answer) => approval = Some(answer),
                    Err(refusal) => return Ok(refusal),
                }
                if task.wait_runnable().await.is_err() {
                    return Ok(CallToolResult::error("This task has ended."));
                }
            }
            Verdict::Ask { .. } | Verdict::Run { .. } => {}
        }

        // From here the call is committed. Count it so sleep can wait for it,
        // and re-check the gate after counting, so a pause that landed in
        // between is honored before anything changes.
        let _in_flight = loop {
            let guard = InFlight::enter(&task.in_flight);
            if *task.gate.borrow() == Gate::Run {
                break guard;
            }
            drop(guard);
            if task.wait_runnable().await.is_err() {
                return Ok(CallToolResult::error("This task has ended."));
            }
        };

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
                            started,
                            None,
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

        // Undo needs the snapshots that existed as the call ran, and how it
        // left the files it changed (ADR 0017).
        let (before, roots) = {
            let state = task.lock();
            (state.snapshots.len(), state.snapshot_roots())
        };
        let (result, seen, sent) = if let Some(call) = routed {
            let (rt, sending) = (Arc::clone(self), Arc::clone(task));
            let sent = tokio::task::spawn_blocking(move || {
                Runtime::send_model(&rt.gateway(), &sending.policy(), &call)
            })
            .await;
            match sent {
                Ok(sent) => {
                    if let Some((summary, targets)) = sent.answered {
                        (preview.summary, preview.targets) = (summary, targets);
                    }
                    let mut state = task.lock();
                    for name in sent.went_to {
                        if !state.models_sent_to.contains(&name) {
                            state.models_sent_to.push(name);
                        }
                    }
                    drop(state);
                    (sent.result, Seen::default(), Some(sent.record))
                }
                Err(e) => (
                    Err(ActionError::Failed(e.to_string())),
                    Seen::default(),
                    None,
                ),
            }
        } else {
            let (ctx, name, input) = (task.call_ctx(), def.name.clone(), args);
            let provider: Arc<dyn ActionProvider> = provider;
            let invoked = tokio::task::spawn_blocking(move || {
                let result = provider.invoke(&ctx, &name, &input);
                // Read right away, before anyone else can change them.
                let seen = match &result {
                    Ok(output) => written::look(
                        &roots,
                        output
                            .changes
                            .iter()
                            .filter(|c| c.reversible)
                            .flat_map(|c| c.paths.iter().map(PathBuf::as_path)),
                    ),
                    Err(_) => Seen::default(),
                };
                (result, seen)
            })
            .await;
            let (result, seen) = match invoked {
                Ok(invoked) => invoked,
                Err(e) => (Err(ActionError::Failed(e.to_string())), Seen::default()),
            };
            (result, seen, None)
        };
        let sent = sent.as_ref().map(model::audit_detail);
        match result {
            Ok(mut output) => {
                // What the app keeps for the record only, such as a sent
                // message's SHA-256 (AU-1, ADR 0037): into the audit record
                // under the app's name, and out of the result.
                let provided = output
                    .data
                    .as_object_mut()
                    .and_then(|data| data.remove(glass_actions::AUDIT_DETAIL))
                    .map(|detail| {
                        let app = def.name.as_str().split('.').next().unwrap_or_default();
                        (app.to_owned(), detail)
                    });
                self.record_with(
                    task,
                    &def,
                    &preview,
                    &verdict,
                    approval,
                    snapshot,
                    AuditOutcome::Completed,
                    started,
                    sent,
                    provided,
                );
                {
                    let mut state = task.lock();
                    state.data_label = state.data_label.max(read_label);
                    let verb = if def.name.as_str() == MODEL_COMPLETE {
                        "ask".to_owned()
                    } else {
                        def.name
                            .as_str()
                            .rsplit('.')
                            .next()
                            .unwrap_or_default()
                            .replace('_', " ")
                    };
                    let target = match preview.targets.as_slice() {
                        // "List granted folders" → "granted folders"
                        [] => preview
                            .summary
                            .split_once(' ')
                            .map(|(_, rest)| rest.to_owned())
                            .unwrap_or_default(),
                        [one] => one.clone(),
                        [first, rest @ ..] => format!("{first} · {} more", rest.len()),
                    };
                    state.record_action(&verb, &target, &preview.summary);
                    if let Some(input) = recorded {
                        state.trace.push(glass_replay::Call {
                            tool: def.mcp_name(),
                            input,
                            text: output.text.clone(),
                            data: output.data.clone(),
                            changes: def.side_effect != SideEffect::None,
                        });
                    }
                    let at = Timestamp::now();
                    for change in output.changes {
                        let id = state.changes.len() as u32;
                        state.changes.push(ChangeEntry {
                            id,
                            at,
                            kind: change.kind,
                            target: change.target,
                            action: def.name.clone(),
                            reversible: change.reversible,
                            paths: change
                                .paths
                                .iter()
                                .map(|p| display_path(&task.user.home, p))
                                .collect(),
                            undone: false,
                        });
                        let paths = change.paths;
                        state.written.touched(id, Touched { paths, before });
                    }
                    state.written.note(seen);
                }
                self.publish(task);
                Ok(CallToolResult::ok(output.text, output.data))
            }
            Err(e) => {
                let failure = AuditOutcome::Failed {
                    error: e.to_string(),
                };
                self.record(
                    task, &def, &preview, &verdict, approval, snapshot, failure, started, sent,
                );
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
        started: Instant,
    ) -> Result<ApprovalRef, CallToolResult> {
        let timeout = task.policy().approval_timeout_secs();
        let items = if preview.items.is_empty() {
            preview.targets.clone()
        } else {
            preview.items.clone()
        };
        let mut view = new_view(
            task.id,
            task.entry.id.clone(),
            def.name.clone(),
            preview.summary.clone(),
            items,
            rule.to_owned(),
            timeout,
        );
        view.labels = def.ask.clone();
        view.texts = review_texts(&preview.texts);
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
                    started,
                    None,
                );
                self.publish(task);
                Err(CallToolResult::error(if task.is_app() {
                    format!(
                        "Declined on the desktop, so this wasn't done: {}.",
                        preview.summary
                    )
                } else {
                    format!(
                        "The user declined: {}. Don't retry it; continue without it or finish.",
                        preview.summary
                    )
                }))
            }
            Outcome::TimedOut if task.is_app() => {
                // An app isn't paused: the person is right there, and its
                // next call should work. The call just didn't happen.
                back_to_work(format!(
                    "No answer in {} min: {}",
                    timeout.div_ceil(60),
                    preview.summary
                ));
                self.record(
                    task,
                    def,
                    preview,
                    verdict,
                    None,
                    None,
                    AuditOutcome::TimedOut,
                    started,
                    None,
                );
                self.publish(task);
                Err(CallToolResult::error(format!(
                    "Nobody answered on the desktop in time, so this wasn't done: {}.",
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
                    started,
                    None,
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
        } else if let ActionError::Failed(detail) | ActionError::NotFound(detail) = &error {
            // A step the agent couldn't take is part of what the user sees,
            // like "openvpn isn't set up on this computer".
            task.lock().note(
                ActivityKind::Notice,
                format!("Not done: {} — {detail}", def.title),
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
        started: Instant,
        model: Option<Value>,
    ) {
        self.record_with(
            task, def, preview, verdict, approval, snapshot, outcome, started, model, None,
        );
    }

    /// [`record`](Self::record), with what the app gave for the record
    /// (`detail.<app>`).
    #[allow(clippy::too_many_arguments)]
    fn record_with(
        &self,
        task: &Task,
        def: &ActionDef,
        preview: &Preview,
        verdict: &Verdict,
        approval: Option<ApprovalRef>,
        snapshot: Option<glass_types::SnapshotId>,
        outcome: AuditOutcome,
        started: Instant,
        model: Option<Value>,
        provided: Option<(String, Value)>,
    ) {
        let mut event: AuditEvent =
            self.task_event(task, EventKind::Action, preview.summary.clone());
        // How long the OS took, from the call to its outcome, for the
        // latency budgets (docs/dev/performance.md). For an Ask it includes
        // the wait for the person.
        event.detail =
            serde_json::json!({ "elapsed_ms": started.elapsed().as_secs_f64() * 1000.0 });
        // A model call's model, provider, tokens, cost, label, redactions,
        // and the sizes and hashes of what crossed (DP-5).
        if let Some(model) = model {
            event.detail["model"] = model;
        }
        if let Some((app, detail)) = provided
            && !matches!(app.as_str(), "elapsed_ms" | "model")
        {
            event.detail[app] = detail;
        }
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
            #[serde(default)]
            title: Option<String>,
            #[serde(default)]
            answer: Option<String>,
        }
        let args: Args = serde_json::from_value(args).map_err(RpcError::invalid_params)?;
        let caption: String = args.caption.chars().take(120).collect();
        let answer: Option<String> = args
            .answer
            .map(|a| a.trim().chars().take(crate::ANSWER_MAX_CHARS).collect())
            .filter(|a: &String| !a.is_empty());
        let title = match args.title.as_deref().map(title::check).transpose() {
            Ok(title) => title,
            Err(problem) => {
                return Ok(CallToolResult::error(format!(
                    "Nothing changed: {problem}."
                )));
            }
        };
        let mut renamed = None;
        {
            let mut state = task.lock();
            if *task.gate.borrow() == Gate::Ended {
                return Ok(CallToolResult::error("This task has ended."));
            }
            // The agent may rename the task once, and never over a title
            // the user gave: the title is how the user finds the task.
            if let Some(title) = title {
                if state.title_fixed {
                    return Ok(CallToolResult::error(format!(
                        "Nothing changed: this task's title is \"{}\" and can't change again. \
                         Call {REPORT_PROGRESS_TOOL} without a title.",
                        state.title
                    )));
                }
                state.title_fixed = true;
                if state.title != title {
                    state.note(ActivityKind::State, format!("Renamed \"{title}\""));
                    renamed = Some(std::mem::replace(&mut state.title, title));
                }
            }
            if state.caption != caption {
                state.note(ActivityKind::Notice, caption.clone());
            }
            state.caption = caption;
            if answer.is_some() {
                state.answer = answer;
            }
            state.progress = match (args.done, args.total) {
                (Some(done), Some(total)) if total > 0 => Some(Progress {
                    done: done.min(total),
                    total,
                }),
                _ => None,
            };
            state.updated_at = Timestamp::now();
        }
        if let Some(was) = renamed {
            let title = task.lock().title.clone();
            let mut event = self.task_event(
                task,
                EventKind::TaskStateChanged,
                format!("Renamed: {title}"),
            );
            event.detail = serde_json::json!({ "title": title, "was": was });
            self.audit(event);
        }
        self.publish(task);
        Ok(CallToolResult::ok("ok", Value::Null))
    }
}

/// Counts a call as running for as long as it lives.
struct InFlight<'a>(&'a AtomicUsize);

impl<'a> InFlight<'a> {
    fn enter(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The most of an item's text an ask shows, in lines; the rest is counted.
const REVIEW_TEXT_LINES: usize = 60;

/// Each item's text as the review shows it: by line, at most
/// [`REVIEW_TEXT_LINES`] of each, then how many more there are.
fn review_texts(texts: &[String]) -> Vec<Vec<String>> {
    texts
        .iter()
        .map(|text| {
            let lines: Vec<&str> = text.trim_end().lines().collect();
            let mut shown: Vec<String> = lines
                .iter()
                .take(REVIEW_TEXT_LINES)
                .map(|l| (*l).to_owned())
                .collect();
            if lines.len() > REVIEW_TEXT_LINES {
                shown.push(format!(
                    "… and {} more lines",
                    lines.len() - REVIEW_TEXT_LINES
                ));
            }
            shown
        })
        .collect()
}

/// `args` for `routines.save` with `from_this_task`: the flag taken off and
/// the script compiled from the task's calls so far in its place (ADR 0046).
/// Any other call's, as they came.
fn task_script(task: &Task, def: &ActionDef, mut args: Value) -> Result<Value, String> {
    let asked = def.name.as_str() == glass_app_routines::SAVE
        && args.get("from_this_task").and_then(Value::as_bool) == Some(true);
    if !asked {
        return Ok(args);
    }
    let taken = task.lock().trace.taken();
    let replay = taken.compile()?;
    if let Some(fields) = args.as_object_mut() {
        fields.remove("from_this_task");
        fields.insert("script".into(), Value::String(replay.script));
    }
    Ok(args)
}
