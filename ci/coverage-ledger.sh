#!/usr/bin/env bash
# BW-00-05-01 — the coverage ledger fields: the original build target, its
# enable condition, the task that maps it, the fixture that verifies it and its
# coverage status.
#
# Usage:
#   ci/coverage-ledger.sh            # record the ledger from the reference tree
#   ci/coverage-ledger.sh self-test  # planted trees: gated + unconditional rows
#
# The reference's CMake build targets (BW_ADD_LIBRARY/EXECUTABLE/TEST/TOOL_EXE/
# TOOL_TEST/ASSETPIPELINE_DLL/ASSETPIPELINE_EXE) are enumerated under lib/,
# server/, client/, tools/, common/ and examples/, and each row carries the five
# fields the coverage review reads. This lane is the field-establishment step;
# the mapping items (BW-00-05-02/03/04) fill fixture and status per row, and the
# BW-00-05-06 item re-checks the ledger as a whole.
#
# One record face:
#   * ledger-record — coverage-ledger.json + coverage-ledger.tsv, one row per
#     reference build target, each with the five fields. An absent reference
#     tree skips naming the ticket; a hollow ledger (no targets, or a row
#     missing a field) is not a record.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
cur_ref() { printf '%s' "${CI_LEDGER_REF:-${BW_REFERENCE:-$HOME/workspaces/BigWorld}}"; }
cur_out() { printf '%s' "${CI_LEDGER_OUT:-$CI_OUT}"; }
ledger_dir() { printf '%s/coverage-ledger' "$(cur_out)"; }

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    ci_step "coverage ledger fields"

    local ref src
    ref="$(cur_ref)"
    src="$ref/programming/bigworld"
    if [ ! -d "$src" ]; then
        ci_skip ledger-record \
            "no reference source at $src — the ledger enumerates the reference CMake targets (BW-00-05-01)"
        return 0
    fi

    local out_dir
    out_dir="$(ledger_dir)"
    local sum
    if ! sum="$(python3 "$REPO_ROOT/ci/coverage-ledger.py" "$src" "$out_dir" --self-check 2>&1)"; then
        ci_fail ledger-record "the ledger failed to record: $(printf '%s' "$sum" | tail -1)"
        return 1
    fi
    local targets
    targets="$(printf '%s' "$sum" | sed -n 's/.*targets=\([0-9]*\).*/\1/p')"
    if [ -z "$targets" ] || [ "$targets" -eq 0 ]; then
        ci_fail ledger-record "the ledger recorded no target count — a hollow ledger is not a record"
        return 1
    fi
    ci_pass ledger-record "$targets reference target(s) recorded, each with original path, enable condition, task, fixture and status"

    # The committed snapshot is the anchor; a ledger that grows as mapping items
    # land is a note, not a failure.
    local f
    for f in coverage-ledger.json coverage-ledger.tsv; do
        if [ -f "$REPO_ROOT/tests/baseline/$f" ] && [ -f "$out_dir/$f" ] \
            && ! cmp -s "$REPO_ROOT/tests/baseline/$f" "$out_dir/$f"; then
            ci_note "the committed tests/baseline/$f differs from this run — snapshot-time anchor, not a failure"
        fi
    done

    ci_note "ledger rows carry the five coverage fields"
}

# A planted reference tree: one unconditional target, one inside IF(BUILD_X) and
# one else-branch target, each row carrying all five fields.
plant_tree() { # $1 reference root
    local ref="$1"
    mkdir -p "$ref/programming/bigworld/lib/a" "$ref/programming/bigworld/lib/b"
    cat >"$ref/programming/bigworld/lib/a/CMakeLists.txt" <<'EOF'
BW_ADD_LIBRARY( alpha ${SRCS} )
IF(BUILD_BETA)
    BW_ADD_LIBRARY( beta ${SRCS} )
ELSE()
    BW_ADD_LIBRARY( gamma ${SRCS} )
ENDIF()
EOF
    cat >"$ref/programming/bigworld/lib/b/CMakeLists.txt" <<'EOF'
BW_ADD_EXECUTABLE( delta main.cpp )
EOF
}

self_test() {
    local fails=0 rc r T M r2 T2 M2 r3 T3 M3
    ci_step "self-test: a planted reference records gated and unconditional targets with five fields each"
    r="$(mktemp -d "$REPO_ROOT/ci-out/ledger-selftest.XXXXXX")"
    T="$r/BigWorld"
    M="$r/manifest/manifest.tsv"
    plant_tree "$T"

    CI_LEDGER_REF="$T" CI_LEDGER_OUT="$r/out" CI_OUT="$r/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'ledger-record	PASS	4 reference target(s) recorded' "$M" \
        && [ "$(grep -c . "$r/out/coverage-ledger/coverage-ledger.tsv")" -eq 5 ] \
        && grep -q '^alpha (lib/a)	always	BW-00-05-02	pending	unmapped$' "$r/out/coverage-ledger/coverage-ledger.tsv" \
        && grep -q '^beta (lib/a)	BUILD_BETA	BW-00-05-02	pending	unmapped$' "$r/out/coverage-ledger/coverage-ledger.tsv" \
        && grep -q '^gamma (lib/a)	else branch	BW-00-05-02	pending	unmapped$' "$r/out/coverage-ledger/coverage-ledger.tsv" \
        && grep -q '^delta (lib/b)	always	BW-00-05-02	pending	unmapped$' "$r/out/coverage-ledger/coverage-ledger.tsv" \
        && python3 -c "import json,sys; d=json.load(open('$r/out/coverage-ledger/coverage-ledger.json')); sys.exit(0 if d['count']==4 and d['schema']==['original_path','enable_condition','task','fixture','status'] else 1)"; then
        ci_pass selftest-ledger-records "each planted target is recorded with its five fields and its gate"
    else
        ci_fail selftest-ledger-records "planted targets were not recorded with five fields: rc=$rc $(grep 'ledger-record' "$M" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: an absent reference tree skips naming the ticket"
    r2="$(mktemp -d "$REPO_ROOT/ci-out/ledger-selftest.XXXXXX")"
    T2="$r2/none"
    M2="$r2/manifest/manifest.tsv"
    CI_LEDGER_REF="$T2" CI_LEDGER_OUT="$r2/out" CI_OUT="$r2/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'ledger-record	SKIP	.*BW-00-05-01' "$M2"; then
        ci_pass selftest-ledger-absent "an absent reference tree reports SKIP naming BW-00-05-01"
    else
        ci_fail selftest-ledger-absent "an absent reference tree did not skip cleanly: rc=$rc"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a hollow tree (no targets) is not a record"
    r3="$(mktemp -d "$REPO_ROOT/ci-out/ledger-selftest.XXXXXX")"
    T3="$r3/empty"
    M3="$r3/manifest/manifest.tsv"
    mkdir -p "$T3/programming/bigworld/lib"
    CI_LEDGER_REF="$T3" CI_LEDGER_OUT="$r3/out" CI_OUT="$r3/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -ne 0 ] \
        && grep -q 'ledger-record	FAIL	.*hollow ledger' "$M3"; then
        ci_pass selftest-ledger-hollow "a tree with no targets fails as hollow rather than recording"
    else
        ci_fail selftest-ledger-hollow "a hollow tree did not fail cleanly: rc=$rc $(grep 'ledger-record' "$M3" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$r2" "$r3" "$REPO_ROOT/ci-out/ledger-selftest".*

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
