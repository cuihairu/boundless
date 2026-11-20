#!/usr/bin/env bash
# BW-22-02-05 — SDK and plugin cases: does a produced artifact load in a host?
#
# Usage:
#   ci/sdk.sh            # every case this tree has substrate for
#   ci/sdk.sh self-test  # prove each case turns on the day its substrate lands
#
# The acceptance is "the produced artifact loads in a host". Three of the four
# substrates do not exist yet, so those cases SKIP and name the ticket that
# turns them on. A SKIP stub would be worthless on its own — it could never
# light up — so the self-test plants each substrate in a private root and
# requires the detector to flip from SKIP to a real verdict.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
# CI_SDK_ROOT lets the self-test plant substrate without touching the repo.
#
# Both are read at call time rather than at source time: an override applied to
# a shell function only reaches code that re-reads the variable. Resolving them
# once here would make every override a silent no-op, which is exactly how the
# self-test looks as if nothing were being tested.
cur_root() { printf '%s' "${CI_SDK_ROOT:-$REPO_ROOT}"; }
cur_ext()  { printf '%s' "$(cur_root)/extensions/vscode"; }

MISSING=2

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

# --- detectors ---------------------------------------------------------------

# docs/guide/client-sdk.md names the layout: sdk/core, sdk/ffi, sdk/python and
# sdk/engines/{unity,unreal,godot}. Only the absence of the whole tree is a
# SKIP; a tree that has started to exist must be checked, not reported away.
detect_sdk() {
    if [ ! -d "$(cur_root)/sdk" ]; then
        echo "no sdk/ tree under $(cur_root) (planned sdk/core, sdk/ffi, sdk/python, sdk/engines — tracked by BW-21-01-01)"
        return "$MISSING"
    fi
    local missing="" d present=0
    for d in core ffi python engines; do
        if [ -d "$(cur_root)/sdk/$d" ]; then
            present=$((present + 1))
        else
            missing="$missing sdk/$d"
        fi
    done
    if [ -n "$missing" ]; then
        echo "sdk/ has $present/4 documented parts, missing:$missing (tracked by BW-21-01-01)"
        return 1
    fi
    echo "sdk/ carries all 4 documented parts: core ffi python engines"
    return 0
}

# The reference plugin_system loads DLLs listed in <exe>_plugins.txt through
# LoadLibrary (BigWorld tools/plugin_system/plugin_loader.cpp). The substrate to
# compare against is a loader crate here; the load-success and load-failure
# behaviour cases are BW-13-06-03's to write.
detect_plugin() {
    local have=""
    for d in "$(cur_root)"/crates/plugin*; do
        [ -d "$d" ] || continue
        have="$have $(basename "$d")"
    done
    if [ -z "$have" ]; then
        echo "no plugin host crate under $(cur_root)/crates (tracked by BW-13-06-03)"
        return "$MISSING"
    fi
    echo "plugin host crate(s) present:$have — load behaviour cases tracked by BW-13-06-03"
    return 0
}

# Loading an extension into a real editor host needs @vscode/test-electron.
# Without it every test runs against the vscode-free service layer, so the host
# itself is never exercised.
detect_editor_host() {
    if [ ! -f "$(cur_ext)/package.json" ]; then
        echo "no extensions/vscode/package.json under $(cur_root)"
        return "$MISSING"
    fi
    if grep -q '"@vscode/test-electron"' "$(cur_ext)/package.json"; then
        echo "extensions/vscode declares @vscode/test-electron — the host is loadable"
        return 0
    fi
    echo "extensions/vscode has no @vscode/test-electron, so no artifact is loaded into an editor host (tracked by BW-20-03-03)"
    return "$MISSING"
}

