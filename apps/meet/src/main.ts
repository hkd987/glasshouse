// Meet (CF-21, ADR 0030 §6): a Glasshouse meeting, sealed end to end. The
// page makes one WebRTC connection to the org's relay, through glassd and
// the console (`meet.join`): it never makes a request of its own. Every
// frame is sealed with SFrame in a worker before it leaves, with a media
// key glassd hands over once; the key stays in memory, never in storage.
// M1 is two people: the offer sends this computer's camera and microphone
// and has one slot to receive the other person's.

import { Bridge, BridgeError, h, Offline, ready, show } from "@glasshouse/sdk";

interface Joined {
  /** The relay's SDP answer. */
  answer: string;
  /** The session's media key, base64. */
  media_key: string;
  /** This computer's index in the meeting: its SFrame KID. */
  participant: number;
}

type Side = "send" | "receive";
type Kind = "audio" | "video";

const bridge = new Bridge({ app: "meet" });
const root = document.getElementById("app") as HTMLElement;
const meeting = decodeURIComponent(location.hash.slice(1));
const worker = new Worker("./worker.js", { type: "module" });

const state = {
  phase: "lobby" as "lobby" | "joining" | "in" | "left" | "failed",
  error: "",
  others: 0,
  mic: true,
  camera: true,
};
let pc: RTCPeerConnection | null = null;
let media: MediaStream | null = null;
const remote = new MediaStream();

const scriptTransform = "RTCRtpScriptTransform" in globalThis;

/** Runs a sender's or receiver's frames through the worker. */
function attach(endpoint: RTCRtpSender | RTCRtpReceiver, side: Side, kind: Kind): void {
  if (scriptTransform) {
    const Transform = (globalThis as unknown as { RTCRtpScriptTransform: new (w: Worker, o: unknown) => unknown }).RTCRtpScriptTransform;
    (endpoint as unknown as { transform: unknown }).transform = new Transform(worker, { side, kind });
    return;
  }
  // The engine's older encoded streams: hand them to the worker.
  const { readable, writable } = (endpoint as unknown as { createEncodedStreams(): { readable: ReadableStream; writable: WritableStream } }).createEncodedStreams();
  worker.postMessage({ type: "streams", readable, writable, side, kind }, [readable as unknown as Transferable, writable as unknown as Transferable]);
}

/** Waits until the offer carries this computer's candidates (bounded). */
function gathered(connection: RTCPeerConnection): Promise<void> {
  if (connection.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((done) => {
    const timer = setTimeout(done, 1000);
    connection.addEventListener("icegatheringstatechange", () => {
      if (connection.iceGatheringState === "complete") {
        clearTimeout(timer);
        done();
      }
    });
  });
}

function bytes(base64: string): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
}

async function join(): Promise<void> {
  state.phase = "joining";
  draw();
  try {
    media = await navigator.mediaDevices.getUserMedia({ audio: true, video: { width: 640, height: 360 } });
  } catch {
    return fail("The camera and microphone can't be used. Your organization may have turned them off.");
  }
  const connection = new RTCPeerConnection({
    bundlePolicy: "max-bundle",
    ...(scriptTransform ? {} : { encodedInsertableStreams: true }),
  } as RTCConfiguration);
  pc = connection;
  const channel = connection.createDataChannel("glass");
  channel.onmessage = ({ data }) => {
    for (const line of String(data).split("\n")) {
      if (!line.trim()) continue;
      const message = JSON.parse(line) as { type?: string; peers?: number[] };
      if (message.type === "peers") {
        state.others = Math.max(0, (message.peers?.length ?? 1) - 1);
        draw();
      }
    }
  };
  const [audio] = media.getAudioTracks();
  const [video] = media.getVideoTracks();
  const sends: [RTCRtpTransceiver, Kind][] = [
    [connection.addTransceiver(audio ?? "audio", { direction: "sendonly", streams: [media] }), "audio"],
    [connection.addTransceiver(video ?? "video", { direction: "sendonly", streams: [media] }), "video"],
  ];
  const receives: [RTCRtpTransceiver, Kind][] = [
    [connection.addTransceiver("audio", { direction: "recvonly" }), "audio"],
    [connection.addTransceiver("video", { direction: "recvonly" }), "video"],
  ];
  for (const [t, kind] of sends) attach(t.sender, "send", kind);
  for (const [t, kind] of receives) {
    attach(t.receiver, "receive", kind);
    remote.addTrack(t.receiver.track);
  }
  await connection.setLocalDescription(await connection.createOffer());
  await gathered(connection);

  let joined: Joined;
  try {
    const result = await bridge.call<Joined>("meet.join", { meeting, offer: connection.localDescription?.sdp ?? "" });
    if (!result.ok) return fail(result.text);
    joined = result.data;
  } catch (error) {
    return fail(error instanceof Offline ? "The meeting can't be reached right now." : error instanceof BridgeError ? error.message : String(error));
  }
  // The key goes to the worker before any frame can: until then the
  // worker drops what it is given, so nothing leaves unsealed.
  const key = bytes(joined.media_key);
  worker.postMessage({ type: "key", key: key.buffer, participant: joined.participant }, [key.buffer]);
  await connection.setRemoteDescription({ type: "answer", sdp: joined.answer });
  state.phase = "in";
  draw();
}

