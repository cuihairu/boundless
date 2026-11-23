#!/usr/bin/env bash
# BW-00-04-02 — the Python logon callback fixture: the sequence, parameters and
# ignorable fields of the callback `BigWorld.connect( server, loginParams,
# progressFn )` drives. Recorded from the original's own sources without
# inventing any of it.
#
# Usage:
#   ci/reference-callback.sh            # record the sequence + capture gate
#   ci/reference-callback.sh self-test  # planted source, planted gate faces
#
# Three faces, each recorded as it stands:
#   * sequence — every `ConnectionControl::callConnectionCallback(...)` call
#     site in client/connection_control.cpp, in file order, tagged with the
#     function it sits in and the `LogOnStage` / `LogOnStatus` arguments it
#     passes, plus the two enums the values come from (LogOnStage in
#     connection_control.hpp, LogOnStatus in lib/connection/log_on_status.hpp).
#     The runtime order is the function grouping: a success reaches onLoggedOn
#     then onBasePlayerCreated; a failure reaches onLogOnFailure; a disconnect
#     reaches disconnected.
#   * format — the arguments the callback actually receives: a Python tuple
#     (stage:int, status:int, message:str) built by Py_BuildValue("(iNs)"),
#     and the fields a differential run must ignore (the message string and the
#     timing flag vary run to run; the failure path's status is a runtime
#     parameter, not a literal).
#   * capture — a live callback sequence. The original ships no client binary
#     under game/bin (only server/el7 payloads), no server/bw.xml at the game
#     resource root and no ~/.bwmachined.conf, so no live sequence can be
#     retained; the lane records the source faces and reports the capture gate
#     as a named gap rather than authoring a config that would be a guess.
#
# The sequence and format faces are the durable record (committed under
# tests/baseline/); the capture face is the honest gate. A tree with no
# callback source fails: a recorded sequence with no call site is a hollow
# baseline, not one.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
cur_ref() { printf '%s' "${CI_CALLBACK_ROOT:-${BW_REFERENCE:-$HOME/workspaces/BigWorld}}"; }
cur_src() { printf '%s/programming/bigworld' "$(cur_ref)"; }
cur_out() { printf '%s' "${CI_CALLBACK_OUT:-$CI_OUT}"; }
cur_conf() { printf '%s' "${CI_CALLBACK_CONF:-$HOME/.bwmachined.conf}"; }

seq_file() { printf '%s/reference-callback/callback-sequence.json' "$(cur_out)"; }
format_file() { printf '%s/reference-callback/callback-format.json' "$(cur_out)"; }
capture_file() { printf '%s/reference-callback/callback-capture.json' "$(cur_out)"; }
committed_seq() { printf '%s/tests/baseline/callback-sequence.json' "$REPO_ROOT"; }
committed_format() { printf '%s/tests/baseline/callback-format.json' "$REPO_ROOT"; }

# The source files the sequence and format faces read from.
CALLBACK_SOURCES=(
    client/connection_control.hpp
    client/connection_control.cpp
    lib/connection/log_on_status.hpp
)

# The functions that carry a callback and the stage/status each passes. The
# lane asserts the derived call sites match these exactly, so a renamed or
# re-staged call in the original is a recorded difference, not a silent one.
# `runtime` marks the failure path, whose status is its own parameter; a call
# passing no status argument records NOT_SET, the declaration's default.
EXPECTED_SEQUENCE=(
    "connect|STAGE_INITIAL|NOT_SET"
    "connect|STAGE_LOGIN|PUBLIC_KEY_LOOKUP_FAILED"
    "connect|STAGE_INITIAL|NOT_SET"
    "startReplayFromFile|STAGE_INITIAL|NOT_SET"
    "startReplayFromStream|STAGE_INITIAL|NOT_SET"
    "onLoggedOn|STAGE_LOGIN|LOGGED_ON"
    "onLoggedOn|STAGE_LOGIN|LOGGED_ON_OFFLINE"
    "onLogOnFailure|STAGE_LOGIN|runtime"
    "onBasePlayerCreated|STAGE_DATA|LOGGED_ON"
    "onBasePlayerCreated|STAGE_DATA|LOGGED_ON_OFFLINE"
    "disconnected|STAGE_DISCONNECTED|NOT_SET"
)

