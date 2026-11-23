//! BW-07-04-05: handle a CellApp death.
//!
//! When a CellApp dies its cells are redistributed to surviving CellApps, and
//! each BaseApp that had base entities with their cell part on the dead app
//! must restore them to the new location. The CellAppMgr gathers every
//! surviving CellApp's acknowledgment first so the BaseApps get a complete
//! picture of the entity population (`cellappmgr.cpp:1879`), then broadcasts to
//! each BaseApp a `handleCellAppDeath` message carrying the dead app's address
//! and a space→new-cell-app map (`baseapp.cpp:1960`).
//!
//! `DyingCellApp` (`dead_cell_apps.cpp:27`) walks the bases and for each one
//! whose `cellAddr()` is the dead app (`dead_cell_apps.cpp:245`) looks up the
//! new destination for its space (`destinationFor`, `:218`). With no restore
//! locations at all the base is discarded (`pBase->discard`, `:267`); otherwise
//! it is restored to the new cell app (`Base::restoreTo`, `base.cpp:3819`),
//! which calls `setCurrentCell` so the base's `cell_entity_addr` becomes the new
//! app. A space with no alternate CellApp is handed to the CellAppMgr to place
//! (`restoreTo` with `Address::NONE`), and a new destination that is itself a
//! recently dead CellApp is likewise treated as missing. The walk stops when the
//! next game tick is pending (`:281`), resuming next tick; once every affected
//! base has been handled the app is forgotten. `DeadCellApps`
//! (`dead_cell_apps.cpp:28`) keeps the set of recently dead apps and the queue
//! of dying apps still being restored.
//!
//! This module models the death record, the space→destination map and the
//! restore walk over the bases, and the recently-dead set that makes a restore
//! to a dead destination immediate.

use crate::baseappmgr::Addr;
use crate::cell_data::BaseAddr;
use crate::entity_creator::{BaseEntity, EntityId};
use foundation::stream::BinaryStream;
use std::collections::{BTreeMap, VecDeque};

/// How long a CellApp stays in the recently-dead set (`DeadCellApp::MAX_AGE`,
/// `dead_cell_apps.cpp:75`, 10 seconds; a tick is modelled as one second).
const MAX_AGE_TICKS: u32 = 10;

/// The address used for "no alternate CellApp"
/// (`Mercury::Address::NONE`, `dead_cell_apps.cpp:224`).
const NO_CELL_APP: Addr = (0, 0);

/// What happened to one base entity whose cell part was on the dead CellApp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreOutcome {
    /// Restored to a specific CellApp; the base's `cell_entity_addr` is the new
    /// app (`restoreTo` with an address, `base.cpp:3859`).
    ToCellApp(Addr),
    /// Restored via the CellAppMgr; the base has no cell entity address yet
    /// (`restoreTo` with `Address::NONE`, `base.cpp:3849`).
    ViaCellAppMgr,
    /// The base is discarded; there were no restore locations at all
    /// (`pBase->discard`, `dead_cell_apps.cpp:267`).
    Discarded,
}

/// What one [`DeadCellApps::tick`] did.
#[derive(Debug, Clone, Default)]
pub struct DeadCellTickOutcome {
    /// The bases handled this tick and how, in walk order.
    pub restored: Vec<(EntityId, RestoreOutcome)>,
    /// The dead apps whose restore walk completed this tick, in order.
    pub completed: Vec<Addr>,
}

/// The BaseApp's view of CellApp deaths: the recently dead apps (so a restore
/// to one is made immediate) and the queue of dying apps still being restored
/// (`DeadCellApps`, `dead_cell_apps.cpp:28`).
#[derive(Debug, Clone)]
pub struct DeadCellApps {
    /// The recently dead apps with the tick they died at.
    recently_dead: Vec<(Addr, u32)>,
    /// The dying apps whose bases are still being restored, oldest first.
    dying: VecDeque<DyingCellApp>,
    /// The walk budget per tick, standing in for the `nextTickPending` cutoff
    /// (`dead_cell_apps.cpp:281`).
    budget: usize,
}

impl Default for DeadCellApps {
    fn default() -> Self {
        DeadCellApps {
            recently_dead: Vec::new(),
            dying: VecDeque::new(),
            budget: usize::MAX,
        }
    }
}

impl DeadCellApps {
    /// An empty view.
    pub fn new() -> Self {
        Self::default()
    }

    /// The per-tick walk budget for a dying app.
    pub fn with_budget(mut self, budget: usize) -> Self {
        self.budget = budget;
        self
    }

