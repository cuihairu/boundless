//! BW-13-04-02 the ModelEditor's material editing face
//! (`tools/modeleditor_core/Models/mutant_materials.cpp`).
//!
//! The editor's material state is the tables gathered from the opened
//! documents by [`MaterialEditor::census`] (the port of
//! `Mutant::regenMaterialsList`): materials keyed by the identifier every
//! `renderSet/geometry/primitiveGroup/material` section carries, dyes keyed
//! by the `matter` each model `dye` section declares, and tints keyed by
//! matter then tint name — plus the synthesized `Default` tint every dye
//! gets. The census also repairs `missingBspMaterial` markers onto the
//! first render set's geometry and applies (and queues stale) the
//! `materialNames` display aliases.
//!
//! Editing goes through [`MaterialEditor::instantiate_mfm`] (expand a
//! legacy `mfm` reference in place), [`MaterialEditor::overload_mfm`], the
//! `clean*` normalisations, the property setters, the naming moves, the
//! texture-feed swap and the flag queries. Structural moves re-run the
//! census ([`MaterialEditor::recensus`]), which is what the reference's
//! `reloadAllLists` ends in; `reloadModel`/`reloadBSP` reload the documents
//! from disk and are not carried — the port edits the in-memory documents,
//! which the caller saves through the model mutator's flows.
//!
//! The reference drives live `Moo::EffectMaterial` objects compiled from
//! `.fx` files. The port has no D3DX runtime, so the seams are explicit:
//! the artist-editable property lists the clean faces need arrive as
//! [`EffectProperties`] data (the reference reads the `artistEditable`
//! annotation off each compiled effect property, iterated in the property
//! map's name order), the dual-UV answer [`MaterialEditor::
//! ensure_shader_correct`] needs arrives as a parameter, and the two
//! effect-parameter queries are text scans of the shader source. The
//! unskinned-on-skinned `MessageBox` prompts are treated as accepted, and
//! the texture-memory-usage face (a GUI number over `TextureManager`) is
//! not carried. Where the reference dereferences a null section (an unknown
//! dye/tint key, the `Default` tint's null data), the port refuses and
//! returns instead.

use std::collections::BTreeMap;

use foundation::math::Vector4;
use resources::binary::{BinSection, BinaryBlock};
use resources::bwresource::{self, BwResource};
use resources::datasection::{self, DataSection};

use crate::model_editor::{open_sections, primitives_file, split_old_primitive_name};
use crate::primitive_file_structs::VertexHeader;

/// `TintInfo::data` is null for the synthesized `Default` tint, so flag
/// reads fall back to the `0xB0B15BAD` sentinel (`mutant_materials.cpp:1366`).
pub const FLAG_NOT_SET: i32 = 0xB0B15BAD_u32 as i32;

/// `Moo::UV2Stream::TYPE_NAME` — the stream-suffix suffix that marks dual UVs.
pub const UV2_STREAM: &str = "uv2";

/// The artist-editable property names an effect exposes. The reference
/// reads them off the compiled effect's property annotations and iterates
/// the property map in name order; the port's seam hands them over as data
/// and [`EffectProperties::editable`] sorts them to reproduce the map
/// order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EffectProperties {
    /// The `.fx` resource id the properties belong to.
    pub fx: String,
    /// The property names carrying the `artistEditable` annotation.
    pub editable: Vec<String>,
}

impl EffectProperties {
    /// The editable names of an effect, name-sorted (the reference's
    /// `BW::map` iteration order); empty for a foreign effect.
    fn editable(&self, fx: &str) -> Vec<String> {
        if self.fx != fx {
            return Vec::new();
        }
        let mut names = self.editable.clone();
        names.sort();
        names
    }
}

/// A material gathered from the visual: the reference `MaterialInfo` minus
/// the live effect objects. `data` holds one path per `material` section
/// sharing the identifier, in visual order.
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialEntry {
    /// The display name: reset to the identifier on every census encounter,
    /// then replaced by the `materialNames` alias when one matched.
    pub name: String,
    /// The `original` field of the alias child standing for this material,
    /// when the census found one (`nameData_`).
    pub name_data: Option<String>,
    /// Paths of the material sections (into the visual).
    pub data: Vec<SectionPath>,
    /// The source vertex format of the last geometry listing this material
    /// (`VerticesManager::get(vertGroup)->sourceFormat()`; assignment, so
    /// the last geometry wins).
    pub format: String,
    /// The reference accumulates `colours || dualUV` — the stream census
    /// computes `hasColours` but the assignment uses `dualUV`
    /// (`mutant.cpp:1116`), so this mirrors [`MaterialEntry::dual_uv`]
    /// rather than ever observing a colour stream. Ported verbatim.
    pub colours: bool,
    pub dual_uv: bool,
}

/// A tint gathered from a model dye: the reference `TintInfo` minus the
/// live effect and fashion. The synthesized `Default` tint has no data,
/// which is what every "is it a real tint?" check in the reference keys off.
#[derive(Debug, Clone, PartialEq)]
pub struct TintEntry {
    /// Path of the `dye/tint` section (into the model); `None` for
    /// `Default`.
    pub data: Option<SectionPath>,
    /// Copied from the tinted material (`tints_[matter][name].format =
    /// materials_[replaces].format`).
    pub format: String,
    pub colours: bool,
    pub dual_uv: bool,
}

/// A child-index path into a document. Structural edits above the target
/// invalidate paths; the structural moves therefore re-run the census.
pub type SectionPath = Vec<usize>;

/// `DataSection::openSection` by child indices instead of tags.
fn section_at<'a>(root: &'a DataSection, path: &[usize]) -> Option<&'a DataSection> {
    let mut section = root;
    for index in path {
        section = section.open_child(*index)?;
    }
    Some(section)
}

/// The mutable twin of [`section_at`].
fn section_at_mut<'a>(root: &'a mut DataSection, path: &[usize]) -> Option<&'a mut DataSection> {
    let mut section = root;
    for index in path {
        section = section.child_mut(*index)?;
    }
    Some(section)
}

/// Read through either document: material paths resolve in the visual, dye
/// and tint paths in the model, and both trees start at the root — so
/// trying the visual first and falling through to the model covers both.
fn section_at_any<'a>(editor: &'a MaterialEditor, path: &[usize]) -> Option<&'a DataSection> {
    section_at(&editor.visual, path).or_else(|| section_at(&editor.model, path))
}

/// Mutable twin of [`section_at_any`]: the path resolves against whichever
/// tree actually holds the section.
fn section_at_mut_any<'a>(
    editor: &'a mut MaterialEditor,
    path: &[usize],
) -> Option<&'a mut DataSection> {
    if section_at(&editor.visual, path).is_some() {
        return section_at_mut(&mut editor.visual, path);
    }
    section_at_mut(&mut editor.model, path)
}

/// `DataSection::readStrings(tag)`: every same-named child's string value.
fn read_strings(section: &DataSection, tag: &str) -> Vec<String> {
    open_sections(section, tag)
        .into_iter()
        .map(|child| child.as_string().to_string())
        .collect()
}

/// The index of the first child named `tag`.
fn child_index_named(section: &DataSection, tag: &str) -> Option<usize> {
    (0..section.count_children()).find(|&index| {
        section
            .open_child(index)
            .is_some_and(|child| child.section_name() == tag)
    })
}

/// Every index of the children named `tag`.
fn children_named(section: &DataSection, tag: &str) -> Vec<usize> {
    (0..section.count_children())
        .filter(|&index| {
            section
                .open_child(index)
                .is_some_and(|child| child.section_name() == tag)
        })
        .collect()
}

/// The child-index path from a tree root to a borrowed section, searched
/// by pointer identity. Census-time only: the trees are small.
fn child_path_of(root: &DataSection, target: &DataSection) -> Option<SectionPath> {
    for index in 0..root.count_children() {
        let Some(child) = root.open_child(index) else {
            continue;
        };
        if std::ptr::eq(child, target) {
            return Some(vec![index]);
        }
        if let Some(mut sub) = child_path_of(child, target) {
            let mut path = Vec::with_capacity(sub.len() + 1);
            path.push(index);
            path.append(&mut sub);
            return Some(path);
        }
    }
    None
}

/// The child-index path of a tint section borrowed from the model.
fn tint_path(model: &DataSection, dye_index: usize, tint: &DataSection) -> SectionPath {
    let mut path = vec![dye_index];
    if let Some(dye) = model.open_child(dye_index) {
        for index in 0..dye.count_children() {
            if dye
                .open_child(index)
                .is_some_and(|child| std::ptr::eq(child, tint))
            {
                path.push(index);
                break;
            }
        }
    }
    path
}

/// The gathered material state of one opened model (`Mutant`'s material
/// tables). The documents are owned; edits land in them and the caller
/// saves through the model mutator's flows.
pub struct MaterialEditor {
    /// The `.model` document (`currModel_`).
    pub model: DataSection,
    /// The `.visual` document (`currVisual_`).
    pub visual: DataSection,
    /// The primitives reference the bare vertices part names resolve
    /// against (`primitivesName_`).
    pub primitives_name: String,
    /// `materials_`: identifier → entry.
    pub materials: BTreeMap<String, MaterialEntry>,
    /// `tints_`: matter → tint name → entry. The synthesized `Default`
    /// tint sits here without data.
    pub tints: BTreeMap<String, BTreeMap<String, TintEntry>>,
    /// `dyes_`: matter → path of the `dye` section. Recorded even when the
    /// dye replaces an unknown material (only the tints are gated).
    pub dyes: BTreeMap<String, SectionPath>,
    /// `currDyes_`: matter → the tint currently applied. Survives a
    /// recensus, like the reference's member.
    pub curr_dyes: BTreeMap<String, String>,
    /// `tintedMaterials_`: material identifier → the matter that tints it.
    pub tinted_materials: BTreeMap<String, String>,
    /// Indices — among the `material` children of `materialNames` — of
    /// alias children naming no known material
    /// (`materialNameDataToRemove_`); [`MaterialEditor::
    /// clean_material_names`] deletes them highest index first.
    pub names_to_remove: Vec<usize>,
    /// The save marks (`Mutant::dirty`), keyed by document.
    pub dirty: std::collections::BTreeSet<String>,
}

impl MaterialEditor {
    /// `Mutant::regenMaterialsList` (`mutant.cpp:996`): repair missing BSP
    /// materials, walk the visual's render sets grouping materials by
    /// identifier, apply the `materialNames` aliases, and gather the dyes
    /// and tints. The live-effect loads (`effect->load`, the `materialList_`
    /// GUI tree) are the render seam and are not carried.
    pub fn census(
        resource: &BwResource,
        model: DataSection,
        visual: DataSection,
        primitives_name: &str,
    ) -> Self {
        let mut editor = Self {
            model,
            visual,
            primitives_name: primitives_name.to_string(),
            materials: BTreeMap::new(),
            tints: BTreeMap::new(),
            dyes: BTreeMap::new(),
            curr_dyes: BTreeMap::new(),
            tinted_materials: BTreeMap::new(),
            names_to_remove: Vec::new(),
            dirty: std::collections::BTreeSet::new(),
        };

        // Missing BSP materials: one new primitiveGroup>material on the
        // first renderSet's geometry per marker, then the markers go. (The
        // reference crashes into a null section when the visual has no
        // renderSet/geometry at all; the port skips the repair and still
        // deletes the markers.)
        let missing: Vec<String> = open_sections(&editor.visual, "missingBspMaterial")
            .into_iter()
            .map(|marker| marker.read_string("material/identifier", ""))
            .collect();
        if !missing.is_empty() {
            let geometry_path: Option<SectionPath> = (|| {
                let rs_index = child_index_named(&editor.visual, "renderSet")?;
                let rs = editor.visual.open_child(rs_index)?;
                let geometry_index = child_index_named(rs, "geometry")?;
                Some(vec![rs_index, geometry_index])
            })();
            if let Some(Some(geometry)) =
                geometry_path.map(|path| section_at_mut(&mut editor.visual, &path))
            {
                for identifier in &missing {
                    let group = geometry.add_child("primitiveGroup");
                    let material = group.add_child("material");
                    material.write_string("identifier", identifier);
                    material.write_int("collisionFlags", 0);
                    material.write_int("materialKind", 0);
                }
            }
            editor.visual.delete_sections("missingBspMaterial");
            editor.mark_dirty("visual");
        }

        // The visual walk: renderSet → geometry → primitiveGroup → material.
        for render_set in open_sections(&editor.visual, "renderSet") {
            for geometry in open_sections(render_set, "geometry") {
                // The vertices part this geometry draws from: a bare part
                // name rides on the primitives reference.
                let mut vert_group = geometry.read_string("vertices", "");
                if !vert_group.contains('/') {
                    vert_group = format!("{}/{}", primitives_name, vert_group);
                }

                // Dual UV comes off the stream names' suffix after the
                // last dot; `hasColours` is computed in the reference but
                // never reaches the table (see [`MaterialEntry::colours`]).
                let mut dual_uv = false;
                for stream in read_strings(geometry, "stream") {
                    let suffix = match stream.rfind('.') {
                        Some(dot) => &stream[dot + 1..],
                        None => stream.as_str(),
                    };
                    if suffix == UV2_STREAM {
                        dual_uv = true;
                    }
                }

                let format = source_format(resource, &vert_group, primitives_name);

                for group in open_sections(geometry, "primitiveGroup") {
                    for material in open_sections(group, "material") {
                        let identifier = material.read_string("identifier", "");
                        let entry =
                            editor
                                .materials
                                .entry(identifier.clone())
                                .or_insert_with(|| MaterialEntry {
                                    name: identifier.clone(),
                                    name_data: None,
                                    data: Vec::new(),
                                    format: String::new(),
                                    colours: false,
                                    dual_uv: false,
                                });
                        // The name resets on every encounter; only the
                        // alias pass below may change it.
                        entry.name = identifier.clone();
                        entry.data.push(
                            child_path_of(&editor.visual, material)
                                .expect("section borrowed from this tree"),
                        );
                        entry.format = format.clone();
                        entry.dual_uv = entry.dual_uv || dual_uv;
                        // Reference bug verbatim: `colours || dualUV`.
                        entry.colours = entry.colours || dual_uv;
                    }
                }
            }
        }

        // The material name aliases. The reference's
        // `openSection("materialNames", true)` CREATES the section when the
        // model lacks one — the census itself mutates the document, and a
        // later save writes the empty section out.
        let _ = editor.model.open_or_make_section("materialNames");
        let names_index =
            child_index_named(&editor.model, "materialNames").expect("created just above");
        let mut removals: Vec<usize> = Vec::new();
        let mut material_children = 0usize;
        let names = editor
            .model
            .open_child(names_index)
            .expect("created just above");
        for child_index in 0..names.count_children() {
            let Some(child) = names.open_child(child_index) else {
                continue;
            };
            if child.section_name() != "material" {
                continue;
            }
            let original = child.read_string("original", "");
            let display = child.read_string("display", &original);
            match editor.materials.get_mut(&original) {
                Some(entry) => {
                    entry.name = display;
                    entry.name_data = Some(original);
                }
                None => removals.push(material_children),
            }
            material_children += 1;
        }
        editor.names_to_remove = removals;

        // The dyes and their tints.
        for dye_index in children_named(&editor.model, "dye") {
            let Some(dye) = editor.model.open_child(dye_index) else {
                continue;
            };
            let replaces = dye.read_string("replaces", "");
            let matter = dye.read_string("matter", "");
            editor.dyes.insert(matter.clone(), vec![dye_index]);

            // Tints gather only when the dye replaces a known material
            // (the reference's `materials_.find(replaces)` gate).
            let Some(tinted) = editor
                .materials
                .get(&replaces)
                .map(|entry| (entry.format.clone(), entry.colours, entry.dual_uv))
            else {
                continue;
            };
            let mut per_matter = BTreeMap::new();
            for tint in open_sections(dye, "tint") {
                let name = tint.read_string("name", "");
                per_matter.insert(
                    name,
                    TintEntry {
                        data: Some(tint_path(&editor.model, dye_index, tint)),
                        format: tinted.0.clone(),
                        colours: tinted.1,
                        dual_uv: tinted.2,
                    },
                );
            }
            // The synthesized Default tint: no data of its own.
            per_matter.insert(
                "Default".to_string(),
                TintEntry {
                    data: None,
                    format: tinted.0.clone(),
                    colours: tinted.1,
                    dual_uv: tinted.2,
                },
            );
            editor.tints.insert(matter.clone(), per_matter);
            editor.tinted_materials.insert(replaces, matter);
        }

        editor
    }

    /// The save mark (`Mutant::dirty`), keyed by document.
    pub fn mark_dirty(&mut self, which: &str) {
        self.dirty.insert(which.to_string());
    }

    /// `Mutant::materialDisplayName`: the display name, empty for an
    /// unknown identifier (the reference's `operator[]` default-insert
    /// yields an entry with an empty `name`).
    pub fn material_display_name(&self, material_name: &str) -> String {
        self.materials
            .get(material_name)
            .map(|entry| entry.name.clone())
            .unwrap_or_default()
    }

    /// `Mutant::getTintName`: the applied tint of a matter, `Default` when
    /// none is applied.
    pub fn get_tint_name(&self, matter_name: &str) -> String {
        self.curr_dyes
            .get(matter_name)
            .cloned()
            .unwrap_or_else(|| "Default".to_string())
    }

    /// `Mutant::setDye`: record the applied tint. The effect handoff and
    /// the fashion rebuild are the render seam; the port carries the table
    /// write only.
    pub fn set_dye(&mut self, matter_name: &str, tint_name: &str) {
        self.curr_dyes
            .insert(matter_name.to_string(), tint_name.to_string());
    }

