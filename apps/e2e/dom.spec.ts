// The SDK's redraw in place (`patch`, `redraw`, `lasting`), in a real
// browser: what stays the element it was, and what a person in the middle
// of something keeps (UI-5). The apps' own suites show it end to end; here
// are the rules, one at a time.

import { join } from "node:path";
import { expect, type Page, test } from "@playwright/test";
import { buildSync } from "esbuild";

const sdk = buildSync({
  entryPoints: [join(import.meta.dirname, "../sdk/src/dom.ts")],
  bundle: true,
  write: false,
  format: "iife",
  globalName: "dom",
}).outputFiles[0]!.text;

type Dom = typeof import("../sdk/src/dom.ts");
declare const dom: Dom;

async function blank(page: Page): Promise<void> {
  await page.setContent('<!doctype html><html><body><div id="root"></div></body></html>');
  await page.addScriptTag({ content: sdk });
}

test.beforeEach(async ({ page }) => await blank(page));

test("ui5 a notice that comes and goes leaves the form under it the elements they were", async ({ page }) => {
  const kept = await page.evaluate(() => {
    const { h, redraw } = dom;
    const root = document.getElementById("root")!;
    const draw = (notice: string | null) => () => [
      h("header", {}, "Settings"),
      h(
        "main",
        {},
        notice && h("div", { class: "banner" }, notice),
        h("section", {}, h("h2", {}, "Browser"), h("div", { "data-setting": "home" }, h("input", { class: "text", value: "https://old.example" }))),
        h("section", {}, h("h2", {}, "Devices"), h("div", { "data-setting": "usb" }, h("input", { type: "checkbox", checked: true }))),
      ),
    ];
    redraw(root, draw(null));
    const field = root.querySelector("input.text") as HTMLInputElement;
    const box = root.querySelector("input[type=checkbox]") as HTMLInputElement;
    const main = root.querySelector("main")!;
    field.focus();
    field.value = "https://new.exam";
    field.setSelectionRange(5, 9);
    const seen: boolean[] = [];
    for (const notice of ["Saved.", null, "Saved again.", "Saved a third time.", null]) {
      redraw(root, draw(notice));
      seen.push(
        root.querySelector("input.text") === field &&
          root.querySelector("input[type=checkbox]") === box &&
          root.querySelector("main") === main &&
          document.activeElement === field &&
          field.value === "https://new.exam" &&
          field.selectionStart === 5 &&
          field.selectionEnd === 9 &&
          (root.querySelector(".banner")?.textContent ?? null) === notice,
      );
    }
    return { seen, banners: root.querySelectorAll(".banner").length, sections: root.querySelectorAll("section").length };
  });
  expect(kept).toEqual({ seen: [true, true, true, true, true], banners: 0, sections: 2 });
});

test("ui5 a row that takes another's place is a new row: what was typed doesn't pass to it", async ({ page }) => {
  const result = await page.evaluate(() => {
    const { h, redraw } = dom;
    const root = document.getElementById("root")!;
    const saved: string[] = [];
    const row = (id: string, value: string) =>
      h("div", { "data-setting": id }, h("label", {}, id), h("input", { class: "text", value, onchange: () => saved.push(id) }));
    redraw(root, () => [row("proxy", ""), row("home", ""), row("search", "")]);
    const home = root.querySelector('[data-setting="home"] input') as HTMLInputElement;
    const search = root.querySelector('[data-setting="search"] input') as HTMLInputElement;
    home.focus();
    home.value = "typed for home";
    // The same number of rows, the middle one another setting.
    redraw(root, () => [row("proxy", ""), row("downloads", ""), row("search", "")]);
    const downloads = root.querySelector('[data-setting="downloads"] input') as HTMLInputElement;
    downloads.dispatchEvent(new Event("change"));
    return {
      homeGone: !home.isConnected,
      downloadsIsNew: downloads !== home,
      downloadsValue: downloads.value,
      searchKept: root.querySelector('[data-setting="search"] input') === search,
      order: Array.from(root.children).map((el) => el.getAttribute("data-setting")),
      saved,
    };
  });
  expect(result).toEqual({
    homeGone: true,
    downloadsIsNew: true,
    downloadsValue: "",
    searchKept: true,
    order: ["proxy", "downloads", "search"],
    saved: ["downloads"],
  });
});

