#!/usr/bin/env bash
# BW-22-02-04 — the compatibility differential: our replacement against the original.
#
# Usage:
#   ci/parity.sh            # every fixture under tests/parity/def
#   ci/parity.sh self-test  # prove a wrong replacement fails and is named
#
# The substitution path is what BW-00-04-05 will automate: one component
# swapped, the rest held constant. This lane holds every component but the
# entity-definition checker constant, feeds the same .def to the original
# `process_defs` and to our checker, and asks whether our checker reports what
# the original reported.
#
# A fixture passes only when every parser-reported diagnostic the original
# emitted appears in our output. A replacement that stays silent where the
# original spoke is the defect this lane exists to catch.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
CASES_DIR="$REPO_ROOT/tests/parity/def"
EXT_DIR="$REPO_ROOT/extensions/vscode"
SANDBOX="$(mktemp -d /tmp/bw4-parity.XXXXXX)"

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

record_path() { printf '%s/parity/%s.tsv' "$CI_OUT" "$1"; }

# --- substrates ------------------------------------------------------------

REFERENCE="${CI_PARITY_REFERENCE:-${BW_REFERENCE:-/home/cui/workspaces/BigWorld}}"
REFERENCE_TOOL="${CI_PARITY_REFERENCE_TOOL:-$REFERENCE/game/bin/server/el7/tools/process_defs}"
REFERENCE_RES="${CI_PARITY_REFERENCE_RES:-$REFERENCE/programming/bigworld/examples/client_integration/python/simple/res}"
RES_ROOT="${CI_PARITY_RES_ROOT:-$REFERENCE/game/res/bigworld}"
ALIAS="$REFERENCE_RES/scripts/entity_defs/alias.xml"

# Everything the probe needs. Collected together so one SKIP names every piece
# rather than reporting the first one and hiding the rest.
missing_substrates() {
    local missing=""
    [ -x "$REFERENCE_TOOL" ] || missing="$missing original tool $REFERENCE_TOOL"
    [ -f "$ALIAS" ] || missing="$missing alias table $ALIAS"
    [ -d "$RES_ROOT" ] || missing="$missing reference res root $RES_ROOT"
    command -v node >/dev/null 2>&1 || missing="$missing node"
    printf '%s' "${missing# }"
}

# The checker runs from extensions/vscode/out. Build it when it is absent; if
# the build cannot happen here, that is a missing substrate, not a divergence.
ensure_extension_built() {
    # Reasons are printed to stdout: the caller captures them into the SKIP
    # detail, so a runner that cannot build the checker says why instead of
    # going quiet.
    if [ -f "$EXT_DIR/out/workspace/diagnostics.js" ]; then
        return 0
    fi
    local builder=""
    if command -v pnpm >/dev/null 2>&1; then builder="pnpm";
    elif command -v npm >/dev/null 2>&1; then builder="npm"; fi
    if [ -z "$builder" ]; then
        echo "no pnpm or npm on this runner, so the extension core cannot be built"
        return 1
    fi
    if ! (cd "$EXT_DIR" && timeout 600 "$builder" run build) >/dev/null 2>&1; then
        echo "the extension core could not be built here (tried $builder run build)"
        return 1
    fi
    if [ ! -f "$EXT_DIR/out/workspace/diagnostics.js" ]; then
        echo "the build ran but produced no out/workspace/diagnostics.js"
        return 1
    fi
    return 0
}

# --- the probe tree --------------------------------------------------------

# The original names the entity after the .def file, so its diagnostics read
# "for Fixture.hp". The fixture is installed as Fixture.def and our checker is
# told the same entity name, or the two halves are comparing different things.
SCRIPTS="$SANDBOX/res/scripts"

install_fixture() { # $1 the fixture .def
    rm -rf "$SCRIPTS"
    mkdir -p "$SCRIPTS/entity_defs" "$SCRIPTS/client" "$SCRIPTS/base" "$SCRIPTS/cell"
    cp "$ALIAS" "$SCRIPTS/entity_defs/alias.xml"
    cp "$1" "$SCRIPTS/entity_defs/Fixture.def"
    cat >"$SCRIPTS/entities.xml" <<'XML'
<root>
	<ClientServerEntities>
		<Fixture/>
	</ClientServerEntities>
	<ServerOnlyEntities/>
</root>
XML
    echo "# fixture client script" >"$SCRIPTS/client/Fixture.py"
    echo "# fixture base script" >"$SCRIPTS/base/Fixture.py"
    echo "# fixture cell script" >"$SCRIPTS/cell/Fixture.py"
}

