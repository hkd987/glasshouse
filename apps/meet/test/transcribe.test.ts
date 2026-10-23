// Transcribing what this computer's person says (ADR 0045 T2): the
// microphone's audio becomes 16 kHz, is cut where they pause, and each
// piece's words become a line; lines go every twelve seconds, and at once
// when they speak to their agent. The last test runs the speech model
// Meet ships on recorded speech (tools/fetch-speech-model.sh, then make
// apps-build), and is skipped without it.

import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { SESSION, recognize } from "../src/moonshine.ts";
import {
  Batcher,
  FLUSH_MS,
  type Line,
  MAX_LINE_CHARS,
  MAX_TRANSCRIBE_CHARS,
  MAX_TRANSCRIPT_LINES,
  MAX_WAITING,
  RATE,
  Resampler,
  type Segment,
  Segmenter,
  WAKE_WAIT_MS,
  cleanLine,
  textOf,
  tokenLimit,
  wake,
} from "../src/transcribe.ts";

const here = dirname(fileURLToPath(import.meta.url));

const tone = (hz: number, seconds: number, rate: number, level = 0.5) =>
  Float32Array.from({ length: Math.round(seconds * rate) }, (_, i) => level * Math.sin((2 * Math.PI * hz * i) / rate));
const rms = (x: Float32Array) => Math.sqrt(x.reduce((s, v) => s + v * v, 0) / x.length);
const join16 = (...parts: Float32Array[]) => {
  const out = new Float32Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
};
/** Quiet room noise, the same every run. */
const noise = (seconds: number, level = 0.002) => {
  let seed = 7;
  return Float32Array.from({ length: Math.round(seconds * RATE) }, () => {
    seed = (seed * 1103515245 + 12345) % 2 ** 31;
    return level * ((seed / 2 ** 31) * 2 - 1) * Math.sqrt(3);
  });
};
/** Speech-like sound: a voice's pitch, its loudness rising and falling with syllables. */
const voice = (seconds: number) =>
  Float32Array.from({ length: Math.round(seconds * RATE) }, (_, i) => {
    const t = i / RATE;
    return 0.2 * (0.6 + 0.4 * Math.sin(2 * Math.PI * 4 * t)) * Math.sin(2 * Math.PI * 180 * t);
  });

/** Pushes audio in 10 ms chunks, as the engine hands it over, with the clock running. */
function segments(audio: Float32Array, start = 1_000_000): Segment[] {
  const s = new Segmenter();
  const out: Segment[] = [];
  const chunk = RATE / 100;
  for (let i = 0; i < audio.length; i += chunk) {
    const part = audio.subarray(i, i + chunk);
    out.push(...s.push(part, start + ((i + part.length) * 1000) / RATE));
  }
  const last = s.flush();
  if (last) out.push(last);
  return out;
}

test("adr0045 t2 the microphone's audio becomes 16 kHz, in chunks as in one, keeping speech and dropping what's above it", () => {
  const second = tone(440, 1, 48_000);
  const once = new Resampler(48_000).push(second);
  assert.ok(Math.abs(once.length - RATE) <= 1, `${once.length} samples`);
  const chunked = new Resampler(48_000);
  const parts: Float32Array[] = [];
  for (let i = 0; i < second.length; i += 480) parts.push(chunked.push(second.subarray(i, i + 480)));
  const joined = join16(...parts);
  assert.equal(joined.length, once.length);
  assert.ok(joined.every((v, i) => Math.abs(v - once[i]!) < 1e-6), "the same, chunk by chunk");
  // A voice's frequencies stay; what the model can't hear anyway is cut.
  assert.ok(rms(new Resampler(48_000).push(tone(3000, 1, 48_000))) > 0.3);
  assert.ok(rms(new Resampler(48_000).push(tone(12_000, 1, 48_000))) < 0.15);
  // 44.1 kHz too, and 16 kHz as it is.
  assert.ok(Math.abs(new Resampler(44_100).push(tone(440, 1, 44_100)).length - RATE) <= 1);
  assert.deepEqual(new Resampler(RATE).push(Float32Array.of(0.1, 0.2)), Float32Array.of(0.1, 0.2));
});

