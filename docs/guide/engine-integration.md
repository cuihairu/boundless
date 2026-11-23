# Engine integration

Unity, Unreal and Godot are the first batch of engines Boundless targets
(BW-21-03, BW-21-04, BW-21-05). Each one is a thin binding over the same SDK
contract, so the interesting work is threading — every engine has a single
thread that entity callbacks are allowed to touch.

::: warning Nothing is delivered in this tree
`sdk/engines/` does not exist. Every statement on this page is a target
contract taken from `todo.md`; no binding, plugin or package can be built
today. The [Client SDK](/guide/client-sdk) page is the shared contract these
build on.
:::

## The common contract

All three engines bind the same SDK surface, and each is accepted by running the
same capability list:

- login, entity spawn, property update, movement, RPC, scene exit
- callbacks dispatched on the engine's single game thread
- no dispatch after the receiving object is destroyed

That last one is what bites in practice: an Unreal component that is torn down
must stop receiving callbacks, and a Godot node that leaves its scene must have
its connection and callbacks cleaned up. Both are separate acceptance items
(BW-21-04-04, BW-21-05-04), which is a signal that they are not implied by the
rest.

## Unity (BW-21-03)

C# wrapper, main-thread event dispatch, packaged as a Unity package.

| Item | Accepted when |
| --- | --- |
| BW-21-03-01 C# interface binding | the editor loads it |
| BW-21-03-02 main-thread dispatch | a callback thread assertion passes |
| BW-21-03-03 packaging | a clean project can import it |
| BW-21-03-04 sample run | login, entity, movement and RPC each produce a result |

## Unreal (BW-21-04)

Plugin plus C++ and Blueprint call interfaces, game-thread dispatch.

| Item | Accepted when |
| --- | --- |
| BW-21-04-01 plugin packaging | an empty project loads the module |
| BW-21-04-02 C++ calls | a sample logs in and sends an RPC |
| BW-21-04-03 Blueprint interface | a blueprint can subscribe to entity events |
| BW-21-04-04 game-thread callbacks | nothing is dispatched after the component is destroyed |
| BW-21-04-05 sample run | component login, sync and exit all pass |

## Godot (BW-21-05)

GDExtension, script bindings and signal dispatch.

| Item | Accepted when |
| --- | --- |
| BW-21-05-01 GDExtension packaging | a clean project loads it |
| BW-21-05-02 script method bindings | a script can log in and send an RPC |
| BW-21-05-03 signal dispatch | the target node receives entity events |
| BW-21-05-04 sample run | leaving the scene leaves the connection and callbacks cleaned up |

## What the bindings share

Whichever engine you use, the wire contract is fixed by source files that
already exist:

- `crates/mercury/src/packet.rs` — payload cap, header and footer sizes, flag
  layout. An engine binding that ignores these will not interoperate with the
  reference server.
- `crates/mercury/src/message_framing.rs` — the message preamble (reply id and
  next-request offset) and the fixed/variable length encoding.
- `crates/mercury/src/udp_bundle.rs` — the bundle-side footers (checksums and
  acks) written across the packet chain.
- `crates/entitydef/src/entity_description.rs` — the `EntityTypeID` space
  (`u16`, `INVALID_ENTITY_TYPE_ID = 0xFFFF`) and `EntityDescription`; ids are
  stamped in load order from `0` by `EntityDescriptionMap`.

See the [Compatibility matrix](/architecture/compatibility-matrix) for the list of
engine and SDK targets and their current status.