# --- the original half -----------------------------------------------------

reference_bodies() { # $1 the fixture .def
    # Keep only parser-reported defects. The original writes them as
    # "ERROR: [DataDescription] DataType::buildDataType: <body>", so the tag is
    # stripped first and the "Parser::method:" prefix is what identifies a
    # defect. The cascade lines that follow
    # ("EntityDescriptionMap: Failed to load or parse def …",
    #  "Failed to parse .def files") carry a single colon and are bookkeeping
    # about a failure already reported, not a defect a replacement must
    # reproduce. WARNINGs are excluded by the ERROR anchor.
    #
    # BW_RES_PATH is the probe tree first, then the reference res root — the
    # same order referenceBaseline.sh uses. Without it the original refuses to
    # run at all ("No res path set."), which would read as a divergence.
    BW_RES_PATH="$SANDBOX/res:$RES_ROOT" \
        "$REFERENCE_TOOL" -v -p "$SCRIPTS" 2>&1 \
        | grep '^ERROR: ' \
        | sed -E 's/^ERROR: \[[^]]*\] //' \
        | grep -E '^[^ ]+::[^ ]+: ' \
        | sed 's/^[^:]*::[^:]*: //'
}

# --- the replacement half ----------------------------------------------------

PORT_DRIVER="$SANDBOX/parity_port.cjs"

write_port_driver() {
    cat >"$PORT_DRIVER" <<'JS'
// The replacement half of the differential: run our checker on one .def and
// print one diagnostic body per line, so both halves share a shape.
const fs = require("node:fs");
const path = require("node:path");

const OUT = process.env.CI_PARITY_OUT || process.argv[3];
const { checkDefText } = require(path.join(OUT, "workspace", "diagnostics.js"));
const { AliasRegistry } = require(path.join(OUT, "def", "registry.js"));

const [aliasFile, defFile, entity] = process.argv.slice(2);
const aliases = AliasRegistry.parse(fs.readFileSync(aliasFile, "utf8"));
for (const d of checkDefText(
    aliases,
    fs.readFileSync(defFile, "utf8"),
    entity,
)) {
    console.log(d.message);
}
JS
}

port_bodies() { # $1 the fixture .def
    # The driver is read at call time, not at source time: the self-test swaps
    # it for a stub that reports a different body, and an override applied to a
    # shell function only reaches code that reads the variable again.
    CI_PARITY_OUT="$EXT_DIR/out" \
        node "${CI_PARITY_PORT_DRIVER:-$PORT_DRIVER}" "$ALIAS" "$1" Fixture
}

# --- the lane ---------------------------------------------------------------