test("adr0045 t2 speech is cut where the person pauses, with when they started, and a click isn't speech", () => {
  const click = Float32Array.from({ length: RATE / 10 }, (_, i) => (i % 2 ? 0.4 : -0.4));
  const audio = join16(noise(1), voice(1.2), noise(1), click, noise(1), voice(2), noise(0.3));
  const found = segments(audio, 0);
  assert.equal(found.length, 2, "two stretches of speech; the click is dropped");
  const [first, second] = found as [Segment, Segment];
  // Each starts 300 ms before its speech, so its first sound isn't cut…
  assert.ok(Math.abs(first.at - 700) <= 40, `first at ${first.at} ms`);
  assert.ok(Math.abs(second.at - (1000 + 1200 + 1000 + 100 + 1000 - 300)) <= 40, `second at ${second.at} ms`);
  // …and keeps a little of the pause after it.
  const seconds = first.samples.length / RATE;
  assert.ok(seconds > 1.6 && seconds < 1.8, `first lasts ${seconds} s`);
});

test("adr0045 t2 a room's steady noise isn't speech, and a long stretch is cut at fifteen seconds", () => {
  const fan = Float32Array.from(noise(6, 0.03));
  assert.equal(segments(fan).length, 0, "a fan never opens a piece");
  const loud = join16(fan, voice(2).map((v) => v * 2), noise(1, 0.03));
  assert.equal(segments(loud).length, 1, "speech over the fan does");
  const long = segments(join16(noise(0.5), voice(20), noise(1)));
  assert.equal(long.length, 2);
  assert.ok(long[0]!.samples.length / RATE <= 15.01, "fifteen seconds at most");
});

test("adr0045 t2 the model's tokens read as text, bytes it spelled out included", () => {
  const tokens = ["<unk>", "<s>", "</s>", "▁Hey", "▁Glass", ",", "<0xC3>", "<0xA9>", "▁caf"];
  assert.equal(textOf([3, 4, 5], tokens), " Hey Glass,");
  assert.equal(textOf([8, 6, 7, 2], tokens), " café");
  assert.equal(tokenLimit(5 * RATE), 37);
});

test("adr0045 t2 a line loses the quotes the model wraps it in, and nothing that isn't a word becomes one", () => {
  assert.equal(cleanLine(' "Hey Glass, find the ABC report."  '), "Hey Glass, find the ABC report.");
  assert.equal(cleanLine("“OK.”"), "OK.");
  assert.equal(cleanLine('He said "ship it" twice'), 'He said "ship it" twice');
  assert.equal(cleanLine("  ...  "), "");
  assert.equal(cleanLine('"'), "");
  const long = cleanLine("word ".repeat(400));
  assert.equal(long.length, MAX_LINE_CHARS);
  assert.ok(long.endsWith("…"));
});

test("adr0045 t2 Hey Glass reads here as glassd reads it", () => {
  // glass_managed::meet's adr0045_hey_glass_and_what_follows_as_said.
  for (const [said, request] of [
    ["Hey Glass, send the Q3 report to Susan.", "send the Q3 report to Susan."],
    ["Hey, glass, send a cute free report to Susan.", "send a cute free report to Susan."],
    ["OK so, hey Glasshouse: what did Ana say?", "what did Ana say?"],
    ["okay GLASS book the room", "book the room"],
    ["Hey Glass.", ""],
    ["Let's move the launch to Thursday.", null],
    ["Hey Sam, glass is fragile", null],
  ] as const) {
    assert.equal(wake(said), request, said);
  }
});

const line = (text: string, at = "2026-10-23T10:00:00.000Z"): Line => ({ at, text });

test("adr0045 t2 lines go together every twelve seconds, and at once when they ask the agent", () => {
  const b = new Batcher();
  assert.equal(b.add(line("I'll send the deck after this call."), 0), false);
  assert.equal(b.add(line("Then we can sign."), 3000), false);
  assert.equal(b.due(FLUSH_MS - 1), false);
  assert.equal(b.due(FLUSH_MS), true);
  assert.deepEqual(
    b.take().map((l) => l.text),
    ["I'll send the deck after this call.", "Then we can sign."],
  );
  assert.equal(b.due(FLUSH_MS * 3), false, "nothing waits");
  assert.equal(b.add(line("Hey Glass, send the ABC report to Susan."), 20_000), true);
  assert.equal(b.take().length, 1);
});