test("ui5 rows that come, go and change places stay the rows they were", async ({ page }) => {
  const result = await page.evaluate(() => {
    const { h, patch } = dom;
    const root = document.getElementById("root")!;
    const draw = (ids: string[]) => ids.map((id) => h("li", { "data-key": id, class: id === "b" ? "unread" : "" }, `message ${id}`));
    patch(root, ...draw(["a", "b", "c", "d"]));
    const first = new Map(Array.from(root.children).map((el) => [el.getAttribute("data-key"), el]));
    const same = (ids: string[]) => {
      patch(root, ...draw(ids));
      const now = Array.from(root.children);
      return (
        now.map((el) => el.getAttribute("data-key")).join("") === ids.join("") &&
        now.every((el) => !first.has(el.getAttribute("data-key")) || first.get(el.getAttribute("data-key")) === el)
      );
    };
    return [
      same(["new", "a", "b", "c", "d"]), // mail arrives at the top
      same(["new", "a", "c", "d"]), // one is deleted
      same(["d", "new", "a", "c"]), // the last moves first
      same(["c", "a"]), // most go
      same(["a", "c"]), // the two left change places
      same([]),
    ];
  });
  expect(result).toEqual([true, true, true, true, true, true]);
});

test("ui5 an element kept does what the newest drawing says, once", async ({ page }) => {
  const calls = await page.evaluate(() => {
    const { h, patch } = dom;
    const root = document.getElementById("root")!;
    const calls: string[] = [];
    const draw = (says: string | null) => h("button", says === null ? {} : { onclick: () => calls.push(says) }, "Send");
    patch(root, draw("first"));
    const button = root.querySelector("button")!;
    button.click();
    patch(root, draw("second"));
    button.click();
    // Drawn with nothing to do, then with something again: one listener
    // still, not one per drawing that had a handler.
    patch(root, draw(null));
    button.click();
    patch(root, draw("third"));
    patch(root, draw(null));
    patch(root, draw("fourth"));
    button.click();
    return { calls, same: root.querySelector("button") === button };
  });
  expect(calls).toEqual({ calls: ["first", "second", "fourth"], same: true });
});

test("ui5 a checkbox shows what the person chose while the page acts on it, then what the page says", async ({ page }) => {
  await page.evaluate(() => {
    const { h, redraw } = dom;
    const root = document.getElementById("root")!;
    const state = { on: true, busy: false };
    const draw = () => [
      h("input", {
        type: "checkbox",
        checked: state.on,
        disabled: state.busy,
        onchange: () => {
          // What Settings does: busy while it asks, then the answer. Here
          // the answer is no (policy refused): the setting is as it was.
          state.busy = true;
          redraw(root, draw);
        },
      }),
    ];
    redraw(root, draw);
    Object.assign(window, {
      answer: (on: boolean) => {
        state.on = on;
        state.busy = false;
        redraw(root, draw);
      },
    });
  });
  const box = page.locator("input[type=checkbox]");
  await box.evaluate((el) => Object.assign(el, { mine: true }));
  await box.click();
  // Being carried out: it shows the choice, and can't be clicked again.
  expect(await box.evaluate((el: HTMLInputElement) => [el.checked, el.disabled])).toEqual([false, true]);
  // Refused: it's on again, as the setting is.
  await page.evaluate(() => (window as unknown as { answer: (on: boolean) => void }).answer(true));
  expect(await box.evaluate((el: HTMLInputElement & { mine?: boolean }) => [el.checked, el.disabled, el.mine])).toEqual([true, false, true]);
  // And carried out, it's off.
  await box.click();
  await page.evaluate(() => (window as unknown as { answer: (on: boolean) => void }).answer(false));
  expect(await box.evaluate((el: HTMLInputElement & { mine?: boolean }) => [el.checked, el.disabled, el.mine])).toEqual([false, false, true]);
});

