//! `data_type.hpp` and `data_types/*`: the definition type system.
//!
//! A [`DataType`] is a parsed `<Type>` section: a meta-type name (`INT32`,
//! `ARRAY`, `FIXED_DICT`, ...) plus the type-specific payload. The reference
//! keeps one C++ subclass per meta type; here the payload lives in [`Kind`]
//! and the shared attributes (`isConst`, stream size, digest) are methods on
//! [`DataType`].
//!
//! Everything that ends up in the definition digest (`addToMD5`), the
//! bandwidth table (`streamSize`) or the reported type string (`typeName`)
//! is pinned against the reference sources, because process_defs output and
//! the client handshake compare those bytes.

use std::collections::HashMap;
use std::sync::Arc;

use resources::datasection::DataSection;

use crate::diagnostics::Diagnostics;
use crate::digest::Md5;

/// `DEFAULT_DATABASE_LENGTH` (`data_description.hpp`).
pub const DEFAULT_DATABASE_LENGTH: i32 = 65535;

/// `persistAsBlob` column length (`SequenceMetaDataType::getType`).
const PERSIST_AS_BLOB_DB_LEN: i32 = 16_777_215;

/// Stream sizes the reference takes from C++ `sizeof` expressions.
/// `sizeof( EntityID ) + sizeof( Mercury::Address )` (`EntityMailBoxRef`).
pub const MAILBOX_STREAM_SIZE: i32 = 4 + 8;
/// `sizeof( Vector2/3/4 )` (float members).
pub const VECTOR2_STREAM_SIZE: i32 = 8;
pub const VECTOR3_STREAM_SIZE: i32 = 12;
pub const VECTOR4_STREAM_SIZE: i32 = 16;
/// `sizeof( UniqueID )` (`UDORefDataType::streamSize`).
pub const UDO_REF_STREAM_SIZE: i32 = 16;

/// Client-safety bits (`DataType::ClientSafety`).
pub const CLIENT_SAFE: u32 = 0;
pub const CLIENT_UNSAFE: u32 = 0x1;
pub const CLIENT_UNUSABLE: u32 = 0x2;

/// One `<Properties>` field of a `FIXED_DICT` / `CLASS`
/// (`ClassDataType::Field`).
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    /// `<DatabaseLength>` (`ClassMetaDataType::buildType`).
    pub db_len: i32,
    /// `<Persistent>` (default true).
    pub is_persistent: bool,
    pub type_: Arc<DataType>,
}

/// The type-specific payload of a `<Type>` section.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// `IntegerDataType<INT_TYPE>` / `LongIntegerDataType<INT_TYPE>`:
    /// `bytes` is `sizeof(INT_TYPE)`.
    Int { bytes: i32, signed: bool },
    /// `FloatDataType<float>`.
    Float32,
    /// `FloatDataType<double>`.
    Float64,
    /// `StringDataType`.
    String,
    /// `UnicodeStringDataType`.
    UnicodeString,
    /// `BlobDataType` (a `StringDataType` subtype).
    Blob,
    /// `VectorDataType<VECTOR>`; `elements` is `NUM_ELEMENTS`.
    Vector { elements: i32 },
    /// `UDORefDataType`.
    UdoRef,
    /// `MailBoxDataType` (MF_SERVER only in the reference).
    MailBox,
    /// `PythonDataType` (SCRIPT_PYTHON only in the reference).
    Python,
    /// `ArrayDataType` / `TupleDataType` (`SequenceDataType`). `size == 0`
    /// means variable length; `db_len` is the `persistAsBlob` column size.
    Sequence {
        is_tuple: bool,
        size: i32,
        db_len: i32,
        element: Arc<DataType>,
    },
    /// `FixedDictDataType`. Empty `module` means no `<implementedBy>`.
    FixedDict {
        allow_none: bool,
        custom_type_name: String,
        module: String,
        instance: String,
        fields: Vec<Field>,
    },
    /// `ClassDataType`.
    Class {
        allow_none: bool,
        fields: Vec<Field>,
    },
    /// `UserDataType`.
    UserType { module: String, instance: String },
}

impl Kind {
    /// The meta-type name of this payload (`pMetaDataType_->name()`).
    pub fn meta_name(&self) -> &'static str {
        match self {
            Self::Int { bytes, signed } => match (*signed, *bytes) {
                (true, 1) => "INT8",
                (true, 2) => "INT16",
                (true, 4) => "INT32",
                (true, 8) => "INT64",
                (false, 1) => "UINT8",
                (false, 2) => "UINT16",
                (false, 4) => "UINT32",
                (false, 8) => "UINT64",
                _ => unreachable!("int byte width"),
            },
            Self::Float32 => "FLOAT32",
            Self::Float64 => "FLOAT64",
            Self::String => "STRING",
            Self::UnicodeString => "UNICODE_STRING",
            Self::Blob => "BLOB",
            Self::Vector { elements } => match elements {
                2 => "VECTOR2",
                3 => "VECTOR3",
                4 => "VECTOR4",
                _ => unreachable!("vector element count"),
            },
            Self::UdoRef => "UDO_REF",
            Self::MailBox => "MAILBOX",
            Self::Python => "PYTHON",
            Self::Sequence { is_tuple, .. } => {
                if *is_tuple {
                    "TUPLE"
                } else {
                    "ARRAY"
                }
            }
            Self::FixedDict { .. } => "FIXED_DICT",
            Self::Class { .. } => "CLASS",
            Self::UserType { .. } => "USER_TYPE",
        }
    }
}

/// A parsed `<Type>` section (`DataType`).
#[derive(Debug, Clone, PartialEq)]
pub struct DataType {
    kind: Kind,
    /// `DataType::isConst_`, fixed by each concrete constructor.
    is_const: bool,
    /// The `<Default>` child, kept for `setDefaultValue`
    /// (`DataDescription::parse` decides between it and a materialised
    /// initial value based on `isConst`).
    default_section: Option<DataSection>,
}

impl Default for DataType {
    fn default() -> Self {
        // The reference holds a null DataTypePtr until parse() fills it in;
        // a placeholder scalar stands in for that null here and is never
        // observed past a successful parse.
        Self {
            kind: Kind::Int {
                bytes: 4,
                signed: true,
            },
            is_const: true,
            default_section: None,
        }
    }
}

