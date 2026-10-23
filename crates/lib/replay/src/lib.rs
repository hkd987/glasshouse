//! Replay code from what a task did (NV-21, DA-4, ADR 0046).
//!
//! A task's tool calls, in order, with what each returned, compile into a
//! script for the agent's script runner: the same calls, as code, with no
//! model. The compiling is deterministic: the same calls always give the
//! same script, so two runs of a request that did the same thing give the
//! same code, and that is how the OS knows a flow repeats. The rules:
//!
//! - A value in a call's input that was never in an earlier result is a
//!   constant: `"Inbox"`, `"Archive"`.
//! - One taken from an item of an earlier result's list is that item's
//!   field (`item.id`), or a constant prefix and the field (`"Invoices/" +
//!   item.name`). An id-like value from an earlier result that isn't a list
//!   is that result's field (`r1.data.id`).
//! - A list in an input whose every element came from items of one earlier
//!   list, the same way, is that list's items mapped (`ids: [...]` is
//!   `r0.data.messages.map(item => item.id)`), kept by a rule as below when
//!   it left some out. When it comes out empty the call is skipped.
//! - Calls made once per item of an earlier list, in blocks of the same
//!   tools with the same constants, are a loop over that list. When they
//!   left some items out, the loop keeps the items the simplest rule
//!   picks: a field equal to one value, not equal to one value, or
//!   containing one word, tried in that order and field by field. When no
//!   rule fits, it replays the same calls.
//! - Every call is checked: one that fails stops the script, and the agent
//!   takes over (NV-21). The answer is what the calls that changed
//!   something said.
//! - A task that only read can't be replayed: its answer was the model's.
//!
//! Two flows are the same steps when their [`Replay::shape`]s are: what
//! changes things exactly the same, reads the same but for numbers such as
//! a limit, which a model sets one way one day and another the next.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One tool call a task made and what it returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Call {
    /// The tool, as the task socket names it: `files_list`.
    pub tool: String,
    pub input: Value,
    pub text: String,
    pub data: Value,
    /// It changed something (its action has a side effect).
    pub changes: bool,
}

/// A task's calls as code, and the steps it takes in words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replay {
    pub script: String,
    pub steps: Vec<String>,
    /// What makes two flows the same steps: every call that changes
    /// something exactly, with its loops and rules, and reads by their tool
    /// and their inputs other than numbers, which are limits and pages
    /// rather than what is read.
    pub shape: String,
}

/// The most calls compiled: a routine is a short flow.
pub const MAX_CALLS: usize = 400;
/// The longest run of calls repeated per item.
const MAX_BLOCK: usize = 4;

/// Compiles `calls` into replay code, or says why they can't be replayed.
pub fn compile(calls: &[Call]) -> Result<Replay, String> {
    if calls.is_empty() {
        return Err("it made no tool calls to replay".into());
    }
    if calls.len() > MAX_CALLS {
        return Err(format!(
            "it made {} tool calls; a routine replays at most {MAX_CALLS}",
            calls.len()
        ));
    }
    if !calls.iter().any(|c| c.changes) {
        return Err(
            "it only read, and its answer was the model's, so there are no steps \
                    to replay; a routine for it needs a script that works the answer out"
                .into(),
        );
    }
    let strings: Vec<Vec<(Path, String)>> = calls.iter().map(|c| strings_of(&c.data)).collect();
    let sources: Vec<Vec<Leaf>> = calls
        .iter()
        .enumerate()
        .map(|(i, call)| leaves(&call.input, &calls[..i], &strings[..i]))
        .collect();
    let steps = segment(calls, &sources);
    Ok(emit(calls, &sources, &steps))
}

// ---- where values came from ------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Field(String),
    Index(usize),
}

type Path = Vec<Key>;

/// A scalar in a call's input and where it came from.
#[derive(Debug, Clone)]
struct Leaf {
    path: Path,
    value: Value,
    from: Option<Source>,
}

