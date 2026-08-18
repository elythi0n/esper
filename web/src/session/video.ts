/**
 * The remote screen, decoded in the page and drawn on a canvas.
 *
 * # Why the page decodes
 *
 * Because it can, and because the alternative costs 129 crates. The webview's
 * own `VideoDecoder` was measured on this product's encoder before any of this
 * was built: 1920×1080 at 151 frames a second with every frame drawn, on
 * software rendering with no GPU — five times what a 30 fps session needs.
 * What the engine sends is the same encoded picture it would have sent a
 * WebRTC peer, repackaged into the one form a webview accepts.
 *
 * # Why a canvas and not a `<video>`
 *
 * A `<video>` wants a container and a clock, and this stream is neither: it is
 * pictures as they arrive, and the only right time to show one is now. A
 * canvas also gives the session what a remote desktop needs and a media
 * element fights — no buffering, no smoothing, no catch-up after a stall.
 */
import { readViewerStream, type Configuration } from "./stream";

/** What the surface knows about itself, for whatever wants to say it. */
export interface ViewerState {
  /** Nothing yet, a picture, or a reason there is not one. */
  status: "waiting" | "playing" | "ended" | "failed";
  /** Pictures drawn since the session opened. */
  pictures: number;
  /** The size the engine says it is sending. */
  size: { width: number; height: number } | null;
  /** What the codec turned out to be, which the HUD shows. */
  codec: string | null;
  /** Why it is not playing, in a sentence somebody can act on. */
  trouble: string | null;
}

export const IDLE: ViewerState = {
  status: "waiting",
  pictures: 0,
  size: null,
  codec: null,
  trouble: null,
};

/**
 * Why there is no picture, in a sentence a person can act on.
 *
 * **The body is used only when it is one.** The window's own refusals are
 * written as sentences and are worth showing — "no session is sending
 * pictures to this window" is the whole answer. What must not reach a screen
 * is a *server's* refusal: a page opened in a browser tab asks a dev server
 * for a route it does not have and gets back an HTML error document, and
 * pasting that under "No picture" would be this product doing the thing its
 * own content rules forbid.
 */
async function refusal(response: Response): Promise<string> {
  if (response.status === 404 || response.status === 501) {
    return "A session's pictures go to the Esper window. A browser tab has no socket for them.";
  }
  const said = (await response.text().catch(() => "")).trim();
  if (said && said.length <= 200 && !said.includes("<")) return said;
  return `This window could not start the picture (${response.status}).`;
}

/**
 * Draw a session's pictures onto a canvas until it ends.
 *
 * Returns a function that stops it. Everything it owns — the decoder, the
 * fetch, the frames — is closed there, because a session that is torn down
 * while a decoder still holds frames is a leak the browser reports as memory
 * rather than as an error.
 */
export function showTheScreen(
  canvas: HTMLCanvasElement,
  { url = "/shell/video", onState }: { url?: string; onState?: (s: ViewerState) => void } = {},
): () => void {
  const ctx = canvas.getContext("2d", { alpha: false });
  const abort = new AbortController();
  const held: { decoder: VideoDecoder | null } = { decoder: null };
  let state: ViewerState = { ...IDLE };

  const say = (next: Partial<ViewerState>) => {
    state = { ...state, ...next };
    onState?.(state);
  };

  const configure = (record: Configuration) => {
    // A size change is a new decoder, not a resized one: the parameter sets
    // changed, and a decoder configured for the old ones is a decoder that
    // will produce nothing and say nothing about why.
    held.decoder?.close();
    canvas.width = record.width;
    canvas.height = record.height;
    held.decoder = new VideoDecoder({
      output: (frame) => {
        ctx?.drawImage(frame, 0, 0);
        frame.close();
        say({ status: "playing", pictures: state.pictures + 1 });
      },
      error: (e) => say({ status: "failed", trouble: `${e.name}: ${e.message}` }),
    });
    held.decoder.configure({
      codec: record.codec,
      description: record.description,
      // A viewer wants the picture that just arrived, not a smooth one.
      optimizeForLatency: true,
    });
    say({ size: { width: record.width, height: record.height }, codec: record.codec });
  };

  (async () => {
    try {
      const response = await fetch(url, { signal: abort.signal });
      if (!response.ok || !response.body) {
        say({ status: "failed", trouble: await refusal(response) });
        return;
      }
      for await (const record of readViewerStream(response.body)) {
        if (record.kind === "configuration") {
          configure(record);
          continue;
        }
        held.decoder?.decode(
          new EncodedVideoChunk({
            type: record.key ? "key" : "delta",
            timestamp: record.timestamp,
            data: record.data,
          }),
        );
      }
      say({ status: state.pictures > 0 ? "ended" : "failed", trouble: state.pictures > 0 ? null : "the session sent no pictures" });
    } catch (e) {
      if (abort.signal.aborted) return;
      say({ status: "failed", trouble: e instanceof Error ? e.message : String(e) });
    } finally {
      try {
        held.decoder?.close();
      } catch {
        // Already closed, which is the ordinary way this ends.
      }
    }
  })();

  return () => {
    abort.abort();
    try {
      held.decoder?.close();
    } catch {
      // As above: closing twice is not an error worth a line.
    }
  };
}
