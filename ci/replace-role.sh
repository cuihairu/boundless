#!/usr/bin/env bash
# BW-00-04-05 — the single-component replacement run, per server role.
#
# Usage:
#   ci/replace-role.sh            # inventory + substitution run attempt
#   ci/replace-role.sh self-test  # planted stacks, planted run gates
#
# The substitution path replaces exactly one original role with the port's
# equivalent and holds every other component constant — the differential
# BW-00-04-04's full-stack comparison cannot isolate. Two record faces:
#
#   * replace-record — the per-role replacement inventory, one row per port
#     role: the role (named on every row, which is the acceptance), the
#     reference binary it would stand in for, the port's entry point, and the
#     substitution gate.
#   * replace-run — the substitution run attempt. The gate is per stack: the
#     original must be launchable (server/bw.xml at the resource root + a
#     machined configuration) and the port's role must be runnable (the server
#     runtime wires Mercury). A closed gate records the missing piece naming
#     the roles; an open one runs each substitution and demands the log name
#     the replaced role — a run that stays silent about which component it
#     swapped is hollow, not a pass.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
cur_ref() { printf '%s' "${CI_REPLACE_REF:-${BW_REFERENCE:-$HOME/workspaces/BigWorld}}"; }
cur_out() { printf '%s' "${CI_REPLACE_OUT:-$CI_OUT}"; }
rr_dir() { printf '%s/replace-role' "$(cur_out)"; }

# The original launch gate — what a live original full stack needs. The
# reference ships the role binaries but no server/bw.xml at the resource root
# and no machined config, so the gate is closed until those are authored.
original_run_gate() { # prints "open|detail" or "closed|detail"
    local ref detail=""
    ref="$(cur_ref)"
    [ -d "$ref" ] || { printf 'closed|no reference tree at %s' "$ref"; return 0; }

    if [ -x "$ref/game/bin/server/el7/tools/bwmachined2" ] \
        || [ -x "$ref/game/bin/server/el7/server/baseapp" ]; then
        : # role binaries present
    else
        detail="no machined/role binary under game/bin/server/el7"
    fi

    if [ -n "${BW_RES_PATH:-}" ] && [ -f "$BW_RES_PATH/server/bw.xml" ]; then
        :
    elif [ -f "$ref/game/res/bigworld/server/bw.xml" ]; then
        :
    else
        detail="${detail:+$detail; }no server/bw.xml at the resource root (BW_RES_PATH=${BW_RES_PATH:-unset})"
    fi
    if [ -z "$detail" ]; then
        printf 'open|role binaries and a server/bw.xml resource root are present'
    else
        printf 'closed|%s' "$detail"
    fi
}

