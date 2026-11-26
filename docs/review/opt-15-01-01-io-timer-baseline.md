# OPT-15-01-01 — 采集各平台 I/O 及定时精度 (collect per-platform I/O and timer precision)

Acceptance: **统一负载下产出报告** (one unified workload produces the report).

`crates/platform/tests/io_timer_baseline.rs` runs a single fixed workload
over the shipped `platform` faces the engine's event loop and storage sit
on, and emits a TSV report (probe, samples, min/median/p99/max ns, note)
to `PLATFORM_BASELINE_OUT`. Every host that runs the workload answers the
same questions with the same probe names, so per-platform numbers are
comparable by construction.

## The unified workload (13 probes)

- **Timer faces** — `platform::clock::monotonic_now_micros` read cost and
  its observable granularity (smallest nonzero adjacent-read delta);
  `Instant::now` read cost and pair-delta resolution (the floor every
  probe in this repo measures with); `thread::sleep` actual duration at
  1/2/5/10 ms — `actual >= requested` is asserted per leg (the tick-budget
  contract), the overshoot is probe data.
- **I/O faces** — 4 KiB `std::fs::write`/`read` per-op latency with
  byte-equal read-back asserted, and the shipped durability face
  `platform::file::atomic_replace` (write + fsync + rename) per-op cost,
  read-back asserted.
- **Event-loop faces** — the self-pipe `Wakeup` round trip (trigger → the
  blocked `epoll_wait` reports the pipe read end; event identity and
  drain-to-empty asserted) and poller timeout precision (`wait(1ms)` with
  no events returns zero events, asserted; actual wait recorded).

## The Linux x86_64 report (committed at `tests/baseline/platform-baseline.tsv`)

| probe | median | p99 | note |
| --- | --- | --- | --- |
| clock_read_latency | 272 ns | — | per call |
| clock_granularity | 1 µs | — | 38 distinct steps over 200k reads |
| instant_read_latency | 195 ns | — | per call |
| instant_resolution | 59 ns | — | min nonzero pair delta |
| sleep_1ms/2ms/5ms/10ms_actual | 1.081/2.088/5.084/10.084 ms | ≤3.8 ms | overshoot ≤0.09 ms median |
| file_write_4k / file_read_4k | 9.18 / 5.44 µs | 28.3 / 27.1 µs | byte-equal |
| atomic_replace_4k | 25.45 µs | 85.5 µs | write+fsync+rename |
| wakeup_roundtrip | 797 ns | 4.28 µs | trigger → blocked wait |
| poller_timeout_1ms | 1.081 ms | 3.79 ms | zero events asserted |

## Three-platform record

The committed catalog carries the Linux rows above plus one pending row
each for `macos-arm64` (BW-15-04: only clock/interface compile; the
file-system and windowing slices own these faces) and `windows-x86_64`
(BW-15-03: the signal layer still gates the workspace build). Pending rows
carry `-` in every measurement field and name their blocker — the grammar
check rejects measurements on a pending row, unknown probe names,
duplicate (platform, probe) pairs, and a measuring platform missing any of
the 13 probes. Running the workload on a compiling host regenerates that
platform's rows via `PLATFORM_BASELINE_OUT`; nothing is asserted against
another host's numbers.

## Gates

- `cargo test -p platform`: 64 lib + 1 + 1 passed (including
  `io_timer_baseline`), 0 failed.
- `cargo fmt -p platform -- --check`: clean.
- `cargo clippy -p platform --all-targets -- -D warnings`: no lint.
