/**
 * Reading the stream the engine sends a window's decoder.
 *
 * The format is written down once, in `esper-media`'s `viewer_stream` module,
 * and there is a fixture both halves read — `testdata/fixtures/viewer_stream.bin`
 * — for the same reason the wire fixtures exist: two implementations of one
 * format drift silently, and the drift shows up as a session that shows
 * nothing rather than as an error anybody can read.
 *
 * # What this does not do
 *
 * It does not parse H.264. Every record carries its own length, so this is a
 * loop over lengths — no start codes, no NAL types, no bitstream. The one
 * place that understands the codec is the engine, in Rust, where it is tested
 * against a decoder rather than against a webview.
 */

/** The first eight bytes, and what a wrong answer on the socket looks like. */
const MAGIC = "ESPRVID\0";

/** The version this reader knows. A stream that says anything else is refused. */
export const STREAM_VERSION = 2;

const CONFIGURATION = 1;
const PICTURE = 2;
const SESSION = 3;
const POINTER = 4;
const POINTER_SHAPE = 5;
const FLAG_KEYFRAME = 0b0000_0001;
const FLAG_PRIMARY = 0b0000_0001;
const FLAG_IN_PICTURE = 0b0000_0001;
const FLAG_DRAW_POINTER = 0b0000_0010;
const FLAG_HAS_IMAGE = 0b0000_0001;

/** Configure the decoder: a new size, codec or parameter set. */
export interface Configuration {
  kind: "configuration";
  width: number;
  height: number;
  /** As `VideoDecoder.configure` takes it, e.g. `avc1.4D4028`. */
  codec: string;
  /** The `avcC` record, which is what WebKit will not decode without. */
  description: Uint8Array;
}

/** One access unit, ready for an `EncodedVideoChunk`. */
export interface Picture {
  kind: "picture";
  /** Whether a decoder can begin here. */
  key: boolean;
  /** Microseconds since the session opened, which is what WebCodecs wants. */
  timestamp: number;
  /**
   * The engine's wall clock, in milliseconds, when this went down the socket.
   *
   * The one number that makes the hop this path adds measurable: subtract it
   * from `Date.now()` when the picture is drawn and what is left is engine to
   * window to glass — one machine, therefore one clock, even when the session
   * itself spans two. `null` when the writer could not read a clock, which is
   * a thing to report rather than a zero to do arithmetic with.
   */
  handedAt: number | null;
  data: Uint8Array;
}

/** One of the host's screens, as a window is told about it. */
export interface Screen {
  /** The host's id for it. Opaque: it is echoed back and never interpreted. */
  id: number;
  /** What the host calls it, which is what a person picks from. */
  name: string;
  /** Where it sits in the host's desktop. Can be negative. */
  x: number;
  y: number;
  width: number;
  height: number;
  /** Whether the host calls this its primary display. */
  primary: boolean;
}

/**
 * What the session is of, and what else it could be of.
 *
 * The host's list of screens reaches the window here and nowhere else, so a
 * switcher built on anything else would be a second idea of what is plugged
 * in to the far machine.
 */
export interface SessionRecord {
  kind: "session";
  /** 0 balanced, 1 text, 2 motion — the viewer stream's own numbering. */
  profile: number;
  /** 0 nothing stated, 1 sharp, 2 balanced, 3 soft. */
  sharpness: number;
  /** The ids of the screens the picture is of. */
  showing: number[];
  /** Every screen the host has. */
  screens: Screen[];
}

/**
 * Where the host's pointer is, and whether this window has to draw it.
 *
 * `draw` is the engine's answer rather than its evidence. A remote pointer is
 * best *worn* — the browser draws it at the person's own hand, with no network
 * in the path — and that is honest only while the local pointer is the host's
 * pointer. Which it is turns on whether this session is driving and whether
 * the host is following, and working that out again here would be a second
 * implementation of a rule that took a finding to get right.
 */
export interface Pointer {
  kind: "pointer";
  /** Whether it is anywhere in this picture at all. */
  inPicture: boolean;
  /** Whether to draw it, rather than wear it. */
  draw: boolean;
  x: number;
  y: number;
}

/** What the host's pointer looks like. `image` is null when it is hidden. */
export interface PointerShape {
  kind: "pointer-shape";
  image: { width: number; height: number; hotX: number; hotY: number; rgba: Uint8Array } | null;
}

/** A record from a version that knows more than this reader does. */
export interface Unknown {
  kind: "unknown";
  /** What it said it was, for a line in the log and nothing else. */
  record: number;
}

export type Record = Configuration | Picture | SessionRecord | Pointer | PointerShape | Unknown;

export class StreamError extends Error {}

/**
 * A byte stream in, records out, as they arrive.
 *
 * # Why it holds a buffer
 *
 * Because a socket delivers bytes, not records: a picture can arrive in four
 * pieces, and two can arrive in one. Anything short of a whole record is kept
 * and re-examined when more arrives — which is the only way a reader of a live
 * stream can be written, and the reason this is not a `map` over chunks.
 *
 * The buffer is bounded by one record. A record that claims a preposterous
 * length is refused rather than accumulated: the stream is loopback and
 * trusted, but a reader that will allocate whatever it is told is a reader
 * that turns a bug at the far end into this window running out of memory.
 */
