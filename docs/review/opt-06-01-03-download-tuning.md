# OPT-06-01-03 — Download transfer duration and memory

Acceptance: **compare transfer duration and memory**.

`crates/server-runtime/src/download_tuning.rs` streams one deterministic
resource corpus (64 payloads, 32–287 bytes, LCG-sized) through the real
`ServerDownloads` → `DataDownloads` faces twice — the shipped per-tick
bundle budget and a tuned one — and records both ledgers whole. No
wall-clock value is sampled: duration is the number of bundle ticks until
the last stream completes, and memory is held bytes, not an allocator
counter (allocation counts across the script bridge are always zero in this
port, so held-bytes is the reproducible face).

## The tuning seam

`DownloadTuning.bundle_budget` — the bytes `ServerDownloads::add_to_bundle`
may put on one bundle tick. `Default` is 64, the shipped drain shape
OPT-06-01-01 used; the tuned drain is four times that.

## Recorded comparison (seed 314_159)

| Ledger field | Shipped (64 B/tick) | Tuned (256 B/tick) |
|---|---:|---:|
| transfer duration | 186 ticks | **45 ticks** (4.1×) |
| bytes transferred | 10 182 | 10 182 (identical, byte-checked) |
| server queue peak | 64 | 64 (corpus queued up front in both) |
| bundle peak (per-tick wire buffer) | 68 | **252** |
| client in-flight peak | 1 | 1 |
| client partial-bytes peak | 272 | 252 |

The trade the acceptance asks for is the duration against the per-tick
bundle: the tuned drain buys 4.1× shorter transfer with a bundle peak of
252 bytes instead of 68. The client-side peaks are recorded but not
direction-asserted — the shipped drain actually held slightly more
uncompleted bytes (272 vs 252), because its slow dribble leaves streams
half-assembled for more ticks while the tuned drain completes a stream in
the same tick its bytes arrive; which side holds more depends on the corpus
shape, so the ledgers carry both numbers and the test only asserts the
budget-driven faces.

## Reproducibility

`the_comparison_reproduces_from_its_seed` asserts the corpus and both
ledgers reproduce bit for bit; `the_default_budget_reproduces_the_shipped_ledger`
pins `Default` to the shipped budget. `run_download_transfer` panics unless
every stream reassembles byte-identical and every sent byte completes.

## Gates

- `cargo test -p server-runtime --lib`: 1719 tests green (16 ignored).
- `cargo fmt -p server-runtime --check`: clean.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: no new
  lint (only the pre-existing unused-workspace-dependency warning for
  `blowfish`).
- llvm-cov 0.9.1, filter `-- download_tuning`: **100%** lines.
