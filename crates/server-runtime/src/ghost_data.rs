//! BW-08-04-02: replicating a Real's properties and position to its Ghosts.
//!
//! A Ghost is created from a snapshot of the Real's state
//! (`Entity::createGhost` + `writeGhostDataToStreamInternal`, entity.cpp:1846):
//! the position, direction, on-ground flag, the last event number, the ghosted
//! properties and the routing addresses. After creation the Real keeps the
//! Ghost current with event-numbered property updates (`ghostedDataUpdate`,
//! cellapp_interface.hpp:328) and volatile position updates.
//!
//! The acceptance, "Ghost 与对应 Real 快照吻合", is that the Ghost's state
//! equals the Real's snapshot — both at creation and after every update the
//! Real sends — and that a late property update never rewinds the event
//! marker.
//!
//! BW-08-04-03 adds the Real-address tracking: during an offload the old Real
//! sends `ghostSetNextReal` (recording the pending address) and the new Real
//! sends `ghostSetReal` (switching the routing), validated by the
//! monotonically increasing `numTimesRealOffloaded` subsequence id. The
//! acceptance, "路由更新后抵达正确进程", is that after a successful routing
//! update a message to the Ghost arrives at the new process.
//!
//! Reference:
//! - `entity.cpp:1829` `writeGhostDataToStream` — the id and entity type.
//! - `entity.cpp:1846` `writeGhostDataToStreamInternal` — the snapshot fields:
//!   `numTimesRealOffloaded_`, `localPosition_`, `isOnGround_`,
//!   `lastEventNumber_`, `volatileInfo_`, the cell address, `baseAddr_`,
//!   `localDirection_`, the ghosted properties, the ghost controllers and
//!   `periodsWithoutWitness_`/`aoiUpdateSchemeID_`.
//! - `cellapp_interface.hpp:328` `ghostedDataUpdate` — the event-numbered
//!   property updates the Real streams to a Ghost.
//! - `entity.cpp:5032` `Controllers::updateGhost` — controller updates are
//!   part of the same replication stream.
//! - `cellapp_interface.hpp:307` `ghostSetReal` and `:310` `ghostSetNextReal`.
//! - `entity.cpp:4927` `ghostSetReal` — the subsequence-id check and the
//!   `pRealChannel_` switch.
//! - `entity.cpp:4970` `ghostSetNextReal` — the sign-off that records the
//!   pending Real.

use std::collections::BTreeMap;

use foundation::math::Vector3;

/// A routing address (a cell or base entity, simplified).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Address(pub u64);

/// `localDirection_`: yaw, pitch and roll.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Direction {
    /// The yaw.
    pub yaw: f64,
    /// The pitch.
    pub pitch: f64,
    /// The roll.
    pub roll: f64,
}

/// A ghosted property value, keyed by its data description index.
pub type PropertyValue = f64;

/// The state a Ghost holds, matching the Real's snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct GhostState {
    /// `localPosition_`.
    pub position: Vector3,
    /// `localDirection_`.
    pub direction: Direction,
    /// `isOnGround_`.
    pub is_on_ground: bool,
    /// `lastEventNumber_`: the latest property event the Ghost has seen.
    pub last_event_number: u32,
    /// The ghosted properties, indexed by their description index.
    pub properties: BTreeMap<u32, PropertyValue>,
}

/// The Real's ghost snapshot (`writeGhostDataToStreamInternal`).
#[derive(Debug, Clone, PartialEq)]
pub struct GhostSnapshot {
    /// `id_`.
    pub id: u32,
    /// The entity type id (`entityTypeID`).
    pub entity_type_id: u16,
    /// `numTimesRealOffloaded_`.
    pub num_times_real_offloaded: u32,
    /// The ghost-relevant state.
    pub state: GhostState,
    /// The cell's address (`CellApp::interface().address()`).
    pub real_address: Address,
    /// `baseAddr_` (the base entity address).
    pub base_address: Address,
    /// `aoiUpdateSchemeID_`.
    pub scheme_id: u8,
}

/// A property update the Real sends to a Ghost (`ghostedDataUpdate`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PropertyUpdate {
    /// The event number the change belongs to.
    pub event_number: u32,
    /// The data description index of the changed property.
    pub index: u32,
    /// The new value.
    pub value: PropertyValue,
}

/// A Ghost of one entity: a copy of the Real's state on a neighbouring cell.
#[derive(Debug, Clone, PartialEq)]
pub struct Ghost {
    /// The entity id.
    pub id: u32,
    /// The replicated state.
    pub state: GhostState,
    /// `pRealChannel_`: the address the Ghost routes the Real's messages to.
    pub real_address: Address,
    /// The base entity address.
    pub base_address: Address,
    /// `nextRealAddr_` (`entity.cpp:4977`): the pending Real during an
    /// offload, set by `ghostSetNextReal`.
    pub next_real_address: Option<Address>,
    /// `numTimesRealOffloaded_` (`entity.cpp:4932`): the subsequence id that
    /// must increase by one for each offload.
    pub num_times_real_offloaded: u32,
}

