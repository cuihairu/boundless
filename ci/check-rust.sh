#!/usr/bin/env bash
# BW-22-01-02 — Rust format, lint, build and test lanes, one entrypoint each.
#
# Usage:
#   ci/check-rust.sh fmt|lint|build|test|all            # whole workspace
#   ci/check-rust.sh all --packages a,b,c               # scope to packages
#   ci/check-rust.sh self-test                          # prove the lanes bite
#
# The same file drives a developer's terminal and the workflow job, so the
# lane set and the flags are here, not duplicated in YAML. A package scope
# means the lane only gates crates someone just touched (BW-22-01-05); an
# unscoped run is the full matrix.
#
# Failures are localized: clippy and rustc reports are reduced to
# `path:line:column` lists in the manifest, so a red lane says which file
# broke rather than dumping its whole transcript.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
shift 2>/dev/null || true

PACKAGES=""
SCOPE_GIVEN=0
while [ "$#" -gt 0 ]; do
    case "$1" in
    --packages) PACKAGES="$2"; SCOPE_GIVEN=1; shift 2 ;;
    *) shift ;;
    esac
done

PKG_FLAGS=()
if [ -n "$PACKAGES" ]; then
    IFS=',' read -r -a _pkgs <<<"$PACKAGES"
    for p in "${_pkgs[@]}"; do
        [ -n "$p" ] && PKG_FLAGS+=(-p "$p")
    done
fi

# Reduce cargo's report to the distinct offending `path:line:col` locations
# so a red lane names its targets. rustc writes the path on its own `-->`
# line and rustfmt prints absolute paths, so relativise before sorting.
# Errors only: a deny-lint buried under a crate of warnings must not be
# drowned out by every warning above it.
locations() {
    local report="$1"
    awk '
        /^error(\[E[0-9]+\])?:/ { err = 1; next }
        /^warning/             { err = 0; next }
        err && /--> / {
            sub(/^.*--> */, ""); sub(/[ \t]+$/, ""); print
            err = 0
        }
    ' "$report" 2>/dev/null | sed "s|^$REPO_ROOT/||; s|^\./||" | sort -u
}

fail_detail() {
    local what="$1" report="$2" where
    where="$(locations "$report" | head -20 | paste -sd, -)"
    if [ -z "$where" ]; then
        # rustfmt and cargo itself report differently.
        where="$(grep -ohE '[A-Za-z0-9_./-]+\.rs:[0-9]+(:[0-9]+)?' "$report" 2>/dev/null \
            | sed "s|^$REPO_ROOT/||; s|^\./||" | sort -u | head -20 | paste -sd, -)"
    fi
    if [ -z "$where" ]; then
        where="$(grep -E '^error' "$report" 2>/dev/null | head -3 | tr '\n' ' ' | cut -c1-180)"
    fi
    printf '%s: %s (log %s)' "$what" "${where:-<no location>}" "${report#"$CI_OUT"/}"
}

run_lane() {
    local name="$1" lane report
    shift
    lane="rust-$name"
    report="$CI_LOG_DIR/$lane.log"
    ci_step "$name"
    ci_pipe "$lane" "$@"
    if [ "${CI_RC:-1}" -eq 0 ]; then
        ci_pass "$lane" "ok"
    else
        ci_fail "$lane" "$(fail_detail "$name" "$report")"
        return 1
    fi
    return 0
}

