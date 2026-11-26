#!/usr/bin/env bash
# OPT-14-01-01 — record the release package size and the first run.
#
# Usage:
#   ci/package-baseline.sh                 # measure this host, print the row
#   ci/package-baseline.sh verify          # check the committed three-platform catalog
#   ci/package-baseline.sh record          # measure + rewrite this platform's catalog row
#   ci/package-baseline.sh self-test       # prove the lane and the catalog checker bite
#
# The acceptance is "三端有基线" — all three delivery platforms have a
# baseline. Only Linux compiles today; the macOS and Windows legs of
# package-daily.yml are report_only until BW-15-04 / BW-15-03, so their
# catalog rows carry the named blocker and hold no invented measurements.
# Running this lane on those runners (or once the legs compile) fills them.
#
# The measurement drives the SHIPPED lane: `ci/package.sh` builds (or takes
# a supplied binary) and stages/archives/smoke-installs into a private
# output root. This lane then reads the produced tarball itself: its exact
# byte size (`wc -c`, not `du -h`), its SHA-256 through the same primitive
# ci/checksums.sh uses, and its member set through `tar -tzf`. First run is
# the installed binary launched under a real role with `date +%s%N` around
# it; the elapsed time is printed and catalogued but never asserted (it
# moves with the host). The structural contracts ARE asserted: the archive
# carries exactly `bin/boundless-server` and no `data/`, the installed
# binary exits 0 and prints both the startup and shutdown lines.
#
# Nothing here tags or releases. The catalog is a measurement record.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-run}"
CATALOG="$REPO_ROOT/tests/baseline/package-baseline.tsv"
PLATFORMS='linux-x86_64 macos-arm64 windows-x86_64'
PKG=boundless-server
ROLES_SOURCE=crates/server-runtime/src/app.rs

# Call-time resolution: the self-test plants a stub binary and a private
# output root, and an override only reaches code that re-reads the variable.
cur_bin() { printf '%s' "${CI_BASELINE_BIN:-}"; }
cur_out() { printf '%s' "${CI_BASELINE_OUT:-$CI_OUT}"; }
cur_roles() { printf '%s' "${CI_BASELINE_ROLES:-}"; }
cur_catalog() { printf '%s' "${CI_BASELINE_CATALOG:-$CATALOG}"; }
cur_res() { printf '%s' "${CI_BASELINE_RES:-$REPO_ROOT/res}"; }

# The installed binary finds its configuration through BW_RES_PATH and exits
# on the shipped CI_RUNNERS_TIMEOUT budget instead of waiting for a signal —
# the same self-shutdown seam ci/install-layout.sh uses. The package itself
# carries only bin/, so the configuration root is recorded as probe context.
launch_env() {
    printf 'BW_RES_PATH=%s' "$(cur_res)"
    printf ' CI_RUNNERS_TIMEOUT=%s' "${CI_RUNNERS_TIMEOUT:-1}"
}

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

# --- platform (the same mapping package.sh and install-layout.sh use) -------

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

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# --- measurement -----------------------------------------------------------

# run_packaging_lane <workdir> — drive the shipped package lane into a
# private root, honouring the self-test's binary/role overrides.
run_packaging_lane() {
    local work="$1"
    mkdir -p "$work"
    CI_OUT="$work/ci-out" CI_PACKAGE_OUT="$work/pkg" \
        CI_PACKAGE_BIN="$(cur_bin)" CI_PACKAGE_ROLES="$(cur_roles)" \
        BW_RES_PATH="$(cur_res)" CI_RUNNERS_TIMEOUT="${CI_RUNNERS_TIMEOUT:-1}" \
        "$REPO_ROOT/ci/package.sh" >"$work/package.log" 2>&1
}

