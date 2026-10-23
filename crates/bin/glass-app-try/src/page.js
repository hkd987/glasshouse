// The page an app someone made is tried in (GA-1, ADR 0032): as much of a
// browser as the SDK and an app's own code use, and a bridge that answers
// with sample data and keeps every call. Nothing here reaches anything:
// there is no network, no file and no person's data, only what glassd put
// in TRY. Time is make-believe: timers run as the page asks, in order,
// for a few seconds of it.

const TRY = globalThis.__TRY;
const said = { calls: [], errors: [], consoleErrors: [], fetches: [] };
let now = 0;
let timers = [];
let nextTimer = 1;

function problem(where, error) {
  const text = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
  said.errors.push(where ? `${where}: ${text}` : text);
}
globalThis.__problem = problem;

globalThis.window = globalThis;
globalThis.self = globalThis;
globalThis.globalThis = globalThis;

globalThis.setTimeout = (fn, ms, ...args) => {
  const id = nextTimer++;
  timers.push({ id, at: now + Math.max(0, Number(ms) || 0), fn, args, every: 0 });
  return id;
};
globalThis.setInterval = (fn, ms, ...args) => {
  const id = nextTimer++;
  const every = Math.max(10, Number(ms) || 0);
  timers.push({ id, at: now + every, fn, args, every });
  return id;
};
globalThis.clearTimeout = globalThis.clearInterval = (id) => {
  timers = timers.filter((t) => t.id !== id);
};
globalThis.requestAnimationFrame = (fn) => setTimeout(() => fn(now), 16);
globalThis.cancelAnimationFrame = clearTimeout;
globalThis.requestIdleCallback = (fn) => setTimeout(() => fn({ didTimeout: false, timeRemaining: () => 10 }), 1);
globalThis.queueMicrotask = (fn) => {
  Promise.resolve().then(fn);
};

/** Runs the next timer due within `horizon` ms of make-believe time; false when none is. */
globalThis.__tick = (horizon) => {
  timers.sort((a, b) => a.at - b.at || a.id - b.id);
  const next = timers[0];
  if (!next || next.at > horizon) return false;
  now = next.at;
  if (next.every) next.at = now + next.every;
  else timers.shift();
  try {
    next.fn(...next.args);
  } catch (error) {
    problem("in a timer", error);
  }
  return true;
};
globalThis.__now = () => now;

globalThis.console = {
  log() {},
  info() {},
  debug() {},
  warn() {},
  error(...args) {
    said.consoleErrors.push(args.map(String).join(" ").slice(0, 300));
  },
};
globalThis.performance = { now: () => now, mark() {}, measure() {}, getEntriesByName: () => [] };

