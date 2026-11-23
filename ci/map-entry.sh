#!/usr/bin/env bash
# BW-00-05-04 — index the common code and the FantasyDemo entries.
#
# Usage:
#   ci/map-entry.sh            # record the common and demo entry indexes
#   ci/map-entry.sh self-test  # planted common + demo trees prove each index
#
# The coverage ledger enumerates build targets; the common code and the
# FantasyDemo scripts and resources are not targets, so this lane records their
# indexes — each entry with the ticket whose source scope names it. The
# example build targets are mapped by the same category's coverage-map step
# (CI_MAP_TASKS=BW-00-05-04); this lane covers the non-target entries.
#
# One record face:
#   * entry-record — coverage-entry.json + coverage-entry.tsv, one row per
#     common-code file and per demo script/resource entry, each with an index.
#     An absent reference source root skips naming the ticket; a hollow map
#     (no entries, or a row without an index) is not a record.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
cur_ref() { printf '%s' "${CI_ENTRY_REF:-${BW_REFERENCE:-$HOME/workspaces/BigWorld}}"; }
cur_todo() { printf '%s' "${CI_ENTRY_TODO:-$REPO_ROOT/todo.md}"; }
cur_out() { printf '%s' "${CI_ENTRY_OUT:-$CI_OUT}"; }
entry_dir() { printf '%s/coverage-entry' "$(cur_out)"; }

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    ci_step "index the common code and the FantasyDemo entries"

    local ref src
    ref="$(cur_ref)"
    src="$ref/programming/bigworld"
    if [ ! -d "$src" ]; then
        ci_skip entry-record \
            "no reference source at $src — the entry map indexes the common code and the FantasyDemo entries (BW-00-05-04)"
        return 0
    fi

    local out_dir
    out_dir="$(entry_dir)"
    local sum
    if ! sum="$(python3 "$REPO_ROOT/ci/map-entry.py" "$src" "$(cur_todo)" \
        "$out_dir" --self-check 2>&1)"; then
        ci_fail entry-record "the entry map failed to record: $(printf '%s' "$sum" | tail -1)"
        return 1
    fi
    local entries code script resource
    entries="$(printf '%s' "$sum" | sed -n 's/.*entries=\([0-9]*\).*/\1/p')"
    code="$(printf '%s' "$sum" | sed -n 's/.*code=\([0-9]*\).*/\1/p')"
    script="$(printf '%s' "$sum" | sed -n 's/.*script=\([0-9]*\).*/\1/p')"
    resource="$(printf '%s' "$sum" | sed -n 's/.*resource=\([0-9]*\).*/\1/p')"
    if [ -z "$entries" ] || [ "$entries" -eq 0 ]; then
        ci_fail entry-record "the entry map recorded no row count — a hollow map is not a record"
        return 1
    fi
    ci_pass entry-record "$entries entry/ies indexed ($code code, $script script, $resource resource)"

    # The committed snapshot is the anchor; a map that grows is a note.
    local f
    for f in json tsv; do
        if [ -f "$REPO_ROOT/tests/baseline/coverage-entry.$f" ] \
            && [ -f "$out_dir/coverage-entry.$f" ] \
            && ! cmp -s "$REPO_ROOT/tests/baseline/coverage-entry.$f" "$out_dir/coverage-entry.$f"; then
            ci_note "the committed tests/baseline/coverage-entry.$f differs from this run — snapshot-time anchor, not a failure"
        fi
    done

    ci_note "every common file and demo entry carries a traceable ticket"
}

# A planted reference with a common dir and a demo tree, plus a planted todo
# naming the scopes a file entry must resolve to.
plant_tree() { # $1 reference root
    local ref="$1"
    mkdir -p "$ref/programming/bigworld/common" \
        "$ref/programming/fantasydemo/res/scripts/cell" \
        "$ref/programming/fantasydemo/res/server"
    printf 'int main(){}\n' >"$ref/programming/bigworld/common/py_network.cpp"
    printf 'int main(){}\n' >"$ref/programming/bigworld/common/py_timer.cpp"
    printf 'x\n' >"$ref/programming/fantasydemo/res/scripts/entities.xml"
    printf 'bw\n' >"$ref/programming/fantasydemo/res/server/bw.xml"
}

