/**
 * Theme and motion preference — two attributes on `<html>`, and the one place
 * that sets them.
 *
 * The same keys are read by the inline script in index.html before the bundle
 * loads, so first paint is already right and nothing here undoes what the
 * document set. This module is for changing them afterwards, from /design and
 * from settings.
 */
import { useCallback, useEffect, useRef, useState } from "react";

/**
 * What somebody may choose, which now includes handing the decision over.
 *
 * `system` is the one that was missing: a machine set to a light desktop, or
 * one asking for more contrast, got a dark window regardless — a whole
 * accessibility setting the window could not hear.
 */
export type Theme = ThemeChoice;
export type Motion = "system" | "reduced" | "full";

export const THEMES: ReadonlyArray<{ id: Theme; label: string; about: string }> = [
  {
    id: "auto",
    label: "Auto",
    about:
      "The default: high contrast if the desktop asked for it, and dark otherwise. It deliberately does **not** follow `prefers-color-scheme` — that query reports light on every machine whose owner expressed no preference, so following it would turn the product light for almost everybody who never asked, which is mistaking a default for a decision. Contrast is different: somebody who set it asked their operating system for it everywhere.",
  },
  { id: "dark", label: "Dark", about: "The primary theme, and the session's theme whatever is chosen here." },
  { id: "light", label: "Light", about: "Ink on bone. Surfaces separate upward." },
  { id: "hc", label: "High contrast", about: "AAA text, a 3px focus ring, strong borders everywhere, no tints." },
];

export { THEME_KEY, MOTION_KEY } from "./first-paint";
import { MOTION_KEY, THEME_KEY, resolveTheme, type ThemeApplied, type ThemeChoice } from "./first-paint";

function read<T extends string>(key: string, allowed: readonly T[], fallback: T): T {
  try {
    const v = localStorage.getItem(key);
    return allowed.includes(v as T) ? (v as T) : fallback;
  } catch {
    return fallback;
  }
}

function write(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Storage may be unavailable. The attribute on <html> still applies for
    // this page; it just does not survive a reload.
  }
}

/**
 * What was chosen, which is not necessarily what is applied.
 *
 * `system` hands the decision to the desktop, and the toggle has to show
 * `system` as selected rather than showing whichever theme that resolved to —
 * or a person who chose "follow my desktop" would find the control claiming
 * they chose "light".
 */
export function currentTheme(): Theme {
  return read(THEME_KEY, ["auto", "dark", "light", "hc"], "auto");
}

/**
 * What is on the element right now.
 *
 * Read from the attribute rather than recomputed, because the inline script
 * has already resolved it — and re-deriving it here would be the second copy
 * of the rule that `first-paint.ts` exists to prevent.
 */
export function appliedTheme(): ThemeApplied {
  const on = document.documentElement.dataset.theme;
  return on === "light" || on === "hc" ? on : "dark";
}

export function currentMotion(): Motion {
  return read(MOTION_KEY, ["system", "reduced", "full"], "system");
}

/**
 * Choose a theme.
 *
 * The choice is stored and the *resolution* goes on the element, through the
 * one implementation of that rule — so choosing `system` on a desktop asking
 * for more contrast lands on high contrast here exactly as it would on the
 * next launch.
 */
export function applyTheme(theme: Theme) {
  write(THEME_KEY, theme);
  document.documentElement.dataset.theme = resolveTheme(theme);
}

/**
 * Keep following the desktop while `auto` is chosen.
 *
 * Two reasons, and the second is why this is not optional. Somebody who turns
 * high contrast on should not have to relaunch — that is the moment they most
 * want it to have worked. And **WebKitGTK does not always know the preference
 * when the document starts parsing**: measured, the inline script saw no
 * preference and the page two frames later saw `prefers-contrast: more`. So
 * the answer can arrive after first paint, and something has to be listening
 * for it.
 */
export function watchTheDesktop(): () => void {
  const query = matchMedia("(prefers-contrast: more)");
  const again = () => {
    if (currentTheme() === "auto") {
      document.documentElement.dataset.theme = resolveTheme("auto");
    }
  };
  // Once now, because the query may already have flipped between the inline
  // script and this running.
  again();
  query.addEventListener("change", again);
  return () => query.removeEventListener("change", again);
}

