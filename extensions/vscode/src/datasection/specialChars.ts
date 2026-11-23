/**
 * XML escape sequences, ported from `resmgr/xml_special_chars.cpp` and
 * mirrored by `crates/resources/src/special_chars.rs` (BW-01-03).
 *
 * The reference operates on NUL-terminated byte buffers, not UTF-8: bytes
 * >= 0x80 are escaped individually on write and character-code escapes
 * truncate to `i8` on read. Keep every function byte-oriented.
 */

/** `strtol`-style prefix parse: whitespace, optional sign, digits in `base`,
 * saturating at the signed 64-bit bounds. */
const I64_MAX = 9223372036854775807;

function strtol(bytes: number[], base: number): number {
  let i = 0;
  while (
    i < bytes.length &&
    [0x20, 0x09, 0x0a, 0x0d, 0x0b, 0x0c].includes(bytes[i])
  ) {
    i++;
  }
  let negative = false;
  if (i < bytes.length && (bytes[i] === 0x2b || bytes[i] === 0x2d)) {
    negative = bytes[i] === 0x2d;
    i++;
  }
  let any = false;
  let acc = 0;
  while (i < bytes.length) {
    const b = bytes[i];
    let digit = -1;
    if (b >= 0x30 && b <= 0x39) digit = b - 0x30;
    else if (b >= 0x61 && b <= 0x7a) digit = b - 0x61 + 10;
    else if (b >= 0x41 && b <= 0x5a) digit = b - 0x41 + 10;
    if (digit < 0 || digit >= base) break;
    any = true;
    // Saturate: the reference uses i64, well above JS safe integers here
    // because the value is truncated to i8 immediately.
    acc = Math.min(acc * base + digit, I64_MAX);
    i++;
  }
  if (!any) return 0;
  return negative ? -acc : acc;
}

/** Hex prefix parse used by `&#x..;` bodies; no digits yields `null`
 * (the reference warning path, which leaves the code at 0). */
function parseHex(bytes: number[]): number | null {
  let any = false;
  let acc = 0;
  for (const b of bytes) {
    let digit = -1;
    if (b >= 0x30 && b <= 0x39) digit = b - 0x30;
    else if (b >= 0x61 && b <= 0x66) digit = b - 0x61 + 10;
    else if (b >= 0x41 && b <= 0x46) digit = b - 0x41 + 10;
    if (digit < 0) break;
    any = true;
    acc = ((acc * 16) >>> 0) + digit;
  }
  return any ? acc : null;
}


/** `XmlSpecialChars::reduce`: turn escape sequences into characters.
 *
 * Recognised: `&amp; &lt; &gt; &quot; &apos;`, decimal `&#65;` and hex
 * `&#x41;` character codes. Unknown or malformed sequences stay literal.
 * Character codes outside 1..=255 still replace (truncated through `i8`,
 * matching the C cast); a zero result truncates the value, because the
 * reference observes strings through `strlen`.
 */
export function reduceEscapeSequences(input: string): string {
  const buf = toBytes(input);
  let len = buf.length;
  let i = 0;

  while (i < len) {
    if (buf[i] !== 0x26 /* & */) {
      i++;
      continue;
    }

    // Collect the escape body up to the terminator. `specialLen` counts the
    // '&' plus every consumed byte including ';'.
    const special = new Array<number>(31).fill(0);
    let specialLen = 1;
    let j = 0;
    let ii = i + 1;
    while (ii < len && j < 31) {
      specialLen++;
      if (buf[ii] === 0x3b /* ; */) break;
      special[j] = buf[ii];
      j++;
      ii++;
    }

    // "Too short" (&; or &<EOL>) and "too long" (no ';' within the scan
    // window) leave the '&' literal and keep scanning.
    if (specialLen <= 2 || specialLen >= 31) {
      i++;
      continue;
    }

    const body = special.slice(0, j);
    let replace: number;
    const known = namedReplace(body);
    if (known !== null) {
      replace = known;
    } else if (body[0] === 0x23 /* # */) {
      const rest = body.slice(1);
      const val = rest[0] === 0x78 /* x */
        ? (parseHex(rest.slice(1)) ?? 0)
        : strtol(rest, 10);
      replace = toI8(val);
    } else {
      // Unknown named sequence: stays literal.
      i++;
      continue;
    }

    // Replace the '&' and remove the rest of the sequence.
    buf[i] = replace;
    const width = specialLen - 1;
    let c = i + 1;
    while (c + width < len) {
      buf[c] = buf[c + width];
      c++;
    }
    len -= width;

    // The reference reads values back through strlen: an inserted NUL
    // truncates everything after it.
    if (replace === 0) {
      len = i;
      break;
    }

    // Forward single-pass scan: the replaced byte is never re-reduced.
    i++;
  }

  return fromBytes(buf.slice(0, len));
}

function namedReplace(body: number[]): number | null {
  switch (join(body)) {
    case "amp":
      return 0x26;
    case "lt":
      return 0x3c;
    case "gt":
      return 0x3e;
    case "quot":
      return 0x22;
    case "apos":
      return 0x27;
    default:
      return null;
  }
}

/** `XmlSpecialChars::expand`: the five named characters plus any byte
 * >= 0x80 (as unsigned) become escape sequences. */
export function expandSpecialChars(input: string): string {
  let out = "";
  for (const b of toBytes(input)) {
    if (b === 0x26) out += "&amp;";
    else if (b === 0x3c) out += "&lt;";
    else if (b === 0x3e) out += "&gt;";
    else if (b === 0x22) out += "&quot;";
    else if (b === 0x27) out += "&apos;";
    else if (b >= 0x80) out += `&#${b};`;
    else out += String.fromCharCode(b);
  }
  return out;
}

function toI8(val: number): number {
  const v = Math.trunc(val);
  if (v >= -128 && v <= 127) return v;
  return ((v % 256) + 256) % 256;
}

function toBytes(input: string): number[] {
  return Array.from(Buffer.from(input, "utf8"));
}

function fromBytes(bytes: number[]): string {
  return Buffer.from(bytes).toString("utf8");
}

function join(bytes: number[]): string {
  return Buffer.from(bytes).toString("latin1");
}

