#!/usr/bin/env bash
# BW-22-03-01 — the PR short load test: run the small-scale scenarios and
# record their results.
#
# Usage:
#   ci/load.sh            # run every scenario under tests/load/scenarios
#   ci/load.sh self-test  # prove the harness records, bounds and reports
#
# Scenario contract — the harness that BW-17's load driver targets:
#
#   - a scenario is an executable file tests/load/scenarios/<name>.<ext>
#   - it runs with env:
#       LOAD_DURATION   wall budget in seconds  (default CI_LOAD_DURATION or 30)
#       LOAD_CLIENTS    concurrency             (default CI_LOAD_CLIENTS or 4)
#       LOAD_TARGET     where to drive load     (passed through when set)
#   - it prints "metric<TAB>value" lines on stdout, one per metric
#   - a scenario passes when it exits 0 inside its wall budget and printed at
#     least one metric line; the lane kills it at the budget and records why
#
# The driver crate (apps/bots, BW-17-01-01) and the scenario set (BW-17-03-05)
# do not exist yet, so every case SKIPs and names its ticket. A SKIP stub that
# could never light up would be worthless — the self-test plants both and
# requires the harness to record a result and to bound a runaway.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
# CI_LOAD_ROOT lets the self-test plant substrate without touching the repo.
# Read at call time: an override applied to a shell function only reaches code
# that re-reads the variable.
cur_root() { printf '%s' "${CI_LOAD_ROOT:-$REPO_ROOT}"; }

MISSING=2

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

record_path() { printf '%s/load/%s.tsv' "$CI_OUT" "$1"; }

# --- detectors ---------------------------------------------------------------

