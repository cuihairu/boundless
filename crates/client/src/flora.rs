//! Flora placement tables, block ring and wind animation.
//!
//! Verbatim port of the deterministic half of the reference flora system:
//! the libc-rand placement tables (`lib/romp/flora.cpp`
//! `initialiseOffsetTable`/`seedOffsetTable`/`nextOffset`/`nextRotation`/
//! `nextRandomFloat`), the wrap-around block ring (`moveBlocks`/
//! `teleportCamera`/`resetBlockAt`), the per-item transform
//! (`FloraBlock::nextTransform`, `lib/romp/flora_block.cpp:430`) and the two
//! wind animation grids the renderer feeds the vertex shader
//! (`lib/romp/animation_grid.cpp` `FloraAnimationLocal`/
//! `FloraAnimationGlobal`, driven by `MyPerlin::noise1`). Every draw goes
//! through libc `rand()` seeded `srand(0)` or the integer-hash perlin, so a
//! fixed seed replays identically — that is this item's acceptance.
//!
//! Registered deviations: the chunk-space machinery
//! (`BaseTerrainBlock::findOutsideBlock`, space-bounds culling) is replaced
//! by a [`TerrainProvider`] probe handed to the transform, and the ecotype
//! mesh generation (`Ecotype::generate`, `lib/romp/ecotype_generators.cpp`)
//! plus the vertex-buffer plumbing beyond the block-offset reservation stay
//! with the render backend. `FloraAnimationGrid`'s `noise3` path is not
//! ported: the live renderer feeds its shader constants from the Local and
//! Global grids only.

use crate::libc_rand::LibcRand;
use crate::perlin::FloraNoise;
use foundation::math::Matrix;
use foundation::math::plane::PlaneEq;
use foundation::math::vector::{Vector2, Vector3, Vector4};

/// `flora_constants.hpp` — the width of one flora block.
pub const BLOCK_WIDTH: f32 = 4.0;
/// `flora_constants.hpp` — the visibility radius.
pub const VISIBILITY: f32 = 50.0;
/// `flora_constants.hpp` — the distance a moved ring edge jumps by.
pub const MOVE_DIST: f32 = 100.0;
/// `flora_constants.hpp` — blocks per ring edge (`(VISIBILITY*2)/BLOCK_WIDTH`).
pub const BLOCK_STRIDE: usize = 25;
/// `flora_constants.hpp` — number of `Vector2`s in the offset table.
pub const LUT_SIZE: usize = 1099;
/// `flora_constants.hpp` — the default vertex-buffer byte budget.
pub const DEFAULT_FLORA_VB_SIZE: u32 = 5_000_000;
/// `flora_constants.hpp` — the default texture atlas width.
pub const DEFAULT_FLORA_TEXTURE_WIDTH: u32 = 512;
/// `flora_constants.hpp` — the default texture atlas height.
pub const DEFAULT_FLORA_TEXTURE_HEIGHT: u32 = 256;
/// `BaseTerrainBlock::NO_TERRAIN` (`base_terrain_block.cpp:19`) — compared
/// exactly, as the reference does.
pub const NO_TERRAIN: f32 = -1000000.0;
/// `flora_vertex_type.hpp` `PACK_UV`.
pub const PACK_UV: f32 = 16383.0;
/// `flora_vertex_type.hpp` `PACK_FLEX`.
pub const PACK_FLEX: f32 = 255.0;
/// The packed size of `FloraVertex` (`#pragma pack(1)`: Vector3 + 2×short +
/// 2×short + Vector4).
pub const FLORA_VERTEX_BYTES: usize = 36;

/// `FloraRenderer::update`'s static watcher defaults
/// (`flora_renderer.cpp:321-328`): local noise persistence.
const LOCAL_PERSISTENCE: f32 = 0.3;
/// Local noise octaves.
const LOCAL_OCTAVES: i32 = 6;
/// Local animation scale.
const LOCAL_SCALE: f32 = 0.7;
/// Local animation frequency.
const LOCAL_FREQUENCY: f32 = 0.4;
/// Global noise persistence (`flora_renderer.cpp:329-332`).
const GLOBAL_PERSISTENCE: f32 = 0.4;
/// Global noise octaves.
const GLOBAL_OCTAVES: i32 = 6;
/// Global animation scale.
const GLOBAL_SCALE: f32 = 0.2;
/// Global animation frequency.
const GLOBAL_FREQUENCY: f32 = 0.3;

/// `FloraBlock::Util::s_positionBlur` (`flora_block.cpp:38`).
const POSITION_BLUR: f32 = 1.3;
/// `FloraBlock::Util::s_ecotypeBlur` (`flora_block.cpp:20`).
const ECOTYPE_BLUR: f32 = 1.0;

/// `nextRotation`'s wrap literal (`flora.cpp:1145`) — deliberately the
/// four digits `6.283`, not 2π.
#[allow(clippy::approx_constant)]
const ROTATION_WRAP: f32 = 6.283;

/// The weather input the reference reads through
/// `enviro.weather()->wind()` and `floraWindFactor()`.
pub struct WeatherWind {
    pub wind: Vector2,
    pub flora_wind_factor: f32,
}

/// The terrain probe `next_transform` samples. Stands in for
/// `BaseTerrainBlock::heightAt`/`normalAt` on the found outside block.
pub trait TerrainProvider {
    /// `heightAt` — [`NO_TERRAIN`] where there is no terrain.
    fn height_at(&self, x: f32, z: f32) -> f32;
    /// `normalAt`.
    fn normal_at(&self, x: f32, z: f32) -> Vector3;
}

/// One ring slot: a `FloraBlock`'s deterministic bookkeeping.
pub struct FloraBlock {
    center: Vector2,
    /// Vertexes this block owns from the start of the VB.
    offset: u32,
    /// The terrain-block parity id (`getTerrainBlockID`); `-1` until filled.
    block_id: i32,
    /// `bRefill_` — set by `center(pos)` and `init`, consumed by a fill.
    needs_refill: bool,
}

impl FloraBlock {
    /// `FloraBlock::init` (`flora_block.cpp:120`): take the offset, then
    /// `center(pos)` (which invalidates the block).
    pub fn init(&mut self, pos: Vector2, offset: u32) {
        self.offset = offset;
        self.set_center(pos);
    }

    /// `FloraBlock::center(const Vector2&)` (`flora_block.cpp:336`):
    /// stores the centre and invalidates the block.
    pub fn set_center(&mut self, center: Vector2) {
        self.center = center;
        self.needs_refill = true;
    }

    pub fn center(&self) -> Vector2 {
        self.center
    }

    pub fn offset(&self) -> u32 {
        self.offset
    }

    pub fn block_id(&self) -> i32 {
        self.block_id
    }

    pub fn needs_refill(&self) -> bool {
        self.needs_refill
    }

