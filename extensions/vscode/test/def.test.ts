import { describe, expect, it } from "vitest";

import {
  AliasRegistry,
  parseEntitiesXml,
} from "../src/def/registry";
import { parseEntityDef } from "../src/def/defFile";
import { DefDiagnostic, checkEntityDef } from "../src/def/checker";
import { ALIAS_XML } from "./fixtures";

/** The reference `entities.xml` for the simple example. */
const ENTITIES_XML = `<root>
	<ClientServerEntities>
		<Guard/>
		<ClientAvatar/>
		<Account/>
		<Space/>
		<ClientOnly/>
	</ClientServerEntities>

	<ServerOnlyEntities/>
</root>`;

const aliases = AliasRegistry.parse(ALIAS_XML);

function defs(def: string, scripts: Record<string, boolean> = { client: true, base: true, cell: true }) {
  const parsed = parseEntityDef(def, "Account");
  const diagnostics = checkEntityDef(parsed, {
    aliases,
    hasScript: (role) => scripts[role] ?? false,
  });
  return diagnostics;
}

function rules(diagnostics: DefDiagnostic[]): string[] {
  return diagnostics.map((d) => d.rule);
}

describe("entity registry", () => {
  it("lists both entity groups", () => {
    const xml = parseEntitiesXml(ENTITIES_XML);
    expect(xml.clientServerEntities).toEqual(["Guard", "ClientAvatar", "Account", "Space", "ClientOnly"]);
    expect(xml.serverOnlyEntities).toEqual([]);
    expect(xml.clientServerMissing).toBe(false);
    expect(xml.serverOnlyMissing).toBe(false);
  });

  it("reports missing groups", () => {
    const xml = parseEntitiesXml("<root></root>");
    expect(xml.clientServerMissing).toBe(true);
    expect(xml.serverOnlyMissing).toBe(true);
  });

  it("resolves aliases into their spec sections", () => {
    expect(aliases.names()).toContain("OBJECT_ID");
    expect(aliases.get("OBJECT_ID")?.value).toBe("INT32");
    // MISSION_STATS is ARRAY of INFO.
    expect(aliases.get("MISSION_STATS")?.value).toBe("ARRAY");
    expect(aliases.get("MISSION_STATS")?.children[0]?.name).toBe("of");
  });

  it("keeps the first of duplicate alias names", () => {
    const dup = AliasRegistry.parse("<root><T>INT32</T><T>UINT32</T></root>");
    expect(dup.names()).toEqual(["T"]);
  });
});

