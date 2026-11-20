#!/usr/bin/env bash
# BW-22-03-04 — the comparison report: one record that makes runs comparable.
#
# Usage:
#   ci/report.sh            # write ci-out/report/report.tsv
#   ci/report.sh self-test  # prove the four sections and the baseline diff
#
# The acceptance is "version, hardware, scenario and database information
# complete". The report is a TSV of section/key/value rows so a later
# comparison can join on any of them. The scenario section reads whatever
# run archives and load records exist; the database section names each
# backend client and its version, or records it absent. A baseline run
# archive (CI_REPORT_BASELINE) adds a compare section through the shared
# diff entry (BW-18-04-02); without one the section says so.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
# Call-time resolution: the self-test plants archives, a baseline and a diff
# driver, and an override applied to a shell function only reaches code that
# re-reads the variable.
cur_root() { printf '%s' "${CI_REPORT_ROOT:-$REPO_ROOT}"; }
cur_out() { printf '%s' "${CI_REPORT_OUT:-$CI_OUT}"; }
cur_diff() { printf '%s' "${CI_REPORT_DIFF:-cargo run -q -p test-support --bin run-tests -- diff}"; }

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

# --- the four sections --------------------------------------------------------

section_version() {
    local rust
    rust="$(rustc -vV 2>/dev/null | sed -n 's/^release: //p' | head -1)"
    printf 'version\tcommit\t%s\n' "$(git -C "$(cur_root)" rev-parse --short HEAD 2>/dev/null || echo unknown)"
    printf 'version\ttoolchain\t%s\n' "${rust:-absent}"
    printf 'version\tgenerated\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)"
}

section_hardware() {
    local cpu
    cpu="$(sed -n 's/^model name[[:space:]]*: //p' /proc/cpuinfo 2>/dev/null | head -1)"
    printf 'hardware\tarch\t%s\n' "$(uname -m 2>/dev/null || echo unknown)"
    printf 'hardware\tcpu\t%s\n' "${cpu:-unknown}"
    printf 'hardware\tcores\t%s\n' "$(nproc 2>/dev/null || echo unknown)"
    printf 'hardware\tmem_kb\t%s\n' "$(awk '/^MemTotal:/ {print $2; exit}' /proc/meminfo 2>/dev/null || echo unknown)"
}

# Every run archive the tree holds, plus every load record: label and its
# one-line summary. Nothing here is invented — a section with no runs says so.
section_scenario() {
    local found=0 f label summary seen=""
    for f in "$(cur_root)"/target/test-results/*/run.txt "$(cur_out)"/fault/run.txt; do
        [ -f "$f" ] || continue
        label="$(sed -n 's/^label: //p' "$f" | head -1)"
        # The fault lane copies the drill's archive into ci-out, so the same
        # label can appear twice; record each run once.
        case "$seen" in
        *"|${label}|"*) continue ;;
        esac
        seen="$seen|${label}|"
        found=$((found + 1))
        summary="$(sed -n 's/^summary: //p' "$f" | head -1)"
        printf 'scenario\t%s\t%s\n' "${label:-unknown}" "${summary:-no summary line}"
    done
    for f in "$(cur_out)"/load/*.tsv; do
        [ -f "$f" ] || continue
        found=$((found + 1))
        printf 'scenario\tload-%s\t%s metric row(s)\n' "$(basename "$f" .tsv)" "$(wc -l <"$f" | tr -d ' ')"
    done
    if [ "$found" -eq 0 ]; then
        printf 'scenario\truns\tnone recorded yet\n'
    fi
}

section_db() {
    local c
    for c in mysql psql sqlite3; do
        if command -v "$c" >/dev/null 2>&1; then
            printf 'db\t%s\t%s\n' "$c" "$("$c" --version 2>/dev/null | head -1 | cut -c1-80)"
        else
            printf 'db\t%s\tabsent on this runner\n' "$c"
        fi
    done
}

# The comparison: the shared diff entry (BW-18-04-02) against a baseline run
# archive. REGRESSED lines are the point of the whole report.
section_compare() {
    local baseline="${CI_REPORT_BASELINE:-}"
    if [ -z "$baseline" ]; then
        printf 'compare\tbaseline\tnot given (set CI_REPORT_BASELINE to a run.txt)\n'
        return 0
    fi
    if [ ! -f "$baseline" ]; then
        printf 'compare\tbaseline\tmissing file %s\n' "$baseline"
        return 0
    fi
    local out current="" f
    for f in "$(cur_root)"/target/test-results/*/run.txt "$(cur_out)"/fault/run.txt; do
        [ -f "$f" ] || continue
        current="$f"
        break
    done
    if [ -z "$current" ]; then
        printf 'compare\tcurrent\tno run archive found to compare\n'
        return 0
    fi
    out="$($(cur_diff) "$baseline" "$current" 2>&1 || true)"
    if printf '%s' "$out" | grep -qE 'REGRESSED|healed|MISSING|ADDED'; then
        printf 'compare\tbaseline\t%s\n' "$(printf '%s' "$out" | grep -E 'REGRESSED|healed|MISSING|ADDED' | head -4 | tr '\n' '; ')"
    else
        printf 'compare\tbaseline\tno differences reported\n'
    fi
}

