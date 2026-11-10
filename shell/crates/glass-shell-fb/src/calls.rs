//! The desktop's calls to glassd, made off its loop.
//!
//! The desktop is one loop: it reads the keyboard and pointer, draws, and
//! turns the clock. A call awaited on that loop holds all of it for as long
//! as glassd takes to answer, and a glassd that has stopped answering holds
//! it for good: no pointer, no keys, no way to sign out. So the loop hands
//! each call here and goes on; the answer comes back to it as an [`Answer`]
//! to take in, like any other thing that happens.
//!
//! Calls are made one at a time, in the order they were asked for (a Pause
//! then a Resume reach glassd as that), and each is given up on after
//! [`limit`]: glassd may still carry it out, and what it did then shows in
//! the events that follow. Once one call on a connection has gone
//! unanswered, those waiting behind it on the same connection get
//! [`STALLED`] each, not their whole limit: the desktop is about to connect
//! again, and what was asked meanwhile is told it wasn't done now, not
//! minutes later.

use std::sync::Arc;
use std::time::{Duration, Instant};

use glass_api::settings::{Brightness, BrightnessChange, BrightnessStep, SettingsView};
use glass_api::{AnswerApproval, ProfileApp, TaskDetail, TaskRef, TaskView, UndoReport, methods};
use glass_rpc::{Client, ClientError};
use glass_shell::live::Effect;
use glass_types::{AgentId, TaskId};
use tokio::sync::mpsc;

/// What glassd said to a call an [`Effect`] asked for, or why it said
/// nothing.
#[derive(Debug)]
pub enum Answer {
    /// `approval.answer`.
    Approval {
        answer: AnswerApproval,
        result: Result<(), ClientError>,
    },
    /// `settings.get`.
    Settings(Result<SettingsView, ClientError>),
    /// `settings.set`, for the setting with this id.
    SettingChosen {
        id: String,
        result: Result<SettingsView, ClientError>,
    },
    /// `display.brightness`.
    Brightness(Result<Brightness, ClientError>),
    /// `apps.list`.
    Apps(Result<Vec<ProfileApp>, ClientError>),
    /// `task.get`, and how long it took.
    Detail {
        task: TaskId,
        took: Duration,
        result: Result<TaskDetail, ClientError>,
    },
    /// `task.start`.
    Started {
        agent: AgentId,
        result: Result<TaskView, ClientError>,
    },
    /// `task.undo`.
    Undone(Result<UndoReport, ClientError>),
    /// A call on one task (`task.pause` and the like), by its method.
    Task {
        method: &'static str,
        result: Result<TaskView, ClientError>,
    },
    /// `task.dismiss` for one or more ended tasks: the first that failed.
    Dismissed(Result<(), ClientError>),
}

impl Answer {
    /// Whether glassd took the call and never answered it: the sign that
    /// it has stopped answering, though the connection is open.
    pub fn unanswered(&self) -> bool {
        let error = match self {
            Answer::Approval { result, .. } => result.as_ref().err(),
            Answer::Settings(result) | Answer::SettingChosen { result, .. } => {
                result.as_ref().err()
            }
            Answer::Brightness(result) => result.as_ref().err(),
            Answer::Apps(result) => result.as_ref().err(),
            Answer::Detail { result, .. } => result.as_ref().err(),
            Answer::Started { result, .. } | Answer::Task { result, .. } => result.as_ref().err(),
            Answer::Undone(result) => result.as_ref().err(),
            Answer::Dismissed(result) => result.as_ref().err(),
        };
        matches!(error, Some(ClientError::TimedOut { .. }))
    }
}

/// How long a call has on a connection whose last call went unanswered.
pub const STALLED: Duration = Duration::from_secs(1);

/// How long glassd has to answer the call `effect` asks for. Long enough
/// that only a glassd that isn't answering runs out of it: a roll back
/// restores files, a start builds a sandbox; the rest read or flip a state.
pub fn limit(effect: &Effect) -> Duration {
    match effect {
        Effect::Undo(_) => Duration::from_secs(300),
        Effect::Start(_) => Duration::from_secs(60),
        Effect::Brightness { .. } => Duration::from_secs(5),
        _ => Duration::from_secs(20),
    }
}

/// The queue of calls: [`Calls::make`] adds one, and its [`Answer`] comes
/// out of the receiver [`Calls::start`] returned.
#[derive(Debug)]
pub struct Calls {
    queue: mpsc::UnboundedSender<(Arc<Client>, Effect)>,
}

