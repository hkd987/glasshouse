//! A JSON-RPC server over a Unix listener.
//!
//! Each connection gets a reader loop and a writer task. Requests run
//! concurrently (up to [`MAX_IN_FLIGHT`] per connection) so one slow call,
//! such as an action waiting on an approval, never blocks the rest.
//!
//! A listener outlives anything one client does to it. A connection that
//! can't be taken is that connection's loss, never the listener's end; how
//! many connections stay open is bounded ([`Limits`]); a reply too large to
//! send fails its own call; and a handler that panics answers with an
//! error. The peer is often an agent, and the process serving it is the one
//! every other task runs in.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Semaphore, mpsc, watch};

use crate::framing::{FrameError, MAX_MESSAGE_BYTES, read_message, write_message};
use crate::{Message, Request, Response, RpcError, codes};

/// Concurrent requests allowed per connection before the reader applies
/// backpressure.
pub const MAX_IN_FLIGHT: usize = 32;

/// How many connections a listener keeps open at once. One past a limit is
/// accepted and closed at once, so its client hears "closed" instead of
/// waiting, and the listener's descriptors and memory have a bound
/// whatever a client does: each open connection can hold a message of
/// [`MAX_MESSAGE_BYTES`] and [`MAX_IN_FLIGHT`] calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// From everyone.
    pub connections: usize,
    /// From one user, as the kernel names them.
    pub per_user: usize,
    /// Whether [`MAX_IN_FLIGHT`] is for all the listener's connections
    /// together, not for each.
    pub shared_in_flight: bool,
}

impl Limits {
    /// A socket people and their programs share: the control and app
    /// sockets. Far above what a desktop, its apps and a busy front open,
    /// and below what one user would need to use glassd's descriptors up.
    pub const SHARED: Self = Self {
        connections: 4096,
        per_user: 512,
        shared_in_flight: false,
    };
    /// A socket one task's agent has to itself. An agent needs one
    /// connection; a harness with helpers, a few.
    pub const ONE_TASK: Self = Self {
        connections: 16,
        per_user: 16,
        shared_in_flight: true,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Self::SHARED
    }
}

/// The connections a listener has open, by user.
#[derive(Debug, Default)]
struct Open {
    all: usize,
    by_user: HashMap<u32, usize>,
}

/// One open connection's place among a listener's, given back when dropped.
#[derive(Debug)]
struct Slot {
    open: Arc<Mutex<Open>>,
    uid: u32,
}

impl Slot {
    /// A place for one more connection from `uid`, if the limits allow it.
    fn take(open: &Arc<Mutex<Open>>, uid: u32, limits: Limits) -> Option<Self> {
        let mut counts = open.lock().unwrap_or_else(PoisonError::into_inner);
        let mine = counts.by_user.get(&uid).copied().unwrap_or(0);
        if counts.all >= limits.connections || mine >= limits.per_user {
            return None;
        }
        counts.all += 1;
        counts.by_user.insert(uid, mine + 1);
        Some(Self {
            open: Arc::clone(open),
            uid,
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut counts = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        counts.all = counts.all.saturating_sub(1);
        match counts.by_user.get_mut(&self.uid) {
            Some(n) if *n > 1 => *n -= 1,
            _ => {
                counts.by_user.remove(&self.uid);
            }
        }
    }
}

/// Where connections come from: a listener, or a test's stand-in for one
/// that fails.
trait Incoming {
    fn next(&self) -> impl Future<Output = io::Result<UnixStream>> + Send;
}

impl Incoming for UnixListener {
    async fn next(&self) -> io::Result<UnixStream> {
        self.accept().await.map(|(stream, _)| stream)
    }
}

/// How long to wait before accepting again after `failures` in a row: 20 ms,
/// doubling to a little over a second. Out of descriptors, an accept fails
/// at once, every time, until something closes.
fn accept_backoff(failures: u32) -> Duration {
    Duration::from_millis(10 << failures.clamp(1, 7))
}

/// Who is on the other end of a Unix socket, as the kernel reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCred {
    pub uid: u32,
    pub gid: u32,
    pub pid: Option<i32>,
}

impl PeerCred {
    pub fn of(stream: &UnixStream) -> io::Result<Self> {
        let cred = stream.peer_cred()?;
        Ok(Self {
            uid: cred.uid(),
            gid: cred.gid(),
            pid: cred.pid(),
        })
    }
}

/// One client connection, as handlers see it.
#[derive(Debug)]
pub struct Connection {
    id: u64,
    peer: PeerCred,
    outbox: mpsc::Sender<Message>,
    closed: watch::Receiver<bool>,
}

impl Connection {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn peer(&self) -> PeerCred {
        self.peer
    }

    /// Sends a server-to-client notification. Returns `false` once the
    /// connection is gone.
    pub async fn notify(&self, method: &str, params: Value) -> bool {
        self.outbox
            .send(Message::notification(method, params))
            .await
            .is_ok()
    }

