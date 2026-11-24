//! BW-13-04-01: the ModelEditor's model open and save face.
//!
//! A headless port of the `tools/modeleditor_core/Models/` faces that open a
//! `.model` file, validate it the way `Mutant::ensureModelValid` does
//! (`mutant_validation.cpp`), and save it the way `Mutant::save`/`saveAs` do
//! (`mutant.cpp`), plus the metadata block the save flow refreshes
//! (`lib/gizmo/meta_data*.cpp`). The read contract — what the original
//! consumes when it loads the file back (`lib/model/model.cpp`,
//! `model_common.hpp`) — is carried as [`extent`], [`parent_resource_id`],
//! [`anim_count`], [`nodefull`], [`has_visibility_box`] and the LOD
//! constants, so a saved model can be judged against the original loader.
//!
//! Faces and seams:
//! - the visual-type cascade follows the *editor* variant
//!   (`ensureModelValid` reads non-empty strings), which is stricter than the
//!   runtime's `createModelFromFile` (`model_common.hpp` tests tag presence);
//! - `locateFile` is the file-dialog seam: a missing `.visual`, `.primitives`
//!   or parent `.model` fails the open instead of asking where the file went,
//!   and the relocation fix-ups that ride on it (`clipToDiffRoot`,
//!   `fixTextures`, `fixTexAnim`, `instantiateMFM`) belong to the material
//!   faces (BW-13-04-02);
//! - `cleanMaterials`/`cleanTints`/`cleanMaterialNames` run at the top of the
//!   reference save; they are the BW-13-04-02 material faces, so [`ModelMutator`]
//!   carries their hook (the dirty set) and their placement;
//! - `Mutant::dirty()` consults `UndoRedo::needsSave` — the undo/redo engine
//!   itself is the `chunk::undoredo` port (`tools/common/undoredo.cpp`), so
//!   this module models the save point with a local flag fed by
//!   [`ModelMutator::note_operation`];
//! - read-only files (`GetFileAttributes`/`FILE_ATTRIBUTE_READONLY`) are a
//!   Windows attribute face; the mutator takes the verdict as a flag;
//! - the `WorldEditorUpdate` mailslot notification at the end of
//!   `Mutant::save` is a Win32 seam and is not carried;
//! - sections are held owned rather than through the reference's shared
//!   census: the reference mutates the cached `DataSectionPtr`, this port
//!   mutates its own copy and the write joins the cache on save;
//! - dirty sections the mutator does not hold are rewritten from their on-disk
//!   state; in-memory-only sections (the anim frame-rate scratch files) arrive
//!   with the faces that create them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use resources::binary::{BinSection, BinaryBlock};
use resources::bwresource::{self, BwResource};
use resources::datasection::{self, DataSection};

use crate::primitive_file_structs::VertexHeader;

/// `Model::LOD_MAX` (`model.cpp:200`): the default `extent` — never hide.
pub const LOD_MAX: f32 = f32::MAX;

/// `Model::LOD_HIDDEN` (`model.cpp:182`): an `extent` of `-1` hides the model.
pub const LOD_HIDDEN: f32 = -1.0;

/// `Model::LOD_MISSING` (`model.cpp:188`): the marker for a model whose LOD
/// parent chain broke.
pub const LOD_MISSING: f32 = -2.0;

/// `MetaData::METADATA_SECTION_NAME` (`meta_data_define.hpp:8`).
pub const METADATA_SECTION_NAME: &str = "metaData";

/// The `AutoConfigString` default behind `s_modelMetaDataConfig`
/// (`model.cpp:44`): the metadata description config the editor reads.
pub const MODEL_METADATA_CONFIG: &str = "helpers/meta_data/model.xml";

/// The kinds of visual bulk a `.model` file can reference, in the order the
/// editor's cascade checks them (`mutant_validation.cpp`, `mutant.cpp
/// saveAs`). The runtime (`model_common.hpp`) has the billboard branch
/// commented out; the editor still accepts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualType {
    /// `<nodefullVisual>` — a node hierarchy visual.
    Nodefull,
    /// `<nodelessVisual>` — a flat visual.
    Nodeless,
    /// `<billboardVisual>` — an always-facing visual.
    Billboard,
}

impl VisualType {
    /// The `.model` tag that names this visual kind.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Nodefull => "nodefullVisual",
            Self::Nodeless => "nodelessVisual",
            Self::Billboard => "billboardVisual",
        }
    }
}

/// The editor's visual-type determination (`ensureModelValid`): the first
/// visual tag whose *string* is non-empty wins, and a file with none cannot
/// be opened or saved. The runtime accepts mere tag presence
/// (`createModelFromFile`); the editor is stricter, and an empty tag that
/// the runtime would still load makes the editor refuse the file.
pub fn detect_editor_visual_type(model: &DataSection) -> Option<VisualType> {
    [
        VisualType::Nodefull,
        VisualType::Nodeless,
        VisualType::Billboard,
    ]
    .into_iter()
    .find(|kind| !model.read_string(kind.tag(), "").is_empty())
}

/// Why a model could not be opened or saved. The message texts are the
/// reference `ERROR_MSG` formats, byte for byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// `ensureModelValid`'s "Cannot determine the visual type" arm.
    CannotDetermineVisualType {
        /// The `.model` resource id.
        model: String,
        /// The operation name the reference formats into the message.
        what: String,
    },
    /// The primitives file the visual names does not exist.
    MissingPrimitives {
        /// The `.model` resource id.
        model: String,
        /// The `.primitives` resource id that is missing.
        primitives: String,
        /// The operation name the reference formats into the message.
        what: String,
    },
    /// `isFormatDepreciated`: the vertices use a retired format.
    DeprecatedFormat {
        /// The `.model` resource id.
        model: String,
    },
    /// The reference's plain `return false` arms: the `.model` section, the
    /// `.visual` section, the parent `.model` or a file in the parent chain
    /// could not be opened (the dialog seam failed silently). No message is
    /// printed in the reference either.
    Invalid,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CannotDetermineVisualType { model, what } => write!(
                f,
                "Cannot determine the visual type of the model\"{model}\".\nUnable to {what} model."
            ),
            Self::MissingPrimitives {
                model,
                primitives,
                what,
            } => write!(
                f,
                "Unable to find the primitives file for \"{model}\".\n\n\
                 Make sure that \"{primitives}\" exists.\n\n\
                 Unable to {what} model.\n"
            ),
            Self::DeprecatedFormat { model } => write!(
                f,
                "The model: \"{model}\" is using a depreciated vertex format.\n\
                 Please re-export this model and try again.\n"
            ),
            Self::Invalid => Ok(()),
        }
    }
}

/// An opened, editable `.model`: the model section, the visual section it
/// names, the resolved primitives file and the loaded metadata. This is the
/// port of the state `Mutant` keeps around `currModel_`/`currVisual_` after
/// `ensureModelValid` has judged the file.
pub struct ModelDoc {
    /// The `.model` resource id.
    pub path: String,
    /// The `<name>.model` section (owned, editable).
    pub model: DataSection,
    /// The visual kind the editor cascade detected.
    pub visual_type: VisualType,
    /// `<root>.visual` — the visual resource id.
    pub visual_name: String,
    /// The visual section (owned, editable).
    pub visual: DataSection,
    /// `primitivesName` (or the visual root) with `.primitives` appended —
    /// the resolved container `ensureModelValid` demands.
    pub primitives_name: String,
    /// The `metaData` block, loaded against the model metadata description.
    pub metadata: MetaData,
}

impl ModelDoc {
    /// `Mutant::ensureModelValid` for the open face: judge the file, resolve
    /// its visual and primitives, walk the parent chain, refuse retired
    /// vertex formats, and load the metadata.
    ///
    /// `what` is the operation name the reference folds into its error
    /// messages ("load", "add", "read", ...).
    pub fn open(resource: &BwResource, name: &str, what: &str) -> Result<ModelDoc, ModelError> {
        Self::open_inner(resource, name, what, 0)
    }

    fn open_inner(
        resource: &BwResource,
        name: &str,
        what: &str,
        depth: u32,
    ) -> Result<ModelDoc, ModelError> {
        // The .model section itself. An unparseable XML file opens as NULL
        // the same way (`createAppropriateSection` returns a null section
        // when the content claims to be XML but is not).
        let Some(raw) = resource.file_system().read_file(name) else {
            return Err(ModelError::Invalid);
        };
        let Ok(model) = datasection::parse(bwresource::get_filename(name), &raw) else {
            return Err(ModelError::Invalid);
        };

        // The visual type and its root name. No visual tag with a value
        // means the file cannot be judged.
        let Some(visual_type) = detect_editor_visual_type(&model) else {
            return Err(ModelError::CannotDetermineVisualType {
                model: name.to_string(),
                what: what.to_string(),
            });
        };
        let root = model.read_string(visual_type.tag(), "");

        // The visual file. Missing means the locate dialog would come up;
        // headless, the open fails the way `locateFile` failing does.
        let visual_name = format!("{root}.visual");
        let Some(visual_raw) = resource.file_system().read_file(&visual_name) else {
            return Err(ModelError::Invalid);
        };
        let Ok(visual) = datasection::parse(bwresource::get_filename(&visual_name), &visual_raw)
        else {
            return Err(ModelError::Invalid);
        };

        // The primitives container. The reference probes it with
        // `openSection`, whose binary fallback (`BinSection`) yields a
        // section for any existing file and nothing else, so this is an
        // existence check by another name.
        let primitives_name = format!("{}.primitives", visual.read_string("primitivesName", &root));
        if !resource.file_exists(&primitives_name) {
            return Err(ModelError::MissingPrimitives {
                model: name.to_string(),
                primitives: primitives_name,
                what: what.to_string(),
            });
        }

        // The parent chain: the parent must open and must itself be a valid
        // model (`ensureModelValid` recurses). The depth guard is port
        // defence — a cyclic parent chain exhausts the reference stack.
        let parent = model.read_string("parent", "");
        if !parent.is_empty() {
            if depth >= 64 {
                return Err(ModelError::Invalid);
            }
            Self::open_inner(resource, &format!("{parent}.model"), what, depth + 1)?;
        }

        // The retired-format check comes last, after the chain is sound.
        if is_format_deprecated(resource, &visual, &primitives_name) {
            return Err(ModelError::DeprecatedFormat {
                model: name.to_string(),
            });
        }

        let metadata = MetaData::load(Some(&model), &model_metadata_desc(resource));
        Ok(ModelDoc {
            path: name.to_string(),
            model,
            visual_type,
            visual_name,
            visual,
            primitives_name,
            metadata,
        })
    }
}