    /// `Mutant::tintNames`: the tint names of a matter, name order.
    pub fn tint_names(&self, matter_name: &str) -> Vec<String> {
        self.tints
            .get(matter_name)
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// `Mutant::instantiateMFM`: expand a legacy `mfm` reference in place.
    /// The mfm file's content replaces the section, with the section's
    /// identifier and fx winning, the collision flags and material kind
    /// carried across, and matching properties copied over (a repeated
    /// source name copies repeatedly, last copy wins — the reference's
    /// unbroken double loop). A missing reference or file leaves the
    /// section alone. The reference's `BWResource::openSection` binary
    /// fallback is not carried: an unparseable mfm behaves as unopenable.
    pub fn instantiate_mfm(&mut self, path: &[usize], resource: &BwResource) {
        let mfm_file = match section_at_any(self, path) {
            Some(section) => section.read_string("mfm", ""),
            None => return,
        };
        if mfm_file.is_empty() {
            return;
        }
        let Some(raw) = resource.file_system().read_file(&mfm_file) else {
            return;
        };
        let Ok(mut mfm_data) = datasection::parse(bwresource::get_filename(&mfm_file), &raw) else {
            return;
        };

        // The overrides and flags come off the referencing section.
        let Some(section) = section_at_any(self, path) else {
            return;
        };
        let identifier = section.read_string("identifier", "");
        let fx = section.read_string("fx", "");
        let collision_flags = section.read_int("collisionFlags", 0);
        let material_kind = section.read_int("materialKind", 0);
        let src_props: Vec<(String, DataSection)> = open_sections(section, "property")
            .into_iter()
            .map(|prop| (prop.as_string().to_string(), prop.clone()))
            .collect();

        if !identifier.is_empty() {
            mfm_data.write_string("identifier", &identifier);
        }
        if !fx.is_empty() {
            mfm_data.write_string("fx", &fx);
        }
        mfm_data.write_int("collisionFlags", collision_flags);
        mfm_data.write_int("materialKind", material_kind);

        for dest_index in 0..mfm_data.count_children() {
            let matches = mfm_data
                .open_child(dest_index)
                .is_some_and(|dest| dest.section_name() == "property");
            if !matches {
                continue;
            }
            let dest_name = mfm_data
                .open_child(dest_index)
                .map(|dest| dest.as_string().to_string())
                .unwrap_or_default();
            for (src_name, src_prop) in &src_props {
                if *src_name == dest_name
                    && let Some(prop) = mfm_data.child_mut(dest_index)
                {
                    prop.copy_from(src_prop);
                }
            }
        }

        if let Some(section) = section_at_mut_any(self, path) {
            section.copy_from(&mfm_data);
            section.del_child("mfm");
        }
    }

    /// `Mutant::overloadMFM`: layer an mfm over a material section — the
    /// fx (when the mfm names one), the flags always, and every mfm
    /// property (matching by name, appending the ones the material lacks).
    pub fn overload_mfm(&mut self, path: &[usize], mfm_data: &DataSection) {
        let Some(section) = section_at_mut_any(self, path) else {
            return;
        };
        let temp = mfm_data.read_string("fx", "");
        if !temp.is_empty() {
            section.write_string("fx", &temp);
        }
        section.write_int("collisionFlags", mfm_data.read_int("collisionFlags", 0));
        section.write_int("materialKind", mfm_data.read_int("materialKind", 0));

        let dest_props: Vec<(usize, String)> = (0..section.count_children())
            .filter(|&index| {
                section
                    .open_child(index)
                    .is_some_and(|child| child.section_name() == "property")
            })
            .map(|index| {
                (
                    index,
                    section
                        .open_child(index)
                        .map(|child| child.as_string().to_string())
                        .unwrap_or_default(),
                )
            })
            .collect();
        let src_props: Vec<(String, DataSection)> = open_sections(mfm_data, "property")
            .into_iter()
            .map(|prop| (prop.as_string().to_string(), prop.clone()))
            .collect();
        for (src_name, src_prop) in &src_props {
            let mut placed = false;
            for (dest_index, dest_name) in &dest_props {
                if dest_name == src_name {
                    placed = true;
                    if let Some(prop) = section.child_mut(*dest_index) {
                        prop.copy_from(src_prop);
                    }
                }
            }
            if !placed {
                section.add_child("property").copy_from(src_prop);
            }
        }
    }

    /// `Mutant::cleanMaterials`: rebuild every material section from its
    /// own pre-clean state — identifier, every `fx` reference, the flags,
    /// then one copy of each artist-editable property the section used to
    /// carry (shared properties of multi-layer effects land once). Marks
    /// the visual dirty.
    pub fn clean_materials(&mut self, resource: &BwResource, effects: &EffectProperties) {
        let names: Vec<String> = self.materials.keys().cloned().collect();
        for material_name in names {
            let data = match self.materials.get(&material_name) {
                Some(entry) => entry.data.clone(),
                None => continue,
            };
            if data.is_empty() {
                continue;
            }

            self.instantiate_mfm(&data[0], resource);

            // The backup of the old material data (the reference's
            // XMLSection "old_state").
            let backup = match section_at_any(self, &data[0]) {
                Some(section) => section.clone(),
                None => continue,
            };
            let fxs = read_strings(&backup, "fx");

            // Erase and rewrite the fixed fields on every section.
            for section_path in &data {
                let Some(section) = section_at_mut(&mut self.visual, section_path) else {
                    continue;
                };
                section.del_children();
                let temp = backup.read_string("identifier", "");
                if !temp.is_empty() {
                    section.write_string("identifier", &temp);
                }
                for fx in &fxs {
                    section.add_child("fx").set_string(fx);
                }
                section.write_int("collisionFlags", backup.read_int("collisionFlags", 0));
                section.write_int("materialKind", backup.read_int("materialKind", 0));
            }

            // The editable properties the backup carried, once each, on
            // every section. The reference iterates one effect's property
            // map; the port walks each fx's editable list (name order) and
            // dedupes across fxs (the reference's `existingProps`).
            let mut existing_props: Vec<String> = Vec::new();
            for fx in &fxs {
                for desc_name in effects.editable(fx) {
                    let first_match: Option<DataSection> = open_sections(&backup, "property")
                        .into_iter()
                        .find(|prop| prop.as_string() == desc_name)
                        .cloned();
                    // The reference adds the first match and skips the
                    // rest through the `existingProps` dedupe.
                    if existing_props.contains(&desc_name) {
                        continue;
                    }
                    let Some(src_prop) = first_match else {
                        continue;
                    };
                    for section_path in &data {
                        if let Some(section) = section_at_mut(&mut self.visual, section_path) {
                            section.add_child("property").copy_from(&src_prop);
                        }
                    }
                    existing_props.push(desc_name);
                }
            }
        }
        self.mark_dirty("visual");
    }

    /// `Mutant::cleanTints`: the same rebuild for every tint's `material`
    /// child — without the multi-layer dedup the material face applies
    /// (every matching backup property lands). Marks the model dirty.
    pub fn clean_tints(&mut self, resource: &BwResource, effects: &EffectProperties) {
        let matters: Vec<String> = self.tints.keys().cloned().collect();
        for matter in matters {
            let names = self
                .tints
                .get(&matter)
                .map(|map| map.keys().cloned().collect::<Vec<String>>())
                .unwrap_or_default();
            for tint_name in names {
                let material_path = match self.tint_material_path(&matter, &tint_name) {
                    Some(path) => path,
                    None => continue,
                };
                self.instantiate_mfm(&material_path, resource);

                let backup = match section_at_any(self, &material_path) {
                    Some(section) => section.clone(),
                    None => continue,
                };
                let fxs = read_strings(&backup, "fx");

                let Some(section) = section_at_mut_any(self, &material_path) else {
                    continue;
                };
                section.del_children();
                let temp = backup.read_string("identifier", "");
                if !temp.is_empty() {
                    section.write_string("identifier", &temp);
                }
                for fx in &fxs {
                    section.add_child("fx").set_string(fx);
                }
                section.write_int("collisionFlags", backup.read_int("collisionFlags", 0));
                section.write_int("materialKind", backup.read_int("materialKind", 0));

                // No dedupe here: the reference's tint face copies every
                // matching backup property.
                for fx in &fxs {
                    for desc_name in effects.editable(fx) {
                        for src_prop in open_sections(&backup, "property") {
                            if desc_name == src_prop.as_string() {
                                section.add_child("property").copy_from(src_prop);
                            }
                        }
                    }
                }
            }
        }
        self.mark_dirty("model");
    }

    /// `Mutant::cleanMaterialNames`: delete the alias children whose
    /// originals named no material, highest index first. The census always
    /// creates `materialNames`, so the missing-section case only fires
    /// before any census ran.
    pub fn clean_material_names(&mut self) {
        if self.names_to_remove.is_empty() {
            return;
        }
        let Some(names_index) = child_index_named(&self.model, "materialNames") else {
            return;
        };
        let material_indices = children_named(
            self.model.open_child(names_index).expect("child exists"),
            "material",
        );
        let mut to_remove: Vec<usize> = self
            .names_to_remove
            .iter()
            .filter_map(|index| material_indices.get(*index).copied())
            .collect();
        to_remove.sort_unstable();
        to_remove.reverse();
        if let Some(names) = self.model.child_mut(names_index) {
            for index in to_remove {
                names.del_child_at(index);
            }
        }
        self.names_to_remove.clear();
        self.mark_dirty("model");
    }

    /// `Mutant::setMaterialProperty`: write one property on every section
    /// of a material, under `TextureFeed/default` when the property carries
    /// a texture feed and under `prop_type` otherwise; a section without
    /// the property gains it. Values dissolve through the resource paths.
    /// An unknown material is a no-op that still reports success (the
    /// reference's `operator[]` default-insert hands back an empty entry).
    pub fn set_material_property(
        &mut self,
        resource: &BwResource,
        material_name: &str,
        desc_name: &str,
        prop_type: &str,
        val: &str,
    ) -> bool {
        let data = self
            .materials
            .get(material_name)
            .map(|entry| entry.data.clone())
            .unwrap_or_default();
        let dissolved = resource.dissolve_filename(val);
        for section_path in &data {
            let Some(section) = section_at_mut(&mut self.visual, section_path) else {
                continue;
            };
            let mut done = false;
            for prop_index in 0..section.count_children() {
                let matches = section.child_mut(prop_index).is_some_and(|prop| {
                    prop.section_name() == "property" && prop.as_string() == desc_name
                });
                if !matches {
                    continue;
                }
                let Some(prop) = section.child_mut(prop_index) else {
                    continue;
                };
                if prop.open_section("TextureFeed").is_some() {
                    prop.write_string("TextureFeed/default", &dissolved);
                } else {
                    prop.write_string(prop_type, &dissolved);
                }
                done = true;
            }
            if !done {
                let prop = section.add_child("property");
                prop.set_string(desc_name);
                prop.write_string(prop_type, &dissolved);
            }
        }
        true
    }

    /// `Mutant::setTintProperty`: the same write on a tint's `material`
    /// child, plus the exposed-property `default` Vector4 on every matching
    /// `property` child of the tint section itself. A `Default` tint
    /// (null data) or a missing material child refuses.
    pub fn set_tint_property(
        &mut self,
        resource: &BwResource,
        matter_name: &str,
        tint_name: &str,
        desc_name: &str,
        prop_type: &str,
        val: &str,
    ) -> bool {
        let Some(data) = self
            .tints
            .get(matter_name)
            .and_then(|map| map.get(tint_name))
            .and_then(|entry| entry.data.clone())
        else {
            return false;
        };
        let Some(material_path) = self.tint_material_path(matter_name, tint_name) else {
            return false;
        };

        self.instantiate_mfm(&material_path, resource);
        let dissolved = resource.dissolve_filename(val);
        let exposed = self.get_exposed_vector4(matter_name, tint_name, desc_name, prop_type, val);
        let Some(section) = section_at_mut(&mut self.model, &material_path) else {
            return false;
        };
        let mut done = false;
        for prop_index in 0..section.count_children() {
            let matches = section.child_mut(prop_index).is_some_and(|prop| {
                prop.section_name() == "property" && prop.as_string() == desc_name
            });
            if !matches {
                continue;
            }
            let Some(prop) = section.child_mut(prop_index) else {
                continue;
            };
            if prop.open_section("TextureFeed").is_some() {
                prop.write_string("TextureFeed/default", &dissolved);
            } else {
                prop.write_string(prop_type, &dissolved);
            }
            done = true;
        }
        if !done {
            let prop = section.add_child("property");
            prop.set_string(desc_name);
            prop.write_string(prop_type, &dissolved);
        }
        let _ = section;

        // The exposed-property defaults ride along so the python face
        // stays in step with the material.
        let Some(tint) = section_at_mut(&mut self.model, &data) else {
            return true;
        };
        for prop_index in 0..tint.count_children() {
            let matches = tint
                .open_child(prop_index)
                .is_some_and(|prop| prop.read_string("name", "") == desc_name);
            if matches
                && let Some(prop) = tint.child_mut(prop_index)
                && let Some(default) = prop.open_or_make_section("default")
            {
                default.set_vector4(exposed);
            }
        }
        true
    }

    /// `Mutant::materialName`: rename a material's display name through its
    /// `materialNames` alias. Same-name exits true, a name another material
    /// holds refuses, and an unknown identifier joins the table (the
    /// reference's `operator[]` default-insert) and still records the
    /// alias.
    pub fn material_name(&mut self, material_name: &str, new_name: &str) -> bool {
        // The operator[] default-insert: an unknown material joins with an
        // empty name.
        if !self.materials.contains_key(material_name) {
            self.materials.insert(
                material_name.to_string(),
                MaterialEntry {
                    name: String::new(),
                    name_data: None,
                    data: Vec::new(),
                    format: String::new(),
                    colours: false,
                    dual_uv: false,
                },
            );
        }
        if self
            .materials
            .get(material_name)
            .is_some_and(|entry| entry.name == new_name)
        {
            return true;
        }
        if self.materials.values().any(|entry| entry.name == new_name) {
            return false;
        }

        // The alias child: an existing one is rewritten, a fresh one is
        // appended under `materialNames` (the census usually made it).
        let names_index = match child_index_named(&self.model, "materialNames") {
            Some(index) => index,
            None => {
                self.model.add_child("materialNames");
                self.model.count_children() - 1
            }
        };
        let alias_original = self
            .materials
            .get(material_name)
            .and_then(|entry| entry.name_data.clone());
        if let Some(original) = alias_original {
            let mut target: Option<usize> = None;
            if let Some(names) = self.model.open_child(names_index) {
                for child_index in 0..names.count_children() {
                    let matches = names
                        .open_child(child_index)
                        .is_some_and(|child| child.read_string("original", "") == original);
                    if matches {
                        target = Some(child_index);
                        break;
                    }
                }
            }
            if let Some(child_index) = target
                && let Some(names) = self.model.child_mut(names_index)
                && let Some(child) = names.child_mut(child_index)
            {
                child.write_string("original", material_name);
                child.write_string("display", new_name);
            }
        } else if let Some(names) = self.model.child_mut(names_index) {
            let child = names.add_child("material");
            child.write_string("original", material_name);
            child.write_string("display", new_name);
        }

        let entry = self
            .materials
            .get_mut(material_name)
            .expect("default-inserted above");
        entry.name = new_name.to_string();
        if entry.name_data.is_none() {
            entry.name_data = Some(material_name.to_string());
        }

        // The tinted-material map re-keys to the new name (the reference
        // copies then erases).
        if let Some(matter) = self.tinted_materials.remove(material_name) {
            self.tinted_materials.insert(new_name.to_string(), matter);
        }
        true
    }

    /// `Mutant::matterName`: move a dye to a new matter name — the dye
    /// section's `matter` field, the dye/tint/curr-dye tables, and every
    /// tinted-material entry pointing at the old name. The undo-free call
    /// skips the duplicate check (the undo replay). An unknown matter
    /// still runs the census and reports success, like the reference (the
    /// reference's per-part moves are all conditional, then it always
    /// reloads the lists).
    pub fn matter_name(
        &mut self,
        resource: &BwResource,
        matter_name: &str,
        new_name: &str,
        undo_redo: bool,
    ) -> bool {
        if matter_name == new_name {
            return true;
        }
        if undo_redo && self.dyes.contains_key(new_name) {
            return false;
        }
        if let Some(tint) = self.curr_dyes.remove(matter_name) {
            self.curr_dyes.insert(new_name.to_string(), tint);
        }
        if let Some(dye_path) = self.dyes.get(matter_name).cloned() {
            if let Some(dye) = section_at_mut(&mut self.model, &dye_path) {
                dye.write_string("matter", new_name);
            }
            let dye = self.dyes.remove(matter_name).expect("checked above");
            self.dyes.insert(new_name.to_string(), dye);
        }
        if let Some(map) = self.tints.remove(matter_name) {
            self.tints.insert(new_name.to_string(), map);
        }
        for target in self.tinted_materials.values_mut() {
            if target == matter_name {
                *target = new_name.to_string();
            }
        }
        self.recensus(resource);
        true
    }

    /// `Mutant::tintName`: rename a tint within its matter — the tint
    /// section's `name` field, the table, and the applied-dye record (set
    /// unconditionally when the matter is applied, like the reference; the
    /// old value is not compared). The undo-free call skips the duplicate
    /// check (the undo replay); an unknown dye or tint refuses here, where
    /// the reference dereferences a null section (documented divergence).
    pub fn tint_name(
        &mut self,
        resource: &BwResource,
        matter_name: &str,
        tint_name: &str,
        new_name: &str,
        undo_redo: bool,
    ) -> bool {
        if tint_name == new_name {
            return true;
        }
        if undo_redo && self.tints.values().any(|map| map.contains_key(new_name)) {
            return false;
        }
        let Some(data) = self
            .tints
            .get(matter_name)
            .and_then(|map| map.get(tint_name))
            .and_then(|entry| entry.data.clone())
        else {
            return false;
        };
        if self.curr_dyes.contains_key(matter_name) {
            self.curr_dyes
                .insert(matter_name.to_string(), new_name.to_string());
        }
        if let Some(tint) = section_at_mut(&mut self.model, &data) {
            tint.write_string("name", new_name);
        }
        if let Some(map) = self.tints.get_mut(matter_name)
            && let Some(entry) = map.remove(tint_name)
        {
            map.insert(new_name.to_string(), entry);
        }
        self.recensus(resource);
        true
    }

    /// `Mutant::newTint` (`mutant_materials.cpp:700`): add a tint. An empty
    /// matter opens a new dye whose matter name derives from the material
    /// (`Utilities::pythonSafeName`); otherwise the tint joins the named
    /// dye. The material face copies the material's own data (or the old
    /// tint's material), the mfm face copies the file content; `fx_file`
    /// wins over `mfm_file`. Returns the (possibly new) matter name — empty
    /// when the mfm file will not open, and for an unknown matter, where
    /// the reference dereferences a null dye section (documented
    /// divergence).
    // The reference signature carries the same eight parameters.
    #[allow(clippy::too_many_arguments)]
    pub fn new_tint(
        &mut self,
        resource: &BwResource,
        material_name: &str,
        matter_name: &str,
        old_tint_name: &str,
        new_tint_name: &str,
        fx_file: &str,
        mfm_file: &str,
    ) -> String {
        let mut mfm_data: Option<DataSection> = None;
        if !mfm_file.is_empty() {
            match resource
                .file_system()
                .read_file(mfm_file)
                .map(|raw| datasection::parse(bwresource::get_filename(mfm_file), &raw))
            {
                Some(Ok(data)) => mfm_data = Some(data),
                // ERROR_MSG "Cannot open MFM file: %s"
                _ => return String::new(),
            }
        }

        // The copy sources, snapshotted before the model tree is touched.
        let material_face =
            !fx_file.is_empty() && (matter_name.is_empty() || old_tint_name == "Default");
        let material_snapshot: Option<DataSection> = if material_face {
            self.materials
                .get(material_name)
                .and_then(|entry| entry.data.first().cloned())
                .and_then(|path| section_at(&self.visual, &path))
                .cloned()
        } else {
            None
        };
        let tint_snapshot: Option<DataSection> = if !fx_file.is_empty() && !material_face {
            self.tints
                .get(matter_name)
                .and_then(|map| map.get(old_tint_name))
                .and_then(|entry| entry.data.clone())
                .and_then(|path| section_at(&self.model, &path))
                .and_then(|tint| tint.open_section("material"))
                .cloned()
        } else {
            None
        };

        if matter_name.is_empty() {
            let new_matter = python_safe_name(material_name);
            let dye = self.model.add_child("dye");
            dye.write_string("matter", &new_matter);
            dye.write_string("replaces", material_name);
            let dye_path = vec![self.model.count_children() - 1];
            self.new_tint_into(
                &dye_path,
                new_tint_name,
                material_snapshot.as_ref(),
                tint_snapshot.as_ref(),
                mfm_data.as_ref(),
                fx_file,
            );
            self.recensus(resource);
            return new_matter;
        }
        let Some(dye_path) = self.dyes.get(matter_name).cloned() else {
            return String::new();
        };
        self.new_tint_into(
            &dye_path,
            new_tint_name,
            material_snapshot.as_ref(),
            tint_snapshot.as_ref(),
            mfm_data.as_ref(),
            fx_file,
        );
        self.recensus(resource);
        matter_name.to_string()
    }

    /// The shared tail of [`MaterialEditor::new_tint`]: append the tint
    /// with its material child under an existing dye.
    fn new_tint_into(
        &mut self,
        dye_path: &[usize],
        new_tint_name: &str,
        material_snapshot: Option<&DataSection>,
        tint_snapshot: Option<&DataSection>,
        mfm_data: Option<&DataSection>,
        fx_file: &str,
    ) {
        let Some(dye) = section_at_mut(&mut self.model, dye_path) else {
            return;
        };
        let tint = dye.add_child("tint");
        tint.write_string("name", new_tint_name);
        let material = tint.add_child("material");
        if !fx_file.is_empty() {
            if let Some(snapshot) = material_snapshot.or(tint_snapshot) {
                material.copy_from(snapshot);
            }
            material.write_string("fx", fx_file);
        } else if let Some(data) = mfm_data {
            material.copy_from(data);
        }
    }

    /// `Mutant::deleteTint`: remove a tint; the dye goes when its last tint
    /// does; the applied-dye record goes either way (the reference erases
    /// `currDyes_[matter]` unconditionally). Missing dye or tint is a
    /// no-op.
    pub fn delete_tint(&mut self, resource: &BwResource, matter_name: &str, tint_name: &str) {
        let Some(dye_path) = self.dyes.get(matter_name).cloned() else {
            return;
        };
        let Some(data) = self
            .tints
            .get(matter_name)
            .and_then(|map| map.get(tint_name))
            .and_then(|entry| entry.data.clone())
        else {
            return;
        };
        if data.len() < 2 {
            return;
        }
        if let Some(dye) = section_at_mut(&mut self.model, &dye_path) {
            dye.del_child_at(data[1]);
        }
        let no_tints =
            section_at(&self.model, &dye_path).is_some_and(|dye| dye.find_child("tint").is_none());
        if no_tints {
            self.model.del_child_at(dye_path[0]);
        }
        self.curr_dyes.remove(matter_name);
        self.recensus(resource);
    }

    /// `Mutant::saveMFM`: export a material or tint as an `.mfm` file. The
    /// material face instantiates every section and exports the last one
    /// (the reference's `data` variable holds the final loop iteration);
    /// the tint face instantiates the tint's material child. The exported
    /// file drops the `identifier` and its root tag is `root` (the
    /// reference's `createFromFile(name, true)` empty-section name).
    /// Returns false when the material, tint or material child is missing,
    /// or the file will not write.
    pub fn save_mfm(
        &mut self,
        resource: &BwResource,
        material_name: &str,
        matter_name: &str,
        tint_name: &str,
        mfm_file: &str,
    ) -> bool {
        let source: SectionPath = if matter_name.is_empty() || tint_name == "Default" {
            let data = match self
                .materials
                .get(material_name)
                .map(|entry| entry.data.clone())
            {
                Some(data) if !data.is_empty() => data,
                _ => return false,
            };
            for path in &data {
                self.instantiate_mfm(path, resource);
            }
            data.last().cloned().expect("checked above")
        } else {
            let Some(material_path) = self.tint_material_path(matter_name, tint_name) else {
                return false;
            };
            self.instantiate_mfm(&material_path, resource);
            material_path
        };

        let Some(section) = section_at_any(self, &source) else {
            return false;
        };
        let mut mfm = DataSection::new("root");
        mfm.copy_from(section);
        mfm.del_child("identifier");
        if resource.save_section_if_exists(mfm_file, &mut mfm) {
            return true;
        }
        resource.save_section(mfm_file, &mut mfm)
    }

    /// `Mutant::ensureShaderCorrect` (`mutant_materials.cpp:855`): the
    /// skinned/UV compatibility checks. An empty shader applies to
    /// anything. The skinned families are read off the shader file name;
    /// an unskinned shader on a skinned object only warns (the reference's
    /// OKCANCEL prompt is treated as accepted); the compiled effect's
    /// dual-UV answer arrives as `fx_dual_uv` (the D3DX seam; `None` is the
    /// reference's unopenable file, which only logs and still applies).
    pub fn ensure_shader_correct(
        &self,
        fx_file: &str,
        format: &str,
        dual_uv: bool,
        fx_dual_uv: Option<bool>,
    ) -> bool {
        if fx_file.is_empty() {
            return true;
        }
        let softskinned = format == "xyznuviiiww" || format == "xyznuviiiwwtb";
        let hardskinned = format == "xyznuvi" || format == "xyznuvitb";
        if fx_file.contains("hardskinned") {
            if softskinned {
                // "Unable to apply a hardskinned shader to a softskinned
                // object."
                return false;
            }
        } else if fx_file.contains("skinned") {
            if hardskinned {
                // "Unable to apply a softskinned shader to a hardskinned
                // object."
                return false;
            } else if !softskinned {
                // "Unable to apply a softskinned shader to an unskinned
                // object."
                return false;
            }
        } else {
            // "Applying an unskinned shader to a skinned object." — the
            // prompt seam accepts.
        }
        match fx_dual_uv {
            Some(true) if !dual_uv => {
                // "Unable to apply a dual UV shader to a single UV object."
                false
            }
            // A single-UV shader on a dual-UV object only warns.
            _ => true,
        }
    }

    /// `Mutant::materialShader` (write): set the shader of a material
    /// (every section) or a tint. The shader correctness gate runs first;
    /// the fx children are replaced with the one value, or removed for an
    /// empty shader. Re-runs the census on success (`reloadAllLists`).
    pub fn set_material_shader(
        &mut self,
        resource: &BwResource,
        material_name: &str,
        matter_name: &str,
        tint_name: &str,
        fx_file: &str,
        fx_dual_uv: Option<bool>,
    ) -> bool {
        if matter_name.is_empty() || tint_name == "Default" {
            let Some(entry) = self.materials.get(material_name) else {
                return false;
            };
            if !self.ensure_shader_correct(fx_file, &entry.format, entry.dual_uv, fx_dual_uv) {
                return false;
            }
            let data = entry.data.clone();
            for path in &data {
                self.instantiate_mfm(path, resource);
                let Some(section) = section_at_mut(&mut self.visual, path) else {
                    continue;
                };
                Self::replace_fx_children(section, fx_file);
            }
            self.recensus(resource);
            return true;
        }

        let Some(entry) = self
            .tints
            .get(matter_name)
            .and_then(|map| map.get(tint_name))
            .cloned()
        else {
            return false;
        };
        if !self.ensure_shader_correct(fx_file, &entry.format, entry.dual_uv, fx_dual_uv) {
            return false;
        }
        let Some(material_path) = self.tint_material_path(matter_name, tint_name) else {
            return false;
        };
        self.instantiate_mfm(&material_path, resource);
        let Some(section) = section_at_mut_any(self, &material_path) else {
            return false;
        };
        Self::replace_fx_children(section, fx_file);
        self.recensus(resource);
        true
    }

    /// The fx-children rewrite shared by the two [`MaterialEditor::
    /// set_material_shader`] faces: delete every `fx` child, then write
    /// the value (or nothing).
    fn replace_fx_children(section: &mut DataSection, fx_file: &str) {
        let fxs: Vec<usize> = (0..section.count_children())
            .filter(|&index| {
                section
                    .open_child(index)
                    .is_some_and(|child| child.section_name() == "fx")
            })
            .collect();
        for index in fxs.into_iter().rev() {
            section.del_child_at(index);
        }
        if !fx_file.is_empty() {
            section.write_string("fx", fx_file);
        } else {
            section.del_child("fx");
        }
    }

    /// `Mutant::materialShader` (read): the current shader of a material or
    /// tint, after instantiating its mfm. Empty when the target is unknown
    /// or carries no material child.
    pub fn material_shader(
        &mut self,
        resource: &BwResource,
        material_name: &str,
        matter_name: &str,
        tint_name: &str,
    ) -> String {
        let Some(source) = self.property_source(material_name, matter_name, tint_name) else {
            return String::new();
        };
        if section_at_any(self, &source).is_none() {
            return String::new();
        }
        self.instantiate_mfm(&source, resource);
        section_at_any(self, &source)
            .map(|section| section.read_string("fx", ""))
            .unwrap_or_default()
    }

    /// `Mutant::materialMFM`: apply an mfm file to a material (every
    /// section) or a tint. Returns the mfm's fx through `fx_file` on
    /// success. The no-tint effect reload is the render seam; the census
    /// (`reloadAllLists`) runs before returning.
    pub fn material_mfm(
        &mut self,
        resource: &BwResource,
        material_name: &str,
        matter_name: &str,
        tint_name: &str,
        mfm_file: &str,
        fx_file: &mut Option<String>,
    ) -> bool {
        let Some(raw) = resource.file_system().read_file(mfm_file) else {
            return false;
        };
        let Ok(mfm_data) = datasection::parse(bwresource::get_filename(mfm_file), &raw) else {
            return false;
        };
        let mfm_fx = mfm_data.read_string("fx", "");
        if mfm_fx.is_empty() {
            return false;
        }
        if matter_name.is_empty() || tint_name == "Default" {
            let Some(entry) = self.materials.get(material_name) else {
                return false;
            };
            if !self.ensure_shader_correct(&mfm_fx, &entry.format, entry.dual_uv, None) {
                return false;
            }
            let data = entry.data.clone();
            for path in &data {
                self.overload_mfm(path, &mfm_data);
            }
        } else {
            let Some(entry) = self
                .tints
                .get(matter_name)
                .and_then(|map| map.get(tint_name))
                .cloned()
            else {
                return false;
            };
            if !self.ensure_shader_correct(&mfm_fx, &entry.format, entry.dual_uv, None) {
                return false;
            }
            let Some(material_path) = self.tint_material_path(matter_name, tint_name) else {
                return false;
            };
            self.overload_mfm(&material_path, &mfm_data);
        }
        self.recensus(resource);
        *fx_file = Some(mfm_fx);
        true
    }

    /// `Mutant::tintFlag` (write): set a flag field on the tint's material
    /// child after instantiating its mfm. The `Default` tint's null data —
    /// a null dereference in the reference — refuses here.
    pub fn set_tint_flag(
        &mut self,
        resource: &BwResource,
        matter_name: &str,
        tint_name: &str,
        flag_name: &str,
        val: u32,
    ) {
        let Some(material_path) = self.tint_material_path(matter_name, tint_name) else {
            return;
        };
        self.instantiate_mfm(&material_path, resource);
        if let Some(section) = section_at_mut(&mut self.model, &material_path) {
            section.write_int(flag_name, val as i32);
        }
        self.recensus(resource);
    }

    /// `Mutant::tintFlag` (read): the flag off the tint's material child,
    /// else the legacy mfm file's, else the `FLAG_NOT_SET` sentinel.
    pub fn tint_flag(
        &mut self,
        resource: &BwResource,
        matter_name: &str,
        tint_name: &str,
        flag_name: &str,
    ) -> i32 {
        let Some(data) = self
            .tints
            .get(matter_name)
            .and_then(|map| map.get(tint_name))
            .and_then(|entry| entry.data.clone())
        else {
            return FLAG_NOT_SET;
        };
        let flag = section_at(&self.model, &data)
            .map(|tint| tint.read_int(&format!("material/{flag_name}"), FLAG_NOT_SET))
            .unwrap_or(FLAG_NOT_SET);
        if flag != FLAG_NOT_SET {
            return flag;
        }
        // The legacy mfm the material still references.
        let mfm_name = section_at(&self.model, &data)
            .map(|tint| tint.read_string("material/mfm", ""))
            .unwrap_or_default();
        match resource
            .file_system()
            .read_file(&mfm_name)
            .map(|raw| datasection::parse(bwresource::get_filename(&mfm_name), &raw))
        {
            Some(Ok(data)) => data.read_int(flag_name, FLAG_NOT_SET),
            _ => FLAG_NOT_SET,
        }
    }

    /// `Mutant::materialFlag` (write): set a flag field on every section of
    /// a material, after instantiating their mfms. The live effect's
    /// material-kind update, the model reload and the BSP reload are the
    /// render seams; the port re-runs the census.
    pub fn set_material_flag(
        &mut self,
        resource: &BwResource,
        material_name: &str,
        flag_name: &str,
        val: u32,
    ) {
        let Some(entry) = self.materials.get(material_name) else {
            return;
        };
        let data = entry.data.clone();
        for path in &data {
            self.instantiate_mfm(path, resource);
            if let Some(section) = section_at_mut(&mut self.visual, path) {
                section.write_int(flag_name, val as i32);
            }
        }
        self.recensus(resource);
    }

    /// `Mutant::materialFlag` (read): the flag off the first section, else
    /// the legacy mfm file's, else zero.
    pub fn material_flag(
        &mut self,
        resource: &BwResource,
        material_name: &str,
        flag_name: &str,
    ) -> i32 {
        let Some(path) = self
            .materials
            .get(material_name)
            .and_then(|entry| entry.data.first().cloned())
        else {
            return 0;
        };
        let Some(section) = section_at(&self.visual, &path) else {
            return 0;
        };
        if section.find_child(flag_name).is_some() {
            return section.read_int(flag_name, 0);
        }
        let mfm_name = section.read_string("mfm", "");
        if mfm_name.is_empty() {
            return 0;
        }
        match resource
            .file_system()
            .read_file(&mfm_name)
            .map(|raw| datasection::parse(bwresource::get_filename(&mfm_name), &raw))
        {
            Some(Ok(data)) => data.read_int(flag_name, 0),
            _ => 0,
        }
    }

    /// `Mutant::materialTextureFeedName` (read): the texture feed of one
    /// property, after instantiating the target's mfm.
    pub fn material_texture_feed_name(
        &mut self,
        resource: &BwResource,
        material_name: &str,
        matter_name: &str,
        tint_name: &str,
        prop_name: &str,
    ) -> String {
        let Some(source) = self.property_source(material_name, matter_name, tint_name) else {
            return String::new();
        };
        if section_at_any(self, &source).is_none() {
            return String::new();
        }
        self.instantiate_mfm(&source, resource);
        let Some(section) = section_at_any(self, &source) else {
            return String::new();
        };
        let mut feed = String::new();
        for prop in open_sections(section, "property") {
            if prop.as_string() == prop_name {
                feed = prop.read_string("TextureFeed", &feed);
            }
        }
        feed
    }

    /// `Mutant::materialPropertyVal`: one property's value under
    /// `data_type` (`"Texture"`, `"Bool"`, …), after instantiating the
    /// target's mfm.
    pub fn material_property_val(
        &mut self,
        resource: &BwResource,
        material_name: &str,
        matter_name: &str,
        tint_name: &str,
        prop_name: &str,
        data_type: &str,
    ) -> String {
        let Some(source) = self.property_source(material_name, matter_name, tint_name) else {
            return String::new();
        };
        if section_at_any(self, &source).is_none() {
            return String::new();
        }
        self.instantiate_mfm(&source, resource);
        let Some(section) = section_at_any(self, &source) else {
            return String::new();
        };
        let mut val = String::new();
        for prop in open_sections(section, "property") {
            if prop.as_string() == prop_name {
                val = prop.read_string(data_type, &val);
            }
        }
        val
    }

    /// `Mutant::materialTextureFeedName` (write): swap the property's
    /// texture feed on every section of a material (or a tint's material
    /// child) through [`MaterialEditor::change_material_feed`], then re-run
    /// the census (`reloadModel`). The material face marks the visual, the
    /// tint face the model.
    pub fn set_material_texture_feed(
        &mut self,
        resource: &BwResource,
        material_name: &str,
        matter_name: &str,
        tint_name: &str,
        prop_name: &str,
        feed_name: &str,
    ) {
        if matter_name.is_empty() || tint_name == "Default" {
            let Some(entry) = self.materials.get(material_name) else {
                return;
            };
            let data = entry.data.clone();
            for path in &data {
                self.change_material_feed(path, prop_name, feed_name, resource);
            }
            self.mark_dirty("visual");
        } else {
            let Some(material_path) = self.tint_material_path(matter_name, tint_name) else {
                return;
            };
            self.change_material_feed(&material_path, prop_name, feed_name, resource);
            self.mark_dirty("model");
        }
        self.recensus(resource);
    }

    /// `Mutant::changeMaterialFeed`: move one property between a plain
    /// `Texture` and a `TextureFeed` with a default. Feed→feed renames the
    /// feed; feed→plain drops the feed and writes the default as the
    /// texture; plain→feed moves the texture under the feed's default.
    pub fn change_material_feed(
        &mut self,
        path: &[usize],
        prop_name: &str,
        feed_name: &str,
        resource: &BwResource,
    ) {
        if section_at_any(self, path).is_none() {
            return;
        }
        self.instantiate_mfm(path, resource);
        let Some(section) = section_at_mut_any(self, path) else {
            return;
        };
        for prop_index in 0..section.count_children() {
            let matches = section.child_mut(prop_index).is_some_and(|prop| {
                prop.section_name() == "property" && prop.as_string() == prop_name
            });
            if !matches {
                continue;
            }
            let Some(prop) = section.child_mut(prop_index) else {
                continue;
            };
            if !prop.read_string("TextureFeed", "").is_empty() {
                if !feed_name.is_empty() {
                    prop.write_string("TextureFeed", feed_name);
                } else {
                    let texture = prop.read_string("TextureFeed/default", "");
                    prop.del_child("TextureFeed");
                    prop.write_string("Texture", &texture);
                }
            } else {
                let texture = prop.read_string("Texture", "");
                prop.del_child("Texture");
                prop.write_string("TextureFeed", feed_name);
                prop.write_string("TextureFeed/default", &texture);
            }
        }
    }

    /// `Mutant::exposedToScriptName`: the property name when the tint
    /// section exposes it, empty otherwise.
    pub fn exposed_to_script_name(
        &self,
        matter_name: &str,
        tint_name: &str,
        prop_name: &str,
    ) -> String {
        if matter_name.is_empty() || tint_name == "Default" {
            return String::new();
        }
        let Some(data) = self
            .tints
            .get(matter_name)
            .and_then(|map| map.get(tint_name))
            .and_then(|entry| entry.data.clone())
        else {
            return String::new();
        };
        let Some(tint) = section_at(&self.model, &data) else {
            return String::new();
        };
        for prop in open_sections(tint, "property") {
            if prop.read_string("name", "") == prop_name {
                return prop_name.to_string();
            }
        }
        String::new()
    }

    /// `Mutant::toggleExposed`: remove the exposed property when present,
    /// add it (with the parsed default) when not. The material child must
    /// exist — the reference returns before touching anything otherwise.
    /// The UndoRedo barriers are the engine seam.
    pub fn toggle_exposed(
        &mut self,
        matter_name: &str,
        tint_name: &str,
        prop_name: &str,
        resource: &BwResource,
    ) {
        let Some(data) = self
            .tints
            .get(matter_name)
            .and_then(|map| map.get(tint_name))
            .and_then(|entry| entry.data.clone())
        else {
            return;
        };
        let Some(material_path) = self.tint_material_path(matter_name, tint_name) else {
            return;
        };
        self.instantiate_mfm(&material_path, resource);

        let exposed = self.get_exposed_vector4(matter_name, tint_name, prop_name, "", "");
        let Some(tint) = section_at_mut(&mut self.model, &data) else {
            return;
        };
        for prop_index in 0..tint.count_children() {
            let matches = tint
                .open_child(prop_index)
                .is_some_and(|prop| prop.read_string("name", "") == prop_name);
            if matches {
                tint.del_child_at(prop_index);
                return;
            }
        }
        let Some(tint) = section_at_mut(&mut self.model, &data) else {
            return;
        };
        let prop = tint.add_child("property");
        prop.write_string("name", prop_name);
        if let Some(default) = prop.open_or_make_section("default") {
            default.set_vector4(exposed);
        }
    }

    /// `Mutant::getExposedVector4`: the Vector4 an exposed property
    /// carries. With a prop type and value the value parses through a
    /// scratch section; without them the reference reads the live effect
    /// property — the render seam — and the port leaves the zero vector.
    pub fn get_exposed_vector4(
        &self,
        matter_name: &str,
        tint_name: &str,
        _desc_name: &str,
        prop_type: &str,
        val: &str,
    ) -> Vector4 {
        let zero = Vector4::ZERO;
        if matter_name.is_empty() || tint_name == "Default" {
            return zero;
        }
        if !self
            .tints
            .get(matter_name)
            .is_some_and(|map| map.contains_key(tint_name))
        {
            return zero;
        }
        if prop_type.is_empty() && val.is_empty() {
            // The live-effect read is the D3DX seam.
            return zero;
        }
        let mut temp = DataSection::new("temp");
        temp.write_string(prop_type, val);
        if prop_type == "Bool" {
            let x = if temp.read_bool(prop_type, false) {
                1.0
            } else {
                0.0
            };
            return Vector4::new(x, 0.0, 0.0, 0.0);
        }
        if prop_type == "Int" {
            let x = temp.read_int(prop_type, 0) as f32;
            return Vector4::new(x, 0.0, 0.0, 0.0);
        }
        if prop_type == "Float" {
            let x = temp.read_float(prop_type, 0.0);
            return Vector4::new(x, 0.0, 0.0, 0.0);
        }
        let mut out = zero;
        for index in 0..temp.count_children() {
            let matches = temp
                .open_child(index)
                .is_some_and(|child| child.section_name() == prop_type);
            if matches {
                if let Some(child) = temp.open_child(index) {
                    out = child.as_vector4(zero);
                }
                break;
            }
        }
        out
    }

    /// `Mutant::modelMaterial` (read): the visual's material kind.
    pub fn model_material(&self) -> i32 {
        self.visual.read_int("materialKind", 0)
    }

    /// `Mutant::modelMaterial` (write): set the visual's material kind,
    /// creating the section when missing.
    pub fn set_model_material(&mut self, id: i32) {
        if let Some(section) = self.visual.open_or_make_section("materialKind") {
            section.set_int(id);
        }
    }

    /// `Mutant::checkMaterials`: whether any material's shader is a sky-box
    /// shader (the reference sets `isSkyBox_`). The compiled-parameter
    /// query becomes a text scan of the shader source through `fx_text`.
    pub fn check_materials(&self, fx_text: &dyn Fn(&str) -> String) -> bool {
        for entry in self.materials.values() {
            for path in &entry.data {
                let Some(section) = section_at(&self.visual, path) else {
                    continue;
                };
                for fx in read_strings(section, "fx") {
                    if Self::effect_is_skybox(&fx_text(&fx)) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// `Mutant::effectIsSkybox` as a text scan: the shader source names the
    /// `isBWSkyBox` parameter. The compiled parameter's bool value is the
    /// D3DX seam; the port reports the parameter's presence.
    pub fn effect_is_skybox(fx_text: &str) -> bool {
        !fx_text.is_empty() && fx_text.contains("isBWSkyBox")
    }

    /// `Mutant::effectHasNormalMap` as a text scan: a `normalMap` parameter
    /// or a `NormalMap` UI widget annotation in the shader source.
    pub fn effect_has_normal_map(fx_text: &str) -> bool {
        !fx_text.is_empty() && (fx_text.contains("normalMap") || fx_text.contains("NormalMap"))
    }

    /// `Mutant::doAnyEffectsHaveNormalMap`: whether any material's shaders
    /// carry a normal map (via [`MaterialEditor::effect_has_normal_map`]).
    /// The loaded-effect `bumpMapped()` check is the render seam; the port
    /// runs only the reference's "hacky search" fallback over the fx
    /// strings.
    pub fn do_any_effects_have_normal_map(&self, fx_text: &dyn Fn(&str) -> String) -> bool {
        for entry in self.materials.values() {
            for path in &entry.data {
                let Some(section) = section_at(&self.visual, path) else {
                    continue;
                };
                for fx in read_strings(section, "fx") {
                    if Self::effect_has_normal_map(&fx_text(&fx)) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// `Mutant::reloadAllLists` ends in `regenMaterialsList`; the port
    /// re-runs the census against the current documents. The model reload
    /// itself is the engine seam; the applied-dye record survives, like the
    /// reference's member.
    pub fn recensus(&mut self, resource: &BwResource) {
        let model = self.model.clone();
        let visual = self.visual.clone();
        let primitives_name = self.primitives_name.clone();
        let fresh = Self::census(resource, model, visual, &primitives_name);
        self.materials = fresh.materials;
        self.tints = fresh.tints;
        self.dyes = fresh.dyes;
        self.tinted_materials = fresh.tinted_materials;
        self.names_to_remove = fresh.names_to_remove;
    }

    /// The document any property face reads through: a material's first
    /// section or a tint's material child.
    fn property_source(
        &self,
        material_name: &str,
        matter_name: &str,
        tint_name: &str,
    ) -> Option<SectionPath> {
        if matter_name.is_empty() || tint_name == "Default" {
            let entry = self.materials.get(material_name)?;
            entry.data.first().cloned()
        } else {
            self.tint_material_path(matter_name, tint_name)
        }
    }

    /// The `material` child of a tint section, as a path — the tint face of
    /// every property/flag/shader call. The `Default` tint (no data) and a
    /// material-less tint both refuse.
    fn tint_material_path(&self, matter_name: &str, tint_name: &str) -> Option<SectionPath> {
        let data = self.tints.get(matter_name)?.get(tint_name)?.data.clone()?;
        let tint = section_at(&self.model, &data)?;
        let index = (0..tint.count_children()).find(|&child_index| {
            tint.open_child(child_index)
                .is_some_and(|child| child.section_name() == "material")
        })?;
        let mut path = data;
        path.push(index);
        Some(path)
    }
}

/// `Utilities::pythonSafeName` (`utilities.cpp:172`): lowercase, keep
/// `[a-z0-9_]`, spaces become underscores, everything else drops; a leading
/// digit gains an underscore **before** the digit (the digit itself still
/// lands — the reference pushes `'_'` and falls through).
pub fn python_safe_name(s: &str) -> String {
    let mut out = String::new();
    let mut first = true;
    for ch in s.chars() {
        if first && ch.is_ascii_digit() {
            out.push('_');
        }
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
            out.push(c);
        } else if c == ' ' {
            out.push('_');
        }
        first = false;
    }
    out
}

/// The source vertex format of a geometry's vertices part
/// (`VerticesManager::get(vertGroup)->sourceFormat()`). The modern
/// `file.primitives/part` form reads the named part out of the (processed
/// first) container; the legacy `file/part` form splits and falls back to
/// the whole-file lookup `fetchOldPrimitivePart` does off the resource
/// root. Missing containers or parts give an empty format.
pub fn source_format(resource: &BwResource, vert_group: &str, _primitives_name: &str) -> String {
    if let Some(cut) = vert_group.find(".primitives/") {
        let file = &vert_group[..cut + ".primitives".len()];
        let part = &vert_group[cut + ".primitives/".len()..];
        return container_format(resource, file, part);
    }
    let (file, part) = split_old_primitive_name(vert_group);
    let format = container_format(resource, &file, &part);
    if !format.is_empty() {
        return format;
    }
    // fetchOldPrimitivePart's root fallback: the legacy standalone
    // `<file>.<part>` file read straight off the resource root.
    match resource.file_system().read_file(&format!("{file}.{part}")) {
        Some(raw) => VertexHeader::read(&raw)
            .map(|header| header.vertex_format)
            .unwrap_or_default(),
        None => String::new(),
    }
}

/// Read a vertices part's `VertexHeader` format from its primitives
/// container (`PrimitiveFile::get(file + ".primitives")`, `.processed`
/// first). A missing container behaves as empty, like the reference's
/// always-succeeds `BinSection("temp", empty)`.
fn container_format(resource: &BwResource, file: &str, part: &str) -> String {
    let container_name = if file.ends_with(".primitives") {
        file.to_string()
    } else {
        format!("{file}.primitives")
    };
    let mut container = primitives_file(resource, &container_name)
        .unwrap_or_else(|| BinSection::new(&container_name, BinaryBlock::default()));
    let Some(blob) = container.read_binary(part) else {
        return String::new();
    };
    VertexHeader::read(blob.data())
        .map(|header| header.vertex_format)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp-dir resource root with fixtures, the `model_editor` shape.
    struct Fixture {
        dir: std::path::PathBuf,
        resource: BwResource,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("bw_material_editor_{tag}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let resource = BwResource::init_paths(&[dir.to_str().unwrap()]);
            Self { dir, resource }
        }

        fn write(&self, name: &str, data: impl AsRef<[u8]>) {
            let path = self.dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, data).unwrap();
        }

        fn section(&self, name: &str) -> Option<DataSection> {
            let raw = std::fs::read(self.dir.join(name)).ok()?;
            datasection::parse(bwresource::get_filename(name), &raw).ok()
        }
    }

    /// The chicken-shaped model (the `model_editor` fixture).
    fn chicken_model() -> String {
        String::from(
            "<chicken.model>\
             <nodefullVisual>characters/npc/chicken/chicken</nodefullVisual>\
             <batched>true</batched>\
             <animation><name>m_walk</name><frameRate>30.000000</frameRate>\
             <nodes>characters/npc/chicken/animation/m_walk</nodes></animation>\
             <animation><name>m_idle</name><frameRate>30.000000</frameRate>\
             <nodes>characters/npc/chicken/animation/m_idle</nodes></animation>\
             <action><name>TurnLeft</name><animation>m_turnleft</animation>\
             <blendInTime>0.001000</blendInTime><filler>false</filler>\
             <track>-1</track><isMovement>true</isMovement>\
             <isCoordinated>false</isCoordinated><isImpacting>true</isImpacting>\
             <match><trigger><minEntitySpeed>0.0</minEntitySpeed>\
             <maxEntitySpeed>0.100000</maxEntitySpeed>\
             <minEntityAux1>-180</minEntityAux1>\
             <maxEntityAux1>-60</maxEntityAux1></trigger>\
             <oneShot>true</oneShot><promoteMotion>true</promoteMotion></match>\
             </action>\
             <dye><matter>body</matter><replaces>skin</replaces>\
             <tint><name>red</name><material><identifier>skin</identifier>\
             <fx>shaders/std_effects/pffx.fx</fx></material></tint></dye>\
             <materialNames><material><original>skin</original>\
             <display>plumage</display></material></materialNames>\
             <visibilityBox><min>-0.249024 -0.017366 -0.217026</min>\
             <max>0.242057 0.492886 0.330074</max></visibilityBox>\
             <extent>44.000000</extent>\
             <metaData><copyright>Copyright BigWorld Pty Ltd.</copyright>\
             <modified_by>adamm</modified_by>\
             <modified_on>1254377014</modified_on></metaData>\
             </chicken.model>",
        )
    }

    /// The visual the chicken names, with one render set.
    fn chicken_visual() -> String {
        String::from(
            "<root>\
             <node><identifier>scene root</identifier>\
             <node><identifier>body</identifier></node></node>\
             <renderSet><nodes>body</nodes>\
             <geometry><vertices>geometry0vertices</vertices>\
             <stream>geometry0vertices</stream>\
             <primitiveGroup><material><identifier>skin</identifier>\
             <collisionFlags>0</collisionFlags>\
             <materialKind>0</materialKind></material></primitiveGroup>\
             </geometry></renderSet></root>",
        )
    }

    /// A primitives container whose one vertices blob carries `format`.
    fn primitives_bytes(format: &str) -> Vec<u8> {
        container_bytes(format, "geometry0vertices")
    }

    fn container_bytes(format: &str, part: &str) -> Vec<u8> {
        let mut prims = BinSection::new("m.primitives", BinaryBlock::default());
        let mut blob = Vec::new();
        VertexHeader {
            vertex_format: format.to_string(),
            n_vertices: 3,
        }
        .write(&mut blob);
        blob.extend_from_slice(&[0u8; 36]);
        prims.new_section(part).set_binary(BinaryBlock::new(blob));
        prims.save_bytes()
    }

    /// A bare vertices-part file: just the header, no container.
    fn header_bytes(format: &str) -> Vec<u8> {
        let mut blob = Vec::new();
        VertexHeader {
            vertex_format: format.to_string(),
            n_vertices: 3,
        }
        .write(&mut blob);
        blob
    }

    /// The standard chicken fixture: model, visual, modern-format primitives.
    fn chicken_fixture(tag: &str) -> Fixture {
        let fixture = Fixture::new(tag);
        fixture.write("characters/npc/chicken/chicken.model", chicken_model());
        fixture.write("characters/npc/chicken/chicken.visual", chicken_visual());
        fixture.write(
            "characters/npc/chicken/chicken.primitives",
            primitives_bytes("xyznrvt2"),
        );
        fixture
    }

    fn chicken_model_doc() -> DataSection {
        datasection::parse("chicken.model", chicken_model().as_bytes()).unwrap()
    }

    fn chicken_visual_doc() -> DataSection {
        datasection::parse("chicken.visual", chicken_visual().as_bytes()).unwrap()
    }

    fn chicken_editor(fixture: &Fixture) -> MaterialEditor {
        MaterialEditor::census(
            &fixture.resource,
            chicken_model_doc(),
            chicken_visual_doc(),
            "characters/npc/chicken/chicken.primitives",
        )
    }

    /// A chicken editor whose dye carries an extra material-less tint.
    fn chicken_editor_bare_tint(fixture: &Fixture) -> MaterialEditor {
        let mut model = chicken_model_doc();
        let dye = dye_mut(&mut model);
        dye.add_child("tint").write_string("name", "bare");
        MaterialEditor::census(
            &fixture.resource,
            model,
            chicken_visual_doc(),
            "characters/npc/chicken/chicken.primitives",
        )
    }

    /// The `dye` child of a parsed model, mutably.
    fn dye_mut(model: &mut DataSection) -> &mut DataSection {
        let index = (0..model.count_children())
            .find(|&index| {
                model
                    .open_child(index)
                    .is_some_and(|child| child.section_name() == "dye")
            })
            .expect("model carries a dye");
        model.child_mut(index).unwrap()
    }

    // ------------------------------------------------------------------
    // The census.
    // ------------------------------------------------------------------

    #[test]
    fn census_gathers_the_material_tables() {
        let fixture = chicken_fixture("census");
        let editor = chicken_editor(&fixture);

        let skin = editor.materials.get("skin").unwrap();
        // The materialNames alias renamed the display.
        assert_eq!(skin.name, "plumage");
        assert_eq!(skin.name_data.as_deref(), Some("skin"));
        assert_eq!(skin.data.len(), 1);
        assert_eq!(skin.format, "xyznrvt2");
        assert!(!skin.dual_uv);
        assert!(!skin.colours);

        // The dye and its tints, plus the synthesized Default.
        assert!(editor.dyes.contains_key("body"));
        assert!(editor.tints["body"].contains_key("red"));
        assert!(editor.tints["body"].contains_key("Default"));
        assert!(editor.tints["body"]["Default"].data.is_none());
        assert_eq!(editor.tints["body"]["red"].format, "xyznrvt2");
        assert_eq!(editor.tinted_materials["skin"], "body");

        // The applied-dye record starts empty.
        assert_eq!(editor.get_tint_name("body"), "Default");
        assert_eq!(editor.tint_names("body"), vec!["Default", "red"]);
        assert_eq!(editor.material_display_name("skin"), "plumage");
        assert_eq!(editor.material_display_name("ghost"), "");
        assert!(editor.dirty.is_empty());
    }

    #[test]
    fn census_creates_material_names_and_queues_stale_aliases() {
        let fixture = chicken_fixture("alias");
        let mut model = chicken_model_doc();
        // Drop the chicken alias; keep a non-material child and a stale
        // alias the walk does not know.
        model.delete_sections("materialNames");
        let names = model.open_or_make_section("materialNames").unwrap();
        names.add_child("note").set_string("not a material child");
        names
            .add_child("material")
            .write_string("original", "ghost");
        names.add_child("material").write_string("original", "skin");
        names.add_child("material").write_string("original", "lost");

        let mut editor = MaterialEditor::census(
            &fixture.resource,
            model,
            chicken_visual_doc(),
            "characters/npc/chicken/chicken.primitives",
        );
        // The skin alias applied; the stale ones queued, counted among the
        // material-named children only.
        assert_eq!(editor.materials["skin"].name, "skin");
        assert_eq!(editor.names_to_remove, vec![0, 2]);

        editor.clean_material_names();
        let names = editor
            .model
            .open_section("materialNames")
            .expect("census created the section");
        let materials: Vec<String> = open_sections(names, "material")
            .iter()
            .map(|child| child.read_string("original", ""))
            .collect();
        assert_eq!(materials, vec!["skin"]);
        // The non-material child survived.
        assert!(names.find_child("note").is_some());
        assert!(editor.dirty.contains("model"));
        // A second clean is a no-op.
        editor.clean_material_names();
        assert!(editor.names_to_remove.is_empty());
    }

    #[test]
    fn census_repairs_missing_bsp_materials() {
        let fixture = chicken_fixture("bsp");
        let mut visual = chicken_visual_doc();
        visual
            .add_child("missingBspMaterial")
            .write_string("material/identifier", "bsp1");

        let editor = MaterialEditor::census(
            &fixture.resource,
            chicken_model_doc(),
            visual,
            "characters/npc/chicken/chicken.primitives",
        );
        // The markers went, the material joined the first geometry, and
        // the census saw the repaired material.
        assert!(editor.visual.find_child("missingBspMaterial").is_none());
        let render_set = editor.visual.open_section("renderSet").unwrap();
        let geometry = render_set.open_section("geometry").unwrap();
        let groups = open_sections(geometry, "primitiveGroup");
        assert_eq!(groups.len(), 2);
        let repaired = groups[1].open_section("material").unwrap();
        assert_eq!(repaired.read_string("identifier", ""), "bsp1");
        assert_eq!(repaired.read_int("collisionFlags", -1), 0);
        assert_eq!(repaired.read_int("materialKind", -1), 0);
        assert!(editor.materials.contains_key("bsp1"));
        assert!(editor.dirty.contains("visual"));
    }

    #[test]
    fn census_reads_dual_uv_off_the_streams() {
        let fixture = chicken_fixture("uv");
        let mut visual = chicken_visual_doc();
        visual
            .open_or_make_section("renderSet/geometry/stream")
            .unwrap()
            .set_string("geometry0vertices.uv2");

        let editor = MaterialEditor::census(
            &fixture.resource,
            chicken_model_doc(),
            visual,
            "characters/npc/chicken/chicken.primitives",
        );
        // A uv2 stream marks dual UV — and, through the reference's
        // `colours || dualUV` bug, colours too.
        assert!(editor.materials["skin"].dual_uv);
        assert!(editor.materials["skin"].colours);

        // A colour stream never reaches the table (the bug).
        let fixture = chicken_fixture("colour");
        let mut visual = chicken_visual_doc();
        visual
            .open_or_make_section("renderSet/geometry/stream")
            .unwrap()
            .set_string("geometry0vertices.colour");
        let editor = MaterialEditor::census(
            &fixture.resource,
            chicken_model_doc(),
            visual,
            "characters/npc/chicken/chicken.primitives",
        );
        assert!(!editor.materials["skin"].dual_uv);
        assert!(!editor.materials["skin"].colours);
    }

    // ------------------------------------------------------------------
    // Source formats.
    // ------------------------------------------------------------------

    #[test]
    fn source_format_modern_legacy_and_standalone_lookups() {
        let fixture = chicken_fixture("format");
        // Modern: file.primitives/part out of the container.
        assert_eq!(
            source_format(
                &fixture.resource,
                "characters/npc/chicken/chicken.primitives/geometry0vertices",
                "characters/npc/chicken/chicken.primitives"
            ),
            "xyznrvt2"
        );
        // The .processed container wins over the bare one.
        fixture.write(
            "characters/npc/chicken/chicken.primitives.processed",
            primitives_bytes("xyznuviiiww"),
        );
        assert_eq!(
            source_format(
                &fixture.resource,
                "characters/npc/chicken/chicken.primitives/geometry0vertices",
                "characters/npc/chicken/chicken.primitives"
            ),
            "xyznuviiiww"
        );

        // Legacy: file/part split, then the model's own container.
        fixture.write("old/terrain.primitives", container_bytes("xyznuv", "p0"));
        assert_eq!(
            source_format(&fixture.resource, "old/terrain.p0", "old/terrain"),
            "xyznuv"
        );

        // No container: the standalone `<file>.<part>` root file.
        fixture.write("bare/rock.p0", header_bytes("xyznuvitb"));
        assert_eq!(
            source_format(&fixture.resource, "bare/rock.p0", "bare/rock"),
            "xyznuvitb"
        );

        // Missing everywhere: empty format, like the empty container.
        assert_eq!(
            source_format(&fixture.resource, "nowhere/nothing.x", "nowhere/nothing"),
            ""
        );
    }

    // ------------------------------------------------------------------
    // Mfm faces.
    // ------------------------------------------------------------------

    #[test]
    fn instantiate_mfm_expands_the_reference_in_place() {
        let fixture = chicken_fixture("inst");
        fixture.write(
            "maps/mat.mfm",
            "<root><identifier>m1</identifier><fx>shaders/mfx.fx</fx>\
             <collisionFlags>7</collisionFlags><materialKind>2</materialKind>\
             <property>SpecPower<default>4.0</default></property></root>",
        );
        let mut editor = chicken_editor(&fixture);
        let path = editor.materials["skin"].data[0].clone();
        section_at_mut(&mut editor.visual, &path)
            .unwrap()
            .write_string("mfm", "maps/mat.mfm");

        editor.instantiate_mfm(&path, &fixture.resource);
        let section = section_at(&editor.visual, &path).unwrap();
        // The section's own fields won; the file's content filled the rest.
        assert_eq!(section.read_string("identifier", ""), "skin");
        assert_eq!(section.read_string("fx", ""), "shaders/mfx.fx");
        assert_eq!(section.read_int("collisionFlags", -1), 0);
        assert_eq!(section.read_int("materialKind", -1), 0);
        let props = open_sections(section, "property");
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].as_string(), "SpecPower");
        assert_eq!(props[0].read_string("default", ""), "4.0");
        assert!(section.find_child("mfm").is_none());

        // A missing reference leaves the section alone.
        let mut visual = chicken_visual_doc();
        section_at_mut(&mut visual, &path)
            .unwrap()
            .write_string("mfm", "maps/gone.mfm");
        let mut editor = MaterialEditor::census(
            &fixture.resource,
            chicken_model_doc(),
            visual,
            "characters/npc/chicken/chicken.primitives",
        );
        let path = editor.materials["skin"].data[0].clone();
        editor.instantiate_mfm(&path, &fixture.resource);
        let section = section_at(&editor.visual, &path).unwrap();
        assert!(section.find_child("fx").is_none());
        assert!(section.find_child("mfm").is_some());
    }

    #[test]
    fn overload_mfm_layers_over_the_section() {
        let fixture = chicken_fixture("over");
        let raw = b"\
            <root><fx>shaders/o.fx</fx>\
             <collisionFlags>5</collisionFlags><materialKind>1</materialKind>\
             <property>SpecPower<default>9.0</default></property>\
             <property>NewProp<default>1</default></property></root>"
            .to_vec();
        let mfm = datasection::parse("over.mfm", &raw).unwrap();

        let mut editor = chicken_editor(&fixture);
        let path = editor.materials["skin"].data[0].clone();
        let section = section_at_mut(&mut editor.visual, &path).unwrap();
        section.add_child("property").set_string("SpecPower");
        section
            .open_or_make_section("property/SpecPower")
            .unwrap()
            .set_float(2.0);

        editor.overload_mfm(&path, &mfm);
        let section = section_at(&editor.visual, &path).unwrap();
        assert_eq!(section.read_string("fx", ""), "shaders/o.fx");
        assert_eq!(section.read_int("collisionFlags", -1), 5);
        assert_eq!(section.read_int("materialKind", -1), 1);
        let props: Vec<&DataSection> = open_sections(section, "property");
        let spec = props
            .iter()
            .find(|prop| prop.as_string() == "SpecPower")
            .expect("the matched property kept its name");
        assert_eq!(spec.read_string("default", ""), "9.0");
        assert!(props.iter().any(|prop| prop.as_string() == "NewProp"));
    }

    // ------------------------------------------------------------------
    // The clean faces.
    // ------------------------------------------------------------------

    #[test]
    fn clean_materials_rebuilds_sections_from_the_backup() {
        let fixture = chicken_fixture("clean");
        fixture.write("old/terrain.primitives", container_bytes("xyznuv", "p0"));
        let visual = String::from(
            "<root><node><identifier>body</identifier></node>\
             <renderSet><nodes>body</nodes><geometry>\
             <vertices>geometry0vertices</vertices><stream>geometry0vertices</stream>\
             <primitiveGroup><material><identifier>skin</identifier>\
             <fx>shaders/a.fx</fx><fx>shaders/b.fx</fx>\
             <collisionFlags>3</collisionFlags><materialKind>2</materialKind>\
             <property>SpecPower<default>4.0</default></property>\
             <property>Junk<default>9</default></property>\
             </material></primitiveGroup>\
             <primitiveGroup><material><identifier>skin</identifier>\
             </material></primitiveGroup>\
             </geometry></renderSet></root>",
        );
        let mut editor = MaterialEditor::census(
            &fixture.resource,
            chicken_model_doc(),
            datasection::parse("layered.visual", visual.as_bytes()).unwrap(),
            "characters/npc/chicken/chicken.primitives",
        );
        let effects = EffectProperties {
            fx: "shaders/a.fx".to_string(),
            editable: vec!["SpecPower".to_string()],
        };
        editor.clean_materials(&fixture.resource, &effects);

        // Both sections of the material rebuilt from the first one's
        // backup: fixed fields, the editable property once, junk gone.
        assert_eq!(editor.materials["skin"].data.len(), 2);
        for path in &editor.materials["skin"].data {
            let section = section_at(&editor.visual, path).unwrap();
            let tags: Vec<&str> = (0..section.count_children())
                .filter_map(|index| section.open_child(index))
                .map(|child| child.section_name())
                .collect();
            assert_eq!(
                tags,
                vec![
                    "identifier",
                    "fx",
                    "fx",
                    "collisionFlags",
                    "materialKind",
                    "property"
                ]
            );
            assert_eq!(section.read_string("fx", ""), "shaders/a.fx");
            assert_eq!(
                section
                    .open_section("property/default")
                    .map(|default| default.read_string("", ""))
                    .unwrap_or_default(),
                "4.0"
            );
            assert!(section.find_child("property").is_some());
            let props: Vec<&str> = open_sections(section, "property")
                .iter()
                .map(|prop| prop.as_string())
                .collect();
            assert_eq!(props, vec!["SpecPower"]);
        }
        assert!(editor.dirty.contains("visual"));
    }

    #[test]
    fn clean_tints_copies_every_matching_property() {
        let fixture = chicken_fixture("tint");
        let mut model = chicken_model_doc();
        let dye = dye_mut(&mut model);
        let tint = dye.add_child("tint");
        tint.write_string("name", "gold");
        let material = tint.add_child("material");
        material.write_string("identifier", "skin");
        material.write_string("fx", "shaders/a.fx");
        material.write_int("collisionFlags", 1);
        material.write_int("materialKind", 1);
        material.add_child("property").set_string("SpecPower");
        material.add_child("property").set_string("SpecPower");

        let mut editor = MaterialEditor::census(
            &fixture.resource,
            model,
            chicken_visual_doc(),
            "characters/npc/chicken/chicken.primitives",
        );
        let effects = EffectProperties {
            fx: "shaders/a.fx".to_string(),
            editable: vec!["SpecPower".to_string()],
        };
        editor.clean_tints(&fixture.resource, &effects);

        let path = editor.tint_material_path("body", "gold").unwrap();
        let material = section_at(&editor.model, &path).unwrap();
        // No dedupe: both backup properties landed.
        let props = open_sections(material, "property");
        assert_eq!(props.len(), 2);
        for prop in &props {
            assert_eq!(prop.as_string(), "SpecPower");
        }
        assert_eq!(material.read_string("fx", ""), "shaders/a.fx");
        assert!(editor.dirty.contains("model"));
    }

    // ------------------------------------------------------------------
    // Properties.
    // ------------------------------------------------------------------

    #[test]
    fn set_material_property_writes_every_section_and_reads_back() {
        let fixture = chicken_fixture("prop");
        let mut editor = chicken_editor(&fixture);

        // A plain property: created with the written value.
        assert!(editor.set_material_property(
            &fixture.resource,
            "skin",
            "SpecPower",
            "Float",
            "2.500000"
        ));
        assert_eq!(
            editor.material_property_val(
                &fixture.resource,
                "skin",
                "",
                "Default",
                "SpecPower",
                "Float"
            ),
            "2.500000"
        );

        // A feed property: the value lands under TextureFeed/default.
        let path = editor.materials["skin"].data[0].clone();
        let section = section_at_mut(&mut editor.visual, &path).unwrap();
        let prop = section.add_child("property");
        prop.set_string("BaseTex");
        prop.write_string("TextureFeed", "feeds/a.feed");
        prop.write_string("TextureFeed/default", "maps/default.dds");
        assert_eq!(
            editor.material_texture_feed_name(&fixture.resource, "skin", "", "Default", "BaseTex"),
            "feeds/a.feed"
        );
        assert!(editor.set_material_property(
            &fixture.resource,
            "skin",
            "BaseTex",
            "Texture",
            "maps/new.dds"
        ));
        assert_eq!(
            editor.material_property_val(
                &fixture.resource,
                "skin",
                "",
                "Default",
                "BaseTex",
                "TextureFeed/default"
            ),
            "maps/new.dds"
        );
        assert_eq!(
            editor.material_texture_feed_name(&fixture.resource, "skin", "", "Default", "BaseTex"),
            "feeds/a.feed"
        );

        // An unknown material still reports success.
        assert!(editor.set_material_property(
            &fixture.resource,
            "ghost",
            "SpecPower",
            "Float",
            "1.0"
        ));
    }

    #[test]
    fn set_tint_property_syncs_the_exposed_default() {
        let fixture = chicken_fixture("tprop");
        let mut editor = chicken_editor(&fixture);
        // Expose SpecPower on the tint.
        editor.toggle_exposed("body", "red", "SpecPower", &fixture.resource);

        assert!(editor.set_tint_property(
            &fixture.resource,
            "body",
            "red",
            "SpecPower",
            "Float",
            "2.500000"
        ));
        // The material child carries the property…
        let path = editor.tint_material_path("body", "red").unwrap();
        let material = section_at(&editor.model, &path).unwrap();
        let spec = open_sections(material, "property")
            .into_iter()
            .find(|prop| prop.as_string() == "SpecPower")
            .expect("the property landed");
        assert_eq!(spec.read_string("Float", ""), "2.500000");
        // …and the exposed default follows as a Vector4.
        let tint_path = editor.tints["body"]["red"].data.clone().unwrap();
        let tint = section_at(&editor.model, &tint_path).unwrap();
        let exposed = tint
            .open_section("property")
            .unwrap()
            .open_section("default")
            .unwrap();
        let v = exposed.as_vector4(Vector4::ZERO);
        assert_eq!(v.x, 2.5);
        assert_eq!(v.y, 0.0);

        // The Default tint refuses.
        assert!(!editor.set_tint_property(
            &fixture.resource,
            "body",
            "Default",
            "SpecPower",
            "Float",
            "2.500000"
        ));
    }

    // ------------------------------------------------------------------
    // Naming moves.
    // ------------------------------------------------------------------

    #[test]
    fn material_name_moves_the_alias() {
        let fixture = chicken_fixture("mname");
        let mut editor = chicken_editor(&fixture);

        assert!(editor.material_name("skin", "feathers"));
        assert_eq!(editor.materials["skin"].name, "feathers");
        assert_eq!(editor.material_display_name("skin"), "feathers");
        // The tinted-material map re-keyed.
        assert_eq!(editor.tinted_materials["feathers"], "body");
        assert!(!editor.tinted_materials.contains_key("skin"));
        // The alias child was rewritten, not duplicated.
        let names = editor.model.open_section("materialNames").unwrap();
        let originals: Vec<String> = open_sections(names, "material")
            .iter()
            .map(|child| child.read_string("original", ""))
            .collect();
        assert_eq!(originals, vec!["skin"]);
        assert_eq!(names.read_string("material/display", ""), "feathers");

        // Same name is a no-op success; a taken name refuses.
        assert!(editor.material_name("skin", "feathers"));
        assert!(
            !editor.material_name("ghost", "feathers") || editor.material_name("ghost", "phantom")
        );

        // A rename onto an existing display name refuses.
        assert!(!editor.material_name("ghost", "feathers"));

        // An unknown material joins the table and records a fresh alias.
        assert!(editor.material_name("ghost", "phantom"));
        assert_eq!(editor.material_display_name("ghost"), "phantom");
        let names = editor.model.open_section("materialNames").unwrap();
        assert_eq!(open_sections(names, "material").len(), 2);
    }

    #[test]
    fn matter_name_moves_every_table() {
        let fixture = chicken_fixture("matter");
        let mut model = chicken_model_doc();
        // A second dye on an unknown material, so the undo duplicate
        // check has something to hit without stealing tinted_materials.
        let extra = model.add_child("dye");
        extra.write_string("matter", "extra");
        extra.write_string("replaces", "nobody");
        let mut editor = MaterialEditor::census(
            &fixture.resource,
            model,
            chicken_visual_doc(),
            "characters/npc/chicken/chicken.primitives",
        );
        editor.set_dye("body", "red");

        // The undo duplicate check fires before anything moves.
        assert!(!editor.matter_name(&fixture.resource, "body", "extra", true));

        assert!(editor.matter_name(&fixture.resource, "body", "torso", false));
        assert!(editor.dyes.contains_key("torso"));
        assert!(!editor.dyes.contains_key("body"));
        assert!(editor.tints["torso"].contains_key("red"));
        assert_eq!(editor.get_tint_name("torso"), "red");
        assert_eq!(editor.tinted_materials["skin"], "torso");
        // The dye section's matter field moved (the census re-gathered it).
        let dye = dye_mut(&mut editor.model);
        assert_eq!(dye.read_string("matter", ""), "torso");
    }

    #[test]
    fn tint_name_moves_and_sets_the_applied_dye() {
        let fixture = chicken_fixture("tname");
        let mut editor = chicken_editor(&fixture);
        editor.set_dye("body", "red");

        assert!(editor.tint_name(&fixture.resource, "body", "red", "crimson", false));
        assert_eq!(editor.get_tint_name("body"), "crimson");
        assert!(editor.tints["body"].contains_key("crimson"));
        assert!(!editor.tints["body"].contains_key("red"));

        // Without an applied dye, no record is created.
        let mut editor = chicken_editor(&fixture);
        assert!(editor.tint_name(&fixture.resource, "body", "red", "crimson", false));
        assert!(!editor.curr_dyes.contains_key("body"));

        // Unknown matter or tint refuses (the reference crashes).
        assert!(!editor.tint_name(&fixture.resource, "body", "ghost", "x", false));
        assert!(!editor.tint_name(&fixture.resource, "ghost", "red", "x", false));

        // The undo duplicate check scans every matter's tints, after the
        // same-name exit.
        let mut editor = chicken_editor(&fixture);
        assert_eq!(
            editor.new_tint(
                &fixture.resource,
                "skin",
                "body",
                "red",
                "gold",
                "shaders/gold.fx",
                ""
            ),
            "body"
        );
        // gold -> red would collide with the existing red.
        assert!(!editor.tint_name(&fixture.resource, "body", "gold", "red", true));
        assert!(editor.tint_name(&fixture.resource, "body", "gold", "gold", true));
    }

    // ------------------------------------------------------------------
    // Tint lifecycle.
    // ------------------------------------------------------------------

    #[test]
    fn new_tint_covers_every_face() {
        let fixture = chicken_fixture("newtint");

        // A fresh matter names itself after the material.
        let mut editor = chicken_editor(&fixture);
        let matter = editor.new_tint(
            &fixture.resource,
            "skin",
            "",
            "Default",
            "gold",
            "shaders/gold.fx",
            "",
        );
        assert_eq!(matter, "skin");
        assert!(editor.dyes.contains_key("skin"));
        let dye = editor
            .model
            .open_child(
                (0..editor.model.count_children())
                    .find(|&index| {
                        editor.model.open_child(index).is_some_and(|child| {
                            child.section_name() == "dye"
                                && child.read_string("matter", "") == "skin"
                        })
                    })
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(dye.read_string("replaces", ""), "skin");
        let path = editor.tint_material_path("skin", "gold").unwrap();
        let material = section_at(&editor.model, &path).unwrap();
        // Copied from the material's own section, fx overwritten.
        assert_eq!(material.read_string("identifier", ""), "skin");
        assert_eq!(material.read_string("fx", ""), "shaders/gold.fx");

        // A known matter with a real old tint copies the old tint's
        // material child.
        let mut editor = chicken_editor(&fixture);
        let matter = editor.new_tint(
            &fixture.resource,
            "skin",
            "body",
            "red",
            "steel",
            "shaders/steel.fx",
            "",
        );
        assert_eq!(matter, "body");
        let path = editor.tint_material_path("body", "steel").unwrap();
        let material = section_at(&editor.model, &path).unwrap();
        assert_eq!(material.read_string("fx", ""), "shaders/steel.fx");
        assert_eq!(material.read_string("identifier", ""), "skin");

        // The mfm face copies the file's content.
        fixture.write(
            "maps/new.mfm",
            "<root><identifier>m</identifier><fx>shaders/m.fx</fx>\
             <collisionFlags>1</collisionFlags><materialKind>1</materialKind></root>",
        );
        let mut editor = chicken_editor(&fixture);
        let matter = editor.new_tint(
            &fixture.resource,
            "skin",
            "body",
            "red",
            "ivory",
            "",
            "maps/new.mfm",
        );
        assert_eq!(matter, "body");
        let path = editor.tint_material_path("body", "ivory").unwrap();
        let material = section_at(&editor.model, &path).unwrap();
        assert_eq!(material.read_string("fx", ""), "shaders/m.fx");
        assert_eq!(material.read_int("collisionFlags", -1), 1);

        // An unopenable mfm fails without touching anything.
        let mut editor = chicken_editor(&fixture);
        assert_eq!(
            editor.new_tint(
                &fixture.resource,
                "skin",
                "body",
                "red",
                "x",
                "",
                "maps/gone.mfm"
            ),
            ""
        );

        // An unknown matter refuses (the reference crashes).
        assert_eq!(
            editor.new_tint(
                &fixture.resource,
                "skin",
                "ghost",
                "Default",
                "x",
                "shaders/x.fx",
                ""
            ),
            ""
        );
    }

    #[test]
    fn delete_tint_removes_the_dye_when_its_last_tint_goes() {
        let fixture = chicken_fixture("deltint");
        let mut model = chicken_model_doc();
        let dye = dye_mut(&mut model);
        dye.add_child("tint").write_string("name", "green");
        let mut editor = MaterialEditor::census(
            &fixture.resource,
            model,
            chicken_visual_doc(),
            "characters/npc/chicken/chicken.primitives",
        );
        editor.set_dye("body", "green");

        // Deleting one of two tints keeps the dye.
        editor.delete_tint(&fixture.resource, "body", "green");
        assert!(editor.tints["body"].contains_key("red"));
        assert!(editor.dyes.contains_key("body"));

        // Deleting the last tint takes the dye and the applied record.
        editor.delete_tint(&fixture.resource, "body", "red");
        assert!(!editor.tints.contains_key("body"));
        assert!(!editor.dyes.contains_key("body"));
        assert!(!editor.curr_dyes.contains_key("body"));
        assert!(editor.model.find_child("dye").is_none());

        // Unknown dye or tint: no-op.
        editor.delete_tint(&fixture.resource, "body", "red");
        editor.delete_tint(&fixture.resource, "ghost", "red");
    }

    // ------------------------------------------------------------------
    // Export.
    // ------------------------------------------------------------------

    #[test]
    fn save_mfm_exports_the_last_section_identifier_free() {
        let fixture = chicken_fixture("save");
        fixture.write("out/keep", b"");
        let mut editor = chicken_editor(&fixture);

        // The material face. The exported root tag is `root` on disk
        // (createFromFile(name, true) re-reads it under the file name).
        assert!(editor.save_mfm(&fixture.resource, "skin", "", "Default", "out/skin.mfm"));
        let raw = std::fs::read(fixture.dir.join("out/skin.mfm")).unwrap();
        assert!(raw.starts_with(b"<root>"));
        let mfm = fixture.section("out/skin.mfm").unwrap();
        assert!(mfm.find_child("identifier").is_none());
        assert_eq!(mfm.read_int("collisionFlags", -1), 0);
        assert_eq!(mfm.read_int("materialKind", -1), 0);

        // The tint face.
        assert!(editor.save_mfm(&fixture.resource, "skin", "body", "red", "out/red.mfm"));
        let raw = std::fs::read(fixture.dir.join("out/red.mfm")).unwrap();
        assert!(raw.starts_with(b"<root>"));
        let mfm = fixture.section("out/red.mfm").unwrap();
        assert!(mfm.find_child("identifier").is_none());
        assert_eq!(mfm.read_string("fx", ""), "shaders/std_effects/pffx.fx");

        // Refusals: an unknown material, and a tint with no material child
        // (matter body + Default is the material face, like the reference).
        assert!(!editor.save_mfm(&fixture.resource, "ghost", "", "Default", "out/x.mfm"));
        assert!(editor.save_mfm(&fixture.resource, "skin", "body", "Default", "out/def.mfm"));
        let mut editor = chicken_editor_bare_tint(&fixture);
        assert!(!editor.save_mfm(&fixture.resource, "skin", "body", "bare", "out/x.mfm"));
    }

    // ------------------------------------------------------------------
    // Shaders.
    // ------------------------------------------------------------------

    #[test]
    fn the_shader_gate_matrix_matches_the_reference() {
        let fixture = chicken_fixture("gate");
        let editor = chicken_editor(&fixture);

        // An empty shader applies to anything.
        assert!(editor.ensure_shader_correct("", "xyznuviiiww", false, Some(true)));
        // Hardskinned on softskinned refuses; on hardskinned applies.
        assert!(!editor.ensure_shader_correct(
            "shaders/hardskinned/x.fx",
            "xyznuviiiww",
            false,
            None
        ));
        assert!(editor.ensure_shader_correct("shaders/hardskinned/x.fx", "xyznuvi", false, None));
        // Softskinned on hardskinned or unskinned refuses; on softskinned applies.
        assert!(!editor.ensure_shader_correct("shaders/skinned/x.fx", "xyznuvi", false, None));
        assert!(!editor.ensure_shader_correct("shaders/skinned/x.fx", "xyznuv", false, None));
        assert!(editor.ensure_shader_correct("shaders/skinned/x.fx", "xyznuviiiwwtb", false, None));
        // An unskinned shader on a skinned object only warns (prompt seam).
        assert!(editor.ensure_shader_correct("shaders/plain.fx", "xyznuviiiww", false, None));
        assert!(editor.ensure_shader_correct("shaders/plain.fx", "xyznuvi", false, None));
        // Dual-UV fx on a single-UV object refuses; the reverse warns.
        assert!(!editor.ensure_shader_correct("shaders/plain.fx", "xyznuv", false, Some(true)));
        assert!(editor.ensure_shader_correct("shaders/plain.fx", "xyznuv", false, Some(false)));
        // An unopenable fx only logs.
        assert!(editor.ensure_shader_correct("shaders/plain.fx", "xyznuv", false, None));
    }

    #[test]
    fn set_material_shader_replaces_every_fx_and_reads_back() {
        let fixture = chicken_fixture("shader");
        let mut editor = chicken_editor(&fixture);

        // The refusal leaves the sections untouched.
        assert!(!editor.set_material_shader(
            &fixture.resource,
            "skin",
            "",
            "Default",
            "shaders/skinned/x.fx",
            None
        ));
        assert_eq!(
            editor.material_shader(&fixture.resource, "skin", "", "Default"),
            ""
        );

        assert!(editor.set_material_shader(
            &fixture.resource,
            "skin",
            "",
            "Default",
            "shaders/std_effects/pffx.fx",
            None
        ));
        assert_eq!(
            editor.material_shader(&fixture.resource, "skin", "", "Default"),
            "shaders/std_effects/pffx.fx"
        );

        // A second set replaces rather than appends.
        assert!(editor.set_material_shader(
            &fixture.resource,
            "skin",
            "",
            "Default",
            "shaders/other.fx",
            None
        ));
        let path = editor.materials["skin"].data[0].clone();
        let section = section_at(&editor.visual, &path).unwrap();
        let fxs = read_strings(section, "fx");
        assert_eq!(fxs, vec!["shaders/other.fx"]);

        // An empty shader removes the fx children.
        assert!(editor.set_material_shader(&fixture.resource, "skin", "", "Default", "", None));
        assert_eq!(
            editor.material_shader(&fixture.resource, "skin", "", "Default"),
            ""
        );

        // The tint face writes the tint's material child.
        assert!(editor.set_material_shader(
            &fixture.resource,
            "skin",
            "body",
            "red",
            "shaders/tint.fx",
            None
        ));
        assert_eq!(
            editor.material_shader(&fixture.resource, "skin", "body", "red"),
            "shaders/tint.fx"
        );
    }

    #[test]
    fn material_mfm_applies_the_file_and_reports_its_fx() {
        let fixture = chicken_fixture("mmfm");
        fixture.write(
            "maps/over.mfm",
            "<root><fx>shaders/o.fx</fx>\
             <collisionFlags>5</collisionFlags><materialKind>1</materialKind>\
             <property>SpecPower<default>4.0</default></property></root>",
        );
        let mut editor = chicken_editor(&fixture);

        let mut fx_out = None;
        assert!(editor.material_mfm(
            &fixture.resource,
            "skin",
            "",
            "Default",
            "maps/over.mfm",
            &mut fx_out
        ));
        assert_eq!(fx_out, Some("shaders/o.fx".to_string()));
        let path = editor.materials["skin"].data[0].clone();
        let section = section_at(&editor.visual, &path).unwrap();
        assert_eq!(section.read_string("fx", ""), "shaders/o.fx");
        assert_eq!(section.read_int("collisionFlags", -1), 5);
        assert_eq!(section.read_int("materialKind", -1), 1);

        // The tint face.
        let mut tint_fx = None;
        assert!(editor.material_mfm(
            &fixture.resource,
            "skin",
            "body",
            "red",
            "maps/over.mfm",
            &mut tint_fx
        ));
        assert_eq!(tint_fx, Some("shaders/o.fx".to_string()));
        let path = editor.tint_material_path("body", "red").unwrap();
        let material = section_at(&editor.model, &path).unwrap();
        assert_eq!(material.read_int("collisionFlags", -1), 5);

        // An fx-less mfm and an unknown material refuse; missing files fail.
        fixture.write(
            "maps/nofx.mfm",
            "<root><collisionFlags>1</collisionFlags></root>",
        );
        let mut out = None;
        assert!(!editor.material_mfm(
            &fixture.resource,
            "skin",
            "",
            "Default",
            "maps/nofx.mfm",
            &mut out
        ));
        assert!(!editor.material_mfm(
            &fixture.resource,
            "ghost",
            "",
            "Default",
            "maps/over.mfm",
            &mut out
        ));
        assert!(!editor.material_mfm(
            &fixture.resource,
            "skin",
            "",
            "Default",
            "maps/gone.mfm",
            &mut out
        ));
    }

    // ------------------------------------------------------------------
    // Flags.
    // ------------------------------------------------------------------

    #[test]
    fn flags_follow_the_sentinel_chain() {
        let fixture = chicken_fixture("flags");
        let mut editor = chicken_editor(&fixture);

        editor.set_material_flag(&fixture.resource, "skin", "collisionFlags", 3);
        assert_eq!(
            editor.material_flag(&fixture.resource, "skin", "collisionFlags"),
            3
        );

        // A material with no local flag falls back to its mfm.
        fixture.write(
            "maps/legacy.mfm",
            "<root><collisionFlags>9</collisionFlags></root>",
        );
        let mut visual = chicken_visual_doc();
        let path_probe = {
            let probe = MaterialEditor::census(
                &fixture.resource,
                chicken_model_doc(),
                visual.clone(),
                "characters/npc/chicken/chicken.primitives",
            );
            probe.materials["skin"].data[0].clone()
        };
        section_at_mut(&mut visual, &path_probe)
            .unwrap()
            .write_string("mfm", "maps/legacy.mfm");
        let mut editor = MaterialEditor::census(
            &fixture.resource,
            chicken_model_doc(),
            visual,
            "characters/npc/chicken/chicken.primitives",
        );
        let path = editor.materials["skin"].data[0].clone();
        section_at_mut(&mut editor.visual, &path)
            .unwrap()
            .del_child("collisionFlags");
        assert_eq!(
            editor.material_flag(&fixture.resource, "skin", "collisionFlags"),
            9
        );
        assert_eq!(
            editor.material_flag(&fixture.resource, "ghost", "collisionFlags"),
            0
        );

        // Tint flags: written on the material child, read through the
        // sentinel, then the tint's mfm.
        let mut editor = chicken_editor(&fixture);
        editor.set_tint_flag(&fixture.resource, "body", "red", "materialKind", 5);
        assert_eq!(
            editor.tint_flag(&fixture.resource, "body", "red", "materialKind"),
            5
        );
        // The Default tint reads the sentinel.
        assert_eq!(
            editor.tint_flag(&fixture.resource, "body", "Default", "materialKind"),
            FLAG_NOT_SET
        );
        assert_eq!(FLAG_NOT_SET, 0xB0B15BAD_u32 as i32);

        // The tint's material child carrying an mfm reference falls back
        // to it.
        fixture.write(
            "maps/tintlegacy.mfm",
            "<root><materialKind>7</materialKind></root>",
        );
        let mut model = chicken_model_doc();
        let dye = dye_mut(&mut model);
        let tint = dye.add_child("tint");
        tint.write_string("name", "old");
        tint.add_child("material")
            .write_string("identifier", "skin");
        let mut editor = MaterialEditor::census(
            &fixture.resource,
            model,
            chicken_visual_doc(),
            "characters/npc/chicken/chicken.primitives",
        );
        let path = editor.tint_material_path("body", "old").unwrap();
        section_at_mut(&mut editor.model, &path)
            .unwrap()
            .write_string("mfm", "maps/tintlegacy.mfm");
        assert_eq!(
            editor.tint_flag(&fixture.resource, "body", "old", "materialKind"),
            7
        );
    }

    // ------------------------------------------------------------------
    // Texture feeds.
    // ------------------------------------------------------------------

    #[test]
    fn feeds_move_between_texture_and_texture_feed() {
        let fixture = chicken_fixture("feed");
        let mut editor = chicken_editor(&fixture);
        let path = editor.materials["skin"].data[0].clone();
        let section = section_at_mut(&mut editor.visual, &path).unwrap();
        let prop = section.add_child("property");
        prop.set_string("BaseTex");
        prop.write_string("Texture", "maps/x.dds");

        // Plain → feed: the texture moves under the feed's default.
        editor.set_material_texture_feed(
            &fixture.resource,
            "skin",
            "",
            "Default",
            "BaseTex",
            "feeds/a.feed",
        );
        assert_eq!(
            editor.material_texture_feed_name(&fixture.resource, "skin", "", "Default", "BaseTex"),
            "feeds/a.feed"
        );
        assert_eq!(
            editor.material_property_val(
                &fixture.resource,
                "skin",
                "",
                "Default",
                "BaseTex",
                "TextureFeed/default"
            ),
            "maps/x.dds"
        );
        assert!(editor.dirty.contains("visual"));

        // Feed → feed: rename.
        editor.set_material_texture_feed(
            &fixture.resource,
            "skin",
            "",
            "Default",
            "BaseTex",
            "feeds/b.feed",
        );
        assert_eq!(
            editor.material_texture_feed_name(&fixture.resource, "skin", "", "Default", "BaseTex"),
            "feeds/b.feed"
        );

        // Feed → plain: the default becomes the texture again.
        editor.set_material_texture_feed(&fixture.resource, "skin", "", "Default", "BaseTex", "");
        assert_eq!(
            editor.material_property_val(
                &fixture.resource,
                "skin",
                "",
                "Default",
                "BaseTex",
                "Texture"
            ),
            "maps/x.dds"
        );
        assert_eq!(
            editor.material_texture_feed_name(&fixture.resource, "skin", "", "Default", "BaseTex"),
            ""
        );

        // The tint face touches the tint's material child, which carries
        // its own copy of the property.
        let red_path = editor.tint_material_path("body", "red").unwrap();
        {
            let material = section_at_mut(&mut editor.model, &red_path).unwrap();
            let prop = material.add_child("property");
            prop.set_string("BaseTex");
            prop.write_string("TextureFeed", "feeds/old.feed");
            prop.write_string("TextureFeed/default", "maps/x.dds");
        }
        editor.set_material_texture_feed(
            &fixture.resource,
            "skin",
            "body",
            "red",
            "BaseTex",
            "feeds/t.feed",
        );
        let path = editor.tint_material_path("body", "red").unwrap();
        let material = section_at(&editor.model, &path).unwrap();
        let base = open_sections(material, "property")
            .into_iter()
            .find(|prop| prop.as_string() == "BaseTex")
            .expect("the tint face carried the property");
        assert_eq!(base.read_string("TextureFeed", ""), "feeds/t.feed");
        assert_eq!(base.read_string("TextureFeed/default", ""), "maps/x.dds");
        assert!(editor.dirty.contains("model"));
    }

    // ------------------------------------------------------------------
    // Exposed properties.
    // ------------------------------------------------------------------

    #[test]
    fn exposed_properties_toggle_and_parse() {
        let fixture = chicken_fixture("expose");
        let mut editor = chicken_editor(&fixture);

        assert_eq!(
            editor.exposed_to_script_name("body", "red", "SpecPower"),
            ""
        );
        editor.toggle_exposed("body", "red", "SpecPower", &fixture.resource);
        assert_eq!(
            editor.exposed_to_script_name("body", "red", "SpecPower"),
            "SpecPower"
        );
        editor.toggle_exposed("body", "red", "SpecPower", &fixture.resource);
        assert_eq!(
            editor.exposed_to_script_name("body", "red", "SpecPower"),
            ""
        );

        // The parsing matrix.
        let editor = chicken_editor(&fixture);
        assert_eq!(
            editor
                .get_exposed_vector4("body", "red", "x", "Float", "2.500000")
                .x,
            2.5
        );
        assert_eq!(
            editor.get_exposed_vector4("body", "red", "x", "Int", "7").x,
            7.0
        );
        assert_eq!(
            editor
                .get_exposed_vector4("body", "red", "x", "Bool", "true")
                .x,
            1.0
        );
        assert_eq!(
            editor
                .get_exposed_vector4("body", "red", "x", "Bool", "false")
                .x,
            0.0
        );
        let v = editor.get_exposed_vector4("body", "red", "x", "Vector4", "1 2 3 4");
        assert_eq!((v.x, v.y, v.z, v.w), (1.0, 2.0, 3.0, 4.0));
        // Unknown type parses through as_vector4.
        let v = editor.get_exposed_vector4("body", "red", "x", "Weird", "5 6 7 8");
        assert_eq!((v.x, v.y, v.z, v.w), (5.0, 6.0, 7.0, 8.0));
        // The live-effect read and the guard families give the zero vector.
        let zero = Vector4::ZERO;
        let v = editor.get_exposed_vector4("body", "red", "x", "", "");
        assert_eq!((v.x, v.y, v.z, v.w), (zero.x, zero.y, zero.z, zero.w));
        let v = editor.get_exposed_vector4("", "red", "x", "Float", "2.5");
        assert_eq!(v.x, 0.0);
        let v = editor.get_exposed_vector4("body", "Default", "x", "Float", "2.5");
        assert_eq!(v.x, 0.0);
        let v = editor.get_exposed_vector4("body", "ghost", "x", "Float", "2.5");
        assert_eq!(v.x, 0.0);
    }

    // ------------------------------------------------------------------
    // Model material and the fx text scans.
    // ------------------------------------------------------------------

    #[test]
    fn model_material_roundtrips_and_the_scans_read_the_fx_source() {
        let fixture = chicken_fixture("scan");
        let mut editor = chicken_editor(&fixture);

        assert_eq!(editor.model_material(), 0);
        editor.set_model_material(6);
        assert_eq!(editor.model_material(), 6);
        assert_eq!(editor.visual.read_int("materialKind", 0), 6);

        // The skybox and normal-map scans read the shader source.
        let path = editor.materials["skin"].data[0].clone();
        let section = section_at_mut(&mut editor.visual, &path).unwrap();
        section.add_child("fx").set_string("shaders/sky.fx");
        section.add_child("fx").set_string("shaders/plain.fx");

        let fx_text = |name: &str| -> String {
            if name.contains("sky") {
                "float isBWSkyBox = 1;".to_string()
            } else {
                "texture normalMap;".to_string()
            }
        };
        assert!(editor.check_materials(&fx_text));
        assert!(editor.do_any_effects_have_normal_map(&fx_text));

        // An empty source matches neither.
        assert!(!MaterialEditor::effect_is_skybox(""));
        assert!(!MaterialEditor::effect_has_normal_map(""));
        assert!(MaterialEditor::effect_is_skybox("bool isBWSkyBox;"));
        assert!(MaterialEditor::effect_has_normal_map("float4 NormalMap;"));

        // A plain source matches neither scan.
        let fx_text = |_name: &str| "float4 colour;".to_string();
        assert!(!editor.check_materials(&fx_text));
        assert!(!editor.do_any_effects_have_normal_map(&fx_text));
    }

    #[test]
    fn python_safe_name_follows_the_reference_rules() {
        assert_eq!(python_safe_name("9lives"), "_9lives");
        assert_eq!(python_safe_name("Hello World!"), "hello_world");
        assert_eq!(python_safe_name("already_safe"), "already_safe");
        assert_eq!(python_safe_name("Big-Bird"), "bigbird");
    }

    // ------------------------------------------------------------------
    // Guard faces.
    // ------------------------------------------------------------------

    #[test]
    fn census_without_a_geometry_still_drops_the_markers() {
        let fixture = chicken_fixture("nors");
        let mut visual = chicken_visual_doc();
        visual.delete_sections("renderSet");
        visual
            .add_child("missingBspMaterial")
            .write_string("material/identifier", "bsp9");

        let editor = MaterialEditor::census(
            &fixture.resource,
            chicken_model_doc(),
            visual,
            "characters/npc/chicken/chicken.primitives",
        );
        // The markers went; with no renderSet/geometry to repair onto,
        // nothing was added (the reference crashes here).
        assert!(editor.visual.find_child("missingBspMaterial").is_none());
        assert!(!editor.materials.contains_key("bsp9"));
        assert!(editor.dirty.contains("visual"));
    }

    #[test]
    fn instantiate_mfm_guard_faces() {
        let fixture = chicken_fixture("instguard");
        fixture.write("maps/junk.mfm", b"<unclosed");
        let mut editor = chicken_editor(&fixture);
        let path = editor.materials["skin"].data[0].clone();

        // No mfm reference: nothing happens.
        editor.instantiate_mfm(&path, &fixture.resource);
        assert!(
            section_at(&editor.visual, &path)
                .unwrap()
                .find_child("fx")
                .is_none()
        );

        // An unparseable mfm: the section stays as it is.
        section_at_mut(&mut editor.visual, &path)
            .unwrap()
            .write_string("mfm", "maps/junk.mfm");
        editor.instantiate_mfm(&path, &fixture.resource);
        let section = section_at(&editor.visual, &path).unwrap();
        assert!(section.find_child("fx").is_none());
        assert!(section.find_child("mfm").is_some());

        // The full expansion: the section's fx overrides the file's, and
        // the section's property lands on the file's matching one.
        fixture.write(
            "maps/full.mfm",
            "<root><identifier>fromfile</identifier><fx>fromfile.fx</fx>             <property>SpecPower<Float>1.0</Float></property>             <property>Other<Float>2.0</Float></property></root>",
        );
        let section = section_at_mut(&mut editor.visual, &path).unwrap();
        section.del_children();
        section.write_string("identifier", "skin");
        section.write_string("fx", "section.fx");
        section.write_string("mfm", "maps/full.mfm");
        let prop = section.add_child("property");
        prop.set_string("SpecPower");
        prop.write_string("Texture", "maps/orig.dds");

        editor.instantiate_mfm(&path, &fixture.resource);
        let section = section_at(&editor.visual, &path).unwrap();
        assert_eq!(section.read_string("fx", ""), "section.fx");
        assert_eq!(section.read_string("identifier", ""), "skin");
        assert!(section.find_child("mfm").is_none());
        let props = open_sections(section, "property");
        let spec = props
            .iter()
            .find(|prop| prop.as_string() == "SpecPower")
            .expect("the merged property kept its name");
        assert_eq!(spec.read_string("Texture", ""), "maps/orig.dds");
        assert!(props.iter().any(|prop| prop.as_string() == "Other"));
    }

    #[test]
    fn clean_and_property_edge_faces() {
        let fixture = chicken_fixture("cleanguard");
        // A material with no identifier and a duplicated editable name.
        let visual = String::from(
            "<root><renderSet><geometry>             <vertices>geometry0vertices</vertices><stream>geometry0vertices</stream>             <primitiveGroup><material><fx>shaders/nx.fx</fx>             <collisionFlags>2</collisionFlags>             <property>A<Float>1.0</Float></property><property>A<Float>2.0</Float></property>             </material></primitiveGroup>             </geometry></renderSet></root>",
        );
        let mut editor = MaterialEditor::census(
            &fixture.resource,
            chicken_model_doc(),
            datasection::parse("no_ident.visual", visual.as_bytes()).unwrap(),
            "characters/npc/chicken/chicken.primitives",
        );
        let effects = EffectProperties {
            fx: "shaders/nx.fx".to_string(),
            // The duplicated name exercises the dedupe skip; the trailing
            // name matches no backup property.
            editable: vec!["A".to_string(), "A".to_string(), "Z".to_string()],
        };
        editor.clean_materials(&fixture.resource, &effects);
        let path = editor.materials[""].data[0].clone();
        let section = section_at(&editor.visual, &path).unwrap();
        // No identifier stayed absent; the editable property landed once.
        assert!(section.find_child("identifier").is_none());
        let props = open_sections(section, "property");
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].as_string(), "A");

        // set_tint_property over pre-existing properties: the plain write
        // and the feed write both land in place.
        let mut editor = chicken_editor(&fixture);
        let red = editor.tint_material_path("body", "red").unwrap();
        {
            let material = section_at_mut(&mut editor.model, &red).unwrap();
            let prop = material.add_child("property");
            prop.set_string("SpecPower");
            let prop = material.add_child("property");
            prop.set_string("BaseTex");
            prop.write_string("TextureFeed", "feeds/a.feed");
        }
        assert!(editor.set_tint_property(
            &fixture.resource,
            "body",
            "red",
            "SpecPower",
            "Float",
            "3.000000"
        ));
        assert!(editor.set_tint_property(
            &fixture.resource,
            "body",
            "red",
            "BaseTex",
            "Texture",
            "maps/q.dds"
        ));
        let material = section_at(&editor.model, &red).unwrap();
        let props = open_sections(material, "property");
        assert_eq!(props.len(), 2);
        let spec = props
            .iter()
            .find(|prop| prop.as_string() == "SpecPower")
            .unwrap();
        assert_eq!(spec.read_string("Float", ""), "3.000000");
        let base = props
            .iter()
            .find(|prop| prop.as_string() == "BaseTex")
            .unwrap();
        assert_eq!(base.read_string("TextureFeed/default", ""), "maps/q.dds");

        // A material-less tint refuses.
        let mut bare = chicken_editor_bare_tint(&fixture);
        assert!(!bare.set_tint_property(&fixture.resource, "body", "bare", "X", "Float", "1.0"));
    }

    #[test]
    fn naming_moves_edge_faces() {
        // A hand-built editor whose model has no materialNames section.
        let mut editor = MaterialEditor {
            model: DataSection::new("m.model"),
            visual: DataSection::new("m.visual"),
            primitives_name: String::new(),
            materials: BTreeMap::new(),
            tints: BTreeMap::new(),
            dyes: BTreeMap::new(),
            curr_dyes: BTreeMap::new(),
            tinted_materials: BTreeMap::new(),
            names_to_remove: Vec::new(),
            dirty: std::collections::BTreeSet::new(),
        };
        editor.materials.insert(
            "solo".to_string(),
            MaterialEntry {
                name: "solo".to_string(),
                name_data: None,
                data: Vec::new(),
                format: String::new(),
                colours: false,
                dual_uv: false,
            },
        );
        assert!(editor.material_name("solo", "renamed"));
        assert_eq!(editor.material_display_name("solo"), "renamed");
        let names = editor.model.open_section("materialNames").unwrap();
        assert_eq!(names.read_string("material/original", ""), "solo");
        assert_eq!(names.read_string("material/display", ""), "renamed");

        // The same-name move exits first.
        let fixture = chicken_fixture("nameguard");
        let mut editor = chicken_editor(&fixture);
        assert!(editor.matter_name(&fixture.resource, "body", "body", true));
        assert_eq!(editor.exposed_to_script_name("body", "ghost", "X"), "");
    }

    #[test]
    fn shader_mfm_and_flag_refusals() {
        let fixture = chicken_fixture("refuse");
        fixture.write("maps/junk.mfm", b"<unclosed");
        fixture.write(
            "maps/skinned.mfm",
            "<root><fx>shaders/skinned/x.fx</fx></root>",
        );
        fixture.write("maps/over.mfm", "<root><fx>shaders/o.fx</fx></root>");
        let mut editor = chicken_editor(&fixture);
        let mut bare = chicken_editor_bare_tint(&fixture);

        // set_material_shader refusals.
        assert!(!editor.set_material_shader(
            &fixture.resource,
            "ghost",
            "",
            "Default",
            "shaders/x.fx",
            None
        ));
        assert!(!editor.set_material_shader(
            &fixture.resource,
            "skin",
            "ghost",
            "red",
            "shaders/x.fx",
            None
        ));
        assert!(!editor.set_material_shader(
            &fixture.resource,
            "skin",
            "body",
            "red",
            "shaders/skinned/x.fx",
            None
        ));
        assert!(!bare.set_material_shader(
            &fixture.resource,
            "skin",
            "body",
            "bare",
            "shaders/x.fx",
            None
        ));

        // material_mfm refusals.
        let mut fx = None;
        assert!(!editor.material_mfm(
            &fixture.resource,
            "skin",
            "",
            "Default",
            "maps/junk.mfm",
            &mut fx
        ));
        assert!(!editor.material_mfm(
            &fixture.resource,
            "skin",
            "",
            "Default",
            "maps/skinned.mfm",
            &mut fx
        ));
        assert!(!editor.material_mfm(
            &fixture.resource,
            "skin",
            "ghost",
            "red",
            "maps/over.mfm",
            &mut fx
        ));
        assert!(!bare.material_mfm(
            &fixture.resource,
            "skin",
            "body",
            "bare",
            "maps/over.mfm",
            &mut fx
        ));

        // Flag guards.
        editor.set_tint_flag(&fixture.resource, "body", "Default", "materialKind", 1);

        let mut editor = chicken_editor(&fixture);
        let red = editor.tint_material_path("body", "red").unwrap();
        section_at_mut(&mut editor.model, &red)
            .unwrap()
            .write_string("mfm", "maps/junk.mfm");
        assert_eq!(
            editor.tint_flag(&fixture.resource, "body", "red", "materialKind"),
            FLAG_NOT_SET
        );
        let path = editor.materials["skin"].data[0].clone();
        {
            let section = section_at_mut(&mut editor.visual, &path).unwrap();
            section.del_child("collisionFlags");
            section.del_child("materialKind");
            section.write_string("mfm", "maps/junk.mfm");
        }
        assert_eq!(
            editor.material_flag(&fixture.resource, "skin", "collisionFlags"),
            0
        );
        editor.set_material_flag(&fixture.resource, "ghost", "collisionFlags", 1);

        // The property reads refuse on unknown keys.
        assert_eq!(
            editor.material_texture_feed_name(&fixture.resource, "ghost", "", "Default", "X"),
            ""
        );
        assert_eq!(
            editor.material_texture_feed_name(&fixture.resource, "skin", "body", "ghost", "X"),
            ""
        );
        assert_eq!(
            editor.material_property_val(&fixture.resource, "ghost", "", "Default", "X", "Float"),
            ""
        );
        assert_eq!(
            editor.material_property_val(&fixture.resource, "skin", "body", "ghost", "X", "Float"),
            ""
        );
    }

    #[test]
    fn exposed_toggle_guards() {
        let fixture = chicken_fixture("exguard");
        let mut editor = chicken_editor(&fixture);
        // The Default tint and a material-less tint both no-op.
        editor.toggle_exposed("body", "Default", "X", &fixture.resource);
        assert_eq!(editor.exposed_to_script_name("body", "Default", "X"), "");
        let mut bare = chicken_editor_bare_tint(&fixture);
        bare.toggle_exposed("body", "bare", "X", &fixture.resource);
        assert_eq!(bare.exposed_to_script_name("body", "bare", "X"), "");
    }

    #[test]
    fn unknown_matter_moves_report_success() {
        let fixture = chicken_fixture("ghostmatter");
        let mut editor = chicken_editor(&fixture);
        // An unknown matter runs the census and still reports success (the
        // reference's per-part moves are all conditional).
        assert!(editor.matter_name(&fixture.resource, "ghost", "renamed", true));
    }

    #[test]
    fn instantiate_and_overload_branch_ends() {
        let fixture = chicken_fixture("branchends");
        fixture.write(
            "maps/any.mfm",
            "<root><fx>shaders/o.fx</fx>             <property>BaseTex<Texture>maps/t.dds</Texture></property></root>",
        );
        let mut editor = chicken_editor(&fixture);
        let path = editor.materials["skin"].data[0].clone();

        // An identifier-free section still expands.
        {
            let section = section_at_mut(&mut editor.visual, &path).unwrap();
            section.del_children();
            section.write_string("mfm", "maps/any.mfm");
        }
        editor.instantiate_mfm(&path, &fixture.resource);
        assert_eq!(
            section_at(&editor.visual, &path)
                .unwrap()
                .read_string("fx", ""),
            "shaders/o.fx"
        );

        // An fx-free mfm still layers the flags without touching the fx.
        let no_fx = datasection::parse(
            "plain.mfm",
            b"<root><collisionFlags>7</collisionFlags></root>",
        )
        .unwrap();
        editor.overload_mfm(&path, &no_fx);
        let section = section_at(&editor.visual, &path).unwrap();
        assert_eq!(section.read_int("collisionFlags", 0), 7);
        assert_eq!(section.read_string("fx", ""), "shaders/o.fx");
    }

    #[test]
    fn clean_faces_skip_identifier_free_backups() {
        let fixture = chicken_fixture("noident");
        // The tint's material child loses its identifier.
        let mut model = chicken_model_doc();
        let dye_index = child_index_named(&model, "dye").expect("dye");
        {
            let dye = model.child_mut(dye_index).unwrap();
            let tint_index = child_index_named(dye, "tint").expect("tint");
            let tint = dye.child_mut(tint_index).unwrap();
            let material_index = child_index_named(tint, "material").expect("material");
            tint.child_mut(material_index)
                .unwrap()
                .del_child("identifier");
        }
        let mut editor = MaterialEditor::census(
            &fixture.resource,
            model,
            chicken_visual_doc(),
            "characters/npc/chicken/chicken.primitives",
        );
        let effects = EffectProperties {
            fx: "shaders/nx.fx".to_string(),
            editable: Vec::new(),
        };
        editor.clean_tints(&fixture.resource, &effects);
        let path = editor.tint_material_path("body", "red").unwrap();
        let section = section_at(&editor.model, &path).unwrap();
        assert!(section.find_child("identifier").is_none());
        // The rewrite takes the backup's fx, not the effect's.
        assert_eq!(section.read_string("fx", ""), "shaders/std_effects/pffx.fx");
        assert_eq!(section.read_int("collisionFlags", 0), 0);

        // A census without stale aliases cleans nothing.
        let mut editor = chicken_editor(&fixture);
        editor.clean_material_names();
    }

    #[test]
    fn material_flag_zero_without_a_reference() {
        let fixture = chicken_fixture("flagzero");
        let mut editor = chicken_editor(&fixture);
        let path = editor.materials["skin"].data[0].clone();
        section_at_mut(&mut editor.visual, &path)
            .unwrap()
            .del_child("collisionFlags");
        assert_eq!(
            editor.material_flag(&fixture.resource, "skin", "collisionFlags"),
            0
        );
    }

    #[test]
    fn texture_feed_write_faces() {
        let fixture = chicken_fixture("feedwrite");
        let mut editor = chicken_editor(&fixture);
        let path = editor.materials["skin"].data[0].clone();
        {
            let section = section_at_mut(&mut editor.visual, &path).unwrap();
            let prop = section.add_child("property");
            prop.set_string("BaseTex");
            prop.write_string("Texture", "maps/plain.dds");
        }
        // The unknown material and the material-less tint both no-op.
        editor.set_material_texture_feed(
            &fixture.resource,
            "ghost",
            "",
            "Default",
            "BaseTex",
            "feeds/x.feed",
        );
        let mut bare = chicken_editor_bare_tint(&fixture);
        bare.set_material_texture_feed(
            &fixture.resource,
            "skin",
            "body",
            "bare",
            "BaseTex",
            "feeds/x.feed",
        );
        // The material face turns the texture into the feed.
        editor.set_material_texture_feed(
            &fixture.resource,
            "skin",
            "",
            "Default",
            "BaseTex",
            "feeds/x.feed",
        );
        let section = section_at(&editor.visual, &path).unwrap();
        let base = open_sections(section, "property")
            .into_iter()
            .find(|prop| prop.as_string() == "BaseTex")
            .expect("the property survived");
        assert_eq!(base.read_string("TextureFeed", ""), "feeds/x.feed");
        assert!(base.find_child("Texture").is_none());
    }

    #[test]
    fn set_material_property_plain_and_foreign_props() {
        let fixture = chicken_fixture("propfaces");
        let mut editor = chicken_editor(&fixture);
        let path = editor.materials["skin"].data[0].clone();
        {
            let section = section_at_mut(&mut editor.visual, &path).unwrap();
            let prop = section.add_child("property");
            prop.set_string("Other");
            let prop = section.add_child("property");
            prop.set_string("SpecPower");
            prop.write_string("Float", "1.000000");
        }
        assert!(editor.set_material_property(
            &fixture.resource,
            "skin",
            "SpecPower",
            "Float",
            "2.500000"
        ));
        let section = section_at(&editor.visual, &path).unwrap();
        let spec = open_sections(section, "property")
            .into_iter()
            .find(|prop| prop.as_string() == "SpecPower")
            .unwrap();
        assert_eq!(spec.read_string("Float", ""), "2.500000");
    }

    #[test]
    fn shader_mfm_delete_save_extra_faces() {
        let fixture = chicken_fixture("extrafaces");
        fixture.write("maps/junk.mfm", b"<unclosed");
        fixture.write(
            "maps/skinned.mfm",
            "<root><fx>shaders/skinned/x.fx</fx></root>",
        );
        fixture.write("maps/exists.mfm", "<root><stale>true</stale></root>");
        let mut editor = chicken_editor(&fixture);

        // The shader read refuses an unknown material.
        assert_eq!(
            editor.material_shader(&fixture.resource, "ghost", "", "Default"),
            ""
        );
        // The tint face gates on the same shader rule.
        let mut fx = None;
        assert!(!editor.material_mfm(
            &fixture.resource,
            "skin",
            "body",
            "red",
            "maps/skinned.mfm",
            &mut fx
        ));
        assert!(fx.is_none());
        // An unknown tint deletes nothing.
        editor.delete_tint(&fixture.resource, "body", "ghost");
        // An existing mfm file is overwritten in place.
        assert!(editor.save_mfm(&fixture.resource, "skin", "", "Default", "maps/exists.mfm"));
        let saved = fixture
            .resource
            .file_system()
            .read_file("maps/exists.mfm")
            .expect("the mfm was written");
        assert!(!saved.windows(4).any(|w| w == b"stale"));
    }

    #[test]
    fn clean_materials_skips_editables_without_a_backup_property() {
        let fixture = chicken_fixture("noeditable");
        let mut editor = chicken_editor(&fixture);
        let effects = EffectProperties {
            fx: "shaders/std_effects/pffx.fx".to_string(),
            editable: vec!["BaseTex".to_string(), "Ghost".to_string()],
        };
        editor.clean_materials(&fixture.resource, &effects);
        let path = editor.materials["skin"].data[0].clone();
        let props: Vec<String> =
            open_sections(section_at(&editor.visual, &path).unwrap(), "property")
                .into_iter()
                .map(|prop| prop.as_string().to_string())
                .collect();
        // The chicken material carries no editable properties, so both
        // names stay off.
        assert!(!props.contains(&"BaseTex".to_string()));
        assert!(!props.contains(&"Ghost".to_string()));
    }

    #[test]
    fn census_skips_foreign_alias_children_and_slashed_vertices() {
        let fixture = chicken_fixture("foreignalias");
        let mut model = chicken_model_doc();
        let names_index = child_index_named(&model, "materialNames").expect("materialNames");
        model
            .child_mut(names_index)
            .unwrap()
            .add_child("keepme")
            .write_string("note", "not a material");
        let visual = String::from(
            "<root><node><identifier>scene root</identifier>             <node><identifier>body</identifier></node></node>             <renderSet><nodes>body</nodes>             <geometry><vertices>sets/geometry0vertices</vertices>             <stream>geometry0vertices</stream>             <primitiveGroup><material><identifier>skin</identifier>             <collisionFlags>0</collisionFlags></material></primitiveGroup>             </geometry></renderSet></root>",
        );
        let editor = MaterialEditor::census(
            &fixture.resource,
            model,
            datasection::parse("slashed.visual", visual.as_bytes()).unwrap(),
            "characters/npc/chicken/chicken.primitives",
        );
        assert!(editor.materials.contains_key("skin"));
        assert_eq!(editor.material_display_name("skin"), "plumage");
    }

    #[test]
    fn effect_scans_reject_partial_sources() {
        assert!(!MaterialEditor::effect_is_skybox(""));
        assert!(!MaterialEditor::effect_is_skybox("plain text"));
        assert!(!MaterialEditor::effect_has_normal_map(""));
        assert!(!MaterialEditor::effect_has_normal_map("plain text"));
    }

    #[test]
    fn naming_and_clean_fallbacks_on_a_bare_editor() {
        let fixture = chicken_fixture("barefallback");
        let mut editor = MaterialEditor {
            model: DataSection::new("m.model"),
            visual: DataSection::new("m.visual"),
            primitives_name: String::new(),
            materials: BTreeMap::new(),
            tints: BTreeMap::new(),
            dyes: BTreeMap::new(),
            curr_dyes: BTreeMap::new(),
            tinted_materials: BTreeMap::new(),
            names_to_remove: Vec::new(),
            dirty: std::collections::BTreeSet::new(),
        };
        // An alias with no matching child in materialNames: nothing is
        // appended and the entry renames anyway.
        editor.model.add_child("materialNames");
        let names_index = child_index_named(&editor.model, "materialNames").expect("just added");
        editor
            .model
            .child_mut(names_index)
            .expect("just added")
            .add_child("material")
            .write_string("original", "other");
        editor.materials.insert(
            "solo".to_string(),
            MaterialEntry {
                name: "solo".to_string(),
                name_data: Some("orig".to_string()),
                data: Vec::new(),
                format: String::new(),
                colours: false,
                dual_uv: false,
            },
        );
        // An entry with no sections: the clean loop skips it.
        editor.materials.insert(
            "empty".to_string(),
            MaterialEntry {
                name: String::new(),
                name_data: None,
                data: Vec::new(),
                format: String::new(),
                colours: false,
                dual_uv: false,
            },
        );
        let effects = EffectProperties {
            fx: "shaders/x.fx".to_string(),
            editable: Vec::new(),
        };
        editor.clean_materials(&fixture.resource, &effects);
        assert!(editor.material_name("solo", "renamed"));
        assert_eq!(editor.material_display_name("solo"), "renamed");
        let names = editor.model.open_section("materialNames").unwrap();
        // Only the non-matching child we seeded; nothing was appended.
        assert_eq!(names.count_children(), 1);

        // A queued stale alias with no materialNames section cleans
        // nothing (the pre-census case).
        let mut pre = MaterialEditor {
            model: DataSection::new("p.model"),
            visual: DataSection::new("p.visual"),
            primitives_name: String::new(),
            materials: BTreeMap::new(),
            tints: BTreeMap::new(),
            dyes: BTreeMap::new(),
            curr_dyes: BTreeMap::new(),
            tinted_materials: BTreeMap::new(),
            names_to_remove: vec![0],
            dirty: std::collections::BTreeSet::new(),
        };
        pre.clean_material_names();
    }

    // ------------------------------------------------------------------
    // The acceptance face: exported fields and render-sample inputs stay
    // comparable across a save/reload cycle.
    // ------------------------------------------------------------------

    #[test]
    fn exported_fields_and_render_inputs_survive_a_save_reload_cycle() {
        let fixture = chicken_fixture("accept");
        let mut editor = chicken_editor(&fixture);

        // A spread of render-input moves.
        assert!(editor.set_material_shader(
            &fixture.resource,
            "skin",
            "",
            "Default",
            "shaders/std_effects/pffx.fx",
            None
        ));
        assert!(editor.set_material_property(
            &fixture.resource,
            "skin",
            "BaseTex",
            "Texture",
            "maps/chicken.dds"
        ));
        editor.set_material_flag(&fixture.resource, "skin", "materialKind", 4);
        assert!(editor.set_tint_property(
            &fixture.resource,
            "body",
            "red",
            "SpecPower",
            "Float",
            "2.500000"
        ));
        editor.set_tint_flag(&fixture.resource, "body", "red", "materialKind", 7);
        assert!(editor.material_name("skin", "feathers"));
        let matter = editor.new_tint(
            &fixture.resource,
            "skin",
            "body",
            "red",
            "gold",
            "shaders/gold.fx",
            "",
        );
        assert_eq!(matter, "body");

        // Save both documents and re-census from the saved files.
        let mut model = editor.model.clone();
        let mut visual = editor.visual.clone();
        fixture.write("out/chicken.model", model.write_to_string());
        fixture.write("out/chicken.visual", visual.write_to_string());
        let mut re = MaterialEditor::census(
            &fixture.resource,
            fixture.section("out/chicken.model").unwrap(),
            fixture.section("out/chicken.visual").unwrap(),
            "characters/npc/chicken/chicken.primitives",
        );

        // The exported tables and every render-sample input compare.
        assert_eq!(re.material_display_name("skin"), "feathers");
        assert_eq!(re.materials["skin"].format, "xyznrvt2");
        assert!(!re.materials["skin"].dual_uv);
        assert_eq!(
            re.material_shader(&fixture.resource, "skin", "", "Default"),
            "shaders/std_effects/pffx.fx"
        );
        assert_eq!(
            re.material_property_val(
                &fixture.resource,
                "skin",
                "",
                "Default",
                "BaseTex",
                "Texture"
            ),
            "maps/chicken.dds"
        );
        assert_eq!(
            re.material_flag(&fixture.resource, "skin", "materialKind"),
            4
        );
        assert!(re.tints["body"].contains_key("red"));
        assert!(re.tints["body"].contains_key("gold"));
        assert_eq!(
            re.material_property_val(
                &fixture.resource,
                "skin",
                "body",
                "red",
                "SpecPower",
                "Float"
            ),
            "2.500000"
        );
        assert_eq!(
            re.tint_flag(&fixture.resource, "body", "red", "materialKind"),
            7
        );
        assert_eq!(re.tinted_materials["skin"], "body");
        assert_eq!(
            re.tint_material_path("body", "gold")
                .and_then(|path| section_at(&re.model, &path).map(|m| m.read_string("fx", ""))),
            Some("shaders/gold.fx".to_string())
        );
    }
}