    /// `DeadCellApps::isRecentlyDead` (`dead_cell_apps.cpp:92`): whether the
    /// app died recently.
    pub fn is_recently_dead(&self, addr: Addr) -> bool {
        self.recently_dead.iter().any(|(a, _)| *a == addr)
    }

    /// `DeadCellApps::addApp` (`dead_cell_apps.cpp:111`): remember the app and
    /// queue its restore walk. `data` is the `handleCellAppDeath` payload, a
    /// list of `(SpaceID, Address)` pairs for the new destinations.
    pub fn add_app(&mut self, dead_addr: Addr, data: &[u8], now: u32) {
        self.remove_old(now);
        self.recently_dead.push((dead_addr, now));
        let new_destinations = parse_destinations(data);
        self.dying
            .push_back(DyingCellApp::new(dead_addr, new_destinations, self.budget));
    }

    /// `DeadCellApps::tick` (`dead_cell_apps.cpp:151`): advance the restore
    /// walks, processing a budget of bases per dying app. A dying app whose
    /// walk completes is dropped.
    pub fn tick(
        &mut self,
        bases: &mut BTreeMap<EntityId, BaseEntity>,
        now: u32,
    ) -> DeadCellTickOutcome {
        self.remove_old(now);
        let recent: Vec<Addr> = self.recently_dead.iter().map(|(a, _)| *a).collect();
        let mut outcome = DeadCellTickOutcome::default();

        while let Some(front) = self.dying.front_mut() {
            let (done, handled) = front.tick(bases, &recent);
            outcome.restored.extend(handled);
            if done {
                let done_app = self.dying.pop_front().expect("front just borrowed");
                outcome.completed.push(done_app.dead_addr);
            } else {
                break;
            }
        }
        outcome
    }

    /// `DeadCellApps::removeOldApps` (`dead_cell_apps.cpp:128`): forget the apps
    /// that died too long ago.
    fn remove_old(&mut self, now: u32) {
        self.recently_dead
            .retain(|(_, died_at)| now - *died_at < MAX_AGE_TICKS);
    }
}

/// One dead CellApp being restored (`DyingCellApp`, `dead_cell_apps.cpp:27`):
/// the dead app and the space→new-cell-app map.
#[derive(Debug, Clone)]
struct DyingCellApp {
    /// The dead app's address.
    dead_addr: Addr,
    /// The new CellApp per space, from the death message.
    new_destinations: BTreeMap<u32, Addr>,
    /// How many bases to restore per tick.
    budget: usize,
    /// How many bases were discarded (no restore locations).
    num_discarded: u32,
}

impl DyingCellApp {
    fn new(dead_addr: Addr, new_destinations: BTreeMap<u32, Addr>, budget: usize) -> Self {
        DyingCellApp {
            dead_addr,
            new_destinations,
            budget,
            num_discarded: 0,
        }
    }

    /// `DyingCellApp::destinationFor` (`dead_cell_apps.cpp:218`): the new CellApp
    /// for a space, or `Address::NONE` if the space is not in the map.
    fn destination_for(&self, space_id: u32) -> Addr {
        self.new_destinations
            .get(&space_id)
            .copied()
            .unwrap_or(NO_CELL_APP)
    }

    /// `DyingCellApp::tick` (`dead_cell_apps.cpp:234`): restore up to the budget
    /// of the bases whose cell part was on the dead app. Returns whether the
    /// walk is complete.
    fn tick(
        &mut self,
        bases: &mut BTreeMap<EntityId, BaseEntity>,
        recently_dead: &[Addr],
    ) -> (bool, Vec<(EntityId, RestoreOutcome)>) {
        let affected: Vec<EntityId> = bases
            .iter()
            .filter(|(_, base)| cell_addr_of(base) == Some(self.dead_addr))
            .map(|(id, _)| *id)
            .collect();

        let mut handled = Vec::new();
        for (index, id) in affected.iter().enumerate() {
            if index >= self.budget {
                return (false, handled);
            }
            let outcome = self.handle_base(*id, bases, recently_dead);
            handled.push((*id, outcome));
        }
        (true, handled)
    }

