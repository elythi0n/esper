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
export const STREAM_VERSION = 1;

const CONFIGURATION = 1;
const PICTURE = 2;
const FLAG_KEYFRAME = 0b0000_0001;

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
  data: Uint8Array;
}

export type Record = Configuration | Picture;

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

      if (!(await have(1))) return;
      const kind = take(1)[0];

      if (kind === CONFIGURATION) {
        if (!(await have(10))) return;
        const fixed = view(take(10));
        const width = fixed.getUint32(0);
        const height = fixed.getUint32(4);
        const codecLength = fixed.getUint16(8);
        if (!(await have(codecLength))) return;
        const codec = new TextDecoder().decode(take(codecLength));
        if (!(await have(4))) return;
        const descriptionLength = view(take(4)).getUint32(0);
        if (!(await have(descriptionLength))) return;
        // Copied, not a view: the buffer it came from is reused, and a
        // description handed to `configure` has to outlive this turn.
        yield { kind: "configuration", width, height, codec, description: new Uint8Array(take(descriptionLength)) };
        continue;
      }

      if (kind === PICTURE) {
        if (!(await have(13))) return;
        const fixed = view(take(13));
        const flags = fixed.getUint8(0);
        // Microseconds, and a session runs far past 2^53 microseconds only
        // after 285 years, so a Number is the honest type here.
        const timestamp = Number(fixed.getBigUint64(1));
        const length = fixed.getUint32(9);
        if (!(await have(length))) return;
        yield { kind: "picture", key: (flags & FLAG_KEYFRAME) !== 0, timestamp, data: new Uint8Array(take(length)) };
        continue;
      }

      throw new StreamError(`a record kind this window does not know: ${kind}`);
    }
  } finally {
    reader.releaseLock();
  }
}
