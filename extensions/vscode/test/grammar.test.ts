import { beforeAll, describe, expect, it } from "vitest";

import {
  loadGrammar,
  scopesAt,
  tokenizeDocument,
  tokenizeLine,
  hasScope,
} from "./tokenize";

/**
 * `Account.def` from
 * `examples/client_integration/python/simple/res/scripts/entity_defs/Account.def`,
 * verbatim. The line numbers below are the ones of this file.
 */
const ACCOUNT_DEF = `<root>
	<Properties>
		<playerName>
			<Type>			STRING				</Type>
			<Flags>			BASE_AND_CLIENT		</Flags>
			<Persistent>	true				</Persistent>
			<Editable>		true				</Editable>
		</playerName>

		<character>
			<Type>			STRING				</Type>
			<Flags>			BASE				</Flags>
			<Persistent>	true				</Persistent>
			<Editable>		true				</Editable>
		</character>
	</Properties>

	<ClientMethods>
		<chatMessage>
			<Arg>	STRING	</Arg>
		</chatMessage>
	</ClientMethods>

	<CellMethods>
	</CellMethods>

	<BaseMethods>
	</BaseMethods>

</root>`;

const LINES = ACCOUNT_DEF.split("\n");

/** The scope stack of the first character of `text` inside `line`. */
function scopeOf(line: string, text: string): string[] {
  const offset = line.indexOf(text);
  expect(offset, `expected '${text}' in '${line}'`).toBeGreaterThanOrEqual(0);
  return scopesAt(line, offset);
}

describe("entities grammar", () => {
  beforeAll(async () => {
    await loadGrammar();
  });

  it("highlights a data type inside <Type>", () => {
    // Line 4: <Type>			STRING				</Type>
    expect(scopeOf(LINES[3], "STRING")).toContain("support.type.entity-type");
  });

  it("highlights the data flag inside <Flags>", () => {
    expect(scopeOf(LINES[4], "BASE_AND_CLIENT")).toContain(
      "support.constant.entity-flag",
    );
  });

  it("highlights booleans inside <Persistent>", () => {
    expect(scopeOf(LINES[5], "true")).toContain("constant.language.boolean");
  });

  it("highlights a method argument type inside <Arg>", () => {
    // Line 20
    expect(scopeOf(LINES[19], "STRING")).toContain("support.type.entity-type");
  });

  it("highlights section tag names", () => {
    // Anchor on the tag name, not the `<`, which is punctuation.
    expect(scopeOf(LINES[1], "Properties")).toContain("entity.tag.section");
    expect(scopeOf(LINES[17], "ClientMethods")).toContain("entity.tag.section");
    // The property's own element name is a tag too.
    expect(scopeOf(LINES[2], "playerName")).toContain("entity.tag.section");
  });

  it("does not invent scopes for plain text", () => {
    const line = "<note>some free text</note>";
    const tokens = tokenizeLine(line);
    expect(tokens.find((t) => t.scopes.includes("support.type.entity-type"))).toBeUndefined();
    expect(hasScope(line, "entity.tag.section")).toBe(true);
  });

  it("comments out <!-- -->", () => {
    const line = "<!-- keep flags in sync with the server -->";
    expect(scopeOf(line, "keep")).toContain("comment.block.xml");
  });

  it("scopes a USER_TYPE implementedBy as an implemented-by value", () => {
    const line =
      "<Type>USER_TYPE <implementedBy>TestDataType.instance</implementedBy></Type>";
    expect(hasScope(line, "support.type.entity-type")).toBe(true);
    expect(hasScope(line, "variable.other.implemented-by")).toBe(true);
  });

  it("scopes the <of> payload of an ARRAY type", () => {
    const line = "<Type>ARRAY <of>INT32</of></Type>";
    expect(scopeOf(line, "INT32")).toContain("support.type.entity-type");
  });

  it("scopes volatile priorities as numbers", () => {
    const line = "<Volatile><position>20</position></Volatile>";
    expect(scopeOf(line, "20")).toContain("constant.numeric");
  });

  it("scopes known flags wherever they appear", () => {
    const line = "<Flags>OTHER_CLIENTS</Flags>";
    expect(hasScope(line, "support.constant.entity-flag")).toBe(true);
    expect(hasScope(line, "support.type.entity-type")).toBe(false);
  });

  it("never scopes the punctuation of a value tag as its value", () => {
    // Regression: `<Flags>…</Flags>` used to leave the `>` of the opening tag
    // inside the value rule, where a greedy `\S+` claimed it as a flag.
    const line = "<Flags>OTHER_CLIENTS</Flags>";
    const flags = tokenizeLine(line).filter(
      (t) => t.scopes.includes("support.constant.entity-flag"),
    );
    expect(flags).toHaveLength(1);
    expect(line.slice(flags[0].start, flags[0].end)).toBe("OTHER_CLIENTS");
  });

  it("carries the grammar state across lines of the real fixture", () => {
    const doc = tokenizeDocument(ACCOUNT_DEF);
    const lineText = LINES[3];
    const token = doc[3].find((t) =>
      lineText.slice(t.start, t.end).includes("STRING"),
    );
    expect(token?.scopes).toContain("support.type.entity-type");
    // The same line must also carry the tag name of the enclosing element.
    expect(doc[3].some((t) => t.scopes.includes("entity.tag.section"))).toBe(true);
  });
});
