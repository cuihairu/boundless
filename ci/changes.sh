#!/usr/bin/env bash
# BW-22-01-05 — decide which lanes a change exercises, from the changed paths.
#
# Usage:
#   ci/changes.sh [base] [head]              # human + machine output
#   ci/changes.sh --machine [base] [head]    # key=value only, for GITHUB_OUTPUT
#
# Lanes are additive: a change that touches only markdown cannot be expected to
# fix the Rust tree, and a change that only touches ci/ must prove the harness
# still bites, not gate the engine. An empty scope is a real answer, not an
# error — downstream lanes record SKIP so the run stays readable.
#
# Package scoping for Rust is derived from the manifest, not hard-coded: a
# crate directory maps to the package name in its own Cargo.toml, and a change
# to the root manifest touches every package because the graph may have moved.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MACHINE=0
case "${1:-}" in
--machine) MACHINE=1; shift ;;
esac

BASE="${1:-}"
HEAD="${2:-HEAD}"

INCLUDE_WIP=1

lanes=()
add_lane() {
    local have=0 l
    for l in "${lanes[@]}"; do [ "$l" = "$1" ] && have=1; done
    [ "$have" -eq 1 ] || lanes+=("$1")
}

changed_paths() {
    local out=""
    out="$(git -C "$REPO_ROOT" diff --name-only --no-renames "$BASE" "$HEAD" 2>/dev/null)"
    if [ "$INCLUDE_WIP" -eq 1 ]; then
        # Uncommitted work gates locally; a push has nothing here.
        out="$out
$(git -C "$REPO_ROOT" ls-files --others --exclude-standard 2>/dev/null)
$(git -C "$REPO_ROOT" diff --name-only --no-renames 2>/dev/null)
$(git -C "$REPO_ROOT" diff --name-only --no-renames --cached 2>/dev/null)"
    fi
    printf '%s\n' "$out" | sed '/^$/d' | sort -u
}

# Map a changed path to the workspace package that owns it, if any. A package
# is the crates/<name> or apps/<name> directory, whatever depth the change sits
# at under it.
owning_package() {
    local path="$1" sub manifest
    case "$path" in
    crates/*/* | apps/*/*) ;;
    *) return 1 ;;
    esac
    sub="$(printf '%s' "$path" | cut -d/ -f1-2)"
    manifest="$sub/Cargo.toml"
    [ -f "$manifest" ] || return 1
    sed -n 's/^name[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$manifest" | head -1
}

