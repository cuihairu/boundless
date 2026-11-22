/**
 * Extension entry point.
 *
 * The logic lives in the vscode-free modules (`workspace/`, `datasection/`,
 * `def/`) so it is testable without an editor; this file only translates the
 * editor's workspace folders into that logic and shows the result.
 */

import { basename, dirname, isAbsolute, join, sep } from "node:path";

import * as vscode from "vscode";
import { mkdirSync, writeFileSync } from "node:fs";

import { nodeProbe } from "./workspace/nodeProbe";
import { definitionTargets } from "./workspace/navigation";
import { ScriptProbe, checkDefText } from "./workspace/diagnostics";
import {
  CLUSTER_ROLES,
  ClusterConfig,
  ClusterControl,
  ClusterRole,
} from "./workspace/cluster";
import { processRunner } from "./workspace/processRunner";
import { formatOutcome } from "./workspace/jobs";
import { nodeJobRunner } from "./workspace/jobRunner";
import {
  mergeLaunchJson,
  pythonAttachConfig,
  rustLaunchConfig,
} from "./workspace/launchConfig";
import {
  DOCS_MCP_DEFAULTS,
  DOCS_MCP_NAME,
  docsMcpServer,
  mergeMcpJson,
} from "./workspace/docsMcp";
import { AliasRegistry } from "./def/registry";
import { ScriptRole, scriptPath } from "./workspace/resourceRoot";
import {
  ResourceRoot,
  displayPath,
  findResourceRoots,
} from "./workspace/resourceRoot";

/** The workspace folder, as the editor's own variable: the configurations the
 * extension writes keep the editor's own substitution instead of a path it
 * would have to update itself. */
const WORKSPACE_FOLDER = "${workspaceFolder}";

let roots: ResourceRoot[] = [];

const cluster = new ClusterControl(processRunner());
const jobs = nodeJobRunner();

/** The workspace folders the editor gives us, in editor order. */
function folderPaths(): string[] {
  return (vscode.workspace.workspaceFolders ?? []).map((f) => f.uri.fsPath);
}

/** The resource root that the cluster commands use. */
function firstRoot(): ResourceRoot | undefined {
  refresh();
  return roots[0];
}

/** How to launch a role, from the resource root and the settings. */
function clusterConfig(root: ResourceRoot): ClusterConfig {
  const settings = vscode.workspace.getConfiguration("boundless.cluster");
  const binary = settings.get<string>("binary", "target/debug/boundless-server");
  return {
    binary: isAbsolute(binary) ? binary : join(root.folder, binary),
    resRoot: root.resRoot,
    cwd: root.folder,
    logDir: join(root.folder, ".boundless", "logs"),
  };
}

/** Which roles `boundless.cluster.roles` asks for, in the reference order. */
function configuredRoles(): ClusterRole[] {
  const wanted =
    vscode.workspace.getConfiguration("boundless.cluster").get<string[]>(
      "roles",
      ["baseapp", "cellapp"],
    ) ?? [];
  return CLUSTER_ROLES.filter((role) => wanted.includes(role));
}

/** The alias table of `root`, empty when the file is absent. */
function aliasesFor(root: ResourceRoot): AliasRegistry {
  return AliasRegistry.parse(nodeProbe().read(root.aliasFile) ?? "");
}

/** Whether the entity has a role script, the reference's component test
 * (`HasScriptOrTagDistributionDecider` probes `scripts/<role>/<Name>.py`). */
function scriptProbeFor(root: ResourceRoot, defName: string): ScriptProbe {
  const probe = nodeProbe();
  return {
    hasScript(role) {
      return probe.isFile(scriptPath(root, role as ScriptRole, defName));
    },
  };
}

/** The deepest resource root that contains `file`, if any. */
function rootFor(file: string): ResourceRoot | undefined {
  const covered = roots.filter((r) => file.startsWith(r.folder + sep));
  return covered.sort((a, b) => b.folder.length - a.folder.length)[0];
}

/** (Re)discovers the resource roots and reports them. */
function refresh(): void {
  roots = findResourceRoots(nodeProbe(), folderPaths());
}

/** Shows the located configuration, one line per workspace folder. */
function report(): string {
  if (roots.length === 0) {
    return "No Boundless resource root found. Open a folder that contains res/scripts/entities.xml.";
  }
  return roots
    .map((r) => {
      const scriptRoles = Object.keys(r.scriptDirs)
        .filter((role) => role !== "editor")
        .join("/");
      return [
        `folder: ${displayPath(r.folder)}`,
        `  res root:   ${displayPath(r.resRoot)}`,
        `  entities:   ${displayPath(r.entitiesFile)}`,
        `  alias:      ${displayPath(r.aliasFile)}`,
        `  defs:       ${displayPath(r.entityDefsDir)}`,
        `  scripts:    ${scriptRoles}`,
      ].join("\n");
    })
    .join("\n\n");
}

