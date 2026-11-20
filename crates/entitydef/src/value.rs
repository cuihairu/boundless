//! Property values: materialising a default, gating a value against its
//! definition type, and moving a value to and from a stream.
//!
//! The reference keeps one value per property as a Python object and moves it
//! over a stream through `DataType::StreamElement` objects
//! (`lib/entitydef/data_types/*`). Here a [`PropertyValue`] holds the scalar
//! set the definition types can express and the wire layout is reproduced on
//! `foundation::stream::BinaryStream`:
//!
//! | meta type | bytes on a stream |
//! |---|---|
//! | `INT8/16/32/64`, `UINT8/16/32/64` | `bytes` little-endian two's complement |
//! | `FLOAT32`, `FLOAT64` | IEEE-754, 4 and 8 bytes |
//! | `STRING` | packed int length + raw bytes (`operator<<`) |
//! | `UNICODE_STRING` | packed int length + UTF-8 bytes |
//! | `BLOB` | packed int length + raw bytes |
//! | `VECTOR2/3/4` | `elements` IEEE-754 floats, back to back |
//!
//! The composite metas (`ARRAY`/`TUPLE`, `FIXED_DICT`, `CLASS`, `USER_TYPE`,
//! `UDO_REF`, `MAILBOX`, `PYTHON`) are outside this module and report
//! [`PropertyError::Unsupported`].

use foundation::math::{Vector2, Vector3, Vector4};
use foundation::stream::BinaryStream;
use resources::datasection::DataSection;

use crate::data_description::DataDescription;
use crate::data_type::{DataType, Kind};

/// A materialised property value.
///
/// The reference has no `BOOL` meta type, so there is no boolean variant:
/// flags are parsed as `i32` attributes and never stored as property values.
/// `Float` carries an `f64` because `FloatDataType<float>::addToStream` narrows
/// to `float` on the way out while `FloatDataType<double>` does not.
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    /// The signed integer metas (`INT8` .. `INT64`).
    Int(i64),
    /// The unsigned integer metas (`UINT8` .. `UINT64`).
    Uint(u64),
    /// `FLOAT32` and `FLOAT64`.
    Float(f64),
    /// `STRING` and `UNICODE_STRING` (both are UTF-8 bytes on the wire).
    String(String),
    /// `BLOB`.
    Blob(Vec<u8>),
    /// `VECTOR2`, `VECTOR3`, `VECTOR4`.
    Vector(Vec<f32>),
    /// `ARRAY` and `TUPLE` (`SequenceDataType`): the elements in order.
    /// A tuple is just a fixed-length array, so both share this variant; the
    /// distinction lives on the type (`Kind::Sequence { is_tuple, .. }`).
    Sequence(Vec<PropertyValue>),
    /// `FIXED_DICT` (`FixedDictDataType`): `None` for the `<AllowNone>` value,
    /// else the fields as `(name, value)` pairs in the type's own field order.
    FixedDict(Option<Vec<(String, PropertyValue)>>),
    /// `MAILBOX` (`EntityMailBoxRef`): the entity id plus its address, which
    /// is an `ip`, a `port` and the `salt` carrying the component and type.
    MailBox {
        id: i32,
        ip: u32,
        port: u16,
        salt: u16,
    },
}

/// The `SimpleStreamElement` / `BlobStreamElement` truncation text.
pub(crate) const SIMPLE_STREAM_ELEMENT: &str = "SimpleStreamElement";
const BLOB_STREAM_ELEMENT: &str = "BlobStreamElement";

/// The `UserDataType` reporting sites (`user_data_type.cpp`). Each names the
/// function that found the Python converter missing.
const USER_ADD_TO_STREAM: &str = "UserDataType::addObjectToStream";
const USER_CREATE_FROM_STREAM: &str = "UserDataType::createObjectFromStream";
const USER_CREATE_FROM_SECTION: &str = "UserDataType::createObjectFromSection";

/// A failure, carrying the reference's own message text where one exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyError {
    /// `IntegerDataType::isSameType`: truncating the value to the type's C++
    /// width changes it.
    OutOfRange { value: i64, truncated: i64 },
    /// `UnicodeStringDataType::createFromSection`: the section text is not
    /// valid UTF-8. Unreachable through the Rust `DataSection`, which
    /// lossily normalises XML text to UTF-8 first, but kept so the reference's
    /// error channel has a name.
    NotUtf8,
    /// The value kind is not one the type can hold. The reference fails
    /// `isSameType` here without printing anything, so this text is ours.
    TypeMismatch { meta: &'static str },
    /// `SimpleStreamElement` / `BlobStreamElement`: the stream ran dry.
    StreamTruncated { element: &'static str },
    /// `SequenceDataType::isSameType`: a fixed-length sequence holding the
    /// wrong number of elements.
    WrongSequenceSize { size: i32, expected: i32 },
    /// `SequenceDataType::isSameType`: an element of the wrong type. The
    /// reference prints the Python `str()` of the value; ours prints the Rust
    /// `Debug` rendering, since this build has no Python host.
    BadSequenceElement {
        index: usize,
        expected: String,
        value: String,
    },
    /// `SequenceStartStreamElement::fromSourceToStream`: a fixed-length
    /// sequence whose value carries a different number of elements.
    FixedSequenceSize { size: i32, expected: i32 },
    /// `SequenceStartStreamElement::fromStreamToSink`: a variable-length
    /// sequence whose count prefix is missing (the stream ran dry).
    MissingSequenceSize,
    /// `SequenceStartStreamElement::fromStreamToSink`: the count exceeds the
    /// bytes left, with variable-sized elements. The reference prints
    /// "fromSourceToStream" here — a copy-paste bug in the original — so we
    /// reproduce it verbatim.
    InvalidSequenceSize { size: usize, remaining: i32 },
    /// As above, but with fixed-sized elements (the count * element size
    /// exceeds the bytes left).
    ExpectedSequenceBytes { bytes: usize, remaining: i32 },
    /// `FixedDictDataType::createFromSection`: called with a null section.
    NullSection,
    /// `FixedDictStartStreamElement::fromStreamToSink`: the None indicator
    /// byte was not on the stream.
    MissingNoneIndicator,
    /// `ClassDataType::Field::createDefaultValue`: the field's default
    /// section would not build.
    FieldDefaultFailed { name: String },
    /// `UserDataType`: the `{module}.{instance}` converter is not on this
    /// host. `site` names the reporting function, as in the reference.
    MissingConverter {
        site: &'static str,
        module: String,
        instance: String,
    },
    /// `MailBoxDataType::isSameType`: a mailbox whose IP is still zero.
    UninitialisedMailBox { id: i32 },
    /// A composite meta type, handled outside this module.
    Unsupported(&'static str),
}

impl std::fmt::Display for PropertyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfRange { value, truncated } => writeln!(
                f,
                "IntegerDataType::isSameType: {value} is out of range (truncated = {truncated})."
            ),
            Self::NotUtf8 => writeln!(
                f,
                "UnicodeStringDataType::createFromSection: Could not convert from UTF8."
            ),
            Self::TypeMismatch { meta } => write!(f, "value is not a {meta}"),
            Self::StreamTruncated { element } => {
                writeln!(f, "{element}: Not enough data on stream to read value")
            }
            Self::WrongSequenceSize { size, expected } => writeln!(
                f,
                "SequenceDataType::isSameType: Wrong size {size} instead of {expected}."
            ),
            Self::BadSequenceElement {
                index,
                expected,
                value,
            } => writeln!(
                f,
                "SequenceDataType::isSameType: Bad element {index}. Expected type {expected}, but element was \"{value}\"."
            ),
            Self::FixedSequenceSize { size, expected } => writeln!(
                f,
                "SequenceStartStreamElement::fromSourceToStream: Fixed-length sequence reported size {size}, but datatype expected size {expected}"
            ),
            Self::MissingSequenceSize => writeln!(
                f,
                "SequenceStartStreamElement::fromStreamToSink: Missing size parameter on stream"
            ),
            Self::InvalidSequenceSize { size, remaining } => writeln!(
                f,
                "SequenceStartStreamElement::fromSourceToStream: Invalid size on stream: {size} ({remaining} bytes remaining)"
            ),
            Self::ExpectedSequenceBytes { bytes, remaining } => writeln!(
                f,
                "SequenceStartStreamElement::fromSourceToStream: Expected bytes on stream: {bytes} ({remaining} bytes remaining)"
            ),
            Self::NullSection => {
                writeln!(f, "FixedDictDataType::createFromSection: Section is NULL.")
            }
            Self::MissingNoneIndicator => writeln!(
                f,
                "FixedDictStartStreamElement::fromStreamToSink Missing None/values indicator on stream"
            ),
            Self::FieldDefaultFailed { name } => writeln!(
                f,
                "ClassDataType::Field::createDefaultValue: Failed to create {name} from default section"
            ),
            Self::MissingConverter {
                site,
                module,
                instance,
            } => writeln!(f, "{site}: Do not have {module}.{instance}"),
            Self::UninitialisedMailBox { id } => writeln!(
                f,
                "MailBoxDataType::isSameType: Mailbox for entity {id} has not been fully initialised yet (IP address is zero)"
            ),
            Self::Unsupported(meta) => write!(f, "{meta} values are not supported here"),
        }
    }
}

impl std::error::Error for PropertyError {}

/// `IntegerDataType::isSameType`: truncate `value` to the `bits` two's-complement
/// width of `INT_TYPE` and require the round trip to be exact.
///
/// ```cpp
/// INT_TYPE value = INT_TYPE( intValue );
/// if (intValue != NativeLong( value )) { ERROR_MSG( ... ); return false; }
/// ```
///
/// `NativeLong` is `intptr_t`, so the incoming value is an `i64` either way:
/// every negative value fails for an unsigned width, and every positive value
/// fails once it does not fit. The boundary values are the ones the reference
/// pins in `pyscript/unit_test/test_conversion.cpp::Script_intOverflow`.
pub fn check_integer(value: i64, bytes: i32, signed: bool) -> Result<(), PropertyError> {
    let truncated = truncate_integer(value, bytes, signed);
    if truncated == value {
        Ok(())
    } else {
        Err(PropertyError::OutOfRange { value, truncated })
    }
}

