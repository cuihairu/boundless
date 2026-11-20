/**
 * The XML `DataSection` tree, a port of `resmgr/xml_section.cpp` observable
 * semantics (mirrored by `crates/resources/src/datasection.rs`, BW-01-03).
 *
 * Model notes, all of which the EntityDef readers depend on:
 * - Attributes are children too: a node's children are `[attributes in
 *   document order] + [element children in document order]`, so `findChild`
 *   can hit an attribute node and `countChildren` counts attributes.
 * - Text values are trimmed at parse time (leading whitespace is skipped,
 *   trailing whitespace is cut); CDATA content is taken raw.
 * - Every reference parse error (mismatched tags, double text values,
 *   unclosed comments, …) fails the whole document.
 * - A single root tag is renamed to `"root"` by `createFromFile`, which is
 *   why every `.def`/`.xml` resource is read through `root` regardless of the
 *   tag actually written.
 */

import { expandSpecialChars, reduceEscapeSequences } from "./specialChars";

/** Absolute offsets are the parser's position currency; editors convert them to
 * line/character spans with the document's line index. `nameOffset` points at
 * the first character of the node name inside its start tag, `valueOffset` at
 * the first non-whitespace character of its text value. */
export const NO_OFFSET = -1;

/** Why an XML document failed to parse. The reference prints an `ERROR_MSG`
 * and returns NULL; callers then fall back to a binary section. */
export class XmlParseError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "XmlParseError";
  }
}

/** One section of the tree: a named node with a string value, an attribute
 * marker and ordered children. */
export interface DataSection {
  readonly name: string;
  readonly value: string;
  /** Mirrors `cval_ != NULL`: only parse-time text/CDATA assignments set it. */
  readonly valueSet: boolean;
  readonly isAttribute: boolean;
  readonly children: readonly DataSection[];
  /** Absolute offset of the node name inside its start tag. */
  readonly nameOffset: number;
  readonly nameLength: number;
  /** Absolute offsets of the trimmed text value, `NO_OFFSET` when absent. */
  readonly valueOffset: number;
  readonly valueLength: number;
}

const NUL = 0x00;
const SLASH = 0x2f;
const BANG = 0x21;
const QUESTION = 0x3f;
const LT = 0x3c;
const GT = 0x3e;
const DQUOTE = 0x22;
const SQUOTE = 0x27;
const EQUALS = 0x3d;
const DASH = 0x2d;
const LBRACKET = 0x5b;

function isWhitespace(b: number): boolean {
  return b === 0x20 || b === 0x09 || b === 0x0d || b === 0x0a;
}

/** Reference `XMLSection::isValidXMLTag`: rejects empty names, a leading
 * digit or byte >= 0x80, and any space, backslash or slash. */
export function isValidXmlTag(name: string): boolean {
  const bytes = Array.from(Buffer.from(name, "utf8"));
  if (bytes.length === 0) return false;
  if (bytes[0] >= 0x80 || (bytes[0] >= 0x30 && bytes[0] <= 0x39)) return false;
  return bytes.every((b) => b !== 0x20 && b !== 0x5c && b !== SLASH);
}

function decode(bytes: Uint8Array, start = 0, end = bytes.length): string {
  return Buffer.from(bytes.subarray(start, end)).toString("utf8");
}

/** `DataSection::new(tag)` — an empty section with a name and no children. */
export function newSection(tag: string): DataSection {
  return {
    name: tag,
    value: "",
    valueSet: false,
    isAttribute: false,
    children: [],
    nameOffset: NO_OFFSET,
    nameLength: 0,
    valueOffset: NO_OFFSET,
    valueLength: 0,
  };
}

class Parser {
  pos = 0;
  private readonly open: DataSection[] = [];
  private root: DataSection | null = null;

  constructor(private readonly data: Uint8Array) {}

  private peek(): number {
    return this.pos < this.data.length ? this.data[this.pos] : NUL;
  }

