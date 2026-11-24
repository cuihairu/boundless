//! BW-08-05-02: switching the Real/Ghost roles across an offload.
//!
//! An offload moves a Real from one cell to an adjacent one. On the old
//! cell the entity is demoted from a Real to a Ghost (`Cell::offloadEntity`
//! then `Entity::offload`), and on the destination cell an existing Ghost is
//! promoted to the Real (`Entity::onload` then `convertGhostToReal`). Every
//! other cell holding a Ghost of the entity is told of the new Real so its
//! routing converges too.
//!
//! The acceptance, "两端任意观测点可定位唯一 Real", is that after the
//! switch there is exactly one Real (the destination cell), and from any
//! observation point that holds a view of the entity the current Real can be
//! located — the Real cell itself, or a Ghost's `realAddr()`.
//!
//! Reference:
//! - `cell.cpp:193-226` `offloadEntity` — remove the Real from the cell and
//!   hand it to `Entity::offload`.
//! - `entity.cpp:1978` `Entity::offload` — start the `onload` message and
//!   call `convertRealToGhost`.
//! - `entity.cpp:1998-2070` `convertRealToGhost` — stream the Real data, set
//!   `pRealChannel_`/`nextRealAddr_` to the destination, call `offloadReal`,
//!   then drop the real-only properties so the entity is a Ghost.
//! - `entity.cpp:3056` `offloadReal` — destroy the Real part, notifying each
//!   haunt of the pending Real.
//! - `real_entity.cpp:260-296` `RealEntity::destroy` — send `ghostSetNextReal`
//!   (`nextRealAddr_`) to every haunt except the destination.
//! - `entity.cpp:4656` `onload` — the destination cell receives the streamed
//!   Real data and promotes its Ghost.
//! - `entity.cpp:4753` `convertGhostToReal` — `++numTimesRealOffloaded_`, read
//!   the Real data, add the entity to the cell's reals.
//! - `real_entity.cpp:294-400` `readOffloadData` — recreate the haunts and send
//!   `ghostSetReal` (`owner` = this app, `numTimesRealOffloaded`) to each.
//! - `entity.cpp:4927` `ghostSetReal` — the ghost validates the monotonic
//!   subsequence id and switches `pRealChannel_`.
//! - `entity.cpp:4970` `ghostSetNextReal` — the sign-off that records the
//!   pending Real.
//! - `entity.cpp:3291` `realAddr` — a Ghost routes to `pRealChannel_->addr()`,
//!   the current Real.

use std::collections::BTreeMap;

use crate::ghost_data::Address;

/// The role one cell's view of the entity plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// This cell holds the single Real (`Entity::isReal`).
    Real,
    /// This cell holds a Ghost routing to the current Real.
    Ghost,
}

/// One cell's view of the entity: its role and its routing to the Real.
#[derive(Debug, Clone, PartialEq)]
pub struct CellView {
    /// The cell holding this view.
    pub cell: Address,
    /// Whether this cell holds the Real or a Ghost.
    pub role: Role,
    /// The address a Ghost routes messages to (`realAddr()`,
    /// `entity.cpp:3291`). For the Real cell this is the cell itself.
    pub real_address: Address,
    /// The pending Real during a switch (`nextRealAddr_`,
    /// `entity.cpp:4977`).
    pub next_real_address: Option<Address>,
    /// `numTimesRealOffloaded_`: the subsequence id that must increase by one
    /// for each offload.
    pub num_times_real_offloaded: u32,
}

impl CellView {
    /// A Real on `cell` (freshly created, offload count 0).
    pub fn real(cell: Address) -> Self {
        CellView {
            cell,
            role: Role::Real,
            real_address: cell,
            next_real_address: None,
            num_times_real_offloaded: 0,
        }
    }

    /// A Ghost on `cell` routing to `real_address`.
    pub fn ghost(cell: Address, real_address: Address) -> Self {
        CellView {
            cell,
            role: Role::Ghost,
            real_address,
            next_real_address: None,
            num_times_real_offloaded: 0,
        }
    }

    /// Whether this view is the Real.
    pub fn is_real(&self) -> bool {
        self.role == Role::Real
    }

    /// `realAddr()` (`entity.cpp:3291`): the address messages to this view
    /// are routed to — the cell itself for the Real, `pRealChannel_` for a
    /// Ghost.
    pub fn route_to(&self) -> Address {
        if self.is_real() {
            self.cell
        } else {
            self.real_address
        }
    }

    /// `ghostSetNextReal` (`entity.cpp:4970`): the old Real's sign-off. The
    /// Ghost then only accepts the routing switch from the pending Real.
    pub fn set_next_real(&mut self, next: Address) {
        self.next_real_address = Some(next);
    }

