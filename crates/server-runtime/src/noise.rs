//! Noise perception: `Entity::makeNoise` and the propagation behind it.
//!
//! Ported from `cellapp/noise_config.*` (the `cellApp/noise/` options),
//! `entity.cpp:7273-7390` (`outdoorPropagateNoise` / `makeNoise` /
//! `heardNoise`), `cell_chunk.cpp:118-181` (the indoor portal walk) and
//! `witness.cpp:2370-2403` (movement noise, event 0).

use std::collections::BTreeMap;

/// `NoiseConfig` — the `cellApp/noise/` server options. The defaults are
/// the reference ones: the speed thresholds are set so high that movement
/// noise never trips unless the deployment lowers them.
pub struct NoiseConfig {
    /// `noise/standardRange` — `makeNoise(noiseLevel)` propagates
    /// `noiseLevel * standardRange` metres.
    pub standard_range: f32,
    /// `noise/verticalSpeed` — falling faster than this (strictly) trips
    /// movement noise.
    pub vertical_speed: f32,
    /// `noise/horizontalSpeed`.
    pub horizontal_speed: f32,
    /// `noise/horizontalSpeedSqr` — recomputed by `postInit`.
    pub horizontal_speed_sqr: f32,
}

impl Default for NoiseConfig {
    fn default() -> Self {
        NoiseConfig {
            standard_range: 10.0,
            vertical_speed: 1000000.0,
            horizontal_speed: 1000000.0,
            horizontal_speed_sqr: 0.0,
        }
    }
}

impl NoiseConfig {
    /// `NoiseConfig::postInit` (`noise_config.cpp:32-37`): the squared
    /// threshold trails the raw one.
    pub fn post_init(&mut self) {
        self.horizontal_speed_sqr = self.horizontal_speed * self.horizontal_speed;
    }
}

/// `Entity::heardNoise` — the payload of the script `onNoise` callback
/// (`entity.cpp:7373-7390`): who made it, how far it carries, how far it
/// travelled to reach us, and the two script integers.
#[derive(Clone, Debug, PartialEq)]
pub struct HeardNoise {
    /// The entity the callback is delivered to — script `this`.
    pub hearer: u32,
    pub who: u32,
    pub prop_range: f32,
    pub distance: f32,
    pub event: i32,
    pub info: i32,
}

/// `ChunkBoundary::Portal` — the noise-relevant slice. A portal without a
/// target chunk is represented by the target being absent from `chunks`.
#[derive(Clone, Debug)]
pub struct Portal {
    pub to_chunk: u32,
    pub centre: (f32, f32, f32),
    pub permissive: bool,
}

/// A chunk's noise-relevant property: `Chunk::isOutsideChunk()` decides
/// whether `makeNoise` walks portals or sweeps the open air.
#[derive(Clone, Copy, Debug)]
pub struct ChunkProps {
    pub outdoor: bool,
}

/// `pChunk_` + `position()` for one entity on the cell.
#[derive(Clone, Debug)]
pub struct NoiseEntity {
    pub position: (f32, f32, f32),
    pub chunk: Option<u32>,
}

/// The propagation harness: the cell's entities, its chunks and the
/// portals between them, plus the `onNoise` callbacks the last noise
/// produced.
#[derive(Default)]
pub struct NoiseWorld {
    pub config: NoiseConfig,
    entities: BTreeMap<u32, NoiseEntity>,
    chunks: BTreeMap<u32, ChunkProps>,
    portals: BTreeMap<u32, Vec<Portal>>,
    /// `Chunk::nextMark()` — one cell-wide counter handing out stamps.
    next_mark: u32,
    /// `chunk_.traverseMark()` per chunk.
    traverse_marks: BTreeMap<u32, u32>,
    /// `Entity::heardNoise` records, in propagation order.
    pub callbacks: Vec<HeardNoise>,
}

impl NoiseWorld {
    pub fn add_chunk(&mut self, chunk: u32, outdoor: bool) {
        self.chunks.insert(chunk, ChunkProps { outdoor });
    }

    pub fn add_entity(&mut self, id: u32, position: (f32, f32, f32), chunk: Option<u32>) {
        self.entities.insert(id, NoiseEntity { position, chunk });
    }

    pub fn add_portal(&mut self, from_chunk: u32, portal: Portal) {
        self.portals.entry(from_chunk).or_default().push(portal);
    }

