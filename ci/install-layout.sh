#!/usr/bin/env bash
# BW-14-04 — installable-artifact layout: stage a complete install prefix
# (native binaries + CLI, the CPython module, config + sample resources),
# archive it, and verify the install / upgrade / uninstall data-retention
# contract the acceptance items demand.
#
# Usage:
#   ci/install-layout.sh            # stage + archive + install contract
#   ci/install-layout.sh self-test  # prove the checks bite on both outcomes
#
# The layout an installer lays down, mirroring the reference /opt/bigworld
# convention under /opt/boundless (compare docs/review/bw14-04-01-...md):
#   bin/boundless-server    the server role binary (CLI: boundless-server <role>)
#   bin/process_defs        the entitydef CLI (CLI: process_defs [-r res] ...)
#   lib/python/boundless.so the CPython module (the cdylib's install rename,
#                           the exact step crates/python-module/tests/import.rs
#                           pins: CPython resolves `boundless.so`, not `lib-`)
#   res/server/bw.xml       server configuration (BW-01-04 config chain)
#   res/scripts/**          sample entity definitions and scripts (BW-14-01)
#   data/                   user-data dir — NOT packaged: install creates it,
#                           upgrade leaves it, uninstall keeps it. That is the
#                           whole data-retention contract (BW-14-04-05).
#
# The archive carries only the package-owned tree (bin lib res). `data/` is
# deliberately outside it, which is exactly what lets upgrade and uninstall
# preserve user data.
#
# Repo rules honoured here (also global red lines): nothing tags or
# releases; the product is a local archive under ci-out/install-layout/ and
# a workflow artifact. Versioned releases stay user-triggered.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
SERVER=boundless-server
CLI=process_defs
FIXTURE=docs/review/fixtures/fantasydemo_def/res
ROLES_SOURCE=crates/server-runtime/src/app.rs

# Call-time resolution for the self-test: stubs are planted behind these and
# an override only reaches code that re-reads the variable at call time.
cur_bin() { printf '%s' "${CI_LAYOUT_BIN:-}"; }
cur_cli() { printf '%s' "${CI_LAYOUT_CLI:-}"; }
cur_mod() { printf '%s' "${CI_LAYOUT_MOD:-}"; }
cur_res() { printf '%s' "${CI_LAYOUT_RES:-}"; }
cur_out() { printf '%s' "${CI_LAYOUT_OUT:-$CI_OUT}"; }
cur_py() { printf '%s' "${CI_LAYOUT_PY:-python3}"; }

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

# --- platform -------------------------------------------------------------

platform_name() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    MINGW* | MSYS* | CYGWIN*) os=windows ;;
    *) ;;
    esac
    printf '%s-%s' "$os" "$arch"
}

first_role() {
    [ -f "$REPO_ROOT/$ROLES_SOURCE" ] || return 1
    sed -n '/pub const KNOWN_ROLES/,/\];/p' "$REPO_ROOT/$ROLES_SOURCE" \
        | grep -oE '"[A-Za-z0-9_-]+"' | tr -d '"' | head -1
}

# --- build, stage, archive -------------------------------------------------

build_artifacts() {
    local out rc
    out="$(cargo build --release -p boundless-server -p entitydef -p python-module 2>&1)"
    rc=$?
    printf '%s\n' "$out" >"$CI_LOG_DIR/install-layout-build.log"
    return "$rc"
}