/// The bit width of an integer meta type. `bytes` is one of 1, 2, 4, 8 (the
/// widths the parser accepts), so the width is simply `bytes * 8` bits.
fn integer_bits(bytes: i32) -> u32 {
    assert!(matches!(bytes, 1 | 2 | 4 | 8), "integer byte width {bytes}");
    bytes as u32 * 8
}

/// Truncate `value` to the `bytes`-wide two's complement pattern of `INT_TYPE`.
fn truncate_integer(value: i64, bytes: i32, signed: bool) -> i64 {
    let bits = integer_bits(bytes);
    if bits == 64 {
        return value;
    }
    let raw = value & ((1i64 << bits) - 1);
    if signed {
        // Sign-extend back to 64 bits.
        (raw << (64 - bits)) >> (64 - bits)
    } else {
        raw
    }
}

/// `SequenceStartStreamElement::fromStreamToSink`: the count is read off the
/// stream, so a hostile count must not get to allocate before we have checked
/// the bytes are there. The reference reports the failure out of the read path
/// as "fromSourceToStream" — a copy-paste slip in the original — and we
/// reproduce it verbatim.
fn check_sequence_capacity(
    element_size: i32,
    count: usize,
    remaining: i32,
) -> Result<(), PropertyError> {
    if element_size < 0 {
        // Variable-sized elements: at least one byte per element.
        if remaining < count as i32 {
            return Err(PropertyError::InvalidSequenceSize {
                size: count,
                remaining,
            });
        }
        return Ok(());
    }
    if element_size > 0 && (remaining < 0 || (remaining as usize) / (element_size as usize) < count)
    {
        // Fixed-size elements: compare byte counts without overflowing.
        // remaining < count * elementSize  <=>  remaining / elementSize < count
        // (`remaining` is a non-negative stream position, so `remaining < 0`
        // guards only against an out-of-range caller.)
        return Err(PropertyError::ExpectedSequenceBytes {
            bytes: count * element_size as usize,
            remaining,
        });
    }
    // An element `streamSize()` of 0 needs no guard, as in the reference:
    // neither branch of its guard is taken.
    Ok(())
}

/// `DataType::isSameType` for the scalar meta types.
fn check_value(kind: &Kind, value: &PropertyValue) -> Result<(), PropertyError> {
    match kind {
        Kind::Int { bytes, signed } => {
            let raw = match value {
                PropertyValue::Int(v) => Some(*v),
                PropertyValue::Uint(v) => Some(*v as i64),
                _ => None,
            };
            match raw {
                Some(v) => check_integer(v, *bytes, *signed),
                None => Err(PropertyError::TypeMismatch { meta: "integer" }),
            }
        }
        // `FloatDataType::isSameType` converts through `float`/`double`, which
        // also accepts Python integers.
        Kind::Float32 | Kind::Float64 => match value {
            PropertyValue::Float(_) | PropertyValue::Int(_) | PropertyValue::Uint(_) => Ok(()),
            _ => Err(PropertyError::TypeMismatch { meta: "float" }),
        },
        Kind::String | Kind::UnicodeString => match value {
            PropertyValue::String(_) => Ok(()),
            _ => Err(PropertyError::TypeMismatch { meta: "string" }),
        },
        Kind::Blob => match value {
            PropertyValue::Blob(_) | PropertyValue::String(_) => Ok(()),
            _ => Err(PropertyError::TypeMismatch { meta: "blob" }),
        },
        Kind::Vector { elements } => match value {
            PropertyValue::Vector(v) if v.len() == *elements as usize => Ok(()),
            _ => Err(PropertyError::TypeMismatch { meta: "vector" }),
        },
        // `SequenceDataType::isSameType`: the wrong length fails before any
        // element is inspected, and each element is checked in turn.
        Kind::Sequence { size, element, .. } => {
            let elements = match value {
                PropertyValue::Sequence(elements) => elements,
                _ => return Err(PropertyError::TypeMismatch { meta: "sequence" }),
            };
            if *size != 0 && elements.len() != *size as usize {
                return Err(PropertyError::WrongSequenceSize {
                    size: elements.len() as i32,
                    expected: *size,
                });
            }
            let mut bad = None;
            for (index, element_value) in elements.iter().enumerate() {
                if check_value(element.kind(), element_value).is_err() {
                    bad = Some((index, element_value));
                    break;
                }
            }
            match bad {
                Some((index, element_value)) => Err(PropertyError::BadSequenceElement {
                    index,
                    expected: element.type_name(),
                    value: format!("{element_value:?}"),
                }),
                None => Ok(()),
            }
        }
        // `FixedDictDataType::isSameType`: None only counts when the type
        // allows it, and otherwise every field must carry a value the field's
        // own type accepts. The reference fails silently here.
        Kind::FixedDict {
            allow_none, fields, ..
        } => match value {
            PropertyValue::FixedDict(None) => {
                if *allow_none {
                    Ok(())
                } else {
                    Err(PropertyError::TypeMismatch { meta: "fixed_dict" })
                }
            }
            PropertyValue::FixedDict(Some(values)) => {
                if values.len() != fields.len() {
                    return Err(PropertyError::TypeMismatch { meta: "fixed_dict" });
                }
                for ((name, value), field) in values.iter().zip(fields.iter()) {
                    if name != &field.name || check_value(field.type_.kind(), value).is_err() {
                        return Err(PropertyError::TypeMismatch { meta: "fixed_dict" });
                    }
                }
                Ok(())
            }
            _ => Err(PropertyError::TypeMismatch { meta: "fixed_dict" }),
        },
        // `UserDataType::isSameType` returns `pObject() != NULL`, which is
        // false without a Python host. The reference prints nothing here.
        Kind::UserType { .. } => Err(PropertyError::TypeMismatch { meta: "user_type" }),
        // `MailBoxDataType::isSameType`: Python `None` passes first (there is
        // no None value here), then a mailbox whose IP is still zero is
        // rejected with a warning.
        Kind::MailBox => match value {
            PropertyValue::MailBox { id, ip, .. } => {
                if *ip == 0 {
                    return Err(PropertyError::UninitialisedMailBox { id: *id });
                }
                Ok(())
            }
            _ => Err(PropertyError::TypeMismatch { meta: "mailbox" }),
        },
        _ => Err(PropertyError::Unsupported(kind.meta_name())),
    }
}

/// `DataDescription::pInitialValue`, as an ordered lookup over the two stored
/// slots and then the type default.
fn initial_value_from(
    kind: &Kind,
    sections: [Option<DataSection>; 2],
    default_section: Option<&DataSection>,
) -> Result<PropertyValue, PropertyError> {
    if let Some(section) = sections.into_iter().flatten().next() {
        return value_from_section(kind, &section, default_section);
    }
    Ok(default_value(kind))
}

/// `DataType::createFromSection` for the scalar metas, over the value text of
/// `section` (the reference reads `pSection->as< INT_TYPE >()` and friends on
/// the `<Default>` section itself).
fn value_from_section(
    kind: &Kind,
    section: &DataSection,
    // The `<Default>` section stored on the type being built
    // (`DataType::pDefaultSection_`), which `Field::createDefaultValue`
    // consults for a missing field.
    default_section: Option<&DataSection>,
) -> Result<PropertyValue, PropertyError> {
    match kind {
        Kind::Int { bytes, signed } => Ok(if *signed {
            PropertyValue::Int(match *bytes {
                8 => section.as_long(0),
                _ => section.as_int(0) as i64,
            })
        } else {
            PropertyValue::Uint(match *bytes {
                8 => section.as_uint64(0),
                _ => section.as_uint(0) as u64,
            })
        }),
        Kind::Float32 => Ok(PropertyValue::Float(section.as_float(0.0) as f64)),
        Kind::Float64 => Ok(PropertyValue::Float(section.as_double(0.0))),
        Kind::String => Ok(PropertyValue::String(section.as_string().to_string())),
        // The reference reports the UTF-8 decoding failure by name.
        Kind::UnicodeString => std::str::from_utf8(section.as_string().as_bytes())
            .map(|s| PropertyValue::String(s.to_string()))
            .map_err(|_| PropertyError::NotUtf8),
        Kind::Blob => Ok(PropertyValue::Blob(section.as_blob())),
        Kind::Vector { elements } => {
            let components: Vec<f32> = match *elements {
                2 => {
                    let v = section.as_vector2(Vector2::ZERO);
                    vec![v.x, v.y]
                }
                3 => {
                    let v = section.as_vector3(Vector3::ZERO);
                    vec![v.x, v.y, v.z]
                }
                4 => {
                    let v = section.as_vector4(Vector4::ZERO);
                    vec![v.x, v.y, v.z, v.w]
                }
                _ => unreachable!("vector element count {elements}"),
            };
            Ok(PropertyValue::Vector(components))
        }
        // `SequenceDataType::createFromSection`: one element per `<item>`
        // child, and the element default for any missing trailing ones.
        Kind::Sequence { size, element, .. } => {
            let children = section.count_children();
            let count = if *size == 0 { children } else { *size as usize };
            let mut values = Vec::with_capacity(count);
            for i in 0..count {
                let value = match section.open_child(i) {
                    Some(child) => {
                        value_from_section(element.kind(), child, element.default_section())?
                    }
                    None => default_value(element.kind()),
                };
                values.push(value);
            }
            Ok(PropertyValue::Sequence(values))
        }
        // `FixedDictDataType::createFromSection`: an empty section is the None
        // value when the type allows it, else one value per field, in the type's
        // own field order.
        Kind::FixedDict {
            allow_none, fields, ..
        } => {
            if *allow_none && section.count_children() == 0 {
                return Ok(PropertyValue::FixedDict(None));
            }
            let mut values = Vec::with_capacity(fields.len());
            for field in fields.iter() {
                let value = match section.open_section(&field.name) {
                    Some(child) => value_from_section(
                        field.type_.kind(),
                        child,
                        field.type_.default_section(),
                    )?,
                    // `Field::createDefaultValue`: the field's own section
                    // inside this type's `<Default>`, else the field type's
                    // default.
                    None => match default_section.and_then(|d| d.open_section(&field.name)) {
                        Some(child) => value_from_section(
                            field.type_.kind(),
                            child,
                            field.type_.default_section(),
                        )
                        .map_err(|_| {
                            PropertyError::FieldDefaultFailed {
                                name: field.name.clone(),
                            }
                        })?,
                        None => field.type_.default_value()?,
                    },
                };
                values.push((field.name.clone(), value));
            }
            Ok(PropertyValue::FixedDict(Some(values)))
        }
        // `UserDataType::createFromSection` bounces to
        // `createObjectFromSection`, which needs the Python converter.
        Kind::UserType { module, instance } => Err(PropertyError::MissingConverter {
            site: USER_CREATE_FROM_SECTION,
            module: module.clone(),
            instance: instance.clone(),
        }),
        // `MailBoxDataType::createFromSection`: the `id`, `ip` and `etc`
        // children, where `etc` packs the port and the salt as one uint32.
        Kind::MailBox => {
            let read = |name| section.open_section(name);
            let etc = read("etc").map_or(0, |child| child.as_uint(0));
            Ok(PropertyValue::MailBox {
                id: read("id").map_or(0, |child| child.as_int(0)),
                ip: read("ip").map_or(0, |child| child.as_uint(0)),
                port: etc as u16,
                salt: (etc >> 16) as u16,
            })
        }
        _ => Err(PropertyError::Unsupported(kind.meta_name())),
    }
}

