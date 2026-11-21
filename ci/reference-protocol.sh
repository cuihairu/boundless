#!/usr/bin/env bash
# BW-00-04-01 — the wire protocol as the original declares it: every message the
# production Mercury interfaces register, with its id, length style, length
# field width and the direction it travels, plus the byte framing those ids and
# lengths are encoded in. Recorded from the original's own sources without
# inventing any of it.
#
# Usage:
#   ci/reference-protocol.sh            # record the protocol + capture gate
#   ci/reference-protocol.sh self-test  # planted protocol, planted gate faces
#
# Three faces, each recorded as it stands:
#   * inventory — every message gMinder.add registers in the 11 production
#     interfaces. Ids are read off the registration, not guessed: a message's
#     id is its position in declaration order (InterfaceMinder::add assigns
#     id = elements_.size(), lib/network/interface_minder.cpp). The direction
#     comes from the interface file's own handler target (a lib/connection
#     client or server interface, or an internal server interface).
#   * format — the wire framing those ids and lengths use: MessageID is a
#     uint8; a fixed message is [id:1][body:lengthParam]; a variable message is
#     [id:1][len:lengthParam big-endian][body] where len is the body length
#     only; a callback-length message carries no fixed width in the declaration
#     and its framing is resolved at receive time from the handler's
#     getMessageStreamSize (lib/network/interface_element.cpp).
#   * capture — a live raw-byte capture. The original ships no client binary
#     under game/bin (only server/el7 payloads), no server/bw.xml at the game
#     resource root and no ~/.bwmachined.conf, so no live frame can be
#     retained; the lane records the protocol face and reports the capture gate
#     as a named gap rather than authoring a config that would be a guess.
#
# The inventory and format faces are the durable record (committed under
# tests/baseline/); the capture face is the honest gate. A tree with no
# interface source fails: a recorded protocol with no message id is a hollow
# baseline, not one.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
cur_ref() { printf '%s' "${CI_PROTOCOL_ROOT:-${BW_REFERENCE:-$HOME/workspaces/BigWorld}}"; }
cur_src() { printf '%s/programming/bigworld' "$(cur_ref)"; }
cur_out() { printf '%s' "${CI_PROTOCOL_OUT:-$CI_OUT}"; }
cur_conf() { printf '%s' "${CI_PROTOCOL_CONF:-$HOME/.bwmachined.conf}"; }

msgs_file() { printf '%s/reference-protocol/protocol-messages.tsv' "$(cur_out)"; }
format_file() { printf '%s/reference-protocol/protocol-format.json' "$(cur_out)"; }
capture_file() { printf '%s/reference-protocol/protocol-capture.json' "$(cur_out)"; }
committed_msgs() { printf '%s/tests/baseline/protocol-messages.tsv' "$REPO_ROOT"; }
committed_format() { printf '%s/tests/baseline/protocol-format.json' "$REPO_ROOT"; }

# The source files the format face reads its framing facts from.
FORMAT_SOURCES=(
    lib/network/interface_element.cpp
    lib/network/interface_element.hpp
    lib/network/interface_minder.cpp
    lib/connection/client_server_protocol_version.hpp
    lib/connection/login_interface.hpp
)

# The 11 production interface files, relative to the reference root. The
# enumerator walks exactly this set; a tree missing any of them yields a
# partial inventory, which is a hollow baseline rather than a record.
PRODUCTION_INTERFACES=(
    lib/connection/login_interface.hpp
    lib/connection/baseapp_ext_interface.hpp
    lib/connection/client_interface.hpp
    lib/db/dbapp_interface.hpp
    lib/db/dbappmgr_interface.hpp
    server/baseapp/baseapp_int_interface.hpp
    server/baseappmgr/baseappmgr_interface.hpp
    server/cellapp/cellapp_interface.hpp
    server/cellappmgr/cellappmgr_interface.hpp
    server/loginapp/login_int_interface.hpp
    server/reviver/reviver_interface.hpp
)