/**
 * `system` removes the attribute so the OS setting decides through
 * `prefers-reduced-motion`. `reduced` and `full` override it either way — and
 * the override is what /design exists for: every animation can be re-run
 * with motion off, and the fallback confirmed rather than assumed.
 */
export function applyMotion(motion: Motion) {
  if (motion === "system") delete document.documentElement.dataset.motion;
  else document.documentElement.dataset.motion = motion;
  write(MOTION_KEY, motion);
}

/** Whether motion is effectively off right now, from either way in. */
export function motionIsReduced(): boolean {
  const attr = document.documentElement.dataset.motion;
  if (attr === "reduced") return true;
  if (attr === "full") return false;
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/**
 * A duration token, in milliseconds, as the document currently resolves it.
 *
 * For timers that must agree with the CSS: the session chrome's hide delay,
 * a toast's dwell. Read from the computed style rather than imported from
 * motion.ts, because a reduced-motion override changes the CSS value and a
 * timer that used the generated constant would fire on the old one.
 */
export function durationMs(token: `--${string}`, element: Element = document.documentElement): number {
  const raw = getComputedStyle(element).getPropertyValue(token).trim();
  if (raw.endsWith("ms")) return Number.parseFloat(raw);
  if (raw.endsWith("s")) return Number.parseFloat(raw) * 1000;
  return 0;
}

/**
 * A length token, in pixels, as the document currently resolves it.
 *
 * For the same reason `durationMs` exists: arithmetic that has to agree with
 * the CSS. `LogView` computes which rows to draw from the row height, and a
 * component holding its own `28` is a component that drifts from
 * `--row-h` the first time somebody makes the tables denser — silently, and
 * as a scroll that lands in the wrong place rather than as a wrong-looking
 * row.
 *
 * Zero when the token is not set, which a caller must treat as "use my
 * default" rather than as a height.
 */
export function pixelsOf(token: `--${string}`, element: Element = document.documentElement): number {
  const raw = getComputedStyle(element).getPropertyValue(token).trim();
  if (raw.endsWith("px")) return Number.parseFloat(raw);
  // A token given in any other unit is a token this cannot use, and
  // guessing at a conversion would be worse than saying so.
  return 0;
}

/**
 * The theme, and a way to change it.
 *
 * # Why mounting must not write
 *
 * It did, and the bug was quiet and complete: the effect ran `applyTheme` on
 * mount, `applyTheme` stores the value, and so **opening the Settings screen
 * recorded a choice nobody had made.** From then on the stored theme was an
 * explicit `dark`, `auto` never applied again, and a desktop asking for high
 * contrast was ignored for ever — by a person who had done nothing but look
 * at a screen.
 *
 * Found by measuring what the shipped window reported: `theme dark
 * [more=true stored=dark]`, on a machine whose desktop was asking for
 * contrast, in a home that had never been touched.
 *
 * So a change writes and a mount does not. The mount still *applies*, because
 * a control that shows one thing while the document does another is worse
 * than either.
 */
export function useTheme(): [Theme, (t: Theme) => void] {
  const [theme, set] = useState<Theme>(currentTheme);
  const chosen = useRef(false);
  useEffect(() => {
    if (chosen.current) applyTheme(theme);
    else document.documentElement.dataset.theme = resolveTheme(theme);
  }, [theme]);
  const choose = useCallback((next: Theme) => {
    chosen.current = true;
    set(next);
  }, []);
  return [theme, choose];
}

/** The same, and for the same reason: mounting a toggle is not a decision. */
export function useMotion(): [Motion, (m: Motion) => void] {
  const [motion, set] = useState<Motion>(currentMotion);
  const chosen = useRef(false);
  useEffect(() => {
    if (chosen.current) applyMotion(motion);
    else if (motion === "system") delete document.documentElement.dataset.motion;
    else document.documentElement.dataset.motion = motion;
  }, [motion]);
  const choose = useCallback((next: Motion) => {
    chosen.current = true;
    set(next);
  }, []);
  return [motion, choose];
}
