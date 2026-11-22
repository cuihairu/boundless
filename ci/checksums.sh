#!/usr/bin/env bash
# BW-22-04-05 — the checksums lane: stamp every packaged artifact with a
# hash, record the release metadata, and prove a tampered artifact fails.
#
# Usage:
#   ci/checksums.sh            # write SHA256SUMS, then verify + metadata
#   ci/checksums.sh verify     # verify an existing SHA256SUMS against the files
#   ci/checksums.sh self-test  # prove tampering and loss are caught
#
# The acceptance is "a modified artifact is detected". The lane hashes every
# artifact the packaging lanes produced (server packages, docs site, engine
# SDKs, the vsix) into ci-out/checksums/SHA256SUMS with a header carrying
# the release metadata (version, commit, generated time); verify re-hashes
# every listed file — a byte changed or a file missing fails and names it.
# The metadata leg checks that artifacts stamped with the current commit
# carry the workspace version; artifacts from earlier commits are counted
# honestly as stale rather than silently mixed in.
#
# Nothing here tags or releases. The original packaged without any hash
# face at all; this lane exists so whatever is eventually delivered is
# checkable by the user who receives it.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
# Call-time resolution: the self-test plants artifacts and a private output
# root, and an override applied to a shell function only reaches code that
# re-reads the variable.
cur_root() { printf '%s' "${CI_SUMS_ROOT:-$REPO_ROOT}"; }
cur_out() { printf '%s' "${CI_SUMS_OUT:-$CI_OUT}"; }

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

ARTIFACT_GLOPS='package/*.tar.gz docs/*.tar.gz docs/index.json sdk/*.tar.gz vsix/*.vsix'

find_artifacts() {
    local out g f
    out="$(cur_out)"
    for g in $ARTIFACT_GLOPS; do
        for f in "$out"/$g; do
            [ -f "$f" ] && printf '%s\n' "$f"
        done
    done
}

sums_file() { printf '%s/checksums/SHA256SUMS' "$(cur_out)"; }

write_sums() {
    local f rel total=0
    mkdir -p "$(dirname "$(sums_file)")"
    {
        printf '# boundless release checksums\n'
        printf '# version: %s\n' "$(ci_version)"
        printf '# commit: %s\n' "$(ci_git_sha)"
        printf '# generated: %s\n' "$(ci_now)"
    } >"$(sums_file)"
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        rel="${f#"$(cur_out)"/}"
        printf '%s  %s\n' "$(sha256sum "$f" | cut -d' ' -f1)" "$rel" >>"$(sums_file)"
        total=$((total + 1))
    done < <(find_artifacts)
    printf '%s' "$total"
}

verify_sums() {
    # verify_sums — re-hash every listed artifact; the output names whatever
    # failed. Exit 0 only when every row checks out.
    local f base failed=0 row name old new
    base="$(cur_out)"
    while IFS= read -r row; do
        name="${row#*  }"
        old="$(printf '%s' "$row" | cut -d' ' -f1)"
        f="$base/$name"
        if [ ! -f "$f" ]; then
            printf 'MISSING %s\n' "$name"
            failed=$((failed + 1))
            continue
        fi
        new="$(sha256sum "$f" | cut -d' ' -f1)"
        if [ "$new" != "$old" ]; then
            printf 'CHANGED %s\n' "$name"
            failed=$((failed + 1))
        fi
    done < <(grep -v '^#' "$(sums_file)")
    [ "$failed" -eq 0 ]
}

verify_metadata() {
    # verify_metadata — the release metadata must be self-consistent: the
    # header's commit is the live commit, and every artifact stamped with
    # that commit carries the header's version. Stale artifacts are counted
    # and named, not silently mixed in.
    local sums base version commit f name stamp_version stamp_commit stale=0 bad=0
    sums="$(sums_file)"
    base="$(cur_out)"
    version="$(sed -n 's/^# version: //p' "$sums" | head -1)"
    commit="$(sed -n 's/^# commit: //p' "$sums" | head -1)"
    if [ "$commit" != "$(ci_git_sha)" ]; then
        printf 'header commit %s is not the live commit %s\n' "$commit" "$(ci_git_sha)"
        return 1
    fi
    while IFS= read -r name; do
        [ -n "$name" ] || continue
        f="$base/$name"
        [ -f "$f" ] || continue
        # Only stamped artifact names carry a version-commit stamp;
        # index.json and friends are metadata files, not stamped artifacts.
        case "$name" in
        *boundless-*-*-*) ;;
        *) continue ;;
        esac
        # boundless-<what>-<version>-<commit>.<ext>: the last two dash
        # groups of the stem are the stamp.
        stamp_commit="$(printf '%s' "$name" | sed 's/\.[^.]*$//' | awk -F- '{print $NF}')"
        stamp_version="$(printf '%s' "$name" | sed 's/\.[^.]*$//' | awk -F- '{print $(NF-1)}')"
        if [ "$stamp_commit" = "$commit" ]; then
            if [ "$stamp_version" != "$version" ]; then
                printf 'MISSTAMPED %s: version %s != header %s\n' "$name" "$stamp_version" "$version"
                bad=$((bad + 1))
            fi
        else
            stale=$((stale + 1))
        fi
    done < <(grep -v '^#' "$sums" | sed 's/^.*  //')
    [ "$stale" -gt 0 ] && printf 'stale: %s artifact(s) from earlier commits, verified but noted\n' "$stale"
    [ "$bad" -eq 0 ]
}