export async function* readViewerStream(
  body: ReadableStream<Uint8Array>,
  { mostBytes = 64 * 1024 * 1024 }: { mostBytes?: number } = {},
): AsyncGenerator<Record> {
  const reader = body.getReader();
  let buffer = new Uint8Array(0);
  let begun = false;

  const more = async (): Promise<boolean> => {
    const { done, value } = await reader.read();
    if (done || !value) return false;
    const next = new Uint8Array(buffer.length + value.length);
    next.set(buffer);
    next.set(value, buffer.length);
    buffer = next;
    return true;
  };

  /** Wait until at least `n` bytes are in hand, or the stream ended. */
  const have = async (n: number): Promise<boolean> => {
    if (n > mostBytes) throw new StreamError(`a record claiming ${n} bytes, which is past any sane picture`);
    while (buffer.length < n) {
      if (!(await more())) return false;
    }
    return true;
  };

  const take = (n: number): Uint8Array => {
    const out = buffer.subarray(0, n);
    buffer = buffer.subarray(n);
    return out;
  };
  const view = (bytes: Uint8Array) => new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  try {
    while (true) {
      if (!begun) {
        if (!(await have(9))) return;
        const head = take(9);
        const magic = new TextDecoder("latin1").decode(head.subarray(0, 8));
        if (magic !== MAGIC) {
          throw new StreamError("this is not a video stream — its first bytes name something else");
        }
        if (head[8] !== STREAM_VERSION) {
          throw new StreamError(`a stream at version ${head[8]}, and this window reads ${STREAM_VERSION}`);
        }
        begun = true;
        continue;
      }

      // Kind, then the body's length. Reading the whole body before parsing
      // it is what lets an unknown kind be walked past — and what keeps a
      // half-arrived record from being mistaken for a malformed one.
      if (!(await have(5))) return;
      const header = take(5);
      const kind = header[0] ?? 0;
      const length = view(header.subarray(1)).getUint32(0);
      if (!(await have(length))) return;
      const body = take(length);

      if (kind === CONFIGURATION) {
        const fixed = view(body);
        const width = fixed.getUint32(0);
        const height = fixed.getUint32(4);
        const codecLength = fixed.getUint16(8);
        const codec = new TextDecoder().decode(body.subarray(10, 10 + codecLength));
        const at = 10 + codecLength;
        const descriptionLength = fixed.getUint32(at);
        // Copied, not a view: the buffer it came from is reused, and a
        // description handed to `configure` has to outlive this turn.
        const description = new Uint8Array(body.subarray(at + 4, at + 4 + descriptionLength));
        yield { kind: "configuration", width, height, codec, description };
        continue;
      }

      if (kind === PICTURE) {
        const fixed = view(body);
        const flags = fixed.getUint8(0);
        // Microseconds, and a session runs past 2^53 of them only after 285
        // years, so a Number is the honest type here.
        const timestamp = Number(fixed.getBigUint64(1));
        const handed = Number(fixed.getBigUint64(9));
        const bytes = fixed.getUint32(17);
        yield {
          kind: "picture",
          key: (flags & FLAG_KEYFRAME) !== 0,
          timestamp,
          // Milliseconds, because that is what `Date.now()` is measured in
          // and comparing the two is the whole purpose of carrying it.
          handedAt: handed > 0 ? handed / 1000 : null,
          data: new Uint8Array(body.subarray(21, 21 + bytes)),
        };
        continue;
      }

      if (kind === SESSION) {
        const fixed = view(body);
        const profile = fixed.getUint8(0);
        const sharpness = fixed.getUint8(1);
        const showingCount = fixed.getUint8(2);
        const showing: number[] = [];
        for (let n = 0; n < showingCount; n++) showing.push(fixed.getUint32(3 + n * 4));
        let at = 3 + showingCount * 4;
        const screenCount = fixed.getUint8(at);
        at += 1;
        const screens: Screen[] = [];
        for (let n = 0; n < screenCount; n++) {
          const id = fixed.getUint32(at);
          const x = fixed.getInt32(at + 4);
          const y = fixed.getInt32(at + 8);
          const width = fixed.getUint32(at + 12);
          const height = fixed.getUint32(at + 16);
          const primary = (fixed.getUint8(at + 20) & FLAG_PRIMARY) !== 0;
          const nameLength = fixed.getUint16(at + 21);
          const name = new TextDecoder().decode(body.subarray(at + 23, at + 23 + nameLength));
          screens.push({ id, name, x, y, width, height, primary });
          at += 23 + nameLength;
        }
        yield { kind: "session", profile, sharpness, showing, screens };
        continue;
      }

      if (kind === POINTER) {
        const fixed = view(body);
        const flags = fixed.getUint8(0);
        yield {
          kind: "pointer",
          inPicture: (flags & FLAG_IN_PICTURE) !== 0,
          draw: (flags & FLAG_DRAW_POINTER) !== 0,
          x: fixed.getInt32(1),
          y: fixed.getInt32(5),
        };
        continue;
      }

      if (kind === POINTER_SHAPE) {
        const fixed = view(body);
        if ((fixed.getUint8(0) & FLAG_HAS_IMAGE) === 0) {
          yield { kind: "pointer-shape", image: null };
          continue;
        }
        const hotX = fixed.getUint16(1);
        const hotY = fixed.getUint16(3);
        const width = fixed.getUint16(5);
        const height = fixed.getUint16(7);
        const bytes = fixed.getUint32(9);
        yield {
          kind: "pointer-shape",
          // Copied, not a view: the buffer it came from is reused, and the
          // pixels outlive this turn — they become an image the surface wears.
          image: { width, height, hotX, hotY, rgba: new Uint8Array(body.subarray(13, 13 + bytes)) },
        };
        continue;
      }

      // A record from a later version. Walked past rather than refused —
      // which is what the length is for, and the difference between a format
      // that can grow and one that cannot.
      yield { kind: "unknown", record: kind };
    }
  } finally {
    reader.releaseLock();
  }
}