main() {
    mkdir -p "$CI_OUT"
    if [ -z "$BASE" ]; then
        BASE="$(ci_base_ref)"
    fi
    if ! git -C "$REPO_ROOT" rev-parse --verify "$HEAD" >/dev/null 2>&1; then
        ci_note "head $HEAD does not resolve; diffing against the working tree instead"
        HEAD=""
    fi

    local paths n
    paths="$(changed_paths)"
    n="$(printf '%s' "$paths" | grep -c . || true)"

    local p owner
    local rust_pkgs=""
    local py=0 ts=0 md=0 rs=0 ci=0 tc=0 run=0 store=0 parity=0 sdk=0 load=0 fault=0 report=0 package=0 docspkg=0 sdkarc=0 vsix=0
    local rust_all=0

    while IFS= read -r p; do
        [ -n "$p" ] || continue
        case "$p" in
        ci/* | .github/workflows/*)
            ci=1
            tc=1
            ;;
        rust-toolchain.toml | Cargo.toml | Cargo.lock)
            tc=1
            # A change to the graph or the pin affects every package.
            rust_all=1
            ;;
        esac

        case "$p" in
        *.rs | crates/*/Cargo.toml | apps/*/Cargo.toml) rs=1 ;;
        esac
        # The three-leg runner matrix is expensive, so it follows the code that
        # a runner actually executes: the platform interface, the role entry
        # point, the runtime it runs in, or the toolchain that compiles it.
        case "$p" in
        crates/platform/* | apps/server/* | crates/server-runtime/* \
            | rust-toolchain.toml | Cargo.toml | Cargo.lock) run=1 ;;
        esac
        case "$p" in
        *.py) py=1 ;;
        esac
        case "$p" in
        *.ts | *.tsx | package.json | package-lock.json | tsconfig*.json) ts=1 ;;
        esac
        case "$p" in
        *.md | *.mdx | docs/* | *.svg) md=1 ;;
        esac
        # Storage cases run against a backend the lane provisions or a service
        # container supplies, so they follow their own case files and the lane.
        case "$p" in
        tests/store/* | ci/store.sh) store=1 ;;
        esac
        # The compatibility differential compares our checker against the
        # original tool, so it follows its fixtures and the lane.
        case "$p" in
        tests/parity/* | ci/parity.sh) parity=1 ;;
        esac
        # The SDK and plugin cases gate on artifacts a host loads, so they
        # follow the SDK tree, the plugin crates and the lane.
        case "$p" in
        sdk/* | tests/sdk/* | crates/plugin*/* | tests/plugins/* | ci/sdk.sh) sdk=1 ;;
        esac
        # The load cases follow their scenario set and the driver crate.
        case "$p" in
        tests/load/* | apps/bots/* | ci/load.sh) load=1 ;;
        esac
        # The fault drill runs the fixtures of the harness crate that provides
        # them, so it follows that crate and the lane.
        case "$p" in
        crates/test-support/* | tests/fault/* | ci/fault.sh) fault=1 ;;
        esac
        # The comparison report follows its lane.
        case "$p" in
        ci/report.sh) report=1 ;;
        esac
        # The installer lane packages the server binary, so it follows the
        # entry crate, the runtime it ships and the lane.
        case "$p" in
        apps/server/* | crates/server-runtime/* | ci/package.sh) package=1 ;;
        esac
        # The docs release lane builds and archives the site, so it follows
        # the docs tree and the lane.
        case "$p" in
        docs/* | ci/docs-package.sh) docspkg=1 ;;
        esac
        # The engine SDK archives follow their adapter trees and the lane.
        case "$p" in
        sdk/engines/* | ci/sdk-archive.sh) sdkarc=1 ;;
        esac
        # The VSIX lane follows the extension it packages and the lane.
        case "$p" in
        extensions/vscode/* | ci/vsix.sh) vsix=1 ;;
        esac

        if owner="$(owning_package "$p")" && [ "$owner" != workspace-root ] && [ -n "$owner" ]; then
            if [[ "$rust_pkgs" != *"$owner"* ]]; then
                rust_pkgs="${rust_pkgs:+$rust_pkgs,}$owner"
            fi
        fi
    done <<<"$paths"

    if [ "$ci" -eq 1 ]; then
        add_lane ci
        add_lane toolchain
        add_lane rust
        add_lane python
        add_lane typescript
        add_lane docs
    else
        [ "$tc" -eq 1 ] && add_lane toolchain
        [ "$rs" -eq 1 ] && add_lane rust
        [ "$py" -eq 1 ] && add_lane python
        [ "$ts" -eq 1 ] && add_lane typescript
        [ "$md" -eq 1 ] && add_lane docs
    fi

    # Outside the ci=1 branch on purpose: a change to ci/ proves the harness
    # bites via the harness job's self-tests, which is the cheap half. The
    # three-leg matrix follows the code a runner executes, whatever else moved.
    [ "$run" -eq 1 ] && add_lane runners
    [ "$store" -eq 1 ] && add_lane store
    [ "$parity" -eq 1 ] && add_lane parity
    [ "$sdk" -eq 1 ] && add_lane sdk
    [ "$load" -eq 1 ] && add_lane load
    [ "$fault" -eq 1 ] && add_lane fault
    [ "$report" -eq 1 ] && add_lane report
    [ "$package" -eq 1 ] && add_lane package
    [ "$docspkg" -eq 1 ] && add_lane docspkg
    [ "$sdkarc" -eq 1 ] && add_lane sdkarc
    [ "$vsix" -eq 1 ] && add_lane vsix

    # A Rust source change whose owning package could not be resolved (a crate
    # directory that has no Cargo.toml yet, a stray .rs outside crates/ and
    # apps/) widens to the whole graph. The alternative is that both rust jobs
    # are skipped, since they gate on a non-empty package list — and a Rust
    # change that gates nothing is the one outcome this lane exists to prevent.
    if [ "$rs" -eq 1 ] && [ -z "$rust_pkgs" ] && [ "$rust_all" -eq 0 ]; then
        rust_all=1
    fi

    # The toolchain lane always runs: it is cheap and it is the check that a
    # checkout resolves the pinned Rust at all.
    add_lane toolchain

    local lane_csv=""
    local l
    for l in "${lanes[@]}"; do
        lane_csv="${lane_csv:+$lane_csv,}$l"
    done

    if [ "$MACHINE" -eq 1 ]; then
        printf 'lanes=%s\n' "$lane_csv"
        printf 'rust_packages=%s\n' "${rust_pkgs:-}"
        printf 'rust_all=%s\n' "$rust_all"
        printf 'changed_files=%s\n' "$n"
        printf 'base=%s\nhead=%s\n' "$BASE" "$HEAD"
        return 0
    fi

    printf '%s\t%s\t%s\t%s\n' "lane" "rust_packages" "base" "head" >"$CI_OUT/changes.tsv"
    printf '%s\t%s\t%s\t%s\n' "$lane_csv" "${rust_pkgs:-none}" "$BASE" "$HEAD" >>"$CI_OUT/changes.tsv"
    printf '\n== changed paths (%s) ==\n' "$n"
    printf '%s\n' "$paths" | head -200
    ci_note "lanes: ${lane_csv:-none}; rust packages: ${rust_pkgs:-none}; graph-wide: $rust_all"
}

main
exit $?
