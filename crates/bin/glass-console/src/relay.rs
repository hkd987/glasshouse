//! The console's side of the relay's private API (ADR 0030 §6): it admits
//! people to a room with their offers, frees their seats, and ends rooms.
//! The API listens on loopback only and has no authentication, so the
//! relay must run beside the console; computers never reach it.

use std::net::SocketAddr;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// How long the relay has to answer.
const WAIT: Duration = Duration::from_secs(5);
/// The most of an answer read back: an SDP answer is a few kilobytes.
const MAX_REPLY: u64 = 256 * 1024;

/// Why the relay didn't do it.
#[derive(Debug)]
pub enum RelayError {
    /// It said no: its words.
    Refused(String),
    /// It couldn't be reached, or answered nonsense.
    Down(String),
}

#[derive(Debug, Clone)]
pub struct Relay {
    api: SocketAddr,
}

impl Relay {
    /// The relay whose API is at `api` (`127.0.0.1:7450`), on loopback.
    pub fn new(api: &str) -> anyhow::Result<Self> {
        let api: SocketAddr = api
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .parse()
            .map_err(|e| anyhow::anyhow!("--relay {api}: {e}"))?;
        anyhow::ensure!(
            api.ip().is_loopback(),
            "--relay must be on this machine: its API has no authentication"
        );
        Ok(Self { api })
    }

    /// Admits participant `participant`, whom the others see as `name`,
    /// to `room` with `offer`: the answer.
    pub async fn join(
        &self,
        room: &str,
        participant: u32,
        name: &str,
        offer: &str,
    ) -> Result<String, RelayError> {
        let body = json!({ "participant": participant, "name": name, "offer": offer });
        let (status, reply) = self
            .call("POST", &format!("/rooms/{room}/participants"), Some(&body))
            .await?;
        match (status, reply["answer"].as_str()) {
            (200, Some(answer)) => Ok(answer.to_owned()),
            (409, _) => Err(RelayError::Refused(
                "This meeting is full: 25 people are in it.".into(),
            )),
            _ => Err(refusal(status, &reply)),
        }
    }

    /// Frees participant `participant`'s seat in `room`.
    pub async fn leave(&self, room: &str, participant: u32) -> Result<(), RelayError> {
        let path = format!("/rooms/{room}/participants/{participant}");
        match self.call("DELETE", &path, None).await? {
            (204 | 404, _) => Ok(()),
            (status, reply) => Err(refusal(status, &reply)),
        }
    }

    /// Who has a seat in `room`: admitted, and their connection not gone
    /// (ADR 0052). Nobody for a room the relay doesn't have.
    pub async fn participants(
        &self,
        room: &str,
    ) -> Result<std::collections::BTreeSet<u32>, RelayError> {
        match self.call("GET", &format!("/rooms/{room}"), None).await? {
            (200, reply) => reply["participants"]
                .as_array()
                .map(|all| {
                    all.iter()
                        .filter_map(|p| p.as_u64().and_then(|p| u32::try_from(p).ok()))
                        .collect()
                })
                .ok_or_else(|| RelayError::Down("the relay answered nonsense".into())),
            (status, reply) => Err(refusal(status, &reply)),
        }
    }

    /// Ends `room`: everyone in it is disconnected.
    pub async fn end(&self, room: &str) -> Result<(), RelayError> {
        match self.call("DELETE", &format!("/rooms/{room}"), None).await? {
            (204 | 404, _) => Ok(()),
            (status, reply) => Err(refusal(status, &reply)),
        }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), RelayError> {
        tokio::time::timeout(WAIT, self.exchange(method, path, body))
            .await
            .map_err(|_| RelayError::Down("the relay didn't answer in time".into()))?
    }

    async fn exchange(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), RelayError> {
        let down = |e: std::io::Error| RelayError::Down(format!("the relay: {e}"));
        let body = body.map(Value::to_string).unwrap_or_default();
        let mut stream = TcpStream::connect(self.api).await.map_err(down)?;
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.api,
            body.len()
        );
        stream.write_all(request.as_bytes()).await.map_err(down)?;
        // Read the head, then exactly Content-Length bytes: a relay that
        // keeps the connection open mustn't make every join wait out WAIT.
        let mut reply = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(whole) = complete(&reply) {
                reply.truncate(whole);
                break;
            }
            let n = stream.read(&mut chunk).await.map_err(down)?;
            if n == 0 || reply.len() as u64 + n as u64 > MAX_REPLY {
                break;
            }
            reply.extend_from_slice(&chunk[..n]);
        }
        parse(&reply).ok_or_else(|| RelayError::Down("the relay answered nonsense".into()))
    }
}

/// The length of the whole reply once its head and Content-Length bytes
/// of body are in; `None` until then (and for a reply without the header,
/// which then ends when the relay closes).
fn complete(reply: &[u8]) -> Option<usize> {
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&reply[..split]).ok()?;
    let length: usize = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse().ok())?
    })?;
    let whole = split + 4 + length;
    (reply.len() >= whole).then_some(whole)
}

fn refusal(status: u16, reply: &Value) -> RelayError {
    let words = reply["error"].as_str().unwrap_or("no reason given");
    if status >= 500 {
        RelayError::Down(format!("the relay answered {status}: {words}"))
    } else {
        RelayError::Refused(words.to_owned())
    }
}

/// The status and JSON body (null when empty) of an HTTP/1.1 reply.
fn parse(reply: &[u8]) -> Option<(u16, Value)> {
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&reply[..split]).ok()?;
    let status = head.split(' ').nth(1)?.parse().ok()?;
    let body = &reply[split + 4..];
    let value = if body.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        serde_json::from_slice(body).ok()?
    };
    Some((status, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf21_the_relay_is_reached_on_this_machine_only() {
        assert!(Relay::new("127.0.0.1:7450").is_ok());
        assert!(Relay::new("http://127.0.0.1:7450/").is_ok());
        assert!(Relay::new("10.0.0.5:7450").is_err());
        assert!(Relay::new("relay.example:7450").is_err());
    }

    #[test]
    fn replies_parse() {
        let ok = b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\n\r\n{\"answer\":\"a\"}";
        assert_eq!(parse(ok).unwrap(), (200, json!({ "answer": "a" })));
        assert_eq!(
            parse(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap(),
            (204, Value::Null)
        );
        assert!(parse(b"garbage").is_none());
    }

    #[test]
    fn a_reply_is_whole_at_its_content_length_not_at_close() {
        let head = b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\n";
        assert_eq!(complete(head), None);
        let mut reply = head.to_vec();
        reply.extend_from_slice(b"null");
        assert_eq!(complete(&reply), Some(reply.len()));
        reply.extend_from_slice(b"extra");
        assert_eq!(complete(&reply), Some(reply.len() - 5));
        assert_eq!(complete(b"HTTP/1.1 204 No Content\r\n\r\n"), None);
    }
}
