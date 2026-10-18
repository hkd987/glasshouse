//! A scripted invoice-reconciliation agent.
//!
//! It stands in for a model-backed agent so the whole delegation loop can be
//! demonstrated and tested: Auto reads, a Notify write, an Auto draft, and a
//! batched Ask to send reminders. It reacts to refusals the way a good agent
//! should: a declined or blocked step is noted and skipped, never retried.
//!
//! Invoice files are `Key: Value` lines (Invoice, Vendor, Email, Amount, Due,
//! Status).

use std::process::ExitCode;

use anyhow::{Context, bail};
use glass_agent_sdk::{Agent, CallToolResult};
use serde_json::{Value, json};

const AP_TEAM: &str = "ap-team@example.com";

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
    agent
        .progress("Looking at what I was given", None, None)
        .await?;

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
                    json!({ "path": path, "content": summary_csv(&invoices) }),
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
        let body = format!(
            "Reconciled {} invoices. {} unpaid, totaling {}.\n\n{}\n",
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

    if !unpaid.is_empty() && agent.has_tool("mail_send") {
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

fn summary_csv(invoices: &[Invoice]) -> String {
    let mut out = String::from("invoice,vendor,amount,due,status,file\n");
    for i in invoices {
        out.push_str(&format!(
            "{},{},{}.{:02},{},{},{}\n",
            i.number,
            i.vendor.replace(',', " "),
            i.amount_cents / 100,
            i.amount_cents % 100,
            i.due,
            if i.unpaid { "unpaid" } else { "paid" },
            i.file
        ));
    }
    let total: u64 = invoices.iter().map(|i| i.amount_cents).sum();
    out.push_str(&format!("TOTAL,,{}.{:02},,,\n", total / 100, total % 100));
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
}
