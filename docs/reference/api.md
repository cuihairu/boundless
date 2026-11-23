# Script and SDK API reference

This page lists the public API the repository actually exposes today, crate by
crate, and the executable example that proves each one. "Executable example"
means a `cargo test` invocation you can run in a checkout.

::: info Reading this page
Every item below is a `pub` declaration in the named crate, so the tables and
the source agree. `entitydef` carries the type registry only — property and
method tables are BW-04-02. See
[Compatibility matrix](/architecture/compatibility-matrix) for what is still
open in each crate.
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

Operating-system primitives. Four modules today, three of them platform-gated:

| Module | Items | Meaning |
| --- | --- | --- |
| `clock` | `monotonic_now_micros() -> Stamp`, `wall_now_micros() -> Stamp` | clocks in microseconds; `Stamp` comes from `foundation::time` |
| `interface` | `name_index()`, `flags(fd, name)`, `address(fd, name)`, `IFNAMSIZ`, `IFF_*`, `SIOCGIF*` | `ioctl`-based network-interface enumeration |
| `socket` (unix) | socket option constants (`SO_*`, `SOCK_DGRAM`, …) and the helpers `Endpoint` uses | socket creation and buffer-size application |
| `poller` (linux) | `create(expected_size)`, `ctl(epfd, op, fd, events)`, `EPOLLIN/OUT/ERR/HUP` | raw `epoll` primitives behind the Mercury event poller |

`clock` is the only piece accepted against its three-platform criterion
(BW-15-01-02); `interface`, `socket` and `poller` landed with the BW-02-01
transport work and are Linux-first (`unix` / `linux` `cfg` gates), with their
BW-15-01 acceptance items still open.

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

Mercury reliable-UDP transport: framing constants plus the BW-02-01 endpoint and
dispatch layer.

| Item | Value |
| --- | --- |
| `PACKET_MAX_SIZE` | 1472 |
| `HEADER_SIZE` | 2 |
| `RESERVED_FOOTER_SIZE` | 27 |
| `MAX_CAPACITY` | 1443 |
| `flags::*` | `HAS_REQUESTS`, `HAS_PIGGYBACKS`, `HAS_ACKS`, `ON_CHANNEL`, `IS_RELIABLE`, `IS_FRAGMENT`, `HAS_SEQUENCE_NUMBER`, … |
| `REPLY_MESSAGE_ID` | `0xFF` |

All multi-byte wire fields are little-endian.

The transport layer around the packet, delivered by BW-02-01-01..04:

| Module | Items | Meaning |
| --- | --- | --- |
| `endpoint` | `Address`, `Endpoint`, `NO_SOCKET`, `INADDR_ANY`, `BROADCAST` | address type and the UDP socket wrapper |
| `misc` | `Reason` (`Success`, `TimerExpired`, `NoSuchPort`, `GeneralNetwork`, `CorruptedPacket`, …) | the reference's `Mercury::Reason` enum |
| `network_utils` | `MIN_SND_SKT_BUF_SIZE = 1_048_576`, `MIN_RCV_SKT_BUF_SIZE = 16_777_216`, `buffer_sizes()` | socket buffer sizing against kernel maxima |
| `network_stats` | `get_queue_sizes(ep, tx, rx)` | the `/proc/net/udp` reader behind `Endpoint::getQueueSizes`, 400 ms cache |
| `network_interface` (linux) | `NetworkInterface`, `InterfaceType`, `ChannelFinder` | one bound interface; dispatches events to the right channel finder |
| `packet_receiver` (linux) | `PacketReceiver`, `LogReporter`, `NoSuchPortCallback` | drains the socket into registered receivers |
| `poller` (linux) | `EventPoller`, `InputHandlerEntry` | epoll-backed registration, wake-up and input/error notification |

The message layer, delivered by BW-02-02:

| Module | Items | Meaning |
| --- | --- | --- |
| `interfaces` | `InputMessageHandler`, `ReplyMessageHandler` | the reference's handler traits; handlers are matched by identity |
| `interface_element` | `FIXED_LENGTH_MESSAGE`, `VARIABLE_LENGTH_MESSAGE`, `CALLBACK_LENGTH_MESSAGE`, `REPLY_MESSAGE_IDENTIFIER = 0xFF` | message length kinds and the reply sentinel |
| `interface_table` | `InterfaceTable::serve / element / handler / msg_name` | the per-interface message registry |
| `interface_minder` | `InterfaceMinder::add / add_range`, `MAX_MESSAGE_ID = 254` | id allocation across ranges |
| `request_manager` | `RequestManager::add_reply_order / cancel_requests_for_channel / cancel_requests_for_handler`, `DEFAULT_REQUEST_TIMEOUT = 5_000_000` | reply correlation, timeouts and cancellation |
| `reply_order` | `ReplyOrder` | per-request reply bookkeeping |
| `message_framing` | `REQUEST_PREAMBLE_SIZE`, body offset and expand/compress length | the wire preamble and length encoding |

The Channel/Bundle layer (`BW-02-03`) is partial: `BW-02-03-01..03` (packet and
bundle codec, channel registration and lifecycle, acks and the send window) are
in; `BW-02-03-04..07` (retransmission timers, out-of-order filtering,
fragmentation reassembly, bidirectional mixed runs) are not yet.

