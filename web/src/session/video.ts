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

/** What the picture path is doing, which the notice over the picture renders. */
export type LinkState = "live" | "reconnecting" | "lost";

/** What the surface knows about itself, for whatever wants to say it. */
export interface ViewerState {
  /** Nothing yet, a picture, or a reason there is not one. */
  status: "waiting" | "playing" | "failed";
  /**
   * The state of the path the pictures come down.
   *
   * Separate from `status` because they answer different questions: `status`
   * is about the picture on the canvas, which stays there when the link goes,
   * and this is about whether anything is still feeding it. A window that
   * conflated them would either throw away the last true thing it knows about
   * that machine or show it as though it were current.
   */
  link: LinkState;
  /** How long the pictures have been gone, in milliseconds, or null. */
  awayMs: number | null;
  /** Pictures drawn since the session opened. */
  pictures: number;
  /** The size the engine says it is sending. */
  size: { width: number; height: number } | null;
  /** What the codec turned out to be, which the HUD shows. */
  codec: string | null;
  /** Why it is not playing, in a sentence somebody can act on. */
  trouble: string | null;
  /**
   * Pictures a second, over the last second.
   *
   * Counted rather than derived from the stream's timestamps: what a person
   * sees is how often this canvas changed, and a stream can carry thirty
   * timestamps a second into a window that drew five of them.
   */
  fps: number;
  /**
   * The hop this path adds, in milliseconds — engine to drawn.
   *
   * `null` until a picture arrives with a clock on it. It is always a local
   * number even when the session is not: the clock is written by *this*
   * machine's engine as it hands the picture over, so the far end's clock
   * never enters it. What it does not include is everything before that —
   * the remote capture, the encode, the link — which belong to the session
   * rather than to the window.
   */
  hopMs: number | null;
  /** The worst of the last hundred, because a mean hides a stall. */
  worstHopMs: number | null;
}

