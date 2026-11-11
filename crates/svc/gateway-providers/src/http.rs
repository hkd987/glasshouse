//! The one HTTP client in the workspace, and all it does: send a request
//! over HTTPS and read one reply (ADR 0018).
//!
//! TLS is the base system's OpenSSL through native-tls, verifying servers
//! against the system trust store with hostname checks, so an org's own CAs
//! and TLS-inspecting proxies work as they do for every base program.
//! httparse reads the reply head; the body is framed by Content-Length,
//! chunked encoding or the end of the connection, read as it arrives, and
//! capped. No redirects, no cookies, no compression. An HTTPS proxy in
//! glassd's environment (`https_proxy`, set by its rc.d script, never by an
//! agent) is used through CONNECT.
//!
//! Connections: one request each (`Connection: close`), unless a client
//! keeps its own ([`Http::keeping_connections`]). S3's does: a computer
//! going to sleep or waking is hundreds of requests to one host, and each
//! paid for a TCP connection and a TLS handshake. A kept connection is used
//! again only if its last reply was read to its end, it has sat unused for
//! less than [`KEEP_IDLE`], and the far end has neither closed it nor said
//! anything since. A model call, a call to the console and one to DynamoDB
//! still take a connection each: they come one in a while, and their POSTs
//! can't be sent a second time when a kept connection turns out closed.
//!
//! Time: connecting, the proxy's tunnel and the TLS handshake each have
//! [`CONNECT_TIMEOUT`]. After that a client waits as long as its kind of
//! call needs: a model's reply (ten minutes, the default), a download, or
//! one short answer ([`Http::quick`]). Every connection has TCP keepalive
//! on, so the networks between (a NAT, a load balancer) don't drop one
//! that is only waiting, and one whose far end is gone ends in minutes.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Mutex, PoisonError};
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
/// How long to wait before each time a model call is sent again
/// ([`Again`]), unless the reply says how long: a few seconds usually
/// clears a rate limit or an overload, where failing over or ending the
/// task would not need to. Two more tries, as Anthropic's own SDKs make.
const WAITS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(4)];
/// The longest `retry-after` honored; past it, the usual wait.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// When a model call is sent again, and how long after: one rule for
/// every model client (audit 3.6). Anthropic's client had its own, and
/// tried a rate-limited or overloaded call twice more; Bedrock's and
/// OpenRouter's had none, so the same 429 from them failed over at once
/// or ended the step.
///
/// A call is sent again when:
/// - the reply is a 408, a 429 or a 5xx (529, overloaded, among them),
///   after the wait the reply asks for (`retry-after`, up to a minute) or
///   the usual one;
/// - the connection was made, and ended before any of a reply came, and
///   that within the time connecting is allowed: a load balancer's reset,
///   a server going away. The call may have been acted on, and a model's
///   answer paid for twice; it changes nothing else.
///
/// And not when:
/// - no connection could be made (no network, no such name, refused): the
///   next model on the route takes over at once, as before, which is what
///   a computer off the network needs at every call;
/// - a wait ran out: trying again would wait as long again;
/// - a reply came and was cut off, too long or unreadable.
///
/// The waits are a little longer each time by chance, so that computers
/// refused together don't all come back together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Again {
    /// The wait before each further try.
    waits: Vec<Duration>,
}

impl Again {
    /// Twice more, after [`WAITS`].
    pub fn usual() -> Self {
        Self {
            waits: WAITS.to_vec(),
        }
    }

    /// `times` more, with no wait between.
    #[cfg(test)]
    pub fn at_once(times: usize) -> Self {
        Self {
            waits: vec![Duration::ZERO; times],
        }
    }

    /// Once and no more.
    #[cfg(test)]
    pub fn never() -> Self {
        Self::at_once(0)
    }
}

/// The wait before sending again after `reply`: what the server asked for,
/// within reason, else `usual`.
fn wait_for(reply: &Reply, usual: Duration) -> Duration {
    reply
        .retry_after
        .filter(|asked| *asked <= MAX_RETRY_AFTER)
        .unwrap_or(usual)
}

/// `wait` and up to a quarter more, by the clock's own nanoseconds.
fn with_jitter(wait: Duration) -> Duration {
    let quarter = u64::try_from(wait.as_millis() / 4).unwrap_or(0);
    if quarter == 0 {
        return wait;
    }
    let chance = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()));
    wait + Duration::from_millis(chance % (quarter + 1))
}

/// A download may sit silent this long before it's an outage; it may take
/// as long as it takes in all (ADR 0061: an image is gigabytes).
const DOWNLOAD_IDLE: Duration = Duration::from_secs(60);
const DOWNLOAD_TOTAL: Duration = Duration::from_secs(24 * 3600);
/// A call that is one question and one short answer (a key, a record, a
/// meter reading, a charge): silent this long, or this long in all, the
/// far end isn't answering. These waited the ten minutes a model's reply
/// may take, and whatever called them waited with them.
const QUICK_IDLE: Duration = Duration::from_secs(20);
const QUICK_TOTAL: Duration = Duration::from_secs(60);
/// How long a connection is silent before the kernel asks the far end
/// whether it's there, and how long between asks. A reply that thinks
/// first says nothing for minutes, and AWS's NAT gateways, load balancers
/// and PrivateLink drop a flow silent for 350 s without telling either
/// end: the reply was lost and the call waited out its ten minutes before
/// failing over. An ask every 45 s keeps the flow.
const KEEPALIVE: Duration = Duration::from_secs(45);
/// Asks unanswered before the connection is given up on: a far end that
/// is gone ends the call in three minutes, not ten.
const KEEPALIVE_TRIES: u32 = 3;
/// The most of a reply's head read, and of a refusal's body.
const MAX_HEAD: usize = 64 * 1024;
/// How much longer a hop in front of a provider waits than the provider's
/// own call (ADR 0039: a computer's call through its console), so the far
/// end gives up first and the near end hears why.
const HOP_SLACK: Duration = Duration::from_secs(30);
/// How long a kept connection may sit unused and still be used again. S3
/// closes one that has been silent a while, and the request sent as it
/// does is the one that goes unanswered; well inside that, a burst of
/// requests shares its connections and nothing is held between bursts.
const KEEP_IDLE: Duration = Duration::from_secs(15);
/// The most connections a client keeps: a save sends four chunks at a
/// time, beside leases being renewed and a listing.
const KEEP_AT_MOST: usize = 8;

