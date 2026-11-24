//! The post-processing chain and the camera projection effects
//! (BW-11-04-06).
//!
//! The reference lives in `lib/post_processing` — `Manager` drives a chain of
//! `Effect`s (`manager.cpp:55-108`), each effect a list of `Phase`s gated by a
//! `bypass` provider (`effect.cpp:113-215`), with two phase kinds registered
//! in this directory (`py_phase.cpp:97`, `py_copy_back_buffer.cpp:76`) fed by
//! filter quads (`py_filter_quad.cpp:116-235`). The camera half is
//! `ProjectionAccess` (`lib/camera/projection_access.cpp:44-167`), which ramps
//! the field of view over time and exposes the clip planes.
//!
//! The D3D device itself is not in the portable surface, so every draw lands
//! in an observable operation ledger ([`Device`]) instead of a back buffer —
//! the *order* of pushes, clears, passes and copies is what the reference
//! pins, and that order is preserved verbatim. Likewise the `.fx` bodies and
//! their technique pass counts are outside the surface, so a phase's material
//! carries a modelled pass count.
//!
//! Registered deviations, all forced by the surface above:
//! * device draws → ledger ops (no pixel output);
//! * `PyFilterQuad::addVert` clamps the tail tap group to the sample count —
//!   the reference indexes `rSample[t]` for four taps regardless and reads
//!   past the vector when the count is not a multiple of four;
//! * the reference's `filterQuadTexture` string-global override needs a
//!   string property type the ported effect surface does not have, so only
//!   the `inputTexture` convention is resolved;
//! * `PyPhase::load` guards the `filterQuad`/`material` children it opens —
//!   the reference dereferences null sections there;
//! * the per-vertex `viewNormal`/`worldNormal` fill in `addVert` needs the
//!   render context camera (`nearPlanePoint`), which the port does not model;
//! * the D3D timestamp-query `profile` path is skipped entirely.
//!
//! The reference bugs that are preserved on purpose:
//! * `Manager::setChain` builds its error count as `slen - effects_.size()`
//!   (`manager.cpp:162-166`), so `None` entries are reported as mismatches
//!   too;
//! * `ProjectionAccess::farPlane` only propagates into the chunk-space
//!   environment — the camera's own far plane is never written
//!   (`projection_access.cpp:131-141`).

use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

use foundation::math::vector::{Vector3, Vector4};
use resources::datasection::DataSection;

use crate::camera::CameraParams;
use crate::material::{EffectManager, EffectMaterial};
use crate::texture::TextureManager;

/// `almostEqual` (`lib/cstdmf/stdmf.hpp:606`): strictly less than the
/// epsilon, whose default is `0.0004f`.
fn almost_equal(f1: f32, f2: f32, epsilon: f32) -> bool {
    (f1 - f2).abs() < epsilon
}

/// The `Vector4Provider` face of the bypass input
/// (`pyscript/script_math.hpp:486`): only the frozen `Vector4Basic` is in
/// the load path.
pub trait Vector4Provider {
    /// `output`.
    fn output(&self) -> Vector4;
}

/// `Vector4Basic`: a constant provider.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vector4Basic(pub Vector4);

impl Vector4Provider for Vector4Basic {
    fn output(&self) -> Vector4 {
        self.0
    }
}

/// A debug viewport rectangle (`RECT`), signed so the empty-rect guards
/// (`debug.cpp:170-172`) keep their negative-difference behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }
}

/// One observable device operation. The ledger is the port's face for the
/// D3D device: the reference's push/clear/pass/copy sequence appears here in
/// the exact order the call tree produces it.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// `MRTSupport().bind()` / `unbind()` (`manager.cpp:67/98`).
    BindMrt,
    UnbindMrt,
    /// `pushRenderState(D3DRS_COLORWRITEENABLE)` — the pushed-then-set RGBA
    /// mask is folded into one op because the value is constant.
    PushColorWrite,
    PopColorWrite,
    /// A render target push/pop, carrying the resource id.
    PushRenderTarget(String),
    PopRenderTarget,
    /// `Clear(D3DCLEAR_TARGET, colour)`.
    Clear(u32),
    /// `PyCopyBackBuffer`'s `StretchRect(backBuffer -> rt, D3DTEXF_LINEAR)`.
    CopyBackBuffer(String, String),
    /// The viewport swaps around a debug-rect draw.
    SetViewport(Rect),
    RestoreViewport,
    /// `EffectMaterial::begin` succeeded.
    BeginMaterial,
    /// `mat->beginPass(i)` / `endPass()`.
    BeginPass(u32),
    EndPass(u32),
    /// `mat->end()`.
    EndMaterial,
    /// `PyFilterQuad::draw`: one `verts4tap_.drawEffect` over the vertices
    /// the last `preDraw` built.
    DrawFilterQuad(usize),
    /// `Debug::drawDisabledPhase`'s grey rect (`debug.cpp:109-126`).
    DisabledPhase(Rect),
    /// `Debug::copyPhaseOutput`'s `StretchRect` into the grid cell.
    CopyPhase(Rect),
}

/// The device face: an operation ledger plus the geometry the filter-quad
/// fixup needs (`Moo::rc().halfScreenWidth/Height`) and the render-target
/// stack the phases push and pop.
#[derive(Debug, Default)]
pub struct Device {
    /// Every operation, in call order.
    pub ops: Vec<Op>,
    /// `halfScreenWidth()`.
    pub half_screen_width: f32,
    /// `halfScreenHeight()`.
    pub half_screen_height: f32,
    /// The pushed render targets, innermost last.
    render_targets: Vec<String>,
}

impl Device {
    /// A device over the given screen geometry.
    pub fn new(half_screen_width: f32, half_screen_height: f32) -> Device {
        Device {
            ops: Vec::new(),
            half_screen_width,
            half_screen_height,
            render_targets: Vec::new(),
        }
    }

    fn op(&mut self, op: Op) {
        self.ops.push(op);
    }

    /// `PyRenderTarget::push` / `pop` (`py_phase.cpp:152-156, 198-201`).
    fn push_render_target(&mut self, resource_id: &str) {
        self.ops.push(Op::PushRenderTarget(resource_id.to_owned()));
        self.render_targets.push(resource_id.to_owned());
    }

    fn pop_render_target(&mut self) {
        self.render_targets.pop();
        self.ops.push(Op::PopRenderTarget);
    }
}

/// The `Moo::RenderTarget` face the phases hold: a resource id, the level-0
/// size and the `valid()` flag the debug preview branches on.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderTarget {
    /// `resourceID()`.
    pub resource_id: String,
    /// `width()`.
    pub width: u32,
    /// `height()`.
    pub height: u32,
    /// `valid()`.
    pub valid: bool,
}

impl RenderTarget {
    /// A valid target with the given geometry.
    pub fn new(resource_id: &str, width: u32, height: u32) -> RenderTarget {
        RenderTarget {
            resource_id: resource_id.to_owned(),
            width,
            height,
            valid: true,
        }
    }
}

/// The `TextureManager` render-target resolution face
/// (`py_phase.cpp:283-307`): a name lookup that only yields render targets.
#[derive(Debug, Default, Clone)]
pub struct RenderTargetStore {
    targets: Vec<RenderTarget>,
}

impl RenderTargetStore {
    pub fn new() -> RenderTargetStore {
        RenderTargetStore {
            targets: Vec::new(),
        }
    }

    /// Install a target under its resource id.
    pub fn install(&mut self, target: RenderTarget) {
        self.targets.push(target);
    }

    /// `renderTargetFromString`'s lookup half.
    pub fn get(&self, resource_id: &str) -> Option<RenderTarget> {
        self.targets
            .iter()
            .find(|target| target.resource_id == resource_id)
            .cloned()
    }
}

/// The load-time services a phase resolve needs. The reference reaches them
/// through global singletons (`TextureManager::instance()`, the scoped
/// resource context); the port threads them as one argument.
pub struct LoadContext<'a> {
    /// The compiled-effect cache `EffectMaterial::load` merges against.
    pub effects: &'a mut EffectManager,
    /// The texture cache the material's texture properties bind through.
    pub textures: &'a mut TextureManager,
    /// The mfm/inheritance resolver.
    pub open_section: &'a dyn Fn(&str) -> Option<DataSection>,
    /// The render-target name table.
    pub render_targets: &'a RenderTargetStore,
}

// ---------------------------------------------------------------------------
// Debug
// ---------------------------------------------------------------------------

/// `PostProcessing::Debug` (`debug.cpp:31-50`): the phase-output grid
/// recorder. Its render target is a checkerboard the chain's phases copy
/// their output into, one cell per effect/phase pair.
#[derive(Debug, Clone)]
pub struct Debug {
    /// `enabled_`, true from the constructor.
    pub enabled: bool,
    /// `pRT_`.
    pub render_target: Option<RenderTarget>,
    /// `nEffects_`.
    n_effects: i32,
    /// `nPhases_`.
    n_phases: i32,
    /// `effect_`, -1 until the first `beginEffect`.
    effect: i32,
    /// `phase_`.
    phase: i32,
}

impl Debug {
    /// The constructor (`debug.cpp:64-72`).
    pub fn new() -> Debug {
        Debug {
            enabled: true,
            render_target: None,
            n_effects: 0,
            n_phases: 0,
            effect: 0,
            phase: 0,
        }
    }

    /// `beginChain` (`debug.cpp:85-89`).
    pub fn begin_chain(&mut self, n_effects: usize) {
        self.n_effects = n_effects as i32;
        self.effect = -1;
    }

    /// `beginEffect` (`debug.cpp:97-102`).
    pub fn begin_effect(&mut self, n_phases: usize) {
        self.n_phases = n_phases as i32;
        self.phase = 0;
        self.effect += 1;
    }

    /// The `renderTarget` attribute.
    pub fn set_render_target(&mut self, render_target: Option<RenderTarget>) {
        self.render_target = render_target;
    }

    /// `nextPhase` (`debug.cpp:133-152`): the cell for the current
    /// effect/phase pair, then the phase index advances. The `top`/`bottom`
    /// rows flip because the cell coordinates are bottom-up while the
    /// rectangle is top-down.
    pub fn next_phase(&mut self) -> Rect {
        let (rt_width, rt_height) = match &self.render_target {
            Some(rt) => (rt.width as f32, rt.height as f32),
            // The reference dereferences `pRT_` unchecked here; the callers
            // all gate on it first, so the port carries the same gate.
            None => (0.0, 0.0),
        };
        let uvs = Self::phase_uv(
            self.effect as u32,
            self.phase as u32,
            self.n_effects as u32,
            self.n_phases as u32,
        );
        let rect = Rect {
            left: (uvs.x * rt_width) as i32,
            top: (rt_height - uvs.w * rt_height) as i32,
            right: (uvs.z * rt_width) as i32,
            bottom: (rt_height - uvs.y * rt_height) as i32,
        };
        self.phase += 1;
        rect
    }

    /// `drawDisabledPhase` (`debug.cpp:109-126`): a grey box where the phase
    /// would have been recorded.
    pub fn draw_disabled_phase(&mut self, device: &mut Device) {
        if self.render_target.is_some() {
            let rect = self.next_phase();
            device.op(Op::DisabledPhase(rect));
        }
    }