# stage_prefix <prefix> — the layout an installer lays down. Only the
# package-owned tree; `data/` is never staged (install creates it).
stage_prefix() {
    local prefix="$1" bin cli mod res
    bin="$(cur_bin)"
    cli="$(cur_cli)"
    mod="$(cur_mod)"
    res="$(cur_res)"

    [ -n "$bin" ] && [ -f "$bin" ] || { echo "no server binary to stage"; return 1; }
    [ -n "$cli" ] && [ -f "$cli" ] || { echo "no process_defs binary to stage"; return 1; }
    [ -n "$mod" ] || { echo "no python module to stage"; return 1; }
    [ -n "$res" ] || { echo "no res tree to stage"; return 1; }

    mkdir -p "$prefix/bin" "$prefix/lib/python"
    cp "$bin" "$prefix/bin/$SERVER" || return 1
    chmod +x "$prefix/bin/$SERVER"
    cp "$cli" "$prefix/bin/$CLI" || return 1
    chmod +x "$prefix/bin/$CLI"

    # A directory module (the self-test's pure-Python stub) lands as its
    # contents under lib/python; a file module is the install rename to the
    # name CPython resolves.
    if [ -d "$mod" ]; then
        cp -a "$mod/." "$prefix/lib/python/" || return 1
    else
        cp "$mod" "$prefix/lib/python/boundless.so" || return 1
    fi

    cp -a "$res" "$prefix/res" || return 1
    [ -f "$prefix/res/server/bw.xml" ] || { echo "staged res has no server/bw.xml"; return 1; }
}

archive_prefix() {
    local prefix="$1" tarball="$2"
    tar -C "$prefix" -czf "$tarball" bin lib res
}

# --- install / upgrade / uninstall -----------------------------------------

install_prefix() {
    # install_prefix <tarball> <prefix> — unpack into a clean prefix and
    # create the (empty) user-data dir the package does not own.
    local tarball="$1" prefix="$2"
    rm -rf "$prefix"
    mkdir -p "$prefix"
    tar -C "$prefix" -xzf "$tarball" >&2 || return 1
    mkdir -p "$prefix/data"
}

upgrade_prefix() {
    # upgrade_prefix <tarball> <prefix> — unpack over the existing prefix.
    # tar replaces the members it carries (bin lib res) and leaves every
    # other path alone, which is exactly the upgrade contract: package
    # content is replaced, `data/` is untouched.
    local tarball="$1" prefix="$2"
    [ -d "$prefix" ] || { echo "nothing to upgrade at $prefix"; return 1; }
    tar -C "$prefix" -xzf "$tarball" >&2 || return 1
}

uninstall_prefix() {
    # uninstall_prefix <prefix> — remove the package-owned tree, keep the
    # user-data dir (and the prefix itself).
    local prefix="$1"
    [ -d "$prefix" ] || { echo "nothing to uninstall at $prefix"; return 1; }
    rm -rf "${prefix:?}/bin" "${prefix:?}/lib" "${prefix:?}/res"
}

# --- checks ----------------------------------------------------------------

# server_launch <prefix> — the installed server binary starts under a role
# and stops cleanly on the runners-lane budget (start AND stop, not just
# start): exit 0 plus the startup line. This is BW-14-04-01/04's observable.
server_launch() {
    local prefix="$1" role out rc
    role="$(first_role)"
    [ -n "$role" ] || { echo "no role list at $ROLES_SOURCE"; return 1; }
    [ -x "$prefix/bin/$SERVER" ] || { echo "no installed bin/$SERVER"; return 2; }
    out="$(CI_RUNNERS_TIMEOUT=3 timeout 30 "$prefix/bin/$SERVER" "$role" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'exit %s: %s' "$rc" "$out"
        return 3
    fi
    if ! printf '%s\n' "$out" | grep -q 'starting'; then
        printf 'exit 0 but no startup line: %s' "$out"
        return 4
    fi
    if ! printf '%s\n' "$out" | grep -q 'has shut down'; then
        printf 'exit 0 but no shutdown line: %s' "$out"
        return 5
    fi
    printf '%s' "$out"
}

# cli_launch <prefix> — the installed CLI runs: `--help` is the reference
# contract (usage on stderr, exit 0).
cli_launch() {
    local prefix="$1" out rc
    [ -x "$prefix/bin/$CLI" ] || { echo "no installed bin/$CLI"; return 2; }
    out="$("$prefix/bin/$CLI" --help 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'exit %s: %s' "$rc" "$out"
        return 3
    fi
    if ! printf '%s\n' "$out" | grep -q 'usage: process_defs'; then
        printf 'exit 0 but no usage line: %s' "$out"
        return 4
    fi
    printf '%s' "$out"
}

