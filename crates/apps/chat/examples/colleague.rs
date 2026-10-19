//! A scripted colleague for Chat's end-to-end tests (ADR 0030): someone on
//! another computer, with the same store, sealing and console client as
//! glassd. Development only: it takes IT's compliance key from the
//! console's lookup answer rather than from a signed release.
//!
//! ```text
//! colleague --enrollment build/console-e2e/console/enrollment.json \
//!           --state build/console-e2e/colleague --as priya <command>
//!   open                 open Chat once, so others can message them
//!   say <login> <text>   message <login>
//!   read <login>         the direct messages with <login>, as JSON
//! ```

// A command-line tool: its answer is what it prints.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use glass_actions::ActionProvider;
use glass_app_chat::{ChatHandle, ChatProvider};
use glass_gateway_providers::ConsoleClient;
use glass_managed::chat::{ChatLink, LookupKeys};
use glass_managed::{ChatRelease, DirectoryHandle};
use serde_json::{Value, json};

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(out) => {
            println!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("colleague: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<String, String> {
    let (mut enrollment, mut state, mut login) = (None, None, None);
    let mut rest = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--enrollment" => enrollment = args.next(),
            "--state" => state = args.next(),
            "--as" => login = args.next(),
            _ => rest.push(arg),
        }
    }
    let (Some(enrollment), Some(state), Some(login)) = (enrollment, state, login) else {
        return Err(
            "usage: colleague --enrollment FILE --state DIR --as LOGIN open|say|read …".into(),
        );
    };
    let e: Value = serde_json::from_str(
        &std::fs::read_to_string(&enrollment).map_err(|e| format!("{enrollment}: {e}"))?,
    )
    .map_err(|e| e.to_string())?;
    let (url, org, token) = (
        e["url"].as_str().unwrap_or_default(),
        e["org"].as_str().unwrap_or_default(),
        e["token"].as_str().unwrap_or_default(),
    );
    let client = ConsoleClient::new(url, token, true)?;
    let compliance = client
        .lookup(&LookupKeys { logins: Vec::new() })
        .map_err(|e| e.to_string())?
        .compliance;
    let key = glass_crypto::PublicKey::from_bytes(
        &hex::decode(&compliance).map_err(|_| "the compliance key isn't hex")?,
    )
    .map_err(|e| e.to_string())?;
    let release = ChatRelease::new(&key);
    let handle = ChatHandle::new(state);
    handle.connect(org, &format!("colleague-{login}"), Arc::new(client));
    handle.set_compliance(&release.compliance, &release.fingerprint)?;
    let chat = ChatProvider::new(handle.clone(), DirectoryHandle::new());
    let call = |action: &str, input: Value| {
        let mut ctx = glass_actions::testing::context(Path::new("/nonexistent"));
        ctx.principal.on_behalf_of.name = login.clone();
        ctx.principal.agent = "chat".parse().expect("static id");
        chat.invoke(&ctx, &action.parse().expect("static name"), &input)
            .map_err(|e| e.to_string())
    };
    // Opening Chat makes and registers their key, then catches up.
    call("chat.list", json!({}))?;
    handle.sync_once(0).map_err(|e| e.to_string())?;
    match rest
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["open"] => Ok(format!("{login} has Chat open")),
        ["say", to, text] => Ok(call(
            "chat.post",
            json!({ "posts": [{ "to": [to], "text": text }] }),
        )?
        .text),
        ["read", with] => {
            let list = call("chat.list", json!({}))?;
            let id = list.data["conversations"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|c| {
                    c["kind"] == "direct"
                        && c["people"]
                            .as_array()
                            .is_some_and(|p| p.iter().any(|x| x["login"] == *with))
                })
                .map(|c| c["id"].clone())
                .ok_or_else(|| format!("no messages with {with}"))?;
            let read = call("chat.read", json!({ "conversation": id }))?;
            Ok(read.data["messages"].to_string())
        }
        _ => Err("say <login> <text> | read <login> | open".into()),
    }
}