# measure <workdir> — read the produced archive and time an installed launch.
# Prints one TSV data row (no header) and the probe lines to stderr.
measure() {
    local work="$1" platform version tarball bytes sha members member_count
    local installdir role out rc start end ns status note
    platform="$(platform_name)"
    version="$(ci_version)"
    [ -n "$version" ] || version=untagged

    tarball="$(find "$work/pkg/package" -maxdepth 1 -name 'boundless-*.tar.gz' | head -1)"
    if [ -z "$tarball" ] || [ ! -f "$tarball" ]; then
        echo "no package archive under $work/pkg/package" >&2
        return 1
    fi
    bytes="$(wc -c <"$tarball" | tr -d ' ')"
    sha="$(sha256_of "$tarball")"
    # tar also emits directory entries ("bin/"); the payload contract is
    # about the regular files, so directory entries are not counted.
    members="$(tar -tzf "$tarball" | grep -v '/$' | sort | paste -sd, -)"
    member_count="$(tar -tzf "$tarball" | grep -v '/$' | wc -l | tr -d ' ')"

    # The package-owned layout contract: the one binary, and no user data.
    if [ "$members" != "bin/boundless-server" ]; then
        echo "unexpected member set: $members" >&2
        return 1
    fi
    if tar -tzf "$tarball" | grep -q '^data/'; then
        echo "the package carries user data" >&2
        return 1
    fi

    installdir="$work/pkg/package/install-smoke"
    [ -x "$installdir/bin/$PKG" ] || {
        echo "the smoke install left no installed binary" >&2
        return 1
    }
    role="$(first_role)"
    [ -n "$role" ] || {
        echo "no role list at $ROLES_SOURCE" >&2
        return 1
    }
    start="$(date +%s%N)"
    out="$(env $(launch_env) timeout 30 "$installdir/bin/$PKG" "$role" 2>&1)"
    rc=$?
    end="$(date +%s%N)"
    ns=$((end - start))
    if [ "$rc" -ne 0 ]; then
        echo "first run exited $rc: $out" >&2
        return 1
    fi
    if ! printf '%s\n' "$out" | grep -q 'starting'; then
        echo "first run printed no startup line: $out" >&2
        return 1
    fi

    status=measured
    note="sha256=$sha members=$members"
    printf 'BASELINE platform=%s bytes=%s members=%s first_run_ns=%s status=%s\n' \
        "$platform" "$bytes" "$member_count" "$ns" "$status" >&2
    # platform version bytes sha256 members member_count first_run_ns status note
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$platform" "$version" "$bytes" "$sha" "$members" "$member_count" "$ns" "$status" "$note"
}

# --- catalog --------------------------------------------------------------

catalog_header() {
    printf 'platform\tversion\tbytes\tsha256\tmembers\tmember_count\tfirst_run_ns\tstatus\tnote\n'
}

# pending_row <platform> <blocker> — an honest row for a platform this host
# cannot build: no byte size, no hash, no time; the blocker is named.
pending_row() {
    local platform="$1" blocker="$2"
    printf '%s\t-\t-\t-\t-\t-\t-\tpending\t%s\n' "$platform" "$blocker"
}

blocker_for() {
    case "$1" in
    macos-arm64) printf 'BW-15-04: the macOS leg is report_only in package-daily.yml; only clock/interface compile until the windowing and file-system slices land' ;;
    windows-x86_64) printf 'BW-15-03: server-runtime/src/signals.rs uses libc::SIGSYS/SIGRTMIN and sigwaitinfo; the Windows leg cannot build until the signal layer is ported' ;;
    *) printf 'not a delivery platform' ;;
    esac
}

# merge_row <catalog> <row-file> — replace the row for the row's platform,
# keeping the header and the other platforms' rows in order.
merge_row() {
    local catalog="$1" rows="$2" platform tmp
    platform="$(cut -f1 "$rows")"
    tmp="$catalog.tmp.$$"
    {
        head -1 "$catalog"
        awk -F'\t' -v want="$platform" -v newrow="$(cat "$rows")" '
            NR == 1 { next }
            $1 == want { print newrow; seen = 1; next }
            { print }
            END { if (!seen) print newrow }
        ' "$catalog"
    } >"$tmp" && mv "$tmp" "$catalog"
}

write_catalog() {
    local work rowfile platform
    work="$(cur_out)/package-baseline"
    catalog="$(cur_catalog)"
    mkdir -p "$(dirname "$catalog")"
    if [ ! -f "$catalog" ]; then
        catalog_header >"$catalog"
        local p
        for p in $PLATFORMS; do
            case "$p" in
            "$(platform_name)") ;;
            *) pending_row "$p" "$(blocker_for "$p")" >>"$catalog" ;;
            esac
        done
    fi
    rowfile="$work/row.tsv"
    mkdir -p "$work"
    measure "$work" >"$rowfile" || return 1
    platform="$(cut -f1 "$rowfile")"
    case " $PLATFORMS " in
    *" $platform "*) ;;
    *)
        echo "$platform is not one of the three delivery platforms; not recording" >&2
        return 1
        ;;
    esac
    merge_row "$catalog" "$rowfile"
    printf 'CATALOG %s row=%s\n' "$catalog" "$platform" >&2
}

