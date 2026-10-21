//! The one HTTP client in the workspace, and all it does: POST a JSON body
//! over HTTPS and read one reply (ADR 0018).
//!
//! TLS is the base system's OpenSSL through native-tls, verifying servers
//! against the system trust store with hostname checks, so an org's own CAs
//! and TLS-inspecting proxies work as they do for every base program.
//! httparse reads the reply head; the body is framed by Content-Length,
//! chunked encoding or the end of the connection, and capped. One request per
//! connection (`Connection: close`), no redirects, no cookies, no
//! compression. An HTTPS proxy in glassd's environment (`https_proxy`, set
//! by its rc.d script, never by an agent) is used through CONNECT.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use glass_gateway::ProviderError;

/// The most of a reply we read. Model replies are text; this is generous.
const MAX_REPLY: usize = 8 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Long replies take a while; a stuck call fails over eventually.
const CALL_TIMEOUT: Duration = Duration::from_secs(300);
/// The longest a connection may sit silent.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// How much longer a hop in front of a provider waits than the provider's
/// own call (ADR 0039: a computer's call through its console), so the far
/// end gives up first and the near end hears why.
const HOP_SLACK: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub(crate) struct Http {
    /// Why not, if OpenSSL couldn't give us a connector; calls report it.
    tls: Result<native_tls::TlsConnector, String>,
    https_only: bool,
    proxy: Option<Proxy>,
    pub(crate) idle: Duration,
    pub(crate) total: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Proxy {
    authority: String,
    /// Hosts reached directly (`no_proxy`).
    bypass: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reply {
    pub status: u16,
    pub body: String,
}

/// Where a request goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) tls: bool,
    pub(crate) host: String,
    port: u16,
    /// Path and query, starting with `/`.
    path: String,
}

trait Stream: Read + Write {}
impl<T: Read + Write> Stream for T {}

impl Http {
    /// A client that only speaks HTTPS, through the environment's proxy if
    /// it has one.
    pub fn new() -> Self {
        let get = |names: [&str; 2]| names.iter().find_map(|n| std::env::var(n).ok());
        let proxy = get(["https_proxy", "HTTPS_PROXY"])
            .and_then(|url| Proxy::parse(&url, &get(["no_proxy", "NO_PROXY"]).unwrap_or_default()));
        Self::build(true, proxy)
    }

    /// Plain HTTP too and no proxy, for tests against a local fake server.
    #[cfg(test)]
    pub fn insecure_for_tests() -> Self {
        Self::build(false, None)
    }

    /// Plain HTTP and no proxy, for a console on this machine's loopback
    /// address in development (ADR 0023). Callers check the address.
    pub fn loopback() -> Self {
        Self::build(false, None)
    }

    fn build(https_only: bool, proxy: Option<Proxy>) -> Self {
        // Verifies the server's chain against the system trust store and its
        // name, as native-tls always does.
        let tls = native_tls::TlsConnector::builder()
            .min_protocol_version(Some(native_tls::Protocol::Tlsv12))
            .build()
            .map_err(|e| e.to_string());
        Self {
            tls,
            https_only,
            proxy,
            idle: IDLE_TIMEOUT,
            total: CALL_TIMEOUT,
        }
    }

    /// Waits longer than a provider's own call does, for requests that
    /// another server forwards to one.
    #[must_use]
    pub fn outwaiting_providers(mut self) -> Self {
        self.idle = IDLE_TIMEOUT + HOP_SLACK;
        self.total = CALL_TIMEOUT + HOP_SLACK;
        self
    }

    /// POSTs `body` with `headers` and reads the reply. Network and TLS
    /// failures are outages (the next model on the route takes over); their
    /// text never carries a header or the body.
    pub fn post(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Reply, ProviderError> {
        self.send("POST", url, headers, body)
    }

    /// Sends `method` (GET, POST, PATCH) with `headers` and `body`, as
    /// [`Http::post`] does.
    pub fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Reply, ProviderError> {
        if !matches!(method, "GET" | "POST" | "PATCH" | "DELETE") {
            return Err(ProviderError::Rejected(format!("no method {method:?}")));
        }
        let target = Target::parse(url).ok_or_else(|| {
            ProviderError::NotConfigured(format!("{url:?} is not an http(s) URL"))
        })?;
        if self.https_only && !target.tls {
            return Err(ProviderError::NotConfigured(
                "model endpoints must use HTTPS".into(),
            ));
        }
        let deadline = Instant::now() + self.total;
        let mut head = format!(
            "{method} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: Glasshouse model gateway\r\nContent-Length: {}\r\nConnection: close\r\n",
            target.path,
            target.authority(),
            body.len()
        );
        for (name, value) in headers {
            if name.contains(['\r', '\n', ':']) || value.contains(['\r', '\n']) {
                return Err(ProviderError::Rejected("a header has a line break".into()));
            }
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        let mut stream = self.open(&target)?;
        stream
            .write_all(head.as_bytes())
            .and_then(|()| stream.write_all(body))
            .and_then(|()| stream.flush())
            .map_err(|e| outage("sending", &e))?;
        read_reply(&mut *stream, deadline)
    }

    /// A connection to `target`, through the proxy and TLS as needed.
    fn open(&self, target: &Target) -> Result<Box<dyn Stream>, ProviderError> {
        let via = self
            .proxy
            .as_ref()
            .filter(|p| target.tls && !p.bypasses(&target.host));
        let tcp = match via {
            Some(proxy) => {
                let mut tcp = dial(&proxy.authority, self.idle)?;
                tunnel(&mut tcp, target)?;
                tcp
            }
            None => dial(&target.address(), self.idle)?,
        };
        if !target.tls {
            return Ok(Box::new(tcp));
        }
        let connector = self.tls.as_ref().map_err(|e| {
            ProviderError::NotConfigured(format!("TLS isn't available on this computer: {e}"))
        })?;
        let tls = connector
            .connect(&target.host, tcp)
            .map_err(|e| ProviderError::Unavailable(format!("TLS handshake failed: {e}")))?;
        Ok(Box::new(tls))
    }
}

fn outage(doing: &str, e: &std::io::Error) -> ProviderError {
    match e.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
            ProviderError::Unavailable(format!("timed out {doing}"))
        }
        kind => ProviderError::Unavailable(format!("network error {doing}: {kind}")),
    }
}

fn dial(authority: &str, idle: Duration) -> Result<TcpStream, ProviderError> {
    let addrs = authority
        .to_socket_addrs()
        .map_err(|_| ProviderError::Unavailable(format!("can't resolve {authority}")))?;
    let mut last = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(tcp) => {
                tcp.set_read_timeout(Some(idle))
                    .and_then(|()| tcp.set_write_timeout(Some(idle)))
                    .map_err(|e| outage("connecting", &e))?;
                return Ok(tcp);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => outage(&format!("connecting to {authority}"), &e),
        None => ProviderError::Unavailable(format!("no address for {authority}")),
    })
}