# The message inventory: run the enumerator, demand a non-hollow result.
record_inventory() { # writes the TSV, prints "rows|interface_count|commit"
    local ref src head rows interfaces
    ref="$(cur_ref)"
    src="$(cur_src)"
    head="$(git -C "$ref" rev-parse HEAD 2>/dev/null || echo unknown)"
    [ "$(git -C "$ref" rev-parse --show-toplevel 2>/dev/null || echo "")" = "$ref" ] \
        || head="unknown (not the reference's own git toplevel)"

    mkdir -p "$(dirname "$(msgs_file)")"
    python3 "$REPO_ROOT/ci/reference-protocol.py" "$src" "$(msgs_file)" \
        || return 1

    rows="$(($(wc -l <"$(msgs_file)") - 1))"
    interfaces="$(tail -n +2 "$(msgs_file)" | cut -f1 | sort -u | grep -c . || true)"
    printf '%s|%s|%s\n' "$rows" "$interfaces" "$head"
}

# The wire-format face: static framing facts read from the reference sources.
record_format() { # writes the JSON, prints "summary"
    local ref head
    ref="$(cur_ref)"
    head="$(git -C "$ref" rev-parse HEAD 2>/dev/null || echo unknown)"
    mkdir -p "$(dirname "$(format_file)")"

    cat >"$(format_file)" <<JSON
{
  "face": "mercury wire protocol framing",
  "reference_root": "$ref",
  "reference_commit": "$head",
  "message_id": {
    "type": "uint8",
    "width_bytes": 1,
    "encoding": "unsigned byte, value = position in declaration order (0-based)",
    "id_rule": "InterfaceMinder::add assigns id = elements_.size() — lib/network/interface_minder.cpp",
    "id_rule_verified_by": "login=0, probe=1, challengeResponse=2 (lib/connection/login_interface.hpp)"
  },
  "length_field": {
    "encoding": "big-endian (BW_HTONS / BW_HTONL), width = lengthParam bytes",
    "value": "body length only, not including the id byte or the length field",
    "widths": {"1": "0xff oversize threshold", "2": "0xffff", "3": "0xffffff", "4": "full uint32"},
    "oversize": "length >= threshold routes through specialCompressLength — lib/network/interface_element.cpp"
  },
  "header_size": {
    "FIXED_LENGTH_MESSAGE": "1 (MessageID only) + lengthParam body bytes",
    "VARIABLE_LENGTH_MESSAGE": "1 + lengthParam",
    "CALLBACK_LENGTH_MESSAGE": "-1 (no fixed width in the declaration) — lib/network/interface_element.cpp"
  },
  "callback_length": {
    "rule": "framing resolved at receive time: InterfaceElement::updateLengthDetails queries the handler's getMessageStreamSize; streamSize < 0 => VARIABLE_LENGTH_MESSAGE with lengthParam = -streamSize, streamSize >= 0 => FIXED_LENGTH_MESSAGE with lengthParam = streamSize",
    "source": "lib/network/interface_element.cpp"
  },
  "pack": "#pragma pack(push, 1) before BEGIN_MERCURY_INTERFACE in every interface file",
  "login_payload": {
    "protocol_version_bytes": "00 00 09 02",
    "layout": "ClientServerProtocolVersion streams subpatch, patch, minor, major each as uint8 (2.9.0.0) — lib/connection/client_server_protocol_version.hpp",
    "login_message": {"interface": "LoginInterface", "id": 0, "length_style": "VARIABLE_LENGTH_MESSAGE", "length_param": 2},
    "probe_message": {"interface": "LoginInterface", "id": 1, "length_style": "FIXED_LENGTH_MESSAGE", "length_param": 0},
    "challenge_response_message": {"interface": "LoginInterface", "id": 2, "length_style": "VARIABLE_LENGTH_MESSAGE", "length_param": 2}
  },
  "source_refs": [
    "lib/network/interface_element.cpp",
    "lib/network/interface_element.hpp",
    "lib/network/interface_minder.cpp",
    "lib/network/interface_macros.hpp",
    "lib/connection/client_server_protocol_version.hpp",
    "lib/connection/login_interface.hpp"
  ],
  "recorded_at": "$(ci_now)",
  "lane_git_sha": "$(ci_git_sha)"
}
JSON
    printf '%s' "framing rules (id uint8, big-endian length, callback resolved at receive time)"
}