#[derive(Debug, Clone, PartialEq)]
enum Source {
    /// A field of an item of a list in call `call`'s data: `list` is the
    /// list's path, `item` its index, `field` the path within the item,
    /// `prefix` a constant before it.
    Item {
        call: usize,
        list: Path,
        item: usize,
        field: Path,
        prefix: String,
    },
    /// A field of call `call`'s data that isn't in a list.
    Field { call: usize, path: Path },
}

fn scalars(value: &Value, path: &mut Path, out: &mut Vec<(Path, Value)>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                path.push(Key::Field(k.clone()));
                scalars(v, path, out);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                path.push(Key::Index(i));
                scalars(v, path, out);
                path.pop();
            }
        }
        scalar => out.push((path.clone(), scalar.clone())),
    }
}

/// An id, a file's name, an address: not a plain word, which a result may
/// hold by chance ("Inbox", "Archive").
fn id_like(text: &str) -> bool {
    text.chars().count() >= 4 && text.chars().any(|c| !c.is_alphabetic() && c != ' ')
}

/// A result's strings, by where they are in its data.
fn strings_of(data: &Value) -> Vec<(Path, String)> {
    let mut found = Vec::new();
    scalars(data, &mut Vec::new(), &mut found);
    found
        .into_iter()
        .filter_map(|(p, v)| match v {
            Value::String(s) => Some((p, s)),
            _ => None,
        })
        .collect()
}

/// The input's scalars, each with the most recent earlier result it came
/// from, if any.
fn leaves(input: &Value, earlier: &[Call], strings: &[Vec<(Path, String)>]) -> Vec<Leaf> {
    let mut found = Vec::new();
    scalars(input, &mut Vec::new(), &mut found);
    found
        .into_iter()
        .map(|(path, value)| {
            let from = value
                .as_str()
                .filter(|v| v.chars().count() >= 2)
                .and_then(|v| source(v, earlier, strings));
            Leaf { path, value, from }
        })
        .collect()
}

/// Where a list item's field is: the list's path, the item's index and the
/// field's path within it.
fn in_item(path: &Path) -> Option<(Path, usize, Path)> {
    let at = path.iter().rposition(|k| matches!(k, Key::Index(_)))?;
    let Key::Index(item) = path[at] else {
        return None;
    };
    Some((path[..at].to_vec(), item, path[at + 1..].to_vec()))
}

fn field_of<'a>(item: &'a Value, field: &Path) -> &'a Value {
    let mut at = item;
    for key in field {
        at = match key {
            Key::Field(k) => &at[k.as_str()],
            Key::Index(i) => &at[*i],
        };
    }
    at
}

fn source(value: &str, earlier: &[Call], strings: &[Vec<(Path, String)>]) -> Option<Source> {
    for (call, found) in strings.iter().enumerate().rev() {
        // An item's field names that item only if no other item of its
        // list has the same: "Inbox" in every message's folder names none.
        let names_one = |list: &Path, field: &Path, text: &str| {
            items_of(&earlier[call].data, list)
                .iter()
                .filter(|item| field_of(item, field).as_str() == Some(text))
                .count()
                == 1
        };
        // Exactly a value there, then a path ending in one.
        for (path, text) in found {
            if text != value {
                continue;
            }
            match in_item(path) {
                Some((list, item, field)) if names_one(&list, &field, text) => {
                    return Some(Source::Item {
                        call,
                        list,
                        item,
                        field,
                        prefix: String::new(),
                    });
                }
                Some(_) => {}
                None if id_like(value) => {
                    return Some(Source::Field {
                        call,
                        path: path.clone(),
                    });
                }
                None => {}
            }
        }
        for (path, text) in found {
            let Some(prefix) = value.strip_suffix(text.as_str()) else {
                continue;
            };
            if text.chars().count() >= 3
                && prefix.ends_with('/')
                && let Some((list, item, field)) = in_item(path)
                && names_one(&list, &field, text)
            {
                return Some(Source::Item {
                    call,
                    list,
                    item,
                    field,
                    prefix: prefix.to_owned(),
                });
            }
        }
    }
    None
}

