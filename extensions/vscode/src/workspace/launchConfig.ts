/**
 * The debug configurations the extension puts into `.vscode/launch.json`.
 *
 * Attaching to a role's Python scripts is the Python extension's debugger
 * (`debugpy`), not ours, so the extension only writes the configuration: the
 * attach target is the port the role was asked to listen on. The merge is kept
 * out of the editor so it can be unit tested.
 */

/** The default attach port per role, matching the role's own log. */
export const ATTACH_PORTS: Record<string, number> = {
  baseapp: 5678,
  baseappmgr: 5688,
  cellapp: 5679,
  cellappmgr: 5689,
  dbapp: 5690,
  dbappmgr: 5691,
};

/** A `debugpy` attach configuration for one role. */
export function pythonAttachConfig(
  role: string,
  port: number = ATTACH_PORTS[role] ?? 5678,
): Record<string, unknown> {
  return {
    name: `Boundless: Attach to ${role}`,
    type: "debugpy",
    request: "attach",
    connect: { host: "127.0.0.1", port },
    justMyCode: false,
  };
}

/** The debugger that can launch a Rust binary on this platform. */
export function rustDebugger(
  platform: string = process.platform,
): string {
  return platform === "win32" ? "cppvsdbg" : "lldb";
}

/** A launch configuration that runs one role under the debugger. */
export function rustLaunchConfig(
  role: string,
  options: { platform?: string; program?: string; resPath?: string } = {},
): Record<string, unknown> {
  const program = options.program ?? "${workspaceFolder}/target/debug/boundless-server";
  const resPath = options.resPath ?? "${workspaceFolder}/res";
  return {
    name: `Boundless: Launch ${role}`,
    type: rustDebugger(options.platform),
    request: "launch",
    program,
    args: [role],
    cwd: "${workspaceFolder}",
    env: { BW_RES_PATH: resPath },
    stopAtEntry: false,
  };
}

/** Inserts our configurations into an existing `launch.json` text. */
export function mergeLaunchJson(
  existing: string | null,
  configs: Record<string, unknown>[],
): string {
  let base: { configurations?: Record<string, unknown>[] } = {};
  if (existing && existing.trim().length > 0) {
    try {
      base = JSON.parse(existing) as typeof base;
    } catch {
      // A broken file is replaced wholesale; the caller shows the result.
      base = {};
    }
  }
  const have = new Set(
    (base.configurations ?? []).map((c) => String(c.name)),
  );
  const merged = [...(base.configurations ?? [])];
  for (const config of configs) {
    if (!have.has(String(config.name))) merged.push(config);
  }
  base.configurations = merged;
  return `${JSON.stringify(base, null, 2)}\n`;
}

/** True when the text already carries a configuration of the given name. */
export function hasConfiguration(text: string | null, name: string): boolean {
  if (!text) return false;
  try {
    const parsed = JSON.parse(text) as { configurations?: unknown[] };
    return (parsed.configurations ?? []).some(
      (c) => (c as { name?: unknown }).name === name,
    );
  } catch {
    return false;
  }
}
