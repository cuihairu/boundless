# Entity scripting

Entities are the unit of game state. Each entity type can have a **Base** part
(account and offline state), a **Cell** part (in-world behaviour) and a **Client**
part (a shadow on the client). This page documents the script model and the
callback names a script can implement.

::: warning Not runnable against this tree yet
The Python host is not implemented: `crates/python-host/src/lib.rs` is a four-line
doc comment reserving the public surface, and there are no `.py` files in the
repository. The scripts below are the reference revision's, which is the
compatibility contract the host must honour (BW-03, BW-14).
:::

## Parts and base classes

Which base class a part uses depends on whether it owns a cell part, and the
FantasyDemo generated scripts show all three options in `base/`:

| Part | Base class | FantasyDemo examples |
| --- | --- | --- |
| Base, no cell part | `BigWorld.Proxy` | `Account`, `Avatar` |
| Base, has a cell part | `BigWorld.Base` | `Guard`, `Space` |
| Cell | `BigWorld.Entity` | `Avatar`, `Guard`, `Space` |
| Client | `BigWorld.Entity` | `Account`, `Avatar`, `Guard`, `Space`, `ClientOnly` |

`Account` is a `Proxy` in `base/` because it never has a cell part, while
`Avatar` is a `Proxy` in `base/` but an `Entity` in `cell/` — the same type name
means different things in different directories.

## Callbacks, and where the framework declares them

Each callback is a documented hook on the C++ side, introduced in the source by a
`/*~ callback ...` comment. They are spread across three parts of the server, so
the citation matters as much as the name:

| Callback | Declared in | Fires when |
| --- | --- | --- |
| `Entity.__init__` | `server/cellapp/entity.cpp` | the entity is created |
| `Entity.onTimer(timerId, userData)` | `server/baseapp/base.cpp` | a timer fires |
| `Entity.onDestroy` | `server/cellapp/entity.cpp` | the entity is destroyed |
| `Entity.onLoseControlledBy` | `server/cellapp/entity.cpp` | the entity loses player control |
| `Entity.onRestore` | `server/cellapp/entity.cpp` | the entity is restored from storage |
| `Base.onLoseCell` | `server/baseapp/base.cpp` | the cell part is destroyed |
| `Base.onGetCell` | `server/baseapp/base.cpp` | a cell part has been acquired |
| `Proxy.onClientDeath` | `server/baseapp/proxy.cpp` | the owning client disconnects |
| `Proxy.onEntitiesEnabled` | `server/baseapp/proxy.cpp` | the client confirms its entity updates |
| `Proxy.onClientGetCell` | `server/baseapp/proxy.cpp` | the client gets its cell part |
| `Entity.onBecomePlayer` / `onBecomeNonPlayer` | `client/py_entity.cpp` | the entity gains or drops the player role |
| `Entity.onEnterWorld` / `onLeaveWorld` | `client/entity.cpp` | the entity joins or leaves a space |

The cell side also declares the AOI pair `Entity.onEnteredAoI` /
`Entity.onLeftAoI` and the witness hooks `Entity.onWitnessed` /
`Entity.onLoseWitness`, both in `server/cellapp/entity.cpp`.

::: warning `onLoseControl` is not a framework hook
`cell/Avatar.py` in the generated FantasyDemo defines `onLoseControl`, but the
reference framework never calls it — the real hook is
`Entity.onLoseControlledBy` (`server/cellapp/entity.cpp`). Grepping the reference
source for `"onLoseControl"` returns only the generated script itself. That
method therefore runs as dead code in the demo; the intended cleanup point is
`onLoseControlledBy`. It is worth knowing before you copy the demo script into
your own code.
:::

## A real script: `cell/Avatar.py`

```python
"Avatar cell entity: ticks a timer and talks to the client."

import BigWorld


class Avatar( BigWorld.Entity ):

	def __init__( self ):
		BigWorld.Entity.__init__( self )
		self._timer = self.addTimer( 1.0, 1.0 )

	def onTimer( self, timerId, userData ):
		self.allClients.fromCell( "Hello from the cell" )
		self.allClients.cellTick( 1 )

	def onLoseControl( self ):
		self.destroy()
```

This is the smallest script with an observable callback. Three things to notice:

- `addTimer(delay, repeat)` starts a repeating timer; `onTimer` receives the id.
- `self.allClients.<method>(...)` sends a **client** method defined in the entity
  definition — see below for the matching `<ClientMethods>` entries.
- The `onLoseControl` method here is a dead-end in the demo: the framework's hook
  is `Entity.onLoseControlledBy`, so this one is never called. See the warning
  above.

The observable output when a client is attached is the method call arriving on
the client as `fromCell: <message>` and `cellTick: <n>` once per second — the
`onTimer` callback is the observable thing.

## Entity definitions wire the methods

Scripts are not enough on their own: the RPC surface comes from the `.def`
files, one per entity type. From `Avatar.def`:

