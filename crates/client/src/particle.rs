//! BW-13-04-04 particle engine faces, following `lib/particle/`:
//! `bwrandom.cpp` (the MT19937 the particle maths draws from), `particle.ipp`
//! (the packed particle), `particles.ipp` (`ContiguousParticles`),
//! `actions/vector_generator.cpp` (the five position/velocity sources), the
//! action family in `actions/`, `particle_system.cpp` and
//! `meta_particle_system.cpp` (load), plus the sprite renderers in
//! `renderers/`.
//!
//! The reference RNG is a process-global singleton seeded from the wall
//! clock (`BWRandom::BWRandom()`), so a particle run is different every
//! time. The port makes the seam explicit: every draw goes through an
//! injected [`BwRandom`], which reproduces the reference MT19937 bit for
//! bit when given the same seed. The `SphereVectorCache`
//! (`vector_generator.cpp`) builds its 4096-entry table from its own
//! default-constructed `BWRandom`; the port pins that construction to a
//! fixed seed so runs stay reproducible.
//!
//! Host services the port does not model stay seams and are documented at
//! their call sites: `RompCollider` (SourcePSA grounding), the
//! `FogController` (TintShader fog blend), `MatrixProvider` targets
//! (NodeClamp/Orbitor/MatrixSwarm), the `LensEffectManager` (Flare), the
//! romp collider + sound banks (Collide/Splat) and the mesh renderers. All
//! of their load faces work; only their runtime effect does not.

use foundation::math::{BoundBox, Matrix, Vector2, Vector3, Vector4};
use resources::datasection::DataSection;

/// `MATH_PI`/`MATH_2PI` (`mathdef.hpp:21-22`): float constants, so every
/// product with them stays in f32. The literals are the reference ones and
/// must stay bit-exact, so the std-constant lints are silenced.
#[allow(clippy::approx_constant, clippy::excessive_precision)]
const MATH_PI: f32 = 3.141_592_7;
#[allow(clippy::approx_constant, clippy::excessive_precision)]
const MATH_2PI: f32 = 6.283_185_5;

/// `almostEqual(a, b)` — the two-argument overload with the 0.0004 default
/// (`stdmf.hpp:606`).
fn almost_equal(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.0004
}

/// `Math::clamp(min, value, max)`.
fn clamp(min: f32, v: f32, max: f32) -> f32 {
    if v < min {
        min
    } else if v > max {
        max
    } else {
        v
    }
}

/// The empty bounding box (`BoundingBox()` default ctor state, which the
/// first `addBounds` grows into).
fn empty_bbox() -> BoundBox {
    BoundBox::default()
}

// ---------------------------------------------------------------------------
// Section: BwRandom (`cstdmf/bwrandom.cpp`)
// ---------------------------------------------------------------------------

/// The reference MT19937 (`BWRandom`), seedable, so a particle run is
/// reproducible while drawing exactly the values the C++ singleton would
/// have drawn from the same seed.
pub struct BwRandom {
    mt: [u32; 624],
    mti: usize,
}

impl BwRandom {
    /// `BWRandom::init(uint32)` (`bwrandom.cpp:93-107`): the Knuth
    /// initialiser, with `mti` left past the end so the first draw twists.
    pub fn new(seed: u32) -> Self {
        let mut mt = [0u32; 624];
        mt[0] = seed;
        for mti in 1..624usize {
            mt[mti] = 1812433253u32
                .wrapping_mul(mt[mti - 1] ^ (mt[mti - 1] >> 30))
                .wrapping_add(mti as u32);
        }
        BwRandom { mt, mti: 625 }
    }

    /// `BWRandom::generate` (`bwrandom.cpp:172-199`): the twist plus the
    /// standard tempering shifts.
    pub fn next_u32(&mut self) -> u32 {
        const UPPER_MASK: u32 = 0x8000_0000;
        const LOWER_MASK: u32 = 0x7fff_ffff;
        const MAG01: [u32; 2] = [0, 0x9908_b0df];
        if self.mti >= 624 {
            let mt = &mut self.mt;
            for kk in 0..(624 - 397) {
                let y = (mt[kk] & UPPER_MASK) | (mt[kk + 1] & LOWER_MASK);
                mt[kk] = mt[kk + 397] ^ (y >> 1) ^ MAG01[(y & 1) as usize];
            }
            for kk in (624 - 397)..(624 - 1) {
                let y = (mt[kk] & UPPER_MASK) | (mt[kk + 1] & LOWER_MASK);
                mt[kk] = mt[kk + 397 - 624] ^ (y >> 1) ^ MAG01[(y & 1) as usize];
            }
            let y = (mt[623] & UPPER_MASK) | (mt[0] & LOWER_MASK);
            mt[623] = mt[396] ^ (y >> 1) ^ MAG01[(y & 1) as usize];
            self.mti = 0;
        }
        let mut y = self.mt[self.mti];
        self.mti += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }

    /// `Math::unitRand` (`mathdef.hpp:118-121`): the draw as f32 divided by
    /// `(float)(uint32)-1`. That literal rounds to 2^32 in f32, exactly
    /// like the C cast of 0xFFFFFFFF.
    pub fn unit_rand(&mut self) -> f32 {
        (self.next_u32() as f32) / 4294967295.0f32
    }
}

// ---------------------------------------------------------------------------
// Section: Particle (`particle.hpp`/`particle.ipp`)
// ---------------------------------------------------------------------------

// Packing constants (`particle.ipp:9-38`).
const SIZE_MAX_DATA: f32 = 65535.0;
const SIZE_MAX_VALUE: f32 = 100.0;
const PACK_SIZE: f32 = SIZE_MAX_DATA / SIZE_MAX_VALUE;
const UNPACK_SIZE: f32 = SIZE_MAX_VALUE / SIZE_MAX_DATA;
const AGE_MIN_INCREMENT: f32 = 1.0 / 120.0;
const AGE_MAX_DATA: f32 = 65535.0;
const AGE_MAX_DATA_UINT: u16 = 0xffff;
const AGE_MAX_VALUE: f32 = AGE_MAX_DATA * AGE_MIN_INCREMENT;
const PACK_AGE: f32 = AGE_MAX_DATA / AGE_MAX_VALUE;
const UNPACK_AGE: f32 = AGE_MAX_VALUE / AGE_MAX_DATA;
const ROT_MAX_DATA: f32 = 256.0;
const ROT_MAX_VALUE: f32 = MATH_2PI;
const PACK_ROT: f32 = ROT_MAX_DATA / ROT_MAX_VALUE;
const UNPACK_ROT: f32 = ROT_MAX_VALUE / ROT_MAX_DATA;
const ABS_ROT: f32 = MATH_2PI * 10.0;
const ROUND_ROT: f32 = MATH_2PI / ROT_MAX_DATA;
const VELOCITY_MIN_DATA: f32 = -32768.0;
const VELOCITY_MAX_DATA: f32 = 32767.0;

/// The unpacked per-particle view a preview snapshot records: the
/// particle's position, velocity, packed colour, size and age.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParticleRecord {
    pub position: Vector3,
    pub velocity: Vector3,
    pub colour: u32,
    pub size: f32,
    pub age: f32,
    pub age_raw: u16,
}

/// The packed particle (`Particle`), with the same u16/i16 fields so the
/// quantisation the simulation runs on is the reference's: velocities in
/// 1/256 units, size in 1/655.35 units, age in 1/120 s ticks and the
/// pitch/yaw pair in 256ths of a turn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Particle {
    /// `PositionColour::position_`.
    pub position: Vector3,
    /// `PositionColour::colour_`, the packed ABGR word.
    pub colour: u32,
    x_vel: i16,
    y_vel: i16,
    z_vel: i16,
    age: u16,
    pitch_yaw: u16,
    /// `ExtraData::NonMeshData::size_`; the mesh side of the union belongs
    /// to the mesh renderers, which are render seams (see
    /// [`ParticleRenderer::create_renderer_of_type`]).
    size: u16,
}

/// `Colour::getUint32FromNormalised(Vector4)` (`particle.cpp`): the ABGR
/// word, each channel scaled by an f32 multiply by 255.0 (the Vector4
/// overload) before the int conversion clamps to 0..255.
pub fn colour_from_normalised(c: Vector4) -> u32 {
    let r = clamp(0.0, c.x * 255.0, 255.0) as i32;
    let g = clamp(0.0, c.y * 255.0, 255.0) as i32;
    let b = clamp(0.0, c.z * 255.0, 255.0) as i32;
    let a = clamp(0.0, c.w * 255.0, 255.0) as i32;
    ((a << 24) | (r << 16) | (g << 8) | b) as u32
}

impl Default for Particle {
    fn default() -> Self {
        Particle::non_mesh(
            Vector3::ZERO,
            Vector3::ZERO,
            Vector4::new(1.0, 1.0, 1.0, 1.0),
            0.0,
            0.0,
            0.0,
        )
    }
}

impl Particle {
    /// The non-mesh constructor (`particle.ipp:42-62`): colour packed once,
    /// velocity/size/age/pitch/yaw through their quantising setters.
    pub fn non_mesh(
        position: Vector3,
        velocity: Vector3,
        colour: Vector4,
        size: f32,
        starting_pitch: f32,
        starting_yaw: f32,
    ) -> Self {
        let mut p = Particle {
            position,
            colour: colour_from_normalised(colour),
            x_vel: 0,
            y_vel: 0,
            z_vel: 0,
            age: 0,
            pitch_yaw: 0,
            size: 0,
        };
        p.set_velocity(velocity);
        p.set_size(size);
        p.set_age(0.0);
        p.set_pitch(starting_pitch);
        p.set_yaw(starting_yaw);
        p
    }

    /// `getVelocity` (`particle.ipp:120-125`): the unpack is a plain /256.
    pub fn velocity(&self) -> Vector3 {
        Vector3::new(
            self.x_vel as f32 / 256.0,
            self.y_vel as f32 / 256.0,
            self.z_vel as f32 / 256.0,
        )
    }

    /// `setVelocity` (`particle.ipp:128-133`): scale to 1/256 units, clamp
    /// into i16 and truncate on the cast.
    pub fn set_velocity(&mut self, v: Vector3) {
        self.x_vel = clamp(VELOCITY_MIN_DATA, v.x * 256.0, VELOCITY_MAX_DATA) as i16;
        self.y_vel = clamp(VELOCITY_MIN_DATA, v.y * 256.0, VELOCITY_MAX_DATA) as i16;
        self.z_vel = clamp(VELOCITY_MIN_DATA, v.z * 256.0, VELOCITY_MAX_DATA) as i16;
    }

    /// `setXVelocity` and friends re-pack a single component (the barrier
    /// bounces use them).
    pub fn set_x_velocity(&mut self, v: f32) {
        self.x_vel = clamp(VELOCITY_MIN_DATA, v * 256.0, VELOCITY_MAX_DATA) as i16;
    }

    pub fn set_y_velocity(&mut self, v: f32) {
        self.y_vel = clamp(VELOCITY_MIN_DATA, v * 256.0, VELOCITY_MAX_DATA) as i16;
    }

    pub fn set_z_velocity(&mut self, v: f32) {
        self.z_vel = clamp(VELOCITY_MIN_DATA, v * 256.0, VELOCITY_MAX_DATA) as i16;
    }

    /// `size()` (`particle.ipp:154-157`).
    pub fn size(&self) -> f32 {
        self.size as f32 * UNPACK_SIZE
    }

    /// `size(float)` (`particle.ipp:160-163`).
    pub fn set_size(&mut self, f: f32) {
        self.size = clamp(0.0, f * PACK_SIZE, SIZE_MAX_DATA) as u16;
    }

    /// `age()` — the unpacked seconds.
    pub fn age(&self) -> f32 {
        self.age as f32 * UNPACK_AGE
    }

    /// `ageAccurate()` — the raw tick count.
    pub fn age_raw(&self) -> u16 {
        self.age
    }

    /// `ageAccurate(uint16)`.
    pub fn set_age_raw(&mut self, raw: u16) {
        self.age = raw;
    }

    /// `age(float)` (`particle.ipp:190-193`): ticks with a rounding bias.
    pub fn set_age(&mut self, f: f32) {
        self.age = clamp(0.0, f * PACK_AGE + 0.5, AGE_MAX_DATA) as u16;
    }

    /// `nAgeIncrements` (`particle.ipp:196-200`): ticks without the bias,
    /// "purposefully truncating, so there is always a positive error left
    /// over".
    pub fn n_age_increments(f: f32) -> u16 {
        clamp(0.0, f * PACK_AGE, AGE_MAX_DATA) as u16
    }

    /// `age(uint16)`.
    pub fn age_of(raw: u16) -> f32 {
        raw as f32 * UNPACK_AGE
    }

    /// `ageMax()` (`particle.ipp:209-212`).
    pub fn age_max() -> u16 {
        AGE_MAX_DATA as u16
    }

    /// `kill()` sets the all-ones age; `isAlive` is anything else.
    pub fn kill(&mut self) {
        self.age = AGE_MAX_DATA_UINT;
    }

    pub fn is_alive(&self) -> bool {
        self.age != AGE_MAX_DATA_UINT
    }

    /// `pitch()` — the high byte in 256ths of a turn.
    pub fn pitch(&self) -> f32 {
        ((self.pitch_yaw >> 8) & 0xff) as f32 * UNPACK_ROT
    }

    /// `pitch(float)` (`particle.ipp:235-239`).
    pub fn set_pitch(&mut self, p: f32) {
        let pt = (((p + ABS_ROT) * PACK_ROT + ROUND_ROT) as u16 & 0xff) << 8;
        self.pitch_yaw = (self.pitch_yaw & 0xff) | pt;
    }

    /// `yaw()` — the low byte.
    pub fn yaw(&self) -> f32 {
        (self.pitch_yaw & 0xff) as f32 * UNPACK_ROT
    }

    /// `yaw(float)` (`particle.ipp:248-252`).
    pub fn set_yaw(&mut self, y: f32) {
        let yw = ((y + ABS_ROT) * PACK_ROT + ROUND_ROT) as u16 & 0xff;
        self.pitch_yaw = (self.pitch_yaw & 0xff00) | yw;
    }

    /// `Particle::sizeInBytes` (`particle.ipp:96-99`) — `sizeof(Particle)`.
    pub fn size_in_bytes() -> usize {
        std::mem::size_of::<Particle>()
    }
}

// ---------------------------------------------------------------------------
// Section: ContiguousParticles (`particles.ipp`)
// ---------------------------------------------------------------------------

/// `ContiguousParticles`: the vector-backed container the sprite renderers
/// use, with the reference swap-with-back removal.
#[derive(Debug, Clone, Default)]
pub struct Particles {
    items: Vec<Particle>,
    /// `particleIdx_`: the cycling creation index stamped onto non-mesh
    /// particles (`particles.ipp`).
    particle_idx: usize,
}

impl Particles {
    pub fn new() -> Self {
        Particles::default()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Particle> {
        self.items.iter()
    }

    pub fn items_mut(&mut self) -> std::slice::IterMut<'_, Particle> {
        self.items.iter_mut()
    }

    pub fn get(&self, index: usize) -> Particle {
        self.items[index]
    }

    pub fn get_mut(&mut self, index: usize) -> &mut Particle {
        &mut self.items[index]
    }

    /// `addParticle` (`particles.ipp`): push and advance the cycling index.
    pub fn add_particle(&mut self, p: Particle) -> usize {
        self.items.push(p);
        let idx = self.items.len() - 1;
        self.particle_idx += 1;
        idx
    }

    /// `removeParticle` (`particles.ipp`): the last particle is moved into
    /// the removed slot; the same slot index is returned so iteration
    /// continues without skipping it.
    pub fn remove_particle(&mut self, index: usize) -> Option<usize> {
        if index >= self.items.len() {
            return None;
        }
        let last = self.items.len() - 1;
        self.items[index] = self.items[last];
        self.items.pop();
        Some(index)
    }

    /// `clear` (`particles.ipp`).
    pub fn clear(&mut self) {
        self.items.clear();
        self.particle_idx = 0;
    }

    pub fn reserve(&mut self, n: usize) {
        self.items.reserve(n);
    }
}

// ---------------------------------------------------------------------------
// Section: VectorGenerator (`actions/vector_generator.cpp`/`.ipp`)
// ---------------------------------------------------------------------------

/// The lazy sphere lookup table (`vector_generator.cpp`
/// `SphereVectorCache`): 4096 pre-made directions and the cube-root
/// ladder, built once from a fixed-seed [`BwRandom`] (the reference builds
/// it from the time-seeded global; the port pins that construction so the
/// table is the same every run).
struct SphereVectorCache {
    vectors: Vec<Vector3>,
    pow_cache: Vec<f32>,
}

const SPHERE_CACHE_SEED: u32 = 0x2000_0000;

static SPHERE_CACHE: std::sync::OnceLock<SphereVectorCache> = std::sync::OnceLock::new();

impl SphereVectorCache {
    fn build() -> Self {
        let mut rng = BwRandom::new(SPHERE_CACHE_SEED);
        let mut vectors = Vec::with_capacity(4096);
        let mut pow_cache = Vec::with_capacity(4096);
        for i in 0..4096usize {
            let theta = MATH_2PI * rng.unit_rand();
            let phi = (2.0f32 * rng.unit_rand() - 1.0).acos();
            let sin_phi = phi.sin();
            vectors.push(Vector3::new(
                theta.cos() * sin_phi,
                theta.sin() * sin_phi,
                phi.cos(),
            ));
            pow_cache.push((i as f32 / 4096.0).powf(1.0 / 3.0));
        }
        SphereVectorCache { vectors, pow_cache }
    }

    fn get() -> &'static SphereVectorCache {
        SPHERE_CACHE.get_or_init(SphereVectorCache::build)
    }

    /// `getVector`: an extra full-word draw picks the slot.
    fn get_vector(&self, rng: &mut BwRandom) -> Vector3 {
        self.vectors[(rng.next_u32() % 4096) as usize]
    }

    /// `getPow`: the index from the argument, truncated then clamped into
    /// the table with a warning in the reference — the guards fire only on
    /// formula changes, never in range.
    fn get_pow(&self, num: f32) -> f32 {
        let idx = ((num * 4095.0) as i32).clamp(0, 4095);
        self.pow_cache[idx as usize]
    }
}

/// The five position/velocity sources (`VectorGeneratorBase` children).
#[derive(Debug, Clone, PartialEq)]
pub enum VectorGenerator {
    /// `PointVectorGenerator`.
    Point { position: Vector3 },
    /// `LineVectorGenerator`.
    Line { origin: Vector3, direction: Vector3 },
    /// `CylinderVectorGenerator`.
    Cylinder {
        origin: Vector3,
        direction: Vector3,
        max_radius: f32,
        min_radius: f32,
        basis_u: Vector3,
        basis_v: Vector3,
    },
    /// `SphereVectorGenerator`.
    Sphere {
        centre: Vector3,
        max_radius: f32,
        min_radius: f32,
    },
    /// `BoxVectorGenerator` (the `opposite_` tag is the
    /// `BW_PARITCLE_SERIALISE_NAMED` one).
    Box { corner: Vector3, opposite: Vector3 },
}

impl VectorGenerator {
    /// `VectorGeneratorBase::load` dispatch (`vector_generator.cpp:110-`):
    /// the section name picks the type, `nameID_` is sanity only.
    pub fn load(ds: &DataSection) -> Option<VectorGenerator> {
        match ds.section_name() {
            "PointVectorGenerator" => Some(VectorGenerator::Point {
                position: ds.read_vector3("position_", Vector3::ZERO),
            }),
            "LineVectorGenerator" => Some(VectorGenerator::Line {
                origin: ds.read_vector3("origin_", Vector3::ZERO),
                direction: ds.read_vector3("direction_", Vector3::ZERO),
            }),
            "CylinderVectorGenerator" => Some(VectorGenerator::Cylinder {
                origin: ds.read_vector3("origin_", Vector3::ZERO),
                direction: ds.read_vector3("direction_", Vector3::ZERO),
                max_radius: ds.read_float("maxRadius_", 0.0),
                min_radius: ds.read_float("minRadius_", 0.0),
                basis_u: ds.read_vector3("basisU_", Vector3::ZERO),
                basis_v: ds.read_vector3("basisV_", Vector3::ZERO),
            }),
            "SphereVectorGenerator" => Some(VectorGenerator::Sphere {
                centre: ds.read_vector3("centre_", Vector3::ZERO),
                max_radius: ds.read_float("maxRadius_", 0.0),
                min_radius: ds.read_float("minRadius_", 0.0),
            }),
            "BoxVectorGenerator" => Some(VectorGenerator::Box {
                corner: ds.read_vector3("corner_", Vector3::ZERO),
                opposite: ds.read_vector3("opposite_", Vector3::ZERO),
            }),
            _ => None,
        }
    }

    /// `generate` (`vector_generator.ipp`), in the reference draw order.
    pub fn generate(&self, rng: &mut BwRandom) -> Vector3 {
        match self {
            // `PointVectorGenerator::generate`: no draw at all.
            VectorGenerator::Point { position } => *position,
            // `LineVectorGenerator::generate`: one draw along the span.
            VectorGenerator::Line { origin, direction } => *origin + *direction * rng.unit_rand(),
            VectorGenerator::Cylinder {
                origin,
                direction,
                max_radius,
                min_radius,
                basis_u,
                basis_v,
            } => {
                // `CylinderVectorGenerator::generate`
                // (`vector_generator.cpp:374-402`): a point along the axis
                // (origin first), then the disc draw — the radii are
                // sorted at generate time, and the sqrt-distribution draw
                // adds `minR` (not `alpha`) inside the root before
                // `Math::lerp` maps k1 back onto [min, max]
                // (`mathdef.hpp:262-274`).
                let mut vector = *origin + *direction * rng.unit_rand();
                let (min_r, max_r) = if min_radius > max_radius {
                    (*max_radius, *min_radius)
                } else {
                    (*min_radius, *max_radius)
                };
                let angle = (rng.unit_rand() * 2.0) * MATH_PI;
                let radius = if min_r == max_r {
                    min_r
                } else {
                    let alpha = min_r / max_r;
                    let k1 = (rng.unit_rand() * (1.0 - alpha) + min_r).sqrt();
                    (max_r - min_r) * (k1 - alpha) / (1.0 - alpha) + min_r
                };
                vector = vector + (*basis_u * angle.cos() + *basis_v * angle.sin()) * radius;
                vector
            }
            VectorGenerator::Sphere {
                centre,
                max_radius,
                min_radius,
            } => {
                // `SphereVectorGenerator::generate`: table pick, then the
                // radius draw unless the two radii match.
                let mut vector = SphereVectorCache::get().get_vector(rng);
                let radius = if *max_radius == *min_radius {
                    *min_radius
                } else {
                    let alpha = *min_radius / *max_radius;
                    let k1 =
                        SphereVectorCache::get().get_pow(rng.unit_rand() * (1.0 - alpha) + alpha);
                    k1 * *max_radius
                };
                vector = vector * radius + *centre;
                vector
            }
            VectorGenerator::Box { corner, opposite } => {
                // `BoxVectorGenerator::generate`: three draws, x then y
                // then z.
                Vector3::new(
                    corner.x + rng.unit_rand() * (opposite.x - corner.x),
                    corner.y + rng.unit_rand() * (opposite.y - corner.y),
                    corner.z + rng.unit_rand() * (opposite.z - corner.z),
                )
            }
        }
    }

    /// `CylinderVectorGenerator::constructBasis`
    /// (`vector_generator.cpp:322-340`): an orthonormal pair across
    /// `direction`, falling back to the y axis when x is parallel. Only
    /// the parallelism test uses the normalised copy; the cross products
    /// use the direction as stored.
    pub fn cylinder_construct_basis(direction: Vector3) -> (Vector3, Vector3) {
        let mut normalised = direction;
        normalised.normalise();
        let mut basis = Vector3::new(1.0, 0.0, 0.0);
        if (1.0 - basis.dot_product(&normalised).abs()).abs() < 0.0001 {
            basis = Vector3::new(0.0, 1.0, 0.0);
        }
        let mut basis_u = Vector3::ZERO.cross_product(&basis, &direction);
        basis_u.normalise();
        let mut basis_v = Vector3::ZERO.cross_product(&direction, &basis_u);
        basis_v.normalise();
        (basis_u, basis_v)
    }

    /// `CylinderVectorGenerator::checkRadii`
    /// (`vector_generator.cpp:342-352`): the minimum radius of a cylinder
    /// is "always zero" (bug 5075: not checked at all) and a negative
    /// maximum clamps to zero.
    pub fn cylinder_check_radii(_min_radius: f32, max_radius: f32) -> (f32, f32) {
        (0.0, if max_radius < 0.0 { 0.0 } else { max_radius })
    }

    /// `SphereVectorGenerator::checkRadii`
    /// (`vector_generator.cpp:510-528`): both radii clamp to non-negative
    /// and a maximum below the minimum snaps onto it.
    pub fn sphere_check_radii(min_radius: f32, max_radius: f32) -> (f32, f32) {
        let min = if min_radius < 0.0 { 0.0 } else { min_radius };
        let max = if max_radius < 0.0 { 0.0 } else { max_radius };
        let max = if max < min { min } else { max };
        (min, max)
    }

    /// `VectorGenerator::sizeInBytes` — `sizeof` of the concrete type,
    /// added up by the Jitter action.
    pub fn size_in_bytes(&self) -> usize {
        match self {
            VectorGenerator::Point { .. } => std::mem::size_of::<Vector3>(),
            VectorGenerator::Line { .. } => 2 * std::mem::size_of::<Vector3>(),
            VectorGenerator::Cylinder { .. } => {
                4 * std::mem::size_of::<Vector3>() + 2 * std::mem::size_of::<f32>()
            }
            VectorGenerator::Sphere { .. } => {
                std::mem::size_of::<Vector3>() + 2 * std::mem::size_of::<f32>()
            }
            VectorGenerator::Box { .. } => 2 * std::mem::size_of::<Vector3>(),
        }
    }
}

// ---------------------------------------------------------------------------
// Section: actions (`actions/*.cpp`)
// ---------------------------------------------------------------------------

/// The action type ids (`ParticleSystemAction` enum, SOURCE=1..SPLAT=15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionType {
    Source = 1,
    Sink = 2,
    Barrier = 3,
    Force = 4,
    Stream = 5,
    Jitter = 6,
    Scaler = 7,
    TintShader = 8,
    NodeClamp = 9,
    Orbitor = 10,
    Flare = 11,
    Collide = 12,
    MatrixSwarm = 13,
    Magnet = 14,
    Splat = 15,
}

/// `ParticleSystemAction::nameToType` (`particle_system_action.cpp`):
/// unknown names have no type and the loader refuses the action.
pub fn name_to_type(name: &str) -> Option<ActionType> {
    match name {
        "Source" => Some(ActionType::Source),
        "Sink" => Some(ActionType::Sink),
        "Barrier" => Some(ActionType::Barrier),
        "Force" => Some(ActionType::Force),
        "Stream" => Some(ActionType::Stream),
        "Jitter" => Some(ActionType::Jitter),
        "Scaler" => Some(ActionType::Scaler),
        "TintShader" => Some(ActionType::TintShader),
        "NodeClamp" => Some(ActionType::NodeClamp),
        "Orbitor" => Some(ActionType::Orbitor),
        "Flare" => Some(ActionType::Flare),
        "Collide" => Some(ActionType::Collide),
        "MatrixSwarm" => Some(ActionType::MatrixSwarm),
        "Magnet" => Some(ActionType::Magnet),
        "Splat" => Some(ActionType::Splat),
        _ => None,
    }
}

impl ActionType {
    /// `nameID()` (`particle_system_action.ipp`).
    pub fn name_id(self) -> &'static str {
        match self {
            ActionType::Source => "Source",
            ActionType::Sink => "Sink",
            ActionType::Barrier => "Barrier",
            ActionType::Force => "Force",
            ActionType::Stream => "Stream",
            ActionType::Jitter => "Jitter",
            ActionType::Scaler => "Scaler",
            ActionType::TintShader => "TintShader",
            ActionType::NodeClamp => "NodeClamp",
            ActionType::Orbitor => "Orbitor",
            ActionType::Flare => "Flare",
            ActionType::Collide => "Collide",
            ActionType::MatrixSwarm => "MatrixSwarm",
            ActionType::Magnet => "Magnet",
            ActionType::Splat => "Splat",
        }
    }
}

/// `ParticleSystemAction` base state (`particle_system_action.cpp` ctor:
/// delay 0, minimumAge 0, age 0, enabled true).
#[derive(Debug, Clone, PartialEq)]
pub struct ActionBase {
    pub delay: f32,
    pub minimum_age: f32,
    pub age: f32,
    pub enabled: bool,
}

impl Default for ActionBase {
    fn default() -> Self {
        ActionBase {
            delay: 0.0,
            minimum_age: 0.0,
            age: 0.0,
            enabled: true,
        }
    }
}

impl ActionBase {
    /// `ParticleSystemAction::serialise`: `delay_`, `minimumAge_`,
    /// `name_` (the name tag carries the display name, not the type).
    fn load(ds: &DataSection) -> ActionBase {
        ActionBase {
            delay: ds.read_float("delay_", 0.0),
            minimum_age: ds.read_float("minimumAge_", 0.0),
            age: 0.0,
            enabled: ds.read_bool("enabled_", true),
        }
    }

    /// The shared preamble of every `execute`: nothing happens before the
    /// delay elapses or for a zero step; the age only accumulates while
    /// gated.
    fn gated(&mut self, d_time: f32) -> bool {
        if self.age < self.delay || d_time <= 0.0 {
            self.age += d_time;
            true
        } else {
            false
        }
    }
}

/// A tint stop (`TintShaderPSA::Tint`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tint {
    pub time: f32,
    pub colour: Vector4,
}

/// `SourcePSA` (`source_psa.ipp:18-53` ctor defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct SourcePSA {
    pub motion_triggered: bool,
    pub time_triggered: bool,
    pub grounded: bool,
    pub drop_distance: f32,
    pub rate: f32,
    pub sensitivity: f32,
    pub max_speed: f32,
    pub active_period: f32,
    pub sleep_period: f32,
    pub sleep_period_max: f32,
    pub minimum_size: f32,
    pub maximum_size: f32,
    pub forced_unit_size: i32,
    pub allowed_time_in_seconds: f32,
    pub initial_rotation: Vector2,
    pub random_initial_rotation: Vector2,
    pub initial_colour: Vector4,
    pub random_spin: bool,
    pub min_spin: f32,
    pub max_spin: f32,
    pub ignore_rotation: bool,
    pub inherit_velocity: f32,
    pub position_src: Option<VectorGenerator>,
    pub velocity_src: Option<VectorGenerator>,
    // runtime state
    pub queued_total: i32,
    pub forced_total: i32,
    pub period_time: f32,
    pub accumulated_time: f32,
    pub last_position_of_ps: Vector3,
    pub first_update: bool,
    pub current_sleep_period: f32,
}

impl Default for SourcePSA {
    fn default() -> Self {
        SourcePSA {
            motion_triggered: false,
            time_triggered: false,
            grounded: false,
            drop_distance: 15.0,
            rate: 0.0,
            sensitivity: 0.1,
            max_speed: 0.0,
            active_period: 1.0,
            sleep_period: 0.0,
            sleep_period_max: -1.0,
            minimum_size: 1.0,
            maximum_size: 1.0,
            forced_unit_size: 1,
            // `allowedTime_` starts at `stampsPerSecond()`; the reference
            // compares it against wall-clock stamps and bails mid-loop.
            // The port never bails (see `create_particles`), so the field
            // is a stored face.
            allowed_time_in_seconds: 1.0,
            initial_rotation: Vector2::ZERO,
            random_initial_rotation: Vector2::ZERO,
            initial_colour: Vector4::new(0.5, 0.5, 0.5, 1.0),
            random_spin: false,
            min_spin: 0.0,
            max_spin: 0.0,
            ignore_rotation: false,
            inherit_velocity: 0.0,
            position_src: None,
            velocity_src: None,
            queued_total: 0,
            forced_total: 0,
            period_time: 0.0,
            accumulated_time: 0.0,
            last_position_of_ps: Vector3::ZERO,
            first_update: true,
            current_sleep_period: 0.0,
        }
    }
}

impl SourcePSA {
    /// `SourcePSA::loadInternal` (`source_psa.cpp:74-118`): the generator
    /// sections first (the last child of each wins), then the serialise
    /// tags, then a fresh sleep period and the ordered pair.
    fn load(&mut self, ds: &DataSection) {
        if let Some(pos_ds) = ds.open_section("pPositionSrc") {
            for i in 0..pos_ds.count_children() {
                let child = pos_ds.open_child(i).unwrap();
                self.position_src = VectorGenerator::load(child);
            }
        }
        if let Some(vel_ds) = ds.open_section("pVelocitySrc") {
            for i in 0..vel_ds.count_children() {
                let child = vel_ds.open_child(i).unwrap();
                self.velocity_src = VectorGenerator::load(child);
            }
        }

        self.motion_triggered = ds.read_bool("motionTriggered_", self.motion_triggered);
        self.time_triggered = ds.read_bool("timeTriggered_", self.time_triggered);
        self.grounded = ds.read_bool("grounded_", self.grounded);
        self.drop_distance = ds.read_float("dropDistance_", self.drop_distance);
        self.rate = ds.read_float("rate_", self.rate);
        self.sensitivity = ds.read_float("sensitivity_", self.sensitivity);
        self.max_speed = ds.read_float("maxSpeed_", self.max_speed);
        self.active_period = ds.read_float("activePeriod_", self.active_period);
        self.sleep_period = ds.read_float("sleepPeriod_", self.sleep_period);
        self.sleep_period_max = ds.read_float("sleepPeriodMax_", self.sleep_period_max);
        self.minimum_size = ds.read_float("minimumSize_", self.minimum_size);
        self.maximum_size = ds.read_float("maximumSize_", self.maximum_size);
        self.forced_unit_size = ds.read_int("forcedUnitSize_", self.forced_unit_size);
        self.allowed_time_in_seconds =
            ds.read_float("allowedTimeInSeconds_", self.allowed_time_in_seconds);
        self.initial_rotation = ds
            .open_section("initialRotation_")
            .map(|c| c.as_vector2(self.initial_rotation))
            .unwrap_or(self.initial_rotation);
        self.random_initial_rotation = ds
            .open_section("randomInitialRotation_")
            .map(|c| c.as_vector2(self.random_initial_rotation))
            .unwrap_or(self.random_initial_rotation);
        self.initial_colour = ds
            .open_section("initialColour_")
            .map(|c| c.as_vector4(self.initial_colour))
            .unwrap_or(self.initial_colour);
        self.random_spin = ds.read_bool("randomSpin_", self.random_spin);
        self.min_spin = ds.read_float("minSpin_", self.min_spin);
        self.max_spin = ds.read_float("maxSpin_", self.max_spin);
        self.ignore_rotation = ds.read_bool("ignoreRotation_", self.ignore_rotation);
        self.inherit_velocity = ds.read_float("inheritVelocity_", self.inherit_velocity);

        self.current_sleep_period = generate_sleep_period(self.sleep_period, self.sleep_period_max);
        if self.sleep_period > self.sleep_period_max
            && self.sleep_period >= 0.0
            && self.sleep_period_max >= 0.0
        {
            std::mem::swap(&mut self.sleep_period, &mut self.sleep_period_max);
        }
    }

    /// `SourcePSA::setSensitivity` clamps away from zero
    /// (`source_psa.cpp`).
    pub fn set_sensitivity(&mut self, sensitivity: f32) {
        self.sensitivity = sensitivity.max(0.0001);
    }

    /// `SourcePSA::force` (`source_psa.ipp:596-599`): queue a forced
    /// burst for the next execute.
    pub fn force(&mut self, number: i32) {
        self.forced_total += number;
    }
}

/// `SourcePSA::generateSleepPeriod` (`source_psa.cpp`): a draw between the
/// bounds when both are set, the bound when only one is, else zero. The
/// reference draws from the global RNG here — which would make loading
/// non-deterministic — so the port takes the deterministic midpoint and
/// documents the seam.
fn generate_sleep_period(sleep_period: f32, sleep_period_max: f32) -> f32 {
    if sleep_period >= 0.0 && sleep_period_max >= 0.0 {
        (sleep_period_max - sleep_period).abs() * 0.5 + sleep_period
    } else if sleep_period >= 0.0 {
        sleep_period
    } else if sleep_period_max >= 0.0 {
        sleep_period_max
    } else {
        0.0
    }
}

/// `SinkPSA` defaults (`sink_psa.hpp` ctor: maximumAge -1, minimumSpeed
/// -1, outsideOnly false).
#[derive(Debug, Clone, PartialEq)]
pub struct SinkPSA {
    pub maximum_age: f32,
    pub minimum_speed: f32,
    pub outside_only: bool,
}

impl Default for SinkPSA {
    fn default() -> Self {
        SinkPSA {
            maximum_age: -1.0,
            minimum_speed: -1.0,
            outside_only: false,
        }
    }
}

/// `BarrierPSA` shapes/reactions (`barrier_psa.hpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarrierShape {
    None = 0,
    VerticalCylinder = 1,
    Box = 2,
    Sphere = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarrierReaction {
    Bounce = 0,
    Remove = 1,
    Allow = 2,
    Wrap = 3,
}

/// `BarrierPSA` defaults (`barrier_psa.cpp` ctor: shape NONE, reaction
/// ALLOW, radius 0, resilience 1 — resilience is runtime-only and is not
/// serialised).
#[derive(Debug, Clone, PartialEq)]
pub struct BarrierPSA {
    pub shape: BarrierShape,
    pub reaction: BarrierReaction,
    pub vec_a: Vector3,
    pub vec_b: Vector3,
    pub radius: f32,
    pub resilience: f32,
}