    /// `Entity::makeNoise` (`entity.cpp:7329-7357`): only a real entity
    /// may call it, the range scales with `noiseLevel`, and the source's
    /// chunk picks the propagation model. A ghost returns false (the
    /// reference raises `TypeError`); no chunk means nothing is heard.
    pub fn make_noise(
        &mut self,
        who: u32,
        noise_level: f32,
        event: i32,
        info: i32,
        is_real: bool,
    ) -> bool {
        if !is_real {
            return false;
        }
        let propagation_range = noise_level * self.config.standard_range;
        let (position, chunk) = match self.entities.get(&who) {
            Some(e) => (e.position, e.chunk),
            None => return true,
        };
        match chunk.and_then(|c| self.chunks.get(&c).copied()) {
            Some(props) if props.outdoor => {
                self.outdoor_propagate_noise(who, position, propagation_range, event, info);
            }
            Some(_) => {
                self.propagate_noise(
                    who,
                    propagation_range,
                    position,
                    propagation_range,
                    event,
                    info,
                    0,
                );
            }
            None => {}
        }
        true
    }

    /// `Entity::outdoorPropagateNoise` (`entity.cpp:7273-7315`): the
    /// open-air sweep. `getEntitiesInRange` walks the range list's X
    /// axis — entities ahead of the source ascending first, then those
    /// behind descending — collecting everything within the squared
    /// radius; the hearing pass then re-checks with a strict `<`.
    fn outdoor_propagate_noise(
        &mut self,
        who: u32,
        position: (f32, f32, f32),
        range: f32,
        event: i32,
        info: i32,
    ) {
        let range_sqr = range * range;

        // The X sweep, in reference visit order.
        let mut forward: Vec<(u32, (f32, f32, f32))> = self
            .entities
            .iter()
            .filter(|(id, e)| **id != who && e.position.0 >= position.0)
            .map(|(id, e)| (*id, e.position))
            .collect();
        forward.sort_by(|a, b| a.1.0.partial_cmp(&b.1.0).unwrap().then(a.0.cmp(&b.0)));
        let mut backward: Vec<(u32, (f32, f32, f32))> = self
            .entities
            .iter()
            .filter(|(id, e)| **id != who && e.position.0 < position.0)
            .map(|(id, e)| (*id, e.position))
            .collect();
        backward.sort_by(|a, b| b.1.0.partial_cmp(&a.1.0).unwrap().then(a.0.cmp(&b.0)));

        let mut hears: Vec<(u32, f32)> = Vec::new();
        for (id, epos) in forward.into_iter().chain(backward) {
            let x_dist = (epos.0 - position.0).abs();
            if x_dist > range {
                continue;
            }
            let d2 = dist_sqr(position, epos);
            if d2 <= range_sqr {
                hears.push((id, d2.sqrt()));
            }
        }

        for (id, dist) in hears {
            if dist < range {
                self.heard_noise(id, who, range, dist, event, info);
            }
        }
    }

