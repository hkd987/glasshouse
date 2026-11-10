// Calendar (UI-5, NV-3): the week, an event's detail, new events. Holds an
// agent made show who made them, "by [agent] for [user]" (AU-10). Creating
// an event on the user's own calendar runs at Notify: it happens, quietly,
// and stays undoable. Inviting people sends mail, so it waits for the
// person's approval on the desktop (Ask); the people come from the org's
// directory as they're typed (AD-1), and any address typed in full works.
//
// An event can be a Glasshouse meeting (CF-21): video with people in the
// org, up to 25. The meetings the person is in come from meet.list, each
// with Join, which opens the Meet app on it. They're fetched with
// bridge.call, never kept in the page's storage: their titles are sealed
// everywhere but on the computer.

import {
  addressField,
  Bridge,
  BridgeError,
  Offline,
  type Person,
  attributionChip,
  h,
  keepFilesOffline,
  parseAddresses,
  parseAttribution,
  ready,
  show,
  showAttribution,
} from "@glasshouse/sdk";
import {
  type CalendarEvent,
  type MeetingRow,
  addDays,
  dayKey,
  formProblems,
  isOwn,
  layout,
  longDay,
  meetUrl,
  shortRange,
  startOfWeek,
  timeRange,
  upcomingMeetings,
  wall,
  weekDays,
  weekTitle,
} from "./week.ts";

const FIRST_HOUR = 7;
const LAST_HOUR = 20;
const HOUR_PX = 56;

interface Draft {
  title: string;
  day: string;
  start: string;
  end: string;
  location: string;
  notes: string;
  /** Who to invite, as typed. */
  attendees: string;
  /** The People field, made once per draft so its suggestions survive redraws. */
  attendeesField: HTMLElement | null;
  /** A Glasshouse meeting (CF-21). */
  meeting: boolean;
  errors: string[];
}

const bridge = new Bridge({ app: "calendar" });
const state = {
  weekStart: startOfWeek(new Date()),
  events: [] as CalendarEvent[],
  /** The meetings the person is in (CF-21). */
  meetings: [] as MeetingRow[],
  selected: null as string | null,
  draft: null as Draft | null,
  offline: false,
  loadedAt: null as number | null,
  notice: null as string | null,
  user: "",
};

const root = document.getElementById("app") as HTMLElement;

async function loadWeek(): Promise<void> {
  try {
    const read = await bridge.read<{ events: CalendarEvent[] }>("calendar.list", {
      from: wall(state.weekStart),
      to: wall(addDays(state.weekStart, 7)),
    });
    state.offline = read.offline;
    state.loadedAt = read.loadedAt ?? null;
    if (read.result.ok) state.events = read.result.data.events;
    else state.notice = read.result.text;
  } catch (error) {
    state.events = [];
    state.offline = error instanceof Offline;
    state.notice = state.offline ? "This week wasn't loaded on this device before going offline." : String(error);
  }
  render();
}

/** The meetings the person is in. Not cached: a call, not a read. */
async function loadMeetings(): Promise<void> {
  try {
    const result = await bridge.call<{ meetings: MeetingRow[] }>("meet.list", {});
    state.meetings = result.ok ? result.data.meetings : [];
  } catch {
    state.meetings = [];
  }
  render();
}

function goTo(weekStart: Date): void {
  state.notice = null;
  state.weekStart = weekStart;
  state.selected = null;
  void loadWeek();
}

function newEvent(day = dayKey(new Date()), start = "10:00"): void {
  state.notice = null;
  const [hh = 10, mm = 0] = start.split(":").map(Number);
  const end = `${String(Math.min(hh + 1, 23)).padStart(2, "0")}:${String(mm).padStart(2, "0")}`;
  state.draft = { title: "", day, start, end, location: "", notes: "", attendees: "", attendeesField: null, meeting: false, errors: [] };
  state.selected = null;
  render();
  root.querySelector<HTMLInputElement>("input[name=title]")?.focus();
}

/** Colleagues in the org's directory whose name or address starts with `query` (AD-1). */
async function lookup(query: string): Promise<Person[]> {
  const found = await bridge.call<{ people: Person[] }>("people.search", { query, limit: 8 });
  // Someone with no address can't be mailed or invited.
  return found.ok ? found.data.people.filter((p) => p.email) : [];
}

