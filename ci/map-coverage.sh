#!/usr/bin/env bash
# BW-00-05-02 — index every build-inventory item in the mapping category.
#
# Usage:
#   ci/map-coverage.sh            # index the engine-library and server targets
#   ci/map-coverage.sh self-test  # planted ledger + todo + port prove each index
#
# The coverage ledger (BW-00-05-01) enumerates the reference's build targets;
# this lane gives each one an index: the port crate that implements it when one
# exists, else the BW-xx ticket whose source scope covers its directory, else a
# recorded bare gap. The task filter is per-item, so BW-00-05-03 (client/tools)
# and BW-00-05-04 (common/examples) map their own categories with the same lane.
#
# One record face:
#   * map-record — coverage-map.json + coverage-map.tsv, one row per ledger
#     target in the category, each with an index and a ticket. A curated port
#     index that no longer exists fails the map (the table is stale, not "no
#     counterpart"); an absent ledger skips naming the ticket.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
cur_out() { printf '%s' "${CI_MAP_OUT:-$CI_OUT}"; }
map_dir() { printf '%s/coverage-map' "$(cur_out)"; }
cur_port() { printf '%s' "${CI_MAP_PORT_ROOT:-$REPO_ROOT}"; }
cur_todo() { printf '%s' "${CI_MAP_TODO:-$REPO_ROOT/todo.md}"; }

# The ledger the map reads: the refbase job's fresh run when it is present
# (coverage-ledger ran just before), else the committed snapshot.
map_ledger() { # prints the ledger path
    if [ -n "${CI_MAP_LEDGER:-}" ]; then
        printf '%s' "$CI_MAP_LEDGER"
    elif [ -f "$(cur_out)/coverage-ledger/coverage-ledger.json" ]; then
        printf '%s/coverage-ledger/coverage-ledger.json' "$(cur_out)"
    else
        printf '%s/tests/baseline/coverage-ledger.json' "$REPO_ROOT"
    fi
}

map_tasks() { # the category this item maps, as --task args
    if [ -n "${CI_MAP_TASKS:-}" ]; then
        printf '%s' "$CI_MAP_TASKS"
    else
        printf 'BW-00-05-02'
    fi
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    ci_step "map the engine-library and server build targets"

    local ledger
    ledger="$(map_ledger)"
    if [ ! -f "$ledger" ]; then
        ci_skip map-record \
            "no coverage ledger at $ledger — the BW-00-05-01 inventory is missing, so BW-00-05-02 cannot be indexed (set CI_MAP_LEDGER or run ci/coverage-ledger.sh)"
        return 0
    fi

    local out_dir args
    out_dir="$(map_dir)"
    args=()
    local t
    for t in $(map_tasks); do args+=(--task "$t"); done
    local sum
    if ! sum="$(python3 "$REPO_ROOT/ci/map-coverage.py" "$ledger" "$(cur_todo)" \
        "$(cur_port)" "$out_dir" "${args[@]}" --self-check 2>&1)"; then
        ci_fail map-record "the map failed to record: $(printf '%s' "$sum" | tail -1)"
        return 1
    fi
    local indexed crate ticket bare
    indexed="$(printf '%s' "$sum" | sed -n 's/.*indexed=\([0-9]*\).*/\1/p')"
    crate="$(printf '%s' "$sum" | sed -n 's/.*crate=\([0-9]*\).*/\1/p')"
    ticket="$(printf '%s' "$sum" | sed -n 's/.*ticket=\([0-9]*\).*/\1/p')"
    bare="$(printf '%s' "$sum" | sed -n 's/.*bare=\([0-9]*\).*/\1/p')"
    if [ -z "$indexed" ] || [ "$indexed" -eq 0 ]; then
        ci_fail map-record "the map recorded no row count — a hollow map is not a record"
        return 1
    fi
    ci_pass map-record "$indexed target(s) indexed ($crate to a port crate, $ticket to a ticket, $bare bare)"

    # The committed snapshot is the anchor; a map that grows as later mapping
    # items land is a note, not a failure.
    local f
    for f in coverage-map.json coverage-map.tsv; do
        if [ -f "$REPO_ROOT/tests/baseline/$f" ] && [ -f "$out_dir/$f" ] \
            && ! cmp -s "$REPO_ROOT/tests/baseline/$f" "$out_dir/$f"; then
            ci_note "the committed tests/baseline/$f differs from this run — snapshot-time anchor, not a failure"
        fi
    done

    ci_note "every indexed target carries a port index or a traceable ticket"
}