/// Opens a tunnel through an HTTP proxy with CONNECT.
fn tunnel(tcp: &mut TcpStream, target: &Target) -> Result<(), ProviderError> {
    let address = target.address();
    write!(tcp, "CONNECT {address} HTTP/1.1\r\nHost: {address}\r\n\r\n")
        .map_err(|e| outage("asking the proxy", &e))?;
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    // Byte by byte, so nothing past the proxy's answer is consumed.
    while !buf.ends_with(b"\r\n\r\n") {
        if buf.len() > 16 * 1024 {
            return Err(ProviderError::Unavailable(
                "the proxy's answer is too long".into(),
            ));
        }
        match tcp.read(&mut byte) {
            Ok(0) => {
                return Err(ProviderError::Unavailable(
                    "the proxy closed the connection".into(),
                ));
            }
            Ok(_) => buf.push(byte[0]),
            Err(e) => return Err(outage("waiting for the proxy", &e)),
        }
    }
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut answer = httparse::Response::new(&mut headers);
    match answer.parse(&buf) {
        Ok(httparse::Status::Complete(_)) if answer.code == Some(200) => Ok(()),
        _ => Err(ProviderError::Unavailable(format!(
            "the proxy refused the tunnel (HTTP {})",
            answer.code.unwrap_or(0)
        ))),
    }
}

