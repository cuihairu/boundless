# Compatibility matrix

The matrix below records the versions and targets Boundless must stay compatible
with, and which of them the working tree currently satisfies. Every row is
traceable to a `todo.md` id or a source file.

## Source baseline

| Axis | Value | Source |
| --- | --- | --- |
| Reference repository | BigWorld | `todo.md` reference repository link |
| Source commit baseline | `4657a17cf10d55e2fbebf258f96da96a3dbe0c22` | `todo.md` preamble |
| Upstream server target | Linux x86-64, C++23, embedded CPython 3.13.16 | `todo.md` preamble |
| Boundless toolchain | Rust **1.100.0** | `rust-toolchain.toml` |
| Cargo edition | 2024 | root `Cargo.toml` |

The toolchain is pinned in `rust-toolchain.toml` so local builds and CI resolve
the same compiler version. Release builds are optimised with `opt-level = 3`
and keep `debug = true` for post-incident symbolication.

## Platform targets

First-batch build targets from BW-15:

| Target | Status | Tracked by |
| --- | --- | --- |
| Linux x86-64 | Interface work started; `crates/platform/src/clock.rs` only | BW-15-02 |
| Windows x86-64 | Not started | BW-15-03 |
| macOS arm64 | Not started | BW-15-04 |

Of the seven platform-interface items (event polling, monotonic clock, process
control, file locking, memory mapping, path casing, network addresses), only
**monotonic clock** is implemented and verified: `crates/platform/src/clock.rs`.
The platform interface is intentionally kept in one crate so platform-specific
code stays out of the role crates.

## Persistence backends

| Backend | Role | Tracked by |
| --- | --- | --- |
| MySQL | historical baseline | BW-09-02 |
| PostgreSQL | target production backend | BW-09-06, BW-09-07 |
| XML | main store (reference default) | BW-09-03 |
| SQLite | secondary store | BW-09-03 |

**None of the four backends is implemented.** No SQL dependency, `.sql` file or
migration directory exists in the tree. What does exist is the id vocabulary:
`DatabaseId = i64` and the `PENDING_DATABASE_ID = -1` sentinel, in
`crates/foundation/src/ids.rs`. The operations section carries the target
database migration and recovery procedure.

## Client and engine targets

| Target | Tracked by | Status |
| --- | --- | --- |
| Python client SDK | BW-21-02-05 | Not implemented |
| Unity | BW-21-03 | Not implemented |
| Unreal | BW-21-04 | Not implemented |
| Godot | BW-21-05 | Not implemented |
| Android / iOS interop tests | BW-14-02 | Not implemented |

The wire contract the SDKs will build against is already present: Mercury
packet framing in `crates/mercury/src/packet.rs` (1472-byte payload cap, 2-byte
header, 27-byte worst-case footer, flag layout), and the entity type id space in
`crates/entitydef/src/entities.rs` (`FIRST_ENTITY_TYPE_ID = 0`,
`INVALID_ENTITY_TYPE_ID = 0xFFFF`, ids assigned in load order).

## Cross-version rules

- **Protocol.** Boundless clients and the reference server must interoperate
  during the migration window, so framing constants are copied from
  `lib/network/packet.hpp` rather than re-derived.
- **Script API.** Entity scripts are written against the reference CPython API;
  the embedded interpreter target is CPython 3.13.16 to match upstream.
- **Resources.** Resource paths are resolved through `BWResource`, so the same
  resource tree must load on every platform; separator is `:` on unix
  (`BW_RES_PATH_SEPARATOR`).

## Workspace boundaries

| Crate | Public modules today |
| --- | --- |
| `foundation` | `ids`, `math` (angle, boundbox, matrix, quaternion, vector), `stream`, `time` |
| `platform` | `clock` |
| `resources` | `base64`, `binary`, `bwresource`, `data_section_cache`, `datasection`, `file_system`, `multi_file_system`, `packed`, `special_chars`, `zip` |
| `mercury` | `packet` |
| `contracts` | *(none — placeholder only)* |
| `entitydef` | entity type registry |
| `python-host` | *(none — surface reserved)* |
| `server-runtime` | `app`, `config`, `dispatcher`, `server_app`, `shutdown`, `signals`, `task_manager`, `tick`, `timers` |

Only `foundation`, `resources`, `server-runtime` and `libc` are enabled as
workspace dependencies in the root `Cargo.toml`; `platform`, `mercury`,
`contracts`, `entitydef` and `python-host` are commented out and get re-added
with their first consumer.