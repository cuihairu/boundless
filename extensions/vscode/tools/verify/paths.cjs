// Shared paths for the verification harnesses in this folder.
const path = require("node:path");

/** Where the compiled extension core is, when `pnpm run build` has been run. */
const EXTENSION_DIR = path.join(__dirname, "..", "..");
const OUT = path.join(EXTENSION_DIR, "out", "workspace");

/** The upstream BigWorld tree, used to check the port against the original. */
const REFERENCE =
  process.env.BW_REFERENCE || "/home/cui/workspaces/BigWorld";

/** The example res tree every harness points at. */
const EXAMPLE_RES = path.join(
  REFERENCE,
  "programming",
  "bigworld",
  "examples",
  "client_integration",
  "python",
  "simple",
  "res",
);

/** The original `process_defs`, for the reference half of the evidence. */
const REFERENCE_TOOL = path.join(
  REFERENCE,
  "game",
  "bin",
  "server",
  "el7",
  "tools",
  "process_defs",
);

module.exports = { EXTENSION_DIR, OUT, REFERENCE, EXAMPLE_RES, REFERENCE_TOOL };
