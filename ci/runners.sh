#!/usr/bin/env bash
# BW-22-02-01 — start the server on this runner and keep the record.
#
# Usage:
#   ci/runners.sh                 # build and start every server role here
#   ci/runners.sh self-test       # prove the lane goes red on a bad role
#
# One lane per operating system. A three-leg matrix in the workflow is three
# calls of this same entrypoint, so what the record says about Linux, Windows
# and macOS is produced by one code path rather than three copies.
#
# The record is the acceptance: every reference role must come up and say so,
# and an unknown role must be refused. Each leg writes its own
# runners/<os>-<arch>.tsv plus a machine-readable summary, so a leg that
# cannot build yet says so in its record instead of being averaged away.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"

# Override the binary and the role list so the self-test can drive this lane
# with a stub instead of a workspace build.
BIN="${CI_RUNNERS_BIN:-}"
ROLES_OVERRIDE="${CI_RUNNERS_ROLES:-}"

# The role set lives in the crate; read it out of the source so adding a role
# there turns the lane on automatically.
ROLES_SOURCE="$REPO_ROOT/crates/server-runtime/src/app.rs"

# A record is one line per role. Kept as TSV with the same separator as the
# manifest so a reader can diff the two.
tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

os_name() {
    case "$(uname -s)" in
        Linux*) echo linux ;;
        Darwin*) echo macos ;;
        MINGW* | MSYS* | CYGWIN*) echo windows ;;
        *) echo "$(uname -s | tr '[:upper:]' '[:lower:]' | tr -c 'a-z0-9' '-')" ;;
    esac
}

detect_target() {
    local os arch
    os="$(os_name)"
    arch="$(uname -m)"
    case "$arch" in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=arm64 ;;
    esac
    printf '%s-%s' "$os" "$arch"
}

source_roles() {
    [ -f "$ROLES_SOURCE" ] || return 1
    sed -n '/pub const KNOWN_ROLES/,/\];/p' "$ROLES_SOURCE" \
        | grep -oE '"[A-Za-z0-9_-]+"' | tr -d '"'
}

roles() {
    # Read the environment each call, not the value captured at script start:
    # a self-test overrides the list after this script has begun.
    local ov="${CI_RUNNERS_ROLES:-$ROLES_OVERRIDE}"
    if [ -n "$ov" ]; then
        # shellcheck disable=SC2086
        printf '%s\n' $ov
    else
        source_roles
    fi
}

record_path() {
    printf '%s/runners/%s.tsv' "$CI_OUT" "$1"
}

# Start one role and return 0 when it came up. $2 is the record line prefix.
start_role() {
    local role="$1" rc out
    out="$(CI_RUNNERS_TIMEOUT="${CI_RUNNERS_TIMEOUT:-20}" "$BIN" "$role" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        printf '%s\tFAIL\t%s\t%s\n' "$role" "$rc" "$(tsv_escape "$out")"
        return 1
    fi
    printf '%s\tok\t%s\t%s\n' "$role" "$rc" "$(tsv_escape "$out")"
}

write_record() {
    local target="$1"
    # Resolved here rather than at script start so a self-test stub is honoured.
    BIN="${CI_RUNNERS_BIN:-$BIN}"
    local rec summary fails=0
    rec="$(record_path "$target")"
    summary="$CI_OUT/runners/$target.json"
    mkdir -p "$(dirname "$rec")"
    local rustc_ver toolchain
    rustc_ver="$(rustc --version 2>/dev/null || echo unavailable)"
    toolchain="$(rustc --version 2>/dev/null | awk '{print $2}')"
    {
        printf 'os\t%s\n' "$(os_name)"
        printf 'arch\t%s\n' "$(uname -m)"
        printf 'rustc\t%s\n' "$rustc_ver"
        printf 'toolchain_resolved\t%s\n' "$toolchain"
        printf 'git_sha\t%s\n' "$(ci_git_sha)"
        printf 'version\t%s\n' "$(ci_version)"
        printf 'binary\t%s\n' "$BIN"
        printf 'role\tresult\texit\tstartup_line\n'
    } >"$rec"
    cat "$rec" >>"$CI_LOG_DIR/runners-$target.log"

    local role line ok=0 total=0 failed=""
    while IFS= read -r role; do
        [ -n "$role" ] || continue
        total=$((total + 1))
        if line="$(start_role "$role")"; then
            ok=$((ok + 1))
        else
            fails=$((fails + 1))
            failed="$failed $role"
        fi
        printf '%s\n' "$line" >>"$rec"
    done < <(roles)

    local unknown_refused=no out
    if out="$(CI_RUNNERS_TIMEOUT=20 "$BIN" not-a-real-role 2>&1)"; then
        unknown_refused="unknown-role-was-accepted"
    elif printf '%s' "$out" | grep -q 'unknown server role'; then
        unknown_refused=yes
    else
        unknown_refused="refused-without-directions"
    fi
    printf 'unknown-role\t%s\t%s\t%s\n' \
        "$unknown_refused" \
        "$([ "$unknown_refused" = yes ] && printf 0 || printf 1)" \
        "an unknown role is refused with the valid set" >>"$rec"
    cat "$rec" >>"$CI_LOG_DIR/runners-$target.log"

    printf '{"os":"%s","arch":"%s","rustc":"%s","git_sha":"%s","roles_total":%s,"roles_ok":%s,"failed":"%s","unknown_role_refused":"%s","recorded_at":"%s"}\n' \
        "$(os_name)" "$(uname -m)" "$toolchain" "$(ci_git_sha)" \
        "$total" "$ok" "$(printf '%s' "$failed" | xargs)" "$unknown_refused" "$(ci_now)" >"$summary"

    if [ "$fails" -gt 0 ] || [ "$unknown_refused" != yes ]; then
        printf '%s record: %s/%s roles started' "$rec" "$ok" "$total"
        return 1
    fi
    printf '%s record: %s/%s roles started' "$rec" "$ok" "$total"
}