describe("properties", () => {
  it("accepts the reference Account.def", () => {
    const def = `<root>
      <Properties>
        <playerName>
          <Type>STRING</Type>
          <Flags>BASE_AND_CLIENT</Flags>
          <Persistent>true</Persistent>
          <Editable>true</Editable>
        </playerName>
        <character>
          <Type>STRING</Type>
          <Flags>BASE</Flags>
          <Persistent>true</Persistent>
        </character>
      </Properties>
      <BaseMethods/>
      <CellMethods/>
      <ClientMethods/>
    </root>`;
    expect(defs(def)).toEqual([]);
  });

  it("accepts alias types, arrays and USER_TYPE", () => {
    const def = `<root>
      <Properties>
        <team>
          <Type>TEAM</Type>
          <Flags>ALL_CLIENTS</Flags>
        </team>
        <stats>
          <Type>ARRAY <of>INT32</of></Type>
          <Flags>CELL_PRIVATE</Flags>
        </stats>
        <custom>
          <Type>USER_TYPE <implementedBy>TestDataType.instance</implementedBy></Type>
          <Flags>BASE_AND_CLIENT</Flags>
        </custom>
      </Properties>
    </root>`;
    expect(defs(def)).toEqual([]);
  });

  it("rejects a property without <Type>", () => {
    const def = `<root><Properties><hp><Flags>BASE</Flags></hp></Properties></root>`;
    const diagnostics = defs(def);
    expect(rules(diagnostics)).toContain("property.type-missing");
    expect(diagnostics[0].severity).toBe("error");
    expect(diagnostics[0].message).toBe("DataType::buildDataType: Could not find MetaDataType ''");
    // The reference names the entity first: `Unable to find data type '' for Account.hp`.
    expect(diagnostics[1].message).toBe(
      "DataDescription::parse: Unable to find data type '' for Account.hp",
    );
  });

  it("rejects an unknown type name with the reference message", () => {
    const def = `<root><Properties><hp><Type>NOPE</Type><Flags>BASE</Flags></hp></Properties></root>`;
    const diagnostics = defs(def);
    expect(diagnostics[0].message).toBe(
      "DataType::buildDataType: Could not find MetaDataType 'NOPE'",
    );
    expect(diagnostics[1].message).toBe(
      "DataDescription::parse: Unable to find data type 'NOPE' for Account.hp",
    );
  });

  it("quotes the written name, not the resolved target, on failure", () => {
    // The reference reports `readString("Type")`, so an unresolvable alias is
    // blamed by the alias name as written.
    const broken = AliasRegistry.parse("<root><A>NOT_A_TYPE</A></root>");
    const def = `<root><Properties><hp><Type>A</Type><Flags>BASE</Flags></hp></Properties></root>`;
    const diagnostics = checkEntityDef(parseEntityDef(def, "Account"), { aliases: broken });
    expect(diagnostics[1].message).toBe(
      "DataDescription::parse: Unable to find data type 'A' for Account.hp",
    );
  });

  it("rejects a bad flags section", () => {
    const def = `<root><Properties><hp><Type>INT32</Type><Flags>WRONG</Flags></hp></Properties></root>`;
    const diagnostics = defs(def);
    expect(diagnostics.some((d) => d.rule === "property.flags-invalid")).toBe(true);
    expect(diagnostics[0].message).toBe(
      "DataDescription::parse: Invalid Flags section 'WRONG' for hp",
    );
  });

  it("rejects a missing flags section", () => {
    const def = `<root><Properties><hp><Type>INT32</Type></hp></Properties></root>`;
    expect(rules(defs(def))).toContain("property.flags-invalid");
  });

  it("requires Identifier properties to be persistent", () => {
    const def = `<root><Properties><hp>
      <Type>STRING</Type><Flags>BASE</Flags><Identifier>true</Identifier>
    </hp></Properties></root>`;
    const diagnostics = defs(def);
    expect(diagnostics.some((d) => d.rule === "property.identifier-not-persistent")).toBe(true);
    expect(diagnostics[0].message).toBe(
      "DataDescription::parse: hp must be persistent to be indexed",
    );
  });

  it("accepts an identifier on a persistent property", () => {
    const def = `<root><Properties><hp>
      <Type>STRING</Type><Flags>BASE</Flags><Identifier>true</Identifier><Persistent>true</Persistent>
    </hp></Properties></root>`;
    expect(defs(def)).toEqual([]);
  });

  it("rejects an array without <of>", () => {
    const def = `<root><Properties><hp><Type>ARRAY</Type><Flags>BASE</Flags></hp></Properties></root>`;
    const diagnostics = defs(def);
    expect(diagnostics.some((d) => d.rule === "property.sequence-no-of")).toBe(true);
  });

  it("rejects USER_TYPE without implementedBy", () => {
    const def = `<root><Properties><hp><Type>USER_TYPE</Type><Flags>BASE</Flags></hp></Properties></root>`;
    const diagnostics = defs(def);
    expect(
      diagnostics.some((d) => d.message.includes("implementedBy is not specified")),
    ).toBe(true);
  });

  it("requires implementedBy to contain a dot", () => {
    const def = `<root><Properties><hp>
      <Type>USER_TYPE <implementedBy>nodot</implementedBy></Type>
      <Flags>BASE</Flags></hp></Properties></root>`;
    const diagnostics = defs(def);
    expect(
      diagnostics.some((d) => d.message.includes("must contain a '.'")),
    ).toBe(true);
  });

  it("flags a MAILBOX member on a client-server property", () => {
    const def = `<root><Properties><box>
      <Type>MAILBOX</Type><Flags>ALL_CLIENTS</Flags>
    </box></Properties></root>`;
    const diagnostics = defs(def);
    expect(rules(diagnostics)).toContain("property.client-server-mailbox");
    // The MAILBOX variant says the type cannot be streamed, not "security hole".
    expect(diagnostics[0].message).toBe(
      "Account.box is a client-server property that has a MAILBOX member (type is not able to be sent to or from client)",
    );
  });

  it("warns on a PYTHON member of a client-server property", () => {
    const def = `<root><Properties><py>
      <Type>PYTHON</Type><Flags>ALL_CLIENTS</Flags>
    </py></Properties></root>`;
    const diagnostics = defs(def);
    expect(diagnostics.some((d) => d.rule === "property.client-server-python")).toBe(true);
    expect(diagnostics[0].severity).toBe("warning");
    expect(diagnostics[0].message).toBe(
      "Account.py is a client-server property that has a PYTHON member (potential security hole)",
    );
  });

  it("also resolves a detail level for ALL_CLIENTS data", () => {
    // isOtherClientData() is set by both OTHER_CLIENTS and ALL_CLIENTS.
    const def = `<root><Properties><hp>
      <Type>INT32</Type><Flags>ALL_CLIENTS</Flags><DetailLevel>far</DetailLevel>
    </hp></Properties></root>`;
    expect(rules(defs(def))).toContain("property.detail-level");
  });

  it("rejects a detail level label that LoDLevels does not declare", () => {
    const def = `<root>
      <Properties><hp>
        <Type>INT32</Type><Flags>OTHER_CLIENTS</Flags><DetailLevel>far</DetailLevel>
      </hp></Properties>
    </root>`;
    expect(rules(defs(def))).toContain("property.detail-level");
  });

  it("accepts a detail level label declared by LoDLevels", () => {
    const def = `<root>
      <LoDLevels>
        <level><label>far</label>1000</level>
      </LoDLevels>
      <Properties><hp>
        <Type>INT32</Type><Flags>OTHER_CLIENTS</Flags><DetailLevel>far</DetailLevel>
      </hp></Properties>
    </root>`;
    expect(defs(def)).toEqual([]);
  });

  it("ignores a detail level on a property that is not other-client data", () => {
    const def = `<root><Properties><hp>
      <Type>INT32</Type><Flags>BASE</Flags><DetailLevel>far</DetailLevel>
    </hp></Properties></root>`;
    expect(rules(defs(def))).not.toContain("property.detail-level");
  });

  it("caps LoDLevels at six levels", () => {
    const levels = [1, 2, 3, 4, 5, 6, 7]
      .map((i) => `<level><label>l${i}</label>${i * 100}</level>`)
      .join("");
    const def = `<root><LoDLevels>${levels}</LoDLevels></root>`;
    expect(rules(defs(def))).toContain("lod.level-count");
  });
});