    /// Restore or discard one affected base.
    fn handle_base(
        &mut self,
        id: EntityId,
        bases: &mut BTreeMap<EntityId, BaseEntity>,
        recently_dead: &[Addr],
    ) -> RestoreOutcome {
        if self.new_destinations.is_empty() {
            bases.remove(&id);
            self.num_discarded += 1;
            return RestoreOutcome::Discarded;
        }

        let space_id = bases
            .get(&id)
            .expect("an affected base is present")
            .cell_space_id;
        let mut dest = self.destination_for(space_id);
        if recently_dead.contains(&dest) {
            dest = NO_CELL_APP;
        }

        if dest == NO_CELL_APP {
            if let Some(base) = bases.get_mut(&id) {
                base.cell_entity_addr = None;
            }
            RestoreOutcome::ViaCellAppMgr
        } else {
            if let Some(base) = bases.get_mut(&id) {
                base.cell_entity_addr = Some(cell_addr(dest));
            }
            RestoreOutcome::ToCellApp(dest)
        }
    }
}

/// Parse the `handleCellAppDeath` payload: `(SpaceID, Address)` pairs
/// (`DyingCellApp` constructor, `dead_cell_apps.cpp:197`).
fn parse_destinations(data: &[u8]) -> BTreeMap<u32, Addr> {
    let mut stream = BinaryStream::new();
    stream.write_bytes(data);
    let mut map = BTreeMap::new();
    while let Ok(space_id) = stream.read_u32() {
        let Ok(ip) = stream.read_u32() else {
            break;
        };
        let Ok(port) = stream.read_u16() else {
            break;
        };
        map.insert(space_id, (ip, port));
    }
    map
}

/// A base's cell entity address as an `Addr`.
fn cell_addr_of(base: &BaseEntity) -> Option<Addr> {
    base.cell_entity_addr
        .as_ref()
        .map(|addr| (addr.ip, addr.port))
}

