//! Scripts agents run instead of working figures out in their heads, and
//! routines that run without a model (ADR 0044).
//!
//! A script is JavaScript, run by Boa with every `import` refused and no
//! files, network, processes or timers: the language has none, and the
//! host adds none. Its one way out is `call(tool, input)`, which the host
//! makes on the task socket, where tiers, asks, Never, labels, scope, the
//! audit log and undo apply as to any call. A few helpers do what models
//! get wrong by hand: dates and rounding.
//!
//! Scripts run synchronously on a thread of their own; [`run_on`] makes
//! their calls on an agent's socket.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::future::Future;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use boa_engine::builtins::promise::PromiseState;
use boa_engine::module::IdleModuleLoader;
use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::{
    Context, JsError, JsNativeError, JsResult, JsString, JsValue, NativeFunction, Source,
};
use glass_agent_sdk::Agent;
use serde_json::{Value, json};

/// Tool calls one script may make.
pub const MAX_CALLS: u32 = 100;
/// A script's own time, not counting its waits for tool calls: what one
/// does between calls is arithmetic on what they returned.
pub const MAX_RUN: Duration = Duration::from_secs(60);
/// Iterations of any one loop, and calls deep: a runaway script stops.
const MAX_LOOP: u64 = 100_000;
const MAX_RECURSION: usize = 256;
/// What the host keeps of what a script printed, and of an error.
const MAX_PRINTED: usize = 200;
const MAX_ERROR_CHARS: usize = 2_000;

/// What a tool call returned, as a script sees it: `{ ok, text, data }`.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub ok: bool,
    pub text: String,
    /// The tool's structured data (a mail list's messages), or `null`.
    pub data: Value,
}

/// Makes a script's tool calls.
pub trait Tools {
    /// One call. `Err` when it couldn't be made at all (the socket went
    /// away); a call policy refused or that failed is a [`Reply`] with
    /// `ok: false`, for the script to handle.
    fn call(&mut self, name: &str, input: Value) -> Result<Reply, String>;
}

/// A script that ran to the end.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// The value of its last expression (or what its promise settled to),
    /// as JSON; `null` if it has none.
    pub result: Value,
    /// What it printed, a line a call.
    pub printed: Vec<String>,
    /// Tool calls it made.
    pub calls: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScriptError {
    /// It didn't parse, threw, or hit a limit. The message says where, for
    /// whoever wrote it to fix.
    #[error("{0}")]
    Script(String),
    /// The host couldn't run it, such as the task socket going away.
    #[error("couldn't run the script: {0}")]
    Host(String),
}

/// Helpers defined before every script. Dates are `YYYY-MM-DD`; a longer
/// timestamp's date part counts. `console.log` is `print`. They're plain
/// properties of the global object, so a script's own `round` or `today`
/// replaces them instead of being a redeclaration error.
const PRELUDE: &str = r#"
globalThis.__date = function (d) {
  const t = Date.parse(String(d).slice(0, 10));
  if (Number.isNaN(t)) throw new TypeError("not a date: " + d);
  return t;
};
globalThis.daysBetween = function (a, b) { return Math.round((__date(b) - __date(a)) / 86400000); };
globalThis.addDays = function (d, n) { return new Date(__date(d) + n * 86400000).toISOString().slice(0, 10); };
globalThis.weekday = function (d) {
  return ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"][new Date(__date(d)).getUTCDay()];
};
globalThis.round = function (x, places) { const f = 10 ** (places === undefined ? 2 : places); return Math.round(x * f) / f; };
globalThis.console = { log: print, info: print, warn: print, error: print };
"#;

