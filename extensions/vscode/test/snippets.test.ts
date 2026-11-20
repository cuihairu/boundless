import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { AliasRegistry } from "../src/def/registry";
import { parseEntityDef } from "../src/def/defFile";
import { checkEntityDef } from "../src/def/checker";
import { ALIAS_XML } from "./fixtures";

interface Snippet {
  prefix: string;
  body: string[];
  description?: string;
}

const here = dirname(fileURLToPath(import.meta.url));
const SNIPPETS_PATH = join(here, "..", "snippets", "entities.json");
const snippets = JSON.parse(readFileSync(SNIPPETS_PATH, "utf8")) as Record<
  string,
  Snippet
>;

/** Turns a TextMate snippet body into the text the editor would insert. */
function materialise(body: string[]): string {
  return body
    .join("\n")
    .replace(/\$\{\d+:([^}]*)\}/g, "$1")
    .replace(/\$\{\d+\}/g, "")
    .replace(/\$\d+/g, "")
    .replace(/\\\$/g, "$");
}

/** Wraps a fragment in the smallest document the checker can parse. */
function root(...inner: string[]): string {
  return `<root>${inner.join("")}<BaseMethods/><CellMethods/><ClientMethods/></root>`;
}

/** How each snippet is embedded for checking, and whether it is a service. */
const WRAPPERS: Record<string, { wrap: (body: string) => string; service?: boolean }> = {
  "bw-prop": { wrap: (b) => root(`<Properties>${b}</Properties>`) },
  "bw-prop-persistent": { wrap: (b) => root(`<Properties>${b}</Properties>`) },
  "bw-prop-array": { wrap: (b) => root(`<Properties>${b}</Properties>`) },
  "bw-prop-user-type": { wrap: (b) => root(`<Properties>${b}</Properties>`) },
  "bw-base-method": { wrap: (b) => root(`<BaseMethods>${b}</BaseMethods>`) },
  "bw-cell-method": { wrap: (b) => root(`<CellMethods>${b}</CellMethods>`) },
  "bw-client-method": { wrap: (b) => root(`<ClientMethods>${b}</ClientMethods>`) },
  "bw-service-method": {
    wrap: (b) => `<root><Methods>${b}</Methods></root>`,
    service: true,
  },
  "bw-method-arg": {
    wrap: (b) => root(`<BaseMethods><onLoad>${b}</onLoad></BaseMethods>`),
  },
  "bw-volatile": { wrap: (b) => root(b) },
  "bw-distribution": { wrap: (b) => root(b) },
  "bw-lod": { wrap: (b) => root(b) },
};

/** Snippets that are already whole documents, keyed by their prefix. */
const COMPLETE: Record<string, boolean> = {
  "bw-entity": false,
  "bw-service": true,
};

/** Snippets that belong to `alias.xml` instead of a `.def`. */
const ALIAS_SNIPPETS = new Set(["bw-alias"]);

const aliases = AliasRegistry.parse(ALIAS_XML);

describe("entity snippets", () => {
  it("declares a prefix for every snippet", () => {
    for (const [title, snippet] of Object.entries(snippets)) {
      expect(snippet.prefix, `${title} has no prefix`).toMatch(/^bw-/);
      expect(snippet.body.length, `${title} has an empty body`).toBeGreaterThan(0);
    }
  });

  it("covers the whole EntityDef surface", () => {
    const prefixes = Object.values(snippets).map((s) => s.prefix);
    for (const needed of [
      "bw-entity",
      "bw-service",
      "bw-prop",
      "bw-prop-persistent",
      "bw-prop-array",
      "bw-prop-user-type",
      "bw-base-method",
      "bw-cell-method",
      "bw-client-method",
      "bw-service-method",
      "bw-method-arg",
      "bw-volatile",
      "bw-distribution",
      "bw-lod",
      "bw-alias",
    ]) {
      expect(prefixes, `missing snippet ${needed}`).toContain(needed);
    }
  });

  it("inserts content the EntityDef checker accepts", () => {
    for (const [title, snippet] of Object.entries(snippets)) {
      const inserted = materialise(snippet.body);
      const prefix = snippet.prefix;

      if (prefix in COMPLETE) {
        const parsed = parseEntityDef(inserted, "Inserted", COMPLETE[prefix]);
        const diagnostics = checkEntityDef(parsed, { aliases });
        expect(
          diagnostics.filter((d) => d.severity === "error"),
          `${title} produced errors:\n${inserted}\n${diagnostics.map((d) => d.message).join("\n")}`,
        ).toEqual([]);
        continue;
      }

      if (ALIAS_SNIPPETS.has(prefix)) {
        const registry = AliasRegistry.parse(`<root>${inserted}</root>`);
        expect(registry.names().length, `${title} produced no alias`).toBe(1);
        continue;
      }

      const wrapper = WRAPPERS[prefix];
      expect(wrapper, `no wrapper defined for ${prefix}`).toBeDefined();
      const parsed = parseEntityDef(wrapper!.wrap(inserted), "Inserted", wrapper!.service);
      const diagnostics = checkEntityDef(parsed, { aliases });
      expect(
        diagnostics.filter((d) => d.severity === "error"),
        `${title} produced errors:\n${wrapper!.wrap(inserted)}\n${diagnostics.map((d) => d.message).join("\n")}`,
      ).toEqual([]);
    }
  });

  it("leaves no TextMate placeholder in the inserted text", () => {
    for (const snippet of Object.values(snippets)) {
      expect(materialise(snippet.body), snippet.prefix).not.toMatch(/\$\{?\d/);
    }
  });

  it("keeps every snippet body parsable as an XML fragment", () => {
    for (const snippet of Object.values(snippets)) {
      expect(() => materialise(snippet.body), snippet.prefix).not.toThrow();
      expect(materialise(snippet.body).trim()).toMatch(/^</);
    }
  });
});