impl DataType {
    /// The meta-type name (`DataType::typeName()` base case:
    /// `pMetaDataType_->name()`).
    pub fn meta_name(&self) -> &'static str {
        self.kind.meta_name()
    }

    /// The whole type, as the reference `typeName()` accumulates it.
    pub fn type_name(&self) -> String {
        match &self.kind {
            Kind::Sequence {
                is_tuple,
                size,
                element,
                ..
            } => {
                let mut name = if *is_tuple {
                    "TUPLE".to_string()
                } else {
                    "ARRAY".to_string()
                };
                if *size != 0 {
                    name.push_str(&format!(" size {size}"));
                }
                name.push_str(&format!(" of ({})", element.type_name()));
                name
            }
            Kind::FixedDict {
                allow_none,
                custom_type_name,
                module,
                instance,
                fields,
            } => {
                let mut name = "FIXED_DICT".to_string();
                if !custom_type_name.is_empty() {
                    name.push_str(&format!(" name:{custom_type_name}"));
                }
                if !module.is_empty() {
                    name.push_str(&format!(" [Implemented by {module}.{instance}]"));
                }
                if *allow_none {
                    name.push_str(" [None Allowed]");
                }
                push_fields(&mut name, fields);
                name
            }
            Kind::Class { allow_none, fields } => {
                let mut name = "CLASS".to_string();
                if *allow_none {
                    name.push_str(" [None Allowed]");
                }
                push_fields(&mut name, fields);
                name
            }
            Kind::UserType { module, instance } => {
                format!("USER_TYPE by {module}.{instance}")
            }
            _ => self.meta_name().to_string(),
        }
    }

    /// `DataType::streamSize`: bytes on a stream, or -1 when variable.
    pub fn stream_size(&self) -> i32 {
        match &self.kind {
            Kind::Int { bytes, .. } => *bytes,
            Kind::Float32 => 4,
            Kind::Float64 => 8,
            Kind::String | Kind::UnicodeString | Kind::Blob | Kind::Python => -1,
            Kind::Vector { elements } => elements * 4,
            Kind::UdoRef => UDO_REF_STREAM_SIZE,
            Kind::MailBox => MAILBOX_STREAM_SIZE,
            Kind::UserType { .. } => -1,
            Kind::Sequence { size, element, .. } => {
                let total = size * element.stream_size();
                // Either variable-length, or contains variable-length
                // elements.
                if total <= 0 { -1 } else { total }
            }
            Kind::FixedDict {
                allow_none, fields, ..
            } => {
                if *allow_none {
                    return -1;
                }
                let mut total = 0;
                for field in fields {
                    let field_size = field.type_.stream_size();
                    if field_size == -1 {
                        return -1;
                    }
                    total += field_size;
                }
                total
            }
            Kind::Class { .. } => -1,
        }
    }

    /// `DataType::clientSafety`: the worst safety level of the type (and of
    /// every `FIXED_DICT` field).
    pub fn client_safety(&self) -> u32 {
        match &self.kind {
            Kind::Python => CLIENT_UNSAFE,
            Kind::MailBox => CLIENT_UNUSABLE,
            Kind::FixedDict { fields, .. } | Kind::Class { fields, .. } => {
                let mut safety = CLIENT_SAFE;
                for field in fields {
                    safety |= field.type_.client_safety();
                }
                safety
            }
            _ => CLIENT_SAFE,
        }
    }

    /// `DataType::isConst`.
    pub fn is_const(&self) -> bool {
        self.is_const
    }

    /// The `<Default>` child of the type spec, if any.
    pub fn default_section(&self) -> Option<&DataSection> {
        self.default_section.as_ref()
    }

    /// `DataType::addToMD5`: the exact byte layout each concrete type
    /// appends. `sizeof("...")` strings include the terminating NUL; sizes
    /// and counts are little-endian 32-bit; names appended by size have no
    /// NUL.
    pub fn add_to_md5(&self, md5: &mut Md5) {
        match &self.kind {
            Kind::Int { bytes, signed } => {
                if *signed {
                    md5.append_cstr("Int");
                } else {
                    md5.append_cstr("Uint");
                }
                md5.append_i32(*bytes);
            }
            Kind::Float32 => md5.append_cstr("Float"),
            Kind::Float64 => md5.append_cstr("Float64"),
            Kind::String => md5.append_cstr("String"),
            Kind::UnicodeString => md5.append_cstr("UnicodeString"),
            Kind::Blob => md5.append_cstr("Blob"),
            Kind::Vector { elements } => {
                md5.append_cstr("Vector");
                md5.append_i32(*elements);
            }
            Kind::UdoRef => md5.append_cstr("UserDataObjectLinkDataType"),
            Kind::MailBox => md5.append_cstr("MailBox"),
            Kind::Python => md5.append_cstr("Python"),
            Kind::Sequence {
                is_tuple,
                size,
                db_len,
                element,
            } => {
                if *is_tuple {
                    md5.append_cstr("Tuple");
                } else {
                    md5.append_cstr("Array");
                }
                md5.append_i32(*size);
                if *db_len > 0 {
                    md5.append_i32(*db_len);
                }
                element.add_to_md5(md5);
            }
            Kind::FixedDict {
                allow_none,
                module,
                instance,
                fields,
                ..
            } => {
                md5.append_cstr("FixedDict");
                // The reference appends the custom-class implementor names
                // only when one is set; an empty module means none.
                if !module.is_empty() {
                    md5.append_text(module);
                    md5.append_text(instance);
                }
                md5.append_u8(u8::from(*allow_none));
                append_fields_to_md5(md5, fields);
            }
            Kind::Class { allow_none, fields } => {
                md5.append_cstr("Class");
                md5.append_u8(u8::from(*allow_none));
                append_fields_to_md5(md5, fields);
            }
            Kind::UserType { module, instance } => {
                md5.append_cstr("User");
                md5.append_text(module);
                md5.append_text(instance);
            }
        }
    }

    /// The kind payload, for structural inspection.
    /// `FixedDictDataType::getFieldDataType` (and `ClassDataType`'s twin):
    /// the type of one field, `None` when out of range or not a composite
    /// with named fields.
    pub fn field_type(&self, index: usize) -> Option<&DataType> {
        match self.kind() {
            Kind::FixedDict { fields, .. } | Kind::Class { fields, .. } => {
                fields.get(index).map(|field| field.type_.as_ref())
            }
            _ => None,
        }
    }

    /// `SequenceDataType::getElemType`: the element type, `None` when not a
    /// sequence.
    pub fn element_type(&self) -> Option<&DataType> {
        match self.kind() {
            Kind::Sequence { element, .. } => Some(element.as_ref()),
            _ => None,
        }
    }

    pub fn kind(&self) -> &Kind {
        &self.kind
    }
}

