//! BW-13-04-04 particle editor faces and preview loop.
//!
//! The reference editor is a Win32 document (`ParticleEditorDoc`) whose
//! per-panel views (`PsProperties`, `PsaProperties`, the per-action
//! dialogs, `PsRendererProperties`, the vector generator custodian)
//! write straight onto a `MetaParticleSystem`, with `PeModule::updateState`
//! ticking `GetMetaParticleSystem()->tick(dTime)` every frame and
//! `updateAnimations` flushing the queued late updates once per frame.
//! This module carries that split: the document and the property faces
//! in [`ParticleEditor`], the fixed-seed preview loop that drives it
//! (`tick`), and the [`PreviewSnapshot`] record a determinism test
//! compares against the Python oracle.
//!
//! The RNG seam matters here: the reference `BWRandom` is a
//! time-seeded global singleton, so its editor preview changes every
//! run. The port seeds it explicitly, which is what the acceptance
//! ("固定种子的可见行为吻合") requires — the same seed runs the same
//! preview twice.

use crate::particle::{
    BarrierReaction, BarrierShape, BwRandom, MAX_CAPACITY, MaterialFX, ParticleEnv, ParticleRecord,
    Tint, VectorGenerator,
};

/// The reference preview frame: every particle of every system, in
/// system order then storage order.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewSnapshot {
    pub systems: Vec<SystemSnapshot>,
}

/// One system's contribution to a preview frame.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemSnapshot {
    pub name: String,
    pub particle_count: usize,
    pub records: Vec<ParticleRecord>,
}

/// The editor document: the meta system it loaded or created, the
/// injected RNG and the environment the preview ticks against.
pub struct ParticleEditor {
    pub meta: crate::particle::MetaParticleSystem,
    rng: BwRandom,
    env: ParticleEnv,
}

impl ParticleEditor {
    /// The port's analogue of the time-seeded global `BWRandom`
    /// singleton: a seed that makes a preview reproducible.
    pub fn new(seed: u32) -> Self {
        ParticleEditor {
            meta: crate::particle::MetaParticleSystem::new(),
            rng: BwRandom::new(seed),
            env: ParticleEnv::default(),
        }
    }

    /// Load a `.particle` document (the reference `PeModule` load path).
    pub fn load(&mut self, xml: &str, name: &str) -> bool {
        self.meta.load_str(xml, name)
    }

    /// Save the document (`MetaParticleSystem::save` + `pDS->save()`): the
    /// XML text a `load` (or the reference editor) reads back. `transient`
    /// keeps the cached bounding boxes instead of recalculating them. The
    /// host owns the file write, exactly like every other editor save.
    pub fn save(&mut self, transient: bool) -> String {
        let mut ds = resources::datasection::DataSection::new("root");
        self.meta.save(&mut ds, transient);
        ds.write_to_string()
    }

    /// Reset to an empty document (`OnNewDocument`).
    pub fn new_document(&mut self) {
        self.meta = crate::particle::MetaParticleSystem::new();
    }

    /// Re-seed the RNG (`BWRandom::init(seed)`).
    pub fn set_seed(&mut self, seed: u32) {
        self.rng = BwRandom::new(seed);
    }

    /// The environment the preview ticks against (the manager seam).
    pub fn env(&mut self) -> &mut ParticleEnv {
        &mut self.env
    }

    /// A frame of the preview: tick the meta system then flush the
    /// queued late updates, in the `PeModule` order.
    pub fn tick(&mut self, d_time: f32) -> bool {
        let updated = self.meta.tick(d_time, &mut self.rng, &self.env);
        for system in &mut self.meta.systems {
            system.flush_late_updates(&mut self.rng);
        }
        updated
    }

    /// Run `frames` preview ticks and record each one.
    pub fn preview(&mut self, frames: usize, d_time: f32) -> Vec<PreviewSnapshot> {
        let mut out = Vec::with_capacity(frames);
        for _ in 0..frames {
            self.tick(d_time);
            out.push(self.snapshot());
        }
        out
    }

