//! The movement controller face (`server/tools/bots/movement_controller.*`
//! and `beeline_controller.*`): the interface every bot walks through and
//! the factory registry that builds controllers from the config strings.
//!
//! Reference quirks preserved:
//! - controller type "None" builds *no* controller and raises no error --
//!   the bot then circles on its fallback path
//!   (`client_app.cpp`, `ClientApp::addMove`'s else arm);
//! - the `bots-extensions/*.so` native controller ABI
//!   (`main_app.cpp:761-787`) is not ported: this tree has no
//!   dynamic-library controller interface, so a `.so:`-prefixed type
//!   reaches the factory lookup unstripped and fails there;
//! - the draws run through a seeded [`Rng`], not the C library's
//!   `rand()`/`bw_random()` streams -- the trajectory acceptance pins the
//!   recurrences, not the stream;
//! - `atof` is the C prefix parse (`c_atof`), so junk tails parse as far
//!   as they go and fall back to zero.

use std::collections::BTreeMap;
use std::rc::Rc;

use foundation::math::Vector3;
use resources::bwresource::BwResource;

/// `RAND_MAX`'s port face: the unit draw divides by `u32::MAX`, the
/// denominator `unitRand` uses (`mathdef.hpp:118-121`,
/// `(float)(uint32)-1`).
pub const RAND_MAX_U32: u32 = u32::MAX;

/// The process RNG the reference draws through `rand()` and
/// `bw_random()`. One stream drives node picks, destinations and stay
/// times.
pub trait Rng {
    /// A uniform draw (`rand()`).
    fn next_u32(&mut self) -> u32;

    /// `unitRand()` (`mathdef.hpp:118-121`): the draw scaled into
    /// [0, 1).
    fn unit_rand(&mut self) -> f32 {
        self.next_u32() as f32 / RAND_MAX_U32 as f32
    }

    /// `randInRange` (`mathdef.hpp:127-130`): `min + unitRand*(max-min)`.
    fn rand_in_range(&mut self, min: f32, max: f32) -> f32 {
        min + self.unit_rand() * (max - min)
    }

    /// `ZigzagPatrol::frand` (`zigzag_patrol_graph.cpp:18`):
    /// `f * rand()/RAND_MAX`.
    fn frand(&mut self, f: f32) -> f32 {
        f * (self.next_u32() as f32 / RAND_MAX_U32 as f32)
    }
}

/// `Direction3D` (`lib/network/basictypes.hpp`): the bots code only
/// touches its yaw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Direction3D {
    pub yaw: f32,
}

impl Direction3D {
    /// The zero direction.
    pub fn new(yaw: f32) -> Self {
        Self { yaw }
    }
}

/// `almostEqual` (`cstdmf/stdmf.hpp`, epsilon 0.0004).
pub(crate) fn almost_equal(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.0004
}

/// C `atof`: the longest `strtod` prefix, 0.0 when nothing parses. An
/// incomplete exponent (`"1e"`) backtracks to the mantissa like
/// `strtod` does.
pub(crate) fn c_atof(s: &str) -> f32 {
    let trimmed = s.trim_start();
    let end = trimmed
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E')))
        .unwrap_or(trimmed.len());
    let mut prefix = &trimmed[..end];
    if prefix.ends_with('e') || prefix.ends_with('E') {
        prefix = &prefix[..prefix.len() - 1];
    }
    prefix.parse::<f64>().unwrap_or(0.0) as f32
}

/// The process RNG stream (`rand()`/`bw_random()`): one stream drives
/// every draw in a Bots process, so the port threads it through the
/// call sites instead of a global.
pub struct Lcg(u64);

impl Lcg {
    /// Seeded like `srand( timestamp() )` (`main_app.cpp:141`).
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
}

impl Rng for Lcg {
    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
}

/// `MovementController` (`movement_controller.hpp:13-20`): one step of
/// the walk. `speed` is in-out (a node can re-draw it), `pos` is
/// advanced in place. The process RNG rides along because the
/// reference's draws hit the global `rand()`.
pub trait MovementController {
    fn next_step(
        &mut self,
        speed: &mut f32,
        d_time: f32,
        pos: &mut Vector3,
        dir: &mut Direction3D,
        rng: &mut dyn Rng,
    ) -> bool;
}