// ---- loops -------------------------------------------------------------------

#[derive(Debug)]
enum Step {
    One(usize),
    Loop(Loop),
}

#[derive(Debug)]
struct Loop {
    /// The call whose data holds the list, and the list's path.
    call: usize,
    list: Path,
    /// The first call of the loop and how many calls each block has.
    start: usize,
    len: usize,
    /// The item each block was made for.
    items: Vec<usize>,
    /// Which items it keeps, when not all.
    keep: Option<Rule>,
}

#[derive(Debug, Clone, PartialEq)]
enum Rule {
    Is(String, Value),
    IsNot(String, Value),
    Has(String, String),
}

/// The list item a block of calls was made for, if every leaf from `list`
/// names the same one.
fn block_item(
    sources: &[Vec<Leaf>],
    range: std::ops::Range<usize>,
    of: (usize, &Path),
) -> Option<usize> {
    let mut item = None;
    for leaf in range.flat_map(|i| &sources[i]) {
        if let Some(Source::Item {
            call,
            list,
            item: k,
            ..
        }) = &leaf.from
            && (*call, list) == (of.0, of.1)
        {
            match item {
                None => item = Some(*k),
                Some(seen) if seen != *k => return None,
                Some(_) => {}
            }
        }
    }
    item
}

/// Whether block `b` (from `at`) does what block 0 (from `start`) did, for
/// its own item.
fn same_block(
    calls: &[Call],
    sources: &[Vec<Leaf>],
    start: usize,
    at: usize,
    len: usize,
    of: (usize, &Path),
) -> bool {
    (0..len).all(|p| {
        let (a, b) = (start + p, at + p);
        if calls[a].tool != calls[b].tool || sources[a].len() != sources[b].len() {
            return false;
        }
        sources[a].iter().zip(&sources[b]).all(|(x, y)| {
            if x.path != y.path {
                return false;
            }
            match (&x.from, &y.from) {
                // Made for the list's items, the same way.
                (
                    Some(Source::Item {
                        call: c1,
                        list: l1,
                        field: f1,
                        prefix: p1,
                        ..
                    }),
                    Some(Source::Item {
                        call: c2,
                        list: l2,
                        field: f2,
                        prefix: p2,
                        ..
                    }),
                ) if (*c1, l1) == (of.0, of.1) => (c2, l2, f2, p2) == (c1, l1, f1, p1),
                // From a call in the block, at the same place.
                (
                    Some(Source::Field { call: c1, path: q1 }),
                    Some(Source::Field { call: c2, path: q2 }),
                ) if (start..start + len).contains(c1) => {
                    c2.checked_sub(at) == Some(c1 - start) && q1 == q2
                }
                _ => x.value == y.value,
            }
        })
    })
}

fn segment(calls: &[Call], sources: &[Vec<Leaf>]) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut i = 0;
    while i < calls.len() {
        match best_loop(calls, sources, i, &steps) {
            Some(found) => {
                i = found.start + found.len * found.items.len();
                steps.push(Step::Loop(found));
            }
            None => {
                steps.push(Step::One(i));
                i += 1;
            }
        }
    }
    steps
}

