#!/bin/sh
# BW-17-03-05 - PR smoke for steady-state movement: the same fixed
# seed walks the same trajectory, so a re-run of a failure is comparable.
set -eu
root="$(cd "$(dirname "$0")/../../.." && pwd)"
exec cargo run --quiet --manifest-path "$root/apps/bots/Cargo.toml" --bin bots -- "$root/tests/load/scenarios/steady-move.xml"
