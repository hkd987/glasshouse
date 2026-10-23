// The starting point for a new app (GA-1, ADR 0032). It reaches the system
// only through the actions its manifest declares, through the bridge:
// change the screen and the logic freely, and declare every action you
// call. apps.actions lists them, with what each takes and what its
// result's data is. Text goes into the page with h() and show(), which
// make text only.

import { Bridge, BridgeError, Offline, h, ready, show } from "@glasshouse/sdk";

interface Person {
  name: string;
  email: string;
}

const bridge = new Bridge({ app: "new-app" });
const root = document.getElementById("app") as HTMLElement;
const state = { query: "", people: [] as Person[], notice: "" };

async function search(query: string): Promise<void> {
  state.query = query;
  if (query.trim() === "") {
    state.people = [];
    return render();
  }
  try {
    const result = await bridge.call<{ people: Person[] }>("people.search", { query, limit: 10 });
    state.people = result.ok ? result.data.people : [];
    state.notice = result.ok ? "" : result.text;
  } catch (error) {
    state.notice = error instanceof Offline ? "The system isn't answering." : error instanceof BridgeError ? error.message : String(error);
  }
  render();
}

function render(): void {
  show(
    root,
    h(
      "main",
      { class: "page" },
      h("h1", {}, "New app"),
      h("input", { type: "search", placeholder: "Find someone", value: state.query, oninput: (e: Event) => search((e.target as HTMLInputElement).value) }),
      state.notice ? h("p", { class: "banner" }, state.notice) : null,
      h("ul", {}, ...state.people.map((p) => h("li", {}, h("strong", {}, p.name), " ", p.email))),
    ),
  );
}

render();
ready();