/// `Model::load`'s extent read (`model.cpp:307`): `readFloat("extent",
/// Model::LOD_MAX)` — a model without an extent never hides.
pub fn extent(model: &DataSection) -> f32 {
    model.read_float("extent", LOD_MAX)
}

/// `Model::load`'s parent read (`model.cpp:297`): the `parent` string names
/// another model file without its extension, so the resource id gains
/// `.model`.
pub fn parent_resource_id(model: &DataSection) -> Option<String> {
    let parent = model.read_string("parent", "");
    if parent.is_empty() {
        None
    } else {
        Some(format!("{parent}.model"))
    }
}

/// `Mutant::animCount` (`mutant.cpp:144`): count `animation` sections over
/// this model and every ancestor. The walk appends `.model` to each `parent`
/// string and stops when that leaves the bare `.model` (no parent), which is
/// also where a missing file stops it.
pub fn anim_count(resource: &BwResource, model_path: &str) -> u32 {
    let mut count = 0;
    let mut model_path = model_path.to_string();
    while let Some(raw) = resource.file_system().read_file(&model_path) {
        let Ok(model) = datasection::parse(bwresource::get_filename(&model_path), &raw) else {
            break;
        };
        count += open_sections(&model, "animation").len() as u32;
        let next = format!("{}.model", model.read_string("parent", ""));
        if next == ".model" {
            break;
        }
        model_path = next;
    }
    count
}

/// `Mutant::nodefull` (`mutant.cpp:178`): the model carries a
/// `nodefullVisual` string.
pub fn nodefull(resource: &BwResource, model_path: &str) -> bool {
    if model_path.is_empty() {
        return false;
    }
    let Some(raw) = resource.file_system().read_file(model_path) else {
        return false;
    };
    let Ok(model) = datasection::parse(bwresource::get_filename(model_path), &raw) else {
        return false;
    };
    !model.read_string("nodefullVisual", "").is_empty()
}

/// `Mutant::hasVisibilityBox` (`mutant.cpp:130`): both corners of the box
/// exist. An empty path answers false, as the reference does.
pub fn has_visibility_box(resource: &BwResource, model_path: &str) -> bool {
    if model_path.is_empty() {
        return false;
    }
    let Some(raw) = resource.file_system().read_file(model_path) else {
        return false;
    };
    let Ok(model) = datasection::parse(bwresource::get_filename(model_path), &raw) else {
        return false;
    };
    model.open_section("visibilityBox/min").is_some()
        && model.open_section("visibilityBox/max").is_some()
}

/// `PrimitiveFile::get` (`primitive_file.cpp:40`): the `.processed`
/// container first, then the bare resource. Binary content only — an XML
/// file is never a primitives container, and the reference's empty-binary
/// fallback becomes the caller's empty container.
fn primitives_file(resource: &BwResource, name: &str) -> Option<BinSection> {
    for candidate in [format!("{name}.processed"), name.to_string()] {
        let Some(raw) = resource.file_system().read_file(&candidate) else {
            continue;
        };
        let looks_xml = raw
            .iter()
            .find(|&&b| b != b' ' && b != b'\t' && b != b'\r' && b != b'\n')
            == Some(&b'<');
        if !looks_xml {
            let mut section = BinSection::new(&candidate, BinaryBlock::new(raw));
            section.introspect();
            return Some(section);
        }
    }
    None
}

/// `splitOldPrimitiveName` (`primitive_file.cpp`): split `file/part` at the
/// last dot before the slash — except that a `.static` marker sitting
/// between two dots is not taken off the file.
fn split_old_primitive_name(resource_id: &str) -> (String, String) {
    let bytes = resource_id.as_bytes();
    let mut slash_pos = bytes.len() as i32 - 1;
    let mut dot_pos = slash_pos;
    let mut dot_pre = dot_pos;
    while slash_pos >= 0 {
        let c = bytes[slash_pos as usize];
        if c == b'.' {
            dot_pre = dot_pos;
            dot_pos = slash_pos;
        }
        if c == b'/' || c == b'\\' {
            break;
        }
        slash_pos -= 1;
    }
    // don't take off a .static
    if dot_pre - dot_pos == 7
        && resource_id.get(dot_pos as usize..dot_pre as usize) == Some(".static")
    {
        dot_pos = dot_pre;
    }
    let cut = dot_pos.max(0) as usize;
    (
        resource_id[..cut.min(resource_id.len())].to_string(),
        resource_id[resource_id.len().min(cut + 1)..].to_string(),
    )
}

/// `Mutant::isFormatDepreciated` (`mutant_validation.cpp`): every geometry's
/// vertices blob in the primitives container carries a `VertexHeader`, and
/// the two shipped-with-the-original formats (`xyznuvi`, `xyznuvitb`) are
/// retired. Old-style `file/part` vertices strings are split and only the
/// part is looked up in this model's own container, like the reference. A
/// container that will not open behaves as empty (`PrimitiveFile::get`
/// "always succeeds" into an empty directory).
fn is_format_deprecated(
    resource: &BwResource,
    visual: &DataSection,
    primitives_name: &str,
) -> bool {
    let mut prim_file = primitives_file(resource, primitives_name)
        .unwrap_or_else(|| BinSection::new(primitives_name, BinaryBlock::default()));
    for render_set in open_sections(visual, "renderSet") {
        for geometry in open_sections(render_set, "geometry") {
            let mut vert_group = geometry.read_string("vertices", "");
            if vert_group.contains('/') {
                let (_file, part) = split_old_primitive_name(&vert_group);
                vert_group = part;
            }
            let Some(blob) = prim_file.read_binary(&vert_group) else {
                continue;
            };
            let Some(header) = VertexHeader::read(blob.data()) else {
                continue;
            };
            if header.vertex_format == "xyznuvi" || header.vertex_format == "xyznuvitb" {
                return true;
            }
        }
    }
    false
}

/// The children named `tag` — `DataSection::openSections`.
fn open_sections<'a>(section: &'a DataSection, tag: &str) -> Vec<&'a DataSection> {
    (0..section.count_children())
        .filter_map(|index| section.open_child(index))
        .filter(|child| child.section_name() == tag)
        .collect()
}

/// `readInt64`/`writeInt64` have no direct DataSection wrappers in this
/// port; read through the child section (`DataSection::readInt64(tag, 0)`).
fn read_int64(section: &DataSection, path: &str) -> i64 {
    section
        .open_section(path)
        .map(|child| child.as_int64(0))
        .unwrap_or(0)
}

/// `writeInt64(tag, value)`: create the child and set it.
fn write_int64(section: &mut DataSection, path: &str, value: i64) {
    if let Some(child) = section.open_or_make_section(path) {
        child.set_int64(value);
    }
}

// ----------------------------------------------------------------------
// Metadata (`lib/gizmo/meta_data*.cpp`).
// ----------------------------------------------------------------------

/// One comment in a COMMENTS metadata property (`Comment`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaComment {
    /// Who wrote it.
    pub username: String,
    /// When, as a raw `time_t`.
    pub time: i64,
    /// What it says.
    pub content: String,
}

/// The value of one metadata property, per property kind.
#[derive(Debug, Clone, PartialEq)]
pub enum MetaValue {
    /// STRING — `TextProperty`.
    Text(String),
    /// INT — `GenIntProperty`.
    Int(i32),
    /// FLOAT — `GenFloatProperty`.
    Float(f32),
    /// DATETIME — a raw `time_t`.
    DateTime(i64),
    /// COMMENTS — the comment list.
    Comments(Vec<MetaComment>),
}

impl MetaValue {
    /// The kind's default value (`Datatype::DefaultValue`): strings are
    /// empty, numbers zero, comment lists empty.
    pub fn is_default(&self) -> bool {
        match self {
            Self::Text(text) => text.is_empty(),
            Self::Int(value) => *value == 0,
            Self::Float(value) => *value == 0.0,
            Self::DateTime(value) => *value == 0,
            Self::Comments(list) => list.is_empty(),
        }
    }
}

/// The kind of one metadata property (`PropertyType` names).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaPropKind {
    /// STRING.
    Text,
    /// INT.
    Int,
    /// FLOAT.
    Float,
    /// DATETIME.
    DateTime,
    /// COMMENTS.
    Comments,
}

impl MetaPropKind {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "STRING" => Some(Self::Text),
            "INT" => Some(Self::Int),
            "FLOAT" => Some(Self::Float),
            "DATETIME" => Some(Self::DateTime),
            "COMMENTS" => Some(Self::Comments),
            _ => None,
        }
    }
}

/// One metadata property description (`PropertyDesc`).
#[derive(Debug, Clone, PartialEq)]
pub struct MetaPropDesc {
    /// The property name — also the section it lives under.
    pub name: String,
    /// The value kind.
    pub kind: MetaPropKind,
    /// The display text of the `<description>` child (empty when the
    /// description has none).
    pub description: String,
    /// The `readonly` flag (`descSection()->readBool("readonly", false)`).
    pub read_only: bool,
}

/// The metadata description (`MetaData::Desc`): which properties a model
/// file carries, in the reference's order — a `BW::map` keyed by name, so
/// the properties always come out name-sorted.
#[derive(Debug, Clone, PartialEq)]
pub struct MetaDataDesc {
    props: Vec<MetaPropDesc>,
}

impl MetaDataDesc {
    /// `Desc::addDefaultDesc` (`meta_data.cpp:133`): the six properties the
    /// editor always carries — description, comments, the created/modified
    /// stamps, with by/modified fields read-only.
    pub fn default_desc() -> Self {
        let props: Vec<MetaPropDesc> = [
            ("description", MetaPropKind::Text, "", false),
            ("comments", MetaPropKind::Comments, "", false),
            ("created_on", MetaPropKind::DateTime, "created on", false),
            ("created_by", MetaPropKind::Text, "", true),
            ("modified_on", MetaPropKind::DateTime, "modified on", false),
            ("modified_by", MetaPropKind::Text, "", true),
        ]
        .into_iter()
        .map(|(name, kind, description, read_only)| MetaPropDesc {
            name: name.to_string(),
            kind,
            description: description.to_string(),
            read_only,
        })
        .collect();
        // `propDescs_` is a BW::map keyed by name, so even the bare defaults
        // iterate name-sorted.
        let mut props = props;
        props.sort_by(|a, b| a.name.cmp(&b.name));
        Self { props }
    }

