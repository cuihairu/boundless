import { describe, expect, it, vi } from "vitest";

/**
 * The host-dependent half of the cluster lifecycle: how a role is started and
 * stopped differs per platform, and no machine here can exercise the others.
 * The platform is injected, so each branch is checked where the code that
 * takes it lives. The platform calls themselves are mocked, and the test reads
 * what the code asked for.
 */

const state = {
  spawnOptions: [] as Record<string, unknown>[],
  taskkill: [] as string[][],
};

vi.mock("node:child_process", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:child_process")>();
  return {
    ...actual,
    spawn(_command: string, _args: string[], options: Record<string, unknown>) {
      state.spawnOptions.push(options);
      const handlers: Record<string, Array<(...a: unknown[]) => void>> = {};
      const child = {
        pid: 4242,
        on(event: string, cb: (...a: unknown[]) => void) {
          (handlers[event] ??= []).push(cb);
          return child;
        },
        once(event: string, cb: (...a: unknown[]) => void) {
          (handlers[event] ??= []).push(cb);
          return child;
        },
        __emit(event: string) {
          for (const cb of handlers[event] ?? []) cb();
        },
      };
      // The platform reports the child once it is up.
      queueMicrotask(() => child.__emit("spawn"));
      return child as never;
    },
    execFile(_file: string, args: string[], callback: () => void) {
      state.taskkill.push([...args]);
      callback();
      return {} as never;
    },
  };
});

const { processRunner } = await import("../src/workspace/processRunner");

const spec = {
  command: "/bin/true",
  args: ["baseapp"],
  env: { BW_RES_PATH: "/res" },
  cwd: "/work",
  logPath: "/work/.boundless/logs/baseapp.log",
};

describe("starting a role", () => {
  it("detaches the role on Linux so the group can be signalled", async () => {
    state.spawnOptions.length = 0;
    await processRunner("linux").spawn(spec as never);
    expect(state.spawnOptions[0].detached).toBe(true);
  });

  it("detaches the role on macOS, where the group signalling works the same", async () => {
    state.spawnOptions.length = 0;
    await processRunner("darwin").spawn(spec as never);
    expect(state.spawnOptions[0].detached).toBe(true);
  });

  it("leaves the role attached on Windows, where taskkill reaches the tree", async () => {
    state.spawnOptions.length = 0;
    await processRunner("win32").spawn(spec as never);
    expect(state.spawnOptions[0].detached).toBe(false);
  });
});

describe("stopping a role", () => {
  it("kills the Windows tree with taskkill", async () => {
    state.taskkill.length = 0;

    await processRunner("win32").terminate(4242, "server baseapp");

    expect(state.taskkill).toEqual([["/pid", "4242", "/t", "/f"]]);
  });

  it("signals the process group on Linux", async () => {
    await signalsTheProcessGroup("linux");
  });

  it("signals the process group on macOS the same way", async () => {
    await signalsTheProcessGroup("darwin");
  });
});

/** The POSIX half: the group gets SIGTERM, then SIGKILL if it refuses. */
async function signalsTheProcessGroup(platform: "linux" | "darwin"): Promise<void> {
  const kills: Array<[number, string]> = [];
  const kill = vi.spyOn(process, "kill").mockImplementation(
    ((pid: number, signal: string) => {
      kills.push([pid, signal]);
      return true;
    }) as never,
  );

  await processRunner(platform).terminate(4242, "server baseapp");
  await new Promise((resolve) => setTimeout(resolve, 400));

  expect(kills).toContainEqual([-4242, "SIGTERM"]);
  expect(kills).toContainEqual([-4242, "SIGKILL"]);
  kill.mockRestore();
}