# docs/operations/load-testing.md fixes the layout: apps/bots plus
# tests/load/scenarios. A whole missing tree is a SKIP; a tree that has
# started to exist must be checked, not reported away.
detect_scenarios() {
    local dir
    dir="$(cur_root)/tests/load/scenarios"
    if [ ! -d "$dir" ]; then
        echo "no scenarios under $(cur_root)/tests/load/scenarios (the PR smoke scenario set is tracked by BW-17-03-05)"
        return "$MISSING"
    fi
    local found=0 f
    for f in "$dir"/*; do
        [ -f "$f" ] && [ -x "$f" ] || continue
        found=$((found + 1))
    done
    if [ "$found" -eq 0 ]; then
        echo "tests/load/scenarios exists but carries no executable scenario"
        return 1
    fi
    echo "tests/load/scenarios carries $found executable scenario(s)"
    return 0
}

# The load driver is BW-17's first substrate item: a crate that opens client
# connections the way the original bots app does.
detect_driver() {
    if [ ! -d "$(cur_root)/apps/bots" ]; then
        echo "no apps/bots crate (the load driver is tracked by BW-17-01-01)"
        return "$MISSING"
    fi
    if [ ! -f "$(cur_root)/apps/bots/Cargo.toml" ]; then
        echo "apps/bots exists but is not a crate yet (no Cargo.toml)"
        return 1
    fi
    echo "apps/bots crate present"
    return 0
}

# --- the runner ---------------------------------------------------------------

# The thresholds lane (BW-17-03-06): the capacity and soak jobs check the
# recorded scenario results against the baseline thresholds in
# tests/load/baselines.tsv — the recorded baselines the acceptance asks the
# jobs' thresholds to come from. Every rule is an envelope-independent
# invariant (rates and counts the offline faces hold at any knob size), so
# the same band holds the PR short lane and the nightly's bigger knobs.
# Pure: reads the baselines table and one TSV record, so the self-test can
# reuse it directly.
check_one() { # $1 scenario name, $2 the tsv record, $3 the baselines table
    local name="$1" tsv="$2" base="$3" verdict
    verdict="$(awk -F '\t' -v scn="$name" '
        FNR == NR {
            if ($1 == scn && $1 != "" && $2 != "") {
                op[$2] = $3; band[$2] = $4; ref[$2] = $5
            }
            next
        }
        NF == 2 && $1 != "" {
            seen[$1] = $2 + 0
            if ($1 in op) {
                if (op[$1] == "min" && $2 + 0 < band[$1] + 0) {
                    bad = bad " " $1 "=" $2 " below the recorded min " band[$1]
                }
                if (op[$1] == "max" && $2 + 0 > band[$1] + 0) {
                    bad = bad " " $1 "=" $2 " above the recorded max " band[$1]
                }
                checked[$1] = 1
            }
        }
        END {
            for (k in op) {
                if (!(k in checked)) bad = bad " " k " (metric missing)"
            }
            if (bad != "") {
                print scn ":" bad; exit 1
            }
            exit 0
        }
    ' "$base" "$tsv")"
    local rc=$?
    [ "$rc" -eq 0 ] && return 0
    printf '%s\n' "$verdict"
    return 1
}

# One scenario, one record. Prints the verdict text and returns 0 only when
# the scenario exited cleanly inside its budget with at least one metric
# line. Pure: no manifest writes, so the self-test can reuse it directly.
run_one() { # $1 the scenario file
    local file="$1" name budget rc rec out metrics
    name="$(basename "$file")"
    name="${name%%.*}"
    budget="${CI_LOAD_DURATION:-30}"
    rec="$(record_path "$name")"
    mkdir -p "$CI_OUT/load"

    out="$(timeout "$budget" env \
        LOAD_DURATION="$budget" \
        LOAD_CLIENTS="${CI_LOAD_CLIENTS:-4}" \
        ${LOAD_TARGET:+LOAD_TARGET=$LOAD_TARGET} \
        "$file" 2>&1)"
    rc=$?

    # Metric lines only: two non-empty tab-separated fields. Everything else
    # the scenario printed is chatter and stays out of the record.
    printf '%s\n' "$out" | awk -F '\t' 'NF == 2 && $1 != "" && $2 != ""' >"$rec"
    metrics="$(wc -l <"$rec")"

    if [ "$rc" -eq 124 ]; then
        echo "$name exceeded the ${budget}s wall budget and was killed"
        return 1
    fi
    if [ "$rc" -ne 0 ]; then
        echo "$name exited rc=$rc: $(printf '%s' "$out" | tail -1 | cut -c1-100)"
        return 1
    fi
    if [ "$metrics" -eq 0 ]; then
        echo "$name produced no result: no metric<TAB>value line on stdout"
        return 1
    fi
    echo "$name produced $metrics metric(s) inside the ${budget}s budget: load/$name.tsv"
    return 0
}

run_scenarios() {
    ci_step "PR short load scenarios (budget ${CI_LOAD_DURATION:-30}s, clients ${CI_LOAD_CLIENTS:-4})"
    local sc_out sc_rc dr_out dr_rc
    sc_out="$(detect_scenarios)"
    sc_rc=$?
    dr_out="$(detect_driver)"
    dr_rc=$?
    if [ "$sc_rc" -eq "$MISSING" ] || [ "$dr_rc" -eq "$MISSING" ]; then
        ci_skip load-run "$(tsv_escape "$sc_out") / $(tsv_escape "$dr_out")"
        return 0
    fi
    if [ "$sc_rc" -ne 0 ] || [ "$dr_rc" -ne 0 ]; then
        ci_fail load-run "$(tsv_escape "$sc_out") / $(tsv_escape "$dr_out")"
        return 1
    fi

    mkdir -p "$CI_OUT/load"
    local dir total=0 ok=0 bad="" f verdict
    dir="$(cur_root)/tests/load/scenarios"
    for f in "$dir"/*; do
        [ -f "$f" ] && [ -x "$f" ] || continue
        total=$((total + 1))
        if verdict="$(run_one "$f")"; then
            ok=$((ok + 1))
        else
            bad="$bad $verdict"
        fi
    done

    if [ -z "$bad" ]; then
        ci_pass load-run "$ok/$total scenario(s) produced results: load/*.tsv"
        return 0
    fi
    ci_fail load-run "$((total - ok))/$total scenario(s) did not produce results:$(tsv_escape "$bad")"
    return 1
}

# Check the just-recorded scenario TSVs against the baselines table. A
# missing table is a SKIP (the thresholds have not been recorded yet), not
# a vacuous PASS. Every scenario's record gets checked; the verdict names
# the metric and the band it broke.
run_thresholds() {
    local base rec name verdict total=0 ok=0 bad=""
    base="$(cur_root)/tests/load/baselines.tsv"
    if [ ! -f "$base" ]; then
        ci_skip load-thresholds "no baselines table: no recorded thresholds to check"
        return 0
    fi
    local dir f
    dir="$(cur_root)/tests/load/scenarios"
    for f in "$dir"/*; do
        [ -f "$f" ] && [ -x "$f" ] || continue
        name="$(basename "$f")"
        name="${name%%.*}"
        rec="$(record_path "$name")"
        total=$((total + 1))
        if verdict="$(check_one "$name" "$rec" "$base")"; then
            ok=$((ok + 1))
        else
            bad="$bad $verdict"
        fi
    done
    if [ -z "$bad" ]; then
        ci_pass load-thresholds "$ok/$total scenario(s) inside the recorded baseline bands"
        return 0
    fi
    ci_fail load-thresholds "$((total - ok))/$total scenario(s) outside the recorded bands:$(tsv_escape "$bad")"
    return 1
}

run_all() {
    ci_step "PR short load test (budget ${CI_LOAD_DURATION:-30}s, clients ${CI_LOAD_CLIENTS:-4})"
    mkdir -p "$CI_OUT"
    local out rc
    out="$(detect_scenarios 2>&1)"
    rc=$?
    case "$rc" in
    0) ci_pass load-scenarios "$(tsv_escape "$out")" ;;
    "$MISSING") ci_skip load-scenarios "$(tsv_escape "$out")" ;;
    *) ci_fail load-scenarios "$(tsv_escape "$out")" ;;
    esac
    out="$(detect_driver 2>&1)"
    rc=$?
    case "$rc" in
    0) ci_pass load-driver "$(tsv_escape "$out")" ;;
    "$MISSING") ci_skip load-driver "$(tsv_escape "$out")" ;;
    *) ci_fail load-driver "$(tsv_escape "$out")" ;;
    esac
    run_scenarios
    run_thresholds
}

self_test() {
    local fails=0 r
    ci_step "self-test: the harness records and bounds a planted scenario"
    r="$(mktemp -d "$REPO_ROOT/ci-out/load-selftest.XXXXXX")"

    mkdir -p "$r/apps/bots" "$r/tests/load/scenarios"
    printf '[package]\nname = "bots"\n' >"$r/apps/bots/Cargo.toml"
    printf '%s\n' '#!/bin/sh' 'printf "ticks\t42\n" >&2' \
        'printf "clients_connected\t3\n"' 'exit 0' >"$r/tests/load/scenarios/good.sh"
    chmod +x "$r/tests/load/scenarios/good.sh"
    printf '%s\n' '#!/bin/sh' 'sleep 300' >"$r/tests/load/scenarios/slow.sh"
    chmod +x "$r/tests/load/scenarios/slow.sh"

    local out rc verdict
    out="$(CI_LOAD_ROOT="$r" detect_scenarios)"
    rc=$?
    if [ "$rc" -eq 0 ]; then
        ci_pass selftest-load "a planted scenario set is detected"
    else
        ci_fail selftest-load "the planted scenario set was not detected, so the case can never turn on"
        fails=$((fails + 1))
    fi
    out="$(CI_LOAD_ROOT="$r" detect_driver)"
    rc=$?
    if [ "$rc" -eq 0 ]; then
        ci_pass selftest-load "a planted driver crate is detected"
    else
        ci_fail selftest-load "the planted driver crate was not detected, so the case can never turn on"
        fails=$((fails + 1))
    fi

    # The good scenario: recorded metrics, verdict PASS. The stderr chatter
    # ("ticks") must stay out of the record — only stdout lines count.
    rm -rf "$CI_OUT/load"
    if verdict="$(CI_LOAD_ROOT="$r" CI_LOAD_DURATION=5 run_one "$r/tests/load/scenarios/good.sh")"; then
        if [ -f "$CI_OUT/load/good.tsv" ] \
            && grep -q '^clients_connected	3$' "$CI_OUT/load/good.tsv" \
            && ! grep -q '^ticks$' "$CI_OUT/load/good.tsv"; then
            ci_pass selftest-load "a planted scenario's stdout metrics are recorded and its stderr is not: $verdict"
        else
            ci_fail selftest-load "the planted scenario passed but its record is wrong (stderr leaked or metrics missing)"
            fails=$((fails + 1))
        fi
    else
        ci_fail selftest-load "a well-behaved planted scenario did not pass: $verdict"
        fails=$((fails + 1))
    fi

    # The runaway: killed at the budget, verdict FAIL, wall clock bounded.
    if verdict="$(CI_LOAD_ROOT="$r" CI_LOAD_DURATION=2 run_one "$r/tests/load/scenarios/slow.sh")"; then
        ci_fail selftest-load "a scenario sleeping past its budget still passed, so the lane cannot bound a runaway"
        fails=$((fails + 1))
    else
        case "$verdict" in
        *"exceeded the 2s wall budget"*)
            ci_pass selftest-load "a runaway scenario is killed at its budget and named: $verdict"
            ;;
        *)
            ci_fail selftest-load "the runaway failed for the wrong reason: $verdict"
            fails=$((fails + 1))
            ;;
        esac
    fi

    ci_step "self-test: an empty tree keeps every case a SKIP"
    r="$(mktemp -d "$REPO_ROOT/ci-out/load-selftest.XXXXXX")"
    local empty=1 d
    for d in detect_scenarios detect_driver; do
        if [ "$(CI_LOAD_ROOT="$r" "$d" >/dev/null 2>&1; echo $?)" != "$MISSING" ]; then
            empty=0
        fi
    done
    if [ "$empty" -eq 1 ]; then
        ci_pass selftest-load-empty "no substrate means every case reports SKIP, not a false PASS"
    else
        ci_fail selftest-load-empty "a bare tree produced a verdict other than SKIP, so a case would pass vacuously"
        fails=$((fails + 1))
    fi

    rm -rf "$REPO_ROOT/ci-out/load-selftest".*

    # The thresholds checker (BW-17-03-06): a record inside its recorded
    # band passes, a metric out of band names itself, and a rule whose
    # metric never arrived is named rather than silently dropped.
    ci_step "self-test: the thresholds checker bands a record against its baselines"
    local tbase tdir
    tdir="$(mktemp -d "$REPO_ROOT/ci-out/load-selftest.XXXXXX")"
    tbase="$tdir/baselines.tsv"
    {
        printf '# scenario\tmetric\top\tvalue\trecorded baseline\n'
        printf 'demo\tlogin_success_rate\tmin\t1.0\tsome-fixture.json\n'
        printf 'demo\tpeak_rss_kib\tmax\t2048\tsome-fixture.json\n'
    } >"$tbase"
    printf 'login_success_rate\t1\npeak_rss_kib\t1024\n' >"$tdir/good.tsv"
    printf 'login_success_rate\t0.5\npeak_rss_kib\t1024\n' >"$tdir/low.tsv"
    printf 'login_success_rate\t1\npeak_rss_kib\t99999\n' >"$tdir/high.tsv"
    printf 'login_success_rate\t1\n' >"$tdir/short.tsv"

    if check_one demo "$tdir/good.tsv" "$tbase"; then
        ci_pass selftest-load-thresholds "a record inside its recorded bands passes"
    else
        ci_fail selftest-load-thresholds "a well-behaved record failed its recorded bands"
        fails=$((fails + 1))
    fi
    for case in low high short; do
        if verdict="$(check_one demo "$tdir/$case.tsv" "$tbase")"; then
            ci_fail "selftest-load-thresholds-$case" "a violating record passed its bands ($case)"
            fails=$((fails + 1))
        elif [ "$case" = short ] && ! printf '%s' "$verdict" | grep -q "metric missing"; then
            ci_fail "selftest-load-thresholds-$case" "a missing metric was not named: $verdict"
            fails=$((fails + 1))
        else
            ci_pass "selftest-load-thresholds-$case" "the violation is named: $verdict"
        fi
    done
    rm -rf "$tdir"

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