run_all() {
    ci_step "comparison report"
    mkdir -p "$(cur_out)/report"
    local report
    report="$(cur_out)/report/report.tsv"
    {
        section_version
        section_hardware
        section_scenario
        section_db
        section_compare
    } >"$report"

    # The acceptance is completeness: the lane reads its own report back and
    # fails rather than shipping a report missing a section.
    local missing=""
    grep -q '^version	' "$report" || missing="$missing version"
    grep -q '^hardware	' "$report" || missing="$missing hardware"
    grep -q '^scenario	' "$report" || missing="$missing scenario"
    grep -q '^db	' "$report" || missing="$missing db"
    if [ -z "$missing" ]; then
        ci_pass report-summary "$(wc -l <"$report" | tr -d ' ') row(s): report/report.tsv"
    else
        ci_fail report-summary "the report is missing section(s):$missing"
    fi
}

self_test() {
    local fails=0 r report stub_diff
    ci_step "self-test: the four sections are complete and truthful"
    r="$(mktemp -d "$REPO_ROOT/ci-out/report-selftest.XXXXXX")"
    mkdir -p "$r/target/test-results/test-support-multiproc"
    cat >"$r/target/test-results/test-support-multiproc/run.txt" <<'TXT'
# run archive
label: test-support-multiproc
run: cargo test -p test-support multiproc
commit: deadbee
when: 2026-11-20T00:00:00Z
PASS multiproc::tests::a_crashed_role_restarts_with_a_fresh_pid
summary: 1 passed, 0 failed, 0 skipped
TXT
    report="$r/report.tsv"
    {
        CI_REPORT_ROOT="$r" section_version
        CI_REPORT_ROOT="$r" section_hardware
        CI_REPORT_ROOT="$r" section_scenario
        CI_REPORT_ROOT="$r" section_db
    } >"$report"

    local ok=1
    for section in version hardware scenario db; do
        grep -q "^$section	" "$report" || ok=0
    done
    grep -q '^scenario	test-support-multiproc	1 passed' "$report" || ok=0
    grep -q '^version	commit	[0-9a-f]' "$report" || ok=0
    grep -q '^hardware	cores	[0-9]' "$report" || ok=0
    grep -q '^db	mysql	' "$report" || ok=0
    if [ "$ok" -eq 1 ]; then
        ci_pass selftest-report "the planted run archive lands in a complete report ($(wc -l <"$report" | tr -d ' ') rows)"
    else
        ci_fail selftest-report "the report is incomplete or does not reflect the planted archive"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a baseline produces a compare section through the diff entry"
    local baseline="$r/baseline.txt"
    cp "$r/target/test-results/test-support-multiproc/run.txt" "$baseline"
    stub_diff="$(mktemp "$REPO_ROOT/ci-out/report-diff.XXXXXX")"
    cat >"$stub_diff" <<'STUB'
#!/bin/sh
echo "REGRESSED multiproc::tests::a_migration_scenario_replays_identically"
exit 0
STUB
    chmod +x "$stub_diff"
    local compare_out
    compare_out="$(CI_REPORT_ROOT="$r" CI_REPORT_BASELINE="$baseline" CI_REPORT_DIFF="$stub_diff" \
        CI_REPORT_OUT="$r" section_compare)"
    if printf '%s\n' "$compare_out" | grep -q '^compare	baseline	REGRESSED multiproc::tests::a_migration_scenario_replays_identically'; then
        ci_pass selftest-report-compare "a baseline flows through the diff entry into the compare section"
    else
        ci_fail selftest-report-compare "the compare section did not report the planted regression: $(printf '%s' "$compare_out" | head -1)"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$stub_diff" "$REPO_ROOT/ci-out/report-selftest".*

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
