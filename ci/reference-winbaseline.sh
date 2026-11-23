#!/usr/bin/env bash
# BW-00-02-05 — the Windows client and tools baseline, per target: a saved
# result or a named gap, never an invented one.
#
# Usage:
#   ci/reference-winbaseline.sh            # record the Windows target face
#   ci/reference-winbaseline.sh self-test  # planted target/gap/payload faces
#
# Two faces, each recorded as it stands:
#   * targets — the Windows build's own enumeration: the BW_CMAKE_TARGET
#     configurations (build/cmake/BWConfiguration_*.cmake) and the binary
#     projects each declares, derived from the files, not hand-copied. The
#     client face (bwclient), the tools face (worldeditor, modeleditor,
#     particle_editor, offline_processor, batch_compiler, jit_compiler, the
#     asset_pipeline converters), navgen, extras and assetprocessor.
#   * per-target — a saved result or a gap for each: the reference ships no
#     Windows payload under game/bin (only server/el7), so every Windows
#     target is a gap naming exactly that; the Linux el7 equivalent, where
#     one ships (process_defs, res_packer in el7/tools; simple_python_client
#     in el7/examples), is recorded beside it so the face says what a retest
#     could actually reach.
#
# A tree with no BWConfiguration files fails: a Windows record with no target
# in it is a hollow baseline, not one. The durable record is committed under
# tests/baseline/.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
cur_ref() { printf '%s' "${CI_WIN_ROOT:-${BW_REFERENCE:-$HOME/workspaces/BigWorld}}"; }
cur_out() { printf '%s' "${CI_WIN_OUT:-$CI_OUT}"; }

tsv_file() { printf '%s/reference-win/win-targets.tsv' "$(cur_out)"; }
json_file() { printf '%s/reference-win/win-baseline.json' "$(cur_out)"; }
committed_tsv() { printf '%s/tests/baseline/win-targets.tsv' "$REPO_ROOT"; }

# The Windows client/tool target groups. exporters and bwentity are recorded
# as groups too — exporters as DCC plugin projects, bwentity as a library.
win_groups() { printf 'client tools navgen extras assetprocessor'; }

# The binary projects a BWConfiguration declares: the SET( BW_BINARY_PROJECTS
# ... ) block's "<name> <dir>" pairs, comments and ${...} expansions dropped.
target_rows() { # "<group>\t<name>\t<source dir>" per line
    local ref cm g block name dir
    ref="$(cur_ref)"
    cm="$ref/programming/bigworld/build/cmake"
    for g in $(win_groups); do
        [ -f "$cm/BWConfiguration_$g.cmake" ] || continue
        # The block runs from SET( BW_BINARY_PROJECTS to the closing paren.
        block="$(sed -n '/SET( *BW_BINARY_PROJECTS/,/^[[:space:]]*)/p' \
            "$cm/BWConfiguration_$g.cmake" \
            | sed -e 's/#.*//' -e '/^[[:space:]]*)/d' \
                -e 's/SET( *BW_BINARY_PROJECTS//')"
        # Name/dir pairs; unit-test ${...} placeholders are skipped.
        while read -r name dir; do
            [ -n "${name:-}" ] || continue
            case "$name" in '${'*) continue ;; esac
            case "${dir:-}" in '${'*) continue ;; esac
            [ -n "${dir:-}" ] || continue
            printf '%s\t%s\t%s\n' "$g" "$name" "$dir"
        done <<<"$(printf '%s\n' "$block" | xargs -n2 printf '%s %s\n' 2>/dev/null)"
    done
}

# The exporter plugins a tree declares (DCC: 3ds max / Maya), if any.
exporter_rows() { # "<name>\t<source dir>" per line, may be empty
    local ref f
    ref="$(cur_ref)"
    f="$ref/programming/bigworld/build/cmake/BWConfiguration_exporters.cmake"
    [ -f "$f" ] || return 0
    sed -n '/SET( *EXPORTER_PROJECTS/,/^[[:space:]]*)/p' "$f" \
        | sed -e 's/#.*//' -e '/^[[:space:]]*)/d' -e 's/SET( *EXPORTER_PROJECTS//' \
        | while read -r name dir; do
            [ -n "${name:-}" ] || continue
            case "$name" in '${'*) continue ;; esac
            [ -n "${dir:-}" ] || continue
            printf '%s\t%s\n' "$name" "$dir"
        done
}