impl Calls {
    /// Starts making calls, on the runtime this is called in.
    pub fn start() -> (Self, mpsc::UnboundedReceiver<Answer>) {
        Self::start_within(None)
    }

    /// [`start`](Self::start), with every call given `most` at most: for
    /// tests, which can't wait out [`limit`].
    pub fn start_within(most: Option<Duration>) -> (Self, mpsc::UnboundedReceiver<Answer>) {
        let (queue, mut waiting) = mpsc::unbounded_channel::<(Arc<Client>, Effect)>();
        let (answers, answered) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            // The connection whose last call wasn't answered, by where
            // its client is: only ever compared, never looked at.
            let mut stalled: Option<usize> = None;
            while let Some((client, effect)) = waiting.recv().await {
                let line = Arc::as_ptr(&client) as usize;
                let mut within = limit(&effect);
                if stalled == Some(line) {
                    within = within.min(STALLED);
                }
                if let Some(most) = most {
                    within = within.min(most);
                }
                let Some(answer) = call(&client, effect, within).await else {
                    continue;
                };
                stalled = answer.unanswered().then_some(line);
                // The desktop is gone: nobody to tell.
                if answers.send(answer).is_err() {
                    return;
                }
            }
        });
        (Self { queue }, answered)
    }

    /// Asks for the call `effect` needs, on `client`, after those already
    /// asked for. An effect that needs no call is dropped.
    pub fn make(&self, client: &Arc<Client>, effect: Effect) {
        // The only way this fails is the queue's task having ended, which
        // it does when the desktop does.
        let _ = self.queue.send((Arc::clone(client), effect));
    }
}