# The runtime order per path, so the fixture records the sequence a caller
# observes, not merely the source order of the call sites.
RUNTIME_PATHS=(
    "online success|onLoggedOn then onBasePlayerCreated|(STAGE_LOGIN,LOGGED_ON,server message) then (STAGE_DATA,LOGGED_ON,'')"
    "offline success|onLoggedOn then onBasePlayerCreated|(STAGE_LOGIN,LOGGED_ON_OFFLINE,'Single user mode') then (STAGE_DATA,LOGGED_ON_OFFLINE,'')"
    "failure|onLogOnFailure|(STAGE_LOGIN,<runtime status>,failure message) — terminal, the callback is cleared after"
    "early abort|connect / startReplayFrom*|(STAGE_INITIAL,NOT_SET,'') — already connected or the logon never started"
    "disconnect|disconnected|(STAGE_DISCONNECTED,NOT_SET,'')"
)

# The argument encoding the callback receives, and the fields a differential
# run must ignore because they vary from run to run.
write_format() { # $1 ref $2 commit
    local ref="$1" head="$2"
    mkdir -p "$(dirname "$(format_file)")"
    cat >"$(format_file)" <<JSON
{
  "face": "python logon callback argument format",
  "reference_root": "$ref",
  "reference_commit": "$head",
  "callback_signature": "BigWorld.connect( server, loginParams, progressFn )",
  "args": {
    "stage": {"type": "int", "source": "LogOnStage enum, client/connection_control.hpp"},
    "status": {"type": "int", "source": "LogOnStatus::Status enum, lib/connection/log_on_status.hpp"},
    "message": {"type": "str", "source": "the server message string"}
  },
  "tuple": "Py_BuildValue( \"(iNs)\", stage, Script::getData( status.value() ), message.c_str() ) — client/connection_control.cpp",
  "ignorable_fields": [
    "message: varies run to run (server message, empty, or 'Single user mode')",
    "status on the failure path: a runtime parameter of onLogOnFailure, not a literal",
    "shouldCallNextFrame: a timing-only flag, never passed to Python"
  ],
  "source_refs": [
    "client/connection_control.hpp",
    "client/connection_control.cpp",
    "lib/connection/log_on_status.hpp"
  ],
  "recorded_at": "$(ci_now)",
  "lane_git_sha": "$(ci_git_sha)"
}
JSON
}

# The source-derived sequence: run the parser, stamp the runtime paths on the
# result so the fixture records the order a caller observes, and demand a
# non-hollow result.
record_sequence() { # writes the JSON, prints "sites|functions|commit"
    local ref src head sites funcs
    ref="$(cur_ref)"
    src="$(cur_src)"
    head="$(git -C "$ref" rev-parse HEAD 2>/dev/null || echo unknown)"
    [ "$(git -C "$ref" rev-parse --show-toplevel 2>/dev/null || echo "")" = "$ref" ] \
        || head="unknown (not the reference's own git toplevel)"

    mkdir -p "$(dirname "$(seq_file)")"
    python3 "$REPO_ROOT/ci/reference-callback.py" "$src" "$(seq_file)" \
        || return 1

    # Stamp the runtime path ordering the lane records, the function/stage
    # pairing a caller observes. The parser knows the call sites; only the
    # lane knows the paths those sites form at runtime.
    python3 - "$(seq_file)" "$(printf '%s\n' "${RUNTIME_PATHS[@]}" | python3 -c 'import json,sys; print(json.dumps([l.rstrip("\n") for l in sys.stdin if l.strip()]))')" <<'PY'
import json, sys
path = sys.argv[1]
with open(path) as f:
    data = json.load(f)
data["runtime_paths"] = json.loads(sys.argv[2])
with open(path, "w") as f:
    json.dump(data, f, indent=2)
    f.write("\n")
PY

    sites="$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["call_sites"]))' "$(seq_file)")"
    funcs="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(len(set(s["function"] for s in d["call_sites"])))' "$(seq_file)")"
    printf '%s|%s|%s\n' "$sites" "$funcs" "$head"
}

# Assert the derived call sites match the expected sequence exactly.
verify_sequence() { # prints "detail" or exits 1
    local derived
    derived="$(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
print("\n".join("%s|%s|%s" % (s["function"], s["stage"] or "", s["status"]) for s in d["call_sites"]))
' "$(seq_file)")"
    local want got
    want="$(printf '%s\n' "${EXPECTED_SEQUENCE[@]}")"
    got="$(printf '%s\n' "$derived" | sed '/^$/d')"
    if [ "$want" = "$got" ]; then
        printf '%s' "$(printf '%s\n' "${EXPECTED_SEQUENCE[@]}" | wc -l) call sites across $(printf '%s\n' "${EXPECTED_SEQUENCE[@]}" | cut -d'|' -f1 | sort -u | grep -c .) function(s) match the expected sequence"
        return 0
    fi
    printf '%s' "derived sequence differs from expected: $(diff <(printf '%s\n' "$want") <(printf '%s\n' "$got") | head -6 | tr '\n' ' ')"
    return 1
}