  private alive(): boolean {
    return this.pos < this.data.length;
  }

  private nextOrError(): number {
    if (!this.alive()) throw new XmlParseError("unexpected end of document");
    return this.data[this.pos++];
  }

  /** C loop condition: continue while a node is open or no root yet. */
  run(): DataSection {
    while (this.open.length > 0 || this.root === null) {
      if (isWhitespace(this.peek())) {
        while (isWhitespace(this.peek()) && this.alive()) this.pos++;
      }

      if (this.peek() === LT) {
        this.pos++;
        switch (this.peek()) {
          case BANG:
            this.pos++;
            this.processBang();
            break;
          case QUESTION:
            this.pos++;
            this.processQuestionMark();
            break;
          default:
            this.processTag();
        }
      } else if (!this.alive()) {
        throw new XmlParseError(`unterminated tag ${this.openName()}`);
      } else if (this.open.length === 0) {
        throw new XmlParseError("file must start with a tag");
      } else {
        this.readValue();
      }
    }
    return this.root as DataSection;
  }

  private openName(): string {
    const n = this.open[this.open.length - 1];
    return n ? n.name : "NULL";
  }

  /** `<!` bodies: `<!-- … -->` (with nesting count), `<![CDATA[…]]>` (raw
   * value on the open node) or any other `<!XY…` stub, of which exactly the
   * type character is consumed. */
  private processBang(): void {
    const typeChar = this.nextOrError();
    if (typeChar === DASH) {
      if (this.nextOrError() !== DASH) {
        throw new XmlParseError('bad comment: expecting "--" following "<!"');
      }
      this.skipComment();
    } else if (typeChar === LBRACKET) {
      for (const expected of Array.from(Buffer.from("CDATA["))) {
        if (this.nextOrError() !== expected) {
          throw new XmlParseError('bad CDATA: expecting "CDATA[" following "<["');
        }
      }
      this.readCdata();
    }
    // Any other `<!` stub (e.g. DOCTYPE) consumes just the type character
    // and lets the rest flow through the main loop.
  }

  /** Sliding-window comment skip with the reference's nesting count: a `-->`
   * decrements, a `<!--` increments. */
  private skipComment(): void {
    let count = 1;
    let prev2 = 0;
    let prev3: number;
    let prev1 = this.nextOrError();
    let curr = this.nextOrError();

    while (this.alive()) {
      prev3 = prev2;
      prev2 = prev1;
      prev1 = curr;
      curr = this.nextOrError();

      if (prev2 === DASH && prev1 === DASH && curr === GT) {
        count--;
        if (count === 0) return;
      } else if (prev3 === LT && prev2 === BANG && prev1 === DASH && curr === DASH) {
        count++;
      }
    }
    throw new XmlParseError("comment not closed");
  }

  /** CDATA content is raw: no whitespace trim, no escape reduction,
   * truncated at the first NUL (the reference observes it via strlen). */
  private readCdata(): void {
    const start = this.pos;
    while (this.alive()) {
      if (this.data[this.pos] === 0x5d /* ] */) {
        const contentEnd = this.pos;
        const rest = this.data.subarray(this.pos);
        if (rest.length >= 3 && rest[1] === 0x5d && rest[2] === GT) {
          this.pos += 3;
          const node = this.open[this.open.length - 1];
          if (!node) throw new XmlParseError("CDATA outside section");
          if (node.valueSet) {
            throw new XmlParseError("trying to set the value twice (CDATA)");
          }
          let content = this.data.subarray(start, contentEnd);
          const nul = content.indexOf(NUL);
          if (nul >= 0) content = content.subarray(0, nul);
          this.replace(node, {
            value: decode(content),
            valueSet: true,
            valueOffset: start,
            valueLength: content.length,
          });
          return;
        }
      }
      this.pos++;
    }
    throw new XmlParseError("CDATA section not closed");
  }

