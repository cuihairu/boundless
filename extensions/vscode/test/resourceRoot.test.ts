import { describe, expect, it } from "vitest";

import {
  FilesystemProbe,
  entityDefPath,
  findResourceRoots,
  scriptPath,
} from "../src/workspace/resourceRoot";

/**
 * Builds a filesystem probe from a map of file paths to arbitrary contents.
 * Directories are derived from the file paths, so a path such as
 * `a/res/scripts/entities.xml` yields the directories `a`, `a/res`,
 * `a/res/scripts`.
 */
function makeFs(files: Record<string, string>): FilesystemProbe {
  const dirs = new Map<string, string[]>();
  for (const path of Object.keys(files)) {
    const parts = path.split("/");
    for (let i = 1; i < parts.length; i++) {
      const dir = parts.slice(0, i).join("/");
      const child = parts[i];
      const children = dirs.get(dir) ?? [];
      if (!children.includes(child)) children.push(child);
      dirs.set(dir, children);
    }
  }
  return {
    isFile: (p) => p in files,
    listDir: (p) => (dirs.has(p) ? [...dirs.get(p)!.sort()] : null),
  };
}

/** The example layout: `.../simple/res/scripts/`. */
const EXAMPLE = {
  "examples/client_integration/python/simple/res/scripts/entities.xml": "<root/>",
  "examples/client_integration/python/simple/res/scripts/entity_defs/alias.xml": "<root/>",
  "examples/client_integration/python/simple/res/scripts/base/ClientAvatar.py": "",
};

describe("resource root discovery", () => {
  it("finds the res root of a single-root project", () => {
    const fs = makeFs({ "game/res/scripts/entities.xml": "<root/>" });
    const roots = findResourceRoots(fs, ["game"]);
    expect(roots).toHaveLength(1);
    expect(roots[0].resRoot).toBe("game/res");
    expect(roots[0].scripts).toBe("game/res/scripts");
    expect(roots[0].entitiesFile).toBe("game/res/scripts/entities.xml");
    expect(roots[0].aliasFile).toBe("game/res/scripts/entity_defs/alias.xml");
    expect(roots[0].scriptDirs.cell).toBe("game/res/scripts/cell");
    expect(roots[0].scriptDirs.service).toBe("game/res/scripts/service");
  });

  it("accepts a project whose scripts directory sits at the root", () => {
    const fs = makeFs({ "project/scripts/entities.xml": "<root/>" });
    const roots = findResourceRoots(fs, ["project"]);
    expect(roots).toHaveLength(1);
    expect(roots[0].resRoot).toBe("project");
  });

  it("descends into a nested project", () => {
    const roots = findResourceRoots(makeFs(EXAMPLE), ["examples"]);
    expect(roots).toHaveLength(1);
    expect(roots[0].resRoot).toBe(
      "examples/client_integration/python/simple/res",
    );
  });

  it("locates each root in a multi-root workspace", () => {
    const fs = makeFs({
      ...EXAMPLE,
      "server/res/scripts/entities.xml": "<root/>",
    });
    const roots = findResourceRoots(fs, ["examples", "server"]);
    expect(roots).toHaveLength(2);
    // Folder order is preserved and each root remembers its own folder.
    expect(roots[0].folder).toBe("examples");
    expect(roots[0].resRoot).toBe(
      "examples/client_integration/python/simple/res",
    );
    expect(roots[1].folder).toBe("server");
    expect(roots[1].resRoot).toBe("server/res");
  });

  it("skips folders that hold no resource root", () => {
    const fs = makeFs({ "docs/readme.md": "x" });
    expect(findResourceRoots(fs, ["docs"])).toEqual([]);
  });

  it("never descends into node_modules", () => {
    const fs = makeFs({
      "app/node_modules/pkg/res/scripts/entities.xml": "<root/>",
    });
    expect(findResourceRoots(fs, ["app"])).toEqual([]);
  });

  it("does not treat a scripts directory without entities.xml as a root", () => {
    const fs = makeFs({ "game/res/scripts/base/Avatar.py": "" });
    expect(findResourceRoots(fs, ["game"])).toEqual([]);
  });

  it("reports the reference paths for defs and scripts", () => {
    const roots = findResourceRoots(
      makeFs({ "game/res/scripts/entities.xml": "<root/>" }),
      ["game"],
    );
    expect(entityDefPath(roots[0], "ClientAvatar")).toBe(
      "game/res/scripts/entity_defs/ClientAvatar.def",
    );
    expect(scriptPath(roots[0], "base", "ClientAvatar")).toBe(
      "game/res/scripts/base/ClientAvatar.py",
    );
  });
});