/// The Python exception family `PyErr_Format` would raise.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MovementErrorKind {
    /// `PyExc_ValueError`.
    ValueError,
    /// `PyExc_IOError`.
    IoError,
    /// `PyExc_TypeError`.
    TypeError,
}

/// A controller construction failure, carrying the exact
/// `PyErr_Format` text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MovementError {
    pub kind: MovementErrorKind,
    pub message: String,
}

impl MovementError {
    pub(crate) fn new(kind: MovementErrorKind, message: String) -> Self {
        Self { kind, message }
    }
}

/// `MovementFactory` (`movement_controller.hpp:27-33`). The reference
/// factories are process statics calling `BWResource::instance()`
/// inside `create`; the port threads the resource instance through.
pub trait MovementFactory {
    fn create(
        &mut self,
        resource: &BwResource,
        data: &str,
        speed: &mut f32,
        position: &mut Vector3,
        rng: &mut dyn Rng,
    ) -> Result<Option<Box<dyn MovementController>>, MovementError>;
}

/// `MainApp::s_pMovementFactories` (`main_app.cpp:731-735,810-818`):
/// the name-keyed factory table. The reference fills it from factory
/// constructors at static init; the port pre-populates it in the
/// `MainApp` constructor (one Bots process per `MainApp`).
pub struct MovementFactories {
    map: BTreeMap<String, Rc<std::cell::RefCell<dyn MovementFactory>>>,
}

impl Default for MovementFactories {
    fn default() -> Self {
        Self::new()
    }
}

impl MovementFactories {
    /// The table with the three built-in factories registered
    /// (`Beeline`, `Patrol`, `ZigzagPatrol` -- the statics
    /// `s_beelineFactory`, `s_patrolFactory`, `s_patrolFactory`).
    pub fn new() -> Self {
        let mut map: BTreeMap<String, Rc<std::cell::RefCell<dyn MovementFactory>>> =
            BTreeMap::new();
        map.insert(
            "Beeline".to_string(),
            Rc::new(std::cell::RefCell::new(BeelineFactory)),
        );
        map.insert(
            "Patrol".to_string(),
            Rc::new(std::cell::RefCell::new(
                crate::patrol_graph::PatrolFactory::new(),
            )),
        );
        map.insert(
            "ZigzagPatrol".to_string(),
            Rc::new(std::cell::RefCell::new(
                crate::zigzag_patrol_graph::ZigzagPatrolFactory::new(),
            )),
        );
        Self { map }
    }

    /// `MainApp::addFactory` (`main_app.cpp:810-818`): registers (or
    /// replaces) a factory under `name`.
    pub fn add_factory(
        &mut self,
        name: &str,
        factory: Rc<std::cell::RefCell<dyn MovementFactory>>,
    ) {
        self.map.insert(name.to_string(), factory);
    }

    /// The lookup `createMovementController` performs
    /// (`main_app.cpp:789-799`).
    pub fn get(&mut self, name: &str) -> Option<Rc<std::cell::RefCell<dyn MovementFactory>>> {
        self.map.get(name).cloned()
    }
}

/// `BeelineController` (`beeline_controller.hpp:19-22`): walk straight
/// at the destination.
pub struct BeelineController {
    destination_pos: Vector3,
}

impl BeelineController {
    /// The constructor.
    pub fn new(destination_pos: Vector3) -> Self {
        Self { destination_pos }
    }
}

impl MovementController for BeelineController {
    /// `BeelineController::nextStep` (`beeline_controller.cpp:26-45`):
    /// move closer while not there yet, yaw toward the destination.
    /// Never finishes -- always true. The beeline draws nothing, so
    /// the RNG rides unused.
    fn next_step(
        &mut self,
        speed: &mut f32,
        d_time: f32,
        pos: &mut Vector3,
        dir: &mut Direction3D,
        _rng: &mut dyn Rng,
    ) -> bool {
        // Distance travelled per tick
        let distance = *speed * d_time;

        // Vector to dest
        let dest_vec = self.destination_pos - *pos;

        // Move closer if we aren't there yet
        if dest_vec.length() > distance {
            *pos = *pos + dest_vec * (distance / dest_vec.length());
            dir.yaw = dest_vec.yaw();
        }

        true
    }
}