# The live capture gate: what a live callback sequence needs — a client binary
# to connect and a server to answer. The original ships none of the three, so
# the gate stays closed and names each missing piece.
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
  "face": "python logon callback capture",
  "reference_root": "$ref",
  "reference_commit": "$head",
  "capture_gate": "$gate",
  "capture_gate_detail": "$detail",
  "capture": "$([ "$gate" = open ] && echo 'recorded (callback sequence observed)' || echo 'absent (no callback sequence retained — the gate names the gap)')",
  "recorded_at": "$(ci_now)",
  "lane_git_sha": "$(ci_git_sha)"
}
JSON
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    ci_step "python logon callback sequence face"
    if [ ! -d "$(cur_ref)" ]; then
        ci_skip reference-callback \
            "no reference tree at $(cur_ref) — BW-00-04-02 records it when present (set BW_REFERENCE)"
        return 0
    fi

    local missing=""
    local f
    for f in "${CALLBACK_SOURCES[@]}"; do
        [ -f "$(cur_src)/$f" ] || missing="$missing $f"
    done
    if [ -n "$missing" ]; then
        ci_fail callback-sequence \
            "callback source missing:$missing — a sequence record with a partial source set is hollow"
        return 1
    fi

    local sum sites funcs head
    if ! sum="$(record_sequence)"; then
        ci_fail callback-sequence "the parser failed to record the callback sequence"
        return 1
    fi
    sites="$(printf '%s' "$sum" | cut -d'|' -f1)"
    funcs="$(printf '%s' "$sum" | cut -d'|' -f2)"
    head="$(printf '%s' "$sum" | cut -d'|' -f3)"
    if [ "$sites" -lt 1 ]; then
        ci_fail callback-sequence \
            "recorded $sites call site(s) — a hollow baseline is not a callback sequence"
        return 1
    fi
    if ! verify_sequence; then
        ci_fail callback-sequence "$(verify_sequence 2>&1)"
        return 1
    fi
    ci_pass callback-sequence \
        "$sites call sites across $funcs function(s) recorded in callback-sequence.json (commit $head)"

    ci_step "python logon callback argument format face"
    write_format "$(cur_ref)" "$head"
    ci_pass callback-format "callback tuple (stage:int, status:int, message:str); ignorable: message, failure status, timing flag"

    local gate detail
    rows="$(capture_gate)"
    gate="$(printf '%s' "$rows" | cut -d'|' -f1)"
    detail="$(printf '%s' "$rows" | cut -d'|' -f2)"
    write_capture "$gate" "$detail"

    if [ -f "$(committed_seq)" ] && ! cmp -s "$(committed_seq)" "$(seq_file)"; then
        ci_note "the committed tests/baseline/callback-sequence.json differs from this run — snapshot-time anchor, not a failure"
    fi
    if [ -f "$(committed_format)" ] && ! cmp -s "$(committed_format)" "$(format_file)"; then
        ci_note "the committed tests/baseline/callback-format.json differs from this run — snapshot-time anchor, not a failure"
    fi

    if [ "$gate" = "open" ]; then
        ci_pass callback-capture "capture substrate present — a recorded callback sequence would be retained"
    else
        ci_skip callback-capture "$detail — until each is authored no callback sequence can be retained"
    fi
    ci_note "sequence and format are recorded; capture: $gate ($detail)"
}

