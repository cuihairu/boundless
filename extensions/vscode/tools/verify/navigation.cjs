// BW-20-01-04 evidence: go-to-definition against the example res tree.
const { EXAMPLE_RES } = require("./paths.cjs");
const { findResourceRoots } = require("../../out/workspace/resourceRoot.js");
const { definitionTargets } = require("../../out/workspace/navigation.js");
const { nodeProbe } = require("../../out/workspace/nodeProbe.js");

const probe = nodeProbe();
const folder = EXAMPLE_RES.replace(/\/res$/, "");
const root = findResourceRoots(probe, [folder])[0];
console.log("res root:", root.resRoot);

const defPath = root.entityDefsDir + "/ClientAvatar.def";
const text = probe.read(defPath);

const lineOf = (file, name) => {
  const lines = probe.read(file).split("\n");
  const re = new RegExp(`^\\s*def\\s+${name}\\s*\\(`);
  return lines.findIndex((l) => re.test(l)) + 1;
};

const show = (label, offset) => {
  const result = definitionTargets(root, probe, text, offset, "ClientAvatar");
  const line = result.line === undefined ? "-" : `${result.line + 1}`;
  console.log(
    `${label}\n  -> ${result.paths
      .map((p) => p.replace(folder + "/", ""))
      .join(", ")} line ${line}`,
  );
};

show("cursor on '<Properties>'", text.indexOf("<Properties>"));
show("cursor on 'logOff'", text.indexOf("logOff"));
show("cursor on 'fromCell'", text.indexOf("fromCell"));
show("cursor on 'cellTick'", text.indexOf("cellTick"));

for (const [file, name] of [
  ["scripts/base/ClientAvatar.py", "logOff"],
  ["scripts/client/ClientAvatar.py", "fromCell"],
  ["scripts/client/ClientAvatar.py", "cellTick"],
]) {
  const abs = root.resRoot + "/" + file;
  console.log(`grep-style: ${file}:${lineOf(abs, name)} defines ${name}()`);
}
