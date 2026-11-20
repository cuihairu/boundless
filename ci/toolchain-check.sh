#!/usr/bin/env bash
# BW-22-01-01 — assert that local and CI resolve the same toolchain and lockfile.
#
# Usage (local and workflow, same entrypoint):
#   ci/toolchain-check.sh            # verify + report
#   ci/toolchain-check.sh --offline  # also prove the lockfile closes the build
#
# The pins live in rust-toolchain.toml and .python-version (BW-00-03-02).
# Nothing here re-pins anything; the job is to fail loudly when a checkout
# resolves something else, and to print what it resolved so the run record
# carries it. For python the hard equality check runs in CI — setup-python
# resolved the pin file there, so any mismatch means the setup step was
# dropped or the image drifted; a dev machine whose default python3 ignores
# the pin file gets both versions recorded instead.

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

    ci_step "python pin"
    PYFILE="$REPO_ROOT/.python-version"
    if [ ! -f "$PYFILE" ]; then
        ci_fail toolchain ".python-version missing — the python pin itself is absent (BW-00-03-02)"
        return
    fi
    PY_PINNED="$(tr -d '[:space:]' <"$PYFILE")"
    if [ -z "$PY_PINNED" ]; then
        ci_fail toolchain ".python-version is empty — a pin that pins nothing"
        return
    fi
    if command -v python3 >/dev/null 2>&1; then
        PY_RESOLVED="$(python3 --version 2>&1 | awk '{print $2}')"
    else
        PY_RESOLVED="<none>"
    fi
    PANEL="$PANEL python_pinned=$PY_PINNED python_resolved=$PY_RESOLVED"
    PY_MM="$(printf '%s' "$PY_RESOLVED" | cut -d. -f1-2)"
    if [ "$PY_MM" = "$PY_PINNED" ]; then
        ci_note "python resolves the pin ($PY_RESOLVED)"
    elif [ -n "${CI:-}" ]; then
        # setup-python resolved the pin file for this job, so python3 and
        # the pin disagreeing means the setup step was dropped or drifted.
        ci_fail toolchain "in CI python3 is $PY_RESOLVED but .python-version pins $PY_PINNED"
        return
    else
        ci_note "local python3 is $PY_RESOLVED, not the pin $PY_PINNED — environments that honor .python-version (CI's setup-python) resolve $PY_PINNED"
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
        # A clean checkout has an empty registry, and `--offline` against an
        # empty one fails with nothing wrong at all. Populate it from
        # Cargo.lock first — resolving against the crates index is the
        # completeness half of the check — then build with no network.
        if ! (cd "$REPO_ROOT" && cargo fetch) >"$CI_LOG_DIR/lockfile-offline.log" 2>&1; then
            ci_fail lockfile "cargo fetch could not resolve Cargo.lock (see logs/lockfile-offline.log)"
            return
        fi
        if ! (cd "$REPO_ROOT" && cargo build --workspace --offline) >>"$CI_LOG_DIR/lockfile-offline.log" 2>&1; then
            ci_fail lockfile "offline build from the committed lockfile failed (see logs/lockfile-offline.log)"
            return
        fi
        OFFLINE_NOTE="offline build resolved"
    fi

    {
        printf '{"toolchain_pinned":"%s","toolchain_resolved":"%s","rust_version_declared":"%s",' \
            "$PINNED" "$GOT" "${DECLARED:-}"
        printf '"python_pinned":"%s","python_resolved":"%s",' \
            "$PY_PINNED" "$PY_RESOLVED"
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