| Module | Items | Meaning |
| --- | --- | --- |
| `udp_channel` | `UdpChannel::new`, `Traits`, `EXTERNAL_CHANNEL_SIZE = 256`, `INTERNAL_CHANNEL_SIZE = 4096`, `INDEXED_CHANNEL_SIZE = 512` | channel registration, lifecycle and the send window |
| `channel_map` | `ChannelMap::add / remove / find / iter` | address-keyed channel table |
| `unacked_packet` | `UnackedPacket`, sequence and resend bookkeeping | one outstanding packet in the window |
| `udp_bundle` | `UdpBundle`, `ReliableType`, `prepare_packets` | bundle write side across a packet chain, footers owed |
| `bundle_dispatch` | `dispatch_messages` | walks an incoming bundle into handler calls |
| `condemned_channels` | `CondemnedChannels::add / delete_finished_channels` | dead-channel reaping |
| `channel_listener` | `ChannelListener` | per-channel event hook |

```bash
cargo test -p mercury
```

## `entitydef`

Definition compilation, data type streams and visibility (BW-04).

| Item | Meaning |
| --- | --- |
| `EntityTypeID = u16`, `INVALID_ENTITY_TYPE_ID = 0xFFFF` | the wire type id space; ids are stamped in load order from `0` |
| `EntityDescription` | one entity definition: `name`, `index`/`client_index`, `can_be_on_{cell,base,client}`, `is_service`, `is_persistent` |
| `EntityDescriptionMap::entity_description / name_to_index` | the load-order map over all entities and services |
| `MessageRanges` | the exposed message id ranges |
| `DataType`, `Arc<DataType>` | the data type tree (scalars, `ARRAY`/`TUPLE`, `FIXED_DICT`, `USER_TYPE`, `MAILBOX`) |
| `MemberDescription` | a property/member table entry |
| `MethodDescription`, `EntityMethodDescriptions`, `ExposedMessageRange` | method tables and their exposed ranges |
| `PropertyChange`, `SinglePropertyChange`, `SlicePropertyChange` | nested property change encoding |
| definition digest (`digest.rs`) | the byte-identical definition digest over the loaded set |

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

## `contracts`

The bwmachined wire contracts, ported from `lib/network/machine_guard.hpp`
(BW-16-01-02): one shared header plus the message and address types every role
sends to `bwmachined`.

| Item | Meaning |
| --- | --- |
| `MERCURY_INTERFACE_VERSION = 1` | Mercury interface version mirrored from the reference |
| `message_id::*` | 15 tool-to-machined ids, `WHOLE_MACHINE` … `HIGH_PRECISION_MACHINE`, plus `MACHINED_ANNOUNCE = 64` |
| `flags::*`, `process_type::*`, `process_param::*`, `category_name()` | the header flag, process-type, process-param and category vocabularies |
| `Header` | `u8 message, u8 flags, u16 seq`, little endian |
| `ProcessMessage`, `ListenerMessage`, `SignalMessage` | the wire messages; each has `write(&self, &mut BinaryStream)` and `read(&mut BinaryStream)` |
| `ProcessMessage: Default` | the empty process record every other variant grows from |

```bash
cargo test -p contracts
```

No role consumes them yet: `contracts` is still commented out of the root
`[workspace.dependencies]`, and the only mention in the tree is the
`test-support` case registry naming the crate's test names.

## `python-host`

The CPython embedding layer ported from the reference `lib/pyscript/` and
`lib/script/` (BW-03). It links no Python dependency: `capi`
declares the interpreter ABI by hand.

| Module | Items | Meaning |
| --- | --- | --- |
| `interpreter` | `Interpreter::init(&InitConfig)`, `run_simple_string`, `init_time_modules`, `fini`, `is_initialized`, `is_finalized` | one-process interpreter lifecycle; keeps the reference's pre-init globals instead of migrating to `PyConfig` |
| `import_paths` | `ImportPaths`, `bw_build_platform()`, `platform_mismatch()`, `role_paths()` | the role-specific Python import path and its platform stamp |
| `init_time_job` | `add_init_time_job(rung, job)`, `run_init_time_jobs`, `add_fini_time_job(_at)`, `run_fini_time_jobs` | init/fini job rungs, the ordering primitive the reference relies on |
| `output_writer` | `ScriptOutputHook`, `add_hook`/`remove_hook`, `set_log_sink`, `LogSink`, `handle_write` | `sys.stdout`/`sys.stderr` redirected into script logging |
| `pyref` | `PyRef` | refcounted object bridge; not `Send`, valid only on the thread that holds the GIL |
| `capi` | `PyObject`, `PyTypeObject`, `PySsize`, `py_none/py_true/py_false`, `is_finalized` | the hand-written CPython ABI surface |
| `binding` | `TypeSpec`, `BindError` | attribute and method binding specs for native types |
| `wrapper` | `WrapError`, `release(id)`, `reset()` | the native wrapper identity cache |
| `sequence` | `SequenceState` | container proxy state shared with script |
| `keepalive` | `PinError`, `unpin(id)`, `is_pinned(id)`, `pinned_count()` | cross-language keepalive pins |
| `gil` | `has_context()`, `init_thread()` | GIL attach/detach and cross-thread callback guards |
| `manifest` | `Scope`, `Kind`, `Status` | the public Python export manifest (BW-03-03-01) |

```bash
cargo test -p python-host
```

The public script modules and role script entry points land with the
remaining BW-03 items (BW-03-03-02..06, BW-03-04).

## Running everything

```bash
cargo build -p boundless-server   # the only binary
cargo test --workspace            # every unit test in the tree
```

There is no `tests/` directory and no integration test harness yet; all tests
are inline `#[cfg(test)]` modules in the crates above. The planned internal test
framework (fixtures, virtual time, multi-process topologies) is **BW-18**. See
[Testing and release](/operations/testing-and-release) for every reproducible
command.
