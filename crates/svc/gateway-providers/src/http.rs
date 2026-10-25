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
/// Long replies take a while; a stuck call fails over eventually. Ten
/// minutes is the most a model API answers in without streaming, and a
/// reply that thinks first can take most of it (ADR 0043).
const CALL_TIMEOUT: Duration = Duration::from_secs(600);
/// The longest a connection may sit silent. Without streaming, a reply
/// says nothing until it's whole.
const IDLE_TIMEOUT: Duration = CALL_TIMEOUT;
/// A download may sit silent this long before it's an outage; it may take
/// as long as it takes in all (ADR 0061: an image is gigabytes).
const DOWNLOAD_IDLE: Duration = Duration::from_secs(60);
const DOWNLOAD_TOTAL: Duration = Duration::from_secs(24 * 3600);
/// The most of a reply's head read, and of a refusal's body.
const MAX_HEAD: usize = 64 * 1024;
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
    /// The most of a reply read.
    pub(crate) max_reply: usize,
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
    /// How long the server asked us to wait before trying again
    /// (`retry-after`, in seconds), when it said.
    pub retry_after: Option<Duration>,
}

/// A request [`Http::download`] sends.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ask<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    pub body: &'a [u8],
}

/// What [`Http::download`] got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Downloaded {
    pub status: u16,
    /// A 2xx body's bytes written.
    pub written: u64,
    /// Another status's body, as text, for its reason.
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
            max_reply: MAX_REPLY,
        }
    }

    /// Reads replies of up to `bytes`: a built kernel patch carries its
    /// kernel (ADR 0057).
    #[must_use]
    pub fn with_max_reply(mut self, bytes: usize) -> Self {
        self.max_reply = bytes;
        self
    }

    /// For downloads (ADR 0061): a minute of silence is an outage, and the
    /// whole may take as long as it takes.
    #[must_use]
    pub fn for_downloads(mut self) -> Self {
        self.idle = DOWNLOAD_IDLE;
        self.total = DOWNLOAD_TOTAL;
        self
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
        let deadline = Instant::now() + self.total;
        let mut stream = self.request(method, url, headers, body)?;
        read_reply(&mut *stream, deadline, self.max_reply)
    }

    /// Sends a request, as [`Http::send`] does, and streams a 2xx reply's
    /// body onto `to`, never all in memory, and binary: an OS image (ADR
    /// 0061). With `from` above 0 it asks for the body from that byte on
    /// (`Range`): a 206 answers from there, and a 200, from a server that
    /// ignores ranges, has its first `from` bytes passed over. At most
    /// `max` bytes are written; what was written before an error stays.
    pub fn download(
        &self,
        ask: &Ask<'_>,
        from: u64,
        to: &mut dyn Write,
        max: u64,
    ) -> Result<Downloaded, ProviderError> {
        let range = format!("bytes={from}-");
        let mut all: Vec<(&str, &str)> = ask.headers.to_vec();
        if from > 0 {
            all.push(("Range", &range));
        }
        let deadline = Instant::now() + self.total;
        let mut stream = self.request(ask.method, ask.url, &all, ask.body)?;
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        let head = loop {
            if let Some(head) = head(&buf, usize::MAX)? {
                break head;
            }
            if buf.len() > MAX_HEAD {
                return Err(ProviderError::Unavailable(
                    "the reply head is too long".into(),
                ));
            }
            match stream.read(&mut chunk) {
                Ok(0) => {
                    return Err(ProviderError::Unavailable(
                        "the connection closed before the reply was complete".into(),
                    ));
                }
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(outage("reading the reply", &e)),
            }
        };
        let left = match head.framing {
            Framing::Length(n) => Left::Length(n as u64),
            Framing::Chunked => Left::ChunkSize,
            Framing::Close => Left::Close,
        };
        let mut reply = Body {
            stream: &mut *stream,
            pending: buf.split_off(head.at),
            at: 0,
            left,
            deadline,
        };
        if !(200..300).contains(&head.status) {
            let mut text = Vec::new();
            let _ = (&mut reply).take(MAX_HEAD as u64).read_to_end(&mut text);
            return Ok(Downloaded {
                status: head.status,
                written: 0,
                body: String::from_utf8_lossy(&text).into_owned(),
            });
        }
        let skip = if head.status == 206 { 0 } else { from };
        let skipped = std::io::copy(&mut (&mut reply).take(skip), &mut std::io::sink())
            .map_err(|e| broken("downloading", &e))?;
        if skipped < skip {
            return Err(ProviderError::Unavailable(
                "the reply ended before where the download stopped".into(),
            ));
        }
        let mut written: u64 = 0;
        let mut part = vec![0u8; 256 * 1024];
        loop {
            let n = reply
                .read(&mut part)
                .map_err(|e| broken("downloading", &e))?;
            if n == 0 {
                break;
            }
            if written + n as u64 > max {
                return Err(ProviderError::Unavailable(format!(
                    "the download is longer than the {max} bytes expected"
                )));
            }
            to.write_all(&part[..n])
                .map_err(|e| ProviderError::Unavailable(format!("writing the download: {e}")))?;
            written += n as u64;
        }
        Ok(Downloaded {
            status: head.status,
            written,
            body: String::new(),
        })
    }

    /// A connection with `method` sent on it, its headers and body.
    fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Box<dyn Stream>, ProviderError> {
        let target = Target::parse(url).ok_or_else(|| {
            ProviderError::NotConfigured(format!("{url:?} is not an http(s) URL"))
        })?;
        if self.https_only && !target.tls {
            return Err(ProviderError::NotConfigured(
                "model endpoints must use HTTPS".into(),
            ));
        }
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
        Ok(stream)
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

/// A download broken off: an outage, in the stream's own words where it
/// has them (a bad chunk, an early end).
fn broken(doing: &str, e: &std::io::Error) -> ProviderError {
    match e.kind() {
        std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof => {
            ProviderError::Unavailable(format!("{doing}: {e}"))
        }
        _ => outage(doing, e),
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

/// A reply's head, once it's all there.
struct Head {
    status: u16,
    /// Where the body starts.
    at: usize,
    framing: Framing,
    retry_after: Option<Duration>,
}

fn read_reply(
    stream: &mut dyn Stream,
    deadline: Instant,
    max: usize,
) -> Result<Reply, ProviderError> {
    let mut buf: Vec<u8> = Vec::with_capacity(16 * 1024);
    let mut chunk = [0u8; 16 * 1024];
    let mut parsed: Option<Head> = None;
    let mut closed = false;
    loop {
        if parsed.is_none() {
            parsed = head(&buf, max)?;
        }
        if let Some(head) = &parsed {
            let body = &buf[head.at..];
            let done = match head.framing {
                Framing::Length(n) if body.len() >= n => Some(body[..n].to_vec()),
                Framing::Chunked => dechunk(body, max)?,
                Framing::Close if closed => Some(body.to_vec()),
                _ => None,
            };
            if let Some(body) = done {
                let body = String::from_utf8(body)
                    .map_err(|_| ProviderError::Unavailable("the reply isn't UTF-8".into()))?;
                return Ok(Reply {
                    status: head.status,
                    body,
                    retry_after: head.retry_after,
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
        if buf.len() > max {
            return Err(ProviderError::Unavailable("the reply is too large".into()));
        }
        match stream.read(&mut chunk) {
            Ok(0) => closed = true,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            // A server that closes without TLS close_notify ends the body
            // of a reply framed by the connection.
            Err(_)
                if parsed
                    .as_ref()
                    .is_some_and(|h| matches!(h.framing, Framing::Close)) =>
            {
                closed = true;
            }
            Err(e) => return Err(outage("reading the reply", &e)),
        }
    }
}

/// What's left of a body being streamed.
#[derive(Debug, Clone, Copy)]
enum Left {
    Length(u64),
    /// A chunk's size line is next.
    ChunkSize,
    /// This much of a chunk, then its CRLF.
    Chunk(u64),
    Close,
    Done,
}

/// A reply's body, read as it arrives: by its length, its chunks, or the
/// connection's end.
struct Body<'a> {
    stream: &'a mut dyn Stream,
    /// Bytes read and not yet given out, from `at`.
    pending: Vec<u8>,
    at: usize,
    left: Left,
    deadline: Instant,
}

impl Body<'_> {
    fn waiting(&self) -> &[u8] {
        &self.pending[self.at..]
    }

    /// More from the connection; false at its end.
    fn fill(&mut self) -> std::io::Result<bool> {
        if Instant::now() > self.deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out",
            ));
        }
        self.pending.drain(..self.at);
        self.at = 0;
        let mut chunk = [0u8; 64 * 1024];
        match self.stream.read(&mut chunk) {
            Ok(0) => Ok(false),
            Ok(n) => {
                self.pending.extend_from_slice(&chunk[..n]);
                Ok(true)
            }
            // A server that closes without TLS close_notify ends a body
            // framed by the connection.
            Err(_) if matches!(self.left, Left::Close) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn early_end() -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "the connection closed before the reply was complete",
        )
    }

    fn bad_chunk() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "bad chunked encoding")
    }

    /// Up to `left` waiting bytes into `out`.
    fn give(&mut self, out: &mut [u8], left: u64) -> usize {
        let n = out
            .len()
            .min(self.waiting().len())
            .min(usize::try_from(left).unwrap_or(usize::MAX));
        out[..n].copy_from_slice(&self.pending[self.at..self.at + n]);
        self.at += n;
        n
    }
}