/// How a reply's body ends.
enum Framing {
    Length(usize),
    Chunked,
    Close,
}

fn read_reply(stream: &mut dyn Stream, deadline: Instant) -> Result<Reply, ProviderError> {
    let mut buf: Vec<u8> = Vec::with_capacity(16 * 1024);
    let mut chunk = [0u8; 16 * 1024];
    let mut parsed: Option<(u16, usize, Framing)> = None;
    let mut closed = false;
    loop {
        if parsed.is_none() {
            parsed = head(&buf)?;
        }
        if let Some((status, at, framing)) = &parsed {
            let body = &buf[*at..];
            let done = match framing {
                Framing::Length(n) if body.len() >= *n => Some(body[..*n].to_vec()),
                Framing::Chunked => dechunk(body)?,
                Framing::Close if closed => Some(body.to_vec()),
                _ => None,
            };
            if let Some(body) = done {
                let body = String::from_utf8(body)
                    .map_err(|_| ProviderError::Unavailable("the reply isn't UTF-8".into()))?;
                return Ok(Reply {
                    status: *status,
                    body,
                });
            }
        }
        if closed {
            return Err(ProviderError::Unavailable(
                "the connection closed before the reply was complete".into(),
            ));
        }
        if Instant::now() > deadline {
            return Err(ProviderError::Unavailable(
                "timed out reading the reply".into(),
            ));
        }
        if buf.len() > MAX_REPLY {
            return Err(ProviderError::Unavailable("the reply is too large".into()));
        }
        match stream.read(&mut chunk) {
            Ok(0) => closed = true,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            // A server that closes without TLS close_notify ends the body
            // of a reply framed by the connection.
            Err(_) if matches!(parsed, Some((_, _, Framing::Close))) => closed = true,
            Err(e) => return Err(outage("reading the reply", &e)),
        }
    }
}

/// The status, where the body starts and how it's framed, once the head is
/// all there.
fn head(buf: &[u8]) -> Result<Option<(u16, usize, Framing)>, ProviderError> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut response = httparse::Response::new(&mut headers);
    let at = match response.parse(buf) {
        Ok(httparse::Status::Complete(at)) => at,
        Ok(httparse::Status::Partial) => return Ok(None),
        Err(e) => {
            return Err(ProviderError::Unavailable(format!(
                "unreadable reply head: {e}"
            )));
        }
    };
    let status = response.code.unwrap_or(0);
    let value = |name: &str| {
        response
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| String::from_utf8_lossy(h.value).trim().to_ascii_lowercase())
    };
    let framing = if value("transfer-encoding").is_some_and(|v| v.contains("chunked")) {
        Framing::Chunked
    } else if let Some(length) = value("content-length") {
        let n: usize = length
            .parse()
            .map_err(|_| ProviderError::Unavailable("bad Content-Length".into()))?;
        if n > MAX_REPLY {
            return Err(ProviderError::Unavailable("the reply is too large".into()));
        }
        Framing::Length(n)
    } else {
        Framing::Close
    };
    Ok(Some((status, at, framing)))
}