  /** `<?…?>` skip; EOF before `?>` is an error. */
  private processQuestionMark(): void {
    let prev1 = this.nextOrError();
    let curr = this.nextOrError();
    while (this.alive() && !(prev1 === QUESTION && curr === GT)) {
      prev1 = curr;
      curr = this.nextOrError();
    }
    if (!this.alive() && !(prev1 === QUESTION && curr === GT)) {
      throw new XmlParseError('"<?" section not closed');
    }
  }

  private processTag(): void {
    const isEndTag = this.peek() === SLASH;
    if (isEndTag) this.pos++;

    // Tag text runs to the next '>'.
    const gtRel = this.data.subarray(this.pos).indexOf(GT);
    if (gtRel < 0) throw new XmlParseError("unclosed tag");
    let tagText = this.data.subarray(this.pos, this.pos + gtRel);
    const tagStart = this.pos;
    this.pos += gtRel + 1;

    // Trailing whitespace of the tag text is trimmed.
    while (tagText.length > 0 && isWhitespace(tagText[tagText.length - 1])) {
      tagText = tagText.subarray(0, tagText.length - 1);
    }

    if (this.open.length === 0 && isEndTag) {
      throw new XmlParseError("cannot have an end tag at start of file");
    }

    if (isEndTag) {
      const endName = decode(tagText);
      const node = this.open.pop() as DataSection;
      if (node.name !== endName) {
        throw new XmlParseError(
          `tags do not match: start "${node.name}" end "${endName}"`,
        );
      }
      const parent = this.open[this.open.length - 1];
      if (parent) this.pushChild(parent, node);
      else this.root = node;
      return;
    }

    // Start tag: extract the name, detect the `<tag/>` shorthand.
    const isEmptyTag = tagText.length > 0 && tagText[tagText.length - 1] === SLASH;
    let nameEnd = 0;
    while (nameEnd < tagText.length) {
      const b = tagText[nameEnd];
      if (b === NUL || b === SLASH || isWhitespace(b)) break;
      nameEnd++;
    }
    const name = decode(tagText, 0, nameEnd);
    const nameOffset = tagStart;
    const children: DataSection[] = [];
    this.parseAttributes(children, tagText.subarray(nameEnd), nameOffset + nameEnd);

    const node: DataSection = {
      name,
      value: "",
      valueSet: false,
      isAttribute: false,
      children,
      nameOffset,
      nameLength: nameEnd,
      valueOffset: NO_OFFSET,
      valueLength: 0,
    };

    if (isEmptyTag) {
      const parent = this.open[this.open.length - 1];
      // A self-closed root completes the document at once; the main loop
      // then exits and ignores any trailing content, exactly like the
      // reference.
      if (parent) this.pushChild(parent, node);
      else this.root = node;
    } else {
      // Non-empty tags descend; children attach when the end tag closes.
      this.open.push(node);
    }
  }

  /** Attribute collection. Syntax problems stop collection for that tag; the
   * tag itself still parses. `baseOffset` is the absolute offset of `rest[0]`. */
  private parseAttributes(out: DataSection[], rest: Uint8Array, baseOffset: number): void {
    let p = 0;
    const end = rest.length;
    for (;;) {
      while (p < end && isWhitespace(rest[p])) p++;

      // Attribute name: up to whitespace, '=' or NUL.
      const nameStart = p;
      while (p < end && rest[p] !== NUL && rest[p] !== EQUALS && !isWhitespace(rest[p])) {
        p++;
      }
      // End of tag text (or a NUL): stop collecting attributes.
      if (p >= end || rest[p] === NUL) break;

      const nextChar = rest[p];
      const name = decode(rest, nameStart, p);
      const nameOffset = baseOffset + nameStart;
      const nameLength = p - nameStart;
      p++;

      // The '=' may follow whitespace.
      if (nextChar !== EQUALS) {
        while (p < end && isWhitespace(rest[p])) p++;
        if (p >= end || rest[p] !== EQUALS) break;
        p++;
      }

      while (p < end && isWhitespace(rest[p])) p++;

      const quote = p < end ? rest[p] : NUL;
      if (quote !== DQUOTE && quote !== SQUOTE) break;
      p++;

      const valueStart = p;
      while (p < end && rest[p] !== NUL && rest[p] !== quote) p++;
      if (p >= end || rest[p] !== quote) break;
      const value = reduceEscapeSequences(decode(rest, valueStart, p));
      p++;

      out.push({
        name,
        value,
        valueSet: true,
        isAttribute: true,
        children: [],
        nameOffset,
        nameLength,
        valueOffset: baseOffset + valueStart,
        valueLength: p - valueStart,
      });
    }
  }

