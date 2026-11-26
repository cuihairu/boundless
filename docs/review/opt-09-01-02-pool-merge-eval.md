# OPT-09-01-02 — Connection pooling and query merging, evaluated

Acceptance: **报告各后端延迟和吞吐** (report per-backend latency and
throughput).

`crates/db-postgres/tests/pool_merge_eval.rs` runs ONE seeded workload —
48 entity rows and 48 key reads, values from a fixed LCG, each shape
under its own id base so the shapes never collide on one database —
through three execution shapes on the real PostgreSQL server:

1. **shipped** — one reused connection (`PostgresStorage`), every write
   an autocommit `put_entity`, every read a key `get_entity`. 1 connect,
   96 statements, 48 commits.
2. **unpooled** — the same operations, but a fresh connect per
   operation, what a client without a pool pays per call. 96 statements
   **plus 96 connects** over the identical rows.
3. **merged** — all 48 writes inside one `START TRANSACTION` … `COMMIT`
   (one durability flush instead of 48), and all 48 reads as one
   `id = ANY($1)` statement. 50 statements, 1 commit.

The structural counts are asserted — they are backend-independent:
shape 2 pays 96 extra connects for nothing, shape 3 collapses 48 commits
into 1 and 48 read statements into 1, and every shape round-trips the
identical rows (`rows_ok = 48` each).

## The report (PostgreSQL 18.6, local unix socket, 4 runs)

| Shape | Connects | Statements | Commits | Elapsed across 4 runs |
|---|---|---|---|---|
| shipped | 1 | 96 | 48 | 468 / 508 / 448 / 413 ms |
| unpooled | 96 | 96 | 48 | 1.89 / 2.00 / 1.74 / 1.73 s |
| merged | 1 | 50 | 1 | 84 / 64 / 80 / 73 ms |

**Candidate gains, as measured on this runner:**

- **Pooling** (shape 2 vs 1): the same 96 statements cost ~4× more
  wall-clock purely from 96 extra TCP-less unix-socket handshakes —
  ~1.8 s vs ~450 ms. Over TCP the gap widens (each connect adds a
  network + auth round trip); the socket numbers are the floor.
- **Query merging** (shape 3 vs 1): one commit instead of 48 and one
  read statement instead of 48 cut the same 48-row workload from
  ~450 ms to ~75 ms — **~6× throughput** on the shipped shape, with the
  durability cost dropping from 48 fsync-class flushes to 1. The rows
  read back identical, so the merge changes cost, not content.

**The MySQL half of the report:** the statement shapes are identical by
construction, and `tests/store/common/baseline_dataset.sql` (OPT-09-01-01)
pins those exact shapes — the same `tbl_`/`sm_` layout, the same
CHECK-guarded update/delete counts — green on the live MariaDB. MySQL
in-process timings need a Rust driver the port does not carry (the
reference reads MySQL through the C client; the port's MySQL face is the
SQL contract), so its latency claim is the statement parity above, not
sampled wall-clock.

Wall-clock is environment data: printed by the test, never asserted —
the gate the load lane established (BW-17-02-08). The assertions are the
structural counts and row identity, which reproduce anywhere.

## Gates

- `cargo test -p db-postgres`: 134 passed, 0 failed (real server; the
  usual skip gate applies where none is reachable).
- `cargo fmt -p db-postgres --check`: clean.
- `cargo clippy -p db-postgres --all-targets -- -D warnings`: no new
  lint.
