#!/usr/bin/env bash
# BW-00-04-04 — the original and the new full-stack comparison run: one
# canonical input (the fantasydemo login identity, protocol version and
# fixture set) drives both sides, each producing its own evidence file. The
# original's evidence is derived from the frozen reference records; the new
# port's evidence is derived from the port's own source. Where a side can
# launch a live full stack the lane runs it and captures the real evidence;
# where it cannot, the lane records the closed run gate naming the missing
# piece, alongside the source-derived expectation.
#
# Usage:
#   ci/compare-fullstack.sh            # input, evidence, run gates, comparison
#   ci/compare-fullstack.sh self-test  # planted stacks, planted run gates
#
# Four record faces:
#   * input — the canonical input both sides run (login identity, protocol
#     version, entity set), from the frozen fixtures.
#   * original — the original full-stack evidence: the login wire, the
#     callback sequence, the database seed and the entity defs, from the
#     recorded protocol/callback/fixture baselines.
#   * new — the new port's full-stack evidence: the Mercury login interface
#     and the entity type-id registry from the port's own source, plus the
#     run gate (the port wires Mercury only at BW-02).
#   * compare — the per-boundary field/timeout/order agreement between the
#     two evidence sets, which is the seed for the BW-00-04-06 report.
#
# Two run faces attempt the live launch and capture the real evidence when
# the gate is open; a closed gate records the missing piece. An absent
# reference tree or port source skips naming the ticket.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
cur_ref() { printf '%s' "${CI_COMPARE_REF:-${BW_REFERENCE:-$HOME/workspaces/BigWorld}}"; }
cur_out() { printf '%s' "${CI_COMPARE_OUT:-$CI_OUT}"; }
cfs_dir() { printf '%s/compare-fullstack' "$(cur_out)"; }

committed() { printf '%s/tests/baseline/%s' "$REPO_ROOT" "$1"; }

# The original launch gate: what a live original full stack needs. The
# reference ships the role binaries (incl. bots) but no server/bw.xml at the
# resource root and no machined config, so the gate is closed until those are
# authored.
original_run_gate() { # prints "open|detail" or "closed|detail"
    local ref machined bwxml detail=""
    ref="$(cur_ref)"
    [ -d "$ref" ] || { printf 'absent|no reference tree at %s' "$ref"; return 0; }

    if [ -x "$ref/game/bin/server/el7/tools/bwmachined2" ] \
        || [ -x "$ref/game/bin/server/el7/server/baseapp" ]; then
        machined="present"
    else
        machined="absent (no machined/role binary under game/bin/server/el7)"
    fi

    if [ -n "${BW_RES_PATH:-}" ] && [ -f "$BW_RES_PATH/server/bw.xml" ]; then
        bwxml="present ($BW_RES_PATH/server/bw.xml)"
    elif [ -f "$ref/game/res/bigworld/server/bw.xml" ]; then
        bwxml="present (game/res/bigworld/server/bw.xml)"
    else
        bwxml="absent (no server/bw.xml at the resource root; BW_RES_PATH=${BW_RES_PATH:-unset})"
    fi

    case "$machined" in present*) ;; *) detail="$machined" ;; esac
    case "$bwxml" in present*) ;; *) detail="${detail:+$detail; }$bwxml" ;; esac
    if [ -z "$detail" ]; then
        printf 'open|role binaries and a server/bw.xml resource root are present'
    else
        printf 'closed|%s' "$detail"
    fi
}

