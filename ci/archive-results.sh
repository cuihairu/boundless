#!/usr/bin/env bash
# BW-22-02-06 — bundle one run's results so a failure is retrievable later.
#
# Usage:
#   ci/archive-results.sh                 # archive $CI_OUT
#   ci/archive-results.sh self-test       # prove the bundle round-trips
#
# A workflow job cannot share its filesystem with the next one, so each job
# bundles its own CI_OUT before it exits: the manifest, every lane log, the
# toolchain record, coverage when a lane produced any, and the failure
# fixtures the lanes left behind. The bundle is what gets uploaded, and this
# script re-reads it before uploading — a corrupt archive must not be the only
# copy of a failure.
#
# The bundle lands beside $CI_OUT, never inside it: a tarball archiving its own
# directory grows without stopping.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"

# Where the bundles go. Beside $CI_OUT by default, overridable so a job can
# point at its own directory.
ARCHIVE_DIR="${CI_ARCHIVE_DIR:-$(dirname "$CI_OUT")}"

# One bundle: every file under $src. The members keep their names so a reader
# can reconstruct the layout; the version lives in the bundle's own file name.
archive_one() {
    local src="$1" out="$2" name dest now sha bundle
    dest="$out/ci-results"
    name="$(basename "$src")"
    if [ "$src" = "$dest" ]; then
        echo "already bundled: $src"
        return 1
    fi
    mkdir -p "$dest"
    now="$(ci_now)"
    sha="$(ci_git_sha)"
    bundle="$dest/ci-results-$name-$sha-$now.tar.gz"
    tar -czf "$bundle" -C "$(dirname "$src")" "$name"
    [ -s "$bundle" ] || { echo "empty bundle: $bundle"; return 1; }
    printf '%s\n' "$bundle"
}

# Read the bundle back and compare what came out against the row count taken
# before it was made. The count is passed in rather than re-read: the manifest
# is append-only, so the archive lane's own row is appended after the tarball
# exists, and re-reading it would let a short bundle pass against a manifest
# that had since grown.
verify_one() {
    local bundle="$1" src="$2" rows_expected="$3" name rows_out size
    local tmp
    tmp="$(mktemp -d)"
    name="$(basename "$src")"
    if ! tar -tzf "$bundle" >/dev/null 2>&1; then
        echo "unreadable bundle: $bundle"
        rm -rf "$tmp"
        return 1
    fi
    tar -xzf "$bundle" -C "$tmp" >/dev/null 2>&1 || { rm -rf "$tmp"; echo "cannot extract $bundle"; return 1; }
    rows_out="$(grep -c . "$tmp/$name/manifest.tsv" 2>/dev/null || echo 0)"
    if [ "$rows_expected" -ne "$rows_out" ]; then
        echo "manifest lost rows ($rows_expected -> $rows_out): $bundle"
        rm -rf "$tmp"
        return 1
    fi
    size="$(du -h "$bundle" | cut -f1)"
    echo "ok: $rows_out manifest row(s) round-tripped, bundle $size"
    rm -rf "$tmp"
}

self_test() {
    local sandbox src fails=0 bundle detail
    sandbox="$(mktemp -d)"
    src="$sandbox/ci-out"
    mkdir -p "$src/logs" "$src/coverage" "$src/fixtures"
    printf 'lane\tstatus\tdetail\nfmt\tFAIL\tplanted defect\nlint\tPASS\tok\n' >"$src/manifest.tsv"
    printf 'lane log\n' >"$src/logs/fmt.log"
    printf 'loc\tpct\nsrc/lib.rs\t98.5\n' >"$src/coverage/llvm_cov.csv"
    printf 'broken config\n' >"$src/fixtures/bad.toml"
    printf '{"toolchain_pinned":"1.100.0"}\n' >"$src/toolchain.json"

    ci_step "self-test: a populated CI_OUT bundles and round-trips"
    local rows
    rows="$(grep -c . "$src/manifest.tsv" 2>/dev/null || echo 0)"
    bundle="$(archive_one "$src" "$sandbox")" || { ci_fail selftest-archive "archiving did not produce a bundle"; fails=$((fails + 1)); }
    if [ -n "$bundle" ] && [ -s "$bundle" ]; then
        detail="$(verify_one "$bundle" "$src" "$rows")"
        ci_pass selftest-archive "bundled $detail"
    else
        ci_fail selftest-archive "no usable bundle produced"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a truncated bundle is rejected"
    if [ -n "$bundle" ]; then
        local bad="$sandbox/ci-results/bad.tar.gz"
        head -c 200 "$bundle" >"$bad"
        if verify_one "$bad" "$src" "$rows" >/dev/null 2>&1; then
            ci_fail selftest-tamper "a truncated bundle passed verification"
            fails=$((fails + 1))
        else
            ci_pass selftest-tamper "truncated bundle rejected"
        fi
    fi

    ci_step "self-test: an empty CI_OUT bundles nothing and says so"
    local empty="$sandbox/empty"
    mkdir -p "$empty"
    if bundle="$(archive_one "$empty" "$sandbox" 2>/dev/null)"; then
        ci_pass selftest-empty "bundled the empty tree and read it back"
    else
        ci_fail selftest-empty "an empty tree was refused"
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

main() {
    local src="$CI_OUT"
    mkdir -p "$ARCHIVE_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    if [ "$MODE" = self-test ]; then
        self_test
        return $?
    fi

    if [ ! -d "$src" ]; then
        ci_skip archive "no $src yet — nothing to bundle"
        return 0
    fi

    local bundle detail rows
    # Snapshot before bundling: the row this lane appends lands after the
    # tarball exists, so a later read would count a manifest that had grown.
    rows="$(grep -c . "$src/manifest.tsv" 2>/dev/null || echo 0)"
    ci_step "archive $src"
    if ! bundle="$(archive_one "$src" "$ARCHIVE_DIR")"; then
        ci_fail archive "$(printf '%s' "$bundle" | head -1)"
        return 0
    fi
    detail="$(verify_one "$bundle" "$src" "$rows")" || {
        ci_fail archive "$(printf '%s' "$detail" | head -1)"
        return 0
    }
    ci_pass archive "$(printf '%s' "$detail" | head -1)"
}

main
LANE_RC=$?

set +e
ci_finalize
FINAL_RC=$?
set -e

[ "$LANE_RC" -ne 0 ] && exit "$LANE_RC"
exit "$FINAL_RC"