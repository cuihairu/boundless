# Entity pipeline

An entity type in BigWorld is defined twice: once as a `.def` XML contract,
once as a Python class. This page follows the chain as the repo implements it
today — `.def` parsing in `crates/entitydef`, the CPython host in
`crates/python-host` — and states plainly which link is still missing: no
runtime crate connects the two, so scripted entities cannot be created yet.

## Paths and file layout

The script-tree constants live in `crates/entitydef/src/constants.rs` (a port
of the reference `EntityDef::Constants`): `scripts/entities.xml` lists the
entity types, `scripts/entity_defs/` holds the `.def` files,
`scripts/entity_defs/alias.xml` the type aliases, `scripts/service_defs/` the
service definitions, `scripts/component_defs/` the component definitions, and
`scripts/base/`, `scripts/cell/`, `scripts/client/`, `scripts/service/` the
Python sources per domain. `defs_dir(is_service)` picks the definitions
directory and `component_def_file(type)` builds a component path.

## Parsing a `.def` file

`EntityDescription::parse` in `crates/entitydef/src/entity_description.rs`
opens `{defs_dir}/{name}.def`, recurses on `<Parent>`, reads `<ClientName>` and
`<Distribution>`, then walks the interface surface:

- `<Implements>` loads interface definitions from
  `scripts/entity_defs/interfaces/*.def`.
- `<Properties>`, `<BaseMethods>`, `<CellMethods>`, `<ClientMethods>` fill the
  property and method tables.
- `<TempProperties>` records names only; `<Components>` loads
  `scripts/component_defs/{Type}.def` and rejects duplicate component names.
- `<Volatile>`, `<AppealRadius>`, `<LoDLevels>`, `ExplicitDatabaseID`,
  `Persistent` and the AOI flags round out the per-entity knobs
  (`crates/entitydef/src/entity_description.rs`, `<Volatile>` parsed in
  `crates/entitydef/src/volatile.rs`, LoD in `crates/entitydef/src/lod.rs`).

