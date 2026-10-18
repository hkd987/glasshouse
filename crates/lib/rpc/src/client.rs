//! A JSON-RPC client over a Unix socket.
//!
//! Calls may run concurrently; a background task routes responses back to
//! their callers and queues server notifications for [`Client::next_notification`].

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

use crate::framing::{read_message, write_message};
use crate::{Id, Message, Request, Response, RpcError, Version};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("cannot connect to {path}: {source}")]
    Connect {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("connection closed")]
    Closed,
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("unexpected response: {0}")]
    Decode(String),
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Response>>>>;

#[derive(Debug)]
pub struct Client {
    outbox: mpsc::Sender<Message>,
    pending: Pending,
    alive: Arc<AtomicBool>,
    next_id: AtomicI64,
    notifications: tokio::sync::Mutex<mpsc::Receiver<Request>>,
}

impl Client {
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        let path = path.as_ref();
        let stream = UnixStream::connect(path)
            .await
            .map_err(|source| ClientError::Connect {
                path: path.display().to_string(),
                source,
            })?;
        Ok(Self::from_stream(stream))
    }

    pub fn from_stream(stream: UnixStream) -> Self {
        let (read_half, mut write_half) = stream.into_split();
        let (outbox, mut queue) = mpsc::channel::<Message>(64);
        let (note_tx, note_rx) = mpsc::channel::<Request>(1024);
        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));

        tokio::spawn(async move {
            while let Some(message) = queue.recv().await {
                if write_message(&mut write_half, &message).await.is_err() {
                    break;
                }
            }
        });

        let routes = Arc::clone(&pending);
        let reader_alive = Arc::clone(&alive);
        tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            while let Ok(Some(message)) = read_message(&mut reader).await {
                match message {
                    Ok(Message::Response(response)) => {
                        let Some(Id::Num(id)) = response.id else {
                            continue;
                        };
                        let waiter = routes.lock().ok().and_then(|mut p| p.remove(&id));
                        if let Some(waiter) = waiter {
                            let _ = waiter.send(response);
                        }
                    }
                    Ok(Message::Request(request)) if request.id.is_none() => {
                        if note_tx.send(request).await.is_err() {
                            break;
                        }
                    }
                    Ok(Message::Request(_)) | Err(_) => {}
                }
            }
            // Fail every outstanding call: their responses will never come.
            // Calls check `alive` after registering, so none can slip past.
            reader_alive.store(false, Ordering::SeqCst);
            if let Ok(mut pending) = routes.lock() {
                pending.clear();
            }
        });

        Self {
            outbox,
            pending,
            alive,
            next_id: AtomicI64::new(1),
            notifications: note_rx.into(),
        }
    }

    /// Calls `method` and decodes its result.
    pub async fn call<R: DeserializeOwned>(
        &self,
        method: &str,
        params: impl Serialize,
    ) -> Result<R, ClientError> {
        let params =
            serde_json::to_value(params).map_err(|e| ClientError::Decode(e.to_string()))?;
        let value = self.call_value(method, params).await?;
        serde_json::from_value(value).map_err(|e| ClientError::Decode(e.to_string()))
    }

    pub async fn call_value(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| ClientError::Closed)?
            .insert(id, tx);
        if !self.alive.load(Ordering::SeqCst) {
            return Err(ClientError::Closed);
        }
        let request = Request {
            jsonrpc: Version,
            id: Some(Id::Num(id)),
            method: method.to_owned(),
            params: (!params.is_null()).then_some(params),
        };
        if self.outbox.send(Message::Request(request)).await.is_err() {
            return Err(ClientError::Closed);
        }
        let response = rx.await.map_err(|_| ClientError::Closed)?;
        match (response.result, response.error) {
            (_, Some(error)) => Err(ClientError::Rpc(error)),
            (Some(result), None) => Ok(result),
            (None, None) => Ok(Value::Null),
        }
    }

    /// Sends a notification (no response expected).
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), ClientError> {
        self.outbox
            .send(Message::notification(method, params))
            .await
            .map_err(|_| ClientError::Closed)
    }

    /// The next notification the server sent, or `None` once disconnected.
    pub async fn next_notification(&self) -> Option<Request> {
        self.notifications.lock().await.recv().await
    }
}