describe("volatile", () => {
  it("accepts a descending priority order", () => {
    const def = `<root><Volatile><position>20</position><yaw>10</yaw><pitch>5</pitch></Volatile></root>`;
    expect(defs(def)).toEqual([]);
  });

  it("rejects an ascending priority order", () => {
    const def = `<root><Volatile><yaw>5</yaw><pitch>10</pitch></Volatile></root>`;
    const diagnostics = defs(def);
    expect(diagnostics.some((d) => d.rule === "volatile.priority-order")).toBe(true);
    expect(diagnostics[0].message).toContain("Invalid direction priorities (yaw < pitch)");
  });

  it("warns about an unrecognised volatile child", () => {
    const def = `<root><Volatile><postion>20</postion></Volatile></root>`;
    expect(rules(defs(def))).toContain("volatile.unknown-child");
  });

  it("treats -1 as ALWAYS and above everything", () => {
    const def = `<root><Volatile><yaw>-1</yaw><pitch>10</pitch><roll>5</roll></Volatile></root>`;
    expect(defs(def)).toEqual([]);
  });
});

describe("methods", () => {
  it("accepts the reference Account methods", () => {
    const def = `<root>
      <BaseMethods/>
      <CellMethods/>
      <ClientMethods>
        <chatMessage><Arg>STRING</Arg></chatMessage>
      </ClientMethods>
    </root>`;
    expect(defs(def)).toEqual([]);
  });

  it("rejects <Exposed> on a client method", () => {
    const def = `<root><ClientMethods><tick><Exposed/></tick></ClientMethods></root>`;
    const diagnostics = defs(def);
    expect(rules(diagnostics)).toContain("method.exposed-in-client");
    expect(diagnostics[0].message).toBe(
      "MethodDescription::parse: Unable to use <Exposed> tag in client method tick.",
    );
  });

  it("accepts ALL_CLIENTS only on cell methods", () => {
    const cell = `<root><CellMethods><tick><Exposed>ALL_CLIENTS</Exposed></tick></CellMethods></root>`;
    expect(defs(cell)).toEqual([]);

    const base = `<root><BaseMethods><tick><Exposed>ALL_CLIENTS</Exposed></tick></BaseMethods></root>`;
    expect(rules(defs(base))).toContain("method.exposed-invalid");
  });

  it("rejects an unknown Exposed flag", () => {
    const def = `<root><BaseMethods><tick><Exposed>NOPE</Exposed></tick></BaseMethods></root>`;
    const diagnostics = defs(def);
    expect(diagnostics[0].message).toContain(
      "Could not recognise NOPE flag in <Exposed> tag in tick",
    );
  });

  it("rejects an unknown ReplayExposureLevel", () => {
    const def = `<root><CellMethods><tick><ReplayExposureLevel>MAX</ReplayExposureLevel></tick></CellMethods></root>`;
    expect(rules(defs(def))).toContain("method.replay-exposure-level");
  });

  it("rejects an unresolvable argument type", () => {
    const def = `<root><BaseMethods><tick><Arg>NOPE</Arg></tick></BaseMethods></root>`;
    expect(rules(defs(def))).toContain("method.arg-type");
  });

  it("accepts a named argument list under <Args>", () => {
    const def = `<root><BaseMethods><tick><Args><x>INT32</x><y>FLOAT</y></Args></tick></BaseMethods></root>`;
    expect(defs(def)).toEqual([]);
  });

  it("rejects duplicate argument names", () => {
    const def = `<root><BaseMethods><tick><Args><x>INT32</x><x>UINT32</x></Args></tick></BaseMethods></root>`;
    expect(rules(defs(def))).toContain("method.arg-duplicate");
  });

  it("rejects a duplicated method name with a different signature", () => {
    const def = `<root><BaseMethods>
      <tick><Args><x>INT32</x></Args></tick>
      <tick><Args><y>UINT32</y></Args></tick>
    </BaseMethods></root>`;
    expect(rules(defs(def))).toContain("method.duplicate-signature");
  });

  it("allows a duplicated method name with the same signature", () => {
    const def = `<root><BaseMethods>
      <tick><Args><x>INT32</x></Args></tick>
      <tick><Args><x>INT32</x></Args></tick>
    </BaseMethods></root>`;
    expect(rules(defs(def))).not.toContain("method.duplicate-signature");
  });

  it("warns on a PYTHON argument to a client method", () => {
    const def = `<root><ClientMethods><tick><Arg>PYTHON</Arg></tick></ClientMethods></root>`;
    const diagnostics = defs(def);
    expect(rules(diagnostics)).toContain("method.client-python-arg");
    // Every client method counts as exposed in the reference.
    expect(diagnostics[0].message).toBe(
      "Account.tick is a client method that takes a PYTHON arg (potential security hole)",
    );
  });

  it("reports an exposed base method's MAILBOX arg as an Exposed method", () => {
    const def = `<root><BaseMethods><tick><Exposed>OWN_CLIENT</Exposed><Arg>MAILBOX</Arg></tick></BaseMethods></root>`;
    const diagnostics = defs(def);
    const mailbox = diagnostics.find((d) => d.rule === "method.exposed-mailbox-arg");
    expect(mailbox?.message).toBe(
      "Account.tick is an Exposed method that takes a MAILBOX arg (type is not able to be sent to or from client)",
    );
  });
});