test("ui5 what was typed stays until the page draws another value, or answers", async ({ page }) => {
  const result = await page.evaluate(() => {
    const { h, patch } = dom;
    const root = document.getElementById("root")!;
    const draw = (value: string, choice: string, busy = false) => [
      h("input", { class: "text", value, disabled: busy }),
      h("textarea", { disabled: busy }, value),
      h("select", { disabled: busy }, ...["one", "two", "three"].map((v) => h("option", { value: v, selected: v === choice }, v))),
      // Drawn with no value, as a password is: only ever what was typed.
      h("input", { type: "password", name: "key", disabled: busy }),
    ];
    patch(root, ...draw("drawn", "one"));
    const [input, area, select, key] = Array.from(root.children) as [HTMLInputElement, HTMLTextAreaElement, HTMLSelectElement, HTMLInputElement];
    const now = () => [input.value, area.value, select.value, key.value];
    const type = () => {
      for (const field of [input, area, key]) {
        field.focus();
        field.value = "typed";
      }
      select.focus();
      select.value = "two";
    };
    const seen: Record<string, string[]> = {};

    // The same drawing again (a timer, mail arriving): theirs stays, in
    // the field with the keyboard and in the ones they've left.
    type();
    patch(root, ...draw("drawn", "one"));
    seen.same = now();
    select.blur();
    patch(root, ...draw("drawn", "one"));
    seen.sameAndNowhere = now();

    // The page draws something new: that's what shows, even mid-word.
    type();
    patch(root, ...draw("saved", "three"));
    seen.another = now();

    // The page carries out what was typed (fields disabled meanwhile) and
    // it is refused: enabled again, they show what is, not what was asked.
    type();
    patch(root, ...draw("saved", "three", true));
    seen.busy = now();
    patch(root, ...draw("saved", "three"));
    seen.answered = now();
    return { seen, same: [input, area, select, key].every((el, i) => root.children[i] === el) };
  });
  expect(result).toEqual({
    seen: {
      same: ["typed", "typed", "two", "typed"],
      sameAndNowhere: ["typed", "typed", "two", "typed"],
      another: ["saved", "saved", "three", "typed"],
      busy: ["typed", "typed", "two", "typed"],
      answered: ["saved", "saved", "three", ""],
    },
    same: true,
  });
});

test("ui5 an element the page keeps is put back as it is, with the keyboard", async ({ page }) => {
  const result = await page.evaluate(() => {
    const { h, lasting, redraw } = dom;
    const root = document.getElementById("root")!;
    // A field with a life of its own (the address field's suggestions).
    const field = lasting(h("label", { class: "field" }, h("input", { name: "to" }), h("ul", {}, h("li", {}, "ana@example.com"))));
    const draw = (notice: boolean) => () => [notice && h("p", {}, "New mail"), h("form", {}, h("div", {}, "To"), field, h("textarea", {}, ""))];
    redraw(root, draw(false));
    const input = field.querySelector("input")!;
    input.focus();
    input.value = "an";
    input.setSelectionRange(1, 2);
    redraw(root, draw(true));
    redraw(root, draw(false));
    return {
      same: root.querySelector("label.field") === field && root.querySelectorAll("label.field").length === 1,
      suggestions: field.querySelectorAll("li").length,
      typing: document.activeElement === input,
      value: input.value,
      caret: [input.selectionStart, input.selectionEnd],
    };
  });
  expect(result).toEqual({ same: true, suggestions: 1, typing: true, value: "an", caret: [1, 2] });
});
