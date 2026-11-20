# Script and SDK API reference

This page lists the public API the repository actually exposes today, crate by
crate, and the executable example that proves each one. "Executable example"
means a `cargo test` invocation you can run in a checkout.

::: info Reading this page
`contracts` and `python-host` appear below with *no* public items — that is
accurate, not a gap in the docs. Both are placeholder crates. See
[Compatibility matrix](/architecture/compatibility-matrix) for their status.
:::

## `foundation`

Ids, math and streaming primitives shared by everything else.

| Item | Meaning |
| --- | --- |
| `EntityId = i32`, `NULL_ENTITY_ID = 0`, `FIRST_ENTITY_ID = 1` | wire entity ids |
| `FIRST_LOCAL_ENTITY_ID = 0x7F00_0000` | start of the client-local id range |
| `SpaceId = i32`, `NULL_SPACE_ID = 0` | spaces |
| `EntityTypeId = u16`, `INVALID_ENTITY_TYPE_ID = u16::MAX` | entity type ids |
| `DatabaseId = i64`, `PENDING_DATABASE_ID = -1` | persistence ids |
| `Vector2`, `Vector3`, `Vector4`, `Matrix`, `Quaternion` | math |
| `QUATERNION_ALMOST_EQUAL_EPSILON = 0.0004` | comparison tolerance |
| `StreamError`, `STREAM_ERROR_BUFFER_MAX = 2048` | stream errors |
| `MAX_PACKED_INT`, `PACKED_INT_EXTEND = 0xFF` | packed int encoding |

```bash
cargo test -p foundation
```

## `platform`

Only the monotonic clock is public today.

| Item | Meaning |
| --- | --- |
| `monotonic_now_micros() -> Stamp` | monotonic clock, microseconds |
| `wall_now_micros() -> Stamp` | wall clock, microseconds |

`Stamp` itself comes from `foundation::time` — the functions above are its only
consumers in this crate.

```bash
cargo test -p platform
```

## `resources`

`BWResource` and the resource container formats.

| Item | Meaning |
| --- | --- |
| `BwResource`, `BinaryBlock` | resource cache and section access |
| `DIR_ENTRY_SIZE = 46`, `DIR_FOOTER_SIZE = 22` | zip directory geometry |
| `LOCAL_HEADER_SIGNATURE`, `DIR_ENTRY_SIGNATURE`, `DIR_FOOTER_SIGNATURE` | zip magic numbers |
| `format_path`, `format_search_path`, `clean_slashes`, `path_is_relative`, `expand`, `reduce` | path handling |

```bash
cargo test -p resources
```

## `mercury`

Mercury reliable-UDP framing constants.

| Item | Value |
| --- | --- |
| `PACKET_MAX_SIZE` | 1472 |
| `HEADER_SIZE` | 2 |
| `RESERVED_FOOTER_SIZE` | 27 |
| `MAX_CAPACITY` | 1443 |
| `flags::*` | `HAS_REQUESTS`, `HAS_PIGGYBACKS`, `HAS_ACKS`, `ON_CHANNEL`, `IS_RELIABLE`, `IS_FRAGMENT`, `HAS_SEQUENCE_NUMBER`, … |
| `REPLY_MESSAGE_ID` | `0xFF` |

All multi-byte wire fields are little-endian.

```bash
cargo test -p mercury
```

## `entitydef`

| Item | Meaning |
| --- | --- |
| `FIRST_ENTITY_TYPE_ID = 0` | ids are assigned in load order |
| `INVALID_ENTITY_TYPE_ID = 0xFFFF` | "no type" on the wire |
| `EntityDescription { name, type_id }` | one entity definition |
| `EntityTypeRegistry::new / register / lookup / name_of` | the load-order registry |

Property and method tables are not there yet — they are BW-04-02.

```bash
cargo test -p entitydef
```

## `server-runtime`

The largest public surface: process bootstrap, configuration, tick, timers and
signals.

| Item | Meaning |
| --- | --- |
| `app::KNOWN_ROLES`, `app::run`, `app::AppError` | role validation and startup |
| `config::BwConfig` | the `bw.xml` chain and `+key value` overrides |
| `server_app::ServerAppConfig`, `DEFAULT_GAME_UPDATE_HERTZ = 10`, `DEFAULT_PERSONALITY_NAME = "BWPersonality"` | app options and defaults |
| `server_app::EXIT_SUCCESS = 0`, `EXIT_FAILURE = 1` | exit codes |
| `shutdown::ShutDownStage`, `shutdown::ShutdownState`, `GameTime = u32` | staged shutdown |
| `task_manager::TaskManager`, `task_manager::TaskPtr`, `task_manager::BackgroundTask` | foreground/background task lists |
| `signals::SignalProcessor`, `signals::Set`, `signals::Blocker`, `signals::SIGNAL_NAMES`, `SIGMIN = 1`, `SIGMAX` | signal handling |
| `tick::Updatables`, `tick::TickState`, `timers::TimeQueueT`, `TimerHandle`, `TimerHandler` | game tick and event timers |
| `dispatcher::Dispatcher`, `dispatcher::FrequentTaskFn` | main-thread dispatch |

```bash
cargo test -p server-runtime
```

## `contracts` and `python-host`

No public items. `contracts/src/lib.rs` carries only the module docstring saying
callers should depend on it instead of on a peer service crate; `python-host`
reserves its surface for the CPython host. Both are documented as
not-implemented on the pages that use them.

## Running everything

```bash
cargo build -p boundless-server   # the only binary
cargo test --workspace            # every unit test in the tree
```

There is no `tests/` directory and no integration test harness yet; all tests
are inline `#[cfg(test)]` modules in the crates above. The operations section
carries the testing and release page these commands feed into.