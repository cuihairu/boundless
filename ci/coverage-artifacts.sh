#!/usr/bin/env bash
# BW-00-05-05 — record the reference distribution, SDK and resource gaps.
#
# Usage:
#   ci/coverage-artifacts.sh            # record the artifact inventory
#   ci/coverage-artifacts.sh self-test  # planted reference and port prove it
#
# The coverage lanes index build targets; the reference also ships artifacts
# that are not targets of this tree — a prebuilt server distribution (game/),
# a client SDK (client_sdk/, on the reference's own lane/client-sdk), and
# runtime resources the servers need that neither the distribution nor this
# port carries. This lane records each with its source and, for every gap, the
# reason it blocks and the ticket that turns it on.
#
# One record face:
#   * artifacts-record — coverage-artifacts.json + coverage-artifacts.tsv. An
#     absent reference root skips naming the ticket; a hollow map (no rows, or
#     a gap row without a source or a blocking reason) is not a record.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
cur_ref() { printf '%s' "${CI_ART_REF:-${BW_REFERENCE:-$HOME/workspaces/BigWorld}}"; }
cur_port() { printf '%s' "${CI_ART_PORT:-$REPO_ROOT}"; }
cur_todo() { printf '%s' "${CI_ART_TODO:-$REPO_ROOT/todo.md}"; }
cur_sdk() { printf '%s' "${CI_ART_SDK:-$HOME/workspaces/bw-clientsdk}"; }
cur_out() { printf '%s' "${CI_ART_OUT:-$CI_OUT}"; }
art_dir() { printf '%s/coverage-artifacts' "$(cur_out)"; }

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    ci_step "record the reference distribution, SDK and resource gaps"

    local ref
    ref="$(cur_ref)"
    if [ ! -d "$ref" ]; then
        ci_skip artifacts-record \
            "no reference tree at $ref — the artifact inventory records the distribution, SDK and resource gaps (BW-00-05-05)"
        return 0
    fi

    local out_dir
    out_dir="$(art_dir)"
    local sum
    if ! sum="$(python3 "$REPO_ROOT/ci/coverage-artifacts.py" "$ref" "$(cur_port)" \
        "$(cur_todo)" "$out_dir" --sdk-root "$(cur_sdk)" --self-check 2>&1)"; then
        ci_fail artifacts-record "the artifact inventory failed to record: $(printf '%s' "$sum" | tail -1)"
        return 1
    fi
    local total dist sdk missing
    total="$(printf '%s' "$sum" | sed -n 's/.*artifacts=\([0-9]*\).*/\1/p')"
    dist="$(printf '%s' "$sum" | sed -n 's/.*distribution=\([0-9]*\).*/\1/p')"
    sdk="$(printf '%s' "$sum" | sed -n 's/.*sdk=\([0-9]*\).*/\1/p')"
    missing="$(printf '%s' "$sum" | sed -n 's/.*missing=\([0-9]*\).*/\1/p')"
    if [ -z "$total" ] || [ "$total" -eq 0 ]; then
        ci_fail artifacts-record "the artifact inventory recorded no row count — a hollow map is not a record"
        return 1
    fi
    ci_pass artifacts-record "$total artifact(s) recorded ($dist distribution, $sdk sdk, $missing missing)"

    # The committed snapshot is the anchor; a map that grows is a note.
    local f
    for f in json tsv; do
        if [ -f "$REPO_ROOT/tests/baseline/coverage-artifacts.$f" ] \
            && [ -f "$out_dir/coverage-artifacts.$f" ] \
            && ! cmp -s "$REPO_ROOT/tests/baseline/coverage-artifacts.$f" "$out_dir/coverage-artifacts.$f"; then
            ci_note "the committed tests/baseline/coverage-artifacts.$f differs from this run — snapshot-time anchor, not a failure"
        fi
    done

    ci_note "every gap carries a source and a blocking reason"
}

# A planted reference with a prebuilt distribution, a planted client SDK tree
# and a planted todo naming the artifact tickets.
plant_ref() { # $1 reference root
    local ref="$1"
    mkdir -p "$ref/game/bin/server/el7/server/cellapp-extensions" \
        "$ref/game/bin/server/el7/tools" \
        "$ref/game/bin/server/el7/examples" \
        "$ref/game/bin/server/el7/unit_tests" \
        "$ref/game/bin/server/el7/third_party/python" \
        "$ref/game/res/bigworld/scripts/common"
    printf 'x' >"$ref/game/bin/server/el7/server/baseapp"
    printf 'x' >"$ref/game/bin/server/el7/server/cellapp-extensions/cellapp_example.so"
    printf 'x' >"$ref/game/bin/server/el7/tools/bwmachined2"
    printf 'x' >"$ref/game/bin/server/el7/examples/simple_python_client"
    printf 'x' >"$ref/game/bin/server/el7/unit_tests/baseapp_test"
    printf 'x' >"$ref/game/res/bigworld/scripts/common/Lib"
}

