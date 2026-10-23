//! Tries an app someone made (GA-1, ADR 0049): its bundle runs once in a
//! make-believe page ([`page.js`](../page.js)) with a bridge that answers
//! every declared action with sample data and keeps each call. It reports
//! what the page showed when it opened, what it called, what pressing each
//! kind of button did (the same words, or a list's rows, are one kind), and
//! what went wrong. glassd runs it in the bundler's jail, after every save,
//! so the agent that wrote the app hears how it went.
//!
//! Nothing it runs reaches anything: no network, no files, no one's data.
//! Time is make-believe; a few seconds of it pass after opening and after
//! each press.

use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::rc::Rc;

use boa_engine::builtins::promise::{OperationType, PromiseState};
use boa_engine::context::HostHooks;
use boa_engine::module::IdleModuleLoader;
use boa_engine::object::builtins::JsPromise;
use boa_engine::{Context, JsError, JsObject, JsString, JsValue, Module, Source, js_string};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How much make-believe time passes after opening and after each press.
const SETTLE_MS: f64 = 3_000.0;
/// The most timers run while settling, and loop turns a script may take.
const MAX_TICKS: usize = 2_000;
const MAX_LOOP: u64 = 5_000_000;
const MAX_RECURSION: usize = 400;
/// The most buttons pressed, the most calls and errors kept, and how much
/// of the screen is told.
const MAX_PRESSES: usize = 8;
const MAX_KEPT: usize = 40;
const MAX_SCREEN_CHARS: usize = 3_000;

const PAGE: &str = include_str!("page.js");

/// What glassd hands the try-out: the app, and the actions it declares,
/// each with sample data shaped as the action's result.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Setup {
    pub app: String,
    pub name: String,
    pub user: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// By action name: `title`, `tier`, `side_effect`, `sample`.
    pub actions: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct Call {
    pub action: String,
    pub input: Value,
}

/// What was typed into a field before a press.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct Typed {
    /// The field, as the screen names it: its label or placeholder.
    pub field: String,
    pub text: String,
}

/// A kind of button on screen, as the page tells it.
#[derive(Debug, Deserialize)]
struct Pressable {
    label: String,
    /// The same for a list's rows, and through redraws.
    kind: String,
}

/// Pressing one button, or typing alone.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct Press {
    /// Empty when nothing was pressed: what the typing did by itself.
    pub button: String,
    /// What was typed into the empty fields just before.
    #[serde(default)]
    pub typed: Vec<Typed>,
    pub calls: Vec<Call>,
    pub errors: Vec<String>,
    /// Lines the screen gained.
    pub appeared: Vec<String>,
}

/// How the try went.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct Report {
    /// What went wrong while it opened: thrown, rejected and not caught.
    pub errors: Vec<String>,
    /// What it wrote with `console.error`.
    pub console_errors: Vec<String>,
    /// What it called while it opened.
    pub calls: Vec<Call>,
    /// What it showed then, as text: buttons in brackets.
    pub screen: String,
    pub presses: Vec<Press>,
}

/// Promises rejected with nobody to handle them: an `async` function
/// called and not awaited that threw.
#[derive(Default)]
struct Rejections {
    open: RefCell<Vec<JsObject>>,
}

impl HostHooks for Rejections {
    fn promise_rejection_tracker(
        &self,
        promise: &JsObject<boa_engine::builtins::promise::Promise>,
        operation: OperationType,
        _context: &mut Context,
    ) {
        let promise = promise.clone().upcast();
        let mut open = self.open.borrow_mut();
        match operation {
            OperationType::Reject => open.push(promise),
            OperationType::Handle => open.retain(|p| !JsObject::equals(p, &promise)),
        }
    }
}

