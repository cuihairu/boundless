// BW-20-03-01 evidence: build the VSIX from the extension folder alone and
// check what it ships, so the package can be installed into an editor that has
// nothing else.
const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const { EXTENSION_DIR } = require("./paths.cjs");
const manifest = JSON.parse(
  fs.readFileSync(path.join(EXTENSION_DIR, "package.json"), "utf8"),
);

const failures = [];
const check = (label, condition, detail) => {
  console.log(`${condition ? "PASS" : "FAIL"} ${label}${detail ? ` — ${detail}` : ""}`);
  if (!condition) failures.push(label);
};

/** The `vsce` of the extension folder itself: no global tooling. */
const vsce = path.join(EXTENSION_DIR, "node_modules", ".bin", "vsce");
check("the extension ships its own packager", fs.existsSync(vsce), vsce);

console.log("building the package");
execFileSync(vsce, ["package", "--no-dependencies"], {
  cwd: EXTENSION_DIR,
  stdio: "inherit",
});

const vsix = path.join(EXTENSION_DIR, `${manifest.name}-${manifest.version}.vsix`);
check("the package carries the manifest's name and version", fs.existsSync(vsix), vsix);

// Outside the extension folder: the packager would ship a leftover archive.
const archive = fs.mkdtempSync(path.join(os.tmpdir(), "bwvsix-"));
execFileSync("unzip", ["-q", vsix, "-d", archive]);

const extension = path.join(archive, "extension");
const walk = (dir) => {
  const found = [];
  for (const name of fs.readdirSync(dir)) {
    const full = path.join(dir, name);
    const stat = fs.statSync(full);
    if (stat.isDirectory()) found.push(...walk(full));
    else if (stat.isFile()) found.push(full);
  }
  return found;
};
const shipped = walk(extension).map((file) =>
  path.relative(extension, file).split(path.sep).join("/"),
);

console.log(`shipped ${shipped.length} files:`, shipped.sort().join(" "));

check("it ships the manifest", shipped.includes("package.json"));
check(
  "it ships the compiled extension at the path the manifest points at",
  shipped.includes(`${manifest.main.replace(/^\.\//, "")}`),
  manifest.main,
);
check(
  "it ships the language, the snippets and the grammar",
  shipped.includes("language-configuration.json") &&
    shipped.includes("snippets/entities.json") &&
    shipped.includes("syntaxes/entities.tmLanguage.json"),
);
check(
  "it ships every module the entry point loads",
  shipped.includes("out/workspace/docsMcp.js") &&
    shipped.includes("out/workspace/launchConfig.js") &&
    shipped.includes("out/workspace/cluster.js"),
);
check(
  "it leaves the sources and the tests behind",
  !shipped.some((f) => f.startsWith("src/") || f.startsWith("test/")),
  shipped.filter((f) => f.startsWith("src/") || f.startsWith("test/")).join(" "),
);
check(
  "it leaves the dev manifest behind",
  !shipped.some((f) => ["tsconfig.json", "pnpm-lock.yaml"].includes(f)),
);
check(
  "it ships no source map that points at files it does not carry",
  !shipped.some((f) => f.endsWith(".map")),
);

fs.rmSync(archive, { recursive: true, force: true });

console.log(
  failures.length === 0
    ? "vsix: all checks passed"
    : `vsix: ${failures.length} check(s) failed`,
);
if (failures.length > 0) process.exitCode = 1;
