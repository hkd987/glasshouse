// Moonshine tiny (ADR 0045 T2): a piece of speech in, its words out. The
// encoder hears the piece once; the decoder then picks the likeliest next
// token, one at a time, until it says the text ended, keeping what it
// worked out for the tokens before (its cache) rather than doing it again.
// The speech worker runs it in ONNX Runtime's WebAssembly build; its test,
// in Node's.

import type * as Ort from "onnxruntime-web";

import { textOf, tokenLimit } from "./transcribe.ts";

/** The model's start and end of text. */
const BOS = 1;
const EOS = 2;
/** The decoder's layers, and each one's attention heads and their size (its config.json). */
const LAYERS = 6;
const HEADS = 8;
const HEAD_SIZE = 36;

export interface Model {
  encoder: Ort.InferenceSession;
  decoder: Ort.InferenceSession;
  /** Each token's text, by id. */
  tokens: string[];
}

/** How the speech worker and its test open the model: not rearranged on load, which costs a quarter of its memory for little speed. */
export const SESSION: Ort.InferenceSession.SessionOptions = { executionProviders: ["wasm"], graphOptimizationLevel: "disabled" };

/** A piece's words, from 16 kHz samples. */
export async function recognize(Tensor: typeof Ort.Tensor, { encoder, decoder, tokens }: Model, samples: Float32Array): Promise<string> {
  const heard = await encoder.run({ input_values: new Tensor("float32", samples, [1, samples.length]) });
  const states = heard.last_hidden_state!;
  // The cache: per layer, what the decoder worked out for the tokens so far, and for the speech.
  const none = new Tensor("float32", new Float32Array(0), [1, HEADS, 0, HEAD_SIZE]);
  const cache: Record<string, Ort.Tensor> = {};
  for (let layer = 0; layer < LAYERS; layer += 1) {
    for (const part of ["decoder", "encoder"]) for (const kv of ["key", "value"]) cache[`past_key_values.${layer}.${part}.${kv}`] = none;
  }
  const ids = [BOS];
  let cached = false;
  try {
    const limit = tokenLimit(samples.length);
    while (ids.length < limit) {
      const out = await decoder.run({
        input_ids: new Tensor("int64", BigInt64Array.of(BigInt(ids[ids.length - 1]!)), [1, 1]),
        encoder_hidden_states: states,
        use_cache_branch: new Tensor("bool", [cached], [1]),
        ...cache,
      });
      const logits = out.logits!;
      const row = logits.data as Float32Array;
      let best = 0;
      for (let v = 1; v < row.length; v += 1) if (row[v]! > row[best]!) best = v;
      logits.dispose();
      for (let layer = 0; layer < LAYERS; layer += 1) {
        for (const kv of ["key", "value"]) {
          const decoded = `past_key_values.${layer}.decoder.${kv}`;
          if (cache[decoded] !== none) cache[decoded]!.dispose();
          cache[decoded] = out[`present.${layer}.decoder.${kv}`]!;
          // The speech's part is worked out once, on the first token; later ones give it back unused.
          const speech = out[`present.${layer}.encoder.${kv}`]!;
          if (cached) speech.dispose();
          else cache[`past_key_values.${layer}.encoder.${kv}`] = speech;
        }
      }
      cached = true;
      if (best === EOS) break;
      ids.push(best);
    }
  } finally {
    states.dispose();
    for (const tensor of new Set(Object.values(cache))) if (tensor !== none) tensor.dispose();
  }
  return textOf(ids.slice(1), tokens);
}