# cli_res_load <prefix> — the installed CLI locates and loads the installed
# config + sample resources on its first run: `process_defs -r <res>` parses
# the packaged entity definitions. This is the installed-tool half of
# BW-14-04-03 ("首次启动可定位").
cli_res_load() {
    local prefix="$1" out rc
    [ -d "$prefix/res" ] || { echo "no installed res tree"; return 2; }
    out="$("$prefix/bin/$CLI" -r "$prefix/res" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'exit %s: %s' "$rc" "$out"
        return 3
    fi
    if ! printf '%s\n' "$out" | grep -q '"digest"'; then
        printf 'exit 0 but no def digest in dump: %.200s' "$out"
        return 4
    fi
    printf '%s' "$out"
}

# module_import <prefix> — the installed CPython module imports into a bare
# interpreter with no dev environment: no cargo, no target dir, no venv.
module_import() {
    local prefix="$1" py out rc
    py="$(cur_py)"
    [ -f "$prefix/lib/python/boundless.so" ] || [ -d "$prefix/lib/python/boundless" ] \
        || { echo "no installed boundless module"; return 2; }
    out="$(PYTHONPATH="$prefix/lib/python" "$py" -c '
import boundless
assert boundless.__name__ == "boundless", boundless.__name__
assert boundless.__version__, "missing __version__"
assert callable(boundless.version), "missing version()"
assert boundless.version() == boundless.__version__, "version() != __version__"
print("boundless", boundless.__version__, "imported")
' 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'import failed: %s' "$out"
        return 3
    fi
    if ! printf '%s\n' "$out" | grep -q 'imported'; then
        printf 'no import line: %s' "$out"
        return 4
    fi
    printf '%s' "$out"
}

# layout_present <prefix> — the staged layout is complete: config, sample
# resources, both binaries, the module and the user-data dir.
layout_present() {
    local prefix="$1"
    [ -f "$prefix/res/server/bw.xml" ] || { echo "missing res/server/bw.xml"; return 1; }
    [ -f "$prefix/res/scripts/entities.xml" ] || { echo "missing res/scripts/entities.xml"; return 1; }
    [ -f "$prefix/res/scripts/entity_defs/Account.def" ] || { echo "missing Account.def"; return 1; }
    [ -x "$prefix/bin/$SERVER" ] || { echo "bin/$SERVER not executable"; return 1; }
    [ -x "$prefix/bin/$CLI" ] || { echo "bin/$CLI not executable"; return 1; }
    if [ ! -f "$prefix/lib/python/boundless.so" ] && [ ! -d "$prefix/lib/python/boundless" ]; then
        echo "lib/python/boundless module missing"
        return 1
    fi
    [ -d "$prefix/data" ] || { echo "data dir missing"; return 1; }
}