/// The running script's host side. Native functions are plain `fn`s,
/// with nothing for the collector to trace, so they find it here.
struct State {
    tools: Box<dyn Tools>,
    printed: Vec<String>,
    calls: u32,
    /// Why the host stopped the script, which a `catch` can't hide.
    failed: Option<String>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Runs `source` to the end on this thread. `today` is the computer's
/// local date, `YYYY-MM-DD`, for the script's `today()`.
pub fn run(source: &str, today: &str, tools: Box<dyn Tools>) -> Result<Run, ScriptError> {
    if !is_date(today) {
        return Err(ScriptError::Host(format!("{today:?} isn't a date")));
    }
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            tools,
            printed: Vec::new(),
            calls: 0,
            failed: None,
        });
    });
    let outcome = evaluate(source, today);
    let state = STATE
        .with(|s| s.borrow_mut().take())
        .ok_or_else(|| ScriptError::Host("the script's state went missing".into()))?;
    if let Some(why) = state.failed {
        return Err(ScriptError::Host(why));
    }
    Ok(Run {
        result: outcome?,
        printed: state.printed,
        calls: state.calls,
    })
}

fn is_date(text: &str) -> bool {
    text.len() == 10
        && text.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

/// `value` with JavaScript's whole numbers as integers: `22`, not `22.0`,
/// which a tool asking for an integer would refuse.
fn tidy(value: Value) -> Value {
    match value {
        Value::Number(n) => match n.as_f64() {
            // Exact: whole and below 2^53.
            #[allow(clippy::cast_possible_truncation)]
            Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 => {
                Value::from(f as i64)
            }
            _ => Value::Number(n),
        },
        Value::Array(items) => Value::Array(items.into_iter().map(tidy).collect()),
        Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, tidy(v))).collect()),
        other => other,
    }
}

fn cut(text: &str) -> String {
    if text.chars().count() <= MAX_ERROR_CHARS {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(MAX_ERROR_CHARS).collect();
    cut.push('…');
    cut
}

fn evaluate(source: &str, today: &str) -> Result<Value, ScriptError> {
    let thrown = |e: JsError| ScriptError::Script(cut(&e.to_string()));
    // Every import is refused: the default loader would read modules from
    // the current folder.
    let mut context = Context::builder()
        .module_loader(Rc::new(IdleModuleLoader))
        .build()
        .map_err(|e| ScriptError::Host(e.to_string()))?;
    let limits = context.runtime_limits_mut();
    limits.set_loop_iteration_limit(MAX_LOOP);
    limits.set_recursion_limit(MAX_RECURSION);
    let natives: [(&str, usize, NativeFunctionPointer); 2] =
        [("call", 2, call), ("print", 1, print)];
    for (name, length, function) in natives {
        context
            .register_global_callable(
                JsString::from(name),
                length,
                NativeFunction::from_fn_ptr(function),
            )
            .map_err(|e| ScriptError::Host(e.to_string()))?;
    }
    let prelude = format!("{PRELUDE}\nglobalThis.today = function () {{ return \"{today}\"; }};\n");
    context
        .eval(Source::from_bytes(&prelude))
        .map_err(|e| ScriptError::Host(e.to_string()))?;
    // A script may end with its result or `return` it, as a function body
    // would, and may `await` calls as if they were fetches: models write
    // all three. An async function's body takes both.
    let value = match context.eval(Source::from_bytes(source)) {
        Err(e)
            if e.to_string().starts_with("SyntaxError")
                && (source.contains("return") || source.contains("await")) =>
        {
            // On the script's first line, so errors say its own lines.
            let body = format!("(async function () {{ {source}\n}})()");
            context
                .eval(Source::from_bytes(&body))
                .map_err(|_| thrown(e))?
        }
        other => other.map_err(thrown)?,
    };
    // Promises settle, so an async script's result is what it resolved to.
    context.run_jobs().map_err(thrown)?;
    let value = match value.as_promise().map(|p| p.state()) {
        Some(PromiseState::Fulfilled(value)) => value,
        Some(PromiseState::Rejected(reason)) => {
            return Err(ScriptError::Script(cut(
                &JsError::from_opaque(reason).to_string()
            )));
        }
        Some(PromiseState::Pending) => {
            return Err(ScriptError::Script(
                "the script's promise never settled".into(),
            ));
        }
        None => value,
    };
    Ok(tidy(
        value
            .to_json(&mut context)
            .map_err(thrown)?
            .unwrap_or(Value::Null),
    ))
}

/// `call(tool, input)`: one call on the task socket, `{ ok, text, data }`.
fn call(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = match args.first() {
        Some(name) if name.is_string() => name.to_string(context)?.to_std_string_escaped(),
        _ => {
            return Err(JsNativeError::typ()
                .with_message("call(tool, input): the tool is a name, such as \"files_list\"")
                .into());
        }
    };
    let input = match args.get(1) {
        Some(input) if !input.is_undefined() => {
            tidy(input.to_json(context)?.unwrap_or_else(|| json!({})))
        }
        _ => json!({}),
    };
    let reply = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let state = s
            .as_mut()
            .ok_or_else(|| "no script is running".to_owned())?;
        if state.calls >= MAX_CALLS {
            return Ok(Err(format!(
                "a script makes at most {MAX_CALLS} tool calls"
            )));
        }
        state.calls += 1;
        match state.tools.call(&name, input) {
            Ok(reply) => Ok(Ok(reply)),
            Err(why) => {
                state.failed = Some(why.clone());
                Err(why)
            }
        }
    });
    match reply {
        Ok(Ok(reply)) => JsValue::from_json(
            &json!({ "ok": reply.ok, "text": reply.text, "data": reply.data }),
            context,
        ),
        Ok(Err(limit)) => Err(JsNativeError::range().with_message(limit).into()),
        Err(why) => Err(JsNativeError::error().with_message(why).into()),
    }
}

