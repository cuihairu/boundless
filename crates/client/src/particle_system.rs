//! Particle system core: packed particles, the fixed-frame-rate update
//! loop, and the source/jitter actions.
//!
//! Verbatim port of `lib/particle/particle.ipp` (the packed particle
//! accessors), `lib/particle/particles.ipp` (the two container layouts),
//! `lib/particle/particle_system.cpp` (`update`/`predictPosition`), and the
//! `SourcePSA`/`JitterPSA` actions from `lib/particle/actions/`. Every
//! random draw goes through `bw_random()` so a fixed seed replays the same
//! particle sequence.
//!
//! Registered deviations: the GPU renderer update is skipped (as are the
//! particle distance sort and the system bounding box, both renderer
//! concerns); the `ParticleSystemManager` singleton gate and wind velocity
//! are supplied as arguments instead of globals; `SourcePSA`'s per-frame
//! CPU budget (`allowedTime`) runs against a synthetic clock that only
//! advances by a configurable per-particle cost (zero by default: the
//! budget never trips for a fast host), and the load-time sleep draw is
//! replayed through
//! [`SourcePSA::arm_sleep_cycle`]; the grounded path takes a
//! host-supplied [`GroundProbe`] (the reference lazily creates a default
//! chunk-space collider, which does not exist in this port's non-chunked
//! host) and, with no probe, leaves the generated position untouched
//! exactly as the reference does. A probe that misses (returns
//! [`NO_GROUND_COLLISION`]) skips the whole per-particle draw block.

use crate::bw_random::BWRandom;
use crate::vector_generator::VectorGenerator;
use foundation::math::matrix::Matrix;
use foundation::math::vector::{Vector2, Vector3, Vector4, clamp};

/// `MATH_2PI`.
const MATH_2PI: f32 = std::f32::consts::PI * 2.0;

/// `RompCollider::NO_GROUND_COLLISION` (`romp_collider.cpp:6`).
pub const NO_GROUND_COLLISION: f32 = -1_000_000.0;

/// `ParticleSystem::MAX_CAPACITY`.
pub const MAX_CAPACITY: usize = 65536;

//constants for packing velocity into 16 bits
const VELOCITY_MIN_DATA: f32 = -32768.0;
const VELOCITY_MAX_DATA: f32 = 32767.0;

//constants for packing and unpacking size data into 16 bits
const SIZE_MAX_DATA: f32 = 65535.0;
const SIZE_MAX_VALUE: f32 = 100.0;
const PACK_SIZE: f32 = SIZE_MAX_DATA / SIZE_MAX_VALUE;
const UNPACK_SIZE: f32 = SIZE_MAX_VALUE / SIZE_MAX_DATA;
const PACK_MESH_SPIN: f32 = SIZE_MAX_DATA / SIZE_MAX_VALUE;
const UNPACK_MESH_SPIN: f32 = SIZE_MAX_VALUE / SIZE_MAX_DATA;

//constants for packing and unpacking age data into 16 bits
//split up the age value into increments of 1/120 seconds.
const AGE_MIN_INCREMENT: f32 = 1.0 / 120.0;
const AGE_MAX_DATA: f32 = 65535.0;
const AGE_MAX_DATA_UINT: u16 = 0xffff;
const AGE_MAX_VALUE: f32 = AGE_MAX_DATA * AGE_MIN_INCREMENT;
const PACK_AGE: f32 = AGE_MAX_DATA / AGE_MAX_VALUE;
const UNPACK_AGE: f32 = AGE_MAX_VALUE / AGE_MAX_DATA;

//constants for packing and unpacking pitch/yaw rotational data
const ROT_MAX_DATA: f32 = 256.0;
const ROT_MAX_VALUE: f32 = MATH_2PI;
const PACK_ROT: f32 = ROT_MAX_DATA / ROT_MAX_VALUE;
const UNPACK_ROT: f32 = ROT_MAX_VALUE / ROT_MAX_DATA;
const ABS_ROT: f32 = MATH_2PI * 10.0;
const ROUND_ROT: f32 = MATH_2PI / ROT_MAX_DATA;
const SPIN_DIR_MAX_DATA: f32 = 255.0;
const SPIN_SPEED_MAX_DATA: f32 = 255.0;
const SPIN_ANGLE_MAX_DATA: f32 = 65535.0;