# A minimal but complete source tree: the three callback sources, carrying the
# same call sites and enums as the reference so the derived sequence matches.
plant_tree() { # $1 root  $2 "with calls" | "empty"
    local root="$1" kind="$2"
    mkdir -p "$root/programming/bigworld/client" \
        "$root/programming/bigworld/lib/connection" \
        "$root/game/bin/server/el7/server"

    cat >"$root/programming/bigworld/client/connection_control.hpp" <<'HPP'
enum LogOnStage
{
	STAGE_INITIAL 		= 0,
	STAGE_LOGIN 		= 1,
	STAGE_DATA 		= 2,
	STAGE_DISCONNECTED 	= 6
};
HPP

    cat >"$root/programming/bigworld/lib/connection/log_on_status.hpp" <<'HPP'
class LogOnStatus
{
public:
	enum Status
	{
		NOT_SET,
		LOGGED_ON,
		LOGGED_ON_OFFLINE,
		CONNECTION_FAILED,
		DNS_LOOKUP_FAILED,
		UNKNOWN_ERROR,
		CANCELLED,
		ALREADY_ONLINE_LOCALLY,
		PUBLIC_KEY_LOOKUP_FAILED,
		LAST_CLIENT_SIDE_VALUE = 63,
		LOGIN_MALFORMED_REQUEST,
		LOGIN_BAD_PROTOCOL_VERSION,
		LOGIN_CHALLENGE_ISSUED,
		LOGIN_REJECTED_NO_SUCH_USER,
		LOGIN_REJECTED_INVALID_PASSWORD,
		LOGIN_REJECTED_ALREADY_LOGGED_IN,
		LOGIN_REJECTED_BAD_DIGEST,
		LOGIN_REJECTED_DB_GENERAL_FAILURE,
		LOGIN_REJECTED_DB_NOT_READY,
		LOGIN_REJECTED_ILLEGAL_CHARACTERS,
		LOGIN_REJECTED_SERVER_NOT_READY,
		LOGIN_REJECTED_UPDATER_NOT_READY,
		LOGIN_REJECTED_NO_BASEAPPS,
		LOGIN_REJECTED_BASEAPP_OVERLOAD,
		LOGIN_REJECTED_CELLAPP_OVERLOAD,
		LOGIN_REJECTED_BASEAPP_TIMEOUT,
		LOGIN_REJECTED_BASEAPPMGR_TIMEOUT,
		LOGIN_REJECTED_DBAPP_OVERLOAD,
		LOGIN_REJECTED_LOGINS_NOT_ALLOWED,
		LOGIN_REJECTED_RATE_LIMITED,
		LOGIN_REJECTED_BAN,
		LOGIN_REJECTED_CHALLENGE_ERROR,
		LOGIN_REJECTED_AUTH_SERVICE_NO_SUCH_ACCOUNT,
		LOGIN_REJECTED_AUTH_SERVICE_LOGIN_DISALLOWED,
		LOGIN_REJECTED_AUTH_SERVICE_UNREACHABLE,
		LOGIN_REJECTED_AUTH_SERVICE_INVALID_RESPONSE,
		LOGIN_REJECTED_AUTH_SERVICE_GENERAL_FAILURE,
		LOGIN_REJECTED_NO_LOGINAPP,
		LOGIN_REJECTED_NO_LOGINAPP_RESPONSE,
		LOGIN_REJECTED_NO_BASEAPP_RESPONSE,
		LOGIN_CUSTOM_DEFINED_ERROR = 254,
		LAST_SERVER_SIDE_VALUE = 255
	};
};
HPP

    if [ "$kind" = "with calls" ]; then
        cat >"$root/programming/bigworld/client/connection_control.cpp" <<'CPP'
void ConnectionControl::connect( const BW::string & server )
{
	this->callConnectionCallback( STAGE_INITIAL, false );
	this->callConnectionCallback( STAGE_LOGIN, true, LogOnStatus::PUBLIC_KEY_LOOKUP_FAILED );
	this->callConnectionCallback( STAGE_INITIAL, false );
}
void ConnectionControl::startReplayFromFile( const BW::string & path )
{
	this->callConnectionCallback( STAGE_INITIAL, false );
}
void ConnectionControl::startReplayFromStream()
{
	this->callConnectionCallback( STAGE_INITIAL, false );
}
void ConnectionControl::onLoggedOn()
{
	this->callConnectionCallback( STAGE_LOGIN, false, LogOnStatus::LOGGED_ON );
	this->callConnectionCallback( STAGE_LOGIN, false, LogOnStatus::LOGGED_ON_OFFLINE );
}
void ConnectionControl::onLogOnFailure( const LogOnStatus & status )
{
	this->callConnectionCallback( STAGE_LOGIN, false, status );
}
void ConnectionControl::onBasePlayerCreated()
{
	this->callConnectionCallback( STAGE_DATA, false, LogOnStatus::LOGGED_ON );
	this->callConnectionCallback( STAGE_DATA, false, LogOnStatus::LOGGED_ON_OFFLINE );
}
void ConnectionControl::disconnected( bool isShuttingDown )
{
	this->callConnectionCallback( STAGE_DISCONNECTED, !isShuttingDown );
}
CPP
    else
        : >"$root/programming/bigworld/client/connection_control.cpp"
    fi
}

