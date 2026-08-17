import { useEffect, useState } from "react";
import { api } from "@/api/client";
import { Button } from "@/components/button";
import { Wordmark } from "@/shell/AppShell";
import { Key } from "./parts";

/**
 * The screen for a window with no control plane behind it.
 *
 * The window starts its own, so this is rarer than it looks — which makes it
 * all the more important that what it says is true. There are two ways to
 * have no control plane and they need different sentences:
 *
 *   - **Nothing answered.** Ours failed to start. `esper api` is the thing
 *     that fixes it, and it is on screen because it is the whole remedy.
 *   - **Something answered and refused us.** Another Esper holds the
 *     address with another home's token. Telling somebody to run `esper api`
 *     here would be advice that cannot work: one is already running.
 *
 * It retries on its own either way. Somebody who fixes it in a terminal
 * should not then have to come back here and press something.
 */
export function FirstRun({
  refused = false,
  at,
  watch = true,
}: {
  refused?: boolean;
  at?: string | undefined;
  /**
   * Whether to keep asking, and to reload the window when something answers.
   *
   * On by default, because retrying is most of what this screen is for. Off
   * for a **specimen**: the design gate renders this screen live beside the
   * others, and a specimen that reloads the window every two seconds is a
   * gallery nobody can review — which is exactly what happened the moment
   * the gate was given a control plane that answers.
   */
  watch?: boolean;
}) {
  const [tries, setTries] = useState(0);

  useEffect(() => {
    if (!watch) return;
    let live = true;
    const id = setInterval(() => {
      api
        .health()
        .then(() => {
          // Reached. The window reloads into the product rather than
          // unpicking whatever each screen cached while nothing answered.
          if (live) window.location.reload();
        })
        .catch(() => {
          if (live) setTries((n) => n + 1);
        });
    }, 2000);
    return () => {
      live = false;
      clearInterval(id);
    };
  }, [watch]);

  return (
    <main className="flex h-full flex-col items-center justify-center gap-10 bg-surface-0 px-8">
      <Wordmark className="h-3.5 text-text-2" />

      <div className="flex max-w-md flex-col items-center gap-4">
        <p className="text-center text-base text-text-2">
          {refused
            ? `Another Esper is answering${at ? ` on ${at}` : ""} and it does not take this window's token.`
            : "Nothing is answering on this machine."}
        </p>
        {refused ? (
          <p className="text-center text-sm text-text-3">
            It was started from a different <code className="font-mono">ESPER_HOME</code>. Stop it, or open this window
            with the same one.
          </p>
        ) : (
          <code className="rounded-sm border border-border bg-surface-1 px-4 py-2 font-mono text-lg text-text">
            esper api
          </code>
        )}
      </div>

      <div className="flex flex-col items-center gap-3">
        <Key>{tries === 0 ? "Looking" : `Looking · ${tries}`}</Key>
        <Button size="sm" disabled={!watch} onClick={() => watch && window.location.reload()}>
          Try now
        </Button>
      </div>
    </main>
  );
}