class Storage {
  #items = new Map();
  get length() {
    return this.#items.size;
  }
  key(i) {
    return [...this.#items.keys()][i] ?? null;
  }
  getItem(k) {
    return this.#items.has(String(k)) ? this.#items.get(String(k)) : null;
  }
  setItem(k, v) {
    this.#items.set(String(k), String(v));
  }
  removeItem(k) {
    this.#items.delete(String(k));
  }
  clear() {
    this.#items.clear();
  }
}
globalThis.localStorage = new Storage();
globalThis.sessionStorage = new Storage();

globalThis.navigator = {
  language: "en-US",
  languages: ["en-US"],
  userAgent: "Glasshouse",
  onLine: true,
  serviceWorker: { register: () => Promise.resolve({}), ready: Promise.resolve({}), addEventListener() {} },
  clipboard: { writeText: () => Promise.resolve(), readText: () => Promise.resolve("") },
};
globalThis.crypto = {
  getRandomValues(array) {
    for (let i = 0; i < array.length; i += 1) array[i] = Math.floor(Math.random() * 256);
    return array;
  },
  randomUUID: () => "00000000-0000-4000-8000-000000000000".replace(/0/g, () => Math.floor(Math.random() * 16).toString(16)),
};
// Intl, as en-US formats: the engine here is built without it, and apps
// format dates and numbers with it. Close enough to read, not exact.
const MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const DAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const two = (n) => String(n).padStart(2, "0");
class DateTimeFormat {
  constructor(_locale, options = {}) {
    const o = { ...options };
    if (o.dateStyle) Object.assign(o, { full: { weekday: "long", month: "long", day: "numeric", year: "numeric" }, long: { month: "long", day: "numeric", year: "numeric" }, medium: { month: "short", day: "numeric", year: "numeric" }, short: { month: "numeric", day: "numeric", year: "2-digit" } }[o.dateStyle] ?? {});
    if (o.timeStyle) Object.assign(o, { hour: "numeric", minute: "2-digit" }, o.timeStyle === "short" ? {} : { second: "2-digit" });
    if (!["weekday", "year", "month", "day", "hour", "minute", "second"].some((k) => o[k])) Object.assign(o, { year: "numeric", month: "numeric", day: "numeric" });
    this.o = o;
  }
  formatToParts(when) {
    const d = when === undefined ? new Date() : new Date(when);
    const o = this.o;
    const parts = [];
    const add = (type, value) => parts.push({ type, value: String(value) });
    const lit = (value) => parts.push({ type: "literal", value });
    if (o.weekday) add("weekday", o.weekday === "long" ? DAYS[d.getDay()] : DAYS[d.getDay()].slice(0, o.weekday === "narrow" ? 1 : 3));
    const dated = o.month || o.day || o.year;
    if (o.weekday && dated) lit(", ");
    if (o.month === "numeric" || o.month === "2-digit") {
      add("month", o.month === "2-digit" ? two(d.getMonth() + 1) : d.getMonth() + 1);
      if (o.day) (lit("/"), add("day", o.day === "2-digit" ? two(d.getDate()) : d.getDate()));
      if (o.year) (lit("/"), add("year", o.year === "2-digit" ? two(d.getFullYear() % 100) : d.getFullYear()));
    } else {
      if (o.month) add("month", o.month === "long" ? MONTHS[d.getMonth()] : MONTHS[d.getMonth()].slice(0, o.month === "narrow" ? 1 : 3));
      if (o.month && o.day) lit(" ");
      if (o.day) add("day", o.day === "2-digit" ? two(d.getDate()) : d.getDate());
      if (o.year) (o.month || o.day ? lit(", ") : null, add("year", o.year === "2-digit" ? two(d.getFullYear() % 100) : d.getFullYear()));
    }
    if (o.hour || o.minute) {
      if (parts.length) lit(", ");
      const h24 = o.hour12 === false || o.hourCycle === "h23";
      const h = d.getHours();
      if (o.hour) add("hour", h24 ? two(h) : ((h + 11) % 12) + 1);
      if (o.minute) (o.hour ? lit(":") : null, add("minute", two(d.getMinutes())));
      if (o.second) (lit(":"), add("second", two(d.getSeconds())));
      if (o.hour && !h24) (lit(" "), add("dayPeriod", h < 12 ? "AM" : "PM"));
    }
    return parts;
  }
  format(when) {
    return this.formatToParts(when).map((p) => p.value).join("");
  }
  formatRange(a, b) {
    return `${this.format(a)} – ${this.format(b)}`;
  }
  resolvedOptions() {
    return { locale: "en-US", timeZone: "UTC", ...this.o };
  }
}
const grouped = (n, digits) => {
  const [whole, part] = Math.abs(n).toFixed(digits).split(".");
  return `${n < 0 ? "-" : ""}${whole.replace(/\B(?=(\d{3})+(?!\d))/g, ",")}${part ? `.${part}` : ""}`;
};
globalThis.Intl = {
  DateTimeFormat,
  NumberFormat: class {
    constructor(_l, o = {}) {
      this.o = o;
    }
    format(n) {
      const digits = this.o.maximumFractionDigits ?? (this.o.style === "currency" ? 2 : Number.isInteger(n) ? 0 : 2);
      const text = grouped(Number(n), digits);
      if (this.o.style === "currency") return `${this.o.currency === "USD" || !this.o.currency ? "$" : `${this.o.currency} `}${text}`;
      if (this.o.style === "percent") return `${grouped(Number(n) * 100, 0)}%`;
      return text;
    }
  },
  Collator: class {
    compare(a, b) {
      return String(a).toLowerCase() < String(b).toLowerCase() ? -1 : String(a).toLowerCase() > String(b).toLowerCase() ? 1 : 0;
    }
  },
  PluralRules: class {
    select(n) {
      return Number(n) === 1 ? "one" : "other";
    }
  },
  RelativeTimeFormat: class {
    format(n, unit) {
      const u = String(unit).replace(/s$/, "");
      const k = Math.abs(n);
      return n < 0 ? `${k} ${u}${k === 1 ? "" : "s"} ago` : `in ${k} ${u}${k === 1 ? "" : "s"}`;
    }
  },
  ListFormat: class {
    format(items) {
      const list = [...items];
      return list.length < 3 ? list.join(" and ") : `${list.slice(0, -1).join(", ")}, and ${list[list.length - 1]}`;
    }
  },
};
Date.prototype.toLocaleDateString = function (l, o) {
  return new DateTimeFormat(l, o ?? { year: "numeric", month: "numeric", day: "numeric" }).format(this);
};
Date.prototype.toLocaleTimeString = function (l, o) {
  return new DateTimeFormat(l, o ?? { hour: "numeric", minute: "2-digit", second: "2-digit" }).format(this);
};
Date.prototype.toLocaleString = function (l, o) {
  return new DateTimeFormat(l, o ?? { year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "2-digit", second: "2-digit" }).format(this);
};
Number.prototype.toLocaleString = function (l, o) {
  return new Intl.NumberFormat(l, o).format(this);
};

globalThis.matchMedia = (media) => ({ matches: false, media, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} });
globalThis.getComputedStyle = () => ({ getPropertyValue: () => "" });
for (const name of ["MutationObserver", "ResizeObserver", "IntersectionObserver"]) {
  globalThis[name] = class {
    observe() {}
    unobserve() {}
    disconnect() {}
    takeRecords() {
      return [];
    }
  };
}
globalThis.BroadcastChannel = class {
  postMessage() {}
  close() {}
  addEventListener() {}
};
globalThis.alert = () => {};
globalThis.confirm = () => true;
globalThis.prompt = () => "";
globalThis.open = () => null;
globalThis.scrollTo = () => {};

