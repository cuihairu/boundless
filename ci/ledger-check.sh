#!/usr/bin/env bash
# BW-00-05-06 — verify the feature-coverage ledger is complete.
#
# Usage:
#   ci/ledger-check.sh            # validate the committed ledger
#   ci/ledger-check.sh self-test  # prove each violation kind is caught
#
# The ledger (docs/review/bw00-05-ledger.tsv, built by docs/review/tools/
# build_ledger.py) indexes every original build target with its enable
# condition, owning lane, fixture and status. This lane is the completeness
# gate over it: every enabled target must have run evidence (covered/partial)
# or a recorded blocker (gap), and the committed artifact must be the one the
# generator produces from the reference tree — so the index cannot silently
# drift. Without the reference tree the regeneration check is skipped and the
# committed ledger is still validated in place.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
SANDBOX="$(mktemp -d /tmp/bw00-ledger.XXXXXX)"

cleanup() {
    rm -rf "$SANDBOX"
}
trap cleanup EXIT
trap 'exit 143' TERM
trap 'exit 130' INT
trap 'exit 129' HUP

# The reference tree defaults to a home-relative checkout, so the lane runs
# locally with no setup. CI_FULLSTACK_REFERENCE or BW_REFERENCE overrides it.
REFERENCE="${CI_FULLSTACK_REFERENCE:-${BW_REFERENCE:-$HOME/workspaces/BigWorld}}"

LEDGER="$REPO_ROOT/docs/review/bw00-05-ledger.tsv"
CHECKER="$REPO_ROOT/docs/review/tools/check_ledger.py"
LEDGER_RECORD="$CI_OUT/ledgercheck/ledger.tsv"

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

# --- the lane ----------------------------------------------------------------

run_all() {
    ci_step "ledger: every enabled target has evidence or an explicit gap"
    mkdir -p "$CI_OUT/ledgercheck" "$CI_LOG_DIR"
    printf 'check\tresult\tdetail\n' >"$LEDGER_RECORD"

    # The committed ledger must be complete on its own.
    local out rc
    out="$(python3 "$CHECKER" "$LEDGER" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'in-place\tFAIL\t%s\n' "$(tsv_escape "$out")" >>"$LEDGER_RECORD"
        ci_fail ledger-completeness "$(tsv_escape "$(printf '%s\n' "$out" | head -1)")"
        cat "$LEDGER_RECORD" >>"$CI_LOG_DIR/ledgercheck.log"
        return 0
    fi

    # And it must be the generator's output. Without the reference tree this
    # half is skipped: the in-place validation still ran.
    if [ ! -d "$REFERENCE/programming/bigworld" ]; then
        printf 'regenerate\tSKIP\tno reference tree (%s)\n' "$REFERENCE" >>"$LEDGER_RECORD"
        printf 'in-place\tPASS\t-\n' >>"$LEDGER_RECORD"
        ci_pass ledger-completeness "the committed ledger is complete"
        ci_skip ledger-regenerate "no reference tree at $REFERENCE — regeneration not run"
        cat "$LEDGER_RECORD" >>"$CI_LOG_DIR/ledgercheck.log"
        return 0
    fi

    out="$(python3 "$CHECKER" "$LEDGER" --regenerate "$REFERENCE" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'regenerate\tFAIL\t%s\n' "$(tsv_escape "$out")" >>"$LEDGER_RECORD"
        ci_fail ledger-regenerate "$(tsv_escape "$(printf '%s\n' "$out" | head -1)")"
    else
        printf 'regenerate\tPASS\t-\n' >>"$LEDGER_RECORD"
        ci_pass ledger-regenerate "the committed ledger matches a fresh build of the reference tree"
    fi
    cat "$LEDGER_RECORD" >>"$CI_LOG_DIR/ledgercheck.log"
}

# --- self-test (BW-00-05-06 acceptance) --------------------------------------
#
# The acceptance: a ledger that is incomplete is rejected. The self-test plants
# one ledger per violation kind and asserts the checker names it — a self-test
# that went green here would mean the gate cannot see what the task is about.
# The planted ledgers live in the sandbox; the failures are not recorded.

self_test() {
    local fails=0 out rc
    local planted="$SANDBOX/planted.tsv"

    # kind 1: an enabled target with neither evidence nor a gap. cstdmf is
    # covered; marking it not-enabled leaves a Linux-enabled row that is
    # neither, which the checker must reject.
    awk -v OFS="	" -F"	" 'NR>1 && $3=="lib/cstdmf" && $1=="cstdmf" { $7="not-enabled" }
                 { print }' "$LEDGER" >"$planted"

    ci_step "self-test: an enabled target without evidence is rejected"
    out="$(python3 "$CHECKER" "$planted" 2>&1)"
    rc=$?
    if [ "$rc" -eq 1 ] && printf '%s' "$out" | grep -q "lib/cstdmf"; then
        ci_pass selftest-ledger-missing-evidence \
            "a Linux-enabled target with no evidence is named"
    else
        ci_fail selftest-ledger-missing-evidence \
            "the missing-evidence violation was not caught (rc=$rc: $(printf '%s' "$out" | head -1))"
        fails=$((fails + 1))
    fi

    # kind 2: a gap row with no recorded blocker
    awk -v OFS="	" -F"	" 'NR>1 && $7=="gap" { $6="" } { print }' "$LEDGER" >"$planted"

    ci_step "self-test: a gap without its blocker is rejected"
    out="$(python3 "$CHECKER" "$planted" 2>&1)"
    rc=$?
    if [ "$rc" -eq 1 ] && printf '%s' "$out" | grep -q "gap"; then
        ci_pass selftest-ledger-unrecorded-gap \
            "a gap with no blocker is rejected"
    else
        ci_fail selftest-ledger-unrecorded-gap \
            "the unrecorded-gap violation was not caught (rc=$rc: $(printf '%s' "$out" | head -1))"
        fails=$((fails + 1))
    fi

    # kind 3: a missing field
    awk -v OFS="	" -F"	" 'NR>1 && $3=="lib/math" { $6="" } { print }' "$LEDGER" >"$planted"

    ci_step "self-test: a row missing its fixture is rejected"
    out="$(python3 "$CHECKER" "$planted" 2>&1)"
    rc=$?
    if [ "$rc" -eq 1 ] && printf '%s' "$out" | grep -q "lib/math"; then
        ci_pass selftest-ledger-missing-field \
            "a row missing its fixture is rejected"
    else
        ci_fail selftest-ledger-missing-field \
            "the missing-field violation was not caught (rc=$rc: $(printf '%s' "$out" | head -1))"
        fails=$((fails + 1))
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

if [ "$LANE_RC" -ne 0 ]; then
    exit "$LANE_RC"
fi
if [ "$FINAL_RC" -ne 0 ]; then
    exit "$FINAL_RC"
fi
