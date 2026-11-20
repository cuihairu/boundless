#!/usr/bin/env bash
# BW-22-01-03 — Python check lanes: every module must compile, and the suite
# under tests/python must pass.
#
# Usage:
#   ci/check-python.sh                 # whole tree
#   ci/check-python.sh self-test       # prove the lanes bite
#
# The repo carries no Python yet: BW-18 puts the suite in tests/python/, the
# scripting API lands under scripts/. Until one of those trees exists the
# lanes SKIP and say which path will turn them on.
#
# A red lane names the module: compileall and pytest both report file and
# line, and the manifest keeps the first offenders instead of the dump.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"

PY=python3

# Directories that may hold Python. Only ones that actually contain a tracked
# .py file count as substrate.
SCAN_ROOTS=(tests/python scripts python tools)

# Discovery goes through git, not find: it is the same file set locally and in
# the workflow checkout, and it never wanders into target/ or a venv. (The
# box this runs on aliases `find` to bfs, which rejects -prune — another
# reason to keep the traversal out of the shell.)
python_files() {
    local f root
    {
        git -C "$REPO_ROOT" ls-files --cached '*.py' 2>/dev/null
        # Uncommitted work must gate too — a half-written module is exactly
        # what this lane exists to catch.
        git -C "$REPO_ROOT" ls-files --others --exclude-standard '*.py' 2>/dev/null
    } | while IFS= read -r f; do
        for root in "${SCAN_ROOTS[@]}"; do
            case "$f" in
            "$root"/*) printf '%s\n' "$REPO_ROOT/$f"; break ;;
            esac
        done
    done
}

first_offenders() {
    local report="$1"
    {
        # compileall and traceback frames both report `File "path", line N`.
        grep -oE 'File "[^"]+", line [0-9]+' "$report" 2>/dev/null \
            | sed -E 's/^File "([^"]+)", line ([0-9]+)$/\1:\2/'
        # pytest's short summary: `FAILED tests/python/x.py::name`, and for a
        # collection error the `::` part is absent.
        grep -oE '(FAILED|ERROR) [^ ]*\.py[^ ]*(::[^ ]+)?' "$report" 2>/dev/null \
            | sed -E 's/^(FAILED|ERROR) //'
    } | sed "s|^$REPO_ROOT/||; s|^\./||" | sort -u | head -12 | paste -sd, -
}

run_lane() {
    local name="$1" lane report rc where
    shift
    lane="python-$name"
    report="$CI_LOG_DIR/$lane.log"
    ci_step "$name"
    ci_pipe "$lane" "$@"
    rc="${CI_RC:-1}"
    if [ "$rc" -eq 0 ]; then
        ci_pass "$lane" "ok"
    elif [ "$rc" -eq 127 ] || [ "$rc" -eq 128 ]; then
        ci_fail "$lane" "interpreter or command missing (log logs/$lane.log)"
    elif [ "$rc" -eq 4 ]; then
        ci_fail "$lane" "usage error in the pytest invocation (log logs/$lane.log)"
    elif [ "$rc" -eq 5 ]; then
        # pytest 5 = nothing collected. It reports no module for a module that
        # cannot even be imported, so the compile lane already named the file.
        ci_fail "$lane" "no tests collected — nothing importable under tests/python; see the python-compile lane for the module"
    else
        where="$(first_offenders "$report" | sed 's/, */, /g')"
        if [ -z "$where" ]; then
            where="not localized — see logs/$lane.log"
        fi
        ci_fail "$lane" "$name error or failing test at: $where"
    fi
}

# Roots that actually exist, so neither compileall nor pytest is handed a
# directory that is not there yet.
present_roots() {
    local root
    for root in "${SCAN_ROOTS[@]}"; do
        [ -d "$root" ] && printf '%s ' "$root"
    done
}

main() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    cd "$REPO_ROOT"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    if [ "$MODE" = self-test ]; then
        self_test
        return $?
    fi

    local files count roots where
    files="$(python_files)"
    roots="$(present_roots)"
    count="$(printf '%s' "$files" | grep -c '\.py$' || true)"

    if [ -z "$files" ]; then
        ci_skip python-compile "no Python under ${SCAN_ROOTS[*]} — lane turns on with tests/python (BW-18)"
        ci_skip python-test "no Python under ${SCAN_ROOTS[*]} — lane turns on with tests/python (BW-18)"
        return 0
    fi

    if [ -z "$roots" ]; then
        where="(none)"
    else
        where="$roots"
    fi
    ci_note "python3 $("$PY" --version 2>&1 | awk '{print $2}'), $count module(s) in: $where"

    # Syntax gate: refuses to even collect a module that will not compile,
    # so the failure names the file instead of turning into a collection error.
    # Unquoted on purpose: $roots is the space-separated list of roots.
    # shellcheck disable=SC2086
    run_lane compile "$PY" -m compileall -q $roots

    if ! "$PY" -c 'import pytest' 2>/dev/null; then
        ci_fail python-test "pytest missing, so $count module(s) cannot be collected"
        return 1
    fi
    run_lane test "$PY" -m pytest -q tests/python --rootdir "$REPO_ROOT"
}

# Same discipline as the Rust self-test: plant a defect per lane in a
# throwaway tree and demand that the lane go red on exactly that file.
self_test() {
    local sandbox root
    sandbox="$(mktemp -d)"
    root="$sandbox/tests/python"
    mkdir -p "$root"

    cat >"$root/broken_module.py" <<'EOF'
def half_written( :
    return
EOF
    cat >"$root/good_module.py" <<'EOF'
def doubled(x):
    return x * 2
EOF
    cat >"$root/test_good.py" <<'EOF'
from good_module import doubled


def test_doubled():
    assert doubled(3) == 6
EOF

    local fails=0
    local rc where

    ci_step "self-test: syntax defect is found and located"
    if "$PY" -m compileall -q "$root" >"$sandbox/compile.log" 2>&1; then
        ci_fail selftest-compile "compileall accepted a module that does not compile"
        fails=$((fails + 1))
    else
        where="$(first_offenders "$sandbox/compile.log")"
        case "$where" in
        *broken_module.py:*) ci_pass selftest-compile "located $where" ;;
        *) ci_fail selftest-compile "went red but reported $where" ; fails=$((fails + 1)) ;;
        esac
    fi

    ci_step "self-test: failing test is found and located"
    cat >"$root/test_bad.py" <<'EOF'
from good_module import doubled


def test_doubled_is_watched():
    assert doubled(3) == 7
EOF
    if ! "$PY" -m pytest -q "$root" >/dev/null 2>&1; then
        ci_pass selftest-test "detects a failing test"
    else
        ci_fail selftest-test "pytest accepted a suite with a failing test"
        fails=$((fails + 1))
    fi

    rm -f "$root/broken_module.py" "$root/test_bad.py"
    ci_step "self-test: clean tree passes"
    rm -rf "$root/__pycache__" 2>/dev/null
    if "$PY" -m compileall -q "$root" >/dev/null 2>&1 \
        && "$PY" -m pytest -q "$root" >/dev/null 2>&1; then
        ci_pass selftest-clean "clean tree passes both lanes"
    else
        ci_fail selftest-clean "a clean tree still goes red"
        fails=$((fails + 1))
    fi

    rm -rf "$sandbox"
    if [ "$fails" -gt 0 ]; then
        ci_note "$fails self-test(s) did not behave"
        return 1
    fi
    ci_note "all self-tests behaved as expected"
    return 0
}

main
LANE_RC=$?

set +e
ci_finalize
FINAL_RC=$?
set -e

[ "$LANE_RC" -ne 0 ] && exit "$LANE_RC"
exit "$FINAL_RC"
