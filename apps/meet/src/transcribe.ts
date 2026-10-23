// What Meet hears its own person say, as lines for the meeting's
// transcript (ADR 0045 T2). The microphone's audio becomes 16 kHz, is cut
// into what was said between pauses, and each piece goes to the speech
// model in a worker (speech.ts); its words come back as a line. Lines go
// to glassd together every twelve seconds, and at once when the person
// speaks to their agent ("Hey Glass, …"). Nothing here keeps audio: a
// piece is dropped once it's text.

/** The speech model's rate. */
export const RATE = 16_000;
/** What `meet.transcribe` takes in one call (glass_managed::meet). */
export const MAX_TRANSCRIPT_LINES = 50;
export const MAX_LINE_CHARS = 1_000;
export const MAX_TRANSCRIBE_CHARS = 10_000;
/** How long a line waits for others to go with it. */
export const FLUSH_MS = 12_000;
/** How long "Hey Glass" said alone waits for the request after it. */
export const WAKE_WAIT_MS = 6_000;
/** Lines kept while glassd can't be reached; the oldest go past it. */
export const MAX_WAITING = 200;

/**
 * The microphone's audio at 16 kHz: each new sample is the mean of the
 * ones it spans, which keeps what the model hears below 8 kHz. What a
 * chunk leaves over waits for the next.
 */
export class Resampler {
  readonly from: number;
  readonly #ratio: number;
  #left = new Float32Array(0);
  /** Where the next output sample starts in `#left`, in input samples. */
  #at = 0;

  constructor(from: number) {
    this.from = from;
    this.#ratio = from / RATE;
  }

