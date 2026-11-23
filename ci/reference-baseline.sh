#!/usr/bin/env bash
# BW-00-02-01 — the reference baseline lane: record what the original tree
# actually is, so every later compatibility claim has a fixed anchor.
#
# Usage:
#   ci/reference-baseline.sh            # collect -> ci-out/reference-baseline/
#   ci/reference-baseline.sh self-test  # planted tree round-trips; absent skips
#
# The acceptance is "record commit, binary hashes, build params, python and
# db versions". The lane resolves the reference checkout the same way the
# parity lane does (BW_REFERENCE, default $HOME/workspaces/BigWorld), and
# when it exists records:
#   * the git commit, subject and date of the reference checkout;
#   * sha256 of every file under game/bin/server/el7/{server,tools,unit_tests}
#     — the original's own binaries — into reference-binaries.tsv, then a
#     single fingerprint of that tsv into the env json;
#   * the bundled python (third_party/python) version, probed from the lib
#     name and the interpreter itself;
#   * the db version as stated in the reference docs, with its source path;
#   * the documented build entry (and configure args when a build tree with
#     config.log exists) — recorded as provenance, never re-run.
# When the tree is absent the lane SKIPs naming the ticket; when it exists
# but is broken (no git, no binaries) the legs FAIL rather than reporting
# a hollow baseline. The collected record is committed under tests/baseline/
# as the durable record; if a committed record names a different commit the
# lane notes the drift (expected as the reference moves) without failing.
#
# Nothing here builds or runs the original; BW-00-02-02 reruns its tests.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
# Call-time resolution: the self-test plants a reference tree and a private
# output root, and an override applied to a shell function only reaches code
# that re-reads the variable.
cur_ref() { printf '%s' "${CI_REFBASE_ROOT:-${BW_REFERENCE:-$HOME/workspaces/BigWorld}}"; }
cur_out() { printf '%s' "${CI_REFBASE_OUT:-$CI_OUT}"; }

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

el7_dir() { printf '%s/game/bin/server/el7' "$(cur_ref)"; }
json_file() { printf '%s/reference-baseline/reference-env.json' "$(cur_out)"; }
tsv_file() { printf '%s/reference-baseline/reference-binaries.tsv' "$(cur_out)"; }

collect_commit() {
    # collect_commit — the anchor everything else hangs off. The tree must
    # be its own git toplevel: git walks up when it has no .git, and a
    # reference planted inside another repo would silently inherit that
    # repo's commit as its provenance.
    local ref sha subject date toplevel
    ref="$(cur_ref)"
    if ! sha="$(git -C "$ref" rev-parse HEAD 2>/dev/null)"; then
        printf 'FAIL\tno git checkout at %s — provenance missing\n' "$ref"
        return 1
    fi
    toplevel="$(git -C "$ref" rev-parse --show-toplevel 2>/dev/null)"
    if [ "$toplevel" != "$ref" ]; then
        printf 'FAIL\t%s is not its own git toplevel (resolves to %s) — inherited provenance, not the reference commit\n' "$ref" "${toplevel:-<none>}"
        return 1
    fi
    subject="$(git -C "$ref" log -1 --format=%s HEAD 2>/dev/null)"
    date="$(git -C "$ref" log -1 --format=%cI HEAD 2>/dev/null)"
    printf 'OK\t%s\t%s\t%s\n' "$sha" "$(tsv_escape "$subject")" "$date"
}

collect_binaries() {
    # collect_binaries — hash every original binary under the three el7
    # payload dirs. The tsv is the hash sheet; its own sha256 is the
    # one-line fingerprint that lands in the env json.
    local el7 f rel total=0
    el7="$(el7_dir)"
    mkdir -p "$(dirname "$(tsv_file)")"
    {
        printf '# reference binaries\n'
        printf '# tree: %s\n' "$(cur_ref)"
        printf '# generated: %s\n' "$(ci_now)"
    } >"$(tsv_file)"
    while IFS= read -r f; do
        [ -f "$f" ] || continue
        rel="${f#"$el7"/}"
        printf '%s\t%s\t%s\n' "$rel" "$(wc -c <"$f" | tr -d ' ')" \
            "$(sha256sum "$f" | cut -d' ' -f1)" >>"$(tsv_file)"
        total=$((total + 1))
    done < <(find "$el7/server" "$el7/tools" "$el7/unit_tests" -type f 2>/dev/null | sort)
    if [ "$total" -eq 0 ]; then
        printf 'FAIL\tel7 tree at %s has no binaries under server/tools/unit_tests\n' "$el7"
        return 1
    fi
    printf 'OK\t%s\t%s\n' "$total" "$(sha256sum "$(tsv_file)" | cut -d' ' -f1)"
}

