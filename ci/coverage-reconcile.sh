#!/usr/bin/env bash
# BW-00-05-06 — every enabled target has evidence or an explicit gap.
#
# Usage:
#   ci/coverage-reconcile.sh            # close the coverage ledger
#   ci/coverage-reconcile.sh self-test  # planted ledger and maps prove it
#
# The ledger (BW-00-05-01) enumerates the reference's build targets and the
# mappers (BW-00-05-02/03/04) index each: a port crate, or the BW-xx ticket
# whose source scope owns it. This lane joins the two and proves the ledger's
# closing acceptance: a target has runnable evidence or a traceable blocking
# reason. A bare target with no curated reason fails the reconcile — the hole
# is named, not waved through.
#
# One record face:
#   * reconcile-record — coverage-reconcile.json + coverage-reconcile.tsv. An
#     absent ledger skips naming the ticket; a hollow map (a bare target
#     without a curated blocking reason) is not a record.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
cur_ledger() {
    if [ -n "${CI_REC_LEDGER:-}" ]; then
        printf '%s' "$CI_REC_LEDGER"
    elif [ -f "$CI_OUT/coverage-ledger/coverage-ledger.json" ]; then
        printf '%s' "$CI_OUT/coverage-ledger/coverage-ledger.json"
    else
        printf '%s' "$REPO_ROOT/tests/baseline/coverage-ledger.json"
    fi
}
cur_maps() {
    if [ -n "${CI_REC_MAPS:-}" ]; then
        printf '%s' "$CI_REC_MAPS"
    elif [ -d "$CI_OUT/coverage-map-BW000502" ]; then
        printf '%s' "$CI_OUT"
    else
        printf '%s' "$REPO_ROOT/tests/baseline"
    fi
}
cur_out() { printf '%s' "${CI_REC_OUT:-$CI_OUT}"; }
rec_dir() { printf '%s/coverage-reconcile' "$(cur_out)"; }

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    ci_step "close the coverage ledger: evidence or an explicit gap"

    local ledger maps
    ledger="$(cur_ledger)"
    maps="$(cur_maps)"
    if [ ! -f "$ledger" ] || { [ ! -e "$maps/coverage-map-BW000502.json" ] \
        && [ ! -d "$maps/coverage-map-BW000502" ]; }; then
        ci_skip reconcile-record \
            "no coverage ledger or maps at $ledger / $maps — the reconcile closes the ledger when they exist (BW-00-05-06)"
        return 0
    fi

    local out_dir
    out_dir="$(rec_dir)"
    local sum
    if ! sum="$(python3 "$REPO_ROOT/ci/coverage-reconcile.py" "$ledger" "$maps" \
        "$out_dir" --self-check 2>&1)"; then
        ci_fail reconcile-record "the reconcile failed to record: $(printf '%s' "$sum" | tail -1)"
        return 1
    fi
    local total evidence gap
    total="$(printf '%s' "$sum" | sed -n 's/.*reconciled=\([0-9]*\).*/\1/p')"
    evidence="$(printf '%s' "$sum" | sed -n 's/.*evidence=\([0-9]*\).*/\1/p')"
    gap="$(printf '%s' "$sum" | sed -n 's/.*gap=\([0-9]*\).*/\1/p')"
    if [ -z "$total" ] || [ "$total" -eq 0 ]; then
        ci_fail reconcile-record "the reconcile recorded no row count — a hollow map is not a record"
        return 1
    fi
    ci_pass reconcile-record "$total target(s) reconciled ($evidence with evidence, $gap explicit gap)"

    # The committed snapshot is the anchor; a map that grows is a note.
    local f
    for f in json tsv; do
        if [ -f "$REPO_ROOT/tests/baseline/coverage-reconcile.$f" ] \
            && [ -f "$out_dir/coverage-reconcile.$f" ] \
            && ! cmp -s "$REPO_ROOT/tests/baseline/coverage-reconcile.$f" "$out_dir/coverage-reconcile.$f"; then
            ci_note "the committed tests/baseline/coverage-reconcile.$f differs from this run — snapshot-time anchor, not a failure"
        fi
    done

    ci_note "every target has runnable evidence or a traceable blocking reason"
}

plant_ledger() { # $1 dir
    python3 - "$1" <<'EOF'
import json, sys
rows = [
    {"original_path": "network (lib/network)", "kind": "library",
     "enable_condition": "always", "task": "BW-00-05-02",
     "fixture": "pending", "status": "unmapped"},
    {"original_path": "worldeditor (tools/worldeditor)", "kind": "executable",
     "enable_condition": "always", "task": "BW-00-05-03",
     "fixture": "pending", "status": "unmapped"},
    {"original_path": "urlrequest (lib/urlrequest)", "kind": "library",
     "enable_condition": "always", "task": "BW-00-05-02",
     "fixture": "pending", "status": "unmapped"},
]
json.dump({"rows": rows, "count": len(rows)},
          open(sys.argv[1] + "/coverage-ledger.json", "w"))
EOF
}

