import { describe, expect, it } from "vitest";

import {
  ATTACH_PORTS,
  hasConfiguration,
  mergeLaunchJson,
  pythonAttachConfig,
  rustDebugger,
  rustLaunchConfig,
} from "../src/workspace/launchConfig";

describe("python attach configuration", () => {
  it("attaches with debugpy on the role's port", () => {
    expect(pythonAttachConfig("baseapp")).toEqual({
      name: "Boundless: Attach to baseapp",
      type: "debugpy",
      request: "attach",
      connect: { host: "127.0.0.1", port: 5678 },
      justMyCode: false,
    });
  });

  it("gives every reference role its own port", () => {
    const ports = new Set(Object.values(ATTACH_PORTS));
    expect(ports.size).toBe(Object.keys(ATTACH_PORTS).length);
    expect(Object.keys(ATTACH_PORTS)).toEqual([
      "baseapp",
      "baseappmgr",
      "cellapp",
      "cellappmgr",
      "dbapp",
      "dbappmgr",
    ]);
  });

  it("uses the given port when told to", () => {
    expect(
      (pythonAttachConfig("baseapp", 9000) as { connect: { port: number } })
        .connect.port,
    ).toBe(9000);
  });
});

describe("merging launch.json", () => {
  it("keeps the user's own configurations", () => {
    const existing = JSON.stringify({
      version: "0.2.0",
      configurations: [
        { name: "My own", type: "node", request: "launch" },
      ],
    });
    const merged = JSON.parse(
      mergeLaunchJson(existing, [pythonAttachConfig("baseapp")]),
    );
    expect(merged.configurations).toHaveLength(2);
    expect(merged.configurations[0]).toEqual({
      name: "My own",
      type: "node",
      request: "launch",
    });
    expect(merged.configurations[1].name).toBe(
      "Boundless: Attach to baseapp",
    );
  });

  it("does not duplicate a configuration it already wrote", () => {
    const first = mergeLaunchJson(null, [pythonAttachConfig("baseapp")]);
    const second = mergeLaunchJson(first, [pythonAttachConfig("baseapp")]);
    const parsed = JSON.parse(second);
    expect(
      parsed.configurations.filter(
        (c: { name: string }) => c.name === "Boundless: Attach to baseapp",
      ),
    ).toHaveLength(1);
  });

  it("starts from an empty document", () => {
    const parsed = JSON.parse(
      mergeLaunchJson("", [pythonAttachConfig("cellapp")]),
    );
    expect(parsed.configurations).toHaveLength(1);
  });

  it("replaces an unparseable document instead of failing", () => {
    const parsed = JSON.parse(mergeLaunchJson("not json at all", [
      pythonAttachConfig("baseapp"),
    ]));
    expect(parsed.configurations).toHaveLength(1);
  });

  it("tells whether a name is already present", () => {
    const text = mergeLaunchJson(null, [pythonAttachConfig("baseapp")]);
    expect(hasConfiguration(text, "Boundless: Attach to baseapp")).toBe(true);
    expect(hasConfiguration(text, "Boundless: Attach to cellapp")).toBe(false);
    expect(hasConfiguration(null, "anything")).toBe(false);
  });
});

describe("rust launch configuration", () => {
  it("launches the role binary under the platform debugger", () => {
    expect(rustLaunchConfig("cellapp", { platform: "linux" })).toEqual({
      name: "Boundless: Launch cellapp",
      type: "lldb",
      request: "launch",
      program: "${workspaceFolder}/target/debug/boundless-server",
      args: ["cellapp"],
      cwd: "${workspaceFolder}",
      env: { BW_RES_PATH: "${workspaceFolder}/res" },
      stopAtEntry: false,
    });
  });

  it("uses the MSVC debugger on Windows", () => {
    expect(rustDebugger("win32")).toBe("cppvsdbg");
    expect(rustDebugger("linux")).toBe("lldb");
    expect(rustDebugger("darwin")).toBe("lldb");
  });

  it("lets the program and res path be overridden", () => {
    const config = rustLaunchConfig("baseapp", {
      program: "/opt/boundless/server",
      resPath: "/game/res",
    });
    expect(config.program).toBe("/opt/boundless/server");
    expect(config.env).toEqual({ BW_RES_PATH: "/game/res" });
  });

  it("merges into launch.json alongside the attach configurations", () => {
    const text = mergeLaunchJson(null, [
      pythonAttachConfig("baseapp"),
      rustLaunchConfig("cellapp", { platform: "linux" }),
    ]);
    const parsed = JSON.parse(text);
    expect(parsed.configurations.map((c: { name: string }) => c.name)).toEqual([
      "Boundless: Attach to baseapp",
      "Boundless: Launch cellapp",
    ]);
  });
});
