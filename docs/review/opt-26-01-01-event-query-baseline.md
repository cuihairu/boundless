# OPT-26-01-01 — fixed event replay and query baseline

The acceptance: **import, query and memory data are kept**
(`保留导入、查询及内存数据`). One deterministic event stream — a
512-entity corpus imported through the migration face, then three
update generations replayed through the storage contract — lands on a
private PostgreSQL server, and every leg's figures are recorded: the
import rate, the replay event rate, per-op `get` and name-lookup
latency, the full-scan `lookup_entities` shapes, and a counting
allocator's per-window allocation/byte/peak figures. The pinned sample
report — counts per level window and per query shape over the final
state — is the baseline OPT-26-01-04 recomputes, so those counts are
asserted exact while the timings are only recorded.

## The fixture

`crates/db-postgres/tests/event_query_baseline.rs`, one live run plus
five serverless structural tests. The corpus is four scalar properties
per entity (identifier string, level, ratio, blob), so the lookup
faces cover the string match, the numeric match, the sort and the
identifier legs:

- **import** — `import_entities` lands all 512 rows (explicit-id
  writes; a fresh database skips nothing).
- **replay** — 3 update generations × 512 entities through
  `put_entity`'s update leg; the level walks +7 per generation
  (wrapping at 200). Read-back after the replay pins every entity at
  its final generation — the replay's data equality, asserted.
- **logons** — every eighth entity checks out a mailbox (64 online,
  448 offline) for the filter legs.
- **query legs** — `get` by id (256 samples), resolve-and-read name
  lookups (128 samples), id-only resolution (128 ops), and the
  `lookup_entities` shapes: ALL, Online, Offline, a property query on
  level 21, and a top-10-by-level LIMIT query.
- **memory** — the mercury counting-allocator pattern (live-set
  ledger paired by pointer) windows the import, the replay, the
  representative full-scan query, and reports allocs / bytes /
  live-bytes peak per window.

The record: `docs/review/fixtures/event-query/event-query.tsv` (39
rows), captured on the private PostgreSQL 18.6 instance (port 5434):

| Leg | figure |
| --- | ---: |
| import | 512 rows, 1136 ms, **451 rows/s** |
| import memory | 104 576 allocs, 4.17 MiB, 16 KiB peak |
| replay | 1536 events, 2247 ms, **683 events/s** |
| replay memory | 311 974 allocs, 9.65 MiB, 17 KiB peak |
| get by id | p50 846 µs, p99 1785 µs, max 4832 µs |
| name lookup | p50 11 649 µs, p99 20 950 µs, max 20 961 µs |
| id-only resolution | 128 ops, 1552 ms |
| lookup ALL (one full scan) | 512 rows, 9.53 ms, 220 KiB peak |
| report counts | ALL 512 / online 64 / offline 448 / level 21 → 3 / top-10 → 10 |
| level windows | 129 / 150 / 133 / 100 (partition sums to 512) |

## Reading

**The name lookup is the expensive shape.** A `get` by id costs ~0.8
ms (one keyed read); a name lookup costs ~11 ms p50 — **14× the get**
— because `get_database_id_from_name` scans `all_entities()` and
string-compares the identifier per row, then the read does a second
round trip. Id-only resolution alone is 1552 ms for 128 ops, so the
scan dominates, not the read. This is the face OPT-26-01-03's
aggregation replaces, and the p50/p99/max triple is its before
record.

**Memory is flat per query but the ledger is exact.** One full scan
allocates ~2 170 blocks / ~380 KiB per call with a 220 KiB live-bytes
peak — the result vectors dominate. The allocation and byte counts
repeated exactly across three capture runs (104 576 import allocs,
311 974 replay allocs, identical), which is what makes this fixture a
usable baseline for 01-02's batch-size evaluation: any change in
allocation shape shows up as a count delta, not noise.

**The report is the recomputation anchor.** The five query-shape
counts and the four level windows partition the corpus exactly
(129+150+133+100 = 512; online+offline = 512; the level-21 property
query returns exactly the three seeds 0/200/400). These are asserted
in the run itself, so 01-04's recomputed report compares against
counts that cannot silently drift.

**Import versus replay:** the migration import moves 512 rows at 451
rows/s while the replay loop sustains 683 events/s on updates — the
import pays one `get` + explicit-id write per row through the
migration face, the replay one update per event on the contract. Both
are recorded per row/event so 01-02 can swap the batch shapes under
the same stream.

## Harness

```sh
BOUNDLESS_PG_URL='host=... port=... dbname=postgres user=...' \
EVENT_QUERY_OUT=<abs path> \
cargo test -p db-postgres --test event_query_baseline -- --nocapture
```

Without a reachable PostgreSQL server the live leg skips with a
notice; the five structural tests (corpus determinism, the replay
step, the window partition, the pinned level-21 seeds, the online
stride) still run.
