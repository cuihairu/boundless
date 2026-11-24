#!/bin/sh
# BW-17-03-05 - PR smoke for the dense AOI: the packed grid density
# and per-bot visible counts, recorded at the harness low concurrency.
set -eu
root="$(cd "$(dirname "$0")/../../.." && pwd)"
exec cargo run --quiet --manifest-path "$root/apps/bots/Cargo.toml" --bin bots -- "$root/tests/load/scenarios/dense-aoi.xml"
