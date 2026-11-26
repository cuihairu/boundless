#!/usr/bin/env bash
# OPT-16-01-01 — record the dependency fanout and the incremental build
# baseline: which workspace crates a single edit recompiles, and how long a
# cold and an incremental build take on this host.
#
# Usage:
#   ci/build-baseline.sh              # fanout table + cold + incremental timings
#   ci/build-baseline.sh fanout       # dependency fanout only (fast, no build)
#
# The fanout table comes from `cargo metadata` (how many workspace crates
# directly depend on each crate): editing a crate with a high fanout is the
# recompile source, and that is what the incremental leg measures. The build
# legs use a private target dir so they never reuse the developer's warm
# artifacts, and the incremental leg touches one source file in the chosen
# crate rather than rewriting it.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
PKG=boundless-server
# The crate one edit expands to the most recompilation. Resolved from the
# metadata below; kept here so a surprising rdep count is visible in review.
HIGH_FANOUT_CRATE="${CI_BUILD_FANOUT_CRATE:-foundation}"
TOUCH_FILE="crates/foundation/src/lib.rs"

fanout_table() {
    cargo metadata --format-version 1 --no-deps | python3 -c '
import json, sys
d = json.load(sys.stdin)
ws = set(d["workspace_members"])
name2id = {p["name"]: p["id"] for p in d["packages"]}
direct = {n: 0 for n in name2id}
for p in d["packages"]:
    if p["id"] not in ws:
        continue
    for dep in p["dependencies"]:
        if dep["name"] in name2id:
            direct[dep["name"]] += 1
for n, c in sorted(direct.items(), key=lambda x: (-x[1], x[0])):
    print("%s\t%d" % (n, c))
'
}

time_cmd() {
    # time_cmd <log> <cmd...> — seconds to stderr, wall on stdout.
    local log="$1"; shift
    local start end
    start="$(date +%s.%N)"
    "$@" >"$log" 2>&1
    local rc=$?
    end="$(date +%s.%N)"
    printf '%s' "$(awk -v a="$start" -v b="$end" 'BEGIN { printf "%.2f", b - a }')"
    return "$rc"
}

run_fanout() {
    local out
    out="$(fanout_table)" || { ci_fail build-fanout "cargo metadata failed"; return 1; }
    local top
    top="$(printf '%s\n' "$out" | head -1 | cut -f1)"
    ci_pass build-fanout "top recompile source: $top ($(printf '%s\n' "$out" | head -1 | cut -f2) direct dependents)"
    printf '%s\n' "$out" >"$CI_OUT/build-fanout.tsv"
    ci_note "fanout table: $CI_OUT/build-fanout.tsv"
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"

    ci_step "dependency fanout"
    run_fanout || return 1

    local target="$CI_OUT/build-baseline-target"
    ci_step "cold build (private target dir)"
    rm -rf "$target"
    local secs
    if ! secs="$(time_cmd "$CI_LOG_DIR/build-cold.log" \
        env CARGO_TARGET_DIR="$target" cargo build --release -p "$PKG")"; then
        ci_fail build-cold "cold release build failed; see build-cold.log"
        return 1
    fi
    ci_pass build-cold "cold release build of $PKG: ${secs}s"

    ci_step "incremental build after touching $TOUCH_FILE"
    touch "$TOUCH_FILE"
    local isecs
    if ! isecs="$(time_cmd "$CI_LOG_DIR/build-incremental.log" \
        env CARGO_TARGET_DIR="$target" cargo build --release -p "$PKG")"; then
        ci_fail build-incremental "incremental build failed; see build-incremental.log"
        return 1
    fi
    # Which crates recompiled: cargo prints "Compiling <name> v..." lines.
    local recompiled
    recompiled="$(grep -c '^   Compiling' "$CI_LOG_DIR/build-incremental.log" || true)"
    ci_pass build-incremental "incremental build after one edit: ${isecs}s, $recompiled crate(s) recompiled"

    ci_note "record cold=${secs}s incremental=${isecs}s recompiled=$recompiled in the fixture"
}

case "$MODE" in
fanout) mkdir -p "$CI_OUT"; run_fanout ;;
*) run_all ;;
esac

LANE_RC=$?
set +e
ci_finalize
FINAL_RC=$?
set -e
[ "$LANE_RC" -ne 0 ] && exit "$LANE_RC"
exit "$FINAL_RC"