fn push_fields(name: &mut String, fields: &[Field]) {
    for (position, field) in fields.iter().enumerate() {
        name.push_str(if position == 0 { " props " } else { ", " });
        name.push_str(&format!("{}:({})", field.name, field.type_.type_name()));
    }
}

fn append_fields_to_md5(md5: &mut Md5, fields: &[Field]) {
    for field in fields {
        md5.append_text(&field.name);
        field.type_.add_to_md5(md5);
    }
}

// ---------------------------------------------------------------------------
// Meta-type registry
// ---------------------------------------------------------------------------

/// The internal `MetaDataType::addAlias( "FLOAT32", "FLOAT" )` pair: alias
/// name -> registered meta name.
const META_ALIASES: &[(&str, &str)] = &[("FLOAT", "FLOAT32")];

/// `MetaDataType::find`, over the registered set plus the internal alias.
fn find_meta(name: &str) -> Option<&'static str> {
    const REGISTERED: &[&str] = &[
        "INT8",
        "INT16",
        "INT32",
        "INT64",
        "UINT8",
        "UINT16",
        "UINT32",
        "UINT64",
        "FLOAT32",
        "FLOAT64",
        "STRING",
        "BLOB",
        "UNICODE_STRING",
        "UDO_REF",
        "MAILBOX",
        "PYTHON",
        "VECTOR2",
        "VECTOR3",
        "VECTOR4",
        "ARRAY",
        "TUPLE",
        "FIXED_DICT",
        "CLASS",
        "USER_TYPE",
    ];
    REGISTERED
        .iter()
        .find(|meta| **meta == name)
        .copied()
        .or_else(|| {
            META_ALIASES
                .iter()
                .find(|(alias, _)| *alias == name)
                .map(|(_, original)| *original)
        })
}

/// Build one of the leaf `SIMPLE_DATA_TYPE` instances from its meta name.
fn simple_type(meta: &str) -> DataType {
    let (kind, is_const) = match meta {
        "INT8" => (
            Kind::Int {
                bytes: 1,
                signed: true,
            },
            true,
        ),
        "INT16" => (
            Kind::Int {
                bytes: 2,
                signed: true,
            },
            true,
        ),
        "INT32" => (
            Kind::Int {
                bytes: 4,
                signed: true,
            },
            true,
        ),
        "INT64" => (
            Kind::Int {
                bytes: 8,
                signed: true,
            },
            true,
        ),
        "UINT8" => (
            Kind::Int {
                bytes: 1,
                signed: false,
            },
            true,
        ),
        "UINT16" => (
            Kind::Int {
                bytes: 2,
                signed: false,
            },
            true,
        ),
        "UINT32" => (
            Kind::Int {
                bytes: 4,
                signed: false,
            },
            true,
        ),
        "UINT64" => (
            Kind::Int {
                bytes: 8,
                signed: false,
            },
            true,
        ),
        "FLOAT32" => (Kind::Float32, true),
        "FLOAT64" => (Kind::Float64, true),
        "STRING" => (Kind::String, true),
        "UNICODE_STRING" => (Kind::UnicodeString, true),
        "BLOB" => (Kind::Blob, true),
        "UDO_REF" => (Kind::UdoRef, false),
        "MAILBOX" => (Kind::MailBox, true),
        "PYTHON" => (Kind::Python, false),
        "VECTOR2" => (Kind::Vector { elements: 2 }, false),
        "VECTOR3" => (Kind::Vector { elements: 3 }, false),
        "VECTOR4" => (Kind::Vector { elements: 4 }, false),
        _ => unreachable!("simple meta type {meta}"),
    };
    DataType {
        kind,
        is_const,
        default_section: None,
    }
}

/// `<size>` / `<persistAsBlob>` / `<of>` of a `SequenceMetaDataType`.
fn build_sequence(
    is_tuple: bool,
    section: &DataSection,
    ctx: &TypeContext,
    diag: &mut Diagnostics,
) -> Option<DataType> {
    let size = read_int(section, "size", 0);
    let persist_as_blob = read_bool(section, "persistAsBlob", false);
    let db_len = if persist_as_blob {
        PERSIST_AS_BLOB_DB_LEN
    } else {
        0
    };

    let element = match section.open_section("of") {
        Some(of) => build_type(ctx, Some(of), diag)?,
        None => {
            diag.error(format!(
                "SequenceMetaDataType::getType: Unable to create sequence of '{}'\n",
                read_string(section, "of", "")
            ));
            return None;
        }
    };
    let element = Arc::new(element);
    // SequenceDataType(pMeta, elementType, size, dbLen, isConst): the ARRAY
    // constructor passes false, the TUPLE constructor passes true, and the
    // sequence base ANDs it with the element's constness.
    let is_const = is_tuple && element.is_const();
    Some(DataType {
        kind: Kind::Sequence {
            is_tuple,
            size,
            db_len,
            element,
        },
        is_const,
        default_section: None,
    })
}

