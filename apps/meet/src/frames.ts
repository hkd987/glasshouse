// How a meeting seals the frames it sends and opens the frames it gets
// (CF-21, ADR 0030 §6). The bytes the packetizer reads stay clear and are
// authenticated as SFrame metadata; the rest is sealed. A frame is never
// sent before there is a key, and one that doesn't open is dropped, so
// nothing unsealed leaves and nothing forged reaches the decoder.
//
// A meeting can have several media keys over its life: when someone
// leaves, a remaining computer makes a new one. Each has a generation; the
// newest seals what this computer sends, and each sender's frames open
// with whichever key that sender uses. A key that a newer one replaced
// still opens frames in flight for two seconds, then is forgotten.

import { decodeHeader, deriveSenderKey, open, seal, type SenderKey } from "../../sdk/src/sframe.ts";

export type Kind = "audio" | "video";

/** VP8's payload header: 10 bytes on a key frame, 3 on others. None for Opus. */
export function clearBytes(kind: Kind, type: string | undefined): number {
  if (kind === "audio") return 0;
  return type === "key" ? 10 : 3;
}

/** How long an old media key still opens frames in flight. */
export const OLD_KEY_MS = 2000;

interface Generation {
  mediaKey: Uint8Array;
  senders: Map<bigint, Promise<SenderKey>>;
  /** When a newer key replaced it, plus OLD_KEY_MS; never while newest. */
  until: number;
}

function senderKey(g: Generation, kid: bigint): Promise<SenderKey> {
  let key = g.senders.get(kid);
  if (!key) {
    key = deriveSenderKey(g.mediaKey, kid);
    g.senders.set(kid, key);
  }
  return key;
}

function same(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((byte, i) => byte === b[i]);
}

/** One participant's frame keys. Lives in the transform worker's memory only. */
export class FrameCipher {
  /** The media keys by generation. */
  #keys = new Map<number, Generation>();
  #newest: number | null = null;
  /** Which generation last opened each sender's (KID's) frames: tried first. */
  #lastOpened = new Map<bigint, number>();
  #me = 0n;
  /**
   * Each (KID, CTR) seals once (RFC 9605 §9.1). Audio and video share
   * the counter; it starts at random, so joining the same session again
   * under the same index doesn't reuse a nonce.
   */
  #ctr: bigint;
  opened = 0;
  dropped = 0;

  constructor() {
    const start = crypto.getRandomValues(new Uint8Array(6));
    this.#ctr = start.reduce((n, b) => (n << 8n) | BigInt(b), 0n);
  }

  /**
   * A media key of the meeting, with this participant's index (its KID)
   * and the key's generation: higher is newer. A newer generation seals
   * from now on, and the ones it replaces open frames for OLD_KEY_MS more.
   * An older one, arriving late, only opens frames in flight. The same
   * generation again changes nothing.
   */
  setKey(participant: number, mediaKey: Uint8Array, generation = 0, now = Date.now()): void {
    this.#me = BigInt(participant);
    const known = this.#keys.get(generation);
    if (known && same(known.mediaKey, mediaKey)) return;
    const newest = this.#newest === null || generation >= this.#newest;
    const key: Generation = { mediaKey: mediaKey.slice(), senders: new Map(), until: newest ? Infinity : now + OLD_KEY_MS };
    if (newest) {
      for (const g of this.#keys.values()) g.until = Math.min(g.until, now + OLD_KEY_MS);
      this.#newest = generation;
    }
    this.#keys.set(generation, key);
  }

  /** The generations that still open frames, newest first; forgets the rest. */
  #live(now: number): number[] {
    for (const [generation, g] of this.#keys) {
      if (generation !== this.#newest && g.until <= now) this.#keys.delete(generation);
    }
    return [...this.#keys.keys()].sort((a, b) => b - a);
  }

  /** The frame to send, sealed with the newest key; null until there is a key. */
  async sealFrame(data: ArrayBuffer, kind: Kind, type?: string): Promise<ArrayBuffer | null> {
    const g = this.#newest === null ? undefined : this.#keys.get(this.#newest);
    if (!g) return null;
    const bytes = new Uint8Array(data);
    const clear = Math.min(clearBytes(kind, type), bytes.length);
    const ctr = this.#ctr;
    this.#ctr += 1n;
    const sealed = await seal(await senderKey(g, this.#me), ctr, bytes.subarray(clear), bytes.subarray(0, clear));
    const out = new Uint8Array(clear + sealed.length);
    out.set(bytes.subarray(0, clear));
    out.set(sealed, clear);
    return out.buffer;
  }

  /** The frame received, opened; null if no key of this meeting opens it. */
  async openFrame(data: ArrayBuffer, kind: Kind, type?: string, now = Date.now()): Promise<ArrayBuffer | null> {
    const bytes = new Uint8Array(data);
    const clear = Math.min(clearBytes(kind, type), bytes.length);
    const metadata = bytes.subarray(0, clear);
    const body = bytes.subarray(clear);
    const kid = decodeHeader(body)?.kid;
    const live = this.#live(now);
    const last = kid === undefined ? undefined : this.#lastOpened.get(kid);
    const order = last !== undefined && live.includes(last) ? [last, ...live.filter((g) => g !== last)] : live;
    for (const generation of order) {
      const g = this.#keys.get(generation);
      if (!g) continue;
      const opened = await open((k) => senderKey(g, k), body, metadata);
      if (opened) {
        if (kid !== undefined) this.#lastOpened.set(kid, generation);
        this.opened += 1;
        const out = new Uint8Array(clear + opened.length);
        out.set(metadata);
        out.set(opened, clear);
        return out.buffer;
      }
    }
    this.dropped += 1;
    return null;
  }

  /** The generations held now, newest first (for tests). */
  generations(now = Date.now()): number[] {
    return this.#live(now);
  }
}
