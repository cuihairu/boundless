//! The ghost lifecycle (`entity_ghost_maintainer.cpp`, `offload_checker.cpp`,
//! `Space::createGhost`, `Entity::initGhost`/`delGhost`,
//! `Entity::writeGhostDataToStream`). A real's `EntityGhostMaintainer`
//! walks the space's cell tree, creating a ghost on every adjacent cell
//! inside the ghost distance and deleting the haunts that fell out; the
//! receiving CellApp materialises the ghost from the streamed state.
//!
//! The port's standing seams:
//!
//!   * there are no `CellAppChannel`s — the maintainer's marks live in
//!     `CellApp::channel_marks` and its `createGhost`/`delGhost` bundles
//!     in `CellApp::ghost_outbox`, which the tests deliver into the peer
//!     `CellApp` (`isGood()` is `dead_channels`' complement);
//!   * the ghost data stream carries the port's one-word CellApp
//!     addresses, not `Mercury::Address` pairs, and its property-stamp
//!     and ghost-property blocks are count-prefixed (the port's stamp
//!     tables have no per-type arity to size them);
//!   * the def-driven ghost property values do not exist yet, so the
//!     `GProperties` block is empty — the count-prefixed shape is the
//!     seam the property-replication task fills;
//!   * `checkEntityForOffload`'s send side (`Cell::offloadEntity`) is
//!     the offload cluster's face; here the offload list only collects.
//!   * the reference keeps one held-message queue per (entity, source
//!     address); the port keeps one queue per entity, so the address
//!     lookup `playSubsequenceFor` performs collapses to the entity's
//!     single queue.
//!   * the reference's `MF_ASSERT`s — the subsequence markers,
//!     `ghostSetReal`'s `!isReal` and `nextRealAddr_ == owner` checks —
//!     come over as `debug_assert!`s, which the port's unit tests run
//!     under.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use foundation::stream::BinaryStream;

use crate::cellapp_navigate::{
    controller_type_code, read_controller_payload, write_controller_payload,
};
use crate::cellapp_real_entity::DOMAIN_GHOST;
use crate::cellapp_real_entity::start_real_controllers;
use crate::cellapp_space::{CellApp, CellInfo, Entity, Space};
use crate::cellappmgr::SpaceId;

/// `DEFAULT_AOI_RADIUS` — `CellAppConfig::ghostDistance()`'s default.
pub const DEFAULT_AOI_RADIUS: f32 = 500.0;
/// `MAX_GHOSTS_TO_DELETE` — `CellAppConfig::maxGhostsToDelete()`'s
/// default (100).
pub const MAX_GHOSTS_TO_DELETE: u32 = 100;
/// `EntityGhostMaintainer::createOrUnmarkRequiredHaunts`'s "TODO: Make
/// this configurable" `GHOST_FUDGE` (20).
pub const GHOST_FUDGE: f32 = 20.0;
/// `minGhostLifespan` (5.f seconds).
pub const MIN_GHOST_LIFESPAN_IN_SECONDS: u32 = 5;
/// `NEW_REAL_KEEP_GHOST_PERIOD_IN_SECONDS` (2).
pub const NEW_REAL_KEEP_GHOST_PERIOD_IN_SECONDS: u32 = 2;

/// `RealEntity::Haunt`: one ghost this real maintains on a remote
/// CellApp, with the tick it was created at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Haunt {
    /// `channel().addr()`.
    pub addr: u32,
    /// `creationTime()`.
    pub creation_time: u32,
}

/// One wire the maintainer sent: the `createGhost` or `delGhost`
/// message a bundle carried to a destination.
#[derive(Clone, Debug, PartialEq)]
pub struct GhostWire {
    /// The bundle's destination address.
    pub dest: u32,
    /// The sender's address (`srcAddr`).
    pub src: u32,
    /// The space the message names.
    pub space_id: SpaceId,
    /// The entity the message is about.
    pub entity: u32,
    /// The message the bundle carries.
    pub message: GhostMessage,
}

/// The ghost-only messages `deliverGhost` dispatches
/// (`cellapp_interface.hpp:298`).
#[derive(Clone, Debug, PartialEq)]
pub enum GhostMessage {
    /// `createGhost`'s payload (`writeGhostDataToStream`, the id and
    /// type already on it).
    Create(Vec<u8>),
    /// `delGhost`: an empty message.
    Del,
    /// `ghostPositionUpdateArgs`, in the reference's field order.
    Position {
        pos: [f32; 3],
        dir: [f32; 3],
        is_on_ground: bool,
        update_number: u32,
    },
    /// `ghostVolatileInfoArgs`.
    VolatileInfo(crate::cellapp_witness::VolatileInfo),
    /// `ghostedDataUpdate`: the property index, then the change stream.
    Property { index: i32, data: Vec<u8> },
    /// `ghostSetRealArgs`: the offload count and the new owner.
    SetReal {
        num_times_real_offloaded: u32,
        owner: u32,
    },
    /// `ghostSetNextRealArgs`: where the real is going.
    SetNextReal { next_real_addr: u32 },
}

/// What a delivered ghost bundle became (`CellApp::processMessage`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DeliverGhost {
    /// A `createGhost` delivery.
    Create(CreateGhostOutcome),
    /// A `delGhost` delivery: whether the ghost was there.
    Del(bool),
    /// A `ghostPositionUpdate` delivery.
    Position(bool),
    /// A `ghostVolatileInfo` delivery.
    VolatileInfo(bool),
    /// A `ghostedDataUpdate` delivery.
    Property(bool),
    /// The routing ladder dropped the message: its ghost was destroyed
    /// while the message was in flight.
    Dropped,
    /// The routing ladder held the message: out of sender order.
    Buffered,
    /// A `ghostSetReal` delivery: applied, or buffered behind the
    /// subsequence it interrupted.
    SetReal(GhostSetReal),
    /// A `ghostSetNextReal` delivery.
    SetNextReal(bool),
}

/// What a `ghostSetReal` delivery became.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GhostSetReal {
    /// The sign-off matched and the ghost now follows the new real.
    Applied,
    /// It arrived out of turn: held for the switch.
    Buffered,
    /// No such entity (or real) to sign off.
    Missing,
}

/// `CellAppInterface::onload` (`cellapp_interface.hpp:296`): the real
/// entity's data leaving for the app it is being offloaded to. The
/// reference streams it into the destination's bundle ahead of the
/// entity's ghosts; the port carries one wire per offload.
#[derive(Clone, Debug, PartialEq)]
pub struct OnloadWire {
    /// The destination app's address.
    pub dest: u32,
    /// The sender's address.
    pub src: u32,
    /// The space the offload runs in.
    pub space_id: SpaceId,
    /// The entity that moves.
    pub entity: u32,
    /// `isTeleport`: the offload checker never teleports.
    pub is_teleport: bool,
    /// The `Entity::writeRealDataToStream` body.
    pub data: Vec<u8>,
}

impl GhostWire {
    /// The `CellAppInterface::createGhost` arguments off a wire's
    /// payload: the entity id the message carries, then the rest.
    pub fn split_create(&self) -> Option<(u32, Vec<u8>)> {
        let data = match &self.message {
            GhostMessage::Create(data) => data,
            _ => return None,
        };
        split_create_payload(data)
    }
}

/// The create payload after the streamed entity id — `MF_*_ENTITY_MSG`
/// carries the id in the message header, and
/// `writeGhostDataToStream` puts it on the body too.
fn split_create_payload(data: &[u8]) -> Option<(u32, Vec<u8>)> {
    let mut stream = BinaryStream::from_slice(data);
    let entity = stream.read_u32().ok()?;
    Some((entity, stream.unread_bytes().to_vec()))
}

/// `BufferedGhostMessage` (`buffered_ghost_message.hpp`): one held
/// message for a ghost whose real is in transit.
#[derive(Clone, Debug, PartialEq)]
pub struct BufferedGhostMessage {
    /// `srcAddr`.
    pub src: u32,
    /// The entity the message names.
    pub entity: u32,
    /// The space the message named.
    pub space_id: SpaceId,
    /// `delaySubsequence` (an unexpected subsequence is draining)
    /// versus a plain `add` behind an already-buffered message.
    pub delayed: bool,
    /// The message itself.
    pub message: GhostMessage,
}

/// `BufferedGhostMessages::isSubsequenceStart`
/// (`buffered_ghost_messages.hpp:56`): a new real's sign-off opens the
/// subsequence — and so does a `createGhost`, whose buffered form is
/// hard-marked a start (`buffered_ghost_message_factory.cpp:132`).
fn is_subsequence_start(message: &GhostMessage) -> bool {
    matches!(
        message,
        GhostMessage::Create(_) | GhostMessage::SetReal { .. }
    )
}

/// `BufferedGhostMessages::isSubsequenceEnd` (line 61): the del and the
/// next-real sign-off close it.
fn is_subsequence_end(message: &GhostMessage) -> bool {
    matches!(
        message,
        GhostMessage::Del | GhostMessage::SetNextReal { .. }
    )
}

/// `EntityReality` (`cellapp_interface.hpp:34`): the message's own
/// constraint — which form of the entity it is for. `WITNESS_ONLY`
/// sits above `REAL_ONLY`, so the ladder's `>= REAL_ONLY` arm catches
/// both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntityReality {
    /// For the ghost form only.
    GhostOnly,
    /// For the real form only.
    RealOnly,
    /// For the entity that holds the witness.
    WitnessOnly,
}

/// What `EntityMessageHandler::handleMessage` does with a message
/// (`entity_message_routing.hpp:24`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityMessageDisposition {
    /// GHOST_ONLY: the ghost was destroyed while the message was in
    /// flight.
    DropDestroyedGhost,
    /// GHOST_ONLY: out of sender order — buffer until the sender
    /// catches up.
    BufferGhostMessage,
    /// REAL_ONLY / WITNESS_ONLY: not holding the real — attempt a
    /// forward.
    ForwardToReal,
    /// WITNESS_ONLY: the entity has no witness to receive it.
    DropNoWitness,
    /// No tick budget left this tick — defer to the next one.
    BufferIfTickPending,
    /// None of the above: call through to the message's handler.
    Dispatch,
}

/// Everything the disposition ladder branches on
/// (`EntityMessageRoutingState`, line 54).
#[derive(Clone, Copy, Debug, Default)]
pub struct EntityMessageRoutingState {
    /// The message's reality.
    pub reality: Option<EntityReality>,
    /// `app.findEntity( entityID ) != NULL`.
    pub entity_exists: bool,
    /// The found entity is the real.
    pub entity_is_real: bool,
    /// `Entity::isOffloadingTo( srcAddr )`.
    pub entity_is_offloading_to_sender: bool,
    /// Only meaningful when the entity exists and is real.
    pub entity_has_witness: bool,
    /// `Entity::shouldBufferMessagesFrom( srcAddr )`.
    pub should_buffer_from_sender: bool,
    /// `BufferedGhostMessages::isDelayingMessagesFor`.
    pub delaying_for_sender: bool,
    /// `BufferedGhostMessages::hasMessagesFor`.
    pub has_messages_for_sender: bool,
    /// The message opens a subsequence.
    pub is_subsequence_start: bool,
    /// The handler's deferral flag and the cellapp's tick state.
    pub should_buffer_if_tick_pending: bool,
    pub next_tick_pending: bool,
}

/// The `EntityMessageHandler::handleMessage` disposition ladder
/// (`classifyEntityMessage`), as a pure function over the routing
/// state. The port's ghost-domain messages all carry
/// `GHOST_ONLY`, so the `REAL_ONLY`/`WITNESS_ONLY` arms are pinned by
/// the classifier's own tests — their side effects (the forward and
/// the witness drop) are the base-message and witness clusters' faces.
pub fn classify_entity_message(s: &EntityMessageRoutingState) -> EntityMessageDisposition {
    // Option-free access: the callers always name a reality.
    let reality = s.reality.unwrap_or(EntityReality::GhostOnly);
    let should_buffer_ghost_message =
        !s.entity_exists || s.should_buffer_from_sender || s.delaying_for_sender;

    let mut is_for_destroyed_ghost = false;

    // Message is for a destroyed ghost if it is out of subsequence order.
    if reality == EntityReality::GhostOnly {
        // Destroyed by restored real; or destroyed by restored real,
        // then recreated when offloading to the sender; or destroyed
        // when the sender's death was handled. A subsequence start has
        // no predecessor to reference its order, so it is never
        // counted as destroyed-ghost.
        is_for_destroyed_ghost = (s.entity_exists && s.entity_is_real)
            || (s.entity_exists && s.entity_is_offloading_to_sender && !s.is_subsequence_start)
            || (should_buffer_ghost_message
                && !s.has_messages_for_sender
                && !s.is_subsequence_start);
    }

    // Drop GHOST_ONLY messages for a destroyed ghost.
    if is_for_destroyed_ghost {
        return EntityMessageDisposition::DropDestroyedGhost;
    }

    // Buffer GHOST_ONLY messages that are out of sender order.
    if reality == EntityReality::GhostOnly && should_buffer_ghost_message {
        return EntityMessageDisposition::BufferGhostMessage;
    }

    // REAL_ONLY messages are forwarded if we do not hold the real;
    // WITNESS_ONLY sits above, so this arm also catches witness
    // messages for entities we do not hold as real.
    if reality >= EntityReality::RealOnly && (!s.entity_exists || !s.entity_is_real) {
        return EntityMessageDisposition::ForwardToReal;
    }

    // Drop WITNESS_ONLY messages for entities without a witness.
    if reality == EntityReality::WitnessOnly && s.entity_exists && !s.entity_has_witness {
        return EntityMessageDisposition::DropNoWitness;
    }

    if s.should_buffer_if_tick_pending && s.next_tick_pending {
        return EntityMessageDisposition::BufferIfTickPending;
    }

    EntityMessageDisposition::Dispatch
}

/// `BufferedGhostMessagesForEntity`
/// (`buffered_ghost_messages_for_entity.{hpp,cpp}`): one ghost's held
/// messages, with the delayed subsequence being collected while an
/// unexpected one drains.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BufferedGhostMessagesForEntity {
    /// The held messages, in arrival order.
    pub messages: Vec<BufferedGhostMessage>,
    /// `pDelayedMessages_`: the subsequence being collected.
    pub delayed: Option<Vec<BufferedGhostMessage>>,
}

impl BufferedGhostMessagesForEntity {
    /// `add` (`buffered_ghost_message_queue.cpp:44`): behind a delayed
    /// subsequence or plain on the queue — whose front must always be
    /// a subsequence start.
    pub fn add(&mut self, held: BufferedGhostMessage) {
        if self.is_delaying() {
            self.delayed.as_mut().expect("delaying").push(held);
            return;
        }
        self.messages.push(held);
        // `isFrontSubsequenceStart`: an empty queue passes vacuously.
        debug_assert!(
            self.messages.is_empty() || is_subsequence_start(&self.messages[0].message),
            "the queue opens with a subsequence start"
        );
    }

    /// `isDelaying` (line 211): mid-collection until the last held
    /// message closes the subsequence.
    pub fn is_delaying(&self) -> bool {
        self.delayed
            .as_ref()
            .is_some_and(|held| !is_subsequence_end(&held.last().expect("non-empty").message))
    }

    /// `promoteDelayedSubsequences` (line 175): the delayed messages
    /// splice in ahead of the queue.
    pub fn promote_delayed_subsequences(&mut self) {
        if let Some(held) = self.delayed.take() {
            let mut promoted = held;
            promoted.append(&mut self.messages);
            self.messages = promoted;
        }
    }

    /// `playFront` (line 96): play one message; answers whether the
    /// queue drained (and nothing is still delayed).
    fn play_front(&mut self, app: &mut CellApp, spaces: &mut BTreeMap<SpaceId, Space>) -> bool {
        let Some(held) = self.messages.first().cloned() else {
            // WARNING "Queue is empty"
            return false;
        };
        self.messages.remove(0);
        let drained = self.messages.is_empty() && self.delayed.is_none();
        play_held_message(app, spaces, &held);
        drained && self.delayed.is_none()
    }

    /// `playSubsequence` (line 193): promote the delayed head, then
    /// replay through the next subsequence end.
    pub fn play_subsequence(
        &mut self,
        app: &mut CellApp,
        spaces: &mut BTreeMap<SpaceId, Space>,
    ) -> bool {
        self.promote_delayed_subsequences();
        let mut finished = false;
        while !self.messages.is_empty() && !finished {
            finished = is_subsequence_end(&self.messages[0].message);
            if self.play_front(app, spaces) {
                return true;
            }
        }
        false
    }

    /// `delaySubsequence` (`buffered_ghost_message_queue.cpp:217`):
    /// hold the unexpected subsequence, dragging the queue head with it
    /// while the collection stays open.
    pub fn delay_subsequence(&mut self, held: BufferedGhostMessage) {
        debug_assert!(
            is_subsequence_start(&held.message),
            "only a subsequence start delays a queue"
        );
        let bucket = self.delayed.get_or_insert_with(Vec::new);
        bucket.push(held);
        while !self.messages.is_empty() && self.is_delaying() {
            let next = self.messages.remove(0);
            self.delayed.as_mut().expect("delaying").push(next);
        }
    }

    /// Nothing held at all.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty() && self.delayed.is_none()
    }
}

/// `BufferedGhostMessage::play`'s dispatch
/// (`buffered_ghost_message_factory.cpp`): the held message replays
/// through the very face a normal delivery would have used.
fn play_held_message(
    app: &mut CellApp,
    spaces: &mut BTreeMap<SpaceId, Space>,
    held: &BufferedGhostMessage,
) {
    match &held.message {
        GhostMessage::Create(data) => {
            if app
                .entity_population
                .get(&held.entity)
                .is_some_and(|entity| !entity.borrow().destroyed)
            {
                // WARNING "BufferedCreateGhostMessage::play: Entity %u
                // already exists" — re-buffer behind the subsequence it
                // interrupted.
                app.delay_ghost_subsequence(BufferedGhostMessage {
                    src: held.src,
                    entity: held.entity,
                    space_id: held.space_id,
                    delayed: true,
                    message: GhostMessage::Create(data.clone()),
                });
            } else {
                app.create_ghost(spaces, held.space_id, held.entity, data);
            }
        }
        GhostMessage::Del => {
            app.del_ghost(spaces, held.entity);
        }
        GhostMessage::Position {
            pos,
            dir,
            is_on_ground,
            update_number,
        } => {
            app.ghost_position_update(
                spaces,
                held.entity,
                *pos,
                *dir,
                *is_on_ground,
                *update_number,
            );
        }
        GhostMessage::VolatileInfo(info) => {
            app.ghost_volatile_info(held.entity, *info);
        }
        GhostMessage::Property { index, data } => {
            app.ghosted_data_update(held.entity, *index, data);
        }
        GhostMessage::SetReal {
            num_times_real_offloaded,
            owner,
        } => {
            let outcome =
                app.ghost_set_real(spaces, held.entity, *num_times_real_offloaded, *owner);
            if outcome == GhostSetReal::Missing {
                // ERROR "BufferedGhostSetRealMessage::play: Entity %u no
                // longer exists" — drop rather than dereference the
                // dead findEntity (CR-P2-09).
                app.ghost_property_notes.push(format!(
                    "BufferedGhostSetRealMessage::play: entity {entity} no longer exists",
                    entity = held.entity
                ));
            }
        }
        GhostMessage::SetNextReal { next_real_addr } => {
            app.ghost_set_next_real(spaces, held.entity, *next_real_addr);
        }
    }
}