/// `ClassMetaDataType::buildType`, shared by `CLASS` and `FIXED_DICT`.
fn build_class_fields(
    section: &DataSection,
    meta: &str,
    ctx: &TypeContext,
    diag: &mut Diagnostics,
) -> Option<Vec<Field>> {
    let mut fields = Vec::new();
    if let Some(parent) = section.open_section("Parent") {
        let parent_type = build_type(ctx, Some(parent), diag)?;
        let parent_meta = parent_type.meta_name();
        if parent_meta != meta {
            diag.error(format!("<Parent> of {meta} must also be a {meta}\n"));
            return None;
        }
        // Just rip its fields out for now.
        fields = match parent_type.kind() {
            Kind::Class { fields, .. } | Kind::FixedDict { fields, .. } => fields.clone(),
            _ => unreachable!("parent of {meta} is a {parent_meta}"),
        };
    }

    let properties = section.open_section("Properties");
    if properties.is_none_or(|p| p.count_children() == 0) && fields.is_empty() {
        diag.error(format!(
            "<Properties> section missing or empty in {meta} type spec\n"
        ));
        return None;
    }

    if let Some(properties) = properties {
        for child in children(properties) {
            let name = child.section_name().to_string();
            let type_ = match child.open_section("Type") {
                Some(type_section) => build_type(ctx, Some(type_section), diag)?,
                None => {
                    diag.error(format!(
                        "Could not build type for property '{name}' in {meta} type\n"
                    ));
                    return None;
                }
            };
            fields.push(Field {
                name,
                db_len: read_int(child, "DatabaseLength", DEFAULT_DATABASE_LENGTH),
                is_persistent: read_bool(child, "Persistent", true),
                type_: Arc::new(type_),
            });
        }
    }
    Some(fields)
}

/// `FixedDictMetaDataType::parseCustomTypeName`.
fn parse_custom_type_name(
    section: &DataSection,
    ctx: &TypeContext,
    diag: &mut Diagnostics,
) -> Option<String> {
    let type_name = read_string(section, "TypeName", "");
    if type_name.is_empty() {
        // Allow empty type names.
        return Some(String::new());
    }
    if type_name.contains(' ') {
        diag.error(format!(
            "FixedDictMetaDataType::parseTypeName: <TypeName> must not contain spaces: \"{type_name}\"\n"
        ));
        return None;
    }
    if !ctx.custom_type_names.borrow_mut().insert(type_name.clone()) {
        diag.error(format!(
            "FixedDictMetaDataType::parseTypeName: <TypeName> is not unique: \"{type_name}\"\n"
        ));
        return None;
    }
    Some(type_name)
}

/// `UserMetaDataType::getType`: split `<implementedBy>` at its last dot.
fn build_user_type(section: &DataSection, diag: &mut Diagnostics) -> Option<DataType> {
    let implemented_by = read_string(section, "implementedBy", "");
    if implemented_by.is_empty() {
        diag.error("UserMetaDataType::getType: implementedBy is not specified.\n");
        return None;
    }
    let split = match implemented_by.rfind('.') {
        Some(pos) => pos,
        None => {
            diag.error(format!(
                "UserMetaDataType::getType: Invalid implementedBy {implemented_by} - must contain a '.'"
            ));
            return None;
        }
    };
    Some(DataType {
        kind: Kind::UserType {
            module: implemented_by[..split].to_string(),
            instance: implemented_by[split + 1..].to_string(),
        },
        is_const: false,
        default_section: None,
    })
}

/// `DataType::buildDataType( DataSectionPtr )`: resolve the `<Type>` text
/// through aliases and the meta-type registry.
///
/// `section == None` mirrors a null `DataSectionPtr` (the missing `<Type>`
/// child) and reports the reference warning.
pub fn build_type(
    ctx: &TypeContext,
    section: Option<&DataSection>,
    diag: &mut Diagnostics,
) -> Option<DataType> {
    let Some(section) = section else {
        diag.warning("DataType::buildDataType: No <Type> section\n");
        return None;
    };

    let type_name = section.as_string();

    // See if it is an alias.
    if let Some(alias) = ctx.aliases.get(type_name) {
        if section.find_child("Default").is_some() {
            diag.warning(format!(
                "DataType::buildDataType: New default value for aliased data type '{type_name}' is ignored. The default value of an aliased data type can only be overridden by the default value of an entity property.\n"
            ));
        }
        return Some((**alias).clone());
    }

    let Some(meta) = find_meta(type_name) else {
        diag.error(format!(
            "DataType::buildDataType: Could not find MetaDataType '{type_name}'\n"
        ));
        return None;
    };

    let mut data_type = match meta {
        "ARRAY" => build_sequence(false, section, ctx, diag)?,
        "TUPLE" => build_sequence(true, section, ctx, diag)?,
        "FIXED_DICT" => {
            let fields = build_class_fields(section, meta, ctx, diag)?;
            let custom_type_name = parse_custom_type_name(section, ctx, diag)?;
            DataType {
                kind: Kind::FixedDict {
                    allow_none: read_bool(section, "AllowNone", false),
                    custom_type_name,
                    // <implementedBy> is a SCRIPT_PYTHON feature; until a
                    // Python host parses definitions here, no custom class
                    // is attached, which is exactly the no-implementedBy
                    // digest path.
                    module: String::new(),
                    instance: String::new(),
                    fields,
                },
                is_const: false,
                default_section: None,
            }
        }
        "CLASS" => DataType {
            kind: Kind::Class {
                allow_none: read_bool(section, "AllowNone", false),
                fields: build_class_fields(section, meta, ctx, diag)?,
            },
            is_const: false,
            default_section: None,
        },
        "USER_TYPE" => build_user_type(section, diag)?,
        _ => simple_type(meta),
    };

    // setDefaultValue( pSection->findChild( "Default" ) ). A mailbox rejects
    // a default section but the type itself is still built, and the section
    // is not stored: `MailBoxDataType::setDefaultValue` returns without the
    // base call, so `pDefaultValue_` stays NULL and the zeroed ref remains
    // the default.
    let default_section = section.find_child("Default");
    if matches!(data_type.kind(), Kind::MailBox) {
        if default_section.is_some() {
            diag.error(
                "MailBoxDataType::setDefaultValue: Default value for mailbox not supported\n",
            );
        }
        return Some(data_type);
    }
    data_type.default_section = default_section.cloned();
    Some(data_type)
}

// ---------------------------------------------------------------------------
// Aliases (`DataType::initAliases`)
// ---------------------------------------------------------------------------

/// The alias table, built once from `scripts/entity_defs/alias.xml`
/// (`DataType::s_aliases_`).
#[derive(Debug, Default)]
pub struct TypeContext {
    aliases: HashMap<String, Arc<DataType>>,
    /// `FixedDictMetaDataType::typeNames_`, the `<TypeName>` uniqueness set.
    /// A `RefCell` because the reference stores this in a static that every
    /// `parseTypeName` call mutates while the context is shared read-only.
    custom_type_names: std::cell::RefCell<std::collections::HashSet<String>>,
}

