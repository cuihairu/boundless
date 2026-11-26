# OPT-08-01-03 — The space-parallelism evaluation

The acceptance reports **the cross-boundary sequence and its cost**
(`报告跨边界时序及耗时`). The harness drives the live migration faces
on a two-cell space (A owns x ∈ [0,100), B owns [100,200)) and
timestamps every stage; the parallelism envelope then measures whether
per-migration cost holds as independent worlds migrate concurrently.

## The cross-boundary sequence (时序)

One real walks from mid-A at 5 units per tick:

| Tick | x | Face |
| --- | --- | --- |
| 1–4 | 55→70 | `OffloadChecker::run` plans nothing — A is home |
| 5 | 75 | **The interest ghost**: B's rect is 25 units away, inside the 30-unit `ghost_distance`; the `CreateGhost` rides B's channel |
| 6–9 | 80→95 | The ghost is held; no plan yet |
| 10 | 100 | **The crossing**: the checker plans the offload to B, exactly on the boundary tick |

The plan tick runs the migration in three stages, in order:

1. **The offload half** (`convert_real_to_ghost`): the real streams its
   haunt list `[B, A]` and dies on A; the ghost record A keeps points
   at B. **Zero messages ride B's channel** — the body travels on the
   payload, and the destination is skipped in the sign-off.
2. **The onload half** (`receive_onload`): B's interest ghost becomes
   the one real (`are_we_haunted` holds — B ghosted the entity first),
   and the haunt pass re-points A. **One message to A** (`ghostSetReal`);
   the spare app stays silent.
3. **The settle** (`receive_ghost_set_real`): A relocates its ghost —
   exactly one real remains, at B.

## The cost (耗时)

| Face | Cost |
| --- | --- |
| Checker per tick (avg / max) | 2 835 / 7 017 ns |
| Offload stage | 10 590 ns |
| Onload stage | 19 446 ns — the most expensive stage |
| Settle stage | 606 ns |
| Migration total (walk + stages) | 93 404 ns |

The cross-space teleport costs less than the same-space migration's
full walk because it skips the checker approach: the send face
(`teleport_entity_to_space`, which deletes every existing ghost first —
the stale `delGhost` to the old haunt rides before anything else) is
12 959 ns, the destination load (`receive_onload_teleported_entity`,
which rewrites the streamed space id to its own) is 13 034 ns —
25 993 ns end to end.

## The parallelism envelope (候选收益)

P independent worlds each migrating one real, back to back:

| P | Total | Per migration |
| --- | --- | --- |
| 1 | 74 387 ns | 74 387 ns |
| 2 | 128 623 ns | 64 312 ns |
| 4 | 274 297 ns | 68 574 ns |
| 8 | 541 516 ns | 67 690 ns |

**Verdict: the migration faces are data-parallel.** The per-migration
cost stays flat (±10%) as the world count grows — nothing in the
offload/onload/settle sequence shares state across worlds, so
parallelizing spaces divides the wall time by the worker count without
changing any stage. The one cross-entity coupling is the checker's
shared ghost-deletion budget: one checker visiting 8 reals costs
8 956 ns against 10 312 ns for 8 single-real runs — same order, and the
budget throttles deletions, not plans. The candidate gain stands: space
parallelism is bounded by scheduling, not by the migration faces.

## Harness

`crates/server-runtime/tests/space_parallel_eval.rs` — the migration
sequence, the teleport, the payload-shape pins, and the envelope.

```sh
SPACE_PARALLEL_OUT=<abs path> cargo test -p server-runtime \
    --test space_parallel_eval -- --nocapture
```

## Fixtures

`docs/review/fixtures/space-parallel/space-parallel.tsv` — 30 rows,
captured with the command above.