# Where a Linux el7 equivalent of a target ships, if anywhere.
el7_state() { # $1 binary name -> "present(<where>)" or "absent"
    local n="$1" ref el7
    ref="$(cur_ref)"
    el7="$ref/game/bin/server/el7"
    [ -x "$el7/tools/$n" ] && { printf 'present (el7/tools/%s)' "$n"; return 0; }
    [ -x "$el7/examples/$n" ] && { printf 'present (el7/examples/%s)' "$n"; return 0; }
    [ -x "$el7/server/$n" ] && { printf 'present (el7/server/%s)' "$n"; return 0; }
    printf 'absent'
}

record_targets() { # writes the tsv, prints "targets exporters"
    local ref head
    ref="$(cur_ref)"
    head="$(git -C "$ref" rev-parse HEAD 2>/dev/null || echo unknown)"
    [ "$(git -C "$ref" rev-parse --show-toplevel 2>/dev/null || echo "")" = "$ref" ] \
        || head="unknown (not the reference's own git toplevel)"

    mkdir -p "$(dirname "$(tsv_file)")"
    {
        printf '# windows client and tools targets\n'
        printf '# tree: %s\n' "$ref"
        printf '# commit: %s\n' "$head"
        printf '# derived from build/cmake/BWConfiguration_*.cmake (BW_BINARY_PROJECTS pairs)\n'
        printf '# columns: group\tname\tsource_dir\twindows_payload\tlinux_el7_equivalent\n'
    } >"$(tsv_file)"

    local g n d total=0 exps=0 plugins=0 state
    while IFS=$'\t' read -r g n d; do
        total=$((total + 1))
        state="$(el7_state "$n")"
        printf '%s\t%s\t%s\t%s\t%s\n' "$g" "$n" "$d" \
            "gap (no windows payload under game/bin — only server/el7 ships)" "$state" \
            >>"$(tsv_file)"
    done < <(target_rows)

    while IFS=$'\t' read -r n d; do
        exps=$((exps + 1))
        # The exporter build lists its DCC plugins and its library
        # dependencies in one set — label them apart, both are gaps.
        case "$d" in
        tools/*) label="exporter-plugin"; plugins=$((plugins + 1)) ;;
        *) label="exporter-lib" ;;
        esac
        printf '%s\t%s\t%s\t%s\t%s\n' "$label" "$n" "$d" \
            "gap (DCC plugin build — no windows payload under game/bin)" "absent" \
            >>"$(tsv_file)"
    done < <(exporter_rows)

    printf '%s %s %s\n' "$total" "$plugins" "$exps"
}

write_json() { # $1 payload targets $2 exporters $3 shippable
    local ref head
    ref="$(cur_ref)"
    head="$(git -C "$ref" rev-parse HEAD 2>/dev/null || echo unknown)"
    mkdir -p "$(dirname "$(json_file)")"
    cat >"$(json_file)" <<JSON
{
  "face": "windows client and tools baseline",
  "reference_root": "$ref",
  "reference_commit": "$head",
  "payload_targets": $1,
  "exporter_plugins": $2,
  "linux_el7_equivalents": $3,
  "windows_payloads": "none (game/bin ships only server/el7 — every Windows target is a recorded gap)",
  "retest_gate": "closed",
  "retest_gate_detail": "no windows payload under game/bin and no windows build host — a retest records the gap until one is built",
  "recorded_at": "$(ci_now)",
  "lane_git_sha": "$(ci_git_sha)"
}
JSON
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    ci_step "windows client and tools baseline"
    if [ ! -d "$(cur_ref)" ]; then
        ci_skip reference-win \
            "no reference tree at $(cur_ref) — BW-00-02-05 records it when present (set BW_REFERENCE)"
        return 0
    fi

    local cm
    cm="$(cur_ref)/programming/bigworld/build/cmake"
    if ! ls "$cm"/BWConfiguration_*.cmake >/dev/null 2>&1; then
        ci_fail win-targets "no BWConfiguration_*.cmake under $cm — a Windows record with no target in it is a hollow baseline"
        return 1
    fi

    local rows total exps plugins deps shippable=0 state groups=0 g
    rows="$(target_rows)"
    if [ -z "$rows" ]; then
        ci_fail win-targets "no BW_BINARY_PROJECTS pairs derived from $cm — the target list is empty"
        return 1
    fi

    while IFS=$'\t' read -r _g n _d; do
        case "$(el7_state "$n")" in present*) shippable=$((shippable + 1)) ;; esac
    done <<<"$rows"
    for g in $(win_groups); do
        [ -f "$cm/BWConfiguration_$g.cmake" ] && groups=$((groups + 1))
    done

    read -r total plugins exps <<<"$(record_targets)"
    deps=$((exps - plugins))
    ci_pass win-targets \
        "$total binary target(s) across $groups BW_CMAKE_TARGET group(s) + $plugins DCC exporter plugin(s) ($deps exporter lib deps) derived into win-targets.tsv"

    # Every Windows payload is a gap — recorded, not failed: the acceptance
    # is a saved result or a named gap per target, and the tree ships none.
    ci_skip win-retest \
        "no windows payload under game/bin (only server/el7 ships) — all $total target(s) + $plugins exporter plugin(s) recorded as gaps; $shippable have a Linux el7 equivalent"
    write_json "$total" "$plugins" "$shippable"

    # A committed record freezing an older face is a snapshot-time anchor, not
    # a failure — say so rather than silently diverging.
    if [ -f "$(committed_tsv)" ] && ! cmp -s "$(committed_tsv)" "$(tsv_file)"; then
        ci_note "the committed tests/baseline/win-targets.tsv differs from this run — snapshot-time anchor, not a failure"
    fi
    ci_note "per-target results or gaps are saved in win-targets.tsv"
}

self_test() {
    local fails=0 r out rc T cm
    ci_step "self-test: a planted build/cmake tree derives its targets and records the gaps"
    r="$(mktemp -d "$REPO_ROOT/ci-out/win-selftest.XXXXXX")"
    T="$r/BigWorld"
    cm="$T/programming/bigworld/build/cmake"
    mkdir -p "$cm" "$T/game/bin/server/el7/tools" "$T/game/bin/server/el7/examples"
    printf 'SET( BW_BINARY_PROJECTS\n\tbwclient\t\tclient\n\temptyvoip\t\tlib/emptyvoip\n)\n' \
        >"$cm/BWConfiguration_client.cmake"
    printf 'SET( BW_BINARY_PROJECTS\n\tworldeditor\t\ttools/worldeditor\n\tprocess_defs\t\ttools/process_defs\n)\n' \
        >"$cm/BWConfiguration_tools.cmake"
    printf 'x' >"$T/game/bin/server/el7/tools/process_defs"
    chmod +x "$T/game/bin/server/el7/tools/process_defs"

    out="$(CI_WIN_ROOT="$T" CI_WIN_OUT="$r/out" CI_OUT="$r/manifest" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -eq 0 ] \
        && printf '%s\n' "$out" | grep -q 'win-targets	PASS	' \
        && printf '%s\n' "$out" | grep -q 'win-retest	SKIP	' \
        && grep -q '^client	bwclient' "$r/out/reference-win/win-targets.tsv" \
        && grep -q 'present (el7/tools/process_defs)' "$r/out/reference-win/win-targets.tsv"; then
        ci_pass selftest-win "planted tree derives its targets, records the gaps and the shipped equivalent"
    else
        ci_fail selftest-win "planted tree did not record: rc=$rc $(printf '%s\n' "$out" | grep 'win-' | head -2)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: no reference tree reports SKIP naming the ticket"
    out="$(CI_WIN_ROOT="$r/none" CI_WIN_OUT="$r/out2" CI_OUT="$r/manifest" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -eq 0 ] && printf '%s\n' "$out" | grep -q 'reference-win	SKIP	.*BW-00-02-05'; then
        ci_pass selftest-win-absent "absent tree reports SKIP naming BW-00-02-05"
    else
        ci_fail selftest-win-absent "absent tree did not skip cleanly: rc=$rc"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a tree without build/cmake fails instead of recording hollow targets"
    mkdir -p "$r/empty/game/bin/server"
    out="$(CI_WIN_ROOT="$r/empty" CI_WIN_OUT="$r/out3" CI_OUT="$r/manifest" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ] && printf '%s\n' "$out" | grep -q 'win-targets	FAIL	.*hollow baseline'; then
        ci_pass selftest-win-empty "a configuration-less tree fails the targets leg"
    else
        ci_fail selftest-win-empty "a configuration-less tree did not fail loudly: rc=$rc"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$REPO_ROOT/ci-out/win-selftest".*

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