    /// The current preview state, one record per live particle.
    pub fn snapshot(&self) -> PreviewSnapshot {
        let mut systems = Vec::new();
        for system in &self.meta.systems {
            let mut records = Vec::new();
            for p in system.particles.iter() {
                if p.is_alive() {
                    records.push(ParticleRecord {
                        position: p.position,
                        velocity: p.velocity(),
                        colour: p.colour,
                        size: p.size(),
                        age: p.age(),
                        age_raw: p.age_raw(),
                    });
                }
            }
            systems.push(SystemSnapshot {
                name: system.name.clone(),
                particle_count: records.len(),
                records,
            });
        }
        PreviewSnapshot { systems }
    }

    // ---- system faces (PsProperties panel) --------------------------------

    /// `PsProperties` capacity setter: `capacity_` clamps to 1..65536.
    pub fn set_capacity(&mut self, index: usize, value: i32) {
        if let Some(system) = self.meta.systems.get_mut(index) {
            system.set_capacity(value);
        }
    }

    /// `PsProperties` windFactor setter: `windFactor_` clamps to 0..1.
    pub fn set_wind_factor(&mut self, index: usize, value: f32) {
        if let Some(system) = self.meta.systems.get_mut(index) {
            system.wind_factor = foundation::math::clamp(0.0, value, 1.0);
        }
    }

    /// `PsProperties` maxLod setter.
    pub fn set_max_lod(&mut self, index: usize, value: f32) {
        if let Some(system) = self.meta.systems.get_mut(index) {
            system.max_lod = value;
        }
    }

    /// The renderer gizmo origin face.
    pub fn set_explicit_position(&mut self, index: usize, position: foundation::math::Vector3) {
        if let Some(system) = self.meta.systems.get_mut(index) {
            system.explicit_position = position;
        }
    }

    /// The renderer gizmo direction face.
    pub fn set_explicit_direction(&mut self, index: usize, direction: foundation::math::Vector3) {
        if let Some(system) = self.meta.systems.get_mut(index) {
            system.explicit_direction = direction;
        }
    }

    // ---- action faces ------------------------------------------------------

    /// `PsaProperties`/`ParticleSystemAction::enabled` face.
    pub fn set_action_enabled(&mut self, system_index: usize, action_index: usize, enabled: bool) {
        if let Some(system) = self.meta.systems.get_mut(system_index)
            && let Some(action) = system.actions.get_mut(action_index)
        {
            action.base.enabled = enabled;
        }
    }

    /// The base `delay_` face.
    pub fn set_action_delay(&mut self, system_index: usize, action_index: usize, delay: f32) {
        if let Some(system) = self.meta.systems.get_mut(system_index)
            && let Some(action) = system.actions.get_mut(action_index)
        {
            action.base.delay = delay;
        }
    }

    /// The base `minimumAge_` face.
    pub fn set_action_minimum_age(
        &mut self,
        system_index: usize,
        action_index: usize,
        minimum_age: f32,
    ) {
        if let Some(system) = self.meta.systems.get_mut(system_index)
            && let Some(action) = system.actions.get_mut(action_index)
        {
            action.base.minimum_age = minimum_age;
        }
    }

    /// A mutable reference to one action, for the panel setters.
    pub fn action_mut(
        &mut self,
        system_index: usize,
        action_index: usize,
    ) -> Option<&mut crate::particle::ParticleAction> {
        let systems = &mut self.meta.systems;
        systems.get_mut(system_index)?.actions.get_mut(action_index)
    }

