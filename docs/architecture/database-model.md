# Database model

Persistence is two crates: `crates/db` is the storage contract (error
vocabulary, backend selection, the entity record shape), and
`crates/db-postgres` is the first real backend. This page describes what is
implemented and what is not; the *procedures* — migration, backup, recovery —
live in [Database migration and recovery](/operations/database) as a tracked
plan.

## The storage contract

`crates/db/src/error.rs` defines the whole error vocabulary. The reference
`DBStorage` interface returns a status rather than throwing, and the port
keeps that shape:

| Variant | Meaning |
| --- | --- |
| `NotFound` | missing DBID / missing name — an outcome, not a failure |
| `Retry` | transient (locked row, unreachable backend); mirrors the reference `RETRY` status |
| `DuplicateAccount` | unique-key violation on the account table |
| `UnknownBackend(name)` | unknown `<db><type>` string, reported verbatim as "Failed to create database of type: {name}" |
| `Storage(msg)` | anything else |

Backend selection is `StorageBackend` in `crates/db/src/config.rs` — `Xml`
(the reference default), `MySql` (the reference production backend) and
`PostgreSql` (this port's production backend) — chosen from the raw
`<db><type>` config string (`lib/db/db_config.cpp` is the cited origin).
`PostgresConnection` defaults to `localhost:5432`, database `bigworld`, user
`bigworld` and renders a libpq conninfo.

The stored record is `EntityRecord` (`crates/db/src/record.rs`): a
`type_id`, a `db_id` (or `PENDING_DATABASE_ID = -1`, `crates/foundation/src/ids.rs`)
and one `PropertyValue` per property **in the reference storage visit order**
(base-not-client, base-client, cell-client, cell-not-client, in declaration
order). The value model is shared with the definition layer — see
[Entity pipeline](/architecture/entity-pipeline).

The trait `EntityStorage` (`crates/db/src/storage.rs`) has **one** method:
`get_entity(&EntityKey) -> Result<Option<EntityRecord>, DbError>`, where
`Ok(None)` is the reference's "row missing" outcome. `EntityKey` pairs a type
id with a db id; its by-name leg is reserved for BW-09-01-04. Writes
(BW-09-01-02), deletes (BW-09-01-03), lookups (BW-09-01-04) and id
allocation (BW-09-01-05) do not exist on the trait yet; the in-tree
`MemoryStorage` is a `HashMap` that serves reads today and is documented as
the future `put_entity` seed.

## Startup selection

`DbApp` in `crates/server-runtime/src/dbapp.rs` mirrors the reference
`dbapp.cpp` startup contract: build the config from the raw `<db><type>`
string (`init_from_type_name`), fail on an unknown name with the reference
message, and report the opened engine as `"Database engine = {name}"`
(`dbapp.cpp:427` is the cited anchor). One honesty note: **the backend is not
constructed at startup** — `DbApp` holds a `DatabaseConfig` only; nothing
calls `PostgresStorage::connect` at boot, and `server-runtime` depends on
`db-postgres` as a *dev*-dependency (the backend-isolation proof in
`crates/server-runtime/tests/db_backend_isolation.rs` runs against a real
unreachable socket).

## The PostgreSQL backend

`crates/db-postgres` ports the reference MySQL storage semantics onto
PostgreSQL (`crates/db-postgres/src/lib.rs` states the mapping intent:
"REPLACE INTO becomes INSERT ... ON CONFLICT DO UPDATE"). The naming rules
(`crates/db-postgres/src/schema.rs`) are the reference `entity_type_mapping` /
`namer` pair: entity tables are `tbl_<TypeName>`, property columns
`sm_<prop>`, dots become `__`, and every identifier is double-quoted because
PostgreSQL folds unquoted identifiers to lower case.

Column types (`ColumnKind::sql_type`, `crates/db-postgres/src/schema.rs`):

| Meta kind | PostgreSQL type | Reference (MySQL) origin |
| --- | --- | --- |
| Int / Uint | `BIGINT` | `INT UNSIGNED` widened (no unsigned type; a bound check rejects u64 above `BIGINT`) |
| Float | `DOUBLE PRECISION` | `DOUBLE` |
| String | `TEXT` | `VARCHAR`/`CHAR` |
| Blob | `BYTEA` | `BLOB` / `VARBINARY` |
| Vector | `REAL[]` | packed binary |
| Sequence / FixedDict / MailBox | `JSONB` | opaque binary column |

Nested values travel as **tagged JSONB** (`crates/db-postgres/src/json.rs`):
every value is a one-field object whose key names the shape — `i` int, `u`
uint, `f` float (with `"nan"`/`"inf"`/`"-inf"` markers), `s` string, `b` blob
(hex), `v` vector, `q` sequence, `d` fixed-dict (`null` when unset, else
`[name, value]` pairs), `m` mailbox (`[id, ip, port, salt]`). Int and Uint
keep distinct tags because both fit `BIGINT` but are different meta types.

Writes are upserts: `put_entity` in `crates/db-postgres/src/store.rs` builds
`INSERT ... ON CONFLICT ("id") DO UPDATE SET col = EXCLUDED.col ...`, rejects
pending db ids, and maps a `UNIQUE_VIOLATION` to `DbError::DuplicateAccount`.
Nested placeholders bind text and cast — `CAST($n::text AS JSONB)` — because
an un-annotated cast makes PostgreSQL infer the parameter itself as `jsonb`.

The account table is the reference `bigworldLogOnMapping`
(`crates/db-postgres/src/accounts.rs`): `logOnName`/`password` as `BYTEA`
(raw bytes, the reference `VARBINARY`), `entityType`, `entityID`. Writes are
deliberately a plain `INSERT`, not an upsert — a duplicate log-on name must
surface as `DbError::DuplicateAccount` rather than silently move the name to
another entity.

Schema lifecycle (`crates/db-postgres/src/schema_init.rs`,
`dry_run.rs`, `upgrade.rs`) mirrors `server/tools/sync_db`:

- **Initialise**: the 11 reference fixed tables (`bigworldInfo`,
  `bigworldEntityTypes`, `bigworldLogOns`, `bigworldLogOnMapping`,
  `bigworldNewID`, `bigworldUsedIDs`, `bigworldGameTime`, `bigworldSpaces`,
  `bigworldSpaceData`, `bigworldSecondaryDatabases`,
  `bigworldEntityDefsChecksum`), 2 indexes, and a version seed row —
  `CURRENT_VERSION = 9` (`DBAPP_CURRENT_VERSION`, reference `versions.hpp`).
- **Dry-run**: `plan_missing_schema` probes `pg_tables`/`pg_indexes` and
  renders `DRYRUN:\t{statement};` lines (the reference
  `mysql_dry_run_wrapper.cpp` format); `is_read_only_statement` admits only
  plain `SELECT`/`SHOW`/`EXPLAIN`. The plan of an up-to-date database is
  empty.
- **Upgrade**: `plan_schema_update` emits per-column
  `ALTER TABLE ... ADD COLUMN ... NOT NULL DEFAULT <neutral>` statements with
  type-appropriate neutral values (0, `0.0`, `''`, empty array, `{"d":null}`);
  existing tables are never dropped or rewritten. The old-sample upgrade path
  (old rows stay readable, new columns get neutral values, second plan empty)
  is proven by tests against a real PostgreSQL.

## The contract seam, stated plainly

`PostgresStorage` does **not** implement the `db::EntityStorage` trait: the
trait takes `&self` and no schema, while the backend needs `&mut self` (the
`postgres::Client`) and an `EntitySchema`. Today the trait's only implementor
is `MemoryStorage`; the account methods are inherent to `PostgresStorage`.
Reconciling that seam is part of the remaining BW-09 work — until then the
"contract" and the real backend speak different interfaces.

## What does not exist yet

Verified absences, each tracked in `todo.md`:

| Missing | `todo.md` |
| --- | --- |
| Entity write (`put_entity`) | BW-09-01-02 |
| Entity delete | BW-09-01-03 |
| Lookup by name / `getDatabaseIDFromName` | BW-09-01-04 |
| DBID allocation and online-state maintenance | BW-09-01-05, BW-09-06-04 |
| Transactions, commit/rollback (no `BEGIN`/transaction handle anywhere; `DbError::Retry` is never constructed) | BW-09-06-06, BW-09-06-07 |
| Concurrent-write conflict handling | BW-09-06-08 |
| Reconnect / relogin after database loss | BW-09-06-09, BW-09-06-10 |
| MySQL backend | BW-09-02 |
| XML backend, SQLite secondary store, archive merge | BW-09-03 |
| Billing and auth extensions | BW-09-04 |
| Operational tools (`sync_db`, `transfer_db`, `snapshot_helper`, …) | BW-09-05 |
| MySQL import, backup/restore, post-import DBID calibration | BW-09-07-04..07 |
| auto-load and secondary-DB hookup | BW-09-06-11 |

`bigworldNewID` / `bigworldUsedIDs` exist as DDL strings and nothing reads or
writes them; a `-1` id is "not yet allocated" and must never be written as a
real id (see the id-space section of
[Database migration and recovery](/operations/database)).

## Testing

`crates/db` carries 11 unit tests. `crates/db-postgres` carries 36 unit tests
(JSONB codec, schema naming, store upserts, schema init, dry-run, accounts,
upgrade) plus 18 integration tests in `crates/db-postgres/tests/` that run
against a real PostgreSQL when reachable: each case creates a private
`boundless_pg_test_{pid}_{n}` database and drops it afterwards, gated through
`BOUNDLESS_PG_URL` → `CI_STORE_PG_*` → the local unix socket, and skips
cleanly when no server exists (`crates/db-postgres/tests/round_trip.rs` holds
the shared pattern).

## State

| Piece | State |
| --- | --- |
| Error vocabulary, backend selection, record shape | Implemented (`crates/db/src/`) |
| `DbApp` startup selection and engine report | Implemented (`crates/server-runtime/src/dbapp.rs`) |
| Entity schema mapping, JSONB codec, upserts | Implemented (`crates/db-postgres/src/schema.rs`, `json.rs`, `store.rs`) |
| Account table with uniqueness contract | Implemented (`accounts.rs`) |
| Schema init, dry-run, incremental upgrade | Implemented (`schema_init.rs`, `dry_run.rs`, `upgrade.rs`) |
| Entity reads via the trait (`get_entity`) | Implemented (`storage.rs`, `MemoryStorage`) |
| Entity writes/deletes/lookups, DBID allocation | **Not implemented** (BW-09-01-02..05, BW-09-06-04) |
| Transactions, conflicts, reconnect, relogin | **Not implemented** (BW-09-06-06..10) |
| MySQL / XML / SQLite backends | **Not implemented** (BW-09-02, BW-09-03) |
| Operational tooling and migration | **Not implemented** (BW-09-05, BW-09-07-04..07; plan in `/operations/database`) |
