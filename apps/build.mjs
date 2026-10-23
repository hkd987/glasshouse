// Builds each app into <app>/dist: its page, one script, one stylesheet and
// the offline service worker. esbuild only; no other build step (UI-7:
// small and fast). `node build.mjs --watch` rebuilds on change.

import { copyFile, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import * as esbuild from "esbuild";

const here = dirname(fileURLToPath(import.meta.url));
export const APPS = ["mail", "calendar", "store", "settings", "routines", "chat", "meet", "artifacts", "docs"];
/** Apps' workers, built beside the page: Meet's frame transform (ADR 0030
 * §6) and its speech (ADR 0045 T2). */
const WORKERS = { meet: { worker: "src/worker.ts", speech: "src/speech.ts" } };
/** The speech model and runtime Meet ships, as tools/fetch-speech-model.sh
 * lists them. Without the list Meet ships without them, and doesn't
 * transcribe. */
const SPEECH = join(here, "../build/cache/models/speech.json");
const ORT_WASM = join(here, "node_modules/onnxruntime-web/dist/ort-wasm-simd-threaded.wasm");
/** Web apps our web servers serve rather than a device: the admin console
 * (ADR 0023). No offline service worker: they read from their server. */
export const SITES = ["console"];

/** @param {string} app */
function options(app) {
  const dir = join(here, app);
  const site = SITES.includes(app);
  return {
    entryPoints: [
      { in: join(dir, "src/main.ts"), out: "app" },
      { in: join(dir, "src/app.css"), out: "app" },
      ...(site ? [] : [{ in: join(here, "sdk/src/sw.ts"), out: "sw" }]),
      ...Object.entries(WORKERS[app] ?? {}).map(([out, file]) => ({ in: join(dir, file), out })),
    ],
    outdir: join(dir, "dist"),
    bundle: true,
    format: "esm",
    target: "chrome120",
    minify: true,
    sourcemap: false,
    legalComments: "none",
    logLevel: "warning",
  };
}

/**
 * Meet's speech files: the model, the runtime's WebAssembly, their
 * licenses, and the model's tokens alone (a tenth of its tokenizer).
 * Whether they're there.
 */
async function speech(dist) {
  let files;
  try {
    files = JSON.parse(await readFile(SPEECH, "utf8"));
  } catch {
    console.warn("meet: no speech model (tools/fetch-speech-model.sh): Meet won't transcribe");
    return false;
  }
  await mkdir(join(dist, "speech"), { recursive: true });
  await mkdir(join(dist, "ort"), { recursive: true });
  for (const [to, from] of Object.entries(files)) {
    if (to !== "speech/tokenizer.json") await copyFile(from, join(dist, to));
  }
  const vocab = JSON.parse(await readFile(files["speech/tokenizer.json"], "utf8")).model.vocab;
  const tokens = [];
  for (const [token, id] of Object.entries(vocab)) tokens[id] = token;
  await writeFile(join(dist, "speech/tokens.json"), JSON.stringify(tokens));
  await copyFile(ORT_WASM, join(dist, "ort/ort-wasm-simd-threaded.wasm"));
  return true;
}

async function build() {
  const watch = process.argv.includes("--watch");
  for (const app of [...APPS, ...SITES]) {
    const dist = join(here, app, "dist");
    await rm(dist, { recursive: true, force: true });
    await mkdir(dist, { recursive: true });
    await copyFile(join(here, app, "index.html"), join(dist, "index.html"));
    if (WORKERS[app]?.speech) await speech(dist);
    if (watch) {
      const context = await esbuild.context(options(app));
      await context.watch();
    } else {
      await esbuild.build(options(app));
    }
  }
}

await build();
