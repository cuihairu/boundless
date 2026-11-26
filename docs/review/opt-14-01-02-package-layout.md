# OPT-14-01-02 — Optimize the package layout or first startup

验收: **报告体积和耗时差异** (report the size and time differences).

The package lane now ships a stripped binary with its debug info split into
a sidecar, and archives it deterministically. `ci/package-baseline.sh`
measures the result against the OPT-14-01-01 baseline on the same host.

## The layout change (`ci/package.sh`)

- `strip_debug_info` runs after staging: for an ELF binary it writes
  `<PKG>.debug` next to the installed binary (`objcopy --only-keep-debug`),
  strips the staged copy (`strip --strip-debug`), and links the sidecar back
  with `objcopy --add-gnu-debuglink` so gdb/lldb resolve it automatically. The
  workspace release profile keeps `debug = true`, so the sidecar is full
  debug info — the change moves the bytes out of the shipped path, it does not
  lose them.
- The sidecar is **not** in the archive: `archive_prefix` ships `bin/boundless-server`
  only. `bin/boundless-server.debug` stays in the staging prefix where
  symbolication runs.
- The archive is byte-deterministic on GNU tar: `--sort=name`, pinned
  `--mtime`/`--owner`/`--group`, `gzip -n` (no stream timestamp). BSD tar keeps
  its default metadata behaviour on the macOS legs.

The strip is a no-op when binutils are missing or the binary is not an ELF
(the stub self-tests, macOS/Windows legs), so the lane still packages
everything.

## The measured difference (Linux x86-64, version 0.1.0)

| | before (OPT-14-01-01) | after | delta |
| --- | --- | --- | --- |
| archive bytes | 26,616,105 | 478,760 | **−98.2 %** |
| stripped binary | 126 MiB (with debug) | 1,123,056 B | ~1.1 % of the debug build |
| debug sidecar | — | 130,193,992 B | stays in staging |
| SHA-256 | `1c51bfde…c6dc4` | `168567e5…d74feb` | new identity |

Two independent `record` runs (full release build each) produced identical
archive bytes **and** identical SHA-256 — the archive is now reproducible in
addition to being small. First-run wall-clock is unchanged within noise
(1.02 s vs 1.03 s, both dominated by `CI_RUNNERS_TIMEOUT=1` self-shutdown):
the optimization is a layout win, not a startup win, and the doc says so
rather than claiming a time difference the numbers do not show.

## Gates

- `ci/package.sh self-test`: 4/4 green, including the new determinism probe
  (two packaging runs → one SHA-256).
- `ci/package-baseline.sh self-test`: 5/5 green.
- `ci/package-baseline.sh record` (real build): 478,760 bytes, deterministic
  SHA-256, catalog verified.
- `ci/package-baseline.sh verify`: 3 platforms, grammar clean.

No version tag, GitHub release or release workflow was touched.