/// `Entity::createGhost`'s and `Space::createGhost`'s outcomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreateGhostOutcome {
    /// The ghost was materialised.
    Created,
    /// The create was buffered (or delayed) — the WARNING face.
    Buffered,
    /// `initGhost`'s stream ran out.
    BadStream,
    /// `Space::newEntity`: the type "has no cell entity class".
    UnknownType,
    /// `IGameDelegate::instance() != NULL` — "Currently not supported
    /// by Delegate Physical spaces".
    Delegate,
    /// The createGhost message named no space this CellApp has.
    NoSpace,
}

/// `Entity::writeGhostDataToStream` (`entity.cpp:1832`): the whole
/// ghost state — the id and type the receiver's `Space::createGhost`
/// reads off, then the body `readGhostDataFromStreamInternal` takes.
pub fn write_ghost_data_to_stream(app: &CellApp, entity: u32) -> Option<Vec<u8>> {
    let entity_rc = app.entity_population.get(&entity)?;
    let borrowed = entity_rc.borrow();
    let mut stream = BinaryStream::new();
    // The id and entityTypeID. `BufferedGhostMessage` reads the
    // numTimesRealOffloaded_ head off this too — it is the first body
    // field.
    stream.write_u32(borrowed.id);
    stream.write_u32(borrowed.type_id);
    stream.write_u32(borrowed.num_times_real_offloaded);
    stream.write_f32(borrowed.local_position[0]);
    stream.write_f32(borrowed.local_position[1]);
    stream.write_f32(borrowed.local_position[2]);
    stream.write_bool(borrowed.is_on_ground);
    stream.write_u32(borrowed.last_event_number);
    stream.write_f32(borrowed.volatile_info.position_priority);
    stream.write_f32(borrowed.volatile_info.yaw_priority);
    stream.write_f32(borrowed.volatile_info.pitch_priority);
    stream.write_f32(borrowed.volatile_info.roll_priority);
    // `CellApp::instance().interface().address()` — this is the real's
    // address the ghost tracks.
    stream.write_u32(app.address);
    let (base_ip, base_port) = borrowed.base_addr.unwrap_or((0, 0));
    stream.write_u32(base_ip);
    stream.write_u16(base_port);
    stream.write_f32(borrowed.local_direction[0]);
    stream.write_f32(borrowed.local_direction[1]);
    stream.write_f32(borrowed.local_direction[2]);
    // `propertyEventStamps_.addToStream` — the port's per-entity stamp
    // table, count-prefixed (the reference's count is the type's).
    let stamps = app
        .entity_properties
        .get(&entity)
        .cloned()
        .unwrap_or_default();
    stream.write_u32(stamps.len() as u32);
    for property in &stamps {
        stream.write_u32(property.stamp as u32);
    }
    stream.write_bytes(&b"GProperties"[..4]);
    // The def-driven ghost property values — empty in the port (the
    // property-replication cluster fills this shape).
    stream.write_u32(0);
    stream.write_bytes(&b"GController"[..4]);
    let velocity = borrowed.velocity;
    let ghost_controllers: Vec<_> = borrowed
        .controllers
        .container
        .iter()
        .filter(|(_, controller)| controller.domain() & DOMAIN_GHOST != 0)
        .collect();
    stream.write_u32(ghost_controllers.len() as u32);
    for (cid, controller) in ghost_controllers {
        stream.write_u32(*cid as u32);
        stream.write_u32(controller_type_code(&controller.kind));
        stream.write_i32(controller.user_arg);
        write_controller_payload(&mut stream, &controller.kind, velocity);
    }
    stream.write_bytes(&b"GTail"[..4]);
    stream.write_u32(borrowed.periods_without_witness);
    stream.write_u8(borrowed.aoi_update_scheme_id);
    drop(borrowed);
    Some(stream.as_bytes().to_vec())
}

/// `Entity::writeRealDataToStream` (`entity.cpp:2061`): the real's whole
/// state — the id, the fixed `false` teleportFailure the onload checks
/// first, then the body `writeRealDataToStreamInternal` takes.
pub fn write_real_data_to_stream(app: &CellApp, entity: u32, is_teleport: bool) -> Option<Vec<u8>> {
    let entity_rc = app.entity_population.get(&entity)?;
    let borrowed = entity_rc.borrow();
    let mut stream = BinaryStream::new();
    stream.write_u32(borrowed.id);
    stream.write_bool(false); // teleportFailure
    // The compression decision (`internalNetworkCompressionType`) has no
    // port face; the body below is the uncompressed one.
    // `writeRealDataToStreamInternal`: the real-only properties
    // (`propCountGhost()..propCountGhostPlusReal()`) — none in the port
    // (the property-replication cluster fills this shape) — then
    // `TOKEN_ADD( data, "RealProps" )`, which is the first four bytes of
    // the tag alone.
    stream.write_bytes(&b"RealProps"[..4]);
    // `RealEntity::writeOffloadData` (`real_entity.cpp:501`).
    // `StreamHelper::addRealEntity( data )`: 0-size today
    // (`cell_data.rs`'s payload record).
    // `pChannel_->addToStream( data )`: the channel's own address, then
    // the entity's base address — the same pair the ghost body carries.
    stream.write_u32(app.address);
    let (base_ip, base_port) = borrowed.base_addr.unwrap_or((0, 0));
    stream.write_u32(base_ip);
    stream.write_u16(base_port);
    // `velocity_ << topSpeed_ << topSpeedY_ << physicsCorrections_`: the
    // port's entity carries the velocity; the physics cluster holds the
    // rest.
    stream.write_f32(borrowed.velocity[0]);
    stream.write_f32(borrowed.velocity[1]);
    stream.write_f32(borrowed.velocity[2]);
    // `uint8( shouldAutoBackup_ )`: the backup-policy cluster's field.
    stream.write_u8(0);
    // The current space ID, so the destination can detect a change.
    stream.write_u32(borrowed.space_id);
    stream.write_bool(is_teleport);
    let haunts = borrowed.haunts.clone();
    let controlled_by = borrowed.controlled_by;
    drop(borrowed);
    // The ghosts this entity has on the other apps. This app's own
    // address is always one of them, since the entity turns into a ghost
    // here when the offload lands.
    let owner = app.address;
    let is_already_haunted = haunts.iter().any(|haunt| haunt.addr == owner);
    stream.write_u32(haunts.len() as u32 + u32::from(!is_already_haunted));
    for haunt in &haunts {
        stream.write_u32(haunt.addr);
    }
    if !is_already_haunted {
        stream.write_u32(owner);
    }
    stream.write_u32(controlled_by);
    // `CellApp::instance().adjustLoadForOffload( ... )`: the load
    // bookkeeping has no port face — the app keeps the load until the
    // destination's onload asks for it.
    // `writeBasePropertiesExposedForReplayToStream`: the packed
    // base-properties block, empty when there are none.
    stream.write_packed_int(0).expect("the stream writes");
    Some(stream.as_bytes().to_vec())
}

impl CellApp {
    /// `Space::createGhost`'s message face (`space.cpp:228`): a create
    /// for an entity that already exists here, or that has messages
    /// buffered, is itself buffered — behind a delayed subsequence when
    /// messages were already held, opening a delayed one when the
    /// entity pre-exists; otherwise the ghost is built.
    pub fn create_ghost_message(
        &mut self,
        spaces: &mut BTreeMap<SpaceId, Space>,
        space_id: SpaceId,
        src_addr: u32,
        entity_id: u32,
        data: &[u8],
    ) -> CreateGhostOutcome {
        let existing = self
            .entity_population
            .get(&entity_id)
            .is_some_and(|entity| !entity.borrow().destroyed);
        let buffered = self
            .buffered_ghost_messages
            .get(&entity_id)
            .is_some_and(|held| {
                held.messages
                    .iter()
                    .chain(held.delayed.iter().flatten())
                    .any(|message| message.entity == entity_id && message.src == src_addr)
            });
        if existing || buffered {
            // WARNING "Buffered createGhost message for entity %u from %s"
            self.ghost_property_notes.push(format!(
                "Space::createGhost: buffered createGhost for {entity_id} from {src_addr}"
            ));
            let held = BufferedGhostMessage {
                src: src_addr,
                entity: entity_id,
                space_id,
                delayed: !buffered,
                message: GhostMessage::Create(data.to_vec()),
            };
            if buffered {
                self.buffer_ghost_message(held);
            } else {
                // TRACE "delaying subsequence"
                self.delay_ghost_subsequence(held);
            }
            return CreateGhostOutcome::Buffered;
        }
        self.create_ghost(spaces, space_id, entity_id, data)
    }

    /// `Space::createGhost( entityID, data )` (`space.cpp:273`): build
    /// the entity, `initGhost` it, and notify the population.
    fn create_ghost(
        &mut self,
        spaces: &mut BTreeMap<SpaceId, Space>,
        space_id: SpaceId,
        entity_id: u32,
        data: &[u8],
    ) -> CreateGhostOutcome {
        if self.use_game_delegate {
            // ERROR "Currently not supported by Delegate Physical spaces"
            return CreateGhostOutcome::Delegate;
        }
        let Some(space) = spaces.get_mut(&space_id) else {
            return CreateGhostOutcome::NoSpace;
        };
        let mut stream = BinaryStream::from_slice(data);
        let Ok(type_id) = stream.read_u32() else {
            return CreateGhostOutcome::BadStream;
        };

        // `Space::newEntity`: revive the destroyed population entry, or
        // build a fresh one through the type (which must be on a cell).
        let entity_rc = match self.entity_population.get(&entity_id).cloned() {
            Some(revived) => {
                debug_assert!(revived.borrow().destroyed);
                debug_assert_eq!(revived.borrow().type_id, type_id);
                let mut reborn = Entity::new(entity_id, false);
                reborn.type_id = type_id;
                *revived.borrow_mut() = reborn;
                revived
            }
            None => {
                if !self.entity_scripts.contains_key(&type_id) {
                    // ERROR "Entity type %d has no cell entity class"
                    return CreateGhostOutcome::UnknownType;
                }
                let mut fresh = Entity::new(entity_id, false);
                fresh.type_id = type_id;
                let fresh = Rc::new(RefCell::new(fresh));
                self.entity_population.insert(entity_id, Rc::clone(&fresh));
                fresh
            }
        };

        // `Entity::initGhost` → `readGhostDataFromStreamInternal`.
        let outcome = self.init_ghost(&mut stream, &entity_rc);
        if outcome != CreateGhostOutcome::Created {
            // A bad stream leaves the half-built ghost destroyed in the
            // population for revival, exactly as `initReal`'s does.
            entity_rc.borrow_mut().destroyed = true;
            return outcome;
        }

        // `pSpace_->addEntity` + `onPositionChanged`, with the callbacks
        // held off; the port's chunk faces are the geometry cluster's.
        self.callbacks_permitted(false);
        space.add_entity(Rc::clone(&entity_rc));
        let range_node = self
            .range_lists
            .entry(space_id)
            .or_default()
            .add_entity_node(Rc::clone(&entity_rc));
        entity_rc.borrow_mut().range_node = Some(range_node);
        self.callbacks_permitted(true);

        // `Entity::population().notifyObservers( *pNewEntity )`.
        self.entity_entered_population(entity_id);

        // `this->callback( "onGhostCreated" )`.
        self.entity_callback(entity_id, "onGhostCreated", true, None);

        CreateGhostOutcome::Created
    }

    /// `Entity::readGhostDataFromStreamInternal` (`entity.cpp:1757`): the
    /// ghost state off the stream, with the reference's tokens.
    fn init_ghost(
        &mut self,
        stream: &mut BinaryStream,
        entity_rc: &Rc<RefCell<Entity>>,
    ) -> CreateGhostOutcome {
        macro_rules! read_or {
            ($method:ident) => {
                match stream.$method() {
                    Ok(value) => value,
                    Err(_) => return CreateGhostOutcome::BadStream,
                }
            };
        }
        let entity_id = entity_rc.borrow().id;
        let type_id = entity_rc.borrow().type_id;
        {
            let mut entity = entity_rc.borrow_mut();
            entity.num_times_real_offloaded = read_or!(read_u32);
            entity.local_position = [read_or!(read_f32), read_or!(read_f32), read_or!(read_f32)];
            entity.is_on_ground = read_or!(read_bool);
            entity.last_event_number = read_or!(read_u32);
            entity.volatile_info.position_priority = read_or!(read_f32);
            entity.volatile_info.yaw_priority = read_or!(read_f32);
            entity.volatile_info.pitch_priority = read_or!(read_f32);
            entity.volatile_info.roll_priority = read_or!(read_f32);
            // `eventHistory_.lastTrimmedEventNumber( lastEventNumber_ )`.
            entity.last_trimmed_event_number = entity.last_event_number;
            // `globalPosition_ = localPosition_` (the unmounted copy).
            entity.position = entity.local_position;

            entity.real_addr = Some(read_or!(read_u32));
            let base_ip = read_or!(read_u32);
            let base_port = read_or!(read_u16);
            entity.base_addr = Some((base_ip, base_port));
            entity.local_direction = [read_or!(read_f32), read_or!(read_f32), read_or!(read_f32)];
            entity.direction = entity.local_direction;
        }

        // `propertyEventStamps_.removeFromStream( data )` — the
        // count-prefixed port shape; the ghost's table takes the read
        // stamps as its own.
        let stamp_count = read_or!(read_u32);
        let mut stamps = Vec::with_capacity(stamp_count as usize);
        for _ in 0..stamp_count {
            stamps.push(read_or!(read_u32) as crate::cellapp_witness::EventNumber);
        }
        if !self.check_token(stream, b"GProperties") {
            return CreateGhostOutcome::BadStream;
        }
        // The ghost property values (empty in the port): any non-zero
        // count names a value the port cannot read.
        let property_count = read_or!(read_u32);
        if property_count != 0 {
            return CreateGhostOutcome::BadStream;
        }
        if !self.check_token(stream, b"GController") {
            return CreateGhostOutcome::BadStream;
        }
        // `readGhostControllersFromStream` — the ghost-domain mirror of
        // the real controllers block.
        let controller_count = read_or!(read_u32);
        let mut read_back = Vec::new();
        for _ in 0..controller_count {
            let cid = read_or!(read_u32);
            let type_code = read_or!(read_u32);
            let user_arg = read_or!(read_i32);
            let Some(kind) = read_controller_payload(stream, type_code) else {
                return CreateGhostOutcome::BadStream;
            };
            read_back.push((cid, kind, user_arg));
        }
        if !self.check_token(stream, b"GTail") {
            return CreateGhostOutcome::BadStream;
        }
        let periods_without_witness = read_or!(read_u32);
        let aoi_update_scheme_id = read_or!(read_u8);
        // `MF_ASSERT( data.remainingLength() == 0 )`.
        if !stream.at_end() {
            return CreateGhostOutcome::BadStream;
        }

        // Everything read — commit the halves that borrow the app.
        let mut table = Vec::with_capacity(stamps.len());
        for stamp in stamps {
            table.push(crate::cellapp_witness::ClientProperty { level: 0, stamp });
        }
        self.entity_properties.insert(entity_id, table);
        for (cid, kind, user_arg) in read_back {
            debug_assert!(
                !entity_rc
                    .borrow()
                    .controllers
                    .container
                    .contains_key(&(cid as crate::cellapp_real_entity::ControllerId)),
                "restoring into a live controller"
            );
            let controller = crate::cellapp_real_entity::Controller { kind, user_arg };
            entity_rc
                .borrow_mut()
                .controllers
                .container
                .insert(cid as crate::cellapp_real_entity::ControllerId, controller);
        }
        let mut entity = entity_rc.borrow_mut();
        entity.periods_without_witness = periods_without_witness;
        entity.aoi_update_scheme_id = aoi_update_scheme_id;
        let _ = (entity_id, type_id);
        CreateGhostOutcome::Created
    }

    /// `TOKEN_CHECK` (`binary_stream.hpp:459`): the token rides as the
    /// first four bytes of the name (`stream << *(int*)TOKEN`), and a
    /// mismatch is the ERROR "TOKEN_CHECK failed!" face — the stream
    /// stays in sync for the caller to refuse.
    fn check_token(&mut self, stream: &mut BinaryStream, token: &[u8]) -> bool {
        let mut read = [0u8; 4];
        for slot in &mut read {
            match stream.read_u8() {
                Ok(byte) => *slot = byte,
                Err(_) => return false,
            }
        }
        read == token[..4]
    }

