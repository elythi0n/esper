// Colour maths shared by the checkers: OKLCH in, WCAG contrast out.
// One copy, so the contrast gate and the presence contract cannot disagree
// about what a number means.

/** OKLCH → linear sRGB (Björn Ottosson's reference matrices). Gamut-clipped. */
export function oklchToLinear(L, C, hDeg, a = 1) {
  const h = (hDeg * Math.PI) / 180;
  const A = C * Math.cos(h);
  const B = C * Math.sin(h);
  const l_ = L + 0.3963377774 * A + 0.2158037573 * B;
  const m_ = L - 0.1055613458 * A - 0.0638541728 * B;
  const s_ = L - 0.0894841775 * A - 1.291485548 * B;
  const l = l_ ** 3, m = m_ ** 3, s = s_ ** 3;
  const clip = (x) => Math.min(1, Math.max(0, x));
  return {
    r: clip(+4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
    g: clip(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
    b: clip(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s),
    a,
  };
}

/** Parse a literal `oklch(L C h [/ a])` string, or null. */
export function parseOklch(expr) {
  const m = expr.trim().match(/^oklch\(\s*([\d.]+)\s+([\d.]+)\s+([\d.]+)\s*(?:\/\s*([\d.]+))?\s*\)$/);
  if (!m) return null;
  return oklchToLinear(Number(m[1]), Number(m[2]), Number(m[3]), m[4] === undefined ? 1 : Number(m[4]));
}

/** Alpha-composite fg over an opaque bg, in linear light. */
export function over(fg, bg) {
  const a = fg.a;
  return { r: a * fg.r + (1 - a) * bg.r, g: a * fg.g + (1 - a) * bg.g, b: a * fg.b + (1 - a) * bg.b, a: 1 };
}

/** WCAG 2 relative luminance from linear sRGB. */
export const luminance = (c) => 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;

/** WCAG 2 contrast ratio, either order. */
export function contrast(fg, bg) {
  const [hi, lo] = [luminance(fg), luminance(bg)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}