    /// `ghostSetReal` (`entity.cpp:4927`): validate the monotonically
    /// increasing subsequence id and, when it matches, switch the routing to
    /// the new Real. Returns whether the switch was applied.
    pub fn set_real(&mut self, owner: Address, num_times_real_offloaded: u32) -> bool {
        if num_times_real_offloaded != self.num_times_real_offloaded + 1
            || self.next_real_address != Some(owner)
        {
            return false;
        }

        self.num_times_real_offloaded = num_times_real_offloaded;
        self.real_address = owner;
        self.next_real_address = None;
        true
    }
}

/// The outcome of `EntityRole::switch_roles`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchOutcome {
    /// The Real moved to the destination cell.
    Switched,
    /// `old_cell` does not hold the Real.
    NotReal,
    /// The destination cell holds no Ghost of the entity
    /// (`areWeHaunted` assertion, `real_entity.cpp:395`).
    NoGhostAtDestination,
}

/// The entity's role across every cell holding a view of it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EntityRole {
    /// The cell views, keyed by cell.
    pub views: BTreeMap<Address, CellView>,
}

impl EntityRole {
    /// An entity with its Real on `real_cell`.
    pub fn with_real(real_cell: Address) -> Self {
        let mut views = BTreeMap::new();
        views.insert(real_cell, CellView::real(real_cell));
        EntityRole { views }
    }

    /// Add a Ghost view on a cell.
    pub fn add_ghost(&mut self, view: CellView) {
        self.views.insert(view.cell, view);
    }

    /// The unique Real: `Some` only when exactly one view is Real
    /// (`唯一 Real`). A switch or a stale promotion must never leave two.
    pub fn real_cell(&self) -> Option<Address> {
        let reals: Vec<Address> = self
            .views
            .values()
            .filter(|v| v.is_real())
            .map(|v| v.cell)
            .collect();
        if reals.len() == 1 {
            Some(reals[0])
        } else {
            None
        }
    }

    /// From any observation point, locate the current Real: the Real cell
    /// itself, or a Ghost's `realAddr()`.
    pub fn locate_real(&self, from: Address) -> Option<Address> {
        let view = self.views.get(&from)?;
        if view.is_real() {
            Some(view.cell)
        } else {
            Some(view.real_address)
        }
    }

