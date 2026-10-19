//! `glass-e2e`: end-to-end scenarios against a live glassd.
//!
//! ```text
//! glass-e2e all                 every single-user scenario
//! glass-e2e invoices labels     just these, one after another
//! glass-e2e perf                end-user latency budgets
//! glass-e2e load                the same reads while two busy agents run
//! glass-e2e hold                start a waiting task, print its id
//! glass-e2e isolation <id>      (as another user) that task is invisible
//! glass-e2e release <id>        stop it
//! glass-e2e sleeping|awake <id> its state after a system suspend / resume
//! glass-e2e models-policy       print the policy the `models` scenario needs
//! glass-e2e models              the model gateway (run-vm.sh installs that policy first)
//! ```
//!
//! `all` runs the scenarios that share nothing side by side, then the rest one
//! at a time. `tests/e2e/run-vm.sh` runs the full suite in the FreeBSD VM,
//! including the parts that need several users and root.

#![allow(clippy::print_stdout)]

mod harness;
mod models;
mod perf;
mod scenarios;
mod studio;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use glass_types::TaskId;
use tokio::task::JoinSet;

use crate::harness::Harness;

/// Scenarios that can run side by side: none of them sends mail (others
/// count the Sent folder), changes the user's layers, or measures latency,
/// and each has at most one live task at a time.
const PARALLEL: &[&str] = &[
    "scope",
    "containment",
    "decline",
    "labels",
    "stop",
    "takeover",
    "changes",
    "it",
    "harness",
];
/// Run after the parallel ones, one at a time: `invoices` sends mail,
/// `tailor` changes the invoice agent's layer, `studio` times a bundle, and
/// `perf` and `load` must run alone.
const SERIAL: &[&str] = &["invoices", "tailor", "studio", "perf", "load"];
/// Live tasks one user may have is 8 (glassd.toml); stay under it.
const AT_ONCE: usize = 6;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("models-policy") {
        return match models::policy() {
            Ok(text) => {
                print!("{text}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("glass-e2e: {e:#}");
                ExitCode::FAILURE
            }
        };
    }
    let harness = match Harness::connect().await {
        Ok(h) => Arc::new(h),
        Err(e) => {
            eprintln!("glass-e2e: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    let result = match args.first().map(String::as_str) {
        Some("hold") => scenarios::hold(&harness).await,
        Some(cmd @ ("isolation" | "release" | "sleeping" | "awake")) => {
            match args.get(1).and_then(|s| s.parse::<TaskId>().ok()) {
                Some(task) => {
                    let outcome = match cmd {
                        "isolation" => scenarios::isolation(&harness, task).await,
                        "sleeping" => scenarios::sleeping(&harness, task).await,
                        "awake" => scenarios::awake(&harness, task).await,
                        _ => scenarios::release(&harness, task).await,
                    };
                    report(cmd, outcome)
                }
                None => Err(anyhow::anyhow!("usage: glass-e2e {cmd} <task-id>")),
            }
        }
        _ => {
            let started = Instant::now();
            let (failed, total) = if args.is_empty() || args.iter().any(|a| a == "all") {
                let (parallel_failed, _) = run_parallel(&harness, PARALLEL).await;
                let (serial_failed, _) = run_serial(&harness, SERIAL.iter().copied()).await;
                (
                    parallel_failed + serial_failed,
                    PARALLEL.len() + SERIAL.len(),
                )
            } else {
                run_serial(&harness, args.iter().map(String::as_str)).await
            };
            println!(
                "{} of {total} scenarios passed in {:.0}s",
                total - failed,
                started.elapsed().as_secs_f64()
            );
            if failed == 0 {
                Ok(())
            } else {
                Err(anyhow::anyhow!("{failed} failed"))
            }
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("glass-e2e: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Runs `names` side by side, at most [`AT_ONCE`] at a time. Returns
/// (failed, run).
async fn run_parallel(harness: &Arc<Harness>, names: &[&'static str]) -> (usize, usize) {
    let mut queue = names.iter().copied();
    let mut running = JoinSet::new();
    let mut failed = 0;
    loop {
        while running.len() < AT_ONCE {
            let Some(name) = queue.next() else { break };
            let harness = Arc::clone(harness);
            running.spawn(async move { timed(name, &harness).await });
        }
        let Some(done) = running.join_next().await else {
            break;
        };
        match done {
            Ok(Ok(())) => {}
            Ok(Err(())) => failed += 1,
            Err(e) => {
                println!("FAIL scenario panicked: {e}");
                failed += 1;
            }
        }
    }
    (failed, names.len())
}

async fn run_serial<'a>(harness: &Harness, names: impl Iterator<Item = &'a str>) -> (usize, usize) {
    let (mut failed, mut total) = (0, 0);
    for name in names {
        total += 1;
        if timed(name, harness).await.is_err() {
            failed += 1;
        }
    }
    (failed, total)
}

/// Runs one scenario and prints PASS or FAIL with how long it took.
async fn timed(name: &str, harness: &Harness) -> Result<(), ()> {
    let started = Instant::now();
    let outcome = scenario(name, harness).await;
    let label = format!("{name} ({:.1}s)", started.elapsed().as_secs_f64());
    report(&label, outcome).map_err(|_| ())
}

async fn scenario(name: &str, h: &Harness) -> anyhow::Result<()> {
    match name {
        "scope" => scenarios::scope(h).await,
        "containment" => scenarios::containment(h).await,
        "invoices" => scenarios::invoices(h).await,
        "decline" => scenarios::decline(h).await,
        "labels" => scenarios::labels(h).await,
        "stop" => scenarios::stop(h).await,
        "takeover" => scenarios::takeover(h).await,
        "changes" => scenarios::changes(h).await,
        "it" => scenarios::it(h).await,
        "tailor" => scenarios::tailor(h).await,
        "harness" => scenarios::harness(h).await,
        "perf" => perf::perf(h).await,
        "models" => models::models(h).await,
        "studio" => studio::studio(h).await,
        "load" => perf::under_load(h).await,
        other => Err(anyhow::anyhow!("unknown scenario {other}")),
    }
}

fn report(name: &str, outcome: anyhow::Result<()>) -> anyhow::Result<()> {
    match &outcome {
        Ok(()) => println!("PASS {name}"),
        Err(e) => println!("FAIL {name}: {e:#}"),
    }
    outcome
}
