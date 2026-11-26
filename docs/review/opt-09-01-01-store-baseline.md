# OPT-09-01 — MySQL and PostgreSQL baselines on one dataset

The section optimizes connection pooling, query merging, write batching,
and secondary-database consolidation. Both backends must be compared on
**the same dataset** so the numbers attribute differences to the storage
path rather than to the payload. This row establishes the harness and
records the baseline; it does not change a hot path.

## Harness

`crates/db-postgres/tests/store_baseline.rs` builds one deterministic
512-row dataset and stores it through both backends:

- **MySQL / MariaDB reference face** — the reference's table shape
  (`tbl_SchemaBaseline`, `id BIGINT AUTO_INCREMENT`, `sm_*` columns,
  `REPLACE INTO` upsert), driven by a pure-Rust MySQL wire client.
  Prepared `REPLACE INTO` for each timed put; prepared `SELECT ... WHERE
  id = ?` for each timed get.
- **PostgreSQL port face** — `EntitySchema::for_record` over the same
  values, `create_entity_table`, `register_entity_type`, and
  `EntityStorage::put_entity(..., explicit id)` / `get_entity`. The port's
  `INSERT ... ON CONFLICT ("id") DO UPDATE` is the write path.

The rows carry five property kinds: Int, Float, String (including
multi-byte characters), Blob (including NUL/high bytes), and a nested
Sequence/FixedDict value that travels through the tagged JSON codec on
both sides. Every read-back is assert-equal to the written row, so a
timed run cannot pass with a lossy or reordered store. Each leg gets its
own private database; the test skips (with a notice) when its server is
unreachable. Latency samples per put and get are summarized as
p50/p90/p99/max/min and **recorded, never asserted tight** — filesystem,
load, and wire configuration are machine dependent.

```sh
BOUNDLESS_MYSQL_URL='mysql://root@localhost/probe?socket=%2Ftmp%2Fdbbench-mysql%2Frun%2Fmysql.sock' \
BOUNDLESS_PG_URL='host=/tmp/dbbench-pg/run port=5434 dbname=postgres user=cui' \
STORE_BASELINE_OUT=<abs path> \
cargo test -p db-postgres --test store_baseline -- --nocapture
```

Without the URL variables, the runner falls back to the local socket
(`CI_STORE_MYSQL_*` / `CI_STORE_PG_*` or the host defaults) and skips any
leg whose server is absent.

## Record

`docs/review/fixtures/store-baseline/store-baseline.tsv` (22 rows),
captured on private MariaDB (unix socket) and PostgreSQL 18.6 (port
5434) instances on an otherwise contended 14-core machine (load average
~30 from other lanes' suites):

| Face | p50 | p90 | p99 | max |
| --- | ---: | ---: | ---: | ---: |
| MySQL put (µs) | 263.6 | 508.0 | 4,832.5 | 623,879.9 |
| MySQL get (µs) | 265.1 | 400.7 | 3,539.0 | 4,201.6 |
| PostgreSQL put (µs) | 1,779.4 | 6,039.6 | 17,099.1 | 47,941.8 |
| PostgreSQL get (µs) | 899.2 | 3,311.6 | 16,366.1 | 86,805.1 |

Reading: the two legs store identical rows and assert identical reads,
so the gap is the storage path. Under this capture the reference's
prepared `REPLACE INTO` is ~6.8× faster at p50 than the port's
`ON CONFLICT` upsert; the read path is ~3.4× faster. The single
623 ms MySQL put max is one connection-establishment outlier, not a
per-row cost: the p99 (4.8 ms) bounds the steady-state tail. Both tails
move together with fleet load, so OPT-09-01-02 must compare same-run
before/after numbers rather than cross-record numbers. A shared-server
capture under the same load produced much larger PG tails (>600 ms p99)
and is deliberately not the canonical record.

## Fixtures

- 512 rows, fixed `type_id = 1`, `db_id = 1..512`; no random inputs.
- Property values derive from the row index, so any two runs feed both
  backends the same bytes.
- The DDL shape is asserted against `SHOW CREATE TABLE` when a MySQL
  server is reachable; the nested value round-trips through the same
  tagged JSON codec the PostgreSQL `JSONB` column uses.