  /** Text content between tags: leading whitespace was already skipped at
   * loop top, trailing whitespace is trimmed, NUL truncates (strlen view),
   * escapes are reduced. A second value assignment is fatal. */
  private readValue(): void {
    const start = this.pos;
    let valueEnd = start;
    while (this.alive() && this.peek() !== LT) {
      const b = this.data[this.pos];
      this.pos++;
      if (!isWhitespace(b)) valueEnd = this.pos;
    }
    if (!this.alive()) {
      throw new XmlParseError(`unterminated tag ${this.openName()}`);
    }

    const node = this.open[this.open.length - 1];
    if (node.valueSet) {
      throw new XmlParseError(`trying to set the value twice on "${node.name}"`);
    }
    let span = this.data.subarray(start, valueEnd);
    const nul = span.indexOf(NUL);
    if (nul >= 0) span = span.subarray(0, nul);
    this.replace(node, {
      value: reduceEscapeSequences(decode(span)),
      valueSet: true,
      valueOffset: start,
      valueLength: span.length,
    });
  }

  /** The open stack is immutable-by-interface, so replacement happens by
   * swapping the entry; children arrays are mutated through `pushChild`. */
  private replace(target: DataSection, patch: Partial<DataSection>): void {
    for (let i = 0; i < this.open.length; i++) {
      if (this.open[i] === target) {
        this.open[i] = { ...target, ...patch };
        return;
      }
    }
    if (this.root === target) this.root = { ...target, ...patch };
  }

  private pushChild(parent: DataSection, child: DataSection): void {
    (parent.children as DataSection[]).push(child);
  }
}

/**
 * Parses a whole XML document into a tree rooted under `rootTag`
 * (`XMLSection::createFromBinary` + `createFromFile`'s root rename).
 *
 * @param rootTag rename target for the document element; `""` keeps the tag
 *   as written.
 */
export function parseXml(rootTag: string, raw: Uint8Array | string): DataSection {
  const bytes = typeof raw === "string" ? new Uint8Array(Buffer.from(raw, "utf8")) : raw;
  const parser = new Parser(bytes);
  const root = parser.run();
  return rootTag ? { ...root, name: rootTag } : root;
}

// ---------------------------------------------------------------------------
// Tree queries (`DataSection` in datasection.cpp)
// ---------------------------------------------------------------------------

export function findChild(node: DataSection, tag: string): DataSection | undefined {
  return node.children.find((c) => c.name === tag);
}

export function sectionName(node: DataSection): string {
  return node.name;
}

/**
 * `DataSection::openSection` with `makeNewSection = false`: walks a
 * `'/'`-separated tag path down to a child. Quirks preserved: an empty path is
 * `self`; a path ending in a slash resolves to the child before the slash;
 * consecutive slashes collapse (`find_first_not_of`).
 */
export function openSection(node: DataSection, path: string): DataSection | undefined {
  if (path === "") return node;
  const slash = path.indexOf("/");
  if (slash < 0) return findChild(node, path);
  const child = findChild(node, path.slice(0, slash));
  if (!child) return undefined;
  let rest = slash;
  while (rest < path.length && path[rest] === "/") rest++;
  // The string ended with a slash.
  if (rest >= path.length) return child;
  return openSection(child, path.slice(rest));
}