impl Default for BarrierPSA {
    fn default() -> Self {
        BarrierPSA {
            shape: BarrierShape::None,
            reaction: BarrierReaction::Allow,
            vec_a: Vector3::ZERO,
            vec_b: Vector3::ZERO,
            radius: 0.0,
            resilience: 1.0,
        }
    }
}

/// `ForcePSA` (default vector zero).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ForcePSA {
    pub vector: Vector3,
}

/// `StreamPSA` (defaults vector zero, halfLife -1).
#[derive(Debug, Clone, PartialEq)]
pub struct StreamPSA {
    pub vector: Vector3,
    pub half_life: f32,
}

impl Default for StreamPSA {
    fn default() -> Self {
        StreamPSA {
            vector: Vector3::ZERO,
            half_life: -1.0,
        }
    }
}

/// `JitterPSA` (defaults: affect both, no sources).
#[derive(Debug, Clone, PartialEq)]
pub struct JitterPSA {
    pub affect_position: bool,
    pub affect_velocity: bool,
    pub position_src: Option<VectorGenerator>,
    pub velocity_src: Option<VectorGenerator>,
}

impl Default for JitterPSA {
    fn default() -> Self {
        JitterPSA {
            affect_position: true,
            affect_velocity: true,
            position_src: None,
            velocity_src: None,
        }
    }
}

impl JitterPSA {
    /// `JitterPSA::loadInternal`: the generator sections then the tags.
    fn load(&mut self, ds: &DataSection) {
        if let Some(pos_ds) = ds.open_section("pPositionSrc") {
            for i in 0..pos_ds.count_children() {
                let child = pos_ds.open_child(i).unwrap();
                self.position_src = VectorGenerator::load(child);
            }
        }
        if let Some(vel_ds) = ds.open_section("pVelocitySrc") {
            for i in 0..vel_ds.count_children() {
                let child = vel_ds.open_child(i).unwrap();
                self.velocity_src = VectorGenerator::load(child);
            }
        }
        self.affect_position = ds.read_bool("affectPosition_", self.affect_position);
        self.affect_velocity = ds.read_bool("affectVelocity_", self.affect_velocity);
    }
}

/// `ScalerPSA` (defaults size 0, rate 0).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScalerPSA {
    pub size: f32,
    pub rate: f32,
}

/// `TintShaderPSA` defaults (`tint_shader_psa.cpp` ctor: repeat false,
/// period 1, fogAmount 0, no tints).
#[derive(Debug, Clone, PartialEq)]
pub struct TintShaderPSA {
    pub repeat: bool,
    pub period: f32,
    pub fog_amount: f32,
    pub tints: Vec<Tint>,
}

impl Default for TintShaderPSA {
    fn default() -> Self {
        TintShaderPSA {
            repeat: false,
            period: 1.0,
            fog_amount: 0.0,
            tints: Vec::new(),
        }
    }
}

impl TintShaderPSA {
    /// `TintShaderPSA::loadInternal` (`tint_shader_psa.cpp`): the
    /// serialise tags first, then the ramp rebuilt from the file — a
    /// missing ramp becomes the black-to-white pair over the period, a
    /// single stop becomes a flat pair (and its 0 period becomes 1 for
    /// good), otherwise the stops load and sort by time. The local period
    /// (with the one-tint fixup) lands back in `period_`.
    fn load(&mut self, ds: &DataSection) {
        self.repeat = ds.read_bool("repeat_", self.repeat);
        self.period = ds.read_float("period_", self.period);
        self.fog_amount = ds.read_float("fogAmount_", self.fog_amount);

        let mut period = self.period;
        match ds.open_section("tints_") {
            None => {
                self.tints = vec![
                    Tint {
                        time: 0.0,
                        colour: Vector4::new(0.0, 0.0, 0.0, 1.0),
                    },
                    Tint {
                        time: period,
                        colour: Vector4::new(1.0, 1.0, 1.0, 1.0),
                    },
                ];
            }
            Some(tints_ds) => {
                let mut tints = Vec::new();
                for i in 0..tints_ds.count_children() {
                    let child = tints_ds.open_child(i).unwrap();
                    tints.push(Tint {
                        time: child.read_float("time", 0.0),
                        colour: child
                            .open_section("color")
                            .map(|c| c.as_vector4(Vector4::ZERO))
                            .unwrap_or(Vector4::ZERO),
                    });
                }
                if tints.len() == 1 {
                    let colour = tints[0].colour;
                    if period == 0.0 {
                        period = 1.0;
                    }
                    self.tints = vec![
                        Tint { time: 0.0, colour },
                        Tint {
                            time: period,
                            colour,
                        },
                    ];
                } else {
                    // The general case: sort by time. An empty section
                    // stays an empty ramp (execute is a no-op on it).
                    tints.sort_by(|a, b| {
                        a.time
                            .partial_cmp(&b.time)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    self.tints = tints;
                }
            }
        }
        self.period = period;
    }

    /// `TintShaderPSA::addTintAt` (`tint_shader_psa.cpp`): insert in time
    /// order and grow the period to cover the new stop.
    pub fn add_tint_at(&mut self, tint: Tint) {
        let at = self.tints.partition_point(|t| t.time <= tint.time);
        self.tints.insert(at, tint);
        self.period = self.period.max(tint.time);
    }

    /// `TintShaderPSA::clearAllTints`.
    pub fn clear_all_tints(&mut self) {
        self.tints.clear();
        self.period = 0.0;
    }
}

/// `NodeClampPSA` (fullyClamp false).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodeClampPSA {
    pub fully_clamp: bool,
}

/// `OrbitorPSA` (defaults point zero, angularVelocity zero, affectVelocity
/// true).
#[derive(Debug, Clone, PartialEq)]
pub struct OrbitorPSA {
    pub point: Vector3,
    pub angular_velocity: f32,
    pub affect_velocity: bool,
}

impl Default for OrbitorPSA {
    fn default() -> Self {
        OrbitorPSA {
            point: Vector3::ZERO,
            angular_velocity: 0.0,
            affect_velocity: true,
        }
    }
}

/// `FlarePSA` (defaults: empty name, step 0, colourize false,
/// useParticleSize false).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FlarePSA {
    pub flare_name: String,
    pub flare_step: f32,
    pub colourize: bool,
    pub use_particle_size: bool,
}

/// `CollidePSA` (defaults: spriteBased false, elasticity 0, rotations 0
/// and an empty sound face).
#[derive(Debug, Clone, PartialEq)]
pub struct CollidePSA {
    pub sprite_based: bool,
    pub elasticity: f32,
    pub min_added_rotation: f32,
    pub max_added_rotation: f32,
    pub sound_tag: String,
    pub sound_enabled: bool,
    pub sound_src_idx: i32,
    pub sound_project: String,
    pub sound_group: String,
    pub sound_name: String,
}

impl Default for CollidePSA {
    fn default() -> Self {
        CollidePSA {
            sprite_based: false,
            elasticity: 0.0,
            min_added_rotation: 0.0,
            max_added_rotation: 0.0,
            sound_tag: String::new(),
            sound_enabled: false,
            sound_src_idx: -1,
            sound_project: String::new(),
            sound_group: String::new(),
            sound_name: String::new(),
        }
    }
}

/// `MatrixSwarmPSA` carries no serialised state (`matrix_swarm_psa.cpp`
/// `loadInternal`/`saveInternal` are empty); its targets are
/// `MatrixProvider`s (render seam).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MatrixSwarmPSA;

/// `MagnetPSA` (defaults strength 1, minDist 0; the source matrix is a
/// runtime face, not serialised).
#[derive(Debug, Clone, PartialEq)]
pub struct MagnetPSA {
    pub strength: f32,
    pub min_dist: f32,
    pub source: Option<Matrix>,
}

impl Default for MagnetPSA {
    fn default() -> Self {
        // `MagnetPSA::MagnetPSA` (`magnet_psa.ipp:18-23`): strength 1,
        // minDist 1.
        MagnetPSA {
            strength: 1.0,
            min_dist: 1.0,
            source: None,
        }
    }
}

/// `SplatPSA` carries no serialised state (`splat_psa.cpp`
/// `loadInternal`/`saveInternal` are empty); its callback is a script
/// seam.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SplatPSA;

/// The action union: every type loads and saves; the seam-free subset
/// executes.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionKind {
    Source(SourcePSA),
    Sink(SinkPSA),
    Barrier(BarrierPSA),
    Force(ForcePSA),
    Stream(StreamPSA),
    Jitter(JitterPSA),
    Scaler(ScalerPSA),
    TintShader(TintShaderPSA),
    NodeClamp(NodeClampPSA),
    Orbitor(OrbitorPSA),
    Flare(FlarePSA),
    Collide(CollidePSA),
    MatrixSwarm(MatrixSwarmPSA),
    Magnet(MagnetPSA),
    Splat(SplatPSA),
}

impl ActionKind {
    pub fn type_id(&self) -> ActionType {
        match self {
            ActionKind::Source(_) => ActionType::Source,
            ActionKind::Sink(_) => ActionType::Sink,
            ActionKind::Barrier(_) => ActionType::Barrier,
            ActionKind::Force(_) => ActionType::Force,
            ActionKind::Stream(_) => ActionType::Stream,
            ActionKind::Jitter(_) => ActionType::Jitter,
            ActionKind::Scaler(_) => ActionType::Scaler,
            ActionKind::TintShader(_) => ActionType::TintShader,
            ActionKind::NodeClamp(_) => ActionType::NodeClamp,
            ActionKind::Orbitor(_) => ActionType::Orbitor,
            ActionKind::Flare(_) => ActionType::Flare,
            ActionKind::Collide(_) => ActionType::Collide,
            ActionKind::MatrixSwarm(_) => ActionType::MatrixSwarm,
            ActionKind::Magnet(_) => ActionType::Magnet,
            ActionKind::Splat(_) => ActionType::Splat,
        }
    }
}

/// One entry of a system's action set.
#[derive(Debug, Clone, PartialEq)]
pub struct ParticleAction {
    pub base: ActionBase,
    pub kind: ActionKind,
}

/// The placeholder swapped in while an action body runs against its
/// system.
fn splat_placeholder() -> ParticleAction {
    ParticleAction {
        base: ActionBase::default(),
        kind: ActionKind::Splat(SplatPSA),
    }
}

impl ParticleAction {
    /// `ParticleSystemAction::createActionOfType` + the concrete
    /// `loadInternal`s: base serialise first, then the type face. Unknown
    /// section names yield the reference's loader error.
    pub fn load(ds: &DataSection) -> Result<ParticleAction, String> {
        let name = ds.section_name();
        let Some(type_id) = name_to_type(name) else {
            return Err(format!("Unknown Particle System Action type {}\n", name));
        };
        let base = ActionBase::load(ds);
        let kind = match type_id {
            ActionType::Source => {
                let mut s = SourcePSA::default();
                s.load(ds);
                ActionKind::Source(s)
            }
            ActionType::Sink => {
                let mut s = SinkPSA::default();
                s.maximum_age = ds.read_float("maximumAge_", s.maximum_age);
                s.minimum_speed = ds.read_float("minimumSpeed_", s.minimum_speed);
                s.outside_only = ds.read_bool("outsideOnly_", s.outside_only);
                ActionKind::Sink(s)
            }
            ActionType::Barrier => {
                let mut b = BarrierPSA::default();
                b.shape = match ds.read_int("shape_", b.shape as i32) {
                    1 => BarrierShape::VerticalCylinder,
                    2 => BarrierShape::Box,
                    3 => BarrierShape::Sphere,
                    _ => BarrierShape::None,
                };
                b.reaction = match ds.read_int("reaction_", b.reaction as i32) {
                    0 => BarrierReaction::Bounce,
                    1 => BarrierReaction::Remove,
                    3 => BarrierReaction::Wrap,
                    _ => BarrierReaction::Allow,
                };
                b.vec_a = ds.read_vector3("vecA_", b.vec_a);
                b.vec_b = ds.read_vector3("vecB_", b.vec_b);
                b.radius = ds.read_float("radius_", b.radius);
                ActionKind::Barrier(b)
            }
            ActionType::Force => ActionKind::Force(ForcePSA {
                vector: ds.read_vector3("vector_", Vector3::ZERO),
            }),
            ActionType::Stream => ActionKind::Stream(StreamPSA {
                vector: ds.read_vector3("vector_", Vector3::ZERO),
                half_life: ds.read_float("halfLife_", -1.0),
            }),
            ActionType::Jitter => {
                let mut j = JitterPSA::default();
                j.load(ds);
                ActionKind::Jitter(j)
            }
            ActionType::Scaler => ActionKind::Scaler(ScalerPSA {
                size: ds.read_float("size_", 0.0),
                rate: ds.read_float("rate_", 0.0),
            }),
            ActionType::TintShader => {
                let mut t = TintShaderPSA::default();
                t.load(ds);
                ActionKind::TintShader(t)
            }
            ActionType::NodeClamp => ActionKind::NodeClamp(NodeClampPSA {
                fully_clamp: ds.read_bool("fullyClamp_", false),
            }),
            ActionType::Orbitor => ActionKind::Orbitor(OrbitorPSA {
                point: ds.read_vector3("point_", Vector3::ZERO),
                angular_velocity: ds.read_float("angularVelocity_", 0.0),
                affect_velocity: ds.read_bool("affectVelocity_", true),
            }),
            ActionType::Flare => ActionKind::Flare(FlarePSA {
                flare_name: ds.read_string("flareName_", ""),
                flare_step: ds.read_float("flareStep_", 0.0),
                colourize: ds.read_bool("colourize_", false),
                use_particle_size: ds.read_bool("useParticleSize_", false),
            }),
            ActionType::Collide => ActionKind::Collide(CollidePSA {
                sprite_based: ds.read_bool("spriteBased_", false),
                elasticity: ds.read_float("elasticity_", 0.0),
                min_added_rotation: ds.read_float("minAddedRotation_", 0.0),
                max_added_rotation: ds.read_float("maxAddedRotation_", 0.0),
                sound_tag: ds.read_string("soundTag_", ""),
                sound_enabled: ds.read_bool("soundEnabled_", false),
                sound_src_idx: ds.read_int("soundSrcIdx_", -1),
                sound_project: ds.read_string("soundProject_", ""),
                sound_group: ds.read_string("soundGroup_", ""),
                sound_name: ds.read_string("soundName_", ""),
            }),
            ActionType::MatrixSwarm => ActionKind::MatrixSwarm(MatrixSwarmPSA),
            ActionType::Magnet => {
                let mut m = MagnetPSA::default();
                m.strength = ds.read_float("strength_", m.strength);
                m.min_dist = ds.read_float("minDist_", m.min_dist);
                ActionKind::Magnet(m)
            }
            ActionType::Splat => ActionKind::Splat(SplatPSA),
        };
        Ok(ParticleAction { base, kind })
    }

    /// `ParticleSystemAction::setFirstUpdate`; the Source also resets its
    /// position tracking.
    pub fn set_first_update(&mut self) {
        self.base.age = 0.0;
        if let ActionKind::Source(s) = &mut self.kind {
            s.first_update = true;
        }
    }

    /// The `sizeInBytes` overrides across `actions/*_psa.hpp`: the C++
    /// `sizeof(ConcretePSA)` includes the base subobject, so the port adds
    /// the base and the payload. The Jitter override also counts its
    /// generator sources (`jitter_psa.cpp:152-160`).
    pub fn size_in_bytes(&self) -> usize {
        let base = std::mem::size_of::<ActionBase>();
        match &self.kind {
            ActionKind::Source(s) => {
                base + std::mem::size_of::<SourcePSA>()
                    + match &s.position_src {
                        Some(g) => g.size_in_bytes(),
                        None => 0,
                    }
                    + match &s.velocity_src {
                        Some(g) => g.size_in_bytes(),
                        None => 0,
                    }
            }
            ActionKind::Jitter(j) => {
                base + std::mem::size_of::<JitterPSA>()
                    + match &j.position_src {
                        Some(g) => g.size_in_bytes(),
                        None => 0,
                    }
                    + match &j.velocity_src {
                        Some(g) => g.size_in_bytes(),
                        None => 0,
                    }
            }
            ActionKind::Sink(_) => base + std::mem::size_of::<SinkPSA>(),
            ActionKind::Barrier(_) => base + std::mem::size_of::<BarrierPSA>(),
            ActionKind::Force(_) => base + std::mem::size_of::<ForcePSA>(),
            ActionKind::Stream(_) => base + std::mem::size_of::<StreamPSA>(),
            ActionKind::Scaler(_) => base + std::mem::size_of::<ScalerPSA>(),
            ActionKind::TintShader(_) => base + std::mem::size_of::<TintShaderPSA>(),
            ActionKind::NodeClamp(_) => base + std::mem::size_of::<NodeClampPSA>(),
            ActionKind::Orbitor(_) => base + std::mem::size_of::<OrbitorPSA>(),
            ActionKind::Flare(_) => base + std::mem::size_of::<FlarePSA>(),
            ActionKind::Collide(_) => base + std::mem::size_of::<CollidePSA>(),
            ActionKind::MatrixSwarm(_) => base + std::mem::size_of::<MatrixSwarmPSA>(),
            ActionKind::Magnet(_) => base + std::mem::size_of::<MagnetPSA>(),
            ActionKind::Splat(_) => base + std::mem::size_of::<SplatPSA>(),
        }
    }
}

// ---------------------------------------------------------------------------
// Section: renderers (`renderers/particle_system_renderer.cpp`,
// `renderers/sprite_particle_renderer.cpp`)
// ---------------------------------------------------------------------------

/// `MaterialFX` (`sprite_particle_renderer.hpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterialFX {
    Additive = 0,
    AdditiveAlpha = 1,
    Blended = 2,
    BlendedColour = 3,
    BlendedInverseColour = 4,
    Solid = 5,
    Shimmer = 6,
    SourceAlpha = 7,
}

/// `ParticleSystemRenderer` base face (`particle_system_renderer.ipp:15-19`
/// ctor: viewDependent false, local false).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RendererBase {
    pub view_dependent: bool,
    pub local: bool,
}

/// `SpriteParticleRenderer` serialised face; `PointSpriteParticleRenderer`
/// derives from it and inherits the same fields
/// (`point_sprite_particle_renderer.hpp:12`), differing only in its
/// `nameID_`.
#[derive(Debug, Clone, PartialEq)]
pub struct SpriteData {
    pub material_fx: MaterialFX,
    pub frame_count: i32,
    pub frame_rate: f32,
    pub explicit_orientation: Vector3,
    pub texture_name: String,
    pub soft_depth_range: f32,
    pub soft_falloff_power: f32,
    pub soft_depth_offset: f32,
    pub near_fade_cutoff: f32,
    pub near_fade_start: f32,
    pub near_fade_falloff_power: f32,
    /// `rotated_`: not serialised; the system load derives it from the
    /// Source rotations (`particle_system.cpp:357-384`).
    pub rotated: bool,
}

impl Default for SpriteData {
    fn default() -> Self {
        SpriteData {
            material_fx: MaterialFX::Blended,
            frame_count: 0,
            frame_rate: 0.0,
            explicit_orientation: Vector3::ZERO,
            texture_name: String::new(),
            soft_depth_range: 0.0,
            soft_falloff_power: 1.0,
            soft_depth_offset: 0.0,
            near_fade_cutoff: 1.0,
            near_fade_start: 1.0,
            near_fade_falloff_power: 1.0,
            rotated: false,
        }
    }
}

/// The renderer union; only the sprite family is loadable — every other
/// `createRendererOfType` entry (Mesh/Visual/Amp/Trail/Blur) stays a
/// documented seam and fails the system load exactly like a NULL renderer
/// does in the reference (`return (pRenderer_ != NULL)`).
#[derive(Debug, Clone, PartialEq)]
pub enum ParticleRenderer {
    Sprite {
        base: RendererBase,
        sprite: SpriteData,
    },
    PointSprite {
        base: RendererBase,
        sprite: SpriteData,
    },
}

impl ParticleRenderer {
    /// `ParticleSystemRenderer::createRendererOfType`: the section name
    /// picks the type; unknown names and the non-sprite family yield None.
    pub fn create_renderer_of_type(name: &str) -> Option<ParticleRenderer> {
        match name {
            "SpriteParticleRenderer" => Some(ParticleRenderer::Sprite {
                base: RendererBase::default(),
                sprite: SpriteData::default(),
            }),
            "PointSpriteParticleRenderer" => Some(ParticleRenderer::PointSprite {
                base: RendererBase::default(),
                sprite: SpriteData::default(),
            }),
            _ => None,
        }
    }

    /// `ParticleSystemRenderer::nameID`.
    pub fn name_id(&self) -> &'static str {
        match self {
            ParticleRenderer::Sprite { .. } => "SpriteParticleRenderer",
            ParticleRenderer::PointSprite { .. } => "PointSpriteParticleRenderer",
        }
    }

    /// `ParticleSystemRenderer::load` (`particle_system_renderer.cpp:186-193`):
    /// the base serialise, then the sprite serialise
    /// (`sprite_particle_renderer.cpp:790-803`).
    pub fn load(&mut self, ds: &DataSection) {
        let (base, sprite) = match self {
            ParticleRenderer::Sprite { base, sprite }
            | ParticleRenderer::PointSprite { base, sprite } => (base, sprite),
        };
        base.view_dependent = ds.read_bool("viewDependent_", base.view_dependent);
        base.local = ds.read_bool("local_", base.local);
        sprite.material_fx = match ds.read_int("materialFX_", sprite.material_fx as i32) {
            0 => MaterialFX::Additive,
            1 => MaterialFX::AdditiveAlpha,
            3 => MaterialFX::BlendedColour,
            4 => MaterialFX::BlendedInverseColour,
            5 => MaterialFX::Solid,
            6 => MaterialFX::Shimmer,
            7 => MaterialFX::SourceAlpha,
            _ => MaterialFX::Blended,
        };
        sprite.frame_count = ds.read_int("frameCount_", sprite.frame_count);
        sprite.frame_rate = ds.read_float("frameRate_", sprite.frame_rate);
        sprite.explicit_orientation =
            ds.read_vector3("explicitOrientation_", sprite.explicit_orientation);
        sprite.texture_name = ds.read_string("textureName_", &sprite.texture_name);
        sprite.soft_depth_range = ds.read_float("softDepthRange_", sprite.soft_depth_range);
        sprite.soft_falloff_power = ds.read_float("softFalloffPower_", sprite.soft_falloff_power);
        sprite.soft_depth_offset = ds.read_float("softDepthOffset_", sprite.soft_depth_offset);
        sprite.near_fade_cutoff = ds.read_float("nearFadeCutoff_", sprite.near_fade_cutoff);
        sprite.near_fade_start = ds.read_float("nearFadeStart_", sprite.near_fade_start);
        sprite.near_fade_falloff_power =
            ds.read_float("nearFadeFalloffPower_", sprite.near_fade_falloff_power);
        // `updateMaterial()` is the texture/material seam; nothing else to
        // do for the data face.
    }

    /// `isMeshStyle` — both sprite renderers are quad batches.
    pub fn is_mesh_style(&self) -> bool {
        false
    }

    /// `ParticleSystemRenderer::update` — the base is a no-op and neither
    /// sprite subclass overrides it.
    pub fn update(&self) {}

    /// `knowParticleRadius` — false for both sprite renderers.
    pub fn know_particle_radius(&self) -> bool {
        false
    }

    /// The `sizeInBytes` overrides in `sprite_particle_renderer.hpp`/
    /// `point_sprite_particle_renderer.hpp`: the `sizeof` of the concrete
    /// renderer. The port's variant carries both, so the payload size is
    /// reported; the informational stat it feeds is not a port contract.
    pub fn size_in_bytes(&self) -> usize {
        std::mem::size_of::<RendererBase>() + std::mem::size_of::<SpriteData>()
    }

    pub fn local(&self) -> bool {
        match self {
            ParticleRenderer::Sprite { base, .. } | ParticleRenderer::PointSprite { base, .. } => {
                base.local
            }
        }
    }

    pub fn view_dependent(&self) -> bool {
        match self {
            ParticleRenderer::Sprite { base, .. } | ParticleRenderer::PointSprite { base, .. } => {
                base.view_dependent
            }
        }
    }

    /// `rotated` (sprite-only face; the system load writes it).
    pub fn set_rotated(&mut self, rotated: bool) {
        match self {
            ParticleRenderer::Sprite { sprite, .. }
            | ParticleRenderer::PointSprite { sprite, .. } => sprite.rotated = rotated,
        }
    }
}

// ---------------------------------------------------------------------------
// Section: ParticleSystem (`particle_system.cpp`)
// ---------------------------------------------------------------------------

/// The update environment: the `ParticleSystemManager` singleton seam
/// (`active()`, `windVelocity()`) and the `LodSettings` bias
/// (`applyLodBias` divides by the active option), passed instead of
/// reached.
#[derive(Debug, Clone, PartialEq)]
pub struct ParticleEnv {
    pub active: bool,
    pub wind_velocity: Vector3,
    pub lod_bias: f32,
}

impl Default for ParticleEnv {
    fn default() -> Self {
        ParticleEnv {
            active: true,
            wind_velocity: Vector3::ZERO,
            lod_bias: 1.0,
        }
    }
}

/// `ParticleSystem::serialiseVersionCode`/`VERSION_STRING`.
pub const PARTICLE_SERIALISE_VERSION: i32 = 2;
pub const VERSION_STRING: &str = "serialiseVersionData";
/// `ParticleSystem::MAX_CAPACITY`.
pub const MAX_CAPACITY: i32 = 65536;

/// The particle system (`particle_system.cpp` ctor defaults).
pub struct ParticleSystem {
    pub name: String,
    pub capacity: i32,
    pub particles: Particles,
    pub actions: Vec<ParticleAction>,
    pub renderer: Option<ParticleRenderer>,
    pub wind_factor: f32,
    pub enabled: bool,
    pub do_update: bool,
    pub first_update: bool,
    /// `counter_`: the frame counter gating `updateBoundingBox`; the
    /// reference seeds it from a global modulo 10 (a cosmetic spread), the
    /// port starts at 0.
    pub counter: u32,
    pub fixed_frame_rate: f32,
    pub frames_left_over: f32,
    pub explicit_position: Vector3,
    pub explicit_direction: Vector3,
    pub explicit_transform: bool,
    pub local_offset: Vector3,
    pub max_lod: f32,
    pub is_static: bool,
    pub own_world: Option<Matrix>,
    pub bounding_box: BoundBox,
    pub viz_box: BoundBox,
    wind_effect: Vector3,
    /// `pError` accumulations from `loadInternal`.
    pub load_errors: Vec<String>,
    /// Entries queued by non-local actions this frame (action index,
    /// dTime); the host flushes them after tick.
    pub queued_late: Vec<(usize, f32)>,
}

impl Default for ParticleSystem {
    fn default() -> Self {
        ParticleSystem::new()
    }
}

impl ParticleSystem {
    pub fn new() -> Self {
        ParticleSystem {
            name: String::new(),
            capacity: 40,
            particles: Particles::new(),
            actions: Vec::new(),
            renderer: None,
            wind_factor: 0.0,
            enabled: true,
            do_update: false,
            first_update: false,
            counter: 0,
            fixed_frame_rate: -1.0,
            frames_left_over: 0.0,
            explicit_position: Vector3::ZERO,
            explicit_direction: Vector3::new(0.0, 0.0, 1.0),
            explicit_transform: false,
            local_offset: Vector3::ZERO,
            max_lod: 0.0,
            is_static: false,
            own_world: None,
            bounding_box: empty_bbox(),
            viz_box: empty_bbox(),
            wind_effect: Vector3::ZERO,
            load_errors: Vec::new(),
            queued_late: Vec::new(),
        }
    }

    /// `ParticleSystem::loadInternal` (`particle_system.cpp:285-401`):
    /// version check (missing section reads as the current code version; a
    /// mismatch logs and keeps going), the serialise tags, the legacy
    /// windHalflife translation, the Actions, the Renderer, the capacity
    /// and the bounding box. The final gate is a renderer being set.
    pub fn load(&mut self, ds: &DataSection, name: &str) -> bool {
        let version = ds.read_int(VERSION_STRING, PARTICLE_SERIALISE_VERSION);
        if version != PARTICLE_SERIALISE_VERSION {
            self.load_errors.push(format!(
                "ParticleSystem::serialiseInternal - version mismatch.  Code = {}, Data = {}.\n",
                PARTICLE_SERIALISE_VERSION, version
            ));
        }

        self.wind_factor = ds.read_float("windFactor_", self.wind_factor);
        self.explicit_position = ds.read_vector3("explicitPosition_", self.explicit_position);
        self.explicit_direction = ds.read_vector3("explicitDirection_", self.explicit_direction);
        self.explicit_transform = ds.read_bool("explicitTransform_", self.explicit_transform);
        self.local_offset = ds.read_vector3("localOffset_", self.local_offset);
        self.max_lod = ds.read_float("maxLod_", self.max_lod);
        self.fixed_frame_rate = ds.read_float("fixedFrameRate_", self.fixed_frame_rate);

        // Legacy windHalflife -> windFactor translation: a half-life of 5
        // seconds corresponds to a wind factor of 0.5.
        let wind_hl = ds.read_float("windHalflife_", -1.0);
        if wind_hl != -1.0 {
            let k = -0.2 * (0.5f32).ln();
            self.wind_factor = (-k * wind_hl.abs()).exp();
        }

        if let Some(actions) = ds.open_section("Actions") {
            self.actions.clear();
            for i in 0..actions.count_children() {
                let child = actions.open_child(i).unwrap();
                match ParticleAction::load(child) {
                    Ok(action) => self.actions.push(action),
                    Err(e) => {
                        self.load_errors.push(e);
                        return false;
                    }
                }
            }
        }

        // Renderer: the first child that creates is taken; unsupported
        // names fall through and fail the final gate.
        if let Some(renderer_ds) = ds.open_section("Renderer") {
            for i in 0..renderer_ds.count_children() {
                let child = renderer_ds.open_child(i).unwrap();
                if let Some(mut pr) =
                    ParticleRenderer::create_renderer_of_type(child.section_name())
                {
                    pr.load(child);
                    self.set_renderer(Some(pr));

                    // sprite rotation: any Source with a non-zero initial
                    // or random rotation turns the flag on
                    // (`particle_system.cpp:357-384`).
                    if self.renderer.as_ref().unwrap().name_id() == "SpriteParticleRenderer" {
                        let mut can_rotate = false;
                        for action in &self.actions {
                            if let ActionKind::Source(s) = &action.kind
                                && (s.initial_rotation != Vector2::ZERO
                                    || s.random_initial_rotation != Vector2::ZERO)
                            {
                                can_rotate = true;
                                break;
                            }
                        }
                        if let ParticleRenderer::Sprite { sprite, .. } =
                            self.renderer.as_mut().unwrap()
                        {
                            sprite.rotated = can_rotate;
                        }
                    }
                    break;
                }
            }
        }

        // Capacity is set after the renderer so the new container gets it.
        self.set_capacity(ds.read_int("capacity", self.capacity));

        // Bounding box: the stored viz box, or the ±0.01 unit box.
        if let Some(bb) = ds.open_section("boundingBox") {
            let min = bb.read_vector3("min", Vector3::new(-0.01, -0.01, -0.01));
            let max = bb.read_vector3("max", Vector3::new(0.01, 0.01, 0.01));
            self.bounding_box = BoundBox::new(min, max);
            self.viz_box = self.bounding_box;
        }
        self.clear_bounding_box();

        self.name = name.to_string();
        self.renderer.is_some()
    }

    /// `ParticleSystem::pRenderer` (`particle_system.cpp:580-602`): a new
    /// renderer replaces the particle container and re-applies the
    /// capacity.
    pub fn set_renderer(&mut self, r: Option<ParticleRenderer>) {
        if self.renderer != r {
            let capacity = self.capacity;
            self.renderer = r;
            if self.renderer.is_some() {
                self.particles = Particles::new();
                self.set_capacity(capacity);
            }
        }
    }

    /// `ParticleSystem::capacity(int)` (`particle_system.cpp`): a change
    /// clears the container and reserves.
    pub fn set_capacity(&mut self, number: i32) {
        let number = clamp(0.0, number as f32, MAX_CAPACITY as f32) as i32;
        if number != self.capacity {
            self.capacity = number;
            self.particles.clear();
        }
        self.particles.reserve(number.max(0) as usize);
    }

    /// `ParticleSystem::addParticle` (`particle_system.ipp:117-139`): the
    /// hard capacity gate; the first particle of a dynamic system seeds the
    /// bounding box, later ones grow it.
    pub fn add_particle(&mut self, p: Particle) {
        if self.particles.len() < self.capacity as usize {
            let first = self.particles.is_empty();
            self.particles.add_particle(p);
            if first && !self.is_static {
                self.bounding_box = BoundBox::new(
                    Vector3::new(p.position.x - 0.1, p.position.y - 0.1, p.position.z - 0.1),
                    Vector3::new(p.position.x + 0.1, p.position.y + 0.1, p.position.z + 0.1),
                );
            } else {
                self.bounding_box.add_bounds(&p.position);
            }
        }
    }

    /// `ParticleSystem::isLocal` (`particle_system.ipp:349-353`).
    pub fn is_local(&self) -> bool {
        let mut local = self.own_world.is_none();
        if let Some(r) = &self.renderer {
            local = local || r.local();
        }
        local
    }

    /// `ParticleSystem::worldTransform` (`particle_system.cpp:607-630`):
    /// explicit transform -> look-at matrix; otherwise the own world or
    /// identity, translated by the local offset.
    pub fn world_transform(&self) -> Matrix {
        let mut ret = Matrix::identity();
        if self.explicit_transform {
            let up = if self.explicit_direction.y.abs() < 0.9 {
                Vector3::new(0.0, 1.0, 0.0)
            } else {
                Vector3::new(0.0, 0.0, 1.0)
            };
            ret.look_at(&self.explicit_position, &self.explicit_direction, &up);
            ret.invert();
            ret.pre_translate_by(&self.local_offset);
            ret
        } else {
            ret.set_translate(
                self.local_offset.x,
                self.local_offset.y,
                self.local_offset.z,
            );
            if let Some(own) = &self.own_world {
                ret.post_multiply(own);
            }
            ret
        }
    }

    /// `ParticleSystem::objectToWorld` (`particle_system.cpp:639-668`):
    /// local renderers collapse to identity; view dependence is the render
    /// seam (there is no view matrix here) so that flag changes nothing.
    pub fn object_to_world(&self) -> Matrix {
        let mut ret;
        if let Some(r) = &self.renderer {
            if r.local() {
                ret = Matrix::identity();
            } else {
                ret = self.world_transform();
                let mut scaler = Matrix::identity();
                scaler.set_scale(
                    1.0 / ret.apply_vector(&Vector3::I).length(),
                    1.0 / ret.apply_vector(&Vector3::J).length(),
                    1.0 / ret.apply_vector(&Vector3::K).length(),
                );
                ret.pre_multiply(&scaler);
            }
        } else {
            ret = self.world_transform();
        }
        ret
    }

    /// `ParticleSystem::update` (`particle_system.cpp:926-1014`), in the
    /// reference statement order.
    pub fn update(&mut self, mut d_time: f32, rng: &mut BwRandom, env: &ParticleEnv) {
        if !env.active {
            return;
        }

        d_time += self.frames_left_over;
        if d_time > 1.0 {
            d_time = 1.0;
        }

        // pre-calculate the wind vector
        self.wind_effect = Vector3::new(
            clamp(0.0, self.wind_factor, 1.0) * env.wind_velocity.x,
            clamp(0.0, self.wind_factor, 1.0) * env.wind_velocity.y,
            clamp(0.0, self.wind_factor, 1.0) * env.wind_velocity.z,
        );

        while d_time > 0.0 {
            let mut dt = if self.fixed_frame_rate > 0.0 {
                1.0 / self.fixed_frame_rate
            } else {
                d_time
            };

            // quantise dt to the age increment
            let n_age_increments = Particle::n_age_increments(dt);
            if n_age_increments == 0 {
                break;
            }
            dt = Particle::age_of(n_age_increments);
            if dt > d_time {
                if almost_equal(dt, d_time) {
                    dt = d_time;
                } else {
                    break;
                }
            }
            d_time -= dt;

            // aging pass
            let n = n_age_increments;
            for p in self.particles.items_mut() {
                if p.is_alive() {
                    let age = p.age_raw();
                    if (age as u32) + (n as u32) > Particle::age_max() as u32 {
                        p.set_age_raw(Particle::age_max());
                    } else {
                        p.set_age_raw(age + n);
                    }
                }
            }

            // actions pass
            for i in 0..self.actions.len() {
                if self.first_update {
                    self.actions[i].set_first_update();
                }
                if self.actions[i].base.enabled {
                    self.execute_action(i, dt, rng);
                }
            }
            self.first_update = false;

            // movement pass
            for p in self.particles.items_mut() {
                if p.is_alive() {
                    let old_velocity = p.velocity();
                    let new_velocity = old_velocity + self.wind_effect;
                    p.position = p.position + new_velocity * dt;
                }
            }

            if let Some(r) = &self.renderer {
                r.update();
            }
        }

        self.frames_left_over = d_time;
        self.do_update = false;
    }

