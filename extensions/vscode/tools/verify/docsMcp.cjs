// BW-20-02-07 evidence: write `.vscode/mcp.json` with the extension's own
// writer, start the server that configuration names, and check that a query
// answers with the documentation of the version the workspace is at.
const { execFileSync, spawn } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const { OUT } = require("./paths.cjs");
const {
  DOCS_MCP_NAME,
  DOCS_MCP_DEFAULTS,
  docsMcpServer,
  mergeMcpJson,
} = require(path.join(OUT, "docsMcp.js"));

const DOC_CRATE = path.resolve(
  __dirname,
  "..",
  "..",
  "..",
  "..",
  "integrations",
  "mcp",
  "docs",
);
const SERVER_BIN = process.env.BW_DOCS_BIN
  ? path.resolve(process.env.BW_DOCS_BIN)
  : path.join(DOC_CRATE, "target", "release", "boundless-docs-mcp");

const failures = [];

/** The servers the run started, so none of them can hold the run open. */
const open = new Set();
process.on("exit", () => {
  for (const client of open) client.kill();
});

/** Reports one check and keeps going, so one run shows the whole picture. */
function check(label, condition, detail) {
  if (condition) {
    console.log(`PASS ${label}`);
  } else {
    failures.push(label);
    console.log(`FAIL ${label}${detail ? ` — ${detail}` : ""}`);
  }
}

const FIRST_EDITION = [
  "# Entities",
  "",
  "## Defining properties",
  "",
  "Flags decide the data of a cell.",
  "",
].join("\n");

const SECOND_EDITION = [
  "# Entities",
  "",
  "## Defining properties",
  "",
  "The second edition states that a cell owns its flags.",
  "",
].join("\n");

const ONLY_IN_V2 = [
  "# Second",
  "",
  "## Edition only",
  "",
  "Only the second edition carries this section.",
  "",
].join("\n");

/** A small documentation repository with two editions, tagged `v1` and `v2`. */
function fixture() {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "bwdocs-mcp-"));
  const git = (args) => execFileSync("git", args, { cwd: repo, stdio: "pipe" });
  git(["init", "-q", "-b", "main"]);
  git(["config", "user.email", "verify@boundless"]);
  git(["config", "user.name", "Boundless verify"]);

  const write = (file, text) => {
    fs.mkdirSync(path.dirname(path.join(repo, file)), { recursive: true });
    fs.writeFileSync(path.join(repo, file), text, "utf8");
  };

  write("docs/guide/entities.md", FIRST_EDITION);
  git(["add", "-A"]);
  git(["commit", "-q", "-m", "first edition"]);
  git(["tag", "v1"]);

  write("docs/guide/entities.md", SECOND_EDITION);
  write("docs/guide/second.md", ONLY_IN_V2);
  git(["add", "-A"]);
  git(["commit", "-q", "-m", "second edition"]);
  git(["tag", "v2"]);

  return { repo, git, write };
}

/** One JSON-RPC exchange with the server the configuration starts. */
class Client {
  constructor(command, args) {
    this.child = spawn(command, args, { stdio: "pipe" });
    this.pending = new Map();
    this.buffer = "";
    this.errors = [];
    this.nextId = 1;
    open.add(this);
    this.child.stderr.on("data", (chunk) => {
      this.errors.push(chunk.toString("utf8"));
    });
    this.child.stdout.on("data", (chunk) => {
      this.buffer += chunk.toString("utf8");
      let at;
      while ((at = this.buffer.indexOf("\n")) >= 0) {
        const line = this.buffer.slice(0, at).trim();
        this.buffer = this.buffer.slice(at + 1);
        if (!line) continue;
        const message = JSON.parse(line);
        const wait = this.pending.get(message.id);
        if (wait) {
          this.pending.delete(message.id);
          wait(message);
        }
      }
    });
  }

  call(method, params) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error(`no answer to ${method}`)),
        15000,
      );
      this.pending.set(id, (message) => {
        clearTimeout(timer);
        resolve(message);
      });
      this.child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    });
  }

  kill() {
    this.child.kill();
  }

  close() {
    // The answers are in; the server must not hold the run open.
    open.delete(this);
    this.child.stdin.end();
    this.child.stdout.removeAllListeners("data");
    this.child.stderr.removeAllListeners("data");
    this.child.kill();
  }
}

/** The result payload of a tool call, and whether the tool reported an error. */
function payload(message) {
  return {
    text: message?.result?.content?.[0]?.text ?? "",
    isError: message?.result?.isError === true,
  };
}

/** Writes `.vscode/mcp.json` the way the command writes it and starts the
 * server the document names, `${workspaceFolder}` substituted the way the
 * editor substitutes it. */