    /// `CellChunk::propagateNoise` (`cell_chunk.cpp:118-181`): the indoor
    /// walk. Entities hear with the *travelled* distance — the spent
    /// range plus the hop from wherever this chunk was entered — and
    /// portals hand the remainder on while it stays positive. The mark
    /// keeps a chunk from hearing the same noise twice.
    #[allow(clippy::too_many_arguments)]
    pub fn propagate_noise(
        &mut self,
        who: u32,
        propagation_range: f32,
        position: (f32, f32, f32),
        remaining_range: f32,
        event: i32,
        info: i32,
        mark: u32,
    ) {
        let start_chunk = match self.entities.get(&who).and_then(|e| e.chunk) {
            Some(c) => c,
            None => return,
        };
        let mark = if mark == 0 {
            self.next_mark += 1;
            self.traverse_marks.insert(start_chunk, self.next_mark);
            self.next_mark
        } else {
            mark
        };
        self.propagate_noise_in(
            start_chunk,
            who,
            propagation_range,
            position,
            remaining_range,
            event,
            info,
            mark,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn propagate_noise_in(
        &mut self,
        chunk_id: u32,
        who: u32,
        propagation_range: f32,
        position: (f32, f32, f32),
        remaining_range: f32,
        event: i32,
        info: i32,
        mark: u32,
    ) {
        let in_chunk: Vec<(u32, (f32, f32, f32))> = self
            .entities
            .iter()
            .filter(|(id, e)| **id != who && e.chunk == Some(chunk_id))
            .map(|(id, e)| (*id, e.position))
            .collect();
        for (id, epos) in in_chunk {
            let dist = dist_sqr(position, epos).sqrt();
            if dist < remaining_range {
                let travel_dist = (propagation_range - remaining_range) + dist;
                self.heard_noise(id, who, propagation_range, travel_dist, event, info);
            }
        }

        let portals = self.portals.get(&chunk_id).cloned().unwrap_or_default();
        for portal in portals {
            // `!portal.hasChunk()` — the far side does not exist here.
            if !portal.permissive || !self.chunks.contains_key(&portal.to_chunk) {
                continue;
            }
            let dist = dist_sqr(position, portal.centre).sqrt();
            let distance_left = remaining_range - dist;
            if distance_left > 0.0 {
                if self.traverse_marks.get(&portal.to_chunk) == Some(&mark) {
                    continue;
                }
                self.traverse_marks.insert(portal.to_chunk, mark);
                self.propagate_noise_in(
                    portal.to_chunk,
                    who,
                    propagation_range,
                    portal.centre,
                    distance_left,
                    event,
                    info,
                    mark,
                );
            }
        }
    }

    /// `Entity::heardNoise` — the `onNoise` script callback; script errors
    /// are swallowed by the reference, so the record lands either way.
    fn heard_noise(
        &mut self,
        hearer: u32,
        who: u32,
        prop_range: f32,
        distance: f32,
        event: i32,
        info: i32,
    ) {
        self.callbacks.push(HeardNoise {
            hearer,
            who,
            prop_range,
            distance,
            event,
            info,
        });
    }
}

fn dist_sqr(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
    let (dx, dy, dz) = (a.0 - b.0, a.1 - b.1, a.2 - b.2);
    dx * dx + dy * dy + dz * dz
}

/// The noise half of `Witness::newPosition` (`witness.cpp:2370-2403`):
/// speed thresholds flag the entity at most every other tick, and a flag
/// becomes event-0 noise carrying `stealthFactor` once more than ten
/// ticks have passed since the last shout.
#[derive(Default)]
pub struct MovementNoise {
    noise_check_time: u32,
    noise_propagated_time: u32,
    noise_made: bool,
    /// `Witness::stealthFactor` — the script-readable noise level used
    /// when movement noise propagates.
    pub stealth_factor: f32,
}

impl MovementNoise {
    /// `Witness::Witness` seeds both stamps with the app time.
    pub fn new(now: u32) -> Self {
        MovementNoise {
            noise_check_time: now,
            noise_propagated_time: now,
            noise_made: false,
            stealth_factor: 0.0,
        }
    }

    /// `Witness::newPosition`'s noise checks. Returns
    /// `Some((noise_level, event))` for the `makeNoise` the reference
    /// issues (`stealthFactor_, 0`), or `None` for a quiet tick.
    pub fn new_position(
        &mut self,
        now: u32,
        velocity: (f32, f32, f32),
        config: &NoiseConfig,
    ) -> Option<(f32, i32)> {
        let dur = now.wrapping_sub(self.noise_check_time);
        let since_noise = now.wrapping_sub(self.noise_propagated_time);

        if dur > 1 && !self.noise_made {
            if velocity.1 < -config.vertical_speed {
                self.noise_made = true;
            } else {
                let horiz_speed_sqr = velocity.0 * velocity.0 + velocity.2 * velocity.2;
                if horiz_speed_sqr > config.horizontal_speed_sqr {
                    self.noise_made = true;
                }
            }
            self.noise_check_time = now;
        }

        if since_noise > 10 && self.noise_made {
            let level = self.stealth_factor;
            self.noise_made = false;
            self.noise_propagated_time = now;
            Some((level, 0))
        } else {
            None
        }
    }
}

// -----------------------------------------------------------------------------
// Section: tests
// -----------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn portal(to_chunk: u32, centre: (f32, f32, f32)) -> Portal {
        Portal {
            to_chunk,
            centre,
            permissive: true,
        }
    }

