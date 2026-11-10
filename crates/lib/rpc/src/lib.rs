//! The one wire format every Glasshouse socket speaks: JSON-RPC 2.0, one
//! message per line, over Unix domain sockets.
//!
//! - The control socket (`glassctl`, the desktop shell) carries the
//!   `glass-api` methods.
//! - Each task's agent socket is an MCP server ([`mcp`]); MCP's stdio
//!   transport uses the same newline framing, so bridging is byte-for-byte.
//!
//! Servers learn the caller's uid from the kernel ([`PeerCred`]) and
//! authorize on that, never on anything the client claims.

mod client;
pub mod framing;
pub mod mcp;
mod message;
mod server;

pub use client::{Client, ClientError};
pub use message::{
    Id, Message, Request, Response, RpcError, Version, codes, parse_params, to_result,
};
pub use server::{
    Connection, Handler, Limits, MAX_IN_FLIGHT, PeerCred, serve, serve_connection, serve_within,
};

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::net::UnixListener;

    use super::*;

    struct Echo;

    impl Handler for Echo {
        async fn call(
            &self,
            conn: Arc<Connection>,
            method: String,
            params: Value,
        ) -> Result<Value, RpcError> {
            match method.as_str() {
                "echo" => Ok(params),
                "whoami" => Ok(json!({ "uid": conn.peer().uid })),
                "slow" => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    Ok(json!("slow"))
                }
                "subscribe" => {
                    for i in 0..3 {
                        conn.notify("tick", json!(i)).await;
                    }
                    Ok(json!("subscribed"))
                }
                // More than a message may carry.
                "huge" => Ok(json!("x".repeat(framing::MAX_MESSAGE_BYTES + 1))),
                "panic" => panic!("a handler that panics, for the test"),
                other => Err(RpcError::method_not_found(other)),
            }
        }
    }

    async fn start() -> (tempfile::TempDir, Client) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(serve(listener, Arc::new(Echo)));
        let client = Client::connect(&path).await.unwrap();
        (dir, client)
    }

    #[tokio::test]
    async fn calls_round_trip_with_peer_credentials() {
        use std::os::unix::fs::MetadataExt;

        let (dir, client) = start().await;
        let v: Value = client.call("echo", json!({"a": [1, 2]})).await.unwrap();
        assert_eq!(v, json!({"a": [1, 2]}));
        // The server sees this process's uid, which also owns the temp dir.
        let me: Value = client.call("whoami", Value::Null).await.unwrap();
        assert_eq!(
            me["uid"],
            json!(std::fs::metadata(dir.path()).unwrap().uid())
        );
    }

    #[tokio::test]
    async fn errors_come_back_typed() {
        let (_dir, client) = start().await;
        let err = client.call::<Value>("nope", Value::Null).await.unwrap_err();
        assert!(matches!(
            err,
            ClientError::Rpc(RpcError {
                code: codes::METHOD_NOT_FOUND,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn slow_calls_do_not_block_fast_ones() {
        let (_dir, client) = start().await;
        let client = Arc::new(client);
        let slow = tokio::spawn({
            let client = Arc::clone(&client);
            async move { client.call::<Value>("slow", Value::Null).await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let started = std::time::Instant::now();
        let _: Value = client.call("echo", json!(1)).await.unwrap();
        assert!(started.elapsed() < Duration::from_millis(150));
        assert_eq!(slow.await.unwrap().unwrap(), json!("slow"));
    }

    #[tokio::test]
    async fn a_reply_too_large_to_send_fails_its_call_and_no_other() {
        let (_dir, client) = start().await;
        let client = Arc::new(client);
        // Under way when the large reply is refused, and answered after it.
        let slow = tokio::spawn({
            let client = Arc::clone(&client);
            async move { client.call::<Value>("slow", Value::Null).await }
        });
        let err = client.call::<Value>("huge", Value::Null).await.unwrap_err();
        assert!(
            matches!(&err, ClientError::Rpc(RpcError { code: codes::INTERNAL_ERROR, message, .. })
                if message.contains("ask for less")),
            "{err:?}"
        );
        assert_eq!(slow.await.unwrap().unwrap(), json!("slow"));
        let v: Value = client.call("echo", json!("still here")).await.unwrap();
        assert_eq!(v, json!("still here"));
    }

    #[tokio::test]
    async fn a_handler_that_panics_answers_with_an_error() {
        let (_dir, client) = start().await;
        let err = client
            .call::<Value>("panic", Value::Null)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                ClientError::Rpc(RpcError {
                    code: codes::INTERNAL_ERROR,
                    ..
                })
            ),
            "{err:?}"
        );
        let v: Value = client.call("echo", json!(1)).await.unwrap();
        assert_eq!(v, json!(1));
    }

    #[tokio::test]
    async fn connections_past_the_limit_are_closed_and_a_freed_place_is_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let limits = Limits {
            connections: 2,
            per_user: 2,
            shared_in_flight: true,
        };
        tokio::spawn(serve_within(listener, Arc::new(Echo), limits));
        let first = Client::connect(&path).await.unwrap();
        let second = Client::connect(&path).await.unwrap();
        for client in [&first, &second] {
            let v: Value = client.call("echo", json!(1)).await.unwrap();
            assert_eq!(v, json!(1));
        }
        // A third connects (the kernel queues it) and is hung up on.
        let third = Client::connect(&path).await.unwrap();
        let err = third.call::<Value>("echo", json!(1)).await.unwrap_err();
        assert!(matches!(err, ClientError::Closed), "{err:?}");
        // The two that are open are untouched.
        let v: Value = first.call("echo", json!(2)).await.unwrap();
        assert_eq!(v, json!(2));
        // One hangs up: its place is free again, as soon as the server
        // has seen it go.
        drop(second);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let next = Client::connect(&path).await.unwrap();
            match next.call::<Value>("echo", json!(3)).await {
                Ok(v) => break assert_eq!(v, json!(3)),
                Err(ClientError::Closed) if std::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(e) => panic!("the freed place wasn't given out: {e:?}"),
            }
        }
    }

    #[tokio::test]
    async fn notifications_reach_the_client() {
        let (_dir, client) = start().await;
        let _: Value = client.call("subscribe", Value::Null).await.unwrap();
        for i in 0..3 {
            let note = client.next_notification().await.unwrap();
            assert_eq!(note.method, "tick");
            assert_eq!(note.params, Some(json!(i)));
        }
    }
}
