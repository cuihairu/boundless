# Process topology

Boundless keeps the BigWorld process split: one executable, one process per
server role, role selected by the first positional argument. This page records
the role set, what each role owns, and which parts of the topology are
implemented today.

## One binary, many roles

The single server target is `boundless-server`, declared in
`apps/server/Cargo.toml`:

```toml
[[bin]]
name = "boundless-server"
path = "src/main.rs"
```

`apps/server/src/main.rs` takes `args[1]` and hands it to
`server_runtime::app::run`. There is no separate binary per role.

## The role set

`KNOWN_ROLES` in `crates/server-runtime/src/app.rs` is the canonical order of
the server processes in the reference revision (its `server/*/` directories):

| # | Role | Owns |
| --- | --- | --- |
| 1 | `baseapp` | account, avatar and offline player data |
| 2 | `baseappmgr` | lifecycle and discovery for the `baseapp` fleet |
| 3 | `cellapp` | an in-game world region and its entities |
| 4 | `cellappmgr` | lifecycle and discovery for the `cellapp` fleet |
| 5 | `dbapp` | persistence reads and writes |
| 6 | `dbappmgr` | lifecycle and discovery for the `dbapp` fleet |
| 7 | `loginapp` | client authentication and world selection |
| 8 | `reviver` | resurrection and offline recovery services |

Roles follow a manager/worker split: every `*appmgr` is the control plane for a
fleet of `*app` workers. Starting an unknown role is a hard error that echoes
the bad role and lists the whole set, so the correct spelling is visible next to
it:

```text
unknown server role 'cellap' (expected one of: baseapp, baseappmgr, cellapp, ...)
```

That behaviour is asserted by the `unknown_role_is_rejected_with_directions`
test in the same file, which checks the message contains both the mistyped
`cellap` and the correct `cellapp`.

## Deployment modes

`ServerAppConfig` carries a `serverMode` string with three values
(`crates/server-runtime/src/server_app.rs`):

- `standalone` (the default) — every role runs in one deployment unit.
- `center` — this node is the account/database/login side of the service.
- `periphery` — this node is the cell side, talking to a remote center.

`standalone` is the mode used for everything in [Running a standalone
server](/guide/standalone-server).

## What is implemented

| Piece | State |
| --- | --- |
| Role validation and startup contract | Implemented (`server-runtime/src/app.rs`) |
| Process bootstrap, exit codes, signals | Implemented (`server-runtime/src/server_app.rs`, `signals.rs`, `shutdown.rs`) |
| Game tick, timers, task dispatch | Implemented (`tick.rs`, `timers.rs`, `task_manager.rs`, `dispatcher.rs`) |
| Role-to-role message contracts | Types only (`crates/contracts/src/machined.rs`): the bwmachined header and wire messages exist, no role sends them |
| Service discovery | **Not implemented** |
| Mercury transport framing | Implemented (`crates/mercury/src/packet.rs`) |
| UDP endpoint, event poll and interface dispatch | Implemented (**BW-02-01-01..04**: `endpoint.rs`, `poller.rs`, `network_interface.rs`, `packet_receiver.rs`) |
| Mercury message layer and channels | **Not implemented** — BW-02-02 and BW-02-03 |

`contracts` now carries the bwmachined wire layouts, but nothing depends on it:
the crate is still commented out of the root `[workspace.dependencies]`, and no
role has a `bwmachined` peer to talk to. Until the manager and worker crates
land the topology above is a *deployed but not yet wired* graph: each role
process starts, validates its config and exits when asked.

## Exit contract

The reference exit order is preserved and documented in
`server_app.rs`: every path runs `fini()` then
`interface_.prepareForShutdown()`, whether or not init succeeded, and prints
`%s has shut down.`. `doBWMainT` returns `-1` when the config options fail
post-init validation — that is a reference quirk, not a typo.

Exit codes observed from the binary: `0` on success, `1` on any error
(`apps/server/src/main.rs`).