/// The body of a chunked reply, once its last chunk has arrived.
fn dechunk(data: &[u8]) -> Result<Option<Vec<u8>>, ProviderError> {
    let bad = || ProviderError::Unavailable("bad chunked encoding".into());
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        match httparse::parse_chunk_size(&data[at..]) {
            Ok(httparse::Status::Complete((used, size))) => {
                at += used;
                if size == 0 {
                    return Ok(Some(out));
                }
                let size = usize::try_from(size).map_err(|_| bad())?;
                if size > MAX_REPLY || out.len() + size > MAX_REPLY {
                    return Err(ProviderError::Unavailable("the reply is too large".into()));
                }
                if data.len() < at + size + 2 {
                    return Ok(None);
                }
                out.extend_from_slice(&data[at..at + size]);
                at += size;
                if &data[at..at + 2] != b"\r\n" {
                    return Err(bad());
                }
                at += 2;
            }
            Ok(httparse::Status::Partial) => return Ok(None),
            Err(_) => return Err(bad()),
        }
    }
}

impl Target {
    pub(crate) fn parse(url: &str) -> Option<Self> {
        let (tls, rest) = if let Some(rest) = url.strip_prefix("https://") {
            (true, rest)
        } else {
            (false, url.strip_prefix("http://")?)
        };
        let (authority, path) = match rest.find('/') {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, "/"),
        };
        if authority.is_empty() || authority.contains(['@', '[', ' ']) {
            return None;
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().ok()?),
            None => (authority, if tls { 443 } else { 80 }),
        };
        Some(Self {
            tls,
            host: host.to_owned(),
            port,
            path: path.to_owned(),
        })
    }

    /// `host:port`, to connect to (and for CONNECT).
    fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// `host` or `host:port`, as the Host header carries it.
    fn authority(&self) -> String {
        match (self.tls, self.port) {
            (true, 443) | (false, 80) => self.host.clone(),
            _ => format!("{}:{}", self.host, self.port),
        }
    }
}

