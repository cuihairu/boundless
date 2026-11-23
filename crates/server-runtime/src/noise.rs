//! BW-08-06-02: noise perception.
//!
//! An entity can make a noise, and every entity strictly closer than the
//! propagation range hears it: the range is the noise level scaled by the
//! configured standard range, and the distance is measured between the two
//! positions. A witnessed entity also makes noise by moving: running faster
//! than `horizontalSpeed` or falling faster than `verticalSpeed` marks the
//! witness as noisy, and while it stays noisy a `makeNoise(stealthFactor, 0)`
//! propagates on a ten-tick cadence.
//!
//! The acceptance, "不同半径与距离的触发对照", is that different radii
//! (levels × standard range) and distances produce a comparable heard set:
//! each listener hears exactly when its distance is strictly below the
//! range, with the range and distance reported on the callback.
//!
//! Reference:
//! - `entity.cpp:7329-7351` `Entity::makeNoise` — the propagation range is
//!   `noiseLevel * NoiseConfig::standardRange()` (`:7334`); outdoors the
//!   noise goes through `outdoorPropagateNoise`, indoors through the chunk
//!   portal walk.
//! - `entity.cpp:7271-7309` `outdoorPropagateNoise` — every entity in range
//!   is visited, and `dist < range` is the strict trigger (`:7304-7306`).
//! - `entity.cpp:7364-7384` `Entity::heardNoise` — the `onNoise` callback
//!   carries `(who, propRange, distance, event, info)` (`:7381`).
//! - `witness.cpp:2372-2406` `Witness::newPosition` — the movement noise:
//!   a speed check at most once every two ticks (`:2374-2377`), falling
//!   below `-verticalSpeed` or running above `horizontalSpeed` sets
//!   `noiseMade_` (`:2379-2389`), and `sinceNoise > 10` propagates
//!   `makeNoise(stealthFactor_, 0)` and resets (`:2394-2402`).
//! - `witness.cpp:2786-2800` — the `stealthFactor` attribute doc: movement
//!   above the thresholds informs other entities every 10 ticks with the
//!   stealth factor as the noise level and event 0.
//! - `noise_config.cpp:32-34` — the option defaults: `standardRange = 10`,
//!   `verticalSpeed` and `horizontalSpeed` a million (movement noise off);
//!   `:44` — `horizontalSpeedSqr` derived in `postInit`.

use crate::movement::Vec3;
use crate::range_list::EntityId;

/// The noise options (`NoiseConfig`, `noise_config.hpp:21`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseConfig {
    /// The standard range a level of 1.0 propagates to
    /// (`standardRange`, `noise_config.cpp:32`).
    pub standard_range: f64,
    /// Falling faster than this makes noise (`verticalSpeed`,
    /// `noise_config.cpp:33`).
    pub vertical_speed: f64,
    /// Running faster than this makes noise, squared
    /// (`horizontalSpeedSqr`, `noise_config.cpp:44`).
    pub horizontal_speed_sqr: f64,
}

impl NoiseConfig {
    /// The options from their raw forms; the horizontal speed is squared
    /// in `postInit` (`noise_config.cpp:44`).
    pub fn new(standard_range: f64, vertical_speed: f64, horizontal_speed: f64) -> Self {
        NoiseConfig {
            standard_range,
            vertical_speed,
            horizontal_speed_sqr: horizontal_speed * horizontal_speed,
        }
    }
}

impl Default for NoiseConfig {
    /// `noise_config.cpp:32-34`: `standardRange = 10`, the movement speeds
    /// default to a million — movement noise is off until configured.
    fn default() -> Self {
        Self::new(10.0, 1_000_000.0, 1_000_000.0)
    }
}

/// The `onNoise` callback arguments, addressed to the hearer
/// (`Entity::heardNoise`, `entity.cpp:7381` — the callback runs on the
/// hearing entity, with the maker passed as `who`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseEvent {
    /// The entity that hears the noise.
    pub hearer: EntityId,
    /// The entity that made the noise.
    pub who: EntityId,
    /// How far the noise propagates.
    pub prop_range: f64,
    /// How far away the noise was made.
    pub distance: f64,
    /// The event number.
    pub event: i32,
    /// The info field.
    pub info: i32,
}

