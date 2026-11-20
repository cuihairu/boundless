import { describe, expect, it } from "vitest";

import { reduceEscapeSequences, expandSpecialChars } from "../src/datasection/specialChars";
import {
  asBool,
  asFloat,
  asInt,
  asLong,
  findChild,
  isValidXmlTag,
  openSection,
  parseXml,
  readBool,
  readString,
  type DataSection,
} from "../src/datasection/xmlSection";

/** Convenience: the value at `path`, or `undefined` when it misses. */
function at(doc: DataSection, path: string): string | undefined {
  return openSection(doc, path)?.value;
}

describe("escape sequences", () => {
  it("reduces the five named entities", () => {
    expect(reduceEscapeSequences("&amp;")).toBe("&");
    expect(reduceEscapeSequences("&lt;&gt;")).toBe("<>");
    expect(reduceEscapeSequences("&quot;x&quot;")).toBe('"x"');
    expect(reduceEscapeSequences("&apos;")).toBe("'");
    expect(reduceEscapeSequences("a&amp;b&lt;c")).toBe("a&b<c");
  });

  it("reduces decimal and lower-case hex character codes", () => {
    expect(reduceEscapeSequences("&#65;")).toBe("A");
    expect(reduceEscapeSequences("&#x41;")).toBe("A");
    // Upper-case 'X' is not the hex selector: atoi("X41") is 0 → NUL cut.
    expect(reduceEscapeSequences("&#X41;")).toBe("");
    expect(reduceEscapeSequences("&#0a;")).toBe("");
  });

  it("leaves malformed sequences literal", () => {
    expect(reduceEscapeSequences("&;")).toBe("&;");
    expect(reduceEscapeSequences("&z")).toBe("&z");
    expect(reduceEscapeSequences("&foo;")).toBe("&foo;");
  });

  it("truncates at a NUL produced by an out-of-range code", () => {
    expect(reduceEscapeSequences("&#300;")).toBe(","); // 300 as i8 == 44
    expect(reduceEscapeSequences("a&#0;b")).toBe("a");
  });

  it("reduces a single pass only", () => {
    expect(reduceEscapeSequences("&amp;amp;")).toBe("&amp;");
  });

  it("expands back to escapes", () => {
    expect(expandSpecialChars("&<>\"'")).toBe("&amp;&lt;&gt;&quot;&apos;");
    expect(expandSpecialChars("ok")).toBe("ok");
    expect(expandSpecialChars("é")).toBe("&#195;&#169;");
  });

  it("round-trips plain ASCII", () => {
    for (const raw of ["plain", "a&b<c>d\"e'f", "tab\tkept", ""]) {
      expect(reduceEscapeSequences(expandSpecialChars(raw))).toBe(raw);
    }
  });
});

describe("XML parsing", () => {
  it("renames the document element to the requested root tag", () => {
    expect(parseXml("root", "<r><c/></r>").name).toBe("root");
    expect(parseXml("", "<r><c/></r>").name).toBe("r");
  });

  it("keeps duplicate children in order and findChild takes the first", () => {
    const doc = parseXml("root", "<r><a>1</a><a>2</a><b>3</b></r>");
    expect(findChild(doc, "a")?.value).toBe("1");
    expect(doc.children).toHaveLength(3);
  });

  it("treats attributes as children before elements", () => {
    const doc = parseXml("root", '<r a="1" b="2"><c>3</c></r>');
    expect(doc.children).toHaveLength(3);
    const a = findChild(doc, "a");
    expect(a?.isAttribute).toBe(true);
    expect(a?.value).toBe("1");
    const c = findChild(doc, "c");
    expect(c?.isAttribute).toBe(false);
  });

  it("trims text but keeps CDATA raw", () => {
    const doc = parseXml("root", "<r><a>\n    hi   \n  </a><b><![CDATA[  x  ]]></b></r>");
    expect(at(doc, "a")).toBe("hi");
    expect(at(doc, "b")).toBe("  x  ");
  });

  it("accepts self-closed tags and empty values", () => {
    const doc = parseXml("root", "<r><a/><b></b></r>");
    expect(at(doc, "a")).toBe("");
    expect(at(doc, "b")).toBe("");
  });

  it("skips the XML declaration and comments", () => {
    const doc = parseXml(
      "root",
      '<?xml version="1.0"?>\n<!-- a comment -->\n<r><!-- inner --><a>1</a></r>',
    );
    expect(at(doc, "a")).toBe("1");
  });

  it("supports nested comments", () => {
    const doc = parseXml("root", "<r><!-- outer <!-- inner --> done --><a>1</a></r>");
    expect(at(doc, "a")).toBe("1");
  });

  it("fails on mismatched tags", () => {
    expect(() => parseXml("root", "<r><a></b></r>")).toThrow(/tags do not match/);
  });

  it("fails on a value assigned twice", () => {
    expect(() => parseXml("root", "<r><a>x<![CDATA[y]]></a></r>")).toThrow(
      /trying to set the value twice/,
    );
  });

  it("fails on unclosed comments and tags", () => {
    expect(() => parseXml("root", "<r><!-- oops </r>")).toThrow();
    expect(() => parseXml("root", "<r><a>1</r>")).toThrow();
    expect(() => parseXml("root", "<r><a>1")).toThrow(/unterminated tag/);
  });

  it("fails when no tag opens the document", () => {
    expect(() => parseXml("root", "hello")).toThrow(/file must start with a tag/);
    expect(() => parseXml("root", "</r>")).toThrow(/end tag at start of file/);
  });
});