class Event {
  constructor(type, init = {}) {
    this.type = type;
    this.bubbles = !!init.bubbles;
    this.detail = init.detail;
    this.key = init.key ?? "";
    this.defaultPrevented = false;
    this.target = null;
    this.currentTarget = null;
    this._stopped = false;
  }
  preventDefault() {
    this.defaultPrevented = true;
  }
  stopPropagation() {
    this._stopped = true;
  }
  stopImmediatePropagation() {
    this._stopped = true;
  }
}
globalThis.Event = Event;
globalThis.CustomEvent = class extends Event {};
globalThis.KeyboardEvent = class extends Event {};
globalThis.MouseEvent = class extends Event {};
globalThis.InputEvent = class extends Event {};
globalThis.SubmitEvent = class extends Event {};

class EventTarget {
  _listeners = new Map();
  addEventListener(type, fn) {
    if (typeof fn !== "function" && !(fn && typeof fn.handleEvent === "function")) return;
    if (!this._listeners.has(type)) this._listeners.set(type, []);
    this._listeners.get(type).push(fn);
  }
  removeEventListener(type, fn) {
    const list = this._listeners.get(type);
    if (list) this._listeners.set(type, list.filter((f) => f !== fn));
  }
  dispatchEvent(event) {
    if (!event.target) event.target = this;
    let at = this;
    while (at) {
      event.currentTarget = at;
      for (const fn of [...(at._listeners.get(event.type) ?? [])]) {
        try {
          const result = typeof fn === "function" ? fn.call(at, event) : fn.handleEvent(event);
          if (result && typeof result.then === "function") result.then(undefined, (error) => problem(`after ${event.type}`, error));
        } catch (error) {
          problem(`on ${event.type}`, error);
        }
      }
      const handler = at[`on${event.type}`];
      if (typeof handler === "function") {
        try {
          handler.call(at, event);
        } catch (error) {
          problem(`on ${event.type}`, error);
        }
      }
      if (!event.bubbles || event._stopped) break;
      at = at.parentNode ?? (at === document ? globalThis : null);
    }
    return !event.defaultPrevented;
  }
}
globalThis.EventTarget = EventTarget;
Object.setPrototypeOf(globalThis, EventTarget.prototype);
globalThis._listeners = new Map();

class Node extends EventTarget {
  constructor(type) {
    super();
    this.nodeType = type;
    this.childNodes = [];
    this.parentNode = null;
  }
  get children() {
    return this.childNodes.filter((n) => n.nodeType === 1);
  }
  get firstChild() {
    return this.childNodes[0] ?? null;
  }
  get lastChild() {
    return this.childNodes[this.childNodes.length - 1] ?? null;
  }
  get firstElementChild() {
    return this.children[0] ?? null;
  }
  get lastElementChild() {
    const c = this.children;
    return c[c.length - 1] ?? null;
  }
  get parentElement() {
    return this.parentNode && this.parentNode.nodeType === 1 ? this.parentNode : null;
  }
  get nextSibling() {
    const s = this.parentNode?.childNodes;
    return s ? (s[s.indexOf(this) + 1] ?? null) : null;
  }
  get previousSibling() {
    const s = this.parentNode?.childNodes;
    return s ? (s[s.indexOf(this) - 1] ?? null) : null;
  }
  get ownerDocument() {
    return document;
  }
  get isConnected() {
    let at = this;
    while (at.parentNode) at = at.parentNode;
    return at === document;
  }
  hasChildNodes() {
    return this.childNodes.length > 0;
  }
  appendChild(child) {
    return this.insertBefore(child, null);
  }
  insertBefore(child, ref) {
    if (child == null || !(child instanceof Node)) throw new TypeError("insertBefore: not a node");
    if (child.nodeType === 11) {
      for (const c of [...child.childNodes]) this.insertBefore(c, ref);
      return child;
    }
    if (child.parentNode) child.parentNode.removeChild(child);
    const at = ref ? this.childNodes.indexOf(ref) : -1;
    if (at < 0) this.childNodes.push(child);
    else this.childNodes.splice(at, 0, child);
    child.parentNode = this;
    return child;
  }
  removeChild(child) {
    const at = this.childNodes.indexOf(child);
    if (at >= 0) {
      this.childNodes.splice(at, 1);
      child.parentNode = null;
    }
    return child;
  }
  replaceChild(next, old) {
    this.insertBefore(next, old);
    return this.removeChild(old);
  }
  replaceChildren(...nodes) {
    for (const c of [...this.childNodes]) this.removeChild(c);
    this.append(...nodes);
  }
  append(...nodes) {
    for (const n of nodes) this.appendChild(n instanceof Node ? n : new Text(String(n)));
  }
  prepend(...nodes) {
    const first = this.firstChild;
    for (const n of nodes) this.insertBefore(n instanceof Node ? n : new Text(String(n)), first);
  }
  before(...nodes) {
    for (const n of nodes) this.parentNode?.insertBefore(n instanceof Node ? n : new Text(String(n)), this);
  }
  after(...nodes) {
    const next = this.nextSibling;
    for (const n of nodes) this.parentNode?.insertBefore(n instanceof Node ? n : new Text(String(n)), next);
  }
  replaceWith(...nodes) {
    this.before(...nodes);
    this.remove();
  }
  remove() {
    this.parentNode?.removeChild(this);
  }
  contains(node) {
    for (let at = node; at; at = at.parentNode) if (at === this) return true;
    return false;
  }
  get textContent() {
    return this.childNodes.map((c) => c.textContent).join("");
  }
  set textContent(text) {
    this.replaceChildren();
    if (text !== null && text !== undefined && String(text) !== "") this.appendChild(new Text(String(text)));
  }
  cloneNode(deep) {
    const copy = this.nodeType === 3 ? new Text(this.data) : this.nodeType === 1 ? document.createElement(this.localName) : new DocumentFragment();
    if (this.nodeType === 1) {
      for (const [k, v] of this._attributes) copy.setAttribute(k, v);
      copy.value = this.value;
    }
    if (deep) for (const c of this.childNodes) copy.appendChild(c.cloneNode(true));
    return copy;
  }
}
globalThis.Node = Node;

