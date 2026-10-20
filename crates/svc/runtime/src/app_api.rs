//! The app socket (UI-5, ADR 0024): what an app runtime says to glassd.
//!
//! Every call is authorized by the caller's kernel-reported uid, which
//! decides who the app acts for. A session belongs to the connection that
//! opened it: no other connection can call through it, and it ends when
//! the connection does, so nothing an app was given outlives it (PA-2).
//! Only the app methods are served here; approvals, tasks and undo stay
//! on the control socket, answered on the desktop.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use glass_api::{AppCall, AppLive, AppOpen, AppSessionRef, AppStatusRequest, app_methods};
use glass_rpc::{Connection, Handler, RpcError, parse_params, to_result};
use glass_types::TaskId;
use serde_json::Value;

use crate::Runtime;
use crate::runtime::blocking;

/// Sessions one connection may hold at once (one per app window).
pub const MAX_SESSIONS_PER_CONNECTION: usize = 8;

pub struct AppApi {
    rt: Arc<Runtime>,
    /// The sessions each connection opened, by connection id.
    sessions: Arc<Mutex<Sessions>>,
}

type Sessions = BTreeMap<u64, BTreeSet<TaskId>>;

fn lock(sessions: &Mutex<Sessions>) -> std::sync::MutexGuard<'_, Sessions> {
    sessions.lock().unwrap_or_else(|p| p.into_inner())
}

impl AppApi {
    pub fn new(rt: Arc<Runtime>) -> Self {
        Self {
            rt,
            sessions: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, Sessions> {
        lock(&self.sessions)
    }

    /// The session, if this connection opened it. Others' read as unknown.
    fn owned(&self, conn: &Connection, session: TaskId) -> Result<TaskId, RpcError> {
        self.sessions()
            .get(&conn.id())
            .filter(|open| open.contains(&session))
            .map(|_| session)
            .ok_or_else(|| RpcError::not_found(format!("no app session {}", session.short())))
    }
}

impl Handler for AppApi {
    async fn call(
        &self,
        conn: Arc<Connection>,
        method: String,
        params: Value,
    ) -> Result<Value, RpcError> {
        let caller = self
            .rt
            .caller(conn.peer().uid)
            .ok_or_else(|| RpcError::permission_denied("unknown user"))?;
        let rt = &self.rt;
        match method.as_str() {
            app_methods::APP_OPEN => {
                let open: AppOpen = parse_params(params)?;
                let held = self.sessions().get(&conn.id()).map_or(0, BTreeSet::len);
                if held >= MAX_SESSIONS_PER_CONNECTION {
                    return Err(RpcError::conflict(format!(
                        "this connection already has {held} apps open"
                    )));
                }
                let session = blocking(|| rt.open_app(&caller, &open.app))?;
                let id = session.session;
                self.sessions().entry(conn.id()).or_default().insert(id);
                // The session ends with the connection, even one that
                // dropped while this call was still opening it (PA-2).
                let (rt, sessions) = (Arc::clone(rt), Arc::clone(&self.sessions));
                tokio::spawn(async move {
                    conn.closed().await;
                    rt.end_app_by_id(id);
                    let mut open = lock(&sessions);
                    if let Some(held) = open.get_mut(&conn.id()) {
                        held.remove(&id);
                        if held.is_empty() {
                            open.remove(&conn.id());
                        }
                    }
                });
                to_result(&session)
            }
            app_methods::APP_CALL => {
                let call: AppCall = parse_params(params)?;
                self.owned(&conn, call.session)?;
                to_result(&rt.app_call(&caller, call).await?)
            }
            app_methods::APP_STATUS => {
                let request = parse_params::<AppStatusRequest>(params)?;
                self.owned(&conn, request.session)?;
                to_result(&rt.app_status_wait(&caller, request).await?)
            }
            app_methods::APP_LIVE => {
                let live: AppLive = parse_params(params)?;
                self.owned(&conn, live.session)?;
                to_result(&rt.app_live(&caller, live).await?)
            }
            app_methods::APP_CLOSE => {
                let session = parse_params::<AppSessionRef>(params)?.session;
                self.owned(&conn, session)?;
                rt.close_app(&caller, session)?;
                if let Some(open) = self.sessions().get_mut(&conn.id()) {
                    open.remove(&session);
                }
                Ok(Value::Null)
            }
            app_methods::APP_FILES => {
                let open: AppOpen = parse_params(params)?;
                to_result(&blocking(|| rt.app_files(&caller, &open.app))?)
            }
            other => Err(RpcError::method_not_found(other)),
        }
    }
}
