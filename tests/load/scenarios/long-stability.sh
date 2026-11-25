#!/bin/sh
# BW-17-02-08 - PR smoke for the long-stability scenario: the fleet
# soaks for the window while the run samples the load process's own
# resource use. The fixed seed makes the soak face re-runnable; the
# resource half is saved in the report, not compared across runs.
set -eu
root="$(cd "$(dirname "$0")/../../.." && pwd)"
exec cargo run --quiet --manifest-path "$root/apps/bots/Cargo.toml" --bin bots -- "$root/tests/load/scenarios/long-stability.xml"