    /// `Entity::delGhost` (`entity.cpp:4984`): the real told us the ghost
    /// is gone — a ghost-only message, and `destroy()` does the work.
    pub fn del_ghost(&mut self, spaces: &mut BTreeMap<SpaceId, Space>, entity: u32) -> bool {
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return false;
        };
        debug_assert!(
            !entity_rc.borrow().is_real,
            "delGhost is a GHOST_ONLY message"
        );
        if entity_rc.borrow().destroyed {
            return false;
        }
        self.destroy_entity(spaces, entity);
        true
    }

    /// `Entity::updateInternalsForNewPositionOfReal`
    /// (`entity.cpp:3905`), the ghost half: `volatileUpdateNumber_`
    /// bumped first, then one `ghostPositionUpdate` per haunt. The
    /// `RealEntity::newPosition` and range-list shuffle faces are the
    /// movement cluster's.
    pub fn send_ghost_position_update(&mut self, entity: u32) {
        let (haunts, space_id, pos, dir, is_on_ground, update_number) = {
            let entity_rc = self
                .entity_population
                .get(&entity)
                .expect("the position update runs for reals");
            let mut borrowed = entity_rc.borrow_mut();
            borrowed.volatile_update_number = borrowed.volatile_update_number.wrapping_add(1);
            (
                borrowed.haunts.clone(),
                borrowed.space_id,
                borrowed.local_position,
                borrowed.local_direction,
                borrowed.is_on_ground,
                borrowed.volatile_update_number as u32,
            )
        };
        for haunt in haunts {
            self.ghost_outbox.push(GhostWire {
                dest: haunt.addr,
                src: self.address,
                space_id,
                entity,
                message: GhostMessage::Position {
                    pos,
                    dir,
                    is_on_ground,
                    update_number,
                },
            });
        }
    }

    /// `Entity::ghostPositionUpdate` (`entity.cpp:4888`): the real's
    /// snapshot lands on the ghost's local faces, then the global face
    /// follows without updating further ghosts.
    pub fn ghost_position_update(
        &mut self,
        _spaces: &mut BTreeMap<SpaceId, Space>,
        entity: u32,
        pos: [f32; 3],
        dir: [f32; 3],
        is_on_ground: bool,
        update_number: u32,
    ) -> bool {
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return false;
        };
        debug_assert!(
            !entity_rc.borrow().is_real,
            "ghostPositionUpdate is a GHOST_ONLY message"
        );
        {
            let mut borrowed = entity_rc.borrow_mut();
            borrowed.local_position = pos;
            borrowed.is_on_ground = is_on_ground;
            borrowed.local_direction = dir;
            borrowed.volatile_update_number = update_number as u16;
        }
        self.update_global_position(&entity_rc, false);
        true
    }

    /// `Entity::setVolatileInfo` (`entity.cpp:6704`): the change, plus
    /// the tell to every haunt. Answers whether the info changed.
    pub fn set_volatile_info(
        &mut self,
        entity: u32,
        new_info: crate::cellapp_witness::VolatileInfo,
    ) -> bool {
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return false;
        };
        // The reference has MF_ASSERT( isReal ) and a redundant release
        // guard; the assert is the port's whole face.
        debug_assert!(entity_rc.borrow().is_real, "setVolatileInfo is a real face");
        if entity_rc.borrow().volatile_info == new_info {
            // The reference returns before touching the haunts.
            return false;
        }
        let (haunts, space_id, less_volatile) = {
            let borrowed = entity_rc.borrow();
            (
                borrowed.haunts.clone(),
                borrowed.space_id,
                new_info.is_less_volatile_than(borrowed.volatile_info),
            )
        };
        // `addDetailedPositionToHistory( false )` on the less-volatile
        // change is the witness cluster's detailed-position face.
        let _ = less_volatile;
        let (haunts, space_id) = (haunts, space_id);
        {
            let mut borrowed = entity_rc.borrow_mut();
            borrowed.volatile_info = new_info;
        }
        for haunt in haunts {
            self.ghost_outbox.push(GhostWire {
                dest: haunt.addr,
                src: self.address,
                space_id,
                entity,
                message: GhostMessage::VolatileInfo(new_info),
            });
        }
        true
    }

    /// `Entity::ghostVolatileInfo` (`entity.cpp:4999`): the ghost takes
    /// the real's volatile info.
    pub fn ghost_volatile_info(
        &mut self,
        entity: u32,
        info: crate::cellapp_witness::VolatileInfo,
    ) -> bool {
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return false;
        };
        debug_assert!(
            !entity_rc.borrow().is_real,
            "ghostVolatileInfo is a GHOST_ONLY message"
        );
        entity_rc.borrow_mut().volatile_info = info;
        true
    }

    /// `Entity::updateGhosts`'s ghost loop (`entity.cpp:5750`): the
    /// property change told to every haunt, the id and property index
    /// streaming first.
    pub fn send_ghosted_data_update(&mut self, entity: u32, index: i32, data: &[u8]) {
        let (haunts, space_id) = {
            let entity_rc = self
                .entity_population
                .get(&entity)
                .expect("the property update runs for reals");
            let borrowed = entity_rc.borrow();
            (borrowed.haunts.clone(), borrowed.space_id)
        };
        for haunt in haunts {
            self.ghost_outbox.push(GhostWire {
                dest: haunt.addr,
                src: self.address,
                space_id,
                entity,
                message: GhostMessage::Property {
                    index,
                    data: data.to_vec(),
                },
            });
        }
    }

    /// `Entity::ghostedDataUpdate` (`entity.cpp:5068`): the ghost's
    /// property data changes. The reference looks the property up on
    /// the entity type's description and CRITICALs on a componentised
    /// one — the port's description table stands in for the def.
    pub fn ghosted_data_update(&mut self, entity: u32, index: i32, data: &[u8]) -> bool {
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return false;
        };
        debug_assert!(
            !entity_rc.borrow().is_real,
            "ghostedDataUpdate is a GHOST_ONLY message"
        );
        let type_id = entity_rc.borrow().type_id;
        let description = self
            .ghost_property_descriptions
            .get(&type_id)
            .and_then(|descriptions| {
                descriptions
                    .iter()
                    .find(|(property_index, _)| *property_index == index)
                    .copied()
            });
        let Some((_, componentised)) = description else {
            self.ghost_property_notes.push(format!(
                "Entity::ghostedDataUpdate: no description for entity {entity}"
            ));
            return false;
        };
        if componentised {
            // CRITICAL "Unable to handle component update"
            self.ghost_property_notes.push(format!(
                "Entity::ghostedDataUpdate: component update {entity}"
            ));
            return false;
        }
        entity_rc
            .borrow_mut()
            .ghost_properties
            .insert(index, data.to_vec());
        self.ghost_property_notes.push(format!(
            "Entity::ghostedDataUpdate: entity {entity} received {index}"
        ));
        true
    }

    /// `BufferedGhostMessages::add` (`buffered_ghost_messages.cpp:11`):
    /// a message the gate could not play yet joins its entity's queue.
    fn buffer_ghost_message(&mut self, held: BufferedGhostMessage) {
        self.buffered_ghost_messages
            .entry(held.entity)
            .or_default()
            .add(held);
    }

    /// `BufferedGhostMessages::delaySubsequence`
    /// (`buffered_ghost_messages.cpp:101`): an unexpected subsequence
    /// opening is held back — along with the queue it interrupted.
    fn delay_ghost_subsequence(&mut self, held: BufferedGhostMessage) {
        self.buffered_ghost_messages
            .entry(held.entity)
            .or_default()
            .delay_subsequence(held);
    }

    /// `BufferedGhostMessages::playSubsequenceFor`
    /// (`buffered_ghost_messages.cpp:22`): replay one ghost's held
    /// messages, erasing the entry once its queue drained. The queue
    /// comes out of the map first — a replayed `ghostSetNextReal`
    /// calls back into this face (the reference's comment names the
    /// recursion).
    pub fn play_subsequence_for(&mut self, spaces: &mut BTreeMap<SpaceId, Space>, entity: u32) {
        let Some(mut queue) = self.buffered_ghost_messages.remove(&entity) else {
            return;
        };
        if queue.play_subsequence(self, spaces) {
            // The drained entry stays erased; a play that buffered
            // fresh messages re-creates its own entry below.
            return;
        }
        self.buffered_ghost_messages.entry(entity).or_insert(queue);
    }

    /// `Entity::ghostSetReal` (`entity.cpp:4927`): the new real's
    /// sign-off. An off-sequence id is held for the switch it
    /// interrupted; a matching one re-points the ghost at the new
    /// real and notifies the extras.
    pub fn ghost_set_real(
        &mut self,
        spaces: &mut BTreeMap<SpaceId, Space>,
        entity: u32,
        num_times_real_offloaded: u32,
        owner: u32,
    ) -> GhostSetReal {
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return GhostSetReal::Missing;
        };
        let space_id = entity_rc.borrow().space_id;
        // `NumTimesRealOffloadedType expected = numTimesRealOffloaded_+1`
        let expected = entity_rc.borrow().num_times_real_offloaded + 1;
        if num_times_real_offloaded != expected {
            // WARNING "Invalid subsequence id. Expected %d. Got %d"
            self.ghost_property_notes.push(format!(
                "Entity::ghostSetReal({entity}): expected {expected}, got {num_times_real_offloaded}"
            ));
            // The message is held keyed by the owner it names — the
            // queue that owner's own sign-off replays.
            self.delay_ghost_subsequence(BufferedGhostMessage {
                src: owner,
                entity,
                space_id,
                delayed: true,
                message: GhostMessage::SetReal {
                    num_times_real_offloaded,
                    owner,
                },
            });
            return GhostSetReal::Buffered;
        }
        entity_rc.borrow_mut().num_times_real_offloaded = num_times_real_offloaded;
        debug_assert!(
            !entity_rc.borrow().is_real,
            "ghostSetReal is a GHOST_ONLY message"
        );
        debug_assert!(
            entity_rc.borrow().next_real_addr == Some(owner),
            "the sign-off must name the address SetNextReal announced"
        );
        // `pRealChannel_ = CellAppChannels::instance().get( args.owner )`
        entity_rc.borrow_mut().real_addr = Some(owner);
        entity_rc.borrow_mut().next_real_addr = None;
        // `this->relocated()` — the extras follow the real.
        self.relocate_entity_extras(&entity_rc);
        let _ = spaces;
        GhostSetReal::Applied
    }

    /// `Entity::ghostSetNextReal` (`entity.cpp:4964`): the current
    /// real's last word — from now on only the named address may send,
    /// and the messages it got ahead with replay now. Answers whether
    /// the entity was here to sign.
    pub fn ghost_set_next_real(
        &mut self,
        spaces: &mut BTreeMap<SpaceId, Space>,
        entity: u32,
        next_real_addr: u32,
    ) -> bool {
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return false;
        };
        debug_assert!(
            entity_rc.borrow().next_real_addr.is_none(),
            "a second sign-off cannot arrive before the switch lands"
        );
        debug_assert!(next_real_addr != 0, "NONE is not a next address");
        entity_rc.borrow_mut().next_real_addr = Some(next_real_addr);
        self.play_subsequence_for(spaces, entity);
        true
    }

    /// `RealEntity::destroy( pNextRealAddr )`'s offload arm
    /// (`real_entity.cpp:255`): every haunt but the next real is told
    /// where the real is going, so they stop trusting this app. The
    /// real itself takes the destination first — `entity.cpp:2039`'s
    /// seam ("it will send a ghostSetReal back to this app").
    pub fn send_ghost_set_next_real(&mut self, entity: u32, next_real_addr: u32) {
        let (haunts, space_id) = {
            let entity_rc = self
                .entity_population
                .get(&entity)
                .expect("the sign-off runs for the offloading real");
            let borrowed = entity_rc.borrow();
            (borrowed.haunts.clone(), borrowed.space_id)
        };
        self.entity_population
            .get(&entity)
            .expect("the sign-off runs for the offloading real")
            .borrow_mut()
            .next_real_addr = Some(next_real_addr);
        for haunt in haunts {
            if haunt.addr == next_real_addr {
                continue;
            }
            self.ghost_outbox.push(GhostWire {
                dest: haunt.addr,
                src: self.address,
                space_id,
                entity,
                message: GhostMessage::SetNextReal { next_real_addr },
            });
        }
        // `pChannel_->reset( NONE, false )` + `destroy()`: the channel
        // face is the offload cluster's — the channel moves to the
        // destination app with the resend history handed over.
    }

    /// `RealEntity::readOffloadData`'s haunt loop (`real_entity.cpp:345`):
    /// the freshly landed real signs itself onto every haunt it can
    /// reach — or, when it came through a space change or a teleport,
    /// tells them the ghost is done instead. Answers `areWeHaunted`:
    /// whether this app held a haunt of its own.
    pub fn send_ghost_set_real(
        &mut self,
        entity: u32,
        bad_haunt_addrs: &[u32],
        changed_space: bool,
        teleported: bool,
    ) -> bool {
        let (haunts, space_id, num_times) = {
            let entity_rc = self
                .entity_population
                .get(&entity)
                .expect("the sign-off runs for the landed real");
            let borrowed = entity_rc.borrow();
            (
                borrowed.haunts.clone(),
                borrowed.space_id,
                borrowed.num_times_real_offloaded,
            )
        };
        let owner = self.address;
        let mut are_we_haunted = false;
        for haunt in haunts {
            if haunt.addr == owner {
                // "we don't care if it's our own address - it could be
                // from a local teleport or some such"
                are_we_haunted = true;
                continue;
            }
            if bad_haunt_addrs.contains(&haunt.addr) {
                // WARNING "Not notifying app %s because of failed teleport"
                self.ghost_property_notes.push(format!(
                    "RealEntity::readOffloadData: failed teleport haunt {} for {entity}",
                    haunt.addr
                ));
                continue;
            }
            if self.dead_channels.contains(&haunt.addr) {
                // WARNING "Not notifying ghost for %u on dead app %s"
                self.ghost_property_notes.push(format!(
                    "RealEntity::readOffloadData: dead haunt {} for {entity}",
                    haunt.addr
                ));
                continue;
            }
            if changed_space || teleported {
                // `addDelGhostMessage` — the del's own face.
                self.ghost_outbox.push(GhostWire {
                    dest: haunt.addr,
                    src: owner,
                    space_id,
                    entity,
                    message: GhostMessage::Del,
                });
                continue;
            }
            // `this->addHaunt( *pAppChannel )`: the haunt list already
            // carries this address — the stream read it in.
            self.ghost_outbox.push(GhostWire {
                dest: haunt.addr,
                src: owner,
                space_id,
                entity,
                message: GhostMessage::SetReal {
                    num_times_real_offloaded: num_times,
                    owner,
                },
            });
        }
        // MF_ASSERT( areWeHaunted ): the offload cluster guarantees a
        // ghost here first ("they should not have offloaded the entity
        // without creating a ghost here first"); the port reports the
        // face instead of asserting it.
        are_we_haunted
    }

    /// `Cell::offloadEntity` (`cell.cpp:193`) → `Entity::offload`
    /// (`entity.cpp:1978`) → `convertRealToGhost` (`entity.cpp:1999`):
    /// the real's state streams to the destination, the channel hands
    /// over, the entity turns into a ghost waiting for the sign-off.
    /// The `isTeleport` arm skips `onLeavingCell` (the teleport's caller
    /// already ran it).
    pub fn offload_entity(
        &mut self,
        spaces: &mut BTreeMap<SpaceId, Space>,
        entity: u32,
        destination: u32,
        is_teleport: bool,
    ) {
        // MF_ASSERT( pEntity->pReal() != NULL ): the caller only hands
        // over reals.
        if self
            .entity_population
            .get(&entity)
            .is_none_or(|entity_rc| !entity_rc.borrow().is_real)
        {
            return;
        }

        // `EntityPtr pCopy = pEntity` + the script call held between the
        // list moves.
        if !is_teleport {
            // `pEntity->callback( "onLeavingCell" )`.
            self.entity_callback(entity, "onLeavingCell", true, None);
        }

        if self
            .entity_population
            .get(&entity)
            .is_some_and(|entity_rc| entity_rc.borrow().is_real)
        {
            // The replay face (`pReplayData_->deleteEntity`) is the
            // recording cluster's.
            // `realEntities_.remove( pEntity )`: off the cell's real
            // list — the space's entity list keeps it (it stays here as
            // a ghost).
            // `pEntity->offload( pChannel, isTeleport )`:
            // `convertRealToGhost`'s stream, then the ghost half.
            let data = write_real_data_to_stream(self, entity, is_teleport)
                .expect("the real's offload data is always writable");
            self.onload_outbox.push(OnloadWire {
                dest: destination,
                src: self.address,
                space_id: self.space_of(entity),
                entity,
                is_teleport,
                data,
            });

            // `pRealChannel_ = pChannel` +
            // `nextRealAddr_ = pRealChannel_->addr()` — the seam the
            // sign-off rides ("it will send a ghostSetReal back to this
            // app, so we better be ready for it").
            self.entity_population
                .get(&entity)
                .expect("checked above")
                .borrow_mut()
                .next_real_addr = Some(destination);

            // `offloadReal()` → `pReal_->destroy( &nextRealAddr_ )`:
            // every haunt but the destination hears where the real is
            // going; the real part dies here.
            self.send_ghost_set_next_real(entity, destination);

            let entity_rc = self
                .entity_population
                .get(&entity)
                .expect("checked above")
                .clone();
            {
                let mut borrowed = entity_rc.borrow_mut();
                // `MF_ASSERT( !this->isReal() )` held; the port's real
                // half is the flag plus the haunt bookkeeping the real
                // owned.
                borrowed.is_real = false;
                // The real-only properties drop (`properties_.erase(
                // propCountGhost() .. )`): none in the port.
            }

            // `this->relocated()` — the extras notification; a ghosted
            // passenger alights from a vehicle this space no longer
            // holds as real.
            self.relocate_entity_extras(&entity_rc);

            // `pEntity->callback( "onLeftCell" )` (`cell.cpp:222`).
            self.entity_callback(entity, "onLeftCell", true, None);
        }
        let _ = spaces;
    }

    /// `Entity::onload` (`entity.cpp:4656`): the destination's receive
    /// face — the ghost here becomes the real, from the stream
    /// [`write_real_data_to_stream`] wrote. The `bad_haunt_addr` is the
    /// failed teleport's origin: its haunt is skipped instead of
    /// notified. Answers `convertGhostToReal`'s outcome.
    pub fn onload_entity(
        &mut self,
        spaces: &mut BTreeMap<SpaceId, Space>,
        wire: &OnloadWire,
        bad_haunt_addr: Option<u32>,
    ) -> bool {
        let Some(entity_rc) = self.entity_population.get(&wire.entity).cloned() else {
            // MF_ASSERT( !this->isReal() ) reached through the
            // population; a stray onload for an unknown id has no
            // entity to convert.
            return false;
        };
        {
            let borrowed = entity_rc.borrow();
            debug_assert!(!borrowed.is_real, "onload converts a ghost");
        }
        let mut stream = BinaryStream::from_slice(&wire.data);

        // The id the writer put first; the reference's channel filter
        // consumes it on the way in, the port carries it in the wire.
        match stream.read_u32() {
            Ok(id) => debug_assert_eq!(id, wire.entity, "the same entity"),
            Err(_) => return false,
        }

        // `data >> teleportFailure`.
        let teleport_failure = stream.read_bool().unwrap_or(false);

        // `this->callback( "onEnteringCell" )`.
        self.entity_callback(wire.entity, "onEnteringCell", true, None);

        // `convertGhostToReal( data, pBadHauntAddr )`
        // (`entity.cpp:4707`): the script is locked out for the
        // conversion, the counter bumps first, the ghost's real
        // channel dies, then the body reads.
        self.callbacks_permitted(false);
        entity_rc.borrow_mut().num_times_real_offloaded += 1;
        entity_rc.borrow_mut().real_addr = None;

        // `readRealDataFromStreamForOnloadInternal`: the real-only
        // properties (none in the port), then the RealProps token.
        let ok = self.read_real_data_from_stream(&mut stream, &entity_rc, bad_haunt_addr);
        if !ok {
            return false;
        }

        // `this->cell().addRealEntity( this, true )` — the ghost entry
        // becomes the real; it already sits on the space's list.
        entity_rc.borrow_mut().is_real = true;

        // `this->relocated()`.
        self.relocate_entity_extras(&entity_rc);

        // The high-priority-buffered `onEnteredCell`
        // (`entity.cpp:4737-4741`).
        self.callback_buffer.enable_high_priority();
        self.entity_callback(wire.entity, "onEnteredCell", true, None);
        self.callback_buffer.disable_high_priority();

        // `Entity::callbacksPermitted( true )`.
        self.callbacks_permitted(true);

        // `Entity::population().onEntityAdd` — the passenger observers.
        self.entity_entered_population(wire.entity);

        // `if (teleportFailure)` (`entity.cpp:4729`): the haunt to the
        // failed destination goes, then `onTeleportFailure`.
        if teleport_failure {
            let src = bad_haunt_addr.unwrap_or(wire.src);
            if let Some(entity) = self.entity_population.get(&wire.entity) {
                let mut borrowed = entity.borrow_mut();
                borrowed.haunts.retain(|haunt| haunt.addr != src);
            }
            self.entity_callback(wire.entity, "onTeleportFailure", true, None);
        }
        let _ = spaces;
        true
    }

    /// `RealEntity::readOffloadData` (`real_entity.cpp:312`) over
    /// [`write_real_data_to_stream`]'s body: the channel and base
    /// addresses, the velocity block, the space and teleport flags, the
    /// haunt list (signed on or told off), the controller id, the
    /// replay block, and the controllers themselves. Answers whether the
    /// read ran out clean; the reference's `needsPhysicsCorrection` arm
    /// never fires (the port's positions are always valid).
    fn read_real_data_from_stream(
        &mut self,
        stream: &mut BinaryStream,
        entity_rc: &Rc<RefCell<Entity>>,
        bad_haunt_addr: Option<u32>,
    ) -> bool {
        // `readRealDataFromStreamForOnloadInternal`: the real-only
        // properties (none in the port) then `TOKEN_CHECK( data,
        // "RealProps" )` — the writer's four tag bytes.
        let token = match stream.read_u32() {
            Ok(value) => value,
            Err(_) => return false,
        };
        debug_assert_eq!(token, u32::from_le_bytes(*b"Real"), "the offload token");
        // `pChannel_->initFromStream( data, entity_.baseAddr() )` —
        // the old real's address, then the base's.
        let old_real_addr = match stream.read_u32() {
            Ok(value) => value,
            Err(_) => return false,
        };
        let base_ip = match stream.read_u32() {
            Ok(value) => value,
            Err(_) => return false,
        };
        let base_port = match stream.read_u16() {
            Ok(value) => value,
            Err(_) => return false,
        };
        let mut velocity = [0.0f32; 3];
        for component in &mut velocity {
            *component = match stream.read_f32() {
                Ok(value) => value,
                Err(_) => return false,
            };
        }
        let _should_auto_backup = stream.read_u8().unwrap_or(0);
        let old_space_id = stream.read_u32().unwrap_or(0);
        let is_teleported = stream.read_bool().unwrap_or(false);

        let entity_id = entity_rc.borrow().id;
        let space_id = entity_rc.borrow().space_id;
        let num_times = entity_rc.borrow().num_times_real_offloaded;
        {
            let mut borrowed = entity_rc.borrow_mut();
            borrowed.velocity = velocity;
            borrowed.base_addr = Some((base_ip, base_port));
        }
        let has_changed_space = old_space_id != space_id;

        // The haunt list: sign each reachable ghost onto this app, or
        // tell it the ghost is done after a space change or teleport.
        let num_haunts = stream.read_u32().unwrap_or(0);
        let mut addresses = Vec::new();
        for _ in 0..num_haunts {
            match stream.read_u32() {
                Ok(addr) => addresses.push(addr),
                Err(_) => return false,
            }
        }
        // `MF_ASSERT( numHaunts > 0 )`: the writer always sends at
        // least its own address.
        debug_assert!(!addresses.is_empty());
        let owner = self.address;
        let are_we_haunted = addresses.contains(&owner);
        // MF_ASSERT( areWeHaunted ): "they should not have offloaded
        // the entity without creating a ghost here first" — the offload
        // checker's own guarantee; the port reports instead.
        if !are_we_haunted {
            return false;
        }
        for addr in addresses {
            if addr == owner {
                continue;
            }
            if Some(addr) == bad_haunt_addr {
                // WARNING "Not notifying app %s because of failed
                // teleport for %u"
                self.ghost_property_notes.push(format!(
                    "RealEntity::readOffloadData: failed teleport haunt {addr} for {entity_id}"
                ));
                continue;
            }
            if self.dead_channels.contains(&addr) {
                // WARNING "Not notifying ghost for %u on dead app %s"
                self.ghost_property_notes.push(format!(
                    "RealEntity::readOffloadData: dead haunt {addr} for {entity_id}"
                ));
                continue;
            }
            if !has_changed_space && !is_teleported {
                // `this->addHaunt( *pAppChannel )` + the sign-off.
                entity_rc.borrow_mut().haunts.push(Haunt {
                    addr,
                    creation_time: self.time,
                });
                self.ghost_outbox.push(GhostWire {
                    dest: addr,
                    src: owner,
                    space_id,
                    entity: entity_id,
                    message: GhostMessage::SetReal {
                        num_times_real_offloaded: num_times,
                        owner,
                    },
                });
            } else {
                // `addDelGhostMessage( pAppChannel->bundle() )`.
                self.ghost_outbox.push(GhostWire {
                    dest: addr,
                    src: owner,
                    space_id,
                    entity: entity_id,
                    message: GhostMessage::Del,
                });
            }
        }

        // `data >> controlledBy_`.
        let controlled_by = stream.read_u32().unwrap_or(0);
        entity_rc.borrow_mut().controlled_by = controlled_by;
        // `data >> entity_.profiler()` + `adjustLoadForOffload`: the
        // load bookkeeping has no port face.
        // The packed replay block (`readBasePropertiesExposedForReplay
        // FromStream`).
        let _replay_len = match stream.read_packed_int() {
            Ok(value) => value,
            Err(_) => return false,
        };
        // `readRealControllersFromStream`: the trailing block.
        let controllers = stream.unread_bytes().to_vec();
        self.read_real_controllers_from_stream(entity_id, &controllers);
        // `startRealControllers` ran inside `convertGhostToReal`'s body
        // (`readRealDataFromStreamForOnloadInternal`'s tail).
        let entity_rc2 = Rc::clone(entity_rc);
        start_real_controllers(self, &entity_rc2);

        // `entity_.readRealControllersFromStream` already consumed the
        // tail; `periodsWithoutWitness_`/`recordingSpaceEntryID_` ride
        // the port's entity fields (the recording face is the replay
        // cluster's).
        let _ = old_real_addr;
        // The reference's return is `needsPhysicsCorrection`
        // (`isTeleported && localPosition_ == INVALID_POSITION`); the
        // port's positions are always valid, so the caller only needs
        // the read's own outcome.
        true
    }

    /// The entity's space, for the wire.
    fn space_of(&self, entity: u32) -> SpaceId {
        self.entity_population
            .get(&entity)
            .map(|entity_rc| entity_rc.borrow().space_id)
            .expect("the offload runs for a live entity")
    }

    /// `Cell::checkOffloadsAndGhosts` (`cell.cpp:174`): run the offload
    /// checker over our cell of `space_id`. The return is the cell's
    /// `isReadyForDeletion` (the port's face: reals gone, space entities
    /// gone, removed, no buffered messages, no pending acks).
    pub fn check_offloads_and_ghosts(
        &mut self,
        spaces: &mut BTreeMap<SpaceId, Space>,
        space_id: SpaceId,
    ) -> bool {
        // If this space is shutting down, don't offload any entities so
        // that they are not lost when the cells are shut down.
        let shutting_down = spaces.get(&space_id).is_some_and(Space::is_shutting_down);
        if !shutting_down {
            let mut checker = OffloadChecker::new(space_id);
            checker.run(self, spaces);
            // `sendOffloads` — the offload cluster's face; the list only
            // collects here.
        }
        // `Cell::isReadyForDeletion`: the buffered-entity and
        // buffered-input queues are the buffering cluster's (vacuously
        // empty here) and `pendingAcks_` has no port face yet.
        let empty = spaces
            .get(&space_id)
            .is_none_or(|space| space.space_entities().is_empty());
        let reals_empty = spaces.get(&space_id).is_none_or(|space| {
            space
                .space_entities()
                .iter()
                .all(|entity| !entity.borrow().is_real)
        });
        self.cells
            .iter()
            .any(|cell| cell.borrow().info.space_id == space_id && cell.borrow().is_removed)
            && empty
            && reals_empty
            && self.buffered_ghost_messages.is_empty()
    }
}

