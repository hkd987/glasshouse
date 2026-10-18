//! A scripted invoice-reconciliation agent.
//!
//! It stands in for a model-backed agent so the whole delegation loop can be
//! demonstrated and tested: Auto reads, a Notify write, an Auto draft, and a
//! batched Ask to send reminders. It reacts to refusals the way a good agent
//! should: a declined or blocked step is noted and skipped, never retried.
//!
//! Invoice files are `Key: Value` lines (Invoice, Vendor, Email, Amount, Due,
//! Status).
//!
//! Users tailor it with a layer (TL-2) through the settings its catalog entry
//! declares: `flag_over` (flag invoices over an amount, in dollars),
//! `remind_vendors` (whether to ask to email vendors) and `instructions`.

use std::process::ExitCode;

use anyhow::{Context, bail};
use glass_agent_sdk::{Agent, CallToolResult};
use serde_json::{Value, json};

const AP_TEAM: &str = "ap-team@example.com";

/// What the user's layer changed, from the agent's settings.
#[derive(Debug)]
struct Settings {
    flag_over_cents: Option<u64>,
    remind_vendors: bool,
    instructions: Option<String>,
}

impl Settings {
    fn of(agent: &Agent) -> Self {
        Self {
            flag_over_cents: agent
                .setting("flag_over")
                .and_then(Value::as_f64)
                .filter(|d| *d >= 0.0)
                .map(|d| (d * 100.0).round() as u64),
            remind_vendors: agent
                .setting("remind_vendors")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            instructions: agent
                .setting("instructions")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        }
    }

    fn flagged(&self, invoice: &Invoice) -> bool {
        self.flag_over_cents
            .is_some_and(|limit| invoice.amount_cents > limit)
    }
}