impl Read for Body<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            match self.left {
                Left::Done | Left::Length(0) => {
                    self.left = Left::Done;
                    return Ok(0);
                }
                Left::Length(left) => {
                    if self.waiting().is_empty() && !self.fill()? {
                        return Err(Self::early_end());
                    }
                    let n = self.give(out, left);
                    self.left = Left::Length(left - n as u64);
                    return Ok(n);
                }
                Left::ChunkSize => match httparse::parse_chunk_size(self.waiting()) {
                    Ok(httparse::Status::Complete((used, size))) => {
                        self.at += used;
                        self.left = if size == 0 {
                            Left::Done
                        } else {
                            Left::Chunk(size)
                        };
                    }
                    Ok(httparse::Status::Partial) => {
                        if self.waiting().len() > 1024 {
                            return Err(Self::bad_chunk());
                        }
                        if !self.fill()? {
                            return Err(Self::early_end());
                        }
                    }
                    Err(_) => return Err(Self::bad_chunk()),
                },
                Left::Chunk(0) => {
                    if self.waiting().len() < 2 {
                        if !self.fill()? {
                            return Err(Self::early_end());
                        }
                        continue;
                    }
                    if &self.waiting()[..2] != b"\r\n" {
                        return Err(Self::bad_chunk());
                    }
                    self.at += 2;
                    self.left = Left::ChunkSize;
                }
                Left::Chunk(left) => {
                    if self.waiting().is_empty() && !self.fill()? {
                        return Err(Self::early_end());
                    }
                    let n = self.give(out, left);
                    self.left = Left::Chunk(left - n as u64);
                    return Ok(n);
                }
                Left::Close => {
                    if self.waiting().is_empty() && !self.fill()? {
                        self.left = Left::Done;
                        return Ok(0);
                    }
                    return Ok(self.give(out, u64::MAX));
                }
            }
        }
    }
}