Inheritance follows the reference rules: an override reuses the original
property slot ("property Child.hp is being overridden."), and a conflicting
parent/interface method signature fails ("Method 'jump' already defined with a
different signature"). Services are a separate flavour — `<Methods>` only, and
`<Properties>` is rejected with the reference warning "Services do not support
properties."

## Client/server split

Each entity decides which domains it exists on. `<Distribution>` carries
`<Client>`, `<Base>` and `<Cell>` tags; once the section exists a missing tag
means `false`, not "unspecified"
(`crates/entitydef/src/entity_description.rs`). When `<Distribution>` is
absent, a `HasScriptDecider` probes for the Python source in `scripts/cell/`,
`scripts/base/` and `scripts/client/` to decide — the reference's
has-script heuristic.

Property visibility is a flag vocabulary, not a per-target list:
`crates/entitydef/src/flags.rs` maps the nine `.def` names to bit masks:

| `.def` flag | Bits |
| --- | --- |
| `CELL_PRIVATE` | none |
| `CELL_PUBLIC` | `GHOSTED` |
| `OTHER_CLIENTS` | `GHOSTED \| OTHER_CLIENT` |
| `OWN_CLIENT` | `OWN_CLIENT` |
| `BASE` | `BASE` |
| `BASE_AND_CLIENT` | `OWN_CLIENT \| BASE` |
| `CELL_PUBLIC_AND_OWN` | `GHOSTED \| OWN_CLIENT` |
| `ALL_CLIENTS` | `GHOSTED \| OTHER_CLIENT \| OWN_CLIENT` |
| `EDITOR_ONLY` | `EDITOR_ONLY` |

Client safety is a property of the *type*:
`crates/entitydef/src/data_type.rs` computes it recursively — the `PYTHON`
meta type is unsafe, `MAILBOX` is unusable, and a `CLIENT_UNSAFE` property on
an exposed path warns ("potential security hole") while `CLIENT_UNUSABLE` is a
hard error.

## Property definitions

`DataDescription::parse` in `crates/entitydef/src/data_description.rs` reads
`<Type>`, `<Flags>`, `<Persistent>`, `<Identifier>`, `<Indexed>`, `<Default>`,
`<ExposedForReplay>` and `<DatabaseLength>`. An `<Identifier>` implies the ID
flag plus indexed+unique; `<Indexed>` requires persistence. The meta-type
system is in `crates/entitydef/src/data_type.rs`: every registered kind
(`INT8..UINT64`, `FLOAT32/64`, `STRING`, `BLOB`, `UNICODE_STRING`, `UDO_REF`,
`MAILBOX`, `PYTHON`, `VECTOR2/3/4`, `ARRAY`, `TUPLE`, `FIXED_DICT`, `CLASS`,
`USER_TYPE`) knows its stream size, its MD5-digest layout and its client
safety. Aliases come from `scripts/entity_defs/alias.xml` via a `TypeContext`
that warns "Couldn't open aliases file 'scripts/entity_defs/alias.xml'" when
missing.

The concrete value model — including the deliberate absence of a BOOL variant,
since the reference has no BOOL meta type — is
`crates/entitydef/src/value.rs` with a wire layout and a `PropertyValue` enum
shared with persistence ([Database model](/architecture/database-model)
reuses it for stored records).

## Methods and exposed messages

`crates/entitydef/src/method.rs` implements `MethodDescription`: `<Args>` (or
old-style `<Arg>` children), `<ReturnValues>`, `<DetailDistance>` (stored
squared) and `<Exposed>`. Exposure rules match the reference:

- `<Exposed>` on a client method is an error ("Unable to use `<Exposed>` tag in
  client method {}.").
- `ALL_CLIENTS` is legal on cell methods only, `OWN_CLIENT` narrows to the
  owning client, and a bare `<Exposed>` means both.
- Exposed methods get message ids through the sub-id slot scheme in
  `ExposedMessageRange` (`msgIDFromExposedID`, `needsSubID`, `numSlots`);
  a client type carries at most 256 top-level properties and
  `256 * num_slots` methods (`crates/entitydef/src/entity_map.rs`).
- Stream size accounts for the 4-byte `EntityID` prefix of an exposed cell
  method called server-to-client.

## Digests, type ids and the factory

`crates/entitydef/src/digest.rs` wraps MD5 with the reference's exact append
semantics (NUL-terminated strings, bool bytes, little-endian i32) and quotes
the sum as uppercase hex. The digest content is pinned in
`crates/entitydef/src/entity_description.rs`: entity name, then client-server
properties (bandwidth-sorted full indexes — fixed-size ascending, then
variable-size by header width), then all client methods, then base/cell
methods that still carry a legacy exposed index. Return values are deliberately
not digested.

`EntityDescriptionMap::parse` in `crates/entitydef/src/entity_map.rs` reads
`scripts/services.xml`, separates `<ClientServerEntities>` from
`<ServerOnlyEntities>`, and stamps `EntityTypeID`s (`u16`,
`INVALID_ENTITY_TYPE_ID = u16::MAX`) in load order — client-server entities
first, server-only next, services last. The whole map is built by
`EntityDescriptionMapFactory::create(paths)` (the `lib/bwentity` analogue,
exercised in `crates/entitydef/tests/bwentity_load.rs`).

## The process_defs tool and its oracles

`crates/entitydef/src/bin/process_defs.rs` is the port of the reference
`tools/process_defs`: `crates/entitydef/src/cli.rs` reproduces the stock
`--help` byte for byte and `crates/entitydef/src/dump.rs` writes the JSON dump
with `sort_keys=True, indent=1` formatting. Correctness is differential: the
tests in `crates/entitydef/tests/differential.rs` compare in-repo fixtures
under `docs/review/fixtures/` against recorded reference oracles in
`docs/review/reference/*_entitydef.json`, and
`crates/entitydef/tests/diagnostics.rs` asserts the stock binary's error
output line for a broken def tree.

## The Python host

`crates/python-host` embeds CPython 3.13 through a hand-declared C API
(`crates/python-host/src/capi.rs`); there is no bindgen or pyo3. The build
probe in `crates/python-host/build.rs` asks `BOUNDLESS_PYTHON` then `python3`
for sysconfig paths and links `libpython3.13` dynamically.

`Interpreter::init` in `crates/python-host/src/interpreter.rs` follows the
reference `Script::init` order: frozen/site/environment flags, `Py_Initialize`,
GIL thread state, stdout/stderr swapped for the logging bridge
(`crates/python-host/src/output_writer.rs`), argv and `sys.path` from the res
paths (`crates/python-host/src/import_paths.rs`), init-time jobs
(`crates/python-host/src/init_time_job.rs`), GC disabled, then the `Math`,
`ResMgr` and `BigWorld` modules installed, a module snapshot taken (the
reference's init-time-modules list), `bw_site` imported and the exception hook
installed. One interpreter per process; `fini` drains GC, blanks `__main__`
and finalises.

The `BigWorld` module surface created by `create_big_world_module`
(`crates/python-host/src/interpreter.rs`) is:

| Face | Domain | Source |
| --- | --- | --- |
| `component`, `serverMode = "standalone"` | both | `interpreter.rs` |
| `addTimer` / `delTimer` / `addFrequentTask` | server | `crates/python-host/src/timers.rs` |
| `callback` / `cancelCallback` | client | `timers.rs` |
| `addEventListener` / `removeEventListener` | server | `crates/python-host/src/script_events.rs` |
| `fetchURL` / `URLResponse` | both | `crates/python-host/src/urlrequest.rs` |

`Math` (`crates/python-host/src/math/`) exposes `Vector2/3/4`, `Matrix`,
`Plane` and the provider classes with the reference's quirks kept (e.g.
`lookAt` writing `m[3][0]` twice). `ResMgr`
(`crates/python-host/src/resmgr.rs`) is the script face of the resource
manager over `crates/resources` — see
[Resource system](/architecture/resource-system). The GIL model
(`crates/python-host/src/gil.rs`) preserves the reference behaviour of never
releasing the lock around a script call, with a cross-thread callback queue
drained by the GIL-owning thread.

`crates/python-host/src/manifest.rs` is the self-audit table: every script
name is recorded with a scope, kind and status (`Ported` / `Covered` /
`Pending`), and `manifest::check()` verifies the Ported rows against a live
interpreter. The pending list is explicit: the watcher family
(`setWatcher`, `getWatcher`, …), the logging functions, `BWConfig` readers,
network/chunk/terrain accessors — all recorded as `Pending`, none claimed as
working.

## Entity creation: the missing link

The first two thirds of the pipeline are delivered (definitions **BW-04**,
scripting host **BW-03**); the third is absent:

- No runtime crate depends on `python-host`. The workspace lists it
  (`Cargo.toml`), but only `crates/test-support` (a dev utility) and the crate
  itself reference it; `crates/python-module/build.rs` reuses its build probe
  only.
- There is no `EntityType` registry and no `createEntity` anywhere in
  `crates/python-host` (`crates/python-host/src/manifest.rs` records none).
- `entitydef` is consumed only by the persistence crates:
  `crates/db/src/record.rs` uses `entitydef::value::PropertyValue` and
  `crates/db-postgres` builds schemas from it. No cell/base entity write path
  exists, and there is no client/server entity split at runtime.
- The nearest things in `crates/server-runtime` are message-level:
  `witness.rs` tracks `createEntity` *messages* and `billing.rs` resolves an
  entity type by name for unknown users — neither creates a scripted entity.
- The hot-reload machinery is modelled as data: `reload.rs` holds the
  `swap_script_class` / `swap_in_modules` / `migrate_instance` primitives and
  `reload_plan.rs` pins the 12-step plan, but both documents record that there
  is no entity shell to drive them (`crates/python-host/src/reload.rs`,
  `crates/python-host/src/reload_plan.rs`).

Script-level exposure rules are already visible to authors — see
[Entity scripting](/guide/entity-authoring) for the writing side and
[Script and SDK API](/reference/api) for the module surface.

## State

| Piece | State |
| --- | --- |
| `.def` parsing, inheritance, interfaces, components | Implemented (`crates/entitydef/src/entity_description.rs`) |
| Distribution and has-script decision | Implemented (`entity_description.rs`, `HasScriptDecider`) |
| Property flags, client safety, identifiers | Implemented (`crates/entitydef/src/flags.rs`, `data_type.rs`) |
| Methods, exposure, exposed message ranges | Implemented (`crates/entitydef/src/method.rs`) |
| Digests, type ids, services.xml map | Implemented (`digest.rs`, `entity_map.rs`) |
| `process_defs` dump + differential oracles | Implemented (`cli.rs`, `dump.rs`, `tests/differential.rs`) |
| CPython 3.13 host, `BigWorld`/`Math`/`ResMgr` modules | Implemented (`crates/python-host/src/`) |
| Script API manifest self-check | Implemented (`crates/python-host/src/manifest.rs`) |
| Watcher/log/BWConfig script faces | **Not implemented** (recorded `Pending` in `manifest.rs`; `BW-16-03-02`) |
| Entity types at runtime, `createEntity`, script classes on entities | **Not implemented** — no crate links `entitydef` to `python-host` |
| Hot reload (`reloadScript`) | Modelled as data only (`reload.rs`, `reload_plan.rs`) |
