// A meeting's frames (CF-21, ADR 0030 §6): VP8's header stays clear for
// the packetizer, nothing is sent before there is a key, a frame no key
// of the meeting opens is dropped, and the old key opens frames in flight
// for two seconds after a new one.

import assert from "node:assert/strict";
import { test } from "node:test";

import { decodeHeader } from "../../sdk/src/sframe.ts";
import { clearBytes, FrameCipher, OLD_KEY_MS } from "../src/frames.ts";

const frame = (n: number) => Uint8Array.from({ length: n }, (_, i) => i % 251).buffer;
const key = () => crypto.getRandomValues(new Uint8Array(16));

test("cf21 the packetizer's bytes stay clear: 10 on a VP8 key frame, 3 on others, none for audio", async () => {
  assert.equal(clearBytes("video", "key"), 10);
  assert.equal(clearBytes("video", "delta"), 3);
  assert.equal(clearBytes("audio", undefined), 0);

  const alice = new FrameCipher();
  alice.setKey(key(), 0);
  const plain = new Uint8Array(frame(200));
  const sealed = new Uint8Array((await alice.sealFrame(plain.buffer.slice(0), "video", "key")) as ArrayBuffer);
  assert.deepEqual(sealed.subarray(0, 10), plain.subarray(0, 10));
  assert.notDeepEqual(sealed.subarray(10, 30), plain.subarray(10, 30));
  const header = decodeHeader(sealed.subarray(10));
  assert.equal(header?.kid, 0n);
  assert.equal(sealed.length, 200 + (header?.length ?? 0) + 16, "the clear bytes, the header, the sealed rest and its tag");
});

test("cf21 nothing leaves before the key, and only the meeting's key opens a frame", async () => {
  const mediaKey = key();
  const alice = new FrameCipher();
  assert.equal(await alice.sealFrame(frame(50), "audio"), null, "no key yet: dropped, never sent clear");
  alice.setKey(mediaKey, 1);
  const bob = new FrameCipher();
  bob.setKey(mediaKey, 2);
  const outsider = new FrameCipher();
  outsider.setKey(key(), 3);

  const sealed = (await alice.sealFrame(frame(120), "video", "delta")) as ArrayBuffer;
  const opened = await bob.openFrame(sealed, "video", "delta");
  assert.deepEqual(new Uint8Array(opened as ArrayBuffer), new Uint8Array(frame(120)));
  assert.equal(await outsider.openFrame(sealed, "video", "delta"), null);
  assert.equal(outsider.dropped, 1);
  assert.equal(bob.opened, 1);
});

test("cf21 after a new key the old one opens frames in flight for two seconds", async () => {
  const first = key();
  const alice = new FrameCipher();
  alice.setKey(first, 0, 0);
  const bob = new FrameCipher();
  bob.setKey(first, 1, 0);
  const inFlight = (await alice.sealFrame(frame(40), "audio")) as ArrayBuffer;
  bob.setKey(key(), 1, 1000);
  assert.ok(await bob.openFrame(inFlight, "audio", undefined, 1000 + OLD_KEY_MS - 1));
  assert.equal(await bob.openFrame(inFlight, "audio", undefined, 1000 + OLD_KEY_MS), null);
});