function fail(message: string): void {
  state.phase = "failed";
  state.error = message;
  stop();
  draw();
}

function stop(): void {
  pc?.close();
  pc = null;
  for (const track of media?.getTracks() ?? []) track.stop();
  media = null;
}

async function leave(): Promise<void> {
  stop();
  state.phase = "left";
  draw();
  try {
    await bridge.call("meet.leave", { meeting });
  } catch {
    // Leaving locally is what matters; the console ends the seat on its own.
  }
}

function toggle(kind: Kind): void {
  const on = kind === "audio" ? (state.mic = !state.mic) : (state.camera = !state.camera);
  for (const track of kind === "audio" ? (media?.getAudioTracks() ?? []) : (media?.getVideoTracks() ?? [])) track.enabled = on;
  draw();
}

function tile(stream: MediaStream | null, name: string, self: boolean): HTMLElement {
  const video = h("video", { autoplay: true, playsinline: true, "data-test": self ? "self" : "other" });
  video.muted = self;
  video.srcObject = stream;
  return h("div", { class: `tile${self ? " self" : ""}` }, video, h("span", { class: "who" }, name));
}

function draw(): void {
  const head = h(
    "header",
    { class: "head" },
    h("h1", {}, "Meeting"),
    h("span", { class: "sealed", title: "Each frame is sealed on this computer; the relay can't open it, and nothing is recorded." }, "🔒 Sealed end to end"),
  );
  if (state.phase === "lobby" || state.phase === "joining") {
    show(
      root,
      head,
      h(
        "main",
        { class: "lobby" },
        h("p", {}, "Your camera and microphone turn on when you join."),
        h("button", { class: "primary", "data-test": "join", disabled: state.phase === "joining", onclick: () => void join() }, state.phase === "joining" ? "Joining…" : "Join"),
      ),
    );
    return;
  }
  if (state.phase === "failed" || state.phase === "left") {
    show(
      root,
      head,
      h("main", { class: "lobby" }, h("p", { "data-test": "message" }, state.phase === "left" ? "You left the meeting." : state.error)),
    );
    return;
  }
  show(
    root,
    head,
    h(
      "main",
      { class: "stage" },
      state.others > 0 ? tile(remote, "In the meeting", false) : h("div", { class: "tile waiting", "data-test": "waiting" }, "Waiting for others to join"),
      tile(media, "You", true),
    ),
    h(
      "footer",
      { class: "controls" },
      h("button", { class: state.mic ? "" : "off", "data-test": "mic", onclick: () => toggle("audio") }, state.mic ? "Mute" : "Unmute"),
      h("button", { class: state.camera ? "" : "off", "data-test": "camera", onclick: () => toggle("video") }, state.camera ? "Stop video" : "Start video"),
      h("button", { class: "danger", "data-test": "leave", onclick: () => void leave() }, "Leave"),
    ),
  );
}

draw();
void bridge.open().finally(ready);