# The substitution run for one role: the original stack comes up, then the
# port's binary stands in for this role.
substitute_run() { # $1 role  $2 port binary — output on stdout
    local role="$1" bin="$2"
    "$(cur_ref)/game/bin/server/el7/tools/bwmachined2" || return 1
    "$bin" "$role" || return 1
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    ci_step "single-component replacement run"

    # The port's role set is the inventory's source; a lane with no role set
    # has nothing to name, which is hollow.
    if [ ! -e "$REPO_ROOT/crates/server-runtime/src/app.rs" ]; then
        ci_fail replace-record \
            "port source missing: crates/server-runtime/src/app.rs — no role set to name"
        return 1
    fi

    local parser_root sum roles
    parser_root="${CI_REPLACE_PARSER_ROOT:-$REPO_ROOT}"
    if ! sum="$(python3 "$REPO_ROOT/ci/replace-role.py" "$parser_root" "$(rr_dir)" \
            "$(cur_ref)" --self-check 2>&1)"; then
        ci_fail replace-record "the inventory failed to record: $(printf '%s' "$sum" | tail -1)"
        return 1
    fi
    roles="$(printf '%s' "$sum" | sed -n 's/.*roles=\([0-9]*\).*/\1/p')"
    if [ -z "$roles" ] || [ "$roles" -lt 1 ]; then
        ci_fail replace-record "recorded $roles role(s) — a report with no role is not a record"
        return 1
    fi
    ci_pass replace-record \
        "$roles role(s) named in replace-roles.tsv (report notes the replaced role)"

    # The substitution run: both stacks must be runnable, and every role's run
    # must name the role it swapped.
    local ogate odetail pgate preason rows
    rows="$(original_run_gate)"
    ogate="$(printf '%s' "$rows" | cut -d'|' -f1)"
    odetail="$(printf '%s' "$rows" | cut -d'|' -f2)"
    pgate="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["gate"])' "$(rr_dir)/replace-roles.json")"
    preason="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["gate_reason"])' "$(rr_dir)/replace-roles.json")"

    local rolelist
    rolelist="$(python3 -c 'import json, sys; print(", ".join(r["role"] for r in json.load(open(sys.argv[1]))["roles"]))' "$(rr_dir)/replace-roles.json")"

    if [ "$ogate" = "open" ] && [ "$pgate" = "open" ]; then
        local log ok=1 role
        log="$(mktemp -d "$REPO_ROOT/ci-out/replace-run.XXXXXX")/run.log"
        for role in $(printf '%s' "$rolelist" | tr ',' ' '); do
            if ! substitute_run "$role" \
                "${CI_REPLACE_PORT_BIN:-$REPO_ROOT/target/debug/boundless-server}" >>"$log" 2>&1; then
                ci_fail replace-run "the substitution of $role failed: $(tail -1 "$log")"
                ok=0
                break
            fi
            if ! grep -q "$role" "$log"; then
                ci_fail replace-run "the substitution of $role does not name the replaced role in its log"
                ok=0
                break
            fi
        done
        if [ "$ok" -eq 1 ]; then
            cat >"$(rr_dir)/replace-run.json" <<JSON
{
  "run": "recorded",
  "gate": "open",
  "roles": "$rolelist",
  "log": "$log"
}
JSON
            ci_pass replace-run \
                "$roles role(s) substituted and each run names the replaced role"
        fi
    else
        local detail="$odetail"
        [ "$pgate" != "open" ] && detail="${detail:+$detail; }$preason"
        cat >"$(rr_dir)/replace-run.json" <<JSON
{
  "run": "not run",
  "gate": "closed",
  "original_gate": "$ogate",
  "original_gate_detail": "$odetail",
  "port_gate": "$pgate",
  "port_gate_reason": "$preason",
  "roles": "$rolelist"
}
JSON
        ci_skip replace-run \
            "$detail — the substitution run is retained when both gates open; $roles role(s) named in replace-roles.tsv"
    fi

    ci_note "replacement inventory recorded; original stack gate: $ogate; port role gate: $pgate"

    # The committed tests/baseline/replace-roles.* are the snapshot-time anchor
    # for the report; a run that differs as the port's role set grows is a note,
    # not a failure.
    local f fresh
    for f in replace-roles.json replace-roles.tsv; do
        fresh="$(rr_dir)/$f"
        if [ -f "$REPO_ROOT/tests/baseline/$f" ] && [ -f "$fresh" ] \
            && ! cmp -s "$REPO_ROOT/tests/baseline/$f" "$fresh"; then
            ci_note "the committed tests/baseline/$f differs from this run — snapshot-time anchor, not a failure"
        fi
    done
}

# A planted reference tree (machined stub + bw.xml, per-role binaries) and a
# planted port whose app.rs carries a small KNOWN_ROLES and, in the wired
# kind, a Mercury-consuming server runtime.
plant_tree() { # $1 root  $2 "with substrate" | "no substrate"
    local root="$1" kind="$2"
    mkdir -p "$root/game/bin/server/el7/tools" \
        "$root/game/bin/server/el7/server" \
        "$root/crates/server-runtime/src" \
        "$root/target/debug"

    printf '#!/bin/sh\necho "full stack launched: original"\nexit 0\n' \
        >"$root/game/bin/server/el7/tools/bwmachined2"
    chmod +x "$root/game/bin/server/el7/tools/bwmachined2"
    printf '#!/bin/sh\nexit 0\n' >"$root/game/bin/server/el7/server/loginapp"
    printf '#!/bin/sh\nexit 0\n' >"$root/game/bin/server/el7/server/baseapp"
    chmod +x "$root/game/bin/server/el7/server/loginapp" \
        "$root/game/bin/server/el7/server/baseapp"

    if [ "$kind" = "with substrate" ]; then
        mkdir -p "$root/res/server"
        printf '<root/>' >"$root/res/server/bw.xml"
    fi

    {
        printf '//! the port roles\n'
        printf 'pub const KNOWN_ROLES: &[&str] = &["loginapp", "baseapp"];\n'
        if [ "$kind" = "with substrate" ]; then
            printf 'use mercury::NetworkInterface;\n'
        fi
        printf 'pub fn run(_role: &str) {}\n'
    } >"$root/crates/server-runtime/src/app.rs"

    # The port binary: a stand-in that names the role it replaced, which is
    # what the run face demands of a real substitution too.
    printf '#!/bin/sh\necho "replaced role $1"\nexit 0\n' \
        >"$root/target/debug/boundless-server"
    chmod +x "$root/target/debug/boundless-server"
}