/// `DataType::getDefaultValue`: the value `setDefaultValue` stored when the
/// type was built, else 0, 0.0, `""`, an empty blob or a zero vector.
fn default_value(kind: &Kind) -> PropertyValue {
    match kind {
        Kind::Int { signed: true, .. } => PropertyValue::Int(0),
        Kind::Int { signed: false, .. } => PropertyValue::Uint(0),
        Kind::Float32 | Kind::Float64 => PropertyValue::Float(0.0),
        Kind::String | Kind::UnicodeString => PropertyValue::String(String::new()),
        Kind::Blob => PropertyValue::Blob(Vec::new()),
        Kind::Vector { elements } => PropertyValue::Vector(vec![0.0; *elements as usize]),
        // `createDefaultValue`: `size` element defaults — so a variable-length
        // sequence (size 0) defaults to the empty sequence.
        Kind::Sequence { size, element, .. } => PropertyValue::Sequence(
            (0..*size as usize)
                .map(|_| default_value(element.kind()))
                .collect(),
        ),
        // `FixedDictDataType::getDefaultValue`: None when the type allows it,
        // else one default per field, in field order.
        Kind::FixedDict {
            allow_none, fields, ..
        } => {
            if *allow_none {
                PropertyValue::FixedDict(None)
            } else {
                PropertyValue::FixedDict(Some(
                    fields
                        .iter()
                        .map(|field| (field.name.clone(), default_value(field.type_.kind())))
                        .collect(),
                ))
            }
        }
        // `MailBoxDataType::getDefaultValue`: a zeroed `EntityMailBoxRef`,
        // which is `Address::NONE`.
        Kind::MailBox => PropertyValue::MailBox {
            id: 0,
            ip: 0,
            port: 0,
            salt: 0,
        },
        _ => PropertyValue::Int(0),
    }
}

impl DataType {
    /// `DataType::isSameType` for a scalar type.
    pub fn check_value(&self, value: &PropertyValue) -> Result<(), PropertyError> {
        check_value(self.kind(), value)
    }

    /// `DataType::default_value`: the type default, honouring a `<Default>`
    /// child of the `<Type>` section (`setDefaultValue`).
    pub fn default_value(&self) -> Result<PropertyValue, PropertyError> {
        match self.default_section() {
            Some(section) => value_from_section(self.kind(), section, Some(section)),
            None => Ok(default_value(self.kind())),
        }
    }

    /// `DataDescription::pInitialValue` on a type alone: the type default.
    pub fn initial_value(&self) -> Result<PropertyValue, PropertyError> {
        self.default_value()
    }

    /// `DataType::createFromSection`.
    pub fn value_from_section(
        &self,
        section: &DataSection,
    ) -> Result<PropertyValue, PropertyError> {
        value_from_section(self.kind(), section, self.default_section())
    }

    /// `DataType::addToStream` for the scalar metas.
    pub fn add_to_stream(
        &self,
        value: &PropertyValue,
        stream: &mut BinaryStream,
    ) -> Result<(), PropertyError> {
        add_to_stream(self.kind(), value, stream, false)
    }

    /// `DataType::addToStream` with `isPersistentOnly`: the database
    /// projection, carrying only the `<Persistent>` fields.
    pub fn add_persistent_to_stream(
        &self,
        value: &PropertyValue,
        stream: &mut BinaryStream,
    ) -> Result<(), PropertyError> {
        add_to_stream(self.kind(), value, stream, true)
    }

    /// `DataType::createFromStream` for the scalar metas.
    pub fn create_from_stream(
        &self,
        stream: &mut BinaryStream,
    ) -> Result<PropertyValue, PropertyError> {
        create_from_stream(self.kind(), stream, false)
    }

    /// `DataType::createFromStream` with `isPersistentOnly`: reads the
    /// database projection back, taking the fields it does not carry from
    /// their defaults.
    pub fn create_persistent_from_stream(
        &self,
        stream: &mut BinaryStream,
    ) -> Result<PropertyValue, PropertyError> {
        create_from_stream(self.kind(), stream, true)
    }
}

impl DataDescription {
    /// `DataDescription::pInitialValue`: a `<Default>` on a const type
    /// (materialised while parsing), then a `<Default>` on a non-const type,
    /// then the type default.
    pub fn initial_value(&self) -> Result<PropertyValue, PropertyError> {
        initial_value_from(
            self.data_type.kind(),
            [
                self.initial_value_section.clone(),
                self.default_section.clone(),
            ],
            self.data_type.default_section(),
        )
    }
}

/// `DataType::addToStream` for the scalar metas, one `StreamElement` each.
pub(crate) fn add_to_stream(
    kind: &Kind,
    value: &PropertyValue,
    stream: &mut BinaryStream,
    persistent_only: bool,
) -> Result<(), PropertyError> {
    match kind {
        Kind::Int { bytes, .. } => match value {
            PropertyValue::Int(v) => write_int(stream, *bytes, *v as u64),
            PropertyValue::Uint(v) => write_int(stream, *bytes, *v),
            _ => return Err(PropertyError::TypeMismatch { meta: "integer" }),
        },
        Kind::Float32 => match value {
            PropertyValue::Float(v) => {
                let _ = stream.write_f32(*v as f32);
            }
            _ => return Err(PropertyError::TypeMismatch { meta: "float" }),
        },
        Kind::Float64 => match value {
            PropertyValue::Float(v) => {
                let _ = stream.write_f64(*v);
            }
            _ => return Err(PropertyError::TypeMismatch { meta: "float" }),
        },
        Kind::String | Kind::UnicodeString => match value {
            PropertyValue::String(text) => {
                let _ = stream
                    .write_string(text)
                    .map_err(|_| PropertyError::StreamTruncated {
                        element: SIMPLE_STREAM_ELEMENT,
                    })?;
            }
            _ => return Err(PropertyError::TypeMismatch { meta: "string" }),
        },
        Kind::Blob => match value {
            PropertyValue::Blob(bytes) => {
                let _ = stream
                    .write_blob(bytes)
                    .map_err(|_| PropertyError::StreamTruncated {
                        element: BLOB_STREAM_ELEMENT,
                    })?;
            }
            _ => return Err(PropertyError::TypeMismatch { meta: "blob" }),
        },
        Kind::Vector { .. } => match value {
            PropertyValue::Vector(components) => {
                for component in components.iter() {
                    let _ = stream.write_f32(*component);
                }
            }
            _ => return Err(PropertyError::TypeMismatch { meta: "vector" }),
        },
        // `SequenceStartStreamElement::fromSourceToStream`. A fixed-length
        // sequence writes **no count prefix** — the count is implicit — while a
        // variable one prefixes a packed int.
        Kind::Sequence { size, element, .. } => match value {
            PropertyValue::Sequence(elements) => {
                if *size != 0 && elements.len() != *size as usize {
                    return Err(PropertyError::FixedSequenceSize {
                        size: elements.len() as i32,
                        expected: *size,
                    });
                }
                if *size == 0 {
                    let _ = stream
                        .write_packed_int(elements.len() as u32)
                        .map_err(|_| PropertyError::StreamTruncated {
                            element: SIMPLE_STREAM_ELEMENT,
                        })?;
                }
                for element_value in elements.iter() {
                    add_to_stream(element.kind(), element_value, stream, persistent_only)?;
                }
            }
            _ => return Err(PropertyError::TypeMismatch { meta: "sequence" }),
        },
        // `FixedDictStartStreamElement::fromSourceToStream`: one byte naming
        // the None state when the type allows it, then every field in the
        // type's own order. The enter/leave/end elements write nothing.
        Kind::FixedDict {
            allow_none, fields, ..
        } => match value {
            PropertyValue::FixedDict(None) => {
                if !*allow_none {
                    return Err(PropertyError::TypeMismatch { meta: "fixed_dict" });
                }
                // `uint8( isNone_ ? 0 : 1 )` — 0 means None.
                let _ = stream.write_u8(0);
            }
            PropertyValue::FixedDict(Some(values)) => {
                if *allow_none {
                    let _ = stream.write_u8(1);
                }
                for (position, field) in fields.iter().enumerate() {
                    if persistent_only && !field.is_persistent {
                        continue;
                    }
                    add_to_stream(
                        field.type_.kind(),
                        &values[position].1,
                        stream,
                        persistent_only,
                    )?;
                }
            }
            _ => return Err(PropertyError::TypeMismatch { meta: "fixed_dict" }),
        },
        // `UserDataType::addObjectToStream` writes an empty string before
        // reporting the missing converter.
        Kind::UserType { module, instance } => {
            let _ = stream.write_string("");
            return Err(PropertyError::MissingConverter {
                site: USER_ADD_TO_STREAM,
                module: module.clone(),
                instance: instance.clone(),
            });
        }
        // `MailBoxStreamElement` is a `SimpleStreamElement<EntityMailBoxRef>`,
        // so the raw bytes of the ref, little-endian on this host.
        Kind::MailBox => match value {
            PropertyValue::MailBox { id, ip, port, salt } => {
                let _ = stream.write_i32(*id);
                let _ = stream.write_u32(*ip);
                let _ = stream.write_u16(*port);
                let _ = stream.write_u16(*salt);
            }
            _ => return Err(PropertyError::TypeMismatch { meta: "mailbox" }),
        },
        _ => return Err(PropertyError::Unsupported(kind.meta_name())),
    }
    Ok(())
}

