//! The material binding face: `EffectMaterial`, its typed properties and
//! the effect records they bind to (BW-11-04-02).
//!
//! Reference sources: the six property types and their factory registry are
//! `lib/moo/effect_property.cpp` (`Vector4Property` :14-75, `MatrixProperty`
//! :79-151, `TextureProperty` :153-240, `FloatProperty` :242-310,
//! `BoolProperty` :312-380, `IntProperty` :382-490, registry :492-543); the
//! material load is `EffectMaterial::load` / `loadInternal`
//! (`lib/moo/effect_material.cpp:181-359`), the default merge
//! `loadProperties` (:362-398), the handle binding `loadPropertyHandles`
//! (:400-443), the typed getters (`boolProperty` :672 onwards) and the
//! apply loop `setProperties` (:113-127). The effect record is the ported
//! `ManagedEffect` (`lib/moo/managed_effect.hpp`): resource id, compile
//! mark, parameter name table and the load-time defaults the D3DX parameter
//! introspection would produce.
//!
//! The HLSL compile itself is D3DX's face and stays outside this item: an
//! effect enters the manager as a pre-built record — the ported `.fxo`
//! binary store — so the binding flow (fx resolution, property factories,
//! mfm inheritance, defaults, handle mapping, apply) is exercised exactly
//! as the reference drives it, with the parameter table playing the part
//! the compiled shader's introspection plays there.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use foundation::math::matrix::Matrix;
use foundation::math::vector::Vector4;
use resources::datasection::DataSection;

use crate::texture::{Texture, TextureManager};

/// A bound property value: the ported `EffectProperty` subclasses. A
/// texture binding may be `None` — `TextureProperty` carries a possibly
/// empty `BaseTexturePtr`, and `apply` then sets the NULL texture
/// (effect_property.cpp:168-171).
#[derive(Debug, Clone, PartialEq)]
pub enum EffectProperty {
    /// `Vector4Property`.
    Vector4(Vector4),
    /// `MatrixProperty`.
    Matrix(Matrix),
    /// `TextureProperty`: the manager result, or `None` for the empty
    /// binding.
    Texture(Option<Rc<Texture>>),
    /// `FloatProperty`.
    Float(f32),
    /// `BoolProperty`.
    Bool(bool),
    /// `IntProperty`.
    Int(i32),
}

impl EffectProperty {
    /// `EffectPropertyFactory::findFactory(sectionName)`: the registry keyed
    /// by the property child's section name
    /// (effect_property.cpp:505-520). `create(name, pChild)` reads the
    /// value out of that child the way each factory does; the name becomes
    /// the mapping key at the call site.
    pub fn create(_name: &str, child: &DataSection, textures: &mut TextureManager) -> Option<Self> {
        match child.section_name() {
            "Vector4" => Some(EffectProperty::Vector4(
                child.as_vector4(Vector4::default()),
            )),
            "Matrix" => {
                // `m.row(i, pSection->readVector4("rowN"))`
                // (effect_property.cpp:120-125).
                let mut m = Matrix::zero();
                for (i, row) in m.m.iter_mut().enumerate() {
                    let tag = format!("row{}", i);
                    let v = child
                        .find_child(&tag)
                        .map(|section| section.as_vector4(Vector4::default()))
                        .unwrap_or_default();
                    *row = [v.x, v.y, v.z, v.w];
                }
                Some(EffectProperty::Matrix(m))
            }
            "Texture" => {
                let texture = textures.get(child.as_string());
                Some(EffectProperty::Texture(texture))
            }
            "Float" => Some(EffectProperty::Float(child.as_float(0.0))),
            "Bool" => Some(EffectProperty::Bool(child.as_bool(false))),
            "Int" => Some(EffectProperty::Int(child.as_int(0))),
            _ => None,
        }
    }

    /// `EffectProperty::apply`: write the value into the effect record's
    /// live parameter values — the `ID3DXEffect::SetVector` family
    /// (effect_property.cpp:19-21, 84-86, 168-172, 249-251, 319-321,
    /// 389-391).
    pub fn apply(&self, name: &str, record: &EffectRecord) {
        match self {
            EffectProperty::Vector4(v) => record.set_value(name, PropertyValue::Vector4(*v)),
            EffectProperty::Matrix(m) => record.set_value(name, PropertyValue::Matrix(*m)),
            EffectProperty::Texture(t) => record.set_value(name, PropertyValue::Texture(t.clone())),
            EffectProperty::Float(f) => record.set_value(name, PropertyValue::Float(*f)),
            EffectProperty::Bool(b) => record.set_value(name, PropertyValue::Bool(*b)),
            EffectProperty::Int(i) => record.set_value(name, PropertyValue::Int(*i)),
        }
    }

    /// `EffectProperty::save` (effect_property.cpp:33-35, 94-96, 201-208,
    /// 275-277, 345-347, 415-417): a typed child on the property section.
    /// The matrix variant writes the reference's literal placeholder.
    pub fn save(&self, section: &mut DataSection) {
        match self {
            EffectProperty::Vector4(v) => {
                let child = section.add_child("Vector4");
                child.set_vector4(*v);
            }
            EffectProperty::Matrix(_) => {
                // `pDS->writeString("Matrix", "NOT YET IMPLEMENTED")`
                // (effect_property.cpp:94-96).
                section
                    .add_child("Matrix")
                    .set_string("NOT YET IMPLEMENTED");
            }
            EffectProperty::Texture(t) => {
                // `getResourceID` hands back "" for the empty binding
                // (effect_property.cpp:190-193).
                let id = t
                    .as_ref()
                    .map(|texture| texture.resource_id.clone())
                    .unwrap_or_default();
                section.add_child("Texture").set_string(&id);
            }
            EffectProperty::Float(f) => section.add_child("Float").set_float(*f),
            EffectProperty::Bool(b) => section.add_child("Bool").set_bool(*b),
            EffectProperty::Int(i) => section.add_child("Int").set_int(*i),
        }
    }

