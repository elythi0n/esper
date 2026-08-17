/**
 * The drawing behind `Identicon`, as arithmetic.
 *
 * Separated from the component for one reason: the claim this makes is
 * measurable, and a claim in a `.tsx` file is a claim nothing can check.
 * `scripts/check-identicon.mjs` imports this and holds it to the property the
 * picture exists for — that two keys differing by one character look
 * different — which is not something anybody can verify by looking at a
 * gallery of five.
 */

/** How many cells across. Odd, so there is a middle column to mirror about. */
export const ACROSS = 5;

/**
 * A digest of the text, as bytes.
 *
 * FNV-1a, which is four lines and has no cryptographic pretension — the right
 * choice precisely *because* it makes no claim: nobody can mistake this for a
 * verification. The input is already a digest; this only spreads it.
 */
export function bitsOf(text: string): number[] {
  let hash = 0x811c9dc5;
  const stream: number[] = [];
  for (let round = 0; round < 4; round++) {
    for (let i = 0; i < text.length; i++) {
      hash ^= text.charCodeAt(i) + round;
      // The FNV prime, by shifts, so this stays in 32 bits without Math.imul
      // rounding surprises.
      hash += (hash << 1) + (hash << 4) + (hash << 7) + (hash << 8) + (hash << 24);
      hash >>>= 0;
    }
    for (let byte = 0; byte < 4; byte++) stream.push((hash >>> (byte * 8)) & 0xff);
  }
  return stream;
}

/** How many of the fifteen half-cells carry ink. */
export const INKED = 7;

/**
 * The grid, row-major, each cell 0 (empty) or 1–3 (a weight).
 *
 * Mirrored about the vertical axis, which is the arrangement that makes two
 * nearby keys look unlike each other rather than like noise — symmetry gives
 * the eye something to hold.
 *
 * # Why the ink is counted rather than sampled
 *
 * The first version inked each cell with probability three quarters, which
 * reads fine in a gallery of five and is wrong in the way only a measurement
 * finds: about one key in eighty drew a **solid block**, and a solid block is
 * the same picture for every key that draws it. `check-identicon.mjs` found
 * fifty of them in four thousand keys on its first run.
 *
 * So exactly `INKED` of the fifteen half-cells are chosen — a partial
 * shuffle driven by the digest — and the rest are empty. Every drawing then
 * has comparable visual weight, which is better than merely non-degenerate:
 * two keys differ by the *shape* of their ink rather than by how much of it
 * there is, and shape is what the eye recognises.
 */
export function cellsOf(of: string): number[] {
  const bits = bitsOf(of || "unknown");
  const half = Math.ceil(ACROSS / 2);
  const places = ACROSS * half;

  // A partial Fisher-Yates over the half-cells, taking its swaps from the
  // digest. Partial because only the first `INKED` positions matter.
  const order = Array.from({ length: places }, (_, i) => i);
  for (let i = 0; i < INKED; i++) {
    const byte = bits[i % bits.length] ?? 0;
    const j = i + (byte % (places - i));
    [order[i], order[j]] = [order[j] ?? 0, order[i] ?? 0];
  }

  const weights = new Array<number>(places).fill(0);
  for (let i = 0; i < INKED; i++) {
    const at = order[i] ?? 0;
    const byte = bits[(i + INKED) % bits.length] ?? 0;
    // Three weights, so a drawing has depth as well as shape.
    weights[at] = 1 + (byte % 3);
  }

  const grid: number[] = [];
  for (let y = 0; y < ACROSS; y++) {
    const row: number[] = [];
    for (let x = 0; x < half; x++) row.push(weights[y * half + x] ?? 0);
    for (let x = ACROSS - half - 1; x >= 0; x--) row.push(row[x] ?? 0);
    grid.push(...row.slice(0, ACROSS));
  }
  return grid;
}
