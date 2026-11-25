# Load testing with bots

Load testing is driven by headless bot clients against a real server topology.
The reference entry point is `server/tools/bots/`; its behavioural parity is
BW-12-04 and its scenario work is BW-17.

::: warning No live topology yet
The bot runtime lives in `crates/load` (fleet, movement controllers,
watcher commands — BW-17-01), the scenario input layer in `crates/load` +
`tests/load/scenarios/` (BW-17-02-01) and the standalone process in
`apps/bots` (BW-17-03-05). The process drives the scenarios' **offline**
faces: a live server topology is not wired in yet, so every number it
reports is the offline expectation a live run will check against.
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

| Scenario | Item | Measured | Runs |
| --- | --- | --- | --- |
| Login surge | BW-17-02-03 | a stated rate of logins, repeatably | PR smoke |
| Steady movement | BW-17-02-04 | same seed produces the same trajectory | PR smoke |
| Dense AOI | BW-17-02-05 | target density and visibility counts | PR smoke |
| Cross-cell | BW-17-02-06 | hand-off success rate | not yet |
| Disconnect/reconnect | BW-17-02-07 | recovery time | not yet |
| Long soak | BW-17-02-08 | resource use of the load generator itself | schedule/manual |

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

## The PR smoke gate (BW-17-03-05)

`ci/load.sh` runs every executable scenario under
`tests/load/scenarios/`: each one runs with `LOAD_DURATION` (wall budget) and
`LOAD_CLIENTS` (concurrency) and must exit 0 inside the budget having printed
at least one `metric<TAB>value` line. The pull-request job
(`load-smoke`) gives the low knobs — 2 clients, 20 seconds — so the
nightly-scale saved scenarios run at a scale a PR runner affords;
`load-schedule` gives the high ones.

The gate is re-runnable on purpose. The saved scenarios carry fixed seeds and
`run_smoke` runs each face twice and refuses to report a scenario whose two
passes disagree, so a red run reproduces its own numbers; `workflow_dispatch`
re-runs it without a push, and the records upload even when the lane is red,
which is what a retry compares against.

## What you can run today

The report envelope (BW-17-03-01) is delivered: a scenario run produces a
deterministic JSON document through `load::ScenarioReport::record` +
`to_json`, carrying the schema tag, the scenario name, the generator
version (`load <pkg version>`), the topology the run targeted (`offline`
until a live server joins), the fixed seed, and the run's recorded
values. Login success and online counts aggregate on top of the same
ledger face (BW-17-03-02 onward).

```bash
cargo test -p load                # fleet, scenarios, ledgers, reports, smoke
ci/load.sh self-test              # the harness records and bounds a case
ci/load.sh                        # the scenarios, low load, records to ci-out/
cargo test -p server-runtime      # tick, timers, task manager
```

`ServerAppConfig.update_hertz` defaults to `10`, from
`DEFAULT_GAME_UPDATE_HERTZ` in `crates/server-runtime/src/server_app.rs`.

See also [Testing and release](/operations/testing-and-release) for the gate
that short scenarios will join.