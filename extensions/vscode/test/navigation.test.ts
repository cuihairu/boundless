import { describe, expect, it } from "vitest";

import {
  FilesystemProbe,
  findResourceRoots,
} from "../src/workspace/resourceRoot";
import {
  NavigationProbe,
  containingSection,
  definitionTargets,
} from "../src/workspace/navigation";
import { parseEntityDef } from "../src/def/defFile";

/** A filesystem probe from a map of path to contents. */
function makeFs(files: Record<string, string>): FilesystemProbe & NavigationProbe {
  const dirs = new Map<string, string[]>();
  for (const path of Object.keys(files)) {
    const parts = path.split("/");
    for (let i = 1; i < parts.length; i++) {
      const dir = parts.slice(0, i).join("/");
      const children = dirs.get(dir) ?? [];
      if (!children.includes(parts[i])) children.push(parts[i]);
      dirs.set(dir, children);
    }
  }
  return {
    isFile: (p) => p in files,
    listDir: (p) => (dirs.has(p) ? [...dirs.get(p)!.sort()] : null),
    read: (p) => files[p] ?? null,
  };
}

const DEF = `<root>
	<Parent>Guard</Parent>
	<Properties>
		<hp>
			<Type>INT32</Type>
			<Flags>BASE</Flags>
		</hp>
	</Properties>
	<BaseMethods>
		<logOff>
			<Exposed/>
		</logOff>
	</BaseMethods>
	<CellMethods>
		<cellTick>
			<Exposed/>
		</cellTick>
	</CellMethods>
	<ClientMethods>
		<fromCell>
			<Arg>STRING</Arg>
		</fromCell>
	</ClientMethods>
</root>`;

const SERVICE_DEF = `<root>
	<Methods>
		<registerService>
			<Arg>STRING</Arg>
		</registerService>
	</Methods>
</root>`;

/** Scripts laid out the way the reference example lays them out. */
const FILES = {
  "game/res/scripts/entities.xml": "<root/>",
  "game/res/scripts/entity_defs/ClientAvatar.def": DEF,
  "game/res/scripts/entity_defs/Guard.def": "<root/>",
  "game/res/scripts/base/ClientAvatar.py": [
    "class ClientAvatar:",
    "\tdef __init__( self ):",
    "\t\tpass",
    "",
    "\tdef logOff( self ):",
    "\t\tpass",
  ].join("\n"),
  "game/res/scripts/cell/ClientAvatar.py": "class ClientAvatar:\n\tpass\n",
  "game/res/scripts/client/ClientAvatar.py": [
    "class ClientAvatar:",
    "\tdef fromCell( self, msg ):",
    "\t\tpass",
  ].join("\n"),
  "game/res/scripts/service_defs/Registry.def": SERVICE_DEF,
  "game/res/scripts/service/Registry.py":
    "class Registry:\n\tdef registerService( self, arg ):\n\t\tpass\n",
};

const fs = makeFs(FILES);
const root = findResourceRoots(fs, ["game"])[0];

/** The offset of `needle` inside the def text. */
function offsetOf(needle: string): number {
  const at = DEF.indexOf(needle);
  expect(at, `expected '${needle}' in the fixture`).toBeGreaterThanOrEqual(0);
  return at;
}

describe("definition navigation", () => {
  it("offers every role script the entity has", () => {
    const result = definitionTargets(root, fs, DEF, 0, "ClientAvatar");
    expect(result.paths).toEqual([
      "game/res/scripts/base/ClientAvatar.py",
      "game/res/scripts/cell/ClientAvatar.py",
      "game/res/scripts/client/ClientAvatar.py",
    ]);
  });

  it("offers only the roles that actually exist", () => {
    const files = makeFs({
      "game/res/scripts/entities.xml": "<root/>",
      "game/res/scripts/entity_defs/ClientOnly.def": DEF,
      "game/res/scripts/client/ClientOnly.py": "class ClientOnly:\n\tpass\n",
    });
    const only = findResourceRoots(files, ["game"])[0];
    expect(definitionTargets(only, files, DEF, 0, "ClientOnly").paths).toEqual([
      "game/res/scripts/client/ClientOnly.py",
    ]);
  });

  it("lands on the base method's symbol", () => {
    const result = definitionTargets(
      root,
      fs,
      DEF,
      offsetOf("logOff"),
      "ClientAvatar",
    );
    expect(result.paths).toEqual(["game/res/scripts/base/ClientAvatar.py"]);
    // `def logOff` is on the fifth line, zero-based index 4.
    expect(result.line).toBe(4);
  });

  it("lands on the client method's symbol", () => {
    const result = definitionTargets(
      root,
      fs,
      DEF,
      offsetOf("fromCell"),
      "ClientAvatar",
    );
    expect(result.paths).toEqual(["game/res/scripts/client/ClientAvatar.py"]);
    expect(result.line).toBe(1);
  });

  it("still opens the script when the symbol is missing from it", () => {
    // `cellTick` is declared in the def but the cell script has no `def`.
    const result = definitionTargets(
      root,
      fs,
      DEF,
      offsetOf("cellTick"),
      "ClientAvatar",
    );
    expect(result.paths).toEqual(["game/res/scripts/cell/ClientAvatar.py"]);
    expect(result.line).toBeUndefined();
  });

  it("resolves <Parent> to the parent's definition file", () => {
    const result = definitionTargets(root, fs, DEF, offsetOf("Guard"), "ClientAvatar");
    expect(result.paths).toEqual(["game/res/scripts/entity_defs/Guard.def"]);
  });

  it("resolves a service def to the service script", () => {
    const at = SERVICE_DEF.indexOf("registerService");
    const result = definitionTargets(
      root,
      fs,
      SERVICE_DEF,
      at,
      "Registry",
      true,
    );
    expect(result.paths).toEqual(["game/res/scripts/service/Registry.py"]);
    expect(result.line).toBe(1);
  });

  it("returns nothing when no script exists", () => {
    const files = makeFs({
      "game/res/scripts/entities.xml": "<root/>",
      "game/res/scripts/entity_defs/Orphan.def": DEF,
    });
    const only = findResourceRoots(files, ["game"])[0];
    expect(definitionTargets(only, files, DEF, 0, "Orphan").paths).toEqual([]);
  });

  it("finds the innermost element at an offset", () => {
    const def = parseEntityDef(DEF, "ClientAvatar");
    const at = offsetOf("STRING");
    const section = containingSection(def.root, at);
    expect(section?.name).toBe("Arg");
  });
});
