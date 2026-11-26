# OPT-05-01-01 — Record the load hotspot and fault baseline

Acceptance: **record the allocation, recovery and reclamation windows**.
`crates/load/src/hotspot.rs` composes the existing load faces over one
seeded run of the real `Bot` connection state machine, and the driver
saves the machine-readable numbers the way every other load scenario
does. This row records a baseline; it changes no dispatch or scheduling
policy.

## The three windows

- **Allocation** — the fleet is brought online and circle-walks the
  whole load window (the `steady` movement, the `long_stability` soak).
  The real `ResourceTracker` reads procfs (utime/stime from
  `/proc/self/stat`, VmRSS from `/proc/self/status`) eight times across
  the run; the peak resident size is the allocation answer.
- **Recovery** — a seeded subset is dropped once mid-window and the
  reconnect behaviour re-issues `logOn` (the `reconnect` face's recovery
  tracks). The mean and max recovery lengths are the recovery answer.
  The drop/relogin/reply floor is two mini-ticks, the same number the
  reconnect scenario pins: the drop lands at tick *d*, the re-login is
  issued on that tick's update, the reply lands on tick *d+2*.
- **Reclamation** — the drain window releases every session with the
  personality's destroy decision (`feed_logged_off(None)`), which clears
  the entity and movement state; the online curve walks to zero while
  the sampling continues onto the tail. The final resident size, and
  the peak-minus-final difference, are the reclamation answer.

## Reproducible face vs wall-clock face

The ledger's `PartialEq` follows the long-stability contract: the online
curve, the drop schedule, the recovery tracks and the sample schedule
(sampling period and sample count) reproduce bit for bit from a fixed
seed and are asserted; the resource numbers are wall-clock by nature and
are excluded from equality. They travel in the saved
`ScenarioReport` via `resource_report_entries`, with the metric
definitions and sampling period alongside, per the BW-17-03-04 report
contract. The stdout `metric<TAB>value` lines carry only the
reproducible face; `LOAD_REPORT` writes the full report.

## Recorded run

Saved fixture:
`docs/review/fixtures/load/hotspot-baseline.report.json` with the metric
lines in `hotspot-baseline.metrics.tsv`. Driver invocation (PR envelope,
150 ticks in 15 s wall, four clients):

```sh
LOAD_REPORT=docs/review/fixtures/load/hotspot-baseline.report.json \
LOAD_TARGET=linux-offline-harness LOAD_DURATION=30 LOAD_CLIENTS=4 \
bash tests/load/scenarios/hotspot-baseline.sh
```

| Face | Value |
|---|---:|
| load window / drain window | 300 / 75 ticks |
| bots / recoveries | 4 / 4 |
| mean / max recovery length | 2 / 2 ticks |
| load-window minimum online share | 0.75 |
| sampling period / sample count | 4.6 s / 8 |
| peak RSS | 3,464 KiB |
| final RSS | 3,464 KiB |
| peak − final | 0 KiB |
| CPU series | 0.07 user / 0.05 system seconds (flat across 8 samples) |

Reading: the fleet holds at four bots except the fault dips (0.75 share),
and every dropped bot is back at the two-tick floor. The resident size
does **not** fall during the drain in this run: the allocator keeps the
small fleet's pages, so the honest reclamation number is zero. That is
why these numbers are **recorded, never asserted** as a band — a
reclamation target would be machine- and allocator-dependent. The
envelope-independent mean-recovery floor (`min 2.0`) is the one
threshold added to `tests/load/baselines.tsv`, mirroring the reconnect
scenario's band.

Two passes of the fixed seed produce the same stdout metrics and the
same online/recovery schedule; the smoke gate runs that double check on
every execution. The saved XML and shell wrapper
(`tests/load/scenarios/hotspot-baseline.*`) ride the same
`ci/load.sh` envelope as the six earlier scenarios: 7/7 scenarios ran
and 7/7 were inside their recorded bands.

## Gates

- `cargo test -p load`: 176 lib tests + 1 integration, green (7 new
  hotspot unit tests + 1 smoke test; the saved-kind list now includes
  `hotspot-baseline`).
- llvm-cov 0.9.1: `hotspot.rs` **269/271 = 99.26%** lines (the two
  unhit lines are the empty-series closure branch and a defensive
  branch).
- `cargo fmt -p load -p bots --check`: clean.
- `cargo clippy -p bots --all-targets -- -D warnings`: clean.
  `cargo clippy -p load --all-targets -- -D warnings` shows exactly the
  four pre-existing debts carried on pristine `HEAD` (`long_stability`
  literal-bool assert and three `expect(&format!(..))` test helpers);
  this delivery adds no new lint.
- `bash ci/load.sh`: load-scenarios / load-driver / load-run /
  load-thresholds all PASS (7/7). The unrelated `rust-lint`
  (resources) and `rust-test` (test-support) reds are standing
  cross-seat debts, untouched here.