test("adr0045 t2 Hey Glass said alone waits for the request after it, and goes as one line", () => {
  const b = new Batcher();
  assert.equal(b.add(line("Hey Glass.", "2026-10-23T10:00:00.000Z"), 0), false);
  assert.equal(b.due(WAKE_WAIT_MS - 1), false, "it waits for what follows");
  assert.equal(b.add(line("Send the ABC report to Susan.", "2026-10-23T10:00:02.000Z"), WAKE_WAIT_MS), true);
  assert.deepEqual(b.take(), [line("Hey Glass. Send the ABC report to Susan.", "2026-10-23T10:00:00.000Z")]);
  // Nothing followed in time: it's a line of its own, and what came later is just said.
  assert.equal(b.add(line("Hey Glass."), 0), false);
  assert.equal(b.add(line("Never mind."), WAKE_WAIT_MS + 1), false);
  assert.deepEqual(
    b.take().map((l) => l.text),
    ["Hey Glass.", "Never mind."],
  );
  b.add(line("Hey Glass."), 0);
  assert.equal(b.due(WAKE_WAIT_MS + 1), false);
  assert.equal(b.due(FLUSH_MS + 1), true);
  // Leaving sends one still waiting.
  b.take();
  b.add(line("Hey Glass."), 0);
  assert.deepEqual(
    b.drain().map((l) => l.text),
    ["Hey Glass."],
  );
});

test("adr0045 t2 a call takes what meet.transcribe takes, and lines it couldn't add wait in order", () => {
  const b = new Batcher();
  for (let i = 0; i < 60; i += 1) b.add(line(`Line ${i}`), i);
  const first = b.take();
  assert.equal(first.length, MAX_TRANSCRIPT_LINES);
  assert.equal(b.size, 10);
  b.putBack(first, 100);
  assert.equal(b.take()[0]!.text, "Line 0", "the oldest go first");
  const wide = new Batcher();
  for (let i = 0; i < 12; i += 1) wide.add(line("x".repeat(MAX_LINE_CHARS)), i);
  assert.equal(wide.take().length, MAX_TRANSCRIBE_CHARS / MAX_LINE_CHARS);
  // glassd unreachable for long: the newest are kept.
  const kept = new Batcher();
  kept.putBack(
    Array.from({ length: MAX_WAITING + 5 }, (_, i) => line(`Line ${i}`)),
    0,
  );
  assert.equal(kept.size, MAX_WAITING);
  assert.equal(kept.take()[0]!.text, "Line 5");
});

/** 16-bit mono PCM, as -1..1 floats, and its rate. */
function wav(path: string): { rate: number; samples: Float32Array } {
  const data = readFileSync(path);
  const rate = data.readUInt32LE(24);
  const at = data.indexOf("data") + 8;
  const samples = new Float32Array((data.length - at) / 2);
  for (let i = 0; i < samples.length; i += 1) samples[i] = data.readInt16LE(at + 2 * i) / 32768;
  return { rate, samples };
}

const SHIPPED = join(here, "../dist/speech");

test(
  "adr0045 t2 recorded speech becomes the lines said, and Hey Glass asks at once",
  { skip: existsSync(join(SHIPPED, "encoder.onnx")) ? false : "no speech model in Meet (tools/fetch-speech-model.sh, make apps-build)" },
  async () => {
    const ort = await import("onnxruntime-web");
    ort.env.wasm.numThreads = 1;
    const model = {
      encoder: await ort.InferenceSession.create(readFileSync(join(SHIPPED, "encoder.onnx")), SESSION),
      decoder: await ort.InferenceSession.create(readFileSync(join(SHIPPED, "decoder.onnx")), SESSION),
      tokens: JSON.parse(readFileSync(join(SHIPPED, "tokens.json"), "utf8")) as string[],
    };
    // LJ Speech's voice, through Piper (fixtures/README.md), at 22.05 kHz.
    const { rate, samples } = wav(join(here, "fixtures/hey-glass.wav"));
    const resampler = new Resampler(rate);
    const segmenter = new Segmenter();
    const pieces: Segment[] = [];
    const chunk = Math.round(rate / 100);
    for (let i = 0; i < samples.length; i += chunk) {
      pieces.push(...segmenter.push(resampler.push(samples.subarray(i, i + chunk)), (i * 1000) / rate));
    }
    const last = segmenter.flush();
    if (last) pieces.push(last);
    assert.equal(pieces.length, 2, "two sentences, a pause between");
    const lines: string[] = [];
    for (const piece of pieces) {
      const started = performance.now();
      lines.push(cleanLine(await recognize(ort.Tensor, model, piece.samples)));
      console.log(`${(piece.samples.length / RATE).toFixed(1)} s of speech in ${(performance.now() - started).toFixed(0)} ms: ${lines.at(-1)}`);
    }
    assert.match(lines[0]!, /send the deck after this call/i);
    assert.match(wake(lines[1]!) ?? "", /^find the ABC report and send it to Susan\.?$/i);
    const batch = new Batcher();
    assert.equal(batch.add(line(lines[0]!), 0), false);
    assert.equal(batch.add(line(lines[1]!), 1000), true, "Hey Glass goes at once");
  },
);
