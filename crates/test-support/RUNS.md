# Runs and failure reproduction (BW-18-04)

One entry drives local and CI. Everything on this page is that entry:
`cargo run -p test-support --bin run-tests -- ...`.

## Calls

| What | Command |
| --- | --- |
| Full suite | `run-tests -- all` |
| One module (a workspace crate) | `run-tests -- server-runtime` |
| One test module inside a crate | `run-tests -- test-support timer_scenarios` |
| Compat diff of two archives | `run-tests -- diff <baseline-run.txt> <current-run.txt>` |
| Coverage (cargo-llvm-cov 0.9.1) | `run-tests -- cov [module]` |

Every run writes an archive to `target/test-results/<label>/run.txt`; the
coverage entry writes `target/test-results/cov-<module>/lcov.info`.
`run-tests -- all` and a module run use the same tree and the same text
format, locally and in CI.

Exit codes: `0` success, `1` misuse/startup error, `2` incompatible diff.

## Dependencies

- `cargo` on `PATH` (stable toolchain from `rust-toolchain.toml`).
- `cargo-llvm-cov` 0.9.1 for `cov` (`cargo install cargo-llvm-cov --version 0.9.1`).
- `git` (the archive records the short commit sha).
- `date` for the ISO-8601 UTC stamp in the archive header.
- Multiprocess fixtures spawn `/bin/sh`, `/bin/sleep` and `/bin/true` and
  bind ephemeral `127.0.0.1` ports; process trees are killed via
  `kill(2)` on their own process group.

## Fixture origins

- `config_fixture` (`ConfigFixture`/`DatabaseFixture`): builds
  `server/bw.xml` + `server/bw_<user>.xml` chains and loads them through
  the production `init_from_sources` path in `server-runtime::config`
  (`BwConfig`), so fixtures run the real chain resolution, not a stub.
- `multiproc` (`Topology`/`ProcessGroup`): named roles, dependency
  topological start order (Kahn, insertion-order ties broken
  deterministically), per-role process groups, tree kill, crash/restart
  (`crash`/`restart`), readiness probes (`PortOpen`/`FileExists`/
  `LogContains`), `RunRoot` + `PortAllocator` from `allocation`.
- `clock` (`ManualClock`): nanosecond stamps starting at zero, injectable
  into `DispatcherTimers::with_clock`; `timer_scenarios` builds scripted
  repeat/once-off timer cases on it.
- `channel` (`ScriptedChannel`): per-send-index fates (`Drop`,
  `Delayed`, `After`), seeded `SeededRng` chance scripting standing in
  for the reference's probabilistic `MangleFilter`.
- `case_registry`: the original suite inventory
  (`cases/original_inventory.txt`, 908 cases) and the `PORTED` seed
  table mapping original names to their new homes.

## Failure reproduction

1. **From an archive.** Every archive names its own rerun:
   ```sh
   grep '^run:' target/test-results/<label>/run.txt
   # run: cargo test -p test-support timer_scenarios
   ```
   Paste that line (after `run: `) onto the same checkout recorded in
   `commit:` — the narrowed slice reruns as-is. `harness::rerun_command`
   extracts it programmatically.
2. **Compare runs.** A red CI run's artifact plus a green local run:
   ```sh
   run-tests -- diff target/test-results/all/run.txt <ci-run.txt>
   ```
   The report names every differing test (`REGRESSED`/`MISSING`/`ADDED`),
   so the failing case is pointed at, not hunted for. Exit 2 marks
   incompatibility.
3. **Intermittent failures.** Timer and channel behaviour runs on
   `ManualClock`/scripted fates, so a failing scenario replays
   bit-for-bit: rerun the recorded `run:` line; the same deliveries
   recur. Nothing waits on wall-clock time.
4. **Process-tree failures.** A leaked child outlives its role name;
   scan by the scenario's marker argument, e.g.
   `ps aux | grep '[b]w18-.*-19433x'` (the bracket hides grep itself).
   `ProcessGroup`'s `Drop` kills the whole group, so a panicking test
   cannot leak trees.
5. **Coverage.** `run-tests -- cov <module>` writes an lcov report whose
   `SF:`/`DA:` records map every measured line back to source; check a
   suspect line's hit count directly in `lcov.info`.