/// The result of a `ghostSetReal` attempt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SetRealOutcome {
    /// The subsequence id matched: the Ghost now routes to the new Real
    /// (`entity.cpp:4946`).
    Applied,
    /// The subsequence id was out of order, or the Ghost never saw the sign-off
    /// for this Real: the message is delayed and the routing is unchanged
    /// (`entity.cpp:4937` `delaySubsequence`).
    OutOfOrder,
}

impl Ghost {
    /// `Space::createGhost`: build the Ghost from the Real's snapshot.
    pub fn from_snapshot(snapshot: &GhostSnapshot) -> Ghost {
        Ghost {
            id: snapshot.id,
            state: snapshot.state.clone(),
            real_address: snapshot.real_address,
            base_address: snapshot.base_address,
            next_real_address: None,
            num_times_real_offloaded: snapshot.num_times_real_offloaded,
        }
    }

    /// Apply a volatile position update so the Ghost matches the Real's new
    /// position (`localPosition_`).
    pub fn apply_position(&mut self, position: Vector3) {
        self.state.position = position;
    }

    /// Apply a direction update.
    pub fn apply_direction(&mut self, direction: Direction) {
        self.state.direction = direction;
    }

    /// Apply a property update (`ghostedDataUpdate`). The event number only
    /// ever advances the Ghost's marker, so a late or out-of-order update
    /// never rewinds it.
    pub fn apply_property(&mut self, update: PropertyUpdate) {
        self.state.properties.insert(update.index, update.value);
        self.state.last_event_number = self.state.last_event_number.max(update.event_number);
    }

    /// Whether the Ghost's state matches a snapshot of the Real (`Ghost 与
    /// 对应 Real 快照吻合`).
    pub fn matches(&self, snapshot: &GhostSnapshot) -> bool {
        self.id == snapshot.id && self.state == snapshot.state
    }

    /// `ghostSetNextReal` (`entity.cpp:4970`): the old Real's sign-off before
    /// it offloads. The Ghost then only accepts messages from the pending Real.
    pub fn set_next_real(&mut self, next: Address) {
        self.next_real_address = Some(next);
    }

    /// `ghostSetReal` (`entity.cpp:4927`): validate the monotonically
    /// increasing subsequence id and, when it matches, switch the routing to
    /// the new Real (`pRealChannel_`).
    pub fn set_real(&mut self, owner: Address, num_times_real_offloaded: u32) -> SetRealOutcome {
        if num_times_real_offloaded != self.num_times_real_offloaded + 1
            || self.next_real_address != Some(owner)
        {
            return SetRealOutcome::OutOfOrder;
        }

        self.num_times_real_offloaded = num_times_real_offloaded;
        self.real_address = owner;
        self.next_real_address = None;
        SetRealOutcome::Applied
    }