/// `DataType::createFromStream` for the scalar metas.
pub(crate) fn create_from_stream(
    kind: &Kind,
    stream: &mut BinaryStream,
    persistent_only: bool,
) -> Result<PropertyValue, PropertyError> {
    let truncated = || PropertyError::StreamTruncated {
        element: SIMPLE_STREAM_ELEMENT,
    };
    match kind {
        Kind::Int { bytes, signed } => {
            let raw = read_integer(stream, *bytes, &truncated)?;
            // `stream >> value` reads into an `INT_TYPE`, so a signed read is sign
            // extended back to `NativeLong`.
            Ok(if *signed {
                PropertyValue::Int(truncate_integer(raw as i64, *bytes, true))
            } else {
                PropertyValue::Uint(raw)
            })
        }
        Kind::Float32 => {
            let v = stream.read_f32().map_err(|_| truncated())?;
            Ok(PropertyValue::Float(v as f64))
        }
        Kind::Float64 => {
            let v = stream.read_f64().map_err(|_| truncated())?;
            Ok(PropertyValue::Float(v))
        }
        Kind::String | Kind::UnicodeString => {
            let text = stream.read_string().map_err(|_| truncated())?;
            Ok(PropertyValue::String(text))
        }
        Kind::Blob => stream
            .read_blob()
            .map_err(|_| PropertyError::StreamTruncated {
                element: BLOB_STREAM_ELEMENT,
            })
            .map(PropertyValue::Blob),
        // `SequenceStartStreamElement::fromStreamToSink`.
        Kind::Sequence { size, element, .. } => {
            let count = if *size == 0 {
                stream
                    .read_packed_int()
                    .map_err(|_| PropertyError::MissingSequenceSize)? as usize
            } else {
                *size as usize
            };
            let element_size = element.stream_size();
            check_sequence_capacity(element_size, count, stream.remaining() as i32)?;
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                values.push(create_from_stream(element.kind(), stream, persistent_only)?);
            }
            Ok(PropertyValue::Sequence(values))
        }
        Kind::Vector { elements } => {
            let mut components = Vec::with_capacity(*elements as usize);
            for _ in 0..*elements {
                components.push(stream.read_f32().map_err(|_| truncated())?);
            }
            Ok(PropertyValue::Vector(components))
        }
        // `FixedDictStartStreamElement::fromStreamToSink`: the None indicator,
        // then every field in the type's own order.
        Kind::FixedDict {
            allow_none, fields, ..
        } => {
            let is_none = if *allow_none {
                let has_values = stream
                    .read_u8()
                    .map_err(|_| PropertyError::MissingNoneIndicator)?;
                has_values == 0
            } else {
                false
            };
            if is_none {
                return Ok(PropertyValue::FixedDict(None));
            }
            let mut values = Vec::with_capacity(fields.len());
            for field in fields.iter() {
                let value = if persistent_only && !field.is_persistent {
                    // `createDefaultValue`: the field is absent from the
                    // projection, so the type default stands in for it.
                    default_value(field.type_.kind())
                } else {
                    create_from_stream(field.type_.kind(), stream, persistent_only)?
                };
                values.push((field.name.clone(), value));
            }
            Ok(PropertyValue::FixedDict(Some(values)))
        }
        // `UserDataType::createObjectFromStream` consumes the string first,
        // even when the converter is missing.
        Kind::UserType { module, instance } => {
            let _ = stream.read_string().map_err(|_| truncated())?;
            Err(PropertyError::MissingConverter {
                site: USER_CREATE_FROM_STREAM,
                module: module.clone(),
                instance: instance.clone(),
            })
        }
        Kind::MailBox => {
            let id = stream.read_i32().map_err(|_| truncated())?;
            let ip = stream.read_u32().map_err(|_| truncated())?;
            let port = stream.read_u16().map_err(|_| truncated())?;
            let salt = stream.read_u16().map_err(|_| truncated())?;
            Ok(PropertyValue::MailBox { id, ip, port, salt })
        }
        _ => Err(PropertyError::Unsupported(kind.meta_name())),
    }
}

/// Write an integer of `bytes` width, little-endian two's complement: the low
/// `bytes` bytes of the 64-bit pattern, which is exactly what the fixed-width
/// reference writers emit.
fn write_int(stream: &mut BinaryStream, bytes: i32, bits: u64) {
    assert!(bytes > 0 && bytes <= 8, "integer byte width {bytes}");
    let mut pattern = [0u8; 8];
    pattern[..bytes as usize].copy_from_slice(&bits.to_le_bytes()[..bytes as usize]);
    let _ = stream.write_bytes(&pattern[..bytes as usize]);
}

