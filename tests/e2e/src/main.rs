//! `glass-e2e`: end-to-end scenarios against a live glassd.
//!
//! ```text
//! glass-e2e all                 every single-user scenario
//! glass-e2e invoices labels     just these
//! glass-e2e hold                start a waiting task, print its id
//! glass-e2e isolation <id>      (as another user) that task is invisible
//! glass-e2e release <id>        stop it
//! ```
//!
//! `tests/e2e/run-vm.sh` runs the full suite in the FreeBSD VM, including the
//! parts that need several users and root.

#![allow(clippy::print_stdout)]

mod harness;
mod scenarios;

use std::process::ExitCode;

use glass_types::TaskId;

use crate::harness::Harness;

const ALL: &[&str] = &[
    "scope",
    "containment",
    "invoices",
    "decline",
    "labels",
    "stop",
    "takeover",
];

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let harness = match Harness::connect().await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("glass-e2e: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    let result = match args.first().map(String::as_str) {
        Some("hold") => scenarios::hold(&harness).await,
        Some(cmd @ ("isolation" | "release")) => {
            match args.get(1).and_then(|s| s.parse::<TaskId>().ok()) {
                Some(task) if cmd == "isolation" => {
                    report("isolation", scenarios::isolation(&harness, task).await)
                }
                Some(task) => report("release", scenarios::release(&harness, task).await),
                None => Err(anyhow::anyhow!("usage: glass-e2e {cmd} <task-id>")),
            }
        }
        _ => {
            let wanted: Vec<&str> = if args.is_empty() || args.iter().any(|a| a == "all") {
                ALL.to_vec()
            } else {
                args.iter().map(String::as_str).collect()
            };
            let mut failed = 0;
            for name in &wanted {
                let outcome = match *name {
                    "scope" => scenarios::scope(&harness).await,
                    "containment" => scenarios::containment(&harness).await,
                    "invoices" => scenarios::invoices(&harness).await,
                    "decline" => scenarios::decline(&harness).await,
                    "labels" => scenarios::labels(&harness).await,
                    "stop" => scenarios::stop(&harness).await,
                    "takeover" => scenarios::takeover(&harness).await,
                    other => Err(anyhow::anyhow!("unknown scenario {other}")),
                };
                if report(name, outcome).is_err() {
                    failed += 1;
                }
            }
            println!(
                "{} of {} scenarios passed",
                wanted.len() - failed,
                wanted.len()
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

fn report(name: &str, outcome: anyhow::Result<()>) -> anyhow::Result<()> {
    match &outcome {
        Ok(()) => println!("PASS {name}"),
        Err(e) => println!("FAIL {name}: {e:#}"),
    }
    outcome
}
