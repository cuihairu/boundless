#!/usr/bin/env bash
# BW-22-04-02 — the docs release lane: build the site, archive it and stamp
# a versioned index whose metadata must agree across every source.
#
# Usage:
#   ci/docs-package.sh            # build + index + archive + consistency into ci-out/docs/
#   ci/docs-package.sh self-test  # prove the consistency gate bites
#
# The acceptance is "version metadata consistent". The index (index.json)
# records version, commit, generated time and the built page count; the
# consistency leg then cross-checks every independent source of version
# metadata in the tree — the workspace Cargo.toml, docs/package.json's own
# version field, a siteVersion declared in the VitePress config, and the
# index stamp against the live commit — and fails naming whichever source
# disagrees. Sources that do not exist yet are recorded as absent, not
# invented.
#
# Nothing here tags or releases: the product is a local archive and index
# under ci-out/docs/ and a workflow artifact. Versioned releases stay
# user-triggered (repo red line); this lane only reads the workspace
# version for names.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
# Call-time resolution: the self-test plants a tree and a private output
# root, and an override applied to a shell function only reaches code that
# re-reads the variable.
cur_root() { printf '%s' "${CI_DOCSPKG_ROOT:-$REPO_ROOT}"; }
cur_out() { printf '%s' "${CI_DOCSPKG_OUT:-$CI_OUT}"; }

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

# --- version metadata sources ---------------------------------------------------

# The workspace version, read from the tree under cur_root (the self-test
# plants its own Cargo.toml; this mirrors lib.sh's ci_version but scoped).
ws_version() {
    sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' \
        "$(cur_root)/Cargo.toml" 2>/dev/null | head -1
}

tree_commit() {
    git -C "$(cur_root)" rev-parse --short HEAD 2>/dev/null || echo norev
}

# docs/package.json's own version field, if the tree declares one.
docs_pkg_version() {
    sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
        "$(cur_root)/docs/package.json" 2>/dev/null | head -1
}

# A siteVersion declared in the VitePress config, if boundless5 adds one.
docs_site_version() {
    grep -E 'siteVersion' "$(cur_root)/docs/.vitepress/config.mjs" 2>/dev/null \
        | sed -n 's/.*"\([^"]*\)".*/\1/p' | head -1
}

# --- build, index, package, consistency ---------------------------------------

build_site() {
    local docs_dir out rc
    docs_dir="$(cur_root)/docs"
    # The lockfile is the reproducibility contract; a tree without one is
    # not installable in a pinned way and the build must say so.
    if [ ! -f "$docs_dir/pnpm-lock.yaml" ]; then
        printf 'no pnpm-lock.yaml under docs/ — install is not pinned'
        return 2
    fi
    # pnpm 12 aborts on ignored postinstall scripts even when the tools we
    # need are in place; the gate is the build below, not the installer.
    pnpm --dir "$docs_dir" install --frozen-lockfile --prefer-offline >&2 || true
    out="$(pnpm --dir "$docs_dir" run build 2>&1)"
    rc=$?
    printf '%s\n' "$out" >"$CI_LOG_DIR/docs-package-build.log"
    [ "$rc" -ne 0 ] && {
        printf 'vitepress build failed (rc=%s), see docs-package-build.log' "$rc"
        return 1
    }
    if [ ! -f "$docs_dir/.vitepress/dist/index.html" ]; then
        printf 'build exited 0 but produced no dist/index.html'
        return 3
    fi
    return 0
}

dist_dir() { printf '%s/docs/.vitepress/dist' "$(cur_root)"; }

write_index() {
    # write_index <version> <commit> — stamp what was actually built.
    local version="$1" commit="$2" dist pages bytes
    dist="$(dist_dir)"
    pages="$(find "$dist" -name '*.html' 2>/dev/null | wc -l | tr -d ' ')"
    bytes="$(du -sb "$dist" 2>/dev/null | cut -f1)"
    mkdir -p "$(cur_out)/docs"
    {
        printf '{\n'
        printf '  "artifact": "boundless-docs",\n'
        printf '  "version": "%s",\n' "$version"
        printf '  "commit": "%s",\n' "$commit"
        printf '  "generated": "%s",\n' "$(ci_now)"
        printf '  "pages": %s,\n' "${pages:-0}"
        printf '  "bytes": %s\n' "${bytes:-0}"
        printf '}\n'
    } >"$(cur_out)/docs/index.json"
}