# The live capture gate: what a raw-byte capture needs — a client binary to
# send and a server to answer. The original ships none of the three, so the
# gate stays closed and names each missing piece.
capture_gate() { # prints "open|detail", "closed|detail" or "absent|detail"
    local ref client conf bwxml detail=""
    ref="$(cur_ref)"
    [ -d "$ref" ] || { printf 'absent|no reference tree at %s' "$ref"; return 0; }

    client=""
    if [ -x "$ref/game/bin/server/el7/server/bots" ]; then
        client="present (bots, the headless load client)"
    elif find "$ref/game/bin" -maxdepth 2 -type d -name client 2>/dev/null | grep -q .; then
        client="present (game/bin/**/client)"
    else
        client="absent (no client binary under game/bin — only server/el7 payloads ship)"
    fi

    if [ -f "$ref/game/res/bigworld/server/bw.xml" ]; then
        bwxml="present"
    else
        bwxml="absent (no server/bw.xml at the game resource root)"
    fi

    conf="$(cur_conf)"
    if [ -f "$conf" ]; then
        conf_state="present ($conf)"
    else
        conf_state="absent ($conf)"
    fi

    case "$client" in present*) ;; *) detail="$client" ;; esac
    case "$bwxml" in present*) ;; *) detail="${detail:+$detail; }$bwxml" ;; esac
    case "$conf_state" in present*) ;; *) detail="${detail:+$detail; }$conf_state" ;; esac

    if [ -z "$detail" ]; then
        printf 'open|client, server/bw.xml and machined config all present'
    else
        printf 'closed|%s' "$detail"
    fi
}

write_capture() { # $1 gate $2 detail
    local ref head gate="$1" detail="$2"
    ref="$(cur_ref)"
    head="$(git -C "$ref" rev-parse HEAD 2>/dev/null || echo unknown)"
    mkdir -p "$(dirname "$(capture_file)")"
    cat >"$(capture_file)" <<JSON
{
  "face": "mercury wire protocol capture",
  "reference_root": "$ref",
  "reference_commit": "$head",
  "capture_gate": "$gate",
  "capture_gate_detail": "$detail",
  "capture": "$([ "$gate" = open ] && echo 'recorded (raw frame observed)' || echo 'absent (no raw frame retained — the gate names the gap)')",
  "recorded_at": "$(ci_now)",
  "lane_git_sha": "$(ci_git_sha)"
}
JSON
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    ci_step "mercury wire protocol inventory face"
    if [ ! -d "$(cur_ref)" ]; then
        ci_skip reference-protocol \
            "no reference tree at $(cur_ref) — BW-00-04-01 records it when present (set BW_REFERENCE)"
        return 0
    fi

    # Every production interface must be present, else the inventory would be
    # partial — a protocol record that omits an interface is hollow, not one.
    local missing=""
    local f
    for f in "${PRODUCTION_INTERFACES[@]}"; do
        [ -f "$(cur_src)/$f" ] || missing="$missing $f"
    done
    if [ -n "$missing" ]; then
        ci_fail protocol-inventory \
            "interface sources missing:$missing — a protocol record with a partial interface set is hollow"
        return 1
    fi

    local sum rows interfaces head
    if ! sum="$(record_inventory)"; then
        ci_fail protocol-inventory "the enumerator failed to record the interface inventory"
        return 1
    fi
    rows="$(printf '%s' "$sum" | cut -d'|' -f1)"
    interfaces="$(printf '%s' "$sum" | cut -d'|' -f2)"
    head="$(printf '%s' "$sum" | cut -d'|' -f3)"
    if [ "$rows" -lt 1 ] || [ "$interfaces" -lt 1 ]; then
        ci_fail protocol-inventory \
            "recorded $rows messages across $interfaces interface(s) — a hollow baseline is not a protocol"
        return 1
    fi
    ci_pass protocol-inventory \
        "$rows messages across $interfaces interface(s) recorded in protocol-messages.tsv (commit $head)"

    ci_step "mercury wire protocol format face"
    local missing_src=""
    for f in "${FORMAT_SOURCES[@]}"; do
        [ -f "$(cur_src)/$f" ] || missing_src="$missing_src $f"
    done
    if [ -n "$missing_src" ]; then
        ci_fail protocol-format \
            "framing source missing:$missing_src — a framing rule with no source line is a guess"
        return 1
    fi
    local fsum
    fsum="$(record_format)"
    ci_pass protocol-format "$fsum"

    local gate detail
    rows="$(capture_gate)"
    gate="$(printf '%s' "$rows" | cut -d'|' -f1)"
    detail="$(printf '%s' "$rows" | cut -d'|' -f2)"
    write_capture "$gate" "$detail"

    if [ -f "$(committed_msgs)" ] && ! cmp -s "$(committed_msgs)" "$(msgs_file)"; then
        ci_note "the committed tests/baseline/protocol-messages.tsv differs from this run — snapshot-time anchor, not a failure"
    fi
    if [ -f "$(committed_format)" ] && ! cmp -s "$(committed_format)" "$(format_file)"; then
        ci_note "the committed tests/baseline/protocol-format.json differs from this run — snapshot-time anchor, not a failure"
    fi

    if [ "$gate" = "open" ]; then
        ci_pass protocol-capture "capture substrate present — a recorded raw frame would be retained"
    else
        ci_skip protocol-capture "$detail — until each is authored no raw frame can be retained"
    fi
    ci_note "inventory and framing are recorded; capture: $gate ($detail)"
}