/// `read` of a fixed-width integer: the low `bytes` bytes, little-endian.
fn read_integer(
    stream: &mut BinaryStream,
    bytes: i32,
    truncated: &dyn Fn() -> PropertyError,
) -> Result<u64, PropertyError> {
    assert!(bytes > 0 && bytes <= 8, "integer byte width {bytes}");
    let width = bytes as usize;
    let mut pattern = [0u8; 8];
    pattern[..width].copy_from_slice(&stream.read_blob_raw(width).map_err(|_| truncated())?);
    Ok(u64::from_le_bytes(pattern))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_xml(xml: &str) -> DataSection {
        resources::datasection::parse("test", xml.as_bytes()).expect("valid fixture xml")
    }

    /// Build a `DataType` out of a `<meta> NAME ... </meta>` section.
    fn build_type(xml: &str) -> DataType {
        let mut diag = crate::diagnostics::Diagnostics::new();
        let ctx = crate::data_type::TypeContext::default();
        let section = parse_xml(xml);
        let type_ = crate::data_type::build_type(&ctx, Some(&section), &mut diag);
        assert!(type_.is_some(), "{xml} builds ({:?})", diag.errors());
        type_.unwrap()
    }

    #[test]
    fn type_defaults_follow_the_reference_order() {
        // No <Default>: the type default.
        assert_eq!(
            build_type("<a> INT32 </a>").initial_value(),
            Ok(PropertyValue::Int(0))
        );
        assert_eq!(
            build_type("<a> FLOAT64 </a>").initial_value(),
            Ok(PropertyValue::Float(0.0))
        );
        assert_eq!(
            build_type("<a> STRING </a>").initial_value(),
            Ok(PropertyValue::String(String::new()))
        );
        assert_eq!(
            build_type("<a> BLOB </a>").initial_value(),
            Ok(PropertyValue::Blob(Vec::new()))
        );
        assert_eq!(
            build_type("<a> VECTOR3 </a>").initial_value(),
            Ok(PropertyValue::Vector(vec![0.0, 0.0, 0.0]))
        );

        // A <Default> on the <Type> section is read by setDefaultValue.
        assert_eq!(
            build_type("<a> INT32 <Default> 10 </Default> </a>").initial_value(),
            Ok(PropertyValue::Int(10))
        );
        assert_eq!(
            build_type("<a> INT64 <Default> 9223372036854775807 </Default> </a>").initial_value(),
            Ok(PropertyValue::Int(i64::MAX))
        );
        assert_eq!(
            build_type("<a> UINT8 <Default> 200 </Default> </a>").initial_value(),
            Ok(PropertyValue::Uint(200))
        );
        assert_eq!(
            build_type("<a> FLOAT32 <Default> 1.5 </Default> </a>").initial_value(),
            Ok(PropertyValue::Float(1.5))
        );
        assert_eq!(
            build_type("<a> STRING <Default> hi </Default> </a>").initial_value(),
            Ok(PropertyValue::String("hi".into()))
        );
        assert_eq!(
            build_type("<a> VECTOR2 <Default> 1 2 </Default> </a>").initial_value(),
            Ok(PropertyValue::Vector(vec![1.0, 2.0]))
        );

        // A <Default> wider than the type is silently truncated, not an error.
        assert_eq!(
            build_type("<a> INT32 <Default> 4294967296 </Default> </a>").initial_value(),
            Ok(PropertyValue::Int(0))
        );
    }

    #[test]
    fn integer_boundaries_match_the_reference_check() {
        let int32 = build_type("<a> INT32 </a>");
        let uint8 = build_type("<a> UINT8 </a>");

        // The Script_intOverflow values (test_conversion.cpp).
        for value in [0, 1, -1, i32::MAX as i64] {
            assert_eq!(
                int32.check_value(&PropertyValue::Int(value)),
                Ok(()),
                "{value}"
            );
        }
        for value in [i32::MIN as i64 - 1, i32::MAX as i64 + 1, 0x8000_0000i64] {
            assert_eq!(
                int32.check_value(&PropertyValue::Int(value)),
                Err(PropertyError::OutOfRange {
                    value,
                    truncated: truncate_integer(value, 4, true),
                }),
                "{value}"
            );
        }

        // UINT8: 0..=255, and every negative value fails.
        assert_eq!(uint8.check_value(&PropertyValue::Int(255)), Ok(()));
        assert_eq!(
            uint8.check_value(&PropertyValue::Int(256)),
            Err(PropertyError::OutOfRange {
                value: 256,
                truncated: 0
            })
        );
        assert_eq!(
            uint8.check_value(&PropertyValue::Int(-1)),
            Err(PropertyError::OutOfRange {
                value: -1,
                truncated: 255
            })
        );
    }

    #[test]
    fn check_value_takes_the_same_payloads_the_reference_does() {
        let int16 = build_type("<a> INT16 </a>");
        let uint32 = build_type("<a> UINT32 </a>");
        let float32 = build_type("<a> FLOAT32 </a>");
        let string = build_type("<a> STRING </a>");
        let blob = build_type("<a> BLOB </a>");
        let vector3 = build_type("<a> VECTOR3 </a>");

        for (type_, value) in [
            (&int16, PropertyValue::Int(1)),
            (&uint32, PropertyValue::Int(1)),
            (&float32, PropertyValue::Int(1)),
            (&string, PropertyValue::String("a".into())),
            (&blob, PropertyValue::String("a".into())),
            (&vector3, PropertyValue::Vector(vec![0.0; 3])),
        ] {
            assert_eq!(type_.check_value(&value), Ok(()));
        }

        for (type_, value) in [
            (&int16, PropertyValue::Float(1.0)),
            (&float32, PropertyValue::Blob(vec![1])),
            (&string, PropertyValue::Int(1)),
            (&blob, PropertyValue::Int(1)),
            // The element count is part of the meta name.
            (&vector3, PropertyValue::Vector(vec![0.0; 4])),
        ] {
            assert!(
                type_.check_value(&value).is_err(),
                "{type_:?} must refuse {value:?}"
            );
        }
    }

    #[test]
    fn an_integer_type_checks_a_uint_payload_and_vice_versa() {
        let int32 = build_type("<a> INT32 </a>");
        assert_eq!(int32.check_value(&PropertyValue::Uint(42)), Ok(()));
        assert_eq!(
            int32.check_value(&PropertyValue::Uint(u32::MAX as u64 + 1)),
            Err(PropertyError::OutOfRange {
                value: u32::MAX as u64 as i64 + 1,
                truncated: (u32::MAX as u64 as i64 + 1) & 0x7fffffff,
            })
        );
    }

    #[test]
    fn a_defaultless_unsigned_type_defaults_to_zero() {
        assert_eq!(
            build_type("<a> UINT8 </a>").initial_value(),
            Ok(PropertyValue::Uint(0))
        );
    }

    #[test]
    fn value_from_section_reads_the_section_of_the_type() {
        let uint32 = build_type("<a> UINT32 </a>");
        assert_eq!(
            uint32.value_from_section(&parse_xml("<v> 7 </v>")),
            Ok(PropertyValue::Uint(7))
        );
        let float64 = build_type("<a> FLOAT64 </a>");
        assert_eq!(
            float64.value_from_section(&parse_xml("<v> 2.5 </v>")),
            Ok(PropertyValue::Float(2.5))
        );
    }

    #[test]
    fn a_sequence_reads_its_item_sections_and_fills_the_gaps() {
        // Two items read, the third missing, so it takes the element default.
        let array = build_type("<a> ARRAY <size> 3 </size> <of> INT8 </of> </a>");
        let section = parse_xml("<v> <item> 1 </item> <item> 2 </item> </v>");
        assert_eq!(
            array.value_from_section(&section),
            Ok(PropertyValue::Sequence(vec![
                PropertyValue::Int(1),
                PropertyValue::Int(2),
                PropertyValue::Int(0),
            ]))
        );
    }

    #[test]
    fn a_sequence_default_has_one_default_element_per_slot() {
        // createDefaultValue: `size` element defaults, so a variable-length
        // sequence defaults to the empty sequence.
        assert_eq!(
            build_type("<a> ARRAY <of> INT8 </of> </a>").initial_value(),
            Ok(PropertyValue::Sequence(Vec::new()))
        );
        assert_eq!(
            build_type("<a> TUPLE <size> 2 </size> <of> INT8 </of> </a>").initial_value(),
            Ok(PropertyValue::Sequence(vec![PropertyValue::Int(0); 2]))
        );
    }

    #[test]
    fn a_sequence_type_rejects_a_wrong_length_or_element() {
        let array = build_type("<a> ARRAY <size> 2 </size> <of> INT8 </of> </a>");

        assert_eq!(
            array.check_value(&PropertyValue::Sequence(vec![
                PropertyValue::Int(1),
                PropertyValue::Int(2),
            ])),
            Ok(())
        );
        assert_eq!(
            array.check_value(&PropertyValue::Sequence(vec![PropertyValue::Int(1)])),
            Err(PropertyError::WrongSequenceSize {
                size: 1,
                expected: 2
            })
        );
        // INT8's isSameType rejects 200, so the element is named in the message.
        assert_eq!(
            array.check_value(&PropertyValue::Sequence(vec![
                PropertyValue::Int(1),
                PropertyValue::Int(200),
            ])),
            Err(PropertyError::BadSequenceElement {
                index: 1,
                // The element's own meta name (DataType::typeName), not the
                // sequence's "ARRAY size 2 of (...)" form.
                expected: "INT8".to_string(),
                value: "Int(200)".to_string(),
            })
        );

        // A non-sequence is not a sequence type at all.
        assert_eq!(
            array.check_value(&PropertyValue::Int(1)),
            Err(PropertyError::TypeMismatch { meta: "sequence" })
        );
    }

    #[test]
    fn a_fixed_length_sequence_has_no_count_prefix() {
        // SequenceStartStreamElement writes no prefix for a fixed size.
        let tuple = build_type("<a> TUPLE <size> 2 </size> <of> INT8 </of> </a>");
        let mut stream = BinaryStream::new();
        tuple
            .add_to_stream(
                &PropertyValue::Sequence(vec![PropertyValue::Int(1), PropertyValue::Int(2)]),
                &mut stream,
            )
            .unwrap();
        assert_eq!(stream.as_bytes(), &[1, 2]);

        let back = tuple.create_from_stream(&mut stream).unwrap();
        assert_eq!(
            back,
            PropertyValue::Sequence(vec![PropertyValue::Int(1), PropertyValue::Int(2)])
        );

        // A variable-length sequence does write its count as a packed int.
        let array = build_type("<a> ARRAY <of> INT8 </of> </a>");
        let mut stream = BinaryStream::new();
        array
            .add_to_stream(
                &PropertyValue::Sequence(vec![PropertyValue::Int(1), PropertyValue::Int(2)]),
                &mut stream,
            )
            .unwrap();
        assert_eq!(stream.as_bytes(), &[2, 1, 2]);
        assert_eq!(
            array.create_from_stream(&mut stream).unwrap(),
            PropertyValue::Sequence(vec![PropertyValue::Int(1), PropertyValue::Int(2)])
        );
    }

    #[test]
    fn a_sequence_rejects_a_length_that_does_not_fit_the_stream() {
        let array = build_type("<a> ARRAY <of> INT8 </of> </a>");
        let mut stream = BinaryStream::new();
        // Claim 100 elements but write only two bytes.
        let _ = stream.write_packed_int(100);
        let _ = stream.write_bytes(&[1, 2]);
        assert_eq!(
            array.create_from_stream(&mut stream),
            Err(PropertyError::ExpectedSequenceBytes {
                bytes: 100,
                remaining: 2
            })
        );

        // A variable-sized element needs at least one byte per element.
        let strings = build_type("<a> ARRAY <of> STRING </of> </a>");
        let mut stream = BinaryStream::new();
        let _ = stream.write_packed_int(3);
        assert_eq!(
            strings.create_from_stream(&mut stream),
            Err(PropertyError::InvalidSequenceSize {
                size: 3,
                remaining: 0
            })
        );
    }

    #[test]
    fn a_fixed_sequence_rejects_a_value_of_the_wrong_length() {
        let tuple = build_type("<a> TUPLE <size> 2 </size> <of> INT8 </of> </a>");
        let mut stream = BinaryStream::new();
        assert_eq!(
            tuple.add_to_stream(
                &PropertyValue::Sequence(vec![
                    PropertyValue::Int(1),
                    PropertyValue::Int(2),
                    PropertyValue::Int(3),
                ]),
                &mut stream,
            ),
            Err(PropertyError::FixedSequenceSize {
                size: 3,
                expected: 2
            })
        );
        // Nothing was written: the reference bails before the payload.
        assert_eq!(stream.as_bytes(), &[]);

        // A non-sequence is not a sequence value at all.
        assert_eq!(
            tuple.add_to_stream(&PropertyValue::Int(1), &mut stream),
            Err(PropertyError::TypeMismatch { meta: "sequence" })
        );
    }

    #[test]
    fn a_variable_element_sequence_round_trips_byte_exact() {
        // ARRAY of STRING: both the sequence and its elements are variable
        // length, so the packed count is written and the per-byte guard applies.
        let array = build_type("<a> ARRAY <of> STRING </of> </a>");
        let value = PropertyValue::Sequence(vec![
            PropertyValue::String("a".into()),
            PropertyValue::String("bc".into()),
        ]);
        let mut stream = BinaryStream::new();
        array.add_to_stream(&value, &mut stream).unwrap();
        assert_eq!(stream.as_bytes(), &[2, 1, b'a', 2, b'b', b'c']);
        assert_eq!(array.create_from_stream(&mut stream).unwrap(), value);
    }

    #[test]
    fn a_variable_sequence_missing_its_count_reports_the_reference_message() {
        // An empty stream: the packed count itself is missing.
        let array = build_type("<a> ARRAY <of> INT8 </of> </a>");
        let mut stream = BinaryStream::new();
        assert_eq!(
            array.create_from_stream(&mut stream),
            Err(PropertyError::MissingSequenceSize)
        );
    }

    #[test]
    fn a_nested_sequence_round_trips_byte_exact() {
        // ARRAY of (TUPLE of 2 INT16s), both variable length.
        let array = build_type("<a> ARRAY <of> TUPLE <size> 2 </size> <of> INT16 </of> </of> </a>");
        let value = PropertyValue::Sequence(vec![
            PropertyValue::Sequence(vec![PropertyValue::Int(1), PropertyValue::Int(2)]),
            PropertyValue::Sequence(vec![PropertyValue::Int(3), PropertyValue::Int(4)]),
        ]);
        let mut stream = BinaryStream::new();
        array.add_to_stream(&value, &mut stream).unwrap();
        assert_eq!(stream.as_bytes(), &[2, 1, 0, 2, 0, 3, 0, 4, 0]);
        assert_eq!(array.create_from_stream(&mut stream).unwrap(), value);
    }

    fn fixed_dict_type() -> DataType {
        build_type(
            "<a> FIXED_DICT <Properties> \
             <ammo> <Type> UINT8 </Type> </ammo> \
             <label> <Type> STRING </Type> </label> \
             </Properties> </a>",
        )
    }

    fn fixed_dict_value() -> PropertyValue {
        PropertyValue::FixedDict(Some(vec![
            ("ammo".to_string(), PropertyValue::Uint(7)),
            ("label".to_string(), PropertyValue::String("x".into())),
        ]))
    }

    #[test]
    fn a_fixed_dict_round_trips_its_fields_in_order() {
        // No <AllowNone>: no indicator byte, and the fields follow the type's
        // own field order — `FixedDictEnterFieldStreamElement` writes nothing.
        let dict = fixed_dict_type();
        let value = fixed_dict_value();
        let mut stream = BinaryStream::new();
        dict.add_to_stream(&value, &mut stream).unwrap();
        assert_eq!(stream.as_bytes(), &[7, 1, b'x']);
        assert_eq!(dict.create_from_stream(&mut stream).unwrap(), value);
    }

    fn persistent_dict_type() -> DataType {
        build_type(
            "<a> FIXED_DICT <Properties>              <ammo> <Type> UINT8 </Type> </ammo>              <temp> <Type> UINT8 </Type> <Persistent> false </Persistent> </temp>              <label> <Type> STRING </Type> </label>              </Properties> </a>",
        )
    }

    fn persistent_dict_value() -> PropertyValue {
        PropertyValue::FixedDict(Some(vec![
            ("ammo".to_string(), PropertyValue::Uint(7)),
            ("temp".to_string(), PropertyValue::Uint(99)),
            ("label".to_string(), PropertyValue::String("x".into())),
        ]))
    }

    #[test]
    fn the_persistent_stream_carries_only_the_persistent_fields() {
        // `FixedDictDataType::addInstanceToStream` skips a field when
        // `isPersistentOnly` is set and the field is not `<Persistent>`; the
        // enter/leave/end stream elements write nothing either way.
        let dict = persistent_dict_type();
        let mut stream = BinaryStream::new();
        dict.add_persistent_to_stream(&persistent_dict_value(), &mut stream)
            .unwrap();
        assert_eq!(stream.as_bytes(), &[7, 1, b'x']);

        let mut full = BinaryStream::new();
        dict.add_to_stream(&persistent_dict_value(), &mut full)
            .unwrap();
        assert_eq!(full.as_bytes(), &[7, 99, 1, b'x']);
    }

    #[test]
    fn a_persistent_read_fills_the_non_persistent_fields_from_their_defaults() {
        // `FixedDictDataType::createInstanceFromStream` takes the
        // non-persistent fields from `createDefaultValue` instead of the
        // stream.
        let dict = persistent_dict_type();
        let mut stream = BinaryStream::new();
        dict.add_persistent_to_stream(&persistent_dict_value(), &mut stream)
            .unwrap();
        assert_eq!(
            dict.create_persistent_from_stream(&mut stream).unwrap(),
            PropertyValue::FixedDict(Some(vec![
                ("ammo".to_string(), PropertyValue::Uint(7)),
                (
                    "temp".to_string(),
                    dict.field_type(1).unwrap().default_value().unwrap(),
                ),
                ("label".to_string(), PropertyValue::String("x".into())),
            ]))
        );
    }

    #[test]
    fn an_allow_none_fixed_dict_writes_its_none_indicator() {
        let dict = build_type(
            "<a> FIXED_DICT <AllowNone> true </AllowNone> <Properties> \
             <ammo> <Type> UINT8 </Type> </ammo> \
             </Properties> </a>",
        );
        // `uint8( isNone_ ? 0 : 1 )` — 0 names the None value.
        let mut stream = BinaryStream::new();
        dict.add_to_stream(&PropertyValue::FixedDict(None), &mut stream)
            .unwrap();
        assert_eq!(stream.as_bytes(), &[0]);
        assert_eq!(
            dict.create_from_stream(&mut stream).unwrap(),
            PropertyValue::FixedDict(None)
        );

        stream = BinaryStream::new();
        let value =
            PropertyValue::FixedDict(Some(vec![("ammo".to_string(), PropertyValue::Uint(9))]));
        dict.add_to_stream(&value, &mut stream).unwrap();
        assert_eq!(stream.as_bytes(), &[1, 9]);
        assert_eq!(dict.create_from_stream(&mut stream).unwrap(), value);
    }

    #[test]
    fn a_fixed_dict_default_depends_on_allow_none() {
        // `getDefaultValue`: None when the type allows it, else one default
        // per field, in field order.
        assert_eq!(
            build_type(
                "<a> FIXED_DICT <AllowNone> true </AllowNone> <Properties> \
                 <ammo> <Type> UINT8 </Type> </ammo> \
                 </Properties> </a>"
            )
            .initial_value(),
            Ok(PropertyValue::FixedDict(None))
        );
        assert_eq!(
            fixed_dict_type().initial_value(),
            Ok(PropertyValue::FixedDict(Some(vec![
                ("ammo".to_string(), PropertyValue::Uint(0)),
                ("label".to_string(), PropertyValue::String(String::new())),
            ])))
        );
    }

    #[test]
    fn a_fixed_dict_reads_its_field_sections_and_fills_the_gaps() {
        // Only `ammo` is present, so `label` takes the field type's default.
        let dict = fixed_dict_type();
        let section = parse_xml("<v> <ammo> 5 </ammo> </v>");
        assert_eq!(
            dict.value_from_section(&section),
            Ok(PropertyValue::FixedDict(Some(vec![
                ("ammo".to_string(), PropertyValue::Uint(5)),
                ("label".to_string(), PropertyValue::String(String::new())),
            ])))
        );
    }

    #[test]
    fn an_empty_section_is_the_none_value_of_an_allow_none_dict() {
        let dict = build_type(
            "<a> FIXED_DICT <AllowNone> true </AllowNone> <Properties> \
             <ammo> <Type> UINT8 </Type> </ammo> \
             </Properties> </a>",
        );
        // `createFromSection`: `countChildren() == 0` means None.
        assert_eq!(
            dict.value_from_section(&parse_xml("<v> </v>")),
            Ok(PropertyValue::FixedDict(None))
        );
    }

    #[test]
    fn a_fixed_dict_rejects_a_wrong_field_set() {
        let dict = fixed_dict_type();
        assert_eq!(dict.check_value(&fixed_dict_value()), Ok(()));

        // Too few fields, and a field the type does not have.
        assert_eq!(
            dict.check_value(&PropertyValue::FixedDict(Some(vec![(
                "ammo".to_string(),
                PropertyValue::Uint(7),
            )]))),
            Err(PropertyError::TypeMismatch { meta: "fixed_dict" })
        );
        assert_eq!(
            dict.check_value(&PropertyValue::FixedDict(Some(vec![
                ("ammo".to_string(), PropertyValue::Uint(7)),
                ("other".to_string(), PropertyValue::String("x".into())),
            ]))),
            Err(PropertyError::TypeMismatch { meta: "fixed_dict" })
        );
        // UINT8 rejects 300.
        assert_eq!(
            dict.check_value(&PropertyValue::FixedDict(Some(vec![
                ("ammo".to_string(), PropertyValue::Uint(300)),
                ("label".to_string(), PropertyValue::String("x".into())),
            ]))),
            Err(PropertyError::TypeMismatch { meta: "fixed_dict" })
        );
        // None is only a value of a type that allows it, and a non-dictionary
        // is not a dictionary at all.
        assert_eq!(
            dict.check_value(&PropertyValue::FixedDict(None)),
            Err(PropertyError::TypeMismatch { meta: "fixed_dict" })
        );
        assert_eq!(
            dict.check_value(&PropertyValue::Int(1)),
            Err(PropertyError::TypeMismatch { meta: "fixed_dict" })
        );
        let mut stream = BinaryStream::new();
        assert_eq!(
            dict.add_to_stream(&PropertyValue::Int(1), &mut stream),
            Err(PropertyError::TypeMismatch { meta: "fixed_dict" })
        );
        // The None value cannot be written by a type that does not allow it.
        assert_eq!(
            dict.add_to_stream(&PropertyValue::FixedDict(None), &mut stream),
            Err(PropertyError::TypeMismatch { meta: "fixed_dict" })
        );
    }

    #[test]
    fn a_fixed_dict_allowing_none_accepts_the_none_value() {
        let dict = build_type(
            "<a> FIXED_DICT <AllowNone> true </AllowNone> <Properties> \
             <ammo> <Type> UINT8 </Type> </ammo> \
             </Properties> </a>",
        );
        assert_eq!(dict.check_value(&PropertyValue::FixedDict(None)), Ok(()));
    }

    #[test]
    fn a_failed_field_default_reports_the_reference_message() {
        // `Field::createDefaultValue` reports the field name when its default
        // section would not build. CLASS is not implemented here yet, so it
        // stands in for the reference's "failed to create from default
        // section" failure.
        let dict = build_type(
            "<a> FIXED_DICT <Properties> \
             <sub> <Type> CLASS <Properties> \
             <x> <Type> INT8 </Type> </x> </Properties> </Type> </sub> \
             </Properties> <Default> <sub> <x> 1 </x> </sub> </Default> </a>",
        );
        assert_eq!(
            dict.value_from_section(&parse_xml("<v> </v>")),
            Err(PropertyError::FieldDefaultFailed { name: "sub".into() })
        );
    }

    #[test]
    fn a_fixed_dict_missing_its_none_indicator_reports_the_reference_message() {
        let dict = build_type(
            "<a> FIXED_DICT <AllowNone> true </AllowNone> <Properties> \
             <ammo> <Type> UINT8 </Type> </ammo> \
             </Properties> </a>",
        );
        let mut stream = BinaryStream::new();
        assert_eq!(
            dict.create_from_stream(&mut stream),
            Err(PropertyError::MissingNoneIndicator)
        );
    }

    #[test]
    fn add_to_stream_rejects_the_wrong_payload() {
        let int32 = build_type("<a> INT32 </a>");
        let float32 = build_type("<a> FLOAT32 </a>");
        let float64 = build_type("<a> FLOAT64 </a>");
        let string = build_type("<a> STRING </a>");
        let blob = build_type("<a> BLOB </a>");
        let vector3 = build_type("<a> VECTOR3 </a>");

        let mut stream = BinaryStream::new();
        let mismatch = |meta: &'static str| PropertyError::TypeMismatch { meta };
        assert_eq!(
            int32.add_to_stream(&PropertyValue::Float(1.0), &mut stream),
            Err(mismatch("integer"))
        );
        assert_eq!(
            float32.add_to_stream(&PropertyValue::Int(1), &mut stream),
            Err(mismatch("float"))
        );
        assert_eq!(
            float64.add_to_stream(&PropertyValue::Int(1), &mut stream),
            Err(mismatch("float"))
        );
        assert_eq!(
            string.add_to_stream(&PropertyValue::Int(1), &mut stream),
            Err(mismatch("string"))
        );
        assert_eq!(
            blob.add_to_stream(&PropertyValue::Int(1), &mut stream),
            Err(mismatch("blob"))
        );
        assert_eq!(
            vector3.add_to_stream(&PropertyValue::Int(1), &mut stream),
            Err(mismatch("vector"))
        );
    }

    /// `write_packed_int` caps a length at `MAX_PACKED_INT` (16 MiB), so an
    /// overlong scalar string or blob fails the stream element.
    #[test]
    fn an_overlong_scalar_reports_the_stream_element_message() {
        use foundation::stream::MAX_PACKED_INT;

        let string = build_type("<a> STRING </a>");
        let mut stream = BinaryStream::new();
        let long = "x".repeat(MAX_PACKED_INT as usize + 1);
        assert_eq!(
            string.add_to_stream(&PropertyValue::String(long), &mut stream),
            Err(PropertyError::StreamTruncated {
                element: SIMPLE_STREAM_ELEMENT
            })
        );

        let blob = build_type("<a> BLOB </a>");
        let mut stream = BinaryStream::new();
        assert_eq!(
            blob.add_to_stream(
                &PropertyValue::Blob(vec![0u8; MAX_PACKED_INT as usize + 1]),
                &mut stream,
            ),
            Err(PropertyError::StreamTruncated {
                element: BLOB_STREAM_ELEMENT
            })
        );
    }

    #[test]
    fn scalar_round_trip_is_byte_exact() {
        for (xml, value) in [
            ("<a> INT8 </a>", PropertyValue::Int(127)),
            ("<a> INT16 </a>", PropertyValue::Int(-1234)),
            ("<a> INT32 </a>", PropertyValue::Int(-1)),
            ("<a> INT64 </a>", PropertyValue::Int(i64::MIN)),
            ("<a> UINT8 </a>", PropertyValue::Uint(255)),
            ("<a> UINT16 </a>", PropertyValue::Uint(65535)),
            ("<a> UINT32 </a>", PropertyValue::Uint(u32::MAX as u64)),
            ("<a> UINT64 </a>", PropertyValue::Uint(u64::MAX)),
            ("<a> FLOAT32 </a>", PropertyValue::Float(1.5)),
            ("<a> FLOAT64 </a>", PropertyValue::Float(1.5)),
            ("<a> STRING </a>", PropertyValue::String("hi".into())),
            (
                "<a> UNICODE_STRING </a>",
                PropertyValue::String("h\u{e9}llo".into()),
            ),
            ("<a> BLOB </a>", PropertyValue::Blob(vec![0, 1, 2])),
            ("<a> VECTOR2 </a>", PropertyValue::Vector(vec![1.0, 2.0])),
            (
                "<a> VECTOR3 </a>",
                PropertyValue::Vector(vec![1.0, -2.0, 3.5]),
            ),
            (
                "<a> VECTOR4 </a>",
                PropertyValue::Vector(vec![1.0, 2.0, 3.0, 4.0]),
            ),
        ] {
            let type_ = build_type(xml);
            let mut stream = BinaryStream::new();
            type_.add_to_stream(&value, &mut stream).unwrap();
            assert_eq!(
                type_.create_from_stream(&mut stream).unwrap(),
                value,
                "{xml}"
            );
        }
    }

    #[test]
    fn a_default_section_reads_every_scalar_width() {
        // "AQID" is the base64 of [1, 2, 3]; as_blob decodes it leniently.
        let type_ = build_type("<a> BLOB <Default> AQID </Default> </a>");
        assert_eq!(
            type_.initial_value(),
            Ok(PropertyValue::Blob(vec![1, 2, 3]))
        );

        let type_ = build_type("<a> VECTOR4 <Default> 1 2 3 4 </Default> </a>");
        assert_eq!(
            type_.initial_value(),
            Ok(PropertyValue::Vector(vec![1.0, 2.0, 3.0, 4.0]))
        );

        let type_ = build_type("<a> UNICODE_STRING <Default> \u{e9} </Default> </a>");
        assert_eq!(
            type_.initial_value(),
            Ok(PropertyValue::String("\u{e9}".into()))
        );

        let type_ = build_type("<a> UINT64 <Default> 18446744073709551615 </Default> </a>");
        assert_eq!(type_.initial_value(), Ok(PropertyValue::Uint(u64::MAX)));

        let type_ = build_type("<a> FLOAT64 <Default> 2.5 </Default> </a>");
        assert_eq!(type_.initial_value(), Ok(PropertyValue::Float(2.5)));
    }
    /// Parse one `<prop>` section into a `DataDescription`.
    fn parse_prop(xml: &str) -> DataDescription {
        let mut prop = DataDescription::default();
        let mut diag = crate::diagnostics::Diagnostics::new();
        let ctx = crate::data_type::TypeContext::default();
        let section = parse_xml(xml);
        assert!(
            prop.parse(&section, "Avatar", "", None, &ctx, &mut diag),
            "{xml}: {:?}",
            diag.messages()
        );
        prop
    }

    #[test]
    fn a_property_initial_value_follows_the_reference_order() {
        // A <Default> on a const type is the stored initial value.
        let const_prop = parse_prop(
            "<hp> <Type> INT32 </Type> <Flags> BASE </Flags> <Default> 10 </Default> </hp>",
        );
        assert_eq!(const_prop.initial_value(), Ok(PropertyValue::Int(10)));

        // VECTOR3 is not const, so its <Default> is kept as a section and read
        // here instead.
        let non_const = parse_prop(
            "<p> <Type> VECTOR3 </Type> <Flags> BASE </Flags> <Default> 1 2 3 </Default> </p>",
        );
        assert_eq!(
            non_const.initial_value(),
            Ok(PropertyValue::Vector(vec![1.0, 2.0, 3.0]))
        );

        // No <Default>: the type default.
        let bare = parse_prop("<hp> <Type> INT32 </Type> <Flags> BASE </Flags> </hp>");
        assert_eq!(bare.initial_value(), Ok(PropertyValue::Int(0)));
    }

    fn user_type() -> DataType {
        build_type("<a> USER_TYPE <implementedBy> demo.custom.Thing </implementedBy> </a>")
    }

    fn mailbox_value() -> PropertyValue {
        PropertyValue::MailBox {
            id: 42,
            ip: 0x0100_007f,
            port: 0x1234,
            salt: 0xabcd,
        }
    }

    #[test]
    fn a_user_type_reports_the_missing_converter_at_every_site() {
        let user = user_type();
        // `UserDataType::isSameType` is `pObject() != NULL`, which is silently
        // false without a Python host.
        assert_eq!(
            user.check_value(&PropertyValue::Int(1)),
            Err(PropertyError::TypeMismatch { meta: "user_type" })
        );
        // `createFromSection` bounces to `createObjectFromSection`.
        assert_eq!(
            user.value_from_section(&parse_xml("<v> </v>")),
            Err(PropertyError::MissingConverter {
                site: USER_CREATE_FROM_SECTION,
                module: "demo.custom".into(),
                instance: "Thing".into(),
            })
        );
        // `addObjectToStream` writes the empty string first.
        let mut stream = BinaryStream::new();
        assert_eq!(
            user.add_to_stream(&PropertyValue::Int(1), &mut stream),
            Err(PropertyError::MissingConverter {
                site: USER_ADD_TO_STREAM,
                module: "demo.custom".into(),
                instance: "Thing".into(),
            })
        );
        assert_eq!(stream.as_bytes(), &[0]);
    }

    #[test]
    fn a_user_type_stream_read_consumes_the_string_before_failing() {
        let user = user_type();
        let mut stream = BinaryStream::new();
        let _ = stream.write_string("payload");
        let err = user.create_from_stream(&mut stream).unwrap_err();
        assert_eq!(
            err,
            PropertyError::MissingConverter {
                site: USER_CREATE_FROM_STREAM,
                module: "demo.custom".into(),
                instance: "Thing".into(),
            }
        );
        // The length-prefixed string is consumed even on failure, so a short
        // stream truncates before the converter is ever consulted.
        let mut stream = BinaryStream::new();
        stream.write_u8(7);
        assert!(matches!(
            user.create_from_stream(&mut stream),
            Err(PropertyError::StreamTruncated { .. })
        ));
    }

    #[test]
    fn a_mailbox_stream_is_the_raw_twelve_byte_ref() {
        let mailbox = build_type("<a> MAILBOX </a>");
        let value = mailbox_value();
        let mut stream = BinaryStream::new();
        mailbox.add_to_stream(&value, &mut stream).unwrap();
        assert_eq!(
            stream.as_bytes(),
            &[
                42, 0, 0, 0, // id
                0x7f, 0x00, 0x00, 0x01, // ip
                0x34, 0x12, // port
                0xcd, 0xab, // salt
            ]
        );
        assert_eq!(mailbox.create_from_stream(&mut stream).unwrap(), value);
    }

    #[test]
    fn a_mailbox_rejects_an_uninitialised_address() {
        let mailbox = build_type("<a> MAILBOX </a>");
        assert_eq!(mailbox.check_value(&mailbox_value()), Ok(()));
        assert_eq!(
            mailbox.check_value(&PropertyValue::MailBox {
                id: 42,
                ip: 0,
                port: 0,
                salt: 0,
            }),
            Err(PropertyError::UninitialisedMailBox { id: 42 })
        );
        assert_eq!(
            mailbox.check_value(&PropertyValue::Int(1)),
            Err(PropertyError::TypeMismatch { meta: "mailbox" })
        );
    }

    #[test]
    fn a_mailbox_default_is_the_zeroed_ref() {
        let mailbox = build_type("<a> MAILBOX </a>");
        assert_eq!(
            mailbox.default_value(),
            Ok(PropertyValue::MailBox {
                id: 0,
                ip: 0,
                port: 0,
                salt: 0,
            })
        );
        // With a <Default> child the type still builds, the reference prints
        // the rejection, and the zeroed ref remains the default because
        // `setDefaultValue` never stores the section.
        let mut diag = crate::diagnostics::Diagnostics::new();
        let ctx = crate::data_type::TypeContext::default();
        let section = parse_xml("<a> MAILBOX <Default> junk </Default> </a>");
        let type_ = crate::data_type::build_type(&ctx, Some(&section), &mut diag).unwrap();
        assert_eq!(
            diag.errors(),
            &[
                "MailBoxDataType::setDefaultValue: Default value for mailbox not supported\n"
                    .to_string()
            ]
        );
        assert_eq!(
            type_.default_value(),
            Ok(PropertyValue::MailBox {
                id: 0,
                ip: 0,
                port: 0,
                salt: 0,
            })
        );
    }

    #[test]
    fn a_mailbox_stream_write_rejects_a_foreign_value() {
        let mailbox = build_type("<a> MAILBOX </a>");
        let mut stream = BinaryStream::new();
        assert_eq!(
            mailbox.add_to_stream(&PropertyValue::Int(1), &mut stream),
            Err(PropertyError::TypeMismatch { meta: "mailbox" })
        );
        assert!(stream.as_bytes().is_empty());
    }

    #[test]
    fn a_mailbox_reads_its_id_ip_and_etc_children() {
        let mailbox = build_type("<a> MAILBOX </a>");
        // `etc` packs the port and the salt as one uint32.
        assert_eq!(
            mailbox.value_from_section(&parse_xml(
                "<v> <id> 42 </id> <ip> 16777343 </ip> <etc> 2882343476 </etc> </v>"
            )),
            Ok(mailbox_value())
        );
        // Missing children read as zero, like `DataSection::readInt(0)`.
        assert_eq!(
            mailbox.value_from_section(&parse_xml("<v> </v>")),
            Ok(PropertyValue::MailBox {
                id: 0,
                ip: 0,
                port: 0,
                salt: 0,
            })
        );
    }

    #[test]
    fn error_text_matches_the_reference_messages() {
        assert_eq!(
            PropertyError::OutOfRange {
                value: 300,
                truncated: 44
            }
            .to_string(),
            "IntegerDataType::isSameType: 300 is out of range (truncated = 44).\n"
        );
        assert_eq!(
            PropertyError::NotUtf8.to_string(),
            "UnicodeStringDataType::createFromSection: Could not convert from UTF8.\n"
        );
        assert_eq!(
            PropertyError::StreamTruncated {
                element: SIMPLE_STREAM_ELEMENT
            }
            .to_string(),
            "SimpleStreamElement: Not enough data on stream to read value\n"
        );
        assert_eq!(
            PropertyError::StreamTruncated {
                element: BLOB_STREAM_ELEMENT
            }
            .to_string(),
            "BlobStreamElement: Not enough data on stream to read value\n"
        );
        assert_eq!(
            PropertyError::TypeMismatch { meta: "vector" }.to_string(),
            "value is not a vector"
        );
        assert_eq!(
            PropertyError::Unsupported("ARRAY").to_string(),
            "ARRAY values are not supported here"
        );

        // The sequence family (`sequence_data_type.cpp`), one per site.
        assert_eq!(
            PropertyError::WrongSequenceSize {
                size: 1,
                expected: 2
            }
            .to_string(),
            "SequenceDataType::isSameType: Wrong size 1 instead of 2.\n"
        );
        assert_eq!(
            PropertyError::BadSequenceElement {
                index: 1,
                expected: "INT8".to_string(),
                value: "Int(200)".to_string(),
            }
            .to_string(),
            "SequenceDataType::isSameType: Bad element 1. Expected type INT8, but \
             element was \"Int(200)\".\n"
        );
        assert_eq!(
            PropertyError::FixedSequenceSize {
                size: 3,
                expected: 2
            }
            .to_string(),
            "SequenceStartStreamElement::fromSourceToStream: Fixed-length sequence \
             reported size 3, but datatype expected size 2\n"
        );
        assert_eq!(
            PropertyError::MissingSequenceSize.to_string(),
            "SequenceStartStreamElement::fromStreamToSink: Missing size parameter \
             on stream\n"
        );
        // The reference prints "fromSourceToStream" from inside the read path —
        // a copy-paste slip in the original, reproduced verbatim.
        assert_eq!(
            PropertyError::InvalidSequenceSize {
                size: 3,
                remaining: 0
            }
            .to_string(),
            "SequenceStartStreamElement::fromSourceToStream: Invalid size on stream: 3 \
             (0 bytes remaining)\n"
        );
        assert_eq!(
            PropertyError::ExpectedSequenceBytes {
                bytes: 100,
                remaining: 2
            }
            .to_string(),
            "SequenceStartStreamElement::fromSourceToStream: Expected bytes on stream: 100 \
             (2 bytes remaining)\n"
        );

        // The fixed-dictionary family.
        assert_eq!(
            PropertyError::NullSection.to_string(),
            "FixedDictDataType::createFromSection: Section is NULL.\n"
        );
        // The reference's two adjacent literals concatenate, so there is no
        // colon after "fromStreamToSink".
        assert_eq!(
            PropertyError::MissingNoneIndicator.to_string(),
            "FixedDictStartStreamElement::fromStreamToSink Missing None/values \
             indicator on stream\n"
        );
        assert_eq!(
            PropertyError::FieldDefaultFailed {
                name: "ammo".into()
            }
            .to_string(),
            "ClassDataType::Field::createDefaultValue: Failed to create ammo \
             from default section\n"
        );

        // The user-type family, one string per reporting site.
        for site in [
            USER_ADD_TO_STREAM,
            USER_CREATE_FROM_STREAM,
            USER_CREATE_FROM_SECTION,
        ] {
            assert_eq!(
                PropertyError::MissingConverter {
                    site,
                    module: "demo".into(),
                    instance: "Thing".into(),
                }
                .to_string(),
                format!("{site}: Do not have demo.Thing\n")
            );
        }
        assert_eq!(
            PropertyError::UninitialisedMailBox { id: 42 }.to_string(),
            "MailBoxDataType::isSameType: Mailbox for entity 42 has not been \
             fully initialised yet (IP address is zero)\n"
        );
    }

    #[test]
    fn a_composite_default_section_falls_to_the_unsupported_arm() {
        // `pInitialValue` hands a `<Default>` section to `value_from_section`,
        // so a composite type with one lands on the unsupported arm. CLASS is
        // the last composite in the family to be implemented.
        let class_ = build_type(
            "<a> CLASS <Properties> <name> <Type> STRING </Type> </name> \
             </Properties> <Default> x </Default> </a>",
        );
        assert_eq!(
            class_.initial_value(),
            Err(PropertyError::Unsupported("CLASS"))
        );
    }

    #[test]
    fn composite_types_report_unsupported() {
        // CLASS is the last composite in the family to be implemented, so it
        // still lands on the documented error.
        let class_ = build_type(
            "<a> CLASS <Properties> <name> <Type> STRING </Type> </name> \
             </Properties> </a>",
        );
        assert_eq!(
            class_.check_value(&PropertyValue::String("x".into())),
            Err(PropertyError::Unsupported("CLASS"))
        );
        // The composite type default lands on the documented stand-in;
        // CLASS's own default arrives with its own item.
        assert_eq!(class_.initial_value(), Ok(PropertyValue::Int(0)));

        let mut stream = BinaryStream::new();
        assert_eq!(
            class_.add_to_stream(&PropertyValue::Int(1), &mut stream),
            Err(PropertyError::Unsupported("CLASS"))
        );
        assert_eq!(
            class_.create_from_stream(&mut stream),
            Err(PropertyError::Unsupported("CLASS"))
        );
    }
    #[test]
    fn unicode_strings_are_read_and_written_as_utf8() {
        let unicode = build_type("<a> UNICODE_STRING </a>");

        // The wire form is a length-prefixed UTF-8 string.
        let value = PropertyValue::String("h\u{e9}llo".into());
        let mut stream = BinaryStream::new();
        unicode.add_to_stream(&value, &mut stream).unwrap();
        assert_eq!(unicode.create_from_stream(&mut stream).unwrap(), value);
    }

    #[test]
    fn a_short_stream_reports_the_reference_message() {
        let int32 = build_type("<a> INT32 </a>");
        let mut stream = BinaryStream::new();
        stream.write_u8(1);
        assert_eq!(
            int32.create_from_stream(&mut stream),
            Err(PropertyError::StreamTruncated {
                element: SIMPLE_STREAM_ELEMENT
            })
        );

        let blob = build_type("<a> BLOB </a>");
        let mut blob_stream = BinaryStream::new();
        blob_stream.write_u8(1);
        assert_eq!(
            blob.create_from_stream(&mut blob_stream),
            Err(PropertyError::StreamTruncated {
                element: BLOB_STREAM_ELEMENT
            })
        );
    }
}
