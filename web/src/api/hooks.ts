import { useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError, onHappening } from "./client";
import type { Device, Happening, Reachable, Seen, Status } from "./types";

/*
  Reading the control plane from React.

  Three states, always, and never collapsed into two: loading, an answer, or
  a failure that carries its own sentence. A hook that returned an empty list
  for "could not ask" would put an empty state on screen where a fault is, and
  an empty device list is the one that invites somebody to pair everything
  again.
*/

export interface Asked<T> {
  data: T | null;
  error: Error | null;
  loading: boolean;
  reload: () => void;
}

export function useAsk<T>(ask: () => Promise<T>, deps: readonly unknown[] = []): Asked<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const [loading, setLoading] = useState(true);
  const [nonce, setNonce] = useState(0);
  const askRef = useRef(ask);
  askRef.current = ask;

  useEffect(() => {
    let current = true;
    setLoading(true);
    askRef
      .current()
      .then((answer) => {
        if (!current) return;
        setData(answer);
        setError(null);
      })
      .catch((cause: unknown) => {
        if (!current) return;
        setError(cause instanceof Error ? cause : new Error(String(cause)));
      })
      .finally(() => {
        if (current) setLoading(false);
      });
    return () => {
      current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nonce, ...deps]);

  const reload = useCallback(() => setNonce((n) => n + 1), []);
  return { data, error, loading, reload };
}

/**
 * The devices this machine trusts, and a live nudge when one of them does
 * something.
 *
 * The store answers the list; the event stream says when to ask again. The
 * stream carries the happening itself, but a happening is not a device — so
 * rather than patching a device from an event, this re-reads the list, which
 * is the file both the engine and the API read. One source, and no way for
 * the screen to hold a device the store does not.
 */
export function useDevices(): Asked<Device[]> & { live: Happening | null } {
  const asked = useAsk(() => api.devices().then((a) => a.devices));
  const [live, setLive] = useState<Happening | null>(null);
  const { reload } = asked;

  useEffect(
    () =>
      onHappening((h) => {
        setLive(h);
        reload();
      }),
    [reload],
  );

  return { ...asked, live };
}

export function useStatus(): Asked<Status> {
  return useAsk(() => api.status());
}

export function useSessionLog(most = 50): Asked<Happening[]> & { live: Happening[] } {
  const asked = useAsk(() => api.sessions(most), [most]);
  const [live, setLive] = useState<Happening[]>([]);

  useEffect(() => onHappening((h) => setLive((all) => [h, ...all].slice(0, most))), [most]);

  return { ...asked, live };
}

/** Whether nothing answered at all, as opposed to answering no. */
export const isUnreachable = (e: Error | null): boolean => e instanceof ApiError && e.unreachable;

/**
 * Which paired devices are on this network.
 *
 * Slowly, and deliberately: the answer costs two seconds of listening on the
 * engine's side, because one mDNS query lost to a busy segment would
 * otherwise read as an empty network. So this asks every half minute and
 * lets the screen show what it last knew — a stale "on this network" is a
 * better answer than a flickering one, and the row that matters is checked
 * again the moment somebody connects.
 */
export function useReachable(): { seen: Record<string, Seen>; looked: string | null } {
  const [answer, setAnswer] = useState<Reachable | null>(null);

  useEffect(() => {
    let live = true;
    const ask = () => {
      api
        .reachable()
        .then((now) => live && setAnswer(now))
        .catch(() => {
          // No engine, or discovery refused. The screen shows nothing rather
          // than claiming every device is unreachable, which is a different
          // and much stronger claim than "we did not look".
        });
    };
    ask();
    const id = setInterval(ask, 30000);
    return () => {
      live = false;
      clearInterval(id);
    };
  }, []);

  return { seen: answer?.seen ?? {}, looked: answer?.looked ?? null };
}