/// The outdoor half of `Entity::makeNoise` (`entity.cpp:7329-7309`): the
/// propagation range is `noiseLevel * standardRange` (`:7334`), and every
/// listener strictly closer than it hears the noise with the callback
/// arguments of `heardNoise` (`:7304-7306`, `:7381`). The heard set is
/// sorted by hearer id.
pub fn propagate_noise(
    config: &NoiseConfig,
    source: EntityId,
    source_pos: Vec3,
    noise_level: f64,
    event: i32,
    info: i32,
    listeners: &[(EntityId, Vec3)],
) -> Vec<NoiseEvent> {
    let prop_range = noise_level * config.standard_range;
    let mut heard: Vec<NoiseEvent> = listeners
        .iter()
        .map(|&(id, pos)| (id, pos.minus(source_pos).length()))
        .filter(|&(_, distance)| distance < prop_range)
        .map(|(hearer, distance)| NoiseEvent {
            hearer,
            who: source,
            prop_range,
            distance,
            event,
            info,
        })
        .collect();
    heard.sort_by_key(|heard| heard.hearer);
    heard
}

/// The movement-driven noise of a witnessed entity
/// (`Witness::newPosition`, `witness.cpp:2372-2406`).
pub struct MovementNoise {
    stealth_factor: f64,
    config: NoiseConfig,
    noise_check_time: u32,
    noise_propagated_time: u32,
    noise_made: bool,
}

impl MovementNoise {
    /// A witness with the given `stealthFactor` (`witness.cpp:2796-2799`)
    /// and the default config.
    pub fn new(stealth_factor: f64) -> Self {
        Self::new_with_config(stealth_factor, NoiseConfig::default())
    }

    /// A witness with an explicit `NoiseConfig`.
    pub fn new_with_config(stealth_factor: f64, config: NoiseConfig) -> Self {
        MovementNoise {
            stealth_factor,
            config,
            noise_check_time: 0,
            noise_propagated_time: 0,
            noise_made: false,
        }
    }

    /// The stealth factor (`stealthFactor`, `witness.cpp:2796`).
    pub fn stealth_factor(&self) -> f64 {
        self.stealth_factor
    }

