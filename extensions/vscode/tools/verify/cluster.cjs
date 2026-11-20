// BW-20-02-01/02/03 evidence: start real role processes, list them, locate
// their logs, stop everything (leaving no process behind), and run a failing
// job end to end.
const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const { ClusterControl } = require("../../out/workspace/cluster.js");
const { isRunning, processRunner } = require("../../out/workspace/processRunner.js");
const { formatOutcome } = require("../../out/workspace/jobs.js");
const { nodeJobRunner } = require("../../out/workspace/jobRunner.js");

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "bwcluster-"));
const logDir = path.join(tmp, ".boundless", "logs");

// A stand-in role binary: logs which role started, then idles.
const fakeServer = path.join(tmp, "server");
fs.writeFileSync(
  fakeServer,
  [
    "#!/bin/sh",
    "# bwcluster-marker",
    'mkdir -p "$PWD/.boundless/logs"',
    'echo "$1 started (pid $$)" >> "$PWD/.boundless/logs/$1.log"',
    "sleep 60",
  ].join("\n"),
);
fs.chmodSync(fakeServer, 0o755);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function main() {
  const config = { binary: fakeServer, resRoot: tmp, cwd: tmp, logDir };
  const cluster = new ClusterControl(processRunner());

  const base = await cluster.start(config, "baseapp");
  const cell = await cluster.start(config, "cellapp");
  await sleep(400);

  // 02-02: the status lists the current roles and their logs.
  console.log("status:", JSON.stringify(cluster.roles(), null, 1));
  for (const role of ["baseapp", "cellapp"]) {
    const log = path.join(logDir, `${role}.log`);
    console.log(
      `log ${path.relative(tmp, log)}:`,
      fs.readFileSync(log, "utf8").trim(),
    );
  }

  // 02-01: stop everything; nothing may survive.
  console.log(
    "alive before stop:",
    cluster.roles().every((i) => isRunning(i.pid)),
  );
  await cluster.stopAll();
  await sleep(700);
  console.log(
    "alive after stop:",
    `baseapp ${isRunning(base.pid)}`,
    `cellapp ${isRunning(cell.pid)}`,
  );
  let left = "";
  try {
    left = execFileSync("pgrep", ["-af", "bwcluster-marker"], {
      encoding: "utf8",
    }).trim();
  } catch {
    // pgrep exits 1 when nothing matches.
  }
  console.log("processes left with the marker:", left === "" ? "none" : left);

  // 02-03: a failing job shows exit status and the results path.
  const outcome = await nodeJobRunner().run({
    name: "bots",
    command: process.execPath,
    args: ["-e", "console.error('scenario failed'); process.exit(7)"],
    cwd: tmp,
    resultPath: path.join(tmp, ".boundless", "reports", "bots.log"),
  });
  console.log("failing job ->", formatOutcome(outcome));
  console.log(
    "results file exists:",
    fs.existsSync(outcome.resultPath),
    `(${path.relative(tmp, outcome.resultPath)})`,
  );
}

main();
