import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "./client";
import type { Health } from "./types";

/**
 * Whether this machine has an engine, asked over and over.
 *
 * # Why this polls
 *
 * An engine that crashes is the case worth showing, and it happens after the
 * page has loaded — so a single question at start-up cannot find it. The
 * event stream is not the place either: `/events` carries what *sessions*
 * did, and hanging a machine-level condition on it would make the stream two
 * things.
 *
 * Five seconds, which is cheap — the request is loopback, and the answer is
 * two fields — and fast enough that somebody who saw the engine die in a
 * terminal does not beat the window to it.
 */
const EVERY = 5000;

export interface EngineCondition {
  /** Null until the first answer, so a screen can avoid claiming anything yet. */
  health: Health | null;
  /** True while a restart is in flight, for the button that asked for it. */
  restarting: boolean;
  /** What the restart itself said, when it failed. */
  refused: string | null;
  restart: () => void;
}

export function useEngine(): EngineCondition {
  const [health, setHealth] = useState<Health | null>(null);
  const [restarting, setRestarting] = useState(false);
  const [refused, setRefused] = useState<string | null>(null);
  // Kept in a ref so the poll can be cancelled from the restart without the
  // effect below being torn down and started again on every render.
  const live = useRef(true);

  const ask = useCallback(async () => {
    try {
      const answer = await api.health();
      if (live.current) setHealth(answer);
    } catch {
      // The control plane itself is unreachable, which the first-run screen
      // owns. Nothing to say about an engine behind a plane that is not there.
      if (live.current) setHealth(null);
    }
  }, []);

  useEffect(() => {
    live.current = true;
    void ask();
    const id = setInterval(() => void ask(), EVERY);
    return () => {
      live.current = false;
      clearInterval(id);
    };
  }, [ask]);

  const restart = useCallback(() => {
    setRestarting(true);
    setRefused(null);
    api
      .restartEngine()
      .then(() => ask())
      .catch((trouble: unknown) => {
        if (live.current) setRefused(trouble instanceof Error ? trouble.message : "The restart was refused.");
      })
      .finally(() => {
        if (live.current) setRestarting(false);
      });
  }, [ask]);

  return { health, restarting, refused, restart };
}