    /// `Witness::newPosition` (`witness.cpp:2372`): at most one speed check
    /// per two ticks (`:2374-2377`) — falling faster than `verticalSpeed`
    /// (`:2379-2381`) or moving faster than `horizontalSpeed`
    /// (`:2383-2389`) sets `noiseMade_`. While made, more than ten ticks
    /// since the last propagation fire `makeNoise(stealthFactor, 0)` and
    /// reset the state (`:2394-2402`); the returned level is the stealth
    /// factor, the event is 0.
    pub fn new_position(&mut self, velocity: Vec3, now: u32) -> Option<f64> {
        let dur = now - self.noise_check_time;
        let since_noise = now - self.noise_propagated_time;
        if dur > 1 && !self.noise_made {
            let horizontal = Vec3::new(velocity.x, 0.0, velocity.z).length_squared();
            if velocity.y < -self.config.vertical_speed
                || horizontal > self.config.horizontal_speed_sqr
            {
                self.noise_made = true;
            }
            self.noise_check_time = now;
        }

        if since_noise > 10 && self.noise_made {
            self.noise_made = false;
            self.noise_propagated_time = now;
            return Some(self.stealth_factor);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: u32) -> EntityId {
        id
    }

    /// The acceptance: different radii and distances produce a comparable
    /// heard set (`entity.cpp:7304-7306`).
    #[test]
    fn the_radius_and_distance_comparison_matches() {
        let config = NoiseConfig::default();
        let listeners = [
            (e(1), Vec3::new(4.0, 0.0, 0.0)),
            (e(2), Vec3::new(9.0, 0.0, 0.0)),
            (e(3), Vec3::new(15.0, 0.0, 0.0)),
            (e(4), Vec3::new(25.0, 0.0, 0.0)),
        ];
        let heard = |level| {
            propagate_noise(&config, e(7), Vec3::ZERO, level, 0, 0, &listeners)
                .iter()
                .map(|event| event.hearer)
                .collect::<Vec<_>>()
        };

        // Level × standardRange 10: 5, 10, 20, 50.
        assert_eq!(heard(0.5), vec![e(1)]);
        assert_eq!(heard(1.0), vec![e(1), e(2)]);
        assert_eq!(heard(2.0), vec![e(1), e(2), e(3)]);
        assert_eq!(heard(5.0), vec![e(1), e(2), e(3), e(4)]);
    }

    /// The trigger is strict: a listener exactly at the range does not hear,
    /// one just inside does, and the callback carries the full payload
    /// (`entity.cpp:7304-7306`, `:7381`).
    #[test]
    fn a_listener_exactly_at_the_range_is_not_heard() {
        let config = NoiseConfig::new(20.0, 1e6, 1e6);
        let listeners = [
            (e(1), Vec3::new(0.0, 0.0, 30.0)),
            (e(2), Vec3::new(0.0, 0.0, 29.5)),
        ];

        // Level 1.5 × 20 = 30.
        let heard = propagate_noise(&config, e(5), Vec3::ZERO, 1.5, 42, 7, &listeners);
        assert_eq!(
            heard,
            vec![NoiseEvent {
                hearer: e(2),
                who: e(5),
                prop_range: 30.0,
                distance: 29.5,
                event: 42,
                info: 7,
            }]
        );
    }

    /// The distance is measured between the source and the listener, not
    /// from the origin (`entity.cpp:7303`).
    #[test]
    fn the_distance_is_measured_between_source_and_listener() {
        let config = NoiseConfig::default();
        let source = Vec3::new(10.0, 0.0, 10.0);

        // 6 away: outside the default range 10? No — inside. Use 4.5 and 6.
        let near = propagate_noise(
            &config,
            e(3),
            source,
            0.5,
            0,
            0,
            &[(e(1), Vec3::new(10.0, 0.0, 14.5))],
        );
        assert_eq!(near.len(), 1, "4.5 < 5 hears the noise");
        assert_eq!(near[0].distance, 4.5);

        let far = propagate_noise(
            &config,
            e(3),
            source,
            0.5,
            0,
            0,
            &[(e(1), Vec3::new(10.0, 0.0, 16.0))],
        );
        assert!(far.is_empty(), "6 > 5 does not");
    }

    /// Running above `horizontalSpeed` propagates `makeNoise(stealthFactor,
    /// 0)` on the ten-tick cadence (`witness.cpp:2394-2402`).
    #[test]
    fn fast_movement_propagates_every_ten_plus_ticks() {
        let config = NoiseConfig::new(10.0, 1e6, 5.0);
        let mut witness = MovementNoise::new_with_config(0.75, config);
        let running = Vec3::new(10.0, 0.0, 0.0);

        // The speed is only checked from the third tick (`dur > 1`),
        // and the propagation needs more than ten ticks since the last.
        for now in 0..11 {
            assert_eq!(witness.new_position(running, now), None, "tick {now}");
        }
        assert_eq!(witness.new_position(running, 11), Some(0.75));

        // Made again on the next checked tick, propagated ten-plus later.
        for now in 12..22 {
            assert_eq!(witness.new_position(running, now), None, "tick {now}");
        }
        assert_eq!(witness.new_position(running, 22), Some(0.75));
    }

    /// Sub-threshold movement — and any movement under the default speeds —
    /// stays silent (`noise_config.cpp:33-34`).
    #[test]
    fn quiet_movement_and_default_speeds_stay_silent() {
        let config = NoiseConfig::new(10.0, 1e6, 5.0);
        let mut witness = MovementNoise::new_with_config(1.0, config);
        let walking = Vec3::new(3.0, 0.0, 4.0);

        // Speed exactly 5: not above the threshold.
        for now in 0..100 {
            assert_eq!(witness.new_position(walking, now), None);
        }

        // The defaults make even an absurd sprint silent.
        let mut witness = MovementNoise::new(0.5);
        let sprint = Vec3::new(1e6, 0.0, 0.0);
        for now in 0..100 {
            assert_eq!(witness.new_position(sprint, now), None);
        }
    }

    /// Falling past `verticalSpeed` makes noise; rising at the same speed
    /// does not (`witness.cpp:2379-2381`).
    #[test]
    fn falling_past_the_vertical_speed_makes_noise() {
        let config = NoiseConfig::new(10.0, 100.0, 1e6);

        let mut witness = MovementNoise::new_with_config(2.0, config);
        let falling = Vec3::new(0.0, -150.0, 0.0);
        let mut fired = false;
        for now in 0..30 {
            fired |= witness.new_position(falling, now).is_some();
        }
        assert!(fired, "a fast fall propagates within 30 ticks");

        let mut witness = MovementNoise::new_with_config(2.0, config);
        let rising = Vec3::new(0.0, 150.0, 0.0);
        for now in 0..30 {
            assert_eq!(witness.new_position(rising, now), None);
        }
    }

    /// The witness reports its stealth factor.
    #[test]
    fn the_witness_reports_its_stealth_factor() {
        assert_eq!(MovementNoise::new(0.25).stealth_factor(), 0.25);
    }
}