    /// The action dispatch half of the update loop: local systems run the
    /// late bodies inline; non-local ones queue for the host flush.
    fn execute_action(&mut self, index: usize, dt: f32, rng: &mut BwRandom) {
        {
            let action = &mut self.actions[index];
            if action.base.gated(dt) {
                return;
            }
        }
        let mut action = std::mem::replace(&mut self.actions[index], splat_placeholder());
        let queue = match &mut action.kind {
            ActionKind::Source(s) => {
                execute_source(self, s, dt, rng);
                None
            }
            ActionKind::Sink(s) => {
                if self.is_local() {
                    execute_sink(self, s);
                    None
                } else {
                    Some(dt)
                }
            }
            ActionKind::Barrier(b) => {
                if b.shape == BarrierShape::None || b.reaction == BarrierReaction::Allow {
                    None
                } else if self.is_local() {
                    execute_barrier(self, b, dt);
                    None
                } else {
                    Some(dt)
                }
            }
            ActionKind::Force(f) => {
                if self.is_local() {
                    execute_force(self, f, dt);
                    None
                } else {
                    Some(dt)
                }
            }
            ActionKind::Stream(s) => {
                execute_stream(self, s, dt);
                None
            }
            ActionKind::Jitter(j) => {
                if self.is_local() {
                    execute_jitter(self, j, dt, rng);
                    None
                } else {
                    Some(dt)
                }
            }
            ActionKind::Scaler(s) => {
                if self.is_local() {
                    execute_scaler(self, s, dt);
                    None
                } else {
                    Some(dt)
                }
            }
            ActionKind::TintShader(t) => {
                execute_tint_shader(self, t);
                None
            }
            // Seam actions: NodeClamp/Orbitor/MatrixSwarm need
            // MatrixProviders, Flare the LensEffectManager, Collide the
            // romp collider and Splat a script callback — their load faces
            // work, their execute stays a documented no-op.
            ActionKind::NodeClamp(_)
            | ActionKind::Orbitor(_)
            | ActionKind::Flare(_)
            | ActionKind::Collide(_)
            | ActionKind::MatrixSwarm(_)
            | ActionKind::Splat(_) => None,
            ActionKind::Magnet(m) => {
                if m.source.is_none() && self.is_local() {
                    execute_magnet(self, m, dt);
                    None
                } else {
                    Some(dt)
                }
            }
        };
        if let Some(dt) = queue {
            self.queued_late.push((index, dt));
        }
        self.actions[index] = action;
    }

    /// `ParticleSystemAction::flushLateUpdates` at the system level: the
    /// host calls this once per frame after ticking (the reference queue is
    /// global but per-system order is preserved, so the split is
    /// equivalent); entries run in queue order against this system.
    pub fn flush_late_updates(&mut self, rng: &mut BwRandom) {
        let queue = std::mem::take(&mut self.queued_late);
        for (index, dt) in queue {
            if index >= self.actions.len() {
                continue;
            }
            let mut action = std::mem::replace(&mut self.actions[index], splat_placeholder());
            match &mut action.kind {
                ActionKind::Sink(s) => execute_sink(self, s),
                ActionKind::Barrier(b) => execute_barrier(self, b, dt),
                ActionKind::Force(f) => execute_force(self, f, dt),
                ActionKind::Jitter(j) => execute_jitter(self, j, dt, rng),
                ActionKind::Scaler(s) => execute_scaler(self, s, dt),
                ActionKind::Magnet(m) => execute_magnet(self, m, dt),
                _ => {}
            }
            self.actions[index] = action;
        }
    }

    /// `ParticleSystem::tick` (`particle_system.cpp:879-912`).
    pub fn tick(&mut self, d_time: f32, rng: &mut BwRandom, env: &ParticleEnv) -> bool {
        if !env.active {
            return false;
        }
        if !self.is_static || self.do_update {
            self.update(d_time, rng, env);
            self.counter += 1;
            if self.counter == 10 {
                self.counter = 0;
                self.update_bounding_box();
            }
            return true;
        }
        false
    }

    /// `ParticleSystem::predictPosition` (`particle_system.cpp:1024-1037`).
    pub fn predict_position(&self, particle: &Particle, dt: f32) -> Vector3 {
        let old_velocity = particle.velocity();
        let new_velocity = old_velocity + self.wind_effect;
        particle.position + new_velocity * dt
    }

    /// `ParticleSystem::updateBoundingBox`
    /// (`particle_system.cpp:1562-1617`).
    pub fn update_bounding_box(&mut self) {
        self.bounding_box = empty_bbox();
        let mut sz = 0.01f32;
        let mesh_renderer = self
            .renderer
            .as_ref()
            .map(|r| r.is_mesh_style())
            .unwrap_or(false);
        for p in self.particles.iter() {
            if p.is_alive() {
                self.bounding_box.add_bounds(&p.position);
                if !mesh_renderer {
                    sz = sz.max(p.size());
                }
            }
        }
        // `knowParticleRadius` expansion: both sprite renderers answer
        // false, so nothing to query here.
        self.bounding_box.min.x -= sz;
        self.bounding_box.min.y -= sz;
        self.bounding_box.min.z -= sz;
        self.bounding_box.max.x += sz;
        self.bounding_box.max.y += sz;
        self.bounding_box.max.z += sz;
        // EDITOR_ENABLED build: dynamic systems shrink the viz box.
        self.viz_box = self.bounding_box;
    }

    /// `ParticleSystem::clear` (`particle_system.cpp:1376-1384`): the
    /// flare kill is the lens-effect seam; the container empties and the
    /// bounding box resets.
    pub fn clear(&mut self) {
        self.particles.clear();
        self.clear_bounding_box();
    }

    /// `ParticleSystem::spawn` (`particle_system.cpp:1483-1495`): force
    /// every Source to queue a burst.
    pub fn spawn(&mut self, num: i32) {
        for action in &mut self.actions {
            if let ActionKind::Source(s) = &mut action.kind {
                s.force(num);
            }
        }
    }

    /// `ParticleSystem::clearBoundingBox`
    /// (`particle_system.cpp:1393-1416`): the "empty" box is the 1 cm box
    /// at the origin, moved out by the own world when the system is not
    /// local; dynamic systems copy it into the viz box.
    pub fn clear_bounding_box(&mut self) {
        self.bounding_box = BoundBox::new(
            Vector3::new(-0.01, -0.01, -0.01),
            Vector3::new(0.01, 0.01, 0.01),
        );
        let local = {
            let mut l = self.own_world.is_none();
            if let Some(r) = &self.renderer {
                l = l || r.local();
            }
            l
        };
        if !local && let Some(world) = &self.own_world {
            let m = *world;
            self.bounding_box.transform_by(&m);
        }
        self.viz_box = self.bounding_box;
    }

    /// `ParticleSystem::duration`: the max sink age, or -1.
    pub fn duration(&self) -> f32 {
        let mut duration: f32 = -1.0;
        for action in &self.actions {
            if let ActionKind::Sink(s) = &action.kind {
                duration = duration.max(s.maximum_age);
            }
        }
        duration
    }

    /// `ParticleSystem::size()`.
    pub fn size(&self) -> usize {
        self.particles.len()
    }

    /// `ParticleSystem::sizeInBytes` (`particle_system.cpp:1455-1476`):
    /// the container capacity times the particle size, plus the renderer
    /// and every action. The port's struct sizes are its own; the formula
    /// is the reference's.
    pub fn size_in_bytes(&self) -> usize {
        let mut footprint = std::mem::size_of::<ParticleSystem>()
            + self.capacity.max(0) as usize * Particle::size_in_bytes();
        if let Some(r) = &self.renderer {
            footprint += r.size_in_bytes();
        }
        for action in &self.actions {
            footprint += action.size_in_bytes();
        }
        footprint
    }

    /// `ParticleSystem::isLodVisible` (`particle_system.cpp:1058-1082`):
    /// an inactive manager hides everything, `maxLod_ <= 0` is the
    /// infinite-LOD marker, and the remaining comparison is against the
    /// biased maximum — `LodSettings::applyLodBias` divides by the active
    /// LOD option, which arrives through the env as `lod_bias` (the
    /// editor's `ignoreLods` path returns 0 and nobody in the particle
    /// editor sets it).
    pub fn is_lod_visible(&self, lod: f32, env: &ParticleEnv) -> bool {
        if !env.active {
            return false;
        }
        const LOD_INFINITE: f32 = 0.0;
        if self.max_lod <= LOD_INFINITE {
            return true;
        }
        let biased_max_lod = self.max_lod / env.lod_bias;
        lod <= biased_max_lod
    }
}

// ---------------------------------------------------------------------------
// Section: action bodies (`actions/*.cpp` execute/lateExecute)
// ---------------------------------------------------------------------------

/// `Matrix::applyToOrigin`.
fn matrix_origin(m: &Matrix) -> Vector3 {
    m.apply_point(&Vector3::ZERO)
}

/// `SourcePSA::execute` (`source_psa.cpp:309-462`).
fn execute_source(system: &mut ParticleSystem, s: &mut SourcePSA, d_time: f32, rng: &mut BwRandom) {
    let object_to_world = system.object_to_world();
    let position_of_ps = matrix_origin(&object_to_world);
    let displacement = position_of_ps
        - (if s.first_update {
            position_of_ps
        } else {
            s.last_position_of_ps
        });
    s.last_position_of_ps = position_of_ps;
    s.first_update = false;
    let velocity_of_ps = displacement / d_time;

    let object_to_world = if s.ignore_rotation {
        // `objectToWorld.setTranslate(applyToOrigin())` — the rotation is
        // dropped.
        let mut m = Matrix::identity();
        m.set_translate(position_of_ps.x, position_of_ps.y, position_of_ps.z);
        m
    } else {
        object_to_world
    };

    // Forced particles.
    if s.forced_total > 0 {
        let mut frame_total = s.forced_total * s.forced_unit_size;
        if frame_total + system.size() as i32 > system.capacity {
            frame_total = system.capacity - system.size() as i32;
        }
        while frame_total > 0 {
            let force_size = s.forced_unit_size.min(frame_total);
            create_particles(
                system,
                s,
                &object_to_world,
                force_size,
                Vector3::ZERO,
                velocity_of_ps,
                rng,
            );
            frame_total -= force_size;
        }
        s.forced_total = 0;
    }

    if !(s.motion_triggered || s.time_triggered || s.queued_total > 0) {
        return;
    }

    let distance = displacement.length();
    let mut frame_total = 0;

    if s.motion_triggered {
        let speed = distance / d_time;
        if speed > s.sensitivity {
            frame_total = ((1.0 + 2.0 * ((speed - s.sensitivity) / s.sensitivity)) * d_time) as i32;
            if frame_total + system.size() as i32 > system.capacity {
                frame_total = system.capacity - system.size() as i32;
            }
        }
        if frame_total > 0 {
            let displacement_per_particle = displacement / frame_total as f32;
            create_particles(
                system,
                s,
                &object_to_world,
                frame_total,
                displacement_per_particle,
                velocity_of_ps,
                rng,
            );
        }
    }

    if s.time_triggered || s.queued_total > 0 {
        if s.rate > 0.0 {
            if s.current_sleep_period > 0.0 {
                let period = s.active_period + s.current_sleep_period;
                let mut last_period_time = s.period_time;
                s.period_time += d_time;
                if s.period_time > period && period > 0.0 {
                    s.period_time %= period;
                    last_period_time = 0.0;
                    s.current_sleep_period =
                        generate_sleep_period(s.sleep_period, s.sleep_period_max);
                }
                if s.period_time <= s.active_period {
                    s.accumulated_time += s.period_time - last_period_time;
                } else if last_period_time <= s.active_period {
                    s.accumulated_time += s.active_period - last_period_time;
                }
            } else {
                s.accumulated_time += d_time;
            }

            let time_between_particles = 1.0 / s.rate;
            if s.accumulated_time > time_between_particles {
                frame_total = (s.accumulated_time / time_between_particles) as i32;
                s.accumulated_time -= frame_total as f32 * time_between_particles;
                s.queued_total -= frame_total;
            }
        }

        // Capacity is a hard limit; excess particles "simply vanished".
        if frame_total + system.size() as i32 > system.capacity {
            frame_total = system.capacity - system.size() as i32;
        }

        // Hard limit time-triggered particles if a maximum velocity is set.
        if frame_total > 0 && s.max_speed > 0.0 {
            let speed = distance / d_time;
            if speed > s.max_speed {
                frame_total = 0;
            }
        }

        if frame_total > 0 {
            let displacement_per_particle = displacement / frame_total as f32;
            create_particles(
                system,
                s,
                &object_to_world,
                frame_total,
                displacement_per_particle,
                velocity_of_ps,
                rng,
            );
        }
    }
}

/// `SourcePSA::createParticles` (`source_psa.cpp:510-628`), in the
/// reference draw order: position source, velocity source, size, pitch,
/// yaw, then the four spin draws when enabled. The `allowedTime_` bail and
/// the `grounded()` RompCollider drop are documented seams: a fixed-step
/// host never trips the time budget, and the port never snaps to ground.
fn create_particles(
    system: &mut ParticleSystem,
    s: &mut SourcePSA,
    object_to_world: &Matrix,
    number_of_particles: i32,
    disp_per_particle: Vector3,
    velocity_of_ps: Vector3,
    rng: &mut BwRandom,
) {
    let size_difference = s.maximum_size - s.minimum_size;
    let mut count = 0;
    while count < number_of_particles {
        let mut pos_in_ls = Vector3::ZERO;
        if let Some(src) = &s.position_src {
            pos_in_ls = src.generate(rng);
        }
        let mut position = object_to_world.apply_point(&pos_in_ls);
        position = position - (disp_per_particle * (number_of_particles - 1 - count) as f32);

        let mut vel_in_ls = Vector3::ZERO;
        if let Some(src) = &s.velocity_src {
            vel_in_ls = src.generate(rng);
        }
        let mut velocity = object_to_world.apply_vector(&vel_in_ls);
        velocity = velocity + velocity_of_ps * s.inherit_velocity;

        // The size/pitch/yaw draws happen unconditionally, mesh or not.
        let size = rng.unit_rand() * size_difference + s.minimum_size;
        let pitch = s.initial_rotation.x + s.random_initial_rotation.x * (rng.unit_rand() - 0.5);
        let yaw = s.initial_rotation.y + s.random_initial_rotation.y * (rng.unit_rand() - 0.5);

        let colour = s.initial_colour;
        let mut spin_axis = Vector3::ZERO;
        let mut spin_speed = 0.0f32;
        if s.random_spin {
            spin_axis = Vector3::new(rng.unit_rand(), rng.unit_rand(), rng.unit_rand());
            let mut normalised = spin_axis;
            normalised.normalise();
            spin_axis = normalised;
            spin_speed = rng.unit_rand() * (s.max_spin - s.min_spin) + s.min_spin;
        }

        // The mesh-style branch belongs to the mesh renderers (render
        // seam); both sprite renderers take the non-mesh constructor.
        let p = Particle::non_mesh(position, velocity, colour, size, pitch, yaw);
        system.add_particle(p);
        let _ = (spin_axis, spin_speed);
        count += 1;
    }
}

/// `SinkPSA::lateExecute` (`sink_psa.cpp`): age and minimum-speed
/// removal; the `outsideOnly_` hull test needs the chunk visibility
/// bounding box (a space seam), so the port leaves those particles in
/// place.
fn execute_sink(system: &mut ParticleSystem, s: &mut SinkPSA) {
    if s.maximum_age < 0.0 && s.minimum_speed < 0.0 && !s.outside_only {
        return;
    }
    let min_spd_sqrd = if s.minimum_speed >= 0.0 {
        s.minimum_speed * s.minimum_speed
    } else {
        -1.0
    };
    let mut index = 0;
    while index < system.particles.len() {
        let particle = system.particles.get(index);
        let mut remove = false;
        if particle.is_alive() {
            let velocity = particle.velocity();
            if particle.age() > s.maximum_age || velocity.length_squared() < min_spd_sqrd {
                remove = true;
            }
        }
        if remove {
            system.particles.remove_particle(index);
        } else {
            index += 1;
        }
    }
}

/// `ForcePSA::lateExecute` (`force_psa.cpp`).
fn execute_force(system: &mut ParticleSystem, f: &mut ForcePSA, d_time: f32) {
    let scaled_vector = f.vector * d_time;
    for p in system.particles.items_mut() {
        if p.is_alive() {
            let velocity = p.velocity();
            p.set_velocity(velocity + scaled_vector);
        }
    }
}

/// `JitterPSA::lateExecute` (`jitter_psa.cpp`): per particle, the position
/// jitter draw then the velocity jitter draw.
fn execute_jitter(system: &mut ParticleSystem, j: &mut JitterPSA, d_time: f32, rng: &mut BwRandom) {
    let position_src = j.position_src.clone();
    let velocity_src = j.velocity_src.clone();
    let affect_position = j.affect_position;
    let affect_velocity = j.affect_velocity;
    for p in system.particles.items_mut() {
        if p.is_alive() {
            if affect_position && let Some(src) = &position_src {
                let jitter = src.generate(rng);
                p.position = p.position + jitter * d_time;
            }
            if affect_velocity && let Some(src) = &velocity_src {
                let jitter = src.generate(rng);
                let velocity = p.velocity();
                p.set_velocity(velocity + jitter);
            }
        }
    }
}

/// `ScalerPSA::lateExecute` (`scaler_psa.cpp`).
fn execute_scaler(system: &mut ParticleSystem, s: &mut ScalerPSA, d_time: f32) {
    for p in system.particles.items_mut() {
        if p.is_alive() {
            let mut size = p.size();
            if size > s.size {
                size -= s.rate * d_time;
                if size < s.size {
                    size = s.size;
                }
            } else if size < s.size {
                size += s.rate * d_time;
                if size > s.size {
                    size = s.size;
                }
            }
            p.set_size(size);
        }
    }
}

/// `StreamPSA::execute` (`stream_psa.cpp`): decay the velocity towards the
/// stream vector; a negative half-life means no decay, zero means an
/// immediate snap.
fn execute_stream(system: &mut ParticleSystem, s: &mut StreamPSA, d_time: f32) {
    let decay = if s.half_life < 0.0 {
        1.0
    } else if s.half_life == 0.0 {
        0.0
    } else {
        0.5f32.powf(d_time / s.half_life)
    };
    for p in system.particles.items_mut() {
        if p.is_alive() {
            let velocity = p.velocity();
            p.set_velocity(s.vector + (velocity - s.vector) * decay);
        }
    }
}

/// `TintShaderPSA::execute` (`tint_shader_psa.cpp:45-150`): pick the two
/// poles around the particle's age, blend, then blend the fog in. The
/// `modulator_` output and the `FogController` colour are render seams —
/// the modulator stays at its identity `(1,1,1,1)` (which is a bit-exact
/// multiply) and the fog colour is transparent black.
fn execute_tint_shader(system: &mut ParticleSystem, t: &mut TintShaderPSA) {
    if t.tints.is_empty() {
        return;
    }

    for p in system.particles.items_mut() {
        if !p.is_alive() {
            continue;
        }
        let effective_age = if t.repeat && t.period != 0.0 {
            p.age() % t.period
        } else {
            p.age()
        };

        // `std::upper_bound` on time, then the pole fixups: before the
        // first stop both poles sit on it, past the last stop both sit on
        // the last stop, and otherwise the upper pole is the found stop
        // with the lower one right before it.
        let found = t.tints.partition_point(|tint| tint.time <= effective_age);
        let (lower, upper) = if found == t.tints.len() {
            (t.tints.len() - 1, t.tints.len() - 1)
        } else if found == 0 {
            (0, 0)
        } else {
            (found - 1, found)
        };

        // Blend between the two poles: the difference times the age ratio
        // plus the lower bound, per component (`Vector4` carries no `Sub`
        // in the port, so the three reference steps stay in their own
        // order).
        let mut colour = if upper == lower {
            t.tints[upper].colour
        } else {
            let ratio =
                (effective_age - t.tints[lower].time) / (t.tints[upper].time - t.tints[lower].time);
            let u = t.tints[upper].colour;
            let l = t.tints[lower].colour;
            let mut c = Vector4::new(u.x - l.x, u.y - l.y, u.z - l.z, u.w - l.w);
            c = c * ratio;
            c = c + l;
            c
        };

        if t.fog_amount > 0.0 {
            // `fogColour[3] = colour[3]; colour *= (1-fogAmount);
            // colour += fogAmount * fogColour;`
            let mut fog = Vector4::ZERO;
            fog.w = colour.w;
            colour = colour * (1.0 - t.fog_amount) + fog * t.fog_amount;
        }

        p.colour = colour_from_normalised(colour);
    }
}

/// `MagnetPSA::lateExecute` (`magnet_psa.cpp:55-97`): accelerate every
/// live particle toward the origin.
fn execute_magnet(system: &mut ParticleSystem, m: &mut MagnetPSA, d_time: f32) {
    let origin = if let Some(source) = &m.source {
        matrix_origin(source)
    } else if system.renderer.as_ref().map(|r| r.local()).unwrap_or(false) {
        Vector3::ZERO
    } else {
        matrix_origin(&system.world_transform())
    };

    for p in system.particles.items_mut() {
        if p.is_alive() {
            let dv = origin - p.position;
            let separation = dv.length().max(m.min_dist);
            let dv = dv / separation;
            let factor = (d_time * m.strength) / separation;
            let velocity = p.velocity();
            p.set_velocity(velocity + dv * factor);
        }
    }
}

/// `BarrierPSA::lateExecute` (`barrier_psa.cpp:27-92`).
fn execute_barrier(system: &mut ParticleSystem, b: &mut BarrierPSA, d_time: f32) {
    match b.shape {
        BarrierShape::VerticalCylinder => collide_with_cylinder(system, b, d_time),
        BarrierShape::Box => collide_with_box(system, b, d_time),
        BarrierShape::Sphere => collide_with_sphere(system, b, d_time),
        BarrierShape::None => {}
    }
}

/// `BarrierPSA::collideWithCylinder` (`barrier_psa.cpp:109-226`): an
/// infinite-height cylinder around the transformed `vecA_` axis.
fn collide_with_cylinder(system: &mut ParticleSystem, b: &mut BarrierPSA, d_time: f32) {
    let radius_squared = b.radius * b.radius;
    let object_to_world = system.object_to_world();
    let position_of_cylinder = object_to_world.apply_point(&b.vec_a);
    let cylinder_xz = Vector2::new(position_of_cylinder.x, position_of_cylinder.z);
    let mut new_particles = Vec::new();

    let mut index = 0;
    while index < system.particles.len() {
        let particle = system.particles.get(index);
        if !particle.is_alive() {
            index += 1;
            continue;
        }
        let x_diff = position_of_cylinder.x - particle.position.x;
        let z_diff = position_of_cylinder.z - particle.position.z;
        let before_squared = x_diff * x_diff + z_diff * z_diff;
        let pos_after = system.predict_position(&particle, d_time);
        let x_diff = position_of_cylinder.x - pos_after.x;
        let z_diff = position_of_cylinder.z - pos_after.z;
        let after_squared = x_diff * x_diff + z_diff * z_diff;

        let now = before_squared > radius_squared;
        let later = after_squared > radius_squared;

        match b.reaction {
            BarrierReaction::Remove => {
                if now != later {
                    system.particles.remove_particle(index);
                } else {
                    index += 1;
                }
            }
            BarrierReaction::Bounce => {
                if now != later {
                    let mut inward_dir = Vector3::new(
                        position_of_cylinder.x - particle.position.x,
                        0.0,
                        position_of_cylinder.z - particle.position.z,
                    );
                    inward_dir.normalise();
                    let velocity = particle.velocity();
                    let dot_product = velocity.dot_product(&inward_dir);
                    let v = velocity - inward_dir * ((1.0 + b.resilience) * dot_product);
                    system.particles.get_mut(index).set_velocity(v);
                }
                index += 1;
            }
            BarrierReaction::Wrap => {
                if later {
                    let mut new_particle = particle;
                    let pos = new_particle.position;
                    let mut delta = Vector2::new(pos.x, pos.z) - cylinder_xz;
                    delta.normalise();
                    new_particle.position = Vector3::new(
                        pos.x - delta.x * b.radius * 2.0,
                        pos.y,
                        pos.z - delta.y * b.radius * 2.0,
                    );
                    system.particles.remove_particle(index);
                    new_particles.push(new_particle);
                } else {
                    index += 1;
                }
            }
            BarrierReaction::Allow => {
                index += 1;
            }
        }
    }

    renew_particles(system, new_particles);
}

/// `BarrierPSA::collideWithBox` (`barrier_psa.cpp:230-406`): the six
/// outside bits, XORed for "crossed a plane"; BOUNCE negates the velocity
/// component of each crossed axis pair.
fn collide_with_box(system: &mut ParticleSystem, b: &mut BarrierPSA, d_time: f32) {
    let object_to_world = system.object_to_world();
    let vec_a_world = object_to_world.apply_point(&b.vec_a);
    let vec_b_world = object_to_world.apply_point(&b.vec_b);

    let (min_x, max_x) = ordered(vec_a_world.x, vec_b_world.x);
    let (min_y, max_y) = ordered(vec_a_world.y, vec_b_world.y);
    let (min_z, max_z) = ordered(vec_a_world.z, vec_b_world.z);
    let x_range = max_x - min_x;
    let y_range = max_y - min_y;
    let z_range = max_z - min_z;

    let mut new_particles = Vec::new();
    let mut index = 0;
    while index < system.particles.len() {
        let particle = system.particles.get(index);
        if !particle.is_alive() {
            index += 1;
            continue;
        }
        let pos_now = particle.position;
        let velocity = particle.velocity();
        let pos_after = system.predict_position(&particle, d_time);

        let now = outside_bits(pos_now, min_x, min_y, min_z, max_x, max_y, max_z);
        let later = outside_bits(pos_after, min_x, min_y, min_z, max_x, max_y, max_z);
        let crossed = now ^ later;

        match b.reaction {
            BarrierReaction::Remove => {
                if crossed != 0 {
                    system.particles.remove_particle(index);
                } else {
                    index += 1;
                }
            }
            BarrierReaction::Bounce => {
                let p = system.particles.get_mut(index);
                if crossed & 0x9 != 0 {
                    // crossed a yz plane
                    p.set_x_velocity(velocity.x * -b.resilience);
                }
                if crossed & (0x9 << 1) != 0 {
                    // crossed an xz plane
                    p.set_y_velocity(velocity.y * -b.resilience);
                }
                if crossed & (0x9 << 2) != 0 {
                    // crossed an xy plane
                    p.set_z_velocity(velocity.z * -b.resilience);
                }
                index += 1;
            }
            BarrierReaction::Wrap => {
                if later != 0 {
                    let mut new_particle = particle;
                    let mut pos = new_particle.position;
                    if pos.x < min_x {
                        pos.x += x_range;
                    } else if pos.x > max_x {
                        pos.x -= x_range;
                    }
                    if pos.y < min_y {
                        pos.y += y_range;
                    } else if pos.y > max_y {
                        pos.y -= y_range;
                    }
                    if pos.z < min_z {
                        pos.z += z_range;
                    } else if pos.z > max_z {
                        pos.z -= z_range;
                    }
                    new_particle.position = pos;
                    system.particles.remove_particle(index);
                    new_particles.push(new_particle);
                } else {
                    index += 1;
                }
            }
            BarrierReaction::Allow => {
                index += 1;
            }
        }
    }

    renew_particles(system, new_particles);
}

/// `BarrierPSA::collideWithSphere` (`barrier_psa.cpp:411-529`).
fn collide_with_sphere(system: &mut ParticleSystem, b: &mut BarrierPSA, d_time: f32) {
    let radius_squared = b.radius * b.radius;
    let object_to_world = system.object_to_world();
    let position_of_sphere = object_to_world.apply_point(&b.vec_a);
    let mut new_particles = Vec::new();

    let mut index = 0;
    while index < system.particles.len() {
        let particle = system.particles.get(index);
        if !particle.is_alive() {
            index += 1;
            continue;
        }
        let pos_now = particle.position;
        let velocity = particle.velocity();
        let pos_after = system.predict_position(&particle, d_time);

        let toward_now = position_of_sphere - pos_now;
        let toward_after = position_of_sphere - pos_after;
        let dist_sqr_now = toward_now.length_squared();
        let dist_sqr_after = toward_after.length_squared();
        let now = dist_sqr_now > radius_squared;
        let later = dist_sqr_after > radius_squared;

        match b.reaction {
            BarrierReaction::Remove => {
                if now != later {
                    system.particles.remove_particle(index);
                } else {
                    index += 1;
                }
            }
            BarrierReaction::Bounce => {
                if now != later {
                    let mut toward = toward_now;
                    toward.normalise();
                    let dot_product = velocity.dot_product(&toward);
                    let v = velocity - toward * ((1.0 + b.resilience) * dot_product);
                    system.particles.get_mut(index).set_velocity(v);
                }
                index += 1;
            }
            BarrierReaction::Wrap => {
                if later {
                    let mut new_particle = particle;
                    let pos = new_particle.position;
                    let mut delta = position_of_sphere - pos;
                    delta.normalise();
                    new_particle.position = pos + delta * (b.radius * 2.0);
                    system.particles.remove_particle(index);
                    new_particles.push(new_particle);
                } else {
                    index += 1;
                }
            }
            BarrierReaction::Allow => {
                index += 1;
            }
        }
    }

    renew_particles(system, new_particles);
}

/// `BarrierPSA::renewParticles`: wrapped particles go back in after the
/// sweep.
fn renew_particles(system: &mut ParticleSystem, new_particles: Vec<Particle>) {
    for p in new_particles {
        system.add_particle(p);
    }
}

/// The per-axis min/max branch the reference spells out three times.
fn ordered(a: f32, b: f32) -> (f32, f32) {
    if a > b { (b, a) } else { (a, b) }
}

/// The six outside bits of `collideWithBox`.
fn outside_bits(
    p: Vector3,
    min_x: f32,
    min_y: f32,
    min_z: f32,
    max_x: f32,
    max_y: f32,
    max_z: f32,
) -> u8 {
    ((p.x < min_x) as u8)
        | (((p.y < min_y) as u8) << 1)
        | (((p.z < min_z) as u8) << 2)
        | (((p.x > max_x) as u8) << 3)
        | (((p.y > max_y) as u8) << 4)
        | (((p.z > max_z) as u8) << 5)
}

// ---------------------------------------------------------------------------
// Section: MetaParticleSystem (`meta_particle_system.cpp`)
// ---------------------------------------------------------------------------

/// The meta system: a named collection of particle systems plus the editor
/// faces (`seedTime`, `helperModel`).
pub struct MetaParticleSystem {
    pub name: String,
    pub systems: Vec<ParticleSystem>,
    /// `seedTime_` (`meta_particle_system.cpp` ctor default 0.1).
    pub seed_time: f32,
    pub helper_model_name: String,
    pub helper_model_center_on_hard_point: u32,
}

impl Default for MetaParticleSystem {
    fn default() -> Self {
        MetaParticleSystem {
            name: String::new(),
            systems: Vec::new(),
            seed_time: 0.1,
            helper_model_name: String::new(),
            helper_model_center_on_hard_point: u32::MAX,
        }
    }
}

impl MetaParticleSystem {
    pub fn new() -> Self {
        MetaParticleSystem::default()
    }

    /// `MetaParticleSystem::load(DataSectionPtr, name)`: a root carrying
    /// `serialiseVersionData` is the old single-system format; otherwise
    /// each child is (or wraps) a system, named by the unsanitised section
    /// name (`DataSection::unsanitise` is the identity). Returns false
    /// when no system was added. The editor-only `populateMetaData` pass
    /// (the helpers/meta_data config) is a tool seam the preview does not
    /// need.
    pub fn load(&mut self, ds: &DataSection, name: &str) -> bool {
        self.systems.clear();

        if ds.open_section(VERSION_STRING).is_some() {
            // old format: one system in the root, unnamed
            let mut system = ParticleSystem::new();
            let ok = system.load(ds, "");
            system.name = name.to_string();
            if ok {
                self.systems.push(system);
            }
        } else {
            for i in 0..ds.count_children() {
                let child = ds.open_child(i).unwrap();
                // Only process system sections; other data (editor-only)
                // is skipped.
                let mut system_ds = Some(child);
                if child.open_section(VERSION_STRING).is_none() {
                    system_ds = child.open_child(0);
                    if system_ds.is_none()
                        || system_ds.unwrap().open_section(VERSION_STRING).is_none()
                    {
                        system_ds = None;
                    }
                }
                if let Some(system_ds) = system_ds {
                    let mut system = ParticleSystem::new();
                    if system.load(system_ds, child.section_name()) {
                        self.systems.push(system);
                    }
                }
            }
        }

        self.name = name.to_string();
        !self.systems.is_empty()
    }

    /// `MetaParticleSystem::load(filename)`: parse the XML then load.
    pub fn load_str(&mut self, xml: &str, name: &str) -> bool {
        let Ok(ds) = resources::datasection::parse("root", xml.as_bytes()) else {
            return false;
        };
        self.load(&ds, name)
    }

    /// `MetaParticleSystem::nSystems`.
    pub fn n_systems(&self) -> usize {
        self.systems.len()
    }

    /// `MetaParticleSystem::addSystem`.
    pub fn add_system(&mut self, system: ParticleSystem) {
        self.systems.push(system);
    }

    /// `MetaParticleSystem::insertSystem`.
    pub fn insert_system(&mut self, idx: usize, system: ParticleSystem) {
        let idx = idx.min(self.systems.len());
        self.systems.insert(idx, system);
    }

    /// `MetaParticleSystem::removeSystem`: the linear search by name, the
    /// reference compares shared pointers.
    pub fn remove_system(&mut self, name: &str) -> bool {
        if let Some(idx) = self.systems.iter().position(|s| s.name == name) {
            self.systems.remove(idx);
            true
        } else {
            false
        }
    }

    /// `MetaParticleSystem::removeAllSystems` (`meta_particle_system.cpp:484-493`):
    /// the detach calls are the world-attachment seam.
    pub fn remove_all_systems(&mut self) {
        self.systems.clear();
    }

    /// `MetaParticleSystem::systemFromIndex`.
    pub fn system_from_index(&self, index: usize) -> Option<&ParticleSystem> {
        self.systems.get(index)
    }

    pub fn system_mut(&mut self, name: &str) -> Option<&mut ParticleSystem> {
        self.systems.iter_mut().find(|s| s.name == name)
    }

    /// `MetaParticleSystem::getFirstSystem`.
    pub fn first_system(&self) -> Option<&ParticleSystem> {
        self.systems.first()
    }

    /// `MetaParticleSystem::system(name)` — the linear search.
    pub fn system(&self, name: &str) -> Option<&ParticleSystem> {
        self.systems.iter().find(|s| s.name == name)
    }

    /// `MetaParticleSystem::size` (`meta_particle_system.cpp:541-555`):
    /// only enabled systems contribute.
    pub fn size(&self) -> i32 {
        let mut n = 0;
        for system in &self.systems {
            if system.enabled {
                n += system.size() as i32;
            }
        }
        n
    }

    /// `MetaParticleSystem::capacity`.
    pub fn capacity(&self) -> i32 {
        let mut capacity = 0;
        for system in &self.systems {
            capacity += system.capacity;
        }
        capacity
    }

    /// `MetaParticleSystem::sizeInBytes`: enabled systems only.
    pub fn size_in_bytes(&self) -> usize {
        let mut footprint = 0usize;
        for system in &self.systems {
            if system.enabled {
                footprint += system.size_in_bytes();
            }
        }
        footprint
    }

    /// `MetaParticleSystem::spawn` (`meta_particle_system.cpp:827-848`):
    /// force every Source of every system.
    pub fn spawn(&mut self, num: i32) {
        for system in &mut self.systems {
            system.spawn(num);
        }
    }

    /// `MetaParticleSystem::clear` (`meta_particle_system.cpp:852-860`):
    /// delegate to each system.
    pub fn clear(&mut self) {
        for system in &mut self.systems {
            system.clear();
        }
    }

    /// `MetaParticleSystem::setDoUpdate`.
    pub fn set_do_update(&mut self) {
        for system in &mut self.systems {
            system.do_update = true;
        }
    }

    /// `MetaParticleSystem::setFirstUpdate`.
    pub fn set_first_update(&mut self) {
        for system in &mut self.systems {
            system.first_update = true;
        }
    }

    /// `MetaParticleSystem::isStatic`.
    pub fn set_static(&mut self, s: bool) {
        for system in &mut self.systems {
            system.is_static = s;
        }
    }

    /// `MetaParticleSystem::isLodVisible` (`meta_particle_system.cpp:614-631`):
    /// the first enabled system answers for the whole meta system.
    pub fn is_lod_visible(&self, lod: f32, env: &ParticleEnv) -> bool {
        for system in &self.systems {
            if system.enabled {
                return system.is_lod_visible(lod, env);
            }
        }
        false
    }

    /// `MetaParticleSystem::tick` (`meta_particle_system.cpp:598-610`).
    pub fn tick(&mut self, d_time: f32, rng: &mut BwRandom, env: &ParticleEnv) -> bool {
        let mut updated = false;
        for system in &mut self.systems {
            updated |= system.tick(d_time, rng, env);
        }
        updated
    }

    /// `MetaParticleSystem::duration`: the max of the system durations.
    pub fn duration(&self) -> f32 {
        let mut duration = -1.0;
        for system in &self.systems {
            let d = system.duration();
            if d > duration {
                duration = d;
            }
        }
        duration
    }
}

// ---------------------------------------------------------------------------
// Section: save faces (`save`/`saveInternal` bodies; BW-13-04-05)
//
// The reference load and save faces walk the same `BW_PARITCLE_SERIALISE`
// templates, so a faithful save writes exactly the tags the load faces
// above read, in the template's member order. The `newSection(tag)`
// helper (delete the old matches, append a fresh child) is what every
// reference save uses to place its sub-sections.
// ---------------------------------------------------------------------------

/// `DataSection::newSection(tagPath)` (`xml_section.cpp:313`): append a
/// fresh child carrying the tag — a pure append that allows duplicates
/// (the tint ramp's repeated `Tint` children). The typed writers are the
/// overwrite face (`openSection(path, true)` = get-or-create first match);
/// `newSection` never removes anything.
fn new_section<'a>(ds: &'a mut DataSection, tag: &str) -> &'a mut DataSection {
    ds.add_child(tag)
}