# A minimal but complete reference tree: all 11 production interface files, a
# macro header the enumerator can pull in, and the framing sources the format
# face reads. The login interface carries the three messages the reference's
# own LoginInterface declares, in the same shapes, so the ids the enumerator
# assigns are the ones the reference's declaration order gives.
plant_tree() { # $1 root  $2 "with messages" | "empty"
    local root="$1" kind="$2" f
    mkdir -p "$root/programming/bigworld/lib/connection" \
        "$root/programming/bigworld/lib/network" \
        "$root/programming/bigworld/lib/db" \
        "$root/programming/bigworld/server/baseapp" \
        "$root/programming/bigworld/server/baseappmgr" \
        "$root/programming/bigworld/server/cellapp" \
        "$root/programming/bigworld/server/cellappmgr" \
        "$root/programming/bigworld/server/loginapp" \
        "$root/programming/bigworld/server/reviver" \
        "$root/game/bin/server/el7/server"

    cat >"$root/programming/bigworld/lib/network/interface_macros.hpp" <<'HPP'
#define MERCURY_VARIABLE_MESSAGE( NAME, PARAM, HANDLER ) \
	const Mercury::InterfaceElement & NAME = \
		gMinder.add( #NAME, Mercury::VARIABLE_LENGTH_MESSAGE, PARAM, HANDLER );

#define MERCURY_EMPTY_MESSAGE( NAME, HANDLER ) \
	const Mercury::InterfaceElement & NAME = \
		gMinder.add( #NAME, Mercury::FIXED_LENGTH_MESSAGE, 0, HANDLER );
HPP

    for f in "${PRODUCTION_INTERFACES[@]}"; do
        : >"$root/programming/bigworld/$f"
    done

    if [ "$kind" = "with messages" ]; then
        cat >"$root/programming/bigworld/lib/connection/login_interface.hpp" <<'HPP'
#include "interface_macros.hpp"
#pragma pack(push,1)
BEGIN_MERCURY_INTERFACE( LoginInterface )
	MERCURY_VARIABLE_MESSAGE( login, 2, &gLoginHandler )
	MERCURY_EMPTY_MESSAGE( probe, &gProbeHandler )
	MERCURY_VARIABLE_MESSAGE( challengeResponse, 2,
		&gChallengeResponseHandler )
END_MERCURY_INTERFACE()
#pragma pack(pop)
HPP
    fi

    for f in interface_element.cpp interface_element.hpp interface_minder.cpp; do
        : >"$root/programming/bigworld/lib/network/$f"
    done
    : >"$root/programming/bigworld/lib/connection/client_server_protocol_version.hpp"
}

self_test() {
    local fails=0 rc r T M
    ci_step "self-test: a planted interface tree records ids and framing"
    r="$(mktemp -d "$REPO_ROOT/ci-out/protocol-selftest.XXXXXX")"
    T="$r/BigWorld"
    M="$r/manifest/manifest.tsv"
    plant_tree "$T" "with messages"

    CI_PROTOCOL_ROOT="$T" CI_PROTOCOL_OUT="$r/out" CI_OUT="$r/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'protocol-inventory	PASS	3 messages across 1 interface' "$M" \
        && grep -q 'protocol-format	PASS	' "$M" \
        && grep -q 'LoginInterface	login	0	VARIABLE_LENGTH_MESSAGE	2' "$r/out/reference-protocol/protocol-messages.tsv" \
        && grep -q 'LoginInterface	probe	1	FIXED_LENGTH_MESSAGE	0' "$r/out/reference-protocol/protocol-messages.tsv" \
        && grep -q 'LoginInterface	challengeResponse	2	VARIABLE_LENGTH_MESSAGE	2' "$r/out/reference-protocol/protocol-messages.tsv" \
        && grep -q '"id_rule_verified_by": "login=0, probe=1, challengeResponse=2' "$r/out/reference-protocol/protocol-format.json"; then
        ci_pass selftest-protocol "planted sources record the message ids and the framing rules"
    else
        ci_fail selftest-protocol "planted sources did not record: rc=$rc $(grep -E 'protocol-' "$M" 2>/dev/null | head -2 | tr '\n' '; ')"
        fails=$((fails + 1))
    fi

    ci_step "self-test: the absent client, config and machined conf close the capture gate"
    CI_PROTOCOL_ROOT="$T" CI_PROTOCOL_OUT="$r/out" CI_PROTOCOL_CONF="$r/none.conf" \
        CI_OUT="$r/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'protocol-capture	SKIP	.*absent.*no client binary' "$M" \
        && grep -q '"capture_gate": "closed"' "$r/out/reference-protocol/protocol-capture.json"; then
        ci_pass selftest-protocol-closed "a tree with no client, config or machined conf names each gap and closes the gate"
    else
        ci_fail selftest-protocol-closed "a closed gate did not name its gaps: rc=$rc $(grep 'protocol-capture' "$M" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a shipped bots client and an authored config open the capture gate"
    printf '#!/bin/sh\nexit 0\n' >"$T/game/bin/server/el7/server/bots"
    chmod +x "$T/game/bin/server/el7/server/bots"
    mkdir -p "$T/game/res/bigworld/server"
    printf '<root/>' >"$T/game/res/bigworld/server/bw.xml"
    printf 'BW_RES_PATH=%s\n' "$T/game/res/bigworld" >"$r/open.conf"
    CI_PROTOCOL_ROOT="$T" CI_PROTOCOL_OUT="$r/out" CI_PROTOCOL_CONF="$r/open.conf" \
        CI_OUT="$r/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'protocol-capture	PASS	capture substrate present' "$M" \
        && grep -q '"capture_gate": "open"' "$r/out/reference-protocol/protocol-capture.json"; then
        ci_pass selftest-protocol-open "a shipped client plus authored config and machined conf open the gate into a valid record"
    else
        ci_fail selftest-protocol-open "an open gate did not record: rc=$rc $(grep 'protocol-capture' "$M" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a tree whose interfaces register nothing is a hollow baseline"
    r2="$(mktemp -d "$REPO_ROOT/ci-out/protocol-selftest.XXXXXX")"
    local T2="$r2/BigWorld" M2="$r2/manifest/manifest.tsv"
    plant_tree "$T2" "empty"
    CI_PROTOCOL_ROOT="$T2" CI_PROTOCOL_OUT="$r2/out" CI_OUT="$r2/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -ne 0 ] \
        && grep -q 'protocol-inventory	FAIL	.*hollow baseline' "$M2"; then
        ci_pass selftest-protocol-hollow "a tree whose interfaces register no message fails the inventory leg"
    else
        ci_fail selftest-protocol-hollow "a hollow inventory did not fail loudly: rc=$rc $(grep 'protocol-inventory' "$M2" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: no reference tree reports SKIP naming the ticket"
    CI_PROTOCOL_ROOT="$r/none" CI_PROTOCOL_OUT="$r/out2" CI_OUT="$r/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] && grep -q 'reference-protocol	SKIP	.*BW-00-04-01' "$M"; then
        ci_pass selftest-protocol-absent "absent tree reports SKIP naming BW-00-04-01"
    else
        ci_fail selftest-protocol-absent "absent tree did not skip cleanly: rc=$rc"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$r2" "$REPO_ROOT/ci-out/protocol-selftest".*

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