    /// The Source panel's numeric faces (`PsaSourceProperties::SetParameters`).
    pub fn set_source(&mut self, system_index: usize, action_index: usize, s: &SourceFace) {
        if let Some(action) = self.action_mut(system_index, action_index) {
            let crate::particle::ActionKind::Source(src) = &mut action.kind else {
                return;
            };
            src.motion_triggered = s.motion_triggered;
            src.time_triggered = s.time_triggered;
            src.grounded = s.grounded;
            src.drop_distance = s.drop_distance;
            src.rate = s.rate;
            src.set_sensitivity(s.sensitivity);
            src.max_speed = s.max_speed;
            src.active_period = s.active_period;
            src.forced_unit_size = s.forced_unit_size;
            src.minimum_size = s.minimum_size;
            src.maximum_size = s.maximum_size;
            src.sleep_period = s.sleep_period;
            src.sleep_period_max = s.sleep_period_max;
            src.allowed_time_in_seconds = s.allowed_time_in_seconds;
            src.inherit_velocity = s.inherit_velocity;
        }
    }

    /// The Sink panel's faces.
    pub fn set_sink(&mut self, system_index: usize, action_index: usize, s: &SinkFace) {
        if let Some(action) = self.action_mut(system_index, action_index) {
            let crate::particle::ActionKind::Sink(sink) = &mut action.kind else {
                return;
            };
            sink.maximum_age = s.maximum_age;
            sink.minimum_speed = s.minimum_speed;
            sink.outside_only = s.outside_only;
        }
    }

    /// The Scaler panel's faces.
    pub fn set_scaler(&mut self, system_index: usize, action_index: usize, size: f32, rate: f32) {
        if let Some(action) = self.action_mut(system_index, action_index) {
            let crate::particle::ActionKind::Scaler(scaler) = &mut action.kind else {
                return;
            };
            scaler.size = size;
            scaler.rate = rate;
        }
    }

    /// The Stream panel's faces.
    pub fn set_stream(&mut self, system_index: usize, action_index: usize, half_life: f32) {
        if let Some(action) = self.action_mut(system_index, action_index) {
            let crate::particle::ActionKind::Stream(stream) = &mut action.kind else {
                return;
            };
            stream.half_life = half_life;
        }
    }

    /// The Magnet panel's faces.
    pub fn set_magnet(
        &mut self,
        system_index: usize,
        action_index: usize,
        strength: f32,
        min_dist: f32,
    ) {
        if let Some(action) = self.action_mut(system_index, action_index) {
            let crate::particle::ActionKind::Magnet(magnet) = &mut action.kind else {
                return;
            };
            magnet.strength = strength;
            magnet.min_dist = min_dist;
        }
    }

    /// The Orbitor panel's faces.
    pub fn set_orbitor(&mut self, system_index: usize, action_index: usize, angular_velocity: f32) {
        if let Some(action) = self.action_mut(system_index, action_index) {
            let crate::particle::ActionKind::Orbitor(orbitor) = &mut action.kind else {
                return;
            };
            orbitor.angular_velocity = angular_velocity;
        }
    }

    /// The Collide panel's faces.
    pub fn set_collide(&mut self, system_index: usize, action_index: usize, elasticity: f32) {
        if let Some(action) = self.action_mut(system_index, action_index) {
            let crate::particle::ActionKind::Collide(collide) = &mut action.kind else {
                return;
            };
            collide.elasticity = elasticity;
        }
    }

    /// The Flare panel's faces.
    pub fn set_flare(&mut self, system_index: usize, action_index: usize, flare_step: f32) {
        if let Some(action) = self.action_mut(system_index, action_index) {
            let crate::particle::ActionKind::Flare(flare) = &mut action.kind else {
                return;
            };
            flare.flare_step = flare_step;
        }
    }

    /// The TintShader panel's fog face.
    pub fn set_tint_fog(&mut self, system_index: usize, action_index: usize, fog_amount: f32) {
        if let Some(action) = self.action_mut(system_index, action_index) {
            let crate::particle::ActionKind::TintShader(tint) = &mut action.kind else {
                return;
            };
            tint.fog_amount = fog_amount;
        }
    }

    // ---- tint set faces (PsaTintShaderProperties) --------------------------

