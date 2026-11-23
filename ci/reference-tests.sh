#!/usr/bin/env bash
# BW-00-02-02 — the reference test lane: rerun the original's own unit tests
# and save the participating list, the command and the result.
#
# Usage:
#   ci/reference-tests.sh            # rerun every registered original test
#   ci/reference-tests.sh self-test  # planted green/red/missing/timeout faces
#
# The participating list is not a directory glob: the original registers a
# test by writing `name := <n>` and `type := unit_test` in a unit_test
# Makefile.rules, and its own full-suite rule (bw-run-all-unit-tests) walks
# exactly those, running each as `cd <unit_test dir> && timeout 1800
# <binary> --root <root>`. This lane derives the same list from the source
# tree, runs the same documented command with the same 1800s budget, and
# records per module: component directory, case count, the failure summary
# line, exit code and seconds. A module that exits nonzero names itself; a
# module derived but not shipped, or one that exceeds the budget, is a red
# result — the lane cannot certify what it did not observe. The make entry
# itself rebuilds the whole tree, which is a build job, not a recording
# job, so the lane runs the binaries it finds already built.
#
# When the reference tree is absent the lane SKIPs naming the ticket — a
# hosted runner has no original tree. When the tree exists but has no honest
# git provenance the lane FAILs: results that cannot name their commit are
# not a baseline.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
# Call-time resolution: the self-test plants a reference tree and a private
# output root, and an override applied to a shell function only reaches code
# that re-reads the variable.
cur_ref() { printf '%s' "${CI_REFTEST_ROOT:-${BW_REFERENCE:-$HOME/workspaces/BigWorld}}"; }
cur_out() { printf '%s' "${CI_REFTEST_OUT:-$CI_OUT}"; }
cur_timeout() { printf '%s' "${CI_REFTEST_TIMEOUT:-1800}"; }

tsv_file() { printf '%s/reference-tests/reference-unit-tests.tsv' "$(cur_out)"; }
committed_record() { printf '%s/tests/baseline/reference-unit-tests.tsv' "$REPO_ROOT"; }

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

derive_list() { # "<name>\t<component unit_test dir, repo-relative>" per line
    local ref f n d
    ref="$(cur_ref)"
    while IFS= read -r f; do
        grep -qE '^[[:space:]]*type[[:space:]]*[:?]=[[:space:]]*unit_test([[:space:]]|$)' "$f" || continue
        n="$(sed -n 's/^name[[:space:]]*[:?]=[[:space:]]*//p' "$f" | head -1 | tr -d '[:space:]')"
        [ -n "$n" ] || continue
        d="$(dirname "$f")"
        printf '%s\t%s\n' "$n" "${d#"$ref"/}"
    done < <(find "$ref/programming" -name Makefile.rules 2>/dev/null | sort)
}

