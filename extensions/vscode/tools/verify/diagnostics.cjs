// BW-20-01-05 evidence: the port's diagnostics for the fixtures the original
// `process_defs` was pointed at. Run `referenceBaseline.sh` first for the
// reference half of the comparison.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { REFERENCE } = require("./paths.cjs");

const { checkDefText } = require("../../out/workspace/diagnostics.js");
const { nodeProbe } = require("../../out/workspace/nodeProbe.js");
const { AliasRegistry } = require("../../out/def/registry.js");

const probe = nodeProbe();
const fixtures = fs.mkdtempSync(path.join(os.tmpdir(), "bwdefdiag-"));
const FILES = {
  "good.def": `<root>
	<Properties>
		<hp>
			<Type>INT32</Type>
			<Flags>BASE</Flags>
		</hp>
	</Properties>
</root>`,
  "badflags.def": `<root>
	<Properties>
		<hp>
			<Type>INT32</Type>
			<Flags>WRONG</Flags>
		</hp>
	</Properties>
</root>`,
  "badtype.def": `<root>
	<Properties>
		<hp>
			<Type>NOPE</Type>
			<Flags>BASE</Flags>
		</hp>
	</Properties>
</root>`,
};
for (const [name, text] of Object.entries(FILES)) {
  fs.writeFileSync(path.join(fixtures, name), text);
}

// The alias table comes from the reference res tree, not a hand-written copy.
const aliasFile = path.join(
  REFERENCE,
  "programming",
  "bigworld",
  "examples",
  "client_integration",
  "python",
  "simple",
  "res",
  "scripts",
  "entity_defs",
  "alias.xml",
);
const aliases = AliasRegistry.parse(probe.read(aliasFile) ?? "");
console.log("alias table:", aliasFile);

for (const name of Object.keys(FILES)) {
  const text = FILES[name];
  const diagnostics = checkDefText(aliases, text, "Fixture");
  console.log(`--- ${name}`);
  if (diagnostics.length === 0) {
    console.log("  (no diagnostics)");
    continue;
  }
  for (const d of diagnostics) {
    console.log(
      `  ${d.severity} ${d.range.startLine + 1}:${d.range.startCharacter + 1} rule=${d.rule}\n    ${d.message}`,
    );
  }
}