    /// Resolves when the client disconnects.
    pub async fn closed(&self) {
        let mut closed = self.closed.clone();
        // An error means the reader is gone, which is also "closed".
        let _ = closed.wait_for(|c| *c).await;
    }
}

/// Application logic behind a listener.
pub trait Handler: Send + Sync + 'static {
    /// Handles one request and returns its result.
    fn call(
        &self,
        conn: Arc<Connection>,
        method: String,
        params: Value,
    ) -> impl Future<Output = Result<Value, RpcError>> + Send;

    /// Handles a client notification. Ignored by default.
    fn notification(
        &self,
        _conn: Arc<Connection>,
        _method: String,
        _params: Value,
    ) -> impl Future<Output = ()> + Send {
        async {}
    }

    /// Called once after the client disconnects.
    fn disconnected(&self, _conn: &Connection) {}
}

static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);

/// Accepts connections forever, serving each on its own task, within
/// [`Limits::SHARED`].
pub async fn serve<H: Handler>(listener: UnixListener, handler: Arc<H>) -> io::Result<()> {
    serve_within(listener, handler, Limits::SHARED).await
}

/// Accepts connections forever, serving each on its own task, at most
/// `limits` of them at once. It doesn't return: an accept that fails (no
/// descriptors left, a peer already gone) is logged and tried again after a
/// pause. The listener ends when its task is dropped or aborted.
pub async fn serve_within<H: Handler>(
    listener: UnixListener,
    handler: Arc<H>,
    limits: Limits,
) -> io::Result<()> {
    serve_from(listener, handler, limits).await
}

async fn serve_from<I: Incoming, H: Handler>(
    incoming: I,
    handler: Arc<H>,
    limits: Limits,
) -> io::Result<()> {
    let open = Arc::new(Mutex::new(Open::default()));
    let shared = limits
        .shared_in_flight
        .then(|| Arc::new(Semaphore::new(MAX_IN_FLIGHT)));
    let mut failures: u32 = 0;
    loop {
        let stream = match incoming.next().await {
            Ok(stream) => {
                failures = 0;
                stream
            }
            Err(e) => {
                failures = failures.saturating_add(1);
                // The first, then every so often: out of descriptors this
                // repeats until something closes.
                if failures.is_power_of_two() {
                    tracing::warn!(error = %e, failures, "couldn't accept a connection; still listening");
                }
                tokio::time::sleep(accept_backoff(failures)).await;
                continue;
            }
        };
        // Who it is, from the kernel; one that hung up already has no one.
        let Ok(peer) = PeerCred::of(&stream) else {
            continue;
        };
        let Some(slot) = Slot::take(&open, peer.uid, limits) else {
            tracing::debug!(uid = peer.uid, "closing a connection: too many are open");
            continue;
        };
        let handler = Arc::clone(&handler);
        let shared = shared.clone();
        tokio::spawn(async move {
            let _slot = slot;
            if let Err(e) = serve_stream(stream, peer, handler, shared).await {
                tracing::debug!(error = %e, "connection ended with error");
            }
        });
    }
}

/// Serves one connection until the client hangs up.
pub async fn serve_connection<H: Handler>(stream: UnixStream, handler: Arc<H>) -> io::Result<()> {
    let peer = PeerCred::of(&stream)?;
    serve_stream(stream, peer, handler, None).await
}

/// What a caller is told when its reply can't be sent for its size.
fn too_large(message: &Message) -> Option<Message> {
    let Message::Response(Response { id: Some(id), .. }) = message else {
        return None;
    };
    Some(Message::Response(Response::err(
        Some(id.clone()),
        RpcError::new(
            codes::INTERNAL_ERROR,
            format!(
                "the reply is larger than a message may be ({} MiB); ask for less at a time",
                MAX_MESSAGE_BYTES / (1024 * 1024)
            ),
        ),
    )))
}

