#!/bin/sh
# BW-17-02-06 - PR smoke for the cross-cell scenario: bots walk the
# boundary and the run reports the crossing success rate. The fixed seed
# makes a re-run comparable; ci/load.sh caps the scale to its envelope.
set -eu
root="$(cd "$(dirname "$0")/../../.." && pwd)"
exec cargo run --quiet --manifest-path "$root/apps/bots/Cargo.toml" --bin bots -- "$root/tests/load/scenarios/cross-cell.xml"
