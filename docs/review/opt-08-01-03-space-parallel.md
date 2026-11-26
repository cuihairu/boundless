# OPT-08-01-03 — Spatial parallelism across the cell boundary, evaluated

Acceptance: **报告跨边界时序及耗时** (report the cross-boundary
timing and cost).

`crates/server-runtime/src/space_parallel.rs` drives the shipped
offload checker (`decide_offload` + `collect_offloads`, the
BW-08-05-01 faces) over the dense AoI baseline's own 120-entity field,
split by a partition plane at x = 16 m — column 4's boundary in the
10×12 grid. Entities keep the baseline's deterministic ±0.75 m step
schedule; each is homed on the side its grid column starts on, so the
step schedule genuinely walks entities across the plane.

## The cross-boundary ledger

| Face | Value |
|---|---|
| crossings (position's cell ≠ own) | **2** |
| offloads (migrations) | **2** — exactly one per crossing |
| stays | 118 (home-side entities + any gated crossings) |
| probe wall-clock of the whole check | **~0.009–0.012 ms** |

Structural assertions, all green:

- **Plane ownership** — the branch rule `coordinate < position` gives
  the boundary plane itself to the **right** child, on both split
  axes (x at 16 m; z at 22 m: 21.999 → left, 22.0 → right). A
  straddler sitting exactly on the plane homed left therefore
  migrates: `OffloadTo(FAR)`.
- **Exactly once** — `offloads == crossings`, and every migration's
  destination equals `p_cell_at(entity)` of its containing side.
- **The four Stay gates** hold against the far cell: uncreated,
  dead channel, missing channel entry (treated as dead), and
  delete-pending each keep the entity home; a disabled
  `cell_should_offload` gate stops every offload outright.
- **Rerun identity** — the ledger reproduces identically.

The check's cost is probe data, printed, never asserted: at this
fixture size one full `collect_offloads` pass (120 entities, BSP
descent each) costs ~0.01 ms — three orders of magnitude under one
10 Hz tick. The real cross-boundary cost driver is not the decision
but the migration payload itself (the handover/restore faces measured
in OPT-08-01-01).

## Gates

- `cargo test -p server-runtime --lib`: 1754 passed, 0 failed (4
  new: plane ownership, exactly-once migration, the Stay gates, the
  rerun identity).
- `cargo fmt -p server-runtime --check`: clean.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: no
  new lint.
