// Meet's speech worker (ADR 0045 T2): turns what this computer's
// microphone hears into lines of text, on the computer. Moonshine tiny
// (English) runs in ONNX Runtime's WebAssembly build, one thread, from
// files Meet ships (`speech/`, `ort/`); nothing it hears or says leaves
// this worker except the lines it posts to the page. Only the page's own
// microphone reaches it: never anyone else's audio.

import * as ort from "onnxruntime-web/wasm";

import { type Model, SESSION, recognize } from "./moonshine.ts";
import { type FromSpeech, Resampler, Segmenter, type Segment, type ToSpeech, cleanLine } from "./transcribe.ts";

/** What this worker uses of its global scope. */
interface Scope {
  location: Location;
  postMessage(message: FromSpeech): void;
  onmessage: ((event: MessageEvent<ToSpeech>) => void) | null;
}
const scope = self as unknown as Scope;

ort.env.wasm.numThreads = 1;
ort.env.wasm.proxy = false;
ort.env.wasm.wasmPaths = { wasm: new URL("./ort/ort-wasm-simd-threaded.wasm", scope.location.href).href };

let model: Promise<Model> | null = null;
let muted = false;
/** Pieces waiting for the model, oldest first, recognized one at a time. */
let work: Promise<void> = Promise.resolve();
const segmenter = new Segmenter();
let resampler: Resampler | null = null;

function post(message: FromSpeech): void {
  scope.postMessage(message);
}

async function bytes(path: string): Promise<Uint8Array> {
  const response = await fetch(path);
  if (!response.ok) throw new Error(`${path}: ${response.status}`);
  return new Uint8Array(await response.arrayBuffer());
}

async function load(): Promise<Model> {
  const started = performance.now();
  const [encoder, decoder, tokens] = await Promise.all([
    bytes("./speech/encoder.onnx").then((b) => ort.InferenceSession.create(b, SESSION)),
    bytes("./speech/decoder.onnx").then((b) => ort.InferenceSession.create(b, SESSION)),
    fetch("./speech/tokens.json").then((r) => r.json() as Promise<string[]>),
  ]);
  post({ type: "ready", loadMs: Math.round(performance.now() - started) });
  return { encoder, decoder, tokens };
}

/** Queues a piece for the model; its line goes to the page. */
function hear(piece: Segment): void {
  work = work.then(async () => {
    try {
      const loaded = await (model ??= load());
      const started = performance.now();
      const text = cleanLine(await recognize(ort.Tensor, loaded, piece.samples));
      if (text) post({ type: "line", line: { at: new Date(piece.at).toISOString(), text }, ms: Math.round(performance.now() - started) });
    } catch (error) {
      post({ type: "failed", error: String(error) });
    }
  });
}

/** One frame of the microphone, its first channel as 32-bit floats. */
function samplesOf(frame: AudioData): Float32Array {
  const samples = new Float32Array(frame.numberOfFrames);
  frame.copyTo(samples, { planeIndex: 0, format: "f32-planar" });
  return samples;
}

async function listen(audio: ReadableStream<AudioData>): Promise<void> {
  // Load while the person starts talking.
  model ??= load();
  model.catch((error: unknown) => post({ type: "failed", error: String(error) }));
  const reader = audio.getReader();
  for (;;) {
    const { value: frame, done } = await reader.read();
    if (done) break;
    try {
      if (muted) continue;
      if (resampler?.from !== frame.sampleRate) resampler = new Resampler(frame.sampleRate);
      for (const piece of segmenter.push(resampler.push(samplesOf(frame)), Date.now())) hear(piece);
    } finally {
      frame.close();
    }
  }
  const last = segmenter.flush();
  if (last) hear(last);
  await work;
  post({ type: "done" });
}

scope.onmessage = ({ data }) => {
  if (data.type === "start") {
    void listen(data.audio).catch((error: unknown) => post({ type: "failed", error: String(error) }));
  } else if (data.type === "muted") {
    muted = data.on;
    if (muted) {
      // What was said before muting is the person's, and others heard it.
      const last = segmenter.flush();
      if (last) hear(last);
    }
  }
};
