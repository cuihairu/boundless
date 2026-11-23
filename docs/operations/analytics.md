# Analytics: event contract and metrics

Analytics turns the game and operations events a running server records into
the counts an operator reads: who was online and when, how long a session ran,
how many of the players who logged in reached each funnel stage, which space is
crowded, and what the business-custom events add up to. The event envelope is
BW-26-01, the metrics are BW-26-02, and the job guarantees — a re-import adds
nothing, a failed run resumes to the one-shot result, and the counts do not
change with date or batch size — are BW-26-03.

::: warning The analysis logic lives in `crates/test-support/`
The metrics are implemented and tested in `crates/test-support/` (modules
`event_contract`, `event_log`, `event_filter`, `session_metrics`,
`session_duration`, `login_funnel`, `space_heat`, `event_aggregate`,
`event_import`, `event_partition`). The standalone CLI entry point named in the
target tree — `python/tools/analytics/` — is not created yet, so today the
metrics are reached through the tests and the runnable example below, not
through a binary. The logic, not the process, is what this page specifies.
:::

## Event envelope (BW-26-01)

Every event carries one envelope, verifiable before the payload is read:

| Field | Meaning |
| --- | --- |
| `version` | the schema version, `"1"` |
| `id` | unique to its emission: `{kind}:{serial}`, e.g. `login:1` |
| `kind` | `login`, `session`, `movement` or `custom` |
| `at` | when the event happened |
| `source` | the process that emitted it |

A `custom` event is a business event with free-form fields: it names itself
(`name`), and a field the contract does not know is carried and marked, never
dropped and never read as a contract field.

## Batch export and boundary redaction (BW-26-01-03)

An export hands a sample to someone outside the recording process, so a
credential or contact value never crosses unchanged. The default policy masks
`password`, `token`, `secret`, `email`, `phone` and `ip` (case-insensitively)
and drops a custom event's unknown fields unless the policy opts to carry them
— a carried sensitive unknown field is still masked. The acceptance is that a
sample leaks nothing sensitive:

```
$ cargo run -p test-support --example analytics
redaction leaks nothing: true
```

## Online and concurrency (BW-26-02-02)

A session is a window `{session, player, started, ended}`, where `ended` is
absent while the session is still open. A player is online at a moment when
their window opened at or before it and has not closed at or before it; an
open window counts from its start. The peak concurrency is the highest such
number, reached at a moment a session opened.

```
online at 250: 1
peak concurrency: 2
```

## Session duration (BW-26-02-03)

A session is one uninterrupted run: its duration is the elapsed time between
opening and closing, counted in full even when it crosses a day boundary —
never sliced at midnight. A session that has not ended has no duration yet: it
is surfaced as open and left out of the counted totals until it closes.

```
sessions closed 2 open 1, cross-day true
longest session: 200
```

## Login funnel (BW-26-02-04)

The stages are login, session start, and first move. A player is keyed by
account on login and by player name on session and movement, so a retried
login — the same account logging in again — counts the player once per stage,
never once per attempt.

```
funnel: login=3 session=0 first_move=3
```

## Space heat (BW-26-02-05)

A space is cold when empty, warm from one player, and hot at five or more
(`HOT_AT = 5`). Occupancy moves with the player: a movement event takes them
out of one space's count and into the next's, never below zero.

```
space 2 warm (2), space 3 cold (0), hot at 5
```

## Custom event aggregation (BW-26-02-06)

A custom event is a named event with free-form fields. The aggregate groups
them by name — how many, and the sum of one numeric field over those that
carry it (a name that never carries the field reads `None`, not a false zero).
Rows come back in name order, so the same input always recomputes the same
report:

```
report:
join: count=1
purchase: count=2 total=250
telemetry: count=1
```

## Job guarantees (BW-26-03)

Each event is stored once by id. Re-sending a batch adds nothing; a run that
fails and resumes with the remaining batches — re-sending the batch that was in
flight — reproduces the one-shot import exactly; and the counts do not change
with how the stream is split into days or chunks:

```
reimport same report: true
```

## Running the examples

Every example above is one runnable target over one fixed fixture, so the
口径 is reproducible by hand:

```bash
cargo run -p test-support --example analytics
```

The example is `crates/test-support/examples/analytics.rs`; the metrics it
prints are each asserted by the module tests that carry the same acceptance
(`cargo test -p test-support`, plus the event-import and partition tests that
pin the job guarantees).
