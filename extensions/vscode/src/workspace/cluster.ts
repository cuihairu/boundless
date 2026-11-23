/**
 * Starting and stopping a local cluster of role processes.
 *
 * The reference runs one binary per role (`bigworld_app baseapp`, …) with the
 * resource path exported as `BW_RES_PATH`; the port keeps that surface
 * (`crates/server-runtime/src/app.rs` accepts the same role names and
 * `crates/resources/src/bwresource.rs` reads `BW_RES_PATH`). The process table
 * and the process handle are injected, so the lifecycle can be tested without
 * starting anything.
 */

/** The roles the reference server directories carry. */
export const CLUSTER_ROLES = [
  "baseapp",
  "baseappmgr",
  "cellapp",
  "cellappmgr",
  "dbapp",
  "dbappmgr",
  "editor",
] as const;

export type ClusterRole = (typeof CLUSTER_ROLES)[number];

/** How to launch a role: executable, arguments, environment and cwd. */
export interface ProcessSpec {
  command: string;
  args: string[];
  env: Record<string, string>;
  cwd: string;
  /** The log the role writes to, if the runner records logs. */
  logPath: string;
}

/** The identity of a running role process. */
export interface ProcessInfo {
  role: ClusterRole;
  pid: number;
  /** `command args…`, for display and for killing the right tree. */
  commandLine: string;
  logPath: string;
}

/** Spawns and terminates processes. Injected so nothing real starts in tests. */
export interface ProcessRunner {
  spawn(spec: ProcessSpec): Promise<number>;
  terminate(pid: number, commandLine: string): Promise<void>;
}

/** What the extension needs to know to launch anything. */
export interface ClusterConfig {
  /** The role binary, e.g. `target/debug/server`. */
  binary: string;
  /** Exported as `BW_RES_PATH`, as the reference does. */
  resRoot: string;
  /** Working directory of every role. */
  cwd: string;
  /** Where role logs go. */
  logDir: string;
}

/** Builds the role command line the reference's process would get. */
export function roleSpec(
  config: ClusterConfig,
  role: ClusterRole,
): ProcessSpec {
  return {
    command: config.binary,
    args: [role],
    env: { BW_RES_PATH: config.resRoot },
    cwd: config.cwd,
    logPath: `${config.logDir}/${role}.log`,
  };
}

/** One cluster of role processes, tracked so stopping never leaks them. */
export class ClusterControl {
  private readonly running = new Map<ClusterRole, ProcessInfo>();

  constructor(private readonly runner: ProcessRunner) {}

  /** Which roles are running now, in role-table order. */
  roles(): ProcessInfo[] {
    return CLUSTER_ROLES.filter((role) => this.running.has(role)).map(
      (role) => this.running.get(role)!,
    );
  }

  /** Starts one role; a running role is left running. */
  async start(config: ClusterConfig, role: ClusterRole): Promise<ProcessInfo> {
    const existing = this.running.get(role);
    if (existing) return existing;
    const spec = roleSpec(config, role);
    const pid = await this.runner.spawn(spec);
    const info: ProcessInfo = {
      role,
      pid,
      commandLine: [spec.command, ...spec.args].join(" "),
      logPath: spec.logPath,
    };
    this.running.set(role, info);
    return info;
  }

  /** Stops one role, or `undefined` when it was not running. */
  async stop(role: ClusterRole): Promise<void> {
    const info = this.running.get(role);
    if (!info) return;
    await this.runner.terminate(info.pid, info.commandLine);
    this.running.delete(role);
  }

  /** Stops every role, leaving nothing behind. */
  async stopAll(): Promise<void> {
    for (const role of CLUSTER_ROLES) await this.stop(role);
  }

  /** True while any role is still tracked. */
  get isRunning(): boolean {
    return this.running.size > 0;
  }
}