    /// The address a message to this Ghost is routed to (the current Real).
    pub fn route_to(&self) -> Address {
        self.real_address
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> GhostSnapshot {
        GhostSnapshot {
            id: 42,
            entity_type_id: 3,
            num_times_real_offloaded: 0,
            state: GhostState {
                position: Vector3::new(10.0, 0.0, 20.0),
                direction: Direction {
                    yaw: 0.5,
                    pitch: 0.0,
                    roll: 0.0,
                },
                is_on_ground: true,
                last_event_number: 5,
                properties: BTreeMap::from([(0, 1.0), (1, 2.5)]),
            },
            real_address: Address(100),
            base_address: Address(200),
            scheme_id: 0,
        }
    }

    /// The acceptance: a Ghost built from the Real's snapshot matches it.
    #[test]
    fn a_ghost_created_from_a_snapshot_matches_the_real() {
        let snapshot = snapshot();
        let ghost = Ghost::from_snapshot(&snapshot);

        assert!(
            ghost.matches(&snapshot),
            "the Ghost equals the Real snapshot"
        );
        assert_eq!(ghost.id, 42);
        assert_eq!(ghost.real_address, Address(100));
        assert_eq!(ghost.base_address, Address(200));
        assert_eq!(ghost.state.last_event_number, 5);
        assert_eq!(ghost.state.properties, BTreeMap::from([(0, 1.0), (1, 2.5)]));
    }

    /// Applying the Real's changes keeps the Ghost equal to the new snapshot.
    #[test]
    fn applied_updates_keep_the_ghost_equal_to_the_real_snapshot() {
        let mut snapshot = snapshot();
        let mut ghost = Ghost::from_snapshot(&snapshot);

        // The Real moves and turns.
        snapshot.state.position = Vector3::new(11.0, 0.0, 21.0);
        snapshot.state.direction = Direction {
            yaw: 0.6,
            pitch: 0.1,
            roll: 0.0,
        };
        ghost.apply_position(snapshot.state.position);
        ghost.apply_direction(snapshot.state.direction);
        assert!(ghost.matches(&snapshot));

        // A ghosted property changes with a new event number.
        let update = PropertyUpdate {
            event_number: 6,
            index: 0,
            value: 3.0,
        };
        snapshot.state.properties.insert(0, 3.0);
        snapshot.state.last_event_number = 6;
        ghost.apply_property(update);
        assert!(ghost.matches(&snapshot), "the Ghost tracks the property");
        assert_eq!(ghost.state.last_event_number, 6);
    }

    /// A late property update advances the event marker but never rewinds it.
    #[test]
    fn a_late_property_update_never_rewinds_the_event_marker() {
        let snapshot = snapshot();
        let mut ghost = Ghost::from_snapshot(&snapshot);

        // Property 2 arrives with an older event number than the marker.
        ghost.apply_property(PropertyUpdate {
            event_number: 3,
            index: 2,
            value: 9.0,
        });
        assert_eq!(
            ghost.state.last_event_number, 5,
            "the marker did not rewind"
        );
        assert_eq!(ghost.state.properties.get(&2), Some(&9.0));

        // A newer event advances it.
        ghost.apply_property(PropertyUpdate {
            event_number: 7,
            index: 1,
            value: 4.0,
        });
        assert_eq!(ghost.state.last_event_number, 7);
    }

    /// A snapshot records the routing addresses and offload counter that the
    /// later tasks read back.
    #[test]
    fn the_snapshot_carries_the_routing_and_offload_state() {
        let mut snapshot = snapshot();
        snapshot.num_times_real_offloaded = 2;
        snapshot.real_address = Address(300);
        snapshot.base_address = Address(400);
        snapshot.scheme_id = 1;

        let ghost = Ghost::from_snapshot(&snapshot);
        assert_eq!(ghost.real_address, Address(300));
        assert_eq!(ghost.base_address, Address(400));
        assert!(ghost.matches(&snapshot));
    }

    /// A Ghost routes every message to its current Real.
    #[test]
    fn a_ghost_routes_messages_to_its_current_real() {
        let ghost = Ghost::from_snapshot(&snapshot());
        assert_eq!(ghost.route_to(), Address(100));
        assert_eq!(ghost.num_times_real_offloaded, 0);
        assert_eq!(ghost.next_real_address, None);
    }

    /// The acceptance: after a routing update, a message to the Ghost reaches
    /// the new process.
    #[test]
    fn after_a_routing_update_messages_reach_the_new_process() {
        let mut ghost = Ghost::from_snapshot(&snapshot());

        // The old Real signs off, then the new Real takes over.
        ghost.set_next_real(Address(200));
        assert_eq!(ghost.next_real_address, Some(Address(200)));
        assert_eq!(
            ghost.set_real(Address(200), 1),
            SetRealOutcome::Applied,
            "the subsequence id matched"
        );

        assert_eq!(
            ghost.route_to(),
            Address(200),
            "the message arrives at the new process"
        );
        assert_eq!(ghost.num_times_real_offloaded, 1);
        assert_eq!(
            ghost.next_real_address, None,
            "the pending address is cleared"
        );
    }

    /// An out-of-order subsequence id is rejected and the routing is unchanged.
    #[test]
    fn an_out_of_order_subsequence_id_is_rejected() {
        let mut ghost = Ghost::from_snapshot(&snapshot());
        ghost.set_next_real(Address(200));

        assert_eq!(
            ghost.set_real(Address(200), 3),
            SetRealOutcome::OutOfOrder,
            "the id must be exactly one more"
        );
        assert_eq!(ghost.route_to(), Address(100), "the routing did not change");
    }

    /// The Ghost must have seen the sign-off for the pending Real first
    /// (`entity.cpp:4941` `MF_ASSERT(nextRealAddr_ == args.owner)`).
    #[test]
    fn the_ghost_must_have_seen_the_sign_off_first() {
        let mut ghost = Ghost::from_snapshot(&snapshot());

        assert_eq!(
            ghost.set_real(Address(200), 1),
            SetRealOutcome::OutOfOrder,
            "no sign-off was received"
        );
        assert_eq!(ghost.route_to(), Address(100));
    }

    /// Offloads accumulate: each routing update advances the subsequence id.
    #[test]
    fn offloads_accumulate_across_several_reals() {
        let mut ghost = Ghost::from_snapshot(&snapshot());

        for (owner, n) in [(Address(200), 1), (Address(300), 2)] {
            ghost.set_next_real(owner);
            assert_eq!(ghost.set_real(owner, n), SetRealOutcome::Applied);
            assert_eq!(ghost.route_to(), owner);
        }
        assert_eq!(ghost.num_times_real_offloaded, 2);
    }
}