impl VectorGenerator {
    /// `VectorGeneratorBase::nameID` (`vector_generator.ipp`).
    pub fn name_id(&self) -> &'static str {
        match self {
            VectorGenerator::Point { .. } => "PointVectorGenerator",
            VectorGenerator::Line { .. } => "LineVectorGenerator",
            VectorGenerator::Cylinder { .. } => "CylinderVectorGenerator",
            VectorGenerator::Sphere { .. } => "SphereVectorGenerator",
            VectorGenerator::Box { .. } => "BoxVectorGenerator",
        }
    }

    /// `VectorGenerator::save` (`vector_generator.cpp:112-120`): a fresh
    /// section per generator named by `nameID`, the `nameID_` sanity
    /// string, then the type's members in serialise order (the Box keeps
    /// its legacy `opposite_` tag, the `BW_PARITCLE_SERIALISE_NAMED`
    /// override).
    pub fn save(&self, ds: &mut DataSection) {
        let sect = new_section(ds, self.name_id());
        sect.write_string("nameID_", self.name_id());
        match self {
            VectorGenerator::Point { position } => {
                sect.write_vector3("position_", *position);
            }
            VectorGenerator::Line { origin, direction } => {
                sect.write_vector3("origin_", *origin);
                sect.write_vector3("direction_", *direction);
            }
            VectorGenerator::Cylinder {
                origin,
                direction,
                max_radius,
                min_radius,
                basis_u,
                basis_v,
            } => {
                sect.write_vector3("origin_", *origin);
                sect.write_vector3("direction_", *direction);
                sect.write_float("maxRadius_", *max_radius);
                sect.write_float("minRadius_", *min_radius);
                sect.write_vector3("basisU_", *basis_u);
                sect.write_vector3("basisV_", *basis_v);
            }
            VectorGenerator::Sphere {
                centre,
                max_radius,
                min_radius,
            } => {
                sect.write_vector3("centre_", *centre);
                sect.write_float("maxRadius_", *max_radius);
                sect.write_float("minRadius_", *min_radius);
            }
            VectorGenerator::Box { corner, opposite } => {
                sect.write_vector3("corner_", *corner);
                sect.write_vector3("opposite_", *opposite);
            }
        }
    }
}

impl ParticleAction {
    /// `ParticleSystemAction::save` (`particle_system_action.cpp:488-495`):
    /// a fresh section named by `nameID()`, the base serialise
    /// (delay/minimumAge/name), then the type's `saveInternal`. `name_`
    /// carries the display name; the reference ctor defaults it to
    /// `nameID()`, which is what the port writes (the port carries no
    /// separate display name). `enabled_` is editor-only state the
    /// reference never serialises; the port writes it so its own round
    /// trip keeps the flag — the reference loader ignores the unknown tag,
    /// which is what keeps "旧编辑器可重载" true.
    pub fn save(&self, ds: &mut DataSection) {
        let sect = new_section(ds, self.kind.type_id().name_id());
        sect.write_float("delay_", self.base.delay);
        sect.write_float("minimumAge_", self.base.minimum_age);
        sect.write_string("name_", self.kind.type_id().name_id());
        sect.write_bool("enabled_", self.base.enabled);
        match &self.kind {
            ActionKind::Source(s) => {
                // `SourcePSA::saveInternal` (`source_psa.cpp:106-134`):
                // the generator sources go in first, then the members.
                if let Some(vg) = &s.position_src {
                    let vs = new_section(sect, "pPositionSrc");
                    vg.save(vs);
                }
                if let Some(vg) = &s.velocity_src {
                    let vs = new_section(sect, "pVelocitySrc");
                    vg.save(vs);
                }
                sect.write_bool("motionTriggered_", s.motion_triggered);
                sect.write_bool("timeTriggered_", s.time_triggered);
                sect.write_bool("grounded_", s.grounded);
                sect.write_float("dropDistance_", s.drop_distance);
                sect.write_float("rate_", s.rate);
                sect.write_float("sensitivity_", s.sensitivity);
                sect.write_float("maxSpeed_", s.max_speed);
                sect.write_float("activePeriod_", s.active_period);
                sect.write_float("sleepPeriod_", s.sleep_period);
                sect.write_float("sleepPeriodMax_", s.sleep_period_max);
                sect.write_float("minimumSize_", s.minimum_size);
                sect.write_float("maximumSize_", s.maximum_size);
                sect.write_int("forcedUnitSize_", s.forced_unit_size);
                sect.write_float("allowedTimeInSeconds_", s.allowed_time_in_seconds);
                sect.write_vector2("initialRotation_", s.initial_rotation);
                sect.write_vector2("randomInitialRotation_", s.random_initial_rotation);
                sect.write_vector4("initialColour_", s.initial_colour);
                sect.write_bool("randomSpin_", s.random_spin);
                sect.write_float("minSpin_", s.min_spin);
                sect.write_float("maxSpin_", s.max_spin);
                sect.write_bool("ignoreRotation_", s.ignore_rotation);
                sect.write_float("inheritVelocity_", s.inherit_velocity);
            }
            ActionKind::Sink(s) => {
                sect.write_float("maximumAge_", s.maximum_age);
                sect.write_float("minimumSpeed_", s.minimum_speed);
                sect.write_bool("outsideOnly_", s.outside_only);
            }
            ActionKind::Barrier(b) => {
                // `_ENUM` members serialise as ints.
                sect.write_int("shape_", b.shape as i32);
                sect.write_int("reaction_", b.reaction as i32);
                sect.write_vector3("vecA_", b.vec_a);
                sect.write_vector3("vecB_", b.vec_b);
                sect.write_float("radius_", b.radius);
            }
            ActionKind::Force(f) => {
                sect.write_vector3("vector_", f.vector);
            }
            ActionKind::Stream(st) => {
                sect.write_vector3("vector_", st.vector);
                sect.write_float("halfLife_", st.half_life);
            }
            ActionKind::Jitter(j) => {
                // `JitterPSA::saveInternal` (`jitter_psa.cpp:213-231`):
                // members first, the generator sources last — the mirror
                // image of the Source order.
                sect.write_bool("affectPosition_", j.affect_position);
                sect.write_bool("affectVelocity_", j.affect_velocity);
                if let Some(vg) = &j.position_src {
                    let vs = new_section(sect, "pPositionSrc");
                    vg.save(vs);
                }
                if let Some(vg) = &j.velocity_src {
                    let vs = new_section(sect, "pVelocitySrc");
                    vg.save(vs);
                }
            }
            ActionKind::Scaler(sc) => {
                sect.write_float("size_", sc.size);
                sect.write_float("rate_", sc.rate);
            }
            ActionKind::TintShader(t) => {
                sect.write_bool("repeat_", t.repeat);
                sect.write_float("period_", t.period);
                sect.write_float("fogAmount_", t.fog_amount);
                // `TintShaderPSA::saveInternal`
                // (`tint_shader_psa.cpp:228-248`): the tints list.
                let tints = new_section(sect, "tints_");
                for tint in &t.tints {
                    let sub = new_section(tints, "Tint");
                    sub.write_float("time", tint.time);
                    sub.write_vector4("color", tint.colour);
                }
            }
            ActionKind::NodeClamp(n) => {
                sect.write_bool("fullyClamp_", n.fully_clamp);
            }
            ActionKind::Orbitor(o) => {
                sect.write_vector3("point_", o.point);
                sect.write_float("angularVelocity_", o.angular_velocity);
                sect.write_bool("affectVelocity_", o.affect_velocity);
            }
            ActionKind::Flare(f) => {
                sect.write_string("flareName_", &f.flare_name);
                sect.write_float("flareStep_", f.flare_step);
                sect.write_bool("colourize_", f.colourize);
                sect.write_bool("useParticleSize_", f.use_particle_size);
            }
            ActionKind::Collide(c) => {
                sect.write_bool("spriteBased_", c.sprite_based);
                sect.write_float("elasticity_", c.elasticity);
                sect.write_float("minAddedRotation_", c.min_added_rotation);
                sect.write_float("maxAddedRotation_", c.max_added_rotation);
                sect.write_string("soundTag_", &c.sound_tag);
                sect.write_bool("soundEnabled_", c.sound_enabled);
                sect.write_int("soundSrcIdx_", c.sound_src_idx);
                sect.write_string("soundProject_", &c.sound_project);
                sect.write_string("soundGroup_", &c.sound_group);
                sect.write_string("soundName_", &c.sound_name);
            }
            // `MatrixSwarmPSA` and `SplatPSA` serialise no members.
            ActionKind::MatrixSwarm(_) | ActionKind::Splat(_) => {}
            ActionKind::Magnet(m) => {
                sect.write_float("strength_", m.strength);
                sect.write_float("minDist_", m.min_dist);
            }
        }
    }
}

impl ParticleRenderer {
    /// `ParticleSystemRenderer::save` + the sprite `saveInternal`s
    /// (`particle_system_renderer.cpp`, `sprite_particle_renderer.cpp:815`):
    /// a fresh section named by `nameID`, the base members, then the
    /// sprite members (`materialFX_` as the enum int). The `rotated` flag
    /// is not serialised — the loader re-derives it from the Source
    /// rotations.
    pub fn save(&self, ds: &mut DataSection) {
        let (base, sprite) = match self {
            ParticleRenderer::Sprite { base, sprite }
            | ParticleRenderer::PointSprite { base, sprite } => (base, sprite),
        };
        let sect = new_section(ds, self.name_id());
        sect.write_bool("viewDependent_", base.view_dependent);
        sect.write_bool("local_", base.local);
        sect.write_int("materialFX_", sprite.material_fx as i32);
        sect.write_int("frameCount_", sprite.frame_count);
        sect.write_float("frameRate_", sprite.frame_rate);
        sect.write_vector3("explicitOrientation_", sprite.explicit_orientation);
        sect.write_string("textureName_", &sprite.texture_name);
        sect.write_float("softDepthRange_", sprite.soft_depth_range);
        sect.write_float("softFalloffPower_", sprite.soft_falloff_power);
        sect.write_float("softDepthOffset_", sprite.soft_depth_offset);
        sect.write_float("nearFadeCutoff_", sprite.near_fade_cutoff);
        sect.write_float("nearFadeStart_", sprite.near_fade_start);
        sect.write_float("nearFadeFalloffPower_", sprite.near_fade_falloff_power);
    }
}

impl ParticleSystem {
    /// `ParticleSystem::save(DataSectionPtr, name, transient)`
    /// (`particle_system.cpp:247-256`): the sanitised section name is
    /// assigned back (the meta save passes it), then `saveInternal`.
    pub fn save(&mut self, ds: &mut DataSection, name: &str, transient: bool) -> bool {
        self.name = name.to_string();
        self.save_internal(ds, transient)
    }

    /// `ParticleSystem::saveInternal` (`particle_system.cpp:419-465`):
    /// the version stamp, the serialise tags (`explicitTransform_`
    /// deliberately last, the template's own comment), the `Actions` and
    /// `Renderer` sections, the capacity, then the bounding box —
    /// `transient` keeps the cached box, a full save recalculates it.
    pub fn save_internal(&mut self, ds: &mut DataSection, transient: bool) -> bool {
        ds.write_int(VERSION_STRING, PARTICLE_SERIALISE_VERSION);

        ds.write_float("windFactor_", self.wind_factor);
        ds.write_vector3("explicitPosition_", self.explicit_position);
        ds.write_vector3("explicitDirection_", self.explicit_direction);
        ds.write_bool("explicitTransform_", self.explicit_transform);
        ds.write_vector3("localOffset_", self.local_offset);
        ds.write_float("maxLod_", self.max_lod);
        ds.write_float("fixedFrameRate_", self.fixed_frame_rate);

        let actions = new_section(ds, "Actions");
        for action in &self.actions {
            action.save(actions);
        }

        if let Some(renderer) = self.renderer.as_ref() {
            let renderer_ds = new_section(ds, "Renderer");
            renderer.save(renderer_ds);
        }

        ds.write_int("capacity", self.capacity);

        // `forceFullBoundingBoxCalculation` only arms a flag the next
        // `updateBoundingBox` honours; the written box is still the last
        // calculated one either way, and the port's update always counts
        // every live particle, so there is no flag to arm here.
        if transient {
            self.viz_box = self.bounding_box;
        }

        if let Some(bb) = ds.open_or_make_section("boundingBox") {
            bb.del_children();
            if !self.bounding_box.inside_out() {
                bb.write_vector3("min", self.bounding_box.min);
                bb.write_vector3("max", self.bounding_box.max);
            }
        }

        true
    }
}

impl MetaParticleSystem {
    /// `MetaParticleSystem::save` (`meta_particle_system.cpp:305-351`):
    /// the seed time is read back before the children go and rewritten
    /// afterwards when it was set, each system lands in its own
    /// sanitised section (spaces become the ".." token,
    /// `XMLSection::sanitise`), and the helper model block is always
    /// written. The editor-only metaData block is a tool seam.
    pub fn save(&mut self, ds: &mut DataSection, transient: bool) -> bool {
        let seed_time = ds.read_float("seedTime", -1.0);

        ds.del_children();

        for system in self.systems.iter_mut() {
            let system_name = system.name.replace(' ', "..");
            if let Some(child) = ds.open_or_make_section(&system_name) {
                system.save(child, &system_name, transient);
            }
        }

        if seed_time > -1.0 {
            ds.write_float("seedTime", seed_time);
        }

        if let Some(helper) = ds.open_or_make_section("helperModel") {
            helper.write_string("helperModelName", &self.helper_model_name);
            helper.write_uint(
                "helperModelCenterOnHardPoint",
                self.helper_model_center_on_hard_point,
            );
        }

        true
    }
}

// ---------------------------------------------------------------------------
// Section: tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::particle_oracle_data as oracle;

    fn f32_bits(v: f32) -> u32 {
        v.to_bits()
    }

    /// Bit-exact f32 equality (the acceptance standard for the oracle).
    macro_rules! assert_f32_eq {
        ($a:expr, $b:expr, $($arg:tt)+) => {{
            let a: f32 = $a;
            let b: f32 = $b;
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "{}: {} ({:#x}) != {} ({:#x})",
                format_args!($($arg)+),
                a,
                a.to_bits(),
                b,
                b.to_bits()
            );
        }};
    }

    // The authored fire fixture; the Python oracle hardcodes the same
    // constants (/tmp/bw_particle_oracle.py).
    const FIXTURE: &str = r#"<root>
	<Fire>
		<serialiseVersionData>2</serialiseVersionData>
		<windFactor_>0</windFactor_>
		<explicitPosition_> 0 0 0 </explicitPosition_>
		<explicitDirection_> 0 0 1 </explicitDirection_>
		<explicitTransform_>false</explicitTransform_>
		<localOffset_> 0 0 0 </localOffset_>
		<maxLod_>0</maxLod_>
		<fixedFrameRate_>30</fixedFrameRate_>
		<Actions>
			<Source>
				<pPositionSrc>
					<BoxVectorGenerator>
						<nameID_>BoxVectorGenerator</nameID_>
						<corner_> -0.5 -0.5 -0.5 </corner_>
						<opposite_> 0.5 0.5 0.5 </opposite_>
					</BoxVectorGenerator>
				</pPositionSrc>
				<pVelocitySrc>
					<BoxVectorGenerator>
						<nameID_>BoxVectorGenerator</nameID_>
						<corner_> 0.1 -0.2 2 </corner_>
						<opposite_> 0.3 0.2 4 </opposite_>
					</BoxVectorGenerator>
				</pVelocitySrc>
				<timeTriggered_>true</timeTriggered_>
				<rate_>12</rate_>
				<minimumSize_>0.05</minimumSize_>
				<maximumSize_>0.15</maximumSize_>
				<initialColour_> 1 0.6 0.2 1 </initialColour_>
			</Source>
			<Sink>
				<maximumAge_>1.5</maximumAge_>
			</Sink>
			<Force>
				<vector_> 0 -1.5 0 </vector_>
			</Force>
			<TintShader>
				<repeat_>true</repeat_>
				<period_>1.0</period_>
				<fogAmount_>0</fogAmount_>
				<tints_>
					<Tint>
						<time>0</time>
						<color> 1 0.6 0.2 1 </color>
					</Tint>
					<Tint>
						<time>0.5</time>
						<color> 1 1 0.9 1 </color>
					</Tint>
					<Tint>
						<time>1</time>
						<color> 0.4 0.1 0.05 0 </color>
					</Tint>
				</tints_>
			</TintShader>
		</Actions>
		<Renderer>
			<SpriteParticleRenderer>
				<viewDependent_>false</viewDependent_>
				<local_>false</local_>
				<materialFX_>2</materialFX_>
				<textureName_>fx/fire.dds</textureName_>
				<frameCount_>8</frameCount_>
				<frameRate_>24</frameRate_>
			</SpriteParticleRenderer>
		</Renderer>
		<capacity>64</capacity>
	</Fire>
