// An address field that suggests people as they're typed (AD-1): the
// org's directory through `people.search`, then people from recent mail.
// It stays a plain text field: any address typed in full is kept as it is,
// and the field's value is what the form sends. Arrows move through the
// suggestions, Enter or Tab puts the highlighted one in, Escape closes them.
//
// The field keeps its own element and state; an app that redraws its page
// puts the same element back (`addressField` is called once per form).

import { h, lasting } from "./dom.ts";
import {
  addressesIn,
  currentToken,
  insertAddress,
  mergeSuggestions,
  narrowed,
  type Person,
  stepHighlight,
} from "./people.ts";

export interface AddressFieldOptions {
  /** The input's name, which the form reads. */
  name: string;
  label: string;
  value: string;
  /** Finds people in the org's directory; rejects or returns [] offline. */
  lookup: (query: string) => Promise<Person[]>;
  /** People from the person's own mail, offered when the directory doesn't have them. */
  recent?: () => readonly Person[];
  /** Every change to the value. */
  onchange: (value: string) => void;
}

/** How long typing pauses before the directory is asked. */
const PAUSE_MS = 120;

let fields = 0;

export function addressField(o: AddressFieldOptions): HTMLElement {
  const id = `addresses-${++fields}`;
  const cache = new Map<string, Person[]>();
  let shown: Person[] = [];
  let highlight = -1;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let asked = "";

  const list = h("ul", { class: "suggestions", role: "listbox", id: `${id}-list`, hidden: true });
  const status = h("span", { class: "visually-hidden", role: "status", "aria-live": "polite" });
  const input = h("input", {
    name: o.name,
    value: o.value,
    autocomplete: "off",
    role: "combobox",
    "aria-autocomplete": "list",
    "aria-expanded": "false",
    "aria-controls": `${id}-list`,
  });

  const close = () => {
    shown = [];
    highlight = -1;
    list.hidden = true;
    list.replaceChildren();
    input.setAttribute("aria-expanded", "false");
    input.removeAttribute("aria-activedescendant");
  };

  const draw = () => {
    if (!shown.length) return close();
    list.replaceChildren(
      ...shown.map((p, n) =>
        h(
          "li",
          {
            role: "option",
            id: `${id}-${n}`,
            class: n === highlight ? "suggestion on" : "suggestion",
            "aria-selected": n === highlight ? "true" : "false",
            "data-email": p.email,
            // Keeps the input focused, so typing can go on after a click.
            onmousedown: (e: Event) => {
              e.preventDefault();
              pick(n);
            },
          },
          h("span", { class: "who" }, p.name),
          h("span", { class: "address" }, p.email),
          p.title || p.department ? h("span", { class: "about" }, [p.title, p.department].filter(Boolean).join(" · ")) : null,
        ),
      ),
    );
    list.hidden = false;
    input.setAttribute("aria-expanded", "true");
    if (highlight >= 0) input.setAttribute("aria-activedescendant", `${id}-${highlight}`);
    else input.removeAttribute("aria-activedescendant");
    status.textContent = `${shown.length} suggestion${shown.length === 1 ? "" : "s"}`;
  };

  const suggest = (directory: readonly Person[], query: string) => {
    shown = mergeSuggestions(directory, o.recent?.() ?? [], addressesIn(input.value), query);
    highlight = shown.length ? 0 : -1;
    draw();
  };

  const typed = () => {
    o.onchange(input.value);
    const { token } = currentToken(input.value, input.selectionStart ?? input.value.length);
    clearTimeout(timer);
    if (!token || (token.includes("@") && /\.[a-z]{2,}$/i.test(token))) {
      // Nothing typed yet, or a whole address: nothing to suggest.
      return token ? suggest(cache.get(token.toLowerCase()) ?? [], token) : close();
    }
    const query = token.toLowerCase();
    const known = cache.get(query) ?? narrowed(cache, query);
    suggest(known ?? [], token);
    if (known || query.length < 2) return;
    asked = query;
    timer = setTimeout(() => {
      o.lookup(query).then(
        (people) => {
          cache.set(query, people);
          if (asked === query && document.activeElement === input) suggest(people, token);
        },
        // Offline or refused: what's typed stands, recent people still show.
        () => cache.set(query, []),
      );
    }, PAUSE_MS);
  };

  const pick = (n: number) => {
    const person = shown[n];
    if (!person) return;
    const caret = input.selectionStart ?? input.value.length;
    const { start, end } = currentToken(input.value, caret);
    const next = insertAddress(input.value, start, end, person.email);
    input.value = next.text;
    input.setSelectionRange(next.caret, next.caret);
    o.onchange(input.value);
    close();
    input.focus();
  };

  input.addEventListener("input", typed);
  input.addEventListener("keydown", (e: KeyboardEvent) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      if (!shown.length) return;
      e.preventDefault();
      highlight = stepHighlight(highlight, shown.length, e.key);
      draw();
    } else if ((e.key === "Enter" || e.key === "Tab") && shown.length && highlight >= 0) {
      e.preventDefault();
      pick(highlight);
    } else if (e.key === "Escape" && shown.length) {
      e.preventDefault();
      close();
    }
  });
  input.addEventListener("blur", () => close());

  // The page keeps this element and draws it again as itself: what it
  // suggests and highlights is in here, not in the page's state.
  return lasting(h("label", { class: "field address-field" }, h("span", {}, o.label), input, list, status));
}