/// `print(...)`: a line of what the script found, strings as they are and
/// the rest as JSON.
fn print(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let mut parts = Vec::with_capacity(args.len());
    for arg in args {
        parts.push(if arg.is_string() {
            arg.to_string(context)?.to_std_string_escaped()
        } else {
            arg.to_json(context)?
                .map_or_else(|| "undefined".to_owned(), |json| tidy(json).to_string())
        });
    }
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut()
            && state.printed.len() < MAX_PRINTED
        {
            state.printed.push(parts.join(" "));
        }
    });
    Ok(JsValue::undefined())
}

type Request = (
    String,
    Value,
    tokio::sync::oneshot::Sender<Result<Reply, String>>,
);

/// A script's calls, sent to the async side that owns the agent.
struct Bridge(tokio::sync::mpsc::Sender<Request>);

impl Tools for Bridge {
    fn call(&mut self, name: &str, input: Value) -> Result<Reply, String> {
        let (back, reply) = tokio::sync::oneshot::channel();
        self.0
            .blocking_send((name.to_owned(), input, back))
            .map_err(|_| "the agent stopped".to_owned())?;
        reply
            .blocking_recv()
            .map_err(|_| "the agent stopped".to_owned())?
    }
}

/// Runs `source` for `agent`, on a thread of its own, its calls made on
/// the agent's task socket to the tools in `allowed` only; any other is a
/// failed call the script sees. It has [`MAX_RUN`] of its own time.
pub async fn run_on(
    agent: &Agent,
    source: String,
    today: String,
    allowed: BTreeSet<String>,
) -> Result<Run, ScriptError> {
    run_with(source, today, MAX_RUN, |name, input| {
        let allowed = allowed.contains(&name);
        async move {
            if !allowed {
                return Ok(Reply {
                    ok: false,
                    text: format!("There's no tool {name:?} here."),
                    data: Value::Null,
                });
            }
            agent
                .call(&name, input)
                .await
                .map_err(|e| e.to_string())
                .map(|r| Reply {
                    ok: !r.is_error,
                    text: r.text(),
                    data: r.structured_content.unwrap_or(Value::Null),
                })
        }
    })
    .await
}

