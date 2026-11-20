# Database migration and recovery

Boundless keeps the reference's four storage backends and adds PostgreSQL
operations on top. This page describes the target procedure for migrating and
restoring data.

::: danger Nothing on this page runs today
There is no database code in the working tree: no SQL dependency, no `.sql`
file, no migration directory, and no `dbapp` logic beyond the role name. The
only persistence primitive that exists is the id vocabulary
(`DatabaseId = i64`, `PENDING_DATABASE_ID = -1` in
`crates/foundation/src/ids.rs`). Every step below is tracked by a `todo.md`
id and is written so that the page becomes true as the work lands.
:::

## Backends

| Backend | Module |
| --- | --- |
| XML | BW-09-03 |
| SQLite (secondary store) | BW-09-03 |
| MySQL | BW-09-02 |
| PostgreSQL | BW-09-06, BW-09-07 |

`dbapp` reports which backend it opened at startup (BW-09-06-01). The
operational tooling is a separate module, BW-09-05, and the Billing extensions
BW-09-04.

## Migration

Migration is **MySQL data into a new PostgreSQL database**, using the reference
tools whose Boundless equivalents BW-09-05 is responsible for:
`sync_db`, `consolidate_dbs`, `transfer_db`, `snapshot_helper`,
`clear_auto_load`, `remove_db`.

### Schema work

| Step | `todo.md` | What "done" means |
| --- | --- | --- |
| Initialise an empty PostgreSQL database | BW-09-07-01 | a schema that boots is generated from scratch |
| Schema **dry-run** | BW-09-07-02 | the database is unchanged and the statements are reported |
| Apply schema updates | BW-09-07-03 | an older sample is readable after the upgrade |

The dry-run is the important one: a migration tool that cannot show you what it
would do before doing it is not operable.

### Data work

| Step | `todo.md` | What "done" means |
| --- | --- | --- |
| Import entities and accounts from MySQL | BW-09-07-05 | ids, relationships and binary fields all match |
| Calibrate DBID allocation after import | BW-09-07-06 | newly created entities do not collide |
| Verify a failed migration and re-run it | BW-09-07-07 | the next run does not import anything twice |

DBID allocation itself is BW-09-06-04: concurrent creation must not repeat an
id, and the sequence must survive a restart.

### Correctness contract for writes

These come from BW-09-06 and are what "the migration produced a correct
database" actually rests on:

- **Commit** — an update committed in one connection is readable from another
  (BW-09-06-06).
- **Rollback** — when a write fails midway, the entity and its related rows
  read back their old values from an independent connection (BW-09-06-07).
- **Conflicts** — two writers on the same entity return the storage contract's
  result, and retrying does not leave duplicates (BW-09-06-08).
- **Reconnect** — after the database comes back, new requests succeed
  (BW-09-06-09).
- **Relogin** — session state matches the MySQL baseline (BW-09-06-10).

Field-level coverage (BW-09-06-02, -03) requires unicode, binary and nested
types to round-trip, and the account table to enforce its uniqueness
constraint so a duplicate account is a contract error.

## Recovery

Recovery is "export a backup, restore into an empty target, confirm the records
are identical":

| Step | `todo.md` | What "done" means |
| --- | --- | --- |
| Export and restore a backup | BW-09-07-04 | an empty target database ends up with the same records |

Against a restored copy, check the same set of values that BW-09-07 uses for
import: entity ids, account links, binary fields, defaults, and the next
allocatable DBID. If the next DBID does not line up after a restore, the
allocation sequence (BW-09-06-04) was not part of the backup.

## Working with the id space today

Until the storage layer lands, the only part of persistence you can rely on is
the id convention:

```rust
use foundation::ids::{DatabaseId, PENDING_DATABASE_ID};

let id: DatabaseId = PENDING_DATABASE_ID;   // -1, not yet assigned
assert_eq!(id, -1);
```

An id of `-1` means "not yet allocated", which is why it must never be written
to a backend as a real id.