    /// `clearAllTints`.
    pub fn clear_tints(&mut self, system_index: usize, action_index: usize) {
        if let Some(action) = self.action_mut(system_index, action_index) {
            let crate::particle::ActionKind::TintShader(tint) = &mut action.kind else {
                return;
            };
            tint.clear_all_tints();
        }
    }

    /// `addTintAt`: the stops load sorted by time, the period grows to
    /// cover the new stop.
    pub fn add_tint(
        &mut self,
        system_index: usize,
        action_index: usize,
        time: f32,
        colour: foundation::math::Vector4,
    ) {
        if let Some(action) = self.action_mut(system_index, action_index) {
            let crate::particle::ActionKind::TintShader(tint) = &mut action.kind else {
                return;
            };
            tint.add_tint_at(Tint { time, colour });
        }
    }

    // ---- barrier faces (PsaBarrierProperties) ------------------------------

    /// The barrier shape/reaction faces: the reaction setters only set
    /// the enum (the geometry the dialog pairs with them comes from the
    /// shape faces).
    pub fn set_barrier_shape(
        &mut self,
        system_index: usize,
        action_index: usize,
        shape: BarrierShape,
    ) {
        if let Some(action) = self.action_mut(system_index, action_index) {
            let crate::particle::ActionKind::Barrier(barrier) = &mut action.kind else {
                return;
            };
            barrier.shape = shape;
        }
    }

    pub fn set_barrier_reaction(
        &mut self,
        system_index: usize,
        action_index: usize,
        reaction: BarrierReaction,
    ) {
        if let Some(action) = self.action_mut(system_index, action_index) {
            let crate::particle::ActionKind::Barrier(barrier) = &mut action.kind else {
                return;
            };
            barrier.reaction = reaction;
        }
    }

    /// `BarrierPSA::verticalCylinder(point, radius)`.
    pub fn set_barrier_cylinder(
        &mut self,
        system_index: usize,
        action_index: usize,
        point: foundation::math::Vector3,
        radius: f32,
    ) {
        if let Some(action) = self.action_mut(system_index, action_index) {
            let crate::particle::ActionKind::Barrier(barrier) = &mut action.kind else {
                return;
            };
            barrier.shape = BarrierShape::VerticalCylinder;
            barrier.reaction = BarrierReaction::Bounce;
            barrier.vec_a = point;
            barrier.radius = radius;
        }
    }

    /// `BarrierPSA::box(corner, oppositeCorner)`.
    pub fn set_barrier_box(
        &mut self,
        system_index: usize,
        action_index: usize,
        corner: foundation::math::Vector3,
        opposite: foundation::math::Vector3,
    ) {
        if let Some(action) = self.action_mut(system_index, action_index) {
            let crate::particle::ActionKind::Barrier(barrier) = &mut action.kind else {
                return;
            };
            barrier.shape = BarrierShape::Box;
            barrier.reaction = BarrierReaction::Bounce;
            barrier.vec_a = corner;
            barrier.vec_b = opposite;
        }
    }

    /// `BarrierPSA::sphere(centre, radius)`.
    pub fn set_barrier_sphere(
        &mut self,
        system_index: usize,
        action_index: usize,
        centre: foundation::math::Vector3,
        radius: f32,
    ) {
        if let Some(action) = self.action_mut(system_index, action_index) {
            let crate::particle::ActionKind::Barrier(barrier) = &mut action.kind else {
                return;
            };
            barrier.shape = BarrierShape::Sphere;
            barrier.reaction = BarrierReaction::Bounce;
            barrier.vec_a = centre;
            barrier.radius = radius;
        }
    }

    // ---- generator faces (vector generator custodian) -----------------------

    /// Swap a system's position or velocity generator.
    pub fn set_source_generator(
        &mut self,
        system_index: usize,
        action_index: usize,
        position: bool,
        generator: VectorGenerator,
    ) -> bool {
        let Some(action) = self.action_mut(system_index, action_index) else {
            return false;
        };
        let crate::particle::ActionKind::Source(src) = &mut action.kind else {
            return false;
        };
        if position {
            src.position_src = Some(generator);
        } else {
            src.velocity_src = Some(generator);
        }
        true
    }

