//! End-user latency budgets (`docs/dev/performance.md`): how long the things
//! a person waits on take on a live glassd. The VM emulates its CPU and is
//! several times slower than a laptop, so a PRD target that holds here holds
//! with room to spare on real hardware, and the other budgets are ceilings
//! that catch a change making something slower.

use std::future::Future;
use std::time::{Duration, Instant};

use anyhow::{Context, ensure};
use glass_api::{
    ActivityKind, ApprovalAnswer, ScopeRequest, TaskRequest, TaskView, TileState, methods,
};
use glass_audit::EventKind;
use glass_types::{TaskId, Tier};
use serde_json::Value;

use crate::harness::Harness;

const SAMPLES: usize = 20;
/// For measurements that take most of a second each.
const SLOW_SAMPLES: usize = 8;
const LONG: Duration = Duration::from_secs(180);

/// One budget: what the person waits for, the requirement it comes from, and
/// the limit on the 95th percentile.
struct Budget {
    what: &'static str,
    source: &'static str,
    p95: Duration,
}

const PLAN: Budget = Budget {
    what: "handoff card (task.plan)",
    source: "ceiling",
    p95: Duration::from_millis(200),
};
const TILES: Budget = Budget {
    what: "tiles (task.list)",
    source: "ceiling",
    p95: Duration::from_millis(100),
};
const EXPANDED: Budget = Budget {
    what: "expanded view (task.get)",
    source: "AW-10",
    p95: Duration::from_millis(200),
};
const FIRST_STEP: Budget = Budget {
    what: "start to first step",
    source: "ceiling",
    p95: Duration::from_secs(4),
};
const READ: Budget = Budget {
    what: "action, no snapshot",
    source: "ceiling",
    p95: Duration::from_millis(100),
};
const WRITE: Budget = Budget {
    what: "action with snapshot first",
    source: "ceiling",
    p95: Duration::from_millis(750),
};
const TAKE_OVER: Budget = Budget {
    what: "take over",
    source: "AW-20",
    p95: Duration::from_secs(1),
};
const UNDO_PREVIEW: Budget = Budget {
    what: "undo preview",
    source: "ceiling",
    // Reads only the files the task changed (ADR 0017); it took 0.8 to 1.3 s
    // when it ran `zfs diff`. Set before the VM measured the new code: lower
    // it to about 1.5x what the VM shows.
    p95: Duration::from_secs(1),
};

