#!/usr/bin/env bash
# BW-22-04-03 — the engine SDK archive lane: package each engine adapter and
# prove it imports from an empty project.
#
# Usage:
#   ci/sdk-archive.sh            # archive every engine SDK present
#   ci/sdk-archive.sh self-test  # prove the import gate bites
#
# The acceptance is "importable from an empty project". For each engine
# tree under sdk/engines/ the lane archives the adapter into
# ci-out/sdk/boundless-sdk-<engine>-<version>-<commit>.tar.gz and then runs
# the adapter's own import-check executable from a clean scratch project —
# the empty project of the acceptance. An engine whose tree does not exist
# yet SKIPs and names the ticket that turns it on; a tree that has started
# but cannot prove importability FAILs, because a half-built adapter is
# exactly what must not be archived silently.
#
# Nothing here tags or releases: the product is a local archive under
# ci-out/sdk/ and a workflow artifact. Versioned releases stay
# user-triggered (repo red line); this lane only reads the workspace
# version for names.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
# Call-time resolution: the self-test plants engine trees and a private
# output root, and an override applied to a shell function only reaches
# code that re-reads the variable.
cur_root() { printf '%s' "${CI_SDKARC_ROOT:-$REPO_ROOT}"; }
cur_out() { printf '%s' "${CI_SDKARC_OUT:-$CI_OUT}"; }

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

ENGINES_ROOT=sdk/engines
# engine -> ticket that owns its packaging acceptance
ticket_for() {
    case "$1" in
    unity) printf 'BW-21-03-03' ;;
    unreal) printf 'BW-21-04-01' ;;
    godot) printf 'BW-21-05-01' ;;
    *) printf 'BW-21-01-01' ;;
    esac
}