impl TypeContext {
    /// `DataType::initAliases`: the internal `FLOAT` alias plus every
    /// `alias.xml` child (full data type instances, not just labels).
    pub fn init_aliases(
        &mut self,
        open_aliases: impl FnOnce() -> Option<DataSection>,
        diag: &mut Diagnostics,
    ) {
        let Some(aliases) = open_aliases() else {
            diag.warning(format!(
                "Couldn't open aliases file '{}'\n",
                crate::constants::ALIASES_FILE
            ));
            return;
        };
        for child in children(&aliases) {
            let name = child.section_name().to_string();
            match build_type(self, Some(child), diag) {
                Some(data_type) => {
                    self.aliases.insert(name, Arc::new(data_type));
                }
                None => {
                    diag.error(format!("DataType::initAliases: Failed to add {name}\n"));
                }
            }
        }
    }

    pub fn alias(&self, name: &str) -> Option<&Arc<DataType>> {
        self.aliases.get(name)
    }

    pub fn alias_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.aliases.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }
}

// ---------------------------------------------------------------------------
// DataSection read helpers (readInt / readBool / readFloat / readString)
// ---------------------------------------------------------------------------

pub(crate) fn read_int(section: &DataSection, path: &str, default: i32) -> i32 {
    section
        .open_section(path)
        .map_or(default, |child| child.as_int(default))
}

pub(crate) fn read_bool(section: &DataSection, path: &str, default: bool) -> bool {
    section
        .open_section(path)
        .map_or(default, |child| child.as_bool(default))
}

pub(crate) fn read_string(section: &DataSection, path: &str, default: &str) -> String {
    section.read_string(path, default)
}