    #[test]
    fn a_noise_reaches_only_entities_inside_its_radius() {
        let mut vw = NoiseWorld::default();
        vw.add_chunk(1, true);
        vw.add_entity(1, (0.0, 0.0, 0.0), Some(1));
        vw.add_entity(2, (9.9, 0.0, 0.0), Some(1));
        vw.add_entity(3, (10.0, 0.0, 0.0), Some(1));
        vw.add_entity(4, (12.0, 0.0, 0.0), Some(1));
        // Inside the X sweep's cutoff but 20 m off to the side: the full
        // radius check still rules it out.
        vw.add_entity(5, (9.0, 0.0, 20.0), Some(1));

        // Level 1 carries 10 m: 2 hears it, 3 sits exactly on the rim and
        // the strict `<` leaves it out.
        assert!(vw.make_noise(1, 1.0, 5, 7, true));
        assert_eq!(
            vw.callbacks,
            vec![HeardNoise {
                hearer: 2,
                who: 1,
                prop_range: 10.0,
                distance: 9.9,
                event: 5,
                info: 7
            }]
        );

        // Level 2 carries 20 m and the rim entity finally hears it, with
        // the level-scaled range in the payload.
        vw.callbacks.clear();
        assert!(vw.make_noise(1, 2.0, 6, 0, true));
        let heard: Vec<(u32, f32)> = vw
            .callbacks
            .iter()
            .map(|c| (c.hearer, c.distance))
            .collect();
        assert_eq!(heard, vec![(2, 9.9), (3, 10.0), (4, 12.0)]);
        assert_eq!(vw.callbacks[0].prop_range, 20.0);
    }

    #[test]
    fn the_outdoor_sweep_visits_the_plus_x_side_first() {
        let mut vw = NoiseWorld::default();
        vw.add_chunk(1, true);
        vw.add_entity(1, (0.0, 0.0, 0.0), Some(1));
        // Ahead of the source: closest first. Behind: closest first too,
        // but only after the whole +x side has been served.
        vw.add_entity(2, (1.0, 0.0, 0.0), Some(1));
        vw.add_entity(3, (5.0, 0.0, 0.0), Some(1));
        vw.add_entity(4, (-1.0, 0.0, 0.0), Some(1));
        vw.add_entity(5, (-4.0, 0.0, 0.0), Some(1));

        assert!(vw.make_noise(1, 1.0, 0, 0, true));
        let order: Vec<f32> = vw.callbacks.iter().map(|c| c.distance).collect();
        assert_eq!(order, vec![1.0, 5.0, 1.0, 4.0]);
    }

    #[test]
    fn an_indoor_noise_reports_the_path_distance() {
        let mut vw = NoiseWorld::default();
        vw.add_chunk(1, false);
        vw.add_chunk(2, false);
        // The portal sits 8 m away; what is left of a 10 m shout is 2 m.
        vw.add_portal(1, portal(2, (8.0, 0.0, 0.0)));
        // And one 12 m out: the 10 m shout cannot even reach it.
        vw.add_portal(1, portal(3, (12.0, 0.0, 0.0)));
        vw.add_entity(1, (0.0, 0.0, 0.0), Some(1));
        // Straight-line distance is 8.14 m — inside the radius — but the
        // noise travels by portal: 8 + 1.5.
        vw.add_entity(2, (8.0, 0.0, 1.5), Some(2));
        // And this one is only 1 m off the portal line but 2.83 m from
        // the portal itself: past the remaining reach.
        vw.add_entity(3, (6.0, 0.0, 2.0), Some(2));

        assert!(vw.make_noise(1, 1.0, 3, 0, true));
        assert_eq!(
            vw.callbacks,
            vec![HeardNoise {
                hearer: 2,
                who: 1,
                prop_range: 10.0,
                distance: 9.5,
                event: 3,
                info: 0
            }]
        );
    }

    #[test]
    fn a_noise_hops_two_portals_with_shrinking_reach() {
        let mut vw = NoiseWorld::default();
        for c in 1..=3 {
            vw.add_chunk(c, false);
        }
        vw.add_portal(1, portal(2, (4.0, 0.0, 0.0)));
        vw.add_portal(2, portal(3, (7.0, 0.0, 0.0)));
        vw.add_entity(1, (0.0, 0.0, 0.0), Some(1));
        vw.add_entity(2, (6.0, 0.0, 0.0), Some(2));
        // 3.1 m past the second portal: the 3 m remainder cannot reach it.
        vw.add_entity(3, (10.1, 0.0, 0.0), Some(3));
        vw.add_entity(4, (9.0, 0.0, 0.0), Some(3));

        assert!(vw.make_noise(1, 1.0, 0, 0, true));
        let heard: Vec<f32> = vw.callbacks.iter().map(|c| c.distance).collect();
        assert_eq!(heard, vec![6.0, 9.0]);
    }