    /// `Cell::offloadEntity` + `Entity::onload`: move the Real from
    /// `old_cell` to `new_cell`, demoting the old Real to a Ghost and
    /// promoting the destination Ghost to the Real, then steering every other
    /// Ghost onto the new Real.
    pub fn switch_roles(&mut self, old_cell: Address, new_cell: Address) -> SwitchOutcome {
        // The entity must be Real on the old cell (`MF_ASSERT isReal`,
        // `cell.cpp:198`).
        if !self.views.get(&old_cell).is_some_and(CellView::is_real) {
            return SwitchOutcome::NotReal;
        }

        // The destination must hold a Ghost of the entity
        // (`areWeHaunted`, `real_entity.cpp:395`).
        if !self
            .views
            .get(&new_cell)
            .is_some_and(|v| v.role == Role::Ghost)
        {
            return SwitchOutcome::NoGhostAtDestination;
        }

        let old_count = self.views[&old_cell].num_times_real_offloaded;

        // Demote the old Real to a Ghost routing to the new cell
        // (`convertRealToGhost`: `pRealChannel_`/`nextRealAddr_` = dest).
        {
            let old = self.views.get_mut(&old_cell).expect("checked above");
            old.role = Role::Ghost;
            old.real_address = new_cell;
            old.next_real_address = Some(new_cell);
        }

        // Promote the destination Ghost to the Real
        // (`convertGhostToReal`: `++numTimesRealOffloaded_`).
        {
            let dest = self.views.get_mut(&new_cell).expect("checked above");
            dest.role = Role::Real;
            dest.num_times_real_offloaded = old_count + 1;
            dest.real_address = new_cell;
            dest.next_real_address = None;
        }

        // The old Real notifies every haunt except the destination with the
        // pending Real (`real_entity.cpp:265-283`).
        for view in self.views.values_mut() {
            if view.cell != new_cell && view.role == Role::Ghost {
                view.set_next_real(new_cell);
            }
        }

        // The new Real sends `ghostSetReal` to every remaining Ghost
        // (`real_entity.cpp:330-397`); each validates the subsequence id and
        // switches its routing.
        for view in self.views.values_mut() {
            if view.role == Role::Ghost {
                view.set_real(new_cell, old_count + 1);
            }
        }

        SwitchOutcome::Switched
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a() -> Address {
        Address(1)
    }
    fn b() -> Address {
        Address(2)
    }
    fn c() -> Address {
        Address(3)
    }

    /// An entity with a Real on A and ghosts on B and C.
    fn entity() -> EntityRole {
        let mut entity = EntityRole::with_real(a());
        entity.add_ghost(CellView::ghost(b(), a()));
        entity.add_ghost(CellView::ghost(c(), a()));
        entity
    }

    /// A fresh entity has exactly one Real, and every point locates it.
    #[test]
    fn a_fresh_entity_has_exactly_one_real() {
        let entity = entity();
        assert_eq!(entity.real_cell(), Some(a()));
        assert_eq!(entity.locate_real(a()), Some(a()));
        assert_eq!(entity.locate_real(b()), Some(a()));
        assert_eq!(entity.locate_real(c()), Some(a()));
    }

    /// The acceptance: after the switch, the destination is the unique Real,
    /// locatable from every observation point.
    #[test]
    fn switching_makes_the_new_cell_the_unique_real_from_every_point() {
        let mut entity = entity();
        assert_eq!(entity.switch_roles(a(), b()), SwitchOutcome::Switched);

        assert_eq!(
            entity.real_cell(),
            Some(b()),
            "exactly one Real, on the destination cell"
        );
        for point in [a(), b(), c()] {
            assert_eq!(
                entity.locate_real(point),
                Some(b()),
                "the Real is locatable from observation point {point:?}"
            );
        }
    }

    /// The old Real is demoted to a Ghost routing to the new cell
    /// (`convertRealToGhost`).
    #[test]
    fn the_old_real_becomes_a_ghost_routing_to_the_new_cell() {
        let mut entity = entity();
        entity.switch_roles(a(), b());

        let old = &entity.views[&a()];
        assert_eq!(old.role, Role::Ghost);
        assert_eq!(old.real_address, b());
        assert_eq!(old.route_to(), b());
        assert_eq!(old.next_real_address, None);
    }

    /// Every ghost advances its offload count to match the new Real
    /// (`++numTimesRealOffloaded_` + `ghostSetReal`).
    #[test]
    fn every_ghost_advances_its_offload_count() {
        let mut entity = entity();
        entity.switch_roles(a(), b());

        assert_eq!(entity.views[&b()].num_times_real_offloaded, 1);
        for cell in [a(), c()] {
            assert_eq!(
                entity.views[&cell].num_times_real_offloaded, 1,
                "ghost on {cell:?} caught up"
            );
        }
    }

    /// A neutral observation point converges on the new Real via
    /// `ghostSetNextReal` + `ghostSetReal`.
    #[test]
    fn a_neutral_ghost_converges_on_the_new_real() {
        let mut entity = entity();
        entity.switch_roles(a(), b());

        let neutral = &entity.views[&c()];
        assert_eq!(neutral.real_address, b());
        assert_eq!(neutral.route_to(), b());
    }

    /// The entity cannot be offloaded from a cell that only holds a Ghost.
    #[test]
    fn switching_a_ghost_cell_is_rejected() {
        let mut entity = entity();
        assert_eq!(entity.switch_roles(b(), c()), SwitchOutcome::NotReal);
        assert_eq!(entity.real_cell(), Some(a()), "the Real did not move");
    }

    /// The destination must already hold a Ghost of the entity
    /// (`areWeHaunted`).
    #[test]
    fn switching_to_a_cell_without_a_ghost_is_rejected() {
        let mut entity = EntityRole::with_real(a());
        assert_eq!(
            entity.switch_roles(a(), b()),
            SwitchOutcome::NoGhostAtDestination
        );
        assert_eq!(entity.real_cell(), Some(a()));
    }

    /// Repeated offloads accumulate: each switch advances the count and keeps
    /// the unique-Real invariant.
    #[test]
    fn repeated_switches_accumulate_offloads() {
        let mut entity = entity();
        assert_eq!(entity.switch_roles(a(), b()), SwitchOutcome::Switched);
        assert_eq!(entity.switch_roles(b(), c()), SwitchOutcome::Switched);

        assert_eq!(entity.real_cell(), Some(c()));
        assert_eq!(entity.views[&c()].num_times_real_offloaded, 2);
        for point in [a(), b(), c()] {
            assert_eq!(entity.locate_real(point), Some(c()));
        }
        for cell in [a(), b()] {
            assert_eq!(
                entity.views[&cell].num_times_real_offloaded, 2,
                "ghost on {cell:?} synced to the second offload"
            );
        }
    }

    /// A stale Ghost (its count is behind) rejects the switch and keeps its
    /// old routing — the case BW-08-05-04's message redirection handles. The
    /// unique-Real invariant still holds.
    #[test]
    fn a_stale_ghost_keeps_its_old_routing() {
        let mut entity = EntityRole::with_real(a());
        // A ghost that is several offloads behind.
        let mut stale = CellView::ghost(c(), a());
        stale.num_times_real_offloaded = 5;
        entity.add_ghost(stale);
        entity.add_ghost(CellView::ghost(b(), a()));

        assert_eq!(entity.switch_roles(a(), b()), SwitchOutcome::Switched);

        // The unique Real moved, but the stale ghost could not validate the
        // switch (`ghostSetReal` failed) and still routes to the old cell.
        assert_eq!(entity.real_cell(), Some(b()));
        assert_eq!(entity.locate_real(c()), Some(a()));
        assert_eq!(
            entity.views[&c()].num_times_real_offloaded,
            5,
            "the stale ghost did not advance"
        );
    }
}
