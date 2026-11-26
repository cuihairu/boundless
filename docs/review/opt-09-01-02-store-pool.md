# OPT-09-01-02 — connection pooling and query preparation

The acceptance: **report per-backend latency and throughput**
(`报告各后端延迟和吞吐`). The reference `lib/db_storage_mysql` keeps
`numConnections_` (default 5, `db_config.cpp:171`) long-lived background
sessions, one MySQL connection per thread (`MySqlThreadData`), and
pre-compiles each entity type's SQL templates at startup
(`EntityTypeMapping::prepareSQL()`, `mappings/entity_type_mapping.cpp:190-199`).
The Boundless PostgreSQL port keeps one `RefCell<Client>` and builds
statement text on every call. This row measures which of those two faces
actually buys throughput, on the frozen OPT-09-01-01 dataset.

## Candidates

Each leg stores all 512 rows and reads all 512 back, asserting every row
equal to the dataset — no candidate wins by skipping work.

- **single** — one persistent connection, SQL built per op (the 01-01 face).
- **pool** — five connections (MySQL `Pool`, five PG `Client`s) rotated
  round-robin, SQL still built per op.
- **prepared** — one connection, statements prepared once and reused.
- **pool_prepared** — five connections, one prepared statement pair each.

## Record

`docs/review/fixtures/store-pool/pool-candidates.tsv` (48 rows), captured
on the same private MariaDB / PostgreSQL instances as 01-01:

| Backend / leg | put p50 | put p99 | get p50 | get p99 | ops/s |
| --- | ---: | ---: | ---: | ---: | ---: |
| MySQL single | 215 µs | 4,314 µs | 427 µs | 1,298 µs | 2,325 |
| MySQL pool | 227 µs | 1,720 µs | 419 µs | 1,607 µs | 2,371 |
| MySQL prepared | **162 µs** | **299 µs** | **336 µs** | **597 µs** | **3,910** |
| MySQL pool+prepared | 190 µs | 701 µs | 352 µs | 819 µs | 3,400 |
| PostgreSQL single | 607 µs | 2,477 µs | 544 µs | 1,266 µs | 1,517 |
| PostgreSQL pool | 618 µs | 7,233 µs | 535 µs | 2,403 µs | 1,224 |
| PostgreSQL prepared | **283 µs** | 5,111 µs | **244 µs** | **615 µs** | **2,875** |
| PostgreSQL pool+prepared | 355 µs | 4,865 µs | 268 µs | 637 µs | 1,818 |

## Reading

**Prepare-once is the dominant candidate; pooling alone does not pay at
this concurrency.** On MySQL it cuts put p50 by 25% and p99 by 93%
(4.3 ms → 0.3 ms) — the per-op prepare round-trip was the entire tail.
Throughput goes from 2,325 to 3,910 ops/s (1.68×). PostgreSQL's put p50
halves (607 → 283 µs) and its get p99 halves (1,266 → 615 µs), for a
1.90× throughput gain. This matches the reference's shape: it prepares
each type's SQL once and reuses it for every task.

Adding five connections on top of preparation **regresses both backends**
(MySQL 3,910 → 3,400 ops/s; PG 2,875 → 1,818) and widens tails (PG put
p99 5.1 → 4.9 ms but p50 grows 283 → 355 µs). The workload is strictly
serial — one put, one get, then the next row on the same rotated
connection — so five open sessions add connection acquisition and
cache-cold statement state without any overlapping work to amortize it.
The reference's pool exists to serve many concurrent background tasks,
not to speed a single serial stream.

**Verdict: take statement reuse; defer the pool.** The port should build
and prepare each entity type's put/get SQL once at registration
(the analogue of `EntityTypeMapping`'s cached `Query` members) and reuse
the prepared statement per row. Pool sizing is a workload decision for a
concurrent caller — the harness pins the reference default of 5 but does
not claim a serial-stream win. Both legs need the same-run before/after
comparison at OPT-09-01-04; cross-record numbers move with fleet load.

## Harness

`crates/db-postgres/tests/store_pool_candidates.rs` — one deterministic
run of all eight legs. Structural tests pin the pool size, the shared
01-01 corpus, and that both SQL shapes name every schema column.

```sh
BOUNDLESS_MYSQL_URL='mysql://...' \
BOUNDLESS_PG_URL='host=... port=... dbname=postgres user=...' \
STORE_POOL_OUT=<abs path> \
cargo test -p db-postgres --test store_pool_candidates -- --nocapture
```

Both legs skip with a notice when their server is unreachable; the
structural tests still run.
