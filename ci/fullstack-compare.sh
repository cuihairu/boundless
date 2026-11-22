#!/usr/bin/env bash
# BW-00-04-04 — the full-stack comparison: the same input through the original
# stack and through the new stack, each producer leaving its own evidence.
#
# Usage:
#   ci/fullstack-compare.sh            # every scenario
#   ci/fullstack-compare.sh self-test  # prove a planted divergence is named
#
# The entity-definition scenario runs both halves live on the same committed
# res tree: the stock `process_defs` binary (the original stack) and our
# `process_defs` (the new stack) each write a JSON description of every parsed
# entity type, and the lane asks whether the two are the same artifact. The
# protocol and callback scenarios record the reference evidence captured in
# BW-00-04-01/02 and SKIP the new half — the mercury wire and python-host
# callback producers do not exist yet — naming every blocker rather than
# silently passing a comparison that has only one side.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
SANDBOX="$(mktemp -d /tmp/bw00-fullstack.XXXXXX)"

cleanup() {
    rm -rf "$SANDBOX"
}
trap cleanup EXIT
# Bash does not run an EXIT trap for a fatal signal, and a cancelled job sends
# exactly that — the one moment a probe tree would outlive the run.
trap 'exit 143' TERM
trap 'exit 130' INT
trap 'exit 129' HUP

# A probe that cannot find its own substrate yields a SKIP naming every missing
# piece; only a real divergence is a FAIL. A clean runner ships no original
# binary, and that must not look like a broken lane.

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

record_path() { printf '%s/fullstack/%s.tsv' "$CI_OUT" "$1"; }

sha256_of() { sha256sum "$1" | cut -d' ' -f1; }

# --- substrates ------------------------------------------------------------

# The reference tree defaults to a home-relative checkout, so the lane runs
# locally with no setup. CI_FULLSTACK_REFERENCE or BW_REFERENCE overrides it.
REFERENCE="${CI_FULLSTACK_REFERENCE:-${BW_REFERENCE:-$HOME/workspaces/BigWorld}}"
REFERENCE_TOOL="${CI_FULLSTACK_REFERENCE_TOOL:-$REFERENCE/game/bin/server/el7/tools/process_defs}"
RES_ROOT="${CI_FULLSTACK_RES_ROOT:-$REFERENCE/game/res/bigworld}"
DUMP_MODULE="$REPO_ROOT/docs/review/tools/process_defs_dump.py"
ORACLE_DIR="$REPO_ROOT/docs/review/reference"
FIXTURES_DIR="$REPO_ROOT/docs/review/fixtures"
NEW_TOOL="$SANDBOX/process_defs"

# Everything the run needs. Collected together so one SKIP names every piece
# rather than reporting the first one and hiding the rest.
missing_substrates() {
    if [ -z "$REFERENCE" ]; then
        echo "no reference tree (set BW_REFERENCE or CI_FULLSTACK_REFERENCE)"
        return 0
    fi
    local missing=""
    [ -x "$REFERENCE_TOOL" ] || missing="$missing original tool $REFERENCE_TOOL"
    [ -d "$RES_ROOT" ] || missing="$missing reference res root $RES_ROOT"
    [ -f "$DUMP_MODULE" ] || missing="$missing dump module $DUMP_MODULE"
    command -v cargo >/dev/null 2>&1 || missing="$missing cargo"
    printf '%s' "${missing# }"
}

# Build the new half. The reference-compatible entry point lives in
# crates/entitydef; a runner that cannot build it says why, instead of going
# quiet like a divergence would.
ensure_new_tool_built() {
    if [ -x "$NEW_TOOL" ]; then
        return 0
    fi
    if ! command -v cargo >/dev/null 2>&1; then
        echo "no cargo on this runner, so the new process_defs cannot be built"
        return 1
    fi
    if ! (cd "$REPO_ROOT" && cargo build -q -p entitydef --bin process_defs) >/dev/null 2>&1; then
        echo "the new process_defs could not be built here"
        return 1
    fi
    cp "$REPO_ROOT/target/debug/process_defs" "$NEW_TOOL"
    return 0
}

# --- the entity-definition scenario -----------------------------------------

# fixture dir : captured-oracle stem. The fixture res trees are committed here;
# the oracle JSON was captured from the original binary on the same tree in
# BW-04-04-03, and a live run of the original on the same tree reproduces it
# byte for byte (see docs/review/bw00-04-04-fullstack-comparison.md).
ENTITYDEF_CASES="full_def:full service_def:service entity_index:entity_index"

# The original half: -m/-f name the dump callback, -p places that module, -r
# gives the fixture res first and the reference res root second. The second
# root is what bootstraps the embedded Python — its scripts/common carries
# bw_site.py, scripts/common/Lib the stdlib, scripts/server_common/lib-dynload
# the extension modules — the same order ci/parity.sh uses.
reference_dump() { # $1 fixture res $2 out file
    ( cd "$SANDBOX" && "$REFERENCE_TOOL" -m process_defs_dump -f dump \
        -p "$REPO_ROOT/docs/review/tools" \
        -r "$1" -r "$RES_ROOT" --use-stdout "$2" ) >"$SANDBOX/ref.log" 2>&1
}

