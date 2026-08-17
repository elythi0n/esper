#!/usr/bin/env node
// The drawing behind a fingerprint, held to the one thing it is for.
//
// # Why a check and not a gallery
//
// `Identicon` exists to make a device that has quietly become a *different*
// device look different from across a desk. That is a claim about pairs of
// keys, and no gallery of five specimens can support it — five drawings that
// happen to differ say nothing about the ten thousand that might not.
//
// So this measures the property directly: keys differing by one hex character,
// which is the case that matters, because a device whose key has changed is
// not a random new key to the eye — it is *a key*, next to the one you
// remember.
//
// It is deliberately **not** a security check and the component says so at
// length. What is measured here is legibility of a difference, not the
// difficulty of forging a resemblance: the drawing is a few dozen bits, and
// anybody willing to grind can make two keys draw the same picture. Every
// screen that shows one shows the hex or the words beside it.
import { ACROSS, cellsOf } from "../src/lib/identicon.ts";

/** How many keys to draw. Enough that a 1-in-1000 collision rate shows up. */
const KEYS = 4000;
/** What fraction of one-character neighbours may look identical. */
const COLLISIONS_ALLOWED = 0.02;
/** A drawing this empty or this full is not a drawing. */
const INK_FLOOR = 0.1;
const INK_CEILING = 0.95;

const hex = "0123456789abcdef";
const draw = (key) => cellsOf(key).join("");

// Sixteen hex characters, which is the fingerprint form the store uses.
function aKey(n) {
  let out = "";
  let x = n * 2654435761;
  for (let i = 0; i < 16; i++) {
    x = (x * 1103515245 + 12345) >>> 0;
    out += hex[(x >>> (i % 4) * 4) & 15];
  }
  return out;
}

const problems = [];

// 1. The same key always draws the same picture. A drawing that moved would
//    be worse than none: it would report a change that had not happened.
if (draw("7503856abe9926c3") !== draw("7503856abe9926c3")) {
  problems.push("the same key drew two different pictures");
}

// 2. A key differing by one character looks different.
let neighbours = 0;
let same = 0;
for (let n = 0; n < KEYS; n++) {
  const key = aKey(n);
  const mine = draw(key);
  for (let at = 0; at < key.length; at += 5) {
    const other = hex[(hex.indexOf(key[at]) + 1) % 16];
    const near = key.slice(0, at) + other + key.slice(at + 1);
    neighbours++;
    if (draw(near) === mine) same++;
  }
}
const collisionRate = same / neighbours;
if (collisionRate > COLLISIONS_ALLOWED) {
  problems.push(
    `${(collisionRate * 100).toFixed(2)}% of one-character neighbours draw the same picture ` +
      `(${same} of ${neighbours}); the drawing does not show a changed key`,
  );
}

// 3. Every drawing has ink in it, and is not solid. A grid that is all empty
//    or all full is the same picture for every key that produces it.
let emptiest = 1;
let fullest = 0;
for (let n = 0; n < KEYS; n++) {
  const cells = cellsOf(aKey(n));
  const ink = cells.filter((c) => c > 0).length / cells.length;
  emptiest = Math.min(emptiest, ink);
  fullest = Math.max(fullest, ink);
}
if (emptiest < INK_FLOOR) {
  problems.push(`the emptiest drawing is ${(emptiest * 100).toFixed(0)}% ink, which is not a drawing`);
}
if (fullest > INK_CEILING) {
  problems.push(`the fullest drawing is ${(fullest * 100).toFixed(0)}% ink, which is a block`);
}

// 4. It is mirrored, which is what makes two keys look unlike rather than
//    noisy — and is the thing a refactor most easily loses.
for (let n = 0; n < 50; n++) {
  const cells = cellsOf(aKey(n));
  for (let y = 0; y < ACROSS; y++) {
    for (let x = 0; x < Math.floor(ACROSS / 2); x++) {
      if (cells[y * ACROSS + x] !== cells[y * ACROSS + (ACROSS - 1 - x)]) {
        problems.push("the drawing is not mirrored about its vertical axis");
        n = 50;
        y = ACROSS;
        break;
      }
    }
  }
}

if (problems.length) {
  console.error("");
  for (const p of problems) console.error(`  ✗ identicon: ${p}`);
  process.exit(1);
}
console.log(
  `  ✓ identicon: ${KEYS} keys, ${neighbours} one-character neighbours — ` +
    `${(collisionRate * 100).toFixed(2)}% look the same, ink ${(emptiest * 100).toFixed(0)}–${(fullest * 100).toFixed(0)}%, mirrored`,
);
