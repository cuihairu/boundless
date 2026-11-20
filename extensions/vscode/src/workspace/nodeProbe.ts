/**
 * The {@link FilesystemProbe} implementation the extension uses, backed by
 * `node:fs`. Kept separate from the search so the search itself has no
 * platform dependency.
 */

import { readdirSync, readFileSync, statSync } from "node:fs";

import { FilesystemProbe } from "./resourceRoot";
import { NavigationProbe } from "./navigation";

/** A probe over the real filesystem, for both discovery and navigation. */
export function nodeProbe(): FilesystemProbe & NavigationProbe {
  return {
    isFile(path) {
      try {
        return statSync(path).isFile();
      } catch {
        return false;
      }
    },
    listDir(path) {
      try {
        const entries = readdirSync(path, { withFileTypes: true });
        return entries.map((e) => e.name);
      } catch {
        return null;
      }
    },
    read(path) {
      try {
        return readFileSync(path, "utf8");
      } catch {
        return null;
      }
    },
  };
}