/// Tries `bundle` as `setup`'s app. An error is the try-out's own failure,
/// not the app's.
pub fn try_app(bundle: &str, setup: &Setup) -> Result<Report, String> {
    let rejections = Rc::new(Rejections::default());
    let mut context = Context::builder()
        .module_loader(Rc::new(IdleModuleLoader))
        .host_hooks(rejections.clone())
        .build()
        .map_err(|e| e.to_string())?;
    let limits = context.runtime_limits_mut();
    limits.set_loop_iteration_limit(MAX_LOOP);
    limits.set_recursion_limit(MAX_RECURSION);
    let setup_json = serde_json::to_value(setup).map_err(|e| e.to_string())?;
    let setup_value = JsValue::from_json(&setup_json, &mut context).map_err(|e| e.to_string())?;
    context
        .global_object()
        .set(js_string!("__TRY"), setup_value, false, &mut context)
        .map_err(|e| e.to_string())?;
    context
        .eval(Source::from_bytes(PAGE))
        .map_err(|e| format!("the try-out's page: {e}"))?;

    let mut report = Report::default();
    match Module::parse(Source::from_bytes(bundle), None, &mut context) {
        Err(e) => report
            .errors
            .push(format!("it doesn't parse: {}", text(&e))),
        Ok(module) => {
            let opened = module.load_link_evaluate(&mut context);
            report.errors.extend(settle(&mut context));
            // Its own rejection is said here, not again as not caught.
            let own: JsObject = opened.clone().into();
            rejections
                .open
                .borrow_mut()
                .retain(|p| !JsObject::equals(p, &own));
            match opened.state() {
                PromiseState::Rejected(reason) => report.errors.push(format!(
                    "when it opened: {}",
                    text(&JsError::from_opaque(reason))
                )),
                PromiseState::Pending if report.errors.is_empty() => report.errors.push(
                    "it never finished opening: its top level waits on something that doesn't come"
                        .into(),
                ),
                _ => {}
            }
        }
    }
    let (calls, errors) = said(&mut context)?;
    report.calls = calls;
    report.errors.extend(errors.iter().cloned());
    report.errors.extend(unhandled(&rejections));
    report.screen = screen(&mut context)?;

    // Each kind of button once, those a press reveals (a form's) before
    // the rest; before each press, the empty fields typed into.
    let mut queue: VecDeque<Pressable> = VecDeque::new();
    let mut known: BTreeSet<String> = BTreeSet::new();
    let mut reveal =
        |context: &mut Context, queue: &mut VecDeque<Pressable>| -> Result<(), String> {
            let found: Vec<Pressable> = js_json(context, "__buttons")?;
            let new: Vec<Pressable> = found
                .into_iter()
                .filter(|b| known.insert(b.kind.clone()))
                .collect();
            for button in new.into_iter().rev() {
                queue.push_front(button);
            }
            Ok(())
        };
    reveal(&mut context, &mut queue)?;
    // What each press finds: the calls, errors and screen the last one left.
    let (mut calls_before, mut errors_before) = (report.calls.len(), errors.len());
    let mut shown = report_lines(&report.screen);
    let mut typed: Vec<Typed> = Vec::new();
    while report.presses.len() < MAX_PRESSES {
        let next = queue.pop_front();
        typed.extend(js_json::<Vec<Typed>>(&mut context, "__fill")?);
        let label = match next {
            Some(Pressable { label, kind }) => {
                let pressed = call_global(
                    &mut context,
                    "__press",
                    &[
                        JsValue::from(JsString::from(label.as_str())),
                        JsValue::from(JsString::from(kind.as_str())),
                    ],
                )?;
                if pressed.as_boolean() != Some(true) {
                    // Gone since: a redraw took it away.
                    continue;
                }
                label
            }
            // Nothing left to press, but a field to type into: a search
            // as you type answers to the typing alone.
            None if !typed.is_empty() => String::new(),
            None => break,
        };
        let stopped = settle(&mut context);
        let (calls_after, errors_after) = said(&mut context)?;
        let calls_now = calls_after.len();
        let errors_now = errors_after.len();
        let mut errors: Vec<String> = errors_after.into_iter().skip(errors_before).collect();
        errors.extend(stopped);
        errors.extend(unhandled(&rejections));
        let now_shown = report_lines(&screen(&mut context)?);
        let appeared = now_shown
            .iter()
            .filter(|l| !shown.contains(l))
            .take(10)
            .cloned()
            .collect();
        report.presses.push(Press {
            button: label,
            typed: std::mem::take(&mut typed),
            calls: calls_after
                .into_iter()
                .skip(calls_before)
                .take(MAX_KEPT)
                .collect(),
            errors,
            appeared,
        });
        (calls_before, errors_before, shown) = (calls_now, errors_now, now_shown);
        reveal(&mut context, &mut queue)?;
    }
    report.console_errors = console_errors(&mut context)?;
    report.errors.truncate(MAX_KEPT);
    report.calls.truncate(MAX_KEPT);
    Ok(report)
}

