# OPT-14-01-01 — Record the release package size and the first run

Acceptance: **三端有基线** (all three delivery platforms have a baseline).

`ci/package-baseline.sh` measures the artifact that the **shipped packaging
lane** actually produces and persists it in
`tests/baseline/package-baseline.tsv`. It never creates a tag, a release or a
versioned artifact: the catalog is a measurement record, not a publish step.

## What is measured

The lane invokes `ci/package.sh` in a private `CI_OUT`/`CI_PACKAGE_OUT` root
(honouring its `CI_PACKAGE_BIN`/`CI_PACKAGE_ROLES` seams) and then reads the
tarball that the lane produced:

- **Exact bytes** with `wc -c` — not the rounded `du -h` figure that
  `package.sh` prints in its own manifest row.
- **SHA-256** via `sha256sum` (`shasum -a 256` on macOS), the same digest
  primitive `ci/checksums.sh` uses.
- **Member set** via `tar -tzf`, with directory entries excluded: the payload
  contract is one regular file, `bin/boundless-server`, and no `data/`.
- **First run** of the *installed* binary, launched under a real role from
  `KNOWN_ROLES` and timed with `date +%s%N` around the launch. The binary is
  given `BW_RES_PATH` and `CI_RUNNERS_TIMEOUT=1`, the shipped self-shutdown
  seam `ci/install-layout.sh` uses, so the launch completes with exit 0 plus
  both the startup and shutdown lines instead of waiting on a signal.

The elapsed time and the byte size are printed (`BASELINE` lines) and
catalogued as probe data; they move with the host and the build. The contracts
asserted by the lane are structural: the archive member set, the clean exit,
the startup message and the catalog grammar.

## Three-platform catalog

| platform | bytes | first_run_ns | status |
| --- | --- | --- | --- |
| `linux-x86_64` | 26,616,105 | 1,017,459,151 | `measured` |
| `macos-arm64` | — | — | `pending` (BW-15-04) |
| `windows-x86_64` | — | — | `pending` (BW-15-03) |

Each of the three rows names its source honestly. Only Linux compiles today:
the package-daily matrix marks the macOS and Windows legs `report_only`
because `crates/server-runtime/src/signals.rs` uses Linux-only signal APIs
(BW-15-03/04 own those ports). Those two rows therefore carry **no invented
numbers** — the byte, hash and time fields are `-`, and the blocker is spelled
out in the note field. When a leg that can build runs `record`, its own row is
replaced with a measured one; the other two rows are preserved.

`verify` checks that all three platforms are present, that the header is
exact, that measured rows have positive numeric bytes, a 64-character
lowercase SHA-256 and a numeric elapsed time, and that pending rows carry no
measurements and do name a BW-15 blocker. It is wired into the harness
self-test job and runs on every package-daily leg; only the explicit `record`
mode can rewrite the catalog, so CI never mutates the checkout.

## Modes and negative tests

- `self-test` plants a stub binary and proves that a green package produces a
  measured row with a real byte count, that a non-zero first run fails the
  measurement, that a two-platform catalog is rejected, and that a pending row
  with a fabricated size is rejected.
- `record` drives the real shipped lane and rewrites this platform's row.
- default/`verify` measures the current host (or just checks the catalog)
  without writing to it.

`ci/changes.sh` routes the lane, the catalog and `ci/package.sh` to the
`package` CI job. Nothing in this change creates a GitHub release or a version
tag.

## Finding: the tarball is not byte-reproducible across packaging runs

Two consecutive real `record`/measure runs of the same tree produced
26,616,105 and 26,616,045 bytes with different SHA-256 values. The shipped
`archive_prefix` calls `tar -czf` directly, so the gzip header carries the
current wall-clock time and the staged binary carries a fresh `cp` mtime. The
catalog therefore records a **sample** from one build, not a stable identity
for the tree; it must not be used as a cache key or a content assertion.
Making the archive deterministic (e.g. `gzip -n` and fixed mtimes) belongs to
the package-layout optimisation step (OPT-14-01-02), not to the baseline lane.

## Gates

- `ci/package-baseline.sh self-test`: 5/5 negative and positive probes green.
- `ci/package-baseline.sh verify`: 3 platforms, grammar clean.
- `ci/package-baseline.sh` (default, real build): package lane PASS, measured
  26,616,045 bytes, catalog PASS.
- `ci/package-baseline.sh record` (real build): 26,616,105 bytes,
  SHA-256 `1c51bfde7d04e334c3089c67f1073b975a40b1d9a413e5240ae810ad6dec6dc4`,
  first run 1,017,459,151 ns, catalog rewritten.