# Record the run face: launch the stack driver and capture its output when the
# gate is open, else record the closed gate naming the gap.
record_run() { # $1 side (original|new) $2 gate $3 detail $4 driver
    local side="$1" gate="$2" detail="$3" driver="$4"
    local runfile
    runfile="$(cfs_dir)/$side-run.json"
    if [ "$gate" != "open" ]; then
        cat >"$runfile" <<JSON
{
  "side": "$side",
  "run": "not run",
  "gate": "$gate",
  "gate_detail": "$detail"
}
JSON
        return 0
    fi
    local log
    log="$(mktemp -d "$REPO_ROOT/ci-out/$side-run.XXXXXX")/run.log"
    if ! "$driver" >"$log" 2>&1; then
        ci_fail "$side-run" "the $side stack driver failed: $(tail -1 "$log")"
        return 1
    fi
    local marker
    marker="$(grep -c 'full stack launched' "$log" || true)"
    cat >"$runfile" <<JSON
{
  "side": "$side",
  "run": "recorded",
  "gate": "open",
  "log": "$log",
  "marker": "$marker"
}
JSON
    return 0
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    ci_step "original and new full-stack comparison"

    # The frozen baselines and the port's source are the record's substrate.
    local missing="" f
    for f in \
        tests/baseline/protocol-format.json tests/baseline/callback-sequence.json \
        tests/baseline/fixture-database.json tests/baseline/fixture-entity.json \
        crates/mercury/src/interface_minder.rs; do
        [ -e "$REPO_ROOT/$f" ] || missing="$missing $f"
    done
    if [ -n "$missing" ]; then
        ci_fail compare-record \
            "comparison source missing:$missing — a record with a partial substrate is hollow"
        return 1
    fi

    local parser_root sum rows agree head
    parser_root="${CI_COMPARE_PARSER_ROOT:-$REPO_ROOT}"
    if ! sum="$(python3 "$REPO_ROOT/ci/compare-fullstack.py" "$parser_root" "$(cfs_dir)" --self-check 2>&1)"; then
        ci_fail compare-record "the comparator failed to record: $(printf '%s' "$sum" | tail -1)"
        return 1
    fi
    rows="$(printf '%s' "$sum" | sed -n 's/.*comparison=\([0-9]*\) rows.*/\1/p')"
    agree="$(printf '%s' "$sum" | sed -n 's/.*, \([0-9]*\) agree.*/\1/p')"
    head="$(printf '%s' "$sum" | sed -n 's/.*commit \([^)]*\)).*/\1/p')"
    if [ -z "$rows" ] || [ "$rows" -lt 1 ]; then
        ci_fail compare-record "recorded $rows comparison row(s) — a hollow comparison is not a record"
        return 1
    fi
    ci_pass compare-record \
        "$rows compatibility boundary(ies) compared, $agree agree (commit $head)"

    # Run faces: attempt the live launch where the gate is open.
    local ogate odetail
    rows="$(original_run_gate)"
    ogate="$(printf '%s' "$rows" | cut -d'|' -f1)"
    odetail="$(printf '%s' "$rows" | cut -d'|' -f2)"
    if [ "$ogate" = "open" ]; then
        record_run original "$ogate" "$odetail" \
            "$(cur_ref)/game/bin/server/el7/tools/bwmachined2" || return 1
        ci_pass original-run "the original full stack launched and its evidence was captured"
    else
        record_run original "$ogate" "$odetail" "" || return 1
        ci_skip original-run "$odetail — until each is authored no original run can be retained"
    fi

    # The new side's run gate is the port's own launch gate, already derived
    # into new-evidence.json.
    local ngate ndetail
    ngate="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["launch"]["gate"])' "$(cfs_dir)/new-evidence.json")"
    ndetail="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["launch"]["reason"])' "$(cfs_dir)/new-evidence.json")"
    if [ "$ngate" = "open" ]; then
        # The port's own binary lives beside the source the comparator read
        # (the repo, or the planted tree in a self-test).
        record_run new "$ngate" "$ndetail" "$parser_root/target/debug/boundless-server" || return 1
        ci_pass new-run "the new port's full stack launched and its evidence was captured"
    else
        record_run new "$ngate" "$ndetail" "" || return 1
        ci_skip new-run "$ndetail — the new run is retained when Mercury joins at BW-02"
    fi

    ci_note "both evidence sets recorded; original run: $ogate; new run: $ngate"

    # The committed tests/baseline/compare-*.json are the snapshot-time anchor
    # for the two evidence sets; a run that differs as the port grows is a note,
    # not a failure — the report lane BW-00-04-06 consumes the fresh run.
    local f fresh
    for f in compare-input.json original-evidence.json new-evidence.json compare-summary.json; do
        fresh="$(cfs_dir)/$f"
        if [ -f "$(committed "$f")" ] && [ -f "$fresh" ] && ! cmp -s "$(committed "$f")" "$fresh"; then
            ci_note "the committed tests/baseline/$f differs from this run — snapshot-time anchor, not a failure"
        fi
    done
}