    /// `Desc::load` (`meta_data.cpp:58`): the defaults first, then every
    /// child of the config section layered on top (keyed by name, so a
    /// config entry replaces a default of the same name). Unknown type
    /// names are skipped with the reference's load failure.
    pub fn from_config(config: Option<&DataSection>) -> Self {
        let mut desc = Self::default_desc();
        let Some(config) = config else {
            return desc;
        };
        for index in 0..config.count_children() {
            let Some(child) = config.open_child(index) else {
                continue;
            };
            let Some(kind) = MetaPropKind::from_name(&child.read_string("type", "")) else {
                continue;
            };
            let entry = MetaPropDesc {
                name: child.section_name().to_string(),
                kind,
                description: child
                    .open_section("description")
                    .map(|d| d.as_string().to_string())
                    .unwrap_or_default(),
                read_only: child.read_bool("readonly", false),
            };
            match desc.props.iter().position(|p| p.name == entry.name) {
                Some(at) => desc.props[at] = entry,
                None => desc.props.push(entry),
            }
        }
        desc.props.sort_by(|a, b| a.name.cmp(&b.name));
        desc
    }

    /// The property descriptions, name-sorted.
    pub fn props(&self) -> &[MetaPropDesc] {
        &self.props
    }
}

/// The model metadata description: `s_modelMetaDataConfig` names
/// [`MODEL_METADATA_CONFIG`]; a file there layers over the defaults, its
/// absence leaves the defaults alone (`AutoConfigString` + `Desc(config)`).
pub fn model_metadata_desc(resource: &BwResource) -> MetaDataDesc {
    let config = resource
        .file_system()
        .read_file(MODEL_METADATA_CONFIG)
        .and_then(|raw| datasection::parse("model.xml", &raw).ok());
    MetaDataDesc::from_config(config.as_ref())
}

/// The loaded metadata of one model (`MetaData::MetaData`): every described
/// property with its value.
#[derive(Debug, Clone, PartialEq)]
pub struct MetaData {
    props: Vec<(MetaPropDesc, MetaValue)>,
}

