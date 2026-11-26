#!/usr/bin/env bash
# OPT-14-01-02 — A/B the packaged binary size with and without symbol
# stripping. Both legs are built from the current tree: the baseline leg
# overrides `strip` back to false, the candidate leg uses the profile as
# committed. Each leg is built into its own target dir so neither can reuse
# the other's artifact.
#
# Nothing is published: these are local binaries and local tarballs.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
PKG=boundless-server
FIXTURE="$REPO_ROOT/docs/review/fixtures/opt-14-01-02/package-layout-ab.tsv"

build_leg() {
    # build_leg <target-dir> <strip-value>; prints the binary path.
    local target="$1" stripval="$2"
    CARGO_TARGET_DIR="$target" cargo build --release -p "$PKG" \
        --config "profile.release.strip=\"$stripval\"" >"$CI_LOG_DIR/package-size-$stripval.log" 2>&1
}

archive_leg() {
    # archive_leg <binary> <tarball> — the same bin-only layout ci/package.sh
    # stages, so the two tar sizes are directly comparable.
    local bin="$1" tarball="$2" stage
    stage="$(mktemp -d "$REPO_ROOT/ci-out/package-size-stage.XXXXXX")"
    mkdir -p "$stage/bin"
    cp "$bin" "$stage/bin/$PKG"
    tar -C "$stage" -czf "$tarball" bin
    rm -rf "$stage"
}

measure() {
    local bin="$1" tarball="$2" prefix="$3"
    printf '%s\t%s\t%s\n' "$prefix-binary-bytes" \
        "$(stat -c %s "$bin" 2>/dev/null || stat -f %z "$bin")" bytes
    printf '%s\t%s\t%s\n' "$prefix-archive-bytes" \
        "$(stat -c %s "$tarball" 2>/dev/null || stat -f %z "$tarball")" bytes
    printf '%s\t%s\t%s\n' "$prefix-binary-sha256" "$(sha256sum "$bin" | cut -d' ' -f1)" hex
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR" "$REPO_ROOT/ci-out"
    local outdir="$CI_OUT/package-size-ab"
    mkdir -p "$outdir"

    local baseline_bin="$REPO_ROOT/target/release/baseline-$PKG"
    local cand_bin="$REPO_ROOT/target/release/candidate-$PKG"

    ci_step "baseline: profile strip=none"
    if ! build_leg "$outdir/baseline-target" none; then
        ci_fail package-size-baseline "baseline release build failed; see package-size-none.log"
        return 1
    fi
    cp "$outdir/baseline-target/release/$PKG" "$baseline_bin"
    ci_step "candidate: profile strip=symbols"
    if ! build_leg "$outdir/candidate-target" symbols; then
        ci_fail package-size-candidate "candidate release build failed; see package-size-symbols.log"
        return 1
    fi
    cp "$outdir/candidate-target/release/$PKG" "$cand_bin"

    archive_leg "$baseline_bin" "$outdir/baseline.tar.gz"
    archive_leg "$cand_bin" "$outdir/candidate.tar.gz"

    local bbin btar cbin ctar
    bbin="$(stat -c %s "$baseline_bin")"
    btar="$(stat -c %s "$outdir/baseline.tar.gz")"
    cbin="$(stat -c %s "$cand_bin")"
    ctar="$(stat -c %s "$outdir/candidate.tar.gz")"

    ci_pass package-size-baseline "binary $bbin B, archive $btar B (strip=none)"
    ci_pass package-size-candidate "binary $cbin B, archive $ctar B (strip=symbols)"
    ci_pass package-size-delta "binary $(awk -v a="$bbin" -v b="$cbin" 'BEGIN{printf "%.2f", a/b}')x smaller, archive $(awk -v a="$btar" -v b="$ctar" 'BEGIN{printf "%.2f", a/b}')x smaller"

    ci_note "fixture: $FIXTURE"
    ci_note "copy the measured values into the fixture after review"
}

main() { run_all; }
main "$@"
LANE_RC=$?

set +e
ci_finalize
FINAL_RC=$?
set -e

[ "$LANE_RC" -ne 0 ] && exit "$LANE_RC"
exit "$FINAL_RC"
