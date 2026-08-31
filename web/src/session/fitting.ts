/**
 * How the picture sits in the surface, and where a pointer is because of it.
 *
 * # Why this is one module and not two
 *
 * There are two consumers of this answer and they must never disagree: the
 * canvas, which is told how to draw, and the pointer mapping, which works out
 * where a click landed *in the picture*. If the canvas is contained and the
 * mapping assumes stretched — or the other way round — every click is offset
 * along one axis and scaled along the other, getting worse the further the
 * window is from the session's shape.
 *
 * That is the kind of wrong that looks like a calibration problem rather than
 * a bug, and this project has had it once already, when the pointer was mapped
 * against the element instead of against the drawn picture. So the mode is a
 * value, the geometry is a function of it, and both consumers read the same
 * one.
 */

/** How a picture is fitted into the surface it is drawn on. */
export type Fit =
  /** Scaled to fit, centred, bars where the shapes differ. The default. */
  | "fit"
  /** One picture pixel per screen pixel, centred, cropped if it does not fit. */
  | "actual"
  /** Stretched to fill, ignoring the shape. */
  | "stretch";

/** Every mode, in the order a control should offer them. */
export const FITS: readonly Fit[] = ["fit", "actual", "stretch"];

/** What a person calls each one. */
export const FIT_NAMES: Readonly<Record<Fit, string>> = {
  fit: "Fit",
  actual: "Actual size",
  stretch: "Stretch",
};

/**
 * The class that makes a canvas sit that way.
 *
 * `object-none` for actual size draws the picture at its own resolution and
 * lets the surface crop it, which is what "1:1" means — not "scaled to
 * something close to 1:1".
 */
export const FIT_CLASS: Readonly<Record<Fit, string>> = {
  fit: "object-contain",
  actual: "object-none",
  stretch: "object-fill",
};

/** A rectangle, in the coordinates `getBoundingClientRect` uses. */
export interface Box {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** A picture's own size, in its own pixels. */
export interface Size {
  width: number;
  height: number;
}

/** Where the picture is drawn, and how big, inside the surface. */
export interface Placement {
  /** Horizontal scale from picture pixels to screen pixels. */
  scaleX: number;
  /** Vertical scale. Differs from `scaleX` only when stretched. */
  scaleY: number;
  /** Where the picture's left edge is, in client coordinates. */
  left: number;
  /** Where its top edge is. */
  top: number;
}

/**
 * Where the picture sits, for a mode, a surface and a picture size.
 *
 * Returns `null` when there is nothing to place — a surface with no area or a
 * picture with no size — so a caller cannot divide by zero and get an
 * `Infinity` that travels as a coordinate.
 */
export function placement(fit: Fit, box: Box, size: Size): Placement | null {
  if (box.width <= 0 || box.height <= 0 || size.width <= 0 || size.height <= 0) {
    return null;
  }

  if (fit === "stretch") {
    // Each axis scaled independently; the picture starts where the box does.
    return {
      scaleX: box.width / size.width,
      scaleY: box.height / size.height,
      left: box.left,
      top: box.top,
    };
  }

  // `fit` scales to the smaller ratio so the whole picture is inside;
  // `actual` does not scale at all. Both are centred, and both can therefore
  // put the picture's origin *outside* the box — which `actual` does whenever
  // the picture is larger than the surface, and which is correct: the middle
  // of the picture is the part somebody is looking at.
  const scale = fit === "actual" ? 1 : Math.min(box.width / size.width, box.height / size.height);
  return {
    scaleX: scale,
    scaleY: scale,
    left: box.left + (box.width - size.width * scale) / 2,
    top: box.top + (box.height - size.height * scale) / 2,
  };
}

/**
 * Where a client point is, in the picture's own pixels.
 *
 * Positions outside the picture are returned rather than refused. Somebody
 * dragging past the edge means it, and the engine is where a position is
 * clamped or dropped — a surface that silently swallowed them would make a
 * drag that left the window end somewhere the far machine never heard about.
 */
export function inPicture(
  fit: Fit,
  box: Box,
  size: Size,
  clientX: number,
  clientY: number,
): { x: number; y: number } | null {
  const at = placement(fit, box, size);
  if (!at) return null;
  return {
    x: Math.round((clientX - at.left) / at.scaleX),
    y: Math.round((clientY - at.top) / at.scaleY),
  };
}