#[derive(Debug)]
pub(crate) struct Http {
    /// Why not, if OpenSSL couldn't give us a connector; calls report it.
    tls: Result<native_tls::TlsConnector, String>,
    pub(crate) https_only: bool,
    proxy: Option<Proxy>,
    /// The longest connecting may take, and the tunnel, and the handshake.
    pub(crate) connect: Duration,
    pub(crate) idle: Duration,
    pub(crate) total: Duration,
    /// The most of a reply read.
    pub(crate) max_reply: usize,
    /// Connections kept between requests, for a client that keeps them.
    kept: Option<Box<Kept>>,
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

pub(crate) trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// A client's connections with no request on them, the one used last at
/// the end.
struct Kept {
    idle: Mutex<Vec<Idle>>,
    /// How long one may sit here.
    for_at_most: Duration,
}

struct Idle {
    to: Key,
    conn: Conn,
    since: Instant,
}

/// A server: over TLS or not, its host and its port.
type Key = (bool, String, u16);

impl std::fmt::Debug for Kept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Kept({})", self.idle().len())
    }
}

impl Kept {
    fn idle(&self) -> std::sync::MutexGuard<'_, Vec<Idle>> {
        self.idle.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A connection to `to` that can take a request, if one is kept.
    fn take(&self, to: &Key) -> Option<Conn> {
        loop {
            let conn = {
                let mut idle = self.idle();
                idle.retain(|kept| kept.since.elapsed() < self.for_at_most);
                let at = idle.iter().rposition(|kept| kept.to == *to)?;
                idle.remove(at).conn
            };
            if conn.quiet() {
                return Some(conn);
            }
        }
    }

    /// Keeps `conn`, whose reply has been read to its end.
    fn put(&self, to: Key, mut conn: Conn) {
        conn.used = true;
        let mut idle = self.idle();
        idle.push(Idle {
            to,
            conn,
            since: Instant::now(),
        });
        if idle.len() > KEEP_AT_MOST {
            idle.remove(0);
        }
    }

    /// Closes every connection kept to `to`: one of them was closed at
    /// the far end, and the rest as likely.
    fn forget(&self, to: &Key) {
        self.idle().retain(|kept| kept.to != *to);
    }
}

/// A connection to a server.
struct Conn {
    stream: Box<dyn Stream>,
    /// The socket under it, to ask whether the far end closed it while it
    /// sat unused. Only a client that keeps connections has one.
    socket: Option<TcpStream>,
    /// A reply has been read on it before: it was kept.
    used: bool,
}

impl Conn {
    /// Whether it is as its last reply left it: the far end hasn't closed
    /// it, and nothing has arrived. Anything waiting on a connection with
    /// no request on it is its end (TLS says goodbye in bytes) or isn't a
    /// reply to anything asked, and either way it takes no more requests.
    fn quiet(&self) -> bool {
        let Some(socket) = &self.socket else {
            return false;
        };
        if socket.set_nonblocking(true).is_err() {
            return false;
        }
        let mut byte = [0u8; 1];
        let silent = matches!(
            socket.peek(&mut byte),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
        );
        socket.set_nonblocking(false).is_ok() && silent
    }
}

/// Why a request got no reply to read.
enum Failed {
    /// Not all of it went out: the server can't have acted on it.
    Unsent(ProviderError),
    /// It went out, and the connection ended before a byte of a reply.
    Unanswered(ProviderError),
    Other(ProviderError),
}

impl From<Failed> for ProviderError {
    fn from(failed: Failed) -> Self {
        match failed {
            Failed::Unsent(e) | Failed::Unanswered(e) | Failed::Other(e) => e,
        }
    }
}

/// A reply whose head has been read: its body is still on the connection.
struct Answer<'a> {
    http: &'a Http,
    status: u16,
    /// The body's length, where the head says.
    length: Option<u64>,
    retry_after: Option<Duration>,
    body: Body,
    /// Where the connection is kept once the body has been read to its
    /// end, if both ends will use it again.
    keep: Option<Key>,
}

impl Answer<'_> {
    /// The whole body, of at most `max` bytes, as text.
    fn reply(&mut self, max: usize) -> Result<Reply, ProviderError> {
        let body = String::from_utf8(self.body.all(max)?)
            .map_err(|_| ProviderError::Unavailable("the reply isn't UTF-8".into()))?;
        Ok(Reply {
            status: self.status,
            body,
            retry_after: self.retry_after,
        })
    }

    /// A 2xx body onto `to`, from byte `from` of what was asked for and
    /// at most `max` bytes; another status's body as text.
    fn save(
        &mut self,
        from: u64,
        to: &mut dyn Write,
        max: u64,
    ) -> Result<Downloaded, ProviderError> {
        if !(200..300).contains(&self.status) {
            let mut text = Vec::new();
            let _ = (&mut self.body)
                .take(MAX_HEAD as u64)
                .read_to_end(&mut text);
            return Ok(Downloaded {
                status: self.status,
                written: 0,
                body: String::from_utf8_lossy(&text).into_owned(),
            });
        }
        let skip = if self.status == 206 { 0 } else { from };
        let skipped = std::io::copy(&mut (&mut self.body).take(skip), &mut std::io::sink())
            .map_err(|e| broken("downloading", &e))?;
        if skipped < skip {
            return Err(ProviderError::Unavailable(
                "the reply ended before where the download stopped".into(),
            ));
        }
        let mut written: u64 = 0;
        let mut part = vec![0u8; 256 * 1024];
        loop {
            let n = self
                .body
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
            status: self.status,
            written,
            body: String::new(),
        })
    }

    /// Done with the reply. Its connection is kept for the next request
    /// if the body was read to its end and nothing more came with it;
    /// otherwise it's closed, as one that isn't to be kept always is.
    fn done(self) {
        let Answer {
            http, body, keep, ..
        } = self;
        if let (Some(to), Some(kept)) = (keep, &http.kept)
            && body.ended()
        {
            kept.put(to, body.conn);
        }
    }
}

impl Http {
    /// A client that only speaks HTTPS, through the environment's proxy if
    /// it has one.
    pub fn new() -> Self {
        let get = |names: [&str; 2]| names.iter().find_map(|n| std::env::var(n).ok());
        let proxy = get(["https_proxy", "HTTPS_PROXY"])
            .and_then(|url| Proxy::parse(&url, &get(["no_proxy", "NO_PROXY"]).unwrap_or_default()));
        Self::build(true, proxy)
    }