```xml
<Properties>
  <!-- each property carries <Type>, <Flags>, <Persistent>, <Editable>,
       <Default> and <Identifier> -->
</Properties>

<ClientMethods>
	<fromCell>
		<Arg> STRING </Arg>
	</fromCell>
	<cellTick>
		<Arg> INT32 </Arg>
	</cellTick>
</ClientMethods>

<CellMethods>
</CellMethods>

<BaseMethods>
	<logOff>
		<Exposed/>
	</logOff>
</BaseMethods>
```

`Account.def` shows the same pattern on a smaller surface: `playerName` is
`BASE_AND_CLIENT`, persistent and editable, and `chatMessage` is a client method
taking one `STRING`. `Avatar.def` also has a `<Volatile>` section holding
`position`, `yaw` and `pitch` — those are the properties that change every tick
and so are streamed separately from the persistent set.

## Property visibility flags

`<Flags>` takes exactly one of nine spellings. The table is the framework's own
flag mapping (`lib/entitydef/data_description.cpp`), which is what decides what
gets streamed and to whom:

| `<Flags>` | Internal flags |
| --- | --- |
| `CELL_PRIVATE` | none — cell side only, never streamed |
| `CELL_PUBLIC` | `DATA_GHOSTED` |
| `OTHER_CLIENTS` | `DATA_GHOSTED \| DATA_OTHER_CLIENT` |
| `OWN_CLIENT` | `DATA_OWN_CLIENT` |
| `BASE` | `DATA_BASE` |
| `BASE_AND_CLIENT` | `DATA_OWN_CLIENT \| DATA_BASE` |
| `CELL_PUBLIC_AND_OWN` | `DATA_GHOSTED \| DATA_OWN_CLIENT` |
| `ALL_CLIENTS` | `DATA_GHOSTED \| DATA_OTHER_CLIENT \| DATA_OWN_CLIENT` |
| `EDITOR_ONLY` | `DATA_EDITOR_ONLY` |

The FantasyDemo uses five of the nine: `ALL_CLIENTS` (4 properties),
`BASE_AND_CLIENT` (2), `OTHER_CLIENTS` (1), `BASE` (1) and `OWN_CLIENT` (1).

The streaming consequence is stated in the bots source
(`server/tools/bots/bot_entity.cpp`): `BASE_AND_CLIENT` properties are streamed
when the player is initialised from its **base** entity, while `OWN_CLIENT`,
`OTHER_CLIENT` and `ALL_CLIENT` properties are streamed when it is initialised
from its **cell** entity.

Methods listed under `<ClientMethods>` are exactly the ones a server-side script
may call through `self.allClients`. `<Exposed/>` on a base method marks it
callable from the client.

The target directory for these is `examples/fantasydemo/res/scripts/entity_defs/`
(BW-14); the wire id space they map into already exists in
`crates/entitydef/src/entities.rs`.

## Timers and entity lifecycle

Beyond `addTimer`, the API surface a script uses is:

- `BigWorld.createEntity(...)` — but see the signature table below, it differs
  per side
- `BigWorld.entities[id]` (`server/baseapp/script_bigworld.cpp`)
- `BigWorld.destroyEntity(id)`
- `BigWorld.globalBases["DefaultSpace"]` (`server/baseapp/script_bigworld.cpp`)
- `self.createCellEntity(mailbox)` / `self.destroyCellEntity()` — documented as
  `Base.createCellEntity` and `Base.destroyCellEntity` in
  `server/baseapp/base.cpp`
- `self.destroy()`
- `self.base.<method>()` / `self.allClients.<method>()`

::: warning `BigWorld.createEntity` has a different signature on each side
The name is the same but the argument lists are not. All three are documented in
the reference source with a `/*~ function` comment:

| Side | Declared in | Signature | Returns |
| --- | --- | --- | --- |
| Cell | `server/cellapp/cellapp.cpp` | `createEntity(class, spaceID, position, direction, state)` | the new entity |
| Client | `client/entity_manager.cpp` | `createEntity(className, spaceID, vehicleID, position, direction, state)` | the entity **id** |
| Base | `server/baseapp/script_bigworld.cpp` | alias for `createBaseLocally(entityType, *params)` — dicts, kwargs or a DataSection | the new base entity |

The extra `vehicleID` argument on the client (pass `0` for no vehicle) is the
only argument-count difference. `direction` is a `(roll, pitch, yaw)` tuple on
both the cell and client sides, and `state` is a property table whose missing
keys fall back to the `.def` defaults.

A real call from `examples/client_integration/python/simple/res/scripts/client/ClientAvatar.py`,
which spawns a client-only entity and immediately calls a method on it:

```python
def createLocalEntity( self ):
	localEntityID = BigWorld.createEntity(
			"ClientOnly",
			self.spaceID, 0,
			self.position, (self.roll, self.pitch, self.yaw),
			{} )
	localEntity = BigWorld.entities[ localEntityID ]
	localEntity.method1( "a random message" )
	self._localEntityID = localEntityID
```

Note that the client side returns an **id**, not an entity — the script has to go
through `BigWorld.entities[id]` to get the object, and keeps the id in
`self._localEntityID` so that a later tick can pass it to
`BigWorld.destroyEntity`.
:::

`onFinish` is a client-side hook too: `ClientAvatar.onFinish` in that same
example calls `self.base.logOff()`, which is the round trip that ends a session.