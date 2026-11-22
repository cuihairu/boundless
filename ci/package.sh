#!/usr/bin/env bash
# BW-22-04-01 — the installer lane: build, stage, archive and smoke-launch
# the server package for the platform this lane runs on.
#
# Usage:
#   ci/package.sh            # build + stage + archive + smoke into ci-out/package/
#   ci/package.sh self-test  # prove the smoke bites on both outcomes
#
# The acceptance is "each target's smoke can install and launch". Install is
# unpacking the archive into a clean prefix; launch is running the packaged
# binary under a role and requiring exit 0 plus the startup line — the same
# observable contract the runners lane records (BW-22-02-01). The lane
# packages the platform it runs on; the other two targets come from the
# packaging workflow's matrix, whose legs report rather than gate until
# BW-15 starts them (only Linux compiles today).
#
# Nothing here tags or releases: the product is a local archive under
# ci-out/package/ and a workflow artifact. Versioned releases stay
# user-triggered (repo red line); this lane only reads the workspace
# version for the file name.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
# Call-time resolution: the self-test plants a built binary, a role list and
# a private output root, and an override applied to a shell function only
# reaches code that re-reads the variable.
cur_bin() { printf '%s' "${CI_PACKAGE_BIN:-}"; }
cur_out() { printf '%s' "${CI_PACKAGE_OUT:-$CI_OUT}"; }
cur_roles() { printf '%s' "${CI_PACKAGE_ROLES:-}"; }

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

PKG=boundless-server
ROLES_SOURCE=crates/server-runtime/src/app.rs

# --- platform -----------------------------------------------------------------

# The name the archive carries. Mapped to the three faces of BW-15-02..04;
# anything else is recorded as what it is, not squeezed into a bucket.
platform_name() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    MINGW* | MSYS* | CYGWIN*) os=windows ;;
    *) ;;
    esac
    printf '%s-%s' "$os" "$arch"
}

first_role() {
    local given
    given="$(cur_roles)"
    if [ -n "$given" ]; then
        printf '%s\n' "$given" | head -1
        return 0
    fi
    [ -f "$REPO_ROOT/$ROLES_SOURCE" ] || return 1
    sed -n '/pub const KNOWN_ROLES/,/\];/p' "$REPO_ROOT/$ROLES_SOURCE" \
        | grep -oE '"[A-Za-z0-9_-]+"' | tr -d '"' | head -1
}

# --- build, stage, archive, smoke ---------------------------------------------

build_binary() {
    local out rc
    out="$(cargo build --release -p "$PKG" 2>&1)"
    rc=$?
    printf '%s\n' "$out" >"$CI_LOG_DIR/package-build.log"
    return "$rc"
}

stage_prefix() {
    # stage_prefix <src-binary> <prefix> — the layout an installer lays down.
    local src="$1" prefix="$2"
    mkdir -p "$prefix/bin"
    cp "$src" "$prefix/bin/$PKG" || return 1
    chmod +x "$prefix/bin/$PKG"
}

archive_prefix() {
    # archive_prefix <prefix> <tarball> — deterministic member order so two
    # builds of the same tree differ only in the binary itself.
    local prefix="$1" tarball="$2"
    tar -C "$prefix" -czf "$tarball" bin
}