/// Scripts given up on whose threads haven't ended yet.
static LEFT_RUNNING: AtomicUsize = AtomicUsize::new(0);

/// Where a script's thread and whoever waits for it agree on how it
/// went: given up on, ended, or both.
#[derive(Default)]
struct Fate {
    given_up: bool,
    ended: bool,
}

/// A script's thread: counted as left running from when its time is up
/// until it ends.
struct Running(Arc<Mutex<Fate>>);

impl Drop for Running {
    /// The thread's end.
    fn drop(&mut self) {
        let mut fate = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        fate.ended = true;
        if fate.given_up {
            LEFT_RUNNING.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// A script's time is up: counted as left running, unless it has ended.
fn give_up(fate: &Mutex<Fate>) {
    let mut fate = fate.lock().unwrap_or_else(PoisonError::into_inner);
    if !fate.ended && !fate.given_up {
        fate.given_up = true;
        LEFT_RUNNING.fetch_add(1, Ordering::SeqCst);
    }
}

/// [`run_on`], its calls made by `call` and its own time `limit`.
///
/// The engine stops a loop after [`MAX_LOOP`] turns and calls
/// [`MAX_RECURSION`] deep, but it counts each call's loops apart, so loops
/// that call each other run for as long as anyone waits (audit 2.10): a
/// routine that never ended, holding its task. A script's own time is
/// what the clock here counts, between its tool calls: a call may wait
/// for the person to answer an ask, for as long as that takes.
///
/// A thread can't be stopped from outside. One whose time is up is told
/// at its next tool call (the call fails, and that ends the script) and
/// otherwise runs on, in its task's sandbox and under its limits, until
/// its task ends; while it does, this process runs no other script.
async fn run_with<F, Fut>(
    source: String,
    today: String,
    limit: Duration,
    mut call: F,
) -> Result<Run, ScriptError>
where
    F: FnMut(String, Value) -> Fut,
    Fut: Future<Output = Result<Reply, String>>,
{
    if LEFT_RUNNING.load(Ordering::SeqCst) > 0 {
        return Err(ScriptError::Script(
            "an earlier script here ran past its time and is still running, so no other \
             can run in this task"
                .into(),
        ));
    }
    let (calls, mut requests) = tokio::sync::mpsc::channel::<Request>(1);
    let (done, mut finished) = tokio::sync::oneshot::channel();
    let fate = Arc::new(Mutex::new(Fate::default()));
    let running = Running(Arc::clone(&fate));
    // A thread of its own, not one of the runtime's: the process can end
    // while a script given up on still runs.
    std::thread::Builder::new()
        .name("glass-script".into())
        .spawn(move || {
            let _counted = running;
            let _ = done.send(run(&source, &today, Box::new(Bridge(calls))));
        })
        .map_err(|e| ScriptError::Host(format!("no thread for the script: {e}")))?;
    let mut left = limit;
    loop {
        let began = Instant::now();
        let next = tokio::time::timeout(left, async {
            tokio::select! {
                // First, so a script that finished in time is never
                // counted as out of it.
                biased;
                done = &mut finished => Err(done),
                request = requests.recv() => Ok(request),
            }
        })
        .await;
        left = left.saturating_sub(began.elapsed());
        match next {
            Ok(Err(done)) => {
                return done
                    .map_err(|_| ScriptError::Host("the script's thread stopped".into()))?;
            }
            Ok(Ok(Some((name, input, back)))) => {
                // Not on the script's clock.
                let _ = back.send(call(name, input).await);
            }
            // The script is done with its tools: its result is on its way.
            Ok(Ok(None)) => {}
            Err(_) => {
                give_up(&fate);
                return Err(ScriptError::Script(format!(
                    "the script ran for more than {} seconds of its own time without finishing; \
                     a loop that doesn't end?",
                    limit.as_secs().max(1)
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests;
