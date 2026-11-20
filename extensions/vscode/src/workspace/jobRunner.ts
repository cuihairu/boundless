/**
 * The real job runner: executes the job's command in its working directory and
 * reports the exit status the acceptance asks for — a failed job has to say
 * which code it died with and where its results went.
 */

import { execFile } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

import { JobOutcome, JobRunner, exitStatus } from "./jobs";

const OUTPUT_KEEP = 4000;

/** Runs jobs with the platform's own command lookup. */
export function nodeJobRunner(): JobRunner {
  return {
    async run(spec) {
      mkdirSync(dirname(spec.resultPath), { recursive: true });
      const { exitCode, signal, output } = await execCapture(spec);
      const outcome: JobOutcome = {
        ok: exitCode === 0,
        exitCode,
        signal,
        resultPath: spec.resultPath,
        output: keepTail(output, OUTPUT_KEEP),
      };
      // The report is the results path the failure message points at.
      writeFileSync(
        spec.resultPath,
        `${spec.name}: ${exitStatus(outcome)}\n${outcome.output}\n`,
        "utf8",
      );
      return outcome;
    },
  };
}

interface Capture {
  exitCode: number | null;
  signal: string | null;
  output: string;
}

/** execFile, keeping stdout and stderr together and the failure reason. */
function execCapture(spec: {
  command: string;
  args: string[];
  cwd: string;
}): Promise<Capture> {
  return new Promise((resolve) => {
    execFile(
      spec.command,
      spec.args,
      { cwd: spec.cwd, maxBuffer: 8 * 1024 * 1024 },
      (error, stdout, stderr) => {
        if (!error) {
          resolve({
            exitCode: 0,
            signal: null,
            output: keepTail(`${stdout}\n${stderr}`.trim()),
          });
          return;
        }
        resolve({
          exitCode: typeof error.code === "number" ? error.code : null,
          signal: error.signal ?? null,
          output: keepTail(
            `${stdout}\n${stderr}\n${error.message}`.trim(),
          ),
        });
      },
    );
  });
}

/** Keeps the tail of a job's output: that is what explains a failure. */
function keepTail(output: string, keep = OUTPUT_KEEP): string {
  return output.length <= keep ? output : output.slice(-keep);
}