plant_maps() { # $1 dir
    python3 - "$1" <<'EOF'
import json, sys
rows = [
    {"original_path": "network (lib/network)", "kind": "library",
     "enable_condition": "always", "index": "crates/mercury",
     "ticket": "BW-02", "mapped": True, "how": "mercury ports lib/network"},
    {"original_path": "worldeditor (tools/worldeditor)", "kind": "executable",
     "enable_condition": "always", "index": "none yet", "ticket": "BW-13",
     "mapped": False, "how": "the ticket's source scope covers it"},
    {"original_path": "urlrequest (lib/urlrequest)", "kind": "library",
     "enable_condition": "always", "index": "none yet", "ticket": "none",
     "mapped": False, "how": "no BW-xx source scope covers it"},
]
json.dump({"rows": rows, "count": len(rows)},
          open(sys.argv[1] + "/coverage-map-BW000502.json", "w"))
EOF
}

self_test() {
    local fails=0 rc r T M r2 T2 M2 r3 T3 M3
    ci_step "self-test: crate and ticket rows resolve and the bare gap carries its curated reason"
    r="$(mktemp -d "$REPO_ROOT/ci-out/rec-selftest.XXXXXX")"
    T="$r/led"
    M="$r/manifest/manifest.tsv"
    mkdir -p "$T" "$r/maps"
    plant_ledger "$T"
    plant_maps "$r/maps"

    CI_REC_OUT="$r/out" CI_OUT="$r/manifest" \
        CI_REC_LEDGER="$T/coverage-ledger.json" CI_REC_MAPS="$r/maps" \
        "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q '^network (lib/network)	library	crate	$' "$r/out/coverage-reconcile/coverage-reconcile.tsv" \
        && grep -q '^worldeditor (tools/worldeditor)	executable	ticket	$' "$r/out/coverage-reconcile/coverage-reconcile.tsv" \
        && grep -q '^urlrequest (lib/urlrequest)	library	none	no port crate ships an HTTP/curl binding' "$r/out/coverage-reconcile/coverage-reconcile.tsv"; then
        ci_pass selftest-reconcile-indexes "crate and ticket rows resolve; the bare target records its curated blocking reason"
    else
        ci_fail selftest-reconcile-indexes "planted rows were not reconciled as expected: rc=$rc $(grep 'reconcile-record' "$M" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: an absent ledger and maps skip naming the ticket"
    r2="$(mktemp -d "$REPO_ROOT/ci-out/rec-selftest.XXXXXX")"
    T2="$r2/none"
    M2="$r2/manifest/manifest.tsv"
    CI_REC_OUT="$r2/out" CI_OUT="$r2/manifest" \
        CI_REC_LEDGER="$T2/coverage-ledger.json" CI_REC_MAPS="$T2" \
        "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'reconcile-record	SKIP	.*BW-00-05-06' "$M2"; then
        ci_pass selftest-reconcile-absent "an absent ledger reports SKIP naming BW-00-05-06"
    else
        ci_fail selftest-reconcile-absent "an absent ledger did not skip cleanly: rc=$rc"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a bare target without a curated reason fails the reconcile"
    r3="$(mktemp -d "$REPO_ROOT/ci-out/rec-selftest.XXXXXX")"
    T3="$r3/led"
    M3="$r3/manifest/manifest.tsv"
    mkdir -p "$T3" "$r3/maps"
    plant_ledger "$T3"
    plant_maps "$r3/maps"
    # Swap the bare row for one the plan has not articulated a reason for —
    # in both the ledger and the map, so the join still finds it bare.
    python3 - "$r3/maps/coverage-map-BW000502.json" "$T3/coverage-ledger.json" <<'EOF'
import json, sys
for p in sys.argv[1:3]:
    d = json.load(open(p))
    for r in d["rows"]:
        if r["original_path"].startswith("urlrequest"):
            r["original_path"] = "mystery (lib/mystery)"
    json.dump(d, open(p, "w"))
EOF

    CI_REC_OUT="$r3/out" CI_OUT="$r3/manifest" \
        CI_REC_LEDGER="$T3/coverage-ledger.json" CI_REC_MAPS="$r3/maps" \
        "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -ne 0 ] \
        && grep -q 'reconcile-record	FAIL	.*hollow map' "$M3"; then
        ci_pass selftest-reconcile-hollow "a bare target with no curated reason fails rather than recording"
    else
        ci_fail selftest-reconcile-hollow "an uncurated bare target did not fail cleanly: rc=$rc $(grep 'reconcile-record' "$M3" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$r2" "$r3" "$REPO_ROOT/ci-out/rec-selftest".*

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