run_one() { # $1 name $2 component unit_test dir (repo-relative)
    # run_one — the documented per-module command, byte-for-byte the shape of
    # the original's bw-run-test-<name> rule, under its own 1800s budget.
    local n="$1" comp="$2" b log rc s e cases res
    b="$(cur_ref)/game/bin/server/el7/unit_tests/${n}_test"
    s="$(date +%s)"
    log="$(cur_out)/reference-tests/${n}_test.log"
    mkdir -p "$(dirname "$log")"
    (
        cd "$(cur_ref)/$comp" || exit 9
        timeout "$(cur_timeout)" "$b" --root "$(cur_ref)"
    ) </dev/null >"$log" 2>&1
    rc=$?
    e="$(date +%s)"
    cases="$(grep -oE '[0-9]+ tests' "$log" | head -1)"
    res="$(grep -aiE 'fail|error' "$log" | tail -1 | tr '\t\n' '  ' | cut -c1-120)"
    printf '%s_test\t%s\t%s\t%s\t%s\t%s\n' "$n" "$comp" "${cases:-none}" \
        "${res:-<no failure line>}" "$rc" "$((e - s))"
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    ci_step "reference unit tests"
    if [ ! -d "$(cur_ref)" ]; then
        ci_skip reference-tests \
            "no reference tree at $(cur_ref) — BW-00-02-02 reruns it when present (set BW_REFERENCE)"
        return 0
    fi

    local ref sha
    ref="$(cur_ref)"
    # Same provenance guard as the baseline lane: git walks up when the tree
    # has no .git, and a record that names another repo's commit is a lie.
    if ! sha="$(git -C "$ref" rev-parse HEAD 2>/dev/null)" \
        || [ "$(git -C "$ref" rev-parse --show-toplevel 2>/dev/null)" != "$ref" ]; then
        ci_fail reference-commit "the reference tree has no honest provenance (no git, or not its own toplevel) — rerun results must name the commit they reran"
        return 1
    fi

    local list
    list="$(derive_list)"
    if [ -z "$list" ]; then
        ci_fail reference-list "no unit_test components derived from $ref/programming — the participating list is empty"
        return 1
    fi

    mkdir -p "$(dirname "$(tsv_file)")" "$(cur_out)/reference-tests"
    {
        printf '# reference unit tests\n'
        printf '# tree: %s\n' "$ref"
        printf '# commit: %s\n' "$sha"
        printf '# participating list: derived from programming/**/unit_test/Makefile.rules (type := unit_test)\n'
        printf '# command: cd <component unit_test dir> && timeout %s <unit_tests binary> --root <tree>\n' "$(cur_timeout)"
        printf '# budget per module: %ss (the original bw-run-all-unit-tests budget)\n' "$(cur_timeout)"
    } >"$(tsv_file)"

    local total=0 red=0 bad="" row n comp rc_field cases_num cases_sum=0 extra
    while IFS=$'\t' read -r n comp; do
        total=$((total + 1))
        if [ ! -x "$ref/game/bin/server/el7/unit_tests/${n}_test" ]; then
            printf '%s_test\t%s\t<missing>\tderived but not shipped in el7 unit_tests\t1\t0\n' \
                "$n" "$comp" >>"$(tsv_file)"
            red=$((red + 1))
            bad="$bad ${n}_test(missing)"
            ci_fail "reference-tests-$n" "derived but no shipped binary — the list and the payload disagree"
            continue
        fi
        row="$(run_one "$n" "$comp")"
        printf '%s\n' "$row" >>"$(tsv_file)"
        rc_field="$(printf '%s' "$row" | cut -f5)"
        if [ "$rc_field" = "0" ]; then
            cases_num="$(printf '%s' "$row" | cut -f3 | sed 's/ tests//')"
            case "$cases_num" in *[!0-9]*) cases_num=0 ;; esac
            cases_sum=$((cases_sum + cases_num))
        else
            red=$((red + 1))
            if [ "$rc_field" = "124" ]; then
                bad="$bad ${n}_test(timeout@$(printf '%s' "$row" | cut -f6)s)"
            else
                bad="$bad ${n}_test(rc=$rc_field)"
            fi
            ci_fail "reference-tests-$n" "$(printf '%s' "$row" | cut -f2-6 | tr '\t' ' ')"
        fi
    done <<<"$list"

    # The other direction: binaries shipped without a registration are not in
    # the participating list — recorded, not failed, so the note is visible.
    extra="$(comm -13 \
        <(printf '%s\n' "$list" | cut -f1 | sed 's/$/_test/' | sort) \
        <(find "$ref/game/bin/server/el7/unit_tests" -maxdepth 1 -type f -name '*_test' -printf '%f\n' 2>/dev/null | sort))"
    [ -n "$extra" ] && ci_note "shipped but not registered in any unit_test Makefile.rules: $(printf '%s' "$extra" | tr '\n' ' ')"

    if [ "$red" -gt 0 ]; then
        ci_fail reference-tests "$red of $total registered module(s) did not come back green:$bad (reference-unit-tests.tsv)"
        return 1
    fi

    local drift
    drift="$(record_drift)"
    [ -n "$drift" ] && ci_note "$drift"
    ci_pass reference-tests \
        "$total registered module(s) rerun green, $cases_sum case(s) counted (reference-unit-tests.tsv)"
    ci_note "participating list, command and per-module result are saved in the tsv"
}

record_drift() { # case-count drift against the committed durable record, if any
    local committed
    committed="$(committed_record)"
    [ -f "$committed" ] || return 0
    local diff_lines
    diff_lines="$(comm -3 \
        <(awk -F'\t' '$1 !~ /^#/ && $1 != "name" {print $1 "\t" $3}' "$committed" | sort) \
        <(awk -F'\t' '$1 !~ /^#/ {print $1 "\t" $3}' "$(tsv_file)" | sort) | grep -c . || true)"
    [ "$diff_lines" -gt 0 ] \
        && printf 'the committed record differs on %s row(s) — the live tree moved or cases changed' "$diff_lines"
}