#[derive(Debug, Default)]
struct Invoice {
    file: String,
    number: String,
    vendor: String,
    email: String,
    amount_cents: u64,
    due: String,
    unpaid: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("glass-demo-agent: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let agent = Agent::from_env()
        .await
        .context("connecting to the task socket")?;
    eprintln!("task {}: {}", agent.task_id, agent.prompt);
    let settings = Settings::of(&agent);
    eprintln!("settings: {settings:?}");
    // A scripted agent can't follow free-form instructions; it shows it
    // got them. Model-backed agents read them from their instructions.
    let opening = match &settings.instructions {
        Some(words) => format!("Looking at what I was given (noted: {words})"),
        None => "Looking at what I was given".to_owned(),
    };
    agent.progress(&opening, None, None).await?;

    let folders = agent.call("files_list", json!({})).await?;
    let folders = folders.structured_content.unwrap_or(Value::Null);
    let folders = folders["folders"].as_array().cloned().unwrap_or_default();
    let name_of = |f: &Value| f["name"].as_str().unwrap_or_default().to_owned();
    let source = folders
        .iter()
        .find(|f| f["writable"] == json!(false))
        .or_else(|| folders.first())
        .map(name_of)
        .context("no folder to read invoices from; give the task one")?;
    let target = folders
        .iter()
        .find(|f| f["writable"] == json!(true))
        .map(name_of);

    let invoices = read_invoices(&agent, &source).await?;
    if invoices.is_empty() {
        bail!("no invoices (.txt files) in {source}");
    }
    let unpaid: Vec<&Invoice> = invoices.iter().filter(|i| i.unpaid).collect();
    let unpaid_total: u64 = unpaid.iter().map(|i| i.amount_cents).sum();

    match &target {
        Some(target) => {
            agent
                .progress("Writing the reconciliation summary", None, None)
                .await?;
            let path = format!("{target}/reconciliation.csv");
            let written = agent
                .call(
                    "files_write",
                    json!({ "path": path, "content": summary_csv(&invoices, &settings) }),
                )
                .await?;
            report_step(&written, "summary");
        }
        None => eprintln!("no writable folder; skipping the summary file"),
    }

    if agent.has_tool("mail_draft") {
        agent
            .progress("Drafting a note to the AP team", None, None)
            .await?;
        let flagged: Vec<String> = invoices
            .iter()
            .filter(|i| settings.flagged(i))
            .map(|i| format!("{} {} {}", i.number, i.vendor, money(i.amount_cents)))
            .collect();
        let flagged = match settings.flag_over_cents {
            Some(limit) if !flagged.is_empty() => {
                format!("\nOver {}: {}\n", money(limit), flagged.join(", "))
            }
            _ => String::new(),
        };
        let body = format!(
            "Reconciled {} invoices. {} unpaid, totaling {}.\n{flagged}\n{}\n",
            invoices.len(),
            unpaid.len(),
            money(unpaid_total),
            unpaid
                .iter()
                .map(|i| format!(
                    "- {} {} {} (due {})",
                    i.number,
                    i.vendor,
                    money(i.amount_cents),
                    i.due
                ))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let draft = agent
            .call(
                "mail_draft",
                json!({ "to": [AP_TEAM], "subject": "Invoice reconciliation", "body": body }),
            )
            .await?;
        report_step(&draft, "draft");
    }

    if !settings.remind_vendors {
        eprintln!("reminders: off in the user's layer");
    } else if !unpaid.is_empty() && agent.has_tool("mail_send") {
        agent
            .progress(
                &format!("Asking to email {} vendors", unpaid.len()),
                None,
                None,
            )
            .await?;
        let messages: Vec<Value> = unpaid
            .iter()
            .map(|i| {
                json!({
                    "to": [i.email],
                    "subject": format!("Payment reminder: {}", i.number),
                    "body": format!("Hello {},\n\nOur records show invoice {} for {} was due {}. Please disregard this if it is already settled.\n\nAccounts Payable\n", i.vendor, i.number, money(i.amount_cents), i.due),
                })
            })
            .collect();
        let sent = agent
            .call("mail_send", json!({ "messages": messages }))
            .await?;
        report_step(&sent, "reminders");
    }

    agent
        .progress(
            &format!(
                "Done: {} invoices, {} unpaid ({})",
                invoices.len(),
                unpaid.len(),
                money(unpaid_total)
            ),
            None,
            None,
        )
        .await?;
    Ok(())
}

async fn read_invoices(agent: &Agent, folder: &str) -> anyhow::Result<Vec<Invoice>> {
    let listing = agent.call("files_list", json!({ "path": folder })).await?;
    if listing.is_error {
        bail!("listing {folder}: {}", listing.text());
    }
    let entries = listing.structured_content.unwrap_or(Value::Null);
    let files: Vec<String> = entries["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["dir"] == json!(false))
        .filter_map(|e| e["name"].as_str())
        .filter(|n| n.ends_with(".txt"))
        .map(str::to_owned)
        .collect();
    let mut invoices = Vec::new();
    for (index, name) in files.iter().enumerate() {
        agent
            .progress(
                "Reading invoices",
                Some(index as u32 + 1),
                Some(files.len() as u32),
            )
            .await?;
        let read = agent
            .call("files_read", json!({ "path": format!("{folder}/{name}") }))
            .await?;
        if read.is_error {
            eprintln!("skipping {name}: {}", read.text());
            continue;
        }
        invoices.push(parse(name, &read.text()));
    }
    Ok(invoices)
}

fn parse(file: &str, text: &str) -> Invoice {
    let mut invoice = Invoice {
        file: file.to_owned(),
        ..Invoice::default()
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "invoice" => invoice.number = value.into(),
            "vendor" => invoice.vendor = value.into(),
            "email" => invoice.email = value.into(),
            "amount" => invoice.amount_cents = cents(value),
            "due" => invoice.due = value.into(),
            "status" => invoice.unpaid = value.eq_ignore_ascii_case("unpaid"),
            _ => {}
        }
    }
    if invoice.number.is_empty() {
        invoice.number = file.trim_end_matches(".txt").into();
    }
    invoice
}

fn cents(amount: &str) -> u64 {
    let cleaned: String = amount
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let (whole, frac) = cleaned.split_once('.').unwrap_or((&cleaned, "0"));
    let frac = format!("{frac:0<2}");
    whole.parse::<u64>().unwrap_or(0) * 100 + frac[..2].parse::<u64>().unwrap_or(0)
}

fn money(cents: u64) -> String {
    format!("${}.{:02}", cents / 100, cents % 100)
}

/// One line per invoice and a total. A `flagged` column is added when the
/// user's layer sets `flag_over`.
fn summary_csv(invoices: &[Invoice], settings: &Settings) -> String {
    let flagging = settings.flag_over_cents.is_some();
    let mut out = String::from("invoice,vendor,amount,due,status,file");
    out.push_str(if flagging { ",flagged\n" } else { "\n" });
    for i in invoices {
        out.push_str(&format!(
            "{},{},{}.{:02},{},{},{}",
            i.number,
            i.vendor.replace(',', " "),
            i.amount_cents / 100,
            i.amount_cents % 100,
            i.due,
            if i.unpaid { "unpaid" } else { "paid" },
            i.file
        ));
        if flagging {
            out.push_str(if settings.flagged(i) { ",yes" } else { "," });
        }
        out.push('\n');
    }
    let total: u64 = invoices.iter().map(|i| i.amount_cents).sum();
    out.push_str(&format!("TOTAL,,{}.{:02},,,", total / 100, total % 100));
    out.push_str(if flagging { ",\n" } else { "\n" });
    out
}

fn report_step(result: &CallToolResult, step: &str) {
    if result.is_error {
        eprintln!("{step}: not done: {}", result.text());
    } else {
        eprintln!("{step}: {}", result.text());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_invoices_and_money() {
        let inv = parse(
            "a.txt",
            "Invoice: INV-1\nVendor: Acme\nEmail: a@acme.example\nAmount: 1,840.5\nDue: 2026-10-01\nStatus: Unpaid\n",
        );
        assert_eq!(inv.number, "INV-1");
        assert_eq!(inv.amount_cents, 184_050);
        assert!(inv.unpaid);
        assert_eq!(money(184_050), "$1840.50");
        assert_eq!(cents("75"), 7_500);
    }

    #[test]
    fn a_layer_adds_a_flagged_column() {
        let invoices = [
            parse("a.txt", "Invoice: A\nAmount: 1500\n"),
            parse("b.txt", "Invoice: B\nAmount: 20\n"),
        ];
        let plain = Settings {
            flag_over_cents: None,
            remind_vendors: true,
            instructions: None,
        };
        assert!(
            summary_csv(&invoices, &plain).starts_with("invoice,vendor,amount,due,status,file\n")
        );
        let flagging = Settings {
            flag_over_cents: Some(100_000),
            ..plain
        };
        let csv = summary_csv(&invoices, &flagging);
        let lines: Vec<&str> = csv.lines().collect();
        assert!(lines[0].ends_with(",flagged"));
        assert!(lines[1].starts_with("A,") && lines[1].ends_with(",yes"));
        assert!(lines[2].starts_with("B,") && lines[2].ends_with(','));
        assert!(lines[3].starts_with("TOTAL,"));
    }
}