/// `BeelineControllerFactory` (`beeline_controller.cpp:57-79`): the
/// "Beeline" factory. The data is `x,y,z`; the parse takes the first
/// and last comma, so a missing middle still lands somewhere.
pub struct BeelineFactory;

impl MovementFactory for BeelineFactory {
    fn create(
        &mut self,
        _resource: &BwResource,
        data: &str,
        _speed: &mut f32,
        _position: &mut Vector3,
        _rng: &mut dyn Rng,
    ) -> Result<Option<Box<dyn MovementController>>, MovementError> {
        // The C++ parse takes find_first_of/find_last_of and rides
        // substr semantics: npos + 1 wraps to 0, a "negative" length
        // (`com2 == com1`, one comma) saturates to the rest of the
        // string, and no comma reads the whole string for every
        // component.
        let com1 = data.find(',');
        let com2 = data.rfind(',');

        let x = c_atof(match com1 {
            Some(i) => &data[..i],
            None => data, // substr(0, npos)
        });

        let y_start = com1.map_or(0, |i| i + 1); // npos + 1 wraps to 0
        let y_len = match (com1, com2) {
            (Some(c1), Some(c2)) => c2.wrapping_sub(c1).wrapping_sub(1),
            _ => usize::MAX, // com2 - com1 - 1 with npos
        };
        let y_end = y_start.saturating_add(y_len).min(data.len());
        let y = c_atof(&data[y_start..y_end]);

        let z = c_atof(match com2 {
            Some(i) => &data[i + 1..],
            None => data, // substr(npos + 1) = substr(0)
        });

        Ok(Some(Box::new(BeelineController::new(Vector3::new(
            x, y, z,
        )))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A draw stream that hands out fixed values in rotation.
    struct FixedRng {
        values: Vec<u32>,
        at: usize,
    }

    impl FixedRng {
        fn new(values: &[u32]) -> Self {
            Self {
                values: values.to_vec(),
                at: 0,
            }
        }
    }

    impl Rng for FixedRng {
        fn next_u32(&mut self) -> u32 {
            let v = self.values[self.at % self.values.len()];
            self.at += 1;
            v
        }
    }

    /// Acceptance: the beeline recurrence (`beeline_controller.cpp:26-45`)
    /// -- each tick advances the exact fraction `distance/len` toward the
    /// destination and yaws at it, and arrival freezes the step.
    #[test]
    fn the_beeline_recurrence_walks_the_exact_fraction() {
        let mut controller = BeelineController::new(Vector3::new(10.0, 0.0, 0.0));
        let mut speed = 5.0f32;
        let mut pos = Vector3::new(0.0, 0.0, 0.0);
        let mut dir = Direction3D::new(0.0);
        let mut rng = FixedRng::new(&[0]);

        // Tick 1: distance 2.5 over length 10 -> pos (2.5, 0, 0), yaw atan2(10, 0) = pi/2.
        assert!(controller.next_step(&mut speed, 0.5, &mut pos, &mut dir, &mut rng));
        assert_eq!(pos.x, 2.5);
        assert_eq!(pos.y, 0.0);
        assert_eq!(pos.z, 0.0);
        assert_eq!(dir.yaw, 10.0f32.atan2(0.0));

        // Tick 2: length 7.5 -> pos (5.0, 0, 0).
        assert!(controller.next_step(&mut speed, 0.5, &mut pos, &mut dir, &mut rng));
        assert_eq!(pos.x, 5.0);

        // Arrival: length 5, distance 5 is not >, so the step freezes.
        assert!(controller.next_step(&mut speed, 1.0, &mut pos, &mut dir, &mut rng));
        assert_eq!(pos.x, 5.0);
    }

    /// Acceptance: the factory parses `x,y,z` through the C `atof`
    /// prefix rules (`beeline_controller.cpp:71-75`) -- junk tails fall
    /// to zero, missing commas degenerate like `npos` slices.
    #[test]
    fn the_beeline_factory_parses_like_c_atof() {
        let mut factory = BeelineFactory;
        let resource = BwResource::new();
        let mut speed = 1.0f32;
        let mut position = Vector3::ZERO;
        let mut rng = FixedRng::new(&[0]);

        let mut controller = factory
            .create(&resource, "1.5,2,3", &mut speed, &mut position, &mut rng)
            .unwrap()
            .unwrap();
        let mut pos = Vector3::new(0.0, 0.0, 0.0);
        let mut dir = Direction3D::new(0.0);
        controller.next_step(&mut speed, 0.1, &mut pos, &mut dir, &mut rng);
        // One tick of distance 0.1 over length sqrt(1.5^2+2^2+3^2)=3.674
        // lands at the destination scaled fraction; the walk is toward
        // (1.5, 2, 3).
        assert!(pos.x > 0.0 && pos.y > 0.0 && pos.z > 0.0);

        // Junk tails: "1,,3" parses y as 0 -- visible through the exact
        // fraction walked (the y component stays zero).
        let mut controller = factory
            .create(&resource, "1,,3", &mut speed, &mut position, &mut rng)
            .unwrap()
            .unwrap();
        let mut pos = Vector3::new(0.0, 0.0, 0.0);
        let mut dir = Direction3D::new(0.0);
        let dest = Vector3::new(1.0, 0.0, 3.0);
        controller.next_step(&mut speed, 1.0, &mut pos, &mut dir, &mut rng);
        assert_eq!(pos, dest * (1.0 / dest.length()));

        // Degenerate comma layouts ride the C++ substr semantics: one
        // comma makes y the tail (a negative length saturates to the
        // rest) and z the tail again; no comma reads the whole string
        // for every component.
        for (data, expected) in [
            ("5,7", Vector3::new(5.0, 7.0, 7.0)),
            ("5", Vector3::new(5.0, 5.0, 5.0)),
        ] {
            let mut controller = factory
                .create(&resource, data, &mut speed, &mut position, &mut rng)
                .unwrap()
                .unwrap();
            let mut pos = Vector3::new(0.0, 0.0, 0.0);
            let mut dir = Direction3D::new(0.0);
            controller.next_step(&mut speed, 1.0, &mut pos, &mut dir, &mut rng);
            assert_eq!(pos, expected * (1.0 / expected.length()), "{data}");
        }
    }

    /// Acceptance: the factory table -- `Default` carries the three
    /// built-ins (`main_app.cpp:731-735`) and `addFactory` registers
    /// a name (`main_app.cpp:810-818`).
    #[test]
    fn the_factory_table_registers_and_resolves() {
        let mut table = MovementFactories::default();
        for name in ["Beeline", "Patrol", "ZigzagPatrol"] {
            assert!(table.get(name).is_some(), "{name} is built in");
        }
        assert!(table.get("Ghost").is_none());

        struct GhostFactory;
        impl MovementFactory for GhostFactory {
            fn create(
                &mut self,
                _resource: &BwResource,
                _data: &str,
                _speed: &mut f32,
                _position: &mut Vector3,
                _rng: &mut dyn Rng,
            ) -> Result<Option<Box<dyn MovementController>>, MovementError> {
                Ok(None)
            }
        }
        table.add_factory("Ghost", Rc::new(std::cell::RefCell::new(GhostFactory)));
        assert!(table.get("Ghost").is_some());

        // The registered factory answers through the trait face.
        let resource = BwResource::new();
        let mut speed = 6.0f32;
        let mut pos = Vector3::ZERO;
        let mut rng = FixedRng::new(&[0]);
        let made = table
            .get("Ghost")
            .unwrap()
            .borrow_mut()
            .create(&resource, "", &mut speed, &mut pos, &mut rng)
            .unwrap();
        assert!(made.is_none(), "the ghost builds no controller");

        // The draw stream interface itself.
        assert_eq!(FixedRng::new(&[7]).next_u32(), 7);
    }

    /// Acceptance: `c_atof` is the C prefix parse -- leading spaces,
    /// junk tails, incomplete exponents.
    #[test]
    fn c_atof_parses_the_c_prefix_rules() {
        assert_eq!(c_atof("2.5,7"), 2.5);
        assert_eq!(c_atof("  3.25"), 3.25);
        assert_eq!(c_atof("1e"), 1.0);
        assert_eq!(c_atof("abc"), 0.0);
        assert_eq!(c_atof("-4.5x"), -4.5);
        assert_eq!(c_atof(""), 0.0);
    }
}