impl CellApp {
    /// `Entity::isOffloadingTo` (`entity.cpp:1952`): the entity was
    /// offloaded to the destination but the onload confirmation has
    /// not landed yet — the ghost's own real address already names the
    /// destination too.
    pub fn entity_is_offloading_to(&self, entity: u32, addr: u32) -> bool {
        self.entity_population
            .get(&entity)
            .is_some_and(|entity_rc| {
                let borrowed = entity_rc.borrow();
                borrowed.real_addr.is_some()
                    && borrowed.real_addr == borrowed.next_real_addr
                    && borrowed.next_real_addr == Some(addr)
            })
    }

    /// `Entity::shouldBufferMessagesFrom` (`entity.cpp:7838`): a ghost
    /// with no real channel never buffers (reals and zombie ghosts);
    /// one mid-switch buffers everything but the next real, and an
    /// settled one everything but its current real.
    pub fn should_buffer_messages_from(&self, entity: u32, addr: u32) -> bool {
        debug_assert!(addr != 0, "NONE is not a sender");
        let Some(entity_rc) = self.entity_population.get(&entity) else {
            return false;
        };
        let borrowed = entity_rc.borrow();
        let Some(real_addr) = borrowed.real_addr else {
            // Reals and zombie ghosts don't buffer.
            return false;
        };
        match borrowed.next_real_addr {
            // Buffer messages not from our next real.
            Some(next_real_addr) => next_real_addr != addr,
            // Otherwise, buffer messages not from our current real.
            None => real_addr != addr,
        }
    }

    /// `BufferedGhostMessages::hasMessagesFor`
    /// (`buffered_ghost_messages.cpp:64`): whether anything is held
    /// for the entity from the given address.
    fn has_ghost_messages_for(&self, entity: u32, addr: u32) -> bool {
        self.buffered_ghost_messages
            .get(&entity)
            .is_some_and(|held| {
                held.messages
                    .iter()
                    .chain(held.delayed.iter().flatten())
                    .any(|message| message.entity == entity && message.src == addr)
            })
    }

    /// `BufferedGhostMessages::isDelayingMessagesFor` (line 82): the
    /// port's single queue per ghost stands in for the per-address
    /// lookup.
    fn is_delaying_messages_for(&self, entity: u32, _addr: u32) -> bool {
        self.buffered_ghost_messages
            .get(&entity)
            .is_some_and(|held| held.is_delaying())
    }

    /// The ghost channel's deliver face: `CellApp::processMessage`
    /// dispatching the kind of message the bundle carries. The six
    /// ghost-domain entity messages run the routing ladder first
    /// (`EntityMessageHandler::handleMessage`); `createGhost` is not
    /// an entity message — `Space::createGhost`'s own buffering face
    /// handles it.
    pub fn deliver_ghost(
        &mut self,
        spaces: &mut BTreeMap<SpaceId, Space>,
        wire: &GhostWire,
    ) -> DeliverGhost {
        let message = wire.message.clone();
        if !matches!(message, GhostMessage::Create(_)) {
            // `EntityMessageRoutingState` over this app, this wire.
            let state = EntityMessageRoutingState {
                reality: Some(EntityReality::GhostOnly),
                entity_exists: self.entity_population.contains_key(&wire.entity),
                entity_is_real: self
                    .entity_population
                    .get(&wire.entity)
                    .is_some_and(|entity| entity.borrow().is_real),
                entity_is_offloading_to_sender: self.entity_is_offloading_to(wire.entity, wire.src),
                entity_has_witness: self
                    .entity_population
                    .get(&wire.entity)
                    .is_some_and(|entity| entity.borrow().has_witness),
                should_buffer_from_sender: self.should_buffer_messages_from(wire.entity, wire.src),
                delaying_for_sender: self.is_delaying_messages_for(wire.entity, wire.src),
                has_messages_for_sender: self.has_ghost_messages_for(wire.entity, wire.src),
                is_subsequence_start: is_subsequence_start(&message),
                // `shouldBufferIfTickPending_` and `nextTickPending`:
                // the tick-budget buffering cluster's face — not this
                // cluster's.
                should_buffer_if_tick_pending: false,
                next_tick_pending: false,
            };
            match classify_entity_message(&state) {
                EntityMessageDisposition::DropDestroyedGhost => {
                    // WARNING "Dropping ghost message ... ghost was
                    // destroyed while in-flight"; `sendFailure` is the
                    // reply cluster's face.
                    self.ghost_property_notes.push(format!(
                        "EntityMessageHandler: dropped ghost message for {} from {}, destroyed while in-flight",
                        wire.entity, wire.src
                    ));
                    return DeliverGhost::Dropped;
                }
                EntityMessageDisposition::BufferGhostMessage => {
                    // WARNING "Buffered ghost message for entity %u"
                    self.ghost_property_notes.push(format!(
                        "EntityMessageHandler: buffered ghost message for {} from {}",
                        wire.entity, wire.src
                    ));
                    self.buffer_ghost_message(BufferedGhostMessage {
                        src: wire.src,
                        entity: wire.entity,
                        space_id: wire.space_id,
                        delayed: false,
                        message: message.clone(),
                    });
                    return DeliverGhost::Buffered;
                }
                // The forward, witness-drop, and tick-defer side
                // effects are other clusters' faces; a ghost-domain
                // wire cannot reach them.
                _ => {}
            }
        }
        match &message {
            GhostMessage::Create(data) => {
                let Some((entity, payload)) = split_create_payload(data) else {
                    // The stream carries no id: the reference's message
                    // header could not name an entity either.
                    return DeliverGhost::Create(CreateGhostOutcome::BadStream);
                };
                DeliverGhost::Create(self.create_ghost_message(
                    spaces,
                    wire.space_id,
                    wire.src,
                    entity,
                    payload.as_slice(),
                ))
            }
            GhostMessage::Del => DeliverGhost::Del(self.del_ghost(spaces, wire.entity)),
            GhostMessage::Position {
                pos,
                dir,
                is_on_ground,
                update_number,
            } => DeliverGhost::Position(self.ghost_position_update(
                spaces,
                wire.entity,
                *pos,
                *dir,
                *is_on_ground,
                *update_number,
            )),
            GhostMessage::VolatileInfo(info) => {
                DeliverGhost::VolatileInfo(self.ghost_volatile_info(wire.entity, *info))
            }
            GhostMessage::Property { index, data } => {
                DeliverGhost::Property(self.ghosted_data_update(wire.entity, *index, data))
            }
            GhostMessage::SetReal {
                num_times_real_offloaded,
                owner,
            } => DeliverGhost::SetReal(self.ghost_set_real(
                spaces,
                wire.entity,
                *num_times_real_offloaded,
                *owner,
            )),
            GhostMessage::SetNextReal { next_real_addr } => DeliverGhost::SetNextReal(
                self.ghost_set_next_real(spaces, wire.entity, *next_real_addr),
            ),
        }
    }
}

/// `OffloadChecker` (`offload_checker.cpp`): the per-check bookkeeping —
/// the offload list the entity checks filled and the ghost-deletion
/// budget.
pub struct OffloadChecker {
    /// The space (its cell) this checker ran over.
    pub space_id: SpaceId,
    /// `offloadList_`: the (entity, destination) pairs
    /// `checkEntityForOffload` collected.
    pub offload_list: Vec<(u32, u32)>,
    /// `numGhostsDeleted_`.
    pub num_ghosts_deleted: u32,
}

impl OffloadChecker {
    /// The constructor: nothing collected, nothing deleted.
    pub fn new(space_id: SpaceId) -> Self {
        Self {
            space_id,
            offload_list: Vec::new(),
            num_ghosts_deleted: 0,
        }
    }

    /// Nothing pending (the return of `sendOffloads`' tail).
    pub fn is_empty(&self) -> bool {
        self.offload_list.is_empty()
    }

    /// `addToOffloads`.
    pub fn add_to_offloads(&mut self, entity: u32, destination: u32) {
        self.offload_list.push((entity, destination));
    }

    /// `canDeleteMoreGhosts`: `numGhostsDeleted_ <= MAX` (the reference
    /// says `<=` — one deletion over budget still goes).
    pub fn can_delete_more_ghosts(&self, app: &CellApp) -> bool {
        self.num_ghosts_deleted <= app.max_ghosts_to_delete
    }

    /// `addDeletedGhost`.
    pub fn add_deleted_ghost(&mut self) {
        self.num_ghosts_deleted += 1;
    }

    /// `run` (`offload_checker.cpp:43`): every real of the cell gets its
    /// `EntityGhostMaintainer::check()`.
    pub fn run(&mut self, app: &mut CellApp, spaces: &mut BTreeMap<SpaceId, Space>) {
        if spaces
            .get(&self.space_id)
            .is_some_and(Space::is_shutting_down)
        {
            return;
        }
        let Some(space) = spaces.get(&self.space_id) else {
            return;
        };
        let reals: Vec<u32> = space
            .space_entities()
            .iter()
            .filter(|entity| entity.borrow().is_real)
            .map(|entity| entity.borrow().id)
            .collect();
        for entity in reals {
            let mut maintainer = GhostMaintainer::new(self.space_id, entity);
            maintainer.check(app, spaces, self);
        }
        // `sendOffloads` (`offload_checker.cpp:74`): the collected pairs
        // go out in check order.
        let offloads = self.offload_list.clone();
        for (entity, destination) in offloads {
            app.offload_entity(spaces, entity, destination, false);
        }
    }
}

/// `EntityGhostMaintainer` (`entity_ghost_maintainer.cpp`): one real's
/// ghost pass.
pub struct GhostMaintainer {
    /// The space the haunts live in.
    pub space_id: SpaceId,
    /// `pEntity_`'s id.
    pub entity: u32,
    /// `ownAddress_`.
    pub own_address: u32,
    /// `pOffloadDestination_`: the destination `checkEntityForOffload`
    /// picked, when any.
    pub offload_destination: Option<u32>,
    /// `numGhostsCreated_` this pass.
    pub num_ghosts_created: u32,
}

impl GhostMaintainer {
    /// The constructor.
    pub fn new(space_id: SpaceId, entity: u32) -> Self {
        Self {
            space_id,
            entity,
            own_address: 0,
            offload_destination: None,
            num_ghosts_created: 0,
        }
    }

    /// `check()` (`entity_ghost_maintainer.cpp:58`): the offload gate,
    /// the mark/unmark/delete sequence, and the MF_ASSERT on the counts.
    pub fn check(
        &mut self,
        app: &mut CellApp,
        spaces: &mut BTreeMap<SpaceId, Space>,
        checker: &mut OffloadChecker,
    ) {
        self.own_address = app.address;
        let should_offload = app.cells.iter().any(|cell| {
            cell.borrow().info.space_id == self.space_id && cell.borrow().should_offload
        });
        if should_offload {
            self.check_entity_for_offload(app, spaces, checker);
        }

        // Mark all the haunts, then unmark the valid ones; the invalid
        // stay marked and are deleted.
        let does_offload_destination_have_ghost = self.mark_haunts(app);
        let hysteresis_area = self.create_or_unmark_required_haunts(app, spaces);
        debug_assert!(
            self.offload_destination.is_none()
                || does_offload_destination_have_ghost
                || self.num_ghosts_created == 1
        );
        let _ = hysteresis_area;
        self.delete_marked_haunts(app, checker);
    }

    /// `checkEntityForOffload` (`entity_ghost_maintainer.cpp:87`): where
    /// the entity really wants to live. The send side is the offload
    /// cluster's — here the pair only lands in the offload list.
    fn check_entity_for_offload(
        &mut self,
        app: &mut CellApp,
        spaces: &mut BTreeMap<SpaceId, Space>,
        checker: &mut OffloadChecker,
    ) {
        let position = app
            .entity_population
            .get(&self.entity)
            .map(|entity| entity.borrow().position)
            .unwrap_or_default();
        let Some(space) = spaces.get(&self.space_id) else {
            return;
        };
        let home_cell = space
            .cells()
            .find(|info| info.contains(position[0], position[2]));
        let Some(home) = home_cell else {
            // Don't offload to ourselves — no cell holds the position.
            return;
        };
        if home.app_addr == self.own_address {
            // Don't offload to ourselves.
            return;
        }
        if home.is_delete_pending {
            // Don't offload to a cell that is being deleted.
            return;
        }
        if app.dead_channels.contains(&home.app_addr) {
            // Don't offload if other cell has failed or died.
            return;
        }
        // OK to offload now.
        self.offload_destination = Some(home.app_addr);
        checker.add_to_offloads(self.entity, home.app_addr);
    }

    /// `markHaunts` (`entity_ghost_maintainer.cpp:129`): every haunt's
    /// channel marked, and whether the offload destination already has
    /// a haunt.
    fn mark_haunts(&mut self, app: &mut CellApp) -> bool {
        let mut does_offload_destination_have_ghost = false;
        let haunts = app
            .entity_population
            .get(&self.entity)
            .map(|entity| entity.borrow().haunts.clone())
            .unwrap_or_default();
        for haunt in haunts {
            if self.offload_destination == Some(haunt.addr) {
                does_offload_destination_have_ghost = true;
            }
            *app.channel_marks.entry(haunt.addr).or_insert(0) = 1;
        }
        does_offload_destination_have_ghost
    }

