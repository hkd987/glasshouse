// A meeting's frames (CF-21, ADR 0030 §6): VP8's header stays clear for
// the packetizer, nothing is sent before there is a key, a frame no key
// of the meeting opens is dropped, a newer media key takes over sending,
// and the old key opens frames in flight for two seconds after it.

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
  alice.setKey(0, key());
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
  alice.setKey(1, mediaKey);
  const bob = new FrameCipher();
  bob.setKey(2, mediaKey);
  const outsider = new FrameCipher();
  outsider.setKey(3, key());

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
  alice.setKey(0, first, 0, 0);
  const bob = new FrameCipher();
  bob.setKey(1, first, 0, 0);
  const inFlight = (await alice.sealFrame(frame(40), "audio")) as ArrayBuffer;
  bob.setKey(1, key(), 1, 1000);
  assert.ok(await bob.openFrame(inFlight, "audio", undefined, 1000 + OLD_KEY_MS - 1));
  assert.equal(await bob.openFrame(inFlight, "audio", undefined, 1000 + OLD_KEY_MS), null);
  assert.deepEqual(bob.generations(1000 + OLD_KEY_MS), [1], "the old key is forgotten");
});

test("cf21 someone who left cannot decrypt after the new key", async () => {
  // Three people share the first key; Carol leaves, and Alice and Bob get
  // a second one she never has.
  const first = key();
  const [alice, bob, carol] = [new FrameCipher(), new FrameCipher(), new FrameCipher()];
  alice.setKey(0, first, 0, 0);
  bob.setKey(1, first, 0, 0);
  carol.setKey(2, first, 0, 0);
  const before = (await alice.sealFrame(frame(64), "video", "delta")) as ArrayBuffer;
  assert.ok(await carol.openFrame(before, "video", "delta", 0));

  const second = key();
  alice.setKey(0, second, 1, 100);
  bob.setKey(1, second, 1, 100);
  const after = (await alice.sealFrame(frame(64), "video", "delta")) as ArrayBuffer;
  assert.ok(await bob.openFrame(after, "video", "delta", 200), "Bob opens Alice's frames under the new key");
  assert.equal(await carol.openFrame(after, "video", "delta", 200), null, "Carol, with the old key only, opens nothing");
  // Bob's frames under the new key open for Alice; frames Bob sealed
  // just before he switched still open for a moment.
  const bobsLast = await (async () => {
    const b = new FrameCipher();
    b.setKey(1, first, 0, 0);
    return (await b.sealFrame(frame(32), "audio")) as ArrayBuffer;
  })();
  assert.ok(await alice.openFrame(bobsLast, "audio", undefined, 100 + OLD_KEY_MS - 1));
  assert.equal(await alice.openFrame(bobsLast, "audio", undefined, 100 + OLD_KEY_MS), null);
});

test("cf21 a key arriving late doesn't take over sending, and the same key again changes nothing", async () => {
  const [older, newer] = [key(), key()];
  const alice = new FrameCipher();
  alice.setKey(0, newer, 5, 0);
  alice.setKey(0, older, 4, 0);
  alice.setKey(0, newer, 5, 10_000);
  const bob = new FrameCipher();
  bob.setKey(1, newer, 5, 0);
  const sealed = (await alice.sealFrame(frame(32), "audio")) as ArrayBuffer;
  assert.ok(await bob.openFrame(sealed, "audio", undefined, 10_000), "Alice still seals with generation 5");
  assert.deepEqual(alice.generations(1), [5, 4]);
  assert.deepEqual(alice.generations(OLD_KEY_MS), [5]);
});
