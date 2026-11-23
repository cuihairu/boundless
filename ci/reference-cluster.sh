#!/usr/bin/env bash
# BW-00-02-03 — the single-machine cluster startup baseline: topology,
# configuration and logs, recorded without inventing any of it.
#
# Usage:
#   ci/reference-cluster.sh            # record the cluster face -> ci-out/
#   ci/reference-cluster.sh self-test  # planted tree + planted config faces
#
# Three faces, each recorded as it stands:
#   * topology  — the process set the original ships: every payload under
#     game/bin/server/el7/server plus the daemons and tools under el7/tools;
#   * config    — what a launch would read: BW_RES_PATH, the resource root,
#     and ~/.bwmachined.conf for the machine daemon;
#   * logs      — the log face after a launch.
#
# The launch gate is deliberate. Nothing in the reference tree authors
# ~/.bwmachined.conf, sets BW_RES_PATH or documents a cluster startup command
# (docs/index.md and docs/zh/index.md are silent on baseappmgr and friends),
# and process_defs already proves the gap: run with neither, it exits with
# "Unable to open ~/.bwmachined.conf" / "No res path set". So the lane records
# the topology and the configuration substrate, then reports the launch gate
# as a named gap rather than authoring a config that would be a guess about
# the original. A tree without el7 fails: a recorded cluster with no payload
# is a hollow baseline, not one.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
cur_ref() { printf '%s' "${CI_CLUSTER_ROOT:-${BW_REFERENCE:-$HOME/workspaces/BigWorld}}"; }
cur_out() { printf '%s' "${CI_CLUSTER_OUT:-$CI_OUT}"; }
cur_conf() { printf '%s' "${CI_CLUSTER_CONF:-$HOME/.bwmachined.conf}"; }

tsv_file() { printf '%s/reference-cluster/cluster-topology.tsv' "$(cur_out)"; }
json_file() { printf '%s/reference-cluster/cluster-baseline.json' "$(cur_out)"; }

classify() { # what a payload name means for the cluster
    case "$1" in
    *mgr) printf 'manager' ;;
    bwmachined*|message_logger) printf 'daemon' ;;
    *extensions) printf 'extension' ;;
    *) printf 'process' ;;
    esac
}

