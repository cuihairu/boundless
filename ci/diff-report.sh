#!/usr/bin/env bash
# BW-00-04-06 — the difference report: the fullstack lane's two evidences per
# input, compared field by field, every difference localized by path and kind.
#
# Usage:
#   ci/diff-report.sh            # compare every pair the fullstack lane produced
#   ci/diff-report.sh self-test  # prove each difference kind is located
#
# The fullstack lane (BW-00-04-04) writes, per fixture, the original stack's
# evidence and the new stack's evidence into $CI_OUT/fullstack/{ref,new}/. This
# lane is the report over that pair: it asks the engine whether the two are the
# same artifact, and when they are not it names the field (injected/omitted),
# the run that never produced evidence (timeout), the reordered array (order),
# or the changed value (value). The protocol and callback scenarios have a
# reference evidence only — the new half's producer does not exist yet — so
# they name their blocker instead of comparing a single side.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
SANDBOX="$(mktemp -d /tmp/bw00-diffreport.XXXXXX)"

cleanup() {
    rm -rf "$SANDBOX"
}
trap cleanup EXIT
# Bash does not run an EXIT trap for a fatal signal, and a cancelled job sends
# exactly that — the one moment a probe tree would outlive the run.
trap 'exit 143' TERM
trap 'exit 130' INT
trap 'exit 129' HUP

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

diffrecord_path() { printf '%s/diffreport/%s.tsv' "$CI_OUT" "$1"; }

# --- substrates ------------------------------------------------------------

DIFF_ENGINE="$REPO_ROOT/ci/diff_report.py"
# The fixtures the fullstack lane produces evidence for; the protocol and
# callback scenarios have no new half yet and are handled separately.
ENTITYDEF_CASES="full_def:full service_def:service entity_index:entity_index"

# Everything the lane needs, collected so one SKIP names every missing piece.
# The fullstack lane is the producer of the evidence this lane reads: without
# it there is nothing to compare, and that is reported as such rather than
# looking like an agreement that never happened.
missing_substrates() {
    local missing=""
    [ -f "$DIFF_ENGINE" ] || missing="$missing diff engine $DIFF_ENGINE"
    [ -d "$CI_OUT/fullstack/ref" ] || \
        missing="$missing fullstack reference evidence ($CI_OUT/fullstack/ref)"
    [ -d "$CI_OUT/fullstack/new" ] || \
        missing="$missing fullstack new evidence ($CI_OUT/fullstack/new)"
    printf '%s' "${missing# }"
}

# The protocol and callback scenarios record the original stack's evidence
# only; the new half names its producer rather than pretending the diff ran.
record_reference_only() { # $1 scenario $2 new-half blocker
    local scenario="$1" blocker="$2"
    ci_skip "diffreport-$scenario" "$(tsv_escape "$blocker")"
}

# --- the lane ----------------------------------------------------------------

run_all() {
    local missing_sub
    ci_step "difference report: fullstack evidences compared field by field"
    mkdir -p "$CI_OUT/diffreport" "$CI_LOG_DIR"

    missing_sub="$(missing_substrates)"
    if [ -n "$missing_sub" ]; then
        ci_skip diffreport "$(tsv_escape "$missing_sub") — the fullstack lane is the evidence producer"
        return 0
    fi

    local rec
    rec="$(diffrecord_path entitydef)"
    printf 'fixture\tresult\tfindings\n' >"$rec"

    local case name ref new rc findings first
    for case in $ENTITYDEF_CASES; do
        name="${case%%:*}"
        ref="$CI_OUT/fullstack/ref/$name.json"
        new="$CI_OUT/fullstack/new/$name.json"
        if [ ! -s "$ref" ] || [ ! -s "$new" ]; then
            printf '%s\tSKIP\tno fullstack evidence for this fixture\n' "$name" >>"$rec"
            ci_skip "diffreport-$name" \
                "no fullstack evidence for this fixture (run ci/fullstack-compare.sh first)"
            continue
        fi

        findings="$(python3 "$DIFF_ENGINE" json "$ref" "$new" 2>&1)"
        rc=$?
        if [ "$rc" -eq 0 ]; then
            printf '%s\tPASS\t-\n' "$name" >>"$rec"
            ci_pass "diffreport-$name" "reference and new evidence agree field by field"
        else
            # rc=1 findings, rc=2 one side missing (timeout).
            first="$(printf '%s\n' "$findings" | head -1)"
            printf '%s\tFAIL\t%s\n' "$name" "$(tsv_escape "$findings")" >>"$rec"
            ci_fail "diffreport-$name" "$(tsv_escape "$first")"
        fi
    done

    cat "$rec" >>"$CI_LOG_DIR/diffreport-entitydef.log"

    record_reference_only protocol \
        "no new protocol evidence — the mercury wire producer does not exist yet (crates/mercury has no runnable byte entry point)"
    record_reference_only callbacks \
        "no new callback evidence — the python-host callback producer does not exist yet (crates/python-host has no ScriptEventList)"
}

