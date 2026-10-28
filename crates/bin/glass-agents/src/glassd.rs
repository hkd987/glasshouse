//! glassd's control socket, as the front uses it.
//!
//! - On a laptop or a development host the front acts as the account it
//!   runs as: one connection, made again when glassd restarts.
//! - On an agent host each request has a connection of its own that acts
//!   for the person it proves (ADR 0067). glassd checks the proof itself,
//!   and refuses the front's own account anything else.

use std::path::PathBuf;
use std::sync::Arc;

use glass_api::{IssuedToken, PersonView, SessionAs, TokenExchange, methods};
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

tokio::task_local! {
    /// This request's own connection, acting for its person (ADR 0067).
    static SESSION: Arc<Client>;
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

    /// A connection of its own that acts for the person `proof` proves.
    /// A proof glassd refuses is 401: sign in again.
    pub async fn session(&self, proof: &SessionAs) -> Result<(Arc<Client>, PersonView), Failure> {
        let client = Client::connect(&self.socket).await.map_err(Failure::from)?;
        let person: PersonView = client.call(methods::SESSION_AS, proof).await.map_err(|e| {
            let failure = Failure::from(e);
            if failure.status == 403 {
                Failure::new(401, failure.message)
            } else {
                failure
            }
        })?;
        Ok((Arc::new(client), person))
    }

    /// Runs `work` with every call through `session`.
    pub async fn scoped<F: std::future::Future>(session: Arc<Client>, work: F) -> F::Output {
        SESSION.scope(session, work).await
    }

    /// This request's session, if it has one.
    pub fn session_client() -> Option<Arc<Client>> {
        SESSION.try_with(Arc::clone).ok()
    }

    /// Trades a code `glass login` brought for a token: the front's own
    /// call, for no one yet.
    pub async fn exchange(&self, code: &str) -> Result<IssuedToken, Failure> {
        let client = Client::connect(&self.socket).await.map_err(Failure::from)?;
        client
            .call(
                methods::TOKEN_EXCHANGE,
                TokenExchange {
                    code: code.to_owned(),
                },
            )
            .await
            .map_err(|e| {
                let failure = Failure::from(e);
                if failure.status == 403 {
                    Failure::new(401, failure.message)
                } else {
                    failure
                }
            })
    }

    /// Calls `method`: through this request's session if it has one, or
    /// else the front's own connection, made again once if glassd went
    /// away.
    pub async fn call<R: DeserializeOwned>(
        &self,
        method: &str,
        params: impl Serialize,
    ) -> Result<R, Failure> {
        let params = serde_json::to_value(params)
            .map_err(|e| Failure::new(500, format!("a request we couldn't write: {e}")))?;
        if let Some(session) = Self::session_client() {
            return session.call(method, params).await.map_err(Failure::from);
        }
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
