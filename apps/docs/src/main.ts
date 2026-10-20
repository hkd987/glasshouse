// Docs (DC-1, DC-3, DC-4, ADR 0035): the page is page.ts; this starts it
// with the app's own bridge.

import { Bridge } from "@glasshouse/sdk";

import { mount } from "./page.ts";

void mount(document.getElementById("app") as HTMLElement, new Bridge({ app: "docs" }));
