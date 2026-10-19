//! The control socket: `glass-api` methods for `glassctl` and the desktop
//! shell. Every call is authorized by the caller's kernel-reported uid.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use glass_api::{
    AnswerApproval, Event, LayerChange, LayerRef, LayerRevert, TaskRef, TaskRequest, UndoRequest,
    methods,
};
use glass_rpc::{Connection, Handler, RpcError, parse_params, to_result};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

use crate::runtime::{OwnedEvent, blocking};
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
        let caller = self
            .rt
            .caller(conn.peer().uid)
            .ok_or_else(|| RpcError::permission_denied("unknown user"))?;
        let rt = &self.rt;
        // Reads stay in memory (task.list, task.get); calls that read or
        // write files run off the async workers that answer everyone else
        // (`blocking`).
        match method.as_str() {
            methods::SYSTEM_STATUS => to_result(&rt.status()),
            methods::AGENTS_LIST => to_result(&rt.agents_for(&caller)),
            methods::APPS_LIST => to_result(&rt.profile_apps(&caller)),
            methods::TASK_PLAN => {
                let request: TaskRequest = parse_params(params)?;
                to_result(&blocking(|| rt.plan(&caller, &request))?)
            }
            methods::TASK_START => {
                let request: TaskRequest = parse_params(params)?;
                to_result(&blocking(|| rt.start(&caller, request))?)
            }
            methods::TASK_LIST => to_result(&rt.list(&caller)),
            methods::TASK_GET => to_result(&rt.get(&caller, task_ref(params)?)?),
            methods::TASK_PAUSE => to_result(&rt.pause(&caller, task_ref(params)?)?),
            methods::TASK_RESUME => to_result(&rt.resume(&caller, task_ref(params)?)?),
            methods::TASK_STOP => to_result(&rt.stop(&caller, task_ref(params)?)?),
            methods::TASK_TAKE_OVER => to_result(&rt.take_over(&caller, task_ref(params)?).await?),
            methods::TASK_HAND_BACK => to_result(&rt.hand_back(&caller, task_ref(params)?).await?),
            methods::TASK_DISMISS => to_result(&rt.dismiss(&caller, task_ref(params)?)?),
            methods::TASK_RETRY => {
                let task = task_ref(params)?;
                to_result(&blocking(|| rt.retry(&caller, task))?)
            }
            methods::TASK_CONTINUE => {
                let task = task_ref(params)?;
                to_result(&blocking(|| rt.continue_task(&caller, task))?)
            }
            methods::TASK_UNDO => {
                let request: UndoRequest = parse_params(params)?;
                to_result(&rt.undo(&caller, request).await?)
            }
            methods::LAYER_GET => {
                let request: LayerRef = parse_params(params)?;
                to_result(&blocking(|| rt.layer_get(&caller, &request.agent))?)
            }
            methods::LAYER_SET => {
                let change: LayerChange = parse_params(params)?;
                to_result(&blocking(|| rt.layer_set(&caller, change))?)
            }
            methods::LAYER_REVERT => {
                let revert: LayerRevert = parse_params(params)?;
                to_result(&blocking(|| rt.layer_revert(&caller, revert))?)
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
        let events = self.rt.subscribe();
        tokio::spawn(async move {
            let visible = |owner: u32| caller.is_admin() || owner == caller.uid;
            let send = |event: Event| {
                let conn = Arc::clone(&conn);
                async move {
                    match serde_json::to_value::<&Event>(&event) {
                        Ok(params) => conn.notify(methods::EVENT, params).await,
                        Err(_) => true,
                    }
                }
            };
            let closed = Arc::clone(&conn);
            forward(events, visible, send, async move { closed.closed().await }).await;
        });
    }
}

/// Updates to one task within this long of each other go out as one, the
/// latest. Each carries the whole tile, so nothing is lost, and a burst (an
/// agent working fast, many tasks updating at once, new settings applied)
/// costs the shell one redraw per task, not one per update. The shell
/// settles for 50 ms itself before redrawing.
const COALESCE: Duration = Duration::from_millis(50);