export const IDLE: ViewerState = {
  status: "waiting",
  link: "live",
  awayMs: null,
  pictures: 0,
  size: null,
  codec: null,
  trouble: null,
  fps: 0,
  hopMs: null,
  worstHopMs: null,
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

/** The first pause before trying the picture again, in milliseconds. */
const FIRST_WAIT_MS = 300;
/** The longest pause between tries. Doubling stops here and stays. */
const LONGEST_WAIT_MS = 5_000;
/**
 * How long the pictures may be gone before the window stops calling it a
 * reconnect and calls it lost. It goes on trying either way — what changes is
 * what it is honest to say.
 */
const LOST_AFTER_MS = 20_000;

/**
 * Draw a session's pictures onto a canvas, and keep drawing them.
 *
 * Returns a function that stops it. Everything it owns — the decoder, the
 * fetch, the frames — is closed there, because a session that is torn down
 * while a decoder still holds frames is a leak the browser reports as memory
 * rather than as an error.
 *
 * # Why it does not stop when the pictures do
 *
 * Because a window that freezes is the worst thing a remote desktop can do:
 * the last picture goes on looking like that machine, and there is no way to
 * tell a screen that stopped changing from a link that stopped carrying. So
 * the stream ending is a *state* rather than the end — the last picture stays,
 * the window says the link is interrupted and counts, and it goes on asking
 * for the picture until one arrives.
 *
 * The one thing it does not retry is a window that has no socket at all: a
 * browser tab asking a dev server for this route gets a 404, and trying that
 * again every second for ever would be a page spinning on something that
 * cannot come.
 */
export function showTheScreen(
  canvas: HTMLCanvasElement,
  { url = "/shell/video", onState }: { url?: string; onState?: (s: ViewerState) => void } = {},
): () => void {
  const ctx = canvas.getContext("2d", { alpha: false });
  const abort = new AbortController();
  const held: { decoder: VideoDecoder | null } = { decoder: null };
  let state: ViewerState = { ...IDLE };
  let skipped = false;
  /** When the pictures stopped, and how long to wait before asking again. */
  const lost: { at: number | null; waitMs: number } = { at: null, waitMs: FIRST_WAIT_MS };

  const say = (next: Partial<ViewerState>) => {
    state = { ...state, ...next };
    onState?.(state);
  };

  /*
    How long each of the last hundred pictures took to arrive and be drawn,
    and how many arrived in the last second.

    A hundred is about three seconds at thirty frames, which is long enough
    to show a stall and short enough that a stall three minutes ago is not
    still being reported as the worst thing that ever happened.
  */
  const hops: number[] = [];
  const drawnAt: number[] = [];
  /*
    When each picture was handed over, by its own timestamp.

    A decoder is allowed to hold pictures and hand them back in order, so the
    frame coming out is not always the chunk that went in. The timestamp is
    the only thing that travels through it, which makes it the only honest way
    to pair a drawn picture with when it was sent.
  */
  const handedFor = new Map<number, number>();

  const recent = (now: number) => {
    drawnAt.push(now);
    while (drawnAt.length > 0 && (drawnAt[0] ?? 0) < now - 1000) drawnAt.shift();
    return drawnAt.length;
  };

  const configure = (record: Configuration) => {
    // A size change is a new decoder, not a resized one: the parameter sets
    // changed, and a decoder configured for the old ones is a decoder that
    // will produce nothing and say nothing about why.
    held.decoder?.close();
    // Setting either clears the canvas, so it is set only when it is not
    // already that — a reconnect at the same size keeps the last picture
    // underneath the notice rather than blanking to black.
    if (canvas.width !== record.width) canvas.width = record.width;
    if (canvas.height !== record.height) canvas.height = record.height;
    held.decoder = new VideoDecoder({
      output: (frame) => {
        ctx?.drawImage(frame, 0, 0);
        frame.close();
        const now = Date.now();
        // **Measured where the picture is drawn, not where it is decoded.**
        // A decoder that is keeping up and a canvas that is not are the same
        // session to everybody except the person watching it.
        const handed = handedFor.get(frame.timestamp);
        handedFor.delete(frame.timestamp);
        let hop = state.hopMs;
        let worst = state.worstHopMs;
        if (handed !== undefined) {
          hop = Math.max(0, now - handed);
          hops.push(hop);
          if (hops.length > 100) hops.shift();
          worst = Math.max(...hops);
        }
        // A picture is the only proof the link is back, so it is the only
        // thing that clears the count and the backoff.
        lost.at = null;
        lost.waitMs = FIRST_WAIT_MS;
        say({
          status: "playing",
          link: "live",
          awayMs: null,
          trouble: null,
          pictures: state.pictures + 1,
          fps: recent(now),
          hopMs: hop,
          worstHopMs: worst,
        });
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

  /*
    One go at the picture: ask for it, read it until it stops.

    What it returns is what to do next — `"again"` for anything that could be a
    link, `"never"` for a window that has no socket to be pointed at. A
    boolean would have lost exactly that distinction.
  */
  const once = async (): Promise<"again" | "never"> => {
    try {
      const response = await fetch(url, { signal: abort.signal });
      if (!response.ok || !response.body) {
        const why = await refusal(response);
        // 404 and 501 are this window saying it has no socket for pictures at
        // all — a browser tab, or a build without the route. Nothing arrives
        // by waiting, so this is the one refusal that ends it.
        const never = response.status === 404 || response.status === 501;
        say({ status: "failed", trouble: why, ...(never ? { link: "live" as const, awayMs: null } : {}) });
        return never ? "never" : "again";
      }
      for await (const record of readViewerStream(response.body)) {
        if (record.kind === "configuration") {
          configure(record);
          continue;
        }
        if (record.kind !== "picture") {
          // A record from a later engine. The stream carries lengths so this
          // window can walk past it; saying so once is better than a silence
          // somebody has to bisect for.
          if (!skipped) {
            skipped = true;
            console.info(`the session sent a record this window does not know (${record.record})`);
          }
          continue;
        }
        if (record.handedAt !== null) {
          handedFor.set(record.timestamp, record.handedAt);
          // A decoder that dropped pictures would leave these behind for ever.
          if (handedFor.size > 240) {
            const oldest = handedFor.keys().next().value;
            if (oldest !== undefined) handedFor.delete(oldest);
          }
        }
        held.decoder?.decode(
          new EncodedVideoChunk({
            type: record.key ? "key" : "delta",
            timestamp: record.timestamp,
            data: record.data,
          }),
        );
      }
      return "again";
    } catch (e) {
      if (abort.signal.aborted) return "never";
      // A stream that broke mid-picture is a link, not a fault to report and
      // stop on: the trouble is kept for a window that never had one.
      if (state.pictures === 0) say({ status: "failed", trouble: e instanceof Error ? e.message : String(e) });
      return "again";
    } finally {
      try {
        held.decoder?.close();
      } catch {
        // Already closed, which is the ordinary way this ends.
      }
      held.decoder = null;
    }
  };

  (async () => {
    while (!abort.signal.aborted) {
      const next = await once();
      if (next === "never" || abort.signal.aborted) return;

      // The link is gone from here until a picture proves otherwise — and
      // `lost` is cleared where that proof arrives, in the decoder's output,
      // rather than after a try. Resetting it per attempt would start the
      // count again every few seconds, so a link that never came back would
      // report a few seconds gone for ever.
      lost.at ??= Date.now();
      const lostAt = lost.at;
      const waitMs = lost.waitMs;
      /*
        Wait, saying how long — and go on saying it while waiting.

        The count is what makes this visible rather than merely automatic: a
        notice that appeared and then sat still is indistinguishable from a
        window that has stopped, which is the thing this exists to avoid.
      */
      const until = Date.now() + waitMs;
      do {
        const away = Date.now() - lostAt;
        say({ link: away >= LOST_AFTER_MS ? "lost" : "reconnecting", awayMs: away });
        await new Promise((r) => setTimeout(r, Math.min(250, Math.max(0, until - Date.now()))));
      } while (Date.now() < until && !abort.signal.aborted);
      lost.waitMs = Math.min(waitMs * 2, LONGEST_WAIT_MS);
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