    /// Swap a jitter generator pair.
    pub fn set_jitter_generators(
        &mut self,
        system_index: usize,
        action_index: usize,
        position: Option<VectorGenerator>,
        velocity: Option<VectorGenerator>,
    ) -> bool {
        let Some(action) = self.action_mut(system_index, action_index) else {
            return false;
        };
        let crate::particle::ActionKind::Jitter(jitter) = &mut action.kind else {
            return false;
        };
        if position.is_some() {
            jitter.position_src = position;
        }
        if velocity.is_some() {
            jitter.velocity_src = velocity;
        }
        true
    }

    /// The cylinder direction face rebuilds the orthonormal basis
    /// (`constructBasis`).
    pub fn set_cylinder_generator(
        &mut self,
        system_index: usize,
        action_index: usize,
        position: bool,
        generator: VectorGenerator,
    ) -> bool {
        let (origin, direction, min_radius, max_radius) = match generator {
            VectorGenerator::Cylinder {
                origin,
                direction,
                min_radius,
                max_radius,
                ..
            } => (origin, direction, min_radius, max_radius),
            _ => return false,
        };
        let (basis_u, basis_v) = VectorGenerator::cylinder_construct_basis(direction);
        let generator = VectorGenerator::Cylinder {
            origin,
            direction,
            min_radius,
            max_radius,
            basis_u,
            basis_v,
        };
        self.set_source_generator(system_index, action_index, position, generator)
    }

    // ---- renderer faces (PsRendererProperties) ------------------------------

    /// The sprite/point-sprite renderer panel's numeric faces.
    pub fn set_sprite(&mut self, system_index: usize, s: &SpriteFace) -> bool {
        let Some(system) = self.meta.systems.get_mut(system_index) else {
            return false;
        };
        let Some(renderer) = system.renderer.as_mut() else {
            return false;
        };
        match renderer {
            crate::particle::ParticleRenderer::Sprite { base, sprite } => {
                apply_sprite_face(base, sprite, s);
            }
            crate::particle::ParticleRenderer::PointSprite { base, sprite } => {
                apply_sprite_face(base, sprite, s);
            }
        }
        true
    }

    /// The local/world-dependent/view-dependent triple: the local
    /// renderer never follows the system transform, the view-dependent
    /// one follows the camera (a render seam), the default one is world
    /// dependent.
    pub fn set_renderer_space(
        &mut self,
        system_index: usize,
        local: bool,
        view_dependent: bool,
    ) -> bool {
        let Some(system) = self.meta.systems.get_mut(system_index) else {
            return false;
        };
        let Some(renderer) = system.renderer.as_mut() else {
            return false;
        };
        match renderer {
            crate::particle::ParticleRenderer::Sprite { base, .. }
            | crate::particle::ParticleRenderer::PointSprite { base, .. } => {
                base.local = local;
                base.view_dependent = view_dependent;
            }
        }
        true
    }

    // ---- document faces -----------------------------------------------------

    /// `PeModule::spawn` — force every source of the document.
    pub fn spawn(&mut self, num: i32) {
        self.meta.spawn(num);
    }

    /// `MetaParticleSystem::spawn` — force one system.
    pub fn spawn_system(&mut self, system_index: usize, num: i32) {
        if let Some(system) = self.meta.systems.get_mut(system_index) {
            system.spawn(num);
        }
    }

    /// `PeModule::clear` (via the doc).
    pub fn clear(&mut self) {
        self.meta.clear();
    }

    /// `MetaParticleSystem::clear` — one system.
    pub fn clear_system(&mut self, system_index: usize) {
        if let Some(system) = self.meta.systems.get_mut(system_index) {
            system.clear();
        }
    }

    /// The performance-pane faces (`PeModule::updateState`): the live
    /// particle count over the enabled systems and the byte footprint.
    pub fn stats(&self) -> (i32, usize) {
        (self.meta.size(), self.meta.size_in_bytes())
    }
}

