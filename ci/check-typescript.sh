#!/usr/bin/env bash
# BW-22-01-04 — TypeScript check lanes.
#
# Usage:
#   ci/check-typescript.sh                 # whole tree
#   ci/check-typescript.sh self-test      # prove the lane bites
#
# Boundless carries TypeScript in two places (todo.md): extensions/vscode/ for
# the BW-20 plugin and docs/.vitepress/ for the BW-19 site. The lane discovers
# every tsconfig.json in the tree, typechecks each project, and runs the
# package's own test script when one exists.
#
# Nothing is there yet, so the lanes SKIP until a tsconfig lands — and say
# which directory will turn them on. Failures are reduced to `file(line,col)`
# lists, which is how tsc reports them.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"

# A tsconfig is the unit of work. Discovery covers the whole checkout, tracked
# or not, so an uncommitted extension project is gated the day it appears.
#
# Filter the basename with grep instead of a git pathspec: `tsconfig*.json`
# as a pathspec matches nothing nested, because a pattern with a wildcard but
# no slash is tested against the whole path, not the file name. Listing files
# and matching `(^|/)tsconfig…\.json` is the same set and does not depend on
# pathspec rules at all.
tsconfigs() {
    {
        git -C "$REPO_ROOT" ls-files --cached 2>/dev/null
        git -C "$REPO_ROOT" ls-files --others --exclude-standard 2>/dev/null
    } | sort -u | grep -E '(^|/)tsconfig[^/]*\.json$' || true
}

# tsc, preferring the project's own copy (its pinned version is what ships).
resolve_tsc() {
    local proj="$1"
    if [ -x "$REPO_ROOT/$proj/node_modules/.bin/tsc" ]; then
        printf '%s' "$REPO_ROOT/$proj/node_modules/.bin/tsc"
        return 0
    fi
    if command -v tsc >/dev/null 2>&1; then
        command -v tsc
        return 0
    fi
    if command -v npx >/dev/null 2>&1 && npx --no-install tsc --version >/dev/null 2>&1; then
        printf 'npx --no-install tsc'
        return 0
    fi
    return 1
}

# tsc prints `src/foo.ts(12,3): error TS2322: ...`; keep the leading part.
# The sandbox in the self-test lives outside the checkout, so drop any leading
# `../tmp/<random>/proj/` scaffolding too and keep just the file position.
first_errors() {
    local report="$1" positioned
    [ -f "$report" ] || return 0
    positioned="$(grep -oE '[A-Za-z0-9_./-]+\([0-9]+,[0-9]+\): error [A-Z]+[0-9]+' "$report" \
        | sed "s|^$REPO_ROOT/||; s|^\./||" \
        | sed -E 's|^(\.\./)+tmp/[^/]+/proj/||' | sort -u | head -12 | paste -sd, -)"
    if [ -n "$positioned" ]; then
        printf '%s' "$positioned"
        return 0
    fi

    # A config-level diagnostic carries no position at all: TS18002/TS18003 for
    # an include that matches nothing, TS18003 for an empty project. Those still
    # name the tsconfig, so say so rather than failing with a blank detail.
    grep -m1 -oE 'error TS[0-9]+[^]]{0,90}' "$report" \
        | sed "s|^$REPO_ROOT/||; s|^\./||"
}

main() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    cd "$REPO_ROOT"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    if [ "$MODE" = self-test ]; then
        self_test
        return $?
    fi

    local configs n
    configs="$(tsconfigs)"
    n="$(printf '%s' "$configs" | grep -c 'tsconfig' || true)"

    if [ -z "$configs" ]; then
        ci_skip ts-typecheck "no tsconfig.json — lane turns on with extensions/vscode (BW-20) or docs/ (BW-19)"
        ci_skip ts-test "no tsconfig.json — lane turns on with extensions/vscode (BW-20) or docs/ (BW-19)"
        return 0
    fi

    ci_note "$n tsconfig project(s)"
    local proj tsc dir
    while IFS= read -r proj; do
        [ -n "$proj" ] || continue
        dir="$(dirname "$proj")"
        ci_step "typecheck $dir"
        if ! tsc="$(resolve_tsc "$dir")"; then
            ci_fail ts-typecheck "no tsc available for $dir — install typescript as a devDependency"
            continue
        fi
        local report="$CI_LOG_DIR/ts-typecheck-$dir.log"
        mkdir -p "$(dirname "$report")"
        ci_pipe "ts-typecheck-$dir" "$tsc" --noEmit -p "$proj"
        if [ "${CI_RC:-1}" -eq 0 ]; then
            ci_pass ts-typecheck "ok ($dir)"
        else
            ci_fail ts-typecheck "$(first_errors "$report" | sed 's/, */, /g')"
        fi

        # The package's own test script is the test lane; a plugin without one
        # has no substrate for it yet.
        if grep -q '"test"' "$dir/package.json" 2>/dev/null; then
            ci_step "test $dir"
            local pkg_report="$CI_LOG_DIR/ts-test-$dir.log"
            ci_pipe "ts-test-$dir" npm test --prefix "$dir" --silent
            if [ "${CI_RC:-1}" -eq 0 ]; then
                ci_pass ts-test "ok ($dir)"
            else
                ci_fail ts-test "$(first_errors "$pkg_report" | sed 's/, */, /g' | sed 's/^$/see logs/' )"
            fi
        else
            ci_skip ts-test "no test script in $dir"
        fi
    done <<<"$configs"
}

# Plant a type error in a throwaway project and demand that tsc flags that
# file; then fix it and demand green. The sandbox lives outside the tree.
self_test() {
    local sandbox proj src tsc fails=0
    sandbox="$(mktemp -d)"
    proj="$sandbox/proj"
    src="$proj/src"
    mkdir -p "$src"
    cat >"$proj/tsconfig.json" <<'EOF'
{
  "compilerOptions": {
    "strict": true,
    "noEmit": true,
    "target": "ES2022",
    "lib": ["ES2022"],
    "types": []
  },
  "include": ["src/**/*.ts"]
}
EOF
    printf 'export function add(a: number, b: number): number {\n    return a + b;\n}\n' >"$src/clean.ts"
    printf 'export function bad(x: string): number {\n    return x;\n}\n' >"$src/bad.ts"

    if ! tsc="$(resolve_tsc "$proj")"; then
        ci_skip selftest-ts "no tsc on this machine, so the lane cannot be proven to bite"
        rm -rf "$sandbox"
        return 0
    fi

    local report="$sandbox/typecheck.log" where
    ci_step "self-test: type error is found and located"
    # shellcheck disable=SC2086
    if $tsc --noEmit -p "$proj/tsconfig.json" >"$report" 2>&1; then
        ci_fail selftest-ts "tsc accepted a project with a type error"
        fails=$((fails + 1))
    else
        where="$(first_errors "$report")"
        case "$where" in
        *bad.ts*) ci_pass selftest-ts "located $where" ;;
        *) ci_fail selftest-ts "went red but reported $where"; fails=$((fails + 1)) ;;
        esac
    fi

    ci_step "self-test: clean project passes"
    rm -f "$src/bad.ts"
    # shellcheck disable=SC2086
    if $tsc --noEmit -p "$proj/tsconfig.json" >/dev/null 2>&1; then
        ci_pass selftest-ts-clean "clean project passes"
    else
        ci_fail selftest-ts-clean "a clean project still goes red"
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
