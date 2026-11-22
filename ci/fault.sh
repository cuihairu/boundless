#!/usr/bin/env bash
# BW-22-03-03 — the manual fault scenarios: reproduce a failure and leave the
# evidence behind.
#
# Usage:
#   ci/fault.sh            # run the process-fault fixtures through the shared entry
#   ci/fault.sh self-test  # prove a failed drill still leaves topology and logs
#
# The acceptance is "when a failure reproduces, there is topology and logs".
# The drill runs the BW-18-03-04 process-fault fixtures (crash, restart,
# migration replay) through the local/CI shared entry (BW-18-04-01,
# run-tests), which writes a run archive with the commit and the rerun
# command in its header. Whatever the outcome, the lane copies that archive,
# records the drill's topology and keeps the full log — a green drill and a
# red drill leave the same three files behind.
#
# Live-cluster fault drills (kill a role in a deployed topology and watch it
# recover) need a deployment to drill into; they are tracked by BW-05-06-04
# and join this lane the day the substrate lands.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
# Overrides are read at call time, not at source time: an override applied to
# a shell function only reaches code that re-reads the variable. The self-test
# swaps the entry binary and the archive directory through these.
cur_bin() { printf '%s' "${CI_FAULT_BIN:-cargo}"; }
cur_root() { printf '%s' "${CI_FAULT_ROOT:-$REPO_ROOT}"; }

MISSING=2
FIXTURES="${CI_FAULT_FIXTURES:-multiproc}"

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

# The entry derives its own archive path from the filter; the lane mirrors
# that derivation, and the self-test overrides it to point at a stub.
archive_path() {
    printf '%s/target/test-results/test-support-%s/run.txt' "$(cur_root)" "$FIXTURES"
}

# --- detectors ---------------------------------------------------------------

detect_entry() {
    if [ ! -f "$(cur_root)/crates/test-support/src/bin/run-tests.rs" ]; then
        echo "no test-support run-tests entry under $(cur_root) (tracked by BW-18-04-01)"
        return "$MISSING"
    fi
    if ! command -v "$(cur_bin)" >/dev/null 2>&1 && [ ! -x "$(cur_bin)" ]; then
        echo "no $(cur_bin) on this runner, so the drill cannot run"
        return "$MISSING"
    fi
    echo "test-support run-tests entry present, driver $(cur_bin)"
    return 0
}

# --- the drill ---------------------------------------------------------------

# One drill: run, then leave topology.tsv, run.txt and the full log behind
# no matter how it ended. Pure of manifest writes so the self-test can drive
# it directly; prints the verdict text, returns 0 only on a green drill.
run_drill() {
    local bin archive rc out fails
    bin="$(cur_bin)"
    local outdir="$CI_OUT/fault"
    mkdir -p "$outdir" "$CI_LOG_DIR"
    archive="$(archive_path)"

    out="$(CI_FAULT_ARCHIVE_DIR="$(dirname "$archive")" \
        "$bin" run -q -p test-support --bin run-tests -- test-support "$FIXTURES" 2>&1)"
    rc=$?
    printf '%s\n' "$out" >"$CI_LOG_DIR/fault-drill.log"

    # The evidence is kept before any verdict: a red drill is exactly the run
    # this lane exists to make retrievable.
    [ -f "$archive" ] && cp "$archive" "$outdir/run.txt"

    # The topology of what ran: which entry, which fixtures, what commit, and
    # where the evidence lives. The commit comes from the archive header —
    # that is the tree the drill actually measured — falling back to the
    # working tree when the entry wrote nothing.
    local drill_commit
    if [ -f "$outdir/run.txt" ] \
        && drill_commit="$(sed -n 's/^commit: //p' "$outdir/run.txt" | head -1)" \
        && [ -n "$drill_commit" ]; then
        :
    else
        drill_commit="$(git -C "$(cur_root)" rev-parse --short HEAD 2>/dev/null || echo unknown)"
    fi
    {
        printf 'commit\t%s\n' "$drill_commit"
        printf 'entry\t%s run -q -p test-support --bin run-tests -- test-support %s\n' "$bin" "$FIXTURES"
        printf 'fixtures\t%s (process fault fixtures: crash, restart, migration replay)\n' "$FIXTURES"
        printf 'cluster\tnone — in-process fixture drills; live-topology drills track BW-05-06-04\n'
        printf 'archive\tfault/run.txt\n'
        printf 'log\tlogs/fault-drill.log\n'
    } >"$outdir/topology.tsv"

    if [ "$rc" -ne 0 ]; then
        echo "the drill failed (rc=$rc): $(printf '%s' "$out" | grep -E '^FAIL ' | head -3 | tr '\n' '; ')$(printf '%s' "$out" | tail -1 | cut -c1-90)"
        return 1
    fi
    if [ ! -f "$archive" ]; then
        echo "the entry ran green but wrote no archive at $archive"
        return 1
    fi
    fails="$(grep -c '^FAIL ' "$outdir/run.txt" || true)"
    if [ "$fails" -gt 0 ]; then
        echo "$fails fixture case(s) failed despite exit 0: $(grep '^FAIL ' "$outdir/run.txt" | head -3 | tr '\n' '; ')"
        return 1
    fi
    echo "$(grep -c '^PASS ' "$outdir/run.txt") fixture case(s) green: fault/run.txt"
    return 0
}