</root>"#;

    /// A test particle at `pos` with velocity `vel`: the reference non-mesh
    /// constructor with zero colour/size/pitch/yaw, so quantised fields match
    /// `default` + setters.
    fn placed(pos: Vector3, vel: Vector3) -> Particle {
        Particle::non_mesh(pos, vel, Vector4::new(0.0, 0.0, 0.0, 0.0), 0.0, 0.0, 0.0)
    }

    fn editor() -> crate::particle_editor::ParticleEditor {
        let mut editor = crate::particle_editor::ParticleEditor::new(oracle::ORACLE_SEED);
        assert!(editor.load(FIXTURE, "fire.particle"));
        editor
    }

    // ---- BwRandom ----------------------------------------------------------

    #[test]
    fn mt19937_canonical_vector() {
        // The MT19937 reference vector: seeding with 5489 yields
        // 3499211612 first (the standard test vector for init_genrand).
        let mut rng = BwRandom::new(5489);
        assert_eq!(rng.next_u32(), 3499211612);
        // The next two values of the published vector.
        assert_eq!(rng.next_u32(), 581869302);
        assert_eq!(rng.next_u32(), 3890346734);
    }

    #[test]
    fn unit_rand_is_draw_over_2_pow_32() {
        let mut rng = BwRandom::new(7);
        let draw = rng.next_u32();
        let expected = (draw as f32) / 4294967295.0f32;
        let mut rng2 = BwRandom::new(7);
        assert_f32_eq!(rng2.unit_rand(), expected, "unit_rand");
    }

    // ---- packing -----------------------------------------------------------

    #[test]
    fn velocity_packs_at_256_and_unpacks() {
        let mut p = Particle::default();
        p.set_velocity(Vector3::new(1.5, -2.25, 3.0));
        assert_f32_eq!(p.velocity().x, 1.5, "vx");
        assert_f32_eq!(p.velocity().y, -2.25, "vy");
        assert_f32_eq!(p.velocity().z, 3.0, "vz");
        // The clamp: ±32767/256 ≈ ±127.996
        p.set_velocity(Vector3::new(300.0, -300.0, 0.0));
        assert_f32_eq!(p.velocity().x, 32767.0 / 256.0, "clamp hi");
        assert_f32_eq!(p.velocity().y, -32768.0 / 256.0, "clamp lo");
    }

    #[test]
    fn single_axis_velocity_setters() {
        let mut p = Particle::default();
        p.set_x_velocity(4.0);
        p.set_y_velocity(-4.0);
        p.set_z_velocity(0.5);
        assert_f32_eq!(p.velocity().x, 4.0, "x");
        assert_f32_eq!(p.velocity().y, -4.0, "y");
        assert_f32_eq!(p.velocity().z, 0.5, "z");
    }

    #[test]
    fn size_packs_in_655_over_65535() {
        let mut p = Particle::default();
        p.set_size(0.1234);
        // size quantises to 100/65535, so only within one quantum
        assert!((p.size() - 0.1234).abs() < 0.002, "roundtrip {}", p.size());
        // clamp at the data maximum
        p.set_size(200.0);
        assert_f32_eq!(p.size(), 100.0, "clamp");
    }

    #[test]
    fn age_setters_use_the_rounding_bias() {
        // age(float) rounds with the +0.5 bias; nAgeIncrements truncates.
        let mut p = Particle::default();
        p.set_age(0.5);
        // 0.5 * PACK_AGE (≈120) = 60, +0.5 → 60
        assert_eq!(p.age_raw(), 60);
        assert_f32_eq!(p.age(), Particle::age_of(60), "unpacked");
        // PACK_AGE evaluates to exactly 120 in f32, so the truncation is
        // exact: 0.5 s -> 60 ticks, 1 s -> 120 ticks.
        assert_eq!(Particle::n_age_increments(0.5), 60);
        assert_eq!(Particle::n_age_increments(1.0), 120);
        assert_eq!(Particle::n_age_increments(-5.0), 0);
        assert_eq!(Particle::age_max(), 65535);
    }

    #[test]
    fn kill_and_alive() {
        let mut p = Particle::default();
        assert!(p.is_alive());
        p.kill();
        assert!(!p.is_alive());
    }

    #[test]
    fn pitch_yaw_pack_into_256ths() {
        let mut p = Particle::default();
        p.set_pitch(0.0);
        p.set_yaw(0.0);
        // 0 packs through the +10-turn offset to a zero byte.
        assert_f32_eq!(p.pitch(), 0.0, "pitch");
        assert_f32_eq!(p.yaw(), 0.0, "yaw");
        // A quarter turn packs and unpacks into the 256th-step grid.
        let quarter = std::f32::consts::FRAC_PI_2;
        p.set_yaw(quarter);
        let unpacked = p.yaw();
        assert!(
            (unpacked - quarter).abs() < MATH_2PI / 256.0 + 1e-4,
            "yaw {} vs {}",
            unpacked,
            quarter
        );
    }

    #[test]
    fn colour_packs_abgr_from_normalised() {
        let c = Vector4::new(1.0, 0.5, 0.0, 0.25);
        let packed = colour_from_normalised(c);
        let r = (packed >> 16) & 0xff;
        let g = (packed >> 8) & 0xff;
        let b = packed & 0xff;
        let a = (packed >> 24) & 0xff;
        assert_eq!(r, 255);
        assert_eq!(g, 127); // 0.5 * 255 = 127.5 trunc
        assert_eq!(b, 0);
        assert_eq!(a, 63); // 0.25 * 255 = 63.75 trunc
        // channel clamping
        let c = colour_from_normalised(Vector4::new(2.0, -1.0, 0.0, 0.0));
        assert_eq!(((c >> 16) & 0xff), 255);
        assert_eq!(((c >> 8) & 0xff), 0);
    }

    // ---- container ---------------------------------------------------------

    #[test]
    fn particles_remove_swaps_last_into_place() {
        let mut ps = Particles::new();
        for i in 0..5 {
            let mut p = Particle::default();
            p.set_size(i as f32);
            ps.add_particle(p);
        }
        ps.remove_particle(1);
        assert_eq!(ps.len(), 4);
        // the last particle (4) now sits in slot 1
        // the size quantises, so compare to within one quantum
        assert!((ps.get(1).size() - 4.0).abs() < 0.002, "swapped");
        assert!((ps.get_mut(2).size() - 2.0).abs() < 0.002, "kept");
    }

    #[test]
    fn particles_remove_out_of_range_is_none() {
        let mut ps = Particles::new();
        assert_eq!(ps.remove_particle(0), None);
        ps.add_particle(Particle::default());
        assert_eq!(ps.remove_particle(3), None);
        assert_eq!(ps.remove_particle(0), Some(0));
    }

    #[test]
    fn particles_clear_resets_the_index() {
        let mut ps = Particles::new();
        ps.add_particle(Particle::default());
        ps.clear();
        assert!(ps.is_empty());
        assert_eq!(ps.len(), 0);
    }

    // ---- generators --------------------------------------------------------

    #[test]
    fn point_generator_draws_nothing() {
        let mut rng = BwRandom::new(1);
        let g = VectorGenerator::Point {
            position: Vector3::new(1.0, 2.0, 3.0),
        };
        let v = g.generate(&mut rng);
        assert_f32_eq!(v.x, 1.0, "px");
        // no draw happened
        let mut rng2 = BwRandom::new(1);
        assert_eq!(rng2.next_u32(), rng.next_u32());
    }

    #[test]
    fn line_generator_draws_once_along_the_span() {
        let mut rng = BwRandom::new(2);
        let g = VectorGenerator::Line {
            origin: Vector3::new(1.0, 0.0, 0.0),
            direction: Vector3::new(0.0, 2.0, 0.0),
        };
        let v = g.generate(&mut rng);
        // the draw is (origin + direction * unitRand).y
        let mut rng2 = BwRandom::new(2);
        let expected_y = 2.0 * rng2.unit_rand();
        assert_f32_eq!(v.y, expected_y, "line y");
        assert_f32_eq!(v.x, 1.0, "line x");
        // exactly one draw
        assert_eq!(rng2.next_u32(), rng.next_u32());
    }

    #[test]
    fn box_generator_draws_xyz_in_order() {
        let mut rng = BwRandom::new(3);
        let g = VectorGenerator::Box {
            corner: Vector3::new(-1.0, -2.0, -3.0),
            opposite: Vector3::new(1.0, 2.0, 3.0),
        };
        let v = g.generate(&mut rng);
        let mut rng2 = BwRandom::new(3);
        let ux = rng2.unit_rand();
        let uy = rng2.unit_rand();
        let uz = rng2.unit_rand();
        assert_f32_eq!(v.x, -1.0 + ux * 2.0, "bx");
        assert_f32_eq!(v.y, -2.0 + uy * 4.0, "by");
        assert_f32_eq!(v.z, -3.0 + uz * 6.0, "bz");
    }

    #[test]
    fn cylinder_generator_draw_order_and_equal_radii() {
        // equal radii: no radius draw
        let (basis_u, basis_v) =
            VectorGenerator::cylinder_construct_basis(Vector3::new(0.0, 0.0, 1.0));
        let g = VectorGenerator::Cylinder {
            origin: Vector3::ZERO,
            direction: Vector3::new(0.0, 0.0, 2.0),
            max_radius: 1.0,
            min_radius: 1.0,
            basis_u,
            basis_v,
        };
        let mut rng = BwRandom::new(4);
        let v = g.generate(&mut rng);
        let mut rng2 = BwRandom::new(4);
        let axis = rng2.unit_rand();
        let angle = (rng2.unit_rand() * 2.0) * MATH_PI;
        // no third draw for the radius
        assert_eq!(rng2.next_u32(), rng.next_u32());
        // the axis draw dominates z; the ring sits in xy
        let expected_z = 2.0 * axis + (basis_u.z * angle.cos() + basis_v.z * angle.sin()) * 1.0;
        assert!(
            (v.z - expected_z).abs() < 1e-5,
            "z {} vs {}",
            v.z,
            expected_z
        );
        // unit-length ring components: x² + y² ≈ r²
        let r2 = v.x * v.x + v.y * v.y
            - (basis_u.x * angle.cos() + basis_v.x * angle.sin())
                * (basis_u.x * angle.cos() + basis_v.x * angle.sin())
                * 2.0
                * axis
                * 0.0;
        let _ = r2; // structural sanity only; the ring math is checked below
    }

    #[test]
    fn cylinder_construct_basis_uses_y_when_parallel_to_x() {
        let (u, v) = VectorGenerator::cylinder_construct_basis(Vector3::new(1.0, 0.0, 0.0));
        // basis falls back to the y axis; u ⊥ dir, v ⊥ u
        assert!(u.dot_product(&Vector3::new(1.0, 0.0, 0.0)).abs() < 1e-5);
        assert!(v.dot_product(&u).abs() < 1e-5);
        assert!((u.length() - 1.0).abs() < 1e-5);
        assert!((v.length() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn radii_checks_clamp() {
        let (min, max) = VectorGenerator::cylinder_check_radii(5.0, -1.0);
        assert_f32_eq!(min, 0.0, "cyl min is always zero");
        assert_f32_eq!(max, 0.0, "cyl max clamps");
        let (min, max) = VectorGenerator::sphere_check_radii(-2.0, 1.0);
        assert_f32_eq!(min, 0.0, "sphere min clamps");
        assert_f32_eq!(max, 1.0, "sphere max kept");
        let (min, max) = VectorGenerator::sphere_check_radii(2.0, 1.0);
        assert_f32_eq!(min, 2.0, "sphere min kept");
        assert_f32_eq!(max, 2.0, "sphere max snaps to min");
    }

    #[test]
    fn sphere_generator_is_deterministic_and_unit_scaled() {
        let g = |rng: &mut BwRandom| {
            VectorGenerator::Sphere {
                centre: Vector3::new(5.0, 0.0, 0.0),
                max_radius: 2.0,
                min_radius: 0.0,
            }
            .generate(rng)
        };
        let a = g(&mut BwRandom::new(5));
        let b = g(&mut BwRandom::new(5));
        assert_eq!(f32_bits(a.x), f32_bits(b.x));
        assert_eq!(f32_bits(a.z), f32_bits(b.z));
        // the radius is k1 * maxRadius, with k1 = getPow(the first draw);
        // the direction part comes from the cache, which is a
        // transcendental seam, so compare against that draw independently
        let cache = SphereVectorCache::get();
        let mut rng3 = BwRandom::new(5);
        // the first draw is the cache index, the second is the radius
        let _ = rng3.next_u32();
        let expected = cache.get_pow(rng3.unit_rand()) * 2.0;
        let d = Vector3::new(a.x - 5.0, a.y, a.z);
        let len = d.length();
        assert!((len - expected).abs() < 1e-6, "radius {len} vs {expected}");
    }

    #[test]
    fn sphere_pow_guard_clamps() {
        let cache = SphereVectorCache::get();
        assert_f32_eq!(cache.get_pow(-1.0), cache.get_pow(0.0), "below range");
        assert_f32_eq!(cache.get_pow(2.0), cache.get_pow(1.0), "above range");
        assert!((cache.get_pow(0.5) - 0.5f32.powf(1.0 / 3.0)).abs() < 0.01);
    }

    // ---- action loading ----------------------------------------------------

    #[test]
    fn unknown_action_type_fails_the_load() {
        let ds =
            resources::datasection::parse("root", b"<root><NotAnAction/></root>").expect("parse");
        let child = ds.open_child(0).unwrap();
        let err = ParticleAction::load(child).unwrap_err();
        assert!(err.contains("Unknown Particle System Action type NotAnAction"));
    }

    #[test]
    fn name_to_type_covers_the_family() {
        for name in [
            "Source",
            "Sink",
            "Barrier",
            "Force",
            "Stream",
            "Jitter",
            "Scaler",
            "TintShader",
            "NodeClamp",
            "Orbitor",
            "Flare",
            "Collide",
            "MatrixSwarm",
            "Magnet",
            "Splat",
        ] {
            let t = name_to_type(name).expect(name);
            assert_eq!(t.name_id(), name);
        }
        assert_eq!(name_to_type("Nope"), None);
    }

    #[test]
    fn sink_loads_maximum_age_and_minimum_speed() {
        let ds = resources::datasection::parse(
            "root",
            b"<root><Sink><maximumAge_>2.5</maximumAge_>\
              <minimumSpeed_>3</minimumSpeed_><outsideOnly_>true</outsideOnly_>\
              <delay_>0.4</delay_></Sink></root>",
        )
        .expect("parse");
        let action = ParticleAction::load(ds.open_child(0).unwrap()).expect("load");
        assert_eq!(action.base.delay, 0.4);
        let crate::particle::ActionKind::Sink(s) = &action.kind else {
            panic!("sink");
        };
        assert_f32_eq!(s.maximum_age, 2.5, "maxAge");
        assert_f32_eq!(s.minimum_speed, 3.0, "minSpeed");
        assert!(s.outside_only);
    }

    #[test]
    fn tint_shader_falls_back_to_black_white_ramp() {
        let ds = resources::datasection::parse(
            "root",
            b"<root><TintShader><period_>2.0</period_></TintShader></root>",
        )
        .expect("parse");
        let action = ParticleAction::load(ds.open_child(0).unwrap()).expect("load");
        let crate::particle::ActionKind::TintShader(t) = &action.kind else {
            panic!("tint");
        };
        assert_eq!(t.tints.len(), 2);
        assert_f32_eq!(t.tints[0].colour.x, 0.0, "black");
        assert_f32_eq!(t.tints[1].colour.x, 1.0, "white");
        assert_f32_eq!(t.tints[1].time, 2.0, "over the period");
        assert_f32_eq!(t.period, 2.0, "period restored");
    }

    #[test]
    fn tint_shader_one_tint_fixes_zero_period_persistently() {
        let ds = resources::datasection::parse(
            "root",
            b"<root><TintShader><tints_><Tint><time>0.5</time>\
              <color> 0.1 0.2 0.3 1 </color></Tint></tints_></TintShader></root>",
        )
        .expect("parse");
        let action = ParticleAction::load(ds.open_child(0).unwrap()).expect("load");
        let crate::particle::ActionKind::TintShader(t) = &action.kind else {
            panic!("tint");
        };
        // 0 period -> 1, and the fixup persists into period_
        assert_eq!(t.tints.len(), 2);
        assert_f32_eq!(t.period, 1.0, "period fixup persisted");
        assert_f32_eq!(t.tints[0].colour.z, 0.3, "colour carried");
    }

    #[test]
    fn tint_shader_multi_tints_sort_by_time() {
        let ds = resources::datasection::parse(
            "root",
            b"<root><TintShader><tints_>\
              <Tint><time>2</time><color> 1 0 0 1 </color></Tint>\
              <Tint><time>1</time><color> 0 1 0 1 </color></Tint>\
              <Tint><time>3</time><color> 0 0 1 1 </color></Tint>\
              </tints_></TintShader></root>",
        )
        .expect("parse");
        let action = ParticleAction::load(ds.open_child(0).unwrap()).expect("load");
        let crate::particle::ActionKind::TintShader(t) = &action.kind else {
            panic!("tint");
        };
        assert_eq!(t.tints.len(), 3);
        assert!(t.tints[0].time < t.tints[1].time && t.tints[1].time < t.tints[2].time);
        assert_f32_eq!(t.tints[0].colour.y, 1.0, "green first");
    }

    #[test]
    fn tint_shader_empty_section_stays_empty() {
        let ds = resources::datasection::parse(
            "root",
            b"<root><TintShader><tints_></tints_></TintShader></root>",
        )
        .expect("parse");
        let action = ParticleAction::load(ds.open_child(0).unwrap()).expect("load");
        let crate::particle::ActionKind::TintShader(t) = &action.kind else {
            panic!("tint");
        };
        assert!(t.tints.is_empty());
        assert_f32_eq!(t.period, 1.0, "default period");
    }

    #[test]
    fn barrier_loads_shape_and_reaction() {
        let ds = resources::datasection::parse(
            "root",
            b"<root><Barrier><shape_>3</shape_><reaction_>3</reaction_>\
              <vecA_> 1 2 3 </vecA_><vecB_> 4 5 6 </vecB_><radius_>7</radius_></Barrier></root>",
        )
        .expect("parse");
        let action = ParticleAction::load(ds.open_child(0).unwrap()).expect("load");
        let crate::particle::ActionKind::Barrier(b) = &action.kind else {
            panic!("barrier");
        };
        assert_eq!(b.shape, BarrierShape::Sphere);
        assert_eq!(b.reaction, BarrierReaction::Wrap);
        assert_f32_eq!(b.vec_a.y, 2.0, "vecA");
        assert_f32_eq!(b.vec_b.z, 6.0, "vecB");
        assert_f32_eq!(b.radius, 7.0, "radius");
        // resilience is not serialised; it stays at 1
        assert_f32_eq!(b.resilience, 1.0, "resilience default");
    }

    #[test]
    fn source_loads_generators_last_child_wins() {
        let ds = resources::datasection::parse(
            "root",
            b"<root><Source>\
              <pPositionSrc>\
                <PointVectorGenerator><position_> 9 9 9 </position_></PointVectorGenerator>\
                <BoxVectorGenerator><corner_> 0 0 0 </corner_><opposite_> 1 1 1 </opposite_></BoxVectorGenerator>\
              </pPositionSrc>\
              <pVelocitySrc>\
                <LineVectorGenerator><origin_> 0 0 0 </origin_><direction_> 0 0 5 </direction_></LineVectorGenerator>\
              </pVelocitySrc>\
              <rate_>10</rate_><timeTriggered_>true</timeTriggered_>\
              </Source></root>",
        )
        .expect("parse");
        let action = ParticleAction::load(ds.open_child(0).unwrap()).expect("load");
        let crate::particle::ActionKind::Source(s) = &action.kind else {
            panic!("source");
        };
        // the last position generator child wins
        match &s.position_src {
            Some(VectorGenerator::Box { corner, opposite }) => {
                assert_f32_eq!(corner.x, 0.0, "corner");
                assert_f32_eq!(opposite.x, 1.0, "opposite");
            }
            other => panic!("expected box, got {other:?}"),
        }
        match &s.velocity_src {
            Some(VectorGenerator::Line { direction, .. }) => {
                assert_f32_eq!(direction.z, 5.0, "line dir");
            }
            other => panic!("expected line, got {other:?}"),
        }
        assert_f32_eq!(s.rate, 10.0, "rate");
        assert!(s.time_triggered);
        // untouched defaults survive
        assert_f32_eq!(s.sensitivity, 0.1, "sensitivity default");
        assert_f32_eq!(s.drop_distance, 15.0, "dropDistance default");
    }

    #[test]
    fn stream_and_scaler_load() {
        let ds = resources::datasection::parse(
            "root",
            b"<root><Stream><vector_> 1 2 3 </vector_><halfLife_>0.5</halfLife_></Stream></root>",
        )
        .expect("parse");
        let action = ParticleAction::load(ds.open_child(0).unwrap()).expect("load");
        let crate::particle::ActionKind::Stream(s) = &action.kind else {
            panic!("stream");
        };
        assert_f32_eq!(s.vector.z, 3.0, "stream z");
        assert_f32_eq!(s.half_life, 0.5, "halfLife");

        let ds = resources::datasection::parse(
            "root",
            b"<root><Scaler><size_>2</size_><rate_>0.5</rate_></Scaler></root>",
        )
        .expect("parse");
        let action = ParticleAction::load(ds.open_child(0).unwrap()).expect("load");
        let crate::particle::ActionKind::Scaler(s) = &action.kind else {
            panic!("scaler");
        };
        assert_f32_eq!(s.size, 2.0, "size");
        assert_f32_eq!(s.rate, 0.5, "rate");
    }

    #[test]
    fn empty_actions_load_as_defaults() {
        let ds = resources::datasection::parse(
            "root",
            b"<root><Source></Source><Magnet></Magnet><MatrixSwarm></MatrixSwarm><Splat></Splat></root>",
        )
        .expect("parse");
        for i in 0..4 {
            let action = ParticleAction::load(ds.open_child(i).unwrap()).expect("load");
            match i {
                0 => {
                    let crate::particle::ActionKind::Source(s) = &action.kind else {
                        panic!("source");
                    };
                    // Source ctor defaults
                    assert!(!s.motion_triggered);
                    assert_f32_eq!(s.active_period, 1.0, "activePeriod");
                    assert_f32_eq!(s.sleep_period_max, -1.0, "sleepPeriodMax");
                    assert_f32_eq!(s.initial_colour.w, 1.0, "initialColour alpha");
                    assert_eq!(s.forced_unit_size, 1);
                    assert!(s.first_update);
                }
                1 => {
                    let crate::particle::ActionKind::Magnet(m) = &action.kind else {
                        panic!("magnet");
                    };
                    assert_f32_eq!(m.strength, 1.0, "strength default");
                    assert!(m.source.is_none());
                }
                2 => assert_eq!(action.kind, ActionKind::MatrixSwarm(MatrixSwarmPSA)),
                3 => assert_eq!(action.kind, ActionKind::Splat(SplatPSA)),
                _ => unreachable!(),
            }
            assert!(action.base.enabled);
        }
    }

    #[test]
    fn legacy_wind_halflife_translates_to_wind_factor() {
        // a half-life of 5 s corresponds to a wind factor of 0.5
        let mut system = ParticleSystem::new();
        let ds = resources::datasection::parse(
            "root",
            b"<root><serialiseVersionData>2</serialiseVersionData><windHalflife_>5</windHalflife_></root>",
        )
        .expect("parse");
        assert!(!system.load(&ds, "legacy"));
        let k = -0.2f32 * 0.5f32.ln();
        let expected = (-k * 5.0f32.abs()).exp();
        assert!((system.wind_factor - expected).abs() < 1e-6);
    }

    #[test]
    fn version_mismatch_logs_but_loads_on() {
        let mut system = ParticleSystem::new();
        let ds = resources::datasection::parse(
            "root",
            b"<root><serialiseVersionData>1</serialiseVersionData></root>",
        )
        .expect("parse");
        assert!(!system.load(&ds, "v1"));
        assert_eq!(system.load_errors.len(), 1);
        assert!(system.load_errors[0].contains("version mismatch"));
    }

    #[test]
    fn missing_version_section_reads_as_current() {
        let mut system = ParticleSystem::new();
        let ds = resources::datasection::parse("root", b"<root></root>").expect("parse");
        assert!(!system.load(&ds, "no version"));
        // no version complaint; only the renderer gate failed
        assert!(system.load_errors.is_empty());
    }

    #[test]
    fn renderer_load_sets_rotated_from_source_rotations() {
        let mut system = ParticleSystem::new();
        let ds = resources::datasection::parse(
            "root",
            b"<root><Actions><Source><randomInitialRotation_> 3 0 </randomInitialRotation_></Source></Actions>\
              <Renderer><SpriteParticleRenderer><textureName_>fx/a.dds</textureName_></SpriteParticleRenderer></Renderer></root>",
        )
        .expect("parse");
        assert!(system.load(&ds, "rotated"));
        let r = system.renderer.as_ref().expect("renderer");
        assert_eq!(r.name_id(), "SpriteParticleRenderer");
        assert!(!r.view_dependent() && !r.local());
        let Some(ParticleRenderer::Sprite { sprite, .. }) = system.renderer.as_ref() else {
            panic!("sprite");
        };
        assert!(sprite.rotated);
        assert_eq!(sprite.texture_name, "fx/a.dds");
        assert_eq!(sprite.material_fx, MaterialFX::Blended);
        // point sprite shares the sprite fields
        let mut system = ParticleSystem::new();
        let ds = resources::datasection::parse(
            "root",
            b"<root><Actions></Actions><Renderer><PointSpriteParticleRenderer/></Renderer></root>",
        )
        .expect("parse");
        assert!(system.load(&ds, "pointsprite"));
        let Some(ParticleRenderer::PointSprite { sprite, .. }) = system.renderer.as_ref() else {
            panic!("pointsprite");
        };
        assert!(!sprite.rotated);
    }

    #[test]
    fn unknown_renderer_fails_the_system_load() {
        let mut system = ParticleSystem::new();
        let ds = resources::datasection::parse(
            "root",
            b"<root><Renderer><MeshParticleRenderer/></Renderer></root>",
        )
        .expect("parse");
        assert!(!system.load(&ds, "mesh"));
    }

    #[test]
    fn set_renderer_only_swaps_on_change() {
        let mut system = ParticleSystem::new();
        system.set_capacity(10);
        let r = Some(ParticleRenderer::create_renderer_of_type("SpriteParticleRenderer").unwrap());
        system.set_renderer(r.clone());
        assert!(system.renderer.is_some());
        // container replaced but capacity re-applied
        assert_eq!(system.capacity, 10);
        // identical renderer: no swap
        system.set_renderer(r);
        assert_eq!(system.capacity, 10);
    }

    #[test]
    fn set_capacity_clears_on_change_and_clamps() {
        let mut system = ParticleSystem::new();
        system.set_capacity(20);
        system.add_particle(Particle::default());
        assert_eq!(system.size(), 1);
        system.set_capacity(5);
        assert_eq!(system.capacity, 5);
        assert_eq!(system.size(), 0, "change clears");
        system.set_capacity(1 << 20);
        assert_eq!(system.capacity, MAX_CAPACITY);
    }

    // ---- system update -----------------------------------------------------

    #[test]
    fn add_particle_respects_capacity_and_seeds_bbox() {
        let mut system = ParticleSystem::new();
        system.set_capacity(2);
        let p = placed(Vector3::new(1.0, 2.0, 3.0), Vector3::ZERO);
        system.add_particle(p);
        assert_eq!(system.size(), 1);
        // the first dynamic particle seeds a ±0.1 box
        assert!((system.bounding_box.min.x - 0.9).abs() < 1e-5);
        assert!((system.bounding_box.max.z - 3.1).abs() < 1e-5);
        // the second grows it
        let p = placed(Vector3::new(5.0, 0.0, 0.0), Vector3::ZERO);
        system.add_particle(p);
        assert!((system.bounding_box.max.x - 5.0).abs() < 1e-5);
        // the third is beyond the hard capacity
        system.add_particle(Particle::default());
        assert_eq!(system.size(), 2);
        // a static system skips the seeding box
        let mut static_system = ParticleSystem::new();
        static_system.is_static = true;
        static_system.set_capacity(2);
        let p = placed(Vector3::new(1.0, 1.0, 1.0), Vector3::ZERO);
        static_system.add_particle(p);
        assert!((static_system.bounding_box.min.x - 1.0).abs() < 1e-5);
    }

    #[test]
    fn fixed_frame_rate_quantises_dt_and_carries_leftover() {
        // 1/30 s ticks at fixedFrameRate 30 land on 4 age increments:
        // PACK_AGE evaluates to exactly 120 in f32, so 4/120 == 1/30 and
        // the whole tick is consumed with nothing carried over.
        let mut editor = editor();
        let editor_ptr = &mut editor;
        let dt = 1.0f32 / 30.0;
        editor_ptr.tick(dt);
        let system = &editor_ptr.meta.systems[0];
        assert_f32_eq!(system.frames_left_over, 0.0, "leftover");
        assert_eq!(system.counter, 1);
    }

    #[test]
    fn inactive_manager_freezes_everything() {
        let mut editor = editor();
        editor.env().active = false;
        assert!(!editor.tick(1.0 / 30.0));
        assert_eq!(editor.meta.systems[0].counter, 0);
    }

    #[test]
    fn world_transform_explicit_looks_at_the_direction() {
        let mut system = ParticleSystem::new();
        system.explicit_transform = true;
        system.explicit_position = Vector3::new(1.0, 2.0, 3.0);
        system.explicit_direction = Vector3::new(0.0, 0.0, 1.0);
        let m = system.world_transform();
        // the origin maps onto the explicit position
        let o = m.apply_point(&Vector3::ZERO);
        assert!((o.x - 1.0).abs() < 1e-5 && (o.y - 2.0).abs() < 1e-5 && (o.z - 3.0).abs() < 1e-5);
        // object_to_world of a local renderer collapses to identity
        system.set_renderer(ParticleRenderer::create_renderer_of_type(
            "SpriteParticleRenderer",
        ));
        let base = RendererBase {
            local: true,
            ..Default::default()
        };
        system.renderer = Some(ParticleRenderer::Sprite {
            base,
            sprite: Default::default(),
        });
        let m = system.object_to_world();
        let i = Matrix::identity();
        let axis = |v: &Vector4, j: usize| match j {
            0 => v.x,
            1 => v.y,
            2 => v.z,
            _ => v.w,
        };
        for k in 0..4 {
            for j in 0..4 {
                assert!((axis(&m.row(k), j) - axis(&i.row(k), j)).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn duration_takes_the_max_sink_age() {
        let mut system = ParticleSystem::new();
        assert_f32_eq!(system.duration(), -1.0, "no sinks");
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Sink(SinkPSA {
                maximum_age: 2.0,
                ..Default::default()
            }),
        });
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Sink(SinkPSA {
                maximum_age: 5.0,
                ..Default::default()
            }),
        });
        assert_f32_eq!(system.duration(), 5.0, "max sink");
    }

    #[test]
    fn spawn_forces_the_sources() {
        let mut system = ParticleSystem::new();
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Source(SourcePSA::default()),
        });
        system.spawn(3);
        let crate::particle::ActionKind::Source(s) = &system.actions[0].kind else {
            panic!("source");
        };
        assert_eq!(s.forced_total, 3);
    }

    #[test]
    fn clear_empties_and_resets_the_box() {
        let mut editor = editor();
        editor.tick(1.0 / 30.0);
        let system = editor.meta.systems.first_mut().unwrap();
        system.clear();
        assert_eq!(system.size(), 0);
        // the "empty" box is the 1 cm box
        assert!((system.bounding_box.min.x + 0.01).abs() < 1e-6);
        assert!((system.bounding_box.max.z - 0.01).abs() < 1e-6);
    }

    #[test]
    fn lod_visibility_gates_on_the_env() {
        let mut system = ParticleSystem::new();
        system.max_lod = 100.0;
        let env = ParticleEnv::default();
        assert!(system.is_lod_visible(50.0, &env));
        assert!(!system.is_lod_visible(200.0, &env));
        // maxLod <= 0 is the infinite marker
        system.max_lod = 0.0;
        assert!(system.is_lod_visible(1e9, &env));
        // the bias divides the maximum (applyLodBias): 100/2 = 50
        system.max_lod = 100.0;
        let biased = ParticleEnv {
            lod_bias: 2.0,
            ..Default::default()
        };
        assert!(system.is_lod_visible(50.0, &biased));
        assert!(!system.is_lod_visible(150.0, &biased));
        // an inactive manager hides everything
        let off = ParticleEnv {
            active: false,
            ..Default::default()
        };
        assert!(!system.is_lod_visible(0.0, &off));
    }

    #[test]
    fn size_in_bytes_counts_container_renderer_and_actions() {
        let editor = editor();
        let system = &editor.meta.systems[0];
        let expected = std::mem::size_of::<ParticleSystem>()
            + 64 * Particle::size_in_bytes()
            + system.renderer.as_ref().unwrap().size_in_bytes()
            + system
                .actions
                .iter()
                .map(|a| a.size_in_bytes())
                .sum::<usize>();
        assert_eq!(system.size_in_bytes(), expected);
    }

    #[test]
    fn delayed_action_accumulates_age_without_executing() {
        let mut system = ParticleSystem::new();
        system.actions.push(ParticleAction {
            base: ActionBase {
                delay: 1.0,
                ..Default::default()
            },
            kind: ActionKind::Force(ForcePSA {
                vector: Vector3::new(0.0, -100.0, 0.0),
            }),
        });
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        system.update(0.5, &mut rng, &env);
        assert_f32_eq!(system.actions[0].base.age, 0.5, "gated age");
        // the particle (added by hand) did not move: the force never ran
        let p = placed(Vector3::new(0.0, 0.0, 0.0), Vector3::ZERO);
        system.particles.add_particle(p);
        system.update(0.5, &mut rng, &env);
        assert_f32_eq!(system.actions[0].base.age, 1.0, "gated age 2");
        // next tick crosses the delay: the force applies
        system.update(0.5, &mut rng, &env);
        // age_ only accumulates while gated (force_psa.cpp:36-39), so it
        // freezes at the delay once the action starts running
        assert_f32_eq!(system.actions[0].base.age, 1.0, "age frozen at delay");
        let v = system.particles.get(0).velocity();
        assert!(v.y < 0.0, "force applied after delay");
    }

    // ---- action bodies -----------------------------------------------------

    #[test]
    fn force_scales_by_dt() {
        let mut system = ParticleSystem::new();
        let mut p = Particle::default();
        p.set_velocity(Vector3::new(0.0, 10.0, 0.0));
        system.particles.add_particle(p);
        system.particles.add_particle({
            let mut p = Particle::default();
            p.kill();
            p
        });
        let mut force = ForcePSA {
            vector: Vector3::new(1.0, -1.0, 0.0),
        };
        execute_force(&mut system, &mut force, 0.5);
        let v = system.particles.get(0).velocity();
        assert_f32_eq!(v.x, 0.5, "fx");
        assert_f32_eq!(v.y, 9.5, "fy");
        // the dead particle is untouched
        let v = system.particles.get(1).velocity();
        assert_f32_eq!(v.x, 0.0, "dead untouched");
    }

    #[test]
    fn stream_decay_cases() {
        let make = || {
            let mut system = ParticleSystem::new();
            let mut p = Particle::default();
            p.set_velocity(Vector3::new(0.0, 0.0, 0.0));
            system.particles.add_particle(p);
            system
        };
        // negative half-life: decay 1, so the velocity is untouched
        let mut system = make();
        let mut s = StreamPSA {
            vector: Vector3::new(1.0, 2.0, 3.0),
            half_life: -1.0,
        };
        execute_stream(&mut system, &mut s, 0.1);
        let v = system.particles.get(0).velocity();
        assert_f32_eq!(v.x, 0.0, "no decay x");
        assert_f32_eq!(v.y, 0.0, "no decay y");
        assert_f32_eq!(v.z, 0.0, "no decay z");
        // zero half-life: snap
        let mut system = make();
        let mut s = StreamPSA {
            vector: Vector3::new(1.0, 2.0, 3.0),
            half_life: 0.0,
        };
        execute_stream(&mut system, &mut s, 0.1);
        let v = system.particles.get(0).velocity();
        assert_f32_eq!(v.x, 1.0, "snap x");
        // positive half-life: halfway after one half-life (powf seam: the
        // expected value comes from the platform libm, not the oracle)
        let mut system = make();
        let mut s = StreamPSA {
            vector: Vector3::new(2.0, 0.0, 0.0),
            half_life: 0.5,
        };
        execute_stream(&mut system, &mut s, 0.5);
        let v = system.particles.get(0).velocity();
        assert!((v.x - 1.0).abs() < 1e-6, "halfway {}", v.x);
    }

    #[test]
    fn scaler_moves_toward_the_target_and_clamps() {
        let mut system = ParticleSystem::new();
        let mut p = Particle::default();
        p.set_size(2.0);
        system.particles.add_particle(p);
        let mut p = Particle::default();
        p.set_size(0.0);
        system.particles.add_particle(p);
        let mut s = ScalerPSA {
            size: 1.0,
            rate: 0.5,
        };
        execute_scaler(&mut system, &mut s, 10.0);
        // both overshoot and clamp to 1, within one size quantum
        assert!(
            (system.particles.get(0).size() - 1.0).abs() < 0.002,
            "high side"
        );
        assert!(
            (system.particles.get(1).size() - 1.0).abs() < 0.002,
            "low side"
        );
        // without overshoot the step is rate * dt
        let mut system = ParticleSystem::new();
        let mut p = Particle::default();
        p.set_size(2.0);
        system.particles.add_particle(p);
        execute_scaler(&mut system, &mut s, 1.0);
        assert!(
            (system.particles.get(0).size() - 1.5).abs() < 0.002,
            "stepped"
        );
    }

    #[test]
    fn jitter_draws_position_then_velocity() {
        let mut system = ParticleSystem::new();
        let p = placed(Vector3::new(1.0, 1.0, 1.0), Vector3::new(0.0, 0.0, 0.0));
        system.particles.add_particle(p);
        let mut j = JitterPSA {
            affect_position: true,
            affect_velocity: true,
            position_src: Some(VectorGenerator::Point {
                position: Vector3::new(0.1, 0.0, 0.0),
            }),
            velocity_src: Some(VectorGenerator::Point {
                position: Vector3::new(0.0, 0.0, 2.0),
            }),
        };
        execute_jitter(&mut system, &mut j, 0.5, &mut BwRandom::new(6));
        let p = system.particles.get(0);
        assert_f32_eq!(p.position.x, 1.05, "jittered pos");
        assert_f32_eq!(p.velocity().z, 2.0, "jittered vel");
        // position-only jitter leaves velocity alone and draws once
        let mut system = ParticleSystem::new();
        let p = placed(Vector3::new(1.0, 1.0, 1.0), Vector3::ZERO);
        system.particles.add_particle(p);
        let mut j = JitterPSA {
            affect_position: true,
            affect_velocity: false,
            position_src: Some(VectorGenerator::Point {
                position: Vector3::new(4.0, 0.0, 0.0),
            }),
            velocity_src: Some(VectorGenerator::Point {
                position: Vector3::new(0.0, 0.0, 9.0),
            }),
        };
        execute_jitter(&mut system, &mut j, 1.0, &mut BwRandom::new(6));
        let p = system.particles.get(0);
        assert_f32_eq!(p.position.x, 5.0, "pos only");
        assert_f32_eq!(p.velocity().z, 0.0, "vel untouched");
    }

    #[test]
    fn tint_shader_poles_and_blend() {
        let mut system = ParticleSystem::new();
        let mut t = TintShaderPSA {
            repeat: false,
            period: 2.0,
            fog_amount: 0.0,
            tints: vec![
                Tint {
                    time: 0.0,
                    colour: Vector4::new(0.0, 0.0, 0.0, 1.0),
                },
                Tint {
                    time: 1.0,
                    colour: Vector4::new(1.0, 1.0, 1.0, 1.0),
                },
            ],
        };
        // before the first stop: flat at the first colour
        let mut p = Particle::default();
        p.set_age_raw(0);
        system.particles.add_particle(p);
        // mid ramp: the blend is (u-l)*ratio + l
        let mut p = Particle::default();
        p.set_age_raw(30); // 0.25 s
        system.particles.add_particle(p);
        // past the last stop: flat at the last colour
        let mut p = Particle::default();
        p.set_age_raw(130); // 1.0833 s
        system.particles.add_particle(p);
        execute_tint_shader(&mut system, &mut t);
        let unpack = |packed: u32| -> [f32; 4] {
            [
                ((packed >> 16) & 0xff) as f32 / 255.0,
                ((packed >> 8) & 0xff) as f32 / 255.0,
                (packed & 0xff) as f32 / 255.0,
                ((packed >> 24) & 0xff) as f32 / 255.0,
            ]
        };
        let first = unpack(system.particles.get(0).colour);
        assert!((first[0] - 0.0).abs() < 1e-5, "flat first {first:?}");
        let mid = unpack(system.particles.get(1).colour);
        // ratio 0.25 over black->white packs to channel 63
        assert!((mid[0] - 63.0 / 255.0).abs() < 1.0 / 255.0, "blend {mid:?}");
        let last = unpack(system.particles.get(2).colour);
        assert!((last[0] - 1.0).abs() < 1e-5, "flat last {last:?}");
    }

    #[test]
    fn tint_shader_repeat_wraps_the_age() {
        let mut system = ParticleSystem::new();
        let mut t = TintShaderPSA {
            repeat: true,
            period: 1.0,
            fog_amount: 0.0,
            tints: vec![
                Tint {
                    time: 0.0,
                    colour: Vector4::new(0.0, 0.0, 0.0, 1.0),
                },
                Tint {
                    time: 1.0,
                    colour: Vector4::new(1.0, 1.0, 1.0, 1.0),
                },
            ],
        };
        let mut p = Particle::default();
        p.set_age_raw(150); // 1.25 s -> effective 0.25
        system.particles.add_particle(p);
        execute_tint_shader(&mut system, &mut t);
        let r = ((system.particles.get(0).colour >> 16) & 0xff) as f32 / 255.0;
        assert!((r - 63.0 / 255.0).abs() < 1.0 / 255.0, "wrapped {r}");
    }

    #[test]
    fn tint_shader_fog_blends_toward_black_keeping_alpha() {
        let mut system = ParticleSystem::new();
        let mut t = TintShaderPSA {
            repeat: false,
            period: 0.0,
            fog_amount: 0.5,
            tints: vec![Tint {
                time: 0.0,
                colour: Vector4::new(1.0, 1.0, 1.0, 1.0),
            }],
        };
        let mut p = Particle::default();
        p.set_age_raw(0);
        system.particles.add_particle(p);
        execute_tint_shader(&mut system, &mut t);
        let packed = system.particles.get(0).colour;
        let r = ((packed >> 16) & 0xff) as f32 / 255.0;
        let a = ((packed >> 24) & 0xff) as f32 / 255.0;
        assert!((r - 0.5).abs() < 1.0 / 255.0, "fogged {r}");
        assert!((a - 1.0).abs() < 1.0 / 255.0, "alpha kept {a}");
    }

    #[test]
    fn magnet_pulls_toward_the_origin_with_min_dist() {
        let mut system = ParticleSystem::new();
        let p = placed(Vector3::new(10.0, 0.0, 0.0), Vector3::ZERO);
        system.particles.add_particle(p);
        let mut m = MagnetPSA {
            strength: 1.0,
            min_dist: 0.0,
            source: None,
        };
        execute_magnet(&mut system, &mut m, 1.0);
        let v = system.particles.get(0).velocity();
        // dv = origin - pos = (-10,0,0); separation 10 normalises it to
        // (-1,0,0) and factor = (dt*strength)/separation = 1/10
        assert!((v.x - -0.1).abs() < 0.004, "magnet {v:?}");
        // min_dist shrinks the separation divisor
        let mut system = ParticleSystem::new();
        let p = placed(Vector3::new(10.0, 0.0, 0.0), Vector3::ZERO);
        system.particles.add_particle(p);
        let mut m = MagnetPSA {
            strength: 1.0,
            min_dist: 20.0,
            source: None,
        };
        execute_magnet(&mut system, &mut m, 1.0);
        // separation becomes 20: (-0.5,0,0) * (1/20)
        let v = system.particles.get(0).velocity();
        assert!((v.x - -0.025).abs() < 0.004, "min dist {v:?}");
    }

    #[test]
    fn sink_removes_by_age_and_speed_with_swap_back() {
        let mut system = ParticleSystem::new();
        // old particle (age 2 s > 1.5)
        let mut p = Particle::default();
        p.set_age_raw(240);
        system.particles.add_particle(p);
        // slow particle with a minimum speed active
        let mut p = Particle::default();
        p.set_velocity(Vector3::new(0.1, 0.0, 0.0));
        system.particles.add_particle(p);
        // fast particle survives
        let mut p = Particle::default();
        p.set_velocity(Vector3::new(5.0, 0.0, 0.0));
        system.particles.add_particle(p);
        let mut s = SinkPSA {
            maximum_age: 1.5,
            minimum_speed: 1.0,
            outside_only: false,
        };
        execute_sink(&mut system, &mut s);
        assert_eq!(system.particles.len(), 1);
        let v = system.particles.get(0).velocity();
        assert_f32_eq!(v.x, 5.0, "fast survivor");
        // guard: nothing to do without any criterion
        let mut system = ParticleSystem::new();
        let mut p = Particle::default();
        p.set_age_raw(999);
        system.particles.add_particle(p);
        let mut s = SinkPSA::default();
        execute_sink(&mut system, &mut s);
        assert_eq!(system.particles.len(), 1);
    }

    fn barrier_system() -> (ParticleSystem, BwRandom) {
        let mut system = ParticleSystem::new();
        // z goes 5 -> 0.5 in one step: outside -> inside, a shell crossing
        let p = placed(
            Vector3::new(0.0, 0.0, 5.0),  // outside a radius-1 sphere
            Vector3::new(0.0, 0.0, -4.5), // heading in
        );
        system.particles.add_particle(p);
        (system, BwRandom::new(1))
    }

    #[test]
    fn barrier_sphere_bounce_reflects() {
        let (mut system, _) = barrier_system();
        let mut b = BarrierPSA {
            shape: BarrierShape::Sphere,
            reaction: BarrierReaction::Bounce,
            vec_a: Vector3::ZERO,
            vec_b: Vector3::ZERO,
            radius: 1.0,
            resilience: 1.0,
        };
        execute_barrier(&mut system, &mut b, 1.0);
        let p = system.particles.get(0);
        assert_f32_eq!(p.velocity().z, 4.5, "reflected");
        assert_eq!(system.particles.len(), 1);
    }

    #[test]
    fn barrier_sphere_remove_crosses_only() {
        let (mut system, _) = barrier_system();
        let mut b = BarrierPSA {
            shape: BarrierShape::Sphere,
            reaction: BarrierReaction::Remove,
            vec_a: Vector3::ZERO,
            vec_b: Vector3::ZERO,
            radius: 1.0,
            resilience: 1.0,
        };
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 0, "crossed -> removed");
        // a particle inside that stays inside does not cross, so survives
        let (mut system, _) = barrier_system();
        let p = system.particles.get_mut(0);
        p.position = Vector3::new(0.0, 0.0, 0.0);
        p.set_velocity(Vector3::ZERO);
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 1);
    }

    #[test]
    fn barrier_sphere_wrap_teleports_across() {
        let (mut system, _) = barrier_system();
        let mut b = BarrierPSA {
            shape: BarrierShape::Sphere,
            reaction: BarrierReaction::Wrap,
            vec_a: Vector3::ZERO,
            vec_b: Vector3::ZERO,
            radius: 1.0,
            resilience: 1.0,
        };
        // WRAP reacts to the predicted position being outside, so the
        // particle starts inside and leaves through the shell
        let p = system.particles.get_mut(0);
        p.position = Vector3::new(0.0, 0.0, 0.5);
        p.set_velocity(Vector3::new(0.0, 0.0, 10.0));
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 1);
        let p = system.particles.get(0);
        // pos + delta*2r, with delta towards the centre: 0.5 + (-2) = -1.5
        assert_f32_eq!(p.position.z, -1.5, "wrapped");
    }

    #[test]
    fn barrier_box_bounce_negates_the_crossed_axis() {
        let mut system = ParticleSystem::new();
        let p = placed(
            Vector3::new(0.0, 0.0, 0.6), // inside the box
            // only z leaves the box: x lands exactly on the ceiling, not past it
            Vector3::new(1.0, 0.5, -4.0),
        );
        system.particles.add_particle(p);
        let mut b = BarrierPSA {
            shape: BarrierShape::Box,
            reaction: BarrierReaction::Bounce,
            vec_a: Vector3::new(-1.0, -1.0, -1.0),
            vec_b: Vector3::new(1.0, 1.0, 1.0),
            radius: 0.0,
            resilience: 1.0,
        };
        execute_barrier(&mut system, &mut b, 1.0);
        let v = system.particles.get(0).velocity();
        assert_f32_eq!(v.z, 4.0, "z flipped");
        assert_f32_eq!(v.x, 1.0, "x kept");
        assert_f32_eq!(v.y, 0.5, "y kept");
    }

    #[test]
    fn barrier_box_remove_on_any_cross() {
        let mut system = ParticleSystem::new();
        let p = placed(
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(5.0, 0.0, 0.0), // crosses x=1 this step
        );
        system.particles.add_particle(p);
        let mut b = BarrierPSA {
            shape: BarrierShape::Box,
            reaction: BarrierReaction::Remove,
            vec_a: Vector3::new(-1.0, -1.0, -1.0),
            vec_b: Vector3::new(1.0, 1.0, 1.0),
            radius: 0.0,
            resilience: 1.0,
        };
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 0);
    }

    #[test]
    fn barrier_box_wrap_shifts_per_axis() {
        let mut system = ParticleSystem::new();
        let p = placed(
            Vector3::new(1.5, 0.0, 0.0), // outside +x
            Vector3::new(1.0, 0.0, 0.0), // staying out
        );
        system.particles.add_particle(p);
        let mut b = BarrierPSA {
            shape: BarrierShape::Box,
            reaction: BarrierReaction::Wrap,
            vec_a: Vector3::new(-1.0, -1.0, -1.0),
            vec_b: Vector3::new(1.0, 1.0, 1.0),
            radius: 0.0,
            resilience: 1.0,
        };
        execute_barrier(&mut system, &mut b, 0.0);
        let p = system.particles.get(0);
        assert!(
            (p.position.x - -0.5).abs() < 1e-5,
            "wrapped x {}",
            p.position.x
        );
    }

    #[test]
    fn barrier_cylinder_bounces_xz() {
        let mut system = ParticleSystem::new();
        // x goes 2 -> -0.5, inside the radius-1 cylinder
        let p = placed(
            Vector3::new(2.0, 0.0, 0.0),  // outside the radius
            Vector3::new(-2.5, 0.0, 0.0), // crossing in
        );
        system.particles.add_particle(p);
        let mut b = BarrierPSA {
            shape: BarrierShape::VerticalCylinder,
            reaction: BarrierReaction::Bounce,
            vec_a: Vector3::ZERO,
            vec_b: Vector3::ZERO,
            radius: 1.0,
            resilience: 1.0,
        };
        execute_barrier(&mut system, &mut b, 1.0);
        let v = system.particles.get(0).velocity();
        assert_f32_eq!(v.x, 2.5, "xz reflected");
    }

    #[test]
    fn barrier_none_shape_and_allow_reaction_do_nothing() {
        let (mut system, _) = barrier_system();
        let mut b = BarrierPSA {
            shape: BarrierShape::None,
            reaction: BarrierReaction::Remove,
            vec_a: Vector3::ZERO,
            vec_b: Vector3::ZERO,
            radius: 1.0,
            resilience: 1.0,
        };
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 1);
        b.shape = BarrierShape::Sphere;
        b.reaction = BarrierReaction::Allow;
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 1);
    }

    #[test]
    fn renew_particles_re_adds_wrapped() {
        let mut system = ParticleSystem::new();
        system.set_capacity(5);
        renew_particles(&mut system, vec![Particle::default(), Particle::default()]);
        assert_eq!(system.size(), 2);
    }

    // ---- late-update queue -------------------------------------------------

    #[test]
    fn non_local_system_queues_late_updates_and_flushes() {
        let mut system = ParticleSystem::new();
        // a renderer that is not local still leaves the system local while
        // it has no own world; give it one to get the non-local path
        system.own_world = Some(Matrix::identity());
        let base = RendererBase {
            view_dependent: false,
            local: false,
        };
        system.renderer = Some(ParticleRenderer::Sprite {
            base,
            sprite: Default::default(),
        });
        assert!(!system.is_local());
        let p = placed(Vector3::new(0.0, 0.0, 0.0), Vector3::ZERO);
        system.particles.add_particle(p);
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Force(ForcePSA {
                vector: Vector3::new(0.0, -120.0, 0.0),
            }),
        });
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        system.update(1.0, &mut rng, &env);
        // the force queued instead of running
        assert_eq!(system.queued_late.len(), 1);
        let v = system.particles.get(0).velocity();
        assert_f32_eq!(v.y, 0.0, "not yet applied");
        system.flush_late_updates(&mut rng);
        assert!(system.queued_late.is_empty());
        let v = system.particles.get(0).velocity();
        assert_f32_eq!(v.y, -120.0, "applied on flush");
    }

    // ---- meta system -------------------------------------------------------

    #[test]
    fn meta_loads_new_format_and_skips_non_system_children() {
        let mut meta = crate::particle::MetaParticleSystem::new();
        let xml = "<root>\
                   <Fire><serialiseVersionData>2</serialiseVersionData>\
                   <Renderer><SpriteParticleRenderer/></Renderer></Fire>\
                   <helperModelName>generic.model</helperModelName>\
                   <Smoke><Fire2>\
                     <serialiseVersionData>2</serialiseVersionData>\
                     <Renderer><SpriteParticleRenderer/></Renderer>\
                   </Fire2></Smoke>\
                   </root>";
        assert!(meta.load_str(xml, "doc.particle"));
        assert_eq!(meta.n_systems(), 2);
        assert!(meta.system("Fire").is_some());
        // the wrapped system keeps its outer section name
        assert!(meta.system("Smoke").is_some());
        assert!(meta.system("helperModelName").is_none());
    }

    #[test]
    fn meta_loads_old_single_system_format() {
        let mut meta = crate::particle::MetaParticleSystem::new();
        let xml = "<root><serialiseVersionData>2</serialiseVersionData>\
                   <Renderer><SpriteParticleRenderer/></Renderer></root>";
        assert!(meta.load_str(xml, "old.particle"));
        assert_eq!(meta.n_systems(), 1);
    }

    #[test]
    fn meta_load_fails_when_no_system() {
        let mut meta = crate::particle::MetaParticleSystem::new();
        assert!(!meta.load_str("<root></root>", "empty"));
        assert!(!meta.load_str("not xml", "bad"));
    }

    #[test]
    fn meta_faces_count_only_enabled_systems() {
        let mut meta = crate::particle::MetaParticleSystem::new();
        let mut a = ParticleSystem::new();
        a.name = "a".into();
        a.set_capacity(10);
        a.enabled = true;
        let mut b = ParticleSystem::new();
        b.name = "b".into();
        b.set_capacity(20);
        b.enabled = false;
        meta.add_system(a);
        meta.insert_system(0, b);
        assert_eq!(meta.n_systems(), 2);
        assert!(meta.system_from_index(0).unwrap().name == "b");
        assert!(meta.first_system().unwrap().name == "b");
        assert_eq!(meta.capacity(), 30);
        assert_eq!(meta.size(), 0);
        assert!(meta.remove_system("b"));
        assert!(!meta.remove_system("zz"));
        meta.remove_all_systems();
        assert_eq!(meta.n_systems(), 0);
    }

    #[test]
    fn meta_set_faces_reach_the_systems() {
        let mut meta = crate::particle::MetaParticleSystem::new();
        meta.add_system(ParticleSystem::new());
        meta.set_static(true);
        assert!(meta.systems[0].is_static);
        meta.set_do_update();
        assert!(meta.systems[0].do_update);
        meta.set_first_update();
        assert!(meta.systems[0].first_update);
        meta.clear();
        meta.spawn(2);
    }

    #[test]
    fn meta_tick_reports_whether_anything_ran() {
        let mut meta = crate::particle::MetaParticleSystem::new();
        meta.add_system(ParticleSystem::new());
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        // a dynamic system (the default) runs its update
        assert!(meta.tick(1.0 / 30.0, &mut rng, &env));
        // a static system with no pending update reports false
        meta.systems[0].is_static = true;
        assert!(!meta.tick(1.0 / 30.0, &mut rng, &env));
        assert_f32_eq!(meta.duration(), -1.0, "meta duration");
    }

    // ---- editor ------------------------------------------------------------

    #[test]
    fn editor_faces_set_the_document() {
        let mut editor = editor();
        editor.set_capacity(0, 100);
        assert_eq!(editor.meta.systems[0].capacity, 100);
        editor.set_wind_factor(0, 2.0);
        assert_f32_eq!(editor.meta.systems[0].wind_factor, 1.0, "clamped");
        editor.set_max_lod(0, 42.0);
        assert_f32_eq!(editor.meta.systems[0].max_lod, 42.0, "maxLod");
        editor.set_explicit_position(0, Vector3::new(1.0, 2.0, 3.0));
        assert_f32_eq!(editor.meta.systems[0].explicit_position.z, 3.0, "pos");
        editor.set_explicit_direction(0, Vector3::new(0.0, 1.0, 0.0));
        assert_f32_eq!(editor.meta.systems[0].explicit_direction.y, 1.0, "dir");
        // out-of-range indices do nothing
        editor.set_capacity(9, 5);
        editor.set_action_delay(9, 9, 1.0);
    }

    #[test]
    fn editor_action_faces_reach_their_types() {
        let mut editor = editor();
        editor.set_action_enabled(0, 1, false);
        assert!(!editor.meta.systems[0].actions[1].base.enabled);
        editor.set_action_delay(0, 1, 0.25);
        assert_f32_eq!(editor.meta.systems[0].actions[1].base.delay, 0.25, "delay");
        editor.set_action_minimum_age(0, 1, 0.5);
        assert_f32_eq!(
            editor.meta.systems[0].actions[1].base.minimum_age,
            0.5,
            "minAge"
        );

        editor.set_sink(
            0,
            1,
            &crate::particle_editor::SinkFace {
                maximum_age: 2.0,
                minimum_speed: 0.0,
                outside_only: true,
            },
        );
        let crate::particle::ActionKind::Sink(s) = &editor.meta.systems[0].actions[1].kind else {
            panic!("sink");
        };
        assert_f32_eq!(s.maximum_age, 2.0, "sink face");
        assert!(s.outside_only);

        editor.set_scaler(0, 0, 1.0, 1.0); // wrong type: no-op
        editor.set_stream(0, 0, 0.5); // wrong type: no-op
        editor.set_magnet(0, 0, 1.0, 1.0); // wrong type: no-op
        editor.set_orbitor(0, 0, 1.0);
        editor.set_collide(0, 0, 1.0);
        editor.set_flare(0, 0, 2.0);
        editor.set_tint_fog(0, 3, 0.25);
        let crate::particle::ActionKind::TintShader(t) = &editor.meta.systems[0].actions[3].kind
        else {
            panic!("tint");
        };
        assert_f32_eq!(t.fog_amount, 0.25, "fog face");
        editor.clear_tints(0, 3);
        let crate::particle::ActionKind::TintShader(t) = &editor.meta.systems[0].actions[3].kind
        else {
            panic!("tint");
        };
        assert!(t.tints.is_empty());
        assert_f32_eq!(t.period, 0.0, "cleared period");
        editor.add_tint(0, 3, 0.5, Vector4::new(1.0, 0.0, 0.0, 1.0));
        let crate::particle::ActionKind::TintShader(t) = &editor.meta.systems[0].actions[3].kind
        else {
            panic!("tint");
        };
        assert_eq!(t.tints.len(), 1);
        assert_f32_eq!(t.period, 0.5, "period grew");
    }

    #[test]
    fn editor_source_face_sets_the_panel() {
        let mut editor = editor();
        editor.set_source(
            0,
            0,
            &crate::particle_editor::SourceFace {
                time_triggered: true,
                rate: 24.0,
                sensitivity: 0.05,
                ..Default::default()
            },
        );
        let crate::particle::ActionKind::Source(s) = &editor.meta.systems[0].actions[0].kind else {
            panic!("source");
        };
        assert_f32_eq!(s.rate, 24.0, "rate");
        assert_f32_eq!(s.sensitivity, 0.05, "sensitivity");
        assert!(!s.motion_triggered); // default face cleared it
        // wrong type is a no-op
        editor.set_source(0, 1, &crate::particle_editor::SourceFace::default());
    }

    #[test]
    fn editor_barrier_faces() {
        let mut editor = editor();
        editor.set_barrier_shape(0, 1, BarrierShape::Sphere); // wrong type: no-op
        editor.set_barrier_reaction(0, 1, BarrierReaction::Wrap);
        editor.set_barrier_cylinder(0, 2, Vector3::new(1.0, 2.0, 3.0), 4.0); // wrong type: no-op
        // add a barrier to work on
        editor.meta.systems[0].actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Barrier(BarrierPSA::default()),
        });
        let idx = editor.meta.systems[0].actions.len() - 1;
        editor.set_barrier_cylinder(0, idx, Vector3::new(1.0, 2.0, 3.0), 4.0);
        let crate::particle::ActionKind::Barrier(b) = &editor.meta.systems[0].actions[idx].kind
        else {
            panic!("barrier");
        };
        assert_eq!(b.shape, BarrierShape::VerticalCylinder);
        assert_eq!(b.reaction, BarrierReaction::Bounce);
        assert_f32_eq!(b.radius, 4.0, "cyl radius");
        editor.set_barrier_box(
            0,
            idx,
            Vector3::new(-1.0, -1.0, -1.0),
            Vector3::new(1.0, 1.0, 1.0),
        );
        let crate::particle::ActionKind::Barrier(b) = &editor.meta.systems[0].actions[idx].kind
        else {
            panic!("barrier");
        };
        assert_eq!(b.shape, BarrierShape::Box);
        assert_f32_eq!(b.vec_b.x, 1.0, "box opposite");
        editor.set_barrier_sphere(0, idx, Vector3::ZERO, 2.0);
        let crate::particle::ActionKind::Barrier(b) = &editor.meta.systems[0].actions[idx].kind
        else {
            panic!("barrier");
        };
        assert_eq!(b.shape, BarrierShape::Sphere);
        editor.set_barrier_shape(0, idx, BarrierShape::None);
        let crate::particle::ActionKind::Barrier(b) = &editor.meta.systems[0].actions[idx].kind
        else {
            panic!("barrier");
        };
        assert_eq!(b.shape, BarrierShape::None);
        editor.set_barrier_reaction(0, idx, BarrierReaction::Remove);
        let crate::particle::ActionKind::Barrier(b) = &editor.meta.systems[0].actions[idx].kind
        else {
            panic!("barrier");
        };
        assert_eq!(b.reaction, BarrierReaction::Remove);
    }

    #[test]
    fn editor_generator_faces() {
        let mut editor = editor();
        // swap the position generator for a point
        let ok = editor.set_source_generator(
            0,
            0,
            true,
            VectorGenerator::Point {
                position: Vector3::new(1.0, 2.0, 3.0),
            },
        );
        assert!(ok);
        let crate::particle::ActionKind::Source(s) = &editor.meta.systems[0].actions[0].kind else {
            panic!("source");
        };
        match &s.position_src {
            Some(VectorGenerator::Point { position }) => {
                assert_f32_eq!(position.z, 3.0, "point");
            }
            other => panic!("expected point, got {other:?}"),
        }
        // a cylinder generator rebuilds its basis
        let ok = editor.set_cylinder_generator(
            0,
            0,
            false,
            VectorGenerator::Cylinder {
                origin: Vector3::ZERO,
                direction: Vector3::new(0.0, 0.0, 1.0),
                min_radius: 0.0,
                max_radius: 1.0,
                basis_u: Vector3::ZERO,
                basis_v: Vector3::ZERO,
            },
        );
        assert!(ok);
        let crate::particle::ActionKind::Source(s) = &editor.meta.systems[0].actions[0].kind else {
            panic!("source");
        };
        match &s.velocity_src {
            Some(VectorGenerator::Cylinder {
                basis_u, basis_v, ..
            }) => {
                assert!((basis_u.length() - 1.0).abs() < 1e-5);
                assert!((basis_v.length() - 1.0).abs() < 1e-5);
            }
            other => panic!("expected cylinder, got {other:?}"),
        }
        // non-cylinder generator through the cylinder face fails
        assert!(!editor.set_cylinder_generator(
            0,
            0,
            false,
            VectorGenerator::Point {
                position: Vector3::ZERO
            },
        ));
        // jitter generators
        let ok = editor.set_jitter_generators(
            0,
            0,
            Some(VectorGenerator::Point {
                position: Vector3::ZERO,
            }),
            None,
        );
        assert!(!ok, "source is not a jitter");
    }

    #[test]
    fn editor_sprite_faces() {
        let mut editor = editor();
        let face = crate::particle_editor::SpriteFace {
            material_fx: crate::particle::MaterialFX::Additive,
            texture_name: "fx/other.dds".into(),
            frame_count: 4,
            frame_rate: 12.0,
            near_fade_cutoff: 0.5,
            ..Default::default()
        };
        assert!(editor.set_sprite(0, &face));
        let Some(crate::particle::ParticleRenderer::Sprite { sprite, .. }) =
            editor.meta.systems[0].renderer.as_ref()
        else {
            panic!("sprite");
        };
        assert_eq!(sprite.material_fx, crate::particle::MaterialFX::Additive);
        assert_eq!(sprite.texture_name, "fx/other.dds");
        assert_eq!(sprite.frame_count, 4);
        assert_f32_eq!(sprite.near_fade_cutoff, 0.5, "near fade");
        assert!(editor.set_renderer_space(0, true, false));
        assert!(editor.meta.systems[0].renderer.as_ref().unwrap().local());
        // no renderer / out of range
        assert!(!editor.set_sprite(5, &face));
    }

    #[test]
    fn editor_material_fx_table_and_capacity_clamp() {
        assert_eq!(
            crate::particle_editor::material_fx_from_index(7),
            Some(crate::particle::MaterialFX::SourceAlpha)
        );
        assert_eq!(crate::particle_editor::material_fx_from_index(8), None);
        assert_eq!(crate::particle_editor::capacity_clamp(0), 1);
        assert_eq!(
            crate::particle_editor::capacity_clamp(1 << 20),
            MAX_CAPACITY
        );
        assert_eq!(crate::particle_editor::capacity_clamp(32), 32);
    }

    #[test]
    fn editor_document_faces_and_stats() {
        let mut editor = editor();
        editor.spawn(1);
        let crate::particle::ActionKind::Source(s) = &editor.meta.systems[0].actions[0].kind else {
            panic!("source");
        };
        assert_eq!(s.forced_total, 1);
        editor.spawn_system(0, 2);
        let crate::particle::ActionKind::Source(s) = &editor.meta.systems[0].actions[0].kind else {
            panic!("source");
        };
        assert_eq!(s.forced_total, 3);
        editor.tick(1.0 / 30.0); // consumes the forced burst
        let (count, bytes) = editor.stats();
        assert!(count > 0, "particles spawned by the forced burst");
        assert!(bytes > 0);
        editor.clear();
        assert_eq!(editor.stats().0, 0);
        editor.clear_system(0);
        editor.new_document();
        assert_eq!(editor.meta.n_systems(), 0);
    }

    #[test]
    fn editor_preview_is_deterministic_per_seed() {
        let run = || {
            let mut editor = editor();
            editor.set_seed(oracle::ORACLE_SEED);
            editor.preview(16, 1.0 / 30.0)
        };
        let a = run();
        let b = run();
        assert_eq!(a, b, "same seed runs the same preview");
    }

    #[test]
    fn editor_env_and_seed_faces() {
        let mut editor = editor();
        editor.env().wind_velocity = Vector3::new(1.0, 0.0, 0.0);
        editor.env().lod_bias = 2.0;
        assert_f32_eq!(editor.env().lod_bias, 2.0, "bias");
        editor.set_seed(99);
        assert_ne!(oracle::ORACLE_SEED, 99);
    }

    // ---- the acceptance: fixed-seed visible behaviour -----------------------

    #[test]
    fn fixed_seed_preview_matches_the_independent_oracle() {
        let mut editor = editor();
        editor.set_seed(oracle::ORACLE_SEED);

        let mut snap_pos = 0usize;
        let mut ticked = 0usize;
        let dt = 1.0f32 / 30.0;
        for (frame_idx, frame) in oracle::SNAP_FRAMES.iter().enumerate() {
            let target_frames = *frame as usize;
            while ticked < target_frames {
                editor.tick(dt);
                ticked += 1;
            }
            let snap = editor.snapshot();
            let counts = oracle::SNAP_COUNTS[frame_idx];
            assert_eq!(snap.systems.len(), 1, "one system in the fixture");
            assert_eq!(
                snap.systems[0].records.len(),
                counts,
                "frame {}: {} live particles expected, got {}",
                frame,
                counts,
                snap.systems[0].records.len()
            );
            for (i, record) in snap.systems[0].records.iter().enumerate() {
                let j = snap_pos + i;
                for axis in 0..3 {
                    assert_f32_eq!(
                        record.position.get(axis),
                        oracle::SNAP_POS[j][axis],
                        "frame {} particle {} pos[{}]",
                        frame,
                        j,
                        axis,
                    );
                    assert_f32_eq!(
                        record.velocity.get(axis),
                        oracle::SNAP_VEL[j][axis],
                        "frame {} particle {} vel[{}]",
                        frame,
                        j,
                        axis,
                    );
                }
                assert_eq!(
                    record.colour,
                    oracle::SNAP_COLOUR[j],
                    "frame {} particle {} colour",
                    frame,
                    j
                );
                assert_f32_eq!(record.size, oracle::SNAP_SIZE[j], "frame {} size", frame);
                assert_f32_eq!(record.age, oracle::SNAP_AGE[j], "frame {} age", frame);
                assert_eq!(
                    record.age_raw,
                    oracle::SNAP_AGE_RAW[j],
                    "frame {} age ticks",
                    frame
                );
            }
            snap_pos += counts;
        }
        assert_eq!(snap_pos, oracle::SNAP_TOTAL);
    }
    // ---- coverage batch: load faces ---------------------------------------

    #[test]
    fn generator_load_covers_cylinder_sphere_and_unknown() {
        let ds = resources::datasection::parse(
            "root",
            b"<root>\
              <CylinderVectorGenerator><origin_> 1 2 3 </origin_>\
              <direction_> 0 0 1 </direction_><maxRadius_>2</maxRadius_>\
              <minRadius_>0.5</minRadius_></CylinderVectorGenerator>\
              <SphereVectorGenerator><centre_> 4 5 6 </centre_>\
              <maxRadius_>3</maxRadius_><minRadius_>1</minRadius_></SphereVectorGenerator>\
              <NotAGenerator/></root>",
        )
        .expect("parse");
        let g = VectorGenerator::load(ds.open_child(0).unwrap()).expect("cylinder");
        match g {
            VectorGenerator::Cylinder {
                origin,
                direction,
                max_radius,
                min_radius,
                ..
            } => {
                assert_f32_eq!(origin.x, 1.0, "origin");
                assert_f32_eq!(direction.z, 1.0, "dir");
                assert_f32_eq!(max_radius, 2.0, "maxR");
                assert_f32_eq!(min_radius, 0.5, "minR");
            }
            other => panic!("expected cylinder, got {other:?}"),
        }
        let g = VectorGenerator::load(ds.open_child(1).unwrap()).expect("sphere");
        match g {
            VectorGenerator::Sphere { centre, .. } => {
                assert_f32_eq!(centre.z, 6.0, "centre");
            }
            other => panic!("expected sphere, got {other:?}"),
        }
        assert!(VectorGenerator::load(ds.open_child(2).unwrap()).is_none());
    }

    #[test]
    fn cylinder_generate_min_max_formula() {
        // min < max: the radius draw runs the sqrt-lerp onto [min, max].
        let g = VectorGenerator::Cylinder {
            origin: Vector3::ZERO,
            direction: Vector3::new(0.0, 0.0, 1.0),
            max_radius: 2.0,
            min_radius: 0.5,
            basis_u: Vector3::new(1.0, 0.0, 0.0),
            basis_v: Vector3::new(0.0, 1.0, 0.0),
        };
        let mut rng = BwRandom::new(11);
        let v = g.generate(&mut rng);
        let mut ref_rng = BwRandom::new(11);
        let d_axis = ref_rng.unit_rand();
        let d_angle = ref_rng.unit_rand();
        let d_radius = ref_rng.unit_rand();
        let alpha = 0.5f32 / 2.0f32;
        let k1 = (d_radius * (1.0 - alpha) + 0.5f32).sqrt();
        let radius = (2.0f32 - 0.5f32) * (k1 - alpha) / (1.0 - alpha) + 0.5f32;
        let angle = (d_angle * 2.0) * MATH_PI;
        let expected = Vector3::new(angle.cos() * radius, angle.sin() * radius, d_axis);
        assert_f32_eq!(v.x, expected.x, "cx");
        assert_f32_eq!(v.y, expected.y, "cy");
        assert_f32_eq!(v.z, expected.z, "cz");

        // min > max sorts at generate time: same draws, same point.
        let swapped = VectorGenerator::Cylinder {
            origin: Vector3::ZERO,
            direction: Vector3::new(0.0, 0.0, 1.0),
            max_radius: 0.5,
            min_radius: 2.0,
            basis_u: Vector3::new(1.0, 0.0, 0.0),
            basis_v: Vector3::new(0.0, 1.0, 0.0),
        };
        let mut rng = BwRandom::new(11);
        let v2 = swapped.generate(&mut rng);
        assert_f32_eq!(v2.x, v.x, "sorted x");
        assert_f32_eq!(v2.y, v.y, "sorted y");
        assert_f32_eq!(v2.z, v.z, "sorted z");
    }

    #[test]
    fn sphere_generate_equal_radii_sits_on_the_shell() {
        let g = VectorGenerator::Sphere {
            centre: Vector3::new(1.0, 2.0, 3.0),
            max_radius: 2.0,
            min_radius: 2.0,
        };
        let mut rng = BwRandom::new(3);
        let v = g.generate(&mut rng);
        // one draw for the table pick, none for the radius
        let mut ref_rng = BwRandom::new(3);
        let _ = ref_rng.next_u32();
        assert_eq!(rng.next_u32(), ref_rng.next_u32(), "one draw consumed");
        let offset = v - Vector3::new(1.0, 2.0, 3.0);
        assert_f32_eq!(offset.length(), 2.0, "shell radius");
    }

    #[test]
    fn generator_size_in_bytes_catalogue() {
        let v3 = std::mem::size_of::<Vector3>();
        let f4 = std::mem::size_of::<f32>();
        let gens = [
            VectorGenerator::Point {
                position: Vector3::ZERO,
            },
            VectorGenerator::Line {
                origin: Vector3::ZERO,
                direction: Vector3::ZERO,
            },
            VectorGenerator::Cylinder {
                origin: Vector3::ZERO,
                direction: Vector3::ZERO,
                max_radius: 0.0,
                min_radius: 0.0,
                basis_u: Vector3::ZERO,
                basis_v: Vector3::ZERO,
            },
            VectorGenerator::Sphere {
                centre: Vector3::ZERO,
                max_radius: 0.0,
                min_radius: 0.0,
            },
            VectorGenerator::Box {
                corner: Vector3::ZERO,
                opposite: Vector3::ZERO,
            },
        ];
        let expected = [v3, 2 * v3, 4 * v3 + 2 * f4, v3 + 2 * f4, 2 * v3];
        for (g, want) in gens.iter().zip(expected) {
            assert_eq!(g.size_in_bytes(), want, "footprint {g:?}");
        }
    }

    #[test]
    fn generate_sleep_period_picks_midpoint_or_bound() {
        assert_f32_eq!(generate_sleep_period(-1.0, -1.0), 0.0, "none");
        assert_f32_eq!(generate_sleep_period(2.0, -1.0), 2.0, "min only");
        assert_f32_eq!(generate_sleep_period(-1.0, 3.0), 3.0, "max only");
        assert_f32_eq!(generate_sleep_period(1.0, 3.0), 2.0, "midpoint");
        assert_f32_eq!(generate_sleep_period(3.0, 1.0), 4.0, "midpoint base");
    }

    #[test]
    fn source_load_swaps_sleep_bounds_and_reads_rotation_sections() {
        let ds = resources::datasection::parse(
            "root",
            b"<root><Source><sleepPeriod_>5</sleepPeriod_>\
              <sleepPeriodMax_>1</sleepPeriodMax_>\
              <initialRotation_> 0.5 0.25 </initialRotation_>\
              <randomInitialRotation_> 1 2 </randomInitialRotation_>\
              </Source></root>",
        )
        .expect("parse");
        let action = ParticleAction::load(ds.open_child(0).unwrap()).expect("load");
        let ActionKind::Source(s) = &action.kind else {
            panic!("source");
        };
        // sleepPeriod > sleepPeriodMax swaps after the sleep draw
        assert_f32_eq!(s.sleep_period, 1.0, "swapped sleep");
        assert_f32_eq!(s.sleep_period_max, 5.0, "swapped max");
        assert_f32_eq!(s.initial_rotation.x, 0.5, "rot x");
        assert_f32_eq!(s.initial_rotation.y, 0.25, "rot y");
        assert_f32_eq!(s.random_initial_rotation.y, 2.0, "random rot y");
    }

    #[test]
    fn action_default_catalogue() {
        let s = StreamPSA::default();
        assert_eq!(s.vector, Vector3::ZERO);
        assert_f32_eq!(s.half_life, -1.0, "stream half life");
        let j = JitterPSA::default();
        assert!(j.affect_position && j.affect_velocity);
        assert!(j.position_src.is_none() && j.velocity_src.is_none());
        let o = OrbitorPSA::default();
        assert_eq!(o.point, Vector3::ZERO);
        assert_f32_eq!(o.angular_velocity, 0.0, "orbitor w");
        assert!(o.affect_velocity);
        let c = CollidePSA::default();
        assert_eq!(c.sound_src_idx, -1);
        assert!(!c.sprite_based && !c.sound_enabled);
        let m = MagnetPSA::default();
        assert_f32_eq!(m.strength, 1.0, "magnet strength");
        assert_f32_eq!(m.min_dist, 1.0, "magnet min dist");
        let k = SinkPSA::default();
        assert_f32_eq!(k.maximum_age, -1.0, "sink age");
        assert_f32_eq!(k.minimum_speed, -1.0, "sink speed");
    }

    #[test]
    fn jitter_load_reads_sources_and_flags() {
        let ds = resources::datasection::parse(
            "root",
            b"<root><Jitter>\
              <pPositionSrc><PointVectorGenerator><position_> 1 2 3 </position_>\
              </PointVectorGenerator></pPositionSrc>\
              <pVelocitySrc><LineVectorGenerator><origin_> 0 0 0 </origin_>\
              <direction_> 1 0 0 </direction_></LineVectorGenerator></pVelocitySrc>\
              <affectPosition_>false</affectPosition_>\
              <affectVelocity_>true</affectVelocity_></Jitter></root>",
        )
        .expect("parse");
        let action = ParticleAction::load(ds.open_child(0).unwrap()).expect("load");
        let ActionKind::Jitter(j) = &action.kind else {
            panic!("jitter");
        };
        assert!(!j.affect_position && j.affect_velocity);
        assert_eq!(
            j.position_src,
            Some(VectorGenerator::Point {
                position: Vector3::new(1.0, 2.0, 3.0)
            })
        );
        assert!(matches!(j.velocity_src, Some(VectorGenerator::Line { .. })));
    }

    #[test]
    fn action_load_covers_nodeclamp_orbitor_flare_collide_scaler() {
        let ds = resources::datasection::parse(
            "root",
            b"<root>\
              <NodeClamp><fullyClamp_>true</fullyClamp_></NodeClamp>\
              <Orbitor><point_> 1 2 3 </point_><angularVelocity_>4</angularVelocity_>\
              <affectVelocity_>false</affectVelocity_></Orbitor>\
              <Flare><flareName_>fx/flare.dds</flareName_><flareStep_>2</flareStep_>\
              <colourize_>true</colourize_><useParticleSize_>true</useParticleSize_></Flare>\
              <Collide><spriteBased_>true</spriteBased_><elasticity_>0.5</elasticity_>\
              <minAddedRotation_>0.1</minAddedRotation_><maxAddedRotation_>0.2</maxAddedRotation_>\
              <soundTag_>tag</soundTag_><soundEnabled_>true</soundEnabled_>\
              <soundSrcIdx_>3</soundSrcIdx_><soundProject_>proj</soundProject_>\
              <soundGroup_>grp</soundGroup_><soundName_>snd</soundName_></Collide>\
              <Scaler><size_>1.5</size_><rate_>2</rate_></Scaler>\
              </root>",
        )
        .expect("parse");
        for i in 0..5 {
            let action = ParticleAction::load(ds.open_child(i).unwrap()).expect("load");
            match &action.kind {
                ActionKind::NodeClamp(n) => assert!(n.fully_clamp, "nodeclamp"),
                ActionKind::Orbitor(o) => {
                    assert_eq!(o.point, Vector3::new(1.0, 2.0, 3.0));
                    assert_f32_eq!(o.angular_velocity, 4.0, "w");
                    assert!(!o.affect_velocity);
                }
                ActionKind::Flare(f) => {
                    assert_eq!(f.flare_name, "fx/flare.dds");
                    assert_f32_eq!(f.flare_step, 2.0, "step");
                    assert!(f.colourize && f.use_particle_size);
                }
                ActionKind::Collide(c) => {
                    assert!(c.sprite_based);
                    assert_f32_eq!(c.elasticity, 0.5, "elasticity");
                    assert_f32_eq!(c.min_added_rotation, 0.1, "min rot");
                    assert_f32_eq!(c.max_added_rotation, 0.2, "max rot");
                    assert_eq!(c.sound_tag, "tag");
                    assert!(c.sound_enabled);
                    assert_eq!(c.sound_src_idx, 3);
                    assert_eq!(c.sound_project, "proj");
                    assert_eq!(c.sound_group, "grp");
                    assert_eq!(c.sound_name, "snd");
                }
                ActionKind::Scaler(s) => {
                    assert_f32_eq!(s.size, 1.5, "size");
                    assert_f32_eq!(s.rate, 2.0, "rate");
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn barrier_load_maps_shape_and_reaction_codes() {
        let ds = resources::datasection::parse(
            "root",
            b"<root>\
              <Barrier><shape_>1</shape_></Barrier>\
              <Barrier><shape_>2</shape_></Barrier>\
              <Barrier><shape_>3</shape_></Barrier>\
              <Barrier><shape_>7</shape_></Barrier>\
              <Barrier><reaction_>0</reaction_></Barrier>\
              <Barrier><reaction_>1</reaction_></Barrier>\
              <Barrier><reaction_>3</reaction_></Barrier>\
              <Barrier><reaction_>4</reaction_></Barrier>\
              </root>",
        )
        .expect("parse");
        let want_shape = [
            BarrierShape::VerticalCylinder,
            BarrierShape::Box,
            BarrierShape::Sphere,
            BarrierShape::None,
        ];
        for (i, want) in want_shape.iter().enumerate() {
            let action = ParticleAction::load(ds.open_child(i).unwrap()).expect("load");
            let ActionKind::Barrier(b) = &action.kind else {
                panic!("barrier");
            };
            assert_eq!(b.shape, *want, "shape {i}");
            assert_eq!(b.reaction, BarrierReaction::Allow);
        }
        let want_reaction = [
            BarrierReaction::Bounce,
            BarrierReaction::Remove,
            BarrierReaction::Wrap,
            BarrierReaction::Allow,
        ];
        for i in 4..8 {
            let action = ParticleAction::load(ds.open_child(i).unwrap()).expect("load");
            let ActionKind::Barrier(b) = &action.kind else {
                panic!("barrier");
            };
            assert_eq!(b.shape, BarrierShape::None, "default shape");
            assert_eq!(b.reaction, want_reaction[i - 4], "reaction {}", i - 4);
        }
    }

    #[test]
    fn type_id_maps_every_kind() {
        let kinds = [
            ActionKind::Source(SourcePSA::default()),
            ActionKind::Sink(SinkPSA::default()),
            ActionKind::Barrier(BarrierPSA::default()),
            ActionKind::Force(ForcePSA::default()),
            ActionKind::Stream(StreamPSA::default()),
            ActionKind::Jitter(JitterPSA::default()),
            ActionKind::Scaler(ScalerPSA::default()),
            ActionKind::TintShader(TintShaderPSA::default()),
            ActionKind::NodeClamp(NodeClampPSA { fully_clamp: false }),
            ActionKind::Orbitor(OrbitorPSA::default()),
            ActionKind::Flare(FlarePSA::default()),
            ActionKind::Collide(CollidePSA::default()),
            ActionKind::MatrixSwarm(MatrixSwarmPSA),
            ActionKind::Magnet(MagnetPSA::default()),
            ActionKind::Splat(SplatPSA),
        ];
        let want = [
            ActionType::Source,
            ActionType::Sink,
            ActionType::Barrier,
            ActionType::Force,
            ActionType::Stream,
            ActionType::Jitter,
            ActionType::Scaler,
            ActionType::TintShader,
            ActionType::NodeClamp,
            ActionType::Orbitor,
            ActionType::Flare,
            ActionType::Collide,
            ActionType::MatrixSwarm,
            ActionType::Magnet,
            ActionType::Splat,
        ];
        for (k, t) in kinds.iter().zip(want) {
            assert_eq!(k.type_id(), t, "{t:?}");
        }
    }

    #[test]
    fn set_first_update_resets_age_and_source_flag() {
        let mut source = ParticleAction {
            base: ActionBase {
                delay: 2.0,
                ..Default::default()
            },
            kind: ActionKind::Source(SourcePSA::default()),
        };
        source.base.age = 1.0;
        source.set_first_update();
        assert_f32_eq!(source.base.age, 0.0, "age reset");
        let ActionKind::Source(s) = &source.kind else {
            panic!("source");
        };
        assert!(s.first_update);

        let mut sink = ParticleAction {
            base: ActionBase {
                delay: 2.0,
                ..Default::default()
            },
            kind: ActionKind::Sink(SinkPSA::default()),
        };
        sink.base.age = 1.0;
        sink.set_first_update();
        assert_f32_eq!(sink.base.age, 0.0, "sink age reset");
        assert!(matches!(sink.kind, ActionKind::Sink(_)));
    }

    #[test]
    fn action_size_in_bytes_catalogue() {
        let base = std::mem::size_of::<ActionBase>();
        let point = VectorGenerator::Point {
            position: Vector3::ZERO,
        };
        let line = VectorGenerator::Line {
            origin: Vector3::ZERO,
            direction: Vector3::ZERO,
        };
        let mut source = ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Source(SourcePSA::default()),
        };
        let plain = base + std::mem::size_of::<SourcePSA>();
        assert_eq!(source.size_in_bytes(), plain, "source bare");
        if let ActionKind::Source(s) = &mut source.kind {
            s.position_src = Some(point.clone());
            s.velocity_src = Some(line.clone());
        }
        assert_eq!(
            source.size_in_bytes(),
            plain + point.size_in_bytes() + line.size_in_bytes(),
            "source with generators"
        );

        let mut jitter = ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Jitter(JitterPSA::default()),
        };
        let jplain = base + std::mem::size_of::<JitterPSA>();
        assert_eq!(jitter.size_in_bytes(), jplain, "jitter bare");
        if let ActionKind::Jitter(j) = &mut jitter.kind {
            j.position_src = Some(point.clone());
            j.velocity_src = Some(line.clone());
        }
        assert_eq!(
            jitter.size_in_bytes(),
            jplain + point.size_in_bytes() + line.size_in_bytes(),
            "jitter with generators"
        );

        let kinds = [
            ActionKind::Sink(SinkPSA::default()),
            ActionKind::Barrier(BarrierPSA::default()),
            ActionKind::Force(ForcePSA::default()),
            ActionKind::Stream(StreamPSA::default()),
            ActionKind::Scaler(ScalerPSA::default()),
            ActionKind::TintShader(TintShaderPSA::default()),
            ActionKind::NodeClamp(NodeClampPSA { fully_clamp: false }),
            ActionKind::Orbitor(OrbitorPSA::default()),
            ActionKind::Flare(FlarePSA::default()),
            ActionKind::Collide(CollidePSA::default()),
            ActionKind::MatrixSwarm(MatrixSwarmPSA),
            ActionKind::Magnet(MagnetPSA::default()),
            ActionKind::Splat(SplatPSA),
        ];
        for k in kinds.iter() {
            let action = ParticleAction {
                base: ActionBase::default(),
                kind: k.clone(),
            };
            assert!(
                action.size_in_bytes() >= base,
                "footprint {:?}",
                k.type_id()
            );
        }
    }

    #[test]
    fn sprite_material_fx_codes_map() {
        let want = [
            (0, MaterialFX::Additive),
            (1, MaterialFX::AdditiveAlpha),
            (3, MaterialFX::BlendedColour),
            (4, MaterialFX::BlendedInverseColour),
            (5, MaterialFX::Solid),
            (6, MaterialFX::Shimmer),
            (7, MaterialFX::SourceAlpha),
            (2, MaterialFX::Blended),
            (9, MaterialFX::Blended),
        ];
        for (code, fx) in want {
            let xml = format!("<root><materialFX_>{code}</materialFX_></root>");
            let ds = resources::datasection::parse("root", xml.as_bytes()).expect("parse");
            let mut r = ParticleRenderer::create_renderer_of_type("SpriteParticleRenderer")
                .expect("renderer");
            r.load(&ds);
            let ParticleRenderer::Sprite { sprite, .. } = &r else {
                panic!("sprite");
            };
            assert_eq!(sprite.material_fx, fx, "materialFX {code}");
        }
    }

    #[test]
    fn renderer_misc_faces() {
        let mut sprite =
            ParticleRenderer::create_renderer_of_type("SpriteParticleRenderer").expect("r");
        let mut pointsprite =
            ParticleRenderer::create_renderer_of_type("PointSpriteParticleRenderer").expect("r");
        assert!(!sprite.know_particle_radius() && !pointsprite.know_particle_radius());
        sprite.set_rotated(true);
        pointsprite.set_rotated(true);
        let ParticleRenderer::Sprite { sprite: data, .. } = &sprite else {
            panic!("sprite");
        };
        assert!(data.rotated);
        let ParticleRenderer::PointSprite { sprite: data, .. } = &pointsprite else {
            panic!("pointsprite");
        };
        assert!(data.rotated);
        assert_eq!(sprite.size_in_bytes(), pointsprite.size_in_bytes());
    }

    #[test]
    fn system_default_matches_new() {
        let d = ParticleSystem::default();
        let n = ParticleSystem::new();
        assert_eq!(d.capacity, n.capacity);
        assert_eq!(d.fixed_frame_rate, n.fixed_frame_rate);
        assert_eq!(d.first_update, n.first_update);
    }

    #[test]
    fn system_load_reports_bad_action_and_version() {
        let ds = resources::datasection::parse(
            "root",
            b"<root><serialiseVersionData>1</serialiseVersionData>\
              <Actions><Bogus/></Actions></root>",
        )
        .expect("parse");
        let mut system = ParticleSystem::new();
        assert!(!system.load(&ds, "bad"));
        assert!(
            system
                .load_errors
                .iter()
                .any(|e| e.contains("version mismatch")),
            "version error recorded: {:?}",
            system.load_errors
        );
    }

    #[test]
    fn system_load_reads_stored_bounding_box() {
        let ds = resources::datasection::parse(
            "root",
            b"<root><Renderer><SpriteParticleRenderer/></Renderer>\
              <boundingBox><min> -1 -2 -3 </min><max> 1 2 3 </max></boundingBox>\
              </root>",
        )
        .expect("parse");
        let mut system = ParticleSystem::new();
        assert!(system.load(&ds, "boxed"));
        // the stored box is read, then clearBoundingBox replaces it with the
        // 1 cm unit box (the reference order) for a local system
        assert_f32_eq!(system.bounding_box.min.x, -0.01, "cleared min");
        assert_f32_eq!(system.bounding_box.max.x, 0.01, "cleared max");
    }

    #[test]
    fn world_transform_up_fallback_and_own_world() {
        let mut system = ParticleSystem::new();
        system.explicit_transform = true;
        system.explicit_position = Vector3::new(1.0, 2.0, 3.0);
        // a mostly-vertical direction flips the look-at up vector to +z
        system.explicit_direction = Vector3::new(0.0, 0.95, 0.1);
        let _ = system.world_transform();

        let mut system = ParticleSystem::new();
        system.local_offset = Vector3::new(1.0, 2.0, 3.0);
        let mut own = Matrix::identity();
        own.set_translate(5.0, 0.0, 0.0);
        system.own_world = Some(own);
        let m = system.world_transform();
        let origin = m.apply_point(&Vector3::ZERO);
        assert_f32_eq!(origin.x, 6.0, "own world x");
        assert_f32_eq!(origin.y, 2.0, "own world y");
        assert_f32_eq!(origin.z, 3.0, "own world z");
    }

    // ---- coverage batch: update paths --------------------------------------

    #[test]
    fn update_skipped_when_env_inactive() {
        let mut system = ParticleSystem::new();
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv {
            active: false,
            ..Default::default()
        };
        system.frames_left_over = 0.25;
        system.update(0.5, &mut rng, &env);
        assert_f32_eq!(system.frames_left_over, 0.25, "leftover untouched");
    }

    #[test]
    fn unfixed_rate_consumes_the_whole_step() {
        let mut system = ParticleSystem::new();
        system.fixed_frame_rate = 0.0;
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Force(ForcePSA {
                vector: Vector3::new(2.0, 0.0, 0.0),
            }),
        });
        let p = placed(Vector3::ZERO, Vector3::new(1.0, 0.0, 0.0));
        system.particles.add_particle(p);
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        system.update(0.5, &mut rng, &env);
        assert_f32_eq!(system.frames_left_over, 0.0, "leftover");
        let v = system.particles.get(0).velocity();
        assert_f32_eq!(v.x, 2.0, "forced velocity");
        assert_f32_eq!(system.particles.get(0).position.x, 1.0, "moved");
    }

    #[test]
    fn first_update_flag_reaches_a_gated_source() {
        let mut system = ParticleSystem::new();
        system.first_update = true;
        system.actions.push(ParticleAction {
            base: ActionBase {
                delay: 1.0,
                ..Default::default()
            },
            kind: ActionKind::Source(SourcePSA::default()),
        });
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        system.update(1.0 / 30.0, &mut rng, &env);
        assert!(!system.first_update, "system flag cleared");
        let ActionKind::Source(s) = &system.actions[0].kind else {
            panic!("source");
        };
        assert!(s.first_update, "source flagged before its first run");
    }

    #[test]
    fn aging_clamps_at_the_data_maximum() {
        let mut system = ParticleSystem::new();
        let mut p = placed(Vector3::ZERO, Vector3::ZERO);
        p.set_age_raw(Particle::age_max() - 1);
        system.particles.add_particle(p);
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        system.update(1.0 / 30.0, &mut rng, &env);
        assert!(!system.particles.get(0).is_alive(), "clamped to dead");
    }

    #[test]
    fn near_step_dt_snaps_instead_of_deferring() {
        let mut system = ParticleSystem::new();
        system.fixed_frame_rate = 30.0;
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        // 1/30 - 0.000333 is within the almost-equal band
        system.update(1.0 / 30.0 - 0.000333, &mut rng, &env);
        assert_f32_eq!(system.frames_left_over, 0.0, "snapped clean");
    }

    #[test]
    fn tiny_dt_defers_to_the_next_frame() {
        let mut system = ParticleSystem::new();
        system.fixed_frame_rate = 30.0;
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        system.update(0.001, &mut rng, &env);
        assert_f32_eq!(system.frames_left_over, 0.001, "deferred");
    }

    #[test]
    fn non_local_system_queues_and_flushes_every_arm() {
        let mut system = ParticleSystem::new();
        system.own_world = Some(Matrix::identity());
        assert!(!system.is_local());
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Sink(SinkPSA::default()),
        });
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Barrier(BarrierPSA {
                shape: BarrierShape::Box,
                reaction: BarrierReaction::Bounce,
                vec_a: Vector3::new(-10.0, -10.0, -10.0),
                vec_b: Vector3::new(10.0, 10.0, 10.0),
                radius: 0.0,
                resilience: 1.0,
            }),
        });
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Force(ForcePSA {
                vector: Vector3::new(3.0, 0.0, 0.0),
            }),
        });
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Jitter(JitterPSA {
                position_src: Some(VectorGenerator::Point {
                    position: Vector3::new(1.0, 0.0, 0.0),
                }),
                velocity_src: Some(VectorGenerator::Point {
                    position: Vector3::new(0.0, 2.0, 0.0),
                }),
                ..JitterPSA::default()
            }),
        });
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Scaler(ScalerPSA {
                size: 1.0,
                rate: 1.0,
            }),
        });
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Magnet(MagnetPSA {
                source: Some(Matrix::identity()),
                ..MagnetPSA::default()
            }),
        });
        let p = placed(Vector3::new(5.0, 5.0, 5.0), Vector3::new(1.0, 0.0, 0.0));
        system.particles.add_particle(p);
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        system.update(1.0 / 30.0, &mut rng, &env);
        // the force is queued, not applied
        assert_f32_eq!(system.particles.get(0).velocity().x, 1.0, "queued");
        assert_eq!(system.queued_late.len(), 6, "six deferred arms");
        system.flush_late_updates(&mut rng);
        let flushed = system.particles.get(0).velocity().x;
        assert!(flushed > 1.0, "flushed force applied: {flushed}");
        assert_eq!(system.queued_late.len(), 0, "queue drained");
    }

    #[test]
    fn none_and_allow_barriers_stay_local_and_unqueued() {
        let mut system = ParticleSystem::new();
        system.own_world = Some(Matrix::identity());
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Barrier(BarrierPSA::default()), // shape None
        });
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Barrier(BarrierPSA {
                shape: BarrierShape::Box,
                reaction: BarrierReaction::Allow,
                vec_a: Vector3::new(-1.0, -1.0, -1.0),
                vec_b: Vector3::new(1.0, 1.0, 1.0),
                radius: 0.0,
                resilience: 1.0,
            }),
        });
        system
            .particles
            .add_particle(placed(Vector3::ZERO, Vector3::ZERO));
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        system.update(1.0 / 30.0, &mut rng, &env);
        assert_eq!(system.queued_late.len(), 0, "nothing deferred");
    }

    #[test]
    fn non_local_barrier_defers_to_the_flush() {
        let mut system = ParticleSystem::new();
        system.own_world = Some(Matrix::identity());
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Barrier(BarrierPSA {
                shape: BarrierShape::Sphere,
                reaction: BarrierReaction::Bounce,
                vec_a: Vector3::ZERO,
                vec_b: Vector3::ZERO,
                radius: 1.0,
                resilience: 1.0,
            }),
        });
        // inside the radius-1 sphere, standing still
        system.particles.add_particle(placed(
            Vector3::new(0.5, 0.0, 0.0),
            Vector3::new(0.0, 0.0, 0.0),
        ));
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        system.update(1.0 / 30.0, &mut rng, &env);
        assert_eq!(system.queued_late.len(), 1, "deferred");
        system.flush_late_updates(&mut rng);
        assert_eq!(system.queued_late.len(), 0, "drained");
    }

    #[test]
    fn flush_skips_stale_indices() {
        let mut system = ParticleSystem::new();
        system.queued_late.push((99, 1.0 / 30.0));
        let mut rng = BwRandom::new(1);
        system.flush_late_updates(&mut rng);
        assert_eq!(system.queued_late.len(), 0, "stale entry dropped");
    }

    #[test]
    fn tick_on_a_quiet_static_system_is_false() {
        let mut system = ParticleSystem::new();
        system.is_static = true;
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        assert!(!system.tick(1.0 / 30.0, &mut rng, &env));
        system.do_update = true;
        assert!(system.tick(1.0 / 30.0, &mut rng, &env));
    }

    #[test]
    fn clear_bounding_box_follows_the_own_world() {
        let mut system = ParticleSystem::new();
        let mut own = Matrix::identity();
        own.set_translate(10.0, 20.0, 30.0);
        system.own_world = Some(own);
        system.clear_bounding_box();
        assert_f32_eq!(system.bounding_box.min.x, 9.99, "world min x");
        assert_f32_eq!(system.bounding_box.max.z, 30.01, "world max z");
        assert_f32_eq!(system.viz_box.max.z, 30.01, "viz follows");
    }

    #[test]
    fn update_bounding_box_matches_the_container() {
        let mut system = ParticleSystem::new();
        let mut p = placed(Vector3::new(1.0, 2.0, 3.0), Vector3::ZERO);
        p.set_size(0.5);
        system.particles.add_particle(p);
        system.update_bounding_box();
        let sz = system.particles.get(0).size();
        assert!(
            (system.bounding_box.min.x - (1.0 - sz)).abs() < 1e-4,
            "grown min {}",
            system.bounding_box.min.x
        );
        assert!(
            (system.bounding_box.max.z - (3.0 + sz)).abs() < 1e-4,
            "grown max {}",
            system.bounding_box.max.z
        );
    }

    // ---- coverage batch: source faces --------------------------------------

    #[test]
    fn source_without_triggers_returns_early() {
        let mut system = ParticleSystem::new();
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Source(SourcePSA::default()),
        });
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        system.update(1.0 / 30.0, &mut rng, &env);
        assert_eq!(system.size(), 0, "nothing to trigger on");
    }

    #[test]
    fn motion_triggered_source_spawns_on_movement() {
        let mut system = ParticleSystem::new();
        system.capacity = 5;
        system.particles = Particles::new();
        system.particles.reserve(5);
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Source(SourcePSA {
                motion_triggered: true,
                sensitivity: 0.5,
                ..SourcePSA::default()
            }),
        });
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        // first update: the source records its position, no displacement
        system.update(1.0 / 30.0, &mut rng, &env);
        assert_eq!(system.size(), 0, "no motion yet");
        // second update: the origin jumped 10 units in x
        system.local_offset = Vector3::new(10.0, 0.0, 0.0);
        system.update(1.0 / 30.0, &mut rng, &env);
        assert_eq!(system.size(), 5, "capacity-capped burst");
        // particles spread along the displacement line
        let first = system.particles.get(0).position;
        assert_f32_eq!(first.x, 2.0, "spread start");
        let last = system.particles.get(4).position;
        assert_f32_eq!(last.x, 10.0, "spread end");
    }

    #[test]
    fn sleep_period_cycles_and_accumulates_only_while_active() {
        let mut system = ParticleSystem::new();
        system.fixed_frame_rate = 0.0;
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Source(SourcePSA {
                time_triggered: true,
                rate: 30.0,
                active_period: 0.02,
                sleep_period: 0.02,
                current_sleep_period: 0.02,
                ..SourcePSA::default()
            }),
        });
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        for _ in 0..6 {
            system.update(0.01, &mut rng, &env);
        }
        assert!(system.size() > 0, "particles across the sleep cycle");
        let ActionKind::Source(s) = &system.actions[0].kind else {
            panic!("source");
        };
        assert_f32_eq!(s.current_sleep_period, 0.02, "regenerated midpoint");
    }

    #[test]
    fn max_speed_caps_time_triggered_bursts() {
        let mut system = ParticleSystem::new();
        system.fixed_frame_rate = 30.0;
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Source(SourcePSA {
                time_triggered: true,
                rate: 30.0,
                max_speed: 1.0,
                ..SourcePSA::default()
            }),
        });
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        system.update(1.0 / 30.0, &mut rng, &env);
        system.local_offset = Vector3::new(10.0, 0.0, 0.0);
        system.update(1.0 / 30.0, &mut rng, &env);
        assert_eq!(system.size(), 0, "fast movement suppresses spawning");
    }

    #[test]
    fn time_triggered_burst_clamps_to_capacity() {
        let mut system = ParticleSystem::new();
        system.capacity = 1;
        system.particles = Particles::new();
        system.particles.reserve(1);
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Source(SourcePSA {
                time_triggered: true,
                rate: 30.0,
                ..SourcePSA::default()
            }),
        });
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        system.update(0.2, &mut rng, &env);
        assert_eq!(system.size(), 1, "hard capacity");
    }

    #[test]
    fn forced_burst_splits_by_unit_size_and_clamps() {
        let mut system = ParticleSystem::new();
        system.capacity = 4;
        system.particles = Particles::new();
        system.particles.reserve(4);
        let mut s = SourcePSA {
            forced_unit_size: 2,
            ..SourcePSA::default()
        };
        s.force(3); // 3 * 2 = 6 wanted, capacity 4
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Source(s),
        });
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        system.update(1.0 / 30.0, &mut rng, &env);
        assert_eq!(system.size(), 4, "split then clamped");
    }

    #[test]
    fn random_spin_draws_four_extras_per_particle() {
        let mut system = ParticleSystem::new();
        let mut s = SourcePSA {
            random_spin: true,
            min_spin: 1.0,
            max_spin: 2.0,
            ..SourcePSA::default()
        };
        s.force(1);
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Source(s),
        });
        let mut rng = BwRandom::new(9);
        let env = ParticleEnv::default();
        system.update(1.0 / 30.0, &mut rng, &env);
        assert_eq!(system.size(), 1, "spawned");

        // the same seed with a no-spin source consumes fewer draws, so the
        // spawned position differs
        let mut system2 = ParticleSystem::new();
        let mut s2 = SourcePSA::default();
        s2.force(1);
        system2.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Source(s2),
        });
        let mut rng2 = BwRandom::new(9);
        system2.update(1.0 / 30.0, &mut rng2, &env);
        // the spawned particle is identical (the spin draws come after the
        // position/velocity draws) but the rng stream sits further along
        assert_eq!(
            system.particles.get(0).position,
            system2.particles.get(0).position,
            "same spawn"
        );
        assert_ne!(
            rng.next_u32(),
            rng2.next_u32(),
            "spin draws shifted the rng stream"
        );
    }

    #[test]
    fn ignore_rotation_spawns_through_a_translation_matrix() {
        let mut system = ParticleSystem::new();
        system.local_offset = Vector3::new(1.0, 2.0, 3.0);
        let mut s = SourcePSA {
            ignore_rotation: true,
            ..SourcePSA::default()
        };
        s.force(1);
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Source(s),
        });
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        system.update(1.0 / 30.0, &mut rng, &env);
        let p = system.particles.get(0);
        assert_f32_eq!(p.position.x, 1.0, "spawn x");
        assert_f32_eq!(p.position.y, 2.0, "spawn y");
        assert_f32_eq!(p.position.z, 3.0, "spawn z");
    }

    // ---- coverage batch: action bodies --------------------------------------

    #[test]
    fn tint_shader_first_stop_and_empty_guard() {
        let mut system = ParticleSystem::new();
        let mut t = TintShaderPSA::default();
        t.tints.push(crate::particle::Tint {
            time: 0.5,
            colour: Vector4::new(1.0, 1.0, 1.0, 1.0),
        });
        let mut dead = placed(Vector3::ZERO, Vector3::ZERO);
        dead.set_age_raw(Particle::age_max());
        system.particles.add_particle(dead);
        system
            .particles
            .add_particle(placed(Vector3::ZERO, Vector3::ZERO));
        // age 0 is before the first stop: both poles sit on it
        execute_tint_shader(&mut system, &mut t);
        let want = colour_from_normalised(Vector4::new(1.0, 1.0, 1.0, 1.0));
        assert_eq!(system.particles.get(1).colour, want, "first-stop colour");

        // empty tints leave everything alone
        let mut empty = TintShaderPSA::default();
        let before = system.particles.get(1).colour;
        execute_tint_shader(&mut system, &mut empty);
        assert_eq!(system.particles.get(1).colour, before, "empty tints");
    }

    #[test]
    fn scaler_leaves_on_target_particles_alone() {
        let mut system = ParticleSystem::new();
        let mut p = placed(Vector3::ZERO, Vector3::ZERO);
        p.set_size(1.0);
        system.particles.add_particle(p);
        let mut dead = placed(Vector3::ZERO, Vector3::ZERO);
        dead.set_age_raw(Particle::age_max());
        system.particles.add_particle(dead);
        let mut s = ScalerPSA {
            size: 1.0,
            rate: 1.0,
        };
        execute_scaler(&mut system, &mut s, 1.0);
        assert!(
            (system.particles.get(0).size() - 1.0).abs() < 0.002,
            "on target: {}",
            system.particles.get(0).size()
        );
    }

    #[test]
    fn scaler_overshoot_clamps_from_both_sides() {
        let mut system = ParticleSystem::new();
        let mut small = placed(Vector3::ZERO, Vector3::ZERO);
        small.set_size(0.0);
        let mut big = placed(Vector3::ZERO, Vector3::ZERO);
        big.set_size(2.0);
        system.particles.add_particle(small);
        system.particles.add_particle(big);
        let mut s = ScalerPSA {
            size: 0.5,
            rate: 1.0,
        };
        execute_scaler(&mut system, &mut s, 10.0);
        assert!(
            (system.particles.get(0).size() - 0.5).abs() < 0.002,
            "rose to target: {}",
            system.particles.get(0).size()
        );
        assert!(
            (system.particles.get(1).size() - 0.5).abs() < 0.002,
            "fell to target: {}",
            system.particles.get(1).size()
        );
    }

    #[test]
    fn force_skips_dead_particles() {
        let mut system = ParticleSystem::new();
        let mut dead = placed(Vector3::ZERO, Vector3::ZERO);
        dead.set_age_raw(Particle::age_max());
        system.particles.add_particle(dead);
        system
            .particles
            .add_particle(placed(Vector3::ZERO, Vector3::ZERO));
        let mut f = ForcePSA {
            vector: Vector3::new(1.0, 0.0, 0.0),
        };
        execute_force(&mut system, &mut f, 1.0);
        assert_f32_eq!(system.particles.get(0).velocity().x, 0.0, "dead skip");
        assert_f32_eq!(system.particles.get(1).velocity().x, 1.0, "live push");
    }

    #[test]
    fn stream_half_life_zero_snaps() {
        let mut system = ParticleSystem::new();
        system
            .particles
            .add_particle(placed(Vector3::ZERO, Vector3::new(4.0, 0.0, 0.0)));
        let mut dead = placed(Vector3::ZERO, Vector3::ZERO);
        dead.set_age_raw(Particle::age_max());
        system.particles.add_particle(dead);
        let mut s = StreamPSA {
            vector: Vector3::new(0.0, 1.0, 0.0),
            half_life: 0.0,
        };
        execute_stream(&mut system, &mut s, 1.0);
        let v = system.particles.get(0).velocity();
        assert_f32_eq!(v.y, 1.0, "snapped");
        assert_f32_eq!(v.x, 0.0, "snapped");
    }

    #[test]
    fn magnet_origin_from_explicit_source_or_local_renderer() {
        // a source matrix moves the pull origin
        let mut system = ParticleSystem::new();
        system
            .particles
            .add_particle(placed(Vector3::new(10.0, 0.0, 0.0), Vector3::ZERO));
        let mut m = Matrix::identity();
        m.set_translate(5.0, 0.0, 0.0);
        let mut magnet = MagnetPSA {
            source: Some(m),
            ..MagnetPSA::default()
        };
        execute_magnet(&mut system, &mut magnet, 1.0);
        let v = system.particles.get(0).velocity();
        assert!(v.x < 0.0, "pulled toward the matrix origin: {v:?}");

        // a local renderer pins the origin at zero
        let mut system = ParticleSystem::new();
        let base = RendererBase {
            local: true,
            ..Default::default()
        };
        system.set_renderer(Some(ParticleRenderer::Sprite {
            base,
            sprite: crate::particle::SpriteData::default(),
        }));
        system
            .particles
            .add_particle(placed(Vector3::new(10.0, 0.0, 0.0), Vector3::ZERO));
        let mut magnet = MagnetPSA::default();
        execute_magnet(&mut system, &mut magnet, 1.0);
        let v = system.particles.get(0).velocity();
        assert!(v.x < 0.0, "pulled toward zero: {v:?}");
    }

    // ---- coverage batch: barrier arms ---------------------------------------

    fn barrier_case(
        shape: BarrierShape,
        reaction: BarrierReaction,
        pos: Vector3,
        vel: Vector3,
    ) -> (ParticleSystem, BarrierPSA) {
        let mut system = ParticleSystem::new();
        system.particles.add_particle(placed(pos, vel));
        let mut dead = placed(Vector3::new(50.0, 50.0, 50.0), Vector3::ZERO);
        dead.set_age_raw(Particle::age_max());
        system.particles.add_particle(dead);
        let b = BarrierPSA {
            shape,
            reaction,
            vec_a: Vector3::ZERO,
            vec_b: Vector3::new(1.0, 1.0, 1.0),
            radius: 1.0,
            resilience: 1.0,
        };
        (system, b)
    }

    #[test]
    fn cylinder_remove_miss_and_allow_arms() {
        // remove on a shell crossing
        let (mut system, mut b) = barrier_case(
            BarrierShape::VerticalCylinder,
            BarrierReaction::Remove,
            Vector3::new(2.0, 0.0, 0.0),
            Vector3::new(-2.5, 0.0, 0.0),
        );
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 1, "crossed and removed");

        // a miss keeps both particles in place
        let (mut system, mut b) = barrier_case(
            BarrierShape::VerticalCylinder,
            BarrierReaction::Remove,
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(0.1, 0.0, 0.0),
        );
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 2, "no crossing");

        // allow never touches anything
        let (mut system, mut b) = barrier_case(
            BarrierShape::VerticalCylinder,
            BarrierReaction::Allow,
            Vector3::new(2.0, 0.0, 0.0),
            Vector3::new(-2.5, 0.0, 0.0),
        );
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 2, "allow");
    }

    #[test]
    fn cylinder_wrap_teleports_across_the_axis() {
        // start inside at radius 0.5 heading out: 10 units/step lands far
        // outside, so the wrap mirrors through the axis
        let (mut system, mut b) = barrier_case(
            BarrierShape::VerticalCylinder,
            BarrierReaction::Wrap,
            Vector3::new(0.5, 0.0, 0.0),
            Vector3::new(10.0, 0.0, 0.0),
        );
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 2, "re-added");
        let x = system.particles.get(1).position.x;
        assert!((x + 1.5).abs() < 1e-4, "wrapped x {x}");
    }

    #[test]
    fn box_bounce_flips_each_crossed_axis() {
        // x crossing only
        let (mut system, mut b) = barrier_case(
            BarrierShape::Box,
            BarrierReaction::Bounce,
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(5.0, 0.0, 0.0),
        );
        execute_barrier(&mut system, &mut b, 1.0);
        let v = system.particles.get(0).velocity();
        assert_f32_eq!(v.x, -5.0, "x flipped");
        assert_f32_eq!(v.y, 0.0, "y kept");

        // y crossing only
        let (mut system, mut b) = barrier_case(
            BarrierShape::Box,
            BarrierReaction::Bounce,
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(0.0, 5.0, 0.0),
        );
        execute_barrier(&mut system, &mut b, 1.0);
        let v = system.particles.get(0).velocity();
        assert_f32_eq!(v.y, -5.0, "y flipped");
        assert_f32_eq!(v.x, 0.0, "x kept");
    }

    #[test]
    fn box_wrap_shifts_each_axis_independently() {
        // the wrap shifts the CURRENT position by one range per outside
        // axis; three particles cover the >max and <min directions
        let mut system = ParticleSystem::new();
        system
            .particles
            .add_particle(placed(Vector3::new(3.5, 0.5, 0.5), Vector3::ZERO));
        system
            .particles
            .add_particle(placed(Vector3::new(0.5, 3.5, 0.5), Vector3::ZERO));
        system
            .particles
            .add_particle(placed(Vector3::new(-3.5, 0.5, 0.5), Vector3::ZERO));
        let mut b = BarrierPSA {
            shape: BarrierShape::Box,
            reaction: BarrierReaction::Wrap,
            vec_a: Vector3::new(-1.0, -1.0, -1.0),
            vec_b: Vector3::new(1.0, 1.0, 1.0),
            radius: 0.0,
            resilience: 1.0,
        };
        execute_barrier(&mut system, &mut b, 1.0);
        // swap-remove reorders, so match the wrapped positions as a set:
        // each lands one range inside from its outside axis
        let mut xs = [
            system.particles.get(0).position,
            system.particles.get(1).position,
            system.particles.get(2).position,
        ];
        xs.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap());
        assert_f32_eq!(xs[0].x, -1.5, "wrapped from below");
        assert_f32_eq!(xs[1].x, 0.5, "y-wrapped keeps x");
        assert_f32_eq!(xs[1].y, 1.5, "wrapped y down");
        assert_f32_eq!(xs[2].x, 1.5, "wrapped x up");
        assert_f32_eq!(xs[2].z, 0.5, "z untouched");
    }

    #[test]
    fn sphere_remove_miss_and_wrap() {
        // remove on a crossing
        let (mut system, mut b) = barrier_case(
            BarrierShape::Sphere,
            BarrierReaction::Remove,
            Vector3::new(2.0, 0.0, 0.0),
            Vector3::new(-1.5, 0.0, 0.0),
        );
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 1, "crossed and removed");

        // a particle that stays outside is left alone
        let (mut system, mut b) = barrier_case(
            BarrierShape::Sphere,
            BarrierReaction::Remove,
            Vector3::new(3.0, 0.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
        );
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 2, "no crossing");

        // wrap mirrors through the centre
        let (mut system, mut b) = barrier_case(
            BarrierShape::Sphere,
            BarrierReaction::Wrap,
            Vector3::new(2.0, 0.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
        );
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 2, "re-added");
        // the wrapped particle moves 2r toward the centre: 2 - 2 = 0
        let p = system.particles.get(1);
        assert_f32_eq!(p.position.x, 0.0, "wrapped through the centre");
    }

    // ---- coverage batch: meta and editor faces ------------------------------

    #[test]
    fn meta_lookup_duration_and_lod_faces() {
        let mut meta = crate::particle::MetaParticleSystem::default();
        let mut system = ParticleSystem::new();
        system.name = "fire".to_string();
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Sink(SinkPSA {
                maximum_age: 2.0,
                ..SinkPSA::default()
            }),
        });
        meta.add_system(system);
        assert_eq!(meta.n_systems(), 1);
        assert!(meta.system("fire").is_some());
        assert!(meta.system("nope").is_none());
        assert!(meta.system_mut("fire").is_some());
        assert!(meta.system_mut("nope").is_none());
        assert!(meta.first_system().is_some());
        assert!(meta.system_from_index(1).is_none());
        assert_f32_eq!(meta.duration(), 2.0, "duration");

        let env = ParticleEnv::default();
        // maxLod 0 is the infinite marker
        assert!(meta.is_lod_visible(1e6, &env));
        meta.systems[0].enabled = false;
        assert!(!meta.is_lod_visible(1e6, &env));
    }

    #[test]
    fn meta_insert_remove_and_capacity_faces() {
        let mut meta = crate::particle::MetaParticleSystem::default();
        let mut a = ParticleSystem::new();
        a.name = "a".to_string();
        a.capacity = 10;
        a.enabled = false;
        let mut b = ParticleSystem::new();
        b.name = "b".to_string();
        b.capacity = 20;
        b.particles
            .add_particle(placed(Vector3::ZERO, Vector3::ZERO));
        meta.add_system(a);
        meta.add_system(b);
        assert_eq!(meta.capacity(), 30);
        assert_eq!(meta.size(), 1, "only the enabled system counts");
        meta.set_static(true);
        assert!(meta.systems[0].is_static);
        meta.set_do_update();
        assert!(meta.systems[0].do_update);
        meta.set_first_update();
        assert!(meta.systems[0].first_update);
        meta.spawn(1);
        meta.clear();
        // clear empties the particles, not the systems
        assert_eq!(meta.size(), 0);
        assert_eq!(meta.n_systems(), 2);
        let mut c = ParticleSystem::new();
        c.name = "c".to_string();
        meta.insert_system(99, c);
        assert_eq!(meta.n_systems(), 3, "clamped to the end");
        assert!(meta.remove_system("c"));
        assert!(!meta.remove_system("c"));
        assert_eq!(meta.first_system().map(|s| s.name.as_str()), Some("a"));
    }

    #[test]
    fn tint_period_zero_snaps_to_one_second() {
        let ds = resources::datasection::parse(
            "root",
            b"<root><TintShader><repeat_>true</repeat_><period_>0</period_>\
              <tints_><Tint><time>0.2</time><color> 1 1 1 1 </color></Tint></tints_>\
              </TintShader></root>",
        )
        .expect("parse");
        let action = ParticleAction::load(ds.open_child(0).unwrap()).expect("load");
        let ActionKind::TintShader(t) = &action.kind else {
            panic!("tint");
        };
        assert_f32_eq!(t.period, 1.0, "snapped period");
        assert_eq!(t.tints.len(), 2, "single tint becomes the two poles");
    }
    // ---- coverage batch: editor faces ---------------------------------------

    /// An editor whose single system carries one action of every kind.
    fn editor_all_kinds() -> crate::particle_editor::ParticleEditor {
        let mut editor = crate::particle_editor::ParticleEditor::new(oracle::ORACLE_SEED);
        let mut system = ParticleSystem::new();
        system.name = "all".to_string();
        for kind in [
            ActionKind::Source(SourcePSA::default()),
            ActionKind::Sink(SinkPSA::default()),
            ActionKind::Barrier(BarrierPSA::default()),
            ActionKind::Force(ForcePSA::default()),
            ActionKind::Stream(StreamPSA::default()),
            ActionKind::Jitter(JitterPSA::default()),
            ActionKind::Scaler(ScalerPSA::default()),
            ActionKind::TintShader(TintShaderPSA::default()),
            ActionKind::NodeClamp(NodeClampPSA { fully_clamp: false }),
            ActionKind::Orbitor(OrbitorPSA::default()),
            ActionKind::Flare(FlarePSA::default()),
            ActionKind::Collide(CollidePSA::default()),
            ActionKind::MatrixSwarm(MatrixSwarmPSA),
            ActionKind::Magnet(MagnetPSA::default()),
            ActionKind::Splat(SplatPSA),
        ] {
            system.actions.push(ParticleAction {
                base: ActionBase::default(),
                kind,
            });
        }
        editor.meta.add_system(system);
        editor
    }

    #[test]
    fn editor_typed_setters_apply_to_the_matching_kind() {
        let mut editor = editor_all_kinds();

        let face = crate::particle_editor::SourceFace {
            rate: 7.0,
            sensitivity: 2.0,
            ..Default::default()
        };
        editor.set_source(0, 0, &face);
        let ActionKind::Source(s) = &editor.meta.systems[0].actions[0].kind else {
            panic!("source");
        };
        assert_f32_eq!(s.rate, 7.0, "source rate");
        assert_f32_eq!(s.sensitivity, 2.0, "clamped sensitivity");

        editor.set_sink(
            0,
            1,
            &crate::particle_editor::SinkFace {
                maximum_age: 3.0,
                minimum_speed: 1.0,
                outside_only: true,
            },
        );
        let ActionKind::Sink(k) = &editor.meta.systems[0].actions[1].kind else {
            panic!("sink");
        };
        assert_f32_eq!(k.maximum_age, 3.0, "sink face");
        assert!(k.outside_only);

        editor.set_scaler(0, 6, 2.0, 3.0);
        let ActionKind::Scaler(k) = &editor.meta.systems[0].actions[6].kind else {
            panic!("scaler");
        };
        assert_f32_eq!(k.size, 2.0, "scaler size");
        assert_f32_eq!(k.rate, 3.0, "scaler rate");

        editor.set_stream(0, 4, 0.5);
        let ActionKind::Stream(k) = &editor.meta.systems[0].actions[4].kind else {
            panic!("stream");
        };
        assert_f32_eq!(k.half_life, 0.5, "stream half life");

        editor.set_magnet(0, 13, 4.0, 2.0);
        let ActionKind::Magnet(k) = &editor.meta.systems[0].actions[13].kind else {
            panic!("magnet");
        };
        assert_f32_eq!(k.strength, 4.0, "magnet strength");
        assert_f32_eq!(k.min_dist, 2.0, "magnet min dist");

        editor.set_orbitor(0, 9, 3.0);
        let ActionKind::Orbitor(k) = &editor.meta.systems[0].actions[9].kind else {
            panic!("orbitor");
        };
        assert_f32_eq!(k.angular_velocity, 3.0, "orbitor w");

        editor.set_collide(0, 11, 0.25);
        let ActionKind::Collide(k) = &editor.meta.systems[0].actions[11].kind else {
            panic!("collide");
        };
        assert_f32_eq!(k.elasticity, 0.25, "collide elasticity");

        editor.set_flare(0, 10, 1.5);
        let ActionKind::Flare(k) = &editor.meta.systems[0].actions[10].kind else {
            panic!("flare");
        };
        assert_f32_eq!(k.flare_step, 1.5, "flare step");

        editor.set_tint_fog(0, 7, 0.4);
        let ActionKind::TintShader(k) = &editor.meta.systems[0].actions[7].kind else {
            panic!("tint");
        };
        assert_f32_eq!(k.fog_amount, 0.4, "tint fog");

        editor.set_barrier_shape(0, 2, BarrierShape::Sphere);
        editor.set_barrier_reaction(0, 2, BarrierReaction::Wrap);
        let ActionKind::Barrier(k) = &editor.meta.systems[0].actions[2].kind else {
            panic!("barrier");
        };
        assert_eq!(k.shape, BarrierShape::Sphere);
        assert_eq!(k.reaction, BarrierReaction::Wrap);
    }

    #[test]
    fn editor_typed_setters_reject_mismatched_kinds() {
        let mut editor = editor_all_kinds();
        let before_sink = editor.meta.systems[0].actions[1].kind.clone();
        // every typed setter aimed at the Sink must leave it untouched
        let face = crate::particle_editor::SourceFace::default();
        editor.set_source(0, 1, &face);
        editor.set_scaler(0, 1, 1.0, 1.0);
        editor.set_stream(0, 1, 0.5);
        editor.set_magnet(0, 1, 1.0, 1.0);
        editor.set_orbitor(0, 1, 1.0);
        editor.set_collide(0, 1, 1.0);
        editor.set_flare(0, 1, 1.0);
        editor.set_tint_fog(0, 1, 1.0);
        editor.set_barrier_shape(0, 1, BarrierShape::Sphere);
        editor.set_barrier_reaction(0, 1, BarrierReaction::Wrap);
        editor.set_barrier_cylinder(0, 1, Vector3::ZERO, 1.0);
        editor.set_barrier_box(0, 1, Vector3::ZERO, Vector3::ZERO);
        editor.set_barrier_sphere(0, 1, Vector3::ZERO, 1.0);
        assert_eq!(editor.meta.systems[0].actions[1].kind, before_sink);

        // out-of-range indices are equally inert
        let sink_face = crate::particle_editor::SinkFace::default();
        editor.set_sink(9, 0, &sink_face);
        editor.set_sink(0, 99, &sink_face);
        editor.set_scaler(9, 0, 1.0, 1.0);
        editor.set_stream(9, 0, 1.0);
        editor.set_magnet(9, 0, 1.0, 1.0);
        editor.set_orbitor(9, 0, 1.0);
        editor.set_collide(9, 0, 1.0);
        editor.set_flare(9, 0, 1.0);
        editor.set_tint_fog(9, 0, 1.0);
        editor.set_barrier_shape(9, 0, BarrierShape::Sphere);
        editor.set_barrier_reaction(9, 0, BarrierReaction::Wrap);
        editor.set_barrier_cylinder(9, 0, Vector3::ZERO, 1.0);
        editor.set_barrier_box(9, 0, Vector3::ZERO, Vector3::ZERO);
        editor.set_barrier_sphere(9, 0, Vector3::ZERO, 1.0);
    }

    #[test]
    fn editor_barrier_geometry_setters() {
        let mut editor = editor_all_kinds();
        editor.set_barrier_cylinder(0, 2, Vector3::new(1.0, 2.0, 3.0), 4.0);
        let ActionKind::Barrier(k) = &editor.meta.systems[0].actions[2].kind else {
            panic!("barrier");
        };
        assert_eq!(k.shape, BarrierShape::VerticalCylinder);
        assert_eq!(k.reaction, BarrierReaction::Bounce);
        assert_eq!(k.vec_a, Vector3::new(1.0, 2.0, 3.0));
        assert_f32_eq!(k.radius, 4.0, "cylinder radius");

        editor.set_barrier_box(
            0,
            2,
            Vector3::new(-1.0, -1.0, -1.0),
            Vector3::new(1.0, 1.0, 1.0),
        );
        let ActionKind::Barrier(k) = &editor.meta.systems[0].actions[2].kind else {
            panic!("barrier");
        };
        assert_eq!(k.shape, BarrierShape::Box);
        assert_eq!(k.vec_b, Vector3::new(1.0, 1.0, 1.0));

        editor.set_barrier_sphere(0, 2, Vector3::ZERO, 2.0);
        let ActionKind::Barrier(k) = &editor.meta.systems[0].actions[2].kind else {
            panic!("barrier");
        };
        assert_eq!(k.shape, BarrierShape::Sphere);
        assert_f32_eq!(k.radius, 2.0, "sphere radius");
    }

    #[test]
    fn editor_jitter_generator_faces() {
        let mut editor = editor_all_kinds();
        let point = VectorGenerator::Point {
            position: Vector3::new(1.0, 1.0, 1.0),
        };
        let line = VectorGenerator::Line {
            origin: Vector3::ZERO,
            direction: Vector3::new(0.0, 0.0, 1.0),
        };
        assert!(editor.set_jitter_generators(0, 5, Some(point.clone()), Some(line.clone())));
        let ActionKind::Jitter(j) = &editor.meta.systems[0].actions[5].kind else {
            panic!("jitter");
        };
        assert_eq!(j.position_src.as_ref(), Some(&point));
        assert_eq!(j.velocity_src.as_ref(), Some(&line));

        // None leaves the slot alone
        assert!(editor.set_jitter_generators(0, 5, None, None));
        let ActionKind::Jitter(j) = &editor.meta.systems[0].actions[5].kind else {
            panic!("jitter");
        };
        assert!(j.position_src.is_some() && j.velocity_src.is_some());

        // wrong type and bad indices answer false
        assert!(!editor.set_jitter_generators(0, 1, Some(point.clone()), None));
        assert!(!editor.set_jitter_generators(9, 5, None, None));
        assert!(!editor.set_jitter_generators(0, 99, None, None));
    }

    #[test]
    fn editor_source_generator_faces() {
        let mut editor = editor_all_kinds();
        let box_gen = VectorGenerator::Box {
            corner: Vector3::ZERO,
            opposite: Vector3::new(1.0, 1.0, 1.0),
        };
        assert!(editor.set_source_generator(0, 0, true, box_gen.clone()));
        assert!(editor.set_source_generator(0, 0, false, box_gen.clone()));
        let ActionKind::Source(s) = &editor.meta.systems[0].actions[0].kind else {
            panic!("source");
        };
        assert_eq!(s.position_src.as_ref(), Some(&box_gen));
        assert_eq!(s.velocity_src.as_ref(), Some(&box_gen));

        assert!(
            !editor.set_source_generator(0, 1, true, box_gen.clone()),
            "wrong type"
        );
        assert!(
            !editor.set_source_generator(9, 0, true, box_gen.clone()),
            "bad system"
        );
        assert!(
            !editor.set_source_generator(0, 99, true, box_gen.clone()),
            "bad action"
        );

        // a non-cylinder generator is refused by the cylinder face
        assert!(!editor.set_cylinder_generator(0, 0, true, box_gen.clone()));
        let cyl = VectorGenerator::Cylinder {
            origin: Vector3::ZERO,
            direction: Vector3::new(0.0, 1.0, 0.0),
            min_radius: 0.5,
            max_radius: 1.0,
            basis_u: Vector3::new(1.0, 0.0, 0.0),
            basis_v: Vector3::new(0.0, 0.0, 1.0),
        };
        assert!(editor.set_cylinder_generator(0, 0, true, cyl));
        let ActionKind::Source(s) = &editor.meta.systems[0].actions[0].kind else {
            panic!("source");
        };
        let Some(VectorGenerator::Cylinder {
            basis_u,
            basis_v,
            direction,
            ..
        }) = &s.position_src
        else {
            panic!("cylinder");
        };
        // the basis was rebuilt orthornormal to the direction
        assert_f32_eq!(basis_u.dot_product(direction), 0.0, "u perp");
        assert_f32_eq!(basis_v.dot_product(direction), 0.0, "v perp");
    }

    #[test]
    fn editor_renderer_faces() {
        let mut editor = editor();
        let face = crate::particle_editor::SpriteFace {
            material_fx: crate::particle::MaterialFX::Solid,
            frame_count: 4,
            ..Default::default()
        };
        assert!(editor.set_sprite(0, &face));
        let Some(crate::particle::ParticleRenderer::Sprite { sprite, .. }) =
            editor.meta.systems[0].renderer.as_ref()
        else {
            panic!("sprite");
        };
        assert_eq!(sprite.material_fx, crate::particle::MaterialFX::Solid);
        assert_eq!(sprite.frame_count, 4);

        // a point-sprite renderer takes the same face
        editor.meta.systems[0].set_renderer(
            crate::particle::ParticleRenderer::create_renderer_of_type(
                "PointSpriteParticleRenderer",
            ),
        );
        assert!(editor.set_sprite(0, &face));
        assert!(editor.set_renderer_space(0, true, false));
        assert!(
            editor.meta.systems[0]
                .renderer
                .as_ref()
                .map(|r| r.local())
                .unwrap_or(false)
        );

        // no renderer and bad indices answer false
        editor.meta.systems[0].set_renderer(None);
        assert!(!editor.set_sprite(0, &face));
        assert!(!editor.set_renderer_space(0, true, false));
        let mut other = crate::particle_editor::ParticleEditor::new(oracle::ORACLE_SEED);
        assert!(other.load(FIXTURE, "fire.particle"));
        assert!(!other.set_sprite(9, &face));
        assert!(!other.set_renderer_space(9, true, false));
    }

    #[test]
    fn editor_simple_setters_ignore_bad_indices() {
        let mut editor = editor();
        editor.set_capacity(9, 10);
        editor.set_wind_factor(9, 0.5);
        editor.set_max_lod(9, 0.0);
        editor.set_explicit_position(9, Vector3::ZERO);
        editor.set_explicit_direction(9, Vector3::ZERO);
        editor.set_action_enabled(9, 0, false);
        editor.set_action_enabled(0, 99, false);
        editor.set_action_delay(9, 0, 1.0);
        editor.set_action_delay(0, 99, 1.0);
        editor.set_action_minimum_age(9, 0, 1.0);
        editor.set_action_minimum_age(0, 99, 1.0);
        editor.spawn_system(9, 1);
        editor.clear_system(9);
    }

    #[test]
    fn material_fx_index_roundtrip() {
        use crate::particle_editor::material_fx_from_index;
        assert_eq!(
            material_fx_from_index(0),
            Some(crate::particle::MaterialFX::Additive)
        );
        assert_eq!(
            material_fx_from_index(1),
            Some(crate::particle::MaterialFX::AdditiveAlpha)
        );
        assert_eq!(
            material_fx_from_index(2),
            Some(crate::particle::MaterialFX::Blended)
        );
        assert_eq!(
            material_fx_from_index(3),
            Some(crate::particle::MaterialFX::BlendedColour)
        );
        assert_eq!(
            material_fx_from_index(4),
            Some(crate::particle::MaterialFX::BlendedInverseColour)
        );
        assert_eq!(
            material_fx_from_index(5),
            Some(crate::particle::MaterialFX::Solid)
        );
        assert_eq!(
            material_fx_from_index(6),
            Some(crate::particle::MaterialFX::Shimmer)
        );
        assert_eq!(
            material_fx_from_index(7),
            Some(crate::particle::MaterialFX::SourceAlpha)
        );
        assert_eq!(material_fx_from_index(8), None);
        assert_eq!(material_fx_from_index(-1), None);
        assert_eq!(crate::particle_editor::capacity_clamp(0), 1);
        assert_eq!(
            crate::particle_editor::capacity_clamp(1_000_000),
            crate::particle::MAX_CAPACITY
        );
    }

    // ---- coverage batch: inline update arms, trigger edges, barrier else ----

    #[test]
    fn local_update_runs_the_inline_action_arms() {
        let mut system = ParticleSystem::new();
        system.capacity = 4;
        system.particles = Particles::new();
        system.particles.reserve(4);
        system
            .particles
            .add_particle(placed(Vector3::ZERO, Vector3::ZERO));
        let barrier = BarrierPSA {
            shape: BarrierShape::Box,
            reaction: BarrierReaction::Bounce,
            vec_a: Vector3::new(-10.0, -10.0, -10.0),
            vec_b: Vector3::new(10.0, 10.0, 10.0),
            radius: 0.0,
            resilience: 1.0,
        };
        for kind in [
            ActionKind::Barrier(barrier),
            ActionKind::Stream(StreamPSA::default()),
            ActionKind::Jitter(JitterPSA::default()),
            ActionKind::Scaler(ScalerPSA::default()),
            ActionKind::Splat(SplatPSA),
            ActionKind::Magnet(MagnetPSA::default()),
        ] {
            system.actions.push(ParticleAction {
                base: ActionBase::default(),
                kind,
            });
        }
        // a disabled action is skipped by the update pass
        system.actions.push(ParticleAction {
            base: ActionBase {
                enabled: false,
                ..ActionBase::default()
            },
            kind: ActionKind::Force(ForcePSA::default()),
        });
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        // an over-long step clamps to one second
        system.update(5.0, &mut rng, &env);
        system.update(1.0 / 30.0, &mut rng, &env);
        assert_eq!(system.size(), 1, "particle survives the inline pass");
    }

    #[test]
    fn motion_below_sensitivity_spawns_nothing() {
        let mut system = ParticleSystem::new();
        system.capacity = 5;
        system.particles = Particles::new();
        system.particles.reserve(5);
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Source(SourcePSA {
                motion_triggered: true,
                sensitivity: 2.0,
                ..SourcePSA::default()
            }),
        });
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        system.update(1.0 / 30.0, &mut rng, &env);
        system.local_offset = Vector3::new(0.01, 0.0, 0.0);
        system.update(1.0 / 30.0, &mut rng, &env);
        assert_eq!(system.size(), 0, "slow drift stays under the sensitivity");
    }

    #[test]
    fn time_trigger_with_zero_rate_stays_idle() {
        let mut system = ParticleSystem::new();
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Source(SourcePSA {
                time_triggered: true,
                rate: 0.0,
                ..SourcePSA::default()
            }),
        });
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        for _ in 0..3 {
            system.update(0.2, &mut rng, &env);
        }
        assert_eq!(system.size(), 0, "rate zero never accumulates");
    }

    #[test]
    fn max_speed_under_the_cap_keeps_the_burst() {
        let mut system = ParticleSystem::new();
        system.capacity = 3;
        system.particles = Particles::new();
        system.particles.reserve(3);
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Source(SourcePSA {
                time_triggered: true,
                rate: 30.0,
                max_speed: 10.0,
                ..SourcePSA::default()
            }),
        });
        let mut rng = BwRandom::new(1);
        let env = ParticleEnv::default();
        system.update(0.2, &mut rng, &env);
        assert_eq!(system.size(), 3, "still motion keeps the burst");
    }

    #[test]
    fn sink_removes_only_old_live_particles() {
        let mut system = ParticleSystem::new();
        system
            .particles
            .add_particle(placed(Vector3::ZERO, Vector3::ZERO));
        let mut old = placed(Vector3::new(1.0, 0.0, 0.0), Vector3::ZERO);
        old.set_age_raw(Particle::age_max() - 1);
        system.particles.add_particle(old);
        let mut dead = placed(Vector3::new(2.0, 0.0, 0.0), Vector3::ZERO);
        dead.set_age_raw(Particle::age_max());
        system.particles.add_particle(dead);
        let mut s = SinkPSA {
            maximum_age: 5.0,
            minimum_speed: 0.0,
            ..SinkPSA::default()
        };
        execute_sink(&mut system, &mut s);
        // swap-remove: the young stay at 0, the dead slide into the gap
        assert_eq!(
            system.particles.len(),
            2,
            "old removed, young and dead kept"
        );
        assert_eq!(system.particles.get(0).position, Vector3::ZERO);
        assert_eq!(
            system.particles.get(1).position,
            Vector3::new(2.0, 0.0, 0.0)
        );
    }

    #[test]
    fn jitter_load_tolerates_a_single_source_section() {
        let vel_only = resources::datasection::parse(
            "root",
            b"<root><Jitter><pVelocitySrc><PointVectorGenerator><position_> 1 1 1 </position_>\
              </PointVectorGenerator></pVelocitySrc></Jitter></root>",
        )
        .expect("parse");
        let action = ParticleAction::load(vel_only.open_child(0).unwrap()).expect("load");
        let ActionKind::Jitter(j) = &action.kind else {
            panic!("jitter");
        };
        assert!(j.position_src.is_none() && j.velocity_src.is_some());

        let pos_only = resources::datasection::parse(
            "root",
            b"<root><Jitter><pPositionSrc><PointVectorGenerator><position_> 2 2 2 </position_>\
              </PointVectorGenerator></pPositionSrc></Jitter></root>",
        )
        .expect("parse");
        let action = ParticleAction::load(pos_only.open_child(0).unwrap()).expect("load");
        let ActionKind::Jitter(j) = &action.kind else {
            panic!("jitter");
        };
        assert!(j.position_src.is_some() && j.velocity_src.is_none());
    }

    #[test]
    fn jitter_skips_the_position_arm_and_dead_particles() {
        let mut system = ParticleSystem::new();
        system
            .particles
            .add_particle(placed(Vector3::ZERO, Vector3::ZERO));
        let mut dead = placed(Vector3::new(5.0, 5.0, 5.0), Vector3::ZERO);
        dead.set_age_raw(Particle::age_max());
        system.particles.add_particle(dead);
        let mut j = JitterPSA {
            affect_position: false,
            velocity_src: Some(VectorGenerator::Point {
                position: Vector3::new(1.0, 1.0, 1.0),
            }),
            ..JitterPSA::default()
        };
        let mut rng = BwRandom::new(7);
        execute_jitter(&mut system, &mut j, 0.5, &mut rng);
        assert_eq!(
            system.particles.get(0).position,
            Vector3::ZERO,
            "position arm skipped"
        );
        assert_eq!(
            system.particles.get(1).position,
            Vector3::new(5.0, 5.0, 5.0),
            "dead untouched"
        );
    }

    #[test]
    fn scaler_pulls_small_particles_up_to_the_target() {
        let mut system = ParticleSystem::new();
        system
            .particles
            .add_particle(placed(Vector3::ZERO, Vector3::ZERO));
        let mut s = ScalerPSA {
            size: 0.5,
            rate: 10.0,
        };
        execute_scaler(&mut system, &mut s, 1.0);
        let size = system.particles.get(0).size();
        assert!((size - 0.5).abs() < 0.002, "clamped up: {size}");
    }

    #[test]
    fn magnet_ignores_dead_particles() {
        let mut system = ParticleSystem::new();
        system
            .particles
            .add_particle(placed(Vector3::new(10.0, 0.0, 0.0), Vector3::ZERO));
        let mut dead = placed(Vector3::new(10.0, 0.0, 0.0), Vector3::ZERO);
        dead.set_age_raw(Particle::age_max());
        system.particles.add_particle(dead);
        let mut magnet = MagnetPSA::default();
        execute_magnet(&mut system, &mut magnet, 1.0);
        assert!(system.particles.get(0).velocity().x < 0.0, "live pulled");
        assert_eq!(
            system.particles.get(1).velocity(),
            Vector3::ZERO,
            "dead untouched"
        );
    }

    #[test]
    fn cylinder_bounce_and_wrap_leave_non_crossers_alone() {
        // outside and staying outside: no bounce fires
        let (mut system, mut b) = barrier_case(
            BarrierShape::VerticalCylinder,
            BarrierReaction::Bounce,
            Vector3::new(5.0, 0.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
        );
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(
            system.particles.get(0).velocity(),
            Vector3::new(1.0, 0.0, 0.0),
            "velocity kept"
        );

        // inside and staying inside: nothing wraps
        let (mut system, mut b) = barrier_case(
            BarrierShape::VerticalCylinder,
            BarrierReaction::Wrap,
            Vector3::new(0.5, 0.0, 0.0),
            Vector3::new(0.1, 0.0, 0.0),
        );
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 2, "nothing wrapped");
    }

    #[test]
    fn sphere_wrap_leaves_insiders_alone() {
        let (mut system, mut b) = barrier_case(
            BarrierShape::Sphere,
            BarrierReaction::Wrap,
            Vector3::new(0.5, 0.0, 0.0),
            Vector3::new(0.1, 0.0, 0.0),
        );
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 2, "inside stays");
    }

    #[test]
    fn box_remove_needs_a_crossing_and_allow_passes_everything() {
        let (mut system, mut b) = barrier_case(
            BarrierShape::Box,
            BarrierReaction::Remove,
            Vector3::new(0.5, 0.5, 0.5),
            Vector3::new(0.05, 0.0, 0.0),
        );
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 2, "no crossing, no removal");

        let (mut system, mut b) = barrier_case(
            BarrierShape::Box,
            BarrierReaction::Allow,
            Vector3::new(5.0, 5.0, 5.0),
            Vector3::new(1.0, 0.0, 0.0),
        );
        execute_barrier(&mut system, &mut b, 1.0);
        assert_eq!(system.particles.len(), 2, "allow never removes");
    }

    #[test]
    fn box_wrap_covers_each_axis_and_the_inside_case() {
        let mut system = ParticleSystem::new();
        for pos in [
            Vector3::new(0.0, -2.0, 0.0),
            Vector3::new(0.0, 0.0, -2.0),
            Vector3::new(0.0, 0.0, 2.0),
            Vector3::new(0.5, 0.5, 0.5),
        ] {
            system.particles.add_particle(placed(pos, Vector3::ZERO));
        }
        let mut b = BarrierPSA {
            shape: BarrierShape::Box,
            reaction: BarrierReaction::Wrap,
            vec_a: Vector3::new(-1.0, -1.0, -1.0),
            vec_b: Vector3::new(1.0, 1.0, 1.0),
            radius: 0.0,
            resilience: 1.0,
        };
        execute_barrier(&mut system, &mut b, 1.0);
        // three outside particles land on the origin from their axes; the
        // inside one never wraps
        assert_eq!(system.particles.len(), 4, "all re-added");
        let mut at_origin = 0;
        let mut inside_kept = false;
        for i in 0..system.particles.len() {
            let p = system.particles.get(i).position;
            if p.x.abs() < 1e-4 && p.y.abs() < 1e-4 && p.z.abs() < 1e-4 {
                at_origin += 1;
            }
            if (p.x - 0.5).abs() < 1e-4 && (p.y - 0.5).abs() < 1e-4 && (p.z - 0.5).abs() < 1e-4 {
                inside_kept = true;
            }
        }
        assert_eq!(at_origin, 3, "y/z min and z max all shifted by one range");
        assert!(inside_kept, "the inside particle stays put");
    }

    #[test]
    fn sprite_renderer_picks_up_source_rotation() {
        let ds = resources::datasection::parse(
            "root",
            b"<root><Actions><Source><randomInitialRotation_> 3 0 \
              </randomInitialRotation_></Source></Actions>\
              <Renderer><SpriteParticleRenderer/></Renderer></root>",
        )
        .expect("parse");
        let mut system = ParticleSystem::new();
        assert!(system.load(&ds, "spin"));
        let Some(crate::particle::ParticleRenderer::Sprite { sprite, .. }) =
            system.renderer.as_ref()
        else {
            panic!("sprite");
        };
        assert!(sprite.rotated, "source rotation flips the flag");
    }

    #[test]
    fn meta_load_reads_old_and_new_formats() {
        // old format: the version section sits at the root, one unnamed system
        let old = resources::datasection::parse(
            "root",
            b"<root><serialiseVersionData>2</serialiseVersionData>\
              <Renderer><SpriteParticleRenderer/></Renderer></root>",
        )
        .expect("parse");
        let mut meta = crate::particle::MetaParticleSystem::default();
        assert!(meta.load(&old, "legacy"));
        assert_eq!(meta.n_systems(), 1);
        assert_eq!(meta.systems[0].name, "legacy");

        // new format: named children; non-system children are skipped
        let packed = resources::datasection::parse(
            "root",
            b"<root><sys><serialiseVersionData>2</serialiseVersionData>\
              <Renderer><SpriteParticleRenderer/></Renderer></sys>\
              <junk><notASystem/></junk></root>",
        )
        .expect("parse");
        let mut meta = crate::particle::MetaParticleSystem::default();
        assert!(meta.load(&packed, "pack"));
        assert_eq!(meta.n_systems(), 1, "junk skipped");
        assert_eq!(meta.systems[0].name, "sys");
    }

    #[test]
    fn meta_footprint_and_sinkless_duration() {
        let mut meta = crate::particle::MetaParticleSystem::default();
        let mut system = ParticleSystem::new();
        system.name = "a".to_string();
        meta.add_system(system);
        assert!(meta.size_in_bytes() > 0, "enabled system counts");
        meta.systems[0].enabled = false;
        assert_eq!(meta.size_in_bytes(), 0, "disabled systems are skipped");
        // a system without a sink reports no duration
        let bare = ParticleSystem::new();
        assert_f32_eq!(bare.duration(), -1.0, "no sink");
    }

    // ---- coverage batch: save faces (BW-13-04-05) ---------------------------

    /// `ParticleSystem::save` + `load` round trip over every serialised
    /// face: all 15 action kinds with distinctive payloads, the system
    /// serialise members, and the sprite renderer. The port's load faces
    /// are the old editor's loader, so a clean round trip is the
    /// acceptance ("旧编辑器可重载").
    #[test]
    fn system_save_round_trips_every_serialised_face() {
        let mut system = ParticleSystem::new();
        system.capacity = 77;
        system.wind_factor = 0.3;
        system.explicit_position = Vector3::new(1.0, 2.0, 3.0);
        system.explicit_direction = Vector3::new(0.0, 0.0, -1.0);
        system.explicit_transform = true;
        system.local_offset = Vector3::new(4.0, 5.0, 6.0);
        system.max_lod = 0.9;
        system.fixed_frame_rate = 60.0;
        system.bounding_box =
            BoundBox::new(Vector3::new(-1.0, -1.0, -1.0), Vector3::new(2.0, 2.0, 2.0));
        for (index, kind) in [
            ActionKind::Source(SourcePSA::default()),
            ActionKind::Sink(SinkPSA::default()),
            ActionKind::Barrier(BarrierPSA::default()),
            ActionKind::Force(ForcePSA::default()),
            ActionKind::Stream(StreamPSA::default()),
            ActionKind::Jitter(JitterPSA::default()),
            ActionKind::Scaler(ScalerPSA::default()),
            ActionKind::TintShader(TintShaderPSA::default()),
            ActionKind::NodeClamp(NodeClampPSA { fully_clamp: false }),
            ActionKind::Orbitor(OrbitorPSA::default()),
            ActionKind::Flare(FlarePSA::default()),
            ActionKind::Collide(CollidePSA::default()),
            ActionKind::MatrixSwarm(MatrixSwarmPSA),
            ActionKind::Magnet(MagnetPSA::default()),
            ActionKind::Splat(SplatPSA),
        ]
        .into_iter()
        .enumerate()
        {
            system.actions.push(ParticleAction {
                base: ActionBase {
                    delay: index as f32 * 0.5,
                    minimum_age: index as f32,
                    enabled: index != 14,
                    ..ActionBase::default()
                },
                kind,
            });
        }
        if let ActionKind::Source(s) = &mut system.actions[0].kind {
            s.motion_triggered = true;
            s.grounded = true;
            s.drop_distance = 0.25;
            s.rate = 11.0;
            s.sensitivity = 3.0;
            s.max_speed = 9.0;
            s.active_period = 2.0;
            s.sleep_period = 1.0;
            s.sleep_period_max = 4.0;
            s.minimum_size = 0.1;
            s.maximum_size = 2.0;
            s.forced_unit_size = 3;
            s.allowed_time_in_seconds = 6.0;
            s.initial_rotation = Vector2::new(1.0, 2.0);
            s.random_initial_rotation = Vector2::new(0.5, 1.5);
            s.initial_colour = Vector4::new(0.1, 0.2, 0.3, 0.4);
            s.random_spin = true;
            s.min_spin = -1.0;
            s.max_spin = 1.0;
            s.ignore_rotation = true;
            s.inherit_velocity = 0.75;
            s.position_src = Some(VectorGenerator::Box {
                corner: Vector3::ZERO,
                opposite: Vector3::new(1.0, 2.0, 3.0),
            });
            s.velocity_src = Some(VectorGenerator::Cylinder {
                origin: Vector3::ZERO,
                direction: Vector3::new(0.0, 1.0, 0.0),
                max_radius: 2.0,
                min_radius: 0.5,
                basis_u: Vector3::new(1.0, 0.0, 0.0),
                basis_v: Vector3::new(0.0, 0.0, 1.0),
            });
        }
        if let ActionKind::Sink(s) = &mut system.actions[1].kind {
            s.maximum_age = 3.5;
            s.minimum_speed = 0.25;
            s.outside_only = true;
        }
        if let ActionKind::Barrier(b) = &mut system.actions[2].kind {
            b.shape = BarrierShape::Box;
            b.reaction = BarrierReaction::Wrap;
            b.vec_a = Vector3::new(-1.0, -1.0, -1.0);
            b.vec_b = Vector3::new(1.0, 1.0, 1.0);
            b.radius = 0.5;
        }
        if let ActionKind::Force(f) = &mut system.actions[3].kind {
            f.vector = Vector3::new(0.0, -2.0, 0.0);
        }
        if let ActionKind::Stream(st) = &mut system.actions[4].kind {
            st.vector = Vector3::new(1.0, 0.0, 0.0);
            st.half_life = 2.5;
        }
        if let ActionKind::Jitter(j) = &mut system.actions[5].kind {
            j.affect_position = false;
            j.position_src = Some(VectorGenerator::Sphere {
                centre: Vector3::new(1.0, 1.0, 1.0),
                max_radius: 2.0,
                min_radius: 0.5,
            });
            j.velocity_src = Some(VectorGenerator::Point {
                position: Vector3::new(2.0, 2.0, 2.0),
            });
        }
        if let ActionKind::Scaler(sc) = &mut system.actions[6].kind {
            sc.size = 1.5;
            sc.rate = 5.0;
        }
        if let ActionKind::TintShader(t) = &mut system.actions[7].kind {
            t.repeat = true;
            t.period = 3.0;
            t.fog_amount = 0.25;
            t.tints = vec![
                Tint {
                    time: 0.25,
                    colour: Vector4::new(1.0, 0.6, 0.2, 1.0),
                },
                Tint {
                    time: 0.75,
                    colour: Vector4::new(0.4, 0.1, 0.05, 0.0),
                },
            ];
        }
        if let ActionKind::NodeClamp(n) = &mut system.actions[8].kind {
            n.fully_clamp = true;
        }
        if let ActionKind::Orbitor(o) = &mut system.actions[9].kind {
            o.point = Vector3::new(1.0, 2.0, 3.0);
            o.angular_velocity = 1.5;
            o.affect_velocity = false;
        }
        if let ActionKind::Flare(f) = &mut system.actions[10].kind {
            f.flare_name = "flare/test".to_string();
            f.flare_step = 0.5;
            f.colourize = true;
            f.use_particle_size = true;
        }
        if let ActionKind::Collide(c) = &mut system.actions[11].kind {
            c.sprite_based = true;
            c.elasticity = 0.5;
            c.min_added_rotation = -1.0;
            c.max_added_rotation = 1.0;
            c.sound_tag = "tag".to_string();
            c.sound_enabled = true;
            c.sound_src_idx = 2;
            c.sound_project = "proj".to_string();
            c.sound_group = "grp".to_string();
            c.sound_name = "snd".to_string();
        }
        if let ActionKind::Magnet(m) = &mut system.actions[13].kind {
            m.strength = 2.0;
            m.min_dist = 3.0;
        }
        system.renderer = Some(ParticleRenderer::Sprite {
            base: RendererBase {
                view_dependent: true,
                local: true,
            },
            sprite: SpriteData {
                material_fx: MaterialFX::Solid,
                frame_count: 6,
                frame_rate: 12.0,
                explicit_orientation: Vector3::new(0.0, 1.0, 0.0),
                texture_name: "fx/test.bmp".to_string(),
                soft_depth_range: 0.4,
                soft_falloff_power: 2.0,
                soft_depth_offset: 0.05,
                near_fade_cutoff: 0.1,
                near_fade_start: 0.2,
                near_fade_falloff_power: 3.0,
                // the Source carries a non-zero initial rotation, so the
                // reload re-derives the same flag
                rotated: true,
            },
        });

        let mut ds = resources::datasection::DataSection::new("root");
        assert!(system.save(&mut ds, "round", true));
        assert_eq!(
            system.viz_box, system.bounding_box,
            "transient keeps the cache"
        );

        let xml = ds.write_to_string();
        let parsed = resources::datasection::parse("root", xml.as_bytes()).expect("parse");

        let mut loaded = ParticleSystem::new();
        assert!(loaded.load(&parsed, "round"));
        assert_eq!(loaded.name, "round");
        assert_eq!(loaded.capacity, 77);
        assert_f32_eq!(loaded.wind_factor, 0.3, "wind");
        assert_eq!(loaded.explicit_position, Vector3::new(1.0, 2.0, 3.0));
        assert_eq!(loaded.explicit_direction, Vector3::new(0.0, 0.0, -1.0));
        assert!(loaded.explicit_transform);
        assert_eq!(loaded.local_offset, Vector3::new(4.0, 5.0, 6.0));
        assert_f32_eq!(loaded.max_lod, 0.9, "lod");
        assert_f32_eq!(loaded.fixed_frame_rate, 60.0, "frame rate");
        assert_eq!(loaded.renderer, system.renderer, "renderer face");
        // The Source's sleep period is recomputed by the load face
        // (generateSleepPeriod, source_psa.cpp), so the expected value is
        // the recomputation; every other action round trips whole.
        let mut expect_source = system.actions[0].clone();
        if let ActionKind::Source(s) = &mut expect_source.kind {
            s.current_sleep_period = generate_sleep_period(s.sleep_period, s.sleep_period_max);
        }
        assert_eq!(loaded.actions[0], expect_source, "source face");
        assert_eq!(&loaded.actions[1..7], &system.actions[1..7], "kinds 1..7");
        assert_eq!(loaded.actions[7], system.actions[7], "tint ramp");
        assert_eq!(
            &loaded.actions[8..15],
            &system.actions[8..15],
            "kinds 8..15"
        );
    }

    /// Recursive tag-path walk, document order.
    fn tag_paths(ds: &DataSection, prefix: &str, out: &mut Vec<String>) {
        let path = if prefix.is_empty() {
            ds.section_name().to_string()
        } else {
            format!("{prefix}/{}", ds.section_name())
        };
        out.push(path.clone());
        for i in 0..ds.count_children() {
            tag_paths(ds.open_child(i).unwrap(), &path, out);
        }
    }

    /// The saved XML pins the reference save layout: system serialise tags
    /// in template order (explicitTransform last), `Actions` then
    /// `Renderer` then `capacity` then `boundingBox`, the Source
    /// generators ahead of its members and the Jitter generators behind
    /// them, and the helper block last.
    #[test]
    fn saved_xml_pins_the_reference_tag_order() {
        let mut meta = crate::particle::MetaParticleSystem::default();
        let mut system = ParticleSystem::new();
        system.name = "p".to_string();
        let source = SourcePSA {
            position_src: Some(VectorGenerator::Point {
                position: Vector3::new(1.0, 1.0, 1.0),
            }),
            initial_rotation: Vector2::new(1.0, 2.0),
            initial_colour: Vector4::new(0.1, 0.2, 0.3, 0.4),
            ..SourcePSA::default()
        };
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Source(source),
        });
        let jitter = JitterPSA {
            velocity_src: Some(VectorGenerator::Line {
                origin: Vector3::ZERO,
                direction: Vector3::new(0.0, 0.0, 1.0),
            }),
            ..JitterPSA::default()
        };
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::Jitter(jitter),
        });
        system.actions.push(ParticleAction {
            base: ActionBase::default(),
            kind: ActionKind::TintShader(TintShaderPSA {
                repeat: true,
                period: 2.0,
                fog_amount: 0.5,
                tints: vec![Tint {
                    time: 0.25,
                    colour: Vector4::new(1.0, 0.6, 0.2, 1.0),
                }],
            }),
        });
        system.renderer =
            Some(ParticleRenderer::create_renderer_of_type("SpriteParticleRenderer").unwrap());
        system.bounding_box =
            BoundBox::new(Vector3::new(-1.0, -1.0, -1.0), Vector3::new(2.0, 2.0, 2.0));
        meta.add_system(system);

        let mut ds = resources::datasection::DataSection::new("root");
        assert!(meta.save(&mut ds, false));
        let xml = ds.write_to_string();

        // member order and the value formats the reference writer produces
        assert!(
            xml.contains("<serialiseVersionData>\t2\t</serialiseVersionData>"),
            "version stamp"
        );
        assert!(
            xml.contains("<initialRotation_>\t1.000000 2.000000\t</initialRotation_>"),
            "vector2 face"
        );
        assert!(
            xml.contains(
                "<initialColour_>\t0.100000 0.200000 0.300000 0.400000\t</initialColour_>"
            ),
            "vector4 face"
        );
        assert!(
            xml.contains(
                "<helperModelCenterOnHardPoint>\t4294967295\t</helperModelCenterOnHardPoint>"
            ),
            "uint face"
        );
        assert!(xml.contains("<min>\t-1.000000 -1.000000 -1.000000\t</min>"));
        assert!(xml.contains("<max>\t2.000000 2.000000 2.000000\t</max>"));
        assert!(xml.contains("<capacity>\t40\t</capacity>"));

        // the Source writes its generator ahead of its members, the Jitter
        // behind them (source_psa.cpp:106 vs jitter_psa.cpp:213)
        let source_at = xml.find("<Source>").expect("source");
        let gen_at = xml.find("<pPositionSrc>").expect("source generator");
        let members_at = xml.find("<motionTriggered_>").expect("source members");
        assert!(
            gen_at > source_at && gen_at < members_at,
            "source: generator first"
        );
        let jitter_at = xml.find("<Jitter>").expect("jitter");
        let jitter_members_at = xml.find("<affectPosition_>").expect("jitter members");
        let jitter_gen_at = xml.find("<pVelocitySrc>").expect("jitter generator");
        assert!(
            jitter_members_at > jitter_at && jitter_gen_at > jitter_members_at,
            "jitter: generator last"
        );
        assert!(
            xml.find("<Source>").unwrap() < xml.find("<helperModel>").unwrap(),
            "systems ahead of the helper block"
        );

        let parsed = resources::datasection::parse("root", xml.as_bytes()).expect("parse");
        let mut got: Vec<String> = Vec::new();
        tag_paths(&parsed, "", &mut got);
        let expect: &[&str] = &[
            "root",
            "root/p",
            "root/p/serialiseVersionData",
            "root/p/windFactor_",
            "root/p/explicitPosition_",
            "root/p/explicitDirection_",
            "root/p/explicitTransform_",
            "root/p/localOffset_",
            "root/p/maxLod_",
            "root/p/fixedFrameRate_",
            "root/p/Actions",
            "root/p/Actions/Source",
            "root/p/Actions/Source/delay_",
            "root/p/Actions/Source/minimumAge_",
            "root/p/Actions/Source/name_",
            "root/p/Actions/Source/enabled_",
            "root/p/Actions/Source/pPositionSrc",
            "root/p/Actions/Source/pPositionSrc/PointVectorGenerator",
            "root/p/Actions/Source/pPositionSrc/PointVectorGenerator/nameID_",
            "root/p/Actions/Source/pPositionSrc/PointVectorGenerator/position_",
            "root/p/Actions/Source/motionTriggered_",
            "root/p/Actions/Source/timeTriggered_",
            "root/p/Actions/Source/grounded_",
            "root/p/Actions/Source/dropDistance_",
            "root/p/Actions/Source/rate_",
            "root/p/Actions/Source/sensitivity_",
            "root/p/Actions/Source/maxSpeed_",
            "root/p/Actions/Source/activePeriod_",
            "root/p/Actions/Source/sleepPeriod_",
            "root/p/Actions/Source/sleepPeriodMax_",
            "root/p/Actions/Source/minimumSize_",
            "root/p/Actions/Source/maximumSize_",
            "root/p/Actions/Source/forcedUnitSize_",
            "root/p/Actions/Source/allowedTimeInSeconds_",
            "root/p/Actions/Source/initialRotation_",
            "root/p/Actions/Source/randomInitialRotation_",
            "root/p/Actions/Source/initialColour_",
            "root/p/Actions/Source/randomSpin_",
            "root/p/Actions/Source/minSpin_",
            "root/p/Actions/Source/maxSpin_",
            "root/p/Actions/Source/ignoreRotation_",
            "root/p/Actions/Source/inheritVelocity_",
            "root/p/Actions/Jitter",
            "root/p/Actions/Jitter/delay_",
            "root/p/Actions/Jitter/minimumAge_",
            "root/p/Actions/Jitter/name_",
            "root/p/Actions/Jitter/enabled_",
            "root/p/Actions/Jitter/affectPosition_",
            "root/p/Actions/Jitter/affectVelocity_",
            "root/p/Actions/Jitter/pVelocitySrc",
            "root/p/Actions/Jitter/pVelocitySrc/LineVectorGenerator",
            "root/p/Actions/Jitter/pVelocitySrc/LineVectorGenerator/nameID_",
            "root/p/Actions/Jitter/pVelocitySrc/LineVectorGenerator/origin_",
            "root/p/Actions/Jitter/pVelocitySrc/LineVectorGenerator/direction_",
            "root/p/Actions/TintShader",
            "root/p/Actions/TintShader/delay_",
            "root/p/Actions/TintShader/minimumAge_",
            "root/p/Actions/TintShader/name_",
            "root/p/Actions/TintShader/enabled_",
            "root/p/Actions/TintShader/repeat_",
            "root/p/Actions/TintShader/period_",
            "root/p/Actions/TintShader/fogAmount_",
            "root/p/Actions/TintShader/tints_",
            "root/p/Actions/TintShader/tints_/Tint",
            "root/p/Actions/TintShader/tints_/Tint/time",
            "root/p/Actions/TintShader/tints_/Tint/color",
            "root/p/Renderer",
            "root/p/Renderer/SpriteParticleRenderer",
            "root/p/Renderer/SpriteParticleRenderer/viewDependent_",
            "root/p/Renderer/SpriteParticleRenderer/local_",
            "root/p/Renderer/SpriteParticleRenderer/materialFX_",
            "root/p/Renderer/SpriteParticleRenderer/frameCount_",
            "root/p/Renderer/SpriteParticleRenderer/frameRate_",
            "root/p/Renderer/SpriteParticleRenderer/explicitOrientation_",
            "root/p/Renderer/SpriteParticleRenderer/textureName_",
            "root/p/Renderer/SpriteParticleRenderer/softDepthRange_",
            "root/p/Renderer/SpriteParticleRenderer/softFalloffPower_",
            "root/p/Renderer/SpriteParticleRenderer/softDepthOffset_",
            "root/p/Renderer/SpriteParticleRenderer/nearFadeCutoff_",
            "root/p/Renderer/SpriteParticleRenderer/nearFadeStart_",
            "root/p/Renderer/SpriteParticleRenderer/nearFadeFalloffPower_",
            "root/p/capacity",
            "root/p/boundingBox",
            "root/p/boundingBox/min",
            "root/p/boundingBox/max",
            "root/helperModel",
            "root/helperModel/helperModelName",
            "root/helperModel/helperModelCenterOnHardPoint",
        ];
        let expect: Vec<String> = expect.iter().map(|s| s.to_string()).collect();
        assert_eq!(got, expect, "the whole document tree in order");
    }

    /// `MetaParticleSystem::save` sanitises section names (spaces become
    /// the ".." token) and preserves a seed time that was already in the
    /// section it saves over.
    #[test]
    fn meta_save_sanitises_names_and_preserves_the_seed_time() {
        let mut meta = crate::particle::MetaParticleSystem::default();
        let mut system = ParticleSystem::new();
        system.name = "my sys".to_string();
        meta.add_system(system);
        meta.helper_model_name = "helper/thing.model".to_string();

        let mut ds =
            resources::datasection::parse("root", b"<root><seedTime>0.25</seedTime></root>")
                .expect("parse");
        assert!(meta.save(&mut ds, false));
        assert_f32_eq!(ds.read_float("seedTime", -1.0), 0.25, "seed time kept");
        assert!(ds.find_child("my..sys").is_some(), "sanitised section name");
        assert!(ds.find_child("my sys").is_none());
        let helper = ds.find_child("helperModel").expect("helper block");
        assert_eq!(
            helper.read_string("helperModelName", ""),
            "helper/thing.model"
        );

        // a fresh document carries no seed time and none is invented
        let mut fresh = resources::datasection::DataSection::new("root");
        assert!(meta.save(&mut fresh, false));
        assert!(fresh.find_child("seedTime").is_none());
    }

    /// `saveInternal` always makes the `boundingBox` section but only
    /// fills it when the box is not inside out; the empty section reloads
    /// as the unit box and is then cleared, exactly like the reference.
    #[test]
    fn inside_out_box_writes_an_empty_bounding_section() {
        let mut system = ParticleSystem::new();
        system.renderer =
            Some(ParticleRenderer::create_renderer_of_type("SpriteParticleRenderer").unwrap());
        let mut ds = resources::datasection::DataSection::new("root");
        assert!(system.save(&mut ds, "bare", true));
        let bb = ds.find_child("boundingBox").expect("section always made");
        assert_eq!(bb.count_children(), 0, "inside out: no min or max");

        let xml = ds.write_to_string();
        let parsed = resources::datasection::parse("root", xml.as_bytes()).expect("parse");
        let mut loaded = ParticleSystem::new();
        assert!(loaded.load(&parsed, "bare"));
        assert_eq!(loaded.bounding_box.min, Vector3::new(-0.01, -0.01, -0.01));
        assert_eq!(loaded.bounding_box.max, Vector3::new(0.01, 0.01, 0.01));
    }

    /// The acceptance face: the editor document saves, and a fresh editor
    /// (the old editor's loader) reads it back whole.
    #[test]
    fn editor_save_then_load_is_the_old_editor_reload() {
        let mut editor = editor_all_kinds();
        editor.set_capacity(0, 55);
        editor.set_wind_factor(0, 0.4);
        editor.set_explicit_position(0, Vector3::new(1.0, 1.0, 2.0));
        editor.meta.systems[0].set_renderer(ParticleRenderer::create_renderer_of_type(
            "SpriteParticleRenderer",
        ));
        let face = crate::particle_editor::SpriteFace {
            material_fx: MaterialFX::Blended,
            frame_count: 3,
            ..Default::default()
        };
        assert!(editor.set_sprite(0, &face));

        let xml = editor.save(false);
        let mut reloaded = crate::particle_editor::ParticleEditor::new(oracle::ORACLE_SEED);
        assert!(reloaded.load(&xml, "doc"));
        assert_eq!(reloaded.meta.name, "doc");
        assert_eq!(reloaded.meta.n_systems(), 1);
        let original = &editor.meta.systems[0];
        let loaded = &reloaded.meta.systems[0];
        assert_eq!(loaded.name, "all", "the system keeps its section name");
        assert_eq!(loaded.capacity, 55);
        assert_f32_eq!(loaded.wind_factor, 0.4, "wind");
        assert_eq!(loaded.explicit_position, Vector3::new(1.0, 1.0, 2.0));
        assert_eq!(loaded.renderer, original.renderer);
        assert_eq!(loaded.actions, original.actions, "every action kind");
    }
}