export function activate(context: vscode.ExtensionContext): void {
  refresh();

  const output = vscode.window.createOutputChannel("Boundless");
  context.subscriptions.push(output);

  context.subscriptions.push(
    vscode.workspace.onDidChangeWorkspaceFolders(() => {
      refresh();
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("boundless.showWorkspaceInfo", async () => {
      refresh();
      output.clear();
      output.appendLine(report());
      output.show();
    }),
  );

  const status = vscode.window.createOutputChannel("Boundless Cluster");
  context.subscriptions.push(status);

  context.subscriptions.push(
    vscode.commands.registerCommand("boundless.startCluster", async () => {
      const root = firstRoot();
      if (!root) {
        vscode.window.showErrorMessage(
          "No Boundless resource root found; nothing to start.",
        );
        return;
      }
      const config = clusterConfig(root);
      for (const role of configuredRoles()) {
        const info = await cluster.start(config, role);
        status.appendLine(
          `started ${info.role} (pid ${info.pid}); log ${displayPath(info.logPath)}`,
        );
      }
      status.show(true);
    }),
    vscode.commands.registerCommand("boundless.stopCluster", async () => {
      await cluster.stopAll();
      status.appendLine("cluster stopped");
      status.show(true);
    }),
    vscode.commands.registerCommand("boundless.showClusterStatus", () => {
      const running = cluster.roles();
      status.clear();
      if (running.length === 0) status.appendLine("no roles running");
      for (const info of running) {
        status.appendLine(
          `${info.role} pid ${info.pid} — ${displayPath(info.logPath)}`,
        );
      }
      status.show(true);
    }),
    vscode.commands.registerCommand("boundless.showClusterLogs", async () => {
      const running = cluster.roles();
      if (running.length === 0) {
        vscode.window.showInformationMessage("No roles running.");
        return;
      }
      const picked = await vscode.window.showQuickPick(
        running.map((i) => ({ label: i.role, logPath: i.logPath })),
        { placeHolder: "Which role log?" },
      );
      if (!picked) return;
      try {
        const doc = await vscode.workspace.openTextDocument(
          vscode.Uri.file(picked.logPath),
        );
        await vscode.window.showTextDocument(doc);
      } catch {
        vscode.window.showInformationMessage(
          `No log yet at ${displayPath(picked.logPath)}.`,
        );
      }
    }),
    vscode.commands.registerCommand("boundless.runTests", () =>
      runConfiguredJob("tests"),
    ),
    vscode.commands.registerCommand("boundless.runBots", () =>
      runConfiguredJob("bots"),
    ),
    vscode.commands.registerCommand("boundless.setupPythonDebug", () =>
      setupPythonDebug(),
    ),
    vscode.commands.registerCommand("boundless.setupRustDebug", () =>
      setupRustDebug(),
    ),
    vscode.commands.registerCommand("boundless.setupDocsMcp", () =>
      setupDocsMcp(),
    ),
  );

  /** Adds the Python attach configurations to every workspace folder's
   * `launch.json`, keeping what the user already had there. */
  function setupPythonDebug(): void {
    const settings = vscode.workspace.getConfiguration("boundless.cluster");
    const roles = settings.get<string[]>("launchRoles", configuredRoles());
    const configs = roles.map((role) => pythonAttachConfig(role));
    writeLaunchConfigurations(configs);
  }

  /** Adds the Rust launch configurations (one per configured role). */
  function setupRustDebug(): void {
    const settings = vscode.workspace.getConfiguration("boundless.cluster");
    const roles = settings.get<string[]>("launchRoles", configuredRoles());
    const program = settings.get<string>("binary", "target/debug/boundless-server");
    const configs = roles.map((role) =>
      rustLaunchConfig(role, {
        program: isAbsolute(program)
          ? program
          : `\${workspaceFolder}/${program}`,
      }),
    );
    writeLaunchConfigurations(configs);
  }

  /** Writes the documentation MCP server into every workspace folder's
   * `mcp.json`, keeping the servers the user already had there. */
  function setupDocsMcp(): void {
    const settings = vscode.workspace.getConfiguration("boundless.docs");
    const server = settings.get<string>("server", "");
    if (!server.trim()) {
      vscode.window.showInformationMessage(
        "No documentation MCP server configured — set boundless.docs.server.",
      );
      return;
    }
    const command = isAbsolute(server)
      ? server
      : `${WORKSPACE_FOLDER}/${server}`;
    const entry = docsMcpServer({
      command,
      root: settings.get<string>("root", WORKSPACE_FOLDER),
      prefix: settings.get<string>("prefix", DOCS_MCP_DEFAULTS.prefix),
      version: settings.get<string>("version", DOCS_MCP_DEFAULTS.version),
      langs:
        settings.get<string[]>("langs", [...DOCS_MCP_DEFAULTS.langs]) ?? [],
      docsUrl: settings.get<string>("url", DOCS_MCP_DEFAULTS.docsUrl),
    });
    for (const folder of vscode.workspace.workspaceFolders ?? []) {
      const file = join(folder.uri.fsPath, ".vscode", "mcp.json");
      const merged = mergeMcpJson(
        nodeProbe().read(file),
        DOCS_MCP_NAME,
        entry,
      );
      mkdirSync(dirname(file), { recursive: true });
      writeFileSync(file, merged, "utf8");
      status.appendLine(`wrote ${displayPath(file)}`);
    }
    status.show(true);
  }

  /** Merges configurations into every workspace folder's `launch.json`. */
  function writeLaunchConfigurations(
    configs: Record<string, unknown>[],
  ): void {
    for (const folder of vscode.workspace.workspaceFolders ?? []) {
      const file = join(folder.uri.fsPath, ".vscode", "launch.json");
      const existing = nodeProbe().read(file);
      const merged = mergeLaunchJson(existing, configs);
      mkdirSync(dirname(file), { recursive: true });
      writeFileSync(file, merged, "utf8");
      status.appendLine(`wrote ${displayPath(file)}`);
    }
    status.show(true);
  }

  const diagnostics = vscode.languages.createDiagnosticCollection(
    "boundless-entitydef",
  );
  context.subscriptions.push(diagnostics);

  /** Checks one `.def` document and publishes what it finds. */
  function publish(document: vscode.TextDocument): void {
    const file = document.fileName;
    if (!file.endsWith(".def")) return;
    let root = rootFor(file);
    if (!root) {
      refresh();
      root = rootFor(file);
    }
    if (!root) return;

    const defName = basename(file, ".def");
    const found = checkDefText(
      aliasesFor(root),
      document.getText(),
      defName,
      basename(dirname(file)) === "service_defs",
      scriptProbeFor(root, defName),
    );
    diagnostics.set(
      document.uri,
      found.map(
        (d) =>
          new vscode.Diagnostic(
            new vscode.Range(
              d.range.startLine,
              d.range.startCharacter,
              d.range.endLine,
              d.range.endCharacter,
            ),
            d.message,
            d.severity === "error"
              ? vscode.DiagnosticSeverity.Error
              : vscode.DiagnosticSeverity.Warning,
          ),
      ),
    );
  }

  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument(publish),
    vscode.workspace.onDidChangeTextDocument((e) => publish(e.document)),
    vscode.workspace.onDidSaveTextDocument(publish),
    vscode.workspace.onDidCloseTextDocument((d) => diagnostics.delete(d.uri)),
  );
  for (const doc of vscode.workspace.textDocuments) publish(doc);

  context.subscriptions.push(
    vscode.languages.registerDefinitionProvider("entities", {
      provideDefinition(document, position) {
        const file = document.fileName;
        if (!file.endsWith(".def")) return null;
        let root = rootFor(file);
        if (!root) {
          refresh();
          root = rootFor(file);
        }
        if (!root) return null;

        const result = definitionTargets(
          root,
          nodeProbe(),
          document.getText(),
          document.offsetAt(position),
          basename(file, ".def"),
          basename(dirname(file)) === "service_defs",
        );
        return result.paths.map((path) => {
          const line = result.line ?? 0;
          return new vscode.Location(
            vscode.Uri.file(path),
            new vscode.Position(line, 0),
          );
        });
      },
    }),
  );

  /** Runs the test or bots job from the settings, reporting status and
   * results to the cluster channel. */
  async function runConfiguredJob(kind: "tests" | "bots"): Promise<void> {
    const settings = vscode.workspace.getConfiguration("boundless.cluster");
    const defaults: Record<"tests" | "bots", string> = {
      tests: "cargo test",
      bots: "",
    };
    const command = settings
      .get<string>(`${kind}Command`, defaults[kind])
      .trim();
    const root = firstRoot();
    const cwd =
      root?.folder ??
      vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ??
      process.cwd();
    if (!command) {
      status.appendLine(
        `${kind}: not configured — set boundless.cluster.${kind}Command`,
      );
      status.show(true);
      return;
    }
    status.appendLine(`${kind}: running ${command} in ${displayPath(cwd)}`);
    const [bin, ...args] = command.split(/\s+/);
    const outcome = await jobs.run({
      name: kind,
      command: bin,
      args,
      cwd,
      resultPath: join(cwd, ".boundless", "reports", `${kind}.log`),
    });
    status.appendLine(`${kind}: ${formatOutcome(outcome)}`);
    if (outcome.output) status.appendLine(outcome.output);
    status.show(true);
  }
}

export function deactivate(): void {
  // Nothing to tear down: subscriptions are disposed by the editor.
}