# A planted ledger with one target per disposition, a planted todo naming two
# source scopes, and a planted port carrying every curated member.
plant_tree() { # $1 root
    local root="$1"
    mkdir -p "$root/ledger" "$root/todo" "$root/port"
    cat >"$root/ledger/coverage-ledger.json" <<'JSON'
{"rows": [
  {"original_path": "cstdmf (lib/cstdmf)", "kind": "library",
   "enable_condition": "always", "task": "BW-00-05-02", "fixture": "pending", "status": "unmapped"},
  {"original_path": "terrain (lib/terrain)", "kind": "library",
   "enable_condition": "always", "task": "BW-00-05-02", "fixture": "pending", "status": "unmapped"},
  {"original_path": "baseapp (server/baseapp)", "kind": "executable",
   "enable_condition": "always", "task": "BW-00-05-02", "fixture": "pending", "status": "unmapped"},
  {"original_path": "moo (lib/moo)", "kind": "library",
   "enable_condition": "always", "task": "BW-00-05-02", "fixture": "pending", "status": "unmapped"},
  {"original_path": "gizmo (lib/gizmo)", "kind": "library",
   "enable_condition": "always", "task": "BW-00-05-03", "fixture": "pending", "status": "unmapped"}
]}
JSON
    cat >"$root/todo/todo.md" <<'EOF'
### BW-01 基础运行时、配置和资源
源码 `lib/cstdmf/`。

### BW-05 集群与恢复
源码 `server/{baseapp,baseappmgr}/`。

### BW-10 地图与场景
源码 `lib/{terrain,chunk}/`。
EOF
    local crate
    for crate in foundation resources mercury python-host entitydef server-runtime; do
        mkdir -p "$root/port/crates/$crate"
        printf '[package]\nname = "%s"\nversion = "0.1.0"\n' "$crate" \
            >"$root/port/crates/$crate/Cargo.toml"
    done
    mkdir -p "$root/port/apps/server"
    printf '[package]\nname = "boundless-server"\nversion = "0.1.0"\n' \
        >"$root/port/apps/server/Cargo.toml"
}

self_test() {
    local fails=0 rc r T M r2 T2 M2 r3 T3 M3
    ci_step "self-test: a planted inventory indexes each disposition and keeps the task filter"
    r="$(mktemp -d "$REPO_ROOT/ci-out/map-selftest.XXXXXX")"
    T="$r/BigWorld"
    M="$r/manifest/manifest.tsv"
    plant_tree "$T"

    CI_MAP_LEDGER="$T/ledger/coverage-ledger.json" CI_MAP_OUT="$r/out" \
        CI_OUT="$r/manifest" CI_MAP_TASKS="BW-00-05-02" \
        CI_MAP_PORT_ROOT="$T/port" CI_MAP_TODO="$T/todo/todo.md" \
        "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q '^cstdmf (lib/cstdmf)	library	crates/foundation	BW-01	' "$r/out/coverage-map/coverage-map.tsv" \
        && grep -q '^terrain (lib/terrain)	library	none yet	BW-10	' "$r/out/coverage-map/coverage-map.tsv" \
        && grep -q '^baseapp (server/baseapp)	executable	apps/server	BW-05	' "$r/out/coverage-map/coverage-map.tsv" \
        && grep -q '^moo (lib/moo)	library	none yet	none	' "$r/out/coverage-map/coverage-map.tsv" \
        && [ "$(grep -c '^gizmo' "$r/out/coverage-map/coverage-map.tsv")" -eq 0 ]; then
        ci_pass selftest-map-indexes "each disposition is indexed (crate/ticket/bare) and out-of-category rows stay out"
    else
        ci_fail selftest-map-indexes "planted targets were not indexed as expected: rc=$rc $(grep 'map-record' "$M" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a curated port index the port no longer carries fails the map"
    r2="$(mktemp -d "$REPO_ROOT/ci-out/map-selftest.XXXXXX")"
    T2="$r2/BigWorld"
    M2="$r2/manifest/manifest.tsv"
    plant_tree "$T2"
    rm -rf "$T2/port/crates/mercury"   # network -> crates/mercury is now stale
    CI_MAP_LEDGER="$T2/ledger/coverage-ledger.json" CI_MAP_OUT="$r2/out" \
        CI_OUT="$r2/manifest" CI_MAP_TASKS="BW-00-05-02" \
        CI_MAP_PORT_ROOT="$T2/port" CI_MAP_TODO="$T2/todo/todo.md" \
        "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -ne 0 ] \
        && grep -q 'map-record	FAIL	.*crates/mercury is not a port member' "$M2"; then
        ci_pass selftest-map-stale "a stale curated index fails the map by name rather than silently unmapping"
    else
        ci_fail selftest-map-stale "a stale curated index did not fail by name: rc=$rc $(grep 'map-record' "$M2" 2>/dev/null | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: an absent ledger skips naming the ticket"
    r3="$(mktemp -d "$REPO_ROOT/ci-out/map-selftest.XXXXXX")"
    T3="$r3/none"
    M3="$r3/manifest/manifest.tsv"
    CI_MAP_LEDGER="$T3/coverage-ledger.json" CI_MAP_OUT="$r3/out" \
        CI_OUT="$r3/manifest" "$0" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ] \
        && grep -q 'map-record	SKIP	.*BW-00-05-02' "$M3"; then
        ci_pass selftest-map-absent "an absent ledger reports SKIP naming BW-00-05-02"
    else
        ci_fail selftest-map-absent "an absent ledger did not skip cleanly: rc=$rc"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$r2" "$r3" "$REPO_ROOT/ci-out/map-selftest".*

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
