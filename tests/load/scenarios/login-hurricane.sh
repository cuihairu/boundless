#!/bin/sh
# BW-17-03-05 - PR smoke for the login hurricane: the saved
# scenario capped to the harness low-load envelope, driven through
# the offline faces. ci/load.sh passes it when it exits 0 inside the wall
# budget having printed metric lines; LOAD_DURATION/LOAD_CLIENTS come
# from the harness.
set -eu
root="$(cd "$(dirname "$0")/../../.." && pwd)"
exec cargo run --quiet --manifest-path "$root/apps/bots/Cargo.toml" --bin bots -- "$root/tests/load/scenarios/login-hurricane.xml"
