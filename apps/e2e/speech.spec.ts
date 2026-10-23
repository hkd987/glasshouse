// Meet's speech, on the computer (ADR 0045 T2), as the app bridge serves
// it: the speech worker fetches the model and the runtime's WebAssembly
// from Meet's own files and runs them under the bridge's policy, which
// allows WebAssembly to Meet alone (it declares `speech`). Recorded speech,
// handed over as the engine's microphone hands it, comes back as its
// lines. Every other app is still refused WebAssembly. The whole call, with
// the engine's microphone, is in meet.e2e.ts.

import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { type Browser, expect, test } from "@playwright/test";

import { REPO, appUrl } from "./env.ts";

const MODEL = join(REPO, "apps/meet/dist/speech/encoder.onnx");
const RECORDING = join(REPO, "apps/meet/test/fixtures/hey-glass.wav");
/** A WebAssembly module's first eight bytes: the smallest one there is. */
const EMPTY_MODULE = [0, 0x61, 0x73, 0x6d, 1, 0, 0, 0];

/** The recording's samples (16-bit mono) and rate. */
function recording(): { rate: number; pcm: number[] } {
  const data = readFileSync(RECORDING);
  const at = data.indexOf("data") + 8;
  const pcm: number[] = [];
  for (let i = at; i + 1 < data.length; i += 2) pcm.push(data.readInt16LE(i));
  return { rate: data.readUInt32LE(24), pcm };
}

async function rendererMb(browser: Browser): Promise<number> {
  const cdp = await browser.newBrowserCDPSession();
  const info = (await cdp.send("SystemInfo.getProcessInfo")) as { processInfo: { type: string; id: number }[] };
  await cdp.detach();
  return Math.max(
    ...info.processInfo
      .filter((p) => p.type === "renderer")
      .map((p) => Number(/VmRSS:\s+(\d+)/.exec(readFileSync(`/proc/${p.id}/status`, "utf8"))?.[1] ?? 0) / 1024),
  );
}

test("adr0045 t2 meet's speech worker runs the model it ships under the bridge's policy, and turns speech into lines", async ({ browser }) => {
  test.skip(!existsSync(MODEL), "no speech model in Meet (tools/fetch-speech-model.sh, then make apps-build)");
  const context = await browser.newContext();
  const page = await context.newPage();
  const refused: string[] = [];
  page.on("console", (m) => {
    if (/Content Security Policy|Refused to/i.test(m.text())) refused.push(m.text());
  });
  await page.goto(appUrl("meet"));
  await page.waitForSelector("html[data-ready=true]", { state: "attached" });

  const { rate, pcm } = recording();
  const started = Date.now();
  const heard = await page.evaluate(
    async ({ rate, pcm }) => {
      // 10 ms frames, as MediaStreamTrackProcessor hands them over.
      const samples = Float32Array.from(pcm, (v) => v / 32768);
      const step = Math.round(rate / 100);
      let at = 0;
      const audio = new ReadableStream<AudioData>({
        pull(controller) {
          if (at >= samples.length) return controller.close();
          const part = samples.slice(at, at + step);
          controller.enqueue(
            new AudioData({ format: "f32-planar", sampleRate: rate, numberOfChannels: 1, numberOfFrames: part.length, timestamp: (at * 1e6) / rate, data: part }),
          );
          at += step;
        },
      });
      const worker = new Worker("./speech.js", { type: "module" });
      return new Promise<{ lines: string[]; loadMs: number; ms: number[] }>((resolve, reject) => {
        const out = { lines: [] as string[], loadMs: 0, ms: [] as number[] };
        worker.onerror = (e) => reject(new Error(e.message));
        worker.onmessage = ({ data }) => {
          if (data.type === "ready") out.loadMs = data.loadMs;
          if (data.type === "line") {
            out.lines.push(data.line.text);
            out.ms.push(data.ms);
          }
          if (data.type === "failed") reject(new Error(data.error));
          if (data.type === "done") resolve(out);
        };
        worker.postMessage({ type: "start", audio }, [audio as unknown as Transferable]);
      });
    },
    { rate, pcm },
  );
  const memoryMb = await rendererMb(browser);
  console.log(
    `speech: model loaded in ${heard.loadMs} ms; lines in ${heard.ms.join(", ")} ms; all in ${Date.now() - started} ms; renderer ${memoryMb.toFixed(0)} MB`,
  );
  expect(heard.lines).toHaveLength(2);
  expect(heard.lines[0]).toMatch(/send the deck after this call/i);
  expect(heard.lines[1]).toMatch(/^hey,? glass,? find the ABC report and send it to Susan\.?$/i);
  expect(refused, "nothing refused by the page's policy").toEqual([]);
  // Meet's own page may compile WebAssembly too: its policy is the same.
  expect(await page.evaluate(async (bytes) => WebAssembly.compile(new Uint8Array(bytes)).then(() => "compiled"), EMPTY_MODULE)).toBe("compiled");
  await context.close();
});

test("adr0045 t2 apps that don't turn speech into text can't run WebAssembly", async ({ browser }) => {
  const context = await browser.newContext();
  for (const app of ["mail", "chat", "docs"]) {
    const page = await context.newPage();
    await page.goto(appUrl(app));
    await page.waitForSelector("html[data-ready=true]", { state: "attached" });
    const result = await page.evaluate(
      async (bytes) =>
        WebAssembly.compile(new Uint8Array(bytes)).then(
          () => "compiled",
          (e: Error) => `${e.name}: ${e.message}`,
        ),
      EMPTY_MODULE,
    );
    expect(result, app).toMatch(/^CompileError: .*(Content Security Policy|unsafe-eval)/);
    await page.close();
  }
  await context.close();
});