package_site() {
    # package_site <version> <commit> — the distributable site archive.
    local version="$1" commit="$2" tarball dist
    dist="$(dist_dir)"
    tarball="$(cur_out)/docs/boundless-docs-${version}-${commit}.tar.gz"
    mkdir -p "$(cur_out)/docs"
    tar -C "$(dirname "$dist")" -czf "$tarball" dist >&2 || return 1
    printf '%s' "$tarball"
}

check_consistency() {
    # check_consistency <version> <commit> — the acceptance: every
    # independent source of version metadata agrees with the workspace.
    # Prints one finding per line; a non-empty mismatch set fails the lane.
    local version="$1" commit="$2" mismatches="" notes=""

    [ -n "$version" ] || mismatches="$mismatches workspace Cargo.toml declares no version"

    local dp
    dp="$(docs_pkg_version)"
    if [ -n "$dp" ] && [ "$dp" != "$version" ]; then
        mismatches="$mismatches docs/package.json version $dp != workspace $version"
    elif [ -z "$dp" ]; then
        notes="$notes docs/package.json declares no version (absent is recorded, not invented)"
    fi

    local sv
    sv="$(docs_site_version)"
    if [ -n "$sv" ] && [ "$sv" != "$version" ]; then
        mismatches="$mismatches config.mjs siteVersion $sv != workspace $version"
    elif [ -n "$sv" ]; then
        notes="$notes config.mjs siteVersion agrees"
    fi

    local idx idx_version idx_commit
    idx="$(cur_out)/docs/index.json"
    if [ -f "$idx" ]; then
        idx_version="$(sed -n 's/.*"version":[[:space:]]*"\([^"]*\)".*/\1/p' "$idx" | head -1)"
        idx_commit="$(sed -n 's/.*"commit":[[:space:]]*"\([^"]*\)".*/\1/p' "$idx" | head -1)"
        if [ "$idx_version" != "$version" ]; then
            mismatches="$mismatches index.json version $idx_version != workspace $version"
        fi
        if [ "$idx_commit" != "$commit" ]; then
            mismatches="$mismatches index.json commit $idx_commit != tree $commit"
        fi
    else
        mismatches="$mismatches index.json is missing"
    fi

    if [ -n "$mismatches" ]; then
        printf 'MISMATCH%s\n' "$mismatches"
        return 1
    fi
    printf 'ok:%s\n' "$notes"
    return 0
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    if [ ! -d "$(cur_root)/docs" ]; then
        ci_skip docs-site "no docs/ tree — the lane turns on with BW-19"
        return 0
    fi
    if ! command -v pnpm >/dev/null 2>&1; then
        ci_skip docs-site "no pnpm on this runner — the docs tree installs with pnpm (BW-19-01-04)"
        return 0
    fi

    ci_step "docs release artifacts"

    local build_out rc
    build_out="$(build_site)"
    rc=$?
    if [ "$rc" -eq 2 ]; then
        ci_skip docs-site "$(tsv_escape "$build_out") (BW-19-01-04)"
        return 0
    fi
    if [ "$rc" -ne 0 ]; then
        ci_fail docs-site "$(tsv_escape "$build_out")"
        return 1
    fi
    ci_pass docs-site "site built with dist/index.html"

    local version commit
    version="$(ws_version)"
    commit="$(tree_commit)"
    write_index "$version" "$commit"
    ci_pass docs-index "index.json stamped: version=$version commit=$commit"

    local tarball
    if ! tarball="$(package_site "$version" "$commit")"; then
        ci_fail docs-package "archiving the built site failed"
        return 1
    fi
    ci_pass docs-package "$(basename "$tarball") ($(du -h "$tarball" | cut -f1))"

    local verdict
    verdict="$(check_consistency "$version" "$commit")"
    if [ $? -eq 0 ]; then
        ci_pass docs-consistency "version metadata agrees across sources$(printf '%s' "$verdict" | sed 's/^ok//')"
    else
        ci_fail docs-consistency "$(tsv_escape "$verdict")"
        return 1
    fi
}

self_test() {
    local fails=0 r verdict tarball
    ci_step "self-test: agreeing sources pass and the archive round-trips"
    r="$(mktemp -d "$REPO_ROOT/ci-out/docs-selftest.XXXXXX")"
    mkdir -p "$r/docs/.vitepress/dist/deep"
    printf '<html>site</html>\n' >"$r/docs/.vitepress/dist/index.html"
    printf '<html>page</html>\n' >"$r/docs/.vitepress/dist/deep/page.html"
    printf '[workspace.package]\nversion = "0.1.0"\n' >"$r/Cargo.toml"
    printf '{\n  "name": "boundless-docs",\n  "version": "0.1.0"\n}\n' >"$r/docs/package.json"
    printf 'const siteVersion = "0.1.0";\n' >"$r/docs/.vitepress/config.mjs"

    CI_DOCSPKG_ROOT="$r" CI_DOCSPKG_OUT="$r/out" write_index 0.1.0 cafe123
    CI_DOCSPKG_ROOT="$r" CI_DOCSPKG_OUT="$r/out" package_site 0.1.0 cafe123 >/dev/null
    tarball="$r/out/docs/boundless-docs-0.1.0-cafe123.tar.gz"
    verdict="$(CI_DOCSPKG_ROOT="$r" CI_DOCSPKG_OUT="$r/out" check_consistency 0.1.0 cafe123)"
    if [ $? -eq 0 ] && [ -f "$tarball" ] \
        && tar -tzf "$tarball" | grep -q 'dist/index.html'; then
        ci_pass selftest-docspkg "agreeing sources pass and the archive carries the site"
    else
        ci_fail selftest-docspkg "an agreeing tree did not pass or the archive is wrong: $verdict"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a docs package version that disagrees fails, named"
    printf '{\n  "name": "boundless-docs",\n  "version": "9.9.9"\n}\n' >"$r/docs/package.json"
    printf 'const siteVersion = "9.9.9";\n' >"$r/docs/.vitepress/config.mjs"
    verdict="$(CI_DOCSPKG_ROOT="$r" CI_DOCSPKG_OUT="$r/out" check_consistency 0.1.0 cafe123)"
    if [ $? -ne 0 ] \
        && printf '%s' "$verdict" | grep -q 'docs/package.json version 9.9.9' \
        && printf '%s' "$verdict" | grep -q 'siteVersion 9.9.9'; then
        ci_pass selftest-docspkg-red "both disagreeing sources are named"
    else
        ci_fail selftest-docspkg-red "a mismatching docs version did not fail: $verdict"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a stale index stamp fails against the live commit"
    printf '{\n  "name": "boundless-docs",\n  "version": "0.1.0"\n}\n' >"$r/docs/package.json"
    printf 'const siteVersion = "0.1.0";\n' >"$r/docs/.vitepress/config.mjs"
    CI_DOCSPKG_ROOT="$r" CI_DOCSPKG_OUT="$r/out" write_index 0.1.0 deadbee
    verdict="$(CI_DOCSPKG_ROOT="$r" CI_DOCSPKG_OUT="$r/out" check_consistency 0.1.0 cafe123)"
    if [ $? -ne 0 ] && printf '%s' "$verdict" | grep -q 'index.json commit deadbee != tree cafe123'; then
        ci_pass selftest-docspkg-stale "a stale stamp is caught and named"
    else
        ci_fail selftest-docspkg-stale "a stale index stamp was not caught: $verdict"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$REPO_ROOT/ci-out/docs-selftest".*

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