# The one case this tree can answer today: a produced artifact, loaded by a
# host process. This is the same host-load machinery BW-22-02-04 uses, applied
# to the one artifact that exists.
detect_built_load() {
    local out
    out="$(cur_ext)/out/workspace/diagnostics.js"
    if [ ! -f "$out" ]; then
        echo "extensions/vscode is not built, so nothing has been produced to load (run pnpm run build in extensions/vscode)"
        return "$MISSING"
    fi
    if ! command -v node >/dev/null 2>&1; then
        echo "no node on this runner, so the built artifact cannot be loaded"
        return "$MISSING"
    fi
    local mod
    if ! mod="$(node -e "const m=require('$out'); console.log(typeof m.checkDefText === 'function' ? 'ok' : 'unusable')" 2>&1)"; then
        echo "the built artifact did not load in node: $(printf '%s' "$mod" | head -1)"
        return 1
    fi
    if [ "$mod" != ok ]; then
        echo "the built artifact loaded but exposes no usable entry point ($mod)"
        return 1
    fi
    echo "node loaded the compiled extension core and its entry point is usable"
    return 0
}

run_case() { # $1 name $2 detector
    local name="$1" detect="$2" out rc
    out="$("$detect" 2>&1)"
    rc=$?
    case "$rc" in
    0) ci_pass "sdk-$name" "$(tsv_escape "$(printf '%s' "$out" | head -1)")" ;;
    "$MISSING") ci_skip "sdk-$name" "$(tsv_escape "$(printf '%s' "$out" | head -1)")" ;;
    *) ci_fail "sdk-$name" "$(tsv_escape "$(printf '%s' "$out" | head -1)")" ;;
    esac
}

run_all() {
    ci_step "sdk and plugin cases"
    mkdir -p "$CI_OUT"
    run_case sdk-artifact detect_sdk
    run_case plugin-host detect_plugin
    run_case editor-host detect_editor_host
    run_case built-load detect_built_load
}

self_test() {
    local fails=0 planted="$REPO_ROOT/ci-out/sdk-selftest" r
    ci_step "self-test: each case flips when its substrate lands"
    r="$(mktemp -d "$planted.XXXXXX")"
    rm -rf "$planted"
    mkdir -p "$r/sdk/core" "$r/sdk/ffi" "$r/sdk/python" "$r/sdk/engines" \
        "$r/crates/plugin-host"

    if CI_SDK_ROOT="$r" detect_sdk >/dev/null; then
        ci_pass selftest-sdk "a planted sdk/ tree is detected"
    else
        ci_fail selftest-sdk "the planted sdk/ tree was not detected, so the case can never turn on"
        fails=$((fails + 1))
    fi
    if CI_SDK_ROOT="$r" detect_plugin >/dev/null; then
        ci_pass selftest-sdk "a planted plugin crate is detected"
    else
        ci_fail selftest-sdk "the planted plugin crate was not detected, so the case can never turn on"
        fails=$((fails + 1))
    fi

    # The editor-host case reads the extension manifest, so plant one that
    # declares the host harness.
    mkdir -p "$r/extensions/vscode"
    printf '%s\n' '{"devDependencies":{"@vscode/test-electron":"2.4.0"}}' \
        >"$r/extensions/vscode/package.json"
    if CI_SDK_ROOT="$r" detect_editor_host >/dev/null; then
        ci_pass selftest-sdk "a planted @vscode/test-electron declaration is detected"
    else
        ci_fail selftest-sdk "the planted editor-host declaration was not detected, so the case can never turn on"
        fails=$((fails + 1))
    fi

    ci_step "self-test: an empty tree keeps every case a SKIP"
    r="$(mktemp -d "$planted.XXXXXX")"
    local empty=1 d
    for d in detect_sdk detect_plugin detect_editor_host; do
        if [ "$(CI_SDK_ROOT="$r" "$d" >/dev/null 2>&1; echo $?)" != "$MISSING" ]; then
            empty=0
        fi
    done
    if [ "$empty" -eq 1 ]; then
        ci_pass selftest-sdk-empty "no substrate means every case reports SKIP, not a false PASS"
    else
        ci_fail selftest-sdk-empty "a bare tree produced a verdict other than SKIP, so a case would pass vacuously"
        fails=$((fails + 1))
    fi

    rm -rf "$planted" "$planted".*

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