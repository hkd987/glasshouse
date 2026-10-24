//! The private API: HTTP/1.1 and JSON on loopback, for the admin console
//! only. Computers never call it; their pages reach the relay with the
//! answer the console hands back.
//!
//! - `POST /rooms/{room}/participants` with `{"participant": <u32>,
//!   "offer": "<sdp>"}`, and optionally `"name": "<display name>"`: 200
//!   `{"answer": "<sdp>"}`. Creates the room on the first join; the same
//!   index again replaces that participant's connection; a 26th index is
//!   409 ([`SEATS`](crate::SEATS)). A bad offer is 400. The name, if given,
//!   is what the others' pages show with that person's video (the `peers`
//!   line's `names`); without one they show "Participant N".
//! - `GET /rooms/{room}`: 200 `{"participants": [<u32>, …]}`, who has a
//!   seat: admitted, and their connection not closed or dropped. A room
//!   nobody is in lists nobody. The console asks before it answers for a
//!   meeting's keys, so whoever dropped out counts as having left (ADR
//!   0052).
//! - `DELETE /rooms/{room}/participants/{participant}`: 204, and everyone
//!   left gets a `peers` message.
//! - `DELETE /rooms/{room}`: 204, and everyone in it is disconnected.
//! - `GET /health`: 200 `{"rooms": <n>, "participants": <n>}` once the
//!   media loop answers.
//!
//! An unknown room or participant is 404. Errors are `{"error": "..."}`.

use std::time::Duration;

use glass_http::{ReadError, Request, Response};
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::media::{Command, JoinError};

/// Room ids are the console's: unreserved URL characters only.
const ROOM_MAX: usize = 128;