class Text extends Node {
  constructor(data = "") {
    super(3);
    this.data = String(data);
  }
  get nodeValue() {
    return this.data;
  }
  get textContent() {
    return this.data;
  }
  set textContent(text) {
    this.data = String(text);
  }
}
globalThis.Text = Text;

class Comment extends Node {
  constructor(data = "") {
    super(8);
    this.data = String(data);
  }
  get textContent() {
    return "";
  }
}

class DocumentFragment extends Node {
  constructor() {
    super(11);
  }
  querySelector(s) {
    return query(this, s, true)[0] ?? null;
  }
  querySelectorAll(s) {
    return query(this, s, false);
  }
}
globalThis.DocumentFragment = DocumentFragment;

const camel = (name) => name.replace(/-([a-z])/g, (_, c) => c.toUpperCase());

function classListOf(el) {
  const names = () => (el.getAttribute("class") ?? "").split(/\s+/).filter(Boolean);
  const set = (list) => el.setAttribute("class", list.join(" "));
  return {
    add: (...c) => set([...new Set([...names(), ...c])]),
    remove: (...c) => set(names().filter((n) => !c.includes(n))),
    contains: (c) => names().includes(c),
    toggle(c, force) {
      const on = force === undefined ? !names().includes(c) : !!force;
      if (on) this.add(c);
      else this.remove(c);
      return on;
    },
    replace(a, b) {
      set(names().map((n) => (n === a ? b : n)));
    },
    get length() {
      return names().length;
    },
    get value() {
      return names().join(" ");
    },
  };
}

