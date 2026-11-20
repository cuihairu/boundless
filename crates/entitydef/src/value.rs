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
}

/// The `SimpleStreamElement` / `BlobStreamElement` truncation text.
const SIMPLE_STREAM_ELEMENT: &str = "SimpleStreamElement";
const BLOB_STREAM_ELEMENT: &str = "BlobStreamElement";

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
        _ => Err(PropertyError::Unsupported(kind.meta_name())),
    }
}

/// `DataDescription::pInitialValue`, as an ordered lookup over the two stored
/// slots and then the type default.
fn initial_value_from(
    kind: &Kind,
    sections: [Option<DataSection>; 2],
) -> Result<PropertyValue, PropertyError> {
    if let Some(section) = sections.into_iter().flatten().next() {
        return value_from_section(kind, &section);
    }
    Ok(default_value(kind))
}

/// `DataType::createFromSection` for the scalar metas, over the value text of
/// `section` (the reference reads `pSection->as< INT_TYPE >()` and friends on
/// the `<Default>` section itself).
fn value_from_section(kind: &Kind, section: &DataSection) -> Result<PropertyValue, PropertyError> {
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
            Some(section) => value_from_section(self.kind(), section),
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
        value_from_section(self.kind(), section)
    }

    /// `DataType::addToStream` for the scalar metas.
    pub fn add_to_stream(
        &self,
        value: &PropertyValue,
        stream: &mut BinaryStream,
    ) -> Result<(), PropertyError> {
        add_to_stream(self.kind(), value, stream)
    }

    /// `DataType::createFromStream` for the scalar metas.
    pub fn create_from_stream(
        &self,
        stream: &mut BinaryStream,
    ) -> Result<PropertyValue, PropertyError> {
        create_from_stream(self.kind(), stream)
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
        )
    }
}

/// `DataType::addToStream` for the scalar metas, one `StreamElement` each.
fn add_to_stream(
    kind: &Kind,
    value: &PropertyValue,
    stream: &mut BinaryStream,
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
        _ => return Err(PropertyError::Unsupported(kind.meta_name())),
    }
    Ok(())
}

/// `DataType::createFromStream` for the scalar metas.
fn create_from_stream(
    kind: &Kind,
    stream: &mut BinaryStream,
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
        Kind::Vector { elements } => {
            let mut components = Vec::with_capacity(*elements as usize);
            for _ in 0..*elements {
                components.push(stream.read_f32().map_err(|_| truncated())?);
            }
            Ok(PropertyValue::Vector(components))
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

        // A composite type has no section reader yet.
        let array = build_type("<a> ARRAY <of> INT8 </of> </a>");
        assert_eq!(
            array.value_from_section(&parse_xml("<v> 7 </v>")),
            Err(PropertyError::Unsupported("ARRAY"))
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
    }

    #[test]
    fn composite_types_report_unsupported() {
        let array = build_type("<a> ARRAY <of> INT8 </of> </a>");
        assert_eq!(
            array.check_value(&PropertyValue::Int(1)),
            Err(PropertyError::Unsupported("ARRAY"))
        );
        // The composite type default lands on the documented stand-in;
        // ARRAY/TUPLE/FIXED_DICT defaults arrive with their own items.
        assert_eq!(array.initial_value(), Ok(PropertyValue::Int(0)));

        let mut stream = BinaryStream::new();
        assert_eq!(
            array.add_to_stream(&PropertyValue::Int(1), &mut stream),
            Err(PropertyError::Unsupported("ARRAY"))
        );
        assert_eq!(
            array.create_from_stream(&mut stream),
            Err(PropertyError::Unsupported("ARRAY"))
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
