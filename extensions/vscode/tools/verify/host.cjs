// BW-20-03-01 / 03-03 evidence: run the extension the VSIX ships inside a host
// that presents the editor's API over a Demo workspace on disk, and check the
// commands it registers do what a Boundless workspace expects.
const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const Module = require("node:module");
const os = require("node:os");
const path = require("node:path");

const { EXTENSION_DIR } = require("./paths.cjs");
const manifest = JSON.parse(
  fs.readFileSync(path.join(EXTENSION_DIR, "package.json"), "utf8"),
);

const failures = [];
const check = (label, condition, detail) => {
  console.log(
    `${condition ? "PASS" : "FAIL"} ${label}${detail ? ` — ${detail}` : ""}`,
  );
  if (!condition) failures.push(label);
};

/** The artifact the user installs: the compiled extension inside the VSIX. */
const vsix = path.join(
  EXTENSION_DIR,
  `${manifest.name}-${manifest.version}.vsix`,
);
if (!fs.existsSync(vsix)) {
  execFileSync(path.join(EXTENSION_DIR, "node_modules", ".bin", "vsce"), [
    "package",
    "--no-dependencies",
  ], { cwd: EXTENSION_DIR, stdio: "inherit" });
}
// Unzip outside the extension folder: the packager would ship the archive.
const unpacked = fs.mkdtempSync(path.join(os.tmpdir(), "bwhost-"));
execFileSync("unzip", ["-q", vsix, "-d", unpacked]);
const entryPoint = path.join(unpacked, "extension", manifest.main.slice(2));
check("the package ships its entry point", fs.existsSync(entryPoint), entryPoint);

/** A Demo workspace, laid out the way a Boundless project is. */
const workspace = fs.mkdtempSync(path.join(os.tmpdir(), "bwdemo-"));
const resRoot = path.join(workspace, "res");
const defsDir = path.join(resRoot, "scripts", "entity_defs");
fs.mkdirSync(path.join(resRoot, "scripts", "base"), { recursive: true });
fs.mkdirSync(path.join(resRoot, "scripts", "client"), { recursive: true });
fs.mkdirSync(defsDir, { recursive: true });
fs.mkdirSync(path.join(workspace, "docs", "guide"), { recursive: true });

const script = ["class Clean:", "\tdef logOn( self ):", "\t\tpass", ""].join("\n");
const write = (file, text) =>
  fs.writeFileSync(path.join(resRoot, "scripts", file), text, "utf8");