    #[test]
    fn a_chunk_is_heard_once_no_matter_how_many_portals_lead_to_it() {
        let mut vw = NoiseWorld::default();
        for c in 1..=4 {
            vw.add_chunk(c, false);
        }
        // Two routes into chunk 3: through chunk 2 along +x, and round
        // through chunk 4 along +z.
        vw.add_portal(1, portal(2, (4.0, 0.0, 0.0)));
        vw.add_portal(2, portal(3, (7.0, 0.0, 0.0)));
        vw.add_portal(1, portal(4, (0.0, 0.0, 4.0)));
        vw.add_portal(4, portal(3, (0.0, 0.0, 7.0)));
        vw.add_entity(1, (0.0, 0.0, 0.0), Some(1));
        // Within reach of the (7,0,0) entry, and of the (0,0,7) one too —
        // either route alone would deliver.
        vw.add_entity(2, (6.5, 0.0, 0.5), Some(3));

        assert!(vw.make_noise(1, 1.0, 0, 0, true));
        assert_eq!(vw.callbacks.len(), 1);
        let via_x = 7.0 + (0.5f32 * 0.5 + 0.5 * 0.5).sqrt();
        assert_eq!(vw.callbacks[0].distance, via_x);
    }

    #[test]
    fn a_blocked_portal_stops_the_noise() {
        let mut vw = NoiseWorld::default();
        vw.add_chunk(1, false);
        vw.add_chunk(2, false);
        vw.add_portal(
            1,
            Portal {
                to_chunk: 2,
                centre: (2.0, 0.0, 0.0),
                permissive: false,
            },
        );
        // A portal pointing at a chunk that does not exist here stands in
        // for `!portal.hasChunk()`.
        vw.add_portal(1, portal(9, (2.0, 0.0, 0.0)));
        vw.add_entity(1, (0.0, 0.0, 0.0), Some(1));
        vw.add_entity(2, (3.0, 0.0, 0.0), Some(2));

        assert!(vw.make_noise(1, 1.0, 0, 0, true));
        assert!(vw.callbacks.is_empty());
    }

    #[test]
    fn make_noise_rejects_a_ghost_and_serves_a_chunkless_real() {
        let mut vw = NoiseWorld::default();
        vw.add_entity(1, (0.0, 0.0, 0.0), None);
        vw.add_entity(2, (1.0, 0.0, 0.0), None);

        // A ghost may not call it — the reference raises TypeError.
        assert!(!vw.make_noise(1, 1.0, 0, 0, false));
        assert!(vw.callbacks.is_empty());

        // A real with no chunk (`pChunk_ == NULL`) is fine but silent.
        assert!(vw.make_noise(1, 1.0, 0, 0, true));
        assert!(vw.callbacks.is_empty());

        // An id this cell has never heard of is silent too.
        assert!(vw.make_noise(9, 1.0, 0, 0, true));

        // The indoor walk guards a source without a chunk, and a
        // non-zero mark means the walk is a re-entry: no new stamp is
        // minted, the caller's is kept.
        vw.add_chunk(1, false);
        vw.add_entity(1, (0.0, 0.0, 0.0), Some(1));
        vw.propagate_noise(9, 10.0, (0.0, 0.0, 0.0), 10.0, 0, 0, 0);
        assert!(vw.callbacks.is_empty());
        vw.add_entity(3, (1.0, 0.0, 0.0), Some(1));
        vw.propagate_noise(1, 10.0, (0.0, 0.0, 0.0), 10.0, 0, 0, 7);
        assert_eq!(
            vw.callbacks,
            vec![HeardNoise {
                hearer: 3,
                who: 1,
                prop_range: 10.0,
                distance: 1.0,
                event: 0,
                info: 0
            }]
        );
    }

