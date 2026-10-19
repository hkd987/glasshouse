// How a meeting seals the frames it sends and opens the frames it gets
// (CF-21, ADR 0030 §6). The bytes the packetizer reads stay clear and are
// authenticated as SFrame metadata; the rest is sealed. A frame is never
// sent before there is a key, and one that doesn't open is dropped, so
// nothing unsealed leaves and nothing forged reaches the decoder.

import { deriveSenderKey, open, seal, type SenderKey } from "../../sdk/src/sframe.ts";

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
}

function generation(mediaKey: Uint8Array): Generation {
  return { mediaKey: mediaKey.slice(), senders: new Map() };
}

function senderKey(g: Generation, kid: bigint): Promise<SenderKey> {
  let key = g.senders.get(kid);
  if (!key) {
    key = deriveSenderKey(g.mediaKey, kid);
    g.senders.set(kid, key);
  }
  return key;
}

/** One participant's frame keys. Lives in the transform worker's memory only. */
export class FrameCipher {
  #current: Generation | null = null;
  #previous: { generation: Generation; until: number } | null = null;
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

  /** The session's media key and this participant's index (its KID). */
  setKey(mediaKey: Uint8Array, participant: number, now = Date.now()): void {
    if (this.#current) this.#previous = { generation: this.#current, until: now + OLD_KEY_MS };
    this.#current = generation(mediaKey);
    this.#me = BigInt(participant);
  }

  /** The frame to send, sealed; null until there is a key. */
  async sealFrame(data: ArrayBuffer, kind: Kind, type?: string): Promise<ArrayBuffer | null> {
    const g = this.#current;
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
    const generations = [this.#current];
    if (this.#previous && now < this.#previous.until) generations.push(this.#previous.generation);
    for (const g of generations) {
      if (!g) continue;
      const opened = await open((kid) => senderKey(g, kid), body, metadata);
      if (opened) {
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
}
