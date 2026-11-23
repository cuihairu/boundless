/**
 * The real process handle for the cluster lifecycle.
 *
 * On POSIX the role is detached into its own process group, so stopping it
 * reaches its children as well; on Windows the tree is killed with `taskkill`.
 * Nothing here is imported by the lifecycle itself, which only sees
 * `ProcessRunner`.
 */

import { spawn } from "node:child_process";

import { ProcessInfo, ProcessRunner } from "./cluster";

/** Spawns a role process, detached so the editor closing cannot leave it behind. */
export function processRunner(platform: string = process.platform): ProcessRunner {
  return {
    async spawn(spec) {
      const child = spawn(spec.command, spec.args, {
        cwd: spec.cwd,
        env: { ...process.env, ...spec.env },
        stdio: ["ignore", "ignore", "ignore"],
        detached: platform !== "win32",
      });
      child.on("error", () => {
        // The role binary may not exist yet; the lifecycle records no pid.
      });
      return new Promise<number>((resolve) => {
        child.once("spawn", () => resolve(child.pid ?? -1));
        child.once("error", () => resolve(-1));
      });
    },
    async terminate(pid, _commandLine) {
      await killTree(pid, platform);
    },
  };
}

/** Kills the process and, on POSIX, the group it leads. */
async function killTree(pid: number, platform: string): Promise<void> {
  if (pid <= 0) return;
  if (platform === "win32") {
    const { execFile } = await import("node:child_process");
    await new Promise<void>((resolve) => {
      execFile("taskkill", ["/pid", String(pid), "/t", "/f"], () => resolve());
    });
    return;
  }
  await new Promise<void>((resolve) => {
    try {
      process.kill(-pid, "SIGTERM");
    } catch {
      // Not a group leader any more; fall through to the single process.
    }
    setTimeout(() => {
      try {
        process.kill(-pid, "SIGKILL");
      } catch {
        try {
          process.kill(pid, "SIGKILL");
        } catch {
          // Already gone.
        }
      }
      resolve();
    }, 300);
  });
}

/** True while `pid` still names a process on this machine. */
export function isRunning(pid: number): boolean {
  if (pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    // EPERM means it exists but is not ours.
    return (e as NodeJS.ErrnoException).code === "EPERM";
  }
}

/** Every role still alive, so the editor can show what it is holding. */
export function runningRoles(infos: ProcessInfo[]): ProcessInfo[] {
  return infos.filter((info) => isRunning(info.pid));
}
