// Usage: node tools/perf/engine-launch.cjs
//
// How long the browser engine takes to show an app, natively on this
// machine (docs/dev/performance.md, "On the device"): started cold, then a
// new window in the running engine, each until Mail's page has loaded.
// The emulated VM runs the engine 40 to 60 times slower than this, so it
// can't show the hardware number. Needs `make apps-build` first.
const path = require("node:path");
const fs = require("node:fs");
const os = require("node:os");

const root = path.resolve(__dirname, "../..");
const { chromium } = require(path.join(root, "apps/node_modules/playwright"));
const page = "file://" + path.join(root, "apps/mail/dist/index.html");
const flags = ["--disable-gpu", "--no-first-run", "--disable-background-networking"];

(async () => {
  const data = fs.mkdtempSync(path.join(os.tmpdir(), "engine-launch-"));
  // The full engine (as on the device), in its new headless mode, not the
  // lighter headless-only build Playwright uses by default.
  const options = { channel: "chromium", headless: true, args: flags };
  const median = (xs) => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)];
  const time = async (f) => {
    const t0 = performance.now();
    await f();
    return Math.round(performance.now() - t0);
  };
  try {
    let engine;
    // The first start creates the engine's data, which happens once a person.
    const first = await time(async () => {
      engine = await chromium.launchPersistentContext(data, options);
      await (engine.pages()[0] ?? (await engine.newPage())).goto(page);
    });
    await engine.close();
    const cold = [];
    for (let i = 0; i < 3; i++) {
      cold.push(
        await time(async () => {
          engine = await chromium.launchPersistentContext(data, options);
          await (engine.pages()[0] ?? (await engine.newPage())).goto(page);
        }),
      );
      if (i < 2) await engine.close();
    }
    const warm = [];
    for (let i = 0; i < 3; i++) {
      warm.push(await time(async () => (await engine.newPage()).goto(page)));
    }
    await engine.close();
    console.log(`first start (new engine data): ${first} ms`);
    console.log(`cold: ${median(cold)} ms (${cold.join(", ")})`);
    console.log(`running engine, new window: ${median(warm)} ms (${warm.join(", ")})`);
  } finally {
    fs.rmSync(data, { recursive: true, force: true });
  }
})();
