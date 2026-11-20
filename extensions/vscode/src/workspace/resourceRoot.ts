/**
 * Locating the Boundless/BigWorld resource root of a workspace.
 *
 * The reference resolves every entity path relative to the resource path it is
 * given (`lib/entitydef/constants.hpp`, `EntityDef::Constants`): the res root
 * holds a `scripts/` directory, and `scripts/entities.xml` is the marker the
 * reference itself loads (`entitiesFile()`). Base, cell, client, service and
 * editor scripts live in fixed subdirectories of it.
 *
 * The filesystem is injected as a probe so the search can be unit tested
 * without the editor API and without touching the real tree.
 */

import { sep, posix } from "path";

/** The reference's own path constants (`EntityDef::Constants`). */
export const RESOURCE_PATHS = {
  entitiesFile: "scripts/entities.xml",
  entityDefsDir: "scripts/entity_defs",
  aliasFile: "scripts/entity_defs/alias.xml",
  serviceDefsDir: "scripts/service_defs",
  componentDefsDir: "scripts/component_defs",
  capabilitiesFile: "scripts/common/capabilities.xml",
  userDataObjectsFile: "scripts/user_data_objects.xml",
  basePath: "scripts/base",
  cellPath: "scripts/cell",
  clientPath: "scripts/client",
  servicePath: "scripts/service",
  editorPath: "scripts/editor",
} as const;

/** The script roles the reference has, plus the editor's. */
export type ScriptRole = "base" | "cell" | "client" | "service" | "editor";

export const SCRIPT_DIRS: Record<ScriptRole, string> = {
  base: RESOURCE_PATHS.basePath,
  cell: RESOURCE_PATHS.cellPath,
  client: RESOURCE_PATHS.clientPath,
  service: RESOURCE_PATHS.servicePath,
  editor: RESOURCE_PATHS.editorPath,
};

/** Everything the rest of the extension needs to find entity definitions. */
export interface ResourceRoot {
  /** The workspace folder the root was found inside. */
  folder: string;
  /** The resource root: the directory that contains `scripts/`. */
  resRoot: string;
  /** `<resRoot>/scripts`. */
  scripts: string;
  /** `<scripts>/entities.xml`. */
  entitiesFile: string;
  /** `<scripts>/entity_defs/alias.xml`. */
  aliasFile: string;
  /** `<scripts>/entity_defs`. */
  entityDefsDir: string;
  /** `<scripts>/service_defs`. */
  serviceDefsDir: string;
  /** One directory per script role. */
  scriptDirs: Readonly<Record<ScriptRole, string>>;
}

/** Read-only access to the filesystem, injected by the adapter. */
export interface FilesystemProbe {
  /** True when `path` exists and is a file. */
  isFile(path: string): boolean;
  /** Immediate children of a directory, or `null` when it is not a directory. */
  listDir(path: string): string[] | null;
}

/** Directories that never contain a resource root and are large enough to make
 * the search useless if descended into. */
const SKIP_DIRS = new Set([
  "node_modules",
  ".git",
  "out",
  "target",
  "dist",
  "build",
  "__pycache__",
]);

/** Upper bound on directory lookups per workspace folder, so an accidentally
 * opened giant tree cannot hang the activation. */
const MAX_LOOKUPS = 4000;

function join(base: string, ...parts: string[]): string {
  const s = parts.length > 0 && base.endsWith(sep) ? base.slice(0, -1) : base;
  return parts.length === 0 ? s : s + sep + parts.join(sep);
}

/** The directory that holds `scripts/`, or `null`. */
function scriptsDirOf(resRoot: string): string {
  return join(resRoot, "scripts");
}

/** True when `resRoot` really is a resource root. */
function isResourceRoot(probe: FilesystemProbe, resRoot: string): boolean {
  return probe.isFile(join(resRoot, RESOURCE_PATHS.entitiesFile));
}

/**
 * The shallowest resource root reachable from one workspace folder, if any.
 *
 * A res root is a directory that holds `scripts/entities.xml`, either the folder
 * itself or a `res/` child of some subdirectory (`examples/…/simple/res` is the
 * layout of the reference examples). Breadth-first search makes the shallowest
 * root win, which is the one the user actually opened.
 */
function findResRoot(probe: FilesystemProbe, folder: string): string | null {
  const queue: string[] = [folder];
  let lookups = 0;

  while (queue.length > 0 && lookups < MAX_LOOKUPS) {
    const dir = queue.shift()!;
    lookups++;

    if (isResourceRoot(probe, dir)) return dir;

    const resChild = join(dir, "res");
    if (isResourceRoot(probe, resChild)) return resChild;

    const children = probe.listDir(dir);
    if (!children) continue;
    for (const child of children) {
      if (child.startsWith(".") || child === "res" || SKIP_DIRS.has(child)) continue;
      const sub = join(dir, child);
      if (probe.listDir(sub) !== null) queue.push(sub);
    }
  }

  return null;
}

/** Builds a {@link ResourceRoot} from a directory that holds `scripts/`. */
function describe(folder: string, resRoot: string): ResourceRoot {
  const scripts = scriptsDirOf(resRoot);
  return {
    folder,
    resRoot,
    scripts,
    entitiesFile: join(resRoot, RESOURCE_PATHS.entitiesFile),
    aliasFile: join(resRoot, RESOURCE_PATHS.aliasFile),
    entityDefsDir: join(resRoot, RESOURCE_PATHS.entityDefsDir),
    serviceDefsDir: join(resRoot, RESOURCE_PATHS.serviceDefsDir),
    scriptDirs: {
      base: join(resRoot, RESOURCE_PATHS.basePath),
      cell: join(resRoot, RESOURCE_PATHS.cellPath),
      client: join(resRoot, RESOURCE_PATHS.clientPath),
      service: join(resRoot, RESOURCE_PATHS.servicePath),
      editor: join(resRoot, RESOURCE_PATHS.editorPath),
    },
  };
}

/** Finds a resource root in each workspace folder, in folder order.
 *
 * Multi-root workspaces yield one entry per folder that holds a resource root,
 * so a client project and a server project opened side by side are both
 * served instead of the first one shadowing the second. */
export function findResourceRoots(
  probe: FilesystemProbe,
  folders: readonly string[],
): ResourceRoot[] {
  const roots: ResourceRoot[] = [];
  for (const folder of folders) {
    const resRoot = findResRoot(probe, folder);
    if (resRoot) roots.push(describe(folder, resRoot));
  }
  return roots;
}

/** The first resource root, if any. */
export function findResourceRoot(
  probe: FilesystemProbe,
  folders: readonly string[],
): ResourceRoot | undefined {
  return findResourceRoots(probe, folders)[0];
}

/** Where the reference looks for the `.def` of `name`
 * (`entitiesDefsPath()/<Name>.def`). */
export function entityDefPath(root: ResourceRoot, name: string): string {
  return join(root.entityDefsDir, `${name}.def`);
}

/** Where the reference looks for the role script of `name`
 * (`scripts/<role>/<Name>.py`). */
export function scriptPath(root: ResourceRoot, role: ScriptRole, name: string): string {
  return join(root.scriptDirs[role], `${name}.py`);
}

/** Joins for display: normalises to forward slashes. */
export function displayPath(path: string): string {
  return path.split(sep).join(posix.sep);
}
