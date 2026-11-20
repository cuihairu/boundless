/**
 * Extension entry point.
 *
 * The logic lives in the vscode-free modules (`workspace/`, `datasection/`,
 * `def/`) so it is testable without an editor; this file only translates the
 * editor's workspace folders into that logic and shows the result.
 */

import { basename, dirname, sep } from "node:path";

import * as vscode from "vscode";

import { nodeProbe } from "./workspace/nodeProbe";
import { definitionTargets } from "./workspace/navigation";
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
}

export function deactivate(): void {
  // Nothing to tear down: subscriptions are disposed by the editor.
}
