//! The control socket: `glass-api` methods for `glassctl` and the desktop
//! shell. Every call is authorized by the caller's kernel-reported uid.

use std::sync::Arc;

use glass_api::{AnswerApproval, Event, TaskRef, TaskRequest, UndoRequest, methods};
use glass_rpc::{Connection, Handler, RpcError, parse_params, to_result};
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;

use crate::{Caller, Runtime};

pub struct ControlApi {
    rt: Arc<Runtime>,
}

impl ControlApi {
    pub fn new(rt: Arc<Runtime>) -> Self {
        Self { rt }
    }
}

impl Handler for ControlApi {
    async fn call(
        &self,
        conn: Arc<Connection>,
        method: String,
        params: Value,
    ) -> Result<Value, RpcError> {
        let caller = Caller::from_uid(conn.peer().uid)
            .ok_or_else(|| RpcError::permission_denied("unknown user"))?;
        let rt = &self.rt;
        match method.as_str() {
            methods::SYSTEM_STATUS => to_result(&rt.status()),
            methods::AGENTS_LIST => to_result(&rt.agents_for(&caller)),
            methods::TASK_PLAN => {
                let request: TaskRequest = parse_params(params)?;
                to_result(&rt.plan(&caller, &request)?)
            }
            methods::TASK_START => {
                let request: TaskRequest = parse_params(params)?;
                to_result(&rt.start(&caller, request)?)
            }
            methods::TASK_LIST => to_result(&rt.list(&caller)),
            methods::TASK_GET => to_result(&rt.get(&caller, task_ref(params)?)?),
            methods::TASK_PAUSE => to_result(&rt.pause(&caller, task_ref(params)?)?),
            methods::TASK_RESUME => to_result(&rt.resume(&caller, task_ref(params)?)?),
            methods::TASK_STOP => to_result(&rt.stop(&caller, task_ref(params)?)?),
            methods::TASK_TAKE_OVER => to_result(&rt.take_over(&caller, task_ref(params)?).await?),
            methods::TASK_HAND_BACK => to_result(&rt.hand_back(&caller, task_ref(params)?).await?),
            methods::TASK_DISMISS => to_result(&rt.dismiss(&caller, task_ref(params)?)?),
            methods::TASK_UNDO => {
                let request: UndoRequest = parse_params(params)?;
                to_result(&rt.undo(&caller, request).await?)
            }
            methods::APPROVAL_LIST => to_result(&rt.approvals(&caller)),
            methods::APPROVAL_ANSWER => {
                let request: AnswerApproval = parse_params(params)?;
                to_result(&rt.answer(&caller, request)?)
            }
            methods::AUDIT_TASK => to_result(&rt.audit_task(&caller, task_ref(params)?)?),
            methods::AUDIT_VERIFY => to_result(&rt.audit_verify(&caller)?),
            methods::SYSTEM_SUSPEND => to_result(&rt.suspend(&caller).await?),
            methods::SYSTEM_RESUME => to_result(&rt.wake(&caller)?),
            methods::EVENTS_SUBSCRIBE => {
                self.forward_events(caller, conn);
                Ok(Value::Null)
            }
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

impl ControlApi {
    /// Streams events about the caller's tasks (all tasks for root) until
    /// the client disconnects.
    fn forward_events(&self, caller: Caller, conn: Arc<Connection>) {
        let mut events = self.rt.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    received = events.recv() => match received {
                        Ok((owner, event)) => {
                            if !(caller.is_admin() || owner == caller.uid) {
                                continue;
                            }
                            let Ok(params) = serde_json::to_value::<&Event>(&event) else { continue };
                            if !conn.notify(methods::EVENT, params).await {
                                break;
                            }
                        }
                        // A slow subscriber missed events; the next update
                        // carries the full tile state anyway.
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => break,
                    },
                    () = conn.closed() => break,
                }
            }
        });
    }
}

fn task_ref(params: Value) -> Result<glass_types::TaskId, RpcError> {
    Ok(parse_params::<TaskRef>(params)?.task)
}
