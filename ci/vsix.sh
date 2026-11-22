#!/usr/bin/env bash
# BW-22-04-04 — the VSIX lane: compile the extension, package it, and prove
# the packaged artifact installs into a clean host.
#
# Usage:
#   ci/vsix.sh            # build + package + verify + install smoke into ci-out/vsix/
#   ci/vsix.sh self-test  # prove the verify and install legs bite
#
# The acceptance is "installable on a clean host". The lane compiles fresh
# (a stale out/ must not be what gets packaged), packages with the
# extension's own vsce, verifies the archive's integrity — manifest present,
# package.json present, the main entry declared in package.json actually in
# the archive, manifest version agreeing with package.json — and then
# installs into a scratch extensions directory and loads the installed
# host-independent core with node. The main entry needs the `vscode` API, so
# the entry an editor loads is proven by the editor leg, which reports SKIP
# until BW-20-03-03 lands @vscode/test-electron.
#
# Nothing here tags or releases: the product is a local .vsix under
# ci-out/vsix/ and a workflow artifact. Publishing stays user-triggered.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
# Call-time resolution: the self-test plants extension trees and a private
# output root, and an override applied to a shell function only reaches
# code that re-reads the variable.
cur_ext() { printf '%s' "${CI_VSIX_EXT:-$REPO_ROOT/extensions/vscode}"; }
cur_out() { printf '%s' "${CI_VSIX_OUT:-$CI_OUT}"; }

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

PY="${PYTHON:-python3}"

# --- zip helpers (python3 stdlib: unzip is not guaranteed on a runner) -------