# --- self-test (BW-00-04-06 acceptance) --------------------------------------
#
# The acceptance: an injected field, a timeout and an order difference are each
# located. The self-test plants exactly those three and asserts the engine names
# the field/run/array — a self-test that went green here would mean the report
# cannot see what the task is about. The planted files live in the sandbox; the
# assertions run against the engine's own exit code and output, so a planted
# failure is not itself recorded in the manifest.

self_test() {
    local fails=0
    ci_step "self-test: an injected field is located by the report"
    local ref="$REPO_ROOT/docs/review/reference/full_entitydef.json"
    local planted="$SANDBOX/planted_injected.json"
    if ! python3 - "$ref" "$planted" <<'EOF'
import copy, json, sys
ref = json.load(open(sys.argv[1]))
new = copy.deepcopy(ref)
new["entityTypes"][0]["allProperties"][0]["injectedFlag"] = True
json.dump(new, open(sys.argv[2], "w"))
EOF
    then
        ci_fail selftest-diffreport-injected "could not plant the injected field"
        fails=$((fails + 1))
    else
        local out rc
        out="$(python3 "$DIFF_ENGINE" json "$ref" "$planted" 2>&1)"
        rc=$?
        if [ "$rc" -eq 1 ] && printf '%s' "$out" | grep -q \
            '^injected	\$\.entityTypes\[0\]\.allProperties\[0\]\.injectedFlag'; then
            ci_pass selftest-diffreport-injected \
                "an injected field is reported with its path"
        else
            ci_fail selftest-diffreport-injected \
                "the injected field was not located (rc=$rc: $(printf '%s' "$out" | head -1))"
            fails=$((fails + 1))
        fi
    fi

    ci_step "self-test: a missing new evidence (timeout) is named"
    out="$(python3 "$DIFF_ENGINE" json "$ref" "$SANDBOX/does_not_exist.json" 2>&1)"
    rc=$?
    if [ "$rc" -eq 2 ] && printf '%s' "$out" | grep -q '^timeout'; then
        ci_pass selftest-diffreport-timeout \
            "a run that produced no evidence is reported as a timeout, not a value mismatch"
    else
        ci_fail selftest-diffreport-timeout \
            "the timeout was not named (rc=$rc: $(printf '%s' "$out" | head -1))"
        fails=$((fails + 1))
    fi

    ci_step "self-test: an order difference is located at the array"
    local trace="$REPO_ROOT/docs/review/fixtures/callbacks/callback-trace.txt"
    local swapped="$SANDBOX/planted_order.txt"
    if ! python3 - "$trace" "$swapped" <<'EOF'
import sys
lines = open(sys.argv[1]).read().splitlines()
idxs = [i for i, l in enumerate(lines) if l.startswith("event ")]
assert len(idxs) >= 2, "the callback trace needs at least two events"
i, j = idxs[0], idxs[1]
bi = idxs[2] if len(idxs) > 2 else len(lines)
open(sys.argv[2], "w").write("\n".join(lines[:i] + lines[j:bi] + lines[i:j] + lines[bi:]))
EOF
    then
        ci_fail selftest-diffreport-order "could not plant the swapped event order"
        fails=$((fails + 1))
    else
        out="$(python3 "$DIFF_ENGINE" trace "$trace" "$swapped" 2>&1)"
        rc=$?
        if [ "$rc" -eq 1 ] && printf '%s' "$out" | grep -q '^order	\$\.events'; then
            ci_pass selftest-diffreport-order \
                "a reordered event sequence is reported at \$.events"
        else
            ci_fail selftest-diffreport-order \
                "the order difference was not located (rc=$rc: $(printf '%s' "$out" | head -1))"
            fails=$((fails + 1))
        fi
    fi

    if [ "$fails" -gt 0 ]; then
        ci_note "$fails self-test(s) did not behave"
        return 1
    fi
    ci_note "all self-tests behaved as expected"
    return 0
}

main() {
    case "$MODE" in
    self-test)
        self_test
        return $?
        ;;
    *)
        run_all
        ;;
    esac
}

main "$@"
LANE_RC=$?

set +e
ci_finalize
FINAL_RC=$?
set -e

# An `[ x ] && exit` tail returns the failed test's status (1) when x is zero,
# so a healthy lane would exit 1 and fail the job. Branch instead.
if [ "$LANE_RC" -ne 0 ]; then
    exit "$LANE_RC"
fi
if [ "$FINAL_RC" -ne 0 ]; then
    exit "$FINAL_RC"
fi
