// SFrame (RFC 9605): each media frame of a meeting sealed end to end, so
// the relay forwards frames it can't open (CF-21, ADR 0030 §6). Cipher
// suite AES_128_GCM_SHA256_128 (0x0004), on WebCrypto. Each sender's key
// is derived from the meeting's media key with the sender's participant
// index as the KID; all participants hold the media key, so this is
// SFrame without signatures, as the ADR says.

export const SUITE = 0x0004;
const NK = 16;
const NN = 12;
const MAX = (1n << 64n) - 1n;

/** One sender's key and salt, from the media key and its KID. */
export interface SenderKey {
  kid: bigint;
  key: CryptoKey;
  salt: Uint8Array;
}

function bigEndian(value: bigint, length: number): Uint8Array {
  const out = new Uint8Array(length);
  for (let i = length - 1; i >= 0; i -= 1) {
    out[i] = Number(value & 0xffn);
    value >>= 8n;
  }
  return out;
}

/** The fewest bytes that hold `value` (at least one). */
function compact(value: bigint): Uint8Array {
  let length = 1;
  while (length < 8 && value >> BigInt(8 * length) !== 0n) length += 1;
  return bigEndian(value, length);
}

function concat(...parts: Uint8Array[]): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

/** The header: the config byte, then KID and CTR when 3 bits don't hold them (§4.3). */
export function encodeHeader(kid: bigint, ctr: bigint): Uint8Array {
  if (kid < 0n || kid > MAX || ctr < 0n || ctr > MAX) throw new RangeError("KID and CTR are 64-bit");
  let config = 0;
  const tail: Uint8Array[] = [];
  if (kid < 8n) {
    config |= Number(kid) << 4;
  } else {
    const bytes = compact(kid);
    config |= 0x80 | ((bytes.length - 1) << 4);
    tail.push(bytes);
  }
  if (ctr < 8n) {
    config |= Number(ctr);
  } else {
    const bytes = compact(ctr);
    config |= 0x08 | (bytes.length - 1);
    tail.push(bytes);
  }
  return concat(Uint8Array.of(config), ...tail);
}

/** The header at the start of `data`: its KID, CTR and length; null if cut short. */
export function decodeHeader(data: Uint8Array): { kid: bigint; ctr: bigint; length: number } | null {
  if (data.length < 1) return null;
  const config = data[0] ?? 0;
  let at = 1;
  const field = (extended: boolean, bits: number): bigint | null => {
    if (!extended) return BigInt(bits);
    const length = bits + 1;
    if (data.length < at + length) return null;
    let value = 0n;
    for (let i = 0; i < length; i += 1) value = (value << 8n) | BigInt(data[at + i] ?? 0);
    at += length;
    return value;
  };
  const kid = field((config & 0x80) !== 0, (config >> 4) & 0x07);
  if (kid === null) return null;
  const ctr = field((config & 0x08) !== 0, config & 0x07);
  if (ctr === null) return null;
  return { kid, ctr, length: at };
}

const encoder = new TextEncoder();

function label(kind: "key" | "salt", kid: bigint): Uint8Array<ArrayBuffer> {
  return concat(encoder.encode(`SFrame 1.0 Secret ${kind} `), bigEndian(kid, 8), bigEndian(BigInt(SUITE), 2));
}

/** The raw key and salt for `kid` (§4.4.2): exported for the RFC's vectors. */
export async function deriveKeySalt(baseKey: Uint8Array, kid: bigint): Promise<{ key: Uint8Array; salt: Uint8Array }> {
  const material = await crypto.subtle.importKey("raw", concat(baseKey), "HKDF", false, ["deriveBits"]);
  const expand = async (info: Uint8Array<ArrayBuffer>, length: number) =>
    new Uint8Array(
      await crypto.subtle.deriveBits({ name: "HKDF", hash: "SHA-256", salt: new Uint8Array(0), info }, material, length * 8),
    );
  return { key: await expand(label("key", kid), NK), salt: await expand(label("salt", kid), NN) };
}

/** The sender key for `kid` under the media key `baseKey`. Not extractable. */
export async function deriveSenderKey(baseKey: Uint8Array, kid: bigint): Promise<SenderKey> {
  const { key, salt } = await deriveKeySalt(baseKey, kid);
  return { kid, salt, key: await crypto.subtle.importKey("raw", concat(key), "AES-GCM", false, ["encrypt", "decrypt"]) };
}

function nonce(salt: Uint8Array, ctr: bigint): Uint8Array<ArrayBuffer> {
  const out = concat(salt);
  const counter = bigEndian(ctr, NN);
  for (let i = 0; i < NN; i += 1) out[i] = (out[i] ?? 0) ^ (counter[i] ?? 0);
  return out;
}

/** header + ciphertext of `plaintext`, with `metadata` authenticated too (§4.4.3). */
export async function seal(sender: SenderKey, ctr: bigint, plaintext: Uint8Array, metadata: Uint8Array): Promise<Uint8Array> {
  const header = encodeHeader(sender.kid, ctr);
  const sealed = await crypto.subtle.encrypt(
    { name: "AES-GCM", iv: nonce(sender.salt, ctr), additionalData: concat(header, metadata), tagLength: 128 },
    sender.key,
    concat(plaintext),
  );
  return concat(header, new Uint8Array(sealed));
}

/**
 * Opens a sealed frame with the key its header names; null if there is no
 * such key or it doesn't open (a wrong key, a changed byte). Callers drop
 * such a frame: it must never reach the decoder.
 */
export async function open(
  keyFor: (kid: bigint) => Promise<SenderKey | undefined> | SenderKey | undefined,
  data: Uint8Array,
  metadata: Uint8Array,
): Promise<Uint8Array | null> {
  const header = decodeHeader(data);
  if (!header) return null;
  const sender = await keyFor(header.kid);
  if (!sender) return null;
  try {
    const opened = await crypto.subtle.decrypt(
      {
        name: "AES-GCM",
        iv: nonce(sender.salt, header.ctr),
        additionalData: concat(data.subarray(0, header.length), metadata),
        tagLength: 128,
      },
      sender.key,
      concat(data.subarray(header.length)),
    );
    return new Uint8Array(opened);
  } catch {
    return null;
  }
}
