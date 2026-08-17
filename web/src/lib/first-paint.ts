/**
 * What the window looks like, decided before the first pixel.
 *
 * # Why this is its own module
 *
 * The theme has to be right in the *first* frame. A window that mounts dark
 * and corrects itself to light is a flash somebody sees every single launch,
 * and no amount of speed elsewhere hides it — so the choice is applied by an
 * inline script in the document, before the bundle loads.
 *
 * That script and the running application then both need the same rule, which
 * is exactly the shape of duplication this project keeps finding the hard way:
 * two copies drift, and the one nobody is looking at is the one that is wrong.
 *
 * So there is **one implementation**, `resolveTheme`, and the inline script is
 * *generated from it* — `BEFORE_PAINT` is that function's own source, injected
 * into `index.html` at build time by a plugin in `vite.config.ts`. There is no
 * second copy to keep in step because there is no second copy.
 */

export const THEME_KEY = "esper.theme";
export const MOTION_KEY = "esper.motion";

/** What somebody may choose. `auto` lets the desktop's *accessibility* setting decide. */
export type ThemeChoice = "auto" | "dark" | "light" | "hc";
/** What actually gets applied. There is no `system` on the element. */
export type ThemeApplied = "dark" | "light" | "hc";

/**
 * Which theme to apply, given what was chosen.
 *
 * # The rule, and the two things it deliberately does not do
 *
 * `auto` — the default — means **high contrast if the desktop asked for it,
 * and this product's own dark otherwise.**
 *
 * It does *not* follow `prefers-color-scheme`, and that was tried first. The
 * query reports `light` on every machine whose owner has expressed no
 * preference at all, so following it would turn the product light for almost
 * everybody who never asked for anything — which is not "respecting the
 * desktop", it is mistaking a default for a decision. Somebody who wants a
 * light window picks Light, once, and it is remembered.
 *
 * But `prefers-contrast: more` is not a preference about looks. Somebody who
 * set it asked their operating system for higher contrast *everywhere*, and
 * an application ignoring it because its own default happened to be pretty
 * would be choosing its identity over their eyesight. So `auto` honours it,
 * and an explicit choice made in this window still wins — that is the more
 * specific instruction of the two.
 *
 * # Why `auto` is a real stored value and not "nothing stored"
 *
 * It was "nothing stored" for an afternoon, and the bug is instructive: the
 * running application read the absence as `dark`, so the moment anything
 * called `applyTheme` the resolution was pinned to dark and the desktop's
 * setting could never be picked up again. Which mattered, because
 * **WebKitGTK does not always know the preference when the document starts
 * parsing** — measured: the inline script saw no preference and the page, two
 * frames later, saw `prefers-contrast: more`. So the choice has to survive as
 * a choice, and `watchTheDesktop` catches up when the answer arrives late.
 *
 * Written as a standalone function with no imports and no closure, because
 * its own source is what runs in the document before anything is loaded.
 */
export function resolveTheme(stored: string | null): ThemeApplied {
  var asked = stored;
  if (asked === "dark" || asked === "light" || asked === "hc") return asked;
  if (typeof matchMedia === "function" && matchMedia("(prefers-contrast: more)").matches) return "hc";
  return "dark";
}

/**
 * The script that runs in `<head>`, built from the function above.
 *
 * `resolveTheme.toString()` is the whole point: the rule is compiled once and
 * this is a copy of the bytes rather than a second telling of it. Motion is
 * a straight read with no rule to duplicate, so it is written out here.
 */
export const BEFORE_PAINT = `(function () {
  try {
    var d = document.documentElement.dataset;
    var resolve = ${resolveTheme.toString()};
    d.theme = resolve(localStorage.getItem(${JSON.stringify(THEME_KEY)}));
    var m = localStorage.getItem(${JSON.stringify(MOTION_KEY)});
    if (m === "reduced" || m === "full") d.motion = m;
  } catch (e) {}
})();`;