plant_todo() { # $1 todo path
    cat >"$1" <<'EOF'
### BW-02 I/O 与事件分发
源码 `lib/network/`、`lib/connection/`、`common/py_network.cpp`。

### BW-03 Python 宿主
源码 `lib/script/`、`lib/pyscript/`、`common/`。

### BW-14 Demo
源码 `../fantasydemo/`、`examples/`。
EOF
}

self_test() {
    local fails=0 rc r T M r2 T2 M2 r3 T3 M3
    ci_step "self-test: planted common and demo entries each resolve to a ticket"
    r="$(mktemp -d "$REPO_ROOT/ci-out/entry-selftest.XXXXXX")"
    T="$r/BigWorld"
    M="$r/manifest/manifest.tsv"
    plant_tree "$T"
    plant_todo "$T/todo.md"

    CI_ENTRY_REF="$T" CI_ENTRY_TODO="$T/todo.md" CI_ENTRY_OUT="$r/out" \
        CI_OUT="$r/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q '^common/py_network.cpp	code	common/py_network.cpp	BW-02,BW-03	' "$r/out/coverage-entry/coverage-entry.tsv" \
        && grep -q '^common/py_timer.cpp	code	common/py_timer.cpp	BW-03	' "$r/out/coverage-entry/coverage-entry.tsv" \
        && grep -q '^res/scripts/cell	script	fantasydemo/res/scripts/cell	BW-14	' "$r/out/coverage-entry/coverage-entry.tsv" \
        && grep -q '^res/server/bw.xml	resource	fantasydemo/res/server/bw.xml	BW-14	' "$r/out/coverage-entry/coverage-entry.tsv" \
        && grep -q '^res/scripts/entities.xml	script	.*	BW-14	' "$r/out/coverage-entry/coverage-entry.tsv"; then
        ci_pass selftest-entry-indexes "common files resolve to BW-03 (plus file-specific scopes) and demo entries to BW-14"
    else
        ci_fail selftest-entry-indexes "planted entries were not indexed as expected: rc=$rc $(grep 'entry-record' "$M" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: an absent reference source root skips naming the ticket"
    r2="$(mktemp -d "$REPO_ROOT/ci-out/entry-selftest.XXXXXX")"
    T2="$r2/none"
    M2="$r2/manifest/manifest.tsv"
    CI_ENTRY_REF="$T2" CI_ENTRY_TODO="$REPO_ROOT/todo.md" CI_ENTRY_OUT="$r2/out" \
        CI_OUT="$r2/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'entry-record	SKIP	.*BW-00-05-04' "$M2"; then
        ci_pass selftest-entry-absent "an absent reference root reports SKIP naming BW-00-05-04"
    else
        ci_fail selftest-entry-absent "an absent reference root did not skip cleanly: rc=$rc"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a source root with neither common code nor a demo tree is hollow"
    r3="$(mktemp -d "$REPO_ROOT/ci-out/entry-selftest.XXXXXX")"
    T3="$r3/empty"
    M3="$r3/manifest/manifest.tsv"
    mkdir -p "$T3/programming/bigworld"
    plant_todo "$T3/todo.md"
    CI_ENTRY_REF="$T3" CI_ENTRY_TODO="$T3/todo.md" CI_ENTRY_OUT="$r3/out" \
        CI_OUT="$r3/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -ne 0 ] \
        && grep -q 'entry-record	FAIL	.*hollow map' "$M3"; then
        ci_pass selftest-entry-hollow "a root with no entries fails as hollow rather than recording"
    else
        ci_fail selftest-entry-hollow "a hollow source root did not fail cleanly: rc=$rc $(grep 'entry-record' "$M3" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$r2" "$r3" "$REPO_ROOT/ci-out/entry-selftest".*

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
