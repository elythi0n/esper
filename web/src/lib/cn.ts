/** Join class names, dropping anything falsy. The whole of what a class utility needs to do. */
export function cn(...parts: Array<string | false | null | undefined>): string {
  return parts.filter(Boolean).join(" ");
}
