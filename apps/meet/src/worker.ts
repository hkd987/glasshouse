// The meeting's frame transform (CF-21, ADR 0030 §6), in a worker: seals
// each frame this computer sends and opens each one it gets. The page
// hands it the media key by message; the key lives in this worker's
// memory only, never in storage.

import { FrameCipher, type Kind } from "./frames.ts";

type Side = "send" | "receive";

interface EncodedFrame {
  data: ArrayBuffer;
  type?: string;
}

interface TransformOptions {
  side: Side;
  kind: Kind;
}

const cipher = new FrameCipher();

function pipe(readable: ReadableStream<EncodedFrame>, writable: WritableStream<EncodedFrame>, { side, kind }: TransformOptions): void {
  const transform = new TransformStream<EncodedFrame, EncodedFrame>({
    async transform(frame, controller) {
      const data =
        side === "send"
          ? await cipher.sealFrame(frame.data, kind, frame.type)
          : await cipher.openFrame(frame.data, kind, frame.type);
      // No key yet, or not this meeting's: drop it, never pass it on.
      if (!data) return;
      frame.data = data;
      controller.enqueue(frame);
    },
  });
  void readable.pipeThrough(transform).pipeTo(writable);
}

interface Scope {
  onmessage: ((event: MessageEvent) => void) | null;
  onrtctransform?: ((event: { transformer: { readable: ReadableStream<EncodedFrame>; writable: WritableStream<EncodedFrame>; options: TransformOptions } }) => void) | null;
  postMessage(message: unknown): void;
}

const scope = self as unknown as Scope;

scope.onmessage = ({ data }) => {
  if (data?.type === "key") {
    cipher.setKey(new Uint8Array(data.key as ArrayBuffer), Number(data.participant));
  } else if (data?.type === "streams") {
    // The engine's older encoded streams, handed over by the page.
    pipe(data.readable, data.writable, { side: data.side, kind: data.kind });
  } else if (data?.type === "counts") {
    scope.postMessage({ type: "counts", opened: cipher.opened, dropped: cipher.dropped });
  }
};

// RTCRtpScriptTransform: the engine runs each sender's and receiver's
// frames through here.
scope.onrtctransform = ({ transformer }) => pipe(transformer.readable, transformer.writable, transformer.options);