    /// `createOrUnmarkRequiredHaunts` (`entity_ghost_maintainer.cpp:175`):
    /// the interest rect over the space's cells; every intersecting
    /// remote cell gets `visit()`ed. Returns the hysteresis area.
    fn create_or_unmark_required_haunts(
        &mut self,
        app: &mut CellApp,
        spaces: &mut BTreeMap<SpaceId, Space>,
    ) -> (f32, f32, f32, f32) {
        let entity = app
            .entity_population
            .get(&self.entity)
            .cloned()
            .expect("the maintainer only runs for reals");
        let position = entity.borrow().position;
        // Entities with an appeal radius have to ghost more — the port
        // carries no per-type appeal radius (0).
        let appeal_radius = 0.0;
        let inflate = app.ghost_distance + appeal_radius;
        let hysteresis_area = (
            position[0] - inflate,
            position[2] - inflate,
            position[0] + inflate,
            position[2] + inflate,
        );
        let interest_area = (
            hysteresis_area.0 - GHOST_FUDGE,
            hysteresis_area.1 - GHOST_FUDGE,
            hysteresis_area.2 + GHOST_FUDGE,
            hysteresis_area.3 + GHOST_FUDGE,
        );

        let Some(space) = spaces.get(&self.space_id) else {
            return hysteresis_area;
        };
        for cell in space.cells() {
            // `visitRect` only reaches cells that overlap the interest
            // area.
            let (min_x, min_z, max_x, max_z) = cell.rect;
            if min_x > interest_area.2
                || max_x < interest_area.0
                || min_z > interest_area.3
                || max_z < interest_area.1
            {
                continue;
            }
            self.visit(app, &entity, cell, hysteresis_area);
        }
        hysteresis_area
    }

    /// `visit` (`entity_ghost_maintainer.cpp:196`): one cell in the
    /// rect — unmark its haunt, or create the ghost.
    fn visit(
        &mut self,
        app: &mut CellApp,
        entity: &Rc<RefCell<Entity>>,
        cell: &CellInfo,
        hysteresis_area: (f32, f32, f32, f32),
    ) {
        // Discard it if it is ourself.
        if cell.app_addr == self.own_address {
            return;
        }
        // Do not have ghosts on cells that are about to be deleted.
        if cell.is_delete_pending {
            return;
        }
        // If it has been marked as an existing haunt then unmark it and
        // bail.
        if app.channel_marks.get(&cell.app_addr).copied().unwrap_or(0) == 1 {
            app.channel_marks.insert(cell.app_addr, 0);
            return;
        }
        // Do not create a ghost if we are about to be offloaded. Let the
        // destination do this.
        if let Some(destination) = self.offload_destination
            && destination != cell.app_addr
        {
            return;
        }
        // If we are not far enough in then toss it too (hysteresis
        // check).
        if !cell.intersects(hysteresis_area) {
            return;
        }

        // Otherwise we should create a new ghost.
        entity.borrow_mut().haunts.push(Haunt {
            addr: cell.app_addr,
            creation_time: app.time,
        });
        let data = write_ghost_data_to_stream(app, self.entity)
            .expect("the real's ghost data is always writable");
        app.ghost_outbox.push(GhostWire {
            dest: cell.app_addr,
            src: self.own_address,
            space_id: self.space_id,
            entity: self.entity,
            message: GhostMessage::Create(data),
        });
        self.num_ghosts_created += 1;
    }

    /// `deleteMarkedHaunts` (`entity_ghost_maintainer.cpp:246`): the
    /// still-marked haunts go, subject to the budget and the new-ghost
    /// keeps.
    fn delete_marked_haunts(&mut self, app: &mut CellApp, checker: &mut OffloadChecker) {
        // `secondsToTicks( minGhostLifespan(), 0 )`.
        let minimum_ghost_lifespan = MIN_GHOST_LIFESPAN_IN_SECONDS * app.update_hertz;
        let new_real_keep_ghost_period = NEW_REAL_KEEP_GHOST_PERIOD_IN_SECONDS * app.update_hertz;
        let game_time = app.time;
        let creation_time = app
            .entity_population
            .get(&self.entity)
            .map(|entity| entity.borrow().creation_time)
            .unwrap_or_default();
        let haunts = app
            .entity_population
            .get(&self.entity)
            .map(|entity| entity.borrow().haunts.clone())
            .unwrap_or_default();

        let mut kept = Vec::new();
        for haunt in haunts {
            let should_del_ghost = checker.can_delete_more_ghosts(app)
                // Keep the ghost if we're offloading there.
                && Some(haunt.addr) != self.offload_destination
                // Keep the ghost if the real entity is new.
                && game_time.wrapping_sub(creation_time)
                    > new_real_keep_ghost_period
                // Keep the ghost if the ghost is new.
                && game_time.wrapping_sub(haunt.creation_time)
                    > minimum_ghost_lifespan;
            let marked = app.channel_marks.get(&haunt.addr).copied().unwrap_or(0) != 0;
            if marked && should_del_ghost {
                // Only bother telling it if it hasn't failed.
                if !app.dead_channels.contains(&haunt.addr) {
                    app.ghost_outbox.push(GhostWire {
                        dest: haunt.addr,
                        src: self.own_address,
                        space_id: self.space_id,
                        entity: self.entity,
                        message: GhostMessage::Del,
                    });
                    checker.add_deleted_ghost();
                }
                // delHaunt: the haunt itself goes.
            } else {
                kept.push(haunt);
            }
            // Always clear the mark for the next user.
            app.channel_marks.insert(haunt.addr, 0);
        }
        if let Some(entity) = app.entity_population.get(&self.entity) {
            entity.borrow_mut().haunts = kept;
        }
    }
}