self_test() {
    local fails=0 r out rc T
    ci_step "self-test: a planted registered suite round-trips list, command and result"
    r="$(mktemp -d "$REPO_ROOT/ci-out/reftest-selftest.XXXXXX")"
    T="$r/BigWorld"
    git init -q -b main "$T"
    git -C "$T" -c user.name=t -c user.email=t@t commit -q --allow-empty -m "original anchor"
    mkdir -p "$T/game/bin/server/el7/unit_tests" \
        "$T/programming/bigworld/lib/alpha/unit_test" \
        "$T/programming/bigworld/server/beta/unit_test"
    printf 'name := alpha\ntype := unit_test\n' >"$T/programming/bigworld/lib/alpha/unit_test/Makefile.rules"
    printf 'name := beta\ntype := unit_test\n' >"$T/programming/bigworld/server/beta/unit_test/Makefile.rules"
    printf '#!/bin/sh\necho "4 tests"; echo "There were no test failures."\n' >"$T/game/bin/server/el7/unit_tests/alpha_test"
    printf '#!/bin/sh\necho "2 tests"; echo "There were no test failures."\n' >"$T/game/bin/server/el7/unit_tests/beta_test"
    chmod +x "$T"/game/bin/server/el7/unit_tests/*_test

    out="$(CI_REFTEST_ROOT="$T" CI_REFTEST_OUT="$r/out" CI_OUT="$r/manifest" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -eq 0 ] \
        && printf '%s\n' "$out" | grep -q 'reference-tests	PASS	2 registered module(s) rerun green, 6 case(s) counted' \
        && grep -q 'lib/alpha/unit_test' "$r/out/reference-tests/reference-unit-tests.tsv" \
        && grep -q "# commit: $(git -C "$T" rev-parse HEAD)" "$r/out/reference-tests/reference-unit-tests.tsv"; then
        ci_pass selftest-reftest "planted suite records 2 modules, 6 cases and the honest commit"
    else
        ci_fail selftest-reftest "planted suite did not round-trip: rc=$rc $(printf '%s\n' "$out" | grep 'reference-' | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a red module and an unshipped module are both named"
    mkdir -p "$T/programming/bigworld/server/gamma/unit_test" \
        "$T/programming/bigworld/server/epsilon/unit_test"
    printf 'name := gamma\ntype := unit_test\n' >"$T/programming/bigworld/server/gamma/unit_test/Makefile.rules"
    printf 'name := epsilon\ntype := unit_test\n' >"$T/programming/bigworld/server/epsilon/unit_test/Makefile.rules"
    printf '#!/bin/sh\necho "6 tests"; echo "2 tests FAILED"; exit 2\n' >"$T/game/bin/server/el7/unit_tests/gamma_test"
    chmod +x "$T/game/bin/server/el7/unit_tests/gamma_test"

    out="$(CI_REFTEST_ROOT="$T" CI_REFTEST_OUT="$r/out" CI_OUT="$r/manifest" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ] \
        && printf '%s\n' "$out" | grep -q 'gamma_test' \
        && printf '%s\n' "$out" | grep -q 'epsilon' \
        && printf '%s\n' "$out" | grep -q 'reference-tests	FAIL	2 of 4 registered module(s) did not come back green'; then
        ci_pass selftest-reftest-red "a red module and a missing module each fail the lane by name"
    else
        ci_fail selftest-reftest-red "red/missing faces were not reported: rc=$rc $(printf '%s\n' "$out" | grep 'reference-tests	FAIL	[0-9]' | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: no reference tree reports SKIP naming the ticket"
    out="$(CI_REFTEST_ROOT="$r/none" CI_REFTEST_OUT="$r/out2" CI_OUT="$r/manifest" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -eq 0 ] && printf '%s\n' "$out" | grep -q 'reference-tests	SKIP	.*BW-00-02-02'; then
        ci_pass selftest-reftest-absent "absent tree reports SKIP naming BW-00-02-02"
    else
        ci_fail selftest-reftest-absent "absent tree did not skip cleanly: rc=$rc"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a module over budget is a red result, not a pass"
    mkdir -p "$T/programming/bigworld/server/delta/unit_test"
    printf 'name := delta\ntype := unit_test\n' >"$T/programming/bigworld/server/delta/unit_test/Makefile.rules"
    printf '#!/bin/sh\nsleep 30\n' >"$T/game/bin/server/el7/unit_tests/delta_test"
    chmod +x "$T/game/bin/server/el7/unit_tests/delta_test"
    out="$(CI_REFTEST_ROOT="$T" CI_REFTEST_OUT="$r/out" CI_OUT="$r/manifest" CI_REFTEST_TIMEOUT=2 "$0" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ] && printf '%s\n' "$out" | grep -q 'delta_test(timeout@'; then
        ci_pass selftest-reftest-timeout "a module exceeding the budget fails named with its budget"
    else
        ci_fail selftest-reftest-timeout "a module over budget was not reported: rc=$rc $(printf '%s\n' "$out" | grep 'reference-tests	FAIL	[0-9]' | head -1)"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$REPO_ROOT/ci-out/reftest-selftest".*

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
