//! A JSON-RPC server over a Unix listener.
//!
//! Each connection gets a reader loop and a writer task. Requests run
//! concurrently (up to [`MAX_IN_FLIGHT`] per connection) so one slow call,
//! such as an action waiting on an approval, never blocks the rest.

use std::future::Future;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Semaphore, mpsc, watch};

use crate::framing::{read_message, write_message};
use crate::{Message, Request, Response, RpcError, codes};

/// Concurrent requests allowed per connection before the reader applies
/// backpressure.
pub const MAX_IN_FLIGHT: usize = 32;

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

/// Accepts connections forever, serving each on its own task.
pub async fn serve<H: Handler>(listener: UnixListener, handler: Arc<H>) -> io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let handler = Arc::clone(&handler);
        tokio::spawn(async move {
            if let Err(e) = serve_connection(stream, handler).await {
                tracing::debug!(error = %e, "connection ended with error");
            }
        });
    }
}

/// Serves one connection until the client hangs up.
pub async fn serve_connection<H: Handler>(stream: UnixStream, handler: Arc<H>) -> io::Result<()> {
    let peer = PeerCred::of(&stream)?;
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
            if let Err(e) = write_message(&mut write_half, &message).await {
                tracing::debug!(error = %e, "write failed; dropping connection output");
                break;
            }
        }
    });

    let permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
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
            let response = match handler.call(Arc::clone(&conn), method, params).await {
                Ok(result) => Response::ok(id, result),
                Err(error) => Response::err(Some(id), error),
            };
            let _ = conn.outbox.send(Message::Response(response)).await;
        }
    }
}
