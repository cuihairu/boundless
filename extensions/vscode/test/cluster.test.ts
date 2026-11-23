import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import {
  ClusterConfig,
  ClusterControl,
  CLUSTER_ROLES,
  ClusterRole,
  ProcessRunner,
  ProcessSpec,
  roleSpec,
} from "../src/workspace/cluster";
import { isRunning, processRunner } from "../src/workspace/processRunner";

const CONFIG: ClusterConfig = {
  binary: "/opt/boundless/server",
  resRoot: "/game/res",
  cwd: "/game",
  logDir: "/game/.boundless/logs",
};

/** A runner that records everything it is asked to do. */
function fakeRunner(): {
  runner: ProcessRunner;
  spawned: ProcessSpec[];
  terminated: number[];
} {
  const spawned: ProcessSpec[] = [];
  const terminated: number[] = [];
  let next = 1000;
  return {
    spawned,
    terminated,
    runner: {
      async spawn(spec) {
        spawned.push(spec);
        return next++;
      },
      async terminate(pid) {
        terminated.push(pid);
      },
    },
  };
}

describe("role command line", () => {
  it("exports the resource path like the reference does", () => {
    const spec = roleSpec(CONFIG, "baseapp");
    expect(spec.args).toEqual(["baseapp"]);
    expect(spec.env).toEqual({ BW_RES_PATH: "/game/res" });
    expect(spec.cwd).toBe("/game");
    expect(spec.logPath).toBe("/game/.boundless/logs/baseapp.log");
  });

  it("knows every reference role", () => {
    expect(CLUSTER_ROLES).toEqual([
      "baseapp",
      "baseappmgr",
      "cellapp",
      "cellappmgr",
      "dbapp",
      "dbappmgr",
      "editor",
    ]);
  });
});

describe("cluster lifecycle", () => {
  it("starts the requested roles only", async () => {
    const { runner, spawned } = fakeRunner();
    const cluster = new ClusterControl(runner);
    await cluster.start(CONFIG, "baseapp");
    await cluster.start(CONFIG, "cellapp");
    expect(spawned.map((s) => s.args)).toEqual([["baseapp"], ["cellapp"]]);
    expect(cluster.roles().map((r) => r.role)).toEqual(["baseapp", "cellapp"]);
    expect(cluster.isRunning).toBe(true);
  });

  it("starts a role at most once", async () => {
    const { runner, spawned } = fakeRunner();
    const cluster = new ClusterControl(runner);
    const first = await cluster.start(CONFIG, "baseapp");
    const again = await cluster.start(CONFIG, "baseapp");
    expect(spawned).toHaveLength(1);
    expect(again).toBe(first);
  });

  it("stops the role that was asked for and keeps the others", async () => {
    const { runner, terminated } = fakeRunner();
    const cluster = new ClusterControl(runner);
    const base = await cluster.start(CONFIG, "baseapp");
    await cluster.start(CONFIG, "cellapp");
    await cluster.stop("baseapp");
    expect(terminated).toEqual([base.pid]);
    expect(cluster.roles().map((r) => r.role)).toEqual(["cellapp"]);
  });

  it("stops every role, leaving nothing behind", async () => {
    const { runner, terminated } = fakeRunner();
    const cluster = new ClusterControl(runner);
    for (const role of ["baseapp", "cellapp", "dbapp"] as ClusterRole[]) {
      await cluster.start(CONFIG, role);
    }
    await cluster.stopAll();
    expect(cluster.roles()).toEqual([]);
    expect(cluster.isRunning).toBe(false);
    expect([...terminated].sort((a, b) => a - b)).toEqual([1000, 1001, 1002]);
  });

  it("ignores a stop for a role it never started", async () => {
    const { runner, terminated } = fakeRunner();
    const cluster = new ClusterControl(runner);
    await expect(cluster.stop("editor")).resolves.toBeUndefined();
    expect(terminated).toEqual([]);
  });

  it("terminates a live process and leaves nothing behind", async () => {
    // A real process, so the acceptance is real: the test ends with nothing
    // of its own still running.
    const cwd = mkdtempSync(join(tmpdir(), "boundless-cluster-"));
    const cluster = new ClusterControl(processRunner());
    const info = await cluster.start(
      { ...CONFIG, binary: process.execPath, cwd },
      "baseapp",
    );
    try {
      expect(isRunning(info.pid)).toBe(true);
    } finally {
      await cluster.stopAll();
    }
    // SIGKILL is given after a short grace period.
    await new Promise((r) => setTimeout(r, 600));
    expect(isRunning(info.pid)).toBe(false);
    expect(cluster.roles()).toEqual([]);
  });
});