/// The loop starting at call `i` that covers the most calls, if one does.
fn best_loop(calls: &[Call], sources: &[Vec<Leaf>], i: usize, before: &[Step]) -> Option<Loop> {
    // Lists in results of calls made once, outside any loop.
    let once: BTreeSet<usize> = before
        .iter()
        .filter_map(|s| match s {
            Step::One(c) => Some(*c),
            Step::Loop(_) => None,
        })
        .collect();
    let mut lists: Vec<(usize, Path)> = Vec::new();
    for leaf in &sources[i] {
        if let Some(Source::Item { call, list, .. }) = &leaf.from
            && once.contains(call)
            && !lists.contains(&(*call, list.clone()))
        {
            lists.push((*call, list.clone()));
        }
    }
    let mut best: Option<Loop> = None;
    for (call, list) in &lists {
        let of = (*call, list);
        let all = items_of(&calls[*call].data, list).len();
        for len in 1..=MAX_BLOCK {
            if i + len > calls.len() {
                break;
            }
            let Some(first) = block_item(sources, i..i + len, of) else {
                continue;
            };
            let mut items = vec![first];
            let mut at = i + len;
            while at + len <= calls.len() && same_block(calls, sources, i, at, len, of) {
                match block_item(sources, at..at + len, of) {
                    Some(k) if !items.contains(&k) => items.push(k),
                    _ => break,
                }
                at += len;
            }
            let keep = if items.len() == all {
                None
            } else {
                match rule(items_of(&calls[*call].data, list), &items) {
                    Some(rule) => Some(rule),
                    // No rule picks them: the same calls again.
                    None => continue,
                }
            };
            let covers = len * items.len();
            let better = best.as_ref().is_none_or(|b| covers > b.len * b.items.len());
            if better {
                best = Some(Loop {
                    call: *call,
                    list: list.clone(),
                    start: i,
                    len,
                    items: items.clone(),
                    keep,
                });
            }
        }
    }
    best
}

fn items_of<'a>(data: &'a Value, list: &Path) -> &'a [Value] {
    let mut at = data;
    for key in list {
        at = match key {
            Key::Field(k) => &at[k.as_str()],
            Key::Index(i) => &at[*i],
        };
    }
    at.as_array().map_or(&[], Vec::as_slice)
}

/// The simplest rule that keeps exactly the `chosen` items: a field equal
/// to one value, not equal to one value, or holding one word, tried in
/// that order, fields in name order. Ids are left out (a field named one,
/// or whose values all differ and all look like one): a rule on an id
/// picks those items and no others, ever. One item chosen is one example:
/// any field whose values all differ would single it out, so its rule must
/// be on a field other items share values of, and never a word. Likewise
/// "not this value" needs two items left out that share it.
fn rule(items: &[Value], chosen: &[usize]) -> Option<Rule> {
    let objects: Vec<&serde_json::Map<String, Value>> =
        items.iter().filter_map(Value::as_object).collect();
    if objects.len() != items.len() || chosen.is_empty() || chosen.len() == items.len() {
        return None;
    }
    let mut chosen_ones = vec![false; items.len()];
    for &i in chosen {
        if let Some(one) = chosen_ones.get_mut(i) {
            *one = true;
        }
    }
    let picked = |i: usize| chosen_ones[i];
    let mut fields: Vec<&String> = objects
        .first()?
        .keys()
        .filter(|k| {
            objects.iter().all(|o| {
                matches!(
                    o.get(*k),
                    Some(Value::String(_) | Value::Bool(_) | Value::Number(_))
                )
            })
        })
        .collect();
    fields.sort();
    let identifies = |f: &str| {
        let values: BTreeSet<String> = objects.iter().map(|o| o[f].to_string()).collect();
        f == "id"
            || f.ends_with("_id")
            || f.ends_with("Id")
            || (values.len() == objects.len()
                && objects.iter().all(|o| o[f].as_str().is_some_and(id_like)))
    };
    let fields: Vec<&String> = fields.into_iter().filter(|f| !identifies(f)).collect();
    // A field some items share values of: a category, not a name.
    let shared = |f: &str| {
        let values: BTreeSet<String> = objects.iter().map(|o| o[f].to_string()).collect();
        values.len() < objects.len()
    };
    let one = chosen.len() == 1;
    let all_same = |f: &str, want: bool| -> Option<Value> {
        let mut values = objects
            .iter()
            .enumerate()
            .filter(|(i, _)| picked(*i) == want)
            .map(|(_, o)| o[f].clone());
        let first = values.next()?;
        values.all(|v| v == first).then_some(first)
    };
    for f in &fields {
        if one && !shared(f) {
            continue;
        }
        if let Some(value) = all_same(f, true)
            && objects
                .iter()
                .enumerate()
                .all(|(i, o)| picked(i) || o[f.as_str()] != value)
        {
            return Some(Rule::Is((*f).clone(), value));
        }
    }
    let left_out = items.len() - chosen.len();
    for f in fields
        .iter()
        .filter(|f| left_out >= 2 && (!one || shared(f)))
    {
        if let Some(value) = all_same(f, false)
            && objects
                .iter()
                .enumerate()
                .all(|(i, o)| !picked(i) || o[f.as_str()] != value)
        {
            return Some(Rule::IsNot((*f).clone(), value));
        }
    }
    let words = |text: &str| -> BTreeSet<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.chars().count() >= 3)
            .map(str::to_owned)
            .collect()
    };
    for f in fields.iter().filter(|_| !one) {
        let texts: Vec<Option<&str>> = objects.iter().map(|o| o[f.as_str()].as_str()).collect();
        if texts.iter().any(Option::is_none) {
            continue;
        }
        let mut common: Option<BTreeSet<String>> = None;
        for (i, text) in texts.iter().enumerate() {
            if picked(i) {
                let these = words(text.unwrap_or_default());
                common = Some(match common {
                    None => these,
                    Some(c) => c.intersection(&these).cloned().collect(),
                });
            }
        }
        let others: BTreeSet<String> = texts
            .iter()
            .enumerate()
            .filter(|(i, _)| !picked(*i))
            .flat_map(|(_, t)| words(t.unwrap_or_default()))
            .collect();
        if let Some(word) = common
            .unwrap_or_default()
            .into_iter()
            .find(|w| !others.contains(w))
        {
            return Some(Rule::Has((*f).clone(), word));
        }
    }
    None
}

