//! The noise propagation: `Entity.makeNoise` scales a radius off the
//! noise level, walks the range list both ways, and every hearer inside
//! gets `onNoise` with the payload tuple.
//!
//! Reference faces:
//! - `server/cellapp/entity.cpp` — `Entity::makeNoise` (:7329, the
//!   real-only script face), `outdoorPropagateNoise` (:7283, the
//!   visitor walk and the strict `<` distance gate), `getEntitiesInRange`
//!   (:6913, the two-way x-chain walk with the `<=` squared gate),
//!   `heardNoise` (:7353, the
//!   `onNoise( who, propRange, distance, event, info )` callback).
//! - `server/cellapp/noise_config.cpp` — the option defaults:
//!   `standardRange` 10.0, the witness speed thresholds 1000000.0 (the
//!   squared one is its square, `postInit`).
//! - `server/cellapp/witness.cpp:2398` — the movement-noise producer:
//!   the stealth propagation calls `makeNoise( stealthFactor_, 0 )`,
//!   the port pumps the recorded `(level, event)` pairs.

use crate::cellapp_space::CellApp;

// ---------------------------------------------------------------------------
// Section: the config defaults and the delivery record
// ---------------------------------------------------------------------------

/// `NoiseConfig::standardRange`'s default (`noise_config.cpp`): a
/// level 1.0 noise carries this many metres.
pub const NOISE_STANDARD_RANGE: f32 = 10.0;

/// `NoiseConfig::verticalSpeed`'s default — no ordinary fall trips it.
pub const NOISE_VERTICAL_SPEED: f32 = 1_000_000.0;

/// `NoiseConfig::horizontalSpeed`'s default; the witness compares
/// against its square (`NoiseConfig::postInit`).
pub const NOISE_HORIZONTAL_SPEED: f32 = 1_000_000.0;

/// One `onNoise` delivery: `(who, propRange, distance, event, info)`
/// — the payload tuple the script call alone cannot carry.
#[derive(Clone, Debug, PartialEq)]
pub struct NoiseCall {
    /// The hearer the callback ran on.
    pub entity: u32,
    /// The entity that made the noise (`who`).
    pub source: u32,
    /// The propagation range the noise carries (`propRange`).
    pub prop_range: f32,
    /// How far away it was heard (`distance`).
    pub distance: f32,
    /// The event number.
    pub event: i32,
    /// The info field.
    pub info: i32,
}

// ---------------------------------------------------------------------------
// Section: the script face and the witness pump
// ---------------------------------------------------------------------------

impl CellApp {
    /// `Entity.makeNoise` (`entity.cpp:7329`): real-only (the script
    /// face answers a TypeError otherwise), the level scales the radius
    /// off [`NOISE_STANDARD_RANGE`], and every entity in range hears
    /// `onNoise` — the maker itself never does (the walk starts at its
    /// neighbours).
    pub fn make_noise(&mut self, entity: u32, noise_level: f32, event: i32, info: i32) -> bool {
        let propagation_range = noise_level * NOISE_STANDARD_RANGE;
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return false;
        };
        // `isRealToScript()`: the port has no
        // treatAllOtherEntitiesAsGhosts, so the real face is `isReal()`.
        if !entity_rc.borrow().is_real {
            // PyErr TypeError "This method can only be called on a
            // real entity."
            return false;
        }
        let (node, space_id, position) = {
            let borrowed = entity_rc.borrow();
            match borrowed.range_node {
                Some(node) => (node, borrowed.space_id, borrowed.position),
                // `pChunk_ == NULL`: real, but nothing carries the
                // noise — the reference answers true regardless.
                None => return true,
            }
        };
        let Some(list) = self.range_lists.get(&space_id) else {
            return true;
        };
        let candidates = list.entities_in_x_range(node, position, propagation_range);
        // `outdoorPropagateNoise`'s strict gate: the walk's `<=`
        // collected the candidates, `dist < range` decides the
        // deliveries (the distances are read off the live positions
        // before any callback runs — the pump convention).
        let mut heard = Vec::new();
        for who in candidates {
            let Some(who_rc) = self.entity_population.get(&who) else {
                continue;
            };
            let who_pos = who_rc.borrow().position;
            let (dx, dy, dz) = (
                who_pos[0] - position[0],
                who_pos[1] - position[1],
                who_pos[2] - position[2],
            );
            let dist = (dx * dx + dy * dy + dz * dz).sqrt();
            if dist < propagation_range {
                heard.push((who, dist));
            }
        }
        // `heardNoise` (`entity.cpp:7353`):
        // `onNoise( who, propRange, distance, event, info )` with
        // `okIfFnNull` — a hearer without the method stays quiet.
        for (who, dist) in heard {
            self.noise_calls.push(NoiseCall {
                entity: who,
                source: entity,
                prop_range: propagation_range,
                distance: dist,
                event,
                info,
            });
            self.entity_callback(who, "onNoise", true, None);
        }
        true
    }

    /// The witness pump: `Witness::newPosition`'s stealth propagation
    /// (`witness.cpp:2398`) called `makeNoise( stealthFactor_, 0 )`
    /// directly; the port's witness records `(level, event)` pairs and
    /// this pump delivers them — the same stand-in the trap sink uses.
    pub fn dispatch_witness_noises(&mut self) {
        let mut due = Vec::new();
        for (&entity, witness) in self.witnesses.iter_mut() {
            for (level, event) in witness.noises.drain(..) {
                due.push((entity, level, event as i32));
            }
        }
        for (entity, level, event) in due {
            self.make_noise(entity, level, event, 0);
        }
    }
}