write("entities.xml", "<root/>");
write(
  "entity_defs/alias.xml",
  "<root>\n\t<HEALTH>\t\tINT32\t\t</HEALTH>\n</root>",
);
write(
  "entity_defs/Clean.def",
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
write(
  "entity_defs/BadFlags.def",
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
write("base/Clean.py", script);
write("client/Clean.py", script);
fs.writeFileSync(
  path.join(workspace, "docs", "guide", "entities.md"),
  "# Entities\n\n## Defining properties\n\nFlags decide the data of a cell.\n",
  "utf8",
);

/* ------------------------------------------------------------------ */
/* The host: the editor's API over that workspace. */

const commands = new Map();
const channels = new Map();
const diagnostics = { set: new Map(), delete: () => {} };
let definitionProvider = null;
const openers = [];

const position = (line, character) => ({ line, character });
const range = (start, end) => ({
  startLine: start[0],
  startCharacter: start[1],
  endLine: end[0],
  endCharacter: end[1],
});
const location = (uri, pos) => ({ uri, position: pos });

class Uri {
  constructor(fsPath) {
    this.fsPath = fsPath;
  }
  toString() {
    return `file://${this.fsPath}`;
  }
}
const uriFile = (fsPath) => new Uri(fsPath);

/** The defaults the editor would hand over: the ones the manifest declares. */
const properties = manifest.contributes.configuration.properties ?? {};
const defaults = (section) => {
  const prefix = `${section}.`;
  return (key, def) => {
    const declared = properties[prefix + key];
    return declared && "default" in declared ? declared.default : def;
  };
};

const vscode = {
  workspace: {
    workspaceFolders: [{ uri: uriFile(workspace), name: "demo", index: 0 }],
    textDocuments: [],
    /** The Demo workspace carries no user settings, so the manifest wins. */
    getConfiguration(section) {
      return { get: defaults(section) };
    },
    onDidChangeWorkspaceFolders: () => ({}),
    onDidOpenTextDocument: (cb) => {
      openers.push(cb);
      return {};
    },
    onDidChangeTextDocument: () => ({}),
    onDidSaveTextDocument: () => ({}),
    onDidCloseTextDocument: () => ({}),
    async openTextDocument(uri) {
      const fileName = uri.fsPath;
      const readText = () => fs.readFileSync(fileName, "utf8");
      const document = {
        uri,
        fileName,
        getText: readText,
        offsetAt(pos) {
          return (
            readText()
              .split("\n")
              .slice(0, pos.line)
              .reduce((sum, line) => sum + line.length + 1, 0) + pos.character
          );
        },
      };
      vscode.workspace.textDocuments.push(document);
      for (const opener of openers) opener(document);
      return document;
    },
  },
  window: {
    errors: [],
    notes: [],
    createOutputChannel(name) {
      const channel = {
        name,
        lines: [],
        appendLine(line) {
          this.lines.push(line);
        },
        show() {},
        clear() {
          this.lines = [];
        },
        dispose() {},
      };
      channels.set(name, channel);
      return channel;
    },
    showErrorMessage(message) {
      vscode.window.errors.push(message);
    },
    showInformationMessage(message) {
      vscode.window.notes.push(message);
    },
    async showQuickPick() {
      return undefined;
    },
    async showTextDocument() {},
  },
  commands: {
    registerCommand(id, handler) {
      commands.set(id, handler);
      return { dispose() {} };
    },
  },
  languages: {
    createDiagnosticCollection() {
      return {
        set(uri, list) {
          diagnostics.set.set(uri.fsPath, list);
        },
        delete(uri) {
          diagnostics.set.delete(uri.fsPath);
        },
        clear() {},
        dispose() {},
      };
    },
    registerDefinitionProvider(_selector, provider) {
      definitionProvider = provider;
      return { dispose() {} };
    },
  },
  Uri: { file: uriFile },
  Range,
  Position,
  Location,
  Diagnostic,
  DiagnosticSeverity: { Error: 0, Warning: 1, Information: 2, Hint: 3 },
};

function Position(line, character) {
  return position(line, character);
}
function Range(startLine, startCharacter, endLine, endCharacter) {
  return range([startLine, startCharacter], [endLine, endCharacter]);
}
/** The editor's own order of arguments: the range comes first. */
function Diagnostic(range, message, data, severity) {
  return { range, message, data, severity };
}
function Location(uri, pos) {
  return location(uri, pos);
}

const originalLoad = Module._load;
Module._load = function (request, parent, isMain) {
  if (request === "vscode") return vscode;
  return originalLoad.apply(this, arguments);
};

const { activate, deactivate } = require(entryPoint);

/* ------------------------------------------------------------------ */
/* The Demo workspace's core actions. */

async function main() {
  activate({ subscriptions: [] });

  const contributed = manifest.contributes.commands.map((c) => c.command);
  check(
    "the host registers every command the manifest contributes",
    contributed.every((id) => commands.has(id)),
    contributed.filter((id) => !commands.has(id)).join(" "),
  );
  check(
    "it opens the cluster and docs channels",
    channels.has("Boundless") && channels.has("Boundless Cluster"),
    [...channels.keys()].join(" / "),
  );

  // The documentation knowledge base: the command writes the editor's MCP config.
  await commands.get("boundless.setupDocsMcp")();
  const mcpFile = path.join(workspace, ".vscode", "mcp.json");
  check("it writes .vscode/mcp.json", fs.existsSync(mcpFile), mcpFile);
  const entry = JSON.parse(fs.readFileSync(mcpFile, "utf8")).servers["boundless-docs"];
  check("the server entry is a stdio server", entry?.type === "stdio", entry?.type);
  check(
    "it points at the configured server binary",
    typeof entry?.command === "string" && entry.command.includes("boundless-docs-mcp"),
    entry?.command,
  );
  const args = entry?.args ?? [];
  check(
    "it serves the documentation of the workspace folder",
    args.includes("--root") && args[args.indexOf("--root") + 1] === "${workspaceFolder}",
    JSON.stringify(args),
  );
  check(
    "it answers for the version the workspace is at",
    args.includes("--version") && args[args.indexOf("--version") + 1] === "HEAD",
    JSON.stringify(args),
  );

  // Debug configurations: written into launch.json, per role and per platform.
  await commands.get("boundless.setupPythonDebug")();
  await commands.get("boundless.setupRustDebug")();
  const launchFile = path.join(workspace, ".vscode", "launch.json");
  const configurations = JSON.parse(fs.readFileSync(launchFile, "utf8")).configurations;
  const byName = new Map(configurations.map((c) => [c.name, c]));
  check(
    "it writes one attach and one launch configuration per role",
    configurations.length === 4,
    configurations.map((c) => c.name).join(" "),
  );
  check(
    "the launch configuration uses this platform's debugger",
    byName.get("Boundless: Launch baseapp")?.type ===
      (process.platform === "win32" ? "cppvsdbg" : "lldb"),
    byName.get("Boundless: Launch baseapp")?.type,
  );
  check(
    "the launch configuration points at the built role binary",
    (byName.get("Boundless: Launch cellapp")?.program ?? "").includes(
      "boundless-server",
    ),
    byName.get("Boundless: Launch cellapp")?.program,
  );

  // The workspace report names the resource root it found.
  await commands.get("boundless.showWorkspaceInfo")();
  const report = channels.get("Boundless").lines.join("\n");
  check(
    "it reports the resource root of the Demo workspace",
    report.includes("res root:") &&
      report.includes(
        path.relative(workspace, resRoot).split(path.sep).join("/"),
      ),
    report
      .split("\n")
      .filter((line) => line.includes("res root"))
      .join(" "),
  );

  // A dirty `.def` gets a diagnostic; a clean one stays quiet.
  const dirty = path.join(defsDir, "BadFlags.def");
  const clean = path.join(defsDir, "Clean.def");
  await vscode.workspace.openTextDocument(uriFile(dirty));
  await vscode.workspace.openTextDocument(uriFile(clean));
  const found = diagnostics.set.get(dirty) ?? [];
  check(
    "it diagnoses the invalid Flags section",
    found.length === 1 && found[0].message.includes("ON_NO_CELLS"),
    found.map((d) => d.message).join(" | "),
  );
  check(
    "it leaves a clean document without diagnostics",
    (diagnostics.set.get(clean) ?? []).length === 0,
  );

  // A method element jumps to the role script that really has it.
  const text = fs.readFileSync(clean, "utf8");
  const at = text.indexOf("logOn");
  const line = text.slice(0, at).split("\n").length - 1;
  const character = at - text.slice(0, at).lastIndexOf("\n") - 1;
  const cleanDoc = vscode.workspace.textDocuments.find(
    (d) => d.fileName === clean,
  );
  const targets = await definitionProvider.provideDefinition(
    cleanDoc,
    position(line, character),
  );
  check(
    "it jumps from the method element to the real script",
    targets?.length === 1 &&
      targets[0].uri.fsPath === path.join(resRoot, "scripts", "base", "Clean.py"),
    targets?.map((t) => t.uri.fsPath).join(" "),
  );

  deactivate();

  console.log(
    failures.length === 0
      ? `host: all checks passed on ${process.platform}`
      : `host: ${failures.length} check(s) failed`,
  );
  if (failures.length > 0) process.exitCode = 1;
}

main().catch((err) => {
  console.error(`host: ${err.message}`);
  process.exitCode = 1;
});