// ---- code ----------------------------------------------------------------------

fn accessor(path: &Path) -> String {
    let mut out = String::new();
    for key in path {
        match key {
            Key::Field(k)
                if k.chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') =>
            {
                let _ = write!(out, ".{k}");
            }
            Key::Field(k) => {
                let _ = write!(out, "[{}]", Value::String(k.clone()));
            }
            Key::Index(i) => {
                let _ = write!(out, "[{i}]");
            }
        }
    }
    out
}

fn words_path(path: &Path) -> String {
    path.iter()
        .filter_map(|k| match k {
            Key::Field(k) => Some(k.as_str()),
            Key::Index(_) => None,
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// `value` as JavaScript, with the leaves at `with`'s paths replaced by
/// their expressions.
fn js(value: &Value, path: &mut Path, with: &BTreeMap<Path, String>) -> String {
    if let Some(expr) = with.get(path) {
        return expr.clone();
    }
    match value {
        Value::Object(map) => {
            let fields: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    path.push(Key::Field(k.clone()));
                    let field = format!("{}: {}", Value::String(k.clone()), js(v, path, with));
                    path.pop();
                    field
                })
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
        Value::Array(items) => {
            let items: Vec<String> = items
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    path.push(Key::Index(i));
                    let item = js(v, path, with);
                    path.pop();
                    item
                })
                .collect();
            format!("[{}]", items.join(", "))
        }
        scalar => scalar.to_string(),
    }
}

fn rule_js(rule: &Rule) -> String {
    match rule {
        Rule::Is(f, v) => format!("item{} === {v}", accessor(&vec![Key::Field(f.clone())])),
        Rule::IsNot(f, v) => format!("item{} !== {v}", accessor(&vec![Key::Field(f.clone())])),
        Rule::Has(f, w) => format!(
            "String(item{}).toLowerCase().includes({})",
            accessor(&vec![Key::Field(f.clone())]),
            Value::String(w.clone())
        ),
    }
}

fn rule_words(rule: &Rule) -> String {
    match rule {
        Rule::Is(f, v) => format!(" whose {f} is {v}"),
        Rule::IsNot(f, v) => format!(" whose {f} isn't {v}"),
        Rule::Has(f, w) => format!(" whose {f} has the word \"{w}\""),
    }
}