# The new half: reference-compatible options, the dump to stdout. The driver is
# read at call time, not at source time: the self-test swaps it for a stub that
# reports a different artifact, and an override applied to a shell variable
# only reaches code that reads it again.
new_dump() { # $1 fixture res $2 out file
    "${CI_FULLSTACK_NEW_DRIVER:-$NEW_TOOL}" -r "$1" >"$2" 2>"$SANDBOX/new.log"
}

# One fixture: the original stack and the new stack over the same res tree.
# The original binary aborts at interpreter teardown after the dump is written
# (a registered reference-tooling artifact — see the evidence doc), so the exit
# code is recorded but the evidence is judged by its bytes against the oracle.
#
# emit=0 is the self-test: the outcome goes only to the record file, so a
# planted divergence is asserted without leaving a FAIL row in the shared
# manifest that would keep the lane red after the self-test has already proven
# the lane can bite. Returns 0 when the pair is byte-identical.
run_entitydef_case() { # $1 fixture name $2 oracle stem $3 record file $4 emit
    local name="$1" oracle_stem="$2" rec="$3" emit="${4:-1}"
    local res="$FIXTURES_DIR/$name/res" oracle="$ORACLE_DIR/${oracle_stem}_entitydef.json"
    local ref_out="$CI_OUT/fullstack/ref/$name.json"
    local new_out="$CI_OUT/fullstack/new/$name.json"
    local ref_rc new_rc ref_sha new_sha ora_sha note="" rc=1
    mkdir -p "$CI_OUT/fullstack/ref" "$CI_OUT/fullstack/new"

    reference_dump "$res" "$ref_out"
    ref_rc=$?
    new_dump "$res" "$new_out"
    new_rc=$?

    if [ ! -s "$ref_out" ]; then
        printf '%s\tFAIL\tno reference evidence\trc=%s\t-\t-\n' "$name" "$ref_rc" >>"$rec"
        [ "$emit" -eq 1 ] && ci_fail "fullstack-entitydef-$name" \
            "the original stack produced no evidence (rc=$ref_rc)"
        return 1
    fi
    if [ ! -s "$new_out" ]; then
        printf '%s\tFAIL\tno new evidence\trc=%s\t-\t-\n' "$name" "$new_rc" >>"$rec"
        [ "$emit" -eq 1 ] && ci_fail "fullstack-entitydef-$name" \
            "the new process_defs produced no evidence (rc=$new_rc)"
        return 1
    fi

    ref_sha="$(sha256_of "$ref_out")"
    new_sha="$(sha256_of "$new_out")"
    ora_sha="$(sha256_of "$oracle")"
    # A teardown abort after the evidence is written is the reference binary's
    # own behaviour, not a comparison defect; the bytes are what is judged.
    if [ "$ref_rc" -ne 0 ]; then
        note="reference teardown rc=$ref_rc after evidence (registered artifact)"
    fi

    if [ "$ref_sha" = "$ora_sha" ] && [ "$new_sha" = "$ora_sha" ]; then
        rc=0
        printf '%s\tPASS\t%s\t%s\t%s\t%s\t%s\n' \
            "$name" "$ref_sha" "$new_sha" "$ora_sha" "$ref_rc" "$(tsv_escape "$note")" >>"$rec"
        [ "$emit" -eq 1 ] && ci_pass "fullstack-entitydef-$name" \
            "original==new==oracle $ref_sha${note:+ ($note)}"
    elif [ "$new_sha" = "$ora_sha" ]; then
        printf '%s\tFAIL\treference diverged\t%s\t%s\t%s\t%s\n' \
            "$name" "$ref_sha" "$new_sha" "$ora_sha" "$ref_rc" >>"$rec"
        [ "$emit" -eq 1 ] && ci_fail "fullstack-entitydef-$name" \
            "the original stack diverged from its captured oracle (ref $ref_sha vs oracle $ora_sha)"
    elif [ "$ref_sha" = "$ora_sha" ]; then
        printf '%s\tFAIL\tnew diverged\t%s\t%s\t%s\t%s\n' \
            "$name" "$ref_sha" "$new_sha" "$ora_sha" "$ref_rc" >>"$rec"
        [ "$emit" -eq 1 ] && ci_fail "fullstack-entitydef-$name" \
            "the new process_defs diverged from the captured oracle (new $new_sha vs oracle $ora_sha)"
    else
        printf '%s\tFAIL\tboth diverged\t%s\t%s\t%s\t%s\n' \
            "$name" "$ref_sha" "$new_sha" "$ora_sha" "$ref_rc" >>"$rec"
        [ "$emit" -eq 1 ] && ci_fail "fullstack-entitydef-$name" \
            "both halves diverged from the oracle and from each other (ref $ref_sha, new $new_sha, oracle $ora_sha)"
    fi
    return "$rc"
}