# Prove the lanes bite: plant one defect in a throwaway crate per lane and
# check that each lane reports FAIL, then `cargo fmt` the same crate away and
# check it reports PASS. The sandbox is a separate workspace, so the tree the
# caller is in is never touched.
self_test() {
    local sandbox
    sandbox="$(mktemp -d)"
    mkdir -p "$sandbox/crate/src" "$sandbox/crate/tests"
    cat >"$sandbox/crate/Cargo.toml" <<'EOF'
[package]
name = "ci-sandbox"
version = "0.1.0"
edition = "2021"

[lib]
name = "ci_sandbox"
path = "src/lib.rs"

[[test]]
name = "bait"
path = "tests/bait.rs"

[workspace]
EOF
    # lib bait: an unformatted fn (fmt lane), a type error (lint/build lanes),
    # and a pub helper the failing test calls (test lane).
    cat >"$sandbox/crate/src/lib.rs" <<'EOF'
pub fn fmt_bait(x:u32)->u32{x}

pub fn broken() -> u32 {
    let x: u32 = "text";
    x
}

pub fn bait() -> u32 {
    1
}
EOF
    cat >"$sandbox/crate/tests/bait.rs" <<'EOF'
#[test]
fn test_lane_defect_is_seen() {
    assert_eq!(ci_sandbox::bait(), 2, "deliberate test failure for the self-test");
}
EOF

    local fails=0
    local lane cmd
    for spec in "fmt:cargo fmt --check" "lint:cargo clippy --all-targets -- -D warnings" \
        "build:cargo build --all-targets" "test:cargo test --no-fail-fast"; do
        lane="${spec%%:*}"
        cmd="${spec#*:}"
        if (cd "$sandbox/crate" && $cmd >/dev/null 2>&1); then
            ci_fail "selftest-$lane" "lane reported success on a crate that is $lane-broken"
            fails=$((fails + 1))
        else
            ci_pass "selftest-$lane" "detects the $lane defect"
        fi
    done

    # And the same crate, healed, must go green on fmt — proves the lane is
    # not red for want of nothing.
    (cd "$sandbox/crate" && cargo fmt) >/dev/null 2>&1 || true
    if (cd "$sandbox/crate" && cargo fmt --check >/dev/null 2>&1); then
        ci_pass selftest-clean "formatted crate passes fmt"
    else
        ci_fail selftest-clean "fmt --check rejected a formatted crate"
        fails=$((fails + 1))
    fi

    rm -rf "$sandbox"
    if [ "$fails" -gt 0 ]; then
        ci_note "$fails self-test(s) did not behave"
        return 1
    fi
    ci_note "all self-tests behaved as expected"
}

# A supplied scope gates just those packages. An explicitly *empty* scope
# means "no Rust changed here" — the lanes record SKIP rather than pretending
# to have gated nothing. No flag at all means the full workspace.
main() {
    if [ -n "$PACKAGES" ] || [ "${SCOPE_GIVEN:-0}" -eq 1 ]; then
        if [ -z "$PACKAGES" ]; then
            mkdir -p "$CI_OUT" "$CI_LOG_DIR"
            ci_skip rust-fmt "no Rust packages in this change's scope"
            ci_skip rust-lint "no Rust packages in this change's scope"
            ci_skip rust-build "no Rust packages in this change's scope"
            ci_skip rust-test "no Rust packages in this change's scope"
            return 0
        fi
        ci_note "package scope: $PACKAGES"
    fi
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    case "$MODE" in
    fmt) run_lane fmt cargo fmt --check "${PKG_FLAGS[@]+"${PKG_FLAGS[@]}"}" ;;
    lint) run_lane lint cargo clippy --locked --all-targets "${PKG_FLAGS[@]+"${PKG_FLAGS[@]}"}" -- -D warnings ;;
    build) run_lane build cargo build --locked --all-targets "${PKG_FLAGS[@]+"${PKG_FLAGS[@]}"}" ;;
    test) run_lane test cargo test --locked --no-fail-fast "${PKG_FLAGS[@]+"${PKG_FLAGS[@]}"}" ;;
    all)
        run_lane fmt cargo fmt --check "${PKG_FLAGS[@]+"${PKG_FLAGS[@]}"}" || true
        run_lane lint cargo clippy --locked --all-targets "${PKG_FLAGS[@]+"${PKG_FLAGS[@]}"}" -- -D warnings || true
        run_lane build cargo build --locked --all-targets "${PKG_FLAGS[@]+"${PKG_FLAGS[@]}"}" || true
        run_lane test cargo test --locked --no-fail-fast "${PKG_FLAGS[@]+"${PKG_FLAGS[@]}"}"
        ;;
    self-test) self_test ;;
    *)
        ci_fail rust "unknown mode $MODE (want fmt|lint|build|test|all|self-test)"
        return 1
        ;;
    esac
}

main
LANE_RC=$?

set +e
ci_finalize
FINAL_RC=$?
set -e

[ "$LANE_RC" -ne 0 ] && exit "$LANE_RC"
exit "$FINAL_RC"