// ---------------------------------------------------------------------------
// Section: tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cellapp_real_entity::{
        ADD_ENTITY_DATA_IS_ON_GROUND, Controller, ControllerKind, EntityScript,
    };
    use crate::cellapp_space::{Cell, CellInfoNode};

    const SID: SpaceId = 3;
    const TYPE: u32 = 7;
    const A_ADDR: u32 = 10;
    const B_ADDR: u32 = 20;
    const C_ADDR: u32 = 30;
    const D_ADDR: u32 = 40;

    /// The topology the two-cell tests share: ours `[0..50]` on the x
    /// edge, then neighbours at `[50..100]` (B) and, where the test
    /// wants three, `[55..100]` (C) — the rects the caller slices.
    fn cell_infos(rects: &[(u32, f32, f32)]) -> Vec<CellInfo> {
        rects
            .iter()
            .enumerate()
            .map(|(index, (addr, min_x, max_x))| CellInfo {
                space_id: SID,
                app_addr: *addr,
                node: if index == 0 {
                    CellInfoNode::first()
                } else {
                    CellInfoNode::next(CellInfoNode::first())
                },
                rect: (*min_x, 0.0, *max_x, 50.0),
                is_delete_pending: false,
            })
            .collect()
    }

    /// A started CellApp with its own cell of the shared topology and
    /// the space carrying every cell of it.
    fn app_at(address: u32, cells: &[CellInfo]) -> (CellApp, BTreeMap<SpaceId, Space>) {
        let mut app = CellApp::with_address(address);
        app.start();
        app.entity_scripts
            .insert(TYPE, EntityScript::with_init(vec![]));
        let mut spaces = BTreeMap::new();
        let mut space = Space::new(SID);
        let own = cells
            .iter()
            .find(|info| info.app_addr == address)
            .cloned()
            .expect("the app owns one of the cells");
        let cell = Rc::new(RefCell::new(Cell {
            info: own,
            space_gone_count: 0,
            reuse_count: 0,
            should_offload: true,
            is_removed: false,
        }));
        space.update_geometry(&mut app, cells.to_vec());
        space.set_cell(cell.clone());
        app.cells.push(cell);
        spaces.insert(SID, space);
        (app, spaces)
    }

    /// The creation stream `initReal` reads: the fixed head, the
    /// position, the on-ground flag, the direction, the event number
    /// and the base address, closed by the witness byte.
    fn create_stream(id: u32, pos: [f32; 3]) -> Vec<u8> {
        let mut stream = Vec::new();
        stream.extend_from_slice(&id.to_le_bytes());
        stream.extend_from_slice(&TYPE.to_le_bytes());
        for value in pos {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.push(ADD_ENTITY_DATA_IS_ON_GROUND);
        for value in [0.25f32, 0.0, 0.0] {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.extend_from_slice(&42u32.to_le_bytes());
        stream.extend_from_slice(&7u32.to_le_bytes());
        stream.extend_from_slice(&6502u16.to_le_bytes());
        stream.push(b'-');
        stream
    }

    /// Deliver a maintainer wire into its destination CellApp — the
    /// channel send the reference's bundles make.
    fn deliver(
        app: &mut CellApp,
        spaces: &mut BTreeMap<SpaceId, Space>,
        wire: &GhostWire,
    ) -> CreateGhostOutcome {
        match app.deliver_ghost(spaces, wire) {
            DeliverGhost::Create(outcome) => outcome,
            DeliverGhost::Del(ok) => {
                if ok {
                    CreateGhostOutcome::Created
                } else {
                    CreateGhostOutcome::BadStream
                }
            }
            DeliverGhost::Position(ok) => {
                if ok {
                    CreateGhostOutcome::Created
                } else {
                    CreateGhostOutcome::BadStream
                }
            }
            DeliverGhost::VolatileInfo(ok) => {
                if ok {
                    CreateGhostOutcome::Created
                } else {
                    CreateGhostOutcome::BadStream
                }
            }
            DeliverGhost::Property(ok) => {
                if ok {
                    CreateGhostOutcome::Created
                } else {
                    CreateGhostOutcome::BadStream
                }
            }
            DeliverGhost::Dropped | DeliverGhost::Buffered => CreateGhostOutcome::BadStream,
            // The sign-offs collapse onto the same Created/BadStream
            // pair: the tests read the entity's own state.
            DeliverGhost::SetReal(outcome) => match outcome {
                GhostSetReal::Applied => CreateGhostOutcome::Created,
                GhostSetReal::Buffered => CreateGhostOutcome::BadStream,
                GhostSetReal::Missing => CreateGhostOutcome::UnknownType,
            },
            DeliverGhost::SetNextReal(ok) => {
                if ok {
                    CreateGhostOutcome::Created
                } else {
                    CreateGhostOutcome::BadStream
                }
            }
        }
    }

    /// A real just inside our cell's far edge, both cells inside the
    /// default ghost distance.
    fn two_cell_fixture() -> (
        CellApp,
        BTreeMap<SpaceId, Space>,
        CellApp,
        BTreeMap<SpaceId, Space>,
    ) {
        let cells = cell_infos(&[(A_ADDR, 0.0, 50.0), (B_ADDR, 50.0, 100.0)]);
        let (mut app_a, mut spaces_a) = app_at(A_ADDR, &cells);
        let (app_b, spaces_b) = app_at(B_ADDR, &cells);
        app_a.ghost_distance = 50.0;
        let outcome =
            app_a.create_real_entity(&mut spaces_a, SID, &create_stream(2, [40.0, 0.0, 25.0]));
        assert!(matches!(
            outcome,
            crate::cellapp_real_entity::CreateRealOutcome::Created(2)
        ));
        (app_a, spaces_a, app_b, spaces_b)
    }

    /// The two-cell layout plus the destination app: the real's
    /// offload target is one of the ghost-holding apps, so the landed
    /// real signs itself onto its old haunts exactly as
    /// `readOffloadData` does.
    #[allow(clippy::type_complexity)]
    fn three_cell_fixture() -> (
        CellApp,
        BTreeMap<SpaceId, Space>,
        CellApp,
        BTreeMap<SpaceId, Space>,
        CellApp,
        BTreeMap<SpaceId, Space>,
    ) {
        let cells = cell_infos(&[
            (A_ADDR, 0.0, 40.0),
            (B_ADDR, 40.0, 55.0),
            (C_ADDR, 55.0, 100.0),
        ]);
        let (mut app_a, mut spaces_a) = app_at(A_ADDR, &cells);
        let (app_b, spaces_b) = app_at(B_ADDR, &cells);
        let (app_c, spaces_c) = app_at(C_ADDR, &cells);
        app_a.ghost_distance = 50.0;
        let outcome =
            app_a.create_real_entity(&mut spaces_a, SID, &create_stream(2, [10.0, 0.0, 25.0]));
        assert!(matches!(
            outcome,
            crate::cellapp_real_entity::CreateRealOutcome::Created(2)
        ));
        (app_a, spaces_a, app_b, spaces_b, app_c, spaces_c)
    }

    /// The create wire with every section: the ghost's volatile block,
    /// a controller, and two stamps.
    fn rich_create_payload() -> (CellApp, BTreeMap<SpaceId, Space>, Vec<u8>) {
        let (mut app_a, mut spaces_a, app_b, spaces_b) = two_cell_fixture();
        {
            let entity = app_a.entity_population.get_mut(&2).unwrap();
            let mut borrowed = entity.borrow_mut();
            borrowed.volatile_info.position_priority = 30.0;
            borrowed.volatile_info.yaw_priority = 12.0;
            borrowed.volatile_info.pitch_priority = 5.0;
            borrowed.volatile_info.roll_priority = -1.0;
            borrowed.last_event_number = 7;
            borrowed.periods_without_witness = 3;
            borrowed.aoi_update_scheme_id = 2;
            borrowed.num_times_real_offloaded = 3;
            borrowed.is_on_ground = false;
            borrowed.controllers.container.insert(
                0x9000,
                Controller {
                    kind: ControllerKind::Passenger { vehicle_id: 4 },
                    user_arg: 77,
                },
            );
        }
        app_a.entity_properties.entry(2).or_default().extend([
            crate::cellapp_witness::ClientProperty { level: 0, stamp: 5 },
            crate::cellapp_witness::ClientProperty { level: 0, stamp: 9 },
        ]);
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        let wire = app_a.ghost_outbox.remove(0);
        let (entity, payload) = wire.split_create().expect("the create wire carries data");
        assert_eq!(entity, 2);
        (app_b, spaces_b, payload)
    }

    /// The offset of a four-byte token in a payload.
    fn token_at(payload: &[u8], token: &[u8]) -> usize {
        payload
            .windows(4)
            .position(|window| window == token)
            .expect("the token rides the stream")
    }

    /// The acceptance face: one check ghosts the real onto the adjacent
    /// cell and the delivered create leaves exactly one real here and
    /// one ghost there — a second check adds nothing.
    #[test]
    fn the_two_cells_hold_one_real_and_one_ghost() {
        let (mut app_a, mut spaces_a, mut app_b, mut spaces_b) = two_cell_fixture();

        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        assert_eq!(app_a.ghost_outbox.len(), 1);
        let wire = app_a.ghost_outbox[0].clone();
        assert_eq!(wire.dest, B_ADDR);
        assert_eq!(wire.src, A_ADDR);
        assert_eq!(
            app_a.entity_population[&2].borrow().haunts,
            vec![Haunt {
                addr: B_ADDR,
                creation_time: 0
            }]
        );

        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &wire),
            CreateGhostOutcome::Created
        );
        // The counts: one real on the home cell, one ghost on the
        // neighbour, nothing doubled.
        assert_eq!(app_a.entity_population.len(), 1);
        assert_eq!(app_b.entity_population.len(), 1);
        let ghost = app_b.entity_population[&2].borrow();
        assert!(!ghost.is_real);
        assert_eq!(ghost.position, [40.0, 0.0, 25.0]);
        assert_eq!(ghost.local_position, [40.0, 0.0, 25.0]);
        assert_eq!(ghost.direction, [0.25, 0.0, 0.0]);
        assert_eq!(ghost.base_addr, Some((7, 6502)));
        assert_eq!(ghost.real_addr, Some(A_ADDR));
        assert!(ghost.range_node.is_some(), "the ghost joins the range list");
        assert!(app_b.range_lists.contains_key(&SID));
        drop(ghost);

        // The ghost is watched through the same range list the
        // witnesses use — the acceptance counts stay right.
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        assert_eq!(
            app_a.ghost_outbox.len(),
            1,
            "the haunt unmarked, no second create"
        );
    }

    /// The ghost state round-trips: everything `writeGhostDataToStream`
    /// wrote is what `initGhost` reads back.
    #[test]
    fn the_ghost_stream_round_trips_the_state() {
        let (mut app_b, mut spaces_b, payload) = rich_create_payload();
        assert_eq!(
            app_b.create_ghost_message(&mut spaces_b, SID, A_ADDR, 2, &payload),
            CreateGhostOutcome::Created
        );

        let ghost = app_b.entity_population[&2].borrow();
        assert_eq!(ghost.volatile_info.position_priority, 30.0);
        assert_eq!(ghost.volatile_info.yaw_priority, 12.0);
        assert_eq!(ghost.volatile_info.pitch_priority, 5.0);
        assert_eq!(ghost.volatile_info.roll_priority, -1.0);
        assert_eq!(ghost.last_event_number, 7);
        assert_eq!(ghost.last_trimmed_event_number, 7);
        assert_eq!(ghost.periods_without_witness, 3);
        assert_eq!(ghost.aoi_update_scheme_id, 2);
        assert_eq!(ghost.num_times_real_offloaded, 3);
        assert!(!ghost.is_on_ground);
        assert_eq!(ghost.real_addr, Some(A_ADDR));
        let controller = ghost
            .controllers
            .container
            .get(&0x9000)
            .expect("the ghost controller rode the stream");
        // The passenger payload is zero bytes on the ghost wire
        // (`PassengerController::writeRealToStream`), so the vehicle
        // link starts detached at 0 and re-forms when the controller
        // starts.
        assert_eq!(controller.kind, ControllerKind::Passenger { vehicle_id: 0 });
        assert_eq!(controller.user_arg, 77);
        drop(ghost);
        let stamps: Vec<_> = app_b.entity_properties[&2]
            .iter()
            .map(|p| p.stamp)
            .collect();
        assert_eq!(stamps, [5, 9]);
    }

    /// `onGhostCreated` fires on the ghost's script when the type
    /// defines it.
    #[test]
    fn the_ghost_scripts_on_ghost_created() {
        let (mut app_a, mut spaces_a, mut app_b, mut spaces_b) = two_cell_fixture();
        let mut script = EntityScript::with_init(vec![]);
        script.methods.insert("onGhostCreated");
        app_b.entity_scripts.insert(TYPE, script);

        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        let wire = app_a.ghost_outbox.remove(0);
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &wire),
            CreateGhostOutcome::Created
        );
        assert!(
            app_b
                .script_calls
                .iter()
                .any(|call| call.entity == 2 && call.name == "onGhostCreated"),
            "the ghost told its script"
        );
    }

    /// A haunt that fell out of the hysteresis area goes, and the
    /// destination destroys its ghost — the counts stay one real, zero
    /// ghosts.
    #[test]
    fn the_ghost_that_fell_out_is_deleted() {
        let (mut app_a, mut spaces_a, mut app_b, mut spaces_b) = two_cell_fixture();
        app_a.time = 1000;
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        let wire = app_a.ghost_outbox.remove(0);
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &wire),
            CreateGhostOutcome::Created
        );

        // The ghost ages past the minimum lifespan (5 seconds at 10 Hz)
        // and the real past its 2-second grace, then the real walks out
        // of the neighbour's interest area.
        app_a.entity_population[&2].borrow_mut().position = [20.0, 0.0, 25.0];
        app_a.time = 1000 + 51;
        app_a.ghost_distance = 5.0;
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        assert_eq!(app_a.ghost_outbox.len(), 1, "the delGhost went out");
        let del = app_a.ghost_outbox.remove(0);
        assert_eq!(del.dest, B_ADDR);
        assert_eq!(del.message, GhostMessage::Del);
        assert!(app_a.entity_population[&2].borrow().haunts.is_empty());

        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &del),
            CreateGhostOutcome::Created
        );
        assert!(app_b.entity_population[&2].borrow().destroyed);
        assert_eq!(app_a.entity_population.len(), 1);
    }

    /// The keeps: a brand-new haunt and a brand-new real both hold
    /// their ghosts past the delete pass.
    #[test]
    fn the_new_haunt_and_the_new_real_keep_their_ghosts() {
        let (mut app_a, mut spaces_a, mut app_b, mut spaces_b) = two_cell_fixture();
        app_a.time = 1000;
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        let wire = app_a.ghost_outbox.remove(0);
        deliver(&mut app_b, &mut spaces_b, &wire);

        // The haunt is 5 ticks old — well inside the 5-second lifespan.
        app_a.time = 1005;
        app_a.ghost_distance = 5.0;
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        assert!(app_a.ghost_outbox.is_empty(), "the new haunt stayed");
        assert_eq!(app_a.entity_population[&2].borrow().haunts.len(), 1);

        // The real is brand new (creation now): even an aged haunt
        // stays while the real is inside its 2-second grace.
        app_a.entity_population[&2].borrow_mut().creation_time = 1005;
        app_a.time = 1000 + 51;
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        assert!(app_a.ghost_outbox.is_empty(), "the new real kept its ghost");
        assert_eq!(app_a.entity_population[&2].borrow().haunts.len(), 1);
    }

    /// `canDeleteMoreGhosts` says `<=`: with the budget at zero the
    /// first deletion of a pass still goes, and only the first.
    #[test]
    fn the_zero_budget_still_deletes_one_haunt() {
        let cells = cell_infos(&[
            (A_ADDR, 0.0, 40.0),
            (B_ADDR, 40.0, 55.0),
            (C_ADDR, 55.0, 100.0),
        ]);
        let (mut app_a, mut spaces_a) = app_at(A_ADDR, &cells);
        app_a.ghost_distance = 50.0;
        app_a.max_ghosts_to_delete = 0;
        assert!(matches!(
            app_a.create_real_entity(&mut spaces_a, SID, &create_stream(2, [35.0, 0.0, 25.0])),
            crate::cellapp_real_entity::CreateRealOutcome::Created(2)
        ));
        app_a.time = 1000;
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        assert_eq!(app_a.ghost_outbox.len(), 2, "both neighbours ghosted");
        app_a.ghost_outbox.clear();

        app_a.entity_population[&2].borrow_mut().position = [10.0, 0.0, 25.0];
        app_a.time = 1000 + 51;
        app_a.ghost_distance = 5.0;
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        let deletes: Vec<u32> = app_a
            .ghost_outbox
            .iter()
            .filter(|wire| matches!(wire.message, GhostMessage::Del))
            .map(|wire| wire.dest)
            .collect();
        assert_eq!(deletes, [B_ADDR], "the first haunt went, the second stayed");
        assert_eq!(
            app_a.entity_population[&2].borrow().haunts,
            vec![Haunt {
                addr: C_ADDR,
                creation_time: 1000
            }]
        );
    }

    /// A createGhost for an entity this CellApp already holds is
    /// buffered — the WARNING face — and the ghost is not doubled.
    #[test]
    fn a_duplicate_create_is_buffered_not_doubled() {
        let (mut app_a, mut spaces_a, mut app_b, mut spaces_b) = two_cell_fixture();
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        let wire = app_a.ghost_outbox.remove(0);
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &wire),
            CreateGhostOutcome::Created
        );

        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &wire),
            CreateGhostOutcome::Buffered
        );
        assert_eq!(app_b.entity_population.len(), 1);
        let held = app_b.buffered_ghost_messages[&2]
            .delayed
            .as_ref()
            .expect("an existing entity delayed the subsequence");
        let held = &held[0];
        assert_eq!(held.entity, 2);
        assert_eq!(held.src, A_ADDR);
        assert!(held.delayed, "an existing entity delayed the subsequence");
        assert!(app_b.buffered_ghost_messages[&2].messages.is_empty());
        assert!(
            matches!(held.message, GhostMessage::Create(_)),
            "the create itself is held"
        );
        // A create behind an already-buffered one rides the queue
        // instead of delaying its own subsequence.
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &wire),
            CreateGhostOutcome::Buffered
        );
        let queued = app_b.buffered_ghost_messages[&2]
            .delayed
            .as_ref()
            .expect("still delaying");
        assert_eq!(queued.len(), 2);
        assert!(
            !queued[1].delayed,
            "it rode the queue behind the first create"
        );
    }

    /// A bad create stream leaves the half-built ghost destroyed in the
    /// population — `initReal`'s failure shape.
    #[test]
    fn a_bad_stream_leaves_the_ghost_destroyed() {
        let (mut app_a, mut spaces_a, mut app_b, mut spaces_b) = two_cell_fixture();
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        let wire = app_a.ghost_outbox.remove(0);
        let (entity, mut payload) = wire.split_create().expect("the create wire carries data");
        payload.truncate(payload.len() - 1);
        assert_eq!(
            app_b.create_ghost_message(&mut spaces_b, SID, wire.src, entity, &payload),
            CreateGhostOutcome::BadStream
        );
        assert!(app_b.entity_population[&2].borrow().destroyed);
    }

    /// The delegate-space gate refuses the ghost wholesale.
    #[test]
    fn the_delegate_space_refuses_the_ghost() {
        let (mut app_a, mut spaces_a, mut app_b, mut spaces_b) = two_cell_fixture();
        app_b.use_game_delegate = true;
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        let wire = app_a.ghost_outbox.remove(0);
        let (entity, payload) = wire.split_create().expect("create data");
        assert_eq!(
            app_b.create_ghost_message(&mut spaces_b, SID, wire.src, entity, &payload),
            CreateGhostOutcome::Delegate
        );
        assert!(!app_b.entity_population.contains_key(&2));
    }

    /// The offload gate: a real sitting on the neighbour's cell
    /// offloads there and ghosts only the destination — the other
    /// adjacent cell gets nothing.
    #[test]
    fn the_offload_gate_suppresses_the_foreign_ghosts() {
        let cells = cell_infos(&[
            (A_ADDR, 0.0, 40.0),
            (B_ADDR, 40.0, 55.0),
            (C_ADDR, 55.0, 100.0),
        ]);
        let (mut app_a, mut spaces_a) = app_at(A_ADDR, &cells);
        app_a.ghost_distance = 50.0;
        // The real lives on B's rect: that is the offload destination.
        assert!(matches!(
            app_a.create_real_entity(&mut spaces_a, SID, &create_stream(2, [45.0, 0.0, 25.0])),
            crate::cellapp_real_entity::CreateRealOutcome::Created(2)
        ));

        let mut checker = OffloadChecker::new(SID);
        checker.run(&mut app_a, &mut spaces_a);
        assert_eq!(checker.offload_list, [(2, B_ADDR)]);
        let creates: Vec<u32> = app_a
            .ghost_outbox
            .iter()
            .filter(|wire| matches!(wire.message, GhostMessage::Create(_)))
            .map(|wire| wire.dest)
            .collect();
        assert_eq!(creates, [B_ADDR], "only the destination ghosted");
        assert_eq!(
            app_a.entity_population[&2].borrow().haunts,
            vec![Haunt {
                addr: B_ADDR,
                creation_time: 0
            }]
        );
    }

    /// A delete for a channel that failed still retires the haunt —
    /// only the telling is skipped ("only bother telling it if it
    /// hasn't failed").
    #[test]
    fn a_failed_channel_skips_the_telling_but_drops_the_haunt() {
        let (mut app_a, mut spaces_a, mut app_b, mut spaces_b) = two_cell_fixture();
        app_a.time = 1000;
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        let wire = app_a.ghost_outbox.remove(0);
        deliver(&mut app_b, &mut spaces_b, &wire);

        app_a.dead_channels.insert(B_ADDR);
        app_a.entity_population[&2].borrow_mut().position = [20.0, 0.0, 25.0];
        app_a.time = 1000 + 51;
        app_a.ghost_distance = 5.0;
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        assert!(
            app_a.ghost_outbox.is_empty(),
            "nothing told to the dead channel"
        );
        assert!(app_a.entity_population[&2].borrow().haunts.is_empty());
    }

    /// `isReadyForDeletion`: the removed cell of an emptied space is
    /// ready; anything still held is not.
    #[test]
    fn readiness_needs_the_removed_and_empty_cell() {
        let (mut app_a, mut spaces_a, mut app_b, mut spaces_b) = two_cell_fixture();
        assert!(!app_a.check_offloads_and_ghosts(&mut spaces_a, SID));
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        let wire = app_a.ghost_outbox.remove(0);
        deliver(&mut app_b, &mut spaces_b, &wire);
        app_a.entity_population[&2].borrow_mut().position = [20.0, 0.0, 25.0];
        app_a.time = 1000 + 51;
        app_a.ghost_distance = 5.0;
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        let del = app_a.ghost_outbox.remove(0);
        deliver(&mut app_b, &mut spaces_b, &del);

        let cell = app_a.cells[0].clone();
        cell.borrow_mut().is_removed = true;
        // The real still holds the space.
        assert!(!app_a.check_offloads_and_ghosts(&mut spaces_a, SID));
        {
            let space = spaces_a.get_mut(&SID).expect("space");
            let entity = space.space_entities()[0].clone();
            space.remove_entity(&entity, &mut app_a);
            app_a.entity_population.remove(&2).unwrap();
        }
        assert!(app_a.check_offloads_and_ghosts(&mut spaces_a, SID));
    }

    /// Every truncation of the create stream is refused, and each
    /// refusal leaves the half-built ghost destroyed in the population
    /// for the next attempt to revive — the full stream then lands.
    #[test]
    fn a_truncated_ghost_stream_is_refused_at_every_point() {
        let (mut app_b, mut spaces_b, payload) = rich_create_payload();
        for len in 0..payload.len() {
            assert_eq!(
                app_b.create_ghost_message(&mut spaces_b, SID, A_ADDR, 2, &payload[..len]),
                CreateGhostOutcome::BadStream,
                "len {len}"
            );
            // A stream too short for the type never enters the
            // population; everything after does, destroyed.
            if len >= 4 {
                assert!(app_b.entity_population[&2].borrow().destroyed, "len {len}");
            } else {
                assert!(!app_b.entity_population.contains_key(&2), "len {len}");
            }
        }
        assert_eq!(
            app_b.create_ghost_message(&mut spaces_b, SID, A_ADDR, 2, &payload),
            CreateGhostOutcome::Created
        );
        assert!(!app_b.entity_population[&2].borrow().destroyed);
    }

    /// A property count the port cannot read refuses the stream.
    #[test]
    fn a_nonzero_property_count_is_refused() {
        let (mut app_b, mut spaces_b, mut payload) = rich_create_payload();
        let at = token_at(&payload, b"GPro");
        payload[at + 4..at + 8].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(
            app_b.create_ghost_message(&mut spaces_b, SID, A_ADDR, 2, &payload),
            CreateGhostOutcome::BadStream
        );
    }

    /// A controller the port cannot decode refuses the stream.
    #[test]
    fn an_unknown_controller_code_is_refused() {
        let (mut app_b, mut spaces_b, mut payload) = rich_create_payload();
        let at = token_at(&payload, b"GCon");
        // count at +4, the controller id at +8, its type code at +12.
        payload[at + 12..at + 16].copy_from_slice(&9999u32.to_le_bytes());
        assert_eq!(
            app_b.create_ghost_message(&mut spaces_b, SID, A_ADDR, 2, &payload),
            CreateGhostOutcome::BadStream
        );
    }

    /// The tail must end the stream exactly (`MF_ASSERT` on the
    /// remaining length).
    #[test]
    fn a_trailing_byte_is_refused() {
        let (mut app_b, mut spaces_b, mut payload) = rich_create_payload();
        payload.push(0xFF);
        assert_eq!(
            app_b.create_ghost_message(&mut spaces_b, SID, A_ADDR, 2, &payload),
            CreateGhostOutcome::BadStream
        );
    }

    /// A type the app has no cell entity class for refuses the create.
    #[test]
    fn an_unknown_type_is_refused() {
        let (mut app_b, mut spaces_b, mut payload) = rich_create_payload();
        payload[0..4].copy_from_slice(&999u32.to_le_bytes());
        assert_eq!(
            app_b.create_ghost_message(&mut spaces_b, SID, A_ADDR, 2, &payload),
            CreateGhostOutcome::UnknownType
        );
    }

    /// A create for a space this app does not hold is refused.
    #[test]
    fn an_unknown_space_refuses_the_create() {
        let (mut app_b, mut spaces_b, payload) = rich_create_payload();
        assert_eq!(
            app_b.create_ghost_message(&mut spaces_b, 999, A_ADDR, 2, &payload),
            CreateGhostOutcome::NoSpace
        );
    }

    /// The del wire's refusal faces: an unknown entity, an already
    /// destroyed one, and a create wire too short to carry an id.
    #[test]
    fn the_del_wire_refusals() {
        let (mut app_b, mut spaces_b, payload) = rich_create_payload();
        assert!(!app_b.del_ghost(&mut spaces_b, 99), "unknown entity");
        assert_eq!(
            app_b.create_ghost_message(&mut spaces_b, SID, A_ADDR, 2, &payload),
            CreateGhostOutcome::Created
        );
        app_b.entity_population[&2].borrow_mut().destroyed = true;
        assert!(!app_b.del_ghost(&mut spaces_b, 2), "already destroyed");

        // The deliver face: a del wire for a missing entity reports the
        // refusal, and a create wire without four bytes of id does too.
        let del = GhostWire {
            dest: B_ADDR,
            src: A_ADDR,
            space_id: SID,
            entity: 99,
            message: GhostMessage::Del,
        };
        assert!(
            del.split_create().is_none(),
            "a del carries no create payload"
        );
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &del),
            CreateGhostOutcome::BadStream
        );
        let short = GhostWire {
            dest: B_ADDR,
            src: A_ADDR,
            space_id: SID,
            entity: 2,
            message: GhostMessage::Create(vec![1, 2, 3]),
        };
        assert!(short.split_create().is_none());
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &short),
            CreateGhostOutcome::BadStream
        );
    }

    /// A shutting-down space runs no offload check at all, and a space
    /// this app does not hold stops the checker before it starts.
    #[test]
    fn the_shutting_down_space_skips_the_check() {
        let (mut app_a, mut spaces_a) = {
            let cells = cell_infos(&[(A_ADDR, 0.0, 50.0), (B_ADDR, 50.0, 100.0)]);
            app_at(A_ADDR, &cells)
        };
        app_a.ghost_distance = 50.0;
        assert!(matches!(
            app_a.create_real_entity(&mut spaces_a, SID, &create_stream(2, [40.0, 0.0, 25.0])),
            crate::cellapp_real_entity::CreateRealOutcome::Created(2)
        ));
        spaces_a.get_mut(&SID).unwrap().shut_down_space(0);
        let mut checker = OffloadChecker::new(SID);
        checker.run(&mut app_a, &mut spaces_a);
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        assert!(app_a.ghost_outbox.is_empty(), "nothing ghosts out");
        assert!(checker.is_empty());

        // A space this app does not hold stops the checker before it
        // starts.
        assert!(!app_a.check_offloads_and_ghosts(&mut spaces_a, 999));
        assert!(OffloadChecker::new(999).is_empty());
    }

    /// `checkEntityForOffload` declines a missing space, a position no
    /// cell holds, and — through `run` — the pair at once.
    #[test]
    fn the_offload_home_is_declined_when_unfindable() {
        let (mut app_a, mut spaces_a) = {
            let cells = cell_infos(&[(A_ADDR, 0.0, 50.0), (B_ADDR, 50.0, 100.0)]);
            app_at(A_ADDR, &cells)
        };
        app_a.ghost_distance = 50.0;
        assert!(matches!(
            app_a.create_real_entity(&mut spaces_a, SID, &create_stream(2, [40.0, 0.0, 25.0])),
            crate::cellapp_real_entity::CreateRealOutcome::Created(2)
        ));

        // No space to look in: the check declines and nothing runs.
        let mut checker = OffloadChecker::new(SID);
        let mut empty = BTreeMap::new();
        GhostMaintainer::new(SID, 2).check(&mut app_a, &mut empty, &mut checker);
        assert!(app_a.ghost_outbox.is_empty());
        assert!(checker.is_empty());

        // A position no cell holds: no home, no offload.
        app_a.entity_population[&2].borrow_mut().position = [200.0, 0.0, 25.0];
        checker = OffloadChecker::new(SID);
        checker.run(&mut app_a, &mut spaces_a);
        assert!(app_a.ghost_outbox.is_empty());
        assert!(checker.is_empty());
    }

    /// A delete-pending home declines the offload and its own visits.
    #[test]
    fn the_delete_pending_home_is_declined() {
        let mut cells = cell_infos(&[
            (A_ADDR, 0.0, 40.0),
            (B_ADDR, 40.0, 55.0),
            (C_ADDR, 55.0, 100.0),
        ]);
        cells[1].is_delete_pending = true;
        let (mut app_a, mut spaces_a) = app_at(A_ADDR, &cells);
        app_a.ghost_distance = 50.0;
        assert!(matches!(
            app_a.create_real_entity(&mut spaces_a, SID, &create_stream(2, [45.0, 0.0, 25.0])),
            crate::cellapp_real_entity::CreateRealOutcome::Created(2)
        ));
        let mut checker = OffloadChecker::new(SID);
        checker.run(&mut app_a, &mut spaces_a);
        // The pending cell gets no ghost; the healthy one does.
        let dests: Vec<u32> = app_a.ghost_outbox.iter().map(|wire| wire.dest).collect();
        assert_eq!(dests, [C_ADDR]);
        assert!(checker.is_empty());
        assert_eq!(
            app_a.entity_population[&2].borrow().haunts,
            vec![Haunt {
                addr: C_ADDR,
                creation_time: app_a.time
            }]
        );
    }

    /// A failed home declines the offload but keeps ghosting it.
    #[test]
    fn the_failed_home_is_declined() {
        let cells = cell_infos(&[
            (A_ADDR, 0.0, 40.0),
            (B_ADDR, 40.0, 55.0),
            (C_ADDR, 55.0, 100.0),
        ]);
        let (mut app_a, mut spaces_a) = app_at(A_ADDR, &cells);
        app_a.ghost_distance = 50.0;
        app_a.dead_channels.insert(B_ADDR);
        assert!(matches!(
            app_a.create_real_entity(&mut spaces_a, SID, &create_stream(2, [45.0, 0.0, 25.0])),
            crate::cellapp_real_entity::CreateRealOutcome::Created(2)
        ));
        let mut checker = OffloadChecker::new(SID);
        checker.run(&mut app_a, &mut spaces_a);
        let dests: Vec<u32> = app_a.ghost_outbox.iter().map(|wire| wire.dest).collect();
        assert_eq!(dests, [B_ADDR, C_ADDR]);
        assert!(checker.is_empty());
    }

    /// The offload destination keeps its own ghost: the second pass
    /// unmarks it instead of creating a second one.
    #[test]
    fn the_offload_destination_keeps_its_ghost() {
        let cells = cell_infos(&[
            (A_ADDR, 0.0, 40.0),
            (B_ADDR, 40.0, 55.0),
            (C_ADDR, 55.0, 100.0),
        ]);
        let (mut app_a, mut spaces_a) = app_at(A_ADDR, &cells);
        app_a.ghost_distance = 50.0;
        assert!(matches!(
            app_a.create_real_entity(&mut spaces_a, SID, &create_stream(2, [45.0, 0.0, 25.0])),
            crate::cellapp_real_entity::CreateRealOutcome::Created(2)
        ));
        let mut checker = OffloadChecker::new(SID);
        checker.run(&mut app_a, &mut spaces_a);
        assert_eq!(checker.offload_list, vec![(2, B_ADDR)]);
        assert!(!checker.is_empty());
        let dests: Vec<u32> = app_a.ghost_outbox.iter().map(|wire| wire.dest).collect();
        assert_eq!(dests, [B_ADDR]);

        checker = OffloadChecker::new(SID);
        checker.run(&mut app_a, &mut spaces_a);
        assert_eq!(app_a.ghost_outbox.len(), 1, "no second create");
        assert_eq!(
            app_a.entity_population[&2].borrow().haunts.len(),
            1,
            "no second haunt"
        );
    }

    /// The hysteresis check declines a cell inside the interest area
    /// but outside the ghosting distance.
    #[test]
    fn the_hysteresis_declines_the_far_cell() {
        let cells = cell_infos(&[(A_ADDR, 0.0, 40.0), (B_ADDR, 70.0, 100.0)]);
        let (mut app_a, mut spaces_a) = app_at(A_ADDR, &cells);
        app_a.ghost_distance = 30.0;
        assert!(matches!(
            app_a.create_real_entity(&mut spaces_a, SID, &create_stream(2, [35.0, 0.0, 25.0])),
            crate::cellapp_real_entity::CreateRealOutcome::Created(2)
        ));
        let mut checker = OffloadChecker::new(SID);
        checker.run(&mut app_a, &mut spaces_a);
        assert!(app_a.ghost_outbox.is_empty(), "the far cell stays bare");
        assert!(app_a.entity_population[&2].borrow().haunts.is_empty());
    }

    /// BW-08-05-01's acceptance: the entity that crosses the test
    /// boundary migrates at the threshold — the checker collects the
    /// pair, the send face streams the real to the destination, the
    /// ghost left behind follows the sign-off, and both ends agree on
    /// who holds the real.
    #[test]
    fn crossing_the_boundary_offloads_at_the_threshold() {
        let (mut app_a, mut spaces_a, mut app_b, mut spaces_b) = two_cell_fixture();
        // Both ends carry the migration callbacks so `entity_callback`
        // records them (`has_method` gates the log).
        for app in [&mut app_a, &mut app_b] {
            let mut script = EntityScript::with_init(vec![]);
            for name in [
                "onLeavingCell",
                "onLeftCell",
                "onEnteringCell",
                "onEnteredCell",
            ] {
                script.methods.insert(name);
            }
            app.entity_scripts.insert(TYPE, script);
        }
        // The boundary is B's rect at 50: the entity starts on A at 40
        // and stays put through the pre-crossing check.
        let mut checker = OffloadChecker::new(SID);
        checker.run(&mut app_a, &mut spaces_a);
        assert!(
            checker.offload_list.is_empty(),
            "nothing to migrate before the boundary"
        );
        assert!(app_a.onload_outbox.is_empty());
        assert!(app_a.entity_population[&2].borrow().is_real);
        // The maintenance pass put a ghost on B before the crossing —
        // the offload has somewhere to land.
        let run_one = app_a.ghost_outbox.len();
        for wire in &app_a.ghost_outbox {
            if wire.dest == B_ADDR {
                assert_eq!(
                    deliver(&mut app_b, &mut spaces_b, wire),
                    CreateGhostOutcome::Created,
                    "B holds the ghost the offload lands on"
                );
            }
        }

        // Across the boundary: the threshold check collects the pair
        // and the send face migrates it.
        {
            let mut borrowed = app_a.entity_population[&2].borrow_mut();
            borrowed.position = [60.0, 0.0, 25.0];
            borrowed.velocity = [1.5, 0.0, 0.0];
        }
        let scripts_before = app_a.script_calls.len();
        let mut checker = OffloadChecker::new(SID);
        checker.run(&mut app_a, &mut spaces_a);
        assert_eq!(
            app_a.onload_outbox.len(),
            1,
            "the real streamed to the destination"
        );
        let wire = app_a.onload_outbox.remove(0);
        assert_eq!(wire.dest, B_ADDR);
        assert_eq!(wire.entity, 2);
        assert!(!wire.is_teleport, "the checker never teleports");

        // The source end: the entity is the ghost now, waiting on the
        // sign-off with `onLeavingCell`/`onLeftCell` run in order.
        assert!(!app_a.entity_population[&2].borrow().is_real);
        let leaving: Vec<&str> = app_a
            .script_calls
            .iter()
            .skip(scripts_before)
            .filter(|call| call.entity == 2)
            .map(|call| call.name)
            .collect();
        assert_eq!(leaving, ["onLeavingCell", "onLeftCell"]);
        let notified: Vec<u32> = app_a
            .ghost_outbox
            .iter()
            .filter(|wire| matches!(wire.message, GhostMessage::SetNextReal { .. }))
            .map(|wire| wire.dest)
            .collect();
        assert!(
            notified.is_empty(),
            "the destination hears nothing until the sign-off"
        );

        // The destination end: the ghost that held the ground becomes
        // the real, signed onto its old haunt through the read.
        // No new create went out with the offload — the destination
        // already holds the ghost from the maintenance pass.
        let create = app_a
            .ghost_outbox
            .iter()
            .skip(run_one)
            .position(|wire| matches!(wire.message, GhostMessage::Create(_)));
        assert!(create.is_none(), "the offload creates no fresh ghost");
        assert!(app_b.entity_population.contains_key(&2));
        assert!(!app_b.entity_population[&2].borrow().is_real);
        assert!(
            app_b.onload_entity(&mut spaces_b, &wire, None),
            "the stream reads back"
        );
        assert!(app_b.entity_population[&2].borrow().is_real);
        // The stream carries the velocity, not the position: the
        // position is the ghost's own, told by the updates.
        assert_eq!(
            app_b.entity_population[&2].borrow().position,
            [40.0, 0.0, 25.0],
            "the ghost keeps the position the updates gave it"
        );
        assert_eq!(
            app_b.entity_population[&2].borrow().velocity,
            [1.5, 0.0, 0.0],
            "the real's velocity rides the stream"
        );
        let entered: Vec<&str> = app_b
            .script_calls
            .iter()
            .filter(|call| call.entity == 2)
            .map(|call| call.name)
            .collect();
        assert_eq!(entered, ["onEnteringCell", "onEnteredCell"]);

        // The sign-off rides back: the ghost on A now follows the real
        // on B, and both ends can name its one real.
        let sign_off: Vec<_> = app_b
            .ghost_outbox
            .iter()
            .filter(|wire| matches!(wire.message, GhostMessage::SetReal { .. }))
            .collect();
        assert_eq!(sign_off.len(), 1, "the read signed the old haunt on");
        assert_eq!(sign_off[0].dest, A_ADDR);
        assert_eq!(
            deliver(&mut app_a, &mut spaces_a, sign_off[0]),
            CreateGhostOutcome::Created
        );
        let a_view = app_a.entity_population[&2].borrow();
        assert!(!a_view.is_real);
        assert_eq!(a_view.real_addr, Some(B_ADDR));
        assert_eq!(a_view.next_real_addr, None, "the switch is settled");
        drop(a_view);
        let b_view = app_b.entity_population[&2].borrow();
        assert!(b_view.is_real);
        assert_eq!(b_view.num_times_real_offloaded, 1);
    }

    /// The real's snapshot rides to the ghost: the position, the ground
    /// flag, the direction, the volatile info and the property data all
    /// end up matching what the real wrote.
    #[test]
    fn the_ghost_matches_the_real_snapshot() {
        let (mut app_a, mut spaces_a, mut app_b, mut spaces_b) = two_cell_fixture();
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        let create = app_a.ghost_outbox.remove(0);
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &create),
            CreateGhostOutcome::Created
        );

        // The volatile info changes on the real: every haunt is told,
        // and an unchanged tell goes nowhere.
        let volatile = crate::cellapp_witness::VolatileInfo {
            position_priority: 8.0,
            yaw_priority: 4.0,
            pitch_priority: 2.0,
            roll_priority: 1.0,
        };
        assert!(app_a.set_volatile_info(2, volatile));
        assert_eq!(app_a.ghost_outbox.len(), 1, "the haunt was told");
        let wire = app_a.ghost_outbox.remove(0);
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &wire),
            CreateGhostOutcome::Created
        );
        assert_eq!(app_b.entity_population[&2].borrow().volatile_info, volatile);
        assert!(
            !app_a.set_volatile_info(2, volatile),
            "an unchanged tell is dropped"
        );
        assert!(app_a.ghost_outbox.is_empty());

        // The real walks: the ghost's local faces, the global face and
        // the volatile counter all follow.
        {
            let mut real = app_a.entity_population[&2].borrow_mut();
            real.local_position = [41.0, 0.0, 26.0];
            real.position = [41.0, 0.0, 26.0];
            real.local_direction = [0.5, 0.25, -1.0];
            real.direction = [0.5, 0.25, -1.0];
            real.is_on_ground = false;
        }
        app_a.send_ghost_position_update(2);
        assert_eq!(app_a.ghost_outbox.len(), 1, "one position update per haunt");
        let wire = app_a.ghost_outbox.remove(0);
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &wire),
            CreateGhostOutcome::Created
        );
        let ghost = app_b.entity_population[&2].borrow();
        assert_eq!(ghost.local_position, [41.0, 0.0, 26.0]);
        assert_eq!(ghost.position, [41.0, 0.0, 26.0]);
        assert_eq!(ghost.local_direction, [0.5, 0.25, -1.0]);
        assert_eq!(ghost.direction, [0.5, 0.25, -1.0]);
        assert!(!ghost.is_on_ground);
        drop(ghost);
        let ghost_counter = app_b.entity_population[&2].borrow().volatile_update_number;
        let real_counter = app_a.entity_population[&2].borrow().volatile_update_number;
        assert_eq!(
            ghost_counter, real_counter,
            "the ghost restamps the real's counter"
        );

        // The property data: the description table sits on the ghost's
        // own app — the ghost is what looks its properties up. A
        // componentised entry is the reference's CRITICAL.
        app_b
            .ghost_property_descriptions
            .insert(TYPE, vec![(0, false), (1, true)]);
        app_a.send_ghosted_data_update(2, 0, &[7, 8]);
        let wire = app_a.ghost_outbox.remove(0);
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &wire),
            CreateGhostOutcome::Created
        );
        assert_eq!(
            app_b.entity_population[&2].borrow().ghost_properties[&0],
            [7, 8]
        );

        app_a.send_ghosted_data_update(2, 1, &[9, 9]);
        let wire = app_a.ghost_outbox.remove(0);
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &wire),
            CreateGhostOutcome::BadStream
        );
        assert!(
            app_b
                .ghost_property_notes
                .iter()
                .any(|note| note.contains("component")),
            "the componentised update is the CRITICAL face"
        );

        // An index the type has no description for is the reference's
        // NULL-description ERROR.
        app_a.send_ghosted_data_update(2, 3, &[1]);
        let wire = app_a.ghost_outbox.remove(0);
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &wire),
            CreateGhostOutcome::BadStream
        );
        assert!(
            app_b
                .ghost_property_notes
                .iter()
                .any(|note| note.contains("no description"))
        );

        // The unknown-entity refusals on every copy face.
        assert!(!app_a.set_volatile_info(99, volatile));
        assert!(!app_b.ghost_position_update(&mut spaces_b, 99, [0.0; 3], [0.0; 3], false, 1));
        assert!(!app_b.ghost_volatile_info(99, volatile));
        assert!(!app_b.ghosted_data_update(99, 0, &[1]));

        // A refused copy reads as a refusal through the deliver face.
        let missing = GhostWire {
            dest: B_ADDR,
            src: A_ADDR,
            space_id: SID,
            entity: 99,
            message: GhostMessage::Position {
                pos: [0.0; 3],
                dir: [0.0; 3],
                is_on_ground: false,
                update_number: 1,
            },
        };
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &missing),
            CreateGhostOutcome::BadStream
        );
        let missing = GhostWire {
            dest: B_ADDR,
            src: A_ADDR,
            space_id: SID,
            entity: 99,
            message: GhostMessage::VolatileInfo(volatile),
        };
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &missing),
            CreateGhostOutcome::BadStream
        );
    }

    /// The volatile-info comparisons are the reference's own.
    #[test]
    fn the_volatile_info_comparisons() {
        let more = crate::cellapp_witness::VolatileInfo::default();
        let less = crate::cellapp_witness::VolatileInfo {
            position_priority: -2.0,
            yaw_priority: -1.0,
            pitch_priority: -1.0,
            roll_priority: -1.0,
        };
        assert!(
            less.is_less_volatile_than(more),
            "the lower position priority is less volatile"
        );
        assert!(!more.is_less_volatile_than(less));
        assert!(more.is_valid(), "equal priorities count as ordered");
        let unsorted = crate::cellapp_witness::VolatileInfo {
            position_priority: 0.0,
            yaw_priority: 1.0,
            pitch_priority: 3.0,
            roll_priority: 2.0,
        };
        assert!(!unsorted.is_valid(), "yaw below pitch is unordered");
        let ordered = crate::cellapp_witness::VolatileInfo {
            position_priority: 0.0,
            yaw_priority: 3.0,
            pitch_priority: 2.0,
            roll_priority: 1.0,
        };
        assert!(ordered.is_valid());
    }

    /// The switch-period markers: a create and a `ghostSetReal` open a
    /// subsequence, a del and a `ghostSetNextReal` close it.
    #[test]
    fn the_sign_off_markers_split_the_subsequence() {
        assert!(is_subsequence_start(&GhostMessage::Create(vec![1, 2])));
        assert!(is_subsequence_start(&GhostMessage::SetReal {
            num_times_real_offloaded: 1,
            owner: C_ADDR
        }));
        assert!(!is_subsequence_start(&GhostMessage::Position {
            pos: [0.0; 3],
            dir: [0.0; 3],
            is_on_ground: true,
            update_number: 0
        }));
        assert!(!is_subsequence_end(&GhostMessage::Create(vec![])));
        assert!(!is_subsequence_end(&GhostMessage::SetReal {
            num_times_real_offloaded: 1,
            owner: C_ADDR
        }));
        assert!(is_subsequence_end(&GhostMessage::Del));
        assert!(is_subsequence_end(&GhostMessage::SetNextReal {
            next_real_addr: C_ADDR
        }));
    }

    /// The delayed subsequence drags the messages behind it, and its
    /// promotion splices the pair ahead of the plain queue.
    #[test]
    fn the_delayed_subsequence_splices_ahead_and_replays_in_order() {
        let mut queue = BufferedGhostMessagesForEntity::default();
        let sign_off = |message| BufferedGhostMessage {
            src: A_ADDR,
            entity: 2,
            space_id: SID,
            delayed: true,
            message,
        };
        queue.delay_subsequence(sign_off(GhostMessage::SetReal {
            num_times_real_offloaded: 1,
            owner: C_ADDR,
        }));
        assert!(queue.is_delaying());
        assert!(queue.messages.is_empty());
        // Mid-subsequence messages ride the delayed bucket.
        queue.add(sign_off(GhostMessage::Position {
            pos: [1.0, 2.0, 3.0],
            dir: [0.0, 0.0, 1.0],
            is_on_ground: true,
            update_number: 4,
        }));
        assert_eq!(
            queue.delayed.as_ref().map(Vec::len),
            Some(2),
            "the position rode the delayed bucket"
        );
        // The closing sign-off stops the collection.
        queue.add(sign_off(GhostMessage::SetNextReal {
            next_real_addr: C_ADDR,
        }));
        assert!(!queue.is_delaying());
        // A plain add lands on the queue once nothing is delayed.
        queue.add(BufferedGhostMessage {
            src: A_ADDR,
            entity: 2,
            space_id: SID,
            delayed: false,
            message: GhostMessage::SetReal {
                num_times_real_offloaded: 2,
                owner: B_ADDR,
            },
        });
        assert_eq!(queue.messages.len(), 1);
        // `promoteDelayedSubsequences` splices ahead, in order.
        queue.promote_delayed_subsequences();
        let kinds: Vec<&GhostMessage> = queue.messages.iter().map(|held| &held.message).collect();
        assert!(matches!(kinds[0], GhostMessage::SetReal { .. }));
        assert!(matches!(kinds[1], GhostMessage::Position { .. }));
        assert!(matches!(kinds[2], GhostMessage::SetNextReal { .. }));
        assert!(matches!(kinds[3], GhostMessage::SetReal { .. }));
        assert!(queue.delayed.is_none());
    }

    /// BW-08-04-03's acceptance: the ghost follows its real to the new
    /// address — the offloading real announces the destination, the new
    /// real signs off, and the ghost's route lands on the new process.
    #[test]
    fn the_ghost_tracks_its_real_to_the_new_address() {
        let (mut app_a, mut spaces_a, mut app_b, mut spaces_b, mut app_c, mut spaces_c) =
            three_cell_fixture();
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        // Every haunt materialises its ghost.
        for wire in app_a.ghost_outbox.drain(..) {
            match wire.dest {
                B_ADDR => {
                    assert_eq!(
                        deliver(&mut app_b, &mut spaces_b, &wire),
                        CreateGhostOutcome::Created
                    )
                }
                C_ADDR => {
                    assert_eq!(
                        deliver(&mut app_c, &mut spaces_c, &wire),
                        CreateGhostOutcome::Created
                    )
                }
                _ => unreachable!("two haunts only"),
            }
        }

        // `RealEntity::destroy( pNextRealAddr )` on the offloading
        // real: the haunt that is not the destination hears it.
        app_a.send_ghost_set_next_real(2, C_ADDR);
        assert_eq!(
            app_a.entity_population[&2].borrow().next_real_addr,
            Some(C_ADDR),
            "entity.cpp:2039 — the real takes the destination first"
        );
        let next_real_wires: Vec<GhostWire> = app_a
            .ghost_outbox
            .drain(..)
            .filter(|wire| matches!(wire.message, GhostMessage::SetNextReal { .. }))
            .collect();
        assert_eq!(next_real_wires.len(), 1, "the destination is excluded");
        assert_eq!(next_real_wires[0].dest, B_ADDR);
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &next_real_wires[0]),
            CreateGhostOutcome::Created
        );
        assert_eq!(
            app_b.entity_population[&2].borrow().next_real_addr,
            Some(C_ADDR)
        );
        assert!(!app_b.buffered_ghost_messages.contains_key(&2));

        // The destination's ghost converts to the real
        // (`convertGhostToReal`'s bump is the offload cluster's face;
        // the count it leaves is what `readOffloadData` streams out).
        {
            let mut landed = app_c.entity_population.get_mut(&2).unwrap().borrow_mut();
            landed.is_real = true;
            landed.num_times_real_offloaded = 1;
            landed.haunts = vec![
                Haunt {
                    addr: A_ADDR,
                    creation_time: 1000,
                },
                Haunt {
                    addr: B_ADDR,
                    creation_time: 1000,
                },
                Haunt {
                    addr: C_ADDR,
                    creation_time: 1000,
                },
            ];
        }
        // `RealEntity::readOffloadData`'s haunt loop: the landed real
        // signs itself onto every haunt it can reach, and its own
        // address is the guarantee.
        assert!(app_c.send_ghost_set_real(2, &[], false, false));
        let set_real_wires: Vec<GhostWire> = app_c
            .ghost_outbox
            .drain(..)
            .filter(|wire| matches!(wire.message, GhostMessage::SetReal { .. }))
            .collect();
        assert_eq!(set_real_wires.len(), 2, "the own haunt hears nothing");
        assert_eq!(
            set_real_wires[0].message,
            GhostMessage::SetReal {
                num_times_real_offloaded: 1,
                owner: C_ADDR
            }
        );
        for wire in &set_real_wires {
            match wire.dest {
                B_ADDR => {
                    assert_eq!(
                        deliver(&mut app_b, &mut spaces_b, wire),
                        CreateGhostOutcome::Created
                    )
                }
                A_ADDR => {}
                _ => unreachable!("two haunts only"),
            }
        }

        // The route now lands on the new process: the ghost holds the
        // sign-off and nothing of the switch is left pending.
        let borrowed = app_b.entity_population[&2].borrow();
        assert_eq!(borrowed.real_addr, Some(C_ADDR));
        assert_eq!(borrowed.num_times_real_offloaded, 1);
        assert_eq!(borrowed.next_real_addr, None);
        drop(borrowed);
        assert!(app_b.buffered_ghost_messages.is_empty());
        assert!(
            !app_b
                .ghost_property_notes
                .iter()
                .any(|note| note.contains("ghostSetReal")),
            "the in-order sign-off never warned"
        );
    }

    /// An out-of-turn `ghostSetReal` is held for the switch, and replays
    /// when the announcement lands — the stale copy stays held, exactly
    /// as the reference leaves it.
    #[test]
    fn the_off_turn_sign_off_waits_for_the_switch() {
        let (mut app_a, mut spaces_a, mut app_b, mut spaces_b) = two_cell_fixture();
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        let create_wire = app_a.ghost_outbox.remove(0);
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &create_wire),
            CreateGhostOutcome::Created
        );

        // The real skipped a hop: the id names two offloads, the ghost
        // has heard of none.
        assert_eq!(
            app_b.ghost_set_real(&mut spaces_b, 2, 2, C_ADDR),
            GhostSetReal::Buffered
        );
        let ghost = app_b.entity_population[&2].borrow();
        assert_eq!(ghost.num_times_real_offloaded, 0);
        assert_eq!(ghost.real_addr, Some(A_ADDR));
        drop(ghost);
        let held = app_b.buffered_ghost_messages[&2]
            .delayed
            .as_ref()
            .expect("the out-of-turn sign-off is held");
        assert!(matches!(held[0].message, GhostMessage::SetReal { .. }));
        assert_eq!(held[0].src, C_ADDR);
        assert!(
            app_b
                .ghost_property_notes
                .iter()
                .any(|note| note.contains("expected 1, got 2")),
            "the WARNING face lands"
        );

        // The announcement replays the held subsequence — the stale id
        // still does not match, so it is held again.
        assert!(app_b.ghost_set_next_real(&mut spaces_b, 2, C_ADDR));
        assert_eq!(
            app_b.entity_population[&2].borrow().next_real_addr,
            Some(C_ADDR)
        );
        assert_eq!(
            app_b.buffered_ghost_messages[&2]
                .delayed
                .as_ref()
                .map(Vec::len),
            Some(1),
            "the replay re-held the stale sign-off"
        );

        // The matching sign-off applies once it arrives.
        assert_eq!(
            app_b.ghost_set_real(&mut spaces_b, 2, 1, C_ADDR),
            GhostSetReal::Applied
        );
        let ghost = app_b.entity_population[&2].borrow();
        assert_eq!(ghost.num_times_real_offloaded, 1);
        assert_eq!(ghost.real_addr, Some(C_ADDR));
        assert_eq!(ghost.next_real_addr, None);
        drop(ghost);
        assert_eq!(
            app_b.ghost_set_real(&mut spaces_b, 99, 1, C_ADDR),
            GhostSetReal::Missing,
            "a sign-off for nothing here is dropped"
        );
    }

    /// `RealEntity::readOffloadData`'s haunt loop skips its own
    /// address, a failed teleport's haunt, and a dead app — and the
    /// space-changed or teleported arm tells the haunts the ghost is
    /// done instead.
    #[test]
    fn the_landed_real_signs_itself_onto_its_haunts() {
        let (mut app_a, mut spaces_a, mut app_b, mut spaces_b) = two_cell_fixture();
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        let create_wire = app_a.ghost_outbox.remove(0);
        assert_eq!(
            deliver(&mut app_b, &mut spaces_b, &create_wire),
            CreateGhostOutcome::Created
        );

        // The landed real (now on B, holding its own haunt) reaches
        // the old real's app.
        app_b
            .entity_population
            .get_mut(&2)
            .unwrap()
            .borrow_mut()
            .haunts = vec![
            Haunt {
                addr: A_ADDR,
                creation_time: 1000,
            },
            Haunt {
                addr: B_ADDR,
                creation_time: 1000,
            },
            Haunt {
                addr: D_ADDR,
                creation_time: 1000,
            },
        ];
        app_b.dead_channels.insert(D_ADDR);
        app_b
            .entity_population
            .get_mut(&2)
            .unwrap()
            .borrow_mut()
            .num_times_real_offloaded = 2;
        assert!(
            app_b.send_ghost_set_real(2, &[], false, false),
            "the app's own haunt is the guarantee"
        );
        let set_real: Vec<&GhostWire> = app_b
            .ghost_outbox
            .iter()
            .filter(|wire| matches!(wire.message, GhostMessage::SetReal { .. }))
            .collect();
        assert_eq!(set_real.len(), 1, "own and dead haunts hear nothing");
        assert_eq!(set_real[0].dest, A_ADDR);
        assert_eq!(
            set_real[0].message,
            GhostMessage::SetReal {
                num_times_real_offloaded: 2,
                owner: B_ADDR
            }
        );
        assert!(
            app_b
                .ghost_property_notes
                .iter()
                .any(|note| note.contains("dead haunt 40")),
            "the dead app is skipped with a WARNING"
        );

        // A failed teleport's haunt is skipped on request: the
        // bad haunt hears nothing while the live haunt still gets the
        // sign-on.
        app_b
            .entity_population
            .get_mut(&2)
            .unwrap()
            .borrow_mut()
            .haunts = vec![
            Haunt {
                addr: A_ADDR,
                creation_time: 1000,
            },
            Haunt {
                addr: B_ADDR,
                creation_time: 1000,
            },
            Haunt {
                addr: C_ADDR,
                creation_time: 1000,
            },
        ];
        app_b.ghost_outbox.clear();
        assert!(app_b.send_ghost_set_real(2, &[A_ADDR], false, false));
        let signed: Vec<&GhostWire> = app_b
            .ghost_outbox
            .iter()
            .filter(|wire| matches!(wire.message, GhostMessage::SetReal { .. }))
            .collect();
        assert_eq!(signed.len(), 1);
        assert_ne!(signed[0].dest, A_ADDR, "the bad haunt hears nothing");
        assert!(
            app_b
                .ghost_property_notes
                .iter()
                .any(|note| note.contains("failed teleport haunt 10")),
            "the bad haunt is skipped with a WARNING"
        );

        // The space-changed or teleported arm sends the del instead.
        app_b.ghost_outbox.clear();
        app_b
            .entity_population
            .get_mut(&2)
            .unwrap()
            .borrow_mut()
            .haunts = vec![Haunt {
            addr: A_ADDR,
            creation_time: 1000,
        }];
        assert!(!app_b.send_ghost_set_real(2, &[], true, false));
        let del: Vec<&GhostWire> = app_b
            .ghost_outbox
            .iter()
            .filter(|wire| matches!(wire.message, GhostMessage::Del))
            .collect();
        assert_eq!(del.len(), 1);
        assert_eq!(del[0].dest, A_ADDR);
    }

    /// The ladder's every disposition, pinned as the pure classifier
    /// the reference extracted it for (`entity_message_routing.hpp`).
    #[test]
    fn the_ladder_pins_every_disposition() {
        let ladder = |reality: EntityReality,
                      edit: &dyn Fn(&mut EntityMessageRoutingState)|
         -> EntityMessageDisposition {
            let mut state = EntityMessageRoutingState {
                reality: Some(reality),
                ..Default::default()
            };
            edit(&mut state);
            classify_entity_message(&state)
        };
        // A ghost message from the current real of a settled ghost.
        let settled = |state: &mut EntityMessageRoutingState| {
            state.entity_exists = true;
        };
        assert_eq!(
            ladder(EntityReality::GhostOnly, &settled),
            EntityMessageDisposition::Dispatch
        );
        // Destroyed by restored real.
        let restored = |state: &mut EntityMessageRoutingState| {
            state.entity_exists = true;
            state.entity_is_real = true;
        };
        assert_eq!(
            ladder(EntityReality::GhostOnly, &restored),
            EntityMessageDisposition::DropDestroyedGhost
        );
        // Destroyed by restored real, then recreated when offloading
        // back to the sender — a start survives, a mid-message drops.
        let offloading = |state: &mut EntityMessageRoutingState| {
            state.entity_exists = true;
            state.entity_is_offloading_to_sender = true;
        };
        let start = offloading;
        let with_start = |state: &mut EntityMessageRoutingState| {
            start(state);
            state.is_subsequence_start = true;
        };
        assert_eq!(
            ladder(EntityReality::GhostOnly, &offloading),
            EntityMessageDisposition::DropDestroyedGhost
        );
        assert_eq!(
            ladder(EntityReality::GhostOnly, &with_start),
            EntityMessageDisposition::Dispatch
        );
        // Destroyed when the sender's death was handled: the
        // entity still exists and buffers the sender, but nothing was
        // held from it, so the predecessor has no reference.
        let orphan = |state: &mut EntityMessageRoutingState| {
            state.entity_exists = true;
            state.should_buffer_from_sender = true;
        };
        assert_eq!(
            ladder(EntityReality::GhostOnly, &orphan),
            EntityMessageDisposition::DropDestroyedGhost
        );
        // A subsequence start with no entity buffers instead.
        let orphan_start = |state: &mut EntityMessageRoutingState| {
            state.is_subsequence_start = true;
        };
        assert_eq!(
            ladder(EntityReality::GhostOnly, &orphan_start),
            EntityMessageDisposition::BufferGhostMessage
        );
        // Out of sender order: the mid-switch ghost holds a stranger.
        let wrong_sender = |state: &mut EntityMessageRoutingState| {
            state.entity_exists = true;
            state.should_buffer_from_sender = true;
            state.has_messages_for_sender = true;
        };
        assert_eq!(
            ladder(EntityReality::GhostOnly, &wrong_sender),
            EntityMessageDisposition::BufferGhostMessage
        );
        // REAL_ONLY and WITNESS_ONLY for a missing or ghost entity
        // both attempt the forward.
        assert_eq!(
            ladder(EntityReality::RealOnly, &|_| {}),
            EntityMessageDisposition::ForwardToReal
        );
        let witness_missing = |state: &mut EntityMessageRoutingState| {
            state.entity_exists = true;
        };
        assert_eq!(
            ladder(EntityReality::WitnessOnly, &witness_missing),
            EntityMessageDisposition::ForwardToReal
        );
        // A witness message for a witness-less real drops.
        let no_witness = |state: &mut EntityMessageRoutingState| {
            state.entity_exists = true;
            state.entity_is_real = true;
        };
        assert_eq!(
            ladder(EntityReality::WitnessOnly, &no_witness),
            EntityMessageDisposition::DropNoWitness
        );
        let with_witness = |state: &mut EntityMessageRoutingState| {
            no_witness(state);
            state.entity_has_witness = true;
        };
        assert_eq!(
            ladder(EntityReality::WitnessOnly, &with_witness),
            EntityMessageDisposition::Dispatch
        );
        // No tick budget left defers to the next tick.
        let tick_pending = |state: &mut EntityMessageRoutingState| {
            state.entity_exists = true;
            state.should_buffer_if_tick_pending = true;
            state.next_tick_pending = true;
        };
        assert_eq!(
            ladder(EntityReality::GhostOnly, &tick_pending),
            EntityMessageDisposition::BufferIfTickPending
        );
    }

    /// BW-08-04-04's acceptance: whatever order the wires land in, the
    /// switch-period messages deliver in sequence and nothing applies
    /// twice — the reordered and in-order runs end in the same state.
    #[test]
    fn the_switch_period_messages_arrive_in_order_whatever_the_wire() {
        let sign_off = GhostMessage::SetReal {
            num_times_real_offloaded: 1,
            owner: C_ADDR,
        };
        let announce = GhostMessage::SetNextReal {
            next_real_addr: C_ADDR,
        };
        let position = GhostMessage::Position {
            pos: [7.0, 8.0, 9.0],
            dir: [0.0, 1.0, 0.0],
            is_on_ground: false,
            update_number: 5,
        };
        let wire = |message: GhostMessage, src: u32| GhostWire {
            dest: B_ADDR,
            src,
            space_id: SID,
            entity: 2,
            message,
        };

        // The reordered run: the new real's messages land before the
        // old real's announcement.
        let (mut app_a, mut spaces_a, mut app_b, mut spaces_b, _app_c, _spaces_c) =
            three_cell_fixture();
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        for create in app_a.ghost_outbox.drain(..) {
            match create.dest {
                B_ADDR => {
                    deliver(&mut app_b, &mut spaces_b, &create);
                }
                C_ADDR => {}
                _ => unreachable!("two haunts only"),
            }
        }
        app_a.send_ghost_set_next_real(2, C_ADDR);
        app_a.ghost_outbox.clear();
        assert_eq!(
            app_b.deliver_ghost(&mut spaces_b, &wire(sign_off.clone(), C_ADDR)),
            DeliverGhost::Buffered,
            "the new real's sign-off beats the announcement"
        );
        assert_eq!(
            app_b.deliver_ghost(&mut spaces_b, &wire(position.clone(), C_ADDR)),
            DeliverGhost::Buffered,
            "and its position rides behind it"
        );
        assert!(
            app_b
                .ghost_property_notes
                .iter()
                .any(|note| note.contains("buffered ghost message for 2 from 30"))
        );
        assert_eq!(
            app_b.entity_population[&2].borrow().local_position,
            [10.0, 0.0, 25.0],
            "nothing applied ahead of the switch"
        );

        // The old real's announcement — from the address the ghost
        // still trusts — dispatches and replays the held pair in order.
        assert_eq!(
            app_b.deliver_ghost(&mut spaces_b, &wire(announce.clone(), A_ADDR)),
            DeliverGhost::SetNextReal(true)
        );
        let borrowed = app_b.entity_population[&2].borrow();
        assert_eq!(borrowed.real_addr, Some(C_ADDR));
        assert_eq!(borrowed.num_times_real_offloaded, 1);
        assert_eq!(
            borrowed.local_position,
            [7.0, 8.0, 9.0],
            "the position replayed"
        );
        drop(borrowed);
        assert!(
            app_b.buffered_ghost_messages.is_empty(),
            "the queue drained"
        );
        let reordered_state = (
            app_b.entity_population[&2].borrow().real_addr,
            app_b.entity_population[&2]
                .borrow()
                .num_times_real_offloaded,
            app_b.entity_population[&2].borrow().local_position,
            app_b.entity_population[&2].borrow().next_real_addr,
        );

        // The in-order run: announcement first, then the new real's
        // pair — the same end state.
        let (mut app_a, mut spaces_a, mut app_b, mut spaces_b, _app_c, _spaces_c) =
            three_cell_fixture();
        app_a.check_offloads_and_ghosts(&mut spaces_a, SID);
        for create in app_a.ghost_outbox.drain(..) {
            match create.dest {
                B_ADDR => {
                    deliver(&mut app_b, &mut spaces_b, &create);
                }
                C_ADDR => {}
                _ => unreachable!("two haunts only"),
            }
        }
        app_a.send_ghost_set_next_real(2, C_ADDR);
        app_a.ghost_outbox.clear();
        assert_eq!(
            app_b.deliver_ghost(&mut spaces_b, &wire(announce, A_ADDR)),
            DeliverGhost::SetNextReal(true)
        );
        assert_eq!(
            app_b.deliver_ghost(&mut spaces_b, &wire(sign_off.clone(), C_ADDR)),
            DeliverGhost::SetReal(GhostSetReal::Applied)
        );
        assert_eq!(
            app_b.deliver_ghost(&mut spaces_b, &wire(position, C_ADDR)),
            DeliverGhost::Position(true)
        );
        let in_order_state = (
            app_b.entity_population[&2].borrow().real_addr,
            app_b.entity_population[&2]
                .borrow()
                .num_times_real_offloaded,
            app_b.entity_population[&2].borrow().local_position,
            app_b.entity_population[&2].borrow().next_real_addr,
        );
        assert_eq!(
            reordered_state, in_order_state,
            "the wire order did not change the outcome"
        );

        // The duplicate sign-off never applies twice: the stale id is
        // held, the route and the count stand still.
        assert_eq!(
            app_b.deliver_ghost(&mut spaces_b, &wire(sign_off, C_ADDR)),
            DeliverGhost::SetReal(GhostSetReal::Buffered)
        );
        assert_eq!(
            app_b.entity_population[&2]
                .borrow()
                .num_times_real_offloaded,
            1
        );
        assert_eq!(app_b.entity_population[&2].borrow().real_addr, Some(C_ADDR));
    }

    /// The destroyed-ghost drops: an in-flight message for a real, for
    /// a mid-offload ghost handing itself to the sender, and for an
    /// entity that never was.
    #[test]
    fn the_destroyed_ghost_drops_the_inflight_messages() {
        let position = GhostMessage::Position {
            pos: [1.0, 2.0, 3.0],
            dir: [0.0, 0.0, 1.0],
            is_on_ground: true,
            update_number: 1,
        };
        let wire = |message: GhostMessage, src: u32| GhostWire {
            dest: A_ADDR,
            src,
            space_id: SID,
            entity: 2,
            message,
        };

        // A ghost-domain message for the real we hold.
        let (mut app_a, mut spaces_a, _app_b, _spaces_b) = two_cell_fixture();
        assert_eq!(
            app_a.deliver_ghost(&mut spaces_a, &wire(position.clone(), B_ADDR)),
            DeliverGhost::Dropped,
            "restored by a real"
        );
        assert!(
            app_a
                .ghost_property_notes
                .iter()
                .any(|note| note.contains("destroyed while in-flight"))
        );

        // A message for an entity that never was: nothing buffered from
        // the sender and no subsequence start to open one.
        let orphan = GhostWire {
            dest: A_ADDR,
            src: B_ADDR,
            space_id: SID,
            entity: 99,
            message: position.clone(),
        };
        assert_eq!(
            app_a.deliver_ghost(&mut spaces_a, &orphan),
            DeliverGhost::Dropped
        );

        // A ghost mid-offload to the very sender: destroyed by the
        // restored real, then recreated when offloading to it.
        app_a
            .entity_population
            .get_mut(&2)
            .unwrap()
            .borrow_mut()
            .is_real = false;
        {
            let mut ghost = app_a.entity_population.get_mut(&2).unwrap().borrow_mut();
            ghost.real_addr = Some(C_ADDR);
            ghost.next_real_addr = Some(C_ADDR);
        }
        assert!(
            app_a.entity_is_offloading_to(2, C_ADDR),
            "the ghost names its destination as its real"
        );
        assert_eq!(
            app_a.deliver_ghost(&mut spaces_a, &wire(position, C_ADDR)),
            DeliverGhost::Dropped
        );
    }
}