/// The child sections of `section`, in document order.
pub(crate) fn children(section: &DataSection) -> impl Iterator<Item = &DataSection> + '_ {
    (0..section.count_children()).filter_map(move |index| section.open_child(index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Diagnostics;

    fn parse_xml(xml: &str) -> DataSection {
        resources::datasection::parse("test", xml.as_bytes()).expect("valid fixture xml")
    }

    fn build(xml: &str) -> (Option<DataType>, Diagnostics) {
        let mut diag = Diagnostics::new();
        diag.set_component(Diagnostics::DATA_DESCRIPTION_COMPONENT);
        let ctx = TypeContext::default();
        let section = parse_xml(xml);
        let built = build_type(&ctx, Some(&section), &mut diag);
        (built, diag)
    }

    #[test]
    fn scalars_parse_with_reference_stream_sizes_and_digests() {
        let (int32, _) = build("<a> INT32 </a>");
        let int32 = int32.expect("INT32 builds");
        assert_eq!(int32.meta_name(), "INT32");
        assert_eq!(int32.type_name(), "INT32");
        assert_eq!(int32.stream_size(), 4);
        assert!(int32.is_const());

        let (uint8, _) = build("<a> UINT8 </a>");
        let uint8 = uint8.expect("UINT8 builds");
        assert_eq!(uint8.stream_size(), 1);

        let (float64, _) = build("<a> FLOAT64 </a>");
        let float64 = float64.expect("FLOAT64 builds");
        assert_eq!(float64.stream_size(), 8);

        let (string, _) = build("<a> STRING </a>");
        let string = string.expect("STRING builds");
        assert_eq!(string.stream_size(), -1);
        assert_eq!(string.type_name(), "STRING");

        // The internal alias makes FLOAT a synonym of FLOAT32.
        let (float, _) = build("<a> FLOAT </a>");
        let float = float.expect("FLOAT builds via the internal alias");
        assert_eq!(float.meta_name(), "FLOAT32");
        assert_eq!(float.stream_size(), 4);
    }

    #[test]
    fn leaf_digests_match_the_reference_byte_layouts() {
        // IntegerDataType: "Int"/"Uint" with NUL, then sizeof(INT_TYPE).
        let (int32, _) = build("<a> INT32 </a>");
        let mut md5 = Md5::new();
        int32.as_ref().expect("INT32").add_to_md5(&mut md5);
        let mut expected = Md5::new();
        expected.append_cstr("Int");
        expected.append_i32(4);
        assert_eq!(md5.quote(), expected.quote());

        let (uint16, _) = build("<a> UINT16 </a>");
        let mut md5 = Md5::new();
        uint16.as_ref().expect("UINT16").add_to_md5(&mut md5);
        let mut expected = Md5::new();
        expected.append_cstr("Uint");
        expected.append_i32(2);
        assert_eq!(md5.quote(), expected.quote());

        // Fixed-size leaf types append only their tag string.
        for (xml, tag) in [
            ("<a> STRING </a>", "String"),
            ("<a> BLOB </a>", "Blob"),
            ("<a> UNICODE_STRING </a>", "UnicodeString"),
            ("<a> MAILBOX </a>", "MailBox"),
            ("<a> PYTHON </a>", "Python"),
            ("<a> UDO_REF </a>", "UserDataObjectLinkDataType"),
        ] {
            let (data_type, _) = build(xml);
            let mut md5 = Md5::new();
            data_type.as_ref().expect(xml).add_to_md5(&mut md5);
            let mut expected = Md5::new();
            expected.append_cstr(tag);
            assert_eq!(md5.quote(), expected.quote(), "{tag}");
        }

        // VectorDataType: tag plus NUM_ELEMENTS.
        let (vector3, _) = build("<a> VECTOR3 </a>");
        let mut md5 = Md5::new();
        vector3.as_ref().expect("VECTOR3").add_to_md5(&mut md5);
        let mut expected = Md5::new();
        expected.append_cstr("Vector");
        expected.append_i32(3);
        assert_eq!(md5.quote(), expected.quote());
        assert_eq!(vector3.expect("VECTOR3").stream_size(), 12);
    }

    #[test]
    fn sequences_carry_size_db_len_and_element() {
        let (array, _) = build("<a> ARRAY <of> INT32 </of> </a>");
        let array = array.expect("ARRAY builds");
        assert_eq!(array.type_name(), "ARRAY of (INT32)");
        assert_eq!(array.stream_size(), -1, "variable length");
        assert!(!array.is_const());

        let (fixed_array, _) = build("<a> ARRAY <size> 4 </size> <of> INT8 </of> </a>");
        let fixed_array = fixed_array.expect("fixed ARRAY builds");
        assert_eq!(fixed_array.type_name(), "ARRAY size 4 of (INT8)");
        assert_eq!(fixed_array.stream_size(), 4);

        let (tuple, _) = build("<a> TUPLE <of> UINT16 </of> </a>");
        let tuple = tuple.expect("TUPLE builds");
        assert_eq!(tuple.type_name(), "TUPLE of (UINT16)");
        assert!(tuple.is_const(), "tuples inherit element constness");

        // Digest: "Array" + size + [dbLen when > 0] + element.
        let (persistence_array, _) = build(
            "<a> ARRAY <size> 2 </size> <persistAsBlob> true </persistAsBlob> <of> INT16 </of> </a>",
        );
        let persistence_array = persistence_array.expect("persistAsBlob array");
        let mut md5 = Md5::new();
        persistence_array.add_to_md5(&mut md5);
        let mut expected = Md5::new();
        expected.append_cstr("Array");
        expected.append_i32(2);
        expected.append_i32(16_777_215);
        expected.append_cstr("Int");
        expected.append_i32(2);
        assert_eq!(md5.quote(), expected.quote());
    }

    #[test]
    fn fixed_dict_accumulates_fields_and_safety() {
        let (fixed_dict, _) = build(
            "<a> FIXED_DICT <Properties> \
                 <name> <Type> STRING </Type> </name> \
                 <ammo> <Type> UINT32 </Type> </ammo> \
             </Properties> </a>",
        );
        let fixed_dict = fixed_dict.expect("FIXED_DICT builds");
        assert_eq!(
            fixed_dict.type_name(),
            "FIXED_DICT props name:(STRING), ammo:(UINT32)"
        );
        assert_eq!(fixed_dict.stream_size(), -1, "STRING field is variable");

        let (all_fixed, _) = build(
            "<a> FIXED_DICT <Properties> \
                 <ammo> <Type> UINT32 </Type> </ammo> \
                 <count> <Type> UINT8 </Type> </count> \
             </Properties> </a>",
        );
        let all_fixed = all_fixed.expect("fixed-size FIXED_DICT");
        assert_eq!(all_fixed.stream_size(), 5);
        assert!(!all_fixed.is_const());

        // Digest: tag, allowNone byte, then per-field name bytes + digest.
        let mut md5 = Md5::new();
        all_fixed.add_to_md5(&mut md5);
        let mut expected = Md5::new();
        expected.append_cstr("FixedDict");
        expected.append_u8(0);
        expected.append_text("ammo");
        expected.append_cstr("Uint");
        expected.append_i32(4);
        expected.append_text("count");
        expected.append_cstr("Uint");
        expected.append_i32(1);
        assert_eq!(md5.quote(), expected.quote());

        // allowNone forces a variable stream size and a set byte in the
        // digest.
        let (allow_none, _) = build(
            "<a> FIXED_DICT <AllowNone> true </AllowNone> \
                 <Properties> <ammo> <Type> UINT32 </Type> </ammo> </Properties> </a>",
        );
        let allow_none = allow_none.expect("AllowNone FIXED_DICT");
        assert_eq!(allow_none.stream_size(), -1);
        let mut md5 = Md5::new();
        allow_none.add_to_md5(&mut md5);
        let mut expected = Md5::new();
        expected.append_cstr("FixedDict");
        expected.append_u8(1);
        expected.append_text("ammo");
        expected.append_cstr("Uint");
        expected.append_i32(4);
        assert_eq!(md5.quote(), expected.quote());
    }

    #[test]
    fn fixed_dict_custom_type_name_is_parsed_and_checked() {
        let (named, diag) = build(
            "<a> FIXED_DICT <TypeName> Gun </TypeName> \
                 <Properties> <ammo> <Type> UINT32 </Type> </ammo> </Properties> </a>",
        );
        let named = named.expect("named FIXED_DICT");
        assert!(named.type_name().starts_with("FIXED_DICT name:Gun "));
        assert!(!diag.has_errors());

        let (spaced, diag) = build(
            "<a> FIXED_DICT <TypeName> has space </TypeName> \
                 <Properties> <ammo> <Type> UINT32 </Type> </ammo> </Properties> </a>",
        );
        assert!(spaced.is_none());
        assert!(diag.has_errors());

        // Two dictionaries in one context cannot share a <TypeName>; the
        // parse-time uniqueness set lives on the TypeContext, so build both
        // through one.
        let mut diag = Diagnostics::new();
        let ctx = TypeContext::default();
        let first = parse_xml(
            "<first> FIXED_DICT <TypeName> Gun </TypeName> \
                 <Properties> <ammo> <Type> UINT32 </Type> </ammo> </Properties> </first>",
        );
        assert!(build_type(&ctx, Some(&first), &mut diag).is_some());
        let second = parse_xml(
            "<second> FIXED_DICT <TypeName> Gun </TypeName> \
                 <Properties> <ammo> <Type> UINT32 </Type> </ammo> </Properties> </second>",
        );
        assert!(build_type(&ctx, Some(&second), &mut diag).is_none());
        assert!(diag.has_errors());
    }

    #[test]
    fn class_types_inherit_parent_fields() {
        let (class, _) = build(
            "<a> CLASS <Properties> \
                 <hp> <Type> INT32 </Type> </hp> \
             </Properties> </a>",
        );
        let class = class.expect("CLASS builds");
        assert_eq!(class.type_name(), "CLASS props hp:(INT32)");
        assert_eq!(class.stream_size(), -1, "CLASS is always variable");

        let (subclass, diag) = build(
            "<a> CLASS <Parent> CLASS <Properties> <hp> <Type> INT32 </Type> </hp> </Properties> </Parent> <Properties> \
                 <mp> <Type> INT16 </Type> </mp> \
             </Properties> </a>",
        );
        let subclass = subclass.expect("CLASS with parent");
        assert_eq!(subclass.type_name(), "CLASS props hp:(INT32), mp:(INT16)");
        assert!(!diag.has_errors());

        // A parent of the wrong meta type is an error.
        let (_, diag) = build(
            "<a> CLASS <Parent> FIXED_DICT <Properties> <hp> <Type> INT32 </Type> </hp> </Properties> </Parent> <Properties> <mp> <Type> INT16 </Type> </mp> </Properties> </a>",
        );
        assert!(diag.has_errors());
    }

    #[test]
    fn user_type_splits_implemented_by_at_the_last_dot() {
        let (user_type, _) =
            build("<a> USER_TYPE <implementedBy> TestDataType.instance </implementedBy> </a>");
        let user_type = user_type.expect("USER_TYPE builds");
        assert_eq!(user_type.type_name(), "USER_TYPE by TestDataType.instance");
        assert_eq!(user_type.stream_size(), -1);
        assert!(!user_type.is_const());

        // Digest: "User" + module + instance (both without NUL).
        let mut md5 = Md5::new();
        user_type.add_to_md5(&mut md5);
        let mut expected = Md5::new();
        expected.append_cstr("User");
        expected.append_text("TestDataType");
        expected.append_text("instance");
        assert_eq!(md5.quote(), expected.quote());

        let (no_dot, diag) = build("<a> USER_TYPE <implementedBy> NoDot </implementedBy> </a>");
        assert!(no_dot.is_none());
        assert!(diag.has_errors());

        let (empty, diag) = build("<a> USER_TYPE </a>");
        assert!(empty.is_none());
        assert!(diag.has_errors());
    }

    #[test]
    fn unknown_meta_names_and_missing_sections_are_errors() {
        let (_, diag) = build("<a> NONSENSE </a>");
        assert_eq!(
            diag.errors(),
            vec!["DataType::buildDataType: Could not find MetaDataType 'NONSENSE'\n"]
        );

        let mut diag = Diagnostics::new();
        let ctx = TypeContext::default();
        assert!(build_type(&ctx, None, &mut diag).is_none());
        assert_eq!(
            diag.warnings(),
            vec!["DataType::buildDataType: No <Type> section\n"]
        );

        // A sequence without an <of> child.
        let (_, diag) = build("<a> ARRAY </a>");
        assert!(
            diag.errors()
                .iter()
                .any(|m| m.contains("Unable to create sequence of ''"))
        );

        // A class with no properties.
        let (_, diag) = build("<a> CLASS </a>");
        assert!(
            diag.errors()
                .iter()
                .any(|m| m.contains("<Properties> section missing or empty in CLASS type spec"))
        );
    }

    #[test]
    fn client_safety_accumulates_through_containers() {
        // PYTHON args are CLIENT_UNSAFE; MAILBOX is CLIENT_UNUSABLE.
        let (python, _) = build("<a> PYTHON </a>");
        assert_eq!(python.expect("PYTHON").client_safety(), CLIENT_UNSAFE);

        let (mailbox, _) = build("<a> MAILBOX </a>");
        assert_eq!(mailbox.expect("MAILBOX").client_safety(), CLIENT_UNUSABLE);

        let (safe_dict, _) =
            build("<a> FIXED_DICT <Properties> <n> <Type> INT32 </Type> </n> </Properties> </a>");
        assert_eq!(safe_dict.expect("dict").client_safety(), CLIENT_SAFE);

        let (unsafe_dict, _) = build(
            "<a> FIXED_DICT <Properties> \
                 <p> <Type> PYTHON </Type> </p> \
                 <m> <Type> MAILBOX </Type> </m> \
             </Properties> </a>",
        );
        assert_eq!(
            unsafe_dict.expect("dict").client_safety(),
            CLIENT_UNSAFE | CLIENT_UNUSABLE
        );
    }

    #[test]
    fn type_defaults_are_kept_for_later_materialisation() {
        let (with_default, _) = build("<a> INT32 <Default> 42 </Default> </a>");
        let with_default = with_default.expect("INT32 with default");
        assert_eq!(
            with_default
                .default_section()
                .expect("default kept")
                .as_string()
                .trim(),
            "42"
        );

        let (without_default, _) = build("<a> INT32 </a>");
        assert!(without_default.expect("INT32").default_section().is_none());
    }

    #[test]
    fn aliases_load_from_the_alias_file_and_shadow_meta_names() {
        let mut diag = Diagnostics::new();
        let mut ctx = TypeContext::default();
        let alias_file = parse_xml(
            "<root> \
                 <OBJECT_ID> INT32 </OBJECT_ID> \
                 <STATS_MATRIX> ARRAY <of> UINT16 </of> </STATS_MATRIX> \
             </root>",
        );
        ctx.init_aliases(|| Some(alias_file), &mut diag);

        assert_eq!(ctx.alias_names(), vec!["OBJECT_ID", "STATS_MATRIX"]);
        let object_id = ctx.alias("OBJECT_ID").expect("alias entry");
        assert_eq!(object_id.meta_name(), "INT32");
        assert_eq!(object_id.type_name(), "INT32");
        assert!(Arc::ptr_eq(
            ctx.alias("OBJECT_ID").expect("stable instance"),
            &ctx.aliases["OBJECT_ID"]
        ));

        let matrix = ctx.alias("STATS_MATRIX").expect("alias entry");
        assert_eq!(
            matrix.type_name(),
            "ARRAY of (UINT16)",
            "aliases are whole data types, not labels"
        );

        // Aliases resolve through build_type and beat the meta registry.
        let mut diag = Diagnostics::new();
        let section = parse_xml("<a> OBJECT_ID </a>");
        let built = build_type(&ctx, Some(&section), &mut diag);
        assert_eq!(built.expect("alias resolves").type_name(), "INT32");

        // A <Default> on an aliased use is ignored with a warning.
        let mut diag = Diagnostics::new();
        let section = parse_xml("<a> OBJECT_ID <Default> 7 </Default> </a>");
        assert!(build_type(&ctx, Some(&section), &mut diag).is_some());
        assert!(
            diag.warnings()
                .iter()
                .any(|m| m
                    .contains("New default value for aliased data type 'OBJECT_ID' is ignored"))
        );
    }

    #[test]
    fn missing_alias_file_is_a_warning_and_empty_names_still_build() {
        let mut diag = Diagnostics::new();
        let mut ctx = TypeContext::default();
        ctx.init_aliases(|| None, &mut diag);
        assert_eq!(
            diag.warnings(),
            vec!["Couldn't open aliases file 'scripts/entity_defs/alias.xml'\n"]
        );
        assert!(ctx.alias_names().is_empty());

        // A broken alias child reports the failure but keeps the rest.
        let mut diag = Diagnostics::new();
        let mut ctx = TypeContext::default();
        let alias_file = parse_xml("<root> <GOOD> INT8 </GOOD> <BAD> NONSENSE </BAD> </root>");
        ctx.init_aliases(|| Some(alias_file), &mut diag);
        assert!(ctx.alias("GOOD").is_some());
        assert!(ctx.alias("BAD").is_none());
        assert!(
            diag.errors()
                .iter()
                .any(|m| m == "DataType::initAliases: Failed to add BAD\n")
        );
    }

    /// Every scalar meta type reports its own name, its wire size, and its
    /// digest contribution (`SimpleMetaDataType`/`TypeMetaDataType` arms).
    #[test]
    fn scalar_meta_types_cover_the_reference_table() {
        let cases: [(&str, &str, i32); 16] = [
            ("INT8", "INT8", 1),
            ("INT16", "INT16", 2),
            ("INT32", "INT32", 4),
            ("INT64", "INT64", 8),
            ("UINT8", "UINT8", 1),
            ("UINT16", "UINT16", 2),
            ("UINT32", "UINT32", 4),
            ("UINT64", "UINT64", 8),
            ("FLOAT32", "FLOAT32", 4),
            ("FLOAT64", "FLOAT64", 8),
            ("STRING", "STRING", -1),
            ("UNICODE_STRING", "UNICODE_STRING", -1),
            ("BLOB", "BLOB", -1),
            ("VECTOR2", "VECTOR2", 8),
            ("VECTOR3", "VECTOR3", 12),
            ("VECTOR4", "VECTOR4", 16),
        ];

        for (name, meta, size) in cases {
            let (built, diag) = build(&format!("<a> {name} </a>"));
            let ty = built.unwrap_or_else(|| panic!("{name} failed: {:?}", diag.messages()));
            assert_eq!(ty.meta_name(), meta, "{name}");
            // Scalars fall through to the meta name in typeName().
            assert_eq!(ty.type_name(), meta, "{name}");
            assert_eq!(ty.stream_size(), size, "{name}");

            let mut md5 = Md5::new();
            ty.add_to_md5(&mut md5);
            assert_eq!(md5.quote().len(), 32, "{name} digests");
        }

        let (udo_ref, _) = build("<a> UDO_REF </a>");
        let udo_ref = udo_ref.expect("UDO_REF builds");
        assert_eq!(udo_ref.meta_name(), "UDO_REF");
        assert_eq!(udo_ref.stream_size(), UDO_REF_STREAM_SIZE);

        let (mailbox, _) = build("<a> MAILBOX </a>");
        let mailbox = mailbox.expect("MAILBOX builds");
        assert_eq!(mailbox.meta_name(), "MAILBOX");
        assert_eq!(mailbox.stream_size(), MAILBOX_STREAM_SIZE);

        let (python, _) = build("<a> PYTHON </a>");
        let python = python.expect("PYTHON builds");
        assert_eq!(python.meta_name(), "PYTHON");
        assert_eq!(python.type_name(), "PYTHON");
        assert_eq!(python.stream_size(), -1);
    }

    /// `FIXED_DICT` and `CLASS` names, and the sequence digest arms.
    #[test]
    fn structured_type_names_and_digests_follow_the_reference_shape() {
        let (fixed, diag) = build(
            "<a> FIXED_DICT <TypeName> Gun </TypeName> \
                 <AllowNone> true </AllowNone> \
                 <Properties> <n> <Type> INT32 </Type> </n> </Properties> </a>",
        );
        let fixed = fixed.unwrap_or_else(|| panic!("{:?}", diag.messages()));
        assert_eq!(
            fixed.type_name(),
            "FIXED_DICT name:Gun [None Allowed] props n:(INT32)"
        );
        assert_eq!(fixed.meta_name(), "FIXED_DICT");

        let (class, diag) = build(
            "<a> CLASS <AllowNone> true </AllowNone> \
                 <Properties> <hp> <Type> INT32 </Type> </hp> </Properties> </a>",
        );
        let class = class.unwrap_or_else(|| panic!("{:?}", diag.messages()));
        assert_eq!(class.type_name(), "CLASS [None Allowed] props hp:(INT32)");
        assert_eq!(class.meta_name(), "CLASS");

        let (user_type, _) =
            build("<a> USER_TYPE <implementedBy> test.GunDef </implementedBy> </a>");
        let user_type = user_type.expect("USER_TYPE builds");
        assert_eq!(user_type.meta_name(), "USER_TYPE");

        let (tuple, _) = build("<a> TUPLE <of> UINT16 </of> </a>");
        let tuple = tuple.expect("TUPLE builds");
        assert_eq!(tuple.meta_name(), "TUPLE");
        let (array, _) = build(
            "<a> ARRAY <size> 2 </size> <persistAsBlob> true </persistAsBlob> \
                 <DatabaseLength> 8 </DatabaseLength> <of> INT16 </of> </a>",
        );
        let array = array.expect("blob ARRAY builds");
        assert_eq!(array.meta_name(), "ARRAY");

        // Both digest arms record their meta kind.
        for ty in [&tuple, &array, &class, &fixed] {
            let mut md5 = Md5::new();
            ty.add_to_md5(&mut md5);
            assert_eq!(md5.quote().len(), 32);
        }
    }

    /// A field with no `<Type>` child fails the whole spec.
    #[test]
    fn a_field_without_a_type_fails_the_class_spec() {
        let (_, diag) = build(
            "<a> CLASS <Properties> \
                 <hp> <Flags> BASE </Flags> </hp> </Properties> </a>",
        );
        assert!(
            diag.errors()
                .iter()
                .any(|m| m.contains("Could not build type for property 'hp' in CLASS type")),
            "{:?}",
            diag.messages()
        );
    }
}
