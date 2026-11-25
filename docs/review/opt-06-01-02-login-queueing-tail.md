# OPT-06-01-02 — Login queueing and session continuation tail latency

Acceptance: **compare tail latency in a login flood scenario**.

`crates/server-runtime/src/login_tuning.rs` runs the OPT-06-01-01 seeded
flood twice through the real login faces — the shipped queueing shape and a
tuned shape — and records both ledgers whole. No wall-clock value is
sampled: latency is the whole-tick delay the faces produce, so the
comparison reproduces from the seed.

## The tuning seam

`LoginTuning` (in `login_baseline.rs`) is the seam; `Default` reproduces the
shipped flood bit for bit (`the_default_tuning_reproduces_the_shipped_ledger`).
Knobs:

- `rate_limit` / `rate_limit_seconds` — the limiter capacity and window the
  post-version gate already reads from `LoginGateConfig`.
- `cache_rejected_attempts` — whether a gate-rejected attempt still occupies
  the cached-request record. The shipped shape records it; the tuned shape
  records only attempts that passed the gates.

## Where the shipped tail comes from

A gate-rejected attempt leaves a params-only record in the cache. The record
reads as in-flight (`is_pending_authentication`), so a later arrival at that
address is ignored (`handle_resent_pending_attempt` → `IgnoreInProgress`)
and answers **two ticks late even though no login ever proceeded** from the
phantom. The flood's p95/p99 latency is exactly this phantom-ignored shape;
the session switch itself adds no tail — it completes within its arrival's
tick under both shapes.

## Recorded comparison (seed 314_159, 96 arrivals)

| Ledger field | Shipped (16 / 2.0 s, keep rejected) | Tuned (16 / 1.0 s, no phantom) |
|---|---:|---:|
| rejected | 32 (24 rate, 7 ban, 1 spoof) | 8 (0 rate, 7 ban, 1 spoof) |
| rate rejections | 24 | **0** |
| accepted sessions | 64 | **88** |
| latency p95 | 2 ticks | **1 tick** |
| latency peak | 2 ticks | **1 tick** |
| arrivals ignored/replayed | 7 | **0** |
| reattempts | 79 | 72 |
| cached-record peak | 17 | 16 |
| downloads | 64 streams, 10 288 bytes | 88 streams, 13 756 bytes |

The tuned window (16 per second against a 12-per-second flood) stops the
spurious rate rejections, and without phantom records no arrival is ignored:
the tail drops from 2 ticks to 1 and the flood accepts 88 of 96 arrivals.
Re-send protection is unchanged — an attempt genuinely waiting on the DBApp
still ignores a duplicate; the flood simply no longer manufactures phantom
in-flight records for attempts that were rejected before login proceeded.

## Reproducibility

`the_comparison_reproduces_from_its_seed` asserts both ledgers reproduce bit
for bit; `both_shapes_complete_every_accepted_switch` asserts every accepted
arrival completed the real client state machine and `log_on_complete` switch
in both runs.

## Gates

- `cargo test -p server-runtime --lib`: 1714 tests green (16 ignored).
- `cargo fmt -p server-runtime --check`: clean.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: no new
  lint (only the pre-existing unused-workspace-dependency warning for
  `blowfish`).
- llvm-cov 0.9.1, filter `-- login_baseline login_tuning`:
  `login_baseline.rs` **99.33%** lines (the post-gate `DropSilently` dead
  arm and two closing braces, see OPT-06-01-01), `login_tuning.rs`
  **100%**.