    #[test]
    fn movement_noise_waits_for_speed_and_spread() {
        let mut config = NoiseConfig::default();
        config.post_init();
        let mut noise = MovementNoise::default();

        // Default thresholds are tuned to never trip: a brisk walk for a
        // long while stays quiet (the check runs on even ticks).
        for now in (2..=40).step_by(2) {
            assert!(noise.new_position(now, (1.0, 0.0, 0.0), &config).is_none());
        }

        // Lower the bar so 3 m/s counts as loud. Tick 41 has dur 1 — the
        // gate skips it; tick 42 checks, flags, and the seed-time spread
        // (still 0) lets the shout leave at once.
        config = NoiseConfig {
            horizontal_speed: 2.0,
            ..NoiseConfig::default()
        };
        config.post_init();
        assert!(noise.new_position(41, (3.0, 0.0, 0.0), &config).is_none());
        assert_eq!(
            noise.new_position(42, (3.0, 0.0, 0.0), &config),
            Some((0.0, 0))
        );

        // Re-flagged at 44, but the ten-tick spread from 42 holds it.
        for now in 43..=52 {
            assert!(noise.new_position(now, (3.0, 0.0, 0.0), &config).is_none());
        }
        // Eleven ticks since the 42 shout: it leaves at 53.
        assert_eq!(
            noise.new_position(53, (3.0, 0.0, 0.0), &config),
            Some((0.0, 0))
        );
    }

    #[test]
    fn a_fall_trips_the_vertical_threshold() {
        let mut config = NoiseConfig {
            vertical_speed: 5.0,
            ..NoiseConfig::default()
        };
        config.post_init();
        // The witness seeds its stamps with the app time it was born at:
        // here, tick 30.
        let mut noise = MovementNoise::new(30);

        // Falling exactly at the threshold is quiet; a shade faster
        // flags the noise — silently, for now, because the ten-tick
        // spread is measured from the seed time. (Checks land on even
        // ticks: 32, 34, 36.)
        assert!(noise.new_position(32, (0.0, -5.0, 0.0), &config).is_none());
        assert!(noise.new_position(34, (0.0, -5.0, 0.0), &config).is_none());
        assert!(noise.new_position(36, (0.0, -5.1, 0.0), &config).is_none());
        // Tick 42 is the first with more than ten ticks since the seed.
        assert_eq!(
            noise.new_position(42, (0.0, 0.0, 0.0), &config),
            Some((0.0, 0))
        );
    }

    #[test]
    fn the_velocity_gate_checks_at_most_every_other_tick() {
        let mut config = NoiseConfig {
            horizontal_speed: 2.0,
            ..NoiseConfig::default()
        };
        config.post_init();
        let mut noise = MovementNoise::default();

        // Tick 1 has dur 1: the gate skips the check entirely. Tick 2
        // checks and silently flags the noise; the spread keeps it quiet
        // until eleven ticks past the seed.
        assert!(noise.new_position(1, (3.0, 0.0, 0.0), &config).is_none());
        for now in 2..=10 {
            assert!(noise.new_position(now, (3.0, 0.0, 0.0), &config).is_none());
        }
        assert_eq!(
            noise.new_position(11, (0.0, 0.0, 0.0), &config),
            Some((0.0, 0))
        );
    }

    #[test]
    fn stealth_scales_the_propagation_range() {
        let mut vw = NoiseWorld::default();
        vw.add_chunk(1, true);
        vw.add_entity(1, (0.0, 0.0, 0.0), Some(1));
        vw.add_entity(2, (4.0, 0.0, 0.0), Some(1));
        vw.add_entity(3, (6.0, 0.0, 0.0), Some(1));

        let mut config = NoiseConfig {
            horizontal_speed: 2.0,
            ..NoiseConfig::default()
        };
        config.post_init();
        let mut noise = MovementNoise {
            stealth_factor: 0.5,
            ..MovementNoise::default()
        };

        // Walk fast; the shout leaves on the eleventh tick.
        let mut shout = None;
        for now in 1..=11u32 {
            shout = noise.new_position(now, (3.0, 0.0, 0.0), &config);
        }
        assert_eq!(shout, Some((0.5, 0)));

        // Stealth halves the level, so the range is 5 m: entity 3 at 6 m
        // misses it.
        let (level, event) = shout.unwrap();
        assert!(vw.make_noise(1, level, event, 0, true));
        assert_eq!(vw.callbacks.len(), 1);
        assert_eq!(vw.callbacks[0].hearer, 2);
        assert_eq!(vw.callbacks[0].prop_range, 5.0);
        assert_eq!(vw.callbacks[0].event, 0);
    }
}