    /// Marks the block refilled (`fill` clears `bRefill_` after staging).
    pub fn mark_filled(&mut self, block_id: i32) {
        self.block_id = block_id;
        self.needs_refill = false;
    }
}

/// `getTerrainBlockID` (`flora.cpp:1176`): the terrain block's checkerboard
/// parity, from its transform and block size.
pub fn terrain_block_id(transform: &Matrix, terrain_block_size: f32) -> i32 {
    let corner = Vector3::new(terrain_block_size / 2.0, 0.0, terrain_block_size / 2.0);
    let pos = transform.apply_point(&corner) / terrain_block_size;
    let x = (pos.x.floor() as i32).abs();
    let z = (pos.z.floor() as i32).abs();
    (x % 2) | ((z % 2) << 1)
}

/// `PlaneEq::y` (`planeeq.ipp:166`), inlined here because foundation's
/// `PlaneEq` does not carry it.
fn plane_y(eq: &PlaneEq, x: f32, z: f32) -> f32 {
    let n = eq.normal();
    // isZero is isEqual(v, 0): an exact compare.
    if n.y == 0.0 {
        return 0.0; // avoid division by zero
    }
    (eq.d() - n.x * x - n.z * z) / n.y
}

/// The flora system: the offset/random tables, the wrap-around block ring
/// and the LUT cursor (`lutSeed_`).
pub struct Flora {
    offsets: Vec<Vector2>,
    randoms: Vec<f32>,
    lut_seed: usize,
    blocks: Vec<FloraBlock>,
    center_block_x: usize,
    center_block_z: usize,
    num_vertices_per_block: u32,
}

impl Flora {
    /// A flora system with `initialiseOffsetTable` drawn from the given
    /// libc generator. The reference seeds it `srand(0)`; pass
    /// `LibcRand::new(0)` to replay it.
    pub fn new(rng: &mut LibcRand) -> Flora {
        let mut ret = Flora {
            offsets: Vec::new(),
            randoms: Vec::new(),
            lut_seed: 0,
            blocks: (0..BLOCK_STRIDE * BLOCK_STRIDE)
                .map(|_| FloraBlock {
                    center: Vector2::ZERO,
                    offset: 0,
                    block_id: -1,
                    needs_refill: true,
                })
                .collect(),
            center_block_x: 0,
            center_block_z: 0,
            num_vertices_per_block: Self::vertices_per_block(DEFAULT_FLORA_VB_SIZE),
        };
        ret.initialise_offset_table(rng);
        ret
    }

    /// The `vbSize()` derivation (`flora.cpp:1279-1315`): total vertices
    /// from the byte budget over the 36-byte `FloraVertex`, then the
    /// per-block share floored to a multiple of three.
    pub fn vertices_per_block(vb_size: u32) -> u32 {
        let num_vertices = vb_size as usize / FLORA_VERTEX_BYTES;
        let mut per_block = std::cmp::max(num_vertices / (BLOCK_STRIDE * BLOCK_STRIDE), 3) as u32;
        if !per_block.is_multiple_of(3) {
            per_block -= per_block % 3;
        }
        per_block
    }

    /// `initialiseOffsetTable` (`flora.cpp:1067`): `LUT_SIZE` entries, each
    /// three `rand()` draws — angle, radius inside the √8 disc, and the
    /// spare random.
    pub fn initialise_offset_table(&mut self, rng: &mut LibcRand) {
        let mut radius = BLOCK_WIDTH / 2.0;
        radius = (2.0 * (radius * radius)).sqrt();

        self.offsets = Vec::with_capacity(LUT_SIZE);
        self.randoms = Vec::with_capacity(LUT_SIZE);
        for _ in 0..LUT_SIZE {
            let t = rng.next_unit() * (2.0 * std::f32::consts::PI);
            let r = rng.next_unit() * radius;
            self.offsets.push(Vector2::new(r * t.cos(), r * t.sin()));
            self.randoms.push(rng.next_unit());
        }
    }

    /// `seedOffsetTable` (`flora.cpp:1119`): "some crazy deterministic
    /// function" — the `(int)` truncation is the reference's.
    pub fn seed_offset_table(&mut self, center: Vector2) {
        self.lut_seed = ((center.x.abs() * 5.0 + center.y.abs() * 13.0) as i32) as usize;
    }

    /// `nextOffset` (`flora.cpp:1130`).
    pub fn next_offset(&mut self) -> Vector2 {
        self.lut_seed = (self.lut_seed + 1) % LUT_SIZE;
        self.offsets[self.lut_seed]
    }

    /// `nextRotation` (`flora.cpp:1139`): the offset's x wrapped by the
    /// literal `6.283f` — not 2π. Table entries never reach the wrap (the
    /// disc radius is √8), so in-domain this returns `x` unchanged.
    pub fn next_rotation(&mut self) -> f32 {
        self.lut_seed = (self.lut_seed + 1) % LUT_SIZE;
        self.offsets[self.lut_seed].x % ROTATION_WRAP
    }

    /// `nextRandomFloat` (`flora.cpp:1146`).
    pub fn next_random_float(&mut self) -> f32 {
        self.lut_seed = (self.lut_seed + 1) % LUT_SIZE;
        self.randoms[self.lut_seed]
    }

    /// The LUT cursor. The reference keeps `lutSeed_` private; exposed so
    /// tests can pin the exact draw order.
    pub fn lut_seed(&self) -> usize {
        self.lut_seed
    }

    /// `teleportCamera` (`flora.cpp:1000`): snap the whole ring around the
    /// camera. Note the corner offset is float math — `(float)25/2.f*4` is
    /// 50, not the 48 the integer division in `moveBlocks` produces.
    pub fn teleport_camera(&mut self, cam_pos2: Vector2) {
        let top_left = Vector2::new(
            cam_pos2.x - (BLOCK_STRIDE as f32) / 2.0 * BLOCK_WIDTH,
            cam_pos2.y + (BLOCK_STRIDE as f32) / 2.0 * BLOCK_WIDTH,
        );
        let left = BLOCK_WIDTH * (top_left.x / BLOCK_WIDTH).floor();
        let top = BLOCK_WIDTH * (top_left.y / BLOCK_WIDTH).floor();

        let mut offset = 0u32;
        let mut pos = Vector2::ZERO;
        pos.y = top - BLOCK_WIDTH / 2.0;
        for z in 0..BLOCK_STRIDE {
            pos.x = left + BLOCK_WIDTH / 2.0;
            for x in 0..BLOCK_STRIDE {
                self.blocks[z * BLOCK_STRIDE + x].init(pos, offset);
                offset += self.num_vertices_per_block;
                pos.x += BLOCK_WIDTH;
            }
            pos.y -= BLOCK_WIDTH;
        }

        self.center_block_x = BLOCK_STRIDE / 2;
        self.center_block_z = BLOCK_STRIDE / 2;
    }

