/**
 * The {@link FilesystemProbe} implementation the extension uses, backed by
 * `node:fs`. Kept separate from the search so the search itself has no
 * platform dependency.
 */

import { readdirSync, statSync } from "node:fs";

import { FilesystemProbe } from "./resourceRoot";

/** A probe over the real filesystem. */
export function nodeProbe(): FilesystemProbe {
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
  };
}