pub(crate) async fn serve(listener: TcpListener, media: mpsc::Sender<Command>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(connection(stream, media.clone()));
            }
            Err(e) => {
                warn!("accepting an API connection: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn connection(mut stream: TcpStream, media: mpsc::Sender<Command>) {
    let response = match glass_http::read_request(&mut stream).await {
        Ok(request) => respond(&request, &media).await,
        Err(ReadError::Gone) => return,
        Err(ReadError::Bad(status, why)) => error(status, why),
    };
    if let Err(e) = glass_http::write_response(&mut stream, response, false).await {
        debug!("answering the API: {e}");
    }
}

async fn respond(request: &Request, media: &mpsc::Sender<Command>) -> Response {
    let path: Vec<&str> = request.path.trim_start_matches('/').split('/').collect();
    match (request.method.as_str(), path.as_slice()) {
        ("GET", ["health"]) => {
            let (reply, answer) = oneshot::channel();
            match ask(media, Command::Health { reply }, answer).await {
                Ok(health) => Response::json(200, &json!(health)),
                Err(response) => response,
            }
        }
        ("POST", ["rooms", room, "participants"]) => {
            let Some(room) = room_id(room) else {
                return error(400, "not a room id");
            };
            join(room, &request.body, media).await
        }
        ("DELETE", ["rooms", room, "participants", participant]) => {
            let Some(room) = room_id(room) else {
                return error(400, "not a room id");
            };
            let Ok(participant) = participant.parse::<u32>() else {
                return error(400, "a participant is a number");
            };
            let (reply, answer) = oneshot::channel();
            let command = Command::Leave {
                room,
                participant,
                reply,
            };
            match ask(media, command, answer).await {
                Ok(true) => no_content(),
                Ok(false) => error(404, "no such participant in the room"),
                Err(response) => response,
            }
        }
        ("GET", ["rooms", room]) => {
            let Some(room) = room_id(room) else {
                return error(400, "not a room id");
            };
            let (reply, answer) = oneshot::channel();
            match ask(media, Command::Room { room, reply }, answer).await {
                Ok(participants) => Response::json(200, &json!({ "participants": participants })),
                Err(response) => response,
            }
        }
        ("DELETE", ["rooms", room]) => {
            let Some(room) = room_id(room) else {
                return error(400, "not a room id");
            };
            let (reply, answer) = oneshot::channel();
            match ask(media, Command::CloseRoom { room, reply }, answer).await {
                Ok(true) => no_content(),
                Ok(false) => error(404, "no such room"),
                Err(response) => response,
            }
        }
        (_, ["health"] | ["rooms", _] | ["rooms", _, "participants"])
        | (_, ["rooms", _, "participants", _]) => error(405, "method not allowed"),
        _ => error(404, "no such route"),
    }
}

#[derive(serde::Deserialize)]
struct Join {
    participant: u32,
    offer: String,
    #[serde(default)]
    name: Option<String>,
}

async fn join(room: String, body: &[u8], media: &mpsc::Sender<Command>) -> Response {
    let Ok(Join {
        participant,
        offer,
        name,
    }) = serde_json::from_slice::<Join>(body)
    else {
        return error(
            400,
            "the body is {\"participant\": <index>, \"offer\": \"<sdp>\"}",
        );
    };
    let (reply, answer) = oneshot::channel();
    let command = Command::Join {
        room,
        participant,
        name,
        offer,
        reply,
    };
    match ask(media, command, answer).await {
        Ok(Ok(answer)) => Response::json(200, &json!({"answer": answer.to_sdp_string()})),
        Ok(Err(e @ JoinError::Full)) => error(409, &e.to_string()),
        Ok(Err(e @ JoinError::Offer(_))) => error(400, &e.to_string()),
        Err(response) => response,
    }
}

/// Sends `command` to the media loop and waits for its reply.
async fn ask<T>(
    media: &mpsc::Sender<Command>,
    command: Command,
    answer: oneshot::Receiver<T>,
) -> Result<T, Response> {
    let stopped = || error(503, "the media loop has stopped");
    media.send(command).await.map_err(|_| stopped())?;
    answer.await.map_err(|_| stopped())
}

fn room_id(room: &str) -> Option<String> {
    let ok = !room.is_empty()
        && room.len() <= ROOM_MAX
        && room
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~'));
    ok.then(|| room.to_owned())
}

fn error(status: u16, why: &str) -> Response {
    Response::json(status, &json!({"error": why}))
}

fn no_content() -> Response {
    Response {
        status: 204,
        headers: Vec::new(),
        body: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_ids_are_plain() {
        assert_eq!(
            room_id("0192f1c3-7a4e-7b1e"),
            Some("0192f1c3-7a4e-7b1e".into())
        );
        assert_eq!(room_id(""), None);
        assert_eq!(room_id("a%2Fb"), None);
        assert_eq!(room_id(".."), Some("..".into()));
        assert_eq!(room_id(&"x".repeat(ROOM_MAX + 1)), None);
    }

    #[tokio::test]
    async fn routes() {
        // A loop that has stopped: every command fails to send.
        let (media, inbox) = mpsc::channel(1);
        drop(inbox);
        let request = |method: &str, path: &str, body: &str| Request {
            method: method.into(),
            path: path.into(),
            query: String::new(),
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        };
        let status = |r: Response| r.status;
        assert_eq!(
            status(respond(&request("GET", "/health", ""), &media).await),
            503
        );
        assert_eq!(
            status(respond(&request("PUT", "/rooms/a", ""), &media).await),
            405
        );
        assert_eq!(
            status(respond(&request("GET", "/rooms/a", ""), &media).await),
            503
        );
        assert_eq!(
            status(respond(&request("PUT", "/rooms/a/participants/1", ""), &media).await),
            405
        );
        assert_eq!(
            status(respond(&request("GET", "/nope", ""), &media).await),
            404
        );
        assert_eq!(
            status(respond(&request("POST", "/rooms/a/participants", "{}"), &media).await),
            400
        );
        assert_eq!(
            status(respond(&request("DELETE", "/rooms/a/participants/x", ""), &media).await),
            400
        );
        assert_eq!(
            status(respond(&request("DELETE", "/rooms/a%20b", ""), &media).await),
            400
        );
    }
}
