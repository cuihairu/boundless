import { describe, expect, it } from "vitest";

import {
  checkDefText,
  lineStarts,
  rangeAt,
} from "../src/workspace/diagnostics";
import { AliasRegistry } from "../src/def/registry";
import { ALIAS_XML } from "./fixtures";

const aliases = AliasRegistry.parse(ALIAS_XML);

/** A def with one bad property: unknown flag on `hp`. */
const BAD_FLAGS = `<root>
	<Properties>
		<hp>
			<Type>INT32</Type>
			<Flags>ON_NO_CELLS</Flags>
		</hp>
	</Properties>
	<BaseMethods/>
	<CellMethods/>
	<ClientMethods/>
</root>`;

const GOOD = `<root>
	<Properties>
		<hp>
			<Type>INT32</Type>
			<Flags>BASE</Flags>
		</hp>
	</Properties>
	<BaseMethods/>
	<CellMethods/>
	<ClientMethods/>
</root>`;

describe("line index", () => {
  it("starts every line after a newline", () => {
    expect(lineStarts("ab\ncd\n")).toEqual([0, 3, 6]);
  });

  it("keeps one line for text without newlines", () => {
    expect(lineStarts("abc")).toEqual([0]);
  });
});

describe("offset to range", () => {
  const text = "abc\ndef\nghi";

  it("maps a single-line offset", () => {
    // The 'e' in 'def'.
    expect(rangeAt(text, 5, 1)).toEqual({
      startLine: 1,
      startCharacter: 1,
      endLine: 1,
      endCharacter: 2,
    });
  });

  it("spans lines when the anchor crosses a newline", () => {
    // From the last 'c' of line 0 (offset 2) six characters forward: the '\n'
    // of line 1 then the first character of line 2.
    expect(rangeAt(text, 2, 6)).toEqual({
      startLine: 0,
      startCharacter: 2,
      endLine: 2,
      endCharacter: 0,
    });
  });

  it("clamps offsets past the end", () => {
    const r = rangeAt(text, 500, 0);
    expect(r.startLine).toBe(2);
  });
});

describe("def diagnostics", () => {
  it("accepts a clean def", () => {
    expect(checkDefText(aliases, GOOD, "Clean")).toEqual([]);
  });

  it("anchors a bad flag on its Flags element", () => {
    const diagnostics = checkDefText(aliases, BAD_FLAGS, "Bad");
    expect(diagnostics).toHaveLength(1);
    const [d] = diagnostics;
    expect(d.severity).toBe("error");
    expect(d.rule).toBe("property.flags-invalid");
    // The checker anchors on the element name, like the reference anchors on
    // the section it failed to read.
    const lines = BAD_FLAGS.split("\n");
    const lineText = lines[d.range.startLine];
    expect(
      lineText.slice(d.range.startCharacter, d.range.endCharacter),
    ).toBe("Flags");
  });

  it("carries the reference message wording", () => {
    const [d] = checkDefText(aliases, BAD_FLAGS, "Bad");
    expect(d.message).toBe(
      "DataDescription::parse: Invalid Flags section 'ON_NO_CELLS' for hp",
    );
  });

  it("reports unparseable XML on the whole file", () => {
    const diagnostics = checkDefText(aliases, "<root><unclosed>", "Broken");
    expect(diagnostics).toHaveLength(1);
    expect(diagnostics[0].severity).toBe("error");
    expect(diagnostics[0].rule).toBe("xml-syntax");
    expect(diagnostics[0].range.endLine).toBe(0);
  });

  it("flags a missing cell script when the entity needs one", () => {
    const text = `<root>
	<Properties>
		<hp>
			<Type>INT32</Type>
			<Flags>CELL_PUBLIC</Flags>
		</hp>
	</Properties>
	<BaseMethods/>
	<CellMethods/>
	<ClientMethods/>
</root>`;
    const diagnostics = checkDefText(aliases, text, "Cellful", false, {
      hasScript: (role) => role !== "cell",
    });
    expect(diagnostics.some((d) => d.rule === "distribution.cell-property")).toBe(true);
  });

  it("uses exactOptionalPropertyTypes-safe context", () => {
    // No script probe at all must behave like a probe that has every script.
    expect(
      checkDefText(aliases, GOOD, "Clean", false, {
        hasScript: () => true,
      }),
    ).toEqual([]);
  });
});
