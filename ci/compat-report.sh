#!/usr/bin/env bash
# BW-00-04-06 — the compatibility differential report.
#
# Usage:
#   ci/compat-report.sh            # locate the differences from the comparison
#   ci/compat-report.sh self-test  # prove an injected difference is located
#
# The comparison lane (BW-00-04-04) records, per compatibility boundary, the
# original and new values and whether they agree. This lane consumes that
# record and locates every difference by kind — a field value, a timeout, or
# an ordering — so a reader can find each one rather than stare at a flat diff.
#
# Two record faces:
#   * compat-report — the located differences: one row per non-agreeing
#     boundary, classified order/timeout/field by its own name and note.
#   * The report reads the comparison the refbase job produced (or the
#     committed snapshot); without one it says so.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
cur_out() { printf '%s' "${CI_COMPAT_OUT:-$CI_OUT}"; }

# The comparison record the report consumes: the refbase job's fresh run when
# it is present (compare-fullstack ran just before), else the committed
# snapshot.
comparison_summary() { # prints the summary path
    if [ -n "${CI_COMPAT_SUMMARY:-}" ]; then
        printf '%s' "$CI_COMPAT_SUMMARY"
    elif [ -f "$(cur_out)/compare-fullstack/compare-summary.json" ]; then
        printf '%s/compare-fullstack/compare-summary.json' "$(cur_out)"
    else
        printf '%s/tests/baseline/compare-summary.json' "$REPO_ROOT"
    fi
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    ci_step "compatibility differential report"

    local summary
    summary="$(comparison_summary)"
    if [ ! -f "$summary" ]; then
        ci_skip compat-report \
            "no comparison summary at $summary — the report reads the BW-00-04-04 comparison (set CI_COMPAT_SUMMARY or run ci/compare-fullstack.sh)"
        return 0
    fi

    local out_dir
    out_dir="$(cur_out)/compat-report"
    local sum
    if ! sum="$(python3 "$REPO_ROOT/ci/compat-report.py" "$summary" "$out_dir" --self-check 2>&1)"; then
        ci_fail compat-report "the report failed to record: $(printf '%s' "$sum" | tail -1)"
        return 1
    fi
    local located
    located="$(printf '%s' "$sum" | sed -n 's/.*differences=\([0-9]*\).*/\1/p')"
    if [ -z "$located" ]; then
        ci_fail compat-report "the report recorded no row count — a hollow report is not a record"
        return 1
    fi
    ci_pass compat-report "$located difference(s) located by kind (order/timeout/field)"

    # The committed snapshot is the anchor; a report that differs as the port
    # grows is a note, not a failure.
    local f
    for f in compat-report.json compat-report.tsv; do
        if [ -f "$REPO_ROOT/tests/baseline/$f" ] && [ -f "$out_dir/$f" ] \
            && ! cmp -s "$REPO_ROOT/tests/baseline/$f" "$out_dir/$f"; then
            ci_note "the committed tests/baseline/$f differs from this run — snapshot-time anchor, not a failure"
        fi
    done

    ci_note "differences located by kind"
}

# A planted comparison summary carrying one difference of each kind: a field,
# a timeout and an ordering.
plant_summary() { # $1 out_dir
    local dir="$1"
    mkdir -p "$dir"
    cat >"$dir/compare-summary.json" <<'JSON'
{
  "comparison": [
    {"boundary": "login_wire.login", "original": {"id": 0, "length_param": 2},
     "new": {"id": 0, "length_param": 4}, "agree": false,
     "note": "the length parameter the login message carries"},
    {"boundary": "login_handshake.timeout", "original": "5000",
     "new": "10000", "agree": false,
     "note": "the handshake timeout the login waits for"},
    {"boundary": "callback.order", "original": "11 site(s), 5 runtime path(s)",
     "new": "6 site(s), 2 runtime path(s)", "agree": false,
     "note": "the callback sequence order; the new side carries it when BW-02 lands"},
    {"boundary": "login_wire.payload_bytes", "original": "00 00 09 02",
     "new": "00 00 09 02", "agree": true, "note": "identical"}
  ]
}
JSON
}

self_test() {
    local fails=0 rc r T M r2 T2 M2
    ci_step "self-test: an injected field, timeout and order difference are each located"
    r="$(mktemp -d "$REPO_ROOT/ci-out/compat-selftest.XXXXXX")"
    T="$r/BigWorld"
    M="$r/manifest/manifest.tsv"
    plant_summary "$T"

    CI_COMPAT_SUMMARY="$T/compare-summary.json" CI_COMPAT_OUT="$r/out" \
        CI_OUT="$r/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'compat-report	PASS	3 difference(s) located by kind' "$M" \
        && grep -q '^order	callback.order' "$r/out/compat-report/compat-report.tsv" \
        && grep -q '^timeout	login_handshake.timeout' "$r/out/compat-report/compat-report.tsv" \
        && grep -q '^field	login_wire.login' "$r/out/compat-report/compat-report.tsv"; then
        ci_pass selftest-report-locates "each injected difference is located by its kind and boundary"
    else
        ci_fail selftest-report-locates "injected differences were not located: rc=$rc $(grep 'compat-report' "$M" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a comparison with no differences reports none and is not hollow"
    r2="$(mktemp -d "$REPO_ROOT/ci-out/compat-selftest.XXXXXX")"
    T2="$r2/clean"
    M2="$r2/manifest/manifest.tsv"
    mkdir -p "$T2"
    cat >"$T2/compare-summary.json" <<'JSON'
{"comparison": [
  {"boundary": "login_wire.login", "original": {"id": 0}, "new": {"id": 0},
   "agree": true, "note": "identical"}
]}
JSON
    CI_COMPAT_SUMMARY="$T2/compare-summary.json" CI_COMPAT_OUT="$r2/out" \
        CI_OUT="$r2/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'compat-report	PASS	0 difference(s) located' "$M2"; then
        ci_pass selftest-report-clean "an agreeing comparison reports zero differences and is not hollow"
    else
        ci_fail selftest-report-clean "an agreeing comparison did not report cleanly: rc=$rc $(grep 'compat-report' "$M2" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: no comparison summary reports SKIP naming the ticket"
    r2="$(mktemp -d "$REPO_ROOT/ci-out/compat-selftest.XXXXXX")"
    T2="$r2/none"
    M2="$r2/manifest/manifest.tsv"
    CI_COMPAT_SUMMARY="$T2/compare-summary.json" CI_COMPAT_OUT="$r2/out" \
        CI_OUT="$r2/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'compat-report	SKIP	.*BW-00-04-04' "$M2"; then
        ci_pass selftest-report-absent "an absent summary reports SKIP naming BW-00-04-04"
    else
        ci_fail selftest-report-absent "an absent summary did not skip cleanly: rc=$rc"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$r2" "$REPO_ROOT/ci-out/compat-selftest".*

    if [ "$fails" -gt 0 ]; then
        ci_note "$fails self-test(s) did not behave"
        return 1
    fi
    ci_note "all self-tests behaved as expected"
    return 0
}

main() {
    case "$MODE" in
    self-test) self_test; return $? ;;
    esac
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