/// Makes the call `effect` asks for; `None` for an effect that isn't a
/// call to glassd (the desktop does those itself).
async fn call(client: &Client, effect: Effect, within: Duration) -> Option<Answer> {
    Some(match effect {
        Effect::Answer(answer) => {
            let result: Result<serde_json::Value, _> = client
                .call_within(methods::APPROVAL_ANSWER, &answer, within)
                .await;
            Answer::Approval {
                answer,
                result: result.map(|_| ()),
            }
        }
        Effect::LoadSettings => {
            Answer::Settings(client.call_within(methods::SETTINGS_GET, (), within).await)
        }
        Effect::ChooseSetting(choice) => Answer::SettingChosen {
            result: client
                .call_within(methods::SETTINGS_SET, &choice, within)
                .await,
            id: choice.id,
        },
        Effect::Brightness { up } => {
            let change = BrightnessChange {
                step: Some(if up {
                    BrightnessStep::Up
                } else {
                    BrightnessStep::Down
                }),
                percent: None,
            };
            Answer::Brightness(
                client
                    .call_within(methods::DISPLAY_BRIGHTNESS, &change, within)
                    .await,
            )
        }
        Effect::ListApps => Answer::Apps(client.call_within(methods::APPS_LIST, (), within).await),
        Effect::Get(task) => {
            let started = Instant::now();
            let result = client
                .call_within(methods::TASK_GET, TaskRef { task }, within)
                .await;
            Answer::Detail {
                task,
                took: started.elapsed(),
                result,
            }
        }
        Effect::Start(request) => Answer::Started {
            result: client
                .call_within(methods::TASK_START, &request, within)
                .await,
            agent: request.agent,
        },
        Effect::Undo(request) => Answer::Undone(
            client
                .call_within(methods::TASK_UNDO, &request, within)
                .await,
        ),
        // glassd answers each with nothing and says the task is gone
        // (`TaskRemoved`), which is what takes its tile off the screen.
        Effect::Dismiss(tasks) => {
            let mut result = Ok(());
            for task in tasks {
                let answer: Result<serde_json::Value, _> = client
                    .call_within(methods::TASK_DISMISS, TaskRef { task }, within)
                    .await;
                if let Err(e) = answer {
                    result = Err(e);
                    break;
                }
            }
            Answer::Dismissed(result)
        }
        other => {
            let (method, task) = other.task_call()?;
            Answer::Task {
                method,
                result: client.call_within(method, &task, within).await,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::Path;

    use super::*;

    /// A glassd that answers `display.brightness` and nothing else: every
    /// other call is taken and never answered, as by a glassd that hung
    /// with the connection open.
    fn glassd(socket: &Path) {
        let listener = UnixListener::bind(socket).expect("a socket in the test's folder");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                std::thread::spawn(move || {
                    let mut answers = stream.try_clone().expect("a second handle");
                    let mut level = 50;
                    for line in BufReader::new(stream).lines() {
                        let Ok(line) = line else { return };
                        let request: serde_json::Value =
                            serde_json::from_str(&line).expect("a request");
                        if request["method"] != methods::DISPLAY_BRIGHTNESS {
                            continue;
                        }
                        level += if request["params"]["step"] == "up" {
                            10
                        } else {
                            -10
                        };
                        let answer = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": request["id"],
                            "result": { "present": true, "percent": level },
                        });
                        if writeln!(answers, "{answer}").is_err() {
                            return;
                        }
                    }
                });
            }
        });
    }

    fn percent(answer: Option<Answer>) -> Option<u8> {
        match answer {
            Some(Answer::Brightness(Ok(level))) => level.percent,
            other => panic!("not a brightness: {other:?}"),
        }
    }

    #[tokio::test]
    async fn calls_are_made_in_the_order_asked_and_answered_in_it() {
        let dir = tempfile::tempdir().expect("a folder");
        let socket = dir.path().join("control.sock");
        glassd(&socket);
        let client = Arc::new(Client::connect(&socket).await.expect("the stand-in glassd"));
        let (calls, mut answers) = Calls::start();
        for up in [true, true, false, true] {
            calls.make(&client, Effect::Brightness { up });
        }
        // Not a call to glassd: nothing comes back for it.
        calls.make(&client, Effect::SignOut);
        calls.make(&client, Effect::Brightness { up: false });
        let mut seen = Vec::new();
        for _ in 0..5 {
            seen.push(percent(answers.recv().await));
        }
        assert_eq!(seen, [Some(60), Some(70), Some(60), Some(70), Some(60)]);
    }

    // The desktop awaited each call on its loop: one glassd never answered
    // held the pointer, the keys and the clock for good.
    #[tokio::test]
    async fn a_call_glassd_never_answers_is_given_up_on_and_the_next_is_made() {
        let dir = tempfile::tempdir().expect("a folder");
        let socket = dir.path().join("control.sock");
        glassd(&socket);
        let client = Arc::new(Client::connect(&socket).await.expect("the stand-in glassd"));
        let (calls, mut answers) = Calls::start_within(Some(Duration::from_millis(100)));
        let task = TaskId::new();
        let asked = Instant::now();
        calls.make(&client, Effect::Get(task));
        calls.make(&client, Effect::Pause(task));
        calls.make(&client, Effect::Brightness { up: true });
        // Asking took no time: the loop that asked goes on at once.
        assert!(asked.elapsed() < Duration::from_millis(50));
        match answers.recv().await {
            Some(Answer::Detail {
                task: of,
                result: Err(ClientError::TimedOut { .. }),
                ..
            }) => assert_eq!(of, task),
            other => panic!("the task's detail should have been given up on: {other:?}"),
        }
        match answers.recv().await {
            Some(Answer::Task {
                method,
                result: Err(ClientError::TimedOut { .. }),
            }) => assert_eq!(method, methods::TASK_PAUSE),
            other => panic!("the pause should have been given up on: {other:?}"),
        }
        assert_eq!(percent(answers.recv().await), Some(60));
    }

    #[tokio::test]
    async fn calls_on_a_connection_that_closed_fail_at_once() {
        let dir = tempfile::tempdir().expect("a folder");
        let socket = dir.path().join("control.sock");
        let listener = UnixListener::bind(&socket).expect("a socket");
        let client = Arc::new(Client::connect(&socket).await.expect("connected"));
        // glassd goes: what it accepted closes with it.
        drop(listener.accept().expect("the desktop's connection"));
        drop(listener);
        let (calls, mut answers) = Calls::start();
        let asked = Instant::now();
        calls.make(
            &client,
            Effect::Start(glass_api::TaskRequest {
                persona: None,
                agent: "desktop-agent".parse().expect("an agent id"),
                prompt: "Draft it".into(),
                scope: glass_api::ScopeRequest::default(),
                title: None,
                computer: None,
            }),
        );
        match answers.recv().await {
            Some(Answer::Started {
                result: Err(ClientError::Closed),
                ..
            }) => {}
            other => panic!("the start should have failed as closed: {other:?}"),
        }
        assert!(
            asked.elapsed() < Duration::from_secs(5),
            "long before its limit"
        );
    }
}
