# OPT-05-01-03 — Re-run migration, shutdown and mixed deployment

Acceptance: **entity and process state are correct**.

`crates/server-runtime/src/lifecycle_scenario.rs` composes the real
migration-recovery, controlled-shutdown and replay-writing faces into one
seeded scenario and snapshots the decisions each makes. A rerun from the
same seed produces the same entity reconciliation, the same process-stage
ledger and the same replay bytes. No wall-clock value is sampled.

## The scenario

`run_lifecycle_scenario(seed)` (seed `41`):

1. **Migration** — the entity's Real starts on cell A with a Ghost on B;
   the Real offloads to B (`offload` outcome), then B dies.
2. **Migration recovery** — two calls arrive while the Real is lost: both
   are `Failed` to the sender and buffered, then `restore` onto C re-issues
   each exactly once (`Forwarded`). `MigrationRecovery::reconcile` checks
   the role state and the message state agree.
3. **Shutdown** — the controlled shutdown starts at the most senior process
   still up (`controlled_shut_down_server`) and the local app walks
   `Request → Inform → Perform`, recording the deadline at Inform. The
   CellAppMgr's Inform phase waits for a cell ack before acking the
   BaseAppMgr.
4. **Task drain** — two completion tasks queue their main-thread phase;
   `stop_all` runs both, in enqueue order, before the process ends.
5. **Replay** — the entity's state is recorded through the real
   `ReplayDataCollector` and written through the real
   `ReplayDataFileWriter`, then finalised. A genuine `ReplayHeader` prefix
   (built with `write_replay_prefix`) so the tail re-reads by the engine's
   own reader.
6. **Mixed version** — the current node accepts its own replay prefix and
   rejects a copy stamped with a different protocol version at the
   `read_replay_prefix` boundary.

## Recorded ledger

| Field | Value |
|---|---|
| offload | `Switched` |
| restore | `Restored(Address(43))` |
| call states | `Failed(50) → Failed(50) → Forwarded(43)` |
| reconciliation | all five predicates true (`real_cell=43`, `all_points_locate_it`, `real_channel_known`, `no_call_outstanding`, `one_answer_per_call`) |
| senior | `DbAppAlpha((2130706433, 4000))` |
| stages | `Request → Inform → Perform` |
| shut_down_time | 100 |
| drained | `["backup", "flush-replay"]` |
| replay | signed prefix + 4-tick chunk, deterministic from the seed |
| mixed_version | `(true, true)` — current accepted, foreign rejected |

## Reproducibility

`the_scenario_reproduces_from_its_seed` runs the scenario twice and asserts
the entire `LifecycleLedger` (entity, process and replay fields) is equal.
`the_mixed_version_gate_accepts_current_and_rejects_foreign` runs the gate
on the same seed.

## Scope note

There is no mixed-version *fleet* runner in this build; the only
runtime-observable mixed-fleet face is the replay prefix's protocol-version
check, which is what this scenario exercises. A full mixed-version cluster
test is a follow-up, not a gap here.

## Gates

- `cargo test -p server-runtime --lib lifecycle_scenario`: 6 tests green.
- `cargo fmt -p server-runtime --check`: clean.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: no new
  lint (only the pre-existing unused-workspace-dependency warning for
  `blowfish`).
- llvm-cov 0.9.1: `lifecycle_scenario.rs` **187/187 = 100%** lines.
