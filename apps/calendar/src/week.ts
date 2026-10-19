// Calendar's logic, apart from the page: weeks, wall-clock times and where
// each event sits in the week view. Times are what the calendar store
// keeps: wall-clock minutes in the user's time zone, "YYYY-MM-DDTHH:MM".

/** One event as calendar.list and calendar.read return it. */
export interface CalendarEvent {
  id: string;
  title: string;
  start: string;
  end: string;
  kind: "event" | "hold";
  location: string;
  notes: string;
  attendees: string[];
  /** The user's login for their own events; the sender for invitations. */
  organizer: string;
  /** "by [agent] for [user]" for whoever last wrote it (AU-10). */
  attribution: string;
}

const pad = (n: number) => String(n).padStart(2, "0");

/** "YYYY-MM-DD" for a local date. */
export function dayKey(date: Date): string {
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

/** "YYYY-MM-DDTHH:MM" for a local time. */
export function wall(date: Date): string {
  return `${dayKey(date)}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** The local time a wall-clock string names. */
export function fromWall(text: string): Date {
  const [day = "", time = "00:00"] = text.split("T");
  const [y = 0, m = 1, d = 1] = day.split("-").map(Number);
  const [hh = 0, mm = 0] = time.split(":").map(Number);
  return new Date(y, m - 1, d, hh, mm);
}

export function addDays(date: Date, days: number): Date {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate() + days, date.getHours(), date.getMinutes());
}

/** Monday 00:00 of the week `date` is in. */
export function startOfWeek(date: Date): Date {
  const monday = (date.getDay() + 6) % 7;
  return new Date(date.getFullYear(), date.getMonth(), date.getDate() - monday);
}

const MONTH = new Intl.DateTimeFormat("en-US", { month: "short" });
const WEEKDAY = new Intl.DateTimeFormat("en-US", { weekday: "short" });
const TIME = new Intl.DateTimeFormat("en-US", { hour: "numeric", minute: "2-digit" });
const LONG_DAY = new Intl.DateTimeFormat("en-US", { weekday: "long", month: "long", day: "numeric" });

/** "Oct 19 – 25, 2026", or "Oct 26 – Nov 1, 2026" across months. */
export function weekTitle(start: Date): string {
  const end = addDays(start, 6);
  const from = `${MONTH.format(start)} ${start.getDate()}`;
  const to = start.getMonth() === end.getMonth() ? `${end.getDate()}` : `${MONTH.format(end)} ${end.getDate()}`;
  return `${from} – ${to}, ${end.getFullYear()}`;
}

export interface Day {
  key: string;
  weekday: string;
  date: number;
  today: boolean;
}

export function weekDays(start: Date, now: Date = new Date()): Day[] {
  return Array.from({ length: 7 }, (_, i) => {
    const day = addDays(start, i);
    return { key: dayKey(day), weekday: WEEKDAY.format(day), date: day.getDate(), today: dayKey(day) === dayKey(now) };
  });
}

/** "9:00 – 9:30 AM" */
export function timeRange(start: string, end: string): string {
  const a = TIME.format(fromWall(start));
  const b = TIME.format(fromWall(end));
  const [aTime, aHalf] = a.split(" ");
  const [, bHalf] = b.split(" ");
  return aHalf === bHalf ? `${aTime} – ${b}` : `${a} – ${b}`;
}

/** "Monday, October 19" */
export function longDay(text: string): string {
  return LONG_DAY.format(fromWall(text));
}

/** An event's block in one day's column of the week view. */
export interface Placed {
  event: CalendarEvent;
  day: number;
  /** Minutes from the top of the visible day, and how long. */
  top: number;
  height: number;
  /** Side by side with the events it overlaps. */
  column: number;
  columns: number;
}

/**
 * Where each event sits: per day, clipped to the hours shown, overlapping
 * events side by side.
 */
export function layout(
  events: readonly CalendarEvent[],
  weekStart: Date,
  firstHour: number,
  lastHour: number,
): Placed[] {
  const placed: Placed[] = [];
  for (let day = 0; day < 7; day++) {
    const open = addDays(weekStart, day);
    open.setHours(firstHour, 0, 0, 0);
    const close = addDays(weekStart, day);
    close.setHours(lastHour, 0, 0, 0);
    const today = events
      .map((event) => ({ event, start: fromWall(event.start), end: fromWall(event.end) }))
      .filter(({ start, end }) => start < close && end > open)
      .sort((a, b) => a.start.getTime() - b.start.getTime() || b.end.getTime() - a.end.getTime());
    // Clusters of events that overlap one another share the width.
    let cluster: { placed: Placed; end: number }[] = [];
    const flush = () => {
      const columns = Math.max(1, ...cluster.map((c) => c.placed.column + 1));
      for (const c of cluster) c.placed.columns = columns;
      cluster = [];
    };
    for (const { event, start, end } of today) {
      const from = Math.max(start.getTime(), open.getTime());
      const to = Math.min(end.getTime(), close.getTime());
      if (cluster.length && from >= Math.max(...cluster.map((c) => c.end))) flush();
      const taken = new Set(cluster.filter((c) => c.end > from).map((c) => c.placed.column));
      let column = 0;
      while (taken.has(column)) column++;
      const p: Placed = {
        event,
        day,
        top: (from - open.getTime()) / 60_000,
        height: Math.max(20, (to - from) / 60_000),
        column,
        columns: 1,
      };
      cluster.push({ placed: p, end: to });
      placed.push(p);
    }
    flush();
  }
  return placed;
}

/** Whether the user organizes the event (only those can be changed here). */
export function isOwn(event: CalendarEvent, user: string): boolean {
  return event.organizer === user;
}

/** What a new-event form lacks. */
export function formProblems(form: { title: string; day: string; start: string; end: string }): string[] {
  const found: string[] = [];
  if (!form.title.trim()) found.push("Give the event a title.");
  if (!/^\d{4}-\d{2}-\d{2}$/.test(form.day)) found.push("Pick a day.");
  if (!/^\d{2}:\d{2}$/.test(form.start) || !/^\d{2}:\d{2}$/.test(form.end)) found.push("Pick a start and an end.");
  else if (form.end <= form.start) found.push("It has to end after it starts.");
  return found;
}
