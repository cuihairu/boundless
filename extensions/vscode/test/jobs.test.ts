import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import { JobOutcome, exitStatus, formatOutcome } from "../src/workspace/jobs";
import { nodeJobRunner } from "../src/workspace/jobRunner";

const runner = nodeJobRunner();

function failing(): JobOutcome {
  return {
    ok: false,
    exitCode: 7,
    signal: null,
    resultPath: "/tmp/boundless/reports/bots.json",
    output: "traceback: boom",
  };
}

describe("job reports", () => {
  it("says a failed job failed, with its exit status and its results", () => {
    const report = formatOutcome(failing());
    expect(report).toContain("failed");
    expect(report).toContain("exit code 7");
    expect(report).toContain("/tmp/boundless/reports/bots.json");
  });

  it("distinguishes a signal from an exit status", () => {
    expect(
      formatOutcome({ ...failing(), exitCode: null, signal: "SIGTERM" }),
    ).toContain("signal SIGTERM");
    expect(exitStatus(failing())).toBe("exit code 7");
    expect(exitStatus({ ...failing(), exitCode: null })).toBe("no exit status");
  });
});

describe("running a job", () => {
  it("reports the exit status of a failing job", async () => {
    const dir = mkdtempSync(join(tmpdir(), "boundless-jobs-"));
    const result = await runner.run({
      name: "tests",
      command: process.execPath,
      args: ["-e", "process.exit(7)"],
      cwd: dir,
      resultPath: join(dir, "reports", "tests.json"),
    });
    expect(result.ok).toBe(false);
    expect(result.exitCode).toBe(7);
    expect(formatOutcome(result)).toContain("exit code 7");
    expect(formatOutcome(result)).toContain(result.resultPath);
  });

  it("reports a passing job", async () => {
    const dir = mkdtempSync(join(tmpdir(), "boundless-jobs-"));
    const result = await runner.run({
      name: "tests",
      command: process.execPath,
      args: ["-e", "process.exit(0)"],
      cwd: dir,
      resultPath: join(dir, "reports", "tests.json"),
    });
    expect(result.ok).toBe(true);
    expect(result.exitCode).toBe(0);
    expect(formatOutcome(result)).toContain("ok");
  });

  it("reports a job whose command does not exist", async () => {
    const dir = mkdtempSync(join(tmpdir(), "boundless-jobs-"));
    const result = await runner.run({
      name: "bots",
      command: "surely-not-a-real-command",
      args: [],
      cwd: dir,
      resultPath: join(dir, "reports", "bots.json"),
    });
    expect(result.ok).toBe(false);
    expect(result.exitCode).toBeNull();
    expect(formatOutcome(result)).toContain("failed");
  });
});