# --- the lane --------------------------------------------------------------

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    local platform
    platform="$(platform_name)"
    ci_step "installable layout for $platform"

    local version tarball staged
    version="$(ci_version)"
    [ -n "$version" ] || version=untagged
    tarball="$(cur_out)/install-layout/boundless-${version}-${platform}.tar.gz"
    staged="$(cur_out)/install-layout/stage"
    mkdir -p "$(cur_out)/install-layout"

    # Build unless the self-test handed us stubs. The three artifacts are
    # the three package faces: the server binary, the entitydef CLI, the
    # CPython module.
    local bin cli mod
    bin="$(cur_bin)"
    cli="$(cur_cli)"
    mod="$(cur_mod)"
    if [ -z "$bin" ] || [ -z "$cli" ] || [ -z "$mod" ]; then
        if ! command -v cargo >/dev/null 2>&1; then
            ci_skip install-build "no cargo on this runner; the pinned toolchain is the toolchain lane's product (BW-22-01)"
            ci_note "build skipped; stage/archive/install need the artifacts"
            return 0
        fi
        ci_step "release build"
        if ! build_artifacts; then
            ci_fail install-build "cargo build --release -p boundless-server -p entitydef -p python-module failed; see install-layout-build.log"
            return 1
        fi
        ci_pass install-build "release build of $SERVER, $CLI, python-module"
        bin="$REPO_ROOT/target/release/$SERVER"
        cli="$REPO_ROOT/target/release/$CLI"
        mod="$REPO_ROOT/target/release/libboundless.so"
    fi

    for f in "$bin" "$cli" "$mod"; do
        [ -e "$f" ] || { ci_fail install-stage "missing artifact: $f"; return 1; }
    done

    # Hand the resolved artifacts to the staging step through the same
    # env-based knobs the self-test uses, so stage_prefix sees one source
    # of truth.
    CI_LAYOUT_BIN="$bin"
    CI_LAYOUT_CLI="$cli"
    CI_LAYOUT_MOD="$mod"

    CI_LAYOUT_RES="${CI_LAYOUT_RES:-$REPO_ROOT/$FIXTURE}"
    rm -rf "$staged"
    if ! stage_prefix "$staged"; then
        ci_fail install-stage "staging the prefix failed"
        return 1
    fi
    ci_pass install-stage "prefix staged: bin/ lib/ res/ (no data/ by design)"

    if ! archive_prefix "$staged" "$tarball"; then
        ci_fail install-archive "tar -czf failed for $tarball"
        return 1
    fi
    ci_pass install-archive "$(basename "$tarball") ($(du -h "$tarball" | cut -f1))"

    local prefix
    prefix="$(cur_out)/install-layout/install-smoke"
    if ! install_prefix "$tarball" "$prefix"; then
        ci_fail install-clean "clean install failed"
        return 1
    fi
    ci_pass install-clean "clean install into $prefix (data/ created)"

    # BW-14-04-01: the installed commands execute.
    local out
    if ! out="$(server_launch "$prefix")"; then
        ci_fail install-smoke-server "installed $SERVER did not start+stop cleanly: $(tsv_escape "$out")"
    else
        ci_pass install-smoke-server "installed $SERVER starts and stops: $(tsv_escape "$out" | cut -c1-80)"
    fi
    if ! out="$(cli_launch "$prefix")"; then
        ci_fail install-cli "installed $CLI did not run: $(tsv_escape "$out")"
    else
        ci_pass install-cli "installed $CLI runs (--help)"
    fi

    # BW-14-04-03 (installed-tool half): first run of an installed command
    # locates and loads the installed config + sample resources.
    if ! out="$(cli_res_load "$prefix")"; then
        ci_fail install-cli-res "installed $CLI could not locate/load the installed res tree: $(tsv_escape "$out")"
    else
        ci_pass install-cli-res "installed res tree locatable and loads on first run"
    fi

    # BW-14-04-02: the installed module imports with no dev environment.
    if ! out="$(module_import "$prefix")"; then
        ci_fail install-module "installed boundless module did not import bare: $(tsv_escape "$out")"
    else
        ci_pass install-module "installed module imports bare: $(tsv_escape "$out")"
    fi

    # BW-14-04-03/05: layout completeness + upgrade/uninstall retention.
    if ! out="$(layout_present "$prefix")"; then
        ci_fail install-layout "installed layout incomplete: $(tsv_escape "$out")"
    else
        ci_pass install-layout "installed layout complete (config, res, binaries, module, data)"
    fi

    # BW-14-04-05: upgrade replaces package content and keeps user data;
    # uninstall removes the package and keeps user data.
    local datafile marker
    datafile="$prefix/data/db/user.db"
    marker="$prefix/res/server/bw.xml"
    mkdir -p "$prefix/data/db"
    printf 'user-data-marker' >"$datafile"
    # Tamper a package-owned file: upgrade must restore the packaged bytes.
    printf 'tampered' >"$marker"
    if ! upgrade_prefix "$tarball" "$prefix"; then
        ci_fail install-upgrade "upgrade failed"
    else
        if grep -q 'tampered' "$marker" 2>/dev/null; then
            ci_fail install-upgrade "upgrade did not replace the package-owned file"
        elif [ ! -f "$datafile" ] || [ "$(cat "$datafile")" != "user-data-marker" ]; then
            ci_fail install-upgrade "upgrade lost user data at $datafile"
        else
            ci_pass install-upgrade "upgrade replaced package content and preserved data/"
        fi
    fi
    if ! uninstall_prefix "$prefix"; then
        ci_fail install-uninstall "uninstall failed"
    elif [ ! -f "$datafile" ] || [ "$(cat "$datafile")" != "user-data-marker" ]; then
        ci_fail install-uninstall "uninstall lost user data at $datafile"
    elif [ -d "$prefix/bin" ] || [ -d "$prefix/lib" ] || [ -d "$prefix/res" ]; then
        ci_fail install-uninstall "uninstall left package-owned dirs behind"
    else
        ci_pass install-uninstall "uninstall removed package and preserved data/"
    fi
}

