//! Just enough HTTP/1.1 for our small servers: one request per connection,
//! heads parsed by httparse, bodies framed by Content-Length. The app
//! bridge's development mode (ADR 0024) and the admin console (ADR 0023)
//! serve with it, on loopback in development; in production the console
//! sits behind our web servers' TLS.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest request head read.
const MAX_HEAD: usize = 16 * 1024;
/// Largest request body read (an app call's input).
pub const MAX_BODY: usize = 1024 * 1024;
/// How long a client may take to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub struct Request {
    pub method: String,
    /// The path without its query.
    pub path: String,
    /// The query, without its `?`; empty when there is none.
    pub query: String,
    /// Header names in lower case.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![("Content-Type", content_type.to_owned())],
            body: body.into(),
        }
    }

    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        let mut response = Self::new(status, "application/json", value.to_string());
        response.headers.push(("Cache-Control", "no-store".into()));
        response
    }

    pub fn text(status: u16, text: &str) -> Self {
        Self::new(status, "text/plain; charset=utf-8", text)
    }

    pub fn with(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        421 => "Misdirected Request",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    /// The client went away or sent nothing in time.
    Gone,
    /// A request we answer with this status and close.
    Bad(u16, &'static str),
}

/// Reads one request.
pub async fn read_request<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Request, ReadError> {
    tokio::time::timeout(READ_TIMEOUT, read(stream))
        .await
        .unwrap_or(Err(ReadError::Gone))
}

async fn read<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Request, ReadError> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];
    let (head_len, method, path, query, headers) = loop {
        let n = stream.read(&mut chunk).await.map_err(|_| ReadError::Gone)?;
        if n == 0 {
            return Err(ReadError::Gone);
        }
        buf.extend_from_slice(&chunk[..n]);
        let mut parsed = [httparse::EMPTY_HEADER; 48];
        let mut request = httparse::Request::new(&mut parsed);
        match request.parse(&buf) {
            Ok(httparse::Status::Complete(len)) => {
                let headers = request
                    .headers
                    .iter()
                    .map(|h| {
                        (
                            h.name.to_ascii_lowercase(),
                            String::from_utf8_lossy(h.value).trim().to_owned(),
                        )
                    })
                    .collect::<Vec<_>>();
                let method = request.method.unwrap_or_default().to_owned();
                let target = request.path.unwrap_or("/");
                let target = target.split('#').next().unwrap_or("/");
                let (path, query) = target.split_once('?').unwrap_or((target, ""));
                break (len, method, path.to_owned(), query.to_owned(), headers);
            }
            Ok(httparse::Status::Partial) if buf.len() < MAX_HEAD => {}
            Ok(httparse::Status::Partial) => {
                return Err(ReadError::Bad(431, "request head too large"));
            }
            Err(httparse::Error::TooManyHeaders) => {
                return Err(ReadError::Bad(431, "too many headers"));
            }
            Err(_) => return Err(ReadError::Bad(400, "not an HTTP request")),
        }
    };
    let length = match headers.iter().find(|(n, _)| n == "content-length") {
        None => 0,
        Some((_, v)) => v
            .parse::<usize>()
            .map_err(|_| ReadError::Bad(400, "bad Content-Length"))?,
    };
    if headers.iter().any(|(n, _)| n == "transfer-encoding") {
        return Err(ReadError::Bad(400, "send a Content-Length"));
    }
    if length > MAX_BODY {
        return Err(ReadError::Bad(413, "request body too large"));
    }
    let mut body = buf.split_off(head_len);
    if body.len() > length {
        return Err(ReadError::Bad(400, "one request per connection"));
    }
    let mut rest = vec![0u8; length - body.len()];
    stream
        .read_exact(&mut rest)
        .await
        .map_err(|_| ReadError::Gone)?;
    body.extend_from_slice(&rest);
    Ok(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

/// Writes `response` and closes. A HEAD request gets the head only; a 204
/// has neither a body nor a Content-Length (RFC 9110).
pub async fn write_response<S: AsyncWrite + Unpin>(
    stream: &mut S,
    response: Response,
    head_only: bool,
) -> std::io::Result<()> {
    let no_content = response.status == 204;
    let length = match no_content {
        true => String::new(),
        false => format!("Content-Length: {}\r\n", response.body.len()),
    };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n{length}Connection: close\r\n\
         X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\
         Cross-Origin-Resource-Policy: same-origin\r\n",
        response.status,
        reason(response.status),
    );
    for (name, value) in &response.headers {
        // Values are ours, but a newline must never split a header.
        let value: String = value.chars().filter(|c| !c.is_control()).collect();
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    if !head_only && !no_content {
        stream.write_all(&response.body).await?;
    }
    stream.flush().await?;
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn parse(raw: &str) -> Result<Request, ReadError> {
        let mut stream = raw.as_bytes();
        read_request(&mut stream).await
    }

    #[tokio::test]
    async fn reads_a_request_with_its_body() {
        let r = parse(
            "POST /_bridge/call?x=1 HTTP/1.1\r\nHost: mail.localhost:7300\r\n\
             Content-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
        )
        .await
        .unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/_bridge/call");
        assert_eq!(r.header("host"), Some("mail.localhost:7300"));
        assert_eq!(r.body, b"{}");
    }

    #[tokio::test]
    async fn refuses_what_it_wont_read() {
        let big = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert_eq!(
            parse(&big).await.unwrap_err(),
            ReadError::Bad(413, "request body too large")
        );
        assert!(matches!(
            parse("POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n").await,
            Err(ReadError::Bad(400, _))
        ));
        assert!(matches!(
            parse("GET / HTTP/1.1\r\nContent-Length: 0\r\n\r\nGET / HTTP/1.1\r\n\r\n").await,
            Err(ReadError::Bad(400, _))
        ));
        assert!(matches!(
            parse("\x16\x03\x01 hello\r\n\r\n").await,
            Err(ReadError::Bad(400, _))
        ));
        assert_eq!(
            parse("GET / HTTP/1.1\r\n").await.unwrap_err(),
            ReadError::Gone
        );
    }

    #[tokio::test]
    async fn responses_never_split_a_header() {
        let mut out = Vec::new();
        let response = Response::text(200, "hi").with("X-Test", "a\r\nSet-Cookie: x=1");
        write_response(&mut out, response, false).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("X-Test: aSet-Cookie: x=1\r\n"), "{text}");
        assert!(text.ends_with("\r\n\r\nhi"));
    }

    #[tokio::test]
    async fn no_content_has_no_length_or_body() {
        let mut out = Vec::new();
        let response = Response {
            status: 204,
            headers: Vec::new(),
            body: b"ignored".to_vec(),
        };
        write_response(&mut out, response, false).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 204 No Content\r\n"), "{text}");
        assert!(!text.contains("Content-Length"), "{text}");
        assert!(text.ends_with("\r\n\r\n"), "{text}");
    }
}