impl MetaData {
    /// `MetaData::load` (`meta_data.cpp:233`): open the model's `metaData`
    /// child (never creating it) and read each described property. A
    /// missing model section leaves every property at its default, and so
    /// do missing values.
    pub fn load(model: Option<&DataSection>, desc: &MetaDataDesc) -> Self {
        let ds = model.and_then(|model| model.open_section(METADATA_SECTION_NAME));
        let props = desc
            .props()
            .iter()
            .map(|desc| {
                let value = match desc.kind {
                    MetaPropKind::Text => MetaValue::Text(
                        ds.map(|ds| ds.read_string(&desc.name, ""))
                            .unwrap_or_default(),
                    ),
                    MetaPropKind::Int => {
                        MetaValue::Int(ds.map(|ds| ds.read_int(&desc.name, 0)).unwrap_or(0))
                    }
                    MetaPropKind::Float => {
                        MetaValue::Float(ds.map(|ds| ds.read_float(&desc.name, 0.0)).unwrap_or(0.0))
                    }
                    MetaPropKind::DateTime => {
                        MetaValue::DateTime(ds.map(|ds| read_int64(ds, &desc.name)).unwrap_or(0))
                    }
                    MetaPropKind::Comments => MetaValue::Comments(
                        ds.and_then(|ds| ds.open_section(&desc.name))
                            .map(|prop| {
                                open_sections(prop, "comment")
                                    .into_iter()
                                    .filter_map(|comment| {
                                        let username = comment.read_string("username", "");
                                        let time = read_int64(comment, "time");
                                        if username.is_empty() || time == 0 {
                                            return None;
                                        }
                                        Some(MetaComment {
                                            username,
                                            time,
                                            content: comment.read_string("content", ""),
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    ),
                };
                (desc.clone(), value)
            })
            .collect();
        Self { props }
    }

    /// `MetaData::save` (`meta_data.cpp:243`): open (creating) the model's
    /// `metaData` child, then per property delete its sections and write
    /// the value back — unless the value is the kind's default, in which
    /// case the property vanishes. Children the description does not know
    /// (a `copyright` line an old file carries) are left alone.
    pub fn save(&self, model: &mut DataSection) -> bool {
        let Some(meta) = model.open_or_make_section(METADATA_SECTION_NAME) else {
            return false;
        };
        for (desc, value) in &self.props {
            meta.delete_sections(&desc.name);
            if value.is_default() {
                continue;
            }
            match value {
                MetaValue::Text(text) => {
                    if let Some(prop) = meta.open_or_make_section(&desc.name) {
                        prop.set_string(text);
                    }
                }
                MetaValue::Int(value) => {
                    if let Some(prop) = meta.open_or_make_section(&desc.name) {
                        prop.set_int(*value);
                    }
                }
                MetaValue::Float(value) => {
                    if let Some(prop) = meta.open_or_make_section(&desc.name) {
                        prop.set_float(*value);
                    }
                }
                MetaValue::DateTime(value) => write_int64(meta, &desc.name, *value),
                MetaValue::Comments(comments) => {
                    let prop = meta.add_child(&desc.name);
                    for comment in comments {
                        let child = prop.add_child("comment");
                        child.write_string("username", &comment.username);
                        write_int64(child, "time", comment.time);
                        child.write_string("content", &comment.content);
                    }
                }
            }
        }
        true
    }

    /// `MetaData::operator[]`: the value of one property, by name.
    pub fn get(&self, name: &str) -> Option<&MetaValue> {
        self.props
            .iter()
            .find(|(desc, _)| desc.name == name)
            .map(|(_, value)| value)
    }

    /// `Property::set`: replace one property's value. Unknown names are
    /// refused (the reference hands back a null property).
    pub fn set(&mut self, name: &str, value: MetaValue) -> bool {
        let Some(at) = self.props.iter().position(|(desc, _)| desc.name == name) else {
            return false;
        };
        self.props[at].1 = value;
        true
    }

    /// `MetaData::updateCreationInfo` (`meta_data_helper.cpp:52`): stamp all
    /// four bookkeeping properties. Undo ops are the engine seam and are not
    /// added here.
    pub fn update_creation_info(&mut self, time: i64, username: &str) {
        if !self.has_props(&["created_on", "created_by", "modified_on", "modified_by"]) {
            return;
        }
        self.set("created_on", MetaValue::DateTime(time));
        self.set("created_by", MetaValue::Text(username.to_string()));
        self.set("modified_on", MetaValue::DateTime(time));
        self.set("modified_by", MetaValue::Text(username.to_string()));
    }

    /// `MetaData::updateModificationInfo` (`meta_data_helper.cpp:75`): stamp
    /// the modified-on/by pair when both exist. The reference's `s_updating`
    /// re-entrancy guard protects the undo round-trip, which this port does
    /// not run; the guard is therefore not carried.
    pub fn update_modification_info(&mut self, time: i64, username: &str) {
        if !self.has_props(&["modified_on", "modified_by"]) {
            return;
        }
        self.set("modified_on", MetaValue::DateTime(time));
        self.set("modified_by", MetaValue::Text(username.to_string()));
    }

    fn has_props(&self, names: &[&str]) -> bool {
        names
            .iter()
            .all(|name| self.props.iter().any(|(desc, _)| &desc.name == name))
    }
}

// ----------------------------------------------------------------------
// The save face (`Mutant::save`/`saveAs`).
// ----------------------------------------------------------------------

/// The save face of one opened model: which files are dirty, whether the
/// undo/redo save point has been crossed, and the save/save-as flows. This
/// is the headless `Mutant` core — the render-facing halves (supermodel,
/// materials) live in the BW-13-04-02 faces.
pub struct ModelMutator {
    doc: ModelDoc,
    /// The `dirty_` set: resource ids of sections to write, in path order
    /// (the reference's `std::set<DataSectionPtr>` walks pointer order,
    /// which no two runs share).
    dirty: BTreeSet<String>,
    /// `UndoRedo::needsSave` — crossed save point since the last save.
    needs_save: bool,
    /// `isReadOnly_`.
    read_only: bool,
    /// The `dataFiles_` map: section path → the file its writes land in.
    data_files: BTreeMap<String, String>,
}

impl ModelMutator {
    /// Take over an opened model. The `dataFiles_` bookkeeping seeds the
    /// visual and the model file (`regenAnimLists`), `isReadOnly_` is the
    /// caller's verdict.
    pub fn new(doc: ModelDoc, read_only: bool) -> Self {
        let mut data_files = BTreeMap::new();
        data_files.insert(doc.visual_name.clone(), doc.visual_name.clone());
        data_files.insert(doc.path.clone(), doc.path.clone());
        Self {
            doc,
            dirty: BTreeSet::new(),
            needs_save: false,
            read_only,
            data_files,
        }
    }

    /// The opened document.
    pub fn doc(&self) -> &ModelDoc {
        &self.doc
    }

    /// The opened document, mutably.
    pub fn doc_mut(&mut self) -> &mut ModelDoc {
        &mut self.doc
    }

    /// Take the document back (a saved-as document replaces it).
    pub fn into_doc(self) -> ModelDoc {
        self.doc
    }

    /// `Mutant::dirty(DataSectionPtr)`: mark a section for the next save.
    pub fn mark_dirty(&mut self, path: &str) {
        self.dirty.insert(path.to_string());
    }

    /// The `UndoRedo::add`/`barrier` seam: an operation has crossed the
    /// save point. Without this the save gate stays shut even with dirty
    /// sections — exactly the reference's `needsSave` behaviour.
    pub fn note_operation(&mut self) {
        self.needs_save = true;
    }

    /// `Mutant::dirty()` (`mutant.cpp:1716`): a read-only model is never
    /// dirty; otherwise the save point has been crossed.
    pub fn dirty(&self) -> bool {
        !self.read_only && self.needs_save
    }

    /// `Mutant::forceClean`: cross the save point back without writing.
    pub fn force_clean(&mut self) {
        self.needs_save = false;
    }

    /// `Mutant::save` (`mutant.cpp:1732`): write every dirty section except
    /// the model itself, refresh the metadata onto the model, then save the
    /// model — each write through `saveIfExists`, falling back to a plain
    /// `save` when no root holds the file. Clean (or read-only) models are
    /// not written, and a save crosses the save point back.
    ///
    /// The `cleanMaterials`/`cleanTints`/`cleanMaterialNames` calls that
    /// run first in the reference are the BW-13-04-02 material faces; their
    /// dirty-marking lands in the same set this method writes.
    pub fn save(&mut self, resource: &BwResource) -> bool {
        if !self.dirty() {
            return false;
        }

        for path in self.dirty.clone() {
            if path == self.doc.path {
                continue;
            }
            let file = self
                .data_files
                .get(&path)
                .cloned()
                .unwrap_or_else(|| path.clone());
            let section = if path == self.doc.visual_name {
                Some(self.doc.visual.clone())
            } else {
                // A section this mutator does not hold: its current on-disk
                // state is what gets rewritten. Faces that hold more
                // sections extend this arm.
                resource
                    .file_system()
                    .read_file(&path)
                    .and_then(|raw| datasection::parse(bwresource::get_filename(&path), &raw).ok())
            };
            let Some(mut section) = section else {
                continue;
            };
            if !resource.save_section_if_exists(&file, &mut section) {
                resource.save_section(&file, &mut section);
            }
            if path == self.doc.visual_name {
                self.doc.visual = section;
            }
        }

        self.doc.metadata.save(&mut self.doc.model);
        if !resource.save_section_if_exists(&self.doc.path, &mut self.doc.model) {
            resource.save_section(&self.doc.path, &mut self.doc.model);
        }

        self.dirty.clear();
        self.needs_save = false;
        // The reference notifies WorldEditor over a mailslot here; the
        // message face is a Win32 seam.
        true
    }

    /// `Mutant::saveAs` (`mutant.cpp:1812`): determine the visual type,
    /// rewrite the visual reference to the new root, save the model under
    /// `new_name`, save the visual beside it, byte-copy the primitives
    /// container when the visual names one, and re-open the renamed model —
    /// the reference ends in `loadModel(newName)`, so a failure to re-read
    /// the saved file fails the save-as.
    ///
    /// A model without a visual string cannot be saved-as, with the same
    /// "Cannot determine the visual type" error the caller prints.
    pub fn save_as(&mut self, resource: &BwResource, new_name: &str) -> Result<(), ModelError> {
        let Some(visual_type) = detect_editor_visual_type(&self.doc.model) else {
            return Err(ModelError::CannotDetermineVisualType {
                model: self.doc.path.clone(),
                what: "save".to_string(),
            });
        };
        let old_root = self.doc.model.read_string(visual_type.tag(), "");
        let new_root = match new_name.rfind('.') {
            Some(at) => &new_name[..at],
            None => new_name,
        };

        self.doc.model.write_string(visual_type.tag(), new_root);
        self.doc.metadata.save(&mut self.doc.model);
        resource.save_section(new_name, &mut self.doc.model);
        let new_visual_name = format!("{new_root}.visual");
        resource.save_section(&new_visual_name, &mut self.doc.visual);

        // The primitives container rides along when the visual names one.
        if !self.doc.visual.read_string("primitivesName", "").is_empty() {
            let old_primitives = format!("{old_root}.primitives");
            let new_primitives = format!("{new_root}.primitives");
            if let Some(bytes) = resource.file_system().read_file(&old_primitives) {
                resource.file_system().write_file(&new_primitives, &bytes);
            }
        }

        self.doc = ModelDoc::open(resource, new_name, "load")?;
        // A fresh load: the saved state is the save point, and the file set
        // is the renamed pair.
        self.dirty.clear();
        self.needs_save = false;
        self.data_files.clear();
        self.data_files
            .insert(self.doc.visual_name.clone(), self.doc.visual_name.clone());
        self.data_files
            .insert(self.doc.path.clone(), self.doc.path.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use resources::bwresource::BwResource;

    /// A temp-dir resource root with fixtures, the `resource_checker` shape.
    struct Fixture {
        dir: std::path::PathBuf,
        resource: BwResource,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("bw_model_editor_{tag}_{}", std::process::id()));
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

        fn remove(&self, name: &str) {
            std::fs::remove_file(self.dir.join(name)).unwrap();
        }

        fn section(&self, name: &str) -> Option<DataSection> {
            let raw = std::fs::read(self.dir.join(name)).ok()?;
            datasection::parse(bwresource::get_filename(name), &raw).ok()
        }
    }

    /// A chicken-shaped model carrying every section kind the format has,
    /// modelled on the reference sample (`lib/model/unit_test/res/.../
    /// chicken.model`).
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
        let mut prims = BinSection::new("m.primitives", BinaryBlock::default());
        let mut blob = Vec::new();
        VertexHeader {
            vertex_format: format.to_string(),
            n_vertices: 3,
        }
        .write(&mut blob);
        blob.extend_from_slice(&[0u8; 36]);
        prims
            .new_section("geometry0vertices")
            .set_binary(BinaryBlock::new(blob));
        prims.save_bytes()
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

    // ------------------------------------------------------------------
    // Visual type detection.
    // ------------------------------------------------------------------

    #[test]
    fn the_visual_type_cascade_follows_the_editor_order() {
        let _fixture = Fixture::new("cascade");
        let mut model = DataSection::new("m.model");
        model.write_string("nodefullVisual", "characters/npc/chicken/chicken");
        assert_eq!(
            detect_editor_visual_type(&model),
            Some(VisualType::Nodefull)
        );
        assert_eq!(VisualType::Nodefull.tag(), "nodefullVisual");

        let mut model = DataSection::new("m.model");
        model.write_string("nodelessVisual", "helpers/models/unit_cube");
        assert_eq!(
            detect_editor_visual_type(&model),
            Some(VisualType::Nodeless)
        );
        assert_eq!(VisualType::Nodeless.tag(), "nodelessVisual");

        let mut model = DataSection::new("m.model");
        model.write_string("billboardVisual", "helpers/billboards/tree");
        assert_eq!(
            detect_editor_visual_type(&model),
            Some(VisualType::Billboard)
        );
        assert_eq!(VisualType::Billboard.tag(), "billboardVisual");

        // nodefull wins over nodeless when both carry values, matching the
        // first-tag-wins order.
        let mut model = DataSection::new("m.model");
        model.write_string("nodelessVisual", "helpers/models/unit_cube");
        model.write_string("nodefullVisual", "characters/npc/chicken/chicken");
        assert_eq!(
            detect_editor_visual_type(&model),
            Some(VisualType::Nodefull)
        );
    }

    #[test]
    fn an_empty_visual_tag_cannot_be_judged() {
        // The runtime accepts a bare tag (`createModelFromFile` checks tag
        // presence); the editor reads strings, so an empty value refuses.
        let mut model = DataSection::new("m.model");
        model.write_string("nodefullVisual", "");
        assert_eq!(detect_editor_visual_type(&model), None);

        let model = DataSection::new("m.model");
        assert_eq!(detect_editor_visual_type(&model), None);
    }

    // ------------------------------------------------------------------
    // Open (ensureModelValid).
    // ------------------------------------------------------------------

    #[test]
    fn open_resolves_the_visual_and_primitives_names() {
        let fixture = chicken_fixture("resolve");
        let doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        assert_eq!(doc.visual_type, VisualType::Nodefull);
        assert_eq!(doc.visual_name, "characters/npc/chicken/chicken.visual");
        // No `primitivesName` override: the visual root names the container.
        assert_eq!(
            doc.primitives_name,
            "characters/npc/chicken/chicken.primitives"
        );
        assert_eq!(
            doc.metadata.get("modified_by"),
            Some(&MetaValue::Text("adamm".into()))
        );
    }

    #[test]
    fn the_primitives_name_override_redirects_the_container() {
        let fixture = chicken_fixture("primname");
        let mut visual = chicken_visual();
        visual = visual.replace(
            "<renderSet>",
            "<primitivesName>characters/npc/chicken/shared</primitivesName><renderSet>",
        );
        fixture.write("characters/npc/chicken/chicken.visual", visual);
        fixture.write(
            "characters/npc/chicken/shared.primitives",
            primitives_bytes("xyznrvt2"),
        );
        let doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        assert_eq!(
            doc.primitives_name,
            "characters/npc/chicken/shared.primitives"
        );
    }

    #[test]
    fn a_missing_visual_file_fails_the_open() {
        let fixture = chicken_fixture("novisual");
        fixture.remove("characters/npc/chicken/chicken.visual");
        // The locate dialog is the seam; headless the open fails silently,
        // exactly the reference's `return false`.
        assert!(matches!(
            ModelDoc::open(
                &fixture.resource,
                "characters/npc/chicken/chicken.model",
                "load"
            ),
            Err(ModelError::Invalid)
        ));
    }

    #[test]
    fn a_missing_primitives_file_is_reported_verbatim() {
        let fixture = chicken_fixture("noprim");
        fixture.remove("characters/npc/chicken/chicken.primitives");
        let Err(err) = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        ) else {
            panic!("the open should fail");
        };
        assert_eq!(
            err.to_string(),
            "Unable to find the primitives file for \"characters/npc/chicken/chicken.model\".\n\n\
             Make sure that \"characters/npc/chicken/chicken.primitives\" exists.\n\n\
             Unable to load model.\n"
        );
    }

    #[test]
    fn a_missing_model_file_fails_the_open() {
        let fixture = chicken_fixture("nomodel");
        assert!(matches!(
            ModelDoc::open(
                &fixture.resource,
                "characters/npc/chicken/missing.model",
                "load"
            ),
            Err(ModelError::Invalid)
        ));
    }

    #[test]
    fn a_visualless_model_cannot_determine_its_type() {
        let fixture = chicken_fixture("novtype");
        let mut model = chicken_model();
        model = model.replace(
            "<nodefullVisual>characters/npc/chicken/chicken</nodefullVisual>",
            "",
        );
        fixture.write("characters/npc/chicken/chicken.model", model);
        let Err(err) = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        ) else {
            panic!("the open should fail");
        };
        assert_eq!(
            err,
            ModelError::CannotDetermineVisualType {
                model: "characters/npc/chicken/chicken.model".into(),
                what: "load".into(),
            }
        );
        assert_eq!(
            err.to_string(),
            "Cannot determine the visual type of the model\"characters/npc/chicken/chicken.model\".\n\
             Unable to load model."
        );
    }

    #[test]
    fn the_parent_chain_must_validate_recursively() {
        let fixture = Fixture::new("parentchain");
        // base: a full valid model.
        let base_model = String::from(
            "<base.model>\
             <nodefullVisual>sets/base/shape</nodefullVisual>\
             <animation><name>base_idle</name><frameRate>30</frameRate>\
             <nodes>sets/base/animation/idle</nodes></animation>\
             </base.model>",
        );
        fixture.write("sets/base/base.model", base_model);
        fixture.write("sets/base/shape.visual", chicken_visual());
        fixture.write("sets/base/shape.primitives", primitives_bytes("xyznrvt2"));
        // child: names base as parent.
        let child_model = String::from(
            "<child.model>\
             <nodefullVisual>sets/child/shape</nodefullVisual>\
             <parent>sets/base/base</parent>\
             </child.model>",
        );
        fixture.write("sets/child/child.model", child_model);
        fixture.write("sets/child/shape.visual", chicken_visual());
        fixture.write("sets/child/shape.primitives", primitives_bytes("xyznrvt2"));

        let doc = ModelDoc::open(&fixture.resource, "sets/child/child.model", "load").unwrap();
        assert_eq!(doc.path, "sets/child/child.model");

        // Breaking a file *inside* the parent chain fails the child open.
        fixture.remove("sets/base/shape.primitives");
        let Err(err) = ModelDoc::open(&fixture.resource, "sets/child/child.model", "load") else {
            panic!("the child open should fail through the parent chain");
        };
        assert_eq!(
            err,
            ModelError::MissingPrimitives {
                model: "sets/base/base.model".into(),
                primitives: "sets/base/shape.primitives".into(),
                what: "load".into(),
            }
        );
    }

    #[test]
    fn a_missing_parent_model_fails_the_open() {
        let fixture = chicken_fixture("orphan");
        let mut model = chicken_model();
        model = model.replace(
            "<batched>true</batched>",
            "<parent>characters/npc/gone</parent>",
        );
        fixture.write("characters/npc/chicken/chicken.model", model);
        assert!(matches!(
            ModelDoc::open(
                &fixture.resource,
                "characters/npc/chicken/chicken.model",
                "load"
            ),
            Err(ModelError::Invalid)
        ));
    }

    #[test]
    fn retired_vertex_formats_are_refused_verbatim() {
        let fixture = chicken_fixture("deprecated");
        fixture.write(
            "characters/npc/chicken/chicken.primitives",
            primitives_bytes("xyznuvi"),
        );
        let Err(err) = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        ) else {
            panic!("the open should fail");
        };
        assert_eq!(
            err,
            ModelError::DeprecatedFormat {
                model: "characters/npc/chicken/chicken.model".into(),
            }
        );
        assert_eq!(
            err.to_string(),
            "The model: \"characters/npc/chicken/chicken.model\" is using a depreciated \
             vertex format.\nPlease re-export this model and try again.\n"
        );
    }

    #[test]
    fn old_style_vertices_paths_split_before_the_lookup() {
        let fixture = chicken_fixture("oldstyle");
        let mut visual = chicken_visual();
        visual = visual.replace(
            "<vertices>geometry0vertices</vertices>",
            "<vertices>characters/old/chicken.geometry0vertices</vertices>",
        );
        fixture.write("characters/npc/chicken/chicken.visual", visual);
        fixture.write(
            "characters/npc/chicken/chicken.primitives",
            primitives_bytes("xyznuvitb"),
        );
        // The `file/part` string splits to `geometry0vertices`, which is
        // found in this model's own container and judged deprecated.
        assert!(matches!(
            ModelDoc::open(
                &fixture.resource,
                "characters/npc/chicken/chicken.model",
                "load"
            ),
            Err(ModelError::DeprecatedFormat { .. })
        ));
    }

    #[test]
    fn processed_containers_are_preferred_over_bare_ones() {
        let fixture = chicken_fixture("processed");
        // The bare container is retired; the .processed one is modern.
        fixture.write(
            "characters/npc/chicken/chicken.primitives",
            primitives_bytes("xyznuvi"),
        );
        fixture.write(
            "characters/npc/chicken/chicken.primitives.processed",
            primitives_bytes("xyznrvt2"),
        );
        let doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        assert_eq!(
            doc.primitives_name,
            "characters/npc/chicken/chicken.primitives"
        );
    }

    #[test]
    fn a_missing_primitives_container_is_not_automatically_deprecated() {
        // `PrimitiveFile::get` "always succeeds" into an empty container,
        // which cannot carry a retired format — but ensureModelValid has
        // already refused the open for the missing file, so this arm is
        // only reachable through the direct helper.
        let fixture = chicken_fixture("emptyprim");
        let visual = fixture
            .section("characters/npc/chicken/chicken.visual")
            .unwrap();
        assert!(!is_format_deprecated(
            &fixture.resource,
            &visual,
            "characters/npc/chicken/nowhere"
        ));
    }

    // ------------------------------------------------------------------
    // The read contract (`lib/model`).
    // ------------------------------------------------------------------

    #[test]
    fn extent_defaults_to_lod_max_and_reads_explicit_values() {
        let fixture = chicken_fixture("extent");
        let model = fixture
            .section("characters/npc/chicken/chicken.model")
            .unwrap();
        assert_eq!(extent(&model), 44.0);

        let mut bare = DataSection::new("bare.model");
        assert_eq!(extent(&bare), LOD_MAX);
        bare.write_float("extent", -1.0);
        assert_eq!(extent(&bare), LOD_HIDDEN);
    }

    #[test]
    fn parent_resource_ids_gain_the_model_extension() {
        let fixture = chicken_fixture("parentid");
        let model = fixture
            .section("characters/npc/chicken/chicken.model")
            .unwrap();
        assert_eq!(parent_resource_id(&model), None);

        let mut with_parent = DataSection::new("child.model");
        with_parent.write_string("parent", "sets/base/base");
        assert_eq!(
            parent_resource_id(&with_parent),
            Some("sets/base/base.model".to_string())
        );
    }

    #[test]
    fn anim_count_walks_the_parent_chain() {
        let fixture = Fixture::new("animcount");
        let base_model = String::from(
            "<base.model>\
             <nodefullVisual>sets/base/shape</nodefullVisual>\
             <animation><name>base_idle</name><frameRate>30</frameRate>\
             <nodes>sets/base/animation/idle</nodes></animation>\
             <animation><name>base_walk</name><frameRate>30</frameRate>\
             <nodes>sets/base/animation/walk</nodes></animation>\
             </base.model>",
        );
        fixture.write("sets/base/base.model", base_model);
        fixture.write("sets/base/shape.visual", chicken_visual());
        fixture.write("sets/base/shape.primitives", primitives_bytes("xyznrvt2"));
        let child_model = String::from(
            "<child.model>\
             <nodefullVisual>sets/child/shape</nodefullVisual>\
             <parent>sets/base/base</parent>\
             <animation><name>child_attack</name><frameRate>24</frameRate>\
             <nodes>sets/child/animation/attack</nodes></animation>\
             </child.model>",
        );
        fixture.write("sets/child/child.model", child_model);
        fixture.write("sets/child/shape.visual", chicken_visual());
        fixture.write("sets/child/shape.primitives", primitives_bytes("xyznrvt2"));

        assert_eq!(anim_count(&fixture.resource, "sets/child/child.model"), 3);
        assert_eq!(anim_count(&fixture.resource, "sets/base/base.model"), 2);
        // A missing file stops the walk with nothing counted.
        assert_eq!(anim_count(&fixture.resource, "sets/gone/gone.model"), 0);
    }

    #[test]
    fn nodefull_and_visibility_box_queries() {
        let fixture = chicken_fixture("queries");
        let path = "characters/npc/chicken/chicken.model";
        assert!(nodefull(&fixture.resource, path));
        assert!(has_visibility_box(&fixture.resource, path));

        // The nodeless unit cube shape: no nodefullVisual, no box min.
        fixture.write(
            "helpers/models/unit_cube.model",
            "<unit_cube.model><nodelessVisual>helpers/models/unit_cube</nodelessVisual></unit_cube.model>",
        );
        assert!(!nodefull(
            &fixture.resource,
            "helpers/models/unit_cube.model"
        ));
        assert!(!has_visibility_box(
            &fixture.resource,
            "helpers/models/unit_cube.model"
        ));

        // An empty path answers false, like the reference guards.
        assert!(!nodefull(&fixture.resource, ""));
        assert!(!has_visibility_box(&fixture.resource, ""));
        assert!(!nodefull(&fixture.resource, "helpers/models/gone.model"));
        assert!(!has_visibility_box(
            &fixture.resource,
            "helpers/models/gone.model"
        ));
    }

    // ------------------------------------------------------------------
    // Metadata.
    // ------------------------------------------------------------------

    #[test]
    fn the_default_desc_carries_the_six_props_name_sorted() {
        let desc = MetaDataDesc::default_desc();
        let names: Vec<&str> = desc.props().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "comments",
                "created_by",
                "created_on",
                "description",
                "modified_by",
                "modified_on"
            ]
        );
        let created_by = desc
            .props()
            .iter()
            .find(|p| p.name == "created_by")
            .unwrap();
        assert!(created_by.read_only);
        assert_eq!(created_by.kind, MetaPropKind::Text);
        let created_on = desc
            .props()
            .iter()
            .find(|p| p.name == "created_on")
            .unwrap();
        assert_eq!(created_on.kind, MetaPropKind::DateTime);
        assert_eq!(created_on.description, "created on");
        let comments = desc.props().iter().find(|p| p.name == "comments").unwrap();
        assert_eq!(comments.kind, MetaPropKind::Comments);
        assert!(!comments.read_only);
    }

    #[test]
    fn a_config_file_extends_and_overrides_the_defaults() {
        let mut config = DataSection::new("model.xml");
        {
            let project = config.add_child("project_name");
            project.write_string("type", "STRING");
            project.write_string("description", "project");
            let modified_on = config.add_child("modified_on");
            modified_on.write_string("type", "DATETIME");
            // A property of unknown type is skipped.
            let weird = config.add_child("weird");
            weird.write_string("type", "TELEPATHY");
        }
        let desc = MetaDataDesc::from_config(Some(&config));
        let names: Vec<&str> = desc.props().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "comments",
                "created_by",
                "created_on",
                "description",
                "modified_by",
                "modified_on",
                "project_name"
            ]
        );
        let project = desc
            .props()
            .iter()
            .find(|p| p.name == "project_name")
            .unwrap();
        assert_eq!(project.description, "project");
        // The config entry replaced the default kind in place.
        let modified_on = desc
            .props()
            .iter()
            .find(|p| p.name == "modified_on")
            .unwrap();
        assert_eq!(modified_on.kind, MetaPropKind::DateTime);

        // No config: just the defaults.
        assert_eq!(
            MetaDataDesc::from_config(None),
            MetaDataDesc::default_desc()
        );
    }

