/**
 * Running the repository's own checks from the editor: the Rust test suite and
 * a bots scenario. The acceptance point is the failure path — a failed job has
 * to show its exit status and where its results landed.
 *
 * The commands come from the caller (the adapter reads extension settings), so
 * this module never invents an entry point it does not own.
 */

/** One job: a command with its working directory and its result file. */
export interface JobSpec {
  name: string;
  command: string;
  args: string[];
  cwd: string;
  /** Where the job's report goes; created on demand. */
  resultPath: string;
}

/** The outcome of a job, success or failure. */
export interface JobOutcome {
  ok: boolean;
  /** Null when the job was killed by a signal instead of exiting. */
  exitCode: number | null;
  signal: string | null;
  resultPath: string;
  /** The job's own output, last lines only. */
  output: string;
}

/** Runs jobs. Injected so tests can fake success and failure alike. */
export interface JobRunner {
  run(spec: JobSpec): Promise<JobOutcome>;
}

/** Renders an outcome the way the output channel shows it. */
export function formatOutcome(outcome: JobOutcome): string {
  const status = outcome.signal
    ? `signal ${outcome.signal}`
    : outcome.exitCode === null
      ? "no exit status"
      : `exit code ${outcome.exitCode}`;
  const verdict = outcome.ok ? "ok" : "failed";
  return `${verdict}: ${status}; results at ${outcome.resultPath}`;
}

/** The exit status a job had, as the failure report reads it. */
export function exitStatus(outcome: JobOutcome): string {
  return outcome.signal
    ? `signal ${outcome.signal}`
    : outcome.exitCode === null
      ? "no exit status"
      : `exit code ${outcome.exitCode}`;
}