    /// `copyPhaseOutput` (`debug.cpp:155-180`): snapshot the phase's render
    /// target into the next grid cell. A `None` source means the back
    /// buffer.
    pub fn copy_phase_output(&mut self, source: Option<&RenderTarget>, device: &mut Device) {
        if self.render_target.is_none() {
            return;
        }
        // The very first cell clears the whole debug target first.
        if self.effect == 0 && self.phase == 0 {
            device.op(Op::Clear(0x00000000));
        }
        let rect = self.next_phase();
        if rect.right - rect.left <= 0 || rect.bottom - rect.top <= 0 {
            return;
        }
        let _ = source;
        device.op(Op::CopyPhase(rect));
    }

    /// `phaseUV` (`debug.cpp:203-228`): the cell for the given pair,
    /// packed as `( l, b, r, t )`. Both grids are square-ish
    /// (`ceil(sqrt(n))` across, `ceil(n/across)` down).
    pub fn phase_uv(effect: u32, phase: u32, n_effects: u32, n_phases: u32) -> Vector4 {
        // u,v how many effects to draw in the x,y directions
        let effect_u = (n_effects as f32).sqrt().ceil();
        let effect_v = (n_effects as f32 / effect_u).ceil();
        // effect grid size
        let effect_w = 1.0f32 / effect_u;
        let effect_h = 1.0f32 / effect_v;
        // effect location
        let effect_x = (effect as f32 % effect_u) * effect_w;
        let effect_y = (effect as f32 / effect_u).floor() * effect_h;

        // u,v how many phases to draw in the x,y directions within
        // our effect region
        let phase_u = (n_phases as f32).sqrt().ceil();
        let phase_v = (n_phases as f32 / phase_u).ceil();
        // phase grid size
        let phase_w = effect_w / phase_u;
        let phase_h = effect_h / phase_v;
        // phase location
        let phase_x = (phase as f32 % phase_u) * phase_w + effect_x;
        let phase_y = (phase as f32 / phase_u).floor() * phase_h + effect_y;

        Vector4::new(phase_x, phase_y, phase_x + phase_w, phase_y + phase_h)
    }
}

impl Default for Debug {
    fn default() -> Debug {
        Debug::new()
    }
}

// ---------------------------------------------------------------------------
// PyFilterQuad
// ---------------------------------------------------------------------------

/// `Moo::FourTapVertex` face: the quad-corner position plus up to four
/// `(u, v, weight)` taps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FourTapVertex {
    /// `pos_`, post-fixup.
    pub pos: [f32; 3],
    /// `uv_[t]` per tap.
    pub uv: [[f32; 3]; 4],
}

/// `PyFilterQuad` (`py_filter_quad.cpp`): a full-screen quad whose texture
/// coordinates come from the filter kernel. Taps are grouped four per
/// vertex; the material must be additive so the passes accumulate.
#[derive(Debug, Clone, Default)]
pub struct PyFilterQuad {
    /// `samples_`: `(u texel offset, v texel offset, weight)` triples.
    pub samples: Vec<Vector3>,
}

impl PyFilterQuad {
    pub fn new() -> PyFilterQuad {
        PyFilterQuad {
            samples: Vec::new(),
        }
    }

    /// `preDraw` (`py_filter_quad.cpp:116-192`): build the vertex list. Four
    /// corners with the half-pixel fixup, then six vertices per group of
    /// four taps — the two triangles `v[0],v[1],v[2]` and `v[0],v[2],v[3]`.
    /// The tap offsets are texel counts, so they divide by the source
    /// texture dimensions from the material's `inputTexture` property.
    pub fn pre_draw(
        &self,
        material: &PyMaterial,
        device: &mut Device,
        textures: &mut TextureManager,
    ) -> Vec<FourTapVertex> {
        let w = 1.0f32;
        let h = 1.0f32;
        let fixup = [
            -0.5 / device.half_screen_width,
            0.5 / device.half_screen_height,
            0.0,
        ];

        // The four quad corners (py_filter_quad.cpp:129-145).
        let mut v = [
            ([-1.0f32, -1.0, 0.1], [0.0f32, 0.0], [0.0f32, h]),
            ([-1.0, 1.0, 0.1], [0.0, 1.0], [0.0, 0.0]),
            ([1.0, 1.0, 0.1], [1.0, 1.0], [w, 0.0]),
            ([1.0, -1.0, 0.1], [1.0, 0.0], [w, h]),
        ];
        for corner in &mut v {
            corner.0[0] += fixup[0];
            corner.0[1] += fixup[1];
            corner.0[2] += fixup[2];
        }

        // The source dimensions for filter taps (py_filter_quad.cpp:147-172):
        // the .fx's main texture is named `inputTexture`. The reference's
        // `filterQuadTexture` string-global override needs a string property
        // type the ported effect surface lacks, so it is not modelled.
        let mut src_dim = [1.0f32, 1.0];
        if let Some(resource_id) = material.material.texture_property("inputTexture")
            && let Some(texture) = textures.get(&resource_id)
        {
            src_dim[0] = texture.width as f32;
            src_dim[1] = texture.height as f32;
        }

        // Six vertices per tap group; the tail group clamps the taps it
        // cannot read (the reference walks off the vector there).
        let mut verts = Vec::new();
        let tap_size = 4usize;
        let mut s = 0usize;
        while s < self.samples.len() {
            for corner in [0usize, 1, 2, 0, 2, 3] {
                let (pos, _, uv2) = &v[corner];
                let mut taps = [[0.0f32; 3]; 4];
                for (t, tap) in taps.iter_mut().enumerate() {
                    match self.samples.get(s + t) {
                        Some(fs) => {
                            *tap = [uv2[0] + fs.x / src_dim[0], uv2[1] + fs.y / src_dim[1], fs.z];
                        }
                        // Missing taps read as a zero-offset, zero-weight
                        // sample instead of out-of-bounds memory.
                        None => *tap = [uv2[0], uv2[1], 0.0],
                    }
                }
                verts.push(FourTapVertex {
                    pos: *pos,
                    uv: taps,
                });
            }
            s += tap_size;
        }
        verts
    }

    /// `draw` (`py_filter_quad.cpp:197-201`): one draw call over the
    /// vertices `preDraw` built.
    pub fn draw(verts: &[FourTapVertex], device: &mut Device) {
        device.op(Op::DrawFilterQuad(verts.len()));
    }

    /// `save` (`py_filter_quad.cpp:203-214`): one `filterTap` child per
    /// sample.
    pub fn save(&self, parent: &mut DataSection) {
        let section = parent.add_child("PyFilterQuad");
        for sample in &self.samples {
            let tap = section.add_child("filterTap");
            tap.set_vector3(*sample);
        }
    }

    /// `load` (`py_filter_quad.cpp:217-231`): every `filterTap` child as a
    /// vector3.
    pub fn load(&mut self, section: &DataSection) {
        self.samples.clear();
        for index in 0..section.count_children() {
            if let Some(child) = section.open_child(index)
                && child.section_name() == "filterTap"
            {
                self.samples.push(child.as_vector3(Vector3::ZERO));
            }
        }
    }
}

/// `FilterQuadFactory` (`filter_quad_factory.cpp`): resolves the
/// factory-labeled child of a `filterQuad` wrapper. Only `PyFilterQuad` is
/// ported; the transfer-mesh variants (`PyPointSpriteTransferMesh`,
/// `PyVisualTransferMesh`, `PyTransferQuad`) resolve to nothing.
pub struct FilterQuadFactory;