describe("distribution", () => {
  it("warns when methods exist without a script", () => {
    const def = `<root><BaseMethods><tick/></BaseMethods></root>`;
    const diagnostics = defs(def, { client: true, base: false, cell: true });
    expect(diagnostics.some((d) => d.rule === "distribution.base-script")).toBe(true);
  });

  it("errors when client methods exist without a client script", () => {
    const def = `<root><ClientMethods><tick/></ClientMethods></root>`;
    const diagnostics = defs(def, { client: false, base: true, cell: true });
    const error = diagnostics.find((d) => d.rule === "distribution.client-script");
    expect(error).toBeDefined();
    expect(error?.severity).toBe("error");
  });

  it("accepts a def whose roles all have scripts", () => {
    const def = `<root><BaseMethods/><CellMethods/><ClientMethods/></root>`;
    expect(defs(def)).toEqual([]);
  });

  it("names the cell property rather than the method when both are missing", () => {
    const def = `<root>
      <Properties><position><Type>VECTOR3</Type><Flags>OTHER_CLIENTS</Flags></position></Properties>
      <CellMethods><tick/></CellMethods>
    </root>`;
    const diagnostics = defs(def, { client: true, base: true, cell: false });
    expect(diagnostics.some((d) => d.rule === "distribution.cell-property")).toBe(true);
    expect(diagnostics.some((d) => d.message === "Entitiy 'Account' does not have a cell script but has cell method 'tick'")).toBe(false);
  });

  it("errors when a service has no service script", () => {
    const def = `<root><Methods/></root>`;
    const parsed = parseEntityDef(def, "Account", true);
    const diagnostics = checkEntityDef(parsed, {
      aliases,
      hasScript: (role) => role === "service" ? false : true,
    });
    expect(diagnostics.some((d) => d.rule === "distribution.service-script")).toBe(true);
    expect(diagnostics.some((d) => d.message === "Service 'Account' does not have a service script.")).toBe(true);
  });
});