run_all() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    ci_step "release checksums"

    local total
    total="$(find_artifacts | grep -c . || true)"
    if [ "$total" -eq 0 ]; then
        ci_skip checksums-write "no packaged artifacts under ci-out/ yet — the packaging lanes produce them (BW-22-04-01..04)"
        return 0
    fi

    total="$(write_sums)"
    ci_pass checksums-write "$total artifact(s) hashed into checksums/SHA256SUMS"

    local vout
    if ! vout="$(verify_sums)"; then
        ci_fail checksums-verify "$(tsv_escape "$vout")"
        return 1
    fi
    ci_pass checksums-verify "every recorded hash matches its artifact"

    local mout rc
    mout="$(verify_metadata)"
    rc=$?
    [ -n "$mout" ] && ci_note "$(tsv_escape "$mout")"
    if [ "$rc" -ne 0 ]; then
        ci_fail checksums-metadata "$(tsv_escape "$mout")"
        return 1
    fi
    ci_pass checksums-metadata "artifact stamps agree with the release header"
}

verify_only() {
    local total vout
    if [ ! -f "$(sums_file)" ]; then
        ci_skip checksums-verify "no SHA256SUMS to verify — run ci/checksums.sh against the packaged tree first"
        return 0
    fi
    ci_step "release checksums (verify only)"
    total="$(grep -vc '^#' "$(sums_file)" || true)"
    if ! vout="$(verify_sums)"; then
        ci_fail checksums-verify "$(tsv_escape "$vout")"
        return 1
    fi
    ci_pass checksums-verify "every recorded hash matches its artifact ($total row(s))"
}

self_test() {
    local fails=0 r out vout rc
    ci_step "self-test: planted artifacts hash, verify and stamp cleanly"
    r="$(mktemp -d "$REPO_ROOT/ci-out/checksums-selftest.XXXXXX")"
    mkdir -p "$r/out/package" "$r/out/docs"
    printf 'server binary' >"$r/out/package/boundless-0.1.0-cafe123.tar.gz"
    printf '{"artifact":"boundless-docs"}' >"$r/out/docs/index.json"
    out="$(CI_SUMS_ROOT="$r" CI_SUMS_OUT="$r/out" "$0" 2>&1)"
    rc=$?
    if [ "$rc" -eq 0 ] \
        && printf '%s\n' "$out" | grep -q 'checksums-verify	PASS' \
        && [ -f "$r/out/checksums/SHA256SUMS" ]; then
        ci_pass selftest-sums "planted artifacts hash and verify (SHA256SUMS written)"
    else
        ci_fail selftest-sums "clean artifacts did not round-trip: $(printf '%s\n' "$out" | grep 'checksums-' | head -1)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: one changed byte fails verification, named"
    printf 'server binary!' >"$r/out/package/boundless-0.1.0-cafe123.tar.gz"
    vout="$(CI_SUMS_ROOT="$r" CI_SUMS_OUT="$r/out" verify_sums)"
    if [ $? -ne 0 ] && printf '%s\n' "$vout" | grep -q 'CHANGED package/boundless-0.1.0-cafe123.tar.gz'; then
        ci_pass selftest-sums-tamper "a tampered artifact is detected and named"
    else
        ci_fail selftest-sums-tamper "a tampered artifact was not detected: $vout"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a missing artifact fails verification, named"
    printf 'server binary' >"$r/out/package/boundless-0.1.0-cafe123.tar.gz"
    rm "$r/out/docs/index.json"
    vout="$(CI_SUMS_ROOT="$r" CI_SUMS_OUT="$r/out" verify_sums)"
    if [ $? -ne 0 ] && printf '%s\n' "$vout" | grep -q 'MISSING docs/index.json'; then
        ci_pass selftest-sums-missing "a missing artifact is detected and named"
    else
        ci_fail selftest-sums-missing "a missing artifact was not detected: $vout"
        fails=$((fails + 1))
    fi

    ci_step "self-test: a current-commit artifact with a wrong version fails metadata"
    printf '{"artifact":"boundless-docs"}' >"$r/out/docs/index.json"
    mkdir -p "$r/out/vsix"
    printf 'extension package' >"$r/out/vsix/boundless-vscode-9.9.9-$(ci_git_sha).vsix"
    CI_SUMS_ROOT="$r" CI_SUMS_OUT="$r/out" write_sums >/dev/null
    vout="$(CI_SUMS_ROOT="$r" CI_SUMS_OUT="$r/out" verify_metadata)"
    rc=$?
    if [ "$rc" -ne 0 ] && printf '%s\n' "$vout" | grep -q 'MISSTAMPED vsix/boundless-vscode-9.9.9'; then
        ci_pass selftest-sums-stamp "a mis-stamped version is caught against the header"
    else
        ci_fail selftest-sums-stamp "a wrong version stamp was not detected: $vout"
        fails=$((fails + 1))
    fi

    rm -rf "$r" "$REPO_ROOT/ci-out/checksums-selftest".*

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
    verify) verify_only; return $? ;;
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