    /// `boolProperty`'s read half: `getBool` on the right variant.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            EffectProperty::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// `intProperty`'s read half.
    pub fn as_int(&self) -> Option<i32> {
        match self {
            EffectProperty::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// `floatProperty`'s read half.
    pub fn as_float(&self) -> Option<f32> {
        match self {
            EffectProperty::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// `vectorProperty`'s read half.
    pub fn as_vector(&self) -> Option<Vector4> {
        match self {
            EffectProperty::Vector4(v) => Some(*v),
            _ => None,
        }
    }

    /// `textureProperty`'s read half: `getResourceID`, `""` when unbound.
    pub fn resource_id(&self) -> Option<String> {
        match self {
            EffectProperty::Texture(t) => Some(
                t.as_ref()
                    .map(|texture| texture.resource_id.clone())
                    .unwrap_or_default(),
            ),
            _ => None,
        }
    }

    /// The bound texture itself, for the render path (`getTexture`).
    pub fn texture(&self) -> Option<&Rc<Texture>> {
        match self {
            EffectProperty::Texture(t) => t.as_ref(),
            _ => None,
        }
    }
}

/// A parameter value in an effect record: what `apply` wrote, or what the
/// record declared as its load-time default.
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    Vector4(Vector4),
    Matrix(Matrix),
    Texture(Option<Rc<Texture>>),
    Float(f32),
    Bool(bool),
    Int(i32),
}

/// The ported `ManagedEffect` (`managed_effect.hpp:160-254`): one compiled
/// effect in the manager's cache. The parameter name table and techniques
/// stand in for the compiled shader's introspection tables; `defaults` is
/// `defaultProperties_` — the parameters' values at load, which
/// `ManagedEffect::load` snapshots from the freshly created effect
/// (managed_effect.cpp:403-429).
pub struct EffectRecord {
    /// The effect resource id (the `.fx` path).
    pub resource_id: String,
    /// `compileMark_`: bumped when the source recompiles, so materials can
    /// notice and reload their handles.
    pub compile_mark: u64,
    /// The technique names the compiled effect carries
    /// (`GetTechniqueByName`).
    pub techniques: Vec<String>,
    /// The parameter names (`GetParameterByName`).
    pub parameters: BTreeSet<String>,
    /// `defaultProperties_`: the load-time snapshot of parameter values.
    pub defaults: BTreeMap<String, PropertyValue>,
    /// The live parameter values — the D3DX effect object's state that the
    /// `Set*` calls mutate. Shared, because a material holds its record by
    /// `Rc` while applying.
    current: RefCell<BTreeMap<String, PropertyValue>>,
    /// The record's own log (`ERROR_MSG` from technique lookups).
    pub log: Vec<String>,
}

impl EffectRecord {
    /// A record for a "compiled" effect: introspection tables and load-time
    /// defaults as the D3DX create would have produced them.
    pub fn new(
        resource_id: &str,
        techniques: &[&str],
        parameters: &[&str],
        defaults: BTreeMap<String, PropertyValue>,
    ) -> Self {
        Self {
            resource_id: resource_id.to_owned(),
            compile_mark: 1,
            techniques: techniques.iter().map(|name| (*name).to_owned()).collect(),
            parameters: parameters.iter().map(|name| (*name).to_owned()).collect(),
            defaults,
            current: RefCell::new(BTreeMap::new()),
            log: Vec::new(),
        }
    }

    /// `GetParameterByName(NULL, name) != NULL`.
    pub fn has_parameter(&self, name: &str) -> bool {
        self.parameters.contains(name)
    }

    /// The `Set*` half of apply: store into the live values.
    fn set_value(&self, name: &str, value: PropertyValue) {
        self.current.borrow_mut().insert(name.to_owned(), value);
    }

    /// A live parameter value, for observing what `begin` applied.
    pub fn current(&self, name: &str) -> Option<PropertyValue> {
        self.current.borrow().get(name).cloned()
    }

    /// `setTechnique` (`effect_material.cpp:556-595`): resolve by name in
    /// the record's technique table; an unknown name logs and fails.
    pub fn set_technique(&mut self, name: &str) -> bool {
        if self.techniques.iter().any(|known| known == name) {
            true
        } else {
            self.log.push(format!(
                "EffectMaterial::hTechnique: could not find valid technique {}",
                name
            ));
            false
        }
    }
}

/// The ported `EffectManager` (`effect_manager.cpp`): the effect cache with
/// the ASSET-msgged miss path.
pub struct EffectManager {
    /// `effects_`: resource id -> compiled effect (`BW::map`).
    effects: BTreeMap<String, Rc<EffectRecord>>,
    /// The manager's log (`ASSET_MSG` from `get`).
    pub log: Vec<String>,
}

impl EffectManager {
    pub fn new() -> Self {
        Self {
            effects: BTreeMap::new(),
            log: Vec::new(),
        }
    }

    /// Put a compiled effect into the store. The reference reaches this
    /// state through `ManagedEffect::load` + D3DX effect creation; the port
    /// takes the pre-built record (the `.fxo` binary face).
    pub fn install(&mut self, record: EffectRecord) {
        let id = record.resource_id.clone();
        self.effects.insert(id, Rc::new(record));
    }

    /// `EffectManager::find`: an exact-key probe (effect_manager.cpp:292-308).
    pub fn find(&self, resource_id: &str) -> Option<Rc<EffectRecord>> {
        self.effects.get(resource_id).cloned()
    }

    /// `EffectManager::get` with `loadIfNotLoaded = true`
    /// (effect_manager.cpp:255-283): a miss is a failed load, ASSET-msgged
    /// and handed back as NULL.
    pub fn get(&mut self, resource_id: &str) -> Option<Rc<EffectRecord>> {
        if let Some(found) = self.find(resource_id) {
            return Some(found);
        }
        self.log.push(format!(
            "EffectManager::get - unable to load {}",
            resource_id
        ));
        None
    }
}

impl Default for EffectManager {
    fn default() -> Self {
        Self::new()
    }
}

/// The ported `EffectMaterial` (`effect_material.hpp`): the material's file
/// bindings plus the subset the effect's parameter table accepted.
pub struct EffectMaterial {
    /// `identifier_`.
    pub identifier: String,
    /// The `fx` section names, first one winning.
    pub fx_names: Vec<String>,
    /// `pManagedEffect_`.
    pub effect: Option<Rc<EffectRecord>>,
    /// `Properties` (`properties_`): the properties that found a parameter,
    /// keyed by the parameter name; this is what `begin` applies and
    /// `save` writes. The reference keeps no member for the raw file
    /// mappings — a property whose name misses the parameter table (or
    /// whose load never reached an effect) is dropped with the call frame.
    pub applied: BTreeMap<String, EffectProperty>,
    /// `materialKind_` (an editor-only field the sections override).
    pub material_kind: i32,
    /// `collisionFlags_`.
    pub collision_flags: i32,
    /// Which of `applied` were overrides of an effect default (their
    /// `setParent` call in `loadProperties`) rather than pure defaults.
    pub overridden: BTreeSet<String>,
    /// The load messages (`WARNING_MSG` / `DEBUG_MSG` / `NOTICE_MSG`).
    pub log: Vec<String>,
}

impl EffectMaterial {
    pub fn new() -> Self {
        Self {
            identifier: String::new(),
            fx_names: Vec::new(),
            effect: None,
            applied: BTreeMap::new(),
            material_kind: 0,
            collision_flags: 0,
            overridden: BTreeSet::new(),
            log: Vec::new(),
        }
    }

    /// `EffectMaterial::load` (effect_material.cpp:181-220): clear the
    /// effect and applied map, run the internal load, and only when it
    /// succeeded with an effect present fold the properties through the
    /// defaults merge and the handle binding.
    pub fn load(
        &mut self,
        section: &DataSection,
        effects: &mut EffectManager,
        textures: &mut TextureManager,
        open_section: &dyn Fn(&str) -> Option<DataSection>,
    ) -> bool {
        // `pManagedEffect_ = NULL; properties_.clear();` (:195-198).
        self.effect = None;
        self.applied.clear();
        self.fx_names.clear();

        let mut mappings: BTreeMap<String, EffectProperty> = BTreeMap::new();
        let ret = self.load_internal(
            section,
            effects,
            textures,
            open_section,
            &mut mappings,
            true,
        );

        if ret && self.effect.is_some() {
            // `loadProperties(effectProperties, addDefault)` then
            // `loadPropertyHandles(effectProperties, suppressPropertyWarnings)`
            // (:204-209).
            self.load_properties(&mut mappings);
            self.load_property_handles(mappings);
        }
        ret
    }

    /// `EffectMaterial::loadInternal` (effect_material.cpp:266-359).
    #[allow(clippy::too_many_arguments)]
    fn load_internal(
        &mut self,
        section: &DataSection,
        effects: &mut EffectManager,
        textures: &mut TextureManager,
        open_section: &dyn Fn(&str) -> Option<DataSection>,
        mappings: &mut BTreeMap<String, EffectProperty>,
        top_level: bool,
    ) -> bool {
        let mut ret = true;

        // The identifier, if not already carried (:279-282).
        if self.identifier.is_empty() {
            self.identifier = section.read_string("identifier", "");
        }

        // The fx list is a local in every `loadInternal` frame
        // (:285-298): the base file of an mfm inheritance resolves its own
        // effect when none is loaded yet, but its fx names never surface on
        // the material. More than one warns; the first is used.
        let fx_names = read_strings(section, "fx");
        if fx_names.len() > 1 {
            self.log.push(format!(
                "EffectMaterial::loadInternal - found multiple .fx files in {}",
                section.section_name()
            ));
        }
        if top_level {
            self.fx_names = fx_names.clone();
        }

        if self.effect.is_none() && !fx_names.is_empty() {
            match effects.get(&fx_names[0]) {
                Some(record) => self.effect = Some(record),
                None => return false, // `!pManagedEffect_` (:300-307).
            }
        }

        // The mfm inheritance: load the named base file first, its result
        // folding into the return (:310-320).
        if let Some(mfm) = section.find_child("mfm") {
            let path = mfm.as_string();
            if let Some(base) = open_section(path) {
                ret &= self.load_internal(&base, effects, textures, open_section, mappings, false);
            }
        }

        // The property sections, taken back-to-front the way the reference
        // pops its vector (:322-345) — so the first section in file order
        // wins a duplicate name.
        let mut property_sections = Vec::new();
        for index in 0..section.count_children() {
            if let Some(child) = section.open_child(index)
                && child.section_name() == "property"
            {
                property_sections.push(child.clone());
            }
        }
        for property_section in property_sections.iter().rev() {
            let name = property_section.as_string();
            if let Some(child) = property_section.open_child(0) {
                match EffectProperty::create(name, child, textures) {
                    Some(property) => {
                        mappings.insert(name.to_owned(), property);
                    }
                    None => {
                        self.log.push(format!(
                            "Could not find property processor for mfm-specified property {}",
                            child.section_name()
                        ));
                    }
                }
            }
        }

        // The editor-only fields, defaulting to what is already carried
        // (:351-353: `readInt("materialKind", materialKind_)`).
        if let Some(field) = section.find_child("materialKind") {
            self.material_kind = field.as_int(self.material_kind);
        }
        if let Some(field) = section.find_child("collisionFlags") {
            self.collision_flags = field.as_int(self.collision_flags);
        }

        ret
    }

    /// `EffectMaterial::loadProperties` (effect_material.cpp:362-398) with
    /// `addDefault = true`: overrides get their parent set to the effect
    /// default (recorded in `overridden`), properties the material never
    /// mentioned are inserted from the defaults.
    fn load_properties(&mut self, mappings: &mut BTreeMap<String, EffectProperty>) {
        let defaults = self
            .effect
            .as_ref()
            .map(|record| record.defaults.clone())
            .unwrap_or_default();
        for (name, default) in defaults {
            match mappings.get(&name) {
                Some(_) => {
                    self.overridden.insert(name);
                }
                None => {
                    let property = match default {
                        PropertyValue::Vector4(v) => EffectProperty::Vector4(v),
                        PropertyValue::Matrix(m) => EffectProperty::Matrix(m),
                        PropertyValue::Texture(t) => EffectProperty::Texture(t),
                        PropertyValue::Float(f) => EffectProperty::Float(f),
                        PropertyValue::Bool(b) => EffectProperty::Bool(b),
                        PropertyValue::Int(i) => EffectProperty::Int(i),
                    };
                    mappings.insert(name, property);
                }
            }
        }
    }

    /// `EffectMaterial::loadPropertyHandles` (effect_material.cpp:400-443):
    /// only properties whose name resolves in the effect's parameter table
    /// enter the applied map; the rest are noticed and dropped.
    fn load_property_handles(&mut self, mappings: BTreeMap<String, EffectProperty>) {
        let record = match self.effect.clone() {
            Some(record) => record,
            None => return,
        };
        for (name, property) in &mappings {
            if record.has_parameter(name) {
                self.applied.insert(name.clone(), property.clone());
            } else {
                self.log.push(format!(
                    "EffectMaterial::load - no parameter \"{}\" found in .fx file \"{}\"",
                    name, record.resource_id
                ));
            }
        }
    }

    /// `EffectMaterial::begin` (effect_material.cpp:88-111): with an effect
    /// present, apply every bound property into the record's live values.
    pub fn begin(&self) -> bool {
        let record = match self.effect.clone() {
            Some(record) => record,
            None => return false,
        };
        for (name, property) in &self.applied {
            property.apply(name, &record);
        }
        true
    }

    /// `boolProperty` (effect_material.cpp:672-691).
    pub fn bool_property(&self, name: &str) -> Option<bool> {
        self.applied.get(name).and_then(EffectProperty::as_bool)
    }

    /// `intProperty`.
    pub fn int_property(&self, name: &str) -> Option<i32> {
        self.applied.get(name).and_then(EffectProperty::as_int)
    }

    /// `floatProperty`.
    pub fn float_property(&self, name: &str) -> Option<f32> {
        self.applied.get(name).and_then(EffectProperty::as_float)
    }

    /// `vectorProperty`.
    pub fn vector_property(&self, name: &str) -> Option<Vector4> {
        self.applied.get(name).and_then(EffectProperty::as_vector)
    }

    /// `textureProperty`: the bound texture's resource id.
    pub fn texture_property(&self, name: &str) -> Option<String> {
        self.applied.get(name).and_then(EffectProperty::resource_id)
    }

    /// `EffectMaterial::save` (effect_material.cpp:221-264): the fx name and
    /// one section per applied property, the property's value carrying the
    /// parameter name and a typed child carrying the value. `materialKind`
    /// and `collisionFlags` are deliberately left out — the reference's TODO
    /// (:259-262).
    pub fn save(&self) -> DataSection {
        let mut section = DataSection::new("material");
        if let Some(record) = &self.effect {
            section.write_string("fx", &record.resource_id);
            for (name, property) in &self.applied {
                let child = section.add_child("property");
                child.set_string(name);
                property.save(child);
            }
        }
        section
    }
}

impl Default for EffectMaterial {
    fn default() -> Self {
        Self::new()
    }
}

/// `pSection->readStrings("fx", fxNames)`: every child section with the tag,
/// each read as its string value.
fn read_strings(section: &DataSection, tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    for index in 0..section.count_children() {
        if let Some(child) = section.open_child(index)
            && child.section_name() == tag
        {
            out.push(child.as_string().to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material_oracle_data::{AXE_DIFFUSE_SAMPLE, AXE_SPEC_FIRST_PIXELS};
    use crate::render::framebuffer::Framebuffer;

    const AXE_VISUAL: &[u8] = include_bytes!("../res/axe.visual");
    const AXE_TGA: &[u8] = include_bytes!("../res/axe.tga");
    const AXE_SPEC_BMP: &[u8] = include_bytes!("../res/axe_spec.bmp");

    /// The axe material's `.fx` path as the visual names it.
    const AXE_FX: &str = "shaders/std_effects/normalmap_chrome.fx";

    /// A compiled-effect record for the axe's effect: the parameter table
    /// carries the diffuse/normal/specular-mask samplers plus a float that
    /// the material never overrides, so the defaults merge is observable.
    fn axe_effect() -> EffectRecord {
        let mut defaults = BTreeMap::new();
        defaults.insert("specPower".to_owned(), PropertyValue::Float(32.0));
        EffectRecord::new(
            AXE_FX,
            &["normalmap_chrome"],
            &["diffuseMap", "normalMap", "reflectionMask", "specPower"],
            defaults,
        )
    }

    /// Managers carrying the vendored fixtures and the axe effect.
    fn managers() -> (EffectManager, TextureManager) {
        let mut effects = EffectManager::new();
        effects.install(axe_effect());
        let mut textures = TextureManager::new();
        textures.add_file("sets/town/props/axe.tga", AXE_TGA);
        textures.add_file("sets/town/props/axe_spec.bmp", AXE_SPEC_BMP);
        (effects, textures)
    }

    /// A minimal compiled effect: the given parameter names, no techniques,
    /// no defaults.
    fn param_effect(resource_id: &str, parameters: &[&str]) -> EffectRecord {
        EffectRecord::new(resource_id, &[], parameters, BTreeMap::new())
    }

    /// The `<material>` section of the vendored axe visual.
    fn axe_material_section() -> DataSection {
        let visual =
            resources::datasection::parse("axe.visual", AXE_VISUAL).expect("visual parses");
        visual
            .find_child("renderSet")
            .expect("renderSet")
            .find_child("geometry")
            .expect("geometry")
            .find_child("primitiveGroup")
            .expect("primitiveGroup")
            .find_child("material")
            .expect("material")
            .clone()
    }

    /// A resolver over an in-test section map, for mfm inheritance.
    fn section_resolver(
        sections: Vec<(&str, &DataSection)>,
    ) -> impl Fn(&str) -> Option<DataSection> {
        let map: BTreeMap<String, DataSection> = sections
            .into_iter()
            .map(|(name, section)| (name.to_owned(), section.clone()))
            .collect();
        move |name| map.get(name).cloned()
    }

    /// Build a small section tree: a tag, an optional value, children.
    fn section(tag: &str, value: &str, children: Vec<DataSection>) -> DataSection {
        let mut out = DataSection::new(tag);
        if !value.is_empty() {
            out.set_string(value);
        }
        for child in children {
            let name = child.section_name().to_owned();
            let slot = out.add_child(&name);
            slot.copy_from(&child);
        }
        out
    }

    #[test]
    fn the_axe_material_loads_its_full_binding() {
        let (mut effects, mut textures) = managers();
        let visual_section = axe_material_section();
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        assert!(material.load(&visual_section, &mut effects, &mut textures, &no_sections));

        assert_eq!(material.identifier, "axehead");
        assert_eq!(material.fx_names, vec![AXE_FX]);
        assert_eq!(material.material_kind, 5);
        assert_eq!(material.collision_flags, 0);

        // The samplers the effect declares are all bound.
        assert_eq!(material.applied.len(), 4);
        let diffuse = material.applied.get("diffuseMap").expect("diffuseMap");
        let diffuse = diffuse.texture().expect("texture binding");
        assert_eq!(diffuse.resource_id, "sets/town/props/axe.tga");
        assert_eq!(diffuse.width, 256);
        assert_eq!(diffuse.status, crate::texture::TextureStatus::Ready);

        let spec = material
            .applied
            .get("reflectionMask")
            .expect("reflectionMask");
        let spec = spec.texture().expect("texture binding");
        assert_eq!(spec.resource_id, "sets/town/props/axe_spec.bmp");
        assert_eq!(spec.width, 128);

        // The normal map file does not exist in the res tree: the property
        // carries the empty binding.
        assert!(matches!(
            material.applied.get("normalMap"),
            Some(EffectProperty::Texture(None))
        ));

        // specPower was never overridden: inserted from the effect defaults.
        assert_eq!(
            material.applied.get("specPower"),
            Some(&EffectProperty::Float(32.0))
        );
        assert!(!material.overridden.contains("specPower"));

        // reflectionMap has no parameter in the compiled effect: dropped
        // from the material entirely, noticed in the log.
        assert!(!material.applied.contains_key("reflectionMap"));
        assert!(material.log.iter().any(|entry| entry
            == "EffectMaterial::load - no parameter \"reflectionMap\" found in .fx file \"shaders/std_effects/normalmap_chrome.fx\""));
    }

    #[test]
    fn begin_applies_the_bound_properties_into_the_effect() {
        let (mut effects, mut textures) = managers();
        let visual_section = axe_material_section();
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        assert!(material.load(&visual_section, &mut effects, &mut textures, &no_sections));
        let record = material.effect.clone().expect("effect");
        assert!(material.begin());

        let diffuse = material
            .applied
            .get("diffuseMap")
            .and_then(EffectProperty::texture);
        assert_eq!(
            record.current("diffuseMap"),
            Some(PropertyValue::Texture(diffuse.cloned()))
        );
        assert_eq!(
            record.current("normalMap"),
            Some(PropertyValue::Texture(None))
        );
        assert_eq!(
            record.current("specPower"),
            Some(PropertyValue::Float(32.0))
        );
        // The dropped property never reaches the effect.
        assert_eq!(record.current("reflectionMap"), None);
    }

    #[test]
    fn the_first_property_section_wins_a_duplicate_name() {
        // The reference pops its property vector back-to-front, so the
        // first section in file order is applied last and wins the map
        // slot (effect_material.cpp:324-344).
        let float_two = section("Float", "2", Vec::new());
        let float_one = section("Float", "1", Vec::new());
        let prop_two = section("property", "test", vec![float_two]);
        let prop_one = section("property", "test", vec![float_one]);
        let fx = section("fx", AXE_FX, Vec::new());
        let root = section("material", "", vec![fx, prop_one, prop_two]);

        let mut effects = EffectManager::new();
        effects.install(param_effect(AXE_FX, &["test"]));
        let mut textures = TextureManager::new();
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        material.load(&root, &mut effects, &mut textures, &no_sections);
        assert_eq!(
            material.applied.get("test"),
            Some(&EffectProperty::Float(1.0))
        );
    }

    #[test]
    fn a_property_of_unknown_type_is_skipped_with_a_log() {
        let weird = section("Weird", "x", Vec::new());
        let property = section("property", "test", vec![weird]);
        let root = section("material", "", vec![property]);

        let (mut effects, mut textures) = managers();
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        material.load(&root, &mut effects, &mut textures, &no_sections);
        assert!(!material.applied.contains_key("test"));
        assert!(material.log.iter().any(|entry| {
            entry.contains("Could not find property processor for mfm-specified property Weird")
        }));
    }

    #[test]
    fn matrix_properties_read_their_rows() {
        let row0 = section("row0", "1 0 0 0", Vec::new());
        let row1 = section("row1", "0 1 0 0", Vec::new());
        let row2 = section("row2", "0 0 1 0", Vec::new());
        let row3 = section("row3", "4 5 6 1", Vec::new());
        let matrix = section("Matrix", "", vec![row0, row1, row2, row3]);
        let property = section("property", "worldMatrix", vec![matrix]);
        let fx = section("fx", AXE_FX, Vec::new());
        let root = section("material", "", vec![fx, property]);

        let mut effects = EffectManager::new();
        effects.install(param_effect(AXE_FX, &["worldMatrix"]));
        let mut textures = TextureManager::new();
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        material.load(&root, &mut effects, &mut textures, &no_sections);
        assert!(matches!(
            material.applied.get("worldMatrix"),
            Some(EffectProperty::Matrix(m))
                if m.m[0] == [1.0, 0.0, 0.0, 0.0] && m.m[3] == [4.0, 5.0, 6.0, 1.0]
        ));
    }

    #[test]
    fn mfm_inheritance_layers_local_over_base() {
        // The base mfm carries a shared property and one the local file
        // never mentions; the local file overrides the shared one and names
        // its own effect, which the recursion must keep.
        let base_shared = section("Int", "7", Vec::new());
        let base_only = section("Float", "0.5", Vec::new());
        let base_prop_shared = section("property", "shared", vec![base_shared]);
        let base_prop_only = section("property", "baseOnly", vec![base_only]);
        let base_fx = section("fx", "shaders/std_effects/other.fx", Vec::new());
        let base = section(
            "material",
            "",
            vec![base_fx, base_prop_shared, base_prop_only],
        );

        let local_shared = section("Int", "9", Vec::new());
        let local_prop_shared = section("property", "shared", vec![local_shared]);
        let mfm = section("mfm", "materials/axe_base.mfm", Vec::new());
        let local_fx = section("fx", AXE_FX, Vec::new());
        let local = section("material", "", vec![local_fx, mfm, local_prop_shared]);

        let mut effects = EffectManager::new();
        effects.install(param_effect(AXE_FX, &["shared", "baseOnly"]));
        let mut textures = TextureManager::new();
        let resolver = section_resolver(vec![("materials/axe_base.mfm", &base)]);
        let mut material = EffectMaterial::new();
        assert!(material.load(&local, &mut effects, &mut textures, &resolver));

        // The local fx won: the effect is the axe's, not the base's — and
        // the base's fx names never surfaced (fxNames is a local of each
        // loadInternal frame), so the uninstalled base fx was never even
        // requested.
        assert_eq!(material.fx_names, vec![AXE_FX]);
        assert_eq!(
            material.effect.as_ref().expect("effect").resource_id,
            AXE_FX
        );
        assert!(effects.log.is_empty());
        // The local override beats the base value; the base-only property
        // survives. Neither matches an effect default, so neither counts as
        // an override.
        assert_eq!(
            material.applied.get("shared"),
            Some(&EffectProperty::Int(9))
        );
        assert_eq!(
            material.applied.get("baseOnly"),
            Some(&EffectProperty::Float(0.5))
        );
        assert!(!material.overridden.contains("shared"));
        assert!(!material.overridden.contains("baseOnly"));
    }

    #[test]
    fn multiple_fx_sections_warn_and_use_the_first() {
        let fx_a = section("fx", AXE_FX, Vec::new());
        let fx_b = section("fx", "shaders/std_effects/other.fx", Vec::new());
        let root = section("material", "", vec![fx_a, fx_b]);

        let (mut effects, mut textures) = managers();
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        material.load(&root, &mut effects, &mut textures, &no_sections);
        assert_eq!(
            material.fx_names,
            vec![AXE_FX, "shaders/std_effects/other.fx"]
        );
        assert!(
            material
                .log
                .iter()
                .any(|entry| entry.contains("found multiple .fx files in material"))
        );
    }

    #[test]
    fn a_missing_effect_fails_the_load_and_leaves_nothing_applied() {
        let fx = section("fx", "shaders/std_effects/missing.fx", Vec::new());
        let root = section("material", "", vec![fx]);

        let (mut effects, mut textures) = managers();
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        assert!(!material.load(&root, &mut effects, &mut textures, &no_sections));
        assert!(material.effect.is_none());
        assert!(material.applied.is_empty());
        assert!(
            effects.log.iter().any(|entry| entry
                == "EffectManager::get - unable to load shaders/std_effects/missing.fx")
        );
    }

    #[test]
    fn a_material_without_fx_loads_but_binds_nothing() {
        // No fx section: no effect, so the property mappings never fold
        // into the applied map — the reference keeps no member for them at
        // all, so they die with the load frame
        // (effect_material.cpp:204: `ret && pManagedEffect_`).
        let colour = section("Vector4", "1 0 0 1", Vec::new());
        let property = section("property", "tint", vec![colour]);
        let root = section("material", "", vec![property]);

        let (mut effects, mut textures) = managers();
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        assert!(material.load(&root, &mut effects, &mut textures, &no_sections));
        assert!(material.applied.is_empty());
        assert!(material.effect.is_none());
    }

    #[test]
    fn typed_getters_read_the_applied_properties() {
        let (mut effects, mut textures) = managers();
        let visual_section = axe_material_section();
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        assert!(material.load(&visual_section, &mut effects, &mut textures, &no_sections));

        assert_eq!(
            material.texture_property("diffuseMap"),
            Some("sets/town/props/axe.tga".to_owned())
        );
        assert_eq!(material.texture_property("normalMap"), Some(String::new()));
        assert_eq!(material.float_property("specPower"), Some(32.0));
        // A texture is not a float, and vice versa.
        assert_eq!(material.float_property("diffuseMap"), None);
        assert_eq!(material.texture_property("specPower"), None);
        assert_eq!(material.bool_property("diffuseMap"), None);
        assert_eq!(material.int_property("diffuseMap"), None);
        assert_eq!(material.vector_property("specPower"), None);
    }

    #[test]
    fn the_technique_resolves_against_the_record() {
        let (mut effects, mut textures) = managers();
        let visual_section = axe_material_section();
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        assert!(material.load(&visual_section, &mut effects, &mut textures, &no_sections));
        let record = material.effect.as_ref().expect("effect");
        assert!(
            record
                .techniques
                .iter()
                .any(|name| name == "normalmap_chrome")
        );

        // Technique resolution is the record's face (`setTechnique`,
        // effect_material.cpp:556-595).
        let mut record = axe_effect();
        assert!(record.set_technique("normalmap_chrome"));
        assert!(!record.set_technique("nope"));
        assert!(
            record
                .log
                .iter()
                .any(|entry| entry.contains("could not find valid technique nope"))
        );
    }

    #[test]
    fn an_override_of_an_effect_default_is_marked() {
        // A material overriding a parameter that carries a load-time default
        // gets the parent link (`loadProperties`, effect_material.cpp:374-381)
        // and stays an override rather than a plain default.
        let power = section("Float", "8", Vec::new());
        let property = section("property", "specPower", vec![power]);
        let fx = section("fx", AXE_FX, Vec::new());
        let root = section("material", "", vec![fx, property]);

        let (mut effects, mut textures) = managers();
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        material.load(&root, &mut effects, &mut textures, &no_sections);
        assert_eq!(
            material.applied.get("specPower"),
            Some(&EffectProperty::Float(8.0))
        );
        assert!(material.overridden.contains("specPower"));
    }

    #[test]
    fn save_writes_fx_and_applied_properties_only() {
        let (mut effects, mut textures) = managers();
        let visual_section = axe_material_section();
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        assert!(material.load(&visual_section, &mut effects, &mut textures, &no_sections));

        let saved = material.save();
        assert_eq!(saved.read_string("fx", ""), AXE_FX);
        // One property section per applied property; reflectionMap is not
        // among them.
        let mut saved_names = Vec::new();
        for index in 0..saved.count_children() {
            if let Some(child) = saved.open_child(index)
                && child.section_name() == "property"
            {
                saved_names.push(child.as_string().to_owned());
            }
        }
        assert_eq!(saved_names.len(), material.applied.len());
        assert!(!saved_names.iter().any(|name| name == "reflectionMap"));
        // materialKind/collisionFlags are deliberately not saved (the
        // reference's TODO).
        assert!(saved.find_child("materialKind").is_none());
        assert!(saved.find_child("collisionFlags").is_none());

        // The texture property saves its resource id.
        let diffuse = saved
            .find_child("property")
            .cloned()
            .expect("first property");
        // (The first child in map order is diffuseMap: `BTreeMap` order.)
        assert_eq!(diffuse.as_string(), "diffuseMap");
        let texture_child = diffuse.find_child("Texture").expect("typed child");
        assert_eq!(texture_child.as_string(), "sets/town/props/axe.tga");
    }

    #[test]
    fn the_axe_material_sample_matches_within_tolerance() {
        // **Acceptance**: the fixed material renders a sample within visual
        // tolerance. The bound diffuse texture is sampled 1:1 into the
        // software back buffer — the fixed-function texture stage with an
        // identity diffuse modulation — and compared against the
        // independent Python oracle per channel.
        let (mut effects, mut textures) = managers();
        let visual_section = axe_material_section();
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        assert!(material.load(&visual_section, &mut effects, &mut textures, &no_sections));
        assert!(material.begin());

        let diffuse = material
            .applied
            .get("diffuseMap")
            .and_then(EffectProperty::texture)
            .expect("bound diffuse");

        let mut framebuffer = Framebuffer::new(64, 64);
        for y in 0u32..64 {
            for x in 0u32..64 {
                // Point sample: texel (x, y) is the visual top-left corner
                // region, already in back-buffer layout (0xAABBGGRR).
                let texel = diffuse.pixels[(y as usize) * diffuse.width as usize + x as usize];
                framebuffer.set_pixel(x, y, texel);
            }
        }

        // Per-channel error against the oracle, tolerance 1.
        let mut max_error = 0u32;
        for (index, expected) in AXE_DIFFUSE_SAMPLE.iter().enumerate() {
            let rendered = framebuffer.pixels[index];
            for shift in [0, 8, 16, 24] {
                let mask = 0xFFu32 << shift;
                let error = (rendered & mask).abs_diff(expected & mask) >> shift;
                max_error = max_error.max(error);
            }
        }
        assert!(
            max_error <= 1,
            "sample diverges from the oracle by {} per channel",
            max_error
        );
        // And the spec-map probe pixels agree with the oracle too (they sit
        // in the material's reflectionMask binding).
        let spec = material
            .applied
            .get("reflectionMask")
            .and_then(EffectProperty::texture)
            .expect("bound spec");
        assert_eq!(&spec.pixels[..8], &AXE_SPEC_FIRST_PIXELS);
    }

    #[test]
    fn every_property_type_round_trips() {
        // The six property processors (the `effect_property.cpp` factory
        // registry) each create from their section, apply into the record
        // and save back as a typed child.
        let rows: Vec<DataSection> = ["1 0 0 0", "0 1 0 0", "0 0 1 0", "0 0 0 1"]
            .iter()
            .enumerate()
            .map(|(index, value)| section(&format!("row{index}"), value, Vec::new()))
            .collect();
        let matrix = section("Matrix", "", rows);
        let texture = section("Texture", "sets/town/props/axe.tga", Vec::new());
        let float = section("Float", "2.5", Vec::new());
        let boolean = section("Bool", "true", Vec::new());
        let int = section("Int", "7", Vec::new());
        let vector = section("Vector4", "1 2 3 4", Vec::new());
        let properties: Vec<DataSection> = [
            ("tint", vector),
            ("worldMatrix", matrix),
            ("diffuseMap", texture),
            ("glow", float),
            ("enabled", boolean),
            ("count", int),
        ]
        .into_iter()
        .map(|(name, value)| section("property", name, vec![value]))
        .collect();
        let fx = section("fx", AXE_FX, Vec::new());
        let mut children = vec![fx];
        children.extend(properties);
        let root = section("material", "", children);

        let mut effects = EffectManager::new();
        effects.install(param_effect(
            AXE_FX,
            &[
                "tint",
                "worldMatrix",
                "diffuseMap",
                "glow",
                "enabled",
                "count",
            ],
        ));
        let mut textures = TextureManager::new();
        textures.add_file("sets/town/props/axe.tga", AXE_TGA);
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        assert!(material.load(&root, &mut effects, &mut textures, &no_sections));
        assert_eq!(material.applied.len(), 6);
        assert!(material.begin());

        let record = material.effect.as_ref().expect("effect");
        assert_eq!(
            record.current("tint"),
            Some(PropertyValue::Vector4(Vector4::new(1.0, 2.0, 3.0, 4.0)))
        );
        assert_eq!(record.current("glow"), Some(PropertyValue::Float(2.5)));
        assert_eq!(record.current("enabled"), Some(PropertyValue::Bool(true)));
        assert_eq!(record.current("count"), Some(PropertyValue::Int(7)));
        let bound = material
            .applied
            .get("diffuseMap")
            .and_then(EffectProperty::texture)
            .expect("bound texture");
        assert_eq!(
            record.current("diffuseMap"),
            Some(PropertyValue::Texture(Some(bound.clone())))
        );

        // The typed getters, one per property class.
        assert_eq!(
            material.vector_property("tint"),
            Some(Vector4::new(1.0, 2.0, 3.0, 4.0))
        );
        assert_eq!(material.float_property("glow"), Some(2.5));
        assert_eq!(material.bool_property("enabled"), Some(true));
        assert_eq!(material.int_property("count"), Some(7));
        assert_eq!(
            material.texture_property("diffuseMap"),
            Some("sets/town/props/axe.tga".to_owned())
        );
        // A float is not a texture: the read half answers None.
        assert!(
            material
                .applied
                .get("glow")
                .expect("glow")
                .texture()
                .is_none()
        );

        // The save face writes one typed child per property, the matrix
        // writing the reference's literal placeholder.
        let saved = material.save();
        let mut names = Vec::new();
        for index in 0..saved.count_children() {
            let child = saved.open_child(index).expect("child");
            if child.section_name() == "fx" {
                // The saved effect resource id leads the section.
                assert_eq!(child.as_string(), AXE_FX);
                continue;
            }
            assert_eq!(child.section_name(), "property");
            let typed = child.open_child(0).expect("typed child");
            names.push(child.as_string().to_owned());
            match child.as_string() {
                "tint" => {
                    assert_eq!(typed.section_name(), "Vector4");
                    assert_eq!(
                        typed.as_vector4(Vector4::default()),
                        Vector4::new(1.0, 2.0, 3.0, 4.0)
                    );
                }
                "worldMatrix" => {
                    // `pDS->writeString("Matrix", "NOT YET IMPLEMENTED")`.
                    assert_eq!(typed.section_name(), "Matrix");
                    assert_eq!(typed.as_string(), "NOT YET IMPLEMENTED");
                }
                "diffuseMap" => {
                    assert_eq!(typed.section_name(), "Texture");
                    assert_eq!(typed.as_string(), "sets/town/props/axe.tga");
                }
                "glow" => assert_eq!(typed.as_float(0.0), 2.5),
                "enabled" => assert_eq!(typed.section_name(), "Bool"),
                "count" => assert_eq!(typed.as_int(0), 7),
                _ => panic!("unexpected property {}", child.as_string()),
            }
        }
        // `BTreeMap` order.
        assert_eq!(
            names,
            vec![
                "count",
                "diffuseMap",
                "enabled",
                "glow",
                "tint",
                "worldMatrix"
            ]
        );
    }

    #[test]
    fn effect_defaults_of_every_type_fill_the_material() {
        // The defaults merge converts each compiled-effect default into a
        // property the material never mentioned.
        let mut defaults = BTreeMap::new();
        defaults.insert(
            "tint".to_owned(),
            PropertyValue::Vector4(Vector4::new(1.0, 2.0, 3.0, 4.0)),
        );
        defaults.insert(
            "worldMatrix".to_owned(),
            PropertyValue::Matrix(Matrix::identity()),
        );
        defaults.insert("diffuseMap".to_owned(), PropertyValue::Texture(None));
        defaults.insert("glow".to_owned(), PropertyValue::Float(1.5));
        defaults.insert("enabled".to_owned(), PropertyValue::Bool(true));
        defaults.insert("count".to_owned(), PropertyValue::Int(3));
        let mut effects = EffectManager::new();
        effects.install(EffectRecord::new(
            AXE_FX,
            &[],
            &[
                "tint",
                "worldMatrix",
                "diffuseMap",
                "glow",
                "enabled",
                "count",
            ],
            defaults,
        ));
        let mut textures = TextureManager::new();
        let fx = section("fx", AXE_FX, Vec::new());
        let root = section("material", "", vec![fx]);
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        assert!(material.load(&root, &mut effects, &mut textures, &no_sections));

        assert_eq!(material.applied.len(), 6);
        assert!(material.overridden.is_empty());
        assert_eq!(
            material.applied.get("tint"),
            Some(&EffectProperty::Vector4(Vector4::new(1.0, 2.0, 3.0, 4.0)))
        );
        assert_eq!(material.applied.get("count"), Some(&EffectProperty::Int(3)));
        assert_eq!(
            material.applied.get("enabled"),
            Some(&EffectProperty::Bool(true))
        );
    }

    #[test]
    fn a_material_section_can_carry_an_identifier() {
        // `identifier_` is filled from the section when the material does
        // not carry a name yet (effect_material.cpp:279-282).
        let fx = section("fx", AXE_FX, Vec::new());
        let id = section("identifier", "materials/axe.materialed", Vec::new());
        let root = section("material", "", vec![fx, id]);

        let mut effects = EffectManager::new();
        effects.install(param_effect(AXE_FX, &[]));
        let mut textures = TextureManager::new();
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        assert!(material.load(&root, &mut effects, &mut textures, &no_sections));
        assert_eq!(material.identifier, "materials/axe.materialed");
    }

    #[test]
    fn begin_and_save_without_an_effect_are_noops() {
        // With no effect the begin loop has nothing to apply and the save
        // writes neither the fx nor any property (effect_material.cpp:88-90,
        // :221-224).
        let colour = section("Vector4", "1 0 0 1", Vec::new());
        let property = section("property", "tint", vec![colour]);
        let root = section("material", "", vec![property]);

        let (mut effects, mut textures) = managers();
        let no_sections = section_resolver(Vec::new());
        let mut material = EffectMaterial::new();
        assert!(material.load(&root, &mut effects, &mut textures, &no_sections));
        assert!(!material.begin());
        let saved = material.save();
        assert_eq!(saved.section_name(), "material");
        assert_eq!(saved.count_children(), 0);
    }

    #[test]
    fn default_constructors_match_new() {
        assert!(EffectManager::default().find("anything").is_none());
        assert_eq!(EffectMaterial::default().material_kind, 0);
    }
}
