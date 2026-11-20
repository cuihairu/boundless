import { describe, expect, it } from "vitest";

import {
  DOCS_MCP_DEFAULTS,
  DOCS_MCP_NAME,
  docsMcpServer,
  hasMcpServer,
  mergeMcpJson,
} from "../src/workspace/docsMcp";

describe("the docs mcp server entry", () => {
  it("starts the built server as a stdio server of the workspace", () => {
    expect(
      docsMcpServer({
        command: "${workspaceFolder}/target/boundless-docs-mcp",
      }),
    ).toEqual({
      type: "stdio",
      command: "${workspaceFolder}/target/boundless-docs-mcp",
      args: [
        "--root",
        "${workspaceFolder}",
        "--prefix",
        "docs",
        "--version",
        "HEAD",
        "--langs",
        "zh",
      ],
    });
  });

  it("reads the documentation out of another repository when told to", () => {
    const server = docsMcpServer({
      command: "boundless-docs-mcp",
      root: "/work/game/boundless",
    }) as { args: string[] };
    const at = server.args.indexOf("--root");
    expect(server.args[at + 1]).toBe("/work/game/boundless");
  });

  it("points at the prefix, the version and the locales that were asked for", () => {
    expect(
      docsMcpServer({
        command: "boundless-docs-mcp",
        prefix: "manual",
        version: "v1.0",
        langs: ["zh", "en"],
      }),
    ).toEqual({
      type: "stdio",
      command: "boundless-docs-mcp",
      args: [
        "--root",
        "${workspaceFolder}",
        "--prefix",
        "manual",
        "--version",
        "v1.0",
        "--langs",
        "zh,en",
      ],
    });
  });

  it("passes the rendered site base only when it is set", () => {
    const without = docsMcpServer({
      command: "boundless-docs-mcp",
      docsUrl: "",
    }) as { args: string[] };
    expect(without.args).not.toContain("--docs-url");

    const withUrl = docsMcpServer({
      command: "boundless-docs-mcp",
      docsUrl: "https://docs.example/",
    }) as { args: string[] };
    expect(without.args.length + 2).toBe(withUrl.args.length);
    expect(withUrl.args).toContain("--docs-url");
    expect(withUrl.args).toContain("https://docs.example/");
  });

  it("ignores locales that are not usable strings", () => {
    // A badly configured setting may hand over blanks and holes.
    const langs = ["zh", "", "  ", null] as unknown as string[];
    const server = docsMcpServer({
      command: "boundless-docs-mcp",
      langs,
    }) as { args: string[] };
    const joined = server.args.join(" ");
    expect(joined).toContain("--langs zh");
    expect(joined).not.toContain("zh,");
    expect(joined).not.toContain(",zh");
  });

  it("defaults to the reference documentation layout", () => {
    expect(DOCS_MCP_DEFAULTS).toEqual({
      prefix: "docs",
      version: "HEAD",
      langs: ["zh"],
      docsUrl: "",
    });
    expect(DOCS_MCP_NAME).toBe("boundless-docs");
  });
});

describe("merging mcp.json", () => {
  it("keeps the user's own servers", () => {
    const existing = JSON.stringify({
      servers: {
        "my-own": { type: "stdio", command: "my-server" },
      },
    });
    const merged = JSON.parse(
      mergeMcpJson(existing, DOCS_MCP_NAME, docsMcpServer({ command: "x" })),
    );
    expect(Object.keys(merged.servers)).toEqual([
      "my-own",
      DOCS_MCP_NAME,
    ]);
    expect(merged.servers["my-own"]).toEqual({
      type: "stdio",
      command: "my-server",
    });
  });

  it("replaces its own entry instead of duplicating it", () => {
    const first = mergeMcpJson(null, DOCS_MCP_NAME, docsMcpServer({ command: "x" }));
    const second = JSON.parse(first) as {
      servers: Record<string, { command?: string; args?: string[] }>;
    };
    second.servers[DOCS_MCP_NAME].args = [
      ...(second.servers[DOCS_MCP_NAME].args ?? []),
      "--version",
      "v1",
    ];
    const text = mergeMcpJson(
      JSON.stringify(second),
      DOCS_MCP_NAME,
      docsMcpServer({ command: "y", version: "v2" }),
    );
    const parsed = JSON.parse(text) as { servers: Record<string, unknown> };
    expect(Object.keys(parsed.servers)).toEqual([DOCS_MCP_NAME]);
    expect((parsed.servers[DOCS_MCP_NAME] as { command: string }).command).toBe(
      "y",
    );
  });

  it("starts from an empty document", () => {
    const parsed = JSON.parse(
      mergeMcpJson("", DOCS_MCP_NAME, docsMcpServer({ command: "x" })),
    ) as { servers: Record<string, unknown> };
    expect(Object.keys(parsed.servers)).toEqual([DOCS_MCP_NAME]);
  });

  it("replaces an unparseable document instead of failing", () => {
    const parsed = JSON.parse(mergeMcpJson("not json at all", DOCS_MCP_NAME, {
      type: "stdio",
      command: "x",
      args: [],
    })) as { servers: Record<string, unknown> };
    expect(Object.keys(parsed.servers)).toEqual([DOCS_MCP_NAME]);
  });

  it("tells whether a server name is already present", () => {
    const text = mergeMcpJson(null, DOCS_MCP_NAME, {
      type: "stdio",
      command: "x",
      args: [],
    });
    expect(hasMcpServer(text, DOCS_MCP_NAME)).toBe(true);
    expect(hasMcpServer(text, "my-own")).toBe(false);
    expect(hasMcpServer(null, "anything")).toBe(false);
    expect(hasMcpServer("not json", DOCS_MCP_NAME)).toBe(false);
  });
});
