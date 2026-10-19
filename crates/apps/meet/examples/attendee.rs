//! A scripted attendee for meetings' end-to-end tests (ADR 0030 §6):
//! someone on another computer, with the same store, sealing and console
//! client as glassd. It shares `colleague`'s state folder, so it is the
//! same person with the same member key. Development only: it takes IT's
//! compliance key from the console's lookup answer rather than from a
//! signed release.
//!
//! ```text
//! attendee --enrollment build/console-e2e/console/enrollment.json \
//!          --state build/console-e2e/colleague --as priya <command>
//!   list               the meetings they're in, as JSON
//!   join <meeting>     join with the WebRTC offer on stdin: the answer,
//!                      the media key and their seat, as JSON
//!   leave <meeting>    leave it
//! ```

// A command-line tool: its answer is what it prints.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::Read;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use glass_actions::ActionProvider;
use glass_app_chat::{ChatHandle, ChatProvider};
use glass_app_meet::{MeetHandle, MeetProvider};
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
            eprintln!("attendee: {e}");
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
            "usage: attendee --enrollment FILE --state DIR --as LOGIN list|join|leave …".into(),
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
    let chat = ChatHandle::new(state);
    // The same computer name as `colleague`, so the member key's checks hold.
    chat.connect(org, &format!("colleague-{login}"), Arc::new(client));
    chat.set_compliance(&release.compliance, &release.fingerprint)?;
    // Catch up first: the meeting is a conversation like any other.
    chat.sync_once(0).map_err(|e| e.to_string())?;
    let meetings = Arc::new(ChatProvider::new(chat, DirectoryHandle::new()));
    let link = MeetHandle::new();
    link.connect(Arc::new(ConsoleClient::new(url, token, true)?));
    let meet = MeetProvider::new(meetings, link, DirectoryHandle::new());
    let call = |action: &str, input: Value| {
        let mut ctx = glass_actions::testing::context(Path::new("/nonexistent"));
        ctx.principal.on_behalf_of.name = login.clone();
        ctx.principal.agent = "meet".parse().expect("static id");
        meet.invoke(&ctx, &action.parse().expect("static name"), &input)
            .map_err(|e| e.to_string())
    };
    match rest
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["list"] => Ok(call("meet.list", json!({}))?.data["meetings"].to_string()),
        ["join", meeting] => {
            let mut offer = String::new();
            std::io::stdin()
                .read_to_string(&mut offer)
                .map_err(|e| format!("reading the offer: {e}"))?;
            let joined = call("meet.join", json!({ "meeting": meeting, "offer": offer }))?;
            Ok(joined.data.to_string())
        }
        ["leave", meeting] => Ok(call("meet.leave", json!({ "meeting": meeting }))?.text),
        _ => Err("list | join <meeting> (offer on stdin) | leave <meeting>".into()),
    }
}