/// Runs what's due: promise jobs, then timers within [`SETTLE_MS`] of
/// make-believe time, in order. What stopped the engine itself, such as
/// a loop that never ends, which no `catch` in the page sees.
fn settle(context: &mut Context) -> Vec<String> {
    let mut stopped = Vec::new();
    let jobs = |context: &mut Context, stopped: &mut Vec<String>| {
        if let Err(e) = context.run_jobs() {
            stopped.push(format!("it was stopped: {}", text(&e)));
        }
    };
    let now = call_global(context, "__now", &[])
        .ok()
        .and_then(|v| v.as_number())
        .unwrap_or(0.0);
    let horizon = JsValue::from(now + SETTLE_MS);
    for _ in 0..MAX_TICKS {
        jobs(context, &mut stopped);
        match call_global(context, "__tick", std::slice::from_ref(&horizon)) {
            Ok(v) if v.as_boolean() == Some(true) => {}
            Ok(_) => break,
            Err(e) => {
                stopped.push(format!("it was stopped: {e}"));
                break;
            }
        }
    }
    jobs(context, &mut stopped);
    stopped.dedup();
    stopped
}

fn call_global(context: &mut Context, name: &str, args: &[JsValue]) -> Result<JsValue, String> {
    let function = context
        .global_object()
        .get(JsString::from(name), context)
        .map_err(|e| e.to_string())?;
    let function = function
        .as_callable()
        .ok_or_else(|| format!("the try-out's page has no {name}"))?;
    function
        .call(&JsValue::undefined(), args, context)
        .map_err(|e| text(&e))
}

/// The calls made and errors seen so far.
/// A page function's answer, as JSON read into `T`.
fn js_json<T: serde::de::DeserializeOwned>(context: &mut Context, name: &str) -> Result<T, String> {
    let value = call_global(context, name, &[])?
        .to_json(context)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    serde_json::from_value(value).map_err(|e| format!("{name}: {e}"))
}

fn said(context: &mut Context) -> Result<(Vec<Call>, Vec<String>), String> {
    let said = call_global(context, "__said", &[])?
        .to_json(context)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let calls = serde_json::from_value(said["calls"].clone()).unwrap_or_default();
    let errors = serde_json::from_value(said["errors"].clone()).unwrap_or_default();
    Ok((calls, errors))
}

fn console_errors(context: &mut Context) -> Result<Vec<String>, String> {
    let said = call_global(context, "__said", &[])?
        .to_json(context)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let mut errors: Vec<String> =
        serde_json::from_value(said["consoleErrors"].clone()).unwrap_or_default();
    errors.truncate(MAX_KEPT);
    Ok(errors)
}

fn screen(context: &mut Context) -> Result<String, String> {
    let text = call_global(context, "__screen", &[])?
        .as_string()
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    Ok(cut(&text, MAX_SCREEN_CHARS))
}

fn report_lines(screen: &str) -> Vec<String> {
    screen.lines().map(str::to_owned).collect()
}

/// Rejections nobody handled since the last look, then forgotten.
fn unhandled(rejections: &Rejections) -> Vec<String> {
    rejections
        .open
        .take()
        .into_iter()
        .filter_map(|promise| {
            let promise = JsPromise::from_object(promise).ok()?;
            match promise.state() {
                PromiseState::Rejected(reason) => Some(format!(
                    "not caught: {}",
                    text(&JsError::from_opaque(reason))
                )),
                _ => None,
            }
        })
        .collect()
}

/// An error as a line: boa's trace after it says nothing about the app's
/// own source, which the bundler minified.
fn text(error: &JsError) -> String {
    let full = error.to_string();
    let line = full.lines().next().unwrap_or_default();
    cut(line.trim_end_matches(" (native)"), 300)
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests;
