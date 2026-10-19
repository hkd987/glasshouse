// Builds each app into <app>/dist: its page, one script, one stylesheet and
// the offline service worker. esbuild only; no other build step (UI-7:
// small and fast). `node build.mjs --watch` rebuilds on change.

import { copyFile, mkdir, rm } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import * as esbuild from "esbuild";

const here = dirname(fileURLToPath(import.meta.url));
export const APPS = ["mail", "calendar"];
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

async function build() {
  const watch = process.argv.includes("--watch");
  for (const app of [...APPS, ...SITES]) {
    const dist = join(here, app, "dist");
    await rm(dist, { recursive: true, force: true });
    await mkdir(dist, { recursive: true });
    await copyFile(join(here, app, "index.html"), join(dist, "index.html"));
    if (watch) {
      const context = await esbuild.context(options(app));
      await context.watch();
    } else {
      await esbuild.build(options(app));
    }
  }
}

await build();