    /// HTTPS only, through `proxy` (`http://host:port`) whatever the
    /// environment says: one call that must take the stack's egress relay,
    /// as a host's others don't (ADR 0067).
    pub fn via(proxy: &str) -> Option<Self> {
        Some(Self::build(true, Some(Proxy::parse(proxy, "")?)))
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
            connect: CONNECT_TIMEOUT,
            idle: IDLE_TIMEOUT,
            total: CALL_TIMEOUT,
            max_reply: MAX_REPLY,
            kept: None,
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

    /// For calls that are one question and one short answer: a minute at
    /// most, twenty seconds of silence. Not for a model's reply.
    #[must_use]
    pub fn quick(mut self) -> Self {
        self.idle = QUICK_IDLE;
        self.total = QUICK_TOTAL;
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

    /// Keeps connections open between requests and uses them again, up to
    /// [`KEEP_AT_MOST`] of them, each for [`KEEP_IDLE`] after its last
    /// reply.
    ///
    /// For a service that gets requests in bursts and whose GET, HEAD, PUT
    /// and DELETE may each be sent twice, as HTTP means them: when a kept
    /// connection turns out to have been closed at the far end and such a
    /// request on it got no reply, it's sent once more on a new one.
    /// Anything else is sent a second time only if the first never all
    /// went out.
    #[must_use]
    pub fn keeping_connections(mut self) -> Self {
        self.kept = Some(Box::new(Kept {
            idle: Mutex::new(Vec::new()),
            for_at_most: KEEP_IDLE,
        }));
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

    /// [`Http::post`] for a model call, sent again as `again` says: see
    /// [`Again`] for when and when not.
    pub(crate) fn post_again(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
        again: &Again,
    ) -> Result<Reply, ProviderError> {
        let mut tries = 0;
        loop {
            let usual = again.waits.get(tries).copied();
            let began = Instant::now();
            let wait = match (self.send_how("POST", url, headers, body), usual) {
                (Ok(reply), Some(usual)) if matches!(reply.status, 408 | 429 | 500..=599) => {
                    wait_for(&reply, usual)
                }
                (Ok(reply), _) => return Ok(reply),
                (Err(Failed::Unsent(_) | Failed::Unanswered(_)), Some(usual))
                    if began.elapsed() < self.connect =>
                {
                    usual
                }
                (Err(failed), _) => return Err(failed.into()),
            };
            std::thread::sleep(with_jitter(wait));
            tries += 1;
        }
    }

    /// Sends `method` (GET, POST, PATCH, DELETE) with `headers` and
    /// `body`, as [`Http::post`] does.
    pub fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Reply, ProviderError> {
        self.send_how(method, url, headers, body)
            .map_err(Into::into)
    }

    /// [`Http::send`], with how it failed when no reply came.
    fn send_how(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Reply, Failed> {
        if !matches!(method, "GET" | "POST" | "PATCH" | "DELETE") {
            return Err(Failed::Other(ProviderError::Rejected(format!(
                "no method {method:?}"
            ))));
        }
        let ask = Ask {
            method,
            url,
            headers,
            body,
        };
        let mut answer = self.exchange_how(&ask, Instant::now() + self.total, self.max_reply)?;
        let reply = answer.reply(self.max_reply);
        answer.done();
        reply.map_err(Failed::Other)
    }

    /// Asks with HEAD: the reply's status, and the length it gives for
    /// what a GET would send.
    pub fn head(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<(u16, Option<u64>), ProviderError> {
        let ask = Ask {
            method: "HEAD",
            url,
            headers,
            body: &[],
        };
        let answer = self.exchange(&ask, Instant::now() + self.total, usize::MAX)?;
        let said = (answer.status, answer.length);
        answer.done();
        Ok(said)
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
        let ask = Ask {
            headers: &all,
            ..*ask
        };
        let mut answer = self.exchange(&ask, Instant::now() + self.total, usize::MAX)?;
        let got = answer.save(from, to, max);
        answer.done();
        got
    }

    /// Sends `ask` and reads the head of its reply, by `deadline`; the
    /// body is the caller's to read. `longest` is the longest body a reply
    /// may say it has.
    fn exchange(
        &self,
        ask: &Ask<'_>,
        deadline: Instant,
        longest: usize,
    ) -> Result<Answer<'_>, ProviderError> {
        self.exchange_how(ask, deadline, longest)
            .map_err(Into::into)
    }

    /// [`Http::exchange`], with how it failed: a connection that couldn't
    /// be made is [`Failed::Other`], as is anything wrong with the request
    /// itself.
    fn exchange_how(
        &self,
        ask: &Ask<'_>,
        deadline: Instant,
        longest: usize,
    ) -> Result<Answer<'_>, Failed> {
        let target = Target::parse(ask.url)
            .ok_or_else(|| {
                ProviderError::NotConfigured(format!("{:?} is not an http(s) URL", ask.url))
            })
            .map_err(Failed::Other)?;
        if self.https_only && !target.tls {
            return Err(Failed::Other(ProviderError::NotConfigured(
                "model endpoints must use HTTPS".into(),
            )));
        }
        let mut request = format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: Glasshouse model gateway\r\nContent-Length: {}\r\n",
            ask.method,
            target.path,
            target.authority(),
            ask.body.len()
        );
        if self.kept.is_none() {
            request.push_str("Connection: close\r\n");
        }
        for (name, value) in ask.headers {
            if name.contains(['\r', '\n', ':']) || value.contains(['\r', '\n']) {
                return Err(Failed::Other(ProviderError::Rejected(
                    "a header has a line break".into(),
                )));
            }
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        let to = target.key();
        if let Some(kept) = &self.kept
            && let Some(conn) = kept.take(&to)
        {
            match converse(conn, &request, ask, deadline, longest) {
                Ok(heard) => return Ok(self.answer(to, ask.method, heard, deadline)),
                // The far end closed it after this client last looked:
                // the request goes on a new connection instead. One that
                // went out whole and may have been acted on is sent again
                // only if sending it twice is the same as sending it once.
                Err(Failed::Unsent(_)) => {}
                Err(Failed::Unanswered(_))
                    if matches!(ask.method, "GET" | "HEAD" | "PUT" | "DELETE") => {}
                Err(failed) => return Err(failed),
            }
            kept.forget(&to);
        }
        let conn = self.open(&target).map_err(Failed::Other)?;
        let heard = converse(conn, &request, ask, deadline, longest)?;
        Ok(self.answer(to, ask.method, heard, deadline))
    }

    /// What's heard, as a reply whose body is still to read.
    fn answer(&self, to: Key, method: &str, heard: Heard, deadline: Instant) -> Answer<'_> {
        let Heard { head, conn, rest } = heard;
        // No body, whatever the head says of one (RFC 9112, 6.3): a reply
        // to HEAD says how long a GET's would be.
        let left = if method == "HEAD" || matches!(head.status, 204 | 304) {
            Left::Done
        } else {
            match head.framing {
                Framing::Length(n) => Left::Length(n as u64),
                Framing::Chunked => Left::ChunkSize,
                Framing::Close => Left::Close,
            }
        };
        let keep = (self.kept.is_some()
            && head.again
            && conn.socket.is_some()
            && !matches!(left, Left::Close))
        .then_some(to);
        Answer {
            http: self,
            status: head.status,
            length: match head.framing {
                Framing::Length(n) => Some(n as u64),
                Framing::Chunked | Framing::Close => None,
            },
            retry_after: head.retry_after,
            body: Body {
                conn,
                pending: rest,
                at: 0,
                left,
                deadline,
                to_its_end: keep.is_some(),
                closed: false,
            },
            keep,
        }
    }

    /// A connection to `target`, through the proxy and TLS as needed.
    fn open(&self, target: &Target) -> Result<Conn, ProviderError> {
        let via = self
            .proxy
            .as_ref()
            .filter(|p| target.tls && !p.bypasses(&target.host));
        // Reads and writes have `self.connect` until the connection is
        // ready for the request: the tunnel and the handshake were under
        // the call's own ten minutes, so a server that took the connection
        // and said nothing held the call that long.
        let tcp = match via {
            Some(proxy) => {
                let mut tcp = dial(&proxy.authority, self.connect)?;
                tunnel(&mut tcp, target)?;
                tcp
            }
            None => dial(&target.address(), self.connect)?,
        };
        let ready = |tcp: &TcpStream| {
            tcp.set_read_timeout(Some(self.idle))
                .and_then(|()| tcp.set_write_timeout(Some(self.idle)))
                .map_err(|e| outage("connecting", &e))
        };
        // A second handle on the socket: a connection that can't have one
        // isn't kept.
        let socket = self.kept.as_ref().and_then(|_| tcp.try_clone().ok());
        if !target.tls {
            ready(&tcp)?;
            return Ok(Conn {
                stream: Box::new(tcp),
                socket,
                used: false,
            });
        }
        let connector = self.tls.as_ref().map_err(|e| {
            ProviderError::NotConfigured(format!("TLS isn't available on this computer: {e}"))
        })?;
        let tls = connector.connect(&target.host, tcp).map_err(|e| {
            ProviderError::Unavailable(match e {
                // The socket's timeout, as native-tls reports it.
                native_tls::HandshakeError::WouldBlock(_) => {
                    "timed out in the TLS handshake".into()
                }
                native_tls::HandshakeError::Failure(e) => format!("TLS handshake failed: {e}"),
            })
        })?;
        ready(tls.get_ref())?;
        Ok(Conn {
            stream: Box::new(tls),
            socket,
            used: false,
        })
    }
}

/// A reply's head, the connection it came on, and what was read past it.
struct Heard {
    head: Head,
    conn: Conn,
    rest: Vec<u8>,
}

/// Sends `request` and `ask`'s body on `conn` and reads the reply's head.
fn converse(
    mut conn: Conn,
    request: &str,
    ask: &Ask<'_>,
    deadline: Instant,
    longest: usize,
) -> Result<Heard, Failed> {
    conn.stream
        .write_all(request.as_bytes())
        .and_then(|()| conn.stream.write_all(ask.body))
        .and_then(|()| conn.stream.flush())
        .map_err(|e| Failed::Unsent(outage("sending", &e)))?;
    let early =
        || ProviderError::Unavailable("the connection closed before the reply was complete".into());
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        if let Some(head) = reply_head(&buf, longest).map_err(Failed::Other)? {
            let rest = buf.split_off(head.at);
            return Ok(Heard { head, conn, rest });
        }
        if buf.len() > MAX_HEAD {
            return Err(Failed::Other(ProviderError::Unavailable(
                "the reply head is too long".into(),
            )));
        }
        if Instant::now() > deadline {
            return Err(Failed::Other(ProviderError::Unavailable(
                "timed out reading the reply".into(),
            )));
        }
        match conn.stream.read(&mut chunk) {
            Ok(0) if buf.is_empty() => return Err(Failed::Unanswered(early())),
            Ok(0) => return Err(Failed::Other(early())),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if buf.is_empty() && ended(&e) => {
                return Err(Failed::Unanswered(outage("reading the reply", &e)));
            }
            Err(e) => return Err(Failed::Other(outage("reading the reply", &e))),
        }
    }
}

/// Whether `e` is a connection's end, as against a wait that ran out.
fn ended(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::{
        BrokenPipe, ConnectionAborted, ConnectionReset, NotConnected, UnexpectedEof,
    };
    matches!(
        e.kind(),
        BrokenPipe | ConnectionAborted | ConnectionReset | NotConnected | UnexpectedEof
    )
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

/// A connection to `authority` with keepalive on, its reads and writes
/// given `within` each, as connecting is.
fn dial(authority: &str, within: Duration) -> Result<TcpStream, ProviderError> {
    let addrs = authority
        .to_socket_addrs()
        .map_err(|_| ProviderError::Unavailable(format!("can't resolve {authority}")))?;
    let mut last = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, within) {
            Ok(tcp) => {
                let keep = socket2::TcpKeepalive::new()
                    .with_time(KEEPALIVE)
                    .with_interval(KEEPALIVE)
                    .with_retries(KEEPALIVE_TRIES);
                socket2::SockRef::from(&tcp)
                    .set_tcp_keepalive(&keep)
                    // A request is written whole, its head and then its
                    // body: neither waits in the kernel for the far end
                    // to acknowledge the other, which on a connection in
                    // use for a while is 40 ms a request.
                    .and_then(|()| tcp.set_nodelay(true))
                    .and_then(|()| tcp.set_read_timeout(Some(within)))
                    .and_then(|()| tcp.set_write_timeout(Some(within)))
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
    /// The server takes another request on this connection: it speaks
    /// HTTP/1.1 and didn't say it's closing.
    again: bool,
}

/// What's left of a body being read.
#[derive(Debug, Clone, Copy)]
enum Left {
    Length(u64),
    /// A chunk's size line is next.
    ChunkSize,
    /// This much of a chunk, then its CRLF.
    Chunk(u64),
    /// After the last chunk: any trailer lines, then an empty one.
    Trailers,
    Close,
    Done,
}

/// A reply's body, read as it arrives: by its length, its chunks, or the
/// connection's end.
struct Body {
    conn: Conn,
    /// Bytes read and not yet given out, from `at`.
    pending: Vec<u8>,
    at: usize,
    left: Left,
    deadline: Instant,
    /// Read a chunked body through the line that ends it, as a connection
    /// to be used again needs. One that won't be is done at its last
    /// chunk, as it always was, whatever a server sends after.
    to_its_end: bool,
    /// The connection has ended.
    closed: bool,
}

impl Body {
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
        match self.conn.stream.read(&mut chunk) {
            Ok(0) => {
                self.closed = true;
                Ok(false)
            }
            Ok(n) => {
                self.pending.extend_from_slice(&chunk[..n]);
                Ok(true)
            }
            // A server that closes without TLS close_notify ends a body
            // framed by the connection.
            Err(_) if matches!(self.left, Left::Close) => {
                self.closed = true;
                Ok(false)
            }
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

    /// Whether the body was read to its end with the connection still
    /// open and nothing more on it: the next request's reply starts clean.
    fn ended(&self) -> bool {
        !self.closed
            && matches!(self.left, Left::Done | Left::Length(0))
            && self.waiting().is_empty()
    }

    /// All of it, of at most `max` bytes.
    fn all(&mut self, max: usize) -> Result<Vec<u8>, ProviderError> {
        let mut out = Vec::new();
        let most = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
        match self.by_ref().take(most).read_to_end(&mut out) {
            Ok(_) if out.len() > max => {
                Err(ProviderError::Unavailable("the reply is too large".into()))
            }
            Ok(_) => Ok(out),
            Err(e) => Err(match e.kind() {
                std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::InvalidData => {
                    ProviderError::Unavailable(e.to_string())
                }
                _ => outage("reading the reply", &e),
            }),
        }
    }
}

impl Read for Body {
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
                        self.left = match size {
                            0 if self.to_its_end => Left::Trailers,
                            0 => Left::Done,
                            size => Left::Chunk(size),
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
                Left::Trailers => match self.waiting().windows(2).position(|w| w == b"\r\n") {
                    // The empty line: the body's end.
                    Some(0) => {
                        self.at += 2;
                        self.left = Left::Done;
                    }
                    // A trailer field; nothing here reads them.
                    Some(line) => self.at += line + 2,
                    None => {
                        if self.waiting().len() > 1024 {
                            return Err(Self::bad_chunk());
                        }
                        // A server that closes here has sent the whole
                        // body: it's the connection that's finished.
                        if !self.fill()? {
                            self.left = Left::Done;
                        }
                    }
                },
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

/// The reply's head, once it's all there; `longest` is the longest body
/// it may give a length for.
fn reply_head(buf: &[u8], longest: usize) -> Result<Option<Head>, ProviderError> {
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
        if n > longest {
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
    let again = response.version == Some(1)
        && !value("connection").is_some_and(|v| v.split(',').any(|said| said.trim() == "close"));
    Ok(Some(Head {
        status,
        at,
        framing,
        retry_after,
        again,
    }))
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

    /// Which server this is, for a connection kept to it.
    fn key(&self) -> Key {
        (self.tls, self.host.to_ascii_lowercase(), self.port)
    }

    /// `host:port`, to connect to (and for CONNECT).
    fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// `host` or `host:port`, as the Host header carries it.
    pub(crate) fn authority(&self) -> String {
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

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
    fn a_connection_asks_whether_the_far_end_is_there_while_it_waits() {
        // What keeps a flow through a NAT or a load balancer that drops
        // silent ones (350 s on AWS) while a model thinks, and ends a
        // call whose far end is gone in minutes.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let tcp = dial(
            &listener.local_addr().unwrap().to_string(),
            Duration::from_secs(3),
        )
        .unwrap();
        let socket = socket2::SockRef::from(&tcp);
        assert!(socket.keepalive().unwrap());
        assert_eq!(socket.tcp_keepalive_time().unwrap(), KEEPALIVE);
        assert_eq!(socket.tcp_keepalive_interval().unwrap(), KEEPALIVE);
        assert_eq!(socket.tcp_keepalive_retries().unwrap(), KEEPALIVE_TRIES);
        assert!(KEEPALIVE < Duration::from_secs(350) / 2);
        // Until it's ready for a request, reads have the connecting limit.
        assert_eq!(tcp.read_timeout().unwrap(), Some(Duration::from_secs(3)));
    }

    #[test]
    fn a_server_silent_in_the_handshake_is_given_up_on_as_connecting_is() {
        // It takes the connection and says nothing. The handshake was
        // under the call's ten minutes of silence, and so was whatever
        // made the call.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(3));
            drop(stream);
        });
        let mut http = Http::build(true, None);
        http.connect = Duration::from_millis(300);
        assert_eq!(http.idle, IDLE_TIMEOUT);
        let started = Instant::now();
        let err = http
            .post(&format!("https://127.0.0.1:{port}/v1/messages"), &[], b"{}")
            .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert!(
            err.to_string().contains("timed out in the TLS handshake"),
            "{err}"
        );
        assert!(err.fails_over());
        server.join().unwrap();
    }

    #[test]
    fn a_short_answer_is_waited_for_a_minute_and_a_models_for_ten() {
        let models = Http::build(true, None);
        assert_eq!((models.idle, models.total), (IDLE_TIMEOUT, CALL_TIMEOUT));
        let quick = Http::build(true, None).quick();
        assert_eq!((quick.idle, quick.total), (QUICK_IDLE, QUICK_TOTAL));
        assert!(quick.total <= Duration::from_secs(60));
        // A request it never answers ends at the quick client's silence,
        // not the ten minutes.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(2));
            drop(stream);
        });
        let mut quick = Http::insecure_for_tests().quick();
        quick.idle = Duration::from_millis(300);
        let started = Instant::now();
        let err = quick
            .send("GET", &format!("http://127.0.0.1:{port}/key"), &[], b"")
            .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert!(err.to_string().contains("timed out"), "{err}");
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

    /// A connection that gives `reply` and takes whatever is sent.
    struct Canned {
        reply: std::io::Cursor<Vec<u8>>,
    }

    impl Canned {
        fn conn(reply: &[u8]) -> Conn {
            Conn {
                stream: Box::new(Self {
                    reply: std::io::Cursor::new(reply.to_vec()),
                }),
                socket: None,
                used: false,
            }
        }
    }

    impl Read for Canned {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            self.reply.read(out)
        }
    }

    impl Write for Canned {
        fn write(&mut self, sent: &[u8]) -> std::io::Result<usize> {
            Ok(sent.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn replies_are_framed_by_length_chunks_or_close() {
        let reply = |raw: &'static [u8]| -> Result<Reply, ProviderError> {
            let http = Http::insecure_for_tests();
            let ask = Ask {
                method: "GET",
                url: "http://test.invalid/",
                headers: &[],
                body: &[],
            };
            let deadline = Instant::now() + Duration::from_secs(5);
            let heard = converse(Canned::conn(raw), "", &ask, deadline, MAX_REPLY)?;
            http.answer((false, "test.invalid".into(), 80), "GET", heard, deadline)
                .reply(MAX_REPLY)
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

    /// The path a request asks for.
    fn path(request: &fake::Request) -> &str {
        request.head.split(' ').nth(1).unwrap_or_default()
    }

    fn get(http: &Http, url: &str) -> String {
        http.send("GET", url, &[], b"").unwrap().body
    }

    #[test]
    fn adr0018_a_client_that_keeps_connections_sends_the_next_request_on_the_last_ones() {
        use fake::Then;
        let (url, had) = fake::serve_kept(|_, _, request| {
            Then::Reply(match path(request) {
                "/chunked" => b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n\
                                6\r\nhello \r\n5\r\nworld\r\n0\r\nx-sum: 1\r\n\r\n"
                    .to_vec(),
                "/gone" => b"HTTP/1.1 204 No Content\r\n\r\n".to_vec(),
                "/size" => b"HTTP/1.1 200 OK\r\ncontent-length: 1234\r\n\r\n".to_vec(),
                "/missing" => b"HTTP/1.1 404 Not Found\r\ncontent-length: 4\r\n\r\nnone".to_vec(),
                _ => fake::ok("hello"),
            })
        });
        let http = Http::insecure_for_tests().keeping_connections();
        assert_eq!(get(&http, &format!("{url}/length")), "hello");
        // Chunks, and what follows the last of them, are read to the end.
        assert_eq!(get(&http, &format!("{url}/chunked")), "hello world");
        // No body and no length: nothing to wait for.
        let gone = http
            .send("DELETE", &format!("{url}/gone"), &[], b"")
            .unwrap();
        assert_eq!((gone.status, gone.body.as_str()), (204, ""));
        // A HEAD reply's length is the object's, not a body's.
        assert_eq!(
            http.head(&format!("{url}/size"), &[]).unwrap(),
            (200, Some(1234))
        );
        let ask = |method: &'static str, url: &str, body: &'static [u8]| {
            let mut out = Vec::new();
            let ask = Ask {
                method,
                url,
                headers: &[],
                body,
            };
            let got = http.download(&ask, 0, &mut out, 100).unwrap();
            (got.status, got.body, out)
        };
        assert_eq!(
            ask("PUT", &format!("{url}/thing"), b"bytes"),
            (200, String::new(), b"hello".to_vec())
        );
        assert_eq!(
            ask("GET", &format!("{url}/missing"), b""),
            (404, "none".to_owned(), Vec::new())
        );
        assert_eq!(get(&http, &format!("{url}/length")), "hello");
        let lines = fake::lines(&had);
        assert_eq!(lines.len(), 7, "{lines:?}");
        assert!(
            lines.iter().all(|(connection, _)| *connection == 0),
            "{lines:?}"
        );
        assert!(
            had.lock()
                .unwrap()
                .iter()
                .all(|(_, request)| !request.head.to_ascii_lowercase().contains("connection:")),
            "a client that keeps its connections doesn't ask for them to be closed"
        );

        // A client that doesn't keep them takes one each, and says so.
        let (url, had) = fake::serve_kept(|_, _, _| Then::Reply(fake::ok("hello")));
        let http = Http::insecure_for_tests();
        assert_eq!(get(&http, &format!("{url}/a")), "hello");
        assert_eq!(get(&http, &format!("{url}/b")), "hello");
        assert_eq!(
            fake::lines(&had),
            [(0, "GET /a".to_owned()), (1, "GET /b".to_owned())]
        );
        assert!(
            had.lock()
                .unwrap()
                .iter()
                .all(|(_, request)| request.head.contains("Connection: close\r\n"))
        );
    }

    #[test]
    fn adr0018_a_connection_the_server_closed_isnt_used_again() {
        // The server answers and closes, without having said it would.
        let (url, had) = fake::serve_kept(|_, _, _| fake::Then::ReplyAndClose(fake::ok("hello")));
        let http = Http::insecure_for_tests().keeping_connections();
        for name in ["a", "b", "c"] {
            assert_eq!(get(&http, &format!("{url}/{name}")), "hello");
        }
        assert_eq!(
            fake::lines(&had),
            [
                (0, "GET /a".to_owned()),
                (1, "GET /b".to_owned()),
                (2, "GET /c".to_owned())
            ]
        );
    }

    #[test]
    fn adr0018_a_request_a_kept_connection_didnt_answer_is_sent_again_only_if_twice_is_once() {
        use fake::Then;
        // Each connection answers one request, and at its second closes
        // without a word: a server that closed the connection as the
        // request was on its way, which a client can't tell from one that
        // acted on it and then went.
        let (url, had) = fake::serve_kept(|_, before, _| {
            if before == 0 {
                Then::Reply(fake::ok("hello"))
            } else {
                Then::Close
            }
        });
        let http = Http::insecure_for_tests().keeping_connections();
        assert_eq!(get(&http, &format!("{url}/a")), "hello");
        // A GET is sent again, on a new connection, and nobody hears of it.
        assert_eq!(get(&http, &format!("{url}/b")), "hello");
        // A POST is not: whoever sent it hears it failed, as on any
        // connection that broke, and decides.
        let err = http.post(&format!("{url}/c"), &[], b"once").unwrap_err();
        assert!(err.fails_over(), "{err}");
        assert!(err.to_string().contains("closed before the reply"), "{err}");
        // Its connection is gone; the next request has a new one.
        assert_eq!(
            http.post(&format!("{url}/c"), &[], b"again").unwrap().body,
            "hello"
        );
        // A DELETE, as a GET.
        let deleted = http.send("DELETE", &format!("{url}/d"), &[], b"").unwrap();
        assert_eq!(deleted.body, "hello");
        let line = |connection: usize, line: &str| (connection, line.to_owned());
        assert_eq!(
            fake::lines(&had),
            [
                line(0, "GET /a"),
                line(0, "GET /b"),
                line(1, "GET /b"),
                line(1, "POST /c"),
                line(2, "POST /c"),
                line(2, "DELETE /d"),
                line(3, "DELETE /d"),
            ]
        );
    }

    #[test]
    fn adr0018_a_request_that_didnt_go_out_goes_on_a_new_connection_whatever_it_is() {
        /// A connection that takes nothing more.
        struct Shut;
        impl Read for Shut {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Ok(0)
            }
        }
        impl Write for Shut {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (url, had) = fake::serve_kept(|_, _, _| fake::Then::Reply(fake::ok("hello")));
        let http = Http::insecure_for_tests().keeping_connections();
        // Kept, and quiet as far as its socket shows.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let near = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let _far = listener.accept().unwrap();
        let to = Target::parse(&format!("{url}/")).unwrap().key();
        http.kept.as_ref().unwrap().put(
            to,
            Conn {
                stream: Box::new(Shut),
                socket: Some(near),
                used: false,
            },
        );
        // Not all of the request went out, so the server can't have acted
        // on it: even a POST goes again.
        let reply = http.post(&format!("{url}/b"), &[], b"once").unwrap();
        assert_eq!(reply.body, "hello");
        assert_eq!(fake::lines(&had), [(0, "POST /b".to_owned())]);
    }

    #[test]
    fn adr0018_only_a_reply_read_to_its_end_leaves_its_connection_to_use() {
        let (url, had) = fake::serve_kept(|_, _, request| {
            fake::Then::Reply(match path(request) {
                "/long" => fake::ok(&"0123456789".repeat(60_000)),
                "/closing" => {
                    b"HTTP/1.1 200 OK\r\nConnection: close\r\ncontent-length: 2\r\n\r\nok".to_vec()
                }
                "/old" => b"HTTP/1.0 200 OK\r\ncontent-length: 2\r\n\r\nok".to_vec(),
                "/more" => b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok and more".to_vec(),
                _ => fake::ok("hello"),
            })
        });
        let http = Http::insecure_for_tests().keeping_connections();
        // A reply given up on part way (it's longer than was to be
        // taken): the rest of it is still on the connection, and must not
        // be read as the next request's reply.
        let long = format!("{url}/long");
        let ask = Ask {
            method: "GET",
            url: &long,
            headers: &[],
            body: &[],
        };
        assert!(http.download(&ask, 0, &mut Vec::new(), 4).is_err());
        assert_eq!(get(&http, &format!("{url}/next")), "hello");
        // The server said it's closing; a server that speaks HTTP/1.0
        // closes without saying; more arrived than the reply.
        assert_eq!(get(&http, &format!("{url}/closing")), "ok");
        assert_eq!(get(&http, &format!("{url}/old")), "ok");
        assert_eq!(get(&http, &format!("{url}/more")), "ok");
        assert_eq!(get(&http, &format!("{url}/last")), "hello");
        let connections: Vec<usize> = fake::lines(&had).iter().map(|(c, _)| *c).collect();
        assert_eq!(connections, [0, 1, 1, 2, 3, 4], "{:?}", fake::lines(&had));
    }

    #[test]
    fn adr0018_a_connection_unused_too_long_is_closed() {
        let (url, had) = fake::serve_kept(|_, _, _| fake::Then::Reply(fake::ok("hello")));
        let mut http = Http::insecure_for_tests().keeping_connections();
        http.kept.as_mut().unwrap().for_at_most = Duration::from_millis(50);
        assert_eq!(get(&http, &format!("{url}/a")), "hello");
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(get(&http, &format!("{url}/b")), "hello");
        assert_eq!(
            fake::lines(&had),
            [(0, "GET /a".to_owned()), (1, "GET /b".to_owned())]
        );
    }

    #[test]
    fn audit_9_5_a_long_chunked_reply_is_decoded_once_as_it_arrives() {
        // 32 MiB in chunks of a little under 8 KiB, so chunks' edges fall
        // anywhere in what a read gives.
        let piece: Vec<u8> = (0..8191u32).map(|i| b'a' + (i % 26) as u8).collect();
        let pieces = 32 * 1024 * 1024 / piece.len();
        let mut reply = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec();
        for _ in 0..pieces {
            reply.extend_from_slice(format!("{:x}\r\n", piece.len()).as_bytes());
            reply.extend_from_slice(&piece);
            reply.extend_from_slice(b"\r\n");
        }
        reply.extend_from_slice(b"0\r\n\r\n");
        let (url, _requests) = fake::serve_raw(vec![reply]);
        let began = Instant::now();
        let got = Http::insecure_for_tests()
            .with_max_reply(64 * 1024 * 1024)
            .send("GET", &format!("{url}/bundle"), &[], b"")
            .unwrap();
        let took = began.elapsed();
        assert_eq!(got.body.len(), pieces * piece.len());
        assert!(got.body.as_bytes().chunks(piece.len()).all(|c| c == piece));
        // Decoded again from its start at every read, this took 18 s (and
        // a 160 MiB kernel bundle, minutes); read once, a fraction of one.
        assert!(took < Duration::from_secs(6), "it took {took:?}");

        // The cap still holds, without the reply being read past it.
        let mut reply = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec();
        for _ in 0..3 {
            reply.extend_from_slice(b"400\r\n");
            reply.extend_from_slice(&[b'x'; 1024]);
            reply.extend_from_slice(b"\r\n");
        }
        reply.extend_from_slice(b"0\r\n\r\n");
        let (url, _requests) = fake::serve_raw(vec![reply]);
        let err = Http::insecure_for_tests()
            .with_max_reply(2048)
            .send("GET", &format!("{url}/big"), &[], b"")
            .unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
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

    const FINE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";

    // Audit 3.6: one rule for every model client. Anthropic's tried a
    // refused status again and nothing else; the others tried nothing.
    #[test]
    fn a_model_call_is_sent_again_after_a_status_that_asks_for_it() {
        let (url, got) = fake::serve_headed(vec![
            (529, "", "{}"),
            (429, "retry-after: 0\r\n", "{}"),
            (200, "", "{}"),
        ]);
        let http = Http::insecure_for_tests();
        let reply = http
            .post_again(&url, &[], b"{}", &Again::at_once(2))
            .unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(got.iter().take(3).count(), 3, "twice more, then the reply");

        // Still refused after the tries it has: the last reply is the answer.
        let (url, got) = fake::serve_each(vec![(503, "{}"); 3]);
        let reply = http
            .post_again(&url, &[], b"{}", &Again::at_once(2))
            .unwrap();
        assert_eq!(reply.status, 503);
        assert_eq!(got.iter().take(3).count(), 3);

        // What trying again wouldn't change is answered at once.
        for status in [400, 401, 402, 404] {
            let (url, got) = fake::serve(status, "{}");
            let reply = http
                .post_again(&url, &[], b"{}", &Again::at_once(2))
                .unwrap();
            assert_eq!(reply.status, status);
            assert_eq!(got.iter().count(), 1, "HTTP {status}");
        }
    }

    #[test]
    fn a_model_call_whose_connection_ended_before_a_reply_is_sent_again() {
        // The server takes the request and closes: nothing of a reply.
        let (url, got) = fake::serve_raw(vec![Vec::new(), FINE.to_vec()]);
        let http = Http::insecure_for_tests();
        let reply = http
            .post_again(&url, &[], b"{\"n\":1}", &Again::at_once(2))
            .unwrap();
        assert_eq!(reply.status, 200);
        let sent: Vec<String> = got.iter().take(2).map(|r| r.body).collect();
        assert_eq!(sent, [r#"{"n":1}"#, r#"{"n":1}"#], "the same call, twice");
        // And as `post` alone it is the outage it always was.
        let (url, _got) = fake::serve_raw(vec![Vec::new(), FINE.to_vec()]);
        assert!(http.post(&url, &[], b"{}").is_err());
    }

    #[test]
    fn a_model_call_is_not_sent_again_when_that_would_not_help() {
        let http = Http::insecure_for_tests();
        // No connection: nothing listens here. Answered at once, with the
        // usual waits untouched, as a computer off the network needs at
        // every call (the next model on the route takes over).
        let asked = Instant::now();
        let err = http
            .post_again("http://127.0.0.1:9/v1", &[], b"{}", &Again::usual())
            .unwrap_err();
        assert!(err.fails_over(), "{err}");
        assert!(
            asked.elapsed() < Duration::from_millis(900),
            "{:?}",
            asked.elapsed()
        );

        // A reply that came and was cut off.
        let cut = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{\"half\":".to_vec();
        let (url, got) = fake::serve_raw(vec![cut, FINE.to_vec()]);
        assert!(
            http.post_again(&url, &[], b"{}", &Again::at_once(2))
                .is_err()
        );
        assert_eq!(got.try_iter().count(), 1, "asked once");

        // A wait that ran out: the server takes the request and says
        // nothing. Trying again would wait as long again.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let taken = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&taken);
        thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming().flatten() {
                count.fetch_add(1, Ordering::SeqCst);
                held.push(stream);
            }
        });
        let mut impatient = Http::insecure_for_tests();
        impatient.idle = Duration::from_millis(150);
        impatient.total = Duration::from_millis(150);
        let err = impatient
            .post_again(&url, &[], b"{}", &Again::at_once(2))
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        assert_eq!(taken.load(Ordering::SeqCst), 1, "asked once");

        // A connection that ended with no reply, but only after longer
        // than connecting is allowed: the call had been waiting a while.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let taken = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&taken);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                count.fetch_add(1, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(250));
                drop(stream);
            }
        });
        let mut http = Http::insecure_for_tests();
        http.connect = Duration::from_millis(100);
        assert!(
            http.post_again(&url, &[], b"{}", &Again::at_once(2))
                .is_err()
        );
        assert_eq!(taken.load(Ordering::SeqCst), 1, "asked once");
    }

    #[test]
    fn a_wait_is_what_the_server_asks_within_reason_and_a_little_more_by_chance() {
        let reply = |retry_after| Reply {
            status: 429,
            body: String::new(),
            retry_after,
        };
        let usual = Duration::from_secs(1);
        assert_eq!(
            wait_for(&reply(Some(Duration::from_secs(20))), usual),
            Duration::from_secs(20)
        );
        assert_eq!(
            wait_for(&reply(Some(Duration::from_secs(600))), usual),
            usual
        );
        assert_eq!(wait_for(&reply(None), usual), usual);
        assert_eq!(
            Again::usual().waits.len(),
            2,
            "as Anthropic's SDKs: twice more"
        );
        // Up to a quarter more, never less, and nothing added to nothing.
        assert_eq!(with_jitter(Duration::ZERO), Duration::ZERO);
        for _ in 0..200 {
            let wait = with_jitter(Duration::from_secs(4));
            assert!((Duration::from_secs(4)..=Duration::from_secs(5)).contains(&wait));
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A one-shot HTTP server on localhost that records the request and
    //! answers with a canned reply, for provider tests with no network.

    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex, mpsc};
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

    /// What a [`serve_kept`] server does with a request.
    pub enum Then {
        /// Sends these bytes and waits for the connection's next request.
        Reply(Vec<u8>),
        /// Sends these bytes and closes the connection.
        ReplyAndClose(Vec<u8>),
        /// Closes the connection without a word.
        Close,
    }

    /// The requests a [`serve_kept`] server has had, each with the number
    /// of the connection it came on (the first is 0).
    pub type Had = Arc<Mutex<Vec<(usize, Request)>>>;

    /// A 200 with `body`, on a connection that stays open.
    pub fn ok(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    /// Serves any number of connections, and on each as many requests as
    /// come, as a server that keeps connections does. `answer` is given
    /// the connection's number, how many requests it had before this one,
    /// and the request. Returns the base URL and the requests it has had.
    pub fn serve_kept(
        answer: impl Fn(usize, usize, &Request) -> Then + Send + Sync + 'static,
    ) -> (String, Had) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let had: Had = Arc::default();
        let (log, answer) = (Arc::clone(&had), Arc::new(answer));
        thread::spawn(move || {
            for (connection, stream) in listener.incoming().enumerate() {
                let Ok(mut stream) = stream else { return };
                let (log, answer) = (Arc::clone(&log), Arc::clone(&answer));
                thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    for before in 0.. {
                        let mut head = String::new();
                        let mut length = 0;
                        loop {
                            let mut line = String::new();
                            // The client closed it, or broke off.
                            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                                return;
                            }
                            if let Some(v) =
                                line.to_ascii_lowercase().strip_prefix("content-length:")
                            {
                                length = v.trim().parse().unwrap();
                            }
                            if line == "\r\n" {
                                break;
                            }
                            head.push_str(&line);
                        }
                        let mut received = vec![0; length];
                        if reader.read_exact(&mut received).is_err() {
                            return;
                        }
                        let request = Request {
                            head,
                            body: String::from_utf8_lossy(&received).into_owned(),
                        };
                        let then = answer(connection, before, &request);
                        log.lock().unwrap().push((connection, request));
                        match then {
                            Then::Reply(reply) => {
                                if stream.write_all(&reply).is_err() {
                                    return;
                                }
                            }
                            Then::ReplyAndClose(reply) => {
                                let _ = stream.write_all(&reply);
                                return;
                            }
                            Then::Close => return,
                        }
                    }
                });
            }
        });
        (url, had)
    }

    /// What `had` holds: each request's connection and its first line
    /// without the version (`GET /path`).
    pub fn lines(had: &Had) -> Vec<(usize, String)> {
        had.lock()
            .unwrap()
            .iter()
            .map(|(connection, request)| {
                let line = request.head.lines().next().unwrap_or_default();
                (*connection, line.trim_end_matches(" HTTP/1.1").to_owned())
            })
            .collect()
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