# The frozen records the comparator reads: the same four baselines the real
# repo ships, planted at a scale that satisfies the self-check. The planted
# protocol and the planted Mercury registry are the same LoginInterface, so the
# comparison reports them as agreeing (the wire is the point of BW-00-04-04).
plant_baselines() { # $1 root
    local root="$1"
    mkdir -p "$root/tests/baseline"
    cat >"$root/tests/baseline/protocol-format.json" <<'JSON'
{
  "login_payload": {
    "protocol_version_bytes": "00 00 09 02",
    "login_message": {"id": 0, "length_style": "VARIABLE_LENGTH_MESSAGE", "length_param": 2},
    "probe_message": {"id": 1, "length_style": "FIXED_LENGTH_MESSAGE", "length_param": 0},
    "challenge_response_message": {"id": 2, "length_style": "VARIABLE_LENGTH_MESSAGE", "length_param": 2}
  }
}
JSON
    cat >"$root/tests/baseline/callback-sequence.json" <<'JSON'
{
  "call_sites": [
    {"file": "connection_control.cpp", "function": "connect", "stage": "INITIAL", "status": "NOT_SET"},
    {"file": "connection_control.cpp", "function": "onLoggedOn", "stage": "LOGIN", "status": "LOGGED_ON"},
    {"file": "connection_control.cpp", "function": "onBasePlayerCreated", "stage": "DATA", "status": "LOGGED_ON"},
    {"file": "connection_control.cpp", "function": "disconnected", "stage": "DISCONNECTED", "status": "NOT_SET"}
  ],
  "runtime_paths": ["login", "onLoggedOn", "onBasePlayerCreated"]
}
JSON
    cat >"$root/tests/baseline/fixture-database.json" <<'JSON'
{
  "sets": [
    {
      "logon_mapping": [
        {"logOnName": "FantasyAccount", "password": "a", "type": "Account", "entityID": 2}
      ],
      "tables": {"Account": [{"playerName": "FantasyAccount", "databaseID": 2}]}
    }
  ]
}
JSON
    cat >"$root/tests/baseline/fixture-entity.json" <<'JSON'
{
  "sets": [
    {
      "entities": {
        "Account": {"properties": ["playerName"], "clientMethods": ["chatMessage"]},
        "Avatar": {"properties": ["playerName"], "cellMethods": ["cellTick"]}
      }
    }
  ],
  "reference_commit": "planted"
}
JSON
}

# A planted reference tree with the launch gate open (machined stub + bw.xml)
# and a planted port whose server runtime consumes Mercury, so both run faces
# can actually launch.
plant_tree() { # $1 root  $2 kind
    local root="$1" kind="$2"
    mkdir -p "$root/game/bin/server/el7/tools" \
        "$root/game/bin/server/el7/server" \
        "$root/programming/bigworld/scripts" \
        "$root/crates/mercury/src" \
        "$root/crates/entitydef/src" \
        "$root/crates/server-runtime/src" \
        "$root/target/debug"
    plant_baselines "$root"

    printf '#!/bin/sh\necho "full stack launched: original"\nexit 0\n' \
        >"$root/game/bin/server/el7/tools/bwmachined2"
    chmod +x "$root/game/bin/server/el7/tools/bwmachined2"
    printf '#!/bin/sh\nexit 0\n' >"$root/game/bin/server/el7/server/baseapp"
    chmod +x "$root/game/bin/server/el7/server/baseapp"

    if [ "$kind" = "with substrate" ]; then
        mkdir -p "$root/res/server"
        printf '<root/>' >"$root/res/server/bw.xml"
    fi

    # The port's Mercury registry: the deterministic login_minder() test. The
    # multi-line .add calls and the 4-space closing brace mirror the real
    # crate, which is what the parser's regex expects.
    cat >"$root/crates/mercury/src/interface_minder.rs" <<'RS'
    fn login_minder() -> InterfaceMinder {
        let minder = InterfaceMinder::new("LoginInterface");
        minder.add(
            "login",
            VARIABLE_LENGTH_MESSAGE,
            2,
            Some(Rc::new(NullHandler)),
        );
        minder.add("probe", FIXED_LENGTH_MESSAGE, 0, Some(Rc::new(NullHandler)));
        minder.add(
            "challengeResponse",
            VARIABLE_LENGTH_MESSAGE,
            2,
            Some(Rc::new(NullHandler)),
        );
        minder
    }
RS
    printf 'pub struct EntityTypeRegistry;\n' >"$root/crates/entitydef/src/entities.rs"

    if [ "$kind" = "with substrate" ]; then
        printf '//! the server runtime\nuse mercury::NetworkInterface;\npub fn run() {}\n' \
            >"$root/crates/server-runtime/src/app.rs"
        printf '#!/bin/sh\necho "full stack launched: new"\nexit 0\n' \
            >"$root/target/debug/boundless-server"
        chmod +x "$root/target/debug/boundless-server"
    else
        printf 'pub fn run() {}\n' >"$root/crates/server-runtime/src/app.rs"
    fi
}