fn emit(calls: &[Call], sources: &[Vec<Leaf>], steps: &[Step]) -> Replay {
    let mut script = String::from(
        "// Written by this computer from the calls a task made, with no model.\n\
         function need(r, tool) { if (!r.ok) throw new Error(tool + \": \" + r.text); return r; }\n\
         const said = [];\n",
    );
    let mut words = Vec::new();
    let mut shape = Vec::new();
    // Calls made once, by the variable that holds each one's result.
    let mut once: BTreeSet<usize> = BTreeSet::new();
    for step in steps {
        match step {
            Step::One(i) => {
                let call = &calls[*i];
                let named = |c: usize| once.contains(&c).then(|| format!("r{c}"));
                let mut with = expressions(&sources[*i], named, None);
                let mut shown = with.clone();
                // Lists made from a listing's items: worked out first, and
                // the call skipped when one comes out empty.
                let mut guards = Vec::new();
                for (n, (path, expr)) in lists(&call.input, &sources[*i], calls, named)
                    .into_iter()
                    .enumerate()
                {
                    let var = format!("l{i}_{n}");
                    let _ = writeln!(script, "const {var} = {expr};");
                    with.insert(path.clone(), var.clone());
                    shown.insert(path, expr);
                    guards.push(format!("{var}.length"));
                }
                let input = js(&call.input, &mut Vec::new(), &with);
                let tool = Value::String(call.tool.clone());
                if guards.is_empty() {
                    let _ = writeln!(script, "const r{i} = need(call({tool}, {input}), {tool});");
                    if call.changes {
                        let _ = writeln!(script, "said.push(r{i}.text);");
                    }
                } else {
                    let when = guards.join(" && ");
                    let _ = writeln!(
                        script,
                        "const r{i} = {when} ? need(call({tool}, {input}), {tool}) : {{ ok: true, text: \"\", data: {{}} }};"
                    );
                    if call.changes {
                        let _ = writeln!(script, "if ({when}) said.push(r{i}.text);");
                    }
                }
                let step = format!("{} {}", call.tool, js(&call.input, &mut Vec::new(), &shown));
                shape.push(shape_of(call, &step, &shown));
                words.push(step);
                once.insert(*i);
            }
            Step::Loop(each) => {
                let _ = writeln!(
                    script,
                    "for (const item of r{}.data{} || []) {{",
                    each.call,
                    accessor(&each.list)
                );
                if let Some(rule) = &each.keep {
                    let _ = writeln!(script, "  if (!({})) continue;", rule_js(rule));
                }
                shape.push(format!(
                    "for r{}.data{} {}",
                    each.call,
                    accessor(&each.list),
                    each.keep.as_ref().map(rule_js).unwrap_or_default()
                ));
                let mut body = Vec::new();
                for p in 0..each.len {
                    let i = each.start + p;
                    let call = &calls[i];
                    let with = expressions(
                        &sources[i],
                        |c| {
                            if once.contains(&c) {
                                Some(format!("r{c}"))
                            } else if (each.start..i).contains(&c) {
                                Some(format!("s{}", c - each.start))
                            } else {
                                None
                            }
                        },
                        Some((each.call, &each.list)),
                    );
                    let input = js(&call.input, &mut Vec::new(), &with);
                    let _ = writeln!(
                        script,
                        "  const s{p} = need(call({}, {input}), {});",
                        Value::String(call.tool.clone()),
                        Value::String(call.tool.clone())
                    );
                    if call.changes {
                        let _ = writeln!(script, "  said.push(s{p}.text);");
                    }
                    let step = format!("{} {input}", call.tool);
                    shape.push(format!("  {}", shape_of(call, &step, &with)));
                    body.push(step);
                }
                script.push_str("}\n");
                words.push(format!(
                    "For each of step {}'s {}{}: {}",
                    each.call + 1,
                    words_path(&each.list),
                    each.keep.as_ref().map(rule_words).unwrap_or_default(),
                    body.join("; then ")
                ));
            }
        }
    }
    script.push_str("return said.length ? said.join(\"\\n\") : \"Nothing to do this time.\";\n");
    Replay {
        script,
        steps: words,
        shape: shape.join("\n"),
    }
}