async function create(draft: Draft): Promise<void> {
  const { addresses, bad } = parseAddresses(draft.attendees);
  draft.errors = formProblems(draft);
  if (bad.length) draft.errors.push(`Not an address: ${bad.join(", ")}`);
  if (draft.errors.length) return render();
  let event: CalendarEvent;
  try {
    const result = await bridge.call<{ event: CalendarEvent }>("calendar.create", {
      title: draft.title.trim(),
      start: `${draft.day}T${draft.start}`,
      end: `${draft.day}T${draft.end}`,
      location: draft.location.trim(),
      notes: draft.notes,
      attendees: addresses,
      meeting: draft.meeting,
    });
    if (!result.ok) {
      draft.errors = [result.text];
      return render();
    }
    event = result.data.event;
    state.draft = null;
    state.selected = event.id;
    state.notice = `${result.text}.`;
    await loadWeek();
  } catch (error) {
    draft.errors = [error instanceof Offline ? "Adding events needs the system; it isn't answering." : String(error)];
    return render();
  }
  if (addresses.length) await invite(event, addresses);
}

/** Invites people to an event: mail leaves the computer, so the desktop asks first. */
async function invite(event: CalendarEvent, to: string[]): Promise<void> {
  const people = to.length === 1 ? to[0] : `${to.length} people`;
  state.notice = `Inviting ${people} waits for your approval on the desktop.`;
  render();
  try {
    const result = await bridge.call("calendar.invite", { id: event.id, to });
    state.notice = result.ok ? `${result.text}.` : result.text;
    // An approved meeting invitation made the meeting: it has Join now.
    if (result.ok && event.meeting) await loadMeetings();
  } catch (error) {
    state.notice =
      error instanceof Offline
        ? "Not invited: the system isn't answering. Nobody was told."
        : error instanceof BridgeError
          ? error.message
          : String(error);
  }
  render();
}

// ---- rendering ------------------------------------------------------------

function render(): void {
  show(root, titlebar(), h("div", { class: "body" }, grid(), side()));
}

function titlebar(): HTMLElement {
  return h(
    "header",
    { class: "titlebar" },
    h("h1", {}, "Calendar"),
    h("span", { class: "week-title", "data-test": "week-title" }, weekTitle(state.weekStart)),
    h(
      "div",
      { class: "nav" },
      h("button", { class: "button icon", "aria-label": "Previous week", onclick: () => goTo(addDays(state.weekStart, -7)) }, "‹"),
      h("button", { class: "button", onclick: () => goTo(startOfWeek(new Date())) }, "Today"),
      h("button", { class: "button icon", "aria-label": "Next week", onclick: () => goTo(addDays(state.weekStart, 7)) }, "›"),
    ),
    h("span", { class: "spacer" }),
    state.user ? h("span", { class: "user" }, state.user) : null,
    h("button", { class: "button primary", disabled: state.offline, onclick: () => newEvent(), "data-test": "new-event" }, "New event"),
  );
}

function grid(): HTMLElement {
  const days = weekDays(state.weekStart);
  const hours = Array.from({ length: LAST_HOUR - FIRST_HOUR }, (_, i) => FIRST_HOUR + i);
  const placed = layout(state.events, state.weekStart, FIRST_HOUR, LAST_HOUR);
  const columns = days.map((day, index) =>
    h(
      "div",
      {
        class: `day${day.today ? " today" : ""}`,
        "data-day": day.key,
        ondblclick: (e: Event) => {
          const y = (e as MouseEvent).offsetY;
          const hour = Math.min(LAST_HOUR - 1, FIRST_HOUR + Math.floor(y / HOUR_PX));
          newEvent(day.key, `${String(hour).padStart(2, "0")}:00`);
        },
      },
      ...hours.map(() => h("div", { class: "hour-line" })),
      ...placed
        .filter((p) => p.day === index)
        .map((p) => {
          const attribution = parseAttribution(p.event.attribution);
          const byAgent = showAttribution(attribution, "calendar");
          const own = isOwn(p.event, state.user);
          const block = h(
            "button",
            {
              class: `event ${p.event.kind}${p.event.meeting ? " meeting" : ""}${own ? "" : " invited"}${byAgent ? " by-agent" : ""}${p.height < 30 ? " short" : ""}${state.selected === p.event.id ? " selected" : ""}`,
              title: `${p.event.title}, ${timeRange(p.event.start, p.event.end)}`,
              "data-id": p.event.id,
              onclick: () => {
                state.selected = p.event.id;
                state.draft = null;
                state.notice = null;
                render();
              },
            },
            h("span", { class: "event-title" }, p.event.title),
            p.height >= 40 ? h("span", { class: "event-time" }, shortRange(p.event.start, p.event.end)) : null,
            p.height >= 55 && (byAgent || p.event.kind === "hold")
              ? h(
                  "span",
                  { class: "event-by" },
                  [p.event.kind === "hold" ? "Hold" : "", byAgent && attribution ? `by ${attribution.by}` : ""]
                    .filter(Boolean)
                    .join(" · "),
                )
              : null,
          );
          const width = 100 / p.columns;
          block.style.top = `${(p.top / 60) * HOUR_PX}px`;
          block.style.height = `${(p.height / 60) * HOUR_PX - 3}px`;
          block.style.left = `calc(${p.column * width}% + 3px)`;
          block.style.width = `calc(${width}% - 6px)`;
          return block;
        }),
    ),
  );
  const now = new Date();
  const minutes = (now.getHours() - FIRST_HOUR) * 60 + now.getMinutes();
  const todayIndex = days.findIndex((d) => d.today);
  return h(
    "section",
    { class: "week", "aria-label": "Week" },
    state.offline ? offlineBanner() : null,
    h(
      "div",
      { class: "week-head" },
      h("div", { class: "gutter" }),
      ...days.map((d) =>
        h("div", { class: `day-head${d.today ? " today" : ""}` }, h("span", { class: "weekday" }, d.weekday), h("span", { class: "date" }, d.date)),
      ),
    ),
    h(
      "div",
      { class: "week-scroll" },
      h(
        "div",
        { class: "week-grid", style: `height:${(LAST_HOUR - FIRST_HOUR) * HOUR_PX}px` },
        h("div", { class: "gutter" }, ...hours.map((hour) => h("div", { class: "hour-label" }, hour === 12 ? "12 PM" : hour > 12 ? `${hour - 12} PM` : `${hour} AM`))),
        ...columns,
        todayIndex >= 0 && minutes > 0 && minutes < (LAST_HOUR - FIRST_HOUR) * 60
          ? nowLine(todayIndex, (minutes / 60) * HOUR_PX)
          : null,
      ),
    ),
  );
}

