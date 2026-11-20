# Client SDK

The client SDK gives a game or a test client login, reconnection, entity
create/destroy, property sync, RPC, movement input and space events. This page
covers what the SDK must do, the smallest verification (a first RPC), and what
exists today.

::: warning Nothing is delivered in this tree
There is no `sdk/` directory and no client code in the repository. The planned
layout is `sdk/core`, `sdk/ffi`, `sdk/python` and `sdk/engines/{unity,unreal,
godot}`, tracked by **BW-21** (`todo.md`). The wire contract the SDK builds on is
already present: Mercury framing in `crates/mercury/src/packet.rs` and the
entity type ids in `crates/entitydef/src/entities.rs`.
:::

## Required capabilities

These are the items the SDK is accepted against (BW-21-01 and BW-21-02):

| Capability | Accepted when |
| --- | --- |
| Login and disconnect | both the reference and the Boundless server accept it |
| Reconnect | a method call still works after the session is restored |
| Entity create/destroy and space events | lifecycle order matches |
| Property sync | a nested property update is readable |
| Two-way RPC | arguments and replies can be compared |
| Movement input | the server receives identical input data |

At the ABI layer (BW-21-02) the SDK must additionally define handle ownership and
release rules, callback threading and dispatch, version negotiation and error
codes, and ship generated public headers.

## The reference client you will verify against

The reference revision's smallest client is a **C++ client with Python embedded**,
at `examples/client_integration/python/simple/` (`main.cpp`, `py_server.cpp`,
`py_entity.cpp` and `res/scripts/`). It is a host for the Python scripts, not a
standalone Python package — there is no pip-installable client in the reference
either, which is why BW-21-02-05 exists to deliver one.

Login looks like this:

```bash
./simple_python_client \
  --res $MF_ROOT/programming/bigworld/examples/client_integration/python/simple/res:$MF_ROOT/game/res/bigworld \
  -server <server_address>:<server_port> -user MyAccount -password a
```

## Verifying a first RPC

"First RPC" here means the shortest round trip that proves a client message
reached a server entity and a server message reached the client. The reference
client exercises all of it, and the expected console output is the acceptance
evidence:

| # | What happens | Where |
| --- | --- | --- |
| 1 | Client logs in, receives the world | `login_request_protocol` handshake |
| 2 | Client's `onFinish` calls `self.base.logOff()` | `client/ClientAvatar.py` — **client → Base RPC** |
| 3 | Base's `onEntitiesEnabled` calls `self.createCellEntity(BigWorld.globalBases["DefaultSpace"].cell)` | `base/ClientAvatar.py` |
| 4 | Cell creates a timer and calls `self.allClients.fromCell("Hello from the cell")` | `cell/Avatar.py` — **Cell → Client RPC** |
| 5 | Client prints the reply | `ClientAvatar.fromCell: <id>: Hello from the cell` |

Step 2 is the first RPC in the client→server direction and step 4 is the first
in the server→client direction. Both are declared in the entity definition, not
invented in the script: `<logOff>` under `<BaseMethods>` and `<fromCell>` under
`<ClientMethods>`. See [Entity scripting](/guide/entity-authoring).

While connected the client prints one line per second, counting up, before the
cell entity delivers its chat message. Reproducing that sequence against the
Boundless server is the acceptance test for BW-14-02-01 ("run the Python simple
example against the new server and complete login").

## Mobile interop

The reference also ships Android and iOS clients under
`examples/client_integration/c_plus_plus/android/` and `.../ios/`. They exist to
prove the *protocol* survives a non-desk host; BW-14-02-02 and BW-14-02-03
accept them by recording protocol results, or a documented host gap. Neither
runs against this tree.

## What to do today

Until BW-21 lands there is no SDK to import. The closest thing to a client check
is the server side of the same contract:

```bash
cargo test -p server-runtime      # config, tick, timers, signals, shutdown
./target/debug/boundless-server loginapp
```

and the framing constants that any client must match:

```text
PACKET_MAX_SIZE          1472   maximum UDP payload sent
HEADER_SIZE               2    one u16 flags field
RESERVED_FOOTER_SIZE     27    worst-case footer
MAX_CAPACITY           1443   payload budget after both
```