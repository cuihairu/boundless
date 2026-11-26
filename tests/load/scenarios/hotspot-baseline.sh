#!/bin/sh
# OPT-05-01-01 - PR smoke for the hotspot/fault baseline: the fleet runs
# hot, a seeded subset faults mid-window and recovers, then the fleet
# drains. The run reports the recovery statistics and the window shape;
# the RSS faces travel in LOAD_REPORT. The fixed seed makes a re-run
# comparable; ci/load.sh caps the scale to its envelope.
set -eu
root="$(cd "$(dirname "$0")/../../.." && pwd)"
exec cargo run --quiet --manifest-path "$root/apps/bots/Cargo.toml" --bin bots -- "$root/tests/load/scenarios/hotspot-baseline.xml"