record_topology() { # writes the tsv, prints "count managers daemons processes"
    local ref el7 n f kind total=0
    ref="$(cur_ref)"
    el7="$ref/game/bin/server/el7"
    mkdir -p "$(dirname "$(tsv_file)")"
    {
        printf '# reference cluster topology\n'
        printf '# tree: %s\n' "$ref"
    } >"$(tsv_file)"
    # Provenance guard: git walks up when the tree has no .git, and a header
    # naming another repo's commit would be a lie about what was recorded.
    local head="" toplevel
    head="$(git -C "$ref" rev-parse HEAD 2>/dev/null || true)"
    toplevel="$(git -C "$ref" rev-parse --show-toplevel 2>/dev/null || true)"
    [ -n "$head" ] && [ "$toplevel" = "$ref" ] || head="unknown (not the reference's own git toplevel)"
    printf '# commit: %s\n' "$head" >>"$(tsv_file)"
    for f in "$el7"/server/* "$el7"/tools/*; do
        [ -e "$f" ] || continue
        n="${f##*/}"
        case "$n" in *.so) continue ;; esac
        case "$f" in
        "$el7"/server/*) printf '%s\t%s\n' "$n" "el7/server/$n" ;;
        *) printf '%s\t%s\n' "$n" "el7/tools/$n" ;;
        esac >>"$(tsv_file)"
        kind="$(classify "$n")"
        case "$kind" in
        manager) mgrs=$((mgrs + 1)) ;;
        daemon) daemons=$((daemons + 1)) ;;
        *) total=$((total + 1)) ;;
        esac
    done
    total=$((total + mgrs + daemons))
    printf '%s %s %s %s\n' "$total" "$mgrs" "$daemons" "$(wc -l <"$(tsv_file)")"
}

record_config() { # prints "res_env|res_root|conf|resroot_note"
    local ref conf root env_state root_state conf_state
    ref="$(cur_ref)"
    conf="$(cur_conf)"
    if [ -n "${BW_RES_PATH:-}" ]; then
        env_state="set (${BW_RES_PATH})"
    else
        env_state="unset"
    fi
    root="$ref/game/res/bigworld"
    if [ -d "$root" ]; then
        root_state="present ($(ls "$root" 2>/dev/null | tr '\n' ' '))"
    else
        root_state="absent"
    fi
    if [ -f "$conf" ]; then
        conf_state="present ($conf)"
    else
        conf_state="absent ($conf)"
    fi
    printf '%s|%s|%s|no cluster startup command documented in reference docs\n' \
        "$env_state" "$root_state" "$conf_state"
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    ci_step "single-machine cluster baseline"
    if [ ! -d "$(cur_ref)" ]; then
        ci_skip reference-cluster \
            "no reference tree at $(cur_ref) — BW-00-02-03 records it when present (set BW_REFERENCE)"
        return 0
    fi

    local el7 total mgrs daemons rows cfg env_state root_state conf_state
    el7="$(cur_ref)/game/bin/server/el7"
    if [ ! -d "$el7/server" ]; then
        ci_fail cluster-topology "no el7/server payload under $el7 — a cluster record with nothing in it is a hollow baseline"
        return 1
    fi

    mgrs=0
    daemons=0
    total=0
    rows="$(record_topology | tail -1)"
    total="$(printf '%s' "$rows" | cut -d' ' -f1)"
    mgrs="$(printf '%s' "$rows" | cut -d' ' -f2)"
    daemons="$(printf '%s' "$rows" | cut -d' ' -f3)"
    if [ "$total" -lt 1 ]; then
        ci_fail cluster-topology "no payload entries recorded under el7/{server,tools}"
        return 1
    fi
    ci_pass cluster-topology \
        "$total payload(s) recorded ($mgrs managers, $daemons daemons) in cluster-topology.tsv"

    cfg="$(record_config)"
    env_state="$(printf '%s' "$cfg" | cut -d'|' -f1)"
    root_state="$(printf '%s' "$cfg" | cut -d'|' -f2)"
    conf_state="$(printf '%s' "$cfg" | cut -d'|' -f3)"
    ci_pass cluster-config "BW_RES_PATH $env_state; resource root $root_state; machined config $conf_state"

    local gate_open=0
    case "$conf_state" in present*) gate_open=1 ;; esac
    [ "$env_state" = "unset" ] && gate_open=0
    if [ "$gate_open" -eq 0 ]; then
        # An external precondition that the reference never ships is a SKIP
        # naming what is missing, not a defect of the tree under record.
        ci_skip cluster-launch \
            "$conf_state and BW_RES_PATH $env_state — the reference ships no machined config, no res path and no documented cluster startup command, so no launch (and no log) can be retained until one is authored"
        write_json "$total" "$mgrs" "$daemons" "$env_state" "$root_state" "$conf_state" "closed"
        ci_note "topology and configuration are recorded; logs: absent (no launch)"
        return 0
    fi

    ci_pass cluster-launch "launch substrate present — a recorded run would retain its log"
    write_json "$total" "$mgrs" "$daemons" "$env_state" "$root_state" "$conf_state" "open"
    ci_note "logs: absent — no launch has been recorded (launch gate closed)"
}

write_json() { # $1 count $2 mgrs $3 daemons $4 env $5 root $6 conf $7 gate
    mkdir -p "$(dirname "$(json_file)")"
    {
        printf '{"reference_root":"%s","reference_commit":"%s",' \
            "$(cur_ref)" "$(git -C "$(cur_ref)" rev-parse HEAD 2>/dev/null || echo unknown)"
        printf '"payloads":%s,"managers":%s,"daemons":%s,' "$1" "$2" "$3"
        printf '"res_path":"%s","resource_root":"%s","machined_config":"%s",' "$4" "$5" "$6"
        printf '"launch_gate":"%s","logs":"absent (no launch recorded)",' "$7"
        printf '"checked_at":"%s","lane_git_sha":"%s"}\n' "$(ci_now)" "$(ci_git_sha)"
    } >"$(json_file)"
}

self_test() {
    local fails=0 r out rc T
    ci_step "self-test: a planted payload records its topology and names the closed gate"
    r="$(mktemp -d "$REPO_ROOT/ci-out/cluster-selftest.XXXXXX")"
    T="$r/BigWorld"
    mkdir -p "$T/game/bin/server/el7/server/baseapp" \
        "$T/game/bin/server/el7/server/baseappmgr" \
        "$T/game/bin/server/el7/server/cellapp-extensions" \
        "$T/game/bin/server/el7/tools" \
        "$T/game/res/bigworld/scripts"
    printf 'x' >"$T/game/bin/server/el7/server/baseapp/baseapp"
    printf 'x' >"$T/game/bin/server/el7/server/baseappmgr/baseappmgr"
    printf 'x' >"$T/game/bin/server/el7/tools/bwmachined2"
    chmod +x "$T/game/bin/server/el7/server/baseapp/baseapp" \
        "$T/game/bin/server/el7/server/baseappmgr/baseappmgr" \
        "$T/game/bin/server/el7/tools/bwmachined2"

    out="$(CI_CLUSTER_ROOT="$T" CI_CLUSTER_OUT="$r/out" CI_CLUSTER_CONF="$r/none.conf" CI_OUT="$r/manifest" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -eq 0 ] \
        && printf '%s\n' "$out" | grep -q 'cluster-topology	PASS	4 payload(s) recorded (1 managers, 1 daemons)' \
        && printf '%s\n' "$out" | grep -q 'cluster-launch	SKIP	.*none.conf' \
        && grep -q 'baseappmgr' "$r/out/reference-cluster/cluster-topology.tsv"; then
        ci_pass selftest-cluster "planted payload records 4 payloads and names the closed launch gate"
    else
        ci_fail selftest-cluster "planted payload did not record: rc=$rc $(printf '%s\n' "$out" | grep 'cluster-' | head -2)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: an open gate keeps its configuration face"
    printf 'BW_RES_PATH=/nowhere\n' >"$r/open.conf"
    out="$(CI_CLUSTER_ROOT="$T" CI_CLUSTER_OUT="$r/out" CI_CLUSTER_CONF="$r/open.conf" \
        CI_OUT="$r/manifest" BW_RES_PATH="$T/game/res/bigworld" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -eq 0 ] \
        && printf '%s\n' "$out" | grep -q 'cluster-launch	PASS	launch substrate present' \
        && python3 -c 'import json,sys; json.load(open(sys.argv[1]))' \
            "$r/out/reference-cluster/cluster-baseline.json" 2>/dev/null; then
        ci_pass selftest-cluster-open "an authored config and a set BW_RES_PATH open the gate into a valid record"
    else
        ci_fail selftest-cluster-open "an open gate did not record: rc=$rc $(printf '%s\n' "$out" | grep 'cluster-launch' | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: no reference tree reports SKIP naming the ticket"
    out="$(CI_CLUSTER_ROOT="$r/none" CI_CLUSTER_OUT="$r/out2" CI_OUT="$r/manifest" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -eq 0 ] && printf '%s\n' "$out" | grep -q 'reference-cluster	SKIP	.*BW-00-02-03'; then
        ci_pass selftest-cluster-absent "absent tree reports SKIP naming BW-00-02-03"
    else
        ci_fail selftest-cluster-absent "absent tree did not skip cleanly: rc=$rc"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a tree without el7 fails instead of recording an empty cluster"
    mkdir -p "$r/empty/game/bin/server"
    out="$(CI_CLUSTER_ROOT="$r/empty" CI_CLUSTER_OUT="$r/out3" CI_OUT="$r/manifest" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ] && printf '%s\n' "$out" | grep -q 'cluster-topology	FAIL	.*hollow baseline'; then
        ci_pass selftest-cluster-empty "a payload-less tree fails the topology leg"
    else
        ci_fail selftest-cluster-empty "a payload-less tree did not fail loudly: rc=$rc"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$REPO_ROOT/ci-out/cluster-selftest".*

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