function nowLine(day: number, top: number): HTMLElement {
  const line = h("div", { class: "now" });
  line.style.top = `${top}px`;
  line.style.left = `calc(56px + (100% - 56px) / 7 * ${day})`;
  line.style.width = `calc((100% - 56px) / 7)`;
  return line;
}

function offlineBanner(): HTMLElement {
  const when = state.loadedAt ? new Date(state.loadedAt).toLocaleTimeString("en-US", { hour: "numeric", minute: "2-digit" }) : null;
  return h("div", { class: "banner offline", "data-test": "offline" }, `Offline · showing what was loaded${when ? ` at ${when}` : ""}. New events come back when the system does.`);
}

function side(): HTMLElement {
  // Shown until the person goes somewhere else (another week, an event, a
  // new one). It was cleared as it was drawn, so the next drawing took it
  // away, sometimes before it had been on screen at all.
  const notice = state.notice ? h("div", { class: "notice", "data-test": "notice" }, state.notice) : null;
  const event = state.events.find((e) => e.id === state.selected);
  return h(
    "aside",
    { class: "side" },
    notice,
    state.draft ? form(state.draft) : event ? detail(event) : upcoming(),
  );
}

function detail(event: CalendarEvent): HTMLElement {
  const attribution = parseAttribution(event.attribution);
  const own = isOwn(event, state.user);
  return h(
    "article",
    { class: "detail", "data-test": "detail" },
    h(
      "div",
      { class: `kind kind-${event.kind}` },
      event.kind === "hold" ? "Hold · tentative" : event.meeting ? "Glasshouse meeting" : own ? "Event" : "Invitation",
    ),
    h("h2", {}, event.title),
    h("div", { class: "when" }, longDay(event.start)),
    h("div", { class: "muted" }, timeRange(event.start, event.end)),
    showAttribution(attribution, "calendar") && attribution
      ? h("div", { class: "detail-attribution" }, attributionChip(attribution.text))
      : null,
    event.location ? row("Where", event.location) : null,
    row("Organizer", own ? `${event.organizer} (you)` : event.organizer),
    event.attendees.length ? row("People", event.attendees.join(", ")) : null,
    event.meeting ? meetingJoin(event.meeting.id) : null,
    event.notes ? h("div", { class: "notes" }, event.notes) : null,
  );
}

/** Join, once the meeting exists: its invitations went out. */
function meetingJoin(id: string): HTMLElement {
  return state.meetings.some((m) => m.id === id)
    ? h("div", { class: "detail-join" }, joinLink(id))
    : h("p", { class: "hint" }, "Join appears here once its invitations go out.");
}

/** Opens Meet, on its own origin, on the meeting (CF-21). */
function joinLink(id: string): HTMLElement {
  return h(
    "a",
    { class: "button primary join", href: meetUrl(location, id), target: "_blank", rel: "noopener", "data-test": "join" },
    "Join",
  );
}

