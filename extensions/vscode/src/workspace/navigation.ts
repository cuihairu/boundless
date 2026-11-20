/**
 * Going from an entity definition to the script that implements it.
 *
 * The reference pairs a `.def` with its scripts purely by path
 * (`EntityDef::Constants::entitiesBasePath()` etc. plus the entity name, which
 * itself comes from the `.def` file name), and it probes `scripts/<role>/<Name>.py`
 * for existence when deciding whether an entity can run on a component
 * (`HasScriptOrTagDistributionDecider`, `entity_description.cpp:72`). The same
 * paths are the navigation targets here.
 */

import { DataSection, findChild } from "../datasection/xmlSection";
import { EntityDef, MethodInfo, parseEntityDef } from "../def/defFile";
import {
  ResourceRoot,
  ScriptRole,
  entityDefPath,
  scriptPath,
} from "./resourceRoot";

/** Read access to a script's text, injected so this stays testable. */
export interface NavigationProbe {
  isFile(path: string): boolean;
  read(path: string): string | null;
}

/** Where a go-to-definition should land. */
export interface DefinitionResult {
  /** Candidate files, most specific first. Empty when there is nothing to go to. */
  paths: string[];
  /** Zero-based line inside `paths[0]`, when a symbol was located in it. */
  line?: number;
}

/** The end offset of an element: the last of its own ranges and its children's. */
function spanEnd(node: DataSection): number {
  let end = Math.max(node.nameOffset + node.nameLength, node.valueOffset + node.valueLength);
  for (const child of node.children) {
    end = Math.max(end, spanEnd(child));
  }
  return end;
}

function contains(node: DataSection, offset: number): boolean {
  return offset >= node.nameOffset && offset <= spanEnd(node);
}

function inValue(node: DataSection, offset: number): boolean {
  return (
    offset >= node.valueOffset && offset < node.valueOffset + node.valueLength
  );
}

/** The innermost element of `doc` whose range covers `offset`. */
export function containingSection(doc: DataSection, offset: number): DataSection | undefined {
  let best: DataSection | undefined;
  const walk = (node: DataSection): void => {
    if (!contains(node, offset)) return;
    best = node;
    for (const child of node.children) walk(child);
  };
  walk(doc);
  return best;
}

/** The zero-based line of `def <name>` in a script, or `undefined`.
 * Tabs are the reference's own style, so indentation is ignored entirely. */
export function symbolLine(text: string | null, name: string): number | undefined {
  if (!text) return undefined;
  const re = new RegExp(`^\\s*def\\s+${name}\\s*\\(`);
  const lines = text.split("\n");
  for (let i = 0; i < lines.length; i++) {
    if (re.test(lines[i])) return i;
  }
  return undefined;
}

/** The role a method component maps to. */
function roleOf(component: MethodInfo["component"]): ScriptRole {
  return component === "service" ? "service" : component;
}

/** Resolves the navigation targets for a cursor position in a `.def`. */
export function definitionTargets(
  resRoot: ResourceRoot,
  probe: NavigationProbe,
  text: string,
  offset: number,
  defName: string,
  isService = false,
): DefinitionResult {
  const def: EntityDef = parseEntityDef(text, defName, isService);

  // `<Parent>` names another entity: its definition file is the target.
  const parent = findChild(def.root, "Parent");
  if (parent && inValue(parent, offset)) {
    const path = entityDefPath(resRoot, parent.value);
    return { paths: probe.isFile(path) ? [path] : [] };
  }

  // A method element: the component's own script, at the symbol if present.
  const method = def.methods.find((m) => contains(m.node, offset));
  if (method) {
    const path = scriptPath(resRoot, roleOf(method.component), defName);
    if (!probe.isFile(path)) return { paths: [] };
    const line = symbolLine(probe.read(path), method.name);
    return line === undefined ? { paths: [path] } : { paths: [path], line };
  }

  // Anything else in the file: the entity's own scripts, every role it has.
  const roles: ScriptRole[] = isService ? ["service"] : ["base", "cell", "client"];
  return {
    paths: roles
      .map((role) => scriptPath(resRoot, role, defName))
      .filter((path) => probe.isFile(path)),
  };
}