// ---------------------------------------------------------------------------
// Section: the tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cellapp_real_entity::{ADD_ENTITY_DATA_IS_ON_GROUND, EntityScript};
    use crate::cellapp_space::Space;
    use crate::cellapp_witness::Witness;
    use crate::cellappmgr::SpaceId;
    use std::collections::BTreeMap;

    const SID: SpaceId = 3;
    const TYPE: u32 = 7;

    /// The type's script: only `onNoise`, so the delivery is the whole
    /// observable.
    fn noise_script() -> EntityScript {
        let mut methods = std::collections::BTreeSet::new();
        methods.insert("__init__");
        methods.insert("onNoise");
        EntityScript::with_methods(methods, BTreeMap::new())
    }

    /// The creation stream: id + type, position, the on-ground flag,
    /// direction, last event number, base address, witness byte.
    fn create_stream(id: u32, pos: [f32; 3]) -> Vec<u8> {
        let mut stream = Vec::new();
        stream.extend_from_slice(&id.to_le_bytes());
        stream.extend_from_slice(&TYPE.to_le_bytes());
        for value in pos {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.push(ADD_ENTITY_DATA_IS_ON_GROUND);
        for value in [0.0_f32; 3] {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.extend_from_slice(&42u32.to_le_bytes());
        stream.extend_from_slice(&7u32.to_le_bytes());
        stream.extend_from_slice(&6502u16.to_le_bytes());
        stream.push(b'-');
        stream
    }

    /// The maker (entity 1) at the origin and the hearers at the
    /// acceptance's radii: 5 m in, exactly at, 15 m out, the diagonal
    /// inside x but outside 3D, and one behind on the negative side.
    fn fixture() -> CellApp {
        fixture_with(&[
            (2, [5.0, 0.0, 0.0]),
            (3, [10.0, 0.0, 0.0]),
            (4, [15.0, 0.0, 0.0]),
            (5, [8.0, 9.0, 0.0]),
            (6, [-7.0, 0.0, 0.0]),
        ])
    }

    fn fixture_with(hearers: &[(u32, [f32; 3])]) -> CellApp {
        let mut app = CellApp::default();
        let mut spaces = BTreeMap::new();
        spaces.insert(SID, Space::new(SID));
        app.entity_scripts.insert(TYPE, noise_script());
        app.create_real_entity(&mut spaces, SID, &create_stream(1, [0.0; 3]));
        for &(id, pos) in hearers {
            app.create_real_entity(&mut spaces, SID, &create_stream(id, pos));
        }
        app.spaces = spaces;
        app
    }

    /// The script calls the noise dispatch made, creation calls aside.
    fn noise_script_calls(app: &CellApp) -> Vec<(u32, &'static str)> {
        app.script_calls
            .iter()
            .filter(|call| call.name == "onNoise")
            .map(|call| (call.entity, call.name))
            .collect()
    }

    #[test]
    fn a_noise_reaches_by_radius_and_distance() {
        let mut app = fixture();
        assert!(app.make_noise(1, 1.0, 7, 42));
        // 5 m in and the 7 m hearer behind; the 12.04 m diagonal (x 8
        // within, 3D out), exactly 10 m and 15 m stay out.
        assert_eq!(
            app.noise_calls
                .iter()
                .map(|call| (call.entity, call.distance))
                .collect::<Vec<_>>(),
            vec![(2, 5.0), (6, 7.0)]
        );
        let call = &app.noise_calls[0];
        assert_eq!(
            (call.source, call.prop_range, call.event, call.info),
            (1, 10.0, 7, 42)
        );
        assert_eq!(
            noise_script_calls(&app),
            vec![(2, "onNoise"), (6, "onNoise")]
        );
    }

    #[test]
    fn the_level_scales_the_radius() {
        // Level 2.0 doubles the radius: the exact-10 hearer is inside
        // now, so is the 15 m one.
        let mut app = fixture();
        assert!(app.make_noise(1, 2.0, 0, 0));
        assert_eq!(
            app.noise_calls.iter().map(|c| c.entity).collect::<Vec<_>>(),
            vec![2, 5, 3, 4, 6]
        );
        assert_eq!(app.noise_calls[2].prop_range, 20.0);
        assert!((app.noise_calls[2].distance - 10.0).abs() < 1e-5);
        assert!((app.noise_calls[3].distance - 15.0).abs() < 1e-5);

        // Level 0.6: a 6 m radius — only the 5 m hearer (the exact-5
        // answer at level 0.5 is the strict gate's).
        let mut app = fixture();
        assert!(app.make_noise(1, 0.6, 0, 0));
        assert_eq!(
            app.noise_calls.iter().map(|c| c.entity).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(app.noise_calls[0].prop_range, 6.0);
    }

    #[test]
    fn the_boundary_gate_is_strict() {
        let mut app = fixture();
        app.make_noise(1, 1.0, 0, 0);
        // Exactly at the radius: the walk's `<=` visited, the strict
        // `<` withheld the delivery.
        assert!(!app.noise_calls.iter().any(|call| call.entity == 3));
        // A hair inside hears.
        let mut app = fixture_with(&[(2, [9.999, 0.0, 0.0])]);
        app.make_noise(1, 1.0, 0, 0);
        assert_eq!(app.noise_calls.len(), 1);
        assert!((app.noise_calls[0].distance - 9.999).abs() < 1e-4);
    }

    #[test]
    fn a_zero_level_noise_reaches_nobody() {
        // Co-located: the walk visits, the strict gate never delivers.
        let mut app = fixture_with(&[(2, [0.0; 3])]);
        assert!(app.make_noise(1, 0.0, 0, 0));
        assert!(app.noise_calls.is_empty());
    }

    #[test]
    fn an_unspaced_maker_stays_silent_but_answers_true() {
        // `pChunk_ == NULL`: a real with no range node carries nothing,
        // and the reference still answers true.
        let mut app = fixture();
        app.entity_population[&1].borrow_mut().range_node = None;
        assert!(app.make_noise(1, 1.0, 0, 0));
        assert!(app.noise_calls.is_empty());
        // A real whose space holds no range list answers true the same
        // way.
        let mut app = fixture();
        app.range_lists.remove(&SID);
        assert!(app.make_noise(1, 1.0, 0, 0));
        assert!(app.noise_calls.is_empty());
        // A candidate gone from the population is passed over.
        let mut app = fixture();
        app.entity_population.remove(&2);
        app.make_noise(1, 1.0, 0, 0);
        assert_eq!(
            app.noise_calls.iter().map(|c| c.entity).collect::<Vec<_>>(),
            vec![6]
        );
    }

    #[test]
    fn the_maker_never_hears_itself() {
        let mut app = fixture_with(&[]);
        assert!(app.make_noise(1, 1.0, 0, 0));
        assert!(app.noise_calls.is_empty());
        assert!(noise_script_calls(&app).is_empty());
    }

    #[test]
    fn a_ghost_or_stranger_makes_no_noise() {
        let mut app = fixture();
        {
            let rc = app.entity_population[&2].clone();
            rc.borrow_mut().is_real = false;
        }
        // The ghost's TypeError face: false, and nobody heard anything.
        assert!(!app.make_noise(2, 1.0, 0, 0));
        assert!(app.noise_calls.is_empty());
        // An unknown id answers false the same way.
        assert!(!app.make_noise(99, 1.0, 0, 0));
        assert!(app.noise_calls.is_empty());
    }

    #[test]
    fn a_hearer_without_on_noise_stays_quiet() {
        let mut app = fixture();
        // Strip the type's script: the deliveries still happen, but the
        // calls vanish and no warning is raised (`okIfFnNull`).
        app.entity_scripts.remove(&TYPE);
        app.make_noise(1, 1.0, 0, 0);
        assert_eq!(app.noise_calls.len(), 2);
        assert!(app.missing_callbacks.is_empty());
        assert!(noise_script_calls(&app).is_empty());
    }

    #[test]
    fn the_witness_pump_propagates_stealth_noise() {
        let mut app = fixture();
        app.witnesses.insert(1, Witness::new(1, SID, 500.0));
        // The fall arms the noise; ten ticks later it propagates. The
        // stealth factor 0.6 → a 6 m radius (a strict 5 m radius would
        // miss the hearer standing exactly on it).
        {
            let witness = app.witnesses.get_mut(&1).expect("the witness");
            witness.new_position(1, 0.6, 3.0, 25.0, [0.0; 3]);
            witness.new_position(2, 0.6, 3.0, 25.0, [0.0, -5.0, 0.0]);
            witness.new_position(13, 0.6, 3.0, 25.0, [0.0; 3]);
        }
        assert!(app.noise_calls.is_empty());
        // The tick's pump delivers it: level 0.6 → a 6 m radius.
        app.advance_tick();
        assert_eq!(app.noise_calls.len(), 1);
        let call = &app.noise_calls[0];
        assert_eq!(
            (
                call.entity,
                call.source,
                call.prop_range,
                call.event,
                call.info
            ),
            (2, 1, 6.0, 0, 0)
        );
        assert!((call.distance - 5.0).abs() < 1e-6);
        assert!(app.witnesses[&1].noises.is_empty());
        // The drain empties the pump: the next tick delivers nothing.
        app.advance_tick();
        assert_eq!(app.noise_calls.len(), 1);
    }
}