impl FilterQuadFactory {
    pub fn load_item(section: Option<&DataSection>) -> Option<PyFilterQuad> {
        let section = section?;
        match section.section_name() {
            "PyFilterQuad" => {
                let mut quad = PyFilterQuad::new();
                quad.load(section);
                Some(quad)
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Materials and phases
// ---------------------------------------------------------------------------

/// The `PyMaterial` wrapper (`client/py_material.cpp` face): the ported
/// `EffectMaterial` plus the technique's pass count, which lives in the
/// `.fx` body and is modelled here.
pub struct PyMaterial {
    /// `pMaterial_`.
    pub material: EffectMaterial,
    /// `numPasses()` of the current technique.
    pub passes: u32,
}

impl PyMaterial {
    /// A material over the given effect with one pass.
    pub fn new(material: EffectMaterial) -> PyMaterial {
        PyMaterial {
            material,
            passes: 1,
        }
    }

    /// Whether the effect's technique table contains `Preview`
    /// (`py_phase.cpp:203`).
    pub fn has_preview_technique(&self) -> bool {
        self.material
            .effect
            .as_ref()
            .is_some_and(|record| record.techniques.iter().any(|t| t == "Preview"))
    }
}

/// `PyPhase` (`py_phase.cpp`): a generic phase — one filter-quad draw with a
/// material, optionally into a render target.
pub struct PyPhase {
    /// `name_`, default `"phase"`.
    pub name: String,
    /// `pFilterQuad_`.
    pub filter_quad: Option<PyFilterQuad>,
    /// `pRenderTarget_`.
    pub render_target: Option<RenderTarget>,
    /// `clearRenderTarget_`.
    pub clear_render_target: bool,
    /// `pMaterial_`.
    pub material: Option<PyMaterial>,
}

impl PyPhase {
    /// The constructor (`py_phase.cpp:99-108`).
    pub fn new() -> PyPhase {
        PyPhase {
            name: "phase".to_owned(),
            filter_quad: None,
            render_target: None,
            clear_render_target: false,
            material: None,
        }
    }

    /// `renderTargetFromString` (`py_phase.cpp:283-307`): resolve the name
    /// through the store; a miss leaves the target unset.
    pub fn render_target_from_string(
        &mut self,
        resource_id: &str,
        store: &RenderTargetStore,
    ) -> bool {
        let mut ok = false;
        if !resource_id.is_empty() {
            match store.get(resource_id) {
                Some(rt) => {
                    self.render_target = Some(rt);
                    ok = true;
                }
                None => {
                    self.render_target = None;
                }
            }
        }
        ok
    }

    /// `draw` (`py_phase.cpp:133-224`). The gate, the render-target push (or
    /// the back-buffer cookie bump), the optional clear, the viewport swap
    /// for a debug rect, the material's pass loop, and the debug output —
    /// all in the reference's order.
    pub fn draw(
        &mut self,
        debug: Option<&mut Debug>,
        debug_rect: Option<Rect>,
        device: &mut Device,
        textures: &mut TextureManager,
    ) {
        if self.filter_quad.is_none() || self.material.is_none() {
            return;
        }

        let mut using_render_target = false;
        if let Some(rt) = self.render_target.clone() {
            device.push_render_target(&rt.resource_id);
            using_render_target = true;
        } else {
            // We are about to write to the back buffer, so increment its
            // cookie.
            PyCopyBackBuffer::increment_draw_cookie();
        }

        if self.clear_render_target && debug_rect.is_none() {
            device.op(Op::Clear(0x00000000));
        }

        if let Some(rect) = debug_rect {
            device.op(Op::SetViewport(rect));
        }

        if self
            .material
            .as_ref()
            .is_some_and(|material| material.material.begin())
        {
            device.op(Op::BeginMaterial);
            let material = self.material.as_ref().expect("checked");
            let verts = self
                .filter_quad
                .as_ref()
                .expect("checked")
                .pre_draw(material, device, textures);
            for pass in 0..material.passes {
                device.op(Op::BeginPass(pass));
                PyFilterQuad::draw(&verts, device);
                device.op(Op::EndPass(pass));
            }
            device.op(Op::EndMaterial);
        }

        if debug_rect.is_some() {
            device.op(Op::RestoreViewport);
        }

        if using_render_target {
            device.pop_render_target();
        }

        if let Some(debug) = debug {
            let rt_valid = self.render_target.as_ref().is_some_and(|rt| rt.valid);
            if rt_valid {
                if self
                    .material
                    .as_ref()
                    .is_some_and(PyMaterial::has_preview_technique)
                {
                    // The preview swap: draw this phase into the debug
                    // cell with the debug target as output. The technique
                    // switch itself is unobservable in the ported face.
                    let rect = debug.next_phase();
                    let saved = self.render_target.take();
                    self.render_target = debug.render_target.clone();
                    self.draw(None, Some(rect), device, textures);
                    self.render_target = saved;
                } else {
                    let rt = self.render_target.clone();
                    debug.copy_phase_output(rt.as_ref(), device);
                }
            } else {
                debug.copy_phase_output(None, device);
            }
        }
    }

    /// `load` (`py_phase.cpp:227-251`).
    pub fn load(&mut self, section: &DataSection, ctx: &mut LoadContext) -> bool {
        self.name = section.read_string("name", &self.name);
        self.clear_render_target = section
            .find_child("clearRenderTarget")
            .map_or(self.clear_render_target, |child| {
                child.as_bool(self.clear_render_target)
            });

        // A missing or failing material section leaves the material unset.
        if let Some(material_section) = section.find_child("material") {
            let mut material = EffectMaterial::new();
            if material.load(
                material_section,
                ctx.effects,
                ctx.textures,
                ctx.open_section,
            ) {
                self.material = Some(PyMaterial::new(material));
            }
        }

        let rt_name = section.read_string("renderTarget", "");
        if !rt_name.is_empty() {
            self.render_target_from_string(&rt_name, ctx.render_targets);
        }

        // The reference dereferences the missing-`filterQuad` null here; the
        // port leaves the quad unset instead.
        if let Some(wrapper) = section.find_child("filterQuad") {
            self.filter_quad = FilterQuadFactory::load_item(wrapper.open_child(0));
        }
        true
    }

    /// `save` (`py_phase.cpp:253-270`).
    pub fn save(&self, parent: &mut DataSection) {
        let section = parent.add_child("PyPhase");
        section.write_string("name", &self.name);
        if let Some(rt) = &self.render_target {
            section.write_string("renderTarget", &rt.resource_id);
        }
        let flag = section.add_child("clearRenderTarget");
        flag.set_bool(self.clear_render_target);
        let wrapper = section.add_child("filterQuad");
        if let Some(quad) = &self.filter_quad {
            quad.save(wrapper);
        }
        let wrapper = section.add_child("material");
        if let Some(material) = &self.material {
            let saved = material.material.save();
            wrapper.copy_from(&saved);
        }
    }
}

impl Default for PyPhase {
    fn default() -> PyPhase {
        PyPhase::new()
    }
}

/// `PyCopyBackBuffer` (`py_copy_back_buffer.cpp`): copies the back buffer
/// into a render target, at most once per frame, tracked by a static draw
/// cookie. The optimisation only works if the same instance is reused.
#[derive(Debug, Clone)]
pub struct PyCopyBackBuffer {
    /// `name_`, default `"copy back buffer"`.
    pub name: String,
    /// `pRenderTarget_`.
    pub render_target: Option<RenderTarget>,
    /// `drawCookie_`, the instance's view of the static counter.
    draw_cookie: u32,
}

/// `s_drawCookie_`: how many times the back buffer has been written since
/// the process started.
static S_DRAW_COOKIE: AtomicU32 = AtomicU32::new(0);

impl PyCopyBackBuffer {
    /// The constructor (`py_copy_back_buffer.cpp:79-85`): the instance
    /// starts one behind the static counter, so the first draw is always
    /// dirty.
    pub fn new() -> PyCopyBackBuffer {
        PyCopyBackBuffer {
            name: "copy back buffer".to_owned(),
            render_target: None,
            draw_cookie: S_DRAW_COOKIE.load(Ordering::Relaxed).wrapping_sub(1),
        }
    }

    /// `tick` (`py_copy_back_buffer.cpp:92-101`): the convenient spot to
    /// increment the static draw cookie.
    pub fn tick(&mut self) {
        Self::increment_draw_cookie();
    }

    /// `isDirty` (`py_copy_back_buffer.cpp:199-202`).
    pub fn is_dirty(&self) -> bool {
        S_DRAW_COOKIE.load(Ordering::Relaxed) != self.draw_cookie
    }

    /// `incrementDrawCookie`.
    pub fn increment_draw_cookie() {
        S_DRAW_COOKIE.fetch_add(1, Ordering::Relaxed);
    }

    /// `drawCookie`.
    pub fn draw_cookie() -> u32 {
        S_DRAW_COOKIE.load(Ordering::Relaxed)
    }

    /// `draw` (`py_copy_back_buffer.cpp:105-151`): the dirty check, the
    /// stretch, the debug half, and the instance cookie resync.
    pub fn draw(&mut self, debug: Option<&mut Debug>, device: &mut Device) {
        if self.is_dirty() {
            // The port always has a back buffer, so the reference's
            // could-not-get-the-surface error branch is unreachable.
            if let Some(rt) = &self.render_target {
                device.op(Op::CopyBackBuffer(
                    "back buffer".to_owned(),
                    rt.resource_id.clone(),
                ));
            }
        }

        if let Some(debug) = debug {
            if self.is_dirty() {
                debug.copy_phase_output(self.render_target.as_ref(), device);
            } else {
                debug.draw_disabled_phase(device);
            }
        }

        self.draw_cookie = Self::draw_cookie();
    }

    /// `load` (`py_copy_back_buffer.cpp:153-165`).
    pub fn load(&mut self, section: &DataSection, store: &RenderTargetStore) -> bool {
        self.name = section.read_string("name", &self.name);
        let rt_name = section.read_string("renderTarget", "");
        if !rt_name.is_empty() {
            match store.get(&rt_name) {
                Some(rt) => self.render_target = Some(rt),
                None => self.render_target = None,
            }
        }
        true
    }

    /// `save` (`py_copy_back_buffer.cpp:167-178`).
    pub fn save(&self, parent: &mut DataSection) {
        let section = parent.add_child("PyCopyBackBuffer");
        section.write_string("name", &self.name);
        if let Some(rt) = &self.render_target {
            section.write_string("renderTarget", &rt.resource_id);
        }
    }
}

impl Default for PyCopyBackBuffer {
    fn default() -> PyCopyBackBuffer {
        PyCopyBackBuffer::new()
    }
}

// ---------------------------------------------------------------------------
// Phase factory
// ---------------------------------------------------------------------------

/// The `Phase` face (`phase.hpp:42`): tick, draw, load, save — over the two
/// phase kinds registered in this directory.
pub enum Phase {
    /// `IMPLEMENT_PHASE(PyPhase, PyPhase)` (`py_phase.cpp:97`).
    PyPhase(PyPhase),
    /// `IMPLEMENT_PHASE(PyCopyBackBuffer, PyCopyBackBuffer)`
    /// (`py_copy_back_buffer.cpp:76`).
    CopyBackBuffer(PyCopyBackBuffer),
}

impl Phase {
    /// `tick`.
    pub fn tick(&mut self) {
        match self {
            // The reference ticks the material's animatable properties; the
            // ported effect surface has no animation face, so the port's
            // PyPhase tick is a no-op.
            Phase::PyPhase(_) => {}
            Phase::CopyBackBuffer(phase) => phase.tick(),
        }
    }

    /// `draw`.
    pub fn draw(
        &mut self,
        debug: Option<&mut Debug>,
        device: &mut Device,
        textures: &mut TextureManager,
    ) {
        match self {
            Phase::PyPhase(phase) => phase.draw(debug, None, device, textures),
            Phase::CopyBackBuffer(phase) => phase.draw(debug, device),
        }
    }

    /// `load`.
    pub fn load(&mut self, section: &DataSection, ctx: &mut LoadContext) -> bool {
        match self {
            Phase::PyPhase(phase) => phase.load(section, ctx),
            Phase::CopyBackBuffer(phase) => phase.load(section, ctx.render_targets),
        }
    }

    /// `save`.
    pub fn save(&self, parent: &mut DataSection) {
        match self {
            Phase::PyPhase(phase) => phase.save(parent),
            Phase::CopyBackBuffer(phase) => phase.save(parent),
        }
    }

    /// The factory label the section name resolves against.
    pub fn section_name(&self) -> &'static str {
        match self {
            Phase::PyPhase(_) => "PyPhase",
            Phase::CopyBackBuffer(_) => "PyCopyBackBuffer",
        }
    }
}

/// `PhaseFactory` (`phase_factory.cpp`): the section-name dispatch the
/// `IMPLEMENT_PHASE` labels registered.
pub struct PhaseFactory;

impl PhaseFactory {
    /// `loadItem`: construct by label, then load. An unknown label answers
    /// `None`, which `Effect::load` turns into its `Unknown phase type`
    /// error.
    pub fn load_item(section: &DataSection, ctx: &mut LoadContext) -> Option<Phase> {
        let mut phase = match section.section_name() {
            "PyPhase" => Phase::PyPhase(PyPhase::new()),
            "PyCopyBackBuffer" => Phase::CopyBackBuffer(PyCopyBackBuffer::new()),
            _ => return None,
        };
        phase.load(section, ctx);
        Some(phase)
    }
}

// ---------------------------------------------------------------------------
// Effect
// ---------------------------------------------------------------------------

/// `PostProcessing::Effect` (`effect.cpp`): a named list of phases with an
/// optional bypass provider. When the bypass `w` reaches zero the phases
/// stop drawing — the effect self-optimises.
#[derive(Default)]
pub struct Effect {
    /// `phases_`.
    pub phases: Vec<Phase>,
    /// `name_`, default empty.
    pub name: String,
    /// `bypass_`.
    pub bypass: Option<Rc<dyn Vector4Provider>>,
    /// The `ERROR_MSG` lines from loading (`Unknown phase type`).
    pub log: Vec<String>,
}

impl Effect {
    pub fn new() -> Effect {
        Effect {
            phases: Vec::new(),
            name: String::new(),
            bypass: None,
            log: Vec::new(),
        }
    }

    /// `tick` (`effect.cpp:90-103`): every phase ticks.
    pub fn tick(&mut self) {
        for phase in &mut self.phases {
            phase.tick();
        }
    }

    /// `draw` (`effect.cpp:108-143`): the bypass gate. A bypass whose `w`
    /// is almost zero (strictly within `0.0004`) skips every phase draw but
    /// still records a grey cell per phase.
    pub fn draw(
        &mut self,
        mut debug: Option<&mut Debug>,
        device: &mut Device,
        textures: &mut TextureManager,
    ) {
        if let Some(debug) = debug.as_deref_mut() {
            debug.begin_effect(self.phases.len());
        }

        let mut bypass = false;
        if let Some(provider) = &self.bypass {
            bypass = almost_equal(provider.output().w, 0.0, 0.0004);
        }

        if !bypass {
            for phase in &mut self.phases {
                phase.draw(debug.as_deref_mut(), device, textures);
            }
        } else if let Some(debug) = debug {
            for _ in 0..self.phases.len() {
                debug.draw_disabled_phase(device);
            }
        }
    }

    /// `load` (`effect.cpp:146-186`).
    pub fn load(&mut self, section: &DataSection, ctx: &mut LoadContext) -> bool {
        // `asString(name_)`: the own value, the current name kept when the
        // section carries none.
        let value = section.as_string();
        if !value.is_empty() {
            self.name = value.to_owned();
        }

        if let Some(bypass_section) = section.find_child("bypass") {
            // `asVector4()`: the own value of the section.
            let vector = bypass_section.as_vector4(Vector4::new(0.0, 0.0, 0.0, 0.0));
            self.bypass = Some(Rc::new(Vector4Basic(vector)));
        }

        self.phases.clear();
        for index in 0..section.count_children() {
            if let Some(child) = section.open_child(index) {
                // Don't try to load 'bypass' as a phase (must check by
                // name; packed sections return a different pointer here
                // than findChild does).
                if child.section_name() == "bypass" {
                    continue;
                }
                match PhaseFactory::load_item(child, ctx) {
                    Some(phase) => self.phases.push(phase),
                    None => {
                        self.log
                            .push(format!("Unknown phase type {}", child.section_name()));
                    }
                }
            }
        }
        true
    }

    /// `save` (`effect.cpp:189-210`): a new `Effect` child on the parent,
    /// the name as the own value, the bypass frozen to its output, each
    /// phase saving its own labelled section.
    pub fn save(&self, parent: &mut DataSection) {
        let section = parent.add_child("Effect");
        section.set_string(&self.name);
        if let Some(provider) = &self.bypass {
            // Note - this freezes the bypass, thus on load it becomes a
            // static value representing the value it was at save time.
            // `writeVector4` puts the value on the section itself.
            let child = section.add_child("bypass");
            child.set_vector4(provider.output());
        }
        for phase in &self.phases {
            phase.save(section);
        }
    }
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

/// One slot of a `PostProcessing.chain` argument: an effect, the `None`
/// placeholder, a non-effect object, or a nested sequence.
pub enum ChainItem {
    /// An `Effect`.
    Effect(Effect),
    /// `Py_None`: counted in the chain length but not stored.
    Null,
    /// Any non-`Effect` object.
    Foreign,
    /// A nested sequence (the singleton-unwrap input).
    Sequence(Vec<ChainItem>),
}

/// `PostProcessing::Manager` (`manager.cpp`): the effect chain singleton.
pub struct Manager {
    /// `effects_`: only the pushed effects — `None` entries are not stored.
    pub effects: Vec<Effect>,
    /// `debug_`.
    pub debug: Option<Debug>,
}

impl Manager {
    pub fn new() -> Manager {
        Manager {
            effects: Vec::new(),
            debug: None,
        }
    }

    /// `tick` (`manager.cpp:44-57`).
    pub fn tick(&mut self) {
        for effect in &mut self.effects {
            effect.tick();
        }
    }

    /// `draw` (`manager.cpp:63-99`): bind the MRT, force the colour-write
    /// mask to RGBA, open the debug grid, draw the chain, restore.
    pub fn draw(&mut self, device: &mut Device, textures: &mut TextureManager) {
        device.op(Op::BindMrt);
        device.op(Op::PushColorWrite);

        // The debug recorder is lent to each effect in turn; it is taken
        // out so the chain loop can borrow the effects alongside it.
        let mut debug = self.debug.take();
        if let Some(debug) = debug.as_mut() {
            debug.begin_chain(self.effects.len());
        }

        for effect in &mut self.effects {
            effect.draw(debug.as_mut(), device, textures);
        }
        self.debug = debug;

        device.op(Op::PopColorWrite);
        device.op(Op::UnbindMrt);
    }

    /// `setChain` (`manager.cpp:120-172`): a single sequence argument is
    /// unwrapped, `None` slots count toward the length but are not stored,
    /// and the mismatch count is the reference's `slen - effects_.size()` —
    /// which reports `None` slots as mismatches too.
    pub fn set_chain(&mut self, args: Vec<ChainItem>) -> Result<(), String> {
        // Check to see if our one and only argument is a sequence.
        let mut slen = args.len();
        let mut seq = args;
        if slen == 1 && matches!(seq[0], ChainItem::Sequence(_)) {
            // Check to see if our one and only argument is a sequence.
            if let ChainItem::Sequence(inner) = seq.pop().expect("one item") {
                slen = inner.len();
                seq = inner;
            }
        }

        let mut n_effects = 0usize;
        self.effects.clear();

        for item in seq {
            match item {
                ChainItem::Effect(effect) => {
                    self.effects.push(effect);
                    n_effects += 1;
                }
                ChainItem::Null => n_effects += 1,
                ChainItem::Foreign => {}
                ChainItem::Sequence(_) => {}
            }
        }

        if slen > n_effects {
            return Err(format!(
                "PostProcessing.chain expects a sequence of Effects.  {} items\
                 \x20in the sequence did not match this type",
                slen - self.effects.len()
            ));
        }
        Ok(())
    }

    /// `load` (`manager.cpp:py_load`): every `Effect` sibling section in the
    /// file section is loaded into the chain.
    pub fn load(&mut self, section: &DataSection, ctx: &mut LoadContext) {
        for index in 0..section.count_children() {
            if let Some(child) = section.open_child(index) {
                if child.section_name() != "Effect" {
                    continue;
                }
                let mut effect = Effect::new();
                if effect.load(child, ctx) {
                    self.effects.push(effect);
                }
            }
        }
    }

    /// `save` (`manager.cpp:py_save`): the children go, then the chain
    /// writes back into the section. (The reference's final file write is
    /// outside the surface.)
    pub fn save(&self, section: &mut DataSection) {
        section.del_children();
        for effect in &self.effects {
            effect.save(section);
        }
    }
}

impl Default for Manager {
    fn default() -> Manager {
        Manager::new()
    }
}

// ---------------------------------------------------------------------------
// ProjectionAccess — the camera half
// ---------------------------------------------------------------------------

/// `ProjectionAccess` (`lib/camera/projection_access.cpp`): "Update any
/// timed effects on the camera" — the smooth FOV ramp plus script access to
/// the clip planes. The reference drives the global
/// `Moo::rc().camera()`; the port owns its [`CameraParams`] model.
pub struct ProjectionAccess {
    /// The projection this access drives.
    pub camera: CameraParams,
    /// `smoothFovTransition_`.
    smooth_fov_transition: bool,
    /// `fovTransitionStart_`.
    fov_transition_start: f32,
    /// `fovTransitionEnd_`.
    fov_transition_end: f32,
    /// `fovTransitionTime_`.
    fov_transition_time: f32,
    /// `fovTransitionTimeElapsed_`.
    fov_transition_time_elapsed: f32,
    /// The chunk-space environment far plane. The reference's `farPlane`
    /// setter propagates only here — the camera's own far plane is never
    /// written (`projection_access.cpp:131-141`).
    pub enviro_far_plane: Option<f32>,
}

impl ProjectionAccess {
    /// The constructor (`projection_access.cpp:31-43`).
    pub fn new(camera: CameraParams) -> ProjectionAccess {
        ProjectionAccess {
            camera,
            smooth_fov_transition: false,
            fov_transition_start: 0.0,
            fov_transition_end: 0.0,
            fov_transition_time: 0.0,
            fov_transition_time_elapsed: 0.0,
            enviro_far_plane: None,
        }
    }

    /// `update` (`projection_access.cpp:49-76`): advance the ramp. All
    /// angles are radians.
    pub fn update(&mut self, d_time: f32) {
        if !self.smooth_fov_transition {
            return;
        }
        let new_fov;
        self.fov_transition_time_elapsed += d_time;
        if self.fov_transition_time_elapsed < self.fov_transition_time {
            let percent = self.fov_transition_time_elapsed / self.fov_transition_time;
            new_fov = self.fov_transition_start
                + percent * (self.fov_transition_end - self.fov_transition_start);
        } else {
            new_fov = self.fov_transition_end;
            self.smooth_fov_transition = false;
        }
        self.camera.fov = new_fov;
    }

    /// `rampFov` (`projection_access.cpp:89-99`): start ramping from the
    /// current fov to the given one over the given time, linearly.
    pub fn ramp_fov(&mut self, new_fov: f32, time_allowed: f32) {
        self.smooth_fov_transition = true;
        self.fov_transition_start = self.camera.fov;
        self.fov_transition_end = new_fov;
        self.fov_transition_time = time_allowed;
        self.fov_transition_time_elapsed = 0.0;
    }

    /// The `nearPlane` attribute, onto the camera.
    pub fn near_plane(&self) -> f32 {
        self.camera.near_plane
    }

    pub fn set_near_plane(&mut self, value: f32) {
        self.camera.near_plane = value;
    }

    /// The `farPlane` getter reads the camera.
    pub fn far_plane(&self) -> f32 {
        self.camera.far_plane
    }

    /// The `farPlane` setter: the reference propagates into the chunk
    /// space's environment and never touches the camera's far plane.
    pub fn set_far_plane(&mut self, value: f32) {
        self.enviro_far_plane = Some(value);
    }

    /// The `fov` attribute.
    pub fn fov(&self) -> f32 {
        self.camera.fov
    }

    /// The `fov` setter: a direct set cancels any smooth transition.
    pub fn set_fov(&mut self, value: f32) {
        self.smooth_fov_transition = false;
        self.camera.fov = value;
    }

    /// Whether a smooth transition is running (the private flag, exposed
    /// for tests).
    pub fn is_smooth_fov_transition(&self) -> bool {
        self.smooth_fov_transition
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::collections::HashMap;

    /// An uncompressed 32-bit top-down TGA of the given size, two pixel
    /// bytes per channel slot (BGRA, all distinct per pixel).
    fn tga(width: u32, height: u32) -> Vec<u8> {
        let mut raw = vec![0u8; 18 + (width * height * 4) as usize];
        raw[2] = 2; // uncompressed true-colour
        raw[12..14].copy_from_slice(&(width as u16).to_le_bytes());
        raw[14..16].copy_from_slice(&(height as u16).to_le_bytes());
        raw[16] = 32;
        raw[17] = 0x20; // top-down
        for (i, byte) in raw[18..].iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }
        raw
    }

    /// A manager carrying one compiled effect with the given parameters and
    /// techniques.
    fn effects_with(resource_id: &str, techniques: &[&str], parameters: &[&str]) -> EffectManager {
        let mut effects = EffectManager::new();
        effects.install(crate::material::EffectRecord::new(
            resource_id,
            techniques,
            parameters,
            BTreeMap::new(),
        ));
        effects
    }

    /// The empty resolver most loads carry: no mfm bases resolve.
    fn no_sections(_: &str) -> Option<DataSection> {
        None
    }

    /// A material section binding one texture property.
    fn material_section(fx: &str, texture_name: &str) -> DataSection {
        let mut section = DataSection::new("material");
        section.write_string("fx", fx);
        // An inheritance stub: the resolver the context carries is
        // consulted for this path on every load.
        section.write_string("mfm", "post/base.mfm");
        let property = section.add_child("property");
        property.set_string("inputTexture");
        property.add_child("Texture").set_string(texture_name);
        section
    }

    fn quad_section(samples: &[(f32, f32, f32)]) -> DataSection {
        let mut wrapper = DataSection::new("filterQuad");
        let mut quad = DataSection::new("PyFilterQuad");
        for (u, v, weight) in samples {
            let tap = quad.add_child("filterTap");
            tap.set_vector3(Vector3::new(*u, *v, *weight));
        }
        wrapper.add_child("PyFilterQuad").copy_from(&quad);
        wrapper
    }

    /// Four taps with texel offsets that divide visibly by a 4x2 source.
    const TAPS: [(f32, f32, f32); 4] = [
        (2.0, 4.0, 0.5),
        (-2.0, -4.0, 0.25),
        (0.0, 0.0, 1.0),
        (1.0, 1.0, 0.125),
    ];

    fn tap_material() -> (EffectManager, TextureManager, DataSection) {
        let effects = effects_with("post/fx", &[], &["inputTexture"]);
        let mut textures = TextureManager::new();
        textures.add_file("post/input.tga", &tga(4, 2));
        (
            effects,
            textures,
            material_section("post/fx", "post/input.tga"),
        )
    }

    // ----- Debug -----

    #[test]
    fn phase_uv_grids_square_off_both_axes() {
        // One effect, two phases: an effect cell of the whole area, a phase
        // grid of ceil(sqrt(2)) = 2 across, 1 down.
        let a = Debug::phase_uv(0, 0, 1, 2);
        assert_eq!(a, Vector4::new(0.0, 0.0, 0.5, 1.0));
        let b = Debug::phase_uv(0, 1, 1, 2);
        assert_eq!(b, Vector4::new(0.5, 0.0, 1.0, 1.0));

        // Three effects: a 2x2 effect grid with the last in the second row.
        assert_eq!(
            Debug::phase_uv(0, 0, 3, 1),
            Vector4::new(0.0, 0.0, 0.5, 0.5)
        );
        assert_eq!(
            Debug::phase_uv(1, 0, 3, 1),
            Vector4::new(0.5, 0.0, 1.0, 0.5)
        );
        assert_eq!(
            Debug::phase_uv(2, 0, 3, 1),
            Vector4::new(0.0, 0.5, 0.5, 1.0)
        );
    }

    #[test]
    fn next_phase_rows_flip_and_the_index_advances() {
        let mut debug = Debug::new();
        debug.set_render_target(Some(RenderTarget::new("debug/rt", 100, 50)));
        debug.begin_chain(1);
        debug.begin_effect(2);
        // phase 0: uvs (0, 0, 0.5, 1) -> top = 50 - 1*50, bottom = 50 - 0.
        assert_eq!(debug.next_phase(), Rect::new(0, 0, 50, 50));
        // phase 1: uvs (0.5, 0, 1, 1).
        assert_eq!(debug.next_phase(), Rect::new(50, 0, 100, 50));
    }

    #[test]
    fn copy_phase_output_clears_only_the_first_cell_and_skips_empty_cells() {
        let mut debug = Debug::new();
        debug.set_render_target(Some(RenderTarget::new("debug/rt", 64, 32)));
        debug.begin_chain(1);
        debug.begin_effect(4);
        let mut device = Device::new(400.0, 300.0);

        // Four phases make a 2x2 grid, so each cell is half a row tall:
        // cell (0,0) is uvs (0, 0, 0.5, 0.5), and the whole-target clear
        // precedes the copy.
        debug.copy_phase_output(None, &mut device);
        assert_eq!(
            device.ops[..2],
            [
                Op::Clear(0x00000000),
                Op::CopyPhase(Rect::new(0, 16, 32, 32))
            ]
        );

        // Later cells do not clear.
        debug.copy_phase_output(None, &mut device);
        assert_eq!(device.ops[2], Op::CopyPhase(Rect::new(32, 16, 64, 32)));

        // A one-by-one target yields zero-width cells: the copy is skipped
        // but the cell is still consumed.
        let mut tiny = Debug::new();
        tiny.set_render_target(Some(RenderTarget::new("debug/rt", 1, 1)));
        tiny.begin_chain(1);
        tiny.begin_effect(4);
        let mut device = Device::new(400.0, 300.0);
        tiny.copy_phase_output(None, &mut device);
        assert_eq!(device.ops, vec![Op::Clear(0x00000000)]);
    }

    #[test]
    fn copy_phase_output_needs_a_render_target() {
        let mut debug = Debug::new();
        let mut device = Device::new(400.0, 300.0);
        debug.copy_phase_output(None, &mut device);
        assert!(device.ops.is_empty());
    }

    #[test]
    fn draw_disabled_phase_greys_the_cell_and_consumes_it() {
        let mut debug = Debug::new();
        debug.set_render_target(Some(RenderTarget::new("debug/rt", 64, 32)));
        debug.begin_chain(1);
        debug.begin_effect(2);
        let mut device = Device::new(400.0, 300.0);
        debug.draw_disabled_phase(&mut device);
        debug.draw_disabled_phase(&mut device);
        assert_eq!(
            device.ops,
            vec![
                Op::DisabledPhase(Rect::new(0, 0, 32, 32)),
                Op::DisabledPhase(Rect::new(32, 0, 64, 32)),
            ]
        );
    }

    // ----- PyFilterQuad -----

    #[test]
    fn filter_quad_builds_two_triangles_per_group_of_four_taps() {
        let (mut effects, mut textures, material_section) = tap_material();
        let mut material = EffectMaterial::new();
        let no_resolver = |_: &str| -> Option<DataSection> { None };
        assert!(material.load(&material_section, &mut effects, &mut textures, &no_resolver));
        let material = PyMaterial::new(material);

        let mut quad = PyFilterQuad::new();
        quad.samples = TAPS
            .iter()
            .map(|(u, v, w)| Vector3::new(*u, *v, *w))
            .collect();
        let mut device = Device::new(400.0, 300.0);
        let verts = quad.pre_draw(&material, &mut device, &mut textures);
        assert_eq!(verts.len(), 6);

        // srcDim (4, 2): texel offsets divide down to uv deltas.
        let offsets = [(0.5, 2.0), (-0.5, -2.0), (0.0, 0.0), (0.25, 0.5)];
        let weights = [0.5, 0.25, 1.0, 0.125];
        // Corner uv2 origins: v0 (0,h), v1 (0,0), v2 (w,0), v3 (w,h).
        let uv2 = [[0.0f32, 1.0f32], [0.0, 0.0], [1.0, 0.0], [1.0, 1.0]];
        let expected_corners = [0usize, 1, 2, 0, 2, 3];
        for (i, vert) in verts.iter().enumerate() {
            let corner = expected_corners[i];
            for (t, tap) in vert.uv.iter().enumerate() {
                let (du, dv) = offsets[t];
                let want = [uv2[corner][0] + du, uv2[corner][1] + dv, weights[t]];
                assert_eq!(*tap, want, "vert {i} corner {corner} tap {t}");
            }
        }

        // The half-pixel fixup with half-screen 400x300: (-0.00125, +1/600).
        assert_eq!(verts[0].pos, [-1.0 - 0.00125, -1.0 + 1.0 / 600.0, 0.1]);
        assert_eq!(verts[1].pos, [-1.0 - 0.00125, 1.0 + 1.0 / 600.0, 0.1]);
        assert_eq!(verts[5].pos, [1.0 - 0.00125, -1.0 + 1.0 / 600.0, 0.1]);
    }

    #[test]
    fn filter_quad_tail_group_reads_zero_taps() {
        let (mut effects, mut textures, material_section) = tap_material();
        let mut material = EffectMaterial::new();
        let no_resolver = |_: &str| -> Option<DataSection> { None };
        assert!(material.load(&material_section, &mut effects, &mut textures, &no_resolver));
        let material = PyMaterial::new(material);

        let mut quad = PyFilterQuad::new();
        quad.samples = vec![Vector3::new(1.0, 1.0, 0.125)];
        let mut device = Device::new(400.0, 300.0);
        let verts = quad.pre_draw(&material, &mut device, &mut textures);
        assert_eq!(verts.len(), 6);
        // The single real tap lands on corner v0's tap 0; taps 1..3 are the
        // zero-fill (v0's uv2 origin).
        assert_eq!(verts[0].uv[0], [0.25, 1.5, 0.125]);
        assert_eq!(verts[0].uv[1], [0.0, 1.0, 0.0]);
        // v1's zero-fill uses its own uv2 origin (0,0).
        assert_eq!(verts[1].uv[1], [0.0, 0.0, 0.0]);
    }

    #[test]
    fn filter_quad_save_and_load_round_trip() {
        let mut quad = PyFilterQuad::new();
        quad.samples = TAPS
            .iter()
            .map(|(u, v, w)| Vector3::new(*u, *v, *w))
            .collect();
        let mut parent = DataSection::new("filterQuad");
        quad.save(&mut parent);
        let mut loaded = PyFilterQuad::new();
        let wrapper = parent.find_child("PyFilterQuad").expect("wrapper");
        loaded.load(wrapper);
        assert_eq!(loaded.samples, quad.samples);
        // Five taps reload as five, not four.
        quad.samples.push(Vector3::new(9.0, 9.0, 9.0));
        let mut parent = DataSection::new("filterQuad");
        quad.save(&mut parent);
        let mut loaded = PyFilterQuad::new();
        loaded.load(parent.find_child("PyFilterQuad").expect("wrapper"));
        assert_eq!(loaded.samples.len(), 5);
    }

    // ----- PyPhase -----

    fn tap_phase(
        rt: Option<RenderTarget>,
        clear: bool,
        techniques: &[&str],
    ) -> (PyPhase, EffectManager, TextureManager) {
        let mut phase = PyPhase::new();
        phase.name = "tap".to_owned();
        phase.render_target = rt;
        phase.clear_render_target = clear;
        phase.filter_quad = Some(PyFilterQuad {
            samples: TAPS
                .iter()
                .map(|(u, v, w)| Vector3::new(*u, *v, *w))
                .collect(),
        });
        let mut effects = effects_with("post/fx", techniques, &["inputTexture"]);
        let mut textures = TextureManager::new();
        textures.add_file("post/input.tga", &tga(4, 2));
        let section = material_section("post/fx", "post/input.tga");
        let mut material = EffectMaterial::new();
        let no_resolver = |_: &str| -> Option<DataSection> { None };
        assert!(material.load(&section, &mut effects, &mut textures, &no_resolver));
        phase.material = Some(PyMaterial::new(material));
        (phase, effects, textures)
    }

    #[test]
    fn py_phase_gates_on_the_quad_and_the_material() {
        let mut device = Device::new(400.0, 300.0);
        let mut textures = TextureManager::new();

        let mut no_quad = PyPhase::new();
        no_quad.material = Some(PyMaterial::new(EffectMaterial::new()));
        no_quad.draw(None, None, &mut device, &mut textures);
        assert!(device.ops.is_empty());

        let mut no_material = PyPhase::new();
        no_material.filter_quad = Some(PyFilterQuad::new());
        no_material.draw(None, None, &mut device, &mut textures);
        assert!(device.ops.is_empty());
    }

    #[test]
    fn py_phase_pushes_clears_and_runs_the_passes_in_order() {
        let (mut phase, _effects, mut textures) =
            tap_phase(Some(RenderTarget::new("phase/rt_b", 64, 32)), true, &[]);
        let mut device = Device::new(400.0, 300.0);
        phase.draw(None, None, &mut device, &mut textures);
        assert_eq!(
            device.ops,
            vec![
                Op::PushRenderTarget("phase/rt_b".to_owned()),
                Op::Clear(0x00000000),
                Op::BeginMaterial,
                Op::BeginPass(0),
                Op::DrawFilterQuad(6),
                Op::EndPass(0),
                Op::EndMaterial,
                Op::PopRenderTarget,
            ]
        );
    }

    #[test]
    fn py_phase_draws_the_preview_technique_into_the_debug_cell() {
        let (mut phase, _effects, mut textures) = tap_phase(
            Some(RenderTarget::new("phase/rt_b", 64, 32)),
            true,
            &["Preview"],
        );
        let mut device = Device::new(400.0, 300.0);
        let mut debug = Debug::new();
        debug.set_render_target(Some(RenderTarget::new("debug/rt", 64, 32)));
        debug.begin_chain(1);
        debug.begin_effect(1);
        phase.draw(Some(&mut debug), None, &mut device, &mut textures);

        // The main draw, then the preview recursion into cell (0,0) of the
        // debug target: the recursion pushes the debug RT, swaps in the
        // cell viewport, and never clears (a debug rect is set).
        assert_eq!(
            device.ops,
            vec![
                Op::PushRenderTarget("phase/rt_b".to_owned()),
                Op::Clear(0x00000000),
                Op::BeginMaterial,
                Op::BeginPass(0),
                Op::DrawFilterQuad(6),
                Op::EndPass(0),
                Op::EndMaterial,
                Op::PopRenderTarget,
                Op::PushRenderTarget("debug/rt".to_owned()),
                Op::SetViewport(Rect::new(0, 0, 64, 32)),
                Op::BeginMaterial,
                Op::BeginPass(0),
                Op::DrawFilterQuad(6),
                Op::EndPass(0),
                Op::EndMaterial,
                Op::RestoreViewport,
                Op::PopRenderTarget,
            ]
        );
    }

    #[test]
    fn py_phase_without_a_preview_copies_the_phase_output() {
        let (mut phase, _effects, mut textures) =
            tap_phase(Some(RenderTarget::new("phase/rt_b", 64, 32)), false, &[]);
        let mut device = Device::new(400.0, 300.0);
        let mut debug = Debug::new();
        debug.set_render_target(Some(RenderTarget::new("debug/rt", 64, 32)));
        debug.begin_chain(1);
        debug.begin_effect(2);
        phase.draw(Some(&mut debug), None, &mut device, &mut textures);
        // The main draw lands, then the copy into the first cell of the
        // two-across grid, with the one whole-target clear of the first
        // cell.
        assert_eq!(device.ops[6], Op::PopRenderTarget);
        assert_eq!(
            device.ops[7..],
            [
                Op::Clear(0x00000000),
                Op::CopyPhase(Rect::new(0, 0, 32, 32)),
            ]
        );
    }

    #[test]
    fn py_phase_writing_the_back_buffer_bumps_the_cookie() {
        let (mut phase, _effects, mut textures) = tap_phase(None, false, &[]);
        let mut device = Device::new(400.0, 300.0);
        let before = PyCopyBackBuffer::draw_cookie();
        phase.draw(None, None, &mut device, &mut textures);
        // No render-target push/clear pair; the cookie took the write.
        assert_eq!(PyCopyBackBuffer::draw_cookie(), before + 1);
        assert!(
            !device
                .ops
                .iter()
                .any(|op| matches!(op, Op::PushRenderTarget(_)))
        );
    }

    #[test]
    fn py_phase_clears_only_without_a_debug_rect() {
        let (mut phase, _effects, mut textures) =
            tap_phase(Some(RenderTarget::new("phase/rt_b", 64, 32)), true, &[]);
        let mut device = Device::new(400.0, 300.0);
        let mut debug = Debug::new();
        debug.set_render_target(Some(RenderTarget::new("debug/rt", 64, 32)));
        debug.begin_chain(1);
        debug.begin_effect(1);
        phase.draw(
            Some(&mut debug),
            Some(Rect::new(0, 0, 64, 32)),
            &mut device,
            &mut textures,
        );
        // The phase's own clear is suppressed under a debug rect: the only
        // clear is the debug copy's, and it lands after the pop.
        assert_eq!(
            device.ops,
            vec![
                Op::PushRenderTarget("phase/rt_b".to_owned()),
                Op::SetViewport(Rect::new(0, 0, 64, 32)),
                Op::BeginMaterial,
                Op::BeginPass(0),
                Op::DrawFilterQuad(6),
                Op::EndPass(0),
                Op::EndMaterial,
                Op::RestoreViewport,
                Op::PopRenderTarget,
                Op::Clear(0x00000000),
                Op::CopyPhase(Rect::new(0, 0, 64, 32)),
            ]
        );
    }

    #[test]
    fn py_phase_load_and_save_round_trip() {
        let mut effects = effects_with("post/fx", &[], &["inputTexture"]);
        let mut textures = TextureManager::new();
        textures.add_file("post/input.tga", &tga(4, 2));
        let mut store = RenderTargetStore::new();
        store.install(RenderTarget::new("phase/rt_b", 64, 32));
        let section = material_section("post/fx", "post/input.tga");
        let open_section = |name: &str| -> Option<DataSection> {
            (name == "post/fx").then(|| material_section("post/fx", "post/input.tga"))
        };

        let mut phase_section = DataSection::new("PyPhase");
        phase_section.write_string("name", "blur");
        phase_section.write_string("renderTarget", "phase/rt_b");
        let flag = phase_section.add_child("clearRenderTarget");
        flag.set_bool(true);
        let wrapper = phase_section.add_child("filterQuad");
        let taps = quad_section(&TAPS);
        wrapper.copy_from(&taps);
        let material_wrapper = phase_section.add_child("material");
        material_wrapper.copy_from(&section);

        let mut ctx = LoadContext {
            effects: &mut effects,
            textures: &mut textures,
            open_section: &open_section,
            render_targets: &store,
        };
        let mut phase = PyPhase::new();
        assert!(phase.load(&phase_section, &mut ctx));
        assert_eq!(phase.name, "blur");
        assert_eq!(
            phase
                .render_target
                .as_ref()
                .map(|rt| rt.resource_id.clone()),
            Some("phase/rt_b".to_owned())
        );
        assert!(phase.clear_render_target);
        assert_eq!(phase.filter_quad.as_ref().expect("quad").samples.len(), 4);
        assert!(phase.material.is_some());

        let mut parent = DataSection::new("Effect");
        phase.save(&mut parent);
        let saved = parent.find_child("PyPhase").expect("saved");
        assert_eq!(saved.read_string("name", ""), "blur");
        assert_eq!(saved.read_string("renderTarget", ""), "phase/rt_b");
        assert!(
            saved
                .find_child("clearRenderTarget")
                .expect("flag")
                .as_bool(false)
        );
        let quad_wrapper = saved.find_child("filterQuad").expect("quad wrapper");
        let quad_child = quad_wrapper.open_child(0).expect("quad child");
        assert_eq!(quad_child.section_name(), "PyFilterQuad");
        assert_eq!(quad_child.count_children(), 4);
        assert!(saved.find_child("material").is_some());
    }

    #[test]
    fn py_phase_load_keeps_a_failing_material_absent() {
        // The fx file is not in the effect manager, so the material load
        // fails and the phase keeps no material (the reference's
        // `if (pMat->load(...))` gate).
        let mut effects = EffectManager::new();
        let mut textures = TextureManager::new();
        let store = RenderTargetStore::new();
        let mut section = DataSection::new("PyPhase");
        let material_wrapper = section.add_child("material");
        material_wrapper.copy_from(&material_section("missing/fx", "post/input.tga"));

        let mut ctx = LoadContext {
            effects: &mut effects,
            textures: &mut textures,
            open_section: &no_sections,
            render_targets: &store,
        };
        let mut phase = PyPhase::new();
        assert!(phase.load(&section, &mut ctx));
        assert!(phase.material.is_none());
    }

    #[test]
    fn py_phase_render_target_from_string_resolves_or_clears() {
        let mut store = RenderTargetStore::new();
        store.install(RenderTarget::new("phase/rt_b", 64, 32));
        let mut phase = PyPhase::new();
        assert!(phase.render_target_from_string("phase/rt_b", &store));
        assert_eq!(
            phase
                .render_target
                .as_ref()
                .map(|rt| rt.resource_id.clone()),
            Some("phase/rt_b".to_owned())
        );
        // A miss clears the target and reports failure.
        phase.render_target = Some(RenderTarget::new("old", 1, 1));
        assert!(!phase.render_target_from_string("nope", &store));
        assert!(phase.render_target.is_none());
        // An empty name does nothing.
        assert!(!phase.render_target_from_string("", &store));
    }

    // ----- PyCopyBackBuffer -----

    #[test]
    fn copy_back_buffer_starts_dirty_and_optimises_the_second_draw() {
        let mut device = Device::new(400.0, 300.0);
        let mut debug = Debug::new();
        debug.set_render_target(Some(RenderTarget::new("debug/rt", 64, 32)));
        debug.begin_chain(1);
        debug.begin_effect(1);

        let mut phase = PyCopyBackBuffer::new();
        phase.render_target = Some(RenderTarget::new("cb/rt", 64, 32));
        assert!(phase.is_dirty(), "a fresh instance starts one behind");

        phase.draw(Some(&mut debug), &mut device);
        // Dirty: the stretch plus the copy into cell (0,0) (with its clear).
        assert_eq!(
            device.ops,
            vec![
                Op::CopyBackBuffer("back buffer".to_owned(), "cb/rt".to_owned()),
                Op::Clear(0x00000000),
                Op::CopyPhase(Rect::new(0, 0, 64, 32)),
            ]
        );

        // The same instance, no tick between: clean, so only the grey cell.
        let mut device = Device::new(400.0, 300.0);
        let mut debug = Debug::new();
        debug.set_render_target(Some(RenderTarget::new("debug/rt", 64, 32)));
        debug.begin_chain(1);
        debug.begin_effect(1);
        phase.draw(Some(&mut debug), &mut device);
        assert_eq!(device.ops, vec![Op::DisabledPhase(Rect::new(0, 0, 64, 32))]);
    }

    #[test]
    fn copy_back_buffer_tick_bumps_the_shared_cookie() {
        let mut phase = PyCopyBackBuffer::new();
        phase.draw(None, &mut Device::new(400.0, 300.0));
        let clean = PyCopyBackBuffer::draw_cookie();
        assert_eq!(clean, PyCopyBackBuffer::draw_cookie());
        phase.tick();
        assert_eq!(PyCopyBackBuffer::draw_cookie(), clean + 1);
        assert!(phase.is_dirty());
    }

    #[test]
    fn copy_back_buffer_save_and_load_round_trip() {
        let mut store = RenderTargetStore::new();
        store.install(RenderTarget::new("cb/rt", 64, 32));
        let mut phase = PyCopyBackBuffer::new();
        phase.render_target = Some(RenderTarget::new("cb/rt", 64, 32));

        let mut parent = DataSection::new("Effect");
        phase.save(&mut parent);
        let saved = parent.find_child("PyCopyBackBuffer").expect("saved");
        assert_eq!(saved.read_string("name", ""), "copy back buffer");
        assert_eq!(saved.read_string("renderTarget", ""), "cb/rt");

        let mut loaded = PyCopyBackBuffer::new();
        assert!(loaded.load(saved, &store));
        assert_eq!(loaded.name, "copy back buffer");
        assert_eq!(
            loaded
                .render_target
                .as_ref()
                .map(|rt| rt.resource_id.clone()),
            Some("cb/rt".to_owned())
        );
    }

    // ----- Phase factory -----

    #[test]
    fn phase_factory_resolves_labels_and_rejects_unknowns() {
        let mut effects = effects_with("post/fx", &[], &["inputTexture"]);
        let mut textures = TextureManager::new();
        let store = RenderTargetStore::new();

        let mut phase_section = DataSection::new("PyPhase");
        phase_section.write_string("name", "factory");
        // A bindable material so the load walks the inheritance resolver.
        let material_wrapper = phase_section.add_child("material");
        material_wrapper.copy_from(&material_section("post/fx", "post/input.tga"));
        textures.add_file("post/input.tga", &tga(4, 2));
        let mut ctx = LoadContext {
            effects: &mut effects,
            textures: &mut textures,
            open_section: &no_sections,
            render_targets: &store,
        };
        let phase = PhaseFactory::load_item(&phase_section, &mut ctx).expect("phase");
        assert_eq!(phase.section_name(), "PyPhase");
        assert!(matches!(&phase, Phase::PyPhase(_)));
        if let Phase::PyPhase(inner) = &phase {
            assert_eq!(inner.name, "factory");
            assert!(inner.material.is_some());
        }

        let copy_section = DataSection::new("PyCopyBackBuffer");
        let phase = PhaseFactory::load_item(&copy_section, &mut ctx).expect("copy");
        assert_eq!(phase.section_name(), "PyCopyBackBuffer");

        // Unported filter quad labels and anything else resolve to nothing.
        assert!(PhaseFactory::load_item(&DataSection::new("PyTransferQuad"), &mut ctx).is_none());
        assert!(PhaseFactory::load_item(&DataSection::new("Mystery"), &mut ctx).is_none());
    }

    // ----- Effect -----

    #[test]
    fn an_invalid_target_copies_none_into_the_debug_cell() {
        let (mut phase, _effects, mut textures) = tap_phase(None, false, &[]);
        let mut rt = RenderTarget::new("phase/invalid", 64, 32);
        rt.valid = false;
        phase.render_target = Some(rt);
        let mut device = Device::new(400.0, 300.0);
        let mut debug = Debug::new();
        debug.set_render_target(Some(RenderTarget::new("debug/rt", 64, 32)));
        debug.begin_chain(1);
        debug.begin_effect(1);
        phase.draw(Some(&mut debug), None, &mut device, &mut textures);
        // The cell still opens: the first cell clears the grid, then copies
        // from no source.
        assert_eq!(
            device.ops[device.ops.len() - 2..],
            [
                Op::Clear(0x00000000),
                Op::CopyPhase(Rect::new(0, 0, 64, 32)),
            ][..]
        );
        assert!(
            device
                .ops
                .iter()
                .any(|op| matches!(op, Op::PushRenderTarget(id) if id == "phase/invalid"))
        );
    }

    #[test]
    fn effect_load_freezes_the_bypass_and_names_unknown_phases() {
        let mut effects = EffectManager::new();
        let mut textures = TextureManager::new();
        let store = RenderTargetStore::new();

        let mut section = DataSection::new("effects");
        section.set_string("fade");
        let bypass = section.add_child("bypass");
        bypass.set_vector4(Vector4::new(1.0, 2.0, 3.0, 4.0));
        section.add_child("MysteryPhase").set_string("x");
        section.add_child("PyCopyBackBuffer");

        let mut ctx = LoadContext {
            effects: &mut effects,
            textures: &mut textures,
            open_section: &no_sections,
            render_targets: &store,
        };
        let mut effect = Effect::new();
        assert!(effect.load(&section, &mut ctx));
        assert_eq!(effect.name, "fade");
        // The bypass froze into a basic provider.
        assert_eq!(
            effect.bypass.as_ref().map(|p| p.output()),
            Some(Vector4::new(1.0, 2.0, 3.0, 4.0))
        );
        // Only the copy back buffer loaded; the bypass was skipped by name
        // and the unknown type logged.
        assert_eq!(effect.phases.len(), 1);
        assert_eq!(effect.phases[0].section_name(), "PyCopyBackBuffer");
        assert_eq!(effect.log, vec!["Unknown phase type MysteryPhase"]);
    }

    /// A tap phase effect with the given bypass `w`, plus its own texture
    /// manager (a fresh pair per draw, like a fresh frame each).
    fn bypassed(w: f32) -> (Effect, TextureManager) {
        let (phase, _effects, textures) =
            tap_phase(Some(RenderTarget::new("phase/rt_b", 64, 32)), false, &[]);
        let mut effect = Effect::new();
        effect.phases = vec![Phase::PyPhase(phase)];
        effect.bypass = Some(Rc::new(Vector4Basic(Vector4::new(0.0, 0.0, 0.0, w))));
        (effect, textures)
    }

    #[test]
    fn the_bypass_gate_skips_draws_strictly_below_the_epsilon() {
        // Fully on: the phase draws.
        let (mut effect, mut textures) = bypassed(1.0);
        let mut device = Device::new(400.0, 300.0);
        effect.draw(None, &mut device, &mut textures);
        assert!(
            device
                .ops
                .iter()
                .any(|op| matches!(op, Op::DrawFilterQuad(6)))
        );

        // The epsilon is strict: w = 0.0004 is not almost zero (the
        // difference is not below 0.0004), so the phase still draws.
        let (mut effect, mut textures) = bypassed(0.0004);
        let mut device = Device::new(400.0, 300.0);
        effect.draw(None, &mut device, &mut textures);
        assert!(
            device
                .ops
                .iter()
                .any(|op| matches!(op, Op::DrawFilterQuad(6)))
        );

        // Below the epsilon and the whole phase tree is skipped.
        let (mut effect, mut textures) = bypassed(0.00039);
        let mut device = Device::new(400.0, 300.0);
        effect.draw(None, &mut device, &mut textures);
        assert!(device.ops.is_empty());
    }

    #[test]
    fn a_bypassed_effect_still_records_grey_cells_per_phase() {
        let (phase_a, _effects, mut textures) =
            tap_phase(Some(RenderTarget::new("phase/rt_b", 64, 32)), false, &[]);
        let (phase_b, _effects, _textures) =
            tap_phase(Some(RenderTarget::new("phase/rt_b", 64, 32)), false, &[]);
        let mut effect = Effect::new();
        effect.phases = vec![Phase::PyPhase(phase_a), Phase::PyPhase(phase_b)];
        effect.bypass = Some(Rc::new(Vector4Basic(Vector4::new(0.0, 0.0, 0.0, 0.0))));

        let mut device = Device::new(400.0, 300.0);
        let mut debug = Debug::new();
        debug.set_render_target(Some(RenderTarget::new("debug/rt", 64, 32)));
        debug.begin_chain(1);
        effect.draw(Some(&mut debug), &mut device, &mut textures);
        // Two grey cells, no device draws.
        assert_eq!(
            device.ops,
            vec![
                Op::DisabledPhase(Rect::new(0, 0, 32, 32)),
                Op::DisabledPhase(Rect::new(32, 0, 64, 32)),
            ]
        );
    }

    #[test]
    fn effect_tick_reaches_the_copy_back_buffer_cookie() {
        let mut effect = Effect::new();
        effect.phases = vec![Phase::CopyBackBuffer(PyCopyBackBuffer::new())];
        let before = PyCopyBackBuffer::draw_cookie();
        effect.tick();
        assert_eq!(PyCopyBackBuffer::draw_cookie(), before + 1);
    }

    #[test]
    fn effect_save_freezes_the_bypass_and_round_trips() {
        let (phase, _effects, _textures) =
            tap_phase(Some(RenderTarget::new("phase/rt_b", 64, 32)), false, &[]);
        let mut effect = Effect::new();
        effect.name = "bloom".to_owned();
        // A live provider reading 5 at save time freezes to 5.
        effect.bypass = Some(Rc::new(Vector4Basic(Vector4::new(1.0, 1.0, 1.0, 5.0))));
        effect.phases = vec![Phase::PyPhase(phase)];

        let mut parent = DataSection::new("effects");
        effect.save(&mut parent);
        let saved = parent.find_child("Effect").expect("saved");
        assert_eq!(saved.as_string(), "bloom");
        assert_eq!(
            saved
                .find_child("bypass")
                .expect("bypass")
                .as_vector4(Vector4::new(0.0, 0.0, 0.0, 0.0)),
            Vector4::new(1.0, 1.0, 1.0, 5.0)
        );

        // Loading the saved section reproduces the frozen bypass and the
        // phase.
        let mut effects = EffectManager::new();
        let mut textures = TextureManager::new();
        let mut store = RenderTargetStore::new();
        store.install(RenderTarget::new("phase/rt_b", 64, 32));
        let mut ctx = LoadContext {
            effects: &mut effects,
            textures: &mut textures,
            open_section: &no_sections,
            render_targets: &store,
        };
        let mut loaded = Effect::new();
        assert!(loaded.load(saved, &mut ctx));
        assert_eq!(loaded.name, "bloom");
        assert_eq!(
            loaded.bypass.as_ref().map(|p| p.output()),
            Some(Vector4::new(1.0, 1.0, 1.0, 5.0))
        );
        assert_eq!(loaded.phases.len(), 1);
    }

    // ----- Manager -----

    #[test]
    fn set_chain_unwraps_a_single_sequence_and_counts_nulls() {
        let mut manager = Manager::new();

        // The python face passes one sequence argument: it is unwrapped.
        assert!(
            manager
                .set_chain(vec![ChainItem::Sequence(vec![
                    ChainItem::Effect(Effect::new()),
                    ChainItem::Null,
                ])])
                .is_ok()
        );
        assert_eq!(manager.effects.len(), 1);

        // None slots are counted in the chain length but not stored.
        assert!(
            manager
                .set_chain(vec![
                    ChainItem::Effect(Effect::new()),
                    ChainItem::Null,
                    ChainItem::Null,
                ])
                .is_ok()
        );
        assert_eq!(manager.effects.len(), 1);

        // A foreign object makes the length outrun the stored effects, and
        // the count reports the nulls as mismatches too (the reference's
        // `slen - effects_.size()`).
        let error = manager
            .set_chain(vec![
                ChainItem::Effect(Effect::new()),
                ChainItem::Null,
                ChainItem::Foreign,
            ])
            .unwrap_err();
        assert_eq!(
            error,
            "PostProcessing.chain expects a sequence of Effects.  2 items \
             in the sequence did not match this type"
        );
        // The effects pushed before the error stay pushed.
        assert_eq!(manager.effects.len(), 1);
    }

    #[test]
    fn the_chain_getter_lists_only_pushed_effects() {
        let mut manager = Manager::new();
        manager
            .set_chain(vec![ChainItem::Effect(Effect::new()), ChainItem::Null])
            .expect("chain");
        assert_eq!(manager.effects.len(), 1);
    }

    #[test]
    fn manager_draw_ledger_matches_the_call_tree() {
        let mut manager = Manager::new();
        let mut effect = Effect::new();
        let mut cb = PyCopyBackBuffer::new();
        cb.render_target = Some(RenderTarget::new("cb/rt", 64, 32));
        effect.phases = vec![
            Phase::CopyBackBuffer(cb),
            Phase::PyPhase(PyPhase::new()), // no quad/material: draws nothing
        ];
        manager
            .set_chain(vec![ChainItem::Effect(effect)])
            .expect("chain");

        let mut device = Device::new(400.0, 300.0);
        let mut textures = TextureManager::new();
        let mut debug = Debug::new();
        debug.set_render_target(Some(RenderTarget::new("debug/rt", 64, 32)));
        manager.debug = Some(debug);
        manager.draw(&mut device, &mut textures);

        assert_eq!(
            device.ops,
            vec![
                Op::BindMrt,
                Op::PushColorWrite,
                Op::CopyBackBuffer("back buffer".to_owned(), "cb/rt".to_owned()),
                Op::Clear(0x00000000),
                // Two phases in the effect make a 2-across grid, full height.
                Op::CopyPhase(Rect::new(0, 0, 32, 32)),
                Op::PopColorWrite,
                Op::UnbindMrt,
            ]
        );
        assert!(manager.debug.as_ref().expect("debug").enabled);
    }

    #[test]
    fn a_bypassed_effect_in_the_chain_greys_its_cells() {
        let mut manager = Manager::new();
        let mut enabled = Effect::new();
        let mut cb = PyCopyBackBuffer::new();
        cb.render_target = Some(RenderTarget::new("cb/rt", 64, 32));
        enabled.phases = vec![Phase::CopyBackBuffer(cb)];

        let mut bypassed = Effect::new();
        bypassed.phases = vec![
            Phase::PyPhase(PyPhase::new()),
            Phase::PyPhase(PyPhase::new()),
        ];
        bypassed.bypass = Some(Rc::new(Vector4Basic(Vector4::new(0.0, 0.0, 0.0, 0.0))));

        manager
            .set_chain(vec![
                ChainItem::Effect(enabled),
                ChainItem::Effect(bypassed),
            ])
            .expect("chain");

        let mut device = Device::new(400.0, 300.0);
        let mut textures = TextureManager::new();
        let mut debug = Debug::new();
        debug.set_render_target(Some(RenderTarget::new("debug/rt", 64, 32)));
        manager.debug = Some(debug);
        manager.draw(&mut device, &mut textures);

        // Effect 0 (copy back buffer, dirty): the copy into its cell, with
        // the one whole-target clear. Effect 1 is bypassed: two grey cells
        // in the second effect column, no draws at all.
        assert_eq!(
            device.ops,
            vec![
                Op::BindMrt,
                Op::PushColorWrite,
                Op::CopyBackBuffer("back buffer".to_owned(), "cb/rt".to_owned()),
                Op::Clear(0x00000000),
                Op::CopyPhase(Rect::new(0, 0, 32, 32)),
                Op::DisabledPhase(Rect::new(32, 0, 48, 32)),
                Op::DisabledPhase(Rect::new(48, 0, 64, 32)),
                Op::PopColorWrite,
                Op::UnbindMrt,
            ]
        );
    }

    #[test]
    fn manager_load_and_save_round_trip() {
        let mut effects = EffectManager::new();
        let mut textures = TextureManager::new();
        let store = RenderTargetStore::new();

        let mut file_section = DataSection::new("postprocessing");
        let effect_section = file_section.add_child("Effect");
        effect_section.set_string("first");
        effect_section.add_child("PyCopyBackBuffer");
        file_section.add_child("NotAnEffect").set_string("skip");

        let mut ctx = LoadContext {
            effects: &mut effects,
            textures: &mut textures,
            open_section: &no_sections,
            render_targets: &store,
        };
        let mut manager = Manager::new();
        manager.load(&file_section, &mut ctx);
        assert_eq!(manager.effects.len(), 1);
        assert_eq!(manager.effects[0].name, "first");

        let mut saved = DataSection::new("postprocessing");
        manager.save(&mut saved);
        assert_eq!(saved.count_children(), 1);
        let child = saved.open_child(0).expect("child");
        assert_eq!(child.section_name(), "Effect");
        assert_eq!(child.as_string(), "first");
    }

    // ----- ProjectionAccess -----

    fn projection() -> ProjectionAccess {
        ProjectionAccess::new(CameraParams::perspective(0.1, 100.0, 1.0, 1.3333))
    }

    #[test]
    fn the_fov_ramp_interpolates_and_lands_exactly() {
        let mut projection = projection();
        projection.ramp_fov(2.0, 1.0);
        assert!(projection.is_smooth_fov_transition());

        // A quarter in: percent 0.25, fov 1 + 0.25 * (2 - 1).
        projection.update(0.25);
        assert_eq!(projection.fov(), 1.25);
        assert!(projection.is_smooth_fov_transition());

        // elapsed reaches 1.0: not below the time, so the ramp lands on the
        // end value and switches itself off.
        projection.update(0.75);
        assert_eq!(projection.fov(), 2.0);
        assert!(!projection.is_smooth_fov_transition());

        // A finished ramp no longer moves.
        projection.update(1.0);
        assert_eq!(projection.fov(), 2.0);
    }

    #[test]
    fn ramp_fov_starts_from_the_current_fov() {
        let mut projection = projection();
        projection.set_fov(1.5);
        projection.ramp_fov(2.0, 0.5);
        projection.update(0.25);
        assert_eq!(projection.fov(), 1.75);
    }

    #[test]
    fn setting_the_fov_cancels_the_ramp() {
        let mut projection = projection();
        projection.ramp_fov(2.0, 1.0);
        projection.set_fov(1.5);
        assert!(!projection.is_smooth_fov_transition());
        projection.update(0.5);
        assert_eq!(projection.fov(), 1.5);
    }

    #[test]
    fn update_without_a_ramp_is_a_no_op() {
        let mut projection = projection();
        projection.update(10.0);
        assert_eq!(projection.fov(), 1.0);
    }

    #[test]
    fn near_plane_and_the_far_plane_getter_hit_the_camera() {
        let mut projection = projection();
        projection.set_near_plane(0.5);
        assert_eq!(projection.near_plane(), 0.5);
        assert_eq!(projection.camera.near_plane, 0.5);
        assert_eq!(projection.far_plane(), 100.0);
    }

    #[test]
    fn the_far_plane_setter_skips_the_camera_and_feeds_the_enviro() {
        // The reference quirk (`projection_access.cpp:131-141`): the setter
        // propagates into the chunk space's environment only.
        let mut projection = projection();
        projection.set_far_plane(500.0);
        assert_eq!(projection.enviro_far_plane, Some(500.0));
        assert_eq!(projection.camera.far_plane, 100.0);
        assert_eq!(projection.far_plane(), 100.0);
    }

    #[test]
    fn the_projection_camera_is_a_perspective_model() {
        let projection = projection();
        assert_eq!(projection.camera.fov, 1.0);
        assert_eq!(projection.camera.near_plane, 0.1);
        assert_eq!(projection.camera.far_plane, 100.0);
        assert!(!projection.camera.ortho);
    }

    // ----- Device -----

    #[test]
    fn device_pushes_and_pops_render_targets_in_order() {
        let mut device = Device::new(400.0, 300.0);
        device.push_render_target("a");
        device.push_render_target("b");
        device.pop_render_target();
        device.pop_render_target();
        device.pop_render_target();
        assert_eq!(
            device.ops,
            vec![
                Op::PushRenderTarget("a".to_owned()),
                Op::PushRenderTarget("b".to_owned()),
                Op::PopRenderTarget,
                Op::PopRenderTarget,
                Op::PopRenderTarget,
            ]
        );
    }

    #[test]
    fn almost_equal_is_strict() {
        assert!(almost_equal(0.0, 0.00039, 0.0004));
        assert!(!almost_equal(0.0, 0.0004, 0.0004));
        assert!(almost_equal(1.0, 1.0, 0.0004));
    }

    #[test]
    fn debug_greys_use_a_zero_rect_without_a_target() {
        let mut device = Device::new(400.0, 300.0);
        let mut debug = Debug::default();
        // The reference constructor starts with debug ON.
        assert!(debug.enabled);
        assert!(debug.render_target.is_none());
        debug.enabled = false;
        assert_eq!(debug.next_phase(), Rect::new(0, 0, 0, 0));
        assert_eq!(debug.n_effects, 0);
        debug.effect = 0;
        debug.phase = 0;
        debug.draw_disabled_phase(&mut device);
        assert!(device.ops.is_empty());
    }

    #[test]
    fn filter_quad_factory_rejects_unknown_labels() {
        let mut quad_section = DataSection::new("PyTransferQuad");
        quad_section.write_string("texture", "post/input.tga");
        assert!(FilterQuadFactory::load_item(Some(&quad_section)).is_none());
        assert!(FilterQuadFactory::load_item(None).is_none());
    }

    #[test]
    fn a_nested_sequence_counts_as_a_mismatch() {
        let mut manager = Manager::default();
        let mut inner_effect = Effect::new();
        inner_effect.name = "nested".to_owned();
        let inner = vec![ChainItem::Effect(inner_effect), ChainItem::Foreign];
        // The nested sequence unwraps, the foreign slot counts toward slen
        // but is not stored, and the mismatch reports one item.
        let err = manager
            .set_chain(vec![ChainItem::Sequence(inner)])
            .expect_err("mismatch");
        assert!(err.contains("did not match this type"));
        assert!(err.contains("1 items"));
        assert_eq!(manager.effects.len(), 1);
        assert_eq!(manager.effects[0].name, "nested");
        // A Sequence slot inside the outer vector is skipped outright, the
        // same as a foreign object.
        let mut other = Effect::new();
        other.name = "other".to_owned();
        assert!(
            manager
                .set_chain(vec![
                    ChainItem::Effect(other),
                    ChainItem::Sequence(vec![ChainItem::Null])
                ])
                .is_err()
        );
    }

    #[test]
    fn effect_tick_reaches_plain_phases() {
        let mut effect = Effect::new();
        effect.phases.push(Phase::PyPhase(PyPhase::default()));
        effect.tick();
    }

    #[test]
    fn copy_back_buffer_load_clears_an_unknown_render_target() {
        let store = RenderTargetStore::new();
        let mut section = DataSection::new("PyCopyBackBuffer");
        section.write_string("name", "copy");
        section.write_string("renderTarget", "no/such/target");
        let mut copy = PyCopyBackBuffer::default();
        assert_eq!(copy.name, "copy back buffer");
        assert!(copy.load(&section, &store));
        assert!(copy.render_target.is_none());
    }

    #[test]
    fn manager_tick_and_draw_without_a_debug() {
        let mut manager = Manager::default();
        let mut effect = Effect::new();
        effect.name = "tick".to_owned();
        manager
            .set_chain(vec![ChainItem::Effect(effect)])
            .expect("chain");
        manager.tick();
        let mut device = Device::new(400.0, 300.0);
        let mut textures = TextureManager::new();
        manager.draw(&mut device, &mut textures);
        // The frame bookkeeping still runs even with no debug grid.
        assert_eq!(
            device.ops,
            vec![
                Op::BindMrt,
                Op::PushColorWrite,
                Op::PopColorWrite,
                Op::UnbindMrt,
            ]
        );
    }

    #[test]
    fn effect_load_keeps_the_name_without_a_value() {
        let mut section = DataSection::new("Effect");
        section.write_string("name", "");
        let mut effects = EffectManager::new();
        let mut textures = TextureManager::new();
        let store = RenderTargetStore::new();
        let mut ctx = LoadContext {
            effects: &mut effects,
            textures: &mut textures,
            open_section: &no_sections,
            render_targets: &store,
        };
        let mut effect = Effect::new();
        assert!(effect.load(&section, &mut ctx));
        assert_eq!(effect.name, "");
        assert!(effect.phases.is_empty());
    }

    #[test]
    fn py_phase_load_consults_the_resolver_for_a_failing_base() {
        let mut effects = effects_with("post/fx", &[], &["inputTexture"]);
        let mut textures = TextureManager::new();
        // The inheritance resolver is consulted (the mfm child exists) and the
        // base's fx does not resolve, yet the port tolerates the failed base
        // and the load still succeeds.
        let mut base = DataSection::new("post/base.mfm");
        base.write_string("fx", "missing/base.fx");
        let table: HashMap<String, DataSection> =
            [("post/base.mfm".to_owned(), base)].into_iter().collect();
        let open_section = move |name: &str| -> Option<DataSection> { table.get(name).cloned() };
        let mut top = DataSection::new("material");
        top.write_string("fx", "post/fx");
        top.write_string("mfm", "post/base.mfm");
        let mut material = EffectMaterial::new();
        assert!(material.load(&top, &mut effects, &mut textures, &open_section));
    }
}
