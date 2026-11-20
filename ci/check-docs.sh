#!/usr/bin/env bash
# BW-22-01-05 — documentation lane: link integrity, then the site build.
#
# Usage:
#   ci/check-docs.sh              # gate the docs tree
#   ci/check-docs.sh self-test    # prove the link checker bites
#
# docs/ does not exist yet: it lands with BW-19 (VitePress, root package.json).
# The lane is written against that shape now so a markdown-only change gates
# the moment the site appears.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
DOCS_DIR="docs"
PY="${PYTHON:-python3}"

main() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    cd "$REPO_ROOT"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    if [ "$MODE" = self-test ]; then
        self_test
        return $?
    fi

    if [ ! -d "$DOCS_DIR" ]; then
        ci_skip docs-links "no $DOCS_DIR/ — lane turns on with BW-19"
        ci_skip docs-build "no $DOCS_DIR/ — lane turns on with BW-19"
        return 0
    fi

    local count
    count="$(git ls-files --cached "$DOCS_DIR" '*.md' | wc -l)"
    ci_note "$count tracked markdown page(s) under $DOCS_DIR"

    ci_step "internal links"
    ci_pipe docs-links "$PY" ci/check-docs-links.py "$DOCS_DIR"
    if [ "${CI_RC:-1}" -eq 0 ]; then
        ci_pass docs-links "ok"
    else
        local log dead
        log="$CI_LOG_DIR/docs-links.log"
        # The manifest shows only this detail, so it must name the page and the
        # target it broke; the log already carries that as `DEAD src -> target`.
        dead="$(grep -m1 '^DEAD' "$log" 2>/dev/null | sed 's/^DEAD //; s/[[:space:]]*$//' | cut -c1-120)"
        if [ -n "$dead" ]; then
            ci_fail docs-links "dead link: $dead"
        else
            ci_fail docs-links "$(tail -1 "$log" | cut -c1-160)"
        fi
    fi

    # BW-19 landed the site as docs/package.json with pnpm managing deps
    # (vitepress 1.6.4), so the build leg now gates on the real entry rather
    # than the root package.json it was written against before the tree
    # existed. pnpm 12 aborts the install on ignored postinstall scripts
    # (esbuild) even when the tools are in place — the same behaviour the
    # SDK lane tolerates — so the gate is the build itself, not the installer.
    if [ -f docs/package.json ] && grep -q '"build"' docs/package.json; then
        if ! command -v pnpm >/dev/null 2>&1; then
            ci_skip docs-build "no pnpm on this runner; the docs tree installs with pnpm (BW-19-01-04)"
        else
            ci_step "site build"
            pnpm --dir docs install --frozen-lockfile --prefer-offline >/dev/null 2>&1 || true
            ci_pipe docs-build pnpm --dir docs run build
            if [ "${CI_RC:-1}" -eq 0 ]; then
                ci_pass docs-build "ok"
            else
                ci_fail docs-build "vitepress build failed (log logs/docs-build.log)"
            fi
        fi
    else
        ci_skip docs-build "no docs/package.json build script yet (BW-19-01-04)"
    fi
}

# Prove the link checker bites: a docs tree with one live and one dead
# internal link must report the dead one, and the same tree repaired must
# come back clean.
self_test() {
    local sandbox docs fails=0
    sandbox="$(mktemp -d)"
    docs="$sandbox/docs"
    mkdir -p "$docs/zh"

    cat >"$docs/index.md" <<'EOF'
# Site

- [guide](guide.md)
- [missing](no-such-page.md)
- [locale tree](/zh/index.md)
- [external](https://example.com/)
EOF
    printf '# Guide\n\ntext\n' >"$docs/guide.md"
    printf '# 中文\n\ntext\n' >"$docs/zh/index.md"

    local report="$sandbox/links.log"
    ci_step "self-test: a dead internal link is caught"
    if "$PY" ci/check-docs-links.py "$docs" >"$report" 2>&1; then
        ci_fail selftest-docs "link checker accepted a page with a dead link"
        fails=$((fails + 1))
    else
        if grep -q 'no-such-page' "$report"; then
            ci_pass selftest-docs "$(tail -1 "$report")"
        else
            ci_fail selftest-docs "went red but did not name no-such-page.md"
            fails=$((fails + 1))
        fi
    fi

    ci_step "self-test: the same page repaired comes back clean"
    sed -i '/missing/d' "$docs/index.md"
    if "$PY" ci/check-docs-links.py "$docs" >"$sandbox/fixed.log" 2>&1; then
        ci_pass selftest-docs-clean "$(tail -1 "$sandbox/fixed.log")"
    else
        ci_fail selftest-docs-clean "a link-complete page still fails"
        fails=$((fails + 1))
    fi

    rm -rf "$sandbox"
    if [ "$fails" -gt 0 ]; then
        ci_note "$fails self-test(s) did not behave"
        return 1
    fi
    ci_note "all self-tests behaved as expected"
    return 0
}

main
LANE_RC=$?

set +e
ci_finalize
FINAL_RC=$?
set -e

[ "$LANE_RC" -ne 0 ] && exit "$LANE_RC"
exit "$FINAL_RC"