    /// `moveBlocks` (`flora.cpp:390`): shift one row or column by
    /// `MOVE_DIST` once the camera leaves the centre block's half-block
    /// threshold. Note `moveZ` inverts the camera's z sign, and the
    /// edge indices use the *integer* `25/2 = 12`.
    pub fn move_blocks(&mut self, cam_pos2: Vector2) -> bool {
        let center_block = &self.blocks[self.center_block_z * BLOCK_STRIDE + self.center_block_x];
        let delta = Vector2::new(
            cam_pos2.x - center_block.center.x,
            cam_pos2.y - center_block.center.y,
        );
        let mut move_x = 0i32;
        let mut move_z = 0i32;
        let viz = BLOCK_WIDTH / 2.0;

        if delta.x > viz {
            move_x = 1;
        } else if delta.x < -viz {
            move_x = -1;
        }
        if delta.y > viz {
            move_z = -1;
        } else if delta.y < -viz {
            move_z = 1;
        }

        if move_x != 0 {
            let left = (self.center_block_x + BLOCK_STRIDE - (BLOCK_STRIDE / 2)) % BLOCK_STRIDE;
            let right = (self.center_block_x + ((BLOCK_STRIDE - 1) / 2)) % BLOCK_STRIDE;
            let step_x = move_x as f32 * MOVE_DIST;
            let col_to_move = if move_x > 0 { left } else { right };
            for z in 0..BLOCK_STRIDE {
                let block = &mut self.blocks[z * BLOCK_STRIDE + col_to_move];
                let center = block.center;
                block.set_center(Vector2::new(center.x + step_x, center.y));
            }
            self.center_block_x = ((self.center_block_x as i32 + move_x + BLOCK_STRIDE as i32)
                % BLOCK_STRIDE as i32) as usize;
        } else if move_z != 0 {
            let front = (self.center_block_z + BLOCK_STRIDE - (BLOCK_STRIDE / 2)) % BLOCK_STRIDE;
            let rear = (self.center_block_z + ((BLOCK_STRIDE - 1) / 2)) % BLOCK_STRIDE;
            let step_y = move_z as f32 * -MOVE_DIST;
            let row_to_move = if move_z > 0 { front } else { rear };
            for x in 0..BLOCK_STRIDE {
                let block = &mut self.blocks[row_to_move * BLOCK_STRIDE + x];
                let center = block.center;
                block.set_center(Vector2::new(center.x, center.y + step_y));
            }
            self.center_block_z = ((self.center_block_z as i32 + move_z + BLOCK_STRIDE as i32)
                % BLOCK_STRIDE as i32) as usize;
        }

        move_x != 0 || move_z != 0
    }

    /// `resetBlockAt` (`flora.cpp:1192`): which ring slot covers the world
    /// position. The `dzIdx` sign flips (the ring's rows run south to
    /// north); a point past `VISIBILITY` is not on the ring.
    pub fn reset_block_at(&mut self, x: f32, z: f32) -> Option<(usize, usize)> {
        let center_block = &self.blocks[self.center_block_z * BLOCK_STRIDE + self.center_block_x];
        let center = center_block.center;
        let dx = x - center.x;
        let dz = z - center.y;
        if dx.abs() >= VISIBILITY {
            return None;
        }
        if dz.abs() >= VISIBILITY {
            return None;
        }

        let dx_idx = if dx > 0.0 {
            (dx / BLOCK_WIDTH + 0.5) as i32
        } else {
            (dx / BLOCK_WIDTH - 0.5) as i32
        };
        let dz_idx = if dz > 0.0 {
            (dz / BLOCK_WIDTH + 0.5) as i32
        } else {
            (dz / BLOCK_WIDTH - 0.5) as i32
        };

        let x_idx = ((self.center_block_x as i32 + dx_idx + BLOCK_STRIDE as i32)
            % BLOCK_STRIDE as i32) as usize;
        let z_idx = ((self.center_block_z as i32 - dz_idx + BLOCK_STRIDE as i32)
            % BLOCK_STRIDE as i32) as usize;
        Some((x_idx, z_idx))
    }

    /// The ring slot at `[z][x]` (the reference indexes `blocks_[z][x]`).
    pub fn block(&self, x: usize, z: usize) -> &FloraBlock {
        &self.blocks[z * BLOCK_STRIDE + x]
    }

    pub fn block_mut(&mut self, x: usize, z: usize) -> &mut FloraBlock {
        &mut self.blocks[z * BLOCK_STRIDE + x]
    }

    pub fn center_block(&self) -> (usize, usize) {
        (self.center_block_x, self.center_block_z)
    }

    /// `FloraBlock::nextTransform` (`flora_block.cpp:430`): the next
    /// placement inside `center`'s block — one offset draw, one rotation
    /// draw and a second offset draw for the ecotype sample point, then the
    /// terrain snap. Returns `None` where the reference returns `false`
    /// (no terrain under the spot), *after* the three draws are spent.
    pub fn next_transform(
        &mut self,
        center: Vector2,
        terrain: &dyn TerrainProvider,
    ) -> Option<(Matrix, Vector2)> {
        // get the new position.
        let off = self.next_offset();
        let rot_y = self.next_rotation();
        let mut pos = Vector3::new(
            center.x + off.x * POSITION_BLUR,
            0.0,
            center.y + off.y * POSITION_BLUR,
        );

        // get the new ecotype. this is blurred so we can encroach on
        // neighbouring ecotypes.
        let off = self.next_offset();

        // convert s_ecotypeBlur to metres, to make it easy for artists to
        // tweak.
        let half_block_width = BLOCK_WIDTH / 2.0;
        let radius = (2.0 * (half_block_width * half_block_width)).sqrt();
        let offset_to_metres = 1.0 / radius;
        let ecotype_sample_pt = Vector2::new(
            off.x * offset_to_metres * ECOTYPE_BLUR + pos.x,
            off.y * offset_to_metres * ECOTYPE_BLUR + pos.z,
        );

        // sit on terrain.
        pos.y = terrain.height_at(pos.x, pos.z);
        if pos.y == NO_TERRAIN {
            return None;
        }
        let int_norm = terrain.normal_at(pos.x, pos.z);

        // align to terrain.
        let eq = PlaneEq::new(int_norm, int_norm.dot_product(&pos));
        let xyz0 = Vector3::new(0.0, plane_y(&eq, 0.0, 0.0), 0.0);
        let xyz1 = Vector3::new(0.0, plane_y(&eq, 0.0, 1.0), 1.0);
        let mut up = xyz1 - xyz0;
        up.normalise();
        let zero = Vector3::ZERO;
        let mut ret = Matrix::identity();
        ret.look_at(&zero, &up, eq.normal());
        ret.invert_orthonormal();

        // rotate randomly.
        let mut rot = Matrix::identity();
        rot.set_rotate_y(rot_y);
        ret.pre_multiply(&rot);

        // move to terrain block local coords.
        ret.translation(&pos);

        Some((ret, ecotype_sample_pt))
    }
}