describe("path walking", () => {
  const doc = parseXml(
    "root",
    "<root><Properties><hp><Type>INT32</Type></hp></Properties></root>",
  );

  it("walks dotted slash paths", () => {
    expect(at(doc, "Properties/hp/Type")).toBe("INT32");
    expect(at(doc, "Properties/hp")).toBe("");
  });

  it("returns undefined for a missing path", () => {
    expect(at(doc, "Properties/mp/Type")).toBeUndefined();
  });

  it("treats an empty path as the node itself", () => {
    expect(openSection(doc, "")).toBe(doc);
  });

  it("resolves a trailing slash to the parent child", () => {
    expect(at(doc, "Properties/hp/")).toBe("");
    expect(at(doc, "Properties/hp/") !== undefined).toBe(true);
  });

  it("collapses consecutive slashes", () => {
    expect(at(doc, "Properties//hp///Type")).toBe("INT32");
  });
});

describe("scalar conversions", () => {
  const node = (value: string): DataSection => ({
    name: "v",
    value,
    valueSet: true,
    isAttribute: false,
    children: [],
  });

  it("parses booleans case-insensitively by name only", () => {
    expect(asBool(node("true"), false)).toBe(true);
    expect(asBool(node("TRUE"), false)).toBe(true);
    expect(asBool(node("False"), true)).toBe(false);
    expect(asBool(node("1"), false)).toBe(false);
    expect(asBool(node("yes"), true)).toBe(true);
  });

  it("parses ints with atoi prefix rules", () => {
    expect(asInt(node("42"), -1)).toBe(42);
    expect(asInt(node("-7"), 0)).toBe(-7);
    expect(asInt(node("12abc"), 0)).toBe(12);
    expect(asInt(node("0"), -1)).toBe(0);
    // A zero outcome from anything other than the exact string "0" fails.
    expect(asInt(node("abc"), -1)).toBe(-1);
    expect(asInt(node(""), -1)).toBe(-1);
  });

  it("parses longs with the True/False pre-map", () => {
    expect(asLong(node("True"), -1)).toBe(1);
    expect(asLong(node("False"), -1)).toBe(0);
    expect(asLong(node("99"), 0)).toBe(99);
  });

  it("parses floats with strtod prefix rules", () => {
    expect(asFloat(node("1.5"), 0)).toBe(1.5);
    expect(asFloat(node("-2e3"), 0)).toBe(-2000);
    expect(asFloat(node(".5rest"), 0)).toBe(0.5);
    expect(asFloat(node("nope"), 7)).toBe(7);
  });

  it("reads values through paths with defaults", () => {
    const d = parseXml("root", "<root><a>on</a></root>");
    expect(readBool(d, "a", true)).toBe(true);
    expect(readString(d, "missing", "fallback")).toBe("fallback");
    expect(readString(d, "a")).toBe("on");
  });
});

describe("tag validity", () => {
  it("accepts ordinary names", () => {
    expect(isValidXmlTag("root")).toBe(true);
    expect(isValidXmlTag("_timer1")).toBe(true);
    expect(isValidXmlTag("playerName")).toBe(true);
  });

  it("rejects empty, digit-leading, slash and space", () => {
    expect(isValidXmlTag("")).toBe(false);
    expect(isValidXmlTag("1abc")).toBe(false);
    expect(isValidXmlTag("a/b")).toBe(false);
    expect(isValidXmlTag("a b")).toBe(false);
    expect(isValidXmlTag("a\\b")).toBe(false);
  });
});