collect_python() {
    # collect_python — the bundled stdlib install under third_party/python.
    # The lib name and the interpreter may disagree in theory, so both are
    # probed and both are recorded; a tree without the install is recorded
    # absent, not failed (python_install is a separate documented target).
    local el7 lib ver=""
    el7="$(el7_dir)"
    lib="$(find "$el7/third_party/python/lib" -maxdepth 1 -name 'libpython3*.a' 2>/dev/null | head -1)"
    lib="${lib##*/}"
    local bin
    for bin in "$el7"/third_party/python/bin/python3*; do
        case "${bin##*/}" in
        *config*) continue ;;
        esac
        [ -x "$bin" ] || continue
        ver="$("$bin" --version 2>&1 | tail -1)"
        break
    done
    printf 'OK\t%s\t%s\n' "${lib:-absent}" "${ver:-absent}"
}

collect_db() {
    # collect_db — the db version as the reference tree states it, with the
    # source that states it. Nothing is invented: no statement, no value.
    local ref hit
    ref="$(cur_ref)"
    hit="$(grep -rn -iE 'MySQL [0-9]+|MariaDB [0-9]+' "$ref/docs" "$ref/TESTING.md" 2>/dev/null | head -1)"
    if [ -n "$hit" ]; then
        printf 'OK\t%s\n' "$(tsv_escape "$hit")"
    else
        printf 'OK\tabsent (no db version statement in reference docs/ or TESTING.md)\n'
    fi
}

collect_build() {
    # collect_build — how the reference says it was built. The documented
    # entry comes from TESTING.md's make lines; when a build tree with a
    # config.log exists, its actual configure args are recorded over that.
    local ref el7 documented actual=""
    ref="$(cur_ref)"
    el7="$(el7_dir)"
    documented="$(grep -m2 -E '^\s*(cd .*&&.*make|make -s)' "$ref/TESTING.md" 2>/dev/null | head -1 | sed 's/^[[:space:]]*//')"
    local cfg="$ref/programming/bigworld/build/el7/config.log"
    if [ -f "$cfg" ]; then
        actual="$(grep -m1 '\$ ./configure' "$cfg" 2>/dev/null)"
        [ -n "$actual" ] && actual="config.log: $(tsv_escape "$actual")"
    fi
    printf 'OK\tdocumented: %s | actual: %s\n' \
        "${documented:-absent}" "${actual:-absent (no build tree in the reference checkout)}"
}

committed_record_commit() {
    # committed_record_commit — what commit the durable record under
    # tests/baseline/ froze, if one is committed.
    local f="$REPO_ROOT/tests/baseline/reference-env.json"
    [ -f "$f" ] || return 1
    sed -n 's/.*"reference_commit":"\([^"]*\)".*/\1/p' "$f" | head -1
}

write_json() { # $1 commit $2 subject $3 date $4 count $5 fingerprint $6 pylib $7 pyver $8 db $9 build ${10} drift
    mkdir -p "$(dirname "$(json_file)")"
    {
        printf '{"reference_root":"%s",' "$(cur_ref)"
        printf '"reference_commit":"%s","reference_subject":"%s","reference_date":"%s",' "$1" "$2" "$3"
        printf '"binaries_count":%s,"binaries_fingerprint":"%s",' "$4" "$5"
        printf '"python_bundled_lib":"%s","python_version":"%s",' "$6" "$7"
        printf '"db_stated":"%s",' "$(printf '%s' "$8" | sed 's/"/\\"/g')"
        printf '"build_params":"%s",' "$(printf '%s' "$9" | sed 's/"/\\"/g')"
        printf '"drift_note":"%s",' "$(printf '%s' "${10}" | sed 's/"/\\"/g')"
        printf '"checked_at":"%s","lane_git_sha":"%s"}\n' "$(ci_now)" "$(ci_git_sha)"
    } >"$(json_file)"
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    ci_step "reference baseline"
    if [ ! -d "$(cur_ref)" ]; then
        ci_skip reference-baseline \
            "no reference tree at $(cur_ref) — BW-00-02-01 records it when present (set BW_REFERENCE)"
        return 0
    fi

    local out rc
    out="$(collect_commit)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        ci_fail reference-commit "$(printf '%s' "$out" | cut -f2-)"
        return 1
    fi
    local sha subject date
    sha="$(printf '%s' "$out" | cut -f2)"
    subject="$(printf '%s' "$out" | cut -f3)"
    date="$(printf '%s' "$out" | cut -f4)"
    ci_pass reference-commit "$sha $subject ($date)"

    out="$(collect_binaries)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        ci_fail reference-binaries "$(printf '%s' "$out" | cut -f2-)"
        return 1
    fi
    local count fingerprint
    count="$(printf '%s' "$out" | cut -f2)"
    fingerprint="$(printf '%s' "$out" | cut -f3)"
    ci_pass reference-binaries "$count file(s) hashed, fingerprint $fingerprint (reference-binaries.tsv)"

    out="$(collect_python)"
    local pylib pyver
    pylib="$(printf '%s' "$out" | cut -f2)"
    pyver="$(printf '%s' "$out" | cut -f3)"
    ci_pass reference-python "bundled lib ${pylib}, interpreter ${pyver}"

    out="$(collect_db)"
    ci_pass reference-db "$(printf '%s' "$out" | cut -f2)"

    out="$(collect_build)"
    ci_pass reference-build "$(printf '%s' "$out" | cut -f2-)"

    local drift="none"
    local recorded
    recorded="$(committed_record_commit)" && [ -n "$recorded" ] && [ "$recorded" != "$sha" ] &&
        drift="committed tests/baseline record freezes $recorded, live reference is at ${sha%% *} — snapshot-time anchor, not a failure"
    write_json "$sha" "$subject" "$date" "$count" "$fingerprint" "$pylib" "$pyver" \
        "$(collect_db | cut -f2)" "$(collect_build | cut -f2-)" "$drift"
    [ "$drift" != "none" ] && ci_note "$drift"

    ci_pass reference-baseline "recorded to $(json_file)"
    ci_note "commit the two files under tests/baseline/ to keep the record durable"
}