/// `FloraAnimationLocal` (`animation_grid.cpp:252`): a per-column sway
/// grid, `width` entries of two noise reads each.
pub struct FloraAnimationLocal {
    grid: Vec<Vector2>,
    noise: FloraNoise,
    total_time: f32,
    scale: f32,
}

impl FloraAnimationLocal {
    pub fn new(width: usize) -> FloraAnimationLocal {
        FloraAnimationLocal {
            grid: vec![Vector2::ZERO; width],
            noise: FloraNoise::new(),
            total_time: 0.0,
            scale: 1.0,
        }
    }

    pub fn noise(&mut self) -> &mut FloraNoise {
        &mut self.noise
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale;
    }

    pub fn total_time(&self) -> f32 {
        self.total_time
    }

    pub fn grid(&self) -> &[Vector2] {
        &self.grid
    }

    pub fn update(&mut self, d_time: f32, enviro: &WeatherWind) {
        self.total_time += d_time;

        let v_wind = Vector2::new(
            enviro.wind.x * enviro.flora_wind_factor,
            enviro.wind.y * enviro.flora_wind_factor,
        );
        let f_wind = (v_wind.length() + 1.0).ln() * self.scale;

        for i in 0..self.grid.len() {
            self.grid[i] = Vector2::new(
                (self.noise.noise1(self.total_time + i as f32 + 1024.0) - 0.5) * 2.0 * f_wind,
                (self.noise.noise1(self.total_time + i as f32) - 0.5) * 2.0 * f_wind,
            );
        }
    }
}

/// `FloraAnimationGlobal` (`animation_grid.cpp:274`): a global gust grid
/// plus the unit wind direction the shader blends in.
pub struct FloraAnimationGlobal {
    grid: Vec<Vector2>,
    noise: FloraNoise,
    total_time: f32,
    scale: f32,
    direction: Vector2,
}

impl FloraAnimationGlobal {
    pub fn new(width: usize) -> FloraAnimationGlobal {
        FloraAnimationGlobal {
            grid: vec![Vector2::ZERO; width],
            noise: FloraNoise::new(),
            total_time: 0.0,
            scale: 1.0,
            direction: Vector2::ZERO,
        }
    }

    pub fn noise(&mut self) -> &mut FloraNoise {
        &mut self.noise
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale;
    }

    pub fn total_time(&self) -> f32 {
        self.total_time
    }

    pub fn grid(&self) -> &[Vector2] {
        &self.grid
    }

    pub fn direction(&self) -> Vector2 {
        self.direction
    }

    pub fn update(&mut self, d_time: f32, enviro: &WeatherWind) {
        self.total_time += d_time;

        let v_wind = Vector2::new(
            enviro.wind.x * enviro.flora_wind_factor * self.scale,
            enviro.wind.y * enviro.flora_wind_factor * self.scale,
        );
        self.direction = v_wind;
        self.direction.normalise();

        for i in 0..self.grid.len() {
            let f_noise = self.noise.noise1(i as f32 + self.total_time);
            self.grid[i] = Vector2::new(v_wind.x * f_noise, v_wind.y * f_noise);
        }
    }
}

/// The pair `FloraRenderer::update` (`flora_renderer.cpp:320-386`) drives:
/// the watcher defaults applied once, then both grids advanced by the
/// frame scaled with their frequencies.
pub struct FloraAnimations {
    pub local: FloraAnimationLocal,
    pub global: FloraAnimationGlobal,
}

impl FloraAnimations {
    /// The renderer's width-64 grids with its static defaults.
    pub fn new() -> FloraAnimations {
        let mut ret = FloraAnimations {
            local: FloraAnimationLocal::new(64),
            global: FloraAnimationGlobal::new(64),
        };
        ret.local.noise().set_persistence(LOCAL_PERSISTENCE);
        ret.local.noise().set_octaves(LOCAL_OCTAVES);
        ret.local.set_scale(LOCAL_SCALE);
        ret.global.noise().set_persistence(GLOBAL_PERSISTENCE);
        ret.global.noise().set_octaves(GLOBAL_OCTAVES);
        ret.global.set_scale(GLOBAL_SCALE);
        ret
    }

    pub fn update(&mut self, d_time: f32, enviro: &WeatherWind) {
        self.local.update(d_time * LOCAL_FREQUENCY, enviro);
        self.global.update(d_time * GLOBAL_FREQUENCY, enviro);
    }
}

impl Default for FloraAnimations {
    fn default() -> FloraAnimations {
        FloraAnimations::new()
    }
}

/// A packed `FloraVertex` (`flora_vertex_type.hpp`): position, UVs packed
/// against `PACK_UV`, the flex/animation shorts packed against
/// `PACK_FLEX`, and the tint.
#[derive(Clone, Copy)]
pub struct FloraVertex {
    pos: Vector3,
    uv: [i16; 2],
    /// `[flex, animationIdx]`.
    animation: [i16; 2],
    tint: Vector4,
}

impl FloraVertex {
    pub fn new() -> FloraVertex {
        FloraVertex {
            pos: Vector3::ZERO,
            uv: [0; 2],
            animation: [0; 2],
            tint: Vector4::ZERO,
        }
    }

    /// `set`: the position and the source visual's UVs.
    pub fn set(&mut self, pos: Vector3, uv: (f32, f32)) {
        self.pos = pos;
        self.uv[0] = (uv.0 * PACK_UV) as i16;
        self.uv[1] = (uv.1 * PACK_UV) as i16;
        self.tint = Vector4::ZERO;
    }

    pub fn pos(&self) -> Vector3 {
        self.pos
    }

    /// `uv`: repack the UVs.
    pub fn set_uv(&mut self, uv: (f32, f32)) {
        self.uv[0] = (uv.0 * PACK_UV) as i16;
        self.uv[1] = (uv.1 * PACK_UV) as i16;
    }

    pub fn uv(&self) -> (f32, f32) {
        (self.uv[0] as f32 / PACK_UV, self.uv[1] as f32 / PACK_UV)
    }

    /// `flex`: the sway weight short.
    pub fn set_flex(&mut self, f: f32) {
        self.animation[0] = (PACK_FLEX * f) as i16;
    }

    pub fn flex(&self) -> f32 {
        self.animation[0] as f32 / PACK_FLEX
    }

    pub fn set_animation_idx(&mut self, idx: i16) {
        self.animation[1] = idx;
    }

    pub fn animation_idx(&self) -> i16 {
        self.animation[1]
    }

    pub fn set_tint(&mut self, tint: Vector4) {
        self.tint = tint;
    }

    pub fn tint(&self) -> Vector4 {
        self.tint
    }
}

impl Default for FloraVertex {
    fn default() -> FloraVertex {
        FloraVertex::new()
    }
}