async fn serve_stream<H: Handler>(
    stream: UnixStream,
    peer: PeerCred,
    handler: Arc<H>,
    in_flight: Option<Arc<Semaphore>>,
) -> io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let (outbox, mut queue) = mpsc::channel::<Message>(256);
    let (closed_tx, closed_rx) = watch::channel(false);
    let conn = Arc::new(Connection {
        id: NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed),
        peer,
        outbox,
        closed: closed_rx,
    });

    let writer = tokio::spawn(async move {
        while let Some(message) = queue.recv().await {
            let sent = match write_message(&mut write_half, &message).await {
                // A reply too large to send is that call's failure, said in
                // its place: the connection, and every other call on it,
                // carries on.
                Err(FrameError::TooLarge) => match too_large(&message) {
                    Some(instead) => write_message(&mut write_half, &instead).await,
                    None => Ok(()),
                },
                sent => sent,
            };
            if let Err(e) = sent {
                tracing::debug!(error = %e, "write failed; dropping connection output");
                break;
            }
        }
    });

    let permits = in_flight.unwrap_or_else(|| Arc::new(Semaphore::new(MAX_IN_FLIGHT)));
    let mut reader = BufReader::new(read_half);
    let result = loop {
        let message = match read_message(&mut reader).await {
            Ok(Some(message)) => message,
            Ok(None) => break Ok(()),
            Err(e) => break Err(io::Error::other(e)),
        };
        let request = match message {
            Ok(Message::Request(request)) => request,
            // Clients never answer us; we send no server-to-client requests.
            Ok(Message::Response(_)) => continue,
            Err(parse_error) => {
                let error =
                    RpcError::new(codes::PARSE_ERROR, format!("parse error: {parse_error}"));
                let _ = conn
                    .outbox
                    .send(Message::Response(Response::err(None, error)))
                    .await;
                continue;
            }
        };
        let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
            break Ok(());
        };
        let handler = Arc::clone(&handler);
        let conn = Arc::clone(&conn);
        tokio::spawn(async move {
            dispatch(handler, conn, request).await;
            drop(permit);
        });
    };

    let _ = closed_tx.send(true);
    handler.disconnected(&conn);
    drop(conn);
    // The writer drains whatever in-flight requests still send, then exits
    // once the last `Connection` clone is dropped.
    let _ = writer.await;
    result
}

async fn dispatch<H: Handler>(handler: Arc<H>, conn: Arc<Connection>, request: Request) {
    let Request {
        id, method, params, ..
    } = request;
    let params = params.unwrap_or(Value::Null);
    match id {
        None => {
            handler
                .notification(Arc::clone(&conn), method, params)
                .await
        }
        Some(id) => {
            // On a task of its own, so that a handler that panics still
            // answers: its caller is told, not left waiting for a reply
            // that will never come.
            let call = {
                let conn = Arc::clone(&conn);
                tokio::spawn(async move { handler.call(conn, method, params).await })
            };
            let response = match call.await {
                Ok(Ok(result)) => Response::ok(id, result),
                Ok(Err(error)) => Response::err(Some(id), error),
                Err(e) => {
                    tracing::error!(error = %e, "a request's handler didn't finish");
                    Response::err(
                        Some(id),
                        RpcError::new(
                            codes::INTERNAL_ERROR,
                            "internal error: the call couldn't be completed",
                        ),
                    )
                }
            };
            let _ = conn.outbox.send(Message::Response(response)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use serde_json::json;

    use super::*;
    use crate::Client;

    struct Echo;

    impl Handler for Echo {
        async fn call(
            &self,
            _conn: Arc<Connection>,
            _method: String,
            params: Value,
        ) -> Result<Value, RpcError> {
            Ok(params)
        }
    }

    /// A listener whose first accepts fail, as they do when the process is
    /// out of descriptors.
    struct Failing {
        listener: UnixListener,
        left: AtomicUsize,
    }

    impl Incoming for Failing {
        async fn next(&self) -> io::Result<UnixStream> {
            let failing = self
                .left
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok();
            if failing {
                return Err(io::Error::other("too many open files"));
            }
            self.listener.next().await
        }
    }

    #[tokio::test]
    async fn an_accept_that_fails_is_tried_again_and_the_listener_lives() {
        // It used to return on the first, and in glassd that was the
        // shutdown of every task (ADR 0055: tasks don't survive one).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let incoming = Failing {
            listener: UnixListener::bind(&path).unwrap(),
            left: AtomicUsize::new(3),
        };
        let serving = tokio::spawn(serve_from(incoming, Arc::new(Echo), Limits::SHARED));
        let client = Client::connect(&path).await.unwrap();
        let v: Value = client
            .call("echo", json!("after three failures"))
            .await
            .unwrap();
        assert_eq!(v, json!("after three failures"));
        assert!(!serving.is_finished());
    }

    #[test]
    fn the_pause_after_a_failed_accept_grows_and_has_a_ceiling() {
        assert_eq!(accept_backoff(1), Duration::from_millis(20));
        assert_eq!(accept_backoff(2), Duration::from_millis(40));
        assert_eq!(accept_backoff(7), accept_backoff(u32::MAX));
        assert!(accept_backoff(u32::MAX) < Duration::from_secs(2));
    }

    #[test]
    fn places_are_counted_for_all_and_for_each_user() {
        let open = Arc::new(Mutex::new(Open::default()));
        let limits = Limits {
            connections: 3,
            per_user: 2,
            shared_in_flight: false,
        };
        let a = Slot::take(&open, 1001, limits).unwrap();
        let _b = Slot::take(&open, 1001, limits).unwrap();
        assert!(Slot::take(&open, 1001, limits).is_none(), "two each");
        let _c = Slot::take(&open, 1002, limits).unwrap();
        assert!(Slot::take(&open, 1003, limits).is_none(), "three in all");
        drop(a);
        assert!(Slot::take(&open, 1003, limits).is_some());
        assert_eq!(
            open.lock().unwrap().all,
            2,
            "a place not kept is given back"
        );
    }
}