self_test() {
    local fails=0 r out rc sha
    ci_step "self-test: a planted reference tree round-trips into a valid record"
    r="$(mktemp -d "$REPO_ROOT/ci-out/refbase-selftest.XXXXXX")"
    local T="$r/BigWorld"
    git init -q -b main "$T"
    git -C "$T" -c user.name=t -c user.email=t@t commit -q --allow-empty -m "original baseline anchor"
    sha="$(git -C "$T" rev-parse HEAD)"
    mkdir -p "$T/game/bin/server/el7/server" "$T/game/bin/server/el7/tools" \
        "$T/game/bin/server/el7/unit_tests" \
        "$T/game/bin/server/el7/third_party/python/lib" \
        "$T/game/bin/server/el7/third_party/python/bin" \
        "$T/docs"
    printf 'ELF-binary-one' >"$T/game/bin/server/el7/server/machine"
    printf 'ELF-binary-two' >"$T/game/bin/server/el7/tools/process_defs"
    printf 'ELF-binary-three' >"$T/game/bin/server/el7/unit_tests/network_test"
    printf 'fake libpython' >"$T/game/bin/server/el7/third_party/python/lib/libpython3.13.a"
    printf '#!/bin/sh\necho "Python 3.13.5"\n' >"$T/game/bin/server/el7/third_party/python/bin/python3.13"
    chmod +x "$T/game/bin/server/el7/third_party/python/bin/python3.13"
    printf 'build with mysql 8 client headers\n' >"$T/TESTING.md"

    out="$(CI_REFBASE_ROOT="$T" CI_REFBASE_OUT="$r/out" CI_OUT="$r/manifest" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -eq 0 ] \
        && printf '%s\n' "$out" | grep -q 'reference-binaries	PASS	3 file(s) hashed, fingerprint [0-9a-f]\{64\}' \
        && printf '%s\n' "$out" | grep -q 'Python 3.13.5' \
        && [ "$(grep -vc '^#' "$r/out/reference-baseline/reference-binaries.tsv" || true)" -eq 3 ] \
        && python3 -c 'import json,sys; json.load(open(sys.argv[1]))' \
            "$r/out/reference-baseline/reference-env.json" 2>/dev/null; then
        ci_pass selftest-refbase "planted tree records commit, 3 binaries, python 3.13.5 and valid json"
    else
        ci_fail selftest-refbase "planted tree did not round-trip: $(printf '%s\n' "$out" | grep 'reference-' | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: no reference tree reports SKIP naming the ticket"
    out="$(CI_REFBASE_ROOT="$r/no-such-tree" CI_REFBASE_OUT="$r/out2" CI_OUT="$r/manifest" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -eq 0 ] && printf '%s\n' "$out" | grep -q 'reference-baseline	SKIP	.*BW-00-02-01'; then
        ci_pass selftest-refbase-absent "absent tree reports SKIP naming BW-00-02-01"
    else
        ci_fail selftest-refbase-absent "absent tree did not skip cleanly: rc=$rc $(printf '%s\n' "$out" | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a tree without git or binaries fails, not a hollow record"
    mkdir -p "$r/broken/game/bin/server/el7"
    out="$(CI_REFBASE_ROOT="$r/broken" CI_REFBASE_OUT="$r/out3" CI_OUT="$r/manifest" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ] && printf '%s\n' "$out" | grep -q 'reference-commit	FAIL	.*not its own git toplevel'; then
        ci_pass selftest-refbase-broken "a tree that inherits another repo's git fails the commit leg instead of adopting its commit"
    else
        ci_fail selftest-refbase-broken "a broken tree did not fail loudly: rc=$rc"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$REPO_ROOT/ci-out/refbase-selftest".*

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
