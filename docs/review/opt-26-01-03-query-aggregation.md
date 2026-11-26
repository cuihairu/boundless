# OPT-26-01-03 — query aggregation evaluation

The acceptance: **the latency change is reported**
(`报告延迟变化`). The OPT-26-01-01 baseline found the expensive query
shape: a name lookup scans `all_entities()` and string-compares the
identifier per row, costing ~14× a keyed `get`. This row evaluates the
aggregation those name maps imply — one full scan builds a
name→id map (512 entries) and a level→ids map (200 keys), then the
same 128-name sample grid resolves through the maps instead of the
scan. The latency change is the before/after pair on that grid, and
every aggregated answer is asserted equal to the scan face's answer.

## Design

`crates/db-postgres/tests/query_aggregation_candidates.rs`: the same
512-entity corpus as the baseline (explicit-id migration import, 64
checkouts for the online split) lands on a fresh table; first the
scan face resolves all 128 sample names and runs a full
`lookup_entities` ALL, then `build_aggregates` performs one scan over
the corpus and both maps are resolved against the identical sample
grid. A property query on the final level pins the level→ids map
against `lookup_entities`' string-match answer. Finally three update
generations replay through `put_entity`, the name map is checked
still valid (identifiers never changed), and the rebuilt level index
is checked against the new seeds — the staleness/rebuild shape a
live aggregation would manage.

The record: `docs/review/fixtures/query-aggr/query-aggr.tsv` (21
rows), captured on the private PostgreSQL 18.6 instance (port 5434):

| Leg | figure |
| --- | ---: |
| name lookup, scan (128 ops) | mean 3009 µs; p50 2960 / p99 4553 / max 4575 µs |
| name lookup, map (128 ops) | mean 634 ns; p50 533 / p99 1364 / max 3417 ns |
| **latency change** | **~4746× (3009 µs → 0.634 µs)** |
| build one scan (both maps) | 4.50 ms → 35 µs amortized per lookup |
| name map entries | 512 (every identifier) |
| level map entries | 200 keys, 512 ids; level-21 query returns 3 seeds |
| after 3 replay generations | name map still valid; level index rebuilt from the new state in 3.12 ms |

## Reading

**The aggregation removes the scan, not the query.** The scan face's
~3 ms per name lookup is almost entirely `all_entities()` deserializing
512 rows; the map face's ~0.6 µs is a `HashMap` get on an owned
string — a **~4700×** latency change on the same sample grid. The
one-time build costs 4.5 ms (one scan), amortized to ~35 µs per
lookup over the 128-op sample; the break-even against the scan face
is about two lookups, so an aggregation is worth building even for a
small batch. The build is also reused across all query shapes, not
just names — the level→ids map answers the property query from memory
too.

**Correctness is pinned twice.** Every map answer is asserted equal
to the scan face's answer (name→id for all 128 samples), and the
level map's top-level index is asserted equal to
`lookup_entities`' own string-match result (three seeds at level 21,
the same count as the sample report). After the replay, the name map
survives because identifiers are stable while the level map would be
stale — the harness rebuilds it and checks the new seeds. This is the
distinction a live aggregation must handle: index the stable
identifier once, refresh the changing properties.

**The trade-off is scan-plus-memory for latency.** One scan builds
both indexes; no per-row server round trip is saved on the replay
itself, only on the queries that would otherwise scan. The 512-entry
name map plus 512 ids under 200 level keys is the retained state;
01-04's recomputed report uses the same counts, so the aggregation
cannot silently change the report's answers.

**Verdict: aggregate the identifier map once, refresh the property
index on replay.** The latency change is four orders of magnitude for
the name leg at one scan of build cost, and the answers are
byte-equal to the scan face's. The per-generation rebuild (~3 ms) is
paid once per update batch rather than per query.

## Harness

```sh
BOUNDLESS_PG_URL='host=... port=... dbname=postgres user=...' \
QUERY_AGGR_OUT=<abs path> \
cargo test -p db-postgres --test query_aggregation_candidates -- --nocapture
```

Without a reachable PostgreSQL server the live leg skips; the two
structural tests (sample grid, corpus determinism) still run.
