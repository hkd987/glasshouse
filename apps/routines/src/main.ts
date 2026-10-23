// Routines (DA-4, NV-21, ADR 0047): what runs for the person, with no
// model unless its code fails. Each routine: what it does, when it runs
// next, how it last ran, what it can reach, the code itself and its runs;
// run it now, pause it, change when it runs, remove it. Above them, what
// the person did the same way more than once, which this computer can
// keep as a routine from the code it wrote (ADR 0046). A routine's
// versions, each against the one in use, and going back to one. Every
// change goes through the bridge and the audit log like any action; the
// page keeps nothing. glassd wakes it when a run starts or ends, or a
// routine changes anywhere (`app.status`).

import { Bridge, BridgeError, Offline, h, ready, show } from "@glasshouse/sdk";

import {
  type Choice,
  DAY_CHOICES,
  type Repeat,
  type Routine,
  type RoutineDetail,
  type Run,
  type Version,
  choiceOf,
  choiceProblem,
  diffLines,
  folder,
  nameFor,
  outcome,
  past,
  scheduleOf,
  tool,
  upcoming,
  whenLine,
} from "./routines.ts";

const bridge = new Bridge({ app: "routines" });
const root = document.getElementById("app") as HTMLElement;

const state = {
  user: "",
  loaded: false,
  routines: [] as Routine[],
  repeats: [] as Repeat[],
  /** The routine open, by id, from the address's #. */
  open: null as string | null,
  detail: null as RoutineDetail | null,
  notice: null as { text: string; tone: "ok" | "bad" } | null,
  busy: null as string | null,
  /** The schedule picker, open for a routine or a repeat being kept. */
  picking: null as { for: string; choice: Choice } | null,
  /** A repeat being kept: its name, and when it should run. */
  keeping: null as { task: string; name: string; choice: Choice } | null,
  /** Asking before a routine is removed. */
  removing: null as string | null,
  /** A version shown against the one in use, once read. */
  comparing: null as { routine: string; version: number; detail: RoutineDetail | null } | null,
  /** Where the page's wait for changes picks up (`app.status`). */
  cursor: 0,
  listening: false,
};

function message(error: unknown): string {
  if (error instanceof Offline) return "The system isn't answering right now. Nothing changed.";
  if (error instanceof BridgeError) return error.message;
  return String(error);
}

async function load(): Promise<void> {
  try {
    if (!state.user) state.user = (await bridge.open()).user;
    // The cursor before the first read, so a change in between is news
    // (NV-3, ADR 0051); offline, the first wait tries again.
    if (!state.listening) {
      state.listening = true;
      state.cursor = await bridge.status().then((s) => s.cursor, () => 0);
      void listen();
    }
    const [listed, repeats] = await Promise.all([
      bridge.call<{ routines: Routine[] }>("routines.list", {}),
      bridge.call<{ repeats: Repeat[] }>("routines.repeats", {}),
    ]);
    if (listed.ok) state.routines = listed.data.routines;
    else state.notice = { text: listed.text, tone: "bad" };
    state.repeats = repeats.ok ? repeats.data.repeats : [];
    if (state.open) {
      const read = await bridge.call<RoutineDetail>("routines.read", { routine: state.open });
      state.detail = read.ok ? read.data : null;
      if (!read.ok) state.open = null;
      const comparing = state.comparing;
      if (comparing && (comparing.routine !== state.open || !state.detail?.versions.some((v) => v.version === comparing.version && !v.in_use))) {
        state.comparing = null;
      }
    }
  } catch (error) {
    state.notice = { text: error instanceof Offline ? "Routines can't be loaded right now." : message(error), tone: "bad" };
  }
  state.loaded = true;
  render();
  ready();
}

/** Runs one action, says how it went, and loads again. */
async function act(busy: string, action: string, input: unknown, after?: (data: unknown) => void): Promise<void> {
  state.busy = busy;
  state.notice = null;
  render();
  try {
    const result = await bridge.call(action, input);
    state.notice = { text: result.text, tone: result.ok ? "ok" : "bad" };
    if (result.ok) after?.(result.data);
  } catch (error) {
    state.notice = { text: message(error), tone: "bad" };
  }
  state.busy = null;
  await load();
}

/** Says what's missing before anything is sent. */
function refuse(text: string): void {
  state.notice = { text, tone: "bad" };
  render();
}

function runNow(r: Routine): Promise<void> {
  return act(`run:${r.routine}`, "routines.run", { routine: r.routine });
}