# --- self-test -------------------------------------------------------------

make_stub() {
    local path="$1" body="$2"
    cat >"$path" <<STUB
#!/bin/sh
$body
STUB
    chmod +x "$path"
}

self_test() {
    local fails=0 r rc out
    ci_step "self-test: stubs round-trip stage, archive, install and the checks"
    r="$(mktemp -d "$REPO_ROOT/ci-out/install-layout-selftest.XXXXXX")"

    local bin cli mod res py
    bin="$r/server-stub"
    cli="$r/cli-stub"
    make_stub "$bin" 'echo "[baseapp] starting (stub)" >&2
echo "[baseapp] has shut down." >&2
exit 0'
    # The green CLI stub models the real CLI's observable contract: `--help`
    # prints the usage and succeeds; `-r <res>` loads the defs and dumps a
    # digest, or fails with the parse error when the res tree lost them.
    make_stub "$cli" 'case "$1" in
--help) echo "usage: process_defs [OPTION] scriptArgs" >&2; exit 0 ;;
-r)
    if [ -d "$2/scripts/entity_defs" ]; then
        echo '"'"'"digest": "0123456789abcdef0123456789abcdef"'"'"'
        exit 0
    fi
    echo "Failed to parse .def files" >&2
    exit 1 ;;
esac
exit 1'

    # The green module stub is a pure-Python package dir: `import boundless`
    # resolves `lib/python/boundless/__init__.py`, the same observable the
    # real `boundless.so` satisfies. The real layout ships the extension
    # module; the stub proves the check measures the import, not the build.
    mod="$r/module"
    mkdir -p "$mod/boundless"
    cat >"$mod/boundless/__init__.py" <<'PY'
__version__ = "0.0-stub"
def version():
    return __version__
PY

    res="$r/res"
    cp -a "$REPO_ROOT/$FIXTURE" "$res"

    out="$(CI_OUT="$r/manifest" CI_LAYOUT_OUT="$r/out" CI_LAYOUT_BIN="$bin" CI_LAYOUT_CLI="$cli" \
        CI_LAYOUT_MOD="$mod" CI_LAYOUT_RES="$res" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -eq 0 ] \
        && printf '%s\n' "$out" | grep -q 'install-module	PASS' \
        && printf '%s\n' "$out" | grep -q 'install-cli-res	PASS' \
        && printf '%s\n' "$out" | grep -q 'install-uninstall	PASS'; then
        ci_pass selftest-install "a green layout installs, launches, imports and retains data"
    else
        ci_fail selftest-install "the green stubs did not survive the contract: $(printf '%s\n' "$out" | grep -E 'install-(smoke|cli|module|uninstall)' | head -2)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a server stub that exits non-zero goes red at smoke"
    make_stub "$bin" 'echo "stub-server: explosion" >&2