/** `DataSection::readString`: the value of the found section
 * (`XMLSection::asString` ignores the default, so an empty tag yields an
 * empty string), or `default` when the path misses. */
export function readString(node: DataSection, path: string, fallback = ""): string {
  const found = openSection(node, path);
  return found ? found.value : fallback;
}

/** Case-insensitive `true`/`false`; everything else (including `1`) yields
 * the default. */
export function asBool(node: DataSection, fallback: boolean): boolean {
  const lower = node.value.toLowerCase();
  if (lower === "true") return true;
  if (lower === "false") return false;
  return fallback;
}

/** `DataSection::readBool`: find the child, convert or default. */
export function readBool(node: DataSection, path: string, fallback: boolean): boolean {
  const found = openSection(node, path);
  return found ? asBool(found, fallback) : fallback;
}

/** `atoi` plus `checkConversion`: prefix parse through i64, i32 truncation,
 * then a zero result must come from the exact string `"0"`. */
export function asInt(node: DataSection, fallback: number): number {
  if (node.value === "") return fallback;
  const parsed = prefixInteger(node.value);
  if (parsed === null) return fallback;
  const result = parsed | 0;
  if (result === 0 && node.value !== "0") return fallback;
  return result;
}

export function readInt(node: DataSection, path: string, fallback: number): number {
  const found = openSection(node, path);
  return found ? asInt(found, fallback) : fallback;
}

/** `readFloat`: `strtod` prefix parse, trailing junk ignored (the reference
 * has no strict eof check here). */
export function asFloat(node: DataSection, fallback: number): number {
  const v = prefixDouble(node.value);
  return v === null ? fallback : v;
}

export function readFloat(node: DataSection, path: string, fallback: number): number {
  const found = openSection(node, path);
  return found ? asFloat(found, fallback) : fallback;
}

/** Exact-case `True`/`False` map to 1/0 before the integer parse. */
export function asLong(node: DataSection, fallback: number): number {
  if (node.value === "True") return 1;
  if (node.value === "False") return 0;
  if (node.value === "") return fallback;
  const parsed = prefixInteger(node.value);
  if (parsed === null) return fallback;
  if (parsed === 0 && node.value !== "0") return fallback;
  return parsed;
}

/** C `strtol`/`atol` prefix parse: whitespace, optional sign, digits,
 * saturating at the i64 bounds (`null` when no digits were consumed). */
function prefixInteger(value: string): number | null {
  const bytes = Array.from(Buffer.from(value, "utf8"));
  let i = 0;
  while (i < bytes.length && [0x20, 0x09, 0x0a, 0x0d, 0x0b, 0x0c].includes(bytes[i])) i++;
  let negative = false;
  if (i < bytes.length && (bytes[i] === 0x2b || bytes[i] === 0x2d)) {
    negative = bytes[i] === 0x2d;
    i++;
  }
  let any = false;
  let acc = 0;
  while (i < bytes.length && bytes[i] >= 0x30 && bytes[i] <= 0x39) {
    any = true;
    acc = Math.min(acc * 10 + (bytes[i] - 0x30), 9223372036854775807);
    i++;
  }
  if (!any) return 0;
  return negative ? -acc : acc;
}

/** `strtod` prefix parse. Recognises the C forms the reference accepts:
 * optional sign, digits with optional fraction, optional exponent, plus
 * `inf`/`infinity`/`nan`. Returns `null` when no conversion happens. */
function prefixDouble(value: string): number | null {
  const text = value.replace(/^[\s\u000b\u000c]+/, "");
  const m = /^[+-]?(?:inf(?:inity)?|nan|(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?)/i.exec(text);
  if (!m) return null;
  const parsed = Number(m[0]);
  return Number.isNaN(parsed) && !/nan/i.test(m[0]) ? null : parsed;
}

/** `DataSection::save`-adjacent helper: the value with escapes expanded,
 * used when rebuilding a section for round-tripping. */
export function expandedValue(node: DataSection): string {
  return expandSpecialChars(node.value);
}