/// The reply's head, once it's all there.
fn head(buf: &[u8], max: usize) -> Result<Option<Head>, ProviderError> {
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
        if n > max {
            return Err(ProviderError::Unavailable("the reply is too large".into()));
        }
        Framing::Length(n)
    } else {
        Framing::Close
    };
    // Seconds only: the HTTP-date form is for humans' clocks.
    let retry_after = value("retry-after")
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs);
    Ok(Some(Head {
        status,
        at,
        framing,
        retry_after,
    }))
}

/// The body of a chunked reply, once its last chunk has arrived.
fn dechunk(data: &[u8], max: usize) -> Result<Option<Vec<u8>>, ProviderError> {
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
                if size > max || out.len() + size > max {
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
            read_reply(
                &mut stream,
                Instant::now() + Duration::from_secs(5),
                MAX_REPLY,
            )
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
    fn a_reply_says_when_to_try_again() {
        let (url, _got) = fake::serve_headed(vec![
            (429, "Retry-After: 7\r\n", "{}"),
            (429, "retry-after: Wed, 21 Oct 2026 07:28:00 GMT\r\n", "{}"),
            (200, "", "{}"),
        ]);
        let http = Http::insecure_for_tests();
        let asked = |http: &Http| http.post(&url, &[], b"{}").unwrap().retry_after;
        assert_eq!(asked(&http), Some(Duration::from_secs(7)));
        assert_eq!(asked(&http), None, "seconds only");
        assert_eq!(asked(&http), None);
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

    /// Serves one request per reply, each reply's bytes exactly as given
    /// (a head and any body, binary), then closes; returns the base URL
    /// and the requests it got.
    pub fn serve_raw(replies: Vec<Vec<u8>>) -> (String, mpsc::Receiver<Request>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for reply in replies {
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
                let _ = stream.write_all(&reply);
                tx.send(Request {
                    head,
                    body: String::from_utf8_lossy(&received).into_owned(),
                })
                .unwrap();
            }
        });
        (url, rx)
    }

    /// Serves one request; returns the base URL and the request it got.
    pub fn serve(status: u16, reply: &'static str) -> (String, mpsc::Receiver<Request>) {
        serve_each(vec![(status, reply)])
    }

    /// Serves one request per reply, in order; returns the base URL and
    /// the requests it got.
    pub fn serve_each(replies: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<Request>) {
        serve_headed(replies.into_iter().map(|(s, r)| (s, "", r)).collect())
    }

    /// As [`serve_each`], each reply with more header lines (`name: value\r\n`).
    pub fn serve_headed(
        replies: Vec<(u16, &'static str, &'static str)>,
    ) -> (String, mpsc::Receiver<Request>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for (status, headers, reply) in replies {
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
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{reply}",
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