run_entitydef() {
    local rec
    rec="$(record_path entitydef)"
    {
        printf 'fixture\tresult\tref_sha256\tnew_sha256\toracle_sha256\tref_rc\tnote\n'
    } >"$rec"

    local case name stem
    for case in $ENTITYDEF_CASES; do
        name="${case%%:*}"
        stem="${case##*:}"
        run_entitydef_case "$name" "$stem" "$rec"
    done

    cat "$rec" >>"$CI_LOG_DIR/fullstack-entitydef.log"
}

# --- the reference-only scenarios -------------------------------------------

# BW-00-04-01/02 captured the original stack's wire bytes and callback traces;
# the new stack has no runnable producer yet. The reference evidence is the
# record; the new half names its blocker rather than silently passing a
# comparison with only one side.
record_reference_only() { # $1 scenario $2 fixtures dir $3 new-half blocker
    local scenario="$1" dir="$2" blocker="$3" rec
    rec="$(record_path "$scenario")"
    : >"$rec"
    local files=0
    while IFS= read -r f; do
        files=$((files + 1))
        printf '%s\t%s\tSKIP\t%s\n' \
            "$(basename "$f")" "$(sha256_of "$f")" "$(tsv_escape "$blocker")" >>"$rec"
    done < <(find "$dir" -type f | sort)
    cat "$rec" >>"$CI_LOG_DIR/fullstack-$scenario.log"
    ci_skip "fullstack-$scenario" \
        "$files reference fixture(s) recorded; new half: $blocker"
}

# --- the lane ----------------------------------------------------------------

run_all() {
    local missing_sub
    ci_step "full-stack comparison: original vs new on the same input"
    mkdir -p "$CI_OUT/fullstack" "$CI_LOG_DIR"

    missing_sub="$(missing_substrates)"
    if [ -n "$missing_sub" ]; then
        ci_skip fullstack-entitydef "$(tsv_escape "$missing_sub") — the original half needs the reference tree"
    elif ! ensure_new_tool_built; then
        ci_skip fullstack-entitydef "the new process_defs could not be built here"
    else
        run_entitydef
    fi

    record_reference_only protocol "$FIXTURES_DIR/protocol" \
        "the mercury wire producer does not exist yet (crates/mercury has no runnable byte entry point)"
    record_reference_only callbacks "$FIXTURES_DIR/callbacks" \
        "the python-host callback producer does not exist yet (crates/python-host has no ScriptEventList)"
}

self_test() {
    local fails=0 missing_sub
    ci_step "self-test: a diverging new half is named, a clean pair passes"
    missing_sub="$(missing_substrates)"
    if [ -n "$missing_sub" ]; then
        ci_skip selftest-fullstack "$(tsv_escape "$missing_sub")"
        return 0
    fi
    if ! ensure_new_tool_built; then
        ci_skip selftest-fullstack "the new process_defs cannot be built here"
        return 0
    fi

    # Plant a new half that reports a different artifact for the same input and
    # run the lane's own assertion against it. A self-test that went green here
    # would mean the comparison cannot see a wrong replacement at all.
    local name="full_def" res oracle
    res="$FIXTURES_DIR/$name/res"
    oracle="$ORACLE_DIR/full_entitydef.json"
    local stub="$SANDBOX/wrong_new.sh"
    cat >"$stub" <<EOF
#!/usr/bin/env bash
# The planted wrong new half: the same res root, but an artifact that differs
# from the oracle in its first byte.
cp "$oracle" "$SANDBOX/planted.json"
printf 'X' | dd of="$SANDBOX/planted.json" bs=1 seek=0 count=1 conv=notrunc 2>/dev/null
cat "$SANDBOX/planted.json"
EOF
    chmod +x "$stub"

    local rec="$SANDBOX/selftest.tsv"
    printf 'fixture\tresult\tref_sha256\tnew_sha256\toracle_sha256\tref_rc\tnote\n' >"$rec"
    # The ref half runs for real; only the new half is stubbed. emit=0 keeps
    # the planted FAIL out of the shared manifest — the self-test's own
    # verdict below is the record that the lane bites.
    CI_FULLSTACK_NEW_DRIVER="$stub" run_entitydef_case "$name" "full" "$rec" 0
    if grep -q "^$name	FAIL" "$rec"; then
        ci_pass selftest-fullstack "a diverging new half is rejected by the lane's own assertion"
    else
        ci_fail selftest-fullstack "the planted divergence passed the assertion, so this lane cannot bite"
        fails=$((fails + 1))
    fi

    ci_step "self-test: the clean pair passes"
    : >"$rec"
    run_entitydef_case "$name" "full" "$rec" 0
    if grep -q "^$name	PASS" "$rec"; then
        ci_pass selftest-fullstack-clean "the clean original/new pair is byte-identical"
    else
        ci_fail selftest-fullstack-clean "the clean pair was not byte-identical"
        fails=$((fails + 1))
    fi

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
    run_all
}

main "$@"
LANE_RC=$?

set +e
ci_finalize
FINAL_RC=$?
set -e

[ "$LANE_RC" -ne 0 ] && exit "$LANE_RC"
[ "$FINAL_RC" -ne 0 ] && exit "$FINAL_RC"