/// The `Addr` form of a cell entity address, for setting the base's cell part.
fn cell_addr(addr: Addr) -> BaseAddr {
    BaseAddr {
        ip: addr.0,
        port: addr.1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn base(id: EntityId, space_id: u32, cell: Option<Addr>) -> BaseEntity {
        BaseEntity {
            id,
            type_id: 1,
            type_name: "Account".to_string(),
            db_id: 7,
            is_proxy: false,
            destroyed: false,
            in_destroy: false,
            base_props: HashMap::new(),
            cell_props: HashMap::new(),
            is_create_cell_pending: false,
            is_get_cell_pending: false,
            cell_entity_addr: cell.map(cell_addr),
            cell_space_id: space_id,
            callbacks: Vec::new(),
        }
    }

    /// Encode the death payload: a `(SpaceID, Address)` pair per destination.
    fn destinations(entries: &[(u32, Addr)]) -> Vec<u8> {
        let mut stream = BinaryStream::new();
        for (space_id, (ip, port)) in entries {
            stream.write_u32(*space_id);
            stream.write_u32(*ip);
            stream.write_u16(*port);
        }
        stream.as_bytes().to_vec()
    }

    fn bases_of(entities: &[BaseEntity]) -> BTreeMap<EntityId, BaseEntity> {
        entities
            .iter()
            .map(|base| (base.id, base.clone()))
            .collect()
    }

    /// Bases on the dead app are restored to the new CellApp for their space;
    /// the base's cell address becomes the new app.
    #[test]
    fn affected_bases_are_restored_to_the_new_cell_app() {
        let dead = (2, 2000);
        let mut dead_cell_apps = DeadCellApps::new();
        dead_cell_apps.add_app(dead, &destinations(&[(5, (3, 3000)), (6, (3, 3000))]), 0);
        let mut bases = bases_of(&[
            base(1, 5, Some(dead)),
            base(2, 6, Some(dead)),
            base(3, 7, Some((4, 4000))), // unaffected
        ]);

        let outcome = dead_cell_apps.tick(&mut bases, 1);

        assert_eq!(outcome.completed, vec![dead]);
        assert_eq!(
            outcome.restored,
            vec![
                (1, RestoreOutcome::ToCellApp((3, 3000))),
                (2, RestoreOutcome::ToCellApp((3, 3000)))
            ]
        );
        assert_eq!(cell_addr_of(&bases[&1]), Some((3, 3000)));
        assert_eq!(cell_addr_of(&bases[&2]), Some((3, 3000)));
        assert_eq!(cell_addr_of(&bases[&3]), Some((4, 4000)), "untouched");
    }

    /// With no restore locations at all every affected base is discarded.
    #[test]
    fn no_restore_locations_discards_the_affected_bases() {
        let dead = (2, 2000);
        let mut dead_cell_apps = DeadCellApps::new();
        dead_cell_apps.add_app(dead, &destinations(&[]), 0);
        let mut bases = bases_of(&[base(1, 5, Some(dead)), base(2, 6, Some(dead))]);

        let outcome = dead_cell_apps.tick(&mut bases, 1);

        assert_eq!(outcome.completed, vec![dead]);
        assert!(bases.is_empty(), "discarded bases are gone");
        assert_eq!(
            outcome.restored,
            vec![
                (1, RestoreOutcome::Discarded),
                (2, RestoreOutcome::Discarded)
            ]
        );
    }

    /// A space with no alternate CellApp is restored via the CellAppMgr, so the
    /// base has no cell address.
    #[test]
    fn a_space_without_a_destination_is_restored_via_the_mgr() {
        let dead = (2, 2000);
        let mut dead_cell_apps = DeadCellApps::new();
        dead_cell_apps.add_app(dead, &destinations(&[(5, (3, 3000))]), 0);
        let mut bases = bases_of(&[base(1, 6, Some(dead))]);

        let outcome = dead_cell_apps.tick(&mut bases, 1);

        assert_eq!(outcome.restored, vec![(1, RestoreOutcome::ViaCellAppMgr)]);
        assert_eq!(bases[&1].cell_entity_addr, None);
    }

    /// A new destination that is itself a recently dead CellApp is treated as
    /// missing and handed to the CellAppMgr.
    #[test]
    fn a_dead_destination_is_treated_as_missing() {
        let dead = (2, 2000);
        let second_dead = (9, 9);
        let mut dead_cell_apps = DeadCellApps::new();
        // The alternate for space 5 is itself already dead.
        dead_cell_apps.add_app(second_dead, &destinations(&[]), 0);
        dead_cell_apps.add_app(dead, &destinations(&[(5, second_dead)]), 0);
        let mut bases = bases_of(&[base(1, 5, Some(dead))]);

        let outcome = dead_cell_apps.tick(&mut bases, 1);

        assert_eq!(outcome.restored, vec![(1, RestoreOutcome::ViaCellAppMgr)]);
    }

    /// A budgeted walk spreads the restore over ticks, resuming where it left
    /// off because an already-restored base no longer points at the dead app.
    #[test]
    fn a_walk_spreads_over_ticks_when_budgeted() {
        let dead = (2, 2000);
        let mut dead_cell_apps = DeadCellApps::new().with_budget(1);
        dead_cell_apps.add_app(dead, &destinations(&[(5, (3, 3000))]), 0);
        let mut bases = bases_of(&[
            base(1, 5, Some(dead)),
            base(2, 5, Some(dead)),
            base(3, 5, Some(dead)),
        ]);

        let first = dead_cell_apps.tick(&mut bases, 1);
        assert_eq!(first.restored.len(), 1);
        assert!(first.completed.is_empty(), "walk is not done");

        let second = dead_cell_apps.tick(&mut bases, 2);
        assert_eq!(second.restored.len(), 1);
        assert!(second.completed.is_empty());

        let third = dead_cell_apps.tick(&mut bases, 3);
        assert_eq!(third.restored.len(), 1);
        assert_eq!(third.completed, vec![dead]);

        for id in [1, 2, 3] {
            assert_eq!(cell_addr_of(&bases[&id]), Some((3, 3000)));
        }
    }

    /// The recently-dead set forgets an app after the age cutoff.
    #[test]
    fn a_death_is_forgotten_after_the_age_cutoff() {
        let mut dead_cell_apps = DeadCellApps::new();
        dead_cell_apps.add_app((2, 2000), &destinations(&[]), 0);
        assert!(dead_cell_apps.is_recently_dead((2, 2000)));

        let mut bases = bases_of(&[base(1, 5, Some((4, 4000)))]);
        let _ = dead_cell_apps.tick(&mut bases, MAX_AGE_TICKS);

        assert!(
            !dead_cell_apps.is_recently_dead((2, 2000)),
            "forgotten after the cutoff"
        );
    }

    /// A base whose cell part is not on the dead app is never touched.
    #[test]
    fn an_unaffected_base_is_not_touched() {
        let dead = (2, 2000);
        let mut dead_cell_apps = DeadCellApps::new();
        dead_cell_apps.add_app(dead, &destinations(&[(5, (3, 3000))]), 0);
        let mut bases = bases_of(&[base(1, 5, Some((4, 4000))), base(2, 5, None)]);

        let outcome = dead_cell_apps.tick(&mut bases, 1);

        assert!(outcome.restored.is_empty());
        assert_eq!(cell_addr_of(&bases[&1]), Some((4, 4000)));
    }
}
