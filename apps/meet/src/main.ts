// Meet (CF-21, ADR 0030 §6): a Glasshouse meeting, sealed end to end. The
// page makes one WebRTC connection to the org's relay, through glassd and
// the console (`meet.join`): it never makes a request of its own. Every
// frame is sealed with SFrame in a worker before it leaves, with media
// keys glassd hands over; they stay in memory, never in storage.
//
// Up to 25 people (M2). The offer sends this computer's microphone and
// camera, has a third line for its screen (empty until shared), and one
// line each to receive someone's audio and video. The relay says on the
// `glass` data channel who is in the meeting and which receiving line
// carries whose microphone, camera or screen; when it lists someone
// without a line, the page offers more over the same channel (the
// protocol is in crates/svc/media-relay/src/lib.rs).

import { Bridge, BridgeError, h, Offline, ready, show } from "@glasshouse/sdk";

interface Joined {
  /** The relay's SDP answer. */
  answer: string;
  /** The session's media key, base64. */
  media_key: string;
  /** Its id (`md-…`), to tell it from later ones. */
  media_key_id?: string;
  /** This computer's index in the meeting: its SFrame KID. */
  participant: number;
}

/** `meet.keys`: the session's media keys, current first. */
interface Keys {
  media_keys: { id: string; key: string }[];
}

/** One of this page's receiving lines, as the relay's `peers` line says. */
interface Track {
  mid: string;
  participant: number;
  kind: Kind;
  screen: boolean;
}

/** The relay's lines on the data channel. */
type FromRelay =
  | { type: "peers"; peers: number[]; sharing?: number[]; names?: Record<string, string>; tracks?: Track[] }
  | { type: "answer"; sdp: string }
  | { type: "refused"; error: string };

type Side = "send" | "receive";
type Kind = "audio" | "video";

const bridge = new Bridge({ app: "meet" });
const root = document.getElementById("app") as HTMLElement;
const meeting = decodeURIComponent(location.hash.slice(1));
const worker = new Worker("./worker.js", { type: "module" });

const state = {
  phase: "lobby" as "lobby" | "joining" | "in" | "left" | "failed",
  error: "",
  notice: "",
  mic: true,
  camera: true,
  sharing: false,
  /** This computer's index in the meeting. */
  me: -1,
  peers: [] as number[],
  sharers: [] as number[],
  names: {} as Record<string, string>,
  tracks: [] as Track[],
};
let pc: RTCPeerConnection | null = null;
let channel: RTCDataChannel | null = null;
let media: MediaStream | null = null;
let screen: MediaStream | null = null;
let screenSender: RTCRtpSender | null = null;
/** An offer is out: the next waits for its answer and the `peers` line after. */
let negotiating = false;
/** The relay refused an offer: don't keep asking. */
let stuck = false;
/** Lines from the relay, handled one at a time, in order. */
let inbox: Promise<void> = Promise.resolve();
/** Media key ids this page holds, and their generations (higher is newer). */
const keyIds = new Map<string, number>();
/** The current wait for a new media key; a newer one replaces it. */
let rekeying = 0;

const scriptTransform = "RTCRtpScriptTransform" in globalThis;
const transformed = new WeakSet<RTCRtpSender | RTCRtpReceiver>();

/** Runs a sender's or receiver's frames through the worker, once. */
function attach(endpoint: RTCRtpSender | RTCRtpReceiver, side: Side, kind: Kind): void {
  if (transformed.has(endpoint)) return;
  transformed.add(endpoint);
  if (scriptTransform) {
    const Transform = (globalThis as unknown as { RTCRtpScriptTransform: new (w: Worker, o: unknown) => unknown }).RTCRtpScriptTransform;
    (endpoint as unknown as { transform: unknown }).transform = new Transform(worker, { side, kind });
    return;
  }
  // The engine's older encoded streams: hand them to the worker.
  const { readable, writable } = (endpoint as unknown as { createEncodedStreams(): { readable: ReadableStream; writable: WritableStream } }).createEncodedStreams();
  worker.postMessage({ type: "streams", readable, writable, side, kind }, [readable as unknown as Transferable, writable as unknown as Transferable]);
}

