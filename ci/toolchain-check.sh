#!/usr/bin/env bash
# BW-22-01-01 — assert that local and CI resolve the same toolchain and lockfile.
#
# Usage (local and workflow, same entrypoint):
#   ci/toolchain-check.sh            # verify + report
#   ci/toolchain-check.sh --offline  # also prove the lockfile closes the build
#
# The pin lives in rust-toolchain.toml (BW-00-03-02). Nothing here re-pins
# anything; the job is to fail loudly when a checkout resolves something
# else, and to print what it resolved so the run record carries it.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

OFFLINE=0
[ "${1:-}" = --offline ] && OFFLINE=1

PANEL=""
TOOLCHAIN_TOML="$REPO_ROOT/rust-toolchain.toml"

main() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    ci_step "resolve toolchain pin"
    if [ ! -f "$TOOLCHAIN_TOML" ]; then
        ci_fail toolchain "rust-toolchain.toml missing — the pin itself is absent"
        return
    fi
    PINNED="$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$TOOLCHAIN_TOML" | head -1)"
    if [ -z "$PINNED" ]; then
        ci_fail toolchain "no [toolchain] channel in $TOOLCHAIN_TOML"
        return
    fi

    # The manifest also carries the declarative minimum so `cargo` refuses a
    # too-old toolchain even outside rustup.
    DECLARED="$(sed -n 's/^rust-version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$REPO_ROOT/Cargo.toml" | head -1)"

    PANEL="pinned=$PINNED declared_rust-version=${DECLARED:-<none>}"
    ci_note "$PANEL"

    # rustup reads rust-toolchain.toml from the working directory, so invoke
    # from the repo root and read the version rustc actually reports.
    (
        cd "$REPO_ROOT"
        rustc --version || exit 1
        cargo --version || exit 1
    ) | tee "$CI_LOG_DIR/toolchain-versions.log"

    GOT="$(cd "$REPO_ROOT" && rustc --version | awk '{print $2}')"
    PANEL="$PANEL resolved=$GOT"

    if [ "$GOT" != "$PINNED" ]; then
        ci_fail toolchain "resolved $GOT but rust-toolchain.toml pins $PINNED ($PANEL)"
        return
    fi
    if [ -n "$DECLARED" ] && [ "$DECLARED" != "$PINNED" ]; then
        ci_fail toolchain "workspace.package rust-version $DECLARED != toolchain pin $PINNED"
        return
    fi

    ci_step "lockfile"
    if git -C "$REPO_ROOT" check-ignore -q Cargo.lock; then
        ci_fail lockfile "Cargo.lock is git-ignored — CI cannot reproduce the same graph"
        return
    fi
    if ! git -C "$REPO_ROOT" ls-files --error-unmatch Cargo.lock >/dev/null 2>&1; then
        ci_fail lockfile "Cargo.lock is not tracked in git"
        return
    fi

    # --locked fails when any Cargo.toml is out of step with the committed
    # graph, so this is the lockfile consistency check itself.
    if ! METALOCK="$(cd "$REPO_ROOT" && cargo metadata --locked --no-deps --format-version 1 2>&1)"; then
        ci_fail lockfile "cargo metadata --locked failed: $(printf '%s' "$METALOCK" | tail -2 | tr '\n' ' ')"
        return
    fi
    MEMBERS="$(printf '%s' "$METALOCK" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["packages"]))' 2>/dev/null || echo '?')"

    OFFLINE_NOTE="offline not requested"
    if [ "$OFFLINE" -eq 1 ]; then
        ci_step "offline resolution"
        if ! (cd "$REPO_ROOT" && cargo build --workspace --offline) >"$CI_LOG_DIR/lockfile-offline.log" 2>&1; then
            ci_fail lockfile "offline build from the committed lockfile failed (see logs/lockfile-offline.log)"
            return
        fi
        OFFLINE_NOTE="offline build resolved"
    fi

    {
        printf '{"toolchain_pinned":"%s","toolchain_resolved":"%s","rust_version_declared":"%s",' \
            "$PINNED" "$GOT" "${DECLARED:-}"
        printf '"members":%s,"cargo_lock_tracked":true,"offline":"%s","checked_at":"%s","git_sha":"%s"}\n' \
            "${MEMBERS:-null}" "$OFFLINE_NOTE" "$(ci_now)" "$(ci_git_sha)"
    } >"$CI_OUT/toolchain.json"

    ci_pass toolchain "$PANEL; members=$MEMBERS; $OFFLINE_NOTE"
    ci_note "record: $CI_OUT/toolchain.json"
}

main
LANE_RC=$?

set +e
ci_finalize
FINAL_RC=$?
set -e

[ "$LANE_RC" -ne 0 ] && exit "$LANE_RC"
exit "$FINAL_RC"