plant_sdk() { # $1 sdk root
    local sdk="$1"
    for p in unity unreal typescript generator; do
        mkdir -p "$sdk/client_sdk/$p"
        printf 'x' >"$sdk/client_sdk/$p/README.md"
    done
}

plant_todo() { # $1 todo path
    cat >"$1" <<'EOF'
### BW-03 CPython 嵌入和脚本 API
源码 `lib/pyscript/`、`lib/script/`。

### BW-14 FantasyDemo、SDK 示例与发行
源码 `../fantasydemo/`、`examples/`、发行物。

### BW-21 多引擎客户端 SDK
源码 `lib/{connection,connection_model,bwentity}/`。
EOF
}

self_test() {
    local fails=0 rc r T M r2 T2 M2 r3 T3 M3 S
    ci_step "self-test: a planted distribution and SDK are each recorded with source and blocker"
    r="$(mktemp -d "$REPO_ROOT/ci-out/art-selftest.XXXXXX")"
    T="$r/BigWorld"
    M="$r/manifest/manifest.tsv"
    S="$r/sdk"
    plant_ref "$T"
    plant_sdk "$S"
    plant_todo "$T/todo.md"
    mkdir -p "$r/port"

    CI_ART_REF="$T" CI_ART_PORT="$r/port" CI_ART_TODO="$T/todo.md" CI_ART_SDK="$S" \
        CI_ART_OUT="$r/out" CI_OUT="$r/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q '^distribution/baseapp	server process	game/bin/server/el7/server/baseapp	BW-14	' "$r/out/coverage-artifacts/coverage-artifacts.tsv" \
        && grep -q '^distribution/cellapp-extensions/cellapp_example.so	server extension	.*	BW-14	' "$r/out/coverage-artifacts/coverage-artifacts.tsv" \
        && grep -q '^distribution/scripts	runtime scripts tree	.*	BW-14	' "$r/out/coverage-artifacts/coverage-artifacts.tsv" \
        && grep -q '^sdk/unity	sdk part	client_sdk/unity	BW-21	' "$r/out/coverage-artifacts/coverage-artifacts.tsv" \
        && grep -q '^sdk/port	port sdk gap	.*	none yet	' "$r/out/coverage-artifacts/coverage-artifacts.tsv" \
        && grep -q '^missing/bundled-scripts	missing resource	.*	none yet	.*BW-03' "$r/out/coverage-artifacts/coverage-artifacts.tsv" \
        && grep -q '^missing/game-project-server-config	missing resource	.*	none yet	.*BW-14' "$r/out/coverage-artifacts/coverage-artifacts.tsv"; then
        ci_pass selftest-artifacts-indexes "distribution rows to BW-14, sdk rows to BW-21, and every missing row carries a source and a blocking reason"
    else
        ci_fail selftest-artifacts-indexes "planted artifacts were not recorded as expected: rc=$rc $(grep 'artifacts-record' "$M" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: an absent reference root skips naming the ticket"
    r2="$(mktemp -d "$REPO_ROOT/ci-out/art-selftest.XXXXXX")"
    T2="$r2/none"
    M2="$r2/manifest/manifest.tsv"
    CI_ART_REF="$T2" CI_ART_PORT="$REPO_ROOT" CI_ART_TODO="$REPO_ROOT/todo.md" \
        CI_ART_OUT="$r2/out" CI_OUT="$r2/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'artifacts-record	SKIP	.*BW-00-05-05' "$M2"; then
        ci_pass selftest-artifacts-absent "an absent reference root reports SKIP naming BW-00-05-05"
    else
        ci_fail selftest-artifacts-absent "an absent reference root did not skip cleanly: rc=$rc"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a reference with no distribution and a port with no gaps is hollow"
    r3="$(mktemp -d "$REPO_ROOT/ci-out/art-selftest.XXXXXX")"
    T3="$r3/empty"
    M3="$r3/manifest/manifest.tsv"
    mkdir -p "$T3"
    plant_todo "$T3/todo.md"
    # A port that already carries the gaps makes the missing set empty; with no
    # distribution either, the map is hollow.
    mkdir -p "$r3/port/res/server" "$r3/port/res/scripts/entity_defs" \
        "$r3/port/game/res/bigworld/scripts"
    CI_ART_REF="$T3" CI_ART_PORT="$r3/port" CI_ART_TODO="$T3/todo.md" \
        CI_ART_OUT="$r3/out" CI_OUT="$r3/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -ne 0 ] \
        && grep -q 'artifacts-record	FAIL	.*hollow map' "$M3"; then
        ci_pass selftest-artifacts-hollow "a reference with no distribution and no gaps fails as hollow rather than recording"
    else
        ci_fail selftest-artifacts-hollow "a hollow artifact tree did not fail cleanly: rc=$rc $(grep 'artifacts-record' "$M3" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$r2" "$r3" "$REPO_ROOT/ci-out/art-selftest".*

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
    esac
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
