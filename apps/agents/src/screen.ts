// Its computer's screen, a terminal read a piece at a time: each read
// (`GET …/screen?since=<offset>`) brings the text from `since` to its
// `offset`, in bytes. The buffer appends only what it hasn't shown, says
// when output was skipped or the screen started over, and turns the
// terminal's control sequences into plain text. No DOM: unit tested.

/** What the page keeps of a long-running screen, in characters. */
export const KEEP = 200_000;

export interface Read {
  /** Where this read ends: the next one starts here. */
  offset: number;
  text: string;
}

export type Applied = "nothing" | "appended" | "gap" | "reset";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

export class ScreenBuffer {
  /** Bytes read so far: the next read's `since`. */
  offset = 0;
  /** What the screen shows, as plain text. */
  text = "";
  /** The end of a read that may continue in the next one: half an escape
   * sequence, or a `\r` that may be half of `\r\n`. */
  private carry = "";
  private readonly keep: number;

  constructor(keep = KEEP) {
    this.keep = keep;
  }

  /** Takes a read, or an answer to typed input that carries the screen.
   * A read that overlaps what's shown appends only what's new; one that
   * starts past it marks the skipped part; one that ends before it means
   * the screen started over. */
  apply(read: Read): Applied {
    const bytes = encoder.encode(read.text);
    const start = read.offset - bytes.length;
    let result: Applied;
    let fresh: string;
    if (read.offset < this.offset) {
      this.text = "";
      this.carry = "";
      fresh = read.text;
      result = "reset";
    } else if (read.offset === this.offset) {
      return "nothing";
    } else if (start > this.offset) {
      if (this.text && !this.text.endsWith("\n")) this.text += "\n";
      this.text += "…\n";
      this.carry = "";
      fresh = read.text;
      result = "gap";
    } else {
      fresh = start === this.offset ? read.text : decoder.decode(bytes.subarray(this.offset - start));
      result = "appended";
    }
    this.offset = read.offset;
    this.write(fresh);
    return result;
  }

  /** Starts over, as when the page opens another computer. */
  clear(): void {
    this.offset = 0;
    this.text = "";
    this.carry = "";
  }

  private write(raw: string): void {
    const { text, carry } = plain(this.carry + raw);
    this.carry = carry;
    let out = this.text;
    for (const part of text.split(/(\r|\x08)/)) {
      if (part === "\r") {
        // Back to the line's start: what follows writes over it.
        out = out.slice(0, out.lastIndexOf("\n") + 1);
      } else if (part === "\x08") {
        if (out && !out.endsWith("\n")) out = out.slice(0, -1);
      } else {
        out += part;
      }
    }
    if (out.length > this.keep) {
      const cut = out.indexOf("\n", out.length - this.keep);
      out = out.slice(cut < 0 ? out.length - this.keep : cut + 1);
    }
    this.text = out;
  }
}

/** Plain text from a terminal's output: escape sequences (colors, cursor
 * moves, titles) out, `\r\n` as `\n`, other control characters out but
 * `\t`, `\n`, a lone `\r` and backspace, which `write` acts on. What may
 * continue in the next read comes back as `carry`. */
export function plain(raw: string): { text: string; carry: string } {
  let carry = "";
  let text = raw;
  // An escape sequence cut off at the end waits for the rest.
  const cut = /\x1b(\[[0-?]*[ -/]*|\][^\x07\x1b]*\x1b?|[ -/]*)?$/.exec(text);
  if (cut) {
    // One that never ends (a broken program) is dropped, not kept.
    carry = cut[0].length <= 256 ? cut[0] : "";
    text = text.slice(0, cut.index);
  } else if (text.endsWith("\r")) {
    carry = "\r";
    text = text.slice(0, -1);
  }
  text = text
    .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "") // CSI: colors, cursor
    .replace(/\x1b\][^\x07\x1b]*(\x07|\x1b\\)/g, "") // OSC: window titles
    .replace(/\x1b[ -/]*[0-~]/g, "") // the rest
    .replace(/\r\n/g, "\n")
    .replace(/[\x00-\x07\x0b\x0c\x0e-\x1f\x7f]/g, "");
  return { text, carry };
}