/// A call's part of a flow's shape: as written when it changes something;
/// a read without the numbers among its constant inputs.
fn shape_of(call: &Call, written: &str, with: &BTreeMap<Path, String>) -> String {
    if call.changes {
        return written.to_owned();
    }
    let mut leaves = Vec::new();
    scalars(&call.input, &mut Vec::new(), &mut leaves);
    let kept: Vec<String> = leaves
        .into_iter()
        .filter(|(path, value)| with.contains_key(path) || !value.is_number())
        .map(|(path, value)| match with.get(&path) {
            Some(expr) => format!("{}={expr}", words_path(&path)),
            None => format!("{}={value}", words_path(&path)),
        })
        .collect();
    format!("{} {}", call.tool, kept.join(" "))
}

/// The lists in `input` whose every element came from items of one earlier
/// list the code can name (`named`), the same way: each as an expression
/// over that list, by where it is in the input.
fn lists(
    input: &Value,
    leaves: &[Leaf],
    calls: &[Call],
    named: impl Fn(usize) -> Option<String>,
) -> Vec<(Path, String)> {
    let mut by_list: BTreeMap<Path, Vec<&Leaf>> = BTreeMap::new();
    for leaf in leaves {
        if let Some((Key::Index(_), parent)) = leaf.path.split_last() {
            by_list.entry(parent.to_vec()).or_default().push(leaf);
        }
    }
    let mut found = Vec::new();
    for (parent, elements) in by_list {
        let length = field_of(input, &parent).as_array().map_or(0, Vec::len);
        let in_order = elements
            .iter()
            .enumerate()
            .all(|(k, e)| e.path.last() == Some(&Key::Index(k)));
        if elements.len() != length || !in_order {
            continue;
        }
        let Some(Source::Item {
            call,
            list,
            field,
            prefix,
            ..
        }) = &elements[0].from
        else {
            continue;
        };
        let mut chosen = Vec::new();
        let same = elements.iter().all(|e| match &e.from {
            Some(Source::Item {
                call: c,
                list: l,
                item,
                field: f,
                prefix: p,
            }) => {
                let fresh = !chosen.contains(item);
                chosen.push(*item);
                (c, l, f, p) == (call, list, field, prefix) && fresh
            }
            _ => false,
        });
        let Some(var) = named(*call).filter(|_| same) else {
            continue;
        };
        let items = items_of(&calls[*call].data, list);
        let keep = match chosen.len() == items.len() {
            true => String::new(),
            false => match rule(items, &chosen) {
                Some(rule) => format!(".filter(item => {})", rule_js(&rule)),
                None => continue,
            },
        };
        let each = match prefix.is_empty() {
            true => format!("item{}", accessor(field)),
            false => format!(
                "{} + item{}",
                Value::String(prefix.clone()),
                accessor(field)
            ),
        };
        found.push((
            parent,
            format!(
                "({var}.data{} || []){keep}.map(item => {each})",
                accessor(list)
            ),
        ));
    }
    found
}

/// The expressions for `leaves` that came from results the code can name:
/// `named` gives a call's variable, and `item` is the loop's list.
fn expressions(
    leaves: &[Leaf],
    named: impl Fn(usize) -> Option<String>,
    item: Option<(usize, &Path)>,
) -> BTreeMap<Path, String> {
    let mut with = BTreeMap::new();
    for leaf in leaves {
        let expr = match &leaf.from {
            Some(Source::Item {
                call,
                list,
                field,
                prefix,
                ..
            }) if item == Some((*call, list)) => {
                let field = format!("item{}", accessor(field));
                if prefix.is_empty() {
                    field
                } else {
                    format!("{} + {field}", Value::String(prefix.clone()))
                }
            }
            Some(Source::Field { call, path }) => match named(*call) {
                Some(var) => format!("{var}.data{}", accessor(path)),
                None => continue,
            },
            // Another list's item outside its loop: the same one again.
            _ => continue,
        };
        with.insert(leaf.path.clone(), expr);
    }
    with
}

#[cfg(test)]
mod tests;