run() {
    local target record
    target="$(detect_target)"
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    ci_step "runner $target"

    if [ -z "$BIN" ]; then
        if ! command -v cargo >/dev/null 2>&1; then
            ci_skip "runner-$target" "no cargo on this runner — cannot build the server role entry point"
            return 0
        fi
        if ! ci_pipe "runners-build-$target" cargo build -q -p boundless-server >/dev/null; then
            ci_fail "runner-$target" "cargo build -p boundless-server failed on $target (see logs/runners-build-$target.log)"
            return 0
        fi
        local exe="$REPO_ROOT/target/debug/boundless-server"
        [ -x "$exe.exe" ] && exe="$exe.exe"
        if [ ! -x "$exe" ]; then
            ci_fail "runner-$target" "no executable produced at $exe"
            return 0
        fi
        BIN="$exe"
    fi

    if [ -z "$(roles)" ]; then
        ci_fail "runner-$target" "no server roles resolved from $(basename "$ROLES_SOURCE")"
        return 0
    fi

    if record="$(write_record "$target")"; then
        ci_pass "runner-$target" "$(printf '%s' "$record" | head -1); unknown role refused"
    else
        ci_fail "runner-$target" "$(printf '%s' "$record" | head -1)"
        return 0
    fi
    ci_note "record: $(record_path "$target")"
}

self_test() {
    local sandbox stub target fails=0
    sandbox="$(mktemp -d)"
    target="$(detect_target)"
    # A private CI_OUT: these lanes must not overwrite the record of a real
    # run that already happened in the same process.
    CI_OUT="$sandbox/ci-out"
    CI_LOG_DIR="$CI_OUT/logs"
    mkdir -p "$CI_LOG_DIR"

    ci_step "self-test: a role that fails to start fails the lane"
    stub="$sandbox/stub-server"
    cat >"$stub" <<'EOF'
#!/usr/bin/env bash
role="$1"
if [ "$role" = "badrole" ]; then
    echo "stub-server: role exploded" >&2
    exit 7
fi
if [ "$role" = "not-a-real-role" ]; then
    echo "stub-server: unknown server role 'not-a-real-role' (expected one of: goodrole)" >&2
    exit 1
fi
echo "[$role] starting (stub)"
EOF
    chmod +x "$stub"
    local rec
    rec="$(record_path "$target")"
    mkdir -p "$(dirname "$rec")"
    local detail rc
    detail="$(CI_RUNNERS_BIN="$stub" CI_RUNNERS_ROLES="goodrole badrole" write_record "$target")"
    rc=$?
    if [ "$rc" -ne 0 ] && grep -q '^badrole	FAIL' "$rec"; then
        ci_pass selftest-runner "failed role named in $(basename "$rec"): $(printf '%s' "$detail" | head -1 | sed "s|$rec|$(basename "$rec")|")"
    else
        ci_fail selftest-runner "a failing role did not fail the lane (rc=$rc)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a clean stub starts every role"
    rm -f "$rec"
    stub="$sandbox/clean-server"
    cat >"$stub" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "not-a-real-role" ]; then
    echo "stub-server: unknown server role 'not-a-real-role'" >&2
    exit 1
fi
echo "[$1] starting (stub)"
EOF
    chmod +x "$stub"
    detail="$(CI_RUNNERS_BIN="$stub" CI_RUNNERS_ROLES="goodrole" write_record "$target")"
    if [ -s "$rec" ] && grep -q '^goodrole	ok' "$rec"; then
        ci_pass selftest-runner-clean "clean stub recorded in $(basename "$rec")"
    else
        ci_fail selftest-runner-clean "the clean stub was not recorded"
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
    if [ "$MODE" = self-test ]; then
        self_test
        return $?
    fi
    run
}

main
LANE_RC=$?

set +e
ci_finalize
FINAL_RC=$?
set -e

[ "$LANE_RC" -ne 0 ] && exit "$LANE_RC"
exit "$FINAL_RC"