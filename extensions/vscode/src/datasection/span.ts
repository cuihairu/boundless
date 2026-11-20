/**
 * Text position in a document: `line` and `character` are 0-based, the
 * character being the column inside `line`. `length` is the number of
 * characters the span covers on that line.
 *
 * Mirrors VS Code's `Position`/`Range` convention so a diagnostic range can be
 * built without conversion.
 */
export interface Span {
  line: number;
  character: number;
  length: number;
}

const WS = " \t\r\n\f";

/**
 * Converts an absolute character offset into a `Span` of `length` characters.
 *
 * `lineStarts` holds the absolute offset of the first character of each line
 * (sorted), built once per document.
 */
export function spanAt(lineStarts: number[], offset: number, length: number): Span {
  let line = 0;
  let lo = 0;
  let hi = lineStarts.length - 1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (lineStarts[mid] <= offset) {
      line = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return { line, character: offset - lineStarts[line], length };
}

/**
 * Absolute offsets of the first character of every line, plus the final offset
 * so documents without a trailing newline still terminate a line.
 */
export function lineStartsOf(text: string): number[] {
  const starts = [0];
  for (let i = 0; i < text.length; i++) {
    if (text.charCodeAt(i) === 10 /* \n */) {
      starts.push(i + 1);
    }
  }
  starts.push(text.length);
  return starts;
}

/** Width of the first `length` characters starting at `offset`, clamped to the
 * end of the line they begin on (a span never crosses a newline). */
export function clampSpan(span: Span, text: string, lineStarts: number[]): Span {
  const lineEnd = (lineStarts[span.line + 1] ?? text.length) - lineStarts[span.line];
  const available = lineEnd - span.character;
  const length = span.length > available ? Math.max(available, 0) : span.length;
  return length === span.length ? span : { ...span, length };
}

/** Offsets that trim leading and trailing whitespace out of a raw slice. */
export function trimOffsets(text: string, start: number, end: number): [number, number] {
  let s = start;
  let e = end;
  while (s < e && WS.includes(text.charAt(s))) s++;
  while (e > s && WS.includes(text.charAt(e - 1))) e--;
  return [s, e];
}