/// The ecotype generator's flex weight (`ecotype_generators.cpp:620-628`):
/// the vertex's height over the mesh's bottom half, clamped, then cut to
/// zero below the 0.1 floor.
pub fn mesh_flex(vertex_y: f32, min_y: f32, flex: f32) -> f32 {
    let mut relative_height = (vertex_y - min_y) / 2.0;
    if relative_height > 1.0 {
        relative_height = 1.0;
    }
    if relative_height > 0.1 {
        flex * relative_height
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat plane at `height` with the given normal.
    struct FlatTerrain {
        height: f32,
        normal: Vector3,
    }

    impl TerrainProvider for FlatTerrain {
        fn height_at(&self, _x: f32, _z: f32) -> f32 {
            self.height
        }
        fn normal_at(&self, _x: f32, _z: f32) -> Vector3 {
            self.normal
        }
    }

    /// The reference probe: everything is [`NO_TERRAIN`].
    struct NoTerrain;

    impl TerrainProvider for NoTerrain {
        fn height_at(&self, _x: f32, _z: f32) -> f32 {
            NO_TERRAIN
        }
        fn normal_at(&self, _x: f32, _z: f32) -> Vector3 {
            Vector3::ZERO
        }
    }

    #[test]
    fn the_offset_table_replays_bit_identically_from_a_fixed_seed() {
        let a = Flora::new(&mut LibcRand::new(0));
        let b = Flora::new(&mut LibcRand::new(0));
        // All 1099 entries agree bit for bit.
        for i in 0..LUT_SIZE {
            assert_eq!(a.offsets[i], b.offsets[i]);
            assert_eq!(a.randoms[i], b.randoms[i]);
        }
        // A different seed gives a different table (seed 1 is no good: the
        // libc maps srand(0) to 1, so 0 and 1 replay identically).
        let c = Flora::new(&mut LibcRand::new(2));
        assert_ne!(a.offsets[0], c.offsets[0]);
        // Every entry sits inside the √8 disc.
        let radius = (2.0 * (BLOCK_WIDTH / 2.0 * BLOCK_WIDTH / 2.0)).sqrt();
        for off in &a.offsets {
            assert!(off.length() <= radius + 1e-6, "{}", off.length());
        }
        // The spare randoms are unit floats.
        for r in &a.randoms {
            assert!((0.0..=1.0).contains(r));
        }
        // The table consumed exactly 3 * LUT_SIZE draws: a fresh generator
        // walked the same 3297 words lands on the same next draw.
        let mut walked = LibcRand::new(0);
        for _ in 0..3 * LUT_SIZE {
            let _ = walked.next();
        }
        let mut fresh = LibcRand::new(0);
        let _ = Flora::new(&mut fresh);
        assert_eq!(walked.next(), fresh.next());
    }

    #[test]
    fn seeding_follows_the_reference_formula_and_the_trio_walks_in_order() {
        let mut flora = Flora::new(&mut LibcRand::new(0));
        // (int)(|0.6|*5 + |0.2|*13) = (int)5.6 = 5 — the cast truncates.
        flora.seed_offset_table(Vector2::new(0.6, 0.2));
        assert_eq!(flora.lut_seed(), 5);
        // Each accessor advances the cursor once before reading.
        assert_eq!(flora.next_offset(), flora.offsets[6]);
        assert_eq!(flora.lut_seed(), 6);
        assert_eq!(flora.next_rotation(), flora.offsets[7].x % ROTATION_WRAP);
        assert_eq!(flora.lut_seed(), 7);
        assert_eq!(flora.next_random_float(), flora.randoms[8]);
        assert_eq!(flora.lut_seed(), 8);
        // The rotation's 6.283f wrap is a no-op in-domain: every table x
        // sits inside the √8 disc.
        assert!(flora.offsets[7].x.abs() < ROTATION_WRAP);
    }

    #[test]
    fn the_cursor_wraps_around_the_table_end() {
        let mut flora = Flora::new(&mut LibcRand::new(0));
        flora.seed_offset_table(Vector2::new(LUT_SIZE as f32 - 1.0, 0.0));
        // (int)(1098*5) = 5490 — stored raw; the mod happens on access.
        assert_eq!(flora.lut_seed(), 5490);
        assert_eq!(flora.next_offset(), flora.offsets[(5490 + 1) % LUT_SIZE]);
        assert_eq!(flora.lut_seed(), (5490 + 1) % LUT_SIZE);
        assert_eq!(
            flora.next_rotation(),
            flora.offsets[(5490 + 2) % LUT_SIZE].x % ROTATION_WRAP
        );
        assert_eq!(flora.lut_seed(), (5490 + 2) % LUT_SIZE);
        assert_eq!(
            flora.next_random_float(),
            flora.randoms[(5490 + 3) % LUT_SIZE]
        );
    }

    #[test]
    fn teleport_camera_snaps_the_ring_around_the_camera() {
        let mut flora = Flora::new(&mut LibcRand::new(0));
        flora.teleport_camera(Vector2::new(7.0, -3.0));
        // The camera sits near the ring centre block.
        assert_eq!(flora.center_block(), (12, 12));
        // topLeft for cam (7, -3) is (-43, 47): left = -44, top = 44, so
        // the first block centres at (-42, 42) and steps by BLOCK_WIDTH.
        assert_eq!(flora.block(0, 0).center(), Vector2::new(-42.0, 42.0));
        assert_eq!(flora.block(1, 0).center(), Vector2::new(-38.0, 42.0));
        assert_eq!(flora.block(0, 1).center(), Vector2::new(-42.0, 38.0));
        // The centre block itself.
        assert_eq!(flora.block(12, 12).center(), Vector2::new(6.0, -6.0));
        // Offsets reserve the VB space block by block.
        assert_eq!(flora.block(0, 0).offset(), 0);
        assert_eq!(
            flora.block(1, 0).offset(),
            Flora::vertices_per_block(DEFAULT_FLORA_VB_SIZE)
        );
        // A teleport invalidates every block, which carries a refill flag.
        assert!(flora.block(24, 24).needs_refill());
    }

    #[test]
    fn the_default_vb_budget_yields_the_reference_vertex_share() {
        // 5,000,000 / 36 = 138,888 vertices; / 625 blocks = 222.2208 -> 222;
        // 222 % 3 == 0, so it stays.
        assert_eq!(Flora::vertices_per_block(DEFAULT_FLORA_VB_SIZE), 222);
        // A share that is not a multiple of three floors down: 4999999/36
        // = 138888 too, but a budget that lands on 223 drops to 222.
        assert_eq!(Flora::vertices_per_block(25 * 25 * 3 * 36 + 1), 3);
        // Tiny budgets clamp to the three-vertex minimum.
        assert_eq!(Flora::vertices_per_block(36), 3);
        assert_eq!(Flora::vertices_per_block(0), 3);
        // Shares that miss the multiple of three floor down: 90000/36 =
        // 2500 vertices is 4 per block -> 3, and the budget beside the
        // reference default lands on 224 -> 222.
        assert_eq!(Flora::vertices_per_block(90_000), 3);
        assert_eq!(Flora::vertices_per_block(5_040_000), 222);
    }

    #[test]
    fn block_bookkeeping_tracks_refill_state() {
        let mut flora = Flora::new(&mut LibcRand::new(0));
        flora.teleport_camera(Vector2::new(7.0, -3.0));
        // A fresh slot is unfilled: the parity id is still unset.
        let block = flora.block_mut(3, 4);
        assert_eq!(block.block_id(), -1);
        assert!(block.needs_refill());
        // Filling stamps the terrain parity and clears the flag.
        block.mark_filled(2);
        assert_eq!(block.block_id(), 2);
        assert!(!block.needs_refill());
        // Re-centring invalidates the slot again.
        let block = flora.block_mut(3, 4);
        block.set_center(Vector2::new(1.5, 2.5));
        assert_eq!(block.center(), Vector2::new(1.5, 2.5));
        assert!(block.needs_refill());
        assert_eq!(flora.block(3, 4).center(), Vector2::new(1.5, 2.5));
    }

    #[test]
    fn move_blocks_shifts_one_row_or_column_at_the_half_block_threshold() {
        let mut flora = Flora::new(&mut LibcRand::new(0));
        flora.teleport_camera(Vector2::new(7.0, -3.0));

        // cam (7, -3) vs centre (6, -6): delta (1, 3) crosses the z
        // threshold only, and the z sign flips: moveZ = -1 moves the rear
        // row by +MOVE_DIST.
        assert!(flora.move_blocks(Vector2::new(7.0, -3.0)));
        assert_eq!(flora.center_block(), (12, 11));
        // The moved row (24) jumped north by 100: -54 -> 46.
        assert_eq!(flora.block(0, 24).center(), Vector2::new(-42.0, 46.0));
        // Other rows stayed.
        assert_eq!(flora.block(0, 0).center(), Vector2::new(-42.0, 42.0));

        // delta (1, -1): the ring has caught up, inside both thresholds.
        assert!(!flora.move_blocks(Vector2::new(7.0, -3.0)));
        assert_eq!(flora.center_block(), (12, 11));

        // Far east: delta.x crosses +2, moving the left column east.
        assert!(flora.move_blocks(Vector2::new(100.0, -3.0)));
        assert_eq!(flora.center_block(), (13, 11));
        assert_eq!(flora.block(0, 0).center(), Vector2::new(58.0, 42.0));
        assert!(flora.block(0, 0).needs_refill());

        // Far west: delta.x < -2, moving the right column west. From
        // centre x 13 the right edge is (13+12)%25 = column 0.
        assert!(flora.move_blocks(Vector2::new(-100.0, -3.0)));
        assert_eq!(flora.center_block(), (12, 11));
        // Column 0 jumped west by 100: 58 -> -42.
        assert_eq!(flora.block(0, 0).center(), Vector2::new(-42.0, 42.0));
    }

    #[test]
    fn move_blocks_catches_the_camera_running_south() {
        let mut flora = Flora::new(&mut LibcRand::new(0));
        flora.teleport_camera(Vector2::new(7.0, -3.0));
        // cam (7, -10) vs centre (6, -6): delta.y = -4 crosses the -2
        // threshold, and moveZ's sign flip moves the front row south by
        // MOVE_DIST.
        assert!(flora.move_blocks(Vector2::new(7.0, -10.0)));
        assert_eq!(flora.center_block(), (12, 13));
        assert_eq!(flora.block(0, 0).center(), Vector2::new(-42.0, -58.0));
        // Other rows stayed.
        assert_eq!(flora.block(0, 12).center(), Vector2::new(-42.0, -6.0));
    }

    #[test]
    fn reset_block_at_covers_negative_and_positive_offsets() {
        let mut flora = Flora::new(&mut LibcRand::new(0));
        flora.teleport_camera(Vector2::new(7.0, -3.0));
        // dx = -4: the negative arm truncates -1.5 toward zero to -1,
        // landing one slot west of centre.
        assert_eq!(flora.reset_block_at(2.0, -6.0), Some((11, 12)));
        // dz = +4: the positive arm rounds 1.5 down to 1, and z_idx flips
        // the sign, landing one row north of centre.
        assert_eq!(flora.reset_block_at(10.0, -2.0), Some((13, 11)));
    }

    #[test]
    fn reset_block_at_maps_world_positions_onto_ring_slots() {
        let mut flora = Flora::new(&mut LibcRand::new(0));
        flora.teleport_camera(Vector2::new(7.0, -3.0));

        // (10, -6) sits exactly on block (13, 12)'s centre.
        assert_eq!(flora.reset_block_at(10.0, -6.0), Some((13, 12)));
        // The mapped slot's centre is the point itself.
        assert_eq!(flora.block(13, 12).center(), Vector2::new(10.0, -6.0));

        // Fractional positions round to the nearest block, x with the
        // sign-aware +/-0.5, z against the flipped sign. (11.9, -7.9) is
        // dx 5.9 -> idx 1; dz -1.9 -> -0.975 truncates toward zero to 0
        // (the reference's int() cast), so slot (13, 12).
        assert_eq!(flora.reset_block_at(11.9, -7.9), Some((13, 12)));

        // Past VISIBILITY there is no slot.
        assert_eq!(flora.reset_block_at(100.0, -6.0), None);
        assert_eq!(flora.reset_block_at(10.0, 60.0), None);
    }

    #[test]
    fn terrain_block_id_is_a_checkerboard_parity() {
        let mut m = Matrix::identity();
        assert_eq!(terrain_block_id(&m, 100.0), 0);
        m.translation(&Vector3::new(100.0, 0.0, 0.0));
        assert_eq!(terrain_block_id(&m, 100.0), 1);
        let mut m = Matrix::identity();
        m.translation(&Vector3::new(0.0, 0.0, 100.0));
        assert_eq!(terrain_block_id(&m, 100.0), 2);
        let mut m = Matrix::identity();
        m.translation(&Vector3::new(100.0, 0.0, 100.0));
        assert_eq!(terrain_block_id(&m, 100.0), 3);
        // Two blocks diagonally wrap the parity back to zero.
        let mut m = Matrix::identity();
        m.translation(&Vector3::new(200.0, 0.0, 200.0));
        assert_eq!(terrain_block_id(&m, 100.0), 0);
    }

    #[test]
    fn next_transform_sits_on_flat_ground_with_the_random_yaw() {
        let mut flora = Flora::new(&mut LibcRand::new(0));
        let center = Vector2::new(6.0, -6.0);
        // Pin the draws the reference spends: offset, rotation, offset.
        let mut probe = Flora::new(&mut LibcRand::new(0));
        probe.seed_offset_table(center);
        let off1 = probe.next_offset();
        let rot_y = probe.next_rotation();
        let _off2 = probe.next_offset();

        let flat = FlatTerrain {
            height: 5.0,
            normal: Vector3::new(0.0, 1.0, 0.0),
        };
        flora.seed_offset_table(center);
        let (matrix, sample) = flora.next_transform(center, &flat).expect("flat ground");
        // |6|*5 + |−6|*13 = 108.
        assert_eq!(flora.lut_seed(), (108 + 3) % LUT_SIZE);

        // The ecotype sample point: the second offset scaled out of the
        // √8 disc into metres, on top of the position. Mirrored expression
        // for expression so the bit compares hold.
        let offset_to_metres = 1.0 / radius_sqrt8();
        let pos_x = center.x + off1.x * POSITION_BLUR;
        let pos_z = center.y + off1.y * POSITION_BLUR;
        let expected_sample = Vector2::new(
            _off2.x * offset_to_metres * ECOTYPE_BLUR + pos_x,
            _off2.y * offset_to_metres * ECOTYPE_BLUR + pos_z,
        );
        assert_eq!(sample, expected_sample);

        // On flat ground the alignment collapses to the yaw only.
        let mut expected = Matrix::identity();
        expected.set_rotate_y(rot_y);
        expected.translation(&Vector3::new(
            center.x + off1.x * POSITION_BLUR,
            5.0,
            center.y + off1.y * POSITION_BLUR,
        ));
        assert_eq!(matrix, expected);
    }

    #[test]
    fn next_transform_rejects_no_terrain_after_spending_the_draws() {
        let mut flora = Flora::new(&mut LibcRand::new(0));
        let center = Vector2::new(0.0, 0.0);
        flora.seed_offset_table(center);
        assert_eq!(flora.next_transform(center, &NoTerrain), None);
        // The draws went first: the cursor moved three steps.
        assert_eq!(flora.lut_seed(), 3 % LUT_SIZE);
        // The probe's normal read is part of the trait surface.
        assert_eq!(NoTerrain.normal_at(0.0, 0.0), Vector3::ZERO);
    }

    #[test]
    fn next_transform_handles_a_wall_normal_with_no_y() {
        // A normal with zero y sends PlaneEq::y down its exact-zero guard:
        // both sampled ys collapse to zero, the up vector is the z axis,
        // and the alignment still composes.
        let mut flora = Flora::new(&mut LibcRand::new(0));
        let center = Vector2::new(6.0, -6.0);
        let wall = FlatTerrain {
            height: 3.0,
            normal: Vector3::new(1.0, 0.0, 0.0),
        };
        flora.seed_offset_table(center);
        let (matrix, _) = flora.next_transform(center, &wall).expect("wall");

        // Pin the draws: offset, rotation, offset.
        let mut probe = Flora::new(&mut LibcRand::new(0));
        probe.seed_offset_table(center);
        let off1 = probe.next_offset();
        let rot_y = probe.next_rotation();
        let _ = probe.next_offset();

        let pos = Vector3::new(
            center.x + off1.x * POSITION_BLUR,
            3.0,
            center.y + off1.y * POSITION_BLUR,
        );
        assert_eq!(matrix.apply_to_origin(), pos);

        // Hand-derived alignment: the guarded plane ys make the forward
        // exactly +z, so the chain must replay bit for bit.
        let mut up = Vector3::new(0.0, 0.0, 1.0);
        up.normalise();
        let eq = PlaneEq::new(wall.normal, wall.normal.dot_product(&pos));
        let mut expected = Matrix::identity();
        let zero = Vector3::ZERO;
        expected.look_at(&zero, &up, eq.normal());
        expected.invert_orthonormal();
        let mut rot = Matrix::identity();
        rot.set_rotate_y(rot_y);
        expected.pre_multiply(&rot);
        expected.translation(&pos);
        assert_eq!(matrix, expected);
    }

    #[test]
    fn next_transform_aligns_to_a_tilted_plane() {
        let mut flora = Flora::new(&mut LibcRand::new(0));
        let center = Vector2::new(6.0, -6.0);
        flora.seed_offset_table(center);
        // Normal (0, 1, 0.5) with height 4: d = n·pos = 4 + 0.5*pos.z, so
        // y(0,0) - y(0,1) = 0.5 and the up vector tilts back.
        let tilted = FlatTerrain {
            height: 4.0,
            normal: Vector3::new(0.0, 1.0, 0.5),
        };
        let (matrix, _) = flora
            .next_transform(center, &tilted)
            .expect("tilted ground");

        // Pin the draws: offset, rotation, offset at 109/110/111.
        let mut probe = Flora::new(&mut LibcRand::new(0));
        probe.seed_offset_table(center);
        let off1 = probe.next_offset();
        let rot_y = probe.next_rotation();

        let pos = Vector3::new(
            center.x + off1.x * POSITION_BLUR,
            4.0,
            center.y + off1.y * POSITION_BLUR,
        );
        assert_eq!(matrix.apply_to_origin(), pos);

        // Hand-derived alignment: y(0,0) - y(0,1) = 0.5, so the aligned
        // forward is (0, -0.5, 1) normalised and the plane constant is
        // n·pos.
        let mut up = Vector3::new(0.0, -0.5, 1.0);
        up.normalise();
        let eq = PlaneEq::new(tilted.normal, tilted.normal.dot_product(&pos));
        let mut expected = Matrix::identity();
        let zero = Vector3::ZERO;
        expected.look_at(&zero, &up, eq.normal());
        expected.invert_orthonormal();
        let mut rot = Matrix::identity();
        rot.set_rotate_y(rot_y);
        expected.pre_multiply(&rot);
        expected.translation(&pos);
        assert_eq!(matrix, expected);

        // The local up, yawed then aligned: the tilt's y tilt survives the
        // yaw scaled by its cosine.
        let world_up = matrix.apply_point(&Vector3::new(0.0, 0.0, 1.0));
        assert!(
            (world_up.y - (4.0 + up.y * rot_y.cos())).abs() < 1e-5,
            "{}",
            world_up.y
        );
    }

    #[test]
    fn the_full_seed_chain_replays_identically() {
        let terrain = FlatTerrain {
            height: 5.0,
            normal: Vector3::new(0.0, 1.0, 0.0),
        };
        let mut a = Flora::new(&mut LibcRand::new(0));
        let mut b = Flora::new(&mut LibcRand::new(0));

        for cam in [
            Vector2::new(7.0, -3.0),
            Vector2::new(20.0, 11.0),
            Vector2::new(-33.0, -27.0),
        ] {
            a.teleport_camera(cam);
            a.move_blocks(cam);
            b.teleport_camera(cam);
            b.move_blocks(cam);
        }

        let center = Vector2::new(6.0, -6.0);
        for _ in 0..50 {
            a.seed_offset_table(center);
            b.seed_offset_table(center);
            let (ma, sa) = a.next_transform(center, &terrain).expect("flat");
            let (mb, sb) = b.next_transform(center, &terrain).expect("flat");
            assert_eq!(ma, mb);
            assert_eq!(sa, sb);
            assert_eq!(a.lut_seed(), b.lut_seed());
        }
    }

    #[test]
    fn the_local_wind_grid_follows_the_reference_formula() {
        let enviro = WeatherWind {
            wind: Vector2::new(0.5, 0.25),
            flora_wind_factor: 2.0,
        };
        let mut local = FloraAnimationLocal::new(64);
        local.noise().set_persistence(LOCAL_PERSISTENCE);
        local.noise().set_octaves(LOCAL_OCTAVES);
        local.set_scale(LOCAL_SCALE);
        local.update(1.0, &enviro);

        // fWind = ln(|wind*factor| + 1) * scale.
        let v_wind = Vector2::new(1.0, 0.5);
        let f_wind = (v_wind.length() + 1.0).ln() * LOCAL_SCALE;
        for (i, entry) in local.grid().iter().enumerate().take(8) {
            let noise = FloraNoise::new()
                .with_persistence(LOCAL_PERSISTENCE)
                .with_octaves(LOCAL_OCTAVES);
            let t = 1.0 + i as f32;
            assert_eq!(
                entry.y,
                (noise.noise1(t) - 0.5) * 2.0 * f_wind,
                "grid[{i}].y"
            );
            assert_eq!(
                entry.x,
                (noise.noise1(t + 1024.0) - 0.5) * 2.0 * f_wind,
                "grid[{i}].x"
            );
        }
        assert_eq!(local.total_time(), 1.0);
        // Zero wind collapses the whole grid: ln(1) is zero.
        let calm = WeatherWind {
            wind: Vector2::ZERO,
            flora_wind_factor: 1.0,
        };
        let mut still = FloraAnimationLocal::new(4);
        still.update(1.0, &calm);
        for entry in still.grid() {
            assert_eq!(entry.x, 0.0);
            assert_eq!(entry.y, 0.0);
        }
    }

    #[test]
    fn the_global_wind_grid_follows_the_reference_formula() {
        let enviro = WeatherWind {
            wind: Vector2::new(0.5, 0.25),
            flora_wind_factor: 2.0,
        };
        let mut global = FloraAnimationGlobal::new(64);
        global.noise().set_persistence(GLOBAL_PERSISTENCE);
        global.noise().set_octaves(GLOBAL_OCTAVES);
        global.set_scale(GLOBAL_SCALE);
        global.update(2.0, &enviro);

        // The direction is the scaled wind normalised.
        let v_wind = Vector2::new(0.5 * 2.0 * GLOBAL_SCALE, 0.25 * 2.0 * GLOBAL_SCALE);
        let mut expected_dir = v_wind;
        expected_dir.normalise();
        assert_eq!(global.direction(), expected_dir);
        // Every entry is the scaled wind times one noise read at i + t.
        for (i, entry) in global.grid().iter().enumerate().take(8) {
            let noise = FloraNoise::new()
                .with_persistence(GLOBAL_PERSISTENCE)
                .with_octaves(GLOBAL_OCTAVES);
            let f_noise = noise.noise1(i as f32 + 2.0);
            assert_eq!(entry.x, v_wind.x * f_noise, "grid[{i}].x");
            assert_eq!(entry.y, v_wind.y * f_noise, "grid[{i}].y");
        }
        assert_eq!(global.total_time(), 2.0);
    }

    #[test]
    fn the_renderer_pace_advances_both_grids_by_their_frequencies() {
        let mut anims = FloraAnimations::new();
        let enviro = WeatherWind {
            wind: Vector2::new(0.5, 0.25),
            flora_wind_factor: 2.0,
        };
        anims.update(1.0, &enviro);
        assert_eq!(anims.local.total_time(), LOCAL_FREQUENCY);
        assert_eq!(anims.global.total_time(), GLOBAL_FREQUENCY);
        // The renderer defaults are the watcher statics.
        let mut l = FloraAnimationLocal::new(64);
        l.noise().set_persistence(LOCAL_PERSISTENCE);
        assert_eq!(l.total_time(), 0.0);
    }

    #[test]
    fn the_default_animations_and_vertices_start_zeroed() {
        // Default carries the same renderer statics as new().
        let anims = FloraAnimations::default();
        assert_eq!(anims.local.total_time(), 0.0);
        assert_eq!(anims.local.grid().len(), 64);
        assert_eq!(anims.global.total_time(), 0.0);
        assert_eq!(anims.global.grid().len(), 64);

        // A default vertex is all zeroes...
        let mut v = FloraVertex::default();
        assert_eq!(v.pos(), Vector3::ZERO);
        assert_eq!(v.uv(), (0.0, 0.0));
        assert_eq!(v.flex(), 0.0);
        assert_eq!(v.animation_idx(), 0);
        assert_eq!(v.tint(), Vector4::ZERO);
        // ...and set_uv repacks the coordinates without touching the rest.
        v.set_uv((0.5, 0.25));
        assert_eq!(v.uv(), (8191.0 / PACK_UV, 4095.0 / PACK_UV));
        assert_eq!(v.flex(), 0.0);
    }

    #[test]
    fn flora_vertices_pack_with_truncation() {
        let mut v = FloraVertex::new();
        v.set(Vector3::new(1.0, 2.0, 3.0), (0.5, 0.25));
        // 0.5 * 16383 = 8191.5 truncates to 8191; 0.25 * 16383 = 4095.75
        // truncates to 4095.
        assert_eq!(v.uv(), (8191.0 / PACK_UV, 4095.0 / PACK_UV));
        v.set_flex(0.5);
        // 255 * 0.5 = 127.5 truncates to 127.
        assert_eq!(v.flex(), 127.0 / PACK_FLEX);
        v.set_animation_idx(7);
        assert_eq!(v.animation_idx(), 7);
        assert_eq!(v.pos(), Vector3::new(1.0, 2.0, 3.0));
        let tint = Vector4::new(1.0, 2.0, 3.0, 0.25);
        v.set_tint(tint);
        assert_eq!(v.tint(), tint);
        // set zeroes the tint again.
        v.set(Vector3::ZERO, (0.0, 0.0));
        assert_eq!(v.tint(), Vector4::ZERO);
    }

    #[test]
    fn mesh_flex_clamps_and_floors() {
        // Two metres above the bottom is full flex.
        assert_eq!(mesh_flex(2.0, 0.0, 1.0), 1.0);
        // Over the top clamps to one.
        assert_eq!(mesh_flex(10.0, 0.0, 0.8), 0.8);
        // Below the 0.1 floor there is no sway.
        assert_eq!(mesh_flex(0.1, 0.0, 1.0), 0.0);
        // Linear in between.
        assert_eq!(mesh_flex(1.0, 0.0, 1.0), 0.5);
    }

    fn radius_sqrt8() -> f32 {
        let half = BLOCK_WIDTH / 2.0;
        (2.0 * (half * half)).sqrt()
    }
}