self_test() {
    local fails=0 rc r T M r2 T2 M2
    ci_step "self-test: a runnable original and a wired port substitute every role and name it"
    r="$(mktemp -d "$REPO_ROOT/ci-out/replace-selftest.XXXXXX")"
    T="$r/BigWorld"
    M="$r/manifest/manifest.tsv"
    plant_tree "$T" "with substrate"

    BW_RES_PATH="$T/res" CI_REPLACE_REF="$T" CI_REPLACE_PARSER_ROOT="$T" \
        CI_REPLACE_PORT_BIN="$T/target/debug/boundless-server" \
        CI_REPLACE_OUT="$r/out" CI_OUT="$r/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'replace-record	PASS	2 role(s) named' "$M" \
        && grep -q 'replace-run	PASS	2 role(s) substituted' "$M" \
        && grep -q '"run": "recorded"' "$r/out/replace-role/replace-run.json" \
        && grep -q '"roles": "loginapp, baseapp"' "$r/out/replace-role/replace-run.json"; then
        ci_pass selftest-replace-runs "a runnable stack substitutes both roles and each run names its role"
    else
        ci_fail selftest-replace-runs "planted substitutions did not record: rc=$rc $(grep -E 'replace-' "$M" 2>/dev/null | head -2 | tr '\n' '; ')"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a tree with no launch substrate closes the substitution run"
    r2="$(mktemp -d "$REPO_ROOT/ci-out/replace-selftest.XXXXXX")"
    T2="$r2/BigWorld"
    M2="$r2/manifest/manifest.tsv"
    plant_tree "$T2" "with substrate"
    rm -rf "$T2/res"
    CI_REPLACE_REF="$T2" CI_REPLACE_PARSER_ROOT="$T2" \
        CI_REPLACE_PORT_BIN="$T2/target/debug/boundless-server" \
        CI_REPLACE_OUT="$r2/out" CI_OUT="$r2/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'replace-run	SKIP	.*server/bw.xml' "$M2" \
        && grep -q 'replace-record	PASS	2 role(s) named' "$M2" \
        && grep -q '"run": "not run"' "$r2/out/replace-role/replace-run.json"; then
        ci_pass selftest-replace-orig-gate "a tree with no bw.xml closes the run and still names both roles"
    else
        ci_fail selftest-replace-orig-gate "a missing bw.xml did not close the run: rc=$rc $(grep 'replace-run' "$M2" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a port with no Mercury wiring closes the substitution run"
    r2="$(mktemp -d "$REPO_ROOT/ci-out/replace-selftest.XXXXXX")"
    T2="$r2/BigWorld"
    M2="$r2/manifest/manifest.tsv"
    plant_tree "$T2" "no substrate"
    CI_REPLACE_REF="$T2" CI_REPLACE_PARSER_ROOT="$T2" \
        CI_REPLACE_PORT_BIN="$T2/target/debug/boundless-server" \
        CI_REPLACE_OUT="$r2/out" CI_OUT="$r2/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'replace-run	SKIP	.*BW-02' "$M2" \
        && grep -q 'replace-record	PASS	2 role(s) named' "$M2"; then
        ci_pass selftest-replace-port-gate "a Mercury-less port closes the run naming BW-02 but still names the roles"
    else
        ci_fail selftest-replace-port-gate "a Mercury-less port did not close the run: rc=$rc $(grep 'replace-run' "$M2" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: no reference tree still names every role and skips the run"
    r2="$(mktemp -d "$REPO_ROOT/ci-out/replace-selftest.XXXXXX")"
    T2="$r2/none"
    M2="$r2/manifest/manifest.tsv"
    CI_REPLACE_REF="$T2" CI_REPLACE_OUT="$r2/out" CI_OUT="$r2/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'replace-record	PASS	8 role(s) named' "$M2" \
        && grep -q 'replace-run	SKIP	.*no reference tree' "$M2"; then
        ci_pass selftest-replace-absent "an absent tree still names all 8 port roles and skips the run"
    else
        ci_fail selftest-replace-absent "an absent tree did not behave: rc=$rc $(grep -E 'replace-' "$M2" 2>/dev/null | head -2 | tr '\n' '; ')"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$r2" "$REPO_ROOT/ci-out/replace-selftest".*

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