/// Forwards the events `visible` allows to `send` until `send` fails, the
/// channel closes or `closed` resolves. An event that follows a quiet spell
/// goes at once; what follows it within [`COALESCE`] is gathered and sent
/// at the end of that window, the latest per task, and so on until a
/// window passes with nothing new.
async fn forward<S, F>(
    mut events: broadcast::Receiver<OwnedEvent>,
    visible: impl Fn(u32) -> bool,
    mut send: S,
    closed: impl Future<Output = ()>,
) where
    S: FnMut(Event) -> F,
    F: Future<Output = bool>,
{
    tokio::pin!(closed);
    let mut pending: Vec<Event> = Vec::new();
    let mut window: Option<tokio::time::Instant> = None;
    loop {
        let flush_at = window.unwrap_or_else(tokio::time::Instant::now);
        tokio::select! {
            received = events.recv() => match received {
                Ok((owner, event)) if visible(owner) => match window {
                    None => {
                        if !send(event).await {
                            return;
                        }
                        window = Some(tokio::time::Instant::now() + COALESCE);
                    }
                    Some(_) => merge(&mut pending, event),
                },
                Ok(_) => {}
                // A slow subscriber missed events; the next update carries
                // the full tile state anyway.
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            },
            () = tokio::time::sleep_until(flush_at), if window.is_some() => {
                if pending.is_empty() {
                    window = None;
                } else {
                    for event in pending.drain(..) {
                        if !send(event).await {
                            return;
                        }
                    }
                    window = Some(tokio::time::Instant::now() + COALESCE);
                }
            }
            () = &mut closed => return,
        }
    }
}

/// Adds `event` to what is waiting, replacing an earlier one for the same
/// task (keeping its place in line).
fn merge(pending: &mut Vec<Event>, event: Event) {
    let task = |e: &Event| match e {
        Event::TaskUpdated { task } => task.id,
        Event::TaskRemoved { task } => *task,
    };
    let id = task(&event);
    match pending.iter_mut().find(|e| task(e) == id) {
        Some(slot) => *slot = event,
        None => pending.push(event),
    }
}

fn task_ref(params: Value) -> Result<glass_types::TaskId, RpcError> {
    Ok(parse_params::<TaskRef>(params)?.task)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use glass_types::TaskId;

    use super::*;

    fn removed(task: TaskId) -> OwnedEvent {
        (1001, Event::TaskRemoved { task })
    }

    #[tokio::test]
    async fn bursts_arrive_coalesced_and_quiet_updates_at_once() {
        let (tx, rx) = broadcast::channel(1024);
        let got: Arc<Mutex<Vec<(tokio::time::Instant, TaskId)>>> = Arc::default();
        let sink = Arc::clone(&got);
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let forwarding = tokio::spawn(forward(
            rx,
            |owner| owner == 1001,
            move |event| {
                let sink = Arc::clone(&sink);
                async move {
                    if let Event::TaskRemoved { task } = event {
                        sink.lock()
                            .unwrap()
                            .push((tokio::time::Instant::now(), task));
                    }
                    true
                }
            },
            async move {
                let _ = stop_rx.await;
            },
        ));
        let (a, b, c) = (TaskId::new(), TaskId::new(), TaskId::new());
        let count = || got.lock().unwrap().len();

        tx.send(removed(a)).unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(count(), 1, "the first update goes at once");
        for _ in 0..100 {
            tx.send(removed(a)).unwrap();
        }
        tx.send(removed(b)).unwrap();
        tx.send((0, Event::TaskRemoved { task: c })).unwrap();
        tokio::time::sleep(Duration::from_millis(250)).await;
        let ids: Vec<TaskId> = got.lock().unwrap().iter().map(|(_, t)| *t).collect();
        assert_eq!(
            ids,
            [a, a, b],
            "a burst is one update per task; others' tasks stay hidden"
        );

        let sent = tokio::time::Instant::now();
        tx.send(removed(c)).unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let last = *got.lock().unwrap().last().unwrap();
        assert_eq!(last.1, c);
        assert!(
            last.0 - sent < COALESCE,
            "after a quiet spell an update isn't held back"
        );
        let _ = stop_tx.send(());
        forwarding.await.unwrap();
    }

    #[test]
    fn a_waiting_update_is_replaced_in_place() {
        let (a, b) = (TaskId::new(), TaskId::new());
        let mut pending = Vec::new();
        for task in [a, b, a] {
            merge(&mut pending, Event::TaskRemoved { task });
        }
        let ids: Vec<TaskId> = pending
            .iter()
            .map(|e| match e {
                Event::TaskRemoved { task } => *task,
                Event::TaskUpdated { task } => task.id,
            })
            .collect();
        assert_eq!(ids, [a, b]);
    }
}