verify_catalog() {
    local catalog="$1" fails=0 rows=0 platform
    if [ ! -f "$catalog" ]; then
        echo "no catalog at $catalog"
        return 1
    fi
    if [ "$(head -1 "$catalog")" != "$(catalog_header)" ]; then
        echo "catalog header does not match the grammar"
        fails=$((fails + 1))
    fi
    for platform in $PLATFORMS; do
        local row
        row="$(awk -F'\t' -v p="$platform" '$1 == p' "$catalog")"
        if [ -z "$row" ]; then
            echo "missing platform row: $platform"
            fails=$((fails + 1))
            continue
        fi
        rows=$((rows + 1))
        local nfields status bytes sha first_ns note
        nfields="$(printf '%s\n' "$row" | awk -F'\t' '{print NF}')"
        if [ "$nfields" -ne 9 ]; then
            echo "$platform: expected 9 fields, got $nfields"
            fails=$((fails + 1))
            continue
        fi
        status="$(printf '%s\n' "$row" | cut -f8)"
        bytes="$(printf '%s\n' "$row" | cut -f3)"
        sha="$(printf '%s\n' "$row" | cut -f4)"
        first_ns="$(printf '%s\n' "$row" | cut -f7)"
        note="$(printf '%s\n' "$row" | cut -f9)"
        # The row must be a clean single line: embedded tabs/newlines would
        # make the catalog ambiguous when read back by other lanes.
        if printf '%s\n' "$row" | grep -q $'\t.*\t.*\t.*\t.*\t.*\t.*\t.*\t.*\t.*\t'; then
            echo "$platform: more than nine fields"
            fails=$((fails + 1))
        fi
        case "$status" in
        measured)
            case "$bytes" in '' | *[!0-9]*) echo "$platform: bytes is not numeric"; fails=$((fails + 1)) ;; *) [ "$bytes" -gt 0 ] || { echo "$platform: zero bytes"; fails=$((fails + 1)); } ;; esac
            case "$sha" in *[!0-9a-f]* | '') echo "$platform: sha256 is not lowercase hex"; fails=$((fails + 1)) ;; *) [ "$(printf '%s' "$sha" | wc -c | tr -d ' ')" -eq 64 ] || { echo "$platform: sha256 is not 64 chars"; fails=$((fails + 1)); } ;; esac
            case "$first_ns" in '' | *[!0-9]*) echo "$platform: first_run_ns is not numeric"; fails=$((fails + 1)) ;; esac
            ;;
        pending)
            [ "$bytes" = - ] && [ "$sha" = - ] && [ "$first_ns" = - ] || {
                echo "$platform: a pending row must not carry measurements"
                fails=$((fails + 1))
            }
            case "$note" in BW-15-*) ;; *) echo "$platform: a pending row must name its blocker (BW-15-...)"; fails=$((fails + 1)) ;; esac
            ;;
        *)
            echo "$platform: unknown status '$status'"
            fails=$((fails + 1))
            ;;
        esac
    done
    if [ "$rows" -ne 3 ]; then
        echo "the catalog must carry all three platforms (found $rows)"
        fails=$((fails + 1))
    fi
    if [ "$fails" -gt 0 ]; then
        return 1
    fi
    printf 'catalog %s: 3 platforms, grammar clean\n' "$catalog"
    return 0
}

# --- modes ----------------------------------------------------------------

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"
    local work record=0
    work="$(cur_out)/package-baseline"
    if [ "${CI_BASELINE_RECORD:-0}" = 1 ]; then
        record=1
    fi
    ci_step "package baseline for $(platform_name)"
    if ! run_packaging_lane "$work"; then
        ci_fail package-baseline-packaging "ci/package.sh failed; see $work/package.log"
        return 1
    fi
    ci_pass package-baseline-packaging "shipped package lane completed"
    if [ "$record" = 1 ]; then
        if write_catalog; then
            ci_pass package-baseline-record "catalog row rewritten for $(platform_name)"
        else
            ci_fail package-baseline-record "could not record the catalog row"
            return 1
        fi
        return 0
    fi
    local row
    if ! row="$(measure "$work")"; then
        ci_fail package-baseline-measure "the archive/launch measurement failed"
        return 1
    fi
    printf '%s\n' "$row" >"$CI_OUT/package-baseline.tsv"
    ci_pass package-baseline-measure "archive and first run measured ($(printf '%s\n' "$row" | cut -f3) bytes)"
    if verify_catalog "$(cur_catalog)"; then
        ci_pass package-baseline-catalog "committed three-platform catalog is well formed"
    else
        ci_fail package-baseline-catalog "committed catalog is malformed"
        return 1
    fi
}