run_all() {
    ci_step "manual fault scenarios (fixtures $FIXTURES)"
    mkdir -p "$CI_OUT"
    local out rc
    out="$(detect_entry 2>&1)"
    rc=$?
    case "$rc" in
    0) ci_pass fault-entry "$(tsv_escape "$out")" ;;
    "$MISSING") ci_skip fault-entry "$(tsv_escape "$out")" ;;
    *) ci_fail fault-entry "$(tsv_escape "$out")" ;;
    esac
    if [ "$rc" -ne 0 ]; then
        ci_skip fault-drill "$(tsv_escape "$out")"
        return 0
    fi
    ci_step "fault drill (fixtures $FIXTURES)"
    if out="$(run_drill)"; then
        ci_pass fault-drill "$(tsv_escape "$out")"
    else
        ci_fail fault-drill "$(tsv_escape "$out") — evidence kept in ci-out/fault/ and logs/"
    fi
}

self_test() {
    local fails=0 stub r
    ci_step "self-test: a failed drill still leaves topology, archive and log"
    stub="$(mktemp "$REPO_ROOT/ci-out/fault-stub.XXXXXX")"
    cat >"$stub" <<'STUB'
#!/bin/sh
# A drill that reproduces a failure: one FAIL outcome, nonzero exit.
dir="${CI_FAULT_ARCHIVE_DIR:?no archive dir}"
mkdir -p "$dir"
cat >"$dir/run.txt" <<'TXT'
# run archive
label: test-support-multiproc
run: cargo test -p test-support multiproc
commit: deadbee
when: 2026-11-20T00:00:00Z
PASS multiproc::tests::a_crashed_role_restarts_with_a_fresh_pid
FAIL multiproc::tests::a_migration_scenario_replays_identically
summary: 1 passed, 1 failed, 0 skipped
TXT
echo "planted failure scene"
exit 1
STUB
    chmod +x "$stub"
    r="$(mktemp -d "$REPO_ROOT/ci-out/fault-selftest.XXXXXX")"

    local out verdict
    rm -rf "$CI_OUT/fault"
    if verdict="$(CI_FAULT_BIN="$stub" run_drill)"; then
        ci_fail selftest-fault "a drill that reported FAIL still passed the lane"
        fails=$((fails + 1))
    fi
    # The acceptance: the failure reproduced AND the evidence is all there.
    if [ -f "$CI_OUT/fault/topology.tsv" ] \
        && grep -q '^entry	.*run-tests' "$CI_OUT/fault/topology.tsv" \
        && grep -q '^commit	deadbee$' "$CI_OUT/fault/topology.tsv" \
        && [ -f "$CI_OUT/fault/run.txt" ] \
        && grep -q '^FAIL multiproc::tests::a_migration_scenario_replays_identically$' "$CI_OUT/fault/run.txt" \
        && grep -q 'planted failure scene' "$CI_LOG_DIR/fault-drill.log"; then
        ci_pass selftest-fault "a failed drill leaves topology.tsv, run.txt and the log: $verdict"
    else
        ci_fail selftest-fault "a failed drill did not leave complete evidence behind"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a green drill passes and records the topology"
    cat >"$stub" <<'STUB'
#!/bin/sh
dir="${CI_FAULT_ARCHIVE_DIR:?no archive dir}"
mkdir -p "$dir"
cat >"$dir/run.txt" <<'TXT'
# run archive
label: test-support-multiproc
run: cargo test -p test-support multiproc
commit: deadbee
when: 2026-11-20T00:00:00Z
PASS multiproc::tests::a_crashed_role_restarts_with_a_fresh_pid
PASS multiproc::tests::a_failure_scene_keeps_the_original_logs_and_a_report
summary: 2 passed, 0 failed, 0 skipped
TXT
exit 0
STUB
    chmod +x "$stub"
    rm -rf "$CI_OUT/fault"
    if verdict="$(CI_FAULT_BIN="$stub" run_drill)"; then
        ci_pass selftest-fault-drill "a green drill passes and keeps the same evidence: $verdict"
    else
        ci_fail selftest-fault-drill "a green drill did not pass: $verdict"
        fails=$((fails + 1))
    fi

    ci_step "self-test: no entry means a SKIP, not a false PASS"
    r="$(mktemp -d "$REPO_ROOT/ci-out/fault-selftest.XXXXXX")"
    if [ "$(CI_FAULT_ROOT="$r" detect_entry >/dev/null 2>&1; echo $?)" = "$MISSING" ]; then
        ci_pass selftest-fault-empty "no entry means the case reports SKIP, not a false PASS"
    else
        ci_fail selftest-fault-empty "a bare tree produced a verdict other than SKIP"
        fails=$((fails + 1))
    fi

    rm -rf "$stub" "$REPO_ROOT/ci-out/fault-selftest".*

    if [ "$fails" -gt 0 ]; then
        ci_note "$fails self-test(s) did not behave"
        return 1
    fi
    ci_note "all self-tests behaved as expected"
    return 0
}

main() {
    if [ "$MODE" = self-test ]; then
        self_test
        return $?
    fi
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