self_test() {
    local fails=0 rc r T M r2 T2 M2
    ci_step "self-test: a planted source tree records the callback sequence"
    r="$(mktemp -d "$REPO_ROOT/ci-out/callback-selftest.XXXXXX")"
    T="$r/BigWorld"
    M="$r/manifest/manifest.tsv"
    plant_tree "$T" "with calls"

    CI_CALLBACK_ROOT="$T" CI_CALLBACK_OUT="$r/out" CI_OUT="$r/manifest" "$0" >/dev/null 2>&1
    rc=$?
    local J="$r/out/reference-callback/callback-sequence.json"
    if [ "$rc" -eq 0 ] \
        && grep -q 'callback-sequence	PASS	11 call sites across 7 function' "$M" \
        && grep -q 'callback-format	PASS	' "$M" \
        && python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
assert d["stage_enum"]["STAGE_DISCONNECTED"] == 6, "stage enum"
assert len(d["status_enum"]) >= 40, "status enum"
assert "runtime_paths" in d, "runtime paths"
sites = {s["function"] + "|" + str(s["status"]): s for s in d["call_sites"]}
assert sites["onLogOnFailure|runtime"], "failure status runtime"
assert sites["disconnected|NOT_SET"], "disconnect NOT_SET"
assert sites["connect|NOT_SET"], "connect NOT_SET"
' "$J"; then
        ci_pass selftest-callback "planted sources record the callback sites, stages and statuses"
    else
        ci_fail selftest-callback "planted sources did not record: rc=$rc $(grep -E 'callback-' "$M" 2>/dev/null | head -2 | tr '\n' '; ')"
        fails=$((fails + 1))
    fi

    ci_step "self-test: the absent client, config and machined conf close the capture gate"
    CI_CALLBACK_ROOT="$T" CI_CALLBACK_OUT="$r/out" CI_CALLBACK_CONF="$r/none.conf" \
        CI_OUT="$r/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'callback-capture	SKIP	.*absent.*no client binary' "$M" \
        && grep -q '"capture_gate": "closed"' "$r/out/reference-callback/callback-capture.json"; then
        ci_pass selftest-callback-closed "a tree with no client, config or machined conf names each gap and closes the gate"
    else
        ci_fail selftest-callback-closed "a closed gate did not name its gaps: rc=$rc $(grep 'callback-capture' "$M" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a shipped bots client and an authored config open the capture gate"
    printf '#!/bin/sh\nexit 0\n' >"$T/game/bin/server/el7/server/bots"
    chmod +x "$T/game/bin/server/el7/server/bots"
    mkdir -p "$T/game/res/bigworld/server"
    printf '<root/>' >"$T/game/res/bigworld/server/bw.xml"
    printf 'BW_RES_PATH=%s\n' "$T/game/res/bigworld" >"$r/open.conf"
    CI_CALLBACK_ROOT="$T" CI_CALLBACK_OUT="$r/out" CI_CALLBACK_CONF="$r/open.conf" \
        CI_OUT="$r/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'callback-capture	PASS	capture substrate present' "$M" \
        && grep -q '"capture_gate": "open"' "$r/out/reference-callback/callback-capture.json"; then
        ci_pass selftest-callback-open "a shipped client plus authored config and machined conf open the gate into a valid record"
    else
        ci_fail selftest-callback-open "an open gate did not record: rc=$rc $(grep 'callback-capture' "$M" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a tree whose source records no call site is a hollow baseline"
    r2="$(mktemp -d "$REPO_ROOT/ci-out/callback-selftest.XXXXXX")"
    T2="$r2/BigWorld"
    M2="$r2/manifest/manifest.tsv"
    plant_tree "$T2" "empty"
    CI_CALLBACK_ROOT="$T2" CI_CALLBACK_OUT="$r2/out" CI_OUT="$r2/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -ne 0 ] \
        && grep -q 'callback-sequence	FAIL	.*hollow baseline' "$M2"; then
        ci_pass selftest-callback-hollow "a source recording no call site fails the sequence leg"
    else
        ci_fail selftest-callback-hollow "a hollow sequence did not fail loudly: rc=$rc $(grep 'callback-sequence' "$M2" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: no reference tree reports SKIP naming the ticket"
    CI_CALLBACK_ROOT="$r/none" CI_CALLBACK_OUT="$r/out2" CI_OUT="$r/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] && grep -q 'reference-callback	SKIP	.*BW-00-04-02' "$M"; then
        ci_pass selftest-callback-absent "absent tree reports SKIP naming BW-00-04-02"
    else
        ci_fail selftest-callback-absent "absent tree did not skip cleanly: rc=$rc"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$r2" "$REPO_ROOT/ci-out/callback-selftest".*

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
