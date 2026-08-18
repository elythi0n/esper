/**
 * A browser's key names, as the numbers the wire carries.
 *
 * # Why this is a table and not a translation
 *
 * The protocol sends **HID usages on page 0x07** — physical positions, not
 * characters, so that a key left of Enter is that key on the far machine
 * whatever either layout calls it. The W3C's `KeyboardEvent.code` values are
 * defined against the same HID table, which is the one piece of luck in this
 * whole path: `KeyA` *is* usage 0x04, by specification, and this file is a
 * name-to-number map rather than a guess about what a key means.
 *
 * What it is not is a list of characters. `event.key` is what the layout
 * produced and belongs nowhere near here; `event.code` is where the finger
 * was, which is the only thing a remote desktop can honestly forward.
 *
 * # It is checked, not reviewed
 *
 * `check-vocabularies.py` reads the engine's own HID table and asserts that
 * every usage this file can produce is one the engine can inject. A name here
 * that mapped to a usage no platform has would be a key that silently does
 * nothing on the far side.
 */
export const USAGE_FOR_CODE: Readonly<Record<string, number>> = {
  // Letters, in HID order — which is alphabetical, and deliberately not the
  // order they appear on any keyboard.
  KeyA: 0x04, KeyB: 0x05, KeyC: 0x06, KeyD: 0x07, KeyE: 0x08, KeyF: 0x09,
  KeyG: 0x0a, KeyH: 0x0b, KeyI: 0x0c, KeyJ: 0x0d, KeyK: 0x0e, KeyL: 0x0f,
  KeyM: 0x10, KeyN: 0x11, KeyO: 0x12, KeyP: 0x13, KeyQ: 0x14, KeyR: 0x15,
  KeyS: 0x16, KeyT: 0x17, KeyU: 0x18, KeyV: 0x19, KeyW: 0x1a, KeyX: 0x1b,
  KeyY: 0x1c, KeyZ: 0x1d,

  // Digits along the top. 1 is 0x1E and 0 is 0x27, which is the order the
  // row is in and not the order the numbers are.
  Digit1: 0x1e, Digit2: 0x1f, Digit3: 0x20, Digit4: 0x21, Digit5: 0x22,
  Digit6: 0x23, Digit7: 0x24, Digit8: 0x25, Digit9: 0x26, Digit0: 0x27,

  Enter: 0x28, Escape: 0x29, Backspace: 0x2a, Tab: 0x2b, Space: 0x2c,
  Minus: 0x2d, Equal: 0x2e, BracketLeft: 0x2f, BracketRight: 0x30,
  Backslash: 0x31, Semicolon: 0x33, Quote: 0x34, Backquote: 0x35,
  Comma: 0x36, Period: 0x37, Slash: 0x38, CapsLock: 0x39,

  F1: 0x3a, F2: 0x3b, F3: 0x3c, F4: 0x3d, F5: 0x3e, F6: 0x3f,
  F7: 0x40, F8: 0x41, F9: 0x42, F10: 0x43, F11: 0x44, F12: 0x45,

  PrintScreen: 0x46, ScrollLock: 0x47, Pause: 0x48,
  Insert: 0x49, Home: 0x4a, PageUp: 0x4b, Delete: 0x4c, End: 0x4d,
  PageDown: 0x4e, ArrowRight: 0x4f, ArrowLeft: 0x50, ArrowDown: 0x51,
  ArrowUp: 0x52,

  NumLock: 0x53, NumpadDivide: 0x54, NumpadMultiply: 0x55,
  NumpadSubtract: 0x56, NumpadAdd: 0x57, NumpadEnter: 0x58,
  Numpad1: 0x59, Numpad2: 0x5a, Numpad3: 0x5b, Numpad4: 0x5c,
  Numpad5: 0x5d, Numpad6: 0x5e, Numpad7: 0x5f, Numpad8: 0x60,
  Numpad9: 0x61, Numpad0: 0x62, NumpadDecimal: 0x63,

  // The key that only exists on keyboards outside the US, between the left
  // shift and Z. Forwarding it is the difference between a German keyboard
  // working and having a hole in it.
  IntlBackslash: 0x64,
  ContextMenu: 0x65,

  // Modifiers, which are the ones that matter most: a stuck one makes the far
  // machine unusable for its owner.
  ControlLeft: 0xe0, ShiftLeft: 0xe1, AltLeft: 0xe2, MetaLeft: 0xe3,
  ControlRight: 0xe4, ShiftRight: 0xe5, AltRight: 0xe6, MetaRight: 0xe7,
};

/** The usage for a `KeyboardEvent.code`, or nothing if this is not a key we carry. */
export function usageFor(code: string): number | null {
  return USAGE_FOR_CODE[code] ?? null;
}

/**
 * The protocol's button for the browser's number.
 *
 * The browser numbers them 0 left, 1 middle, 2 right, 3 back, 4 forward. The
 * protocol's own order happens to be the same, and this function exists so
 * that "happens to be" is written down in one place rather than assumed in
 * three — the engine refuses anything outside it.
 */
export function buttonFor(browser: number): number | null {
  return browser >= 0 && browser <= 4 ? browser : null;
}
