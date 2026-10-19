// SFrame (RFC 9605) as meetings seal frames (CF-21, ADR 0030 §6): the
// RFC's own vectors for the header and for cipher suite 0x0004, and a
// frame sealed for one key that no other key opens.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { decodeHeader, deriveKeySalt, deriveSenderKey, encodeHeader, open, seal } from "../src/sframe.ts";

const hex = (s: string) => Uint8Array.from(s.match(/../g) ?? [], (b) => parseInt(b, 16));
const toHex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

test("cf21 sframe headers encode and decode as RFC 9605 appendix C.1 says, all 289 cases", () => {
  const vectors = JSON.parse(readFileSync(new URL("./sframe-headers.json", import.meta.url), "utf8")) as [string, string, string][];
  assert.equal(vectors.length, 289);
  for (const [kid, ctr, header] of vectors) {
    const k = BigInt(`0x${kid}`);
    const c = BigInt(`0x${ctr}`);
    assert.equal(toHex(encodeHeader(k, c)), header, `kid ${kid} ctr ${ctr}`);
    assert.deepEqual(decodeHeader(hex(header)), { kid: k, ctr: c, length: header.length / 2 });
  }
  assert.equal(decodeHeader(hex("99")), null, "cut short");
});

test("cf21 sframe seals as RFC 9605 appendix C.3 says for AES_128_GCM_SHA256_128", async () => {
  const baseKey = hex("000102030405060708090a0b0c0d0e0f");
  const kid = 0x123n;
  const ctr = 0x4567n;
  const { key, salt } = await deriveKeySalt(baseKey, kid);
  assert.equal(toHex(key), "d34f547f4ca4f9a7447006fe7fcbf768");
  assert.equal(toHex(salt), "75234edefe07819026751816");

  const metadata = hex("4945544620534672616d65205747");
  const plaintext = hex("64726166742d696574662d736672616d652d656e63");
  const sender = await deriveSenderKey(baseKey, kid);
  const sealed = await seal(sender, ctr, plaintext, metadata);
  assert.equal(toHex(sealed), "9901234567b7412c2513a1b66dbb48841bbaf17f598751176ad847681a69c6d0b091c07018ce4adb34eb");
  assert.deepEqual(await open(() => sender, sealed, metadata), plaintext);
});

test("cf21 a frame opens only with its meeting's key, its sender's KID and its metadata", async () => {
  const mediaKey = crypto.getRandomValues(new Uint8Array(16));
  const alice = await deriveSenderKey(mediaKey, 0n);
  const frame = new TextEncoder().encode("a video frame");
  const prefix = Uint8Array.of(0x10, 0x02, 0x00);
  const sealed = await seal(alice, 42n, frame, prefix);

  const keys = new Map([[0n, alice]]);
  assert.deepEqual(await open((kid) => keys.get(kid), sealed, prefix), frame);
  // Someone without the meeting's key: same KID, another media key.
  const outsider = await deriveSenderKey(crypto.getRandomValues(new Uint8Array(16)), 0n);
  assert.equal(await open(() => outsider, sealed, prefix), null);
  // No key for that sender at all.
  assert.equal(await open(() => undefined, sealed, prefix), null);
  // The relay can't change the bytes the packetizer reads, nor the frame.
  assert.equal(await open((kid) => keys.get(kid), sealed, Uint8Array.of(0x11, 0x02, 0x00)), null);
  const tampered = sealed.slice();
  tampered[tampered.length - 1] = (tampered[tampered.length - 1] ?? 0) ^ 1;
  assert.equal(await open((kid) => keys.get(kid), tampered, prefix), null);
});
