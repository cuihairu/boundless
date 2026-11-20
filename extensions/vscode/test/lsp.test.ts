import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import { nodeProbe } from "../src/workspace/nodeProbe";
import { findResourceRoots } from "../src/workspace/resourceRoot";
import { checkDefText } from "../src/workspace/diagnostics";
import { definitionTargets } from "../src/workspace/navigation";
import { AliasRegistry } from "../src/def/registry";

/**
 * The LSP-facing half of the extension: over a real workspace on disk the
 * documents must be diagnosable and navigable, the way a language server
 * (BW-27) drives the same services. Everything here runs on the real
 * filesystem with the real probe — no injected doubles.
 */
describe("lsp services over a real workspace", () => {
  const workspace = mkdtempSync(join(tmpdir(), "boundless-lsp-"));
  const resRoot = join(workspace, "res");
  const defsDir = join(resRoot, "scripts", "entity_defs");
  mkdirSync(join(resRoot, "scripts", "base"), { recursive: true });
  mkdirSync(join(resRoot, "scripts", "client"), { recursive: true });
  mkdirSync(defsDir, { recursive: true });

  const cleanScript = [
    "class Clean:",
    "\tdef __init__( self ):",
    "\t\tpass",
    "",
    "\tdef logOn( self ):",
    "\t\tpass",
  ].join("\n");

  writeFileSync(join(resRoot, "scripts", "entities.xml"), "<root/>");
  writeFileSync(
    join(defsDir, "alias.xml"),
    "<root>\n\t<HEALTH>\t\tINT32\t\t</HEALTH>\n</root>",
  );
  writeFileSync(
    join(defsDir, "Clean.def"),
    [
      "<root>",
      "\t<Properties>",
      "\t\t<hp>",
      "\t\t\t<Type>INT32</Type>",
      "\t\t\t<Flags>BASE</Flags>",
      "\t\t</hp>",
      "\t</Properties>",
      "\t<BaseMethods>",
      "\t\t<logOn>",
      "\t\t\t<Exposed/>",
      "\t\t</logOn>",
      "\t</BaseMethods>",
      "\t<CellMethods/>",
      "\t<ClientMethods/>",
      "</root>",
    ].join("\n"),
  );
  writeFileSync(
    join(defsDir, "BadFlags.def"),
    [
      "<root>",
      "\t<Properties>",
      "\t\t<hp>",
      "\t\t\t<Type>INT32</Type>",
      "\t\t\t<Flags>ON_NO_CELLS</Flags>",
      "\t\t</hp>",
      "\t</Properties>",
      "\t<BaseMethods/>",
      "\t<CellMethods/>",
      "\t<ClientMethods/>",
      "</root>",
    ].join("\n"),
  );
  writeFileSync(join(resRoot, "scripts", "base", "Clean.py"), cleanScript);
  writeFileSync(join(resRoot, "scripts", "client", "Clean.py"), cleanScript);

  const probe = nodeProbe();
  const root = findResourceRoots(probe, [workspace])[0];

  it("locates the resource root of a real folder", () => {
    expect(root).toBeDefined();
    expect(root?.resRoot).toBe(resRoot);
  });

  it("diagnoses a dirty document and keeps a clean one quiet", () => {
    const aliases = AliasRegistry.parse(probe.read(root!.aliasFile) ?? "");
    const hasScript = (name: string) => (role: string) =>
      probe.isFile(join(resRoot, "scripts", role, `${name}.py`));

    const dirtyText = probe.read(join(defsDir, "BadFlags.def"))!;
    const bad = checkDefText(aliases, dirtyText, "BadFlags", false, {
      hasScript: hasScript("BadFlags"),
    });
    expect(bad).toHaveLength(1);
    expect(bad[0].rule).toBe("property.flags-invalid");
    expect(bad[0].message).toContain("Invalid Flags section 'ON_NO_CELLS'");

    const cleanText = probe.read(join(defsDir, "Clean.def"))!;
    expect(
      checkDefText(aliases, cleanText, "Clean", false, {
        hasScript: hasScript("Clean"),
      }),
    ).toEqual([]);
  });

  it("jumps from a method element to the real script", () => {
    const text = probe.read(join(defsDir, "Clean.def"))!;
    const at = text.indexOf("logOn");
    const result = definitionTargets(root!, probe, text, at, "Clean");
    expect(result.paths).toEqual([join(resRoot, "scripts", "base", "Clean.py")]);
    // `def logOn` is the fifth line, zero-based 4.
    expect(result.line).toBe(4);
  });

  it("offers every role script the entity really has", () => {
    const text = probe.read(join(defsDir, "Clean.def"))!;
    const result = definitionTargets(
      root!,
      probe,
      text,
      text.indexOf("<Properties>"),
      "Clean",
    );
    expect(result.paths).toEqual([
      join(resRoot, "scripts", "base", "Clean.py"),
      join(resRoot, "scripts", "client", "Clean.py"),
    ]);
  });
});