fn apply_sprite_face(
    base: &mut crate::particle::RendererBase,
    sprite: &mut crate::particle::SpriteData,
    s: &SpriteFace,
) {
    base.view_dependent = s.view_dependent;
    base.local = s.local;
    sprite.material_fx = s.material_fx;
    sprite.frame_count = s.frame_count;
    sprite.frame_rate = s.frame_rate;
    sprite.explicit_orientation = s.explicit_orientation;
    sprite.texture_name = s.texture_name.clone();
    sprite.soft_depth_range = s.soft_depth_range;
    sprite.soft_falloff_power = s.soft_falloff_power;
    sprite.soft_depth_offset = s.soft_depth_offset;
    sprite.near_fade_cutoff = s.near_fade_cutoff;
    sprite.near_fade_start = s.near_fade_start;
    sprite.near_fade_falloff_power = s.near_fade_falloff_power;
}

/// The Source panel's control state (`psa_source_properties.cpp`
/// `SetParameters`), the fields that reach the engine.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SourceFace {
    pub motion_triggered: bool,
    pub time_triggered: bool,
    pub grounded: bool,
    pub drop_distance: f32,
    pub rate: f32,
    pub sensitivity: f32,
    pub max_speed: f32,
    pub active_period: f32,
    pub forced_unit_size: i32,
    pub minimum_size: f32,
    pub maximum_size: f32,
    pub sleep_period: f32,
    pub sleep_period_max: f32,
    pub allowed_time_in_seconds: f32,
    pub inherit_velocity: f32,
}

/// The Sink panel's control state.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SinkFace {
    pub maximum_age: f32,
    pub minimum_speed: f32,
    pub outside_only: bool,
}

/// The sprite renderer panel's control state
/// (`ps_renderer_properties.cpp`), the sprite-only fields.
#[derive(Debug, Clone, PartialEq)]
pub struct SpriteFace {
    pub local: bool,
    pub view_dependent: bool,
    pub material_fx: MaterialFX,
    pub texture_name: String,
    pub frame_count: i32,
    pub frame_rate: f32,
    pub explicit_orientation: foundation::math::Vector3,
    pub soft_depth_range: f32,
    pub soft_falloff_power: f32,
    pub soft_depth_offset: f32,
    pub near_fade_cutoff: f32,
    pub near_fade_start: f32,
    pub near_fade_falloff_power: f32,
}

impl Default for SpriteFace {
    fn default() -> Self {
        SpriteFace {
            local: false,
            view_dependent: false,
            material_fx: MaterialFX::Blended,
            texture_name: String::new(),
            frame_count: 0,
            frame_rate: 0.0,
            explicit_orientation: foundation::math::Vector3::ZERO,
            soft_depth_range: 0.0,
            soft_falloff_power: 1.0,
            soft_depth_offset: 0.0,
            near_fade_cutoff: 1.0,
            near_fade_start: 1.0,
            near_fade_falloff_power: 1.0,
        }
    }
}

/// `MaterialFX::from_int` (`sprite_particle_renderer.hpp`): the panel
/// casts the combo index back to the enum.
pub fn material_fx_from_index(index: i32) -> Option<MaterialFX> {
    match index {
        0 => Some(MaterialFX::Additive),
        1 => Some(MaterialFX::AdditiveAlpha),
        2 => Some(MaterialFX::Blended),
        3 => Some(MaterialFX::BlendedColour),
        4 => Some(MaterialFX::BlendedInverseColour),
        5 => Some(MaterialFX::Solid),
        6 => Some(MaterialFX::Shimmer),
        7 => Some(MaterialFX::SourceAlpha),
        _ => None,
    }
}

/// The capacity clamp of the source panel (`SetMinimum(1)`).
pub fn capacity_clamp(value: i32) -> i32 {
    value.clamp(1, MAX_CAPACITY)
}