class Element extends Node {
  constructor(tag) {
    super(1);
    this.localName = String(tag).toLowerCase();
    this.tagName = this.localName.toUpperCase();
    this.nodeName = this.tagName;
    this._attributes = new Map();
    this.style = new Proxy(
      { setProperty() {}, removeProperty() {}, getPropertyValue: () => "", cssText: "" },
      { set: (t, k, v) => ((t[k] = v), true) },
    );
    this.dataset = {};
    this.value = "";
    this.checked = false;
    this.selected = false;
    this.open = false;
    this.scrollTop = 0;
    this.scrollLeft = 0;
    this.scrollHeight = 0;
    this.clientHeight = 0;
    this.clientWidth = 0;
    this.offsetWidth = 0;
    this.offsetHeight = 0;
    this.tabIndex = 0;
    this.files = [];
    this.selectionStart = 0;
    this.selectionEnd = 0;
  }
  get classList() {
    return classListOf(this);
  }
  get className() {
    return this.getAttribute("class") ?? "";
  }
  set className(v) {
    this.setAttribute("class", String(v));
  }
  get id() {
    return this.getAttribute("id") ?? "";
  }
  set id(v) {
    this.setAttribute("id", String(v));
  }
  get type() {
    return this.getAttribute("type") ?? (this.localName === "button" ? "submit" : this.localName === "input" ? "text" : "");
  }
  set type(v) {
    this.setAttribute("type", String(v));
  }
  get disabled() {
    return this._attributes.has("disabled");
  }
  set disabled(v) {
    if (v) this.setAttribute("disabled", "");
    else this.removeAttribute("disabled");
  }
  get hidden() {
    return this._attributes.has("hidden");
  }
  set hidden(v) {
    if (v) this.setAttribute("hidden", "");
    else this.removeAttribute("hidden");
  }
  get title() {
    return this.getAttribute("title") ?? "";
  }
  set title(v) {
    this.setAttribute("title", String(v));
  }
  get href() {
    return this.getAttribute("href") ?? "";
  }
  set href(v) {
    this.setAttribute("href", String(v));
  }
  get placeholder() {
    return this.getAttribute("placeholder") ?? "";
  }
  set placeholder(v) {
    this.setAttribute("placeholder", String(v));
  }
  get name() {
    return this.getAttribute("name") ?? "";
  }
  set name(v) {
    this.setAttribute("name", String(v));
  }
  get innerText() {
    return this.textContent;
  }
  set innerText(v) {
    this.textContent = v;
  }
  get form() {
    for (let at = this.parentNode; at; at = at.parentNode) if (at.localName === "form") return at;
    return null;
  }
  get elements() {
    return query(this, "input, select, textarea, button", false);
  }
  get options() {
    return query(this, "option", false);
  }
  get valueAsNumber() {
    return Number(this.value);
  }
  setAttribute(name, value) {
    name = String(name).toLowerCase();
    value = String(value);
    this._attributes.set(name, value);
    if (name.startsWith("data-")) this.dataset[camel(name.slice(5))] = value;
    if (name === "value") this.value = value;
    if (name === "checked") this.checked = true;
  }
  getAttribute(name) {
    name = String(name).toLowerCase();
    return this._attributes.has(name) ? this._attributes.get(name) : null;
  }
  hasAttribute(name) {
    return this._attributes.has(String(name).toLowerCase());
  }
  removeAttribute(name) {
    name = String(name).toLowerCase();
    this._attributes.delete(name);
    if (name.startsWith("data-")) delete this.dataset[camel(name.slice(5))];
  }
  toggleAttribute(name, force) {
    const on = force === undefined ? !this.hasAttribute(name) : !!force;
    if (on) this.setAttribute(name, "");
    else this.removeAttribute(name);
    return on;
  }
  getAttributeNames() {
    return [...this._attributes.keys()];
  }
  querySelector(s) {
    return query(this, s, true)[0] ?? null;
  }
  querySelectorAll(s) {
    return query(this, s, false);
  }
  getElementsByTagName(t) {
    return query(this, t, false);
  }
  getElementsByClassName(c) {
    return query(this, `.${c}`, false);
  }
  matches(s) {
    return String(s).split(",").some((one) => matches(this, one.trim()));
  }
  closest(s) {
    for (let at = this; at && at.nodeType === 1; at = at.parentNode) if (at.matches(s)) return at;
    return null;
  }
  focus() {
    document.activeElement = this;
  }
  blur() {
    if (document.activeElement === this) document.activeElement = document.body;
  }
  click() {
    if (this.disabled) return;
    this.dispatchEvent(new Event("click", { bubbles: true }));
    const form = this.form;
    if (this.localName === "button" && this.type === "submit" && form) form.requestSubmit();
    if (this.localName === "input" && (this.type === "checkbox" || this.type === "radio")) {
      this.checked = !this.checked;
      this.dispatchEvent(new Event("change", { bubbles: true }));
    }
  }
  requestSubmit() {
    this.dispatchEvent(new Event("submit", { bubbles: true }));
  }
  submit() {
    this.requestSubmit();
  }
  reset() {}
  select() {}
  setSelectionRange() {}
  showModal() {
    this.open = true;
  }
  show() {
    this.open = true;
  }
  close() {
    this.open = false;
  }
  scrollIntoView() {}
  scrollTo() {}
  scrollBy() {}
  getBoundingClientRect() {
    return { x: 0, y: 0, top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0 };
  }
  getClientRects() {
    return [];
  }
  animate() {
    return { finished: Promise.resolve(), cancel() {} };
  }
  attachShadow() {
    return this;
  }
  setPointerCapture() {}
  releasePointerCapture() {}
}
globalThis.Element = Element;
globalThis.HTMLElement = Element;
for (const name of ["HTMLInputElement", "HTMLButtonElement", "HTMLSelectElement", "HTMLTextAreaElement", "HTMLFormElement", "HTMLDivElement", "HTMLAnchorElement", "HTMLDialogElement", "HTMLVideoElement", "HTMLImageElement", "HTMLLIElement"]) {
  globalThis[name] = Element;
}

/** Elements under `root` matching `selector`: tags, #ids, .classes, [attributes] and descendants. */
function query(root, selector, first) {
  const found = [];
  const alternatives = String(selector).split(",").map((s) => s.trim()).filter(Boolean);
  const walk = (node) => {
    for (const child of node.childNodes) {
      if (child.nodeType !== 1) continue;
      if (alternatives.some((s) => matchesDeep(child, s, root))) {
        found.push(child);
        if (first) return true;
      }
      if (walk(child)) return true;
    }
    return false;
  };
  walk(root);
  return found;
}