smoke_install() {
    # smoke_install <tarball> <install-dir> <role> — unpack into a clean
    # prefix and launch what was installed, not what was built.
    local tarball="$1" installdir="$2" role="$3" out rc
    mkdir -p "$installdir"
    # tar's own noise goes to the lane log, not into the capture below.
    tar -C "$installdir" -xzf "$tarball" >&2 || {
        printf 'the archive did not extract'
        return 1
    }
    [ -x "$installdir/bin/$PKG" ] || {
        printf 'the archive has no executable bin/%s' "$PKG"
        return 2
    }
    out="$(timeout 30 "$installdir/bin/$PKG" "$role" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'exit %s: %s' "$rc" "$out"
        return 3
    fi
    if ! printf '%s\n' "$out" | grep -q 'starting'; then
        printf 'exit 0 but no startup line: %s' "$out"
        return 4
    fi
    printf '%s' "$out"
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    local platform
    platform="$(platform_name)"
    ci_step "package for $platform"

    local version tarball staged
    version="$(ci_version)"
    [ -n "$version" ] || version=untagged
    tarball="$(cur_out)/package/boundless-${version}-${platform}.tar.gz"
    staged="$(cur_out)/package/stage"
    mkdir -p "$(cur_out)/package"

    # Build, unless the self-test hands us an already-built binary.
    local bin
    bin="$(cur_bin)"
    if [ -z "$bin" ]; then
        if ! command -v cargo >/dev/null 2>&1; then
            ci_skip package-build "no cargo on this runner; the pinned toolchain is the toolchain lane's product (BW-22-01)"
            ci_note "build skipped; stage/archive/smoke need a binary to package"
            return 0
        fi
        ci_step "release build"
        if ! build_binary; then
            ci_fail package-build "cargo build --release -p $PKG failed; see package-build.log"
            return 1
        fi
        ci_pass package-build "release build of $PKG"
        bin="$REPO_ROOT/target/release/$PKG"
    fi

    if [ ! -f "$bin" ]; then
        ci_fail package-stage "the built binary is missing: $bin"
        return 1
    fi
    rm -rf "$staged"
    if ! stage_prefix "$bin" "$staged"; then
        ci_fail package-stage "staging the prefix failed"
        return 1
    fi
    ci_pass package-stage "prefix staged: bin/$PKG"

    if ! archive_prefix "$staged" "$tarball"; then
        ci_fail package-archive "tar -czf failed for $tarball"
        return 1
    fi
    ci_pass package-archive "$(basename "$tarball") ($(du -h "$tarball" | cut -f1))"

    local role smoke_out
    role="$(first_role)"
    if [ -z "$role" ]; then
        ci_skip package-smoke "no role list at $ROLES_SOURCE (BW-00-03-05 owns the roles)"
        return 0
    fi
    local installdir
    installdir="$(cur_out)/package/install-smoke"
    rm -rf "$installdir"
    if ! smoke_out="$(smoke_install "$tarball" "$installdir" "$role")"; then
        ci_fail package-smoke "installed $PKG ($role) did not launch cleanly: $(tsv_escape "$smoke_out")"
        return 1
    fi
    ci_pass package-smoke "installed prefix launches: $(tsv_escape "$smoke_out")"
}

self_test() {
    local fails=0 r stub out
    ci_step "self-test: a green stub round-trips stage, archive and smoke"
    r="$(mktemp -d "$REPO_ROOT/ci-out/package-selftest.XXXXXX")"
    stub="$r/$PKG"
    cat >"$stub" <<'STUB'
#!/bin/sh
echo "[baseapp] starting (boundless minimal entry)" >&2
exit 0
STUB
    chmod +x "$stub"
    # The child gets a private manifest too: its stub-driven package rows are
    # about the smoke logic, not the tree, and must not read as a real build.
    out="$(CI_OUT="$r/manifest" CI_PACKAGE_BIN="$stub" CI_PACKAGE_ROLES=baseapp CI_PACKAGE_OUT="$r/out" \
        "$0" 2>&1)"
    rc=$?
    set -- "$r"/out/package/boundless-*.tar.gz
    if [ "$rc" -eq 0 ] \
        && printf '%s\n' "$out" | grep -q 'package-smoke	PASS	installed prefix launches' \
        && [ -f "$1" ]; then
        ci_pass selftest-package "a packaged prefix installs and launches ($(basename "$1"))"
    else
        ci_fail selftest-package "the green stub did not survive stage/archive/smoke: $(printf '%s\n' "$out" | grep package-smoke | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a stub that exits non-zero goes red at smoke"
    cat >"$stub" <<'STUB'
#!/bin/sh
echo "stub-server: explosion" >&2
exit 3
STUB
    chmod +x "$stub"
    out="$(CI_OUT="$r/manifest" CI_PACKAGE_BIN="$stub" CI_PACKAGE_ROLES=baseapp CI_PACKAGE_OUT="$r/out" \
        "$0" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ] && printf '%s\n' "$out" | grep -q 'package-smoke	FAIL.*exit 3'; then
        ci_pass selftest-package-red "a launch failure is named with its exit code"
    else
        ci_fail selftest-package-red "the red stub did not fail the smoke: $(printf '%s\n' "$out" | grep package-smoke | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a missing binary fails staging instead of packaging air"
    out="$(CI_OUT="$r/manifest" CI_PACKAGE_BIN="$r/does-not-exist" CI_PACKAGE_ROLES=baseapp CI_PACKAGE_OUT="$r/out" \
        "$0" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ] && printf '%s\n' "$out" | grep -q 'package-stage	FAIL.*missing'; then
        ci_pass selftest-package-missing "a missing built binary fails the stage"
    else
        ci_fail selftest-package-missing "a missing binary did not fail staging: $(printf '%s\n' "$out" | grep package-stage | head -1)"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$REPO_ROOT/ci-out/package-selftest".*

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
