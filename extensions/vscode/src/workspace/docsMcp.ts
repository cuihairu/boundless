/**
 * The Model Context Protocol configuration the extension writes for the
 * documentation knowledge base.
 *
 * The server (`boundless-docs-mcp`, `integrations/mcp/docs`) answers for the
 * ref the workspace is at, so a query always returns the documentation the
 * checked-out code carries. The configuration is written into
 * `.vscode/mcp.json`, which the editor picks up without further setup; the merge
 * is kept out of the editor so it can be unit tested.
 */

/** The name the extension gives its server in `mcp.json`. */
export const DOCS_MCP_NAME = "boundless-docs";

/** The defaults of `boundless-docs-mcp`, so the configuration the extension
 * writes is the configuration the server would use anyway. */
export const DOCS_MCP_DEFAULTS = {
  prefix: "docs",
  version: "HEAD",
  langs: ["zh"],
  docsUrl: "",
} as const;

/** The workspace folder, as a server argument: the server reads the
 * documentation out of the git repository it sits in. */
const WORKSPACE = "${workspaceFolder}";

/** Everything the server needs to be pointed at a documentation tree. */
export interface DocsMcpOptions {
  /** The command that starts the server; `${workspaceFolder}` may be part of it. */
  command: string;
  /** The git repository the documentation is read from. */
  root?: string;
  /** The documentation directory inside the repository. */
  prefix?: string;
  /** The ref label the workspace is at. */
  version?: string;
  /** Locale directories inside the documentation root. */
  langs?: string[];
  /** The base URL of the rendered documentation site; empty keeps the source
   * links repository-relative. */
  docsUrl?: string;
}

/** The `mcp.json` entry that serves the documentation of the workspace. */
export function docsMcpServer(
  options: DocsMcpOptions,
): Record<string, unknown> {
  const args: string[] = [
    "--root",
    options.root ?? WORKSPACE,
    "--prefix",
    options.prefix ?? DOCS_MCP_DEFAULTS.prefix,
    "--version",
    options.version ?? DOCS_MCP_DEFAULTS.version,
    "--langs",
    (options.langs ?? DOCS_MCP_DEFAULTS.langs)
      .filter((lang): lang is string => typeof lang === "string")
      .filter((lang) => lang.trim().length > 0)
      .map((lang) => lang.trim())
      .join(","),
  ];
  const docsUrl = options.docsUrl ?? DOCS_MCP_DEFAULTS.docsUrl;
  if (docsUrl.trim().length > 0) {
    args.push("--docs-url", docsUrl);
  }
  return { type: "stdio", command: options.command, args };
}

/** The shape of a `.vscode/mcp.json` document. */
interface McpJson {
  servers?: Record<string, unknown>;
}

/** Puts our server into the document, replacing what the extension wrote before
 * and keeping the user's own servers in place. */
export function mergeMcpJson(
  existing: string | null,
  name: string,
  server: Record<string, unknown>,
): string {
  let base: McpJson = {};
  if (existing && existing.trim().length > 0) {
    try {
      base = JSON.parse(existing) as McpJson;
    } catch {
      // A broken file is replaced wholesale; the caller shows the result.
      base = {};
    }
  }
  const servers: Record<string, unknown> = { ...(base.servers ?? {}) };
  delete servers[name];
  servers[name] = server;
  return `${JSON.stringify({ servers }, null, 2)}\n`;
}

/** True when the document already carries a server of the given name. */
export function hasMcpServer(text: string | null, name: string): boolean {
  if (!text) return false;
  try {
    const parsed = JSON.parse(text) as McpJson;
    return Object.prototype.hasOwnProperty.call(parsed.servers ?? {}, name);
  } catch {
    return false;
  }
}