function serve(repo, options) {
  // The server binary lives outside the fixture workspace, which is the
  // extension's absolute-path case; the workspace folder itself is still
  // addressed through the editor's own variable.
  const entry = docsMcpServer({
    command: SERVER_BIN,
    root: "${workspaceFolder}",
    prefix: DOCS_MCP_DEFAULTS.prefix,
    version: options.version,
    langs: options.langs,
    docsUrl: options.docsUrl,
  });
  const file = path.join(repo, ".vscode", "mcp.json");
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(
    file,
    mergeMcpJson(
      fs.existsSync(file) ? fs.readFileSync(file, "utf8") : null,
      DOCS_MCP_NAME,
      entry,
    ),
    "utf8",
  );

  const written = JSON.parse(fs.readFileSync(file, "utf8"));
  const server = written.servers[DOCS_MCP_NAME];
  const command = server.command.replaceAll("${workspaceFolder}", repo);
  const args = server.args.map((a) => a.replaceAll("${workspaceFolder}", repo));
  console.log(`  wrote ${path.relative(repo, file)}:`, JSON.stringify(server, null, 0));
  return new Client(command, args);
}

async function main() {
  const { repo, git } = fixture();
  const current = git(["rev-parse", "--short", "HEAD"]).toString().trim();
  check(
    "the workspace is at the second edition",
    git(["rev-parse", "--short", "v2"]).toString().trim() === current,
  );
  console.log(`workspace repo ${repo} at ${current}`);

  // The command's defaults: the documentation the workspace is at.
  const client = serve(repo, {
    version: DOCS_MCP_DEFAULTS.version,
    langs: DOCS_MCP_DEFAULTS.langs,
    docsUrl: "https://docs.boundless.example/",
  });
  await client.call("initialize", { protocolVersion: "2025-06-18" });

  // `tools/list` is a protocol method, so its answer is the RPC result.
  const listed = (await client.call("tools/list", {})).result.tools.map((t) => t.name);
  check(
    "the server offers the doc tools",
    listed.includes("search_docs") && listed.includes("read_doc"),
    listed.join(","),
  );

  const found = payload(await client.call("tools/call", {
    name: "search_docs",
    arguments: { query: "second edition", limit: 5 },
  }));
  check("a query does not report a tool error", !found.isError, found.text);
  const hits = JSON.parse(found.text);
  check(
    "the query hits the page of the current edition",
    hits.length > 0 && hits[0].path === "guide/second.md",
    JSON.stringify(hits.map((h) => `${h.version}:${h.path}`)),
  );
  check(
    "the hit is labelled with the ref the configuration asked for",
    hits.every((h) => h.version === DOCS_MCP_DEFAULTS.version),
    JSON.stringify([...new Set(hits.map((h) => h.version))]),
  );

  const page = payload(await client.call("tools/call", {
    name: "read_doc",
    arguments: { path: "guide/second.md" },
  }));
  const doc = JSON.parse(page.text);
  check(
    "the page comes back from the ref the workspace is at",
    doc.version === DOCS_MCP_DEFAULTS.version,
    doc.version,
  );
  check(
    "the page comes from the commit the workspace is at",
    doc.commit === current,
    `${doc.commit} != ${current}`,
  );
  check(
    "the source link opens on the rendered site",
    doc.source === "https://docs.boundless.example/guide/second.html",
    doc.source,
  );
  client.close();

  // The same configuration with `boundless.docs.version` pointed at the older
  // edition must answer for that edition, and nothing from the newer one.
  const older = serve(repo, {
    version: "v1",
    langs: DOCS_MCP_DEFAULTS.langs,
    docsUrl: "",
  });
  await older.call("initialize", { protocolVersion: "2025-06-18" });

  const stale = payload(await older.call("tools/call", {
    name: "search_docs",
    arguments: { query: "second edition" },
  }));
  check(
    "an older version has no hit from the newer edition",
    JSON.parse(stale.text).length === 0,
    JSON.stringify(JSON.parse(stale.text)),
  );

  const flags = payload(await older.call("tools/call", {
    name: "read_doc",
    arguments: { path: "guide/entities.md" },
  }));
  const oldDoc = JSON.parse(flags.text);
  const oldBody = oldDoc.sections
    .map((s) => s.body)
    .join("\n")
    .replaceAll("\n", " ")
    .trim();
  check(
    "an older version answers with its own wording",
    oldDoc.version === "v1" && oldBody.includes("Flags decide the data of a cell."),
    `${oldDoc.version}: ${oldBody}`,
  );
  check(
    "without a site base the link stays repository-relative",
    oldDoc.source === "docs/guide/entities.html",
    oldDoc.source,
  );
  older.close();

  console.log(
    failures.length === 0
      ? "docsMcp: all checks passed"
      : `docsMcp: ${failures.length} check(s) failed`,
  );
  if (failures.length > 0) process.exitCode = 1;
}

if (!fs.existsSync(SERVER_BIN)) {
  console.log("building boundless-docs-mcp for release");
  execFileSync("cargo", ["build", "--release"], { cwd: DOC_CRATE, stdio: "inherit" });
}

main().catch((err) => {
  console.error(`docsMcp: ${err.message}`);
  process.exitCode = 1;
});
