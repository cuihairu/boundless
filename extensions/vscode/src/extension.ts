/**
 * Extension entry point.
 *
 * The logic lives in the vscode-free modules (`workspace/`, `datasection/`,
 * `def/`) so it is testable without an editor; this file only translates the
 * editor's workspace folders into that logic and shows the result.
 */

import * as vscode from "vscode";

import { nodeProbe } from "./workspace/nodeProbe";
import {
  ResourceRoot,
  displayPath,
  findResourceRoots,
} from "./workspace/resourceRoot";

let roots: ResourceRoot[] = [];

/** The workspace folders the editor gives us, in editor order. */
function folderPaths(): string[] {
  return (vscode.workspace.workspaceFolders ?? []).map((f) => f.uri.fsPath);
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
}

export function deactivate(): void {
  // Nothing to tear down: subscriptions are disposed by the editor.
}