pub async fn perf(h: &Harness) -> anyhow::Result<()> {
    let ws = h.workspace("perf")?;
    let request = TaskRequest {
        agent: "invoice-demo".parse().map_err(|e| anyhow::anyhow!("{e}"))?,
        prompt: "Reconcile invoices".into(),
        scope: ScopeRequest {
            read: vec![ws.invoices.clone()],
            write: vec![ws.reports.clone()],
            restrict: Vec::new(),
        },
        title: None,
    };
    let mut results = Vec::new();

    results.push(measure(
        &PLAN,
        timed(SAMPLES, || h.call::<Value>(methods::TASK_PLAN, &request)).await?,
    ));

    // Three runs; the last stays waiting on its ask for the measurements
    // below.
    let mut first_steps = Vec::new();
    let mut tasks = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        let view: TaskView = h.call(methods::TASK_START, &request).await?;
        first_step(h, view.id).await?;
        first_steps.push(started.elapsed());
        h.wait_for(view.id, TileState::NeedsYou, LONG).await?;
        if let Some(&earlier) = tasks.last() {
            finish(h, earlier).await?;
        }
        tasks.push(view.id);
    }
    let task = *tasks.last().context("a task ran")?;
    results.push(measure(&FIRST_STEP, first_steps));

    results.push(measure(
        &TILES,
        timed(SAMPLES, || {
            h.call::<Vec<TaskView>>(methods::TASK_LIST, Value::Null)
        })
        .await?,
    ));
    results.push(measure(&EXPANDED, timed(SAMPLES, || h.detail(task)).await?));

    let mut take_overs = Vec::new();
    for _ in 0..5 {
        let started = Instant::now();
        let view = h.act(methods::TASK_TAKE_OVER, task).await?;
        take_overs.push(started.elapsed());
        ensure!(view.state == TileState::YouAreDriving, "{:?}", view.state);
        h.act(methods::TASK_HAND_BACK, task).await?;
    }
    results.push(measure(&TAKE_OVER, take_overs));

    // The OS's share of each action, from the audit trail. Asks are left
    // out: their time is mostly the person deciding.
    let (mut reads, mut writes) = (Vec::new(), Vec::new());
    for &task in &tasks {
        for record in h.audit(task).await? {
            let event = record.event;
            let asked = event.decision.as_ref().is_some_and(|d| d.tier == Tier::Ask);
            let Some(ms) = event.detail["elapsed_ms"].as_f64() else {
                continue;
            };
            if event.kind != EventKind::Action || asked {
                continue;
            }
            let took = Duration::from_secs_f64(ms / 1000.0);
            if event.snapshot.is_some() {
                writes.push(took);
            } else {
                reads.push(took);
            }
        }
    }
    ensure!(
        !reads.is_empty() && !writes.is_empty(),
        "the tasks read and wrote"
    );
    results.push(measure(&READ, reads));
    results.push(measure(&WRITE, writes));

    finish(h, task).await?;
    results.push(measure(
        &UNDO_PREVIEW,
        timed(SLOW_SAMPLES, || h.undo(task, true)).await?,
    ));
    for task in tasks {
        h.undo(task, false).await?;
    }

    let over: Vec<&str> = results.iter().filter(|r| !r.ok).map(|r| r.what).collect();
    ensure!(over.is_empty(), "over budget: {}", over.join(", "));
    Ok(())
}

struct Measured {
    what: &'static str,
    ok: bool,
}

/// Prints one line per budget and says whether the 95th percentile fits.
fn measure(budget: &Budget, mut samples: Vec<Duration>) -> Measured {
    samples.sort();
    let at =
        |q: f64| samples[((samples.len() as f64 * q).ceil() as usize).clamp(1, samples.len()) - 1];
    let (p50, p95) = (at(0.5), at(0.95));
    let ok = p95 <= budget.p95;
    println!(
        "    {} {:<30} p50 {:>7}  p95 {:>7}  budget {:>7}  ({}, n={})",
        if ok { "ok  " } else { "OVER" },
        budget.what,
        ms(p50),
        ms(p95),
        ms(budget.p95),
        budget.source,
        samples.len(),
    );
    Measured {
        what: budget.what,
        ok,
    }
}

fn ms(d: Duration) -> String {
    format!("{:.1}ms", d.as_secs_f64() * 1000.0)
}

/// Runs `call` `n` times, one after another, and returns each duration.
async fn timed<T, F, Fut>(n: usize, mut call: F) -> anyhow::Result<Vec<Duration>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        let started = Instant::now();
        call().await?;
        samples.push(started.elapsed());
    }
    Ok(samples)
}

/// Waits for the agent's first action to show in the activity log, polling
/// often enough not to blur the measurement.
async fn first_step(h: &Harness, task: TaskId) -> anyhow::Result<()> {
    let deadline = Instant::now() + LONG;
    while Instant::now() < deadline {
        let detail = h.detail(task).await?;
        if detail
            .activity
            .iter()
            .any(|a| a.kind == ActivityKind::Action)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    anyhow::bail!("no step within {LONG:?}")
}

async fn finish(h: &Harness, task: TaskId) -> anyhow::Result<()> {
    let ask = h.the_ask(task).await?;
    h.answer(&ask, ApprovalAnswer::Deny).await?;
    h.wait_for(task, TileState::Done, LONG).await?;
    Ok(())
}