function meetings(): HTMLElement | null {
  const shown = upcomingMeetings(state.meetings);
  if (!shown.length) return null;
  return h(
    "section",
    { class: "meetings", "aria-label": "Meetings", "data-test": "meetings" },
    h("h2", {}, "Meetings"),
    ...shown.map((m) =>
      h(
        "div",
        { class: "meeting-row", "data-test": "meeting", "data-id": m.id },
        h(
          "div",
          { class: "meeting-text" },
          h("strong", {}, m.title),
          h("span", { class: "muted" }, `${longDay(m.start)} · ${timeRange(m.start, m.end)}`),
          h("span", { class: "muted" }, m.organizer === state.user ? `You and ${m.people.length - 1} more` : `From ${m.organizer_name}`),
        ),
        joinLink(m.id),
      ),
    ),
  );
}

function row(label: string, value: string): HTMLElement {
  return h("div", { class: "detail-row" }, h("span", { class: "muted" }, label), h("span", {}, value));
}

function upcoming(): HTMLElement {
  const holds = state.events.filter((e) => e.kind === "hold");
  return h(
    "div",
    { class: "upcoming" },
    h("h2", {}, "This week"),
    h("p", { class: "muted" }, `${state.events.length} event${state.events.length === 1 ? "" : "s"}${holds.length ? `, ${holds.length} hold${holds.length === 1 ? "" : "s"}` : ""}. Select one to see it.`),
    meetings(),
    ...holds.map((hold) => {
      const attribution = parseAttribution(hold.attribution);
      return h(
        "button",
        { class: "hold-row", onclick: () => { state.selected = hold.id; state.notice = null; render(); } },
        h("strong", {}, hold.title),
        h("span", { class: "muted" }, `${longDay(hold.start)} · ${timeRange(hold.start, hold.end)}`),
        showAttribution(attribution, "calendar") && attribution ? attributionChip(attribution.text) : null,
      );
    }),
  );
}

/** What the form says about inviting, for an event or a meeting (CF-21). */
function inviteHint(meeting: boolean): string {
  return meeting
    ? "Video with people in your organization, up to 25, sealed end to end. Inviting them waits for your approval on the desktop."
    : "Inviting people waits for your approval on the desktop.";
}

function form(draft: Draft): HTMLElement {
  const hint = h("p", { class: "hint" }, inviteHint(draft.meeting));
  const input = (name: keyof Omit<Draft, "errors" | "attendees" | "attendeesField" | "meeting">, label: string, type = "text") =>
    h(
      "label",
      { class: "field" },
      h("span", {}, label),
      h("input", {
        name,
        type,
        value: draft[name],
        oninput: (e: Event) => {
          draft[name] = (e.target as HTMLInputElement).value;
        },
      }),
    );
  const notes = h("textarea", {
    name: "notes",
    rows: 4,
    oninput: (e: Event) => {
      draft.notes = (e.target as HTMLTextAreaElement).value;
    },
  });
  notes.value = draft.notes;
  return h(
    "form",
    {
      class: "event-form",
      "data-test": "event-form",
      onsubmit: (e: Event) => {
        e.preventDefault();
        void create(draft);
      },
    },
    h("h2", {}, "New event"),
    input("title", "Title"),
    input("day", "Day", "date"),
    h("div", { class: "times" }, input("start", "Starts", "time"), input("end", "Ends", "time")),
    input("location", "Where"),
    (draft.attendeesField ??= addressField({
      name: "attendees",
      label: "People",
      value: draft.attendees,
      lookup,
      onchange: (value) => {
        draft.attendees = value;
      },
    })),
    h(
      "label",
      { class: "toggle", "data-test": "meeting-toggle" },
      h("input", {
        type: "checkbox",
        name: "meeting",
        checked: draft.meeting,
        onchange: (e: Event) => {
          draft.meeting = (e.target as HTMLInputElement).checked;
          hint.textContent = inviteHint(draft.meeting);
        },
      }),
      h("span", {}, "Glasshouse meeting"),
    ),
    hint,
    h("label", { class: "field" }, h("span", {}, "Notes"), notes),
    ...draft.errors.map((e) => h("div", { class: "banner refused" }, e)),
    h(
      "div",
      { class: "actions" },
      h("button", { class: "button primary", type: "submit", "data-test": "add-event" }, "Add event"),
      h("button", { class: "button", type: "button", onclick: () => { state.draft = null; render(); } }, "Cancel"),
    ),
  );
}

// ---- start ----------------------------------------------------------------

async function start(): Promise<void> {
  keepFilesOffline();
  try {
    state.user = (await bridge.open()).user;
  } catch (error) {
    state.notice = error instanceof Offline ? "Calendar hasn't been loaded on this device yet." : String(error);
  }
  await Promise.all([loadWeek(), loadMeetings()]);
  // Start the day's hours in view.
  root.querySelector(".week-scroll")?.scrollTo({ top: 1 * HOUR_PX });
  ready();
}

void start();