    #[test]
    fn metadata_load_reads_known_props_and_leaves_unknown_children() {
        let fixture = chicken_fixture("mdload");
        let model = fixture
            .section("characters/npc/chicken/chicken.model")
            .unwrap();
        let metadata = MetaData::load(Some(&model), &model_metadata_desc(&fixture.resource));
        // modified_on is read as an int64 timestamp; modified_by as text.
        assert_eq!(
            metadata.get("modified_on"),
            Some(&MetaValue::DateTime(1254377014))
        );
        assert_eq!(
            metadata.get("modified_by"),
            Some(&MetaValue::Text("adamm".into()))
        );
        // The description the file never carries is at its default.
        assert_eq!(
            metadata.get("description"),
            Some(&MetaValue::Text("".into()))
        );
        // The `copyright` child is not in the description, so no property
        // holds it — `save` must not drop it either.
        assert!(metadata.get("copyright").is_none());

        // A missing model section loads every default.
        let empty = MetaData::load(None, &MetaDataDesc::default_desc());
        assert_eq!(empty.get("created_on"), Some(&MetaValue::DateTime(0)));
    }

    #[test]
    fn metadata_save_deletes_then_writes_non_defaults() {
        let fixture = chicken_fixture("mdsave");
        let mut metadata = MetaData::load(
            fixture
                .section("characters/npc/chicken/chicken.model")
                .as_ref(),
            &model_metadata_desc(&fixture.resource),
        );
        // The stamp fields get real values; description stays empty.
        metadata.update_modification_info(1700000000, "cuihairu");
        metadata.set("description", MetaValue::Text("a bird".into()));
        metadata.set(
            "comments",
            MetaValue::Comments(vec![MetaComment {
                username: "adamm".into(),
                time: 1254377014,
                content: "first".into(),
            }]),
        );

        let mut model = fixture
            .section("characters/npc/chicken/chicken.model")
            .unwrap();
        assert!(metadata.save(&mut model));

        let meta = model.open_section("metaData").unwrap();
        // Copyright survived: save never clears the section.
        assert!(meta.open_section("copyright").is_some());
        assert_eq!(meta.read_string("description", ""), "a bird");
        assert_eq!(read_int64(meta, "modified_on"), 1700000000);
        assert_eq!(meta.read_string("modified_by", ""), "cuihairu");
        let comments = meta.open_section("comments").unwrap();
        let kept: Vec<&DataSection> = open_sections(comments, "comment");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].read_string("username", ""), "adamm");
        assert_eq!(kept[0].read_string("content", ""), "first");
        assert_eq!(read_int64(kept[0], "time"), 1254377014);

        // Resetting everything back to defaults removes the written props
        // (delete-then-write-if-non-default), copyright still standing.
        metadata.update_modification_info(0, "");
        metadata.set("description", MetaValue::Text(String::new()));
        metadata.set("comments", MetaValue::Comments(Vec::new()));
        assert!(metadata.save(&mut model));
        let meta = model.open_section("metaData").unwrap();
        assert!(meta.open_section("description").is_none());
        assert!(meta.open_section("modified_on").is_none());
        assert!(meta.open_section("comments").is_none());
        assert!(meta.open_section("copyright").is_some());
    }

    #[test]
    fn comment_loading_filters_nameless_and_untimed_entries() {
        // The load face drops every comment without a username or time.
        let mut model = DataSection::new("m.model");
        {
            let meta = model.open_or_make_section("metaData").unwrap();
            let comments = meta.add_child("comments");
            let good = comments.add_child("comment");
            good.write_string("username", "adamm");
            write_int64(good, "time", 1254377014);
            good.write_string("content", "first");
            let nameless = comments.add_child("comment");
            write_int64(nameless, "time", 1);
            let untimed = comments.add_child("comment");
            untimed.write_string("username", "adamm");
            // Comments that are not `comment` children are ignored.
            let note = comments.add_child("note");
            note.write_string("username", "adamm");
            write_int64(note, "time", 5);
        }
        let metadata = MetaData::load(Some(&model), &MetaDataDesc::default_desc());
        assert_eq!(
            metadata.get("comments"),
            Some(&MetaValue::Comments(vec![MetaComment {
                username: "adamm".into(),
                time: 1254377014,
                content: "first".into(),
            }]))
        );
    }

    #[test]
    fn metadata_set_refuses_unknown_names_and_creation_stamps_all_four() {
        let fixture = chicken_fixture("mdset");
        let mut metadata = MetaData::load(
            fixture
                .section("characters/npc/chicken/chicken.model")
                .as_ref(),
            &model_metadata_desc(&fixture.resource),
        );
        assert!(!metadata.set("copyright", MetaValue::Text("x".into())));
        assert!(metadata.set("description", MetaValue::Text("d".into())));

        metadata.update_creation_info(1000, "builder");
        assert_eq!(metadata.get("created_on"), Some(&MetaValue::DateTime(1000)));
        assert_eq!(
            metadata.get("created_by"),
            Some(&MetaValue::Text("builder".into()))
        );
        assert_eq!(
            metadata.get("modified_on"),
            Some(&MetaValue::DateTime(1000))
        );
        assert_eq!(
            metadata.get("modified_by"),
            Some(&MetaValue::Text("builder".into()))
        );
    }

    // ------------------------------------------------------------------
    // Save / save-as.
    // ------------------------------------------------------------------

    #[test]
    fn save_is_gated_on_dirty_and_resets_the_save_point() {
        let fixture = chicken_fixture("savegate");
        let doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        let mut mutator = ModelMutator::new(doc, false);
        // Clean: nothing written.
        assert!(!mutator.save(&fixture.resource));

        mutator.note_operation();
        assert!(mutator.dirty());
        assert!(mutator.save(&fixture.resource));
        // The save point is crossed back: a second save is a no-op.
        assert!(!mutator.dirty());
        assert!(!mutator.save(&fixture.resource));

        // forceClean crosses the point without writing.
        mutator.note_operation();
        mutator.force_clean();
        assert!(!mutator.dirty());
    }

    #[test]
    fn read_only_models_are_never_dirty() {
        let fixture = chicken_fixture("readonly");
        let doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        let mut mutator = ModelMutator::new(doc, true);
        mutator.note_operation();
        mutator.mark_dirty("characters/npc/chicken/chicken.visual");
        assert!(!mutator.dirty());
        assert!(!mutator.save(&fixture.resource));
    }

    #[test]
    fn save_writes_the_visual_in_place_and_the_model_with_fresh_metadata() {
        let fixture = chicken_fixture("saveflow");
        let doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        let mut mutator = ModelMutator::new(doc, false);

        // Mutate both held sections, stamp the metadata like the reference
        // does on modification, then save.
        let doc = mutator.doc_mut();
        doc.visual
            .write_string("primitivesName", "characters/npc/chicken/shared");
        doc.metadata
            .update_modification_info(1700000001, "cuihairu");
        mutator.note_operation();
        mutator.mark_dirty("characters/npc/chicken/chicken.visual");
        assert!(mutator.save(&fixture.resource));

        // The visual landed in place with the override.
        let visual = fixture
            .section("characters/npc/chicken/chicken.visual")
            .unwrap();
        assert_eq!(
            visual.read_string("primitivesName", ""),
            "characters/npc/chicken/shared"
        );

        // The model was written after the metadata refresh, so the saved
        // file carries the fresh stamps — and the copyright child that the
        // metadata description never knew about.
        let model = fixture
            .section("characters/npc/chicken/chicken.model")
            .unwrap();
        let meta = model.open_section("metaData").unwrap();
        assert_eq!(read_int64(meta, "modified_on"), 1700000001);
        assert_eq!(meta.read_string("modified_by", ""), "cuihairu");
        assert_eq!(
            meta.read_string("copyright", ""),
            "Copyright BigWorld Pty Ltd."
        );
    }

    #[test]
    fn save_falls_back_from_save_if_exists_to_a_fresh_save() {
        let fixture = chicken_fixture("fallback");
        let doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        let mut mutator = ModelMutator::new(doc, false);
        mutator.note_operation();
        mutator.mark_dirty("characters/npc/chicken/chicken.visual");
        // The file vanishes underneath the mutator: saveIfExists finds no
        // root holding it, the fallback save recreates it at the first
        // valid root.
        fixture.remove("characters/npc/chicken/chicken.visual");
        assert!(mutator.save(&fixture.resource));
        let visual = fixture
            .section("characters/npc/chicken/chicken.visual")
            .unwrap();
        assert!(visual.open_section("renderSet").is_some());
    }

    /// A visual that names its primitives container by override — the shape
    /// the exporter writes and the only shape whose save-as reloads.
    fn chicken_visual_with_override() -> String {
        chicken_visual().replace(
            "<renderSet>",
            "<primitivesName>characters/npc/chicken/shared</primitivesName><renderSet>",
        )
    }

    #[test]
    fn save_as_renames_the_pair_and_copies_the_primitives() {
        let fixture = chicken_fixture("saveas");
        fixture.write(
            "characters/npc/chicken/chicken.visual",
            chicken_visual_with_override(),
        );
        fixture.write(
            "characters/npc/chicken/shared.primitives",
            primitives_bytes("xyznrvt2"),
        );
        let doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        assert_eq!(
            doc.primitives_name,
            "characters/npc/chicken/shared.primitives"
        );
        let mut mutator = ModelMutator::new(doc, false);
        mutator
            .save_as(
                &fixture.resource,
                "characters/npc/chicken/chicken_new.model",
            )
            .unwrap();

        // The renamed model names the renamed visual.
        let model = fixture
            .section("characters/npc/chicken/chicken_new.model")
            .unwrap();
        assert_eq!(
            model.read_string("nodefullVisual", ""),
            "characters/npc/chicken/chicken_new"
        );
        // The visual was written under the new root, override intact — so
        // the reopened document resolves the shared container.
        let visual = fixture
            .section("characters/npc/chicken/chicken_new.visual")
            .unwrap();
        assert_eq!(
            visual.read_string("primitivesName", ""),
            "characters/npc/chicken/shared"
        );
        // The primitives container rode along byte-identically from the old
        // root to the new one.
        let old = std::fs::read(
            fixture
                .dir
                .join("characters/npc/chicken/chicken.primitives"),
        )
        .unwrap();
        let new = std::fs::read(
            fixture
                .dir
                .join("characters/npc/chicken/chicken_new.primitives"),
        )
        .unwrap();
        assert_eq!(old, new);
        // The originals are untouched.
        assert!(
            fixture
                .section("characters/npc/chicken/chicken.model")
                .is_some()
        );
        assert!(
            fixture
                .section("characters/npc/chicken/chicken.visual")
                .is_some()
        );

        // The mutator now edits the renamed pair.
        assert_eq!(
            mutator.doc().path,
            "characters/npc/chicken/chicken_new.model"
        );
        assert_eq!(
            mutator.doc().visual_name,
            "characters/npc/chicken/chicken_new.visual"
        );
        assert_eq!(
            mutator.doc().primitives_name,
            "characters/npc/chicken/shared.primitives"
        );
    }

    #[test]
    fn save_as_without_a_primitives_override_leaves_the_container_behind() {
        // The reference guard reads `primitivesName` off the visual; the
        // shipped chicken visual carries none, so nothing is copied and the
        // trailing loadModel fails on the renamed container — the renamed
        // files exist on disk while the save-as reports the primitives
        // error, and the mutator keeps the old document.
        let fixture = chicken_fixture("saveasnocopy");
        let doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        let mut mutator = ModelMutator::new(doc, false);
        let err = mutator
            .save_as(
                &fixture.resource,
                "characters/npc/chicken/chicken_new.model",
            )
            .unwrap_err();
        assert_eq!(
            err,
            ModelError::MissingPrimitives {
                model: "characters/npc/chicken/chicken_new.model".into(),
                primitives: "characters/npc/chicken/chicken_new.primitives".into(),
                what: "load".into(),
            }
        );
        // The renamed pair was written anyway.
        assert!(
            fixture
                .section("characters/npc/chicken/chicken_new.model")
                .is_some()
        );
        assert!(
            fixture
                .section("characters/npc/chicken/chicken_new.visual")
                .is_some()
        );
        assert!(
            !fixture
                .dir
                .join("characters/npc/chicken/chicken_new.primitives")
                .exists()
        );
        // The mutator still holds the original document, with the visual
        // reference rewritten the way the in-memory section was.
        assert_eq!(mutator.doc().path, "characters/npc/chicken/chicken.model");
        assert_eq!(
            mutator.doc().model.read_string("nodefullVisual", ""),
            "characters/npc/chicken/chicken_new"
        );
    }

    #[test]
    fn save_as_requires_a_visual_type() {
        let fixture = chicken_fixture("saveasbad");
        let mut doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        // Strip the visual string in memory: the state saveAs guards
        // against.
        doc.model.write_string("nodefullVisual", "");
        let mut mutator = ModelMutator::new(doc, false);
        let err = mutator
            .save_as(&fixture.resource, "characters/npc/chicken/renamed.model")
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Cannot determine the visual type of the model\"characters/npc/chicken/chicken.model\".\n\
             Unable to save model."
        );
        // Nothing was written.
        assert!(
            !fixture
                .dir
                .join("characters/npc/chicken/renamed.model")
                .exists()
        );
    }

    #[test]
    fn save_as_reopens_the_renamed_model_clean() {
        let fixture = chicken_fixture("saveasreload");
        fixture.write(
            "characters/npc/chicken/chicken.visual",
            chicken_visual_with_override(),
        );
        fixture.write(
            "characters/npc/chicken/shared.primitives",
            primitives_bytes("xyznrvt2"),
        );
        let doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        let mut mutator = ModelMutator::new(doc, false);
        mutator.note_operation();
        mutator
            .save_as(&fixture.resource, "characters/npc/chicken/moved.model")
            .unwrap();
        // The re-opened document is the saved state: clean, resolving the
        // override container.
        assert!(!mutator.dirty());
        assert_eq!(mutator.doc().path, "characters/npc/chicken/moved.model");
        assert_eq!(
            mutator.doc().primitives_name,
            "characters/npc/chicken/shared.primitives"
        );
    }

    // ------------------------------------------------------------------
    // Acceptance: a model saved by the new version reads back under the
    // original's read contract.
    // ------------------------------------------------------------------

    /// The original loader's contract, as a test-side checker: what
    /// `createModelFromFile` + `Model::load` + `loadAnimations` + the
    /// editor's own `ensureModelValid` need from a `.model` file.
    fn assert_original_read_contract(model: &DataSection, visual: &DataSection) {
        // createModelFromFile: the nodefullVisual tag must be present and
        // name a visual.
        let root = model.read_string("nodefullVisual", "");
        assert!(!root.is_empty(), "no nodefullVisual for the runtime");
        // Model::load: parent and extent read without exploding; extent
        // falls back to LOD_MAX.
        let _ = parent_resource_id(model);
        let _ = extent(model);
        // loadAnimations: every animation section carries name/frameRate/nodes.
        for anim in open_sections(model, "animation") {
            assert!(
                !anim.read_string("name", "").is_empty(),
                "animation has no name"
            );
            let frame_rate = anim.read_float("frameRate", 30.0);
            assert!(frame_rate > 0.0);
            assert!(
                !anim.read_string("nodes", "").is_empty(),
                "animation has no nodes"
            );
        }
        // ensureModelValid: the visual opens and the primitives container
        // resolves from it.
        let primitives = format!("{}.primitives", visual.read_string("primitivesName", &root));
        assert!(!primitives.is_empty());
        assert!(
            visual.open_section("renderSet").is_some(),
            "visual has no renderSet"
        );
    }

    #[test]
    fn a_saved_old_model_reads_back_under_the_original_contract() {
        let fixture = chicken_fixture("acceptance");

        // Open the original model, edit it the way the editor would.
        let doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        let mut mutator = ModelMutator::new(doc, false);
        {
            let doc = mutator.doc_mut();
            // A designer tweak: one more animation and an extent change.
            let anim = doc.model.add_child("animation");
            anim.write_string("name", "m_turnleft");
            anim.write_float("frameRate", 30.0);
            anim.write_string("nodes", "characters/npc/chicken/animation/m_turnleft");
            doc.model.write_float("extent", 50.0);
            doc.metadata
                .update_modification_info(1700000002, "cuihairu");
        }
        mutator.note_operation();
        mutator.mark_dirty("characters/npc/chicken/chicken.visual");
        mutator.mark_dirty("characters/npc/chicken/chicken.model");
        assert!(mutator.save(&fixture.resource));

        // Now read the saved file back the way the original would.
        let saved = fixture
            .section("characters/npc/chicken/chicken.model")
            .unwrap();
        let saved_visual = fixture
            .section("characters/npc/chicken/chicken.visual")
            .unwrap();
        assert_original_read_contract(&saved, &saved_visual);

        // The untouched sections survived the round trip value-identically.
        let untouched = chicken_model();
        let original_model = datasection::parse("chicken.model", untouched.as_bytes()).unwrap();
        for tag in [
            "batched",
            "visibilityBox/min",
            "visibilityBox/max",
            "materialNames",
        ] {
            assert_eq!(
                saved.read_string(tag, ""),
                original_model.read_string(tag, ""),
                "section {tag} changed"
            );
        }
        // The dye round-tripped with its tint material.
        let dye = saved.open_section("dye").unwrap();
        assert_eq!(dye.read_string("matter", ""), "body");
        assert_eq!(dye.read_string("replaces", ""), "skin");
        let tint = dye.open_section("tint").unwrap();
        assert_eq!(tint.read_string("name", ""), "red");
        assert_eq!(
            tint.read_string("material/fx", ""),
            "shaders/std_effects/pffx.fx"
        );
        // The action kept its match block.
        let action = saved.open_section("action").unwrap();
        assert_eq!(action.read_string("name", ""), "TurnLeft");
        assert_eq!(action.read_string("match/trigger/maxEntityAux1", ""), "-60");
        // The new animation landed; the edited extent and fresh stamps too.
        let names: Vec<String> = open_sections(&saved, "animation")
            .into_iter()
            .map(|anim| anim.read_string("name", ""))
            .collect();
        assert_eq!(names, ["m_walk", "m_idle", "m_turnleft"]);
        assert_eq!(extent(&saved), 50.0);
        let meta = saved.open_section("metaData").unwrap();
        assert_eq!(read_int64(meta, "modified_on"), 1700000002);
        assert_eq!(meta.read_string("modified_by", ""), "cuihairu");
        assert_eq!(
            meta.read_string("copyright", ""),
            "Copyright BigWorld Pty Ltd."
        );

        // And the port's own open judges the saved file as a valid model.
        let doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        assert_eq!(doc.visual_type, VisualType::Nodefull);
    }

    // ------------------------------------------------------------------
    // splitOldPrimitiveName corners.
    // ------------------------------------------------------------------

    #[test]
    fn split_old_primitive_name_covers_the_corners() {
        // file/part: the split lands on the dot before the slash side.
        let (file, part) = split_old_primitive_name("sets/old/chicken.geometry0vertices");
        assert_eq!(file, "sets/old/chicken");
        assert_eq!(part, "geometry0vertices");

        // .static stays with the part.
        let (file, part) = split_old_primitive_name("sets/old/house.static");
        assert_eq!(file, "sets/old/house");
        assert_eq!(part, "static");

        // no dot, no slash: the reference truncates the last character.
        let (file, part) = split_old_primitive_name("plainname");
        assert_eq!(file, "plainnam");
        assert_eq!(part, "");

        // a bare part name: no dot, nothing to cut.
        let (file, part) = split_old_primitive_name("geometry0vertices");
        assert_eq!(file, "geometry0vertice");
        assert_eq!(part, "");
    }

    #[test]
    fn the_static_exception_keeps_the_marker_with_the_file() {
        // `house.static.visual`: the `.static` between the dots is not taken
        // off the file side — `don't take off a .static`.
        let (file, part) = split_old_primitive_name("house.static.visual");
        assert_eq!(file, "house.static");
        assert_eq!(part, "visual");

        // One dot only: no marker to keep, and the part keeps the `.static`.
        let (file, part) = split_old_primitive_name("a.b.static");
        assert_eq!(file, "a");
        assert_eq!(part, "b.static");
    }

    #[test]
    fn unparseable_files_fail_the_open_as_invalid() {
        let fixture = chicken_fixture("garbage");

        // A `.model` whose content is not XML fails to parse.
        fixture.write("bad/bad.model", b"\x00\x01not a section");
        assert!(matches!(
            ModelDoc::open(&fixture.resource, "bad/bad.model", "load"),
            Err(ModelError::Invalid)
        ));

        // A model pointing at a visual that exists but does not parse.
        let mut model = String::new();
        model.push_str("<m.model><nodefullVisual>bad/badvis</nodefullVisual></m.model>");
        fixture.write("bad/vis.model", model);
        fixture.write("bad/badvis.visual", b"\xff\xfe");
        assert!(matches!(
            ModelDoc::open(&fixture.resource, "bad/vis.model", "load"),
            Err(ModelError::Invalid)
        ));

        // The error renders as nothing: the reference prints nothing for the
        // locate-file dialog path.
        assert_eq!(ModelError::Invalid.to_string(), "");

        // The empty-path query helpers refuse without touching the disk.
        assert!(!nodefull(&fixture.resource, ""));
        assert!(!has_visibility_box(&fixture.resource, ""));

        // A garbage model fails the visibility query the same way.
        fixture.write("bad/garb.model", b"\x00");
        assert!(!has_visibility_box(&fixture.resource, "bad/garb.model"));
        assert!(!nodefull(&fixture.resource, "bad/garb.model"));
    }

    #[test]
    fn a_cyclic_parent_chain_is_cut_by_the_depth_guard() {
        let fixture = Fixture::new("cycle");
        fixture.write(
            "sets/a.model",
            "<a.model><nodefullVisual>sets/v</nodefullVisual><parent>sets/b</parent></a.model>",
        );
        fixture.write(
            "sets/b.model",
            "<b.model><nodefullVisual>sets/v</nodefullVisual><parent>sets/a</parent></b.model>",
        );
        fixture.write("sets/v.visual", chicken_visual());
        fixture.write("sets/v.primitives", primitives_bytes("xyznrvt2"));
        // The reference overflows the stack on this input; the port's depth
        // guard fails the open instead.
        assert!(matches!(
            ModelDoc::open(&fixture.resource, "sets/a.model", "load"),
            Err(ModelError::Invalid)
        ));
    }

    #[test]
    fn an_unparseable_parent_stops_the_animation_walk() {
        let fixture = Fixture::new("animwalk");
        fixture.write(
            "sets/child.model",
            "<c.model><nodefullVisual>sets/v</nodefullVisual><parent>sets/base</parent>\
             <animation><name>a</name></animation></c.model>",
        );
        fixture.write("sets/base.model", b"\x00\x01");
        // The chain stops at the unparseable parent: only the child's own
        // animation is counted.
        assert_eq!(anim_count(&fixture.resource, "sets/child.model"), 1);
    }

    #[test]
    fn an_xml_container_is_never_a_primitives_container() {
        let fixture = chicken_fixture("xmlprim");
        // A `.processed` file that is XML is declined; the bare container
        // underneath is used.
        fixture.write(
            "characters/npc/chicken/chicken.primitives.processed",
            "<root><geometry0vertices>not binary</geometry0vertices></root>",
        );
        fixture.write(
            "characters/npc/chicken/chicken.primitives",
            primitives_bytes("xyznuvi"),
        );
        assert!(matches!(
            ModelDoc::open(
                &fixture.resource,
                "characters/npc/chicken/chicken.model",
                "load"
            ),
            Err(ModelError::DeprecatedFormat { .. })
        ));
    }

    #[test]
    fn an_unreadable_vertices_part_is_not_deprecated() {
        let fixture = chicken_fixture("shortblob");
        let mut prims = BinSection::new("m.primitives", BinaryBlock::default());
        prims
            .new_section("geometry0vertices")
            .set_binary(BinaryBlock::new(vec![0u8, 1, 2]));
        fixture.write(
            "characters/npc/chicken/chicken.primitives",
            prims.save_bytes(),
        );
        // Too short to be a vertex header: the part is skipped, and without
        // any readable header there is nothing to judge deprecated.
        assert!(
            ModelDoc::open(
                &fixture.resource,
                "characters/npc/chicken/chicken.model",
                "load"
            )
            .is_ok()
        );
    }

    #[test]
    fn numeric_and_comment_properties_round_trip() {
        // A config extends the defaults with INT, FLOAT and COMMENTS props.
        let mut config = DataSection::new("model.xml");
        config.add_child("priority").write_string("type", "INT");
        config.add_child("scale").write_string("type", "FLOAT");
        config.add_child("notes").write_string("type", "COMMENTS");
        let desc = MetaDataDesc::from_config(Some(&config));
        assert_eq!(
            desc.props()
                .iter()
                .find(|p| p.name == "priority")
                .unwrap()
                .kind,
            MetaPropKind::Int
        );
        assert_eq!(
            desc.props()
                .iter()
                .find(|p| p.name == "scale")
                .unwrap()
                .kind,
            MetaPropKind::Float
        );
        assert_eq!(
            desc.props()
                .iter()
                .find(|p| p.name == "notes")
                .unwrap()
                .kind,
            MetaPropKind::Comments
        );

        // A model carrying values for each.
        let mut model = DataSection::new("m.model");
        let meta = model.add_child(METADATA_SECTION_NAME);
        meta.write_int("priority", 3);
        meta.write_float("scale", 1.5);
        let notes = meta.add_child("notes");
        let comment = notes.add_child("comment");
        comment.write_string("username", "cuihairu");
        comment.write_string("time", "1700000001");
        comment.write_string("content", "check the wing");
        let loaded = MetaData::load(Some(&model), &desc);
        assert_eq!(loaded.get("priority"), Some(&MetaValue::Int(3)));
        assert_eq!(loaded.get("scale"), Some(&MetaValue::Float(1.5)));
        assert_eq!(
            loaded.get("notes"),
            Some(&MetaValue::Comments(vec![MetaComment {
                username: "cuihairu".to_string(),
                time: 1700000001,
                content: "check the wing".to_string(),
            }]))
        );
        // Zero is each numeric kind's default.
        assert!(MetaValue::Int(0).is_default());
        assert!(MetaValue::Float(0.0).is_default());
        assert!(!MetaValue::Int(3).is_default());
        assert!(!MetaValue::Float(1.5).is_default());

        // Edited values save back; a reset numeric vanishes as a default.
        let mut edited = loaded;
        edited.set("priority", MetaValue::Int(7));
        edited.set("scale", MetaValue::Float(2.5));
        let mut out = DataSection::new("m.model");
        edited.save(&mut out);
        let saved = out.open_section(METADATA_SECTION_NAME).unwrap();
        assert_eq!(saved.read_int("priority", 0), 7);
        assert_eq!(saved.read_float("scale", 0.0), 2.5);
        // The untouched comments child survived the delete-then-write cycle.
        assert_eq!(
            saved
                .open_section("notes")
                .and_then(|n| n.open_section("comment"))
                .map(|c| c.read_string("content", ""))
                .unwrap(),
            "check the wing"
        );
    }

    #[test]
    fn a_garbage_metadata_config_falls_back_to_the_defaults() {
        let fixture = Fixture::new("badmeta");
        fixture.write("helpers/meta_data/model.xml", b"\x00\x01");
        let desc = model_metadata_desc(&fixture.resource);
        assert_eq!(desc.props().len(), 6);
        assert_eq!(desc.props()[0].name, "comments");
    }

    #[test]
    fn the_stamps_do_nothing_without_their_properties() {
        // A metadata block whose description carries none of the stamp
        // properties: both helpers return before touching anything.
        let mut meta = MetaData { props: vec![] };
        meta.update_creation_info(1700000001, "cuihairu");
        meta.update_modification_info(1700000002, "cuihairu");
        assert!(meta.get("created_on").is_none());
        assert!(meta.get("modified_by").is_none());
    }

    #[test]
    fn into_doc_hands_back_the_open_document() {
        let fixture = chicken_fixture("intodoc");
        let doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        let path = doc.path.clone();
        let mutator = ModelMutator::new(doc, false);
        assert_eq!(mutator.into_doc().path, path);
    }

    #[test]
    fn save_rewrites_unheld_dirty_files_and_skips_unreadable_ones() {
        let fixture = chicken_fixture("extrasave");
        let doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        let mut mutator = ModelMutator::new(doc, false);

        // A dirty file the mutator does not hold is rewritten from its
        // on-disk state.
        fixture.write(
            "characters/npc/chicken/extra.model",
            "<e.model><nodefullVisual>characters/npc/chicken/chicken\
             </nodefullVisual></e.model>",
        );
        mutator.mark_dirty("characters/npc/chicken/extra.model");

        // A dirty path with no file at all is skipped, not a failure.
        mutator.mark_dirty("characters/npc/chicken/ghost.model");

        // A missing model file falls back from save-if-exists to a fresh
        // save for the document itself.
        fixture.remove("characters/npc/chicken/chicken.model");

        mutator.note_operation();
        assert!(mutator.save(&fixture.resource));
        assert!(
            fixture
                .section("characters/npc/chicken/extra.model")
                .is_some()
        );
        assert!(
            fixture
                .section("characters/npc/chicken/chicken.model")
                .is_some()
        );
        assert!(
            !fixture
                .dir
                .join("characters/npc/chicken/ghost.model")
                .exists()
        );
    }

    #[test]
    fn save_as_accepts_a_name_without_an_extension() {
        let fixture = Fixture::new("saveasext");
        fixture.write("characters/npc/chicken/chicken.model", chicken_model());
        fixture.write(
            "characters/npc/chicken/chicken.visual",
            chicken_visual_with_override(),
        );
        fixture.write(
            "characters/npc/chicken/shared.primitives",
            primitives_bytes("xyznrvt2"),
        );
        let doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        let mut mutator = ModelMutator::new(doc, false);
        // No dot in the new name: the whole string is the new root.
        mutator
            .save_as(&fixture.resource, "characters/npc/chicken2")
            .unwrap();
        assert_eq!(mutator.doc().path, "characters/npc/chicken2");
    }

    #[test]
    fn save_as_leaves_no_copy_when_the_container_is_gone() {
        let fixture = Fixture::new("saveasgone");
        fixture.write("characters/npc/chicken/chicken.model", chicken_model());
        fixture.write(
            "characters/npc/chicken/chicken.visual",
            chicken_visual_with_override(),
        );
        // No container at the old root at all: the override names the
        // shared one, so the open validates that instead.
        fixture.write(
            "characters/npc/chicken/shared.primitives",
            primitives_bytes("xyznrvt2"),
        );
        let doc = ModelDoc::open(
            &fixture.resource,
            "characters/npc/chicken/chicken.model",
            "load",
        )
        .unwrap();
        let mut mutator = ModelMutator::new(doc, false);
        // The copy step finds nothing at the old root to carry over, and the
        // shared container vanishing since the open fails the reopen.
        fixture.remove("characters/npc/chicken/shared.primitives");
        let Err(err) = mutator.save_as(&fixture.resource, "characters/npc/chicken/chicken2.model")
        else {
            panic!("the save-as should fail at the reopen");
        };
        assert_eq!(
            err,
            ModelError::MissingPrimitives {
                model: "characters/npc/chicken/chicken2.model".into(),
                primitives: "characters/npc/chicken/shared.primitives".into(),
                what: "load".into(),
            }
        );
    }
}