  push(input: Float32Array): Float32Array {
    if (this.from === RATE) return input.slice();
    const buf = new Float32Array(this.#left.length + input.length);
    buf.set(this.#left);
    buf.set(input, this.#left.length);
    const out = new Float32Array(Math.ceil(buf.length / this.#ratio) + 1);
    let n = 0;
    let t = this.#at;
    for (;;) {
      const lo = Math.floor(t);
      const hi = Math.max(lo + 1, Math.floor(t + this.#ratio));
      if (hi > buf.length) break;
      let sum = 0;
      for (let i = lo; i < hi; i += 1) sum += buf[i]!;
      out[n++] = sum / (hi - lo);
      t += this.#ratio;
    }
    const kept = Math.min(Math.floor(t), buf.length);
    this.#left = buf.slice(kept);
    this.#at = t - kept;
    return out.subarray(0, n);
  }
}

/** One stretch of speech between pauses, and when it started (ms since 1970). */
export interface Segment {
  samples: Float32Array;
  at: number;
}

/** 30 ms, the segmenter's step. */
const FRAME = 480;
/** Speech is at least this loud (RMS of samples in -1..1)… */
const MIN_SPEECH = 0.01;
/** …and this many times the room's own noise. */
const OVER_NOISE = 3;
/** Kept from before speech starts, so its first sound isn't cut: 300 ms. */
const BEFORE = 10;
/** A pause this long ends a piece: 690 ms. */
const PAUSE = 23;
/** Kept of the pause after speech: 210 ms. */
const AFTER = 7;
/** Less speech than this is a cough or a click, not words: 240 ms. */
const SHORTEST = 8;
/** The longest piece the model gets; a longer one is cut there: 15 s. */
const LONGEST = 500;

/** The room's noise is the quietest frame of the last 3 s: words have gaps. */
const NOISE_WINDOW = 100;

/**
 * Cuts 16 kHz audio into what was said between pauses, by loudness over
 * the room's noise.
 */
export class Segmenter {
  #carry = new Float32Array(0);
  /** The last frames' loudness, for the room's noise. */
  #levels: number[] = [];
  /** The last frames heard while not in speech: what comes before it. */
  #before: Float32Array[] = [];
  /** The open piece, its speech frames, the quiet frames since the last, and when it started. */
  #open: Float32Array[] | null = null;
  #speech = 0;
  #quiet = 0;
  #at = 0;

  /** Takes audio ending at `now` (ms since 1970); returns the pieces it closed. */
  push(samples: Float32Array, now: number): Segment[] {
    const buf = new Float32Array(this.#carry.length + samples.length);
    buf.set(this.#carry);
    buf.set(samples, this.#carry.length);
    const done: Segment[] = [];
    let i = 0;
    for (; i + FRAME <= buf.length; i += FRAME) {
      const frame = buf.slice(i, i + FRAME);
      // When this frame started.
      const at = now - ((buf.length - i) * 1000) / RATE;
      const piece = this.#frame(frame, at);
      if (piece) done.push(piece);
    }
    this.#carry = buf.slice(i);
    return done;
  }

  /** Closes the open piece, if it has words: when the person mutes or leaves. */
  flush(): Segment | null {
    this.#carry = new Float32Array(0);
    this.#before = [];
    return this.#close();
  }

  #frame(frame: Float32Array, at: number): Segment | null {
    let sum = 0;
    for (const x of frame) sum += x * x;
    const level = Math.sqrt(sum / frame.length);
    this.#levels.push(level);
    if (this.#levels.length > NOISE_WINDOW) this.#levels.shift();
    const speech = level > Math.max(MIN_SPEECH, Math.min(...this.#levels) * OVER_NOISE);

    if (!this.#open) {
      if (!speech) {
        this.#before.push(frame);
        if (this.#before.length > BEFORE) this.#before.shift();
        return null;
      }
      this.#at = at - this.#before.length * ((FRAME * 1000) / RATE);
      this.#open = [...this.#before, frame];
      this.#before = [];
      this.#speech = 1;
      this.#quiet = 0;
      return null;
    }
    this.#open.push(frame);
    if (speech) {
      this.#speech += 1;
      this.#quiet = 0;
    } else {
      this.#quiet += 1;
    }
    if (this.#quiet >= PAUSE) return this.#close();
    // Cut a long one here; speech that goes on opens the next.
    if (this.#open.length >= LONGEST) return this.#close();
    return null;
  }

  #close(): Segment | null {
    const open = this.#open;
    const speech = this.#speech;
    const quiet = this.#quiet;
    this.#open = null;
    this.#speech = 0;
    this.#quiet = 0;
    if (!open || speech < SHORTEST) return null;
    const frames = open.slice(0, open.length - Math.max(0, quiet - AFTER));
    const samples = new Float32Array(frames.length * FRAME);
    frames.forEach((f, n) => samples.set(f, n * FRAME));
    return { samples, at: this.#at };
  }
}

/** The most tokens a piece may take, about 6.5 a second and a few more: stops a model that loops. */
export function tokenLimit(samples: number): number {
  return Math.ceil((samples / RATE) * 6.5) + 4;
}

/**
 * The model's tokens as text: `▁` is a space, and `<0xNN>` tokens are
 * bytes of a character the vocabulary lacks.
 */
export function textOf(ids: number[], tokens: string[]): string {
  const bytes: number[] = [];
  const decoder = new TextDecoder();
  let text = "";
  const spill = () => {
    if (bytes.length === 0) return;
    text += decoder.decode(Uint8Array.from(bytes));
    bytes.length = 0;
  };
  for (const id of ids) {
    const token = tokens[id] ?? "";
    const byte = /^<0x([0-9A-F]{2})>$/.exec(token);
    if (byte) {
      bytes.push(Number.parseInt(byte[1]!, 16));
      continue;
    }
    spill();
    // The model's own markers (<s>, </s>, <unk>) aren't words.
    if (/^<[^>]*>$/.test(token)) continue;
    text += token.replaceAll("▁", " ");
  }
  spill();
  return text;
}

/**
 * A line as it goes into the transcript: the quotes the model sometimes
 * wraps a whole line in go, spaces are single, and it fits a line. Empty
 * when nothing in it is a word.
 */
export function cleanLine(text: string): string {
  let line = text.replace(/\s+/g, " ").trim();
  for (;;) {
    const unwrapped = line.replace(/^["'“”‘’]+|["'“”‘’]+$/g, "").trim();
    if (unwrapped === line) break;
    line = unwrapped;
  }
  if (!/[\p{L}\p{N}]/u.test(line)) return "";
  if (line.length > MAX_LINE_CHARS) line = `${line.slice(0, MAX_LINE_CHARS - 1).trimEnd()}…`;
  return line;
}

/** What wakes the person's agent, word by word: glass_managed::meet::WAKE_WORDS. */
const WAKE_WORDS = [
  ["hey", "glass"],
  ["hey", "glasshouse"],
  ["ok", "glass"],
  ["okay", "glass"],
];

/**
 * Whether `line` speaks to the person's agent, as glassd reads it
 * (glass_managed::meet::addressed): the request after "Hey Glass", "" when
 * nothing follows it yet, null when it doesn't.
 */
export function wake(line: string): string | null {
  const words = line.split(/\s+/).filter(Boolean);
  const plain = words.map((w) => w.toLowerCase().replace(/[^\p{L}\p{N}]/gu, ""));
  for (let start = 0; start < plain.length; start += 1) {
    for (const phrase of WAKE_WORDS) {
      const end = start + phrase.length;
      if (end <= plain.length && phrase.every((w, k) => plain[start + k] === w)) {
        return words
          .slice(end)
          .join(" ")
          .replace(/^[\s!-/:-@[-`{-~]+/, "")
          .trim();
      }
    }
  }
  return null;
}

/** A line of the transcript, as `meet.transcribe` takes it. */
export interface Line {
  /** When it was said (RFC 3339). */
  at: string;
  text: string;
}

/**
 * The lines waiting to go. A line goes with the others every
 * [`FLUSH_MS`], or at once when it asks the agent for something. "Hey
 * Glass" said alone, then a pause, waits for what follows and goes with
 * it as one line, as the person meant it.
 */
export class Batcher {
  #lines: Line[] = [];
  /** When the oldest waiting line came. */
  #since = 0;
  /** "Hey Glass" alone, waiting for its request, and when it came. */
  #held: Line | null = null;
  #heldSince = 0;

  get size(): number {
    return this.#lines.length + (this.#held ? 1 : 0);
  }

  /** Adds a line heard at `now`; true when it should go at once. */
  add(line: Line, now: number): boolean {
    if (this.#held) {
      const held = this.#held;
      this.#held = null;
      if (now - this.#heldSince <= WAKE_WAIT_MS) {
        line = { at: held.at, text: `${held.text} ${line.text}` };
      } else {
        this.#push(held, this.#heldSince);
      }
    }
    const request = wake(line.text);
    if (request === "") {
      this.#held = line;
      this.#heldSince = now;
      return false;
    }
    this.#push(line, now);
    return request !== null;
  }

  /** Whether the lines have waited long enough to go. */
  due(now: number): boolean {
    if (this.#held && now - this.#heldSince > WAKE_WAIT_MS) {
      const held = this.#held;
      this.#held = null;
      this.#push(held, this.#heldSince);
    }
    return this.#lines.length > 0 && now - this.#since >= FLUSH_MS;
  }

  /** The lines for one call, oldest first, within what it takes; "Hey Glass" waiting stays. */
  take(): Line[] {
    const out: Line[] = [];
    let chars = 0;
    while (out.length < MAX_TRANSCRIPT_LINES && this.#lines.length > 0) {
      const next = this.#lines[0]!;
      if (out.length > 0 && chars + next.text.length > MAX_TRANSCRIBE_CHARS) break;
      out.push(next);
      chars += next.text.length;
      this.#lines.shift();
    }
    return out;
  }

  /** Lines a call didn't add: they go first next time, the oldest dropped past [`MAX_WAITING`]. */
  putBack(lines: Line[], now: number): void {
    if (this.#lines.length === 0) this.#since = now;
    this.#lines = [...lines, ...this.#lines].slice(-MAX_WAITING);
  }

  /** Everything, "Hey Glass" waiting too: when the person leaves. */
  drain(): Line[] {
    if (this.#held) {
      this.#lines.push(this.#held);
      this.#held = null;
    }
    return this.take();
  }

  #push(line: Line, now: number): void {
    if (this.#lines.length === 0) this.#since = now;
    this.#lines.push(line);
    if (this.#lines.length > MAX_WAITING) this.#lines.shift();
  }
}

/** The page to the speech worker. */
export type ToSpeech =
  /** The microphone's audio, as the engine hands it over. */
  | { type: "start"; audio: ReadableStream<AudioData> }
  /** Muted: what's heard from now on isn't transcribed; what was said before is. */
  | { type: "muted"; on: boolean };

/** The speech worker to the page. */
export type FromSpeech =
  | { type: "ready"; loadMs: number }
  | { type: "line"; line: Line; ms: number }
  /** The audio ended and every piece of it is text. */
  | { type: "done" }
  | { type: "failed"; error: string };
