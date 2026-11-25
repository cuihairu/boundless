# OPT-05-01-02 — Tune load reporting and split/merge

Acceptance: **compare the scheduling effect of a fixed scenario**.

`crates/server-runtime/src/balance_baseline.rs` runs the real `CellAppMgr`
twice over one seeded, two-phase load sequence and records every scheduling
decision each run makes. No wall-clock number is sampled: the comparison is
over the ledger the manager produces, so it is reproducible from the seed.

## The tuning seam

The reference implementation hard-codes the scheduling constants.
`BalanceTuning` is the seam that varies them while `Default` reproduces the
shipped values exactly:

- `load_smoothing_bias` (`CellAppRecord::inform_of_load_with`, the EMA weight
  on each report),
- `estimated_initial_cell_load`,
- `safety_bound` and `safety_ratio` (`Space::load_balance`),
- `meta_load_balance_tolerance`, `lower_bound` (`meta_load_balance`),
- `limit_to_chunks` (`BalanceContext`, chunk-limited partition movement).

The existing callers keep using `inform_of_load`, so their behaviour is
unchanged; the new `_with` form is used only by the comparison harness.

## The fixed scenario

Three CellApps, one space with three cells, a 10,000 x 10,000 bounded space
on a 100-unit grid. A seeded LCG supplies jitter. The first half of the
96 ticks reports a hot, skewed load per app; the second half reports a cool
load so the split/merge and retire passes both run on a deterministic
schedule. Each tick runs `inform_of_load_with`, `load_balance`,
`meta_load_balance` and `tick` in that order.

`ScheduleLedger` counts cell additions, cell removals, retired apps,
partition lines that moved off their initial position, and load reports.

## Recorded comparison

Seed `27_182_828`, 96 ticks, two runs from the same initial manager state:

| Ledger field | Baseline (reference defaults) | Candidate (`limit_to_chunks: false`) |
|---|---:|---:|
| reports | 96 | 96 |
| cells added | 0 | 0 |
| cells removed | 0 | 0 |
| apps retired | 0 | 0 |
| **partitions moved** | **0** | **2** |

Final smoothed loads and cell counts are identical
(`0.066659 / 0.038615 / 0.028998`, three cells in space 1): the tuned knob
changes *how the scheduler reaches the same distribution*, which is the
decision-level effect the acceptance asks to compare. The reference keeps
both partition lines pinned because the loaded chunks do not cover the
candidate move; disabling the chunk limit lets the entity bound alone drive
two partition moves.

`the_comparison_is_stable_across_seeds` runs the same comparison twice and
asserts both sides reproduce bit for bit; `the_fixed_scenario_reproduces_from_its_seed`
does the same for a single run.

## Gates

- `cargo test -p server-runtime`: 1693 lib tests green (5 new
  `balance_baseline` tests), 16 ignored; all integration suites green.
- `cargo fmt -p server-runtime --check`: clean.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: no new
  lint (only the pre-existing unused-workspace-dependency warning for
  `blowfish`).
- llvm-cov 0.9.1: `balance_baseline.rs` **171/171 = 100%** lines.
