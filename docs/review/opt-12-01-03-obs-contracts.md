# OPT-12-01-03 — The backpressure rerun and the tool-query interop

The acceptance: **queries and data recording match the contracts**
(`查询及数据记录符合契约`). After the cost work of 01-01/01-02 the
rerun drives the contracts end to end — a real writer, a real query
engine, a real watcher tree — so the tools' view is proven unchanged.

## The recording contract under backpressure

A real `UserLogWriter` (uid 1001, `alice`, deterministic segment
suffixes) writes 12 `%d` records against a 96-byte segment budget:

| Face | Contract | Result |
| --- | --- | --- |
| Segment rolls | `is_full` flips → next message lands in a fresh segment | **3 rolls** (~9 records per segment at the budget) |
| Single-active invariant | only one open segment after each write | **holds** |
| `active_files` rewrite | `entries/args/metadata.<suffix>` per user, naming the active segment only | **exact** |
| Disk read-back | `read_entry(0)` of the rolled-to segment replays the (12−count)th record written, `bounds()` agrees | **byte-exact** |

## The query contract

The written log replays into a `QueryableLog` and `run_query` answers
the tool shape:

| Face | Contract | Result |
| --- | --- | --- |
| Full query | 12 records answer, interpolated text `"42"`, component/pid/category/source filled | **12/12** |
| PID filter | a foreign pid rejects every record | **empty** |
| Severity floor | records at priority 2 excluded when only 1 passes | **empty** |
| Unknown category | a specifier the log doesn't know **fails the query** (`UnknownCategory`) | **errors** |
| Backwards walk | newest-first returns the same span, reversed order | **12/12, order matches** |

## The watcher contract

| Face | Contract | Result |
| --- | --- | --- |
| Query shape | value + comment + mode per leaf (`"100"`, `"hit points"`, `ReadWrite`) | **exact** |
| Set semantics | a parseable write stores; a malformed one leaves the value | **110 then 110** |
| Event-stats mount | `enabled`/`counts`/`sizes`/`oversizes` under `events` in a `WatcherTree` | **4 children** |

## Harness

`crates/server-runtime/tests/observation_contracts_rerun.rs` — one
deterministic run, temp dir under the OS temp root, fixed timestamps
via the writer's suffix source.

```sh
OBS_CONTRACTS_OUT=<abs path> cargo test -p server-runtime \
    --test observation_contracts_rerun -- --nocapture
```

## Fixtures

`docs/review/fixtures/obs-contracts/obs-contracts.tsv` — 18 rows,
captured with the command above.
