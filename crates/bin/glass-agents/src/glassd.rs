//! glassd's control socket, as the front uses it: one connection, made
//! again when glassd restarts. glassd decides who the caller is by the
//! connection's uid, so the front acts as the account it runs as (ADR 0066,
//! "Identity on a host").

use std::path::PathBuf;
use std::sync::Arc;

use glass_rpc::{Client, ClientError, codes};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;

/// An error for people, with the HTTP status it answers with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub status: u16,
    pub message: String,
}

impl Failure {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl From<ClientError> for Failure {
    fn from(e: ClientError) -> Self {
        match e {
            ClientError::Rpc(e) => {
                let status = match e.code {
                    codes::INVALID_PARAMS | codes::INVALID_REQUEST => 400,
                    codes::PERMISSION_DENIED => 403,
                    codes::NOT_FOUND => 404,
                    codes::CONFLICT => 409,
                    _ => 500,
                };
                Self::new(status, e.message)
            }
            ClientError::Connect { .. } | ClientError::Closed => Self::new(
                503,
                "Glasshouse isn't answering on this host; try again in a moment",
            ),
            ClientError::Decode(why) => {
                Self::new(502, format!("an answer we couldn't read: {why}"))
            }
        }
    }
}

#[derive(Debug)]
pub struct Glassd {
    socket: PathBuf,
    client: Mutex<Option<Arc<Client>>>,
}

impl Glassd {
    pub fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            client: Mutex::new(None),
        }
    }

    async fn client(&self) -> Result<Arc<Client>, ClientError> {
        let mut held = self.client.lock().await;
        if let Some(client) = held.as_ref() {
            return Ok(Arc::clone(client));
        }
        let client = Arc::new(Client::connect(&self.socket).await?);
        *held = Some(Arc::clone(&client));
        Ok(client)
    }

    /// Calls `method`, connecting again once if glassd went away.
    pub async fn call<R: DeserializeOwned>(
        &self,
        method: &str,
        params: impl Serialize,
    ) -> Result<R, Failure> {
        let params = serde_json::to_value(params)
            .map_err(|e| Failure::new(500, format!("a request we couldn't write: {e}")))?;
        for attempt in 0..2 {
            let client = self.client().await?;
            match client.call(method, params.clone()).await {
                Err(ClientError::Closed) if attempt == 0 => {
                    *self.client.lock().await = None;
                }
                other => return other.map_err(Failure::from),
            }
        }
        Err(ClientError::Closed.into())
    }

    /// A connection of its own, for glassd's events.
    pub async fn subscribe(&self) -> Result<Client, ClientError> {
        let client = Client::connect(&self.socket).await?;
        let _: serde_json::Value = client
            .call(
                glass_api::methods::EVENTS_SUBSCRIBE,
                serde_json::Value::Null,
            )
            .await?;
        Ok(client)
    }
}