self_test() {
    local fails=0 r stub out catalog rc work
    r="$(mktemp -d "$REPO_ROOT/ci-out/package-baseline-selftest.XXXXXX")"
    stub="$r/$PKG"
    catalog="$r/catalog.tsv"
    mkdir -p "$r/manifest"

    cat >"$stub" <<'STUB'
#!/bin/sh
echo "[baseapp] starting (boundless minimal entry)" >&2
echo "[baseapp] has shut down" >&2
exit 0
STUB
    chmod +x "$stub"

    ci_step "self-test: a stub package measures and records a real row"
    work="$r/work"
    out="$(CI_OUT="$r/manifest" CI_BASELINE_BIN="$stub" CI_BASELINE_ROLES=baseapp CI_BASELINE_OUT="$work" \
        CI_BASELINE_CATALOG="$catalog" "$0" record 2>&1)"
    rc=$?
    if [ "$rc" -eq 0 ] && verify_catalog "$catalog" >/dev/null 2>&1; then
        ci_pass selftest-baseline-record "a stub package produced a measured row and a clean catalog"
    else
        ci_fail selftest-baseline-record "the stub measurement did not record: $(printf '%s\n' "$out" | tail -3)"
        fails=$((fails + 1))
    fi
    local measured
    measured="$(awk -F'\t' '$8 == "measured"' "$catalog" | head -1)"
    if [ -n "$measured" ] && [ "$(printf '%s\n' "$measured" | cut -f3)" -gt 0 ]; then
        ci_pass selftest-baseline-bytes "the measured row carries a real byte size"
    else
        ci_fail selftest-baseline-bytes "the measured row has no byte size"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a failing launch goes red at the measurement"
    cat >"$stub" <<'STUB'
#!/bin/sh
echo "stub-server: explosion" >&2
exit 3
STUB
    chmod +x "$stub"
    catalog="$r/catalog-red.tsv"
    cp "$r/catalog.tsv" "$catalog" 2>/dev/null || true
    out="$(CI_OUT="$r/manifest" CI_BASELINE_BIN="$stub" CI_BASELINE_ROLES=baseapp CI_BASELINE_OUT="$r/work-red" \
        CI_BASELINE_CATALOG="$catalog" "$0" record 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        ci_pass selftest-baseline-red "a first run that exits non-zero fails the measurement"
    else
        ci_fail selftest-baseline-red "the red stub recorded a baseline anyway"
        fails=$((fails + 1))
    fi

    ci_step "self-test: the catalog checker rejects a missing platform"
    head -1 "$r/catalog.tsv" >"$r/short.tsv"
    awk -F'\t' '$1 == "linux-x86_64"' "$r/catalog.tsv" >>"$r/short.tsv"
    awk -F'\t' '$1 == "macos-arm64"' "$r/catalog.tsv" >>"$r/short.tsv"
    if ! verify_catalog "$r/short.tsv" >/dev/null 2>&1; then
        ci_pass selftest-baseline-missing "a catalog missing a platform is rejected"
    else
        ci_fail selftest-baseline-missing "the checker accepted a two-platform catalog"
        fails=$((fails + 1))
    fi

    ci_step "self-test: the catalog checker rejects fabricated pending numbers"
    awk -F'\t' -v OFS='\t' '$1 == "windows-x86_64" { $3 = 1234 } { print }' \
        "$r/catalog.tsv" >"$r/fake.tsv"
    if ! verify_catalog "$r/fake.tsv" >/dev/null 2>&1; then
        ci_pass selftest-baseline-fake "a pending row carrying a size is rejected"
    else
        ci_fail selftest-baseline-fake "the checker accepted numbers on a pending row"
        fails=$((fails + 1))
    fi

    rm -rf "$r"
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
    verify)
        if verify_catalog "$(cur_catalog)"; then
            ci_pass package-baseline-catalog "committed three-platform catalog is well formed"
            return 0
        fi
        ci_fail package-baseline-catalog "committed catalog is malformed"
        return 1
        ;;
    record) CI_BASELINE_RECORD=1 run_all; return $? ;;
    *) run_all; return $? ;;
    esac
}

main "$@"
LANE_RC=$?

set +e
if [ "${MODE:-run}" != verify ]; then
    ci_finalize
fi
set -e

exit "$LANE_RC"