# The manifest each engine's package carries once the adapter is real.
# A tree with none of these has not started packaging at all.
has_manifest() {
    local dir="$1" engine="$2"
    case "$engine" in
    unity) [ -f "$dir/package.json" ] ;;
    unreal) ls "$1"/*.uplugin >/dev/null 2>&1 ;;
    godot) [ -f "$dir/plugin.cfg" ] || [ -f "$dir/project.godot" ] ;;
    *) return 1 ;;
    esac
}

# --- archive and import smoke ---------------------------------------------------

archive_engine() {
    # archive_engine <engine> <dir> <version> <commit> — tar the adapter
    # tree, excluding nothing: what the engine imports is what we archive.
    local engine="$1" dir="$2" version="$3" commit="$4" tarball
    tarball="$(cur_out)/sdk/boundless-sdk-${engine}-${version}-${commit}.tar.gz"
    mkdir -p "$(cur_out)/sdk"
    tar -C "$(dirname "$dir")" -czf "$tarball" "$(basename "$dir")" >&2 || return 1
    printf '%s' "$tarball"
}

import_smoke() {
    # import_smoke <dir> — the acceptance: from an empty project, the
    # adapter's own import-check proves the package loads. The check runs
    # inside the scratch project with the SDK path as its only argument;
    # exit 0 is the whole verdict.
    local dir="$1" check project out rc
    check="$dir/import-check"
    if [ ! -x "$check" ]; then
        printf 'the package has no executable import-check — importability unproven'
        return 2
    fi
    project="$(mktemp -d "$(cur_out)/sdk/empty-project.XXXXXX")"
    out="$(cd "$project" && "$check" "$dir" 2>&1)"
    rc=$?
    rm -rf "$project"
    if [ "$rc" -ne 0 ]; then
        printf 'import-check exited %s from an empty project: %s' "$rc" "$out"
        return 1
    fi
    printf '%s' "$out"
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    local version commit engines_dir engine dir fail_total=0 any=0
    version="$(ci_version)"
    [ -n "$version" ] || version=untagged
    commit="$(ci_git_sha)"
    engines_dir="$(cur_root)/$ENGINES_ROOT"

    if [ ! -d "$engines_dir" ]; then
        ci_skip sdk-archive "no $ENGINES_ROOT/ tree (planned sdk/engines/{unity,unreal,godot} — tracked by BW-21-01-01)"
        return 0
    fi

    ci_step "engine SDK archives"

    for engine in unity unreal godot; do
        dir="$engines_dir/$engine"
        if [ ! -d "$dir" ]; then
            ci_skip "sdk-archive-$engine" \
                "no $ENGINES_ROOT/$engine tree — packaging is tracked by $(ticket_for "$engine")"
            continue
        fi
        any=1
        if ! has_manifest "$dir" "$engine"; then
            ci_fail "sdk-archive-$engine" \
                "$ENGINES_ROOT/$engine exists but has no package manifest — a started adapter must be checkable"
            fail_total=$((fail_total + 1))
            continue
        fi
        local tarball smoke_out
        if ! tarball="$(archive_engine "$engine" "$dir" "$version" "$commit")"; then
            ci_fail "sdk-archive-$engine" "archiving the adapter failed"
            fail_total=$((fail_total + 1))
            continue
        fi
        if ! smoke_out="$(import_smoke "$dir")"; then
            ci_fail "sdk-archive-$engine" "$(tsv_escape "$smoke_out")"
            fail_total=$((fail_total + 1))
            continue
        fi
        ci_pass "sdk-archive-$engine" \
            "$(basename "$tarball") imports from an empty project: $(tsv_escape "$smoke_out")"
    done

    if [ "$any" -eq 0 ]; then
        ci_note "no engine tree exists yet; every engine reported SKIP"
    fi
    [ "$fail_total" -eq 0 ]
}

self_test() {
    local fails=0 r out stub_dir
    ci_step "self-test: an engine with a manifest and import-check archives and imports"
    r="$(mktemp -d "$REPO_ROOT/ci-out/sdkarc-selftest.XXXXXX")"
    stub_dir="$r/sdk/engines/unity"
    mkdir -p "$stub_dir"
    printf '{"name": "com.boundless.sdk"}\n' >"$stub_dir/package.json"
    cat >"$stub_dir/import-check" <<'STUB'
#!/bin/sh
echo "unity: package.json accepted by a stub importer"
exit 0
STUB
    chmod +x "$stub_dir/import-check"
    out="$(CI_OUT="$r/manifest" CI_SDKARC_ROOT="$r" CI_SDKARC_OUT="$r/out" "$0" 2>&1)"
    rc=$?
    set -- "$r"/out/sdk/boundless-sdk-unity-*.tar.gz
    if [ "$rc" -eq 0 ] \
        && printf '%s\n' "$out" | grep -q 'sdk-archive-unity	PASS	.*imports from an empty project' \
        && [ -f "$1" ] \
        && tar -tzf "$1" | grep -q 'unity/import-check'; then
        ci_pass selftest-sdkarc "a packaged adapter archives and imports from an empty project"
    else
        ci_fail selftest-sdkarc "the green adapter did not survive archive/import: $(printf '%s\n' "$out" | grep 'sdk-archive-unity' | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: the other engines stay honest SKIP while absent"
    if printf '%s\n' "$out" | grep -q 'sdk-archive-unreal	SKIP.*BW-21-04-01' \
        && printf '%s\n' "$out" | grep -q 'sdk-archive-godot	SKIP.*BW-21-05-01'; then
        ci_pass selftest-sdkarc-absent "absent engines report SKIP and name their tickets"
    else
        ci_fail selftest-sdkarc-absent "absent engines did not SKIP with tickets"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a manifest without import-check fails, not silently archives"
    rm "$stub_dir/import-check"
    out="$(CI_OUT="$r/manifest" CI_SDKARC_ROOT="$r" CI_SDKARC_OUT="$r/out" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ] && printf '%s\n' "$out" | grep -q 'sdk-archive-unity	FAIL.*no executable import-check'; then
        ci_pass selftest-sdkarc-unproven "importability cannot be skipped by omitting the check"
    else
        ci_fail selftest-sdkarc-unproven "a package without import-check did not fail: $(printf '%s\n' "$out" | grep 'sdk-archive-unity' | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: an import-check that fails in the empty project goes red"
    cat >"$stub_dir/import-check" <<'STUB'
#!/bin/sh
echo "unity: import exploded" >&2
exit 3
STUB
    chmod +x "$stub_dir/import-check"
    out="$(CI_OUT="$r/manifest" CI_SDKARC_ROOT="$r" CI_SDKARC_OUT="$r/out" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ] && printf '%s\n' "$out" | grep -q 'sdk-archive-unity	FAIL.*exited 3'; then
        ci_pass selftest-sdkarc-red "a failing import is named with its exit code"
    else
        ci_fail selftest-sdkarc-red "a failing import-check did not fail the lane: $(printf '%s\n' "$out" | grep 'sdk-archive-unity' | head -1)"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$REPO_ROOT/ci-out/sdkarc-selftest".*

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