/**
 * Waits on glassd for the person's routines to change (25 s at a time):
 * a run starting or ending, wherever it was started, or a routine saved,
 * paused or removed by their agent or another window. Then loads again.
 */
async function listen(): Promise<void> {
  for (;;) {
    try {
      const status = await bridge.status({ waitSecs: 25, since: state.cursor });
      state.cursor = status.cursor;
      // A change of the page's own reloads when it's done.
      if (status.changed.includes("routines") && state.busy === null) await load();
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 3000));
    }
  }
}

/** Shows version `version` of the open routine against the one in use, or stops. */
async function compare(d: RoutineDetail, version: number): Promise<void> {
  if (state.comparing?.version === version) {
    state.comparing = null;
    return render();
  }
  state.comparing = { routine: d.routine, version, detail: null };
  render();
  try {
    const read = await bridge.call<RoutineDetail>("routines.read", { routine: d.routine, version });
    if (state.comparing?.version !== version) return;
    if (read.ok) state.comparing.detail = read.data;
    else {
      state.comparing = null;
      state.notice = { text: read.text, tone: "bad" };
    }
  } catch (error) {
    state.comparing = null;
    state.notice = { text: message(error), tone: "bad" };
  }
  render();
}

function navigate(): void {
  const id = decodeURIComponent(location.hash.replace(/^#\/?/, ""));
  state.open = id || null;
  state.detail = null;
  state.picking = null;
  state.removing = null;
  state.comparing = null;
  state.notice = null;
  void load();
}

// ---- pieces -----------------------------------------------------------------

function button(label: string, attrs: Record<string, unknown>, primary = false): HTMLButtonElement {
  return h("button", { class: primary ? "button primary" : "button", type: "button", ...(attrs as object) }, label);
}

function lastRun(r: Routine): HTMLElement {
  const now = new Date();
  if (r.running) {
    return h("div", { class: "last running", "data-test": "running" }, h("span", { class: "dot running" }), "Running now");
  }
  const run = r.last_run;
  if (!run) return h("div", { class: "last" }, h("span", { class: "dot none" }), "Hasn't run yet");
  const how = outcome(run);
  return h(
    "div",
    { class: "last", "data-test": "last-run" },
    h("span", { class: `dot ${how.tone}` }),
    h("span", {}, `${run.scheduled ? "Ran" : "You ran it"} ${past(run.ended_at, now)} · ${how.text}`),
    run.said ? h("span", { class: "said" }, ` · ${run.said}`) : null,
  );
}

/** The schedule picker: when it runs, in the words a person picks. */
function picker(choice: Choice, onChange: (c: Choice) => void): HTMLElement {
  const at = "at" in choice ? choice.at : "09:00";
  const kind = h(
    "select",
    {
      name: "kind",
      "aria-label": "When it runs",
      onchange: (e: Event) => {
        const k = (e.target as HTMLSelectElement).value;
        if (k === "off" || k === "hourly") onChange({ kind: k });
        else if (k === "weekly") onChange({ kind: "weekly", day: "fri", at });
        else if (k === "daily" || k === "weekdays" || k === "weekends") onChange({ kind: k, at });
      },
    },
    ...[
      ["off", "Only when I start it"],
      ["daily", "Every day"],
      ["weekdays", "Every weekday"],
      ["weekends", "Every weekend day"],
      ["weekly", "Every week on"],
      ["hourly", "Every hour"],
      ...(choice.kind === "other" ? [["other", choice.text]] : []),
    ].map(([value, text]) => h("option", { value, selected: choice.kind === value }, text)),
  );
  const day =
    choice.kind === "weekly"
      ? h(
          "select",
          { name: "day", "aria-label": "Day", onchange: (e: Event) => onChange({ ...choice, day: (e.target as HTMLSelectElement).value as typeof choice.day }) },
          ...DAY_CHOICES.map((d) => h("option", { value: d.day, selected: d.day === choice.day }, d.name)),
        )
      : null;
  const time =
    "at" in choice
      ? h("input", {
          type: "time",
          name: "at",
          "aria-label": "Time",
          value: choice.at,
          // Kept as it's typed, never redrawn: a redraw mid-way would take
          // the field, and what's half typed, away from the person.
          oninput: (e: Event) => {
            if ("at" in choice) choice.at = (e.target as HTMLInputElement).value;
          },
        })
      : null;
  return h("span", { class: "picker" }, kind, day, time ? h("span", { class: "muted" }, "at") : null, time);
}

function repeatCard(rep: Repeat): HTMLElement {
  const now = new Date();
  const keeping = state.keeping?.task === rep.task ? state.keeping : null;
  const busy = state.busy !== null;
  const steps = h(
    "details",
    { class: "steps" },
    h("summary", {}, `The steps, as code this computer wrote (${rep.steps.length})`),
    h("ol", {}, ...rep.steps.map((s) => h("li", {}, s))),
    h("pre", { class: "code small" }, rep.script),
  );
  const actions = keeping
    ? h(
        "div",
        { class: "keep-form" },
        h("label", { class: "field" }, h("span", {}, "Name"), h("input", { type: "text", name: "name", value: keeping.name, maxlength: 60, oninput: (e: Event) => (keeping.name = (e.target as HTMLInputElement).value) })),
        h("label", { class: "field" }, h("span", {}, "Runs"), picker(keeping.choice, (c) => ((keeping.choice = c), render()))),
        h(
          "div",
          { class: "buttons" },
          button("Cancel", { disabled: busy, onclick: () => ((state.keeping = null), render()) }),
          button(
            "Keep it",
            {
              "data-test": "keep",
              disabled: busy,
              onclick: () => {
                const problem = keeping.name.trim() ? choiceProblem(keeping.choice) : "Name it.";
                if (problem) return refuse(problem);
                const schedule = scheduleOf(keeping.choice);
                state.keeping = null;
                void act(`keep:${rep.task}`, "routines.keep", { task: rep.task, name: keeping.name.trim(), ...(schedule ? { schedule } : {}) });
              },
            },
            true,
          ),
        ),
      )
    : h(
        "div",
        { class: "buttons" },
        button("Not now", { "data-test": "dismiss", disabled: busy, onclick: () => act(`dismiss:${rep.task}`, "routines.dismiss", { task: rep.task }) }),
        button("Keep as a routine", { "data-test": "start-keep", disabled: busy, onclick: () => ((state.keeping = { task: rep.task, name: nameFor(rep.request), choice: { kind: "off" } }), render()) }, true),
      );
  return h(
    "article",
    { class: "card repeat", "data-repeat": rep.task },
    h("div", { class: "request" }, `“${rep.request}”`),
    h("div", { class: "meta" }, `Done the same way ${rep.times} times, last ${past(rep.last_at, now)}. This computer can do it again with no model.`),
    steps,
    actions,
  );
}

function routineCard(r: Routine): HTMLElement {
  const now = new Date();
  const busy = state.busy !== null;
  return h(
    "article",
    { class: "card", "data-routine": r.routine },
    h(
      "div",
      { class: "head" },
      h("a", { class: "name", href: `#/${encodeURIComponent(r.routine)}` }, r.name),
      r.paused ? h("span", { class: "chip paused" }, "Paused") : null,
    ),
    h("div", { class: "request" }, r.request),
    h("div", { class: "when", "data-test": "when" }, whenLine(r, now)),
    lastRun(r),
    h(
      "div",
      { class: "buttons" },
      r.schedule
        ? button(r.paused ? "Resume" : "Pause", {
            "data-test": "pause",
            disabled: busy,
            onclick: () => act(`pause:${r.routine}`, "routines.pause", { routine: r.routine, paused: !r.paused }),
          })
        : null,
      button("Run now", { "data-test": "run", disabled: busy || r.running !== null, onclick: () => runNow(r) }, true),
    ),
  );
}

function list(): HTMLElement[] {
  const sections: HTMLElement[] = [];
  if (state.repeats.length) {
    sections.push(
      h(
        "section",
        { class: "group", "data-test": "repeats" },
        h("h2", {}, "Done the same way more than once"),
        h("p", { class: "hint" }, "Keep one as a routine and this computer does it for you, the same way, with no model. You'll see the code first."),
        ...state.repeats.map(repeatCard),
      ),
    );
  }
  sections.push(
    h(
      "section",
      { class: "group", "data-test": "routines" },
      h("h2", {}, "Your routines"),
      ...(state.routines.length
        ? state.routines.map(routineCard)
        : [
            h(
              "div",
              { class: "empty", "data-test": "empty" },
              h("div", { class: "empty-title" }, "No routines yet"),
              h("p", {}, "Ask your agent to do something regularly, such as “every Friday at 5, archive the newsletters”, or do something the same way twice, and it shows up here."),
            ),
          ]),
    ),
  );
  return sections;
}

function runRow(run: Run): HTMLElement {
  const how = outcome(run);
  return h(
    "tr",
    {},
    h("td", {}, past(run.ended_at, new Date())),
    h("td", {}, run.scheduled ? "On its schedule" : "You started it"),
    h("td", {}, h("span", { class: `dot ${how.tone}` }), how.text),
    h("td", { class: "said" }, run.said || (run.changes ? `${run.changes} changes` : "")),
  );
}

/** One saved version: what it was, and, if it isn't the one in use, its changes and going back to it. */
function versionRow(d: RoutineDetail, v: Version): HTMLElement {
  const busy = state.busy !== null;
  const open = state.comparing?.routine === d.routine && state.comparing.version === v.version;
  return h(
    "li",
    { class: v.in_use ? "version in-use" : "version", "data-version": String(v.version) },
    h(
      "div",
      { class: "head" },
      h("strong", {}, `Version ${v.version}`),
      v.in_use ? h("span", { class: "chip in-use" }, "In use") : null,
      h("span", { class: "muted" }, `${past(v.saved_at, new Date())} · runs ${v.runs}`),
    ),
    h("div", { class: "note" }, v.note),
    v.in_use
      ? null
      : h(
          "div",
          { class: "buttons" },
          button(open ? "Hide changes" : "Show changes", { "data-test": "compare", disabled: busy, onclick: () => compare(d, v.version) }),
          button(v.version < d.version ? "Go back to this version" : "Use this version", {
            "data-test": "use-version",
            disabled: busy,
            onclick: () => act(`use:${d.routine}`, "routines.use", { routine: d.routine, version: v.version }, () => (state.comparing = null)),
          }),
        ),
    open ? comparison(d) : null,
  );
}

/** The version being compared against the one in use: when it runs, and its code, line by line. */
function comparison(d: RoutineDetail): HTMLElement {
  const other = state.comparing?.detail;
  if (!other) return h("div", { class: "muted" }, "Loading…");
  const lines = diffLines(d.script, other.script);
  const changed = lines.some((l) => l.kind !== "same");
  const mark = { same: " ", added: "+", removed: "\u2212" } as const;
  return h(
    "div",
    { class: "comparison", "data-test": "comparison" },
    h(
      "p",
      { class: "hint" },
      `Against version ${d.version}, in use: `,
      other.runs === d.runs ? `the same schedule (runs ${d.runs})` : `runs ${other.runs}, not ${d.runs}`,
      changed ? "; its code differs where marked." : "; the same code.",
    ),
    changed
      ? h(
          "pre",
          { class: "code diff" },
          ...lines.map((l) => h("span", { class: `line ${l.kind}` }, h("span", { class: "n" }, mark[l.kind]), h("span", { class: "text" }, l.text), "\n")),
        )
      : null,
  );
}

function detail(d: RoutineDetail): HTMLElement[] {
  const now = new Date();
  const busy = state.busy !== null;
  const picking = state.picking?.for === d.routine ? state.picking : null;
  const when = picking
    ? h(
        "div",
        { class: "row-form" },
        picker(picking.choice, (c) => ((picking.choice = c), render())),
        button("Cancel", { disabled: busy, onclick: () => ((state.picking = null), render()) }),
        button(
          "Save",
          {
            "data-test": "save-schedule",
            disabled: busy,
            onclick: () => {
              const problem = choiceProblem(picking.choice);
              if (problem) return refuse(problem);
              const schedule = scheduleOf(picking.choice);
              state.picking = null;
              void act(`schedule:${d.routine}`, "routines.schedule", { routine: d.routine, schedule });
            },
          },
          true,
        ),
      )
    : h(
        "div",
        { class: "row-form" },
        h("span", { "data-test": "when" }, whenLine(d, now)),
        button("Change", { "data-test": "change-schedule", disabled: busy, onclick: () => ((state.picking = { for: d.routine, choice: choiceOf(d.schedule) }), render()) }),
        d.schedule ? button(d.paused ? "Resume" : "Pause", { "data-test": "pause", disabled: busy, onclick: () => act(`pause:${d.routine}`, "routines.pause", { routine: d.routine, paused: !d.paused }) }) : null,
      );
  const reach = [
    ...(d.read.length ? [h("div", {}, h("span", { class: "label" }, "Reads "), d.read.map((p) => folder(p, state.user)).join(", "))] : []),
    ...(d.write.length ? [h("div", {}, h("span", { class: "label" }, "Changes "), d.write.map((p) => folder(p, state.user)).join(", "))] : []),
    h("div", {}, h("span", { class: "label" }, "Calls "), d.calls.length ? d.calls.map(tool).join(", ") : "no tools"),
  ];
  const lines = d.script.replace(/\n+$/, "").split("\n");
  const code = h(
    "pre",
    { class: "code", "data-test": "code" },
    ...lines.map((line, i) => h("span", { class: "line" }, h("span", { class: "n" }, String(i + 1)), h("span", { class: "text" }, line), "\n")),
  );
  const remove =
    state.removing === d.routine
      ? h(
          "div",
          { class: "row-form danger" },
          h("span", {}, "Remove it? It won't run again; undo on the desktop puts it back."),
          button("Cancel", { disabled: busy, onclick: () => ((state.removing = null), render()) }),
          button("Remove", {
            class: "button danger",
            "data-test": "remove-yes",
            disabled: busy,
            onclick: () => act(`remove:${d.routine}`, "routines.delete", { routine: d.routine }, () => (location.hash = "")),
          }),
        )
      : button("Remove this routine", { class: "button quiet", "data-test": "remove", disabled: busy, onclick: () => ((state.removing = d.routine), render()) });
  return [
    h("section", { class: "group" }, h("div", { class: "request big" }, d.request), lastRun(d)),
    h("section", { class: "group" }, h("h2", {}, "When it runs"), when),
    h(
      "section",
      { class: "group" },
      h("h2", {}, "What it can reach"),
      h("div", { class: "reach" }, ...reach),
      h("p", { class: "hint" }, "Each run is a task like any other: the same rules, asks, record and undo."),
    ),
    h(
      "section",
      { class: "group" },
      h("h2", {}, "The code"),
      h("p", { class: "hint" }, `${d.note} · version ${d.version}, saved ${past(d.saved_at, now)}. It runs with no model; if a step fails, the agent does the request itself.`),
      code,
    ),
    h(
      "section",
      { class: "group" },
      h("h2", {}, "Runs"),
      d.runs_kept.length
        ? h("table", { class: "runs", "data-test": "runs" }, h("tbody", {}, ...d.runs_kept.map(runRow)))
        : h("p", { class: "muted" }, "It hasn't run yet."),
      d.next_run_at && !d.paused ? h("p", { class: "hint" }, `Next: ${upcoming(d.next_run_at, now)}.`) : null,
    ),
    h(
      "section",
      { class: "group", "data-test": "versions" },
      h("h2", {}, "Versions"),
      d.versions.length > 1
        ? h("p", { class: "hint" }, "Each was approved when it was saved. Going back to one is undoable from the desktop.")
        : h("p", { class: "hint" }, "This is its first version. Changing when it runs, or your agent changing it, saves the next."),
      h("ol", { class: "versions" }, ...d.versions.map((v) => versionRow(d, v))),
    ),
    h("section", { class: "group" }, remove),
  ];
}

function render(): void {
  const scrolled = root.querySelector(".page")?.scrollTop ?? 0;
  const d = state.open ? state.detail : null;
  const title = d
    ? h(
        "header",
        { class: "titlebar" },
        h("a", { class: "back", href: "#", "aria-label": "All routines" }, "‹ Routines"),
        h("h1", {}, d.name),
        h("span", { class: "spacer" }),
        button("Run now", { "data-test": "run", disabled: state.busy !== null || d.running !== null, onclick: () => runNow(d) }, true),
      )
    : h("header", { class: "titlebar" }, h("h1", {}, "Routines"), h("span", { class: "spacer" }), h("span", { class: "muted" }, "What runs for you, with no model"));
  show(
    root,
    title,
    h(
      "main",
      { class: "page" },
      state.notice && h("div", { class: `banner ${state.notice.tone === "ok" ? "done" : "refused"}`, role: "status", "data-test": "notice" }, state.notice.text),
      ...(!state.loaded ? [h("div", { class: "empty" }, "Loading…")] : d ? detail(d) : list()),
    ),
  );
  const page = root.querySelector(".page");
  if (page) page.scrollTop = scrolled;
}

window.addEventListener("hashchange", navigate);
// Back in front: what ran meanwhile.
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible" && state.busy === null) void load();
});
navigate();
