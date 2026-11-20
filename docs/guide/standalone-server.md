# Running a standalone server

A standalone deployment runs every role in one deployment unit using
`serverMode = standalone`. That is the default
(`DEFAULT_SERVER_MODE` in `crates/server-runtime/src/server_app.rs`), so no
configuration is needed for it; the other accepted values are `center` and
`periphery`.

This page covers starting and stopping a single role.

## Starting one role

```bash
cargo build -p boundless-server
./target/debug/boundless-server loginapp
```

Observed on this tree (Linux x86-64, Rust 1.100.0):

```text
[loginapp] starting (boundless minimal entry)
```

Exit status `0`. The role name is echoed into the line, matching the reference
behaviour where every process logs a startup line naming the app.

All eight roles behave the same way. A role not in the set is rejected before
anything starts:

```bash
$ ./target/debug/boundless-server bogus
boundless-server: unknown server role 'bogus' (expected one of: baseapp, baseappmgr, cellapp, cellappmgr, dbapp, dbappmgr, loginapp, reviver)
$ echo $?
1
```

Passing no argument is the same path with an empty role, so it also exits `1`.

## Stopping one role

::: warning The role process exits by itself today
`server_runtime::app::run` prints the startup line and returns; it does not
enter a network loop, so the process ends on its own and there is no long-lived
process to signal. Sending `SIGINT` or `SIGTERM` to a running role has no
registered handler yet.
:::

The shutdown machinery itself is implemented and tested, just not wired into
the entry point:

| Piece | Where | State |
| --- | --- | --- |
| Staged shutdown vocabulary | `server-runtime/src/shutdown.rs` | Implemented, `ShutDownStage::None` … `Trigger` |
| Signal processor | `server-runtime/src/signals.rs` | Implemented, async-signal-safe pending set |
| Tick and task dispatch | `server-runtime/src/tick.rs`, `task_manager.rs` | Implemented |
| Wiring into `app::run` | `server-runtime/src/app.rs` | **Not done** — the module docstring says the full init chain arrives with BW-01-04 |

`ShutDownStage` keeps the reference's own comments as its documentation, which
is the order you will need when the wiring lands:

```text
None = 0          not shutting down; typical initial value
Request = 1       start your phase, which may ask others to shut down
Inform = 2        tell a waiting parent which stage you are at
DisconnectProxies = 3   BaseApp only
Perform = 4       shut down now, propagate nothing
Trigger = 5       BaseAppMgr only: try LoginApp, then DBApp, then BaseAppMgr
```

## A single-role smoke check

The fastest way to confirm a role still comes up and goes away cleanly:

```bash
set -e
./target/debug/boundless-server loginapp && echo "started and exited cleanly"
if ./target/debug/boundless-server notarole 2>/dev/null; then
  echo "unexpected: bad role was accepted"; exit 1
fi
echo "bad role correctly rejected"
```

## Where this goes next

Starting a *set* of roles — a cell side plus a center side, or a real login
flow — needs role-to-role messages, which is why `crates/contracts` is still a
placeholder. Until it lands, each role in a standalone deployment is an
independent process that starts, reports itself and exits. The role set and its
canonical order are the process topology documented in the architecture
section.

## The reference baseline for comparing against

The reference revision ships already-built role binaries, which is the baseline
these single-role commands are compared against:

```
game/bin/server/el7/server/
  baseapp baseappmgr cellapp cellapp-extensions cellappmgr commands
  dbapp dbapp-extensions dbappmgr loginapp reviver serviceapp bots
```

Two differences from `KNOWN_ROLES` are worth knowing before you diff behaviour:
the installed layout carries extras (`serviceapp`, `commands`, and the
`*-extensions` pairs) that the source `server/*/` role set does not, and `bots`
is the load generator rather than a server role. The eight roles in
`KNOWN_ROLES` are the ones the role-set test pins to the reference source
directories.