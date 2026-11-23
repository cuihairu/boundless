# Load testing with bots

Load testing is driven by headless bot clients against a real server topology.
The reference entry point is `server/tools/bots/`; its behavioural parity is
BW-12-04 and its scenario work is BW-17.

::: warning No bot runner exists in the working tree
There is no `apps/bots/` and no bot runtime code anywhere in this repository — a
search for the bot runner returns nothing but `todo.md`. The scenario input
layer (`crates/load` + `tests/load/scenarios/`) is in place (BW-17-02-01); this
page specifies the target so the scenario files can be written before the
runner exists.
:::

## Target layout

```
apps/bots/                    # the standalone load-generation process and its CLI
tests/load/scenarios/         # scenario definitions: seed, accounts, ramp, duration
```

## Bot runtime (BW-17-01)

The bots reuse the client connection layer, so anything they can do, a real
client can do.

| Item | Accepted when |
| --- | --- |
| BW-17-01-01 client connection layer | a bot can log in and log out |
| BW-17-01-02 batch add/remove and tag filter | control commands only affect the tagged bots |
| BW-17-01-03 Python behaviour entry | a script drives the movement |
| BW-17-01-04 beeline | matches the reference's fixed route |
| BW-17-01-05 patrol path | waypoint order and loopback match |
| BW-17-01-06 Watcher control | the reference's callable path can be cross-tested |

Tag filtering matters for diagnosis: when a scenario goes wrong you need to be
able to talk to one group of bots without touching the rest.

## Scenarios (BW-17-02)

A scenario is a fixed-seed file under `tests/load/scenarios/` that names the
account pool, concurrency, ramp rate, warm-up and duration. Fixed seeds are what
make a second run comparable to the first.

| Scenario | Item | Measured |
| --- | --- | --- |
| Login surge | BW-17-02-03 | a stated rate of logins, repeatably |
| Steady movement | BW-17-02-04 | same seed produces the same trajectory |
| Dense AOI | BW-17-02-05 | target density and visibility counts |
| Cross-cell | BW-17-02-06 | hand-off success rate |
| Disconnect/reconnect | BW-17-02-07 | recovery time |
| Long soak | BW-17-02-08 | resource use of the load generator itself |

Scenario input loading (seeds, account pools, concurrency) is BW-17-02-01, and
ramp/warm-up/duration timing is BW-17-02-02.

## Reports (BW-17-03)

The acceptance criterion for this documentation item is that a sample scenario
produces a report. The report is machine-readable and must be traceable:

| Field | Item |
| --- | --- |
| version, topology, seed | BW-17-03-01 |
| login success rate, online count | BW-17-03-02 |
| latency percentiles and timeouts | BW-17-03-03 (p95/p99 checked against a fixed sample) |

Plus tick timeouts, bandwidth and resource usage. Regression policy: short
scenarios run in the PR smoke gate, capacity and long-soak scenarios run on a
schedule or manually, with thresholds set from measured baselines.

## What you can run today

Nothing in this section. The only reproducible load-related fact in the tree is
the tick rate the server is configured for, which is what a load test ultimately
stresses:

```bash
cargo test -p server-runtime      # tick, timers, task manager
```

`ServerAppConfig.update_hertz` defaults to `10`, from
`DEFAULT_GAME_UPDATE_HERTZ` in `crates/server-runtime/src/server_app.rs`.

See also [Testing and release](/operations/testing-and-release) for the gate
that short scenarios will join.