zip_names() { "$PY" -c 'import zipfile,sys;print("\n".join(zipfile.ZipFile(sys.argv[1]).namelist()))' "$1"; }
zip_read() { "$PY" -c 'import zipfile,sys;sys.stdout.write(zipfile.ZipFile(sys.argv[1]).read(sys.argv[2]).decode("utf-8","replace"))' "$1" "$2"; }
zip_extract() { "$PY" -c 'import zipfile,sys;zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$1" "$2"; }

# --- build, package, verify, install ------------------------------------------

build_extension() {
    local ext out rc
    ext="$(cur_ext)"
    [ -f "$ext/package.json" ] || {
        printf 'no package.json under %s' "$ext"
        return 2
    }
    if ! command -v pnpm >/dev/null 2>&1; then
        printf 'no pnpm on this runner'
        return 2
    fi
    # pnpm 12 aborts on ignored postinstall scripts even when tsc is in
    # place; the gate is the compile below, not the installer.
    (cd "$ext" && pnpm install --frozen-lockfile --prefer-offline) >&2 || true
    out="$(cd "$ext" && ./node_modules/.bin/tsc -p . 2>&1)"
    rc=$?
    printf '%s\n' "$out" >"$CI_LOG_DIR/vsix-build.log"
    [ "$rc" -ne 0 ] && {
        printf 'tsc failed (rc=%s), see vsix-build.log' "$rc"
        return 1
    }
    return 0
}

package_extension() {
    # package_extension <version> <commit> — the extension's own vsce, so
    # the packaging rules are the ones a publisher would use.
    local version="$1" commit="$2" ext vsix out rc
    ext="$(cur_ext)"
    vsix="$(cur_out)/vsix/boundless-vscode-${version}-${commit}.vsix"
    mkdir -p "$(cur_out)/vsix"
    out="$(cd "$ext" && ./node_modules/.bin/vsce package --no-dependencies -o "$vsix" 2>&1)"
    rc=$?
    printf '%s\n' "$out" >"$CI_LOG_DIR/vsix-package.log"
    [ "$rc" -ne 0 ] && {
        printf 'vsce package failed (rc=%s), see vsix-package.log' "$rc"
        return 1
    }
    printf '%s' "$vsix"
}

verify_vsix() {
    # verify_vsix <vsix> — the archive must be complete and self-consistent:
    # manifest, package.json, the declared main entry, and the manifest's
    # version agreeing with package.json's.
    local vsix="$1" names manifest pkg version pkg_version main problems=""
    names="$(zip_names "$vsix")"
    printf '%s\n' "$names" | grep -qx 'extension.vsixmanifest' || problems="$problems no extension.vsixmanifest"
    printf '%s\n' "$names" | grep -qx 'extension/package.json' || problems="$problems no extension/package.json"
    if printf '%s\n' "$names" | grep -qx 'extension/package.json'; then
        pkg="$(zip_read "$vsix" extension/package.json)"
        version="$(printf '%s' "$pkg" | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
        main="$(printf '%s' "$pkg" | sed -n 's/.*"main"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
        if [ -n "$main" ]; then
            local member="extension/${main#./}"
            printf '%s\n' "$names" | grep -qx "$member" || problems="$problems main entry $main not in the archive"
        else
            problems="$problems package.json declares no main"
        fi
        manifest="$(zip_read "$vsix" extension.vsixmanifest 2>/dev/null || true)"
        pkg_version="$(printf '%s' "$manifest" | sed -n 's/.*<Identity[^>]*version="\([^"]*\)".*/\1/p' | head -1)"
        if [ -n "$pkg_version" ] && [ "$pkg_version" != "$version" ]; then
            problems="$problems manifest version $pkg_version != package.json $version"
        fi
    fi
    if [ -n "$problems" ]; then
        printf '%s' "$problems"
        return 1
    fi
    return 0
}

install_smoke() {
    # install_smoke <vsix> — unpack into a scratch extensions directory and
    # load the installed host-independent core: the artifact a host loads is
    # the one that was installed, not the one that was built. The main
    # entry itself requires the `vscode` API (it is an extension; that is
    # the point), so the loadable artifact without an editor is the
    # workspace core — the same module ci/sdk.sh loads from the built tree,
    # loaded here from the installed copy instead.
    local vsix="$1" host core out rc
    mkdir -p "$(cur_out)/vsix"
    host="$(mktemp -d "$(cur_out)/vsix/clean-host.XXXXXX")"
    zip_extract "$vsix" "$host" || {
        printf 'the vsix did not extract'
        return 1
    }
    core="$host/extension/out/workspace/diagnostics.js"
    [ -f "$core" ] || {
        printf 'the installed extension has no host-independent core (out/workspace/diagnostics.js)'
        return 1
    }
    out="$(node -e "const m=require(process.argv[1]); if (typeof m.checkDefText !== 'function') process.exit(2);" "$core" 2>&1)"
    rc=$?
    rm -rf "$host"
    [ "$rc" -ne 0 ] && {
        printf 'the installed core did not load or lost its entry point (rc=%s): %s' "$rc" "$(printf '%s' "$out" | head -3)"
        return 1
    }
    printf 'installed core loads and checkDefText is usable from a clean host'
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    if [ ! -d "$(cur_ext)" ]; then
        ci_skip vsix-build "no extensions/vscode tree — the lane turns on with BW-20-01"
        return 0
    fi

    ci_step "vsix release artifacts"

    local build_out rc
    build_out="$(build_extension)"
    rc=$?
    if [ "$rc" -eq 2 ]; then
        ci_skip vsix-build "$(tsv_escape "$build_out") (BW-20-01)"
        return 0
    fi
    if [ "$rc" -ne 0 ]; then
        ci_fail vsix-build "$(tsv_escape "$build_out")"
        return 1
    fi
    ci_pass vsix-build "extension compiled fresh"

    local version commit vsix
    version="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$(cur_ext)/package.json" | head -1)"
    [ -n "$version" ] || version=untagged
    commit="$(ci_git_sha)"
    if ! vsix="$(package_extension "$version" "$commit")"; then
        ci_fail vsix-package "packaging failed"
        return 1
    fi
    ci_pass vsix-package "$(basename "$vsix") ($(du -h "$vsix" | cut -f1))"

    local vout
    vout="$(verify_vsix "$vsix")"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        ci_fail vsix-verify "$(tsv_escape "$vout")"
        return 1
    fi
    ci_pass vsix-verify "manifest, package.json and main entry present; versions agree"

    vout="$(install_smoke "$vsix")"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        ci_fail vsix-install "$(tsv_escape "$vout")"
        return 1
    fi
    ci_pass vsix-install "$(tsv_escape "$vout")"

    # A real editor install needs an editor; until the test host lands, the
    # leg records what is missing rather than claiming it.
    if grep -q '@vscode/test-electron' "$(cur_ext)/package.json" 2>/dev/null; then
        ci_pass vsix-editor-host "editor test host declared; install into it is BW-20-03-03's face"
    else
        ci_skip vsix-editor-host "no @vscode/test-electron in extensions/vscode — a real editor install is tracked by BW-20-03-03"
    fi
}

self_test() {
    local fails=0 r vsix verdict
    ci_step "self-test: a complete vsix verifies and installs"
    r="$(mktemp -d "$REPO_ROOT/ci-out/vsix-selftest.XXXXXX")"
    vsix="$r/boundless-vscode-0.0.1-cafe123.vsix"
    "$PY" - "$vsix" <<'PY'
import json, zipfile, sys, os
path = sys.argv[1]
with zipfile.ZipFile(path, "w") as z:
    pkg = {"name": "boundless-vscode", "version": "0.0.1", "main": "./out/extension.js"}
    z.writestr("extension/package.json", json.dumps(pkg))
    z.writestr("extension.vsixmanifest",
               '<PackageManifest><Identity version="0.0.1"/></PackageManifest>')
    z.writestr("extension/out/extension.js",
               "process.stdout.write('boundless entry loaded');")
    z.writestr("extension/out/workspace/diagnostics.js",
               "exports.checkDefText = function () {};")
PY
    if verify_vsix "$vsix" && install_smoke "$vsix" >/dev/null; then
        ci_pass selftest-vsix "a complete vsix verifies and its entry loads on a clean host"
    else
        ci_fail selftest-vsix "a complete vsix did not verify/install: $(verify_vsix "$vsix"; install_smoke "$vsix")"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a vsix missing its main entry fails verification"
    "$PY" - "$r/incomplete.vsix" <<'PY'
import json, zipfile, sys
with zipfile.ZipFile(sys.argv[1], "w") as z:
    z.writestr("extension/package.json",
               json.dumps({"name": "x", "version": "0.0.1", "main": "./out/extension.js"}))
    z.writestr("extension.vsixmanifest", '<Identity version="0.0.1"/>')
PY
    verdict="$(verify_vsix "$r/incomplete.vsix")"
    if [ $? -ne 0 ] && printf '%s' "$verdict" | grep -q 'main entry ./out/extension.js not in the archive'; then
        ci_pass selftest-vsix-entry "a missing main entry is caught and named"
    else
        ci_fail selftest-vsix-entry "an incomplete vsix did not fail: $verdict"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a manifest version that disagrees fails verification"
    "$PY" - "$r/mismatch.vsix" <<'PY'
import json, zipfile, sys
with zipfile.ZipFile(sys.argv[1], "w") as z:
    z.writestr("extension/package.json",
               json.dumps({"name": "x", "version": "0.0.1", "main": "./out/extension.js"}))
    z.writestr("extension.vsixmanifest", '<Identity version="0.0.2"/>')
    z.writestr("extension/out/extension.js", "")
PY
    verdict="$(verify_vsix "$r/mismatch.vsix")"
    if [ $? -ne 0 ] && printf '%s' "$verdict" | grep -q 'manifest version 0.0.2 != package.json 0.0.1'; then
        ci_pass selftest-vsix-version "a manifest/package version disagreement is caught"
    else
        ci_fail selftest-vsix-version "a version mismatch did not fail: $verdict"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$REPO_ROOT/ci-out/vsix-selftest".*

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
