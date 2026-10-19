// Checks each app's manifest.json the way glassd will (DV-3), so a bad one
// fails `make apps-lint` instead of leaving the app uninstalled.

import { readdirSync, readFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { manifestProblems } from "./sdk/src/manifest.ts";

const here = dirname(fileURLToPath(import.meta.url));
let failed = 0;
let apps = 0;
for (const dir of readdirSync(here, { withFileTypes: true })) {
  const path = join(here, dir.name, "manifest.json");
  if (!dir.isDirectory() || !existsSync(path)) continue;
  apps += 1;
  let problems: string[];
  try {
    const manifest = JSON.parse(readFileSync(path, "utf8")) as { id?: unknown };
    problems = manifestProblems(manifest);
    if (manifest.id !== dir.name) problems.push(`the folder must be named after the id (${String(manifest.id)})`);
  } catch (error) {
    problems = [`doesn't parse: ${String(error)}`];
  }
  for (const file of ["index.html", "src/main.ts", "src/app.css"]) {
    if (!existsSync(join(here, dir.name, file))) problems.push(`missing ${file}`);
  }
  for (const problem of problems) console.error(`  ${dir.name}/manifest.json: ${problem}`);
  failed += problems.length;
}
if (failed) {
  console.error(`manifests: ${failed} problem(s)`);
  process.exit(1);
}
console.log(`manifests: ${apps} apps, all check out`);
