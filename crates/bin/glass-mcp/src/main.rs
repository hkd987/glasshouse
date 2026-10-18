//! `glass-mcp`: an MCP stdio server that is really the task socket.
//!
//! Harnesses such as Claude Code and Codex launch MCP servers as child
//! processes speaking newline-delimited JSON-RPC on stdio. The task socket
//! speaks exactly that, so this bridge only copies bytes both ways (HX-1,
//! HX-6). Inside a task sandbox:
//!
//! ```text
//! claude mcp add glasshouse -- glass-mcp
//! ```
//!
//! The socket comes from `GLASS_AGENT_SOCKET` (set by glassd for every
//! agent), or the first argument.

use std::process::ExitCode;

use tokio::io::{self, AsyncWriteExt};
use tokio::net::UnixStream;

const SOCKET_ENV: &str = "GLASS_AGENT_SOCKET";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let Some(path) = std::env::args()
        .nth(1)
        .or_else(|| std::env::var(SOCKET_ENV).ok())
    else {
        eprintln!(
            "glass-mcp: no task socket; run inside a Glasshouse task or pass the socket path"
        );
        return ExitCode::from(2);
    };
    let stream = match UnixStream::connect(&path).await {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("glass-mcp: cannot connect to {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (mut from_socket, mut to_socket) = stream.into_split();
    let upstream = async {
        let copied = io::copy(&mut io::stdin(), &mut to_socket).await;
        // The client closed stdin: tell the server we're done sending.
        let _ = to_socket.shutdown().await;
        copied
    };
    let downstream = async {
        let mut stdout = io::stdout();
        let copied = io::copy(&mut from_socket, &mut stdout).await;
        let _ = stdout.flush().await;
        copied
    };
    match tokio::try_join!(upstream, downstream) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("glass-mcp: {e}");
            ExitCode::FAILURE
        }
    }
}
