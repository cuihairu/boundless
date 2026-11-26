#!/usr/bin/env bash
# OPT-14-01-01 — record the release-artifact size and first-run baseline.
#
# Usage:
#   ci/release-baseline.sh              # measure the host target, append manifest rows
#   ci/release-baseline.sh self-test    # prove the parser bites on green/red stubs
#
# This lane only measures. It does not tag, release or publish anything:
# the version in the file name is the workspace version read from Cargo.toml,
# and the product stays a local tarball under ci-out/package (repo red line).
#
# The acceptance is "三端有基线" — a row per platform. This script measures
# whichever host it runs on and names the missing legs as SKIP, never as PASS.
# A runner for another target can source it with CI_RELEASE_TARGET set to add
# that leg's own measurement; the fixture keeps one row per target.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
PKG=boundless-server
FIXTURE="$REPO_ROOT/docs/review/fixtures/opt-14-01-01/release-baseline.tsv"
FIRST_RUN_STARTUP='BaseApp is running'

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

# The archive name the packaging lane produces, derived from the same
# version/platform vocabulary so the two lanes measure the same artifact.
platform_name() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    MINGW* | MSYS* | CYGWIN*) os=windows ;;
    esac
    printf '%s-%s' "$os" "$arch"
}

release_binary() {
    local target="${CI_RELEASE_TARGET:-}"
    if [ -n "$target" ]; then
        printf '%s/target/%s/release/%s' "$REPO_ROOT" "$target" "$PKG"
    else
        printf '%s/target/release/%s' "$REPO_ROOT" "$PKG"
    fi
}

ci_out_of_self() { printf '%s' "${CI_RELEASE_OUT:-$CI_OUT}"; }

# First run: launch the packaged binary from the repository root (where res/
# lives) and wait for the role's running line. A clean launch is exit 0 with
# the startup line; the baseline records wall-clock to that line and stops the
# process rather than waiting for the tick loop to end by itself.
measure_first_run() {
    local bin="$1" log="$2" startup="$3" elapsed
    local start end
    start="$(date +%s.%N)"
    "$bin" baseapp >"$log" 2>&1 &
    local pid=$!
    local ok=0
    for _ in $(seq 1 600); do
        if grep -q "$startup" "$log" 2>/dev/null; then ok=1; break; fi
        sleep 0.05
    done
    end="$(date +%s.%N)"
    kill "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
    if [ "$ok" -ne 1 ]; then
        printf 'no startup line: %s' "$(tsv_escape "$(head -c 200 "$log")")"
        return 1
    fi
    elapsed="$(awk -v a="$start" -v b="$end" 'BEGIN { printf "%.6f", b - a }')"
    printf '%s' "$elapsed"
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    local platform version tarball bin
    platform="$(platform_name)"
    version="$(ci_version)"
    [ -n "$version" ] || version=untagged
    bin="$(release_binary)"
    # ci/package.sh stages the archive under $CI_OUT/package/, so the two
    # lanes only agree if they read the version together — a CI_OUT chosen for
    # this lane alone has no archive yet, and the row says SKIP.
    tarball="$(ci_out_of_self)/package/boundless-${version}-${platform}.tar.gz"

    if [ ! -f "$bin" ]; then
        ci_skip "release-size-$platform" "no release binary at $bin (build it with ci/package.sh)"
        ci_skip "first-run-$platform" "no release binary at $bin"
        return 0
    fi
    local bytes
    bytes="$(stat -c %s "$bin" 2>/dev/null || stat -f %z "$bin")"
    ci_pass "release-size-$platform" "target/release/$PKG = $bytes bytes"

    local archive_bytes=NA
    if [ -f "$tarball" ]; then
        archive_bytes="$(stat -c %s "$tarball" 2>/dev/null || stat -f %z "$tarball")"
        ci_pass "release-archive-$platform" "$(basename "$tarball") = $archive_bytes bytes"
    else
        ci_skip "release-archive-$platform" "no archive at $tarball (run ci/package.sh first)"
    fi

    local log="$CI_LOG_DIR/release-first-run-$platform.log"
    local elapsed
    if ! elapsed="$(measure_first_run "$bin" "$log" "$FIRST_RUN_STARTUP")"; then
        ci_fail "first-run-$platform" "$elapsed"
        return 1
    fi
    ci_pass "first-run-$platform" "startup in ${elapsed}s ($FIRST_RUN_STARTUP)"

    ci_note "fixture rows: $FIXTURE (record the numbers above there, by hand, after review)"
}

self_test() {
    local fails=0 r stub log elapsed
    r="$(mktemp -d "$REPO_ROOT/ci-out/release-baseline-selftest.XXXXXX")"
    stub="$r/green"
    cat >"$stub" <<'STUB'
#!/bin/sh
echo "BaseApp is running"
STUB
    chmod +x "$stub"
    log="$r/green.log"
    if elapsed="$(measure_first_run "$stub" "$log" "$FIRST_RUN_STARTUP")" \
        && printf '%s' "$elapsed" | grep -qE '^[0-9]+\.[0-9]+$'; then
        ci_pass selftest-first-run "a startup line produces a numeric elapsed"
    else
        ci_fail selftest-first-run "the green stub was not measured: ${elapsed:-none}"
        fails=$((fails + 1))
    fi

    cat >"$stub" <<'STUB'
#!/bin/sh
echo "no startup here"
exit 0
STUB
    if elapsed="$(measure_first_run "$stub" "$log" "$FIRST_RUN_STARTUP")"; then
        ci_fail selftest-first-run-red "a stub without the startup line went green"
        fails=$((fails + 1))
    else
        ci_pass selftest-first-run-red "missing startup line is named: $elapsed"
    fi
    rm -rf "$r" "$REPO_ROOT/ci-out/release-baseline-selftest".*

    if [ "$fails" -gt 0 ]; then return 1; fi
    ci_note "all self-tests behaved as expected"
}

main() {
    if [ "$MODE" = self-test ]; then self_test; return $?; fi
    run_all
}

main "$@"
LANE_RC=$?

set +e
ci_finalize
FINAL_RC=$?
set -e

[ "$LANE_RC" -ne 0 ] && exit "$LANE_RC"
exit "$FINAL_RC"
