// A computer's own desktop (ADR 0072): where a point on its picture, as
// the page shows it, lands on its display, and which button is which.

import assert from "node:assert/strict";
import { test } from "node:test";

import { buttonOf, toDisplay } from "../src/desktop.ts";

test("adr0072 a point on the picture lands on the display's pixel", () => {
  // A 1280x800 display shown at half its size, 100 px in from the page's corner.
  const rect = { left: 100, top: 50, width: 640, height: 400 };
  assert.deepEqual(toDisplay(100, 50, rect, 1280, 800), { x: 0, y: 0 });
  assert.deepEqual(toDisplay(420, 250, rect, 1280, 800), { x: 640, y: 400 });
  assert.deepEqual(toDisplay(739.9, 449.9, rect, 1280, 800), { x: 1279, y: 799 });
  // Off the picture, or before it has a size: nowhere.
  assert.equal(toDisplay(99, 50, rect, 1280, 800), null);
  assert.equal(toDisplay(740, 250, rect, 1280, 800), null);
  assert.equal(toDisplay(200, 100, rect, 0, 0), null);
  assert.equal(toDisplay(200, 100, { left: 0, top: 0, width: 0, height: 0 }, 1280, 800), null);
});

test("adr0072 the mouse's buttons are the display's", () => {
  assert.equal(buttonOf(0), "left");
  assert.equal(buttonOf(1), "middle");
  assert.equal(buttonOf(2), "right");
  assert.equal(buttonOf(3), null);
});