function matchesDeep(el, selector, root) {
  const parts = selector.split(/\s*>\s*|\s+/).filter(Boolean);
  if (!matches(el, parts[parts.length - 1])) return false;
  let at = el.parentNode;
  for (let i = parts.length - 2; i >= 0; i -= 1) {
    while (at && at !== root && !(at.nodeType === 1 && matches(at, parts[i]))) at = at.parentNode;
    if (!at || at === root) return false;
    at = at.parentNode;
  }
  return true;
}

function matches(el, compound) {
  if (!compound || el.nodeType !== 1) return false;
  const tokens = compound.match(/(^[a-zA-Z][\w-]*|\*)|#[\w-]+|\.[\w-]+|\[[^\]]+\]|:[\w-]+(\([^)]*\))?/g) ?? [];
  if (tokens.join("") !== compound) return false;
  return tokens.every((t) => {
    if (t === "*") return true;
    if (t.startsWith("#")) return el.id === t.slice(1);
    if (t.startsWith(".")) return el.classList.contains(t.slice(1));
    if (t.startsWith("[")) {
      const m = /^\[\s*([\w-]+)\s*(?:([~^$*|]?=)\s*["']?([^"'\]]*)["']?)?\s*\]$/.exec(t);
      if (!m) return false;
      const v = el.getAttribute(m[1]);
      if (!m[2]) return v !== null;
      if (v === null) return false;
      if (m[2] === "=") return v === m[3];
      if (m[2] === "~=") return v.split(/\s+/).includes(m[3]);
      if (m[2] === "^=") return v.startsWith(m[3]);
      if (m[2] === "$=") return v.endsWith(m[3]);
      if (m[2] === "*=") return v.includes(m[3]);
      return v === m[3] || v.startsWith(`${m[3]}-`);
    }
    if (t.startsWith(":")) {
      if (t === ":checked") return !!el.checked;
      if (t === ":disabled") return el.disabled;
      if (t === ":first-child") return el.parentNode?.children[0] === el;
      return true;
    }
    return el.localName === t.toLowerCase();
  });
}

class Document extends Node {
  constructor() {
    super(9);
    this.documentElement = new Element("html");
    this.documentElement.parentNode = this;
    this.childNodes.push(this.documentElement);
    this.head = new Element("head");
    this.body = new Element("body");
    this.documentElement.append(this.head, this.body);
    this.activeElement = this.body;
    this.visibilityState = "visible";
    this.hidden = false;
    this.title = "";
    this.cookie = "";
    this.readyState = "complete";
  }
  createElement(tag) {
    return new Element(tag);
  }
  createElementNS(_ns, tag) {
    return new Element(tag);
  }
  createTextNode(text) {
    return new Text(text);
  }
  createComment(text) {
    return new Comment(text);
  }
  createDocumentFragment() {
    return new DocumentFragment();
  }
  getElementById(id) {
    return query(this, `#${id}`, true)[0] ?? null;
  }
  querySelector(s) {
    return query(this, s, true)[0] ?? null;
  }
  querySelectorAll(s) {
    return query(this, s, false);
  }
  getElementsByTagName(t) {
    return query(this, t, false);
  }
  getElementsByClassName(c) {
    return query(this, `.${c}`, false);
  }
  hasFocus() {
    return true;
  }
}
const document = new Document();
globalThis.document = document;
const app = document.createElement("div");
app.id = "app";
app.className = "app";
document.body.appendChild(app);

const origin = `http://${TRY.app}.localhost`;
let hash = "";
globalThis.location = {
  get href() {
    return `${origin}/${hash}`;
  },
  set href(v) {
    const at = String(v).indexOf("#");
    if (at >= 0) this.hash = String(v).slice(at);
  },
  origin,
  protocol: "http:",
  host: `${TRY.app}.localhost`,
  hostname: `${TRY.app}.localhost`,
  port: "",
  pathname: "/",
  search: "",
  get hash() {
    return hash;
  },
  set hash(v) {
    const next = String(v) === "" || String(v) === "#" ? "" : String(v).startsWith("#") ? String(v) : `#${v}`;
    if (next === hash) return;
    hash = next;
    setTimeout(() => globalThis.dispatchEvent(new Event("hashchange")), 0);
  },
  assign(v) {
    this.href = v;
  },
  replace(v) {
    this.href = v;
  },
  reload() {},
};
globalThis.history = {
  length: 1,
  state: null,
  pushState(_s, _t, url) {
    if (url !== undefined) location.href = url;
  },
  replaceState() {},
  back() {},
  forward() {},
  go() {},
};

/** The bridge, as glassd would answer this app: the actions it declares, with sample data. */
const declared = TRY.actions;
function answer(endpoint, body) {
  if (endpoint === "session") {
    return {
      app: TRY.app,
      name: TRY.name,
      version: "1",
      user: TRY.user,
      permissions: TRY.permissions,
      actions: Object.entries(declared).map(([name, a]) => ({ name, title: a.title, tier: a.tier, side_effect: a.side_effect })),
    };
  }
  if (endpoint === "call") {
    const action = String(body?.action ?? "");
    said.calls.push({ action, input: body?.input ?? null, at: now });
    const known = declared[action];
    if (!known) return { ok: false, text: `${action} isn't one of this app's actions`, data: null };
    return { ok: true, text: `(sample) ${known.title}`, data: JSON.parse(JSON.stringify(known.sample ?? null)) };
  }
  if (endpoint === "status") {
    const result = { live: true, waiting: null, cursor: 1, changed: [], actions: answer("session").actions };
    if (body?.wait_secs) return new Promise((resolve) => setTimeout(() => resolve(result), body.wait_secs * 1000));
    return result;
  }
  if (endpoint === "live") {
    if (body?.op === "pull" && body?.wait_secs) return new Promise(() => {});
    return { cursor: 0, updates: [] };
  }
  return null;
}

globalThis.fetch = (url, init = {}) => {
  const path = String(url).replace(origin, "");
  said.fetches.push(path);
  const m = /^\/_bridge\/([a-z]+)$/.exec(path);
  if (!m) return Promise.reject(new TypeError(`fetch ${path}: an app reaches only its own bridge`));
  let body = null;
  try {
    body = init.body ? JSON.parse(init.body) : null;
  } catch {
    body = null;
  }
  return Promise.resolve(answer(m[1], body)).then((value) => ({
    ok: value !== null,
    status: value === null ? 404 : 200,
    headers: { get: () => "application/json" },
    json: () => Promise.resolve(value),
    text: () => Promise.resolve(JSON.stringify(value)),
  }));
};
globalThis.Response = class {};
globalThis.Request = class {};
globalThis.Headers = class {
  get() {
    return null;
  }
};
globalThis.WebSocket = class {
  constructor() {
    throw new TypeError("WebSocket: an app reaches only its own bridge");
  }
};
globalThis.XMLHttpRequest = class {
  open() {
    throw new TypeError("XMLHttpRequest: an app reaches only its own bridge");
  }
};

/** What a button says: its words, a space between its parts. */
function label(el) {
  const words = [];
  const walk = (node) => {
    if (node.nodeType === 3) words.push(node.data);
    else if (node.nodeType === 1 && !node.hidden) for (const c of node.childNodes) walk(c);
    if (node.nodeType === 1) words.push(" ");
  };
  for (const c of el.childNodes) walk(c);
  return words.join("").replace(/\s+/g, " ").trim() || el.getAttribute("aria-label") || el.title || "button";
}

/** What the page shows, as text: blocks on their own lines, buttons in brackets. */
const BLOCKS = new Set(["div", "p", "li", "ul", "ol", "h1", "h2", "h3", "h4", "h5", "h6", "header", "footer", "main", "section", "article", "nav", "aside", "form", "table", "tr", "dl", "dt", "dd", "details", "summary", "dialog", "label", "fieldset", "pre", "blockquote", "br", "hr"]);
function seen(node, out) {
  if (node.nodeType === 3) {
    out.push(node.data.replace(/\s+/g, " "));
    return;
  }
  if (node.nodeType !== 1 || node.hidden || node.localName === "template" || node.localName === "style" || node.localName === "script") return;
  if (node.localName === "dialog" && !node.open) return;
  const block = BLOCKS.has(node.localName);
  if (block) out.push("\n");
  if (node.localName === "button") {
    out.push(`[${label(node)}${node.disabled ? ", disabled" : ""}]`);
  } else if (node.localName === "input" || node.localName === "textarea") {
    const what = node.type === "checkbox" || node.type === "radio" ? `${node.type}${node.checked ? ", checked" : ""}` : node.value ? `"${node.value}"` : node.placeholder || node.getAttribute("aria-label") || node.name || node.type;
    out.push(`[${node.localName === "textarea" ? "text box" : "input"}: ${what}]`);
  } else if (node.localName === "select") {
    const chosen = node.options.find((o) => o.selected) ?? node.options[0];
    out.push(`[choice: ${chosen ? chosen.textContent.trim() : ""}]`);
  } else if (node.localName === "img") {
    out.push(`[image: ${node.getAttribute("alt") ?? ""}]`);
  } else {
    for (const c of node.childNodes) seen(c, out);
  }
  if (block) out.push("\n");
}
globalThis.__screen = () => {
  const out = [];
  seen(document.body, out);
  return out
    .join("")
    .split("\n")
    .map((l) => l.replace(/\s+/g, " ").trim())
    .filter(Boolean)
    .join("\n");
};

/** Fields a person types into: text of any kind, not passwords. */
const TYPED = new Set(["text", "search", "email", "url", "tel", "number", "date", "time", "datetime-local", "month"]);

/** What a person could use now, in one walk of the page: the buttons they
 * could press, the fields they could type into, and the labels that name
 * fields by id. Nothing hidden, nor anything in it. */
function controls() {
  const buttons = [];
  const fields = [];
  const labels = new Map();
  const walk = (node) => {
    for (const c of node.childNodes) {
      if (c.nodeType !== 1 || c.hidden || c.localName === "template" || (c.localName === "dialog" && !c.open)) continue;
      const tag = c.localName;
      if (tag === "button") {
        if (!c.disabled) buttons.push(c);
      } else if (tag === "input" || tag === "textarea") {
        if (!c.disabled && !c.readOnly && c.getAttribute("readonly") === null && (tag === "textarea" || TYPED.has(c.type))) fields.push(c);
      } else {
        if (tag === "label" && c.getAttribute("for")) labels.set(c.getAttribute("for"), c);
        walk(c);
      }
    }
  };
  if (document.body) walk(document.body);
  return { buttons, fields, labels };
}
/** What pressing `b` runs, as its code; null when that can't be read.
 * A list's buttons run the same code on different data: one kind. */
function runs(b) {
  const code = [...(b._listeners.get("click") ?? []), b.onclick]
    .filter((f) => typeof f === "function")
    .map((f) => Function.prototype.toString.call(f));
  return code.length && !code.some((c) => c.includes("[native code]")) ? code.join("\n") : null;
}
/** The buttons a person could press now, one of each kind: the same words,
 * or the same code behind them (a list's rows), make one kind. Each with
 * its kind, which stays the same as the page redraws. Fields only Enter
 * submits (a search box) are pressed as "Enter in …". */
function pressable() {
  const found = [];
  const labels = new Set();
  const kinds = new Set();
  const now = controls();
  for (const b of now.buttons) {
    const said = label(b);
    if (labels.has(said)) continue;
    const code = runs(b);
    if (code !== null && kinds.has(code)) continue;
    labels.add(said);
    if (code !== null) kinds.add(code);
    found.push({ label: said, kind: code ?? `words: ${said}` });
  }
  for (const field of entering(now)) {
    const said = `Enter in ${fieldName(field, now.labels)}`;
    if (labels.has(said)) continue;
    labels.add(said);
    found.push({ label: said, kind: `enter: ${said}` });
  }
  return found;
}

/** What a field is called on screen. */
function fieldName(f, labels) {
  let around = null;
  for (let at = f.parentNode; at && at.nodeType === 1 && !around; at = at.parentNode) if (at.localName === "label") around = at;
  const words = ((f.id && labels.get(f.id)) || around)?.textContent?.replace(/\s+/g, " ").trim();
  return f.getAttribute("aria-label") || words || f.placeholder || f.name || f.id || f.type;
}
/** Fields Enter is how to submit: in a form with no button, or listening for keys. */
function entering({ buttons, fields }) {
  return fields.filter((f) => {
    if (f.localName === "textarea") return false;
    const form = f.form;
    if (form) return !buttons.some((b) => b.form === form);
    return ["keydown", "keyup", "keypress"].some((k) => (f._listeners.get(k) ?? []).length || typeof f[`on${k}`] === "function");
  });
}
/** What a person might type there: by its kind, then by what it's for. */
function sample(f, labels) {
  const today = new Date().toISOString().slice(0, 10);
  const called = fieldName(f, labels);
  const name = `${called} ${f.name} ${f.id}`.toLowerCase();
  switch (f.type) {
    case "email":
      return "sam@example.com";
    case "url":
      return "https://example.com";
    case "tel":
      return "+1 555 0100";
    case "number":
      return f.getAttribute("min") || "3";
    case "date":
      return today;
    case "time":
      return "09:30";
    case "datetime-local":
      return `${today}T09:30`;
    case "month":
      return today.slice(0, 7);
  }
  if (f.localName === "textarea") return "Sample text.";
  if (/e-?mail|\bto\b|recipient|\bcc\b|attendee|invite/.test(name)) return "sam@example.com";
  if (/search|find|filter|query|look/.test(name)) return "Sample";
  return `Sample ${called.toLowerCase()}`.slice(0, 60);
}
/** Types into each empty field, as a person would before pressing: what went where. */
globalThis.__fill = () => {
  const typed = [];
  const { fields, labels } = controls();
  for (const f of fields) {
    if (f.value) continue;
    const text = sample(f, labels);
    f.focus();
    f.value = text;
    f.dispatchEvent(new Event("input", { bubbles: true }));
    f.dispatchEvent(new Event("change", { bubbles: true }));
    typed.push({ field: fieldName(f, labels), text });
  }
  return typed;
};
globalThis.__buttons = () => pressable();
/** Presses the button of `kind` that says `said`: two buttons may say the
 * same (a folder and a move both "Archive") and do different things. */
globalThis.__press = (said, kind) => {
  const now = controls();
  if (kind.startsWith("enter: ")) {
    const field = entering(now).find((f) => `Enter in ${fieldName(f, now.labels)}` === said);
    if (!field) return false;
    for (const type of ["keydown", "keypress", "keyup"]) field.dispatchEvent(new KeyboardEvent(type, { key: "Enter", bubbles: true }));
    if (field.form) field.form.requestSubmit();
    return true;
  }
  const b = now.buttons.find((b) => label(b) === said && (runs(b) ?? `words: ${said}`) === kind);
  if (b) b.click();
  return !!b;
};
globalThis.__said = () => said;
