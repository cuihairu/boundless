# Shared helpers for the ci/ entrypoints.
#
# Every entrypoint is a single shell function that runs the same way locally
# and inside a workflow job: `ci/<name>.sh [args]`. Outcomes go to $CI_OUT as
# a machine-readable manifest (lane, status, detail) plus per-lane logs, so
# `ci/archive-results.sh` can ship them as one artifact in either place.
#
# Status vocabulary:
#   PASS — the lane ran and everything it checked held.
#   FAIL — the lane ran and found a defect.
#   SKIP — the lane has no substrate in this checkout yet (the repo is still
#          under construction). A SKIP names the missing path and the entry
#          point that will consume it, so the lane lights up the day the code
#          lands without any workflow edit.
#
# A SKIP is not a pass: `ci_finalize` only exits clean when there is no FAIL.
#
# Callers set their own error mode before sourcing: `set -euo pipefail`.
# This file never changes the caller's shell options, because a sourced
# library turning on `-e` would abort the very commands the lanes expect to
# run and measure (a red clippy must be recorded, not fatal).

# shellcheck disable=SC2034
CI_LIB_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$CI_LIB_ROOT/.." && pwd)"
CI_OUT="${CI_OUT:-$REPO_ROOT/ci-out}"
CI_MANIFEST="$CI_OUT/manifest.tsv"
CI_LOG_DIR="$CI_OUT/logs"

ci_out_env() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    printf '%s\n' "CI_OUT=$CI_OUT"
    printf '%s\n' "CI_MANIFEST=$CI_MANIFEST"
    printf '%s\n' "CI_LOG_DIR=$CI_LOG_DIR"
}

ci_step() {
    printf '\n== %s\n' "$*"
}

ci_note() {
    printf '[ci] %s\n' "$*"
}

# Record one outcome. Logs are captured with tee so the terminal and the
# archive agree on what the lane saw.
ci_record() {
    local lane="$1" status="$2" detail="$3"
    mkdir -p "$CI_OUT"
    printf '%s\t%s\t%s\n' "$lane" "$status" "$detail" >>"$CI_MANIFEST"
    printf '[ci] %-34s %-5s %s\n' "$lane" "$status" "$detail"
}

ci_pass() { ci_record "$1" PASS "$2"; }
ci_fail() { ci_record "$1" FAIL "$2"; }
ci_skip() { ci_record "$1" SKIP "$2"; }

# Run a command while teeing its output to the lane log, so the terminal and
# the archive agree on what the lane saw. The command's own status comes back
# in $CI_RC — taken from PIPESTATUS, not from the tee at the end of the
# pipeline.
ci_pipe() {
    local lane="$1"
    shift
    local log="$CI_LOG_DIR/$lane.log"
    mkdir -p "$CI_LOG_DIR"
    # shellcheck disable=SC2086
    "$@" 2>&1 | tee "$log"
    CI_RC="${PIPESTATUS[0]}"
}

# Resolve the git ref that the current event is measured against.
ci_base_ref() {
    if [ -n "${GITHUB_BASE_REF:-}" ]; then
        printf '%s' "$GITHUB_BASE_REF"
    elif [ "${GITHUB_EVENT_NAME:-push}" = pull_request ]; then
        printf '%s' "origin/$GITHUB_BASE_REF"
    else
        printf '%s' "origin/main"
    fi
}

# Exit clean unless some lane failed. SKIP never blocks a push.
ci_finalize() {
    local failed=0
    if [ -f "$CI_MANIFEST" ]; then
        failed=$(awk -F'\t' '$2 == "FAIL"' "$CI_MANIFEST" | wc -l)
        printf '\n== manifest: %s\n' "$CI_MANIFEST"
        column -s "$(printf '\t')" "$CI_MANIFEST" 2>/dev/null || cat "$CI_MANIFEST"
    fi
    if [ "$failed" -gt 0 ]; then
        ci_note "$failed lane(s) failed"
        return 1
    fi
    ci_note "no failed lane"
    return 0
}

# True when the repo has not grown past this checkout — used by the freshness
# gate in daily builds. Counted by commit date, so a quiet branch skips.
ci_commits_since() {
    local since="$1"
    git -C "$REPO_ROOT" rev-list --count --since="$since" HEAD
}

ci_now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
ci_git_sha() { git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo norev; }
ci_version() {
    sed -n 's/^version = "\([^"]*\)".*/\1/p' "$REPO_ROOT/Cargo.toml" | head -1
}