/// `Colour::getUint32FromNormalised` for a Vector4 (`colour.hpp:138`):
/// each channel is scaled by 255.0 as a double, clamped to 0..255, and
/// packed ARGB.
pub fn colour_u32_from_normalised(colour: &Vector4) -> u32 {
    let mut r = (colour.x as f64 * 255.0) as i32;
    let mut g = (colour.y as f64 * 255.0) as i32;
    let mut b = (colour.z as f64 * 255.0) as i32;
    let mut a = (colour.w as f64 * 255.0) as i32;

    r = r.clamp(0, 255);
    g = g.clamp(0, 255);
    b = b.clamp(0, 255);
    a = a.clamp(0, 255);

    ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// The ground probe the grounded source path drops particles onto
/// (`RompCollider` in the reference). Returns the ground height under
/// `position` within `drop_distance`, or [`NO_GROUND_COLLISION`].
pub trait GroundProbe {
    fn ground(&mut self, position: &Vector3, drop_distance: f32) -> f32;
}

/// A flat ground at a fixed height, the test stand-in for the chunk-space
/// collider.
pub struct FlatGround {
    pub height: f32,
}

impl GroundProbe for FlatGround {
    fn ground(&mut self, _position: &Vector3, _drop_distance: f32) -> f32 {
        self.height
    }
}

/// The per-particle payload. The reference stores this as a union
/// discriminated by `isMesh_`; the mesh style trades size/colour for a
/// spin axis, spin speed and spin age.
#[derive(Clone, Copy, Debug)]
enum Extra {
    Mesh {
        spin_axis: [u8; 3],
        spin_speed: u8,
        spin_age: u16,
    },
    NonMesh {
        size: u16,
        colour: u32,
        index: u16,
    },
}

/// One particle, packed exactly as the reference packs it (`particle.ipp`).
/// Velocity lives in three i16s scaled by 256, age in a u16 counted in
/// 1/120 s increments, pitch/yaw in one u16 holding a byte each.
#[derive(Clone, Copy, Debug)]
pub struct Particle {
    position: Vector3,
    x_vel: i16,
    y_vel: i16,
    z_vel: i16,
    age: u16,
    pitch_yaw: u16,
    extra: Extra,
}

impl Default for Particle {
    /// The reference default constructor calls `kill()`, so a reserved slot
    /// reads as dead.
    fn default() -> Particle {
        Particle {
            position: Vector3::ZERO,
            x_vel: 0,
            y_vel: 0,
            z_vel: 0,
            age: AGE_MAX_DATA_UINT,
            pitch_yaw: 0,
            extra: Extra::NonMesh {
                size: 0,
                colour: 0,
                index: 0,
            },
        }
    }
}

impl Particle {
    /// The sprite constructor as the source action calls it: colour, size,
    /// pitch, yaw.
    pub fn new_sprite(
        position: Vector3,
        velocity: Vector3,
        colour: Vector4,
        size: f32,
        starting_pitch: f32,
        starting_yaw: f32,
    ) -> Particle {
        let mut ret = Particle {
            position,
            x_vel: 0,
            y_vel: 0,
            z_vel: 0,
            age: 0,
            pitch_yaw: 0,
            extra: Extra::NonMesh {
                size: 0,
                colour: colour_u32_from_normalised(&colour),
                index: 0,
            },
        };
        ret.set_velocity(velocity);
        ret.set_size(size);
        ret.set_age(0.0);
        ret.set_pitch(starting_pitch);
        ret.set_yaw(starting_yaw);
        ret
    }

    /// The mesh constructor as the source action calls it: solid white,
    /// pitch/yaw, a spin axis and speed.
    pub fn new_mesh(
        position: Vector3,
        velocity: Vector3,
        starting_pitch: f32,
        starting_yaw: f32,
        spin_axis: Vector3,
        spin_speed: f32,
    ) -> Particle {
        let mut ret = Particle {
            position,
            x_vel: 0,
            y_vel: 0,
            z_vel: 0,
            age: 0,
            pitch_yaw: 0,
            extra: Extra::Mesh {
                spin_axis: [0; 3],
                spin_speed: 0,
                spin_age: 0,
            },
        };
        ret.set_velocity(velocity);
        ret.set_age(0.0);
        ret.set_pitch(starting_pitch);
        ret.set_yaw(starting_yaw);
        ret.set_mesh_spin_axis(spin_axis);
        ret.set_mesh_spin_speed(spin_speed);
        ret
    }

    pub fn position(&self) -> Vector3 {
        self.position
    }

    pub fn set_position(&mut self, position: Vector3) {
        self.position = position;
    }

    pub fn is_mesh(&self) -> bool {
        matches!(self.extra, Extra::Mesh { .. })
    }

    /// `getVelocity`: unpack the three i16s over 256.
    pub fn velocity(&self) -> Vector3 {
        Vector3::new(
            self.x_vel as f32 / 256.0,
            self.y_vel as f32 / 256.0,
            self.z_vel as f32 / 256.0,
        )
    }

    /// `setVelocity`: scale by 256 and clamp into the i16 range.
    pub fn set_velocity(&mut self, v: Vector3) {
        self.x_vel = clamp(VELOCITY_MIN_DATA, v.x * 256.0, VELOCITY_MAX_DATA) as i16;
        self.y_vel = clamp(VELOCITY_MIN_DATA, v.y * 256.0, VELOCITY_MAX_DATA) as i16;
        self.z_vel = clamp(VELOCITY_MIN_DATA, v.z * 256.0, VELOCITY_MAX_DATA) as i16;
    }

    /// `size()`: unpacked metres.
    pub fn size(&self) -> f32 {
        let raw = match self.extra {
            Extra::NonMesh { size, .. } => size,
            Extra::Mesh { .. } => 0,
        };
        raw as f32 * UNPACK_SIZE
    }

    /// `size(f)`: clamp into the u16 size table.
    pub fn set_size(&mut self, f: f32) {
        if let Extra::NonMesh { size, .. } = &mut self.extra {
            *size = clamp(0.0, f * PACK_SIZE, SIZE_MAX_DATA) as u16;
        }
    }

    /// `sizeRaw()`.
    pub fn size_raw(&self) -> u16 {
        match self.extra {
            Extra::NonMesh { size, .. } => size,
            Extra::Mesh { .. } => 0,
        }
    }

    /// The packed ARGB colour (sprite particles only).
    pub fn colour_raw(&self) -> u32 {
        match self.extra {
            Extra::NonMesh { colour, .. } => colour,
            Extra::Mesh { .. } => 0,
        }
    }

    pub fn set_colour_raw(&mut self, colour: u32) {
        if let Extra::NonMesh { colour: c, .. } = &mut self.extra {
            *c = colour;
        }
    }

    /// The container slot index assigned on add (sprite particles only).
    pub fn index(&self) -> u16 {
        match self.extra {
            Extra::NonMesh { index, .. } => index,
            Extra::Mesh { .. } => 0,
        }
    }

    pub fn set_index(&mut self, index: u16) {
        if let Extra::NonMesh { index: i, .. } = &mut self.extra {
            *i = index;
        }
    }

    /// `isAlive()`: a maximally aged particle is dead.
    pub fn is_alive(&self) -> bool {
        self.age != AGE_MAX_DATA_UINT
    }

    /// `kill()`.
    pub fn kill(&mut self) {
        self.age = AGE_MAX_DATA_UINT;
    }

    /// `age()`: unpacked seconds.
    pub fn age(&self) -> f32 {
        self.age as f32 * UNPACK_AGE
    }

    /// `age(f)`: pack with the +0.5 rounding bias.
    pub fn set_age(&mut self, f: f32) {
        self.age = clamp(0.0, f * PACK_AGE + 0.5, AGE_MAX_DATA) as u16;
    }

    /// `nAgeIncrements(f)`: deliberate truncation, so a positive error
    /// always remains.
    pub fn n_age_increments(f: f32) -> u16 {
        clamp(0.0, f * PACK_AGE, AGE_MAX_DATA) as u16
    }

    /// `age(uint16 accurateAge)`: the unpack for an exact increment count.
    pub fn age_of(accurate_age: u16) -> f32 {
        accurate_age as f32 * UNPACK_AGE
    }

    /// `ageMax()`.
    pub fn age_max() -> u16 {
        AGE_MAX_DATA as u16
    }

    /// `ageAccurate()`.
    pub fn age_accurate(&self) -> u16 {
        self.age
    }

    /// `ageAccurate(a)`.
    pub fn set_age_accurate(&mut self, a: u16) {
        self.age = a;
    }

    /// `pitch()`: the top byte over the unpack scale.
    pub fn pitch(&self) -> f32 {
        (self.pitch_yaw >> 8) as f32 * UNPACK_ROT
    }

    /// `pitch(p)`: the +ABS_ROT guard keeps the cast positive; the Rust
    /// f32-to-u16 cast saturates where the reference's is undefined.
    pub fn set_pitch(&mut self, p: f32) {
        let pt = ((((p + ABS_ROT) * PACK_ROT) + ROUND_ROT) as u16 & 0xff) << 8;
        self.pitch_yaw = (self.pitch_yaw & 0xff) | pt;
    }

    /// `yaw()`: the bottom byte over the unpack scale.
    pub fn yaw(&self) -> f32 {
        (self.pitch_yaw & 0xff) as f32 * UNPACK_ROT
    }

    /// `yaw(y)`.
    pub fn set_yaw(&mut self, y: f32) {
        let yw = ((((y + ABS_ROT) * PACK_ROT) + ROUND_ROT) as u16) & 0xff;
        self.pitch_yaw = (self.pitch_yaw & 0xff00) | yw;
    }

    /// `meshSpinAxis()`: three u8s over 255.
    pub fn mesh_spin_axis(&self) -> Vector3 {
        match self.extra {
            Extra::Mesh { spin_axis, .. } => Vector3::new(
                spin_axis[0] as f32 / 255.0,
                spin_axis[1] as f32 / 255.0,
                spin_axis[2] as f32 / 255.0,
            ),
            Extra::NonMesh { .. } => Vector3::ZERO,
        }
    }

    /// `meshSpinAxis(axis)`.
    pub fn set_mesh_spin_axis(&mut self, axis: Vector3) {
        if let Extra::Mesh { spin_axis, .. } = &mut self.extra {
            spin_axis[0] = clamp(0.0, 255.0 * axis.x, SPIN_DIR_MAX_DATA) as u8;
            spin_axis[1] = clamp(0.0, 255.0 * axis.y, SPIN_DIR_MAX_DATA) as u8;
            spin_axis[2] = clamp(0.0, 255.0 * axis.z, SPIN_DIR_MAX_DATA) as u8;
        }
    }

    /// `meshSpinSpeed()`: one u8 over 255.
    pub fn mesh_spin_speed(&self) -> f32 {
        match self.extra {
            Extra::Mesh { spin_speed, .. } => spin_speed as f32 / 255.0,
            Extra::NonMesh { .. } => 0.0,
        }
    }

    /// `meshSpinSpeed(amount)`.
    pub fn set_mesh_spin_speed(&mut self, amount: f32) {
        if let Extra::Mesh { spin_speed, .. } = &mut self.extra {
            *spin_speed = clamp(0.0, 255.0 * amount, SPIN_SPEED_MAX_DATA) as u8;
        }
    }

    /// `meshSpinAge()`.
    pub fn mesh_spin_age(&self) -> f32 {
        match self.extra {
            Extra::Mesh { spin_age, .. } => spin_age as f32 * UNPACK_MESH_SPIN,
            Extra::NonMesh { .. } => 0.0,
        }
    }

    /// `meshSpinAge(s)`.
    pub fn set_mesh_spin_age(&mut self, s: f32) {
        if let Extra::Mesh { spin_age, .. } = &mut self.extra {
            *spin_age = clamp(0.0, s * PACK_MESH_SPIN, SPIN_ANGLE_MAX_DATA) as u16;
        }
    }
}

/// The container layouts of `lib/particle/particles.ipp`. `Contiguous` is
/// the reference default (push_back with a swap-pop removal);
/// `FixedIndex` resizes to capacity up front and reuses dead slots in
/// place.
enum Container {
    Contiguous {
        items: Vec<Particle>,
        /// `particleIdx_`, the stable index handed to the next particle.
        index_counter: u16,
        capacity: usize,
    },
    FixedIndex {
        items: Vec<Particle>,
        /// `size_`, the live count.
        size: usize,
        /// `next_`, the scan cursor.
        next: usize,
    },
}

impl Container {
    fn contiguous(capacity: usize) -> Container {
        Container::Contiguous {
            items: Vec::new(),
            index_counter: 0,
            capacity,
        }
    }

    fn fixed_index(capacity: usize) -> Container {
        Container::FixedIndex {
            items: vec![Particle::default(); capacity],
            size: 0,
            next: 0,
        }
    }

    /// `reserve`: clear, then size the store. For `FixedIndex` this acts
    /// like a resize — every slot now holds a dead particle.
    fn reserve(&mut self, amount: usize) {
        match self {
            Container::Contiguous {
                items,
                index_counter,
                capacity,
            } => {
                items.clear();
                items.reserve(amount);
                *index_counter = 0;
                *capacity = amount;
            }
            Container::FixedIndex { items, size, next } => {
                *items = vec![Particle::default(); amount];
                *size = 0;
                *next = 0;
            }
        }
    }

    /// `clear`: `FixedIndex` kills in place and resets its cursors;
    /// `Contiguous` empties the vector.
    fn clear(&mut self) {
        match self {
            Container::Contiguous { items, .. } => items.clear(),
            Container::FixedIndex { items, size, next } => {
                for p in items.iter_mut() {
                    p.kill();
                }
                *size = 0;
                *next = 0;
            }
        }
    }

    fn size(&self) -> usize {
        match self {
            Container::Contiguous { items, .. } => items.len(),
            Container::FixedIndex { size, .. } => *size,
        }
    }

    fn capacity(&self) -> usize {
        match self {
            Container::Contiguous { capacity, .. } => *capacity,
            Container::FixedIndex { items, .. } => items.len(),
        }
    }

    /// `addParticle`: returns false when the container is full.
    fn add_particle(&mut self, particle: Particle, is_mesh: bool) -> bool {
        match self {
            Container::Contiguous {
                items,
                index_counter,
                capacity,
            } => {
                if items.len() >= *capacity {
                    return false;
                }
                // The reference asserts !isMesh for contiguous containers.
                if is_mesh {
                    return false;
                }
                let mut p = particle;
                p.set_index(*index_counter);
                *index_counter = ((*index_counter as usize + 1) % *capacity) as u16;
                items.push(p);
                true
            }
            Container::FixedIndex { items, size, next } => {
                let available = items.len();
                if *size >= available {
                    return false;
                }
                while items[*next].is_alive() {
                    *next = (*next + 1) % available;
                }
                let mut p = particle;
                if !is_mesh {
                    p.set_index(*next as u16);
                }
                items[*next] = p;
                *size += 1;
                *next = (*next + 1) % available;
                true
            }
        }
    }

    /// `removeParticle` at an index: swap-pop for `Contiguous`, kill in
    /// place for `FixedIndex`.
    fn remove_particle(&mut self, index: usize) {
        match self {
            Container::Contiguous { items, .. } => {
                items[index] = *items.last().expect("empty container");
                items.pop();
            }
            Container::FixedIndex { items, size, .. } => {
                *size -= 1;
                items[index].kill();
            }
        }
    }

    fn items(&self) -> &[Particle] {
        match self {
            Container::Contiguous { items, .. } => items,
            Container::FixedIndex { items, .. } => items,
        }
    }

    fn items_mut(&mut self) -> &mut [Particle] {
        match self {
            Container::Contiguous { items, .. } => items,
            Container::FixedIndex { items, .. } => items,
        }
    }
}

/// The action interface the update loop drives
/// (`ParticleSystemAction`).
pub trait ParticleSystemAction {
    /// `setFirstUpdate`.
    fn set_first_update(&mut self);

    /// `enabled`.
    fn enabled(&self) -> bool;

    /// `execute`. `rng` is the shared `bw_random` stream.
    fn execute(&mut self, ps: &mut ParticleSystem, d_time: f32, rng: &mut BWRandom);
}

/// The `ParticleSystemAction` base fields: delay, minimum age, elapsed
/// age and the enabled flag (`particle_system_action.ipp:16`).
struct ActionBase {
    delay: f32,
    age: f32,
    enabled: bool,
}

impl ActionBase {
    fn new() -> ActionBase {
        ActionBase {
            delay: 0.0,
            age: 0.0,
            enabled: true,
        }
    }

    /// The common gate: do nothing (but still age) while the action is
    /// younger than its delay or the frame time is empty.
    fn tick_age(&mut self, d_time: f32) -> bool {
        if self.age < self.delay || d_time <= 0.0 {
            self.age += d_time;
            return false;
        }
        true
    }
}

/// `SourcePSA`: the particle factory (`actions/source_psa.cpp`).
pub struct SourcePSA {
    base: ActionBase,
    motion_triggered: bool,
    time_triggered: bool,
    grounded: bool,
    rate: f32,
    sensitivity: f32,
    max_speed: f32,
    active_period: f32,
    sleep_period: f32,
    sleep_period_max: f32,
    minimum_size: f32,
    maximum_size: f32,
    forced_unit_size: i32,
    /// Allowed creation time per frame in seconds (reference: CPU stamps,
    /// `allowedTimeInSeconds`).
    allowed_time: f32,
    /// The synthetic wall clock `timestamp()` reads. It advances only by
    /// the per-particle cost, i.e. by the CPU time creation is modelled
    /// to burn (zero by default: an infinitely fast host).
    now: f32,
    /// `currentTime_`, the timestamp taken at the head of each execute.
    frame_stamp: f32,
    /// The synthetic CPU cost per created particle.
    per_particle_cost: f32,
    position_src: Option<VectorGenerator>,
    velocity_src: Option<VectorGenerator>,
    queued_total: i32,
    forced_total: i32,
    period_time: f32,
    accumulated_time: f32,
    last_position_of_ps: Vector3,
    first_update: bool,
    drop_distance: f32,
    ground: Option<Box<dyn GroundProbe>>,
    current_sleep_period: f32,
    initial_rotation: Vector2,
    random_initial_rotation: Vector2,
    initial_colour: Vector4,
    ignore_rotation: bool,
    random_spin: bool,
    min_spin: f32,
    max_spin: f32,
    inherit_velocity: f32,
    velocity_of_ps: Vector3,
}

impl SourcePSA {
    /// The reference constructor defaults (`source_psa.ipp:18`).
    pub fn new(
        position_src: Option<VectorGenerator>,
        velocity_src: Option<VectorGenerator>,
    ) -> SourcePSA {
        SourcePSA {
            base: ActionBase::new(),
            motion_triggered: false,
            time_triggered: false,
            grounded: false,
            rate: 0.0,
            sensitivity: 0.1,
            max_speed: 0.0,
            active_period: 1.0,
            sleep_period: 0.0,
            sleep_period_max: -1.0,
            minimum_size: 1.0,
            maximum_size: 1.0,
            forced_unit_size: 1,
            allowed_time: 1.0,
            now: 0.0,
            frame_stamp: 0.0,
            per_particle_cost: 0.0,
            position_src,
            velocity_src,
            queued_total: 0,
            forced_total: 0,
            period_time: 0.0,
            accumulated_time: 0.0,
            last_position_of_ps: Vector3::ZERO,
            first_update: true,
            drop_distance: 15.0,
            ground: None,
            current_sleep_period: 0.0,
            initial_rotation: Vector2::new(0.0, 0.0),
            random_initial_rotation: Vector2::new(0.0, 0.0),
            initial_colour: Vector4::new(0.5, 0.5, 0.5, 1.0),
            ignore_rotation: false,
            random_spin: false,
            min_spin: 0.0,
            max_spin: 0.0,
            inherit_velocity: 0.0,
            velocity_of_ps: Vector3::ZERO,
        }
    }

    pub fn set_delay(&mut self, seconds: f32) {
        self.base.delay = seconds;
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.base.enabled = enabled;
    }

    pub fn set_motion_triggered(&mut self, flag: bool) {
        self.motion_triggered = flag;
    }

    pub fn set_time_triggered(&mut self, flag: bool) {
        self.time_triggered = flag;
    }

    pub fn set_grounded(&mut self, flag: bool) {
        self.grounded = flag;
    }

    pub fn set_ground(&mut self, ground: Box<dyn GroundProbe>) {
        self.ground = Some(ground);
    }

    pub fn set_drop_distance(&mut self, distance: f32) {
        self.drop_distance = distance;
    }

    pub fn set_rate(&mut self, rate: f32) {
        self.rate = rate;
    }

    pub fn set_sensitivity(&mut self, value: f32) {
        self.sensitivity = value;
    }

    pub fn set_max_speed(&mut self, value: f32) {
        self.max_speed = value;
    }

    pub fn set_active_period(&mut self, seconds: f32) {
        self.active_period = seconds;
    }

    pub fn set_sleep_period(&mut self, seconds: f32) {
        self.sleep_period = seconds;
    }

    pub fn set_sleep_period_max(&mut self, seconds: f32) {
        self.sleep_period_max = seconds;
    }

    pub fn set_minimum_size(&mut self, size: f32) {
        self.minimum_size = size;
    }

    pub fn set_maximum_size(&mut self, size: f32) {
        self.maximum_size = size;
    }

    pub fn set_forced_unit_size(&mut self, size: i32) {
        self.forced_unit_size = size;
    }

    pub fn set_allowed_time(&mut self, seconds: f32) {
        self.allowed_time = seconds;
    }

    /// The synthetic CPU cost of creating one particle. The reference
    /// measures real CPU time between `timestamp()` calls; this port
    /// advances its clock by this amount per particle instead.
    pub fn set_per_particle_cost(&mut self, seconds: f32) {
        self.per_particle_cost = seconds;
    }

    pub fn set_initial_rotation(&mut self, rotation: Vector2) {
        self.initial_rotation = rotation;
    }

    pub fn set_random_initial_rotation(&mut self, rotation: Vector2) {
        self.random_initial_rotation = rotation;
    }

    pub fn set_initial_colour(&mut self, colour: Vector4) {
        self.initial_colour = colour;
    }

    pub fn set_random_spin(&mut self, enable: bool) {
        self.random_spin = enable;
    }

    pub fn set_min_spin(&mut self, amount: f32) {
        self.min_spin = amount;
    }

    pub fn set_max_spin(&mut self, amount: f32) {
        self.max_spin = amount;
    }

    pub fn set_ignore_rotation(&mut self, option: bool) {
        self.ignore_rotation = option;
    }

    pub fn set_inherit_velocity(&mut self, amount: f32) {
        self.inherit_velocity = amount;
    }

    /// `create(number)`: demand particles over time.
    pub fn create(&mut self, number: i32) {
        self.queued_total += number;
    }

    /// `force(number)`: chains of `forcedUnitSize` particles next execute.
    pub fn force(&mut self, number: i32) {
        self.forced_total += number;
    }

    /// The load-time call (`source_psa.cpp:111`): after properties load,
    /// the first sleep period is drawn from the configured bounds. Systems
    /// configured with a sleep period must arm the cycle once before the
    /// first execute.
    pub fn arm_sleep_cycle(&mut self, rng: &mut BWRandom) {
        self.current_sleep_period = self.generate_sleep_period(rng);
    }

    /// `generateSleepPeriod` (`source_psa.ipp:276`): a unit draw between
    /// the two period bounds, or the fixed bound when only one is set.
    fn generate_sleep_period(&self, rng: &mut BWRandom) -> f32 {
        if self.sleep_period_max >= 0.0 && self.sleep_period >= 0.0 {
            (self.sleep_period_max - self.sleep_period).abs() * rng.unit_rand() + self.sleep_period
        } else if self.sleep_period >= 0.0 {
            self.sleep_period
        } else if self.sleep_period_max >= 0.0 {
            self.sleep_period_max
        } else {
            0.0
        }
    }

    /// `createParticles` (`source_psa.cpp:494`): the per-particle draw
    /// order is position, velocity, size, pitch, yaw, then the spin axis
    /// and speed when random spin is on. A grounded particle whose ground
    /// probe misses creates nothing at all.
    fn create_particles(
        &mut self,
        ps: &mut ParticleSystem,
        object_to_world: &Matrix,
        number_of_particles: i32,
        disp_per_particle: Vector3,
        rng: &mut BWRandom,
    ) {
        let size_difference = self.maximum_size - self.minimum_size;

        let mut count = 0;
        while count < number_of_particles
            && (self.allowed_time == 0.0 || self.now - self.frame_stamp < self.allowed_time)
        {
            // The particle's position in local space, then world space
            // with the chain spacing subtracted.
            let mut pos_in_ls = Vector3::ZERO;
            if let Some(src) = self.position_src.as_mut() {
                pos_in_ls = src.generate(rng);
            }
            let mut position = object_to_world.apply_point(&pos_in_ls);
            position = position - disp_per_particle * (number_of_particles - 1 - count) as f32;

            // The particle's velocity in local space, then world space,
            // plus the emitter's inherited motion.
            let mut vel_in_ls = Vector3::ZERO;
            if let Some(src) = self.velocity_src.as_mut() {
                vel_in_ls = src.generate(rng);
            }
            let mut velocity = object_to_world.apply_vector(&vel_in_ls);
            velocity = velocity + self.velocity_of_ps * self.inherit_velocity;

            // Drop to the ground when asked; a missing probe finds
            // nothing.
            if self.grounded
                && let Some(probe) = &mut self.ground
            {
                // Nudge up so a collision at ground level still hits.
                position.y += 0.05;
                position.y = probe.ground(&position, self.drop_distance + 0.05);
            }

            if position.y != NO_GROUND_COLLISION {
                let size = rng.unit_rand() * size_difference + self.minimum_size;

                let pitch = self.initial_rotation.x
                    + self.random_initial_rotation.x * (rng.unit_rand() - 0.5);
                let yaw = self.initial_rotation.y
                    + self.random_initial_rotation.y * (rng.unit_rand() - 0.5);

                let colour = self.initial_colour;
                let mut spin_axis = Vector3::ZERO;
                let mut spin_speed = 0.0;
                if self.random_spin {
                    spin_axis = Vector3::new(rng.unit_rand(), rng.unit_rand(), rng.unit_rand());
                    spin_axis.normalise();
                    spin_speed = rng.unit_rand() * (self.max_spin - self.min_spin) + self.min_spin;
                }

                if ps.mesh_style() {
                    // Mesh particles take their base orientation from the
                    // emitter's forward axis.
                    let forward = object_to_world.apply_to_unit_axis_vector(2);
                    let pitch = pitch + forward.pitch();
                    let yaw = yaw + forward.yaw();
                    ps.add_particle(
                        Particle::new_mesh(position, velocity, pitch, yaw, spin_axis, spin_speed),
                        true,
                    );
                } else {
                    ps.add_particle(
                        Particle::new_sprite(position, velocity, colour, size, pitch, yaw),
                        false,
                    );
                }
            }
            count += 1;
            // The synthetic clock burns the modelled CPU cost of the
            // iteration that just ran; the loop condition re-reads it.
            self.now += self.per_particle_cost;
        }
    }
}

impl ParticleSystemAction for SourcePSA {
    /// `setFirstUpdate` also re-arms the source's own displacement
    /// baseline.
    fn set_first_update(&mut self) {
        self.first_update = true;
    }

    fn enabled(&self) -> bool {
        self.base.enabled
    }

    /// `SourcePSA::execute` (`source_psa.cpp:309`).
    fn execute(&mut self, ps: &mut ParticleSystem, d_time: f32, rng: &mut BWRandom) {
        // Do nothing if the dTime passed was zero or if the action is not
        // quite old enough to be active.
        if !self.base.tick_age(d_time) {
            return;
        }

        // The reference stamps the wall clock here; this port stamps its
        // synthetic clock.
        self.frame_stamp = self.now;

        // Particle system velocity and displacement since last frame.
        let mut object_to_world = *ps.object_to_world();
        let position_of_ps = object_to_world.apply_to_origin();
        let displacement = position_of_ps
            - if self.first_update {
                position_of_ps
            } else {
                self.last_position_of_ps
            };
        self.last_position_of_ps = position_of_ps;
        self.first_update = false;
        self.velocity_of_ps = displacement / d_time;

        if self.ignore_rotation {
            let origin = object_to_world.apply_to_origin();
            object_to_world.set_translate(origin.x, origin.y, origin.z);
        }

        // Forced particles.
        if self.forced_total > 0 {
            let mut frame_total = self.forced_total * self.forced_unit_size;

            if frame_total + ps.size() as i32 > ps.capacity() as i32 {
                frame_total = ps.capacity() as i32 - ps.size() as i32;
            }

            while frame_total > 0 {
                let force_size = i32::min(self.forced_unit_size, frame_total);
                self.create_particles(ps, &object_to_world, force_size, Vector3::ZERO, rng);
                frame_total -= force_size;
            }

            self.forced_total = 0;
        }

        // Motion and time triggered creation. Note `frame_total` is
        // declared once and *not* reset after the motion block, so a
        // motion-triggered spawn this frame is re-spawned by the time
        // block — preserved verbatim.
        if self.motion_triggered || self.time_triggered || self.queued_total > 0 {
            let distance = displacement.length();
            let mut frame_total: i32 = 0;

            // Motion triggered particles.
            if self.motion_triggered {
                let speed = distance / d_time;
                if speed > self.sensitivity {
                    // Sensitivity cannot be zero or less.
                    frame_total = ((1.0 + 2.0 * ((speed - self.sensitivity) / self.sensitivity))
                        * d_time) as i32;

                    if frame_total + ps.size() as i32 > ps.capacity() as i32 {
                        frame_total = ps.capacity() as i32 - ps.size() as i32;
                    }
                }

                if frame_total > 0 {
                    let displacement_per_particle = displacement / frame_total as f32;
                    self.create_particles(
                        ps,
                        &object_to_world,
                        frame_total,
                        displacement_per_particle,
                        rng,
                    );
                }
            }

            // Time triggered / queued particles.
            if self.time_triggered || self.queued_total > 0 {
                if self.rate > 0.0 {
                    if self.current_sleep_period > 0.0 {
                        let period = self.active_period + self.current_sleep_period;

                        // Where we are in the active/sleep cycle.
                        let mut last_period_time = self.period_time;
                        self.period_time += d_time;
                        if self.period_time > period && period > 0.0 {
                            self.period_time %= period;
                            last_period_time = 0.0;

                            self.current_sleep_period = self.generate_sleep_period(rng);
                        }

                        // Accumulate only the active time of this period.
                        if self.period_time <= self.active_period {
                            self.accumulated_time += self.period_time - last_period_time;
                        } else if last_period_time <= self.active_period {
                            // We were awake last frame; bank the awake
                            // remainder.
                            self.accumulated_time += self.active_period - last_period_time;
                        }
                    } else {
                        // Always active.
                        self.accumulated_time += d_time;
                    }

                    let time_between_particles = 1.0 / self.rate;
                    if self.accumulated_time > time_between_particles {
                        frame_total = (self.accumulated_time / time_between_particles) as i32;
                        self.accumulated_time -= frame_total as f32 * time_between_particles;
                        self.queued_total -= frame_total;
                    }
                }

                // Capacity is a hard limit; excess particles vanish.
                if frame_total + ps.size() as i32 > ps.capacity() as i32 {
                    frame_total = ps.capacity() as i32 - ps.size() as i32;
                }

                // Hard limit time triggered particles past max speed.
                if frame_total > 0 && self.max_speed > 0.0 {
                    let speed = distance / d_time;
                    if speed > self.max_speed {
                        frame_total = 0;
                    }
                }

                if frame_total > 0 {
                    let displacement_per_particle = displacement / frame_total as f32;
                    self.create_particles(
                        ps,
                        &object_to_world,
                        frame_total,
                        displacement_per_particle,
                        rng,
                    );
                }
            }
        }
    }
}

/// `JitterPSA` (`actions/jitter_psa.cpp:86`): random movement on the alive
/// particles. The reference defers non-local systems to a late-update
/// queue; this port is always the local path.
pub struct JitterPSA {
    base: ActionBase,
    affect_position: bool,
    affect_velocity: bool,
    position_src: Option<VectorGenerator>,
    velocity_src: Option<VectorGenerator>,
}

impl JitterPSA {
    /// The reference constructor defaults both affect flags to false.
    pub fn new(
        position_src: Option<VectorGenerator>,
        velocity_src: Option<VectorGenerator>,
    ) -> JitterPSA {
        JitterPSA {
            base: ActionBase::new(),
            affect_position: false,
            affect_velocity: false,
            position_src,
            velocity_src,
        }
    }

    pub fn set_affect_position(&mut self, flag: bool) {
        self.affect_position = flag;
    }

    pub fn set_affect_velocity(&mut self, flag: bool) {
        self.affect_velocity = flag;
    }

    pub fn set_delay(&mut self, seconds: f32) {
        self.base.delay = seconds;
    }
}

impl ParticleSystemAction for JitterPSA {
    fn set_first_update(&mut self) {}

    fn enabled(&self) -> bool {
        self.base.enabled
    }

    fn execute(&mut self, ps: &mut ParticleSystem, d_time: f32, rng: &mut BWRandom) {
        if !self.base.tick_age(d_time) {
            return;
        }

        for particle in ps.container.items_mut() {
            if particle.is_alive() {
                if self.affect_position
                    && let Some(src) = self.position_src.as_mut()
                {
                    let jitter = src.generate(rng);
                    let moved = particle.position() + jitter * d_time;
                    particle.set_position(moved);
                }

                if self.affect_velocity
                    && let Some(src) = self.velocity_src.as_mut()
                {
                    let jitter = src.generate(rng);
                    let velocity = particle.velocity() + jitter;
                    particle.set_velocity(velocity);
                }
            }
        }
    }
}

/// The particle system (`particle_system.cpp`): a container, a list of
/// actions, and the fixed-frame-rate update loop.
pub struct ParticleSystem {
    wind_factor: f32,
    first_update: bool,
    container: Container,
    /// Whether the attached renderer draws mesh particles; decides the
    /// sprite/mesh branch of the source action.
    mesh_style: bool,
    object_to_world: Matrix,
    fixed_frame_rate: f32,
    frames_left_over: f32,
    actions: Vec<Box<dyn ParticleSystemAction>>,
    /// The pre-computed wind vector for the current frame.
    wind_effect: Vector3,
}

impl ParticleSystem {
    /// `ParticleSystem(initialCapacity)` with a chosen container layout.
    pub fn new(contiguous: bool, initial_capacity: usize) -> ParticleSystem {
        let container = if contiguous {
            Container::contiguous(initial_capacity)
        } else {
            Container::fixed_index(initial_capacity)
        };
        ParticleSystem {
            wind_factor: 0.0,
            first_update: false,
            container,
            mesh_style: false,
            object_to_world: Matrix::identity(),
            fixed_frame_rate: -1.0,
            frames_left_over: 0.0,
            actions: Vec::new(),
            wind_effect: Vector3::ZERO,
        }
    }

    /// `capacity(number)`: clamped at MAX_CAPACITY; a change clears and
    /// reserves.
    pub fn set_capacity(&mut self, number: usize) {
        let number = usize::min(number, MAX_CAPACITY);
        if number != self.container.capacity() {
            self.container.clear();
            self.container.reserve(number);
        }
    }

    pub fn capacity(&self) -> usize {
        self.container.capacity()
    }

    /// Live particle count.
    pub fn size(&self) -> usize {
        self.container.size()
    }

    pub fn clear(&mut self) {
        self.container.clear();
    }

    /// `addParticle`, gated on the capacity like the reference.
    pub fn add_particle(&mut self, particle: Particle, is_mesh: bool) -> bool {
        if self.size() < self.capacity() {
            self.container.add_particle(particle, is_mesh)
        } else {
            false
        }
    }

    /// Remove the particle at a container index.
    pub fn remove_particle(&mut self, index: usize) {
        self.container.remove_particle(index);
    }

    pub fn particle(&self, index: usize) -> &Particle {
        &self.container.items()[index]
    }

    pub fn particles(&self) -> &[Particle] {
        self.container.items()
    }

    pub fn push_action(&mut self, action: Box<dyn ParticleSystemAction>) {
        self.actions.push(action);
    }

    pub fn set_object_to_world(&mut self, matrix: Matrix) {
        self.object_to_world = matrix;
    }

    pub fn object_to_world(&self) -> &Matrix {
        &self.object_to_world
    }

    pub fn set_wind_factor(&mut self, factor: f32) {
        self.wind_factor = factor;
    }

    pub fn set_fixed_frame_rate(&mut self, rate: f32) {
        self.fixed_frame_rate = rate;
    }

    pub fn frames_left_over(&self) -> f32 {
        self.frames_left_over
    }

    pub fn set_mesh_style(&mut self, mesh_style: bool) {
        self.mesh_style = mesh_style;
    }

    pub fn mesh_style(&self) -> bool {
        self.mesh_style
    }

    /// `firstUpdate`: when armed, every action's `setFirstUpdate` runs
    /// before its next execute.
    pub fn arm_first_update(&mut self) {
        self.first_update = true;
    }

    /// `predictPosition` (`particle_system.cpp:1062`).
    fn predict_position(particle: &Particle, dt: f32, wind_effect: Vector3) -> Vector3 {
        let old_velocity = particle.velocity();
        let new_velocity = old_velocity + wind_effect;
        particle.position() + new_velocity * dt
    }

    /// `ParticleSystem::update` (`particle_system.cpp:924`): bank the
    /// leftover time, clamp the frame to one second, pre-compute the wind,
    /// then loop quantised time slices of aging, actions and movement.
    /// The manager-active gate and the renderer update are host concerns
    /// and are not modelled here.
    pub fn update(&mut self, d_time: f32, wind_velocity: Vector3, rng: &mut BWRandom) {
        let mut d_time = d_time + self.frames_left_over;

        if d_time > 1.0 {
            d_time = 1.0;
        }

        // Pre-calculate the wind vector for this frame.
        self.wind_effect = wind_velocity * clamp(0.0, self.wind_factor, 1.0);

        while d_time > 0.0 {
            let mut dt = if self.fixed_frame_rate > 0.0 {
                1.0 / self.fixed_frame_rate
            } else {
                d_time
            };

            // Quantise dt to the particle's age increment.
            let n_age_increments = Particle::n_age_increments(dt);
            if n_age_increments == 0 {
                break;
            }

            dt = Particle::age_of(n_age_increments);

            if dt > d_time {
                if (dt - d_time).abs() < 0.0004 {
                    // Allow some inaccuracy.
                    dt = d_time;
                } else {
                    break;
                }
            }

            d_time -= dt;

            // Aging pass.
            for particle in self.container.items_mut() {
                if particle.is_alive() {
                    let age = particle.age_accurate();
                    if age as u32 + n_age_increments as u32 > Particle::age_max() as u32 {
                        particle.set_age_accurate(Particle::age_max());
                    } else {
                        particle.set_age_accurate(age + n_age_increments);
                    }
                }
            }

            // Actions pass. Note `frame_total`-style carryover: the
            // reference iterates actions and executes each enabled one.
            let mut actions = std::mem::take(&mut self.actions);
            for action in actions.iter_mut() {
                if self.first_update {
                    action.set_first_update();
                }
                if action.enabled() {
                    action.execute(self, dt, rng);
                }
            }
            self.actions = actions;
            self.first_update = false;

            // Movement pass.
            let wind_effect = self.wind_effect;
            for particle in self.container.items_mut() {
                if particle.is_alive() {
                    let moved = Self::predict_position(particle, dt, wind_effect);
                    particle.set_position(moved);
                }
            }
        }

        self.frames_left_over = d_time;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_generator::{
        BoxVectorGenerator, CylinderVectorGenerator, LineVectorGenerator, PointVectorGenerator,
        SphereVectorGenerator, VectorGenerator,
    };

    fn point(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3::new(x, y, z)
    }

    /// A pure translation matrix (`Matrix::translation` is a mutating
    /// method in this port's foundation).
    fn translation(x: f32, y: f32, z: f32) -> foundation::math::matrix::Matrix {
        let mut m = foundation::math::matrix::Matrix::identity();
        m.translation(&point(x, y, z));
        m
    }

    /// Run `n` quantised 1/120 s slices (one increment each). Rate-based
    /// expectations use 8-frame multiples: 8 x age_of(1) is exact in f32
    /// and just over the 1/16 s slice a rate of 16 needs for one particle.
    fn run_slices(ps: &mut ParticleSystem, rng: &mut BWRandom, n: usize) {
        ps.set_fixed_frame_rate(120.0);
        for _ in 0..n {
            ps.update(1.0 / 120.0, Vector3::ZERO, rng);
        }
    }

    #[test]
    fn velocity_packs_with_clamps() {
        let mut p =
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0);
        p.set_velocity(point(1.5, -2.25, 200.0));
        assert_eq!(p.velocity(), point(1.5, -2.25, 32767.0 / 256.0));
        // Clamps at the i16 range over 256, and reads back unpacked.
        p.set_velocity(point(1.0e6, -1.0e6, 200.0));
        assert_eq!(
            p.velocity(),
            point(32767.0 / 256.0, -32768.0 / 256.0, 32767.0 / 256.0)
        );
        // Truncation toward zero on the packed value: 0.009 * 256 = 2.3
        // truncates to 2 increments, and 0.003 * 256 rounds down to none.
        p.set_velocity(point(0.009, -0.009, 0.0));
        assert_eq!(p.velocity(), point(0.0078125, -0.0078125, 0.0));
        p.set_velocity(point(0.003, -0.003, 0.0));
        assert_eq!(p.velocity(), Vector3::ZERO);
    }

    #[test]
    fn size_packs_into_a_u16() {
        let mut p =
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0);
        p.set_size(0.5);
        // 0.5 * 655.35 truncates on the way in and loses a little on the
        // way back out.
        assert_eq!(p.size_raw(), 327);
        assert!((p.size() - 0.5).abs() < 0.01);
        p.set_size(200.0);
        assert_eq!(p.size(), 100.0);
        p.set_size(-1.0);
        assert_eq!(p.size(), 0.0);
    }

    #[test]
    fn age_packs_in_120th_second_increments() {
        let mut p =
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0);
        // The +0.5 bias rounds 0.5 increments up.
        p.set_age(2.0 / 120.0);
        assert_eq!(p.age_accurate(), 2);
        assert!((p.age() - 2.0 / 120.0).abs() < 1e-6);
        // nAgeIncrements truncates deliberately.
        assert_eq!(Particle::n_age_increments(0.009), 1); // 0.009 * 120 = 1.08
        assert_eq!(Particle::n_age_increments(0.004), 0);
        // age_of is the exact unpack.
        assert_eq!(Particle::age_of(120), 1.0);
        assert_eq!(Particle::age_max(), 65535);
        // kill marks the sentinel and isAlive reads it.
        assert!(p.is_alive());
        p.kill();
        assert!(!p.is_alive());
        assert_eq!(p.age_accurate(), 0xffff);
    }

    #[test]
    fn pitch_and_yaw_pack_a_byte_each() {
        let mut p =
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0);
        p.set_pitch(0.0);
        p.set_yaw(0.0);
        assert_eq!(p.pitch(), 0.0);
        assert_eq!(p.yaw(), 0.0);
        // A quarter turn each: 256 * (pi/2) / 2pi = 64 slots.
        p.set_pitch(std::f32::consts::FRAC_PI_2);
        p.set_yaw(std::f32::consts::FRAC_PI_2);
        assert!((p.pitch() - std::f32::consts::FRAC_PI_2).abs() < 1e-3);
        assert!((p.yaw() - std::f32::consts::FRAC_PI_2).abs() < 1e-3);
        // Pitch and yaw share the word without clobbering each other.
        p.set_yaw(0.0);
        assert!((p.pitch() - std::f32::consts::FRAC_PI_2).abs() < 1e-3);
        assert_eq!(p.yaw(), 0.0);
    }

    #[test]
    fn mesh_spin_packs_bytes_and_an_angle() {
        let mut p = Particle::new_mesh(
            Vector3::ZERO,
            Vector3::ZERO,
            0.0,
            0.0,
            point(1.0, 0.5, -1.0),
            0.5,
        );
        assert!(p.is_mesh());
        // Each byte holds 255 * component, truncated; 0.5 * 255 = 127.
        assert_eq!(p.mesh_spin_axis(), point(1.0, 127.0 / 255.0, 0.0));
        assert_eq!(p.mesh_spin_speed(), 127.0 / 255.0);
        p.set_mesh_spin_age(1.0);
        // The age word quantises through 655.35, so 1.0 s reads back one
        // increment short.
        assert!((p.mesh_spin_age() - 1.0).abs() < 0.001);
        // Negatives clamp to zero, positives saturate at 255.
        p.set_mesh_spin_axis(point(-9.0, 0.5, 9.0));
        assert_eq!(p.mesh_spin_axis(), point(0.0, 127.0 / 255.0, 1.0));
        p.set_mesh_spin_speed(-3.0);
        assert_eq!(p.mesh_spin_speed(), 0.0);
    }

    #[test]
    fn sprite_colour_packs_argb_with_clamps() {
        let colour = Vector4::new(1.0, 0.5, 0.0, 0.25);
        let packed = colour_u32_from_normalised(&colour);
        // int() truncation: 0.25*255.0 = 63, 0.5*255.0 = 127. The A/R/G/B
        // byte lanes: blue lands on zero.
        assert_eq!(packed, (63 << 24) | (255 << 16) | (127 << 8));

        // Out-of-range channels clamp per channel; green saturates at 255
        // and the other lanes clamp to zero.
        let clamped = colour_u32_from_normalised(&Vector4::new(2.0, -1.0, 1.5, 0.0));
        assert_eq!(clamped, (255 << 16) | 255);

        // The sprite constructor stores the packed colour.
        let mut p = Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, colour, 1.0, 0.0, 0.0);
        assert_eq!(p.colour_raw(), packed);
        p.set_colour_raw(0x1122_3344);
        assert_eq!(p.colour_raw(), 0x1122_3344);
    }

    #[test]
    fn sprite_index_round_trips() {
        let mut p =
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0);
        assert_eq!(p.index(), 0);
        p.set_index(7);
        assert_eq!(p.index(), 7);
        // Mesh particles keep the index path inert.
        let mut m = Particle::new_mesh(Vector3::ZERO, Vector3::ZERO, 0.0, 0.0, Vector3::ZERO, 0.0);
        m.set_index(9);
        assert_eq!(m.index(), 0);
        assert_eq!(m.colour_raw(), 0);
        assert_eq!(m.size_raw(), 0);
    }

    #[test]
    fn the_default_particle_is_dead() {
        let p = Particle::default();
        assert!(!p.is_alive());
    }

    #[test]
    fn contiguous_container_indexes_and_swap_pops() {
        let mut ps = ParticleSystem::new(true, 3);
        for _ in 0..3 {
            assert!(ps.add_particle(
                Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0),
                false
            ));
        }
        // Full.
        assert!(!ps.add_particle(
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0),
            false
        ));
        // Stable indices 0, 1, 2 handed out in order.
        assert_eq!(ps.particle(0).index(), 0);
        assert_eq!(ps.particle(1).index(), 1);
        assert_eq!(ps.particle(2).index(), 2);
        // Swap-pop removal moves the tail into the hole.
        ps.remove_particle(0);
        assert_eq!(ps.size(), 2);
        assert_eq!(ps.particle(0).index(), 2);
        // Mesh particles are refused by the contiguous layout.
        assert!(!ps.add_particle(
            Particle::new_mesh(Vector3::ZERO, Vector3::ZERO, 0.0, 0.0, Vector3::ZERO, 0.0),
            true
        ));
    }

    #[test]
    fn fixed_index_container_reuses_dead_slots() {
        let mut ps = ParticleSystem::new(false, 3);
        for _ in 0..3 {
            assert!(ps.add_particle(
                Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0),
                false
            ));
        }
        assert!(!ps.add_particle(
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0),
            false
        ));
        // Kill the middle slot; the next add scans forward into it.
        ps.remove_particle(1);
        assert!(ps.add_particle(
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0),
            false
        ));
        assert_eq!(ps.particle(1).index(), 1);
        // clear kills everything and resets the cursors.
        ps.clear();
        assert_eq!(ps.size(), 0);
        assert!(ps.particles().iter().all(|p| !p.is_alive()));
        assert!(ps.add_particle(
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0),
            false
        ));
        assert_eq!(ps.particle(0).index(), 0);
    }

    #[test]
    fn capacity_is_capped_and_reserve_resets() {
        let mut ps = ParticleSystem::new(false, 1);
        assert_eq!(ps.capacity(), 1);
        ps.set_capacity(MAX_CAPACITY + 100);
        assert_eq!(ps.capacity(), MAX_CAPACITY);
        assert_eq!(ps.size(), 0);
        // Shrinking clears the population.
        ps.add_particle(
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0),
            false,
        );
        ps.set_capacity(4);
        assert_eq!(ps.capacity(), 4);
        assert_eq!(ps.size(), 0);
    }

    #[test]
    fn update_ages_and_moves_with_wind() {
        let mut ps = ParticleSystem::new(false, 4);
        ps.set_fixed_frame_rate(60.0);
        ps.set_wind_factor(1.0);
        ps.add_particle(
            Particle::new_sprite(
                Vector3::ZERO,
                point(1.0, 0.0, 0.0),
                Vector4::new(0.0, 0.0, 0.0, 1.0),
                1.0,
                0.0,
                0.0,
            ),
            false,
        );

        // The quantised slice: dt = age_of(2) for a 60 Hz frame.
        let dt = Particle::age_of(2);
        let mut rng = BWRandom::new(5489);
        ps.update(1.0 / 60.0, point(0.0, 2.0, 0.0), &mut rng);

        // dt was quantised to 2 age increments (1/60 * 120 = 2).
        let p = ps.particle(0);
        assert_eq!(p.age_accurate(), 2);
        // Position moved by (velocity + wind) * dt.
        assert_eq!(p.position(), point(dt, 2.0 * dt, 0.0));

        // Zero wind factor cancels the wind entirely.
        ps.set_wind_factor(0.0);
        ps.update(1.0 / 60.0, point(0.0, 100.0, 0.0), &mut rng);
        let p = ps.particle(0);
        assert_eq!(p.age_accurate(), 4);
        assert_eq!(p.position(), point(2.0 * dt, 2.0 * dt, 0.0));
    }

    #[test]
    fn a_too_small_frame_breaks_and_banks_the_leftover() {
        let mut ps = ParticleSystem::new(false, 4);
        ps.set_fixed_frame_rate(60.0);
        ps.add_particle(
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0),
            false,
        );
        let mut rng = BWRandom::new(5489);
        // 1 ms yields less than one 1/120 s increment: the loop breaks
        // before doing any work and banks the whole frame.
        ps.update(0.001, Vector3::ZERO, &mut rng);
        assert_eq!(ps.particle(0).age_accurate(), 0);
        assert!((ps.frames_left_over() - 0.001).abs() < 1e-6);
    }

    #[test]
    fn banked_leftover_flows_into_the_next_frame_clamped() {
        let mut ps = ParticleSystem::new(false, 4);
        ps.set_fixed_frame_rate(60.0);
        ps.add_particle(
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0),
            false,
        );
        let mut rng = BWRandom::new(5489);
        ps.update(0.001, Vector3::ZERO, &mut rng);
        // The next frame absorbs the leftover; the sum clamps at 1 s.
        ps.update(2.0, Vector3::ZERO, &mut rng);
        let p = ps.particle(0);
        // 1 s at 120 increments per second.
        assert_eq!(p.age_accurate(), 120);
    }

    #[test]
    fn a_frame_just_under_a_slice_counts_as_a_slice() {
        let mut ps = ParticleSystem::new(false, 4);
        ps.set_fixed_frame_rate(119.5); // dt = 1/119.5 > 1/120 by 0.000035
        ps.add_particle(
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0),
            false,
        );
        let mut rng = BWRandom::new(5489);
        ps.update(1.0 / 120.0, Vector3::ZERO, &mut rng);
        // The quantised slice overshoots dTime by less than the
        // almostEqual epsilon, so the frame still counts and the loop
        // ends with no leftover.
        assert_eq!(ps.particle(0).age_accurate(), 1);
        assert_eq!(ps.frames_left_over(), 0.0);
    }

    #[test]
    fn time_triggered_source_creates_at_the_rate() {
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(point(
                1.0, 2.0, 3.0,
            )))),
            None,
        );
        src.set_time_triggered(true);
        src.set_rate(10.0);
        ps.push_action(Box::new(src));

        let mut rng = BWRandom::new(5489);
        ps.update(1.0, Vector3::ZERO, &mut rng);

        assert_eq!(ps.size(), 10);
        // All ten sit at the point source's world position (static system,
        // zero displacement).
        for i in 0..10 {
            assert_eq!(ps.particle(i).position(), point(1.0, 2.0, 3.0));
        }
        // The aging pass runs before the actions pass, so this frame's
        // particles are still brand new.
        assert_eq!(ps.particle(0).age_accurate(), 0);
        // The next slice ages them by one increment and spawns nothing
        // (1/120 s at rate 10 is under one particle).
        ps.update(1.0 / 120.0, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 10);
        assert_eq!(ps.particle(0).age_accurate(), 1);
    }

    #[test]
    fn source_sprite_draws_replay_bit_for_bit() {
        // Replay the draw stream by hand: point position (no draws), line
        // velocity (1), size (1), pitch (1), yaw (1).
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(point(
                0.0, 0.0, 0.0,
            )))),
            Some(VectorGenerator::Line(LineVectorGenerator::new(
                point(0.0, 0.0, 0.0),
                point(4.0, 8.0, 12.0),
            ))),
        );
        src.set_time_triggered(true);
        src.set_rate(16.0);
        src.set_minimum_size(2.0);
        src.set_maximum_size(4.0);
        src.set_initial_rotation(Vector2::new(0.25, 0.5));
        src.set_random_initial_rotation(Vector2::new(1.0, 2.0));
        src.set_initial_colour(Vector4::new(1.0, 0.0, 0.0, 1.0));
        ps.push_action(Box::new(src));

        let mut replay = BWRandom::new(5489);
        let u_vel = replay.unit_rand();
        let u_size = replay.unit_rand();
        let u_pitch = replay.unit_rand();
        let u_yaw = replay.unit_rand();

        // Expected values after the same packing the particle does.
        let mut scratch =
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 0.0, 0.0, 0.0);
        scratch.set_velocity(point(u_vel * 4.0, u_vel * 8.0, u_vel * 12.0));
        let expected_velocity = scratch.velocity();
        scratch.set_size(u_size * 2.0 + 2.0);
        let expected_size = scratch.size();
        scratch.set_pitch(0.25 + 1.0 * (u_pitch - 0.5));
        let expected_pitch = scratch.pitch();
        scratch.set_yaw(0.5 + 2.0 * (u_yaw - 0.5));
        let expected_yaw = scratch.yaw();

        let mut rng = BWRandom::new(5489);
        run_slices(&mut ps, &mut rng, 8);

        assert_eq!(ps.size(), 1);
        let p = ps.particle(0);
        assert_eq!(p.velocity(), expected_velocity);
        assert_eq!(p.size(), expected_size);
        assert_eq!(p.pitch(), expected_pitch);
        assert_eq!(p.yaw(), expected_yaw);
        assert_eq!(p.colour_raw(), (255 << 24) | (255 << 16));
    }

    #[test]
    fn source_with_random_spin_draws_the_axis_after_the_yaw() {
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_time_triggered(true);
        src.set_rate(16.0);
        src.set_random_spin(true);
        src.set_min_spin(-1.0);
        src.set_max_spin(1.0);
        // Only mesh particles store the spin axis; make the system mesh
        // style so the drawn axis lands in the particle.
        ps.set_mesh_style(true);
        ps.push_action(Box::new(src));

        let mut replay = BWRandom::new(5489);
        // The draw order is size, pitch, yaw, then the spin axis — the
        // first three draws happen before the axis even though the
        // rotation spread is zero.
        let _ = replay.unit_rand(); // size
        let _ = replay.unit_rand(); // pitch
        let _ = replay.unit_rand(); // yaw
        let raw_axis = point(replay.unit_rand(), replay.unit_rand(), replay.unit_rand());
        let mut axis = raw_axis;
        axis.normalise();
        let spin = replay.unit_rand() * 2.0 - 1.0;

        let mut rng = BWRandom::new(5489);
        run_slices(&mut ps, &mut rng, 8);

        assert_eq!(ps.size(), 1);
        let p = ps.particle(0);
        // The pitch/yaw draws happen with zero spread, so both pack to
        // the zero slot.
        assert_eq!(p.pitch(), 0.0);
        assert_eq!(p.yaw(), 0.0);
        // The axis bytes hold 255 * component.
        let mesh = Particle::new_mesh(Vector3::ZERO, Vector3::ZERO, 0.0, 0.0, axis, spin);
        assert_eq!(p.mesh_spin_axis(), mesh.mesh_spin_axis());
        assert_eq!(p.mesh_spin_speed(), mesh.mesh_spin_speed());
    }

    #[test]
    fn forced_particles_arrive_in_unit_sized_chains() {
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_forced_unit_size(3);
        src.force(2);
        ps.push_action(Box::new(src));

        let mut rng = BWRandom::new(5489);
        ps.update(1.0 / 120.0, Vector3::ZERO, &mut rng);
        // 2 chains x 3 particles, spacing zero.
        assert_eq!(ps.size(), 6);

        // A second frame with nothing forced creates nothing.
        ps.update(1.0 / 120.0, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 6);
    }

    #[test]
    fn forced_and_motion_creates_clamp_at_capacity() {
        let mut ps = ParticleSystem::new(false, 5);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_forced_unit_size(10);
        src.force(2);
        ps.push_action(Box::new(src));
        let mut rng = BWRandom::new(5489);
        ps.update(1.0 / 120.0, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 5);
    }

    #[test]
    fn motion_triggered_scales_with_the_moved_distance() {
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_motion_triggered(true);
        src.set_sensitivity(0.1);
        ps.push_action(Box::new(src));

        let mut rng = BWRandom::new(5489);
        // First frame: firstUpdate zeroes the displacement, so no spawn.
        ps.set_object_to_world(translation(0.0, 0.0, 0.0));
        ps.update(0.5, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 0);

        // Second frame: moved 1 m in 0.5 s = 2 m/s; with sensitivity 0.1,
        // frameTotal = int((1 + 2*(2-0.1)/0.1) * 0.5) = int(19.5) = 19.
        ps.set_object_to_world(translation(1.0, 0.0, 0.0));
        ps.update(0.5, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 19);
    }

    /// A probe that never finds the ground.
    struct MissingGround;
    impl GroundProbe for MissingGround {
        fn ground(&mut self, _position: &Vector3, _distance: f32) -> f32 {
            NO_GROUND_COLLISION
        }
    }

    #[test]
    fn grounded_particles_drop_to_the_probe_or_vanish() {
        // A probe that misses returns NO_GROUND_COLLISION: the whole
        // per-particle draw block is skipped and nothing is created.
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(point(
                0.0, 30.0, 0.0,
            )))),
            None,
        );
        src.set_time_triggered(true);
        src.set_rate(16.0);
        src.set_grounded(true);
        src.set_drop_distance(5.0);
        src.set_ground(Box::new(MissingGround));
        ps.push_action(Box::new(src));
        let mut rng = BWRandom::new(5489);
        run_slices(&mut ps, &mut rng, 8);
        assert_eq!(ps.size(), 0);

        // With a probe that hits: created and snapped onto the ground
        // plane.
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(point(
                0.0, 30.0, 0.0,
            )))),
            None,
        );
        src.set_time_triggered(true);
        src.set_rate(16.0);
        src.set_grounded(true);
        src.set_ground(Box::new(FlatGround { height: 5.0 }));
        ps.push_action(Box::new(src));
        let mut rng = BWRandom::new(5489);
        run_slices(&mut ps, &mut rng, 8);
        assert_eq!(ps.size(), 1);
        assert_eq!(ps.particle(0).position().y, 5.0);
    }

    #[test]
    fn sleep_period_gate_accumulates_only_active_time() {
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_time_triggered(true);
        src.set_rate(16.0);
        // Fixed sleep: max == period means the generated sleep is exactly
        // one second; the cycle is 1 s awake, 1 s asleep. The load-time
        // draw must be replayed before the first frame.
        src.set_sleep_period(1.0);
        src.set_sleep_period_max(1.0);
        let mut rng = BWRandom::new(5489);
        src.arm_sleep_cycle(&mut rng);
        ps.push_action(Box::new(src));

        // Each frame is one exact 0.5 s slice (dt = age_of(60)).
        let half_second = |ps: &mut ParticleSystem, rng: &mut BWRandom| {
            ps.update(0.5, Vector3::ZERO, rng);
        };

        // First half second: awake, 8 particles (0.5 s at rate 16).
        half_second(&mut ps, &mut rng);
        assert_eq!(ps.size(), 8);
        // Second: periodTime 1.0 is still <= the active period, 8 more.
        half_second(&mut ps, &mut rng);
        assert_eq!(ps.size(), 16);
        // Third: periodTime 1.5 is past the awake half; the awake
        // remainder since lastPeriodTime 1.0 is zero, nothing spawns.
        half_second(&mut ps, &mut rng);
        assert_eq!(ps.size(), 16);
        // Fourth: periodTime reaches exactly the period; the strict
        // comparison keeps the system asleep, still 16.
        half_second(&mut ps, &mut rng);
        assert_eq!(ps.size(), 16);
        // Fifth: the cycle wraps, the sleep redraws, the awake half
        // banks 0.5 s again: 8 more.
        half_second(&mut ps, &mut rng);
        assert_eq!(ps.size(), 24);
    }

    #[test]
    fn delay_gates_the_source_but_banks_age() {
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_time_triggered(true);
        src.set_rate(16.0);
        src.set_delay(0.75);
        ps.push_action(Box::new(src));

        let mut rng = BWRandom::new(5489);
        // While below the delay the frame time only ages the action.
        ps.update(0.5, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 0);
        ps.update(0.5, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 0);
        // Age 1.0 passed the 0.75 delay: the frame's full rate budget
        // flows immediately (0.5 s at rate 16).
        ps.update(0.5, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 8);
    }

    #[test]
    fn queued_particles_decrement_without_forcing_production() {
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_time_triggered(true);
        src.set_rate(16.0);
        src.create(3);
        ps.push_action(Box::new(src));

        let mut rng = BWRandom::new(5489);
        // The queue only feeds the entry gate and is decremented by what
        // the rate produces; it never adds to the produced count itself.
        run_slices(&mut ps, &mut rng, 8);
        assert_eq!(ps.size(), 1);
        run_slices(&mut ps, &mut rng, 8);
        assert_eq!(ps.size(), 2);
    }

    #[test]
    fn disabled_and_zero_delta_time_frames_do_nothing() {
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_time_triggered(true);
        src.set_rate(1000.0);
        src.set_enabled(false);
        ps.push_action(Box::new(src));
        let mut rng = BWRandom::new(5489);
        ps.update(1.0 / 120.0, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 0);

        // A zero delta frame skips the action entirely.
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_time_triggered(true);
        src.set_rate(1000.0);
        ps.push_action(Box::new(src));
        let mut rng = BWRandom::new(5489);
        ps.update(0.0, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 0);

        // A time-triggered source with no rate produces nothing even with
        // queued demand: the rate gate never opens.
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(None, None);
        src.set_time_triggered(true);
        src.create(5);
        ps.push_action(Box::new(src));
        let mut rng = BWRandom::new(5489);
        ps.update(0.5, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 0);
    }

    #[test]
    fn jitter_moves_alive_particles_only() {
        let mut ps = ParticleSystem::new(false, 10);
        let mut jitter = JitterPSA::new(
            Some(VectorGenerator::Box(BoxVectorGenerator::new(
                point(1.0, 0.0, 0.0),
                point(1.0, 0.0, 0.0),
            ))),
            Some(VectorGenerator::Cylinder(CylinderVectorGenerator::new(
                Vector3::ZERO,
                point(0.0, 2.0, 0.0),
                0.0,
                0.0,
            ))),
        );
        jitter.set_affect_position(true);
        jitter.set_affect_velocity(true);
        ps.push_action(Box::new(jitter));

        ps.add_particle(
            Particle::new_sprite(
                point(0.0, 0.0, 0.0),
                Vector3::ZERO,
                Vector4::ZERO,
                1.0,
                0.0,
                0.0,
            ),
            false,
        );
        ps.add_particle(
            Particle::new_sprite(
                point(9.0, 9.0, 9.0),
                Vector3::ZERO,
                Vector4::ZERO,
                1.0,
                0.0,
                0.0,
            ),
            false,
        );
        // The second particle dies before the jitter frame.
        ps.remove_particle(1);

        let dt = 0.5;
        // Hand-replay the draw order: the collapsed box still draws one
        // unit rand per axis (always x = 1, y = 0, z = 0), then the
        // cylinder's axis and angle draws for the velocity jitter (zero
        // radius, so the disc term vanishes and no third draw happens).
        let mut replay = BWRandom::new(5489);
        let _ = replay.unit_rand(); // box x
        let _ = replay.unit_rand(); // box y
        let _ = replay.unit_rand(); // box z
        let u_vel = replay.unit_rand();
        let _ = replay.unit_rand(); // cylinder angle
        // The velocity is packed like the reference before it moves.
        let mut scratch =
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0);
        scratch.set_velocity(point(0.0, 2.0 * u_vel, 0.0));
        let velocity_y = scratch.velocity().y;

        let mut rng = BWRandom::new(5489);
        ps.update(dt, Vector3::ZERO, &mut rng);

        // Position jitter moved x by 1 * dt; the movement pass then added
        // dt * the packed velocity to every alive particle.
        let p = ps.particle(0);
        assert_eq!(p.position(), point(0.5, velocity_y * dt, 0.0));
        assert_eq!(p.velocity(), point(0.0, velocity_y, 0.0));
    }

    #[test]
    fn jitter_with_no_flags_is_a_no_op() {
        let mut ps = ParticleSystem::new(false, 10);
        let jitter = JitterPSA::new(
            Some(VectorGenerator::Box(BoxVectorGenerator::new(
                point(1.0, 1.0, 1.0),
                point(2.0, 2.0, 2.0),
            ))),
            Some(VectorGenerator::Cylinder(CylinderVectorGenerator::new(
                Vector3::ZERO,
                point(0.0, 2.0, 0.0),
                1.0,
                0.0,
            ))),
        );
        // affectPosition_/affectVelocity_ default to false in the
        // reference constructor.
        ps.push_action(Box::new(jitter));

        ps.add_particle(
            Particle::new_sprite(
                point(3.0, 4.0, 5.0),
                Vector3::ZERO,
                Vector4::ZERO,
                1.0,
                0.0,
                0.0,
            ),
            false,
        );

        let mut rng = BWRandom::new(5489);
        ps.update(0.5, Vector3::ZERO, &mut rng);
        // Nothing moved and no draws were consumed.
        assert_eq!(ps.particle(0).position(), point(3.0, 4.0, 5.0));
        let mut pristine = BWRandom::new(5489);
        assert_eq!(rng.next(), pristine.next());
    }

    #[test]
    fn sphere_jitter_and_delay_gate() {
        let mut ps = ParticleSystem::new(false, 10);
        let mut rng = BWRandom::new(5489);
        let mut jitter = JitterPSA::new(
            Some(VectorGenerator::Sphere(SphereVectorGenerator::new(
                Vector3::ZERO,
                1.0,
                0.0,
                &mut rng,
            ))),
            None,
        );
        jitter.set_affect_position(true);
        jitter.set_delay(10.0);
        ps.push_action(Box::new(jitter));

        ps.add_particle(
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0),
            false,
        );
        // Below the delay the jitter never touches the particle.
        ps.update(1.0, Vector3::ZERO, &mut rng);
        assert_eq!(ps.particle(0).position(), Vector3::ZERO);
    }

    #[test]
    fn source_velocity_inherits_the_emitter_motion() {
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_time_triggered(true);
        src.set_rate(16.0);
        src.set_inherit_velocity(0.5);
        ps.push_action(Box::new(src));

        let mut rng = BWRandom::new(5489);
        // Frame 1 primes the baseline (firstUpdate zeroes displacement):
        // 8 particles with zero inherited velocity.
        ps.set_object_to_world(translation(0.0, 0.0, 0.0));
        ps.update(0.5, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 8);
        // Frame 2: the emitter moved 1 m in 0.5 s = 2 m/s; the second
        // batch inherits 0.5 * (2, 0, 0).
        ps.set_object_to_world(translation(1.0, 0.0, 0.0));
        ps.update(0.5, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 16);
        assert_eq!(ps.particle(15).velocity(), point(1.0, 0.0, 0.0));
        // The first batch keeps its zero velocity.
        assert_eq!(ps.particle(0).velocity(), Vector3::ZERO);
    }

    #[test]
    fn ignore_rotation_strips_the_emitter_rotation() {
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(point(
                0.0, 1.0, 0.0,
            )))),
            None,
        );
        src.set_time_triggered(true);
        src.set_rate(1000.0);
        src.set_ignore_rotation(true);
        ps.push_action(Box::new(src));

        let mut rng = BWRandom::new(5489);
        ps.update(1.0 / 120.0, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 8);
        for i in 0..8 {
            assert_eq!(ps.particle(i).position(), point(0.0, 1.0, 0.0));
        }
    }

    #[test]
    fn mesh_style_takes_the_emitter_forward_as_base_orientation() {
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_time_triggered(true);
        src.set_rate(16.0);
        src.set_initial_rotation(Vector2::new(1.0, 1.0));
        ps.set_mesh_style(true);
        ps.push_action(Box::new(src));

        let mut rng = BWRandom::new(5489);
        run_slices(&mut ps, &mut rng, 8);
        assert_eq!(ps.size(), 1);
        assert!(ps.particle(0).is_mesh());
        // Zero rotation spread still consumes the pitch/yaw draws; the
        // identity transform's forward is +z, whose pitch and yaw are
        // both zero, so the base orientation is exactly the configured
        // initial rotation. Pitch and yaw are byte-quantised.
        let step = std::f32::consts::TAU / 256.0;
        let p = ps.particle(0);
        assert!((p.pitch() - 1.0).abs() <= step, "pitch {}", p.pitch());
        assert!((p.yaw() - 1.0).abs() <= step, "yaw {}", p.yaw());

        // Sanity: the wiring really is initial + forward's pitch/yaw.
        let forward = foundation::math::matrix::Matrix::identity().apply_to_unit_axis_vector(2);
        assert_eq!(forward.pitch(), 0.0);
        assert_eq!(forward.yaw(), 0.0);
    }

    #[test]
    fn max_speed_caps_time_triggered_spawns() {
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_time_triggered(true);
        src.set_rate(16.0);
        src.set_max_speed(1.0);
        ps.push_action(Box::new(src));

        let mut rng = BWRandom::new(5489);
        // Frame 1 primes the baseline and spawns 8 normally.
        ps.update(0.5, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 8);
        // Frame 2: the emitter teleports 10 m in 0.5 s = 20 m/s, past the
        // 1 m/s cap. The rate budget is consumed but every particle is
        // dropped.
        ps.set_object_to_world(translation(10.0, 0.0, 0.0));
        ps.update(0.5, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 8);
    }

    #[test]
    fn allowed_time_budget_stops_creation_mid_chain() {
        // The budget is CPU time per frame: execute stamps the clock at
        // its head and the creation loop re-reads it every iteration. The
        // port models the CPU cost with a per-particle charge.
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_time_triggered(true);
        src.set_rate(16.0);
        src.force(100);
        src.set_allowed_time(0.5);
        src.set_per_particle_cost(0.1);
        ps.push_action(Box::new(src));

        let mut rng = BWRandom::new(5489);
        // The forced chain wants 100 particles; after five the elapsed
        // clock hits 0.5 s, which is not < 0.5, and the chain stops
        // mid-way. The rate batch of the same frame is then refused too.
        ps.update(0.5, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 5);
        // A fresh frame re-stamps the clock, so creation resumes.
        run_slices(&mut ps, &mut rng, 8);
        assert_eq!(ps.size(), 6);
    }

    #[test]
    fn motion_and_time_share_one_frame_total_per_frame() {
        // The reference declares frameTotal once before the motion block
        // and never resets it, so a motion-triggered spawn is re-spawned
        // by the time block when the rate accumulator has not crossed a
        // particle. Preserved verbatim.
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_motion_triggered(true);
        src.set_time_triggered(true);
        src.set_rate(16.0);
        ps.push_action(Box::new(src));

        let mut rng = BWRandom::new(5489);
        // Frame 1 primes the baseline: no displacement, and 1/120 s is
        // under one rate particle, so nothing spawns.
        ps.set_object_to_world(translation(0.0, 0.0, 0.0));
        run_slices(&mut ps, &mut rng, 1);
        assert_eq!(ps.size(), 0);

        // Frame 2: 1 m in 1/120 s = 120 m/s; motion wants
        // int((1 + 2*((120-0.1)/0.1)) * 1/120) = 19 particles, and the
        // time block reuses that 19 without spending rate budget.
        ps.set_object_to_world(translation(1.0, 0.0, 0.0));
        run_slices(&mut ps, &mut rng, 1);
        assert_eq!(ps.size(), 38);
    }

    #[test]
    fn arming_the_first_update_zeroes_the_next_displacement() {
        let mut ps = ParticleSystem::new(false, 100);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_motion_triggered(true);
        src.set_sensitivity(0.1);
        ps.push_action(Box::new(src));

        let mut rng = BWRandom::new(5489);
        // Frame 1 primes the baseline.
        ps.set_object_to_world(translation(0.0, 0.0, 0.0));
        ps.update(0.5, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 0);
        // Frame 2: 1 m in 0.5 s = 2 m/s, 19 motion particles.
        ps.set_object_to_world(translation(1.0, 0.0, 0.0));
        ps.update(0.5, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 19);
        // Frame 3 with the first-update armed again: the source re-primes
        // its displacement baseline, so the further 2 m jump spawns
        // nothing.
        ps.arm_first_update();
        ps.set_object_to_world(translation(3.0, 0.0, 0.0));
        ps.update(0.5, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 19);
    }

    #[test]
    fn cross_variant_accessors_read_zero_and_ignore_writes() {
        // Mesh particles have no size: reading and writing it is inert.
        let mut mesh =
            Particle::new_mesh(Vector3::ZERO, Vector3::ZERO, 0.0, 0.0, Vector3::ZERO, 0.0);
        assert_eq!(mesh.size(), 0.0);
        mesh.set_size(5.0);
        assert_eq!(mesh.size_raw(), 0);
        // The colour lane is sprite-only too.
        mesh.set_colour_raw(0xff00_ff00);
        assert_eq!(mesh.colour_raw(), 0);

        // Sprite particles carry no spin state.
        let mut sprite =
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0);
        assert_eq!(sprite.mesh_spin_axis(), Vector3::ZERO);
        sprite.set_mesh_spin_axis(point(1.0, 1.0, 1.0));
        assert_eq!(sprite.mesh_spin_axis(), Vector3::ZERO);
        assert_eq!(sprite.mesh_spin_speed(), 0.0);
        sprite.set_mesh_spin_speed(1.0);
        assert_eq!(sprite.mesh_spin_speed(), 0.0);
        assert_eq!(sprite.mesh_spin_age(), 0.0);
        sprite.set_mesh_spin_age(1.0);
        assert_eq!(sprite.mesh_spin_age(), 0.0);
    }

    #[test]
    fn a_contiguous_capacity_change_clears_and_reserves() {
        let mut ps = ParticleSystem::new(true, 4);
        // The same capacity: no clear, no reserve.
        ps.set_capacity(4);
        ps.add_particle(
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0),
            false,
        );
        assert_eq!(ps.size(), 1);
        // A change wipes the population and resizes the store.
        ps.set_capacity(7);
        assert_eq!(ps.capacity(), 7);
        assert_eq!(ps.size(), 0);
        // Stable indices restart after the reserve.
        ps.add_particle(
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0),
            false,
        );
        assert_eq!(ps.particle(0).index(), 0);
        // The aging pass walks the contiguous store in place.
        let mut rng = BWRandom::new(5489);
        ps.set_fixed_frame_rate(120.0);
        ps.update(1.0 / 120.0, Vector3::ZERO, &mut rng);
        assert_eq!(ps.particle(0).age_accurate(), 1);
    }

    #[test]
    fn both_container_layouts_refuse_particles_when_full() {
        // The container-level gate, below the system-level one.
        let mut contiguous = Container::contiguous(1);
        let p = Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0);
        assert!(contiguous.add_particle(p, false));
        assert!(!contiguous.add_particle(p, false));

        let mut fixed = Container::fixed_index(1);
        assert!(fixed.add_particle(p, false));
        assert!(!fixed.add_particle(p, false));
    }

    #[test]
    fn the_time_block_clamps_at_capacity() {
        let mut ps = ParticleSystem::new(false, 10);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_time_triggered(true);
        src.set_rate(1000.0);
        ps.push_action(Box::new(src));
        let mut rng = BWRandom::new(5489);
        // Half a second at rate 1000 wants 500; capacity holds it to 10.
        ps.update(0.5, Vector3::ZERO, &mut rng);
        assert_eq!(ps.size(), 10);
    }

    #[test]
    fn the_motion_block_clamps_at_capacity() {
        let mut ps = ParticleSystem::new(false, 5);
        let mut src = SourcePSA::new(
            Some(VectorGenerator::Point(PointVectorGenerator::new(
                Vector3::ZERO,
            ))),
            None,
        );
        src.set_motion_triggered(true);
        src.set_sensitivity(0.1);
        ps.push_action(Box::new(src));
        let mut rng = BWRandom::new(5489);
        // Frame 1 primes the baseline.
        ps.set_object_to_world(translation(0.0, 0.0, 0.0));
        run_slices(&mut ps, &mut rng, 1);
        // Frame 2: 100 m in 1/120 s = 12000 m/s wants thousands; the
        // motion block's own clamp holds five.
        ps.set_object_to_world(translation(100.0, 0.0, 0.0));
        run_slices(&mut ps, &mut rng, 1);
        assert_eq!(ps.size(), 5);
    }

    #[test]
    fn sleep_fallbacks_pin_the_single_bound_and_free_running_cases() {
        // Each configuration runs the same five half-second frames; the
        // generated sleep period decides whether frames three and four
        // are gated.
        let cycle = |sleep_period: f32, sleep_period_max: f32| {
            let mut ps = ParticleSystem::new(false, 100);
            let mut src = SourcePSA::new(
                Some(VectorGenerator::Point(PointVectorGenerator::new(
                    Vector3::ZERO,
                ))),
                None,
            );
            src.set_time_triggered(true);
            src.set_rate(16.0);
            src.set_active_period(1.0);
            src.set_sleep_period(sleep_period);
            src.set_sleep_period_max(sleep_period_max);
            let mut rng = BWRandom::new(5489);
            src.arm_sleep_cycle(&mut rng);
            ps.push_action(Box::new(src));
            let mut sizes = Vec::new();
            for _ in 0..5 {
                ps.update(0.5, Vector3::ZERO, &mut rng);
                sizes.push(ps.size());
            }
            sizes
        };
        // Only a minimum, or only a maximum: the bound applies verbatim,
        // the same one-second sleep cycle as the fixed case.
        assert_eq!(cycle(1.0, -1.0), vec![8, 16, 16, 16, 24]);
        assert_eq!(cycle(-1.0, 1.0), vec![8, 16, 16, 16, 24]);
        // Neither bound: no sleep is generated and the system free-runs.
        assert_eq!(cycle(-1.0, -1.0), vec![8, 16, 24, 32, 40]);
    }

    #[test]
    fn a_source_without_generators_spawns_at_the_origin() {
        let mut ps = ParticleSystem::new(false, 10);
        let mut src = SourcePSA::new(None, None);
        src.set_time_triggered(true);
        src.set_rate(16.0);
        ps.push_action(Box::new(src));
        let mut rng = BWRandom::new(5489);
        run_slices(&mut ps, &mut rng, 8);
        // No position or velocity source: both local-space samples stay
        // at the origin.
        assert_eq!(ps.size(), 1);
        assert_eq!(ps.particle(0).position(), Vector3::ZERO);
        assert_eq!(ps.particle(0).velocity(), Vector3::ZERO);
        // The size and rotation draws still ran with zero spread; the
        // default min/max size of one survives the u16 round trip.
        assert!((ps.particle(0).size() - 1.0).abs() < 0.01);
        assert_eq!(ps.particle(0).pitch(), 0.0);
        assert_eq!(ps.particle(0).yaw(), 0.0);
    }

    #[test]
    fn arming_first_update_reaches_every_action() {
        // JitterPSA's setFirstUpdate is an explicit no-op; the armed pass
        // must still reach it without disturbing the particle.
        let mut ps = ParticleSystem::new(false, 10);
        ps.push_action(Box::new(JitterPSA::new(None, None)));
        ps.add_particle(
            Particle::new_sprite(
                point(2.0, 3.0, 4.0),
                Vector3::ZERO,
                Vector4::ZERO,
                1.0,
                0.0,
                0.0,
            ),
            false,
        );
        ps.arm_first_update();
        let mut rng = BWRandom::new(5489);
        ps.update(0.5, Vector3::ZERO, &mut rng);
        assert_eq!(ps.particle(0).position(), point(2.0, 3.0, 4.0));
    }

    #[test]
    fn a_frame_under_one_increment_breaks_before_any_work() {
        // Without a fixed rate the whole frame is one slice; 1 ms is under
        // the 1/120 s age increment, so the loop breaks at once.
        let mut ps = ParticleSystem::new(false, 4);
        ps.add_particle(
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0),
            false,
        );
        let mut rng = BWRandom::new(5489);
        ps.update(0.001, Vector3::ZERO, &mut rng);
        assert_eq!(ps.particle(0).age_accurate(), 0);
        assert!((ps.frames_left_over() - 0.001).abs() < 1e-6);
    }

    #[test]
    fn a_slice_fractionally_over_the_frame_takes_the_frame_time() {
        // 30 Hz: dt 1/30 quantises to four age increments whose exact
        // overshoot of the 0.0333 frame is inside the tolerance, so the
        // slice is the frame time and no leftover is banked.
        let mut ps = ParticleSystem::new(false, 4);
        ps.set_fixed_frame_rate(30.0);
        ps.add_particle(
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0),
            false,
        );
        let mut rng = BWRandom::new(5489);
        ps.update(0.0333, Vector3::ZERO, &mut rng);
        assert_eq!(ps.particle(0).age_accurate(), 4);
        assert_eq!(ps.frames_left_over(), 0.0);
    }

    #[test]
    fn the_aging_pass_clamps_at_max_age() {
        let mut ps = ParticleSystem::new(false, 4);
        ps.add_particle(
            Particle::new_sprite(Vector3::ZERO, Vector3::ZERO, Vector4::ZERO, 1.0, 0.0, 0.0),
            false,
        );
        // Ten increments past the sentinel boundary: the sum clamps at
        // ageMax instead of wrapping.
        for p in ps.container.items_mut() {
            p.set_age_accurate(65530);
        }
        let mut rng = BWRandom::new(5489);
        ps.set_fixed_frame_rate(120.0);
        ps.update(1.0 / 12.0, Vector3::ZERO, &mut rng);
        assert_eq!(ps.particle(0).age_accurate(), Particle::age_max());
        // A maximally aged particle reads as dead.
        assert!(!ps.particle(0).is_alive());
    }
}
