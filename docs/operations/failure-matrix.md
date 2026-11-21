# Failure matrix

The matrix below records, per cluster role, what happens when the process dies
abnormally, which of those faults the reference BigWorld server recovers from
**automatically**, and where Boundless stands on each row. Every row is
traceable to a `todo.md` id and a reference source file.

The scope is the cluster lifecycle tracked by **BW-05-06**:
**BW-05-06-04** covers abnormal exit and restart; **BW-05-06-05** extends the
same matrix with isolation, reconnection and duplicate notification.

## Two recovery paths

The reference splits recovery across two actors.

**The Reviver process** (`server/reviver/`) is the only component that ever
creates a replacement process. Its `ComponentReviver` table registers exactly
five targets — CellAppMgr, BaseAppMgr, DBAppMgr, DBApp and LoginApp
(`server/reviver/component_reviver.cpp`, `MF_REVIVER_HANDLER`). When a target
stops answering `reviverPing`, the Reviver asks `bwmachined` to create a new
instance (`Reviver::revive`, `server/reviver/reviver.cpp`). A replacement
process restarts cold and the surviving workers re-register through the birth
listeners they installed for the manager interface
(`server/dbapp/dbapp.cpp`, `DBApp::initBirthDeathListeners`).

**The managers** detect death of their own workers and recover *state* without
spawning a process: BaseAppMgr via `BaseAppSubSet::onBaseAppDeath`, CellAppMgr
via `CellAppMgr::handleCellAppDeath`, DBAppMgr via its node-death handling.
These paths kill the corpse for good, re-route the dead app's load (cells or
backed-up bases) onto surviving apps, and trigger a controlled cluster
shutdown when the loss leaves the cluster in a bad state.

The config switches that decide the shutdown side of a death are declared in
`lib/server/manager_app_config.cpp`:

| Option | Default |
| --- | --- |
| `shutDownServerOnBadState` | `true` |
| `shutDownServerOnBaseAppDeath` | `false` |
| `shutDownServerOnCellAppDeath` | `false` |
| `shutDownServerOnServiceAppDeath` | `false` |
| `shutDownServerOnServiceDeath` | `false` |
| `isBadStateWithNoServiceApps` | `false` |

## Abnormal exit and restart (BW-05-06-04)

| Role | Fault | Reference recovery path | Auto-recovery | Boundless | Evidence |
| --- | --- | --- | --- | --- | --- |
| CellAppMgr | crash | Reviver detects death, asks machined to create `cellappmgr`; surviving CellApps re-register via birth listener | **Yes** | Baseline target + revive decision modelled | `reviver.rs::with_baseline`, `death_triggers_create_and_birth_restores_service` |
| BaseAppMgr | crash | Reviver detects death, asks machined to create `baseappmgr`; BaseApps re-register via birth listener | **Yes** | Baseline target + revive decision modelled | `reviver.rs::with_baseline`, `death_triggers_create_and_birth_restores_service` |
| DBAppMgr | crash | Reviver detects death, asks machined to create `dbappmgr`; DBApps re-register via birth listener | **Yes** | Baseline target + revive decision modelled | `reviver.rs::with_baseline`, `death_triggers_create_and_birth_restores_service` |
| DBApp | crash | Reviver detects death, asks machined to create `dbapp`; DBAppMgr promotes the next alpha on the node death | **Yes** | Node-death promote modelled | `dbappmgr.rs::handle_db_app_death`, `alpha_death_promotes_next_and_notifies` |
| LoginApp | crash | Reviver detects death, asks machined to create `loginapp`; pending logins on the dead instance are lost | **Yes** (process; sessions lost) | Node-death notify modelled | `dbappmgr.rs::handle_login_app_death`, `login_app_death_and_manager_births` |
| BaseApp | crash | **Not** a Reviver target — no process restart. `BaseAppSubSet::onBaseAppDeath` killApp, re-adjust backup locations (`OP_CRASH`), `stopBackupsTo`; last BaseApp gone with `shutDownServerOnBadState` (default on) starts a controlled shutdown | **No** process; state restored from backups | App removal modelled at the scheduler level; the death decision and backup re-adjust are the **BW-07** entity-backup lane | `baseappmgr.rs::remove_app`, `server/baseappmgr/baseappmgr.cpp:1613` |
| CellApp | crash | **Not** a Reviver target — no process restart. `CellAppMgr::handleCellAppDeath` SIGQUIT the corpse, broadcast the death to surviving CellApps, wait for their acks, tell BaseAppMgr to restore the entity stream, recreate the dead app's cells on an alternate app; no CellApps left with `shutDownServerOnBadState` (default on) starts a controlled shutdown | **No** process; cells recreated and entities restored | Death path modelled at the state-machine level; the entity restore and the ack-before-restore ordering are the **BW-07** lane | `cellappmgr.rs::handle_cell_app_death`, `handle_cell_app_death_recreates_cell_and_broadcasts`, `handle_cell_app_death_of_last_app_triggers_shutdown` |
| Reviver | crash | No component watches the Reviver. The watched components keep running; their `ReviverSubject` only reattaches to another Reviver that is already up (`TIMEOUT_REATTACH` pass keeps priorities dense) | **No** (attach to a surviving Reviver only) | Subject reattach and the reattach pass modelled | `reviver.rs`, `unaddressed_monitor_is_picked_up_by_reattach`, `subject_reaccepts_a_reviver_after_timeout` |
| bwmachined | crash | The Reviver asks machined to create replacements, so revival is impossible while the daemon is down; every other process loses its creator | **No** | Transport not yet wired (Mercury lane) | `reviver.rs` module doc |

## Isolation, reconnection and duplicate notification (BW-05-06-05)

| Role | Fault | Reference recovery path | Auto-recovery | Boundless | Evidence |
| --- | --- | --- | --- | --- | --- |
| Any role | network partition | Death is only declared after the liveness checks expire; a partition looks like a death and takes the same path above | Depends on the row | Pending — recorded when BW-05-06-05 lands | — |
| Any role | duplicate death notification | The managers guard against a repeated notification (`lastCellAppDeath_`, `already killed`) | **Yes** | Pending — recorded when BW-05-06-05 lands | — |

## How a row is read

- **Auto-recovery** is whether the reference server returns to service without
  operator action. A `Yes` is a process restart through the Reviver or a state
  restore through a manager; a `No` needs a human to bring the role back.
- **Boundless** is the honest state of the working tree today. `modelled` means
  the decision is unit-tested at the state-machine level; the Mercury transport
  that actually sends the revive/create/notify messages is the **BW-02** lane
  (`crates/server-runtime/src/reviver.rs` module doc), so a row that is fully
  modelled still needs that lane before it runs in a live cluster.
- Rows marked **BW-07** depend on the entity backup and restore lane; their
  recovery path is recorded here so the failure behaviour is decided up front,
  not invented later.