impl Proxy {
    /// `http://host:port` (an HTTP proxy for HTTPS traffic) and a
    /// `no_proxy` list. Proxies that need a login aren't supported yet.
    fn parse(url: &str, no_proxy: &str) -> Option<Self> {
        let target = Target::parse(url.trim_end_matches('/'))?;
        if target.tls {
            return None;
        }
        Some(Self {
            authority: format!("{}:{}", target.host, target.port),
            bypass: no_proxy
                .split(',')
                .map(|h| h.trim().trim_start_matches('.').to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect(),
        })
    }

    fn bypasses(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.bypass
            .iter()
            .any(|b| b == "*" || host == *b || host.ends_with(&format!(".{b}")))
    }
}

/// How a provider's HTTP status maps onto what the gateway does next.
pub(crate) fn classify(status: u16, message: String) -> ProviderError {
    match status {
        // Bad or expired credentials, or a model this account can't use:
        // another provider on the route might work.
        401 | 403 | 404 => ProviderError::NotConfigured(format!("HTTP {status}: {message}")),
        // Rate limits, timeouts, overload and outages.
        408 | 429 | 500..=599 => ProviderError::Unavailable(format!("HTTP {status}: {message}")),
        _ => ProviderError::Rejected(format!("HTTP {status}: {message}")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn https_goes_through_the_tls_stack() {
        // A server that answers in plain text: the handshake has to fail,
        // as an outage the route can fail over from.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
        });
        let err = Http::build(true, None)
            .post(&format!("https://127.0.0.1:{port}/v1/messages"), &[], b"{}")
            .unwrap_err();
        assert!(err.to_string().contains("TLS handshake failed"), "{err}");
        assert!(err.fails_over());
        server.join().unwrap();
    }

    #[test]
    fn only_https_unless_testing() {
        let err = Http::new()
            .post("http://127.0.0.1:9/", &[], b"")
            .unwrap_err();
        assert_eq!(
            err,
            ProviderError::NotConfigured("model endpoints must use HTTPS".into())
        );
    }

    #[test]
    fn https_requests_tunnel_through_the_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            // Read the whole request, so closing doesn't reset the connection.
            let mut line = String::new();
            while line != "\r\n" {
                line.clear();
                reader.read_line(&mut line).unwrap();
            }
            let mut stream = stream;
            let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n");
            first
        });
        let http = Http::build(true, Proxy::parse(&proxy, "localhost, .internal"));
        let err = http
            .post("https://api.example.com/v1/messages", &[], b"{}")
            .unwrap_err();
        assert_eq!(
            server.join().unwrap(),
            "CONNECT api.example.com:443 HTTP/1.1\r\n"
        );
        assert!(
            err.to_string().contains("refused the tunnel (HTTP 403)"),
            "{err}"
        );

        let proxy = Proxy::parse("http://proxy:3128/", "localhost, .internal").unwrap();
        assert_eq!(proxy.authority, "proxy:3128");
        assert!(proxy.bypasses("bedrock.internal") && proxy.bypasses("LOCALHOST"));
        assert!(!proxy.bypasses("api.anthropic.com"));
    }

    #[test]
    fn replies_are_framed_by_length_chunks_or_close() {
        let reply = |raw: &'static [u8]| {
            let mut stream = std::io::Cursor::new(raw.to_vec());
            read_reply(&mut stream, Instant::now() + Duration::from_secs(5))
        };
        let r = reply(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello, and more").unwrap();
        assert_eq!((r.status, r.body.as_str()), (200, "hello"));
        let r = reply(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\nhello \r\n5\r\nworld\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!(r.body, "hello world");
        let r = reply(b"HTTP/1.1 429 Too Many\r\n\r\nslow down").unwrap();
        assert_eq!((r.status, r.body.as_str()), (429, "slow down"));
        assert!(reply(b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\nshort").is_err());
        assert!(reply(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n").is_err());
        assert!(reply(b"NOT HTTP\r\n\r\n").is_err());
    }

    #[test]
    fn urls_and_headers_are_checked() {
        let t = Target::parse("https://bedrock-runtime.us-east-1.amazonaws.com/model/x/converse")
            .unwrap();
        assert_eq!(
            (t.tls, t.port, t.path.as_str()),
            (true, 443, "/model/x/converse")
        );
        assert_eq!(t.authority(), "bedrock-runtime.us-east-1.amazonaws.com");
        assert_eq!(t.address(), "bedrock-runtime.us-east-1.amazonaws.com:443");
        let t = Target::parse("http://127.0.0.1:8080").unwrap();
        assert_eq!(
            (t.authority().as_str(), t.path.as_str()),
            ("127.0.0.1:8080", "/")
        );
        assert!(Target::parse("ftp://x").is_none());
        assert!(Target::parse("https://user@host/").is_none());

        let err = Http::insecure_for_tests()
            .post(
                "http://127.0.0.1:9/",
                &[("x-api-key", "a\r\nInjected: 1")],
                b"",
            )
            .unwrap_err();
        assert!(!err.fails_over());
    }

    #[test]
    fn statuses_say_whether_to_fail_over() {
        assert!(classify(429, String::new()).fails_over());
        assert!(classify(503, String::new()).fails_over());
        assert!(classify(403, String::new()).fails_over());
        assert!(!classify(400, String::new()).fails_over());
        assert!(!classify(302, String::new()).fails_over(), "no redirects");
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A one-shot HTTP server on localhost that records the request and
    //! answers with a canned reply, for provider tests with no network.

    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    #[derive(Debug)]
    pub struct Request {
        pub head: String,
        pub body: String,
    }

    /// Serves one request; returns the base URL and the request it got.
    pub fn serve(status: u16, reply: &'static str) -> (String, mpsc::Receiver<Request>) {
        serve_each(vec![(status, reply)])
    }

    /// Serves one request per reply, in order; returns the base URL and
    /// the requests it got.
    pub fn serve_each(replies: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<Request>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for (status, reply) in replies {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                let mut received = vec![0; length];
                reader.read_exact(&mut received).unwrap();
                let mut stream = stream;
                write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
                tx.send(Request {
                    head,
                    body: String::from_utf8(received).unwrap(),
                })
                .unwrap();
            }
        });
        (url, rx)
    }
}
