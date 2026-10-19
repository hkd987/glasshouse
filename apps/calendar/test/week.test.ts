// Calendar's logic (UI-5): weeks, wall-clock times and the week view's
// layout of overlapping events and holds.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  type CalendarEvent,
  addDays,
  formProblems,
  fromWall,
  isOwn,
  layout,
  startOfWeek,
  timeRange,
  wall,
  weekDays,
  weekTitle,
} from "../src/week.ts";

const event = (id: string, start: string, end: string, extra: Partial<CalendarEvent> = {}): CalendarEvent => ({
  id,
  title: id,
  start,
  end,
  kind: "event",
  location: "",
  notes: "",
  attendees: [],
  organizer: "demo",
  attribution: "",
  ...extra,
});

test("weeks start on Monday", () => {
  const monday = startOfWeek(new Date(2026, 9, 21, 15, 30));
  assert.equal(wall(monday), "2026-10-19T00:00");
  assert.equal(wall(startOfWeek(new Date(2026, 9, 25))), "2026-10-19T00:00", "Sunday is the week's end");
  assert.equal(weekTitle(monday), "Oct 19 – 25, 2026");
  assert.equal(weekTitle(addDays(monday, 7)), "Oct 26 – Nov 1, 2026");
  const days = weekDays(monday, new Date(2026, 9, 21, 9));
  assert.deepEqual(
    days.map((d) => d.weekday),
    ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
  );
  assert.equal(days.find((d) => d.today)?.date, 21);
});

test("wall-clock times round-trip", () => {
  assert.equal(wall(fromWall("2026-10-20T09:30")), "2026-10-20T09:30");
  assert.match(timeRange("2026-10-20T09:00", "2026-10-20T09:30"), /^9:00 – 9:30\sAM$/);
  assert.match(timeRange("2026-10-20T11:30", "2026-10-20T13:00"), /^11:30\sAM – 1:00\sPM$/);
});

test("overlapping events share the width; others get it all", () => {
  const monday = new Date(2026, 9, 19);
  const placed = layout(
    [
      event("standup", "2026-10-19T09:00", "2026-10-19T09:30"),
      event("hold", "2026-10-19T09:15", "2026-10-19T10:15", { kind: "hold" }),
      event("lunch", "2026-10-19T12:00", "2026-10-19T13:00"),
      event("late", "2026-10-21T19:30", "2026-10-21T21:00"),
      event("next week", "2026-10-27T09:00", "2026-10-27T10:00"),
    ],
    monday,
    7,
    20,
  );
  const by = (id: string) => placed.find((p) => p.event.id === id);
  assert.equal(by("standup")?.columns, 2);
  assert.equal(by("hold")?.column, 1);
  assert.equal(by("lunch")?.columns, 1);
  assert.equal(by("standup")?.top, 120, "two hours below 7 AM");
  assert.equal(by("late")?.day, 2);
  assert.equal(by("late")?.height, 30, "clipped at 8 PM");
  assert.equal(by("next week"), undefined);
});

test("own events, and what a new event lacks", () => {
  assert.equal(isOwn(event("a", "", ""), "demo"), true);
  assert.equal(isOwn(event("a", "", "", { organizer: "dana@northwind.example" }), "demo"), false);
  assert.deepEqual(formProblems({ title: "Review", day: "2026-10-20", start: "10:00", end: "11:00" }), []);
  assert.deepEqual(formProblems({ title: " ", day: "", start: "11:00", end: "10:00" }), [
    "Give the event a title.",
    "Pick a day.",
    "It has to end after it starts.",
  ]);
});