self_test() {
    local fails=0 rc r T M r2 T2 M2
    ci_step "self-test: a runnable original and a Mercury-wired port record their runs"
    r="$(mktemp -d "$REPO_ROOT/ci-out/compare-selftest.XXXXXX")"
    T="$r/BigWorld"
    M="$r/manifest/manifest.tsv"
    plant_tree "$T" "with substrate"

    BW_RES_PATH="$T/res" CI_COMPARE_REF="$T" CI_COMPARE_PARSER_ROOT="$T" \
        CI_COMPARE_OUT="$r/out" CI_OUT="$r/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'compare-record	PASS	' "$M" \
        && grep -q 'original-run	PASS	' "$M" \
        && grep -q 'new-run	PASS	' "$M" \
        && grep -q '"run": "recorded"' "$r/out/compare-fullstack/original-run.json" \
        && grep -q '"run": "recorded"' "$r/out/compare-fullstack/new-run.json"; then
        ci_pass selftest-compare-runs "a substrate-bearing tree and a Mercury-wired port record both live runs"
    else
        ci_fail selftest-compare-runs "planted runnable stacks did not record: rc=$rc $(grep -E 'compare-|run' "$M" 2>/dev/null | head -2 | tr '\n' '; ')"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a tree with no launch substrate closes the original run gate"
    r2="$(mktemp -d "$REPO_ROOT/ci-out/compare-selftest.XXXXXX")"
    T2="$r2/BigWorld"
    M2="$r2/manifest/manifest.tsv"
    plant_tree "$T2" "with substrate"
    rm -rf "$T2/res"
    CI_COMPARE_REF="$T2" CI_COMPARE_PARSER_ROOT="$T2" \
        CI_COMPARE_OUT="$r2/out" CI_OUT="$r2/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'original-run	SKIP	.*server/bw.xml' "$M2" \
        && grep -q '"gate": "closed"' "$r2/out/compare-fullstack/original-run.json" \
        && grep -q 'compare-record	PASS	' "$M2"; then
        ci_pass selftest-compare-orig-gate "a tree with no bw.xml closes the original gate and still records the comparison"
    else
        ci_fail selftest-compare-orig-gate "a missing bw.xml did not close the gate: rc=$rc $(grep 'original-run' "$M2" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a port with no Mercury wiring closes the new run gate"
    r2="$(mktemp -d "$REPO_ROOT/ci-out/compare-selftest.XXXXXX")"
    T2="$r2/BigWorld"
    M2="$r2/manifest/manifest.tsv"
    plant_tree "$T2" "no substrate"
    CI_COMPARE_REF="$T2" CI_COMPARE_PARSER_ROOT="$T2" \
        CI_COMPARE_OUT="$r2/out" CI_OUT="$r2/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'new-run	SKIP	.*BW-02' "$M2" \
        && grep -q '"gate": "closed"' "$r2/out/compare-fullstack/new-run.json"; then
        ci_pass selftest-compare-new-gate "a port with no Mercury wiring closes the new gate naming BW-02"
    else
        ci_fail selftest-compare-new-gate "a Mercury-less port did not close the gate: rc=$rc $(grep 'new-run' "$M2" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: no reference tree still records from the frozen baselines and skips the original run"
    r2="$(mktemp -d "$REPO_ROOT/ci-out/compare-selftest.XXXXXX")"
    T2="$r2/none"
    M2="$r2/manifest/manifest.tsv"
    CI_COMPARE_REF="$T2" CI_COMPARE_OUT="$r2/out" CI_OUT="$r2/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'compare-record	PASS	' "$M2" \
        && grep -q 'original-run	SKIP	.*no reference tree' "$M2"; then
        ci_pass selftest-compare-absent "an absent tree records from the frozen baselines and skips the live original run"
    else
        ci_fail selftest-compare-absent "an absent tree did not behave: rc=$rc $(grep -E 'compare-record|original-run' "$M2" 2>/dev/null | head -2 | tr '\n' '; ')"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$r2" "$REPO_ROOT/ci-out/compare-selftest".*

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