/** Offers only what the relay forwards: Opus, and VP8 with its retransmissions. Keeps offers small for 25 people. */
function preferCodecs(transceiver: RTCRtpTransceiver, kind: Kind): void {
  const wanted = kind === "audio" ? ["audio/opus"] : ["video/vp8", "video/rtx"];
  const codecs = (RTCRtpReceiver.getCapabilities(kind)?.codecs ?? []).filter((c) => wanted.includes(c.mimeType.toLowerCase()));
  try {
    if (codecs.length > 0) transceiver.setCodecPreferences(codecs);
  } catch {
    // The engine offers its defaults; the relay answers VP8 and Opus anyway.
  }
}

/** A transceiver, its codecs chosen and its frames sealed or opened in the worker. */
function line(connection: RTCPeerConnection, what: MediaStreamTrack | Kind, kind: Kind, direction: "sendonly" | "recvonly", streams: MediaStream[] = []): RTCRtpTransceiver {
  const transceiver = connection.addTransceiver(what, { direction, streams });
  preferCodecs(transceiver, kind);
  if (direction === "sendonly") attach(transceiver.sender, "send", kind);
  else attach(transceiver.receiver, "receive", kind);
  return transceiver;
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

/** Hands the worker a media key. The newest generation seals from then on. */
function giveKey(base64: string, generation: number): void {
  const key = bytes(base64);
  worker.postMessage({ type: "key", key: key.buffer, participant: state.me, generation }, [key.buffer]);
}

/**
 * Takes `meet.keys`' answer (current first): each key not held yet goes to
 * the worker. A new current key gets the next generation and seals from
 * now on; older ones not seen before only open frames in flight. Whether
 * the current key is new.
 */
function useKeys(keys: { id: string; key: string }[]): boolean {
  let newer = false;
  for (let i = keys.length - 1; i >= 0; i -= 1) {
    const entry = keys[i];
    if (!entry || keyIds.has(entry.id)) continue;
    const known = [...keyIds.values()];
    const generation = i === 0 ? Math.max(-1, ...known) + 1 : Math.min(0, ...known) - 1;
    keyIds.set(entry.id, generation);
    giveKey(entry.key, generation);
    if (i === 0) newer = true;
  }
  return newer;
}

/**
 * Someone left: a remaining computer makes a new media key (ADR 0030 §6).
 * Asks glassd for it a few times, backing off, until it's there.
 */
async function awaitNewKey(): Promise<void> {
  const mine = (rekeying += 1);
  for (const wait of [250, 500, 1000, 2000, 4000, 8000]) {
    await new Promise((done) => setTimeout(done, wait));
    if (mine !== rekeying || state.phase !== "in") return;
    try {
      const result = await bridge.call<Keys>("meet.keys", { meeting });
      if (result.ok && useKeys(result.data?.media_keys ?? [])) return;
    } catch {
      // Not reachable right now: try again.
    }
  }
}

/** One line to the relay. */
function say(message: object): void {
  if (channel?.readyState === "open") channel.send(`${JSON.stringify(message)}\n`);
}

/** Offers a receiving line for each track the relay lists without one. */
async function offerLines(): Promise<void> {
  const connection = pc;
  if (!connection || negotiating || stuck || connection.signalingState !== "stable") return;
  const has = (p: number, kind: Kind, screen: boolean) => state.tracks.some((t) => t.participant === p && t.kind === kind && t.screen === screen);
  const lacking: Kind[] = [];
  for (const p of state.peers) {
    if (p === state.me) continue;
    if (!has(p, "audio", false)) lacking.push("audio");
    if (!has(p, "video", false)) lacking.push("video");
  }
  for (const p of state.sharers) if (p !== state.me && !has(p, "video", true)) lacking.push("video");
  if (lacking.length === 0) return;
  negotiating = true;
  for (const kind of lacking) line(connection, kind, kind, "recvonly");
  await connection.setLocalDescription(await connection.createOffer());
  say({ type: "offer", sdp: connection.localDescription?.sdp ?? "" });
}

async function handle(message: FromRelay): Promise<void> {
  const connection = pc;
  if (!connection) return;
  if (message.type === "peers") {
    const gone = state.peers.some((p) => p !== state.me && !message.peers.includes(p));
    state.peers = message.peers;
    state.sharers = message.sharing ?? [];
    state.names = message.names ?? {};
    state.tracks = message.tracks ?? [];
    if (gone) void awaitNewKey();
    draw();
    await offerLines();
  } else if (message.type === "answer") {
    await connection.setRemoteDescription({ type: "answer", sdp: message.sdp });
    negotiating = false;
  } else if (message.type === "refused") {
    await connection.setLocalDescription({ type: "rollback" });
    negotiating = false;
    stuck = true;
  }
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
  channel = connection.createDataChannel("glass");
  channel.onmessage = ({ data }) => {
    for (const text of String(data).split("\n")) {
      if (!text.trim()) continue;
      let message: FromRelay;
      try {
        message = JSON.parse(text) as FromRelay;
      } catch {
        continue;
      }
      inbox = inbox.then(() => handle(message)).catch(() => undefined);
    }
  };
  const [audio] = media.getAudioTracks();
  const [video] = media.getVideoTracks();
  // The order is the relay's: microphone, camera, screen, then receiving lines.
  line(connection, audio ?? "audio", "audio", "sendonly", [media]);
  line(connection, video ?? "video", "video", "sendonly", [media]);
  screenSender = line(connection, "video", "video", "sendonly").sender;
  line(connection, "audio", "audio", "recvonly");
  line(connection, "video", "video", "recvonly");
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
  state.me = joined.participant;
  keyIds.set(joined.media_key_id ?? "joined", 0);
  giveKey(joined.media_key, 0);
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
  channel = null;
  screenSender = null;
  for (const track of [...(media?.getTracks() ?? []), ...(screen?.getTracks() ?? [])]) track.stop();
  media = null;
  screen = null;
  state.sharing = false;
  rekeying += 1;
  videos.clear();
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

/** Shares this computer's screen on the line kept for it, or stops. */
async function share(): Promise<void> {
  if (state.sharing) return unshare();
  const sender = screenSender;
  if (!sender) return;
  let display: MediaStream;
  try {
    display = await navigator.mediaDevices.getDisplayMedia({ video: { frameRate: { max: 15 } }, audio: false });
  } catch {
    state.notice = "Your screen wasn't shared.";
    return draw();
  }
  const [track] = display.getVideoTracks();
  if (!track || screenSender !== sender) {
    for (const t of display.getTracks()) t.stop();
    return;
  }
  track.contentHint = "detail";
  // The engine's own "Stop sharing" ends the track.
  track.addEventListener("ended", () => void unshare());
  await sender.replaceTrack(track);
  screen = display;
  state.sharing = true;
  state.notice = "";
  say({ type: "screen", on: true });
  draw();
}

async function unshare(): Promise<void> {
  if (!state.sharing) return;
  state.sharing = false;
  say({ type: "screen", on: false });
  await screenSender?.replaceTrack(null);
  for (const track of screen?.getTracks() ?? []) track.stop();
  screen = null;
  draw();
}

/** The track a receiving line carries, by its mid. */
function received(mid: string): MediaStreamTrack | undefined {
  return pc?.getTransceivers().find((t) => t.mid === mid)?.receiver.track;
}

function nameOf(participant: number): string {
  return state.names[String(participant)] ?? `Participant ${participant + 1}`;
}

/** Video elements kept across draws, so playback doesn't restart. */
const videos = new Map<string, HTMLVideoElement>();

function videoOf(key: string, tracks: MediaStreamTrack[], muted: boolean, test: string, used: Set<string>): HTMLVideoElement {
  used.add(key);
  const ids = tracks.map((t) => t.id).join(" ");
  let video = videos.get(key);
  if (!video || video.dataset.tracks !== ids) {
    video = h("video", { autoplay: true, playsinline: true, "data-test": test, "data-tracks": ids });
    video.muted = muted;
    video.srcObject = new MediaStream(tracks);
    videos.set(key, video);
  }
  return video;
}

/** One person: their camera and microphone, with their name. */
function tile(participant: number, used: Set<string>): HTMLElement {
  const of = (kind: Kind) => state.tracks.find((t) => t.participant === participant && t.kind === kind && !t.screen);
  const tracks = [of("audio"), of("video")]
    .map((t) => (t ? received(t.mid) : undefined))
    .filter((t): t is MediaStreamTrack => t !== undefined);
  const hasVideo = of("video") !== undefined;
  return h(
    "div",
    { class: "tile", "data-test": "tile", "data-participant": String(participant) },
    tracks.length > 0 ? videoOf(`p${participant}`, tracks, false, "other", used) : null,
    hasVideo ? null : h("span", { class: "joining" }, "Joining…"),
    h("span", { class: "who" }, nameOf(participant)),
  );
}

function selfTile(used: Set<string>): HTMLElement {
  return h(
    "div",
    { class: "tile self" },
    media ? videoOf("self", media.getVideoTracks(), true, "self", used) : null,
    h("span", { class: "who" }, state.sharing ? "You · sharing your screen" : "You"),
  );
}

function draw(): void {
  const others = state.peers.filter((p) => p !== state.me);
  const head = h(
    "header",
    { class: "head" },
    h("h1", {}, "Meeting"),
    h("span", { class: "sealed", title: "Each frame is sealed on this computer; the relay can't open it, and nothing is recorded." }, "🔒 Sealed end to end"),
    state.phase === "in" ? h("span", { class: "count", "data-test": "count" }, others.length + 1 === 1 ? "Just you" : `${others.length + 1} people`) : null,
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
  const used = new Set<string>();
  const shown = state.tracks.find((t) => t.screen && t.participant !== state.me);
  const screenTrack = shown ? received(shown.mid) : undefined;
  const tiles = [...others.map((p) => tile(p, used)), selfTile(used)];
  let stage: HTMLElement;
  if (shown && screenTrack) {
    stage = h(
      "main",
      { class: "stage sharing" },
      h(
        "div",
        { class: "screen" },
        videoOf(`s${shown.participant}`, [screenTrack], true, "screen", used),
        h("span", { class: "who" }, `${nameOf(shown.participant)}'s screen`),
      ),
      h("div", { class: "strip" }, ...tiles),
    );
  } else {
    const grid = h(
      "div",
      { class: "grid" },
      ...(others.length === 0 ? [h("div", { class: "tile waiting", "data-test": "waiting" }, "Waiting for others to join")] : []),
      ...tiles,
    );
    const count = Math.max(2, tiles.length);
    const cols = Math.ceil(Math.sqrt(count));
    grid.style.setProperty("--cols", String(cols));
    grid.style.setProperty("--rows", String(Math.ceil(count / cols)));
    stage = h("main", { class: "stage" }, grid);
  }
  for (const key of [...videos.keys()]) if (!used.has(key)) videos.delete(key);
  show(
    root,
    head,
    stage,
    state.notice ? h("p", { class: "notice", "data-test": "notice" }, state.notice) : null,
    h(
      "footer",
      { class: "controls" },
      h("button", { class: state.mic ? "" : "off", "data-test": "mic", onclick: () => toggle("audio") }, state.mic ? "Mute" : "Unmute"),
      h("button", { class: state.camera ? "" : "off", "data-test": "camera", onclick: () => toggle("video") }, state.camera ? "Stop video" : "Start video"),
      h(
        "button",
        { class: state.sharing ? "on" : "", "data-test": "share", disabled: !screenSender, onclick: () => void share() },
        state.sharing ? "Stop sharing" : "Share screen",
      ),
      h("button", { class: "danger", "data-test": "leave", onclick: () => void leave() }, "Leave"),
    ),
  );
}

draw();
void bridge.open().finally(ready);
