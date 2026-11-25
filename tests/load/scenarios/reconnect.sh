#!/bin/sh
# BW-17-02-07 - PR smoke for the reconnect scenario: sessions are dropped
# mid-window and the run reports the mean recovery length. The fixed seed
# makes a re-run comparable; ci/load.sh caps the scale to its envelope.
set -eu
root="$(cd "$(dirname "$0")/../../.." && pwd)"
exec cargo run --quiet --manifest-path "$root/apps/bots/Cargo.toml" --bin bots -- "$root/tests/load/scenarios/reconnect.xml"