# One row per reference diagnostic: it is either reproduced by our checker or it
# is the defect. The record is what makes a red run readable afterwards.
run_fixtures() {
    local rec missing_sub driver_ok=1
    ci_step "compatibility differential"
    rec="$(record_path def)"
    mkdir -p "$CI_OUT/parity" "$CI_LOG_DIR"

    if [ ! -d "$CASES_DIR" ] || ! ls "$CASES_DIR"/*.def >/dev/null 2>&1; then
        ci_skip parity-def "no fixtures under $CASES_DIR; this lane is tracked by BW-00-04-05 (single-component substitution)"
        return 0
    fi
    missing_sub="$(missing_substrates)"
    if [ -n "$missing_sub" ]; then
        ci_skip parity-def "$(tsv_escape "$missing_sub") — substitution path needs BW-00-04-05"
        return 0
    fi
    if ! driver_ok="$(ensure_extension_built)"; then
        ci_skip parity-def "$(tsv_escape "$driver_ok") — extension core tracks BW-20-01-05"
        return 0
    fi
    write_port_driver

    local total=0 reproduced=0 missing=""
    local file name ref port unmatched
    {
        printf 'fixture\treference_diagnostic\tresult\n'
    } >"$rec"

    for file in "$CASES_DIR"/*.def; do
        name="$(basename "$file" .def)"
        total=$((total + 1))
        install_fixture "$file"
        ref="$(reference_bodies "$file")"
        port="$(port_bodies "$file")"
        unmatched=""
        while IFS= read -r body; do
            [ -n "$body" ] || continue
            if printf '%s\n' "$port" | grep -qF "$body"; then
                reproduced=$((reproduced + 1))
                printf '%s\t%s\treproduced\n' "$name" "$(tsv_escape "$body")" >>"$rec"
            else
                unmatched="$unmatched $name:$(printf '%s' "$body" | cut -c1-80)"
                printf '%s\t%s\tnot reproduced\n' "$name" "$(tsv_escape "$body")" >>"$rec"
            fi
        done <<<"$ref"
    done

    cat "$rec" >>"$CI_LOG_DIR/parity-def.log"

    if [ -z "$unmatched" ]; then
        ci_pass parity-def "$reproduced reference diagnostic(s) reproduced across $total fixture(s): $(basename "$rec")"
    else
        ci_fail parity-def "reference diagnostics our checker does not reproduce:$unmatched"
    fi
}

self_test() {
    local fails=0 missing_sub
    ci_step "self-test: a replacement that diverges fails the case"
    if [ ! -d "$CASES_DIR" ] || ! ls "$CASES_DIR"/*.def >/dev/null 2>&1; then
        ci_skip selftest-parity "no fixtures under $CASES_DIR"
        return 0
    fi
    missing_sub="$(missing_substrates)"
    if [ -n "$missing_sub" ]; then
        ci_skip selftest-parity "$(tsv_escape "$missing_sub")"
        return 0
    fi
    if ! ensure_extension_built >/dev/null 2>&1; then
        ci_skip selftest-parity "the extension core cannot be built here"
        return 0
    fi

    # Plant a replacement that reports a different body for the same defect and run
    # the lane's own assertion against it. A self-test that went green here
    # would mean the differential cannot see a wrong replacement at all.
    local stub="$SANDBOX/wrong_port.cjs"
    cat >"$stub" <<'JS'
console.log("DataDescription::parse: this replacement says something else");
JS
    # Only a fixture the original actually complains about can prove the
    # rejection, so find one rather than assuming the first file will do.
    local file ref port body target="" probe
    for probe in "$CASES_DIR"/*.def; do
        install_fixture "$probe"
        if [ -n "$(reference_bodies "$probe")" ]; then
            target="$probe"
            break
        fi
    done
    if [ -z "$target" ]; then
        ci_fail selftest-parity "no fixture produced a reference diagnostic, so a divergence cannot be demonstrated"
        return 1
    fi
    install_fixture "$target"
    ref="$(reference_bodies "$target")"
    port="$(CI_PARITY_PORT_DRIVER="$stub" port_bodies "$target")"
    local diverged=""
    while IFS= read -r body; do
        [ -n "$body" ] || continue
        printf '%s\n' "$port" | grep -qF "$body" || \
            diverged="$diverged $(printf '%s' "$body" | cut -c1-70)"
    done <<<"$ref"
    if [ -n "$diverged" ]; then
        ci_pass selftest-parity "a diverging replacement is rejected by the lane's own assertion: unaccounted$diverged"
    else
        ci_fail selftest-parity "the planted divergence passed the assertion, so this lane cannot bite"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a clean fixture passes without inventing a diagnostic"
    local clean="$SANDBOX/clean.def"
    cat >"$clean" <<'XML'
<root>
	<Properties>
		<hp>
			<Type>INT32</Type>
			<Flags>BASE</Flags>
		</hp>
	</Properties>
</root>
XML
    install_fixture "$clean"
    local ref
    ref="$(reference_bodies "$clean")"
    if [ -z "$ref" ]; then
        ci_pass selftest-parity-clean "a clean fixture reports no reference diagnostic, so there is nothing to diverge from"
    else
        ci_fail selftest-parity-clean "the clean fixture reported unexpected reference diagnostics: $(printf '%s' "$ref" | head -1 | cut -c1-80)"
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
    run_fixtures
}

main "$@"
LANE_RC=$?

set +e
ci_finalize
FINAL_RC=$?
set -e

[ "$LANE_RC" -ne 0 ] && exit "$LANE_RC"
exit "$FINAL_RC"