exit 3'
    out="$(CI_OUT="$r/manifest" CI_LAYOUT_OUT="$r/out" CI_LAYOUT_BIN="$bin" CI_LAYOUT_CLI="$cli" \
        CI_LAYOUT_MOD="$mod" CI_LAYOUT_RES="$res" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ] && printf '%s\n' "$out" | grep -q 'install-smoke-server	FAIL.*exit 3'; then
        ci_pass selftest-server-red "a launch failure is named with its exit code"
    else
        ci_fail selftest-server-red "the red server stub did not fail the smoke: $(printf '%s\n' "$out" | grep install-smoke-server | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a CLI stub that exits non-zero goes red at the CLI check"
    make_stub "$bin" 'echo "[baseapp] starting (stub)" >&2
echo "[baseapp] has shut down." >&2
exit 0'
    make_stub "$cli" 'echo "stub-cli: no such option" >&2
exit 1'
    out="$(CI_OUT="$r/manifest" CI_LAYOUT_OUT="$r/out" CI_LAYOUT_BIN="$bin" CI_LAYOUT_CLI="$cli" \
        CI_LAYOUT_MOD="$mod" CI_LAYOUT_RES="$res" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ] && printf '%s\n' "$out" | grep -q 'install-cli	FAIL.*exit 1'; then
        ci_pass selftest-cli-red "a broken CLI is named with its exit code"
    else
        ci_fail selftest-cli-red "the red CLI stub did not fail the CLI check: $(printf '%s\n' "$out" | grep install-cli | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: an unloadable module goes red at the import check"
    # The green CLI stub models the real CLI's observable contract: `--help`
    # prints the usage and succeeds; `-r <res>` loads the defs and dumps a
    # digest, or fails with the parse error when the res tree lost them.
    make_stub "$cli" 'case "$1" in
--help) echo "usage: process_defs [OPTION] scriptArgs" >&2; exit 0 ;;
-r)
    if [ -d "$2/scripts/entity_defs" ]; then
        echo '"'"'"digest": "0123456789abcdef0123456789abcdef"'"'"'
        exit 0
    fi
    echo "Failed to parse .def files" >&2
    exit 1 ;;
esac
exit 1'
    rm -rf "$mod"
    mkdir -p "$mod"
    printf 'not a shared object' >"$mod/boundless.so"
    out="$(CI_OUT="$r/manifest" CI_LAYOUT_OUT="$r/out" CI_LAYOUT_BIN="$bin" CI_LAYOUT_CLI="$cli" \
        CI_LAYOUT_MOD="$mod" CI_LAYOUT_RES="$res" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ] && printf '%s\n' "$out" | grep -q 'install-module	FAIL.*import failed'; then
        ci_pass selftest-module-red "an unloadable module fails the import check"
    else
        ci_fail selftest-module-red "the junk module did not fail the import: $(printf '%s\n' "$out" | grep install-module | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a res tree without the entity defs goes red at the res-load check"
    # Staging must still succeed (bw.xml is there); what fails is the
    # installed CLI loading definitions from a res tree that lost them.
    rm -rf "$res"
    cp -a "$REPO_ROOT/$FIXTURE" "$res"
    rm -rf "$res/scripts/entity_defs"
    out="$(CI_OUT="$r/manifest" CI_LAYOUT_OUT="$r/out" CI_LAYOUT_BIN="$bin" CI_LAYOUT_CLI="$cli" \
        CI_LAYOUT_MOD="$mod" CI_LAYOUT_RES="$res" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ] && printf '%s\n' "$out" | grep -q 'install-cli-res	FAIL'; then
        ci_pass selftest-res-red "an unloadable res tree fails the res-load check"
    else
        ci_fail selftest-res-red "the empty res tree did not fail the res-load: $(printf '%s\n' "$out" | grep install-cli-res | head -1)"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$REPO_ROOT/ci-out/install-layout-selftest".*

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
exit "$FINAL_RC"
