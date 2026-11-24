//! The JSONB codec for nested property values.
//!
//! The reference streams nested properties (`ARRAY`, `TUPLE`, `FIXED_DICT`,
//! `MAILBOX`) as opaque binary columns; PostgreSQL has no `VARBINARY`
//! mapping for structured values, so this backend stores them as JSONB
//! and needs to tell the shapes apart on the way back out. The encoding
//! is therefore tagged: every value is a one-field object whose key names
//! the [`PropertyValue`] shape, and fixed-dict fields travel as
//! `[name, value]` pairs so the field order survives (JSON objects are
//! unordered). The integer shapes stay distinct the same way — `Int` and
//! `Uint` both fit `BIGINT` but are different metas.

use db::DbError;
use entitydef::value::PropertyValue;
use serde_json::{Map, Value as Json};

fn storage_error(message: impl Into<String>) -> DbError {
    DbError::Storage(message.into())
}

/// Encodes a float. JSON has no `NaN` or `Infinity`, so the non-finite
/// values travel as the strings `"nan"`, `"inf"`, `"-inf"`.
fn float_to_json(x: f64) -> Json {
    if x.is_finite() {
        // Exact: serde_json's number formatting round-trips f64.
        Json::from(x)
    } else if x.is_nan() {
        Json::String("nan".to_string())
    } else if x > 0.0 {
        Json::String("inf".to_string())
    } else {
        Json::String("-inf".to_string())
    }
}

fn float_from_json(json: &Json) -> Result<f64, DbError> {
    match json {
        Json::Number(n) => Ok(n.as_f64().unwrap_or(f64::NAN)),
        Json::String(s) if s == "nan" => Ok(f64::NAN),
        Json::String(s) if s == "inf" => Ok(f64::INFINITY),
        Json::String(s) if s == "-inf" => Ok(f64::NEG_INFINITY),
        other => Err(storage_error(format!(
            "float column carries {other}, not a number"
        ))),
    }
}

/// Encodes a value as its tagged object.
pub fn encode(value: &PropertyValue) -> Json {
    let mut object = Map::new();
    match value {
        PropertyValue::Int(n) => {
            object.insert("i".to_string(), Json::from(*n));
        }
        PropertyValue::Uint(n) => {
            object.insert("u".to_string(), Json::from(*n));
        }
        PropertyValue::Float(x) => {
            object.insert("f".to_string(), float_to_json(*x));
        }
        PropertyValue::String(s) => {
            object.insert("s".to_string(), Json::from(s.as_str()));
        }
        PropertyValue::Blob(bytes) => {
            let mut hex = String::with_capacity(bytes.len() * 2);
            for byte in bytes {
                hex.push_str(&format!("{byte:02x}"));
            }
            object.insert("b".to_string(), Json::from(hex));
        }
        PropertyValue::Vector(components) => {
            let items = components
                .iter()
                .map(|c| float_to_json(*c as f64))
                .collect();
            object.insert("v".to_string(), Json::Array(items));
        }
        PropertyValue::Sequence(items) => {
            let items = items.iter().map(encode).collect();
            object.insert("q".to_string(), Json::Array(items));
        }
        PropertyValue::FixedDict(None) => {
            object.insert("d".to_string(), Json::Null);
        }
        PropertyValue::FixedDict(Some(fields)) => {
            // Field order matters (`FixedDictDataType`'s own field order), so
            // the fields travel as [name, value] pairs, not an object.
            let items = fields
                .iter()
                .map(|(name, value)| Json::Array(vec![Json::from(name.as_str()), encode(value)]))
                .collect();
            object.insert("d".to_string(), Json::Array(items));
        }
        PropertyValue::MailBox { id, ip, port, salt } => {
            object.insert(
                "m".to_string(),
                Json::Array(vec![
                    Json::from(*id),
                    Json::from(*ip),
                    Json::from(*port),
                    Json::from(*salt),
                ]),
            );
        }
    }
    Json::Object(object)
}

/// Encodes a value as the JSONB text to store.
pub fn encode_to_string(value: &PropertyValue) -> String {
    encode(value).to_string()
}

/// Decodes a tagged object back into a value.
pub fn decode(json: &Json) -> Result<PropertyValue, DbError> {
    let Json::Object(object) = json else {
        return Err(storage_error("nested column carries a non-object value"));
    };
    let (key, value) = object
        .iter()
        .next()
        .ok_or_else(|| storage_error("nested column carries an empty object"))?;
    match key.as_str() {
        "i" => {
            let n = value
                .as_i64()
                .ok_or_else(|| storage_error("int column carries a non-int number"))?;
            Ok(PropertyValue::Int(n))
        }
        "u" => {
            let n = value
                .as_u64()
                .ok_or_else(|| storage_error("uint column carries a non-uint number"))?;
            Ok(PropertyValue::Uint(n))
        }
        "f" => Ok(PropertyValue::Float(float_from_json(value)?)),
        "s" => {
            let Json::String(s) = value else {
                return Err(storage_error("string column carries a non-string"));
            };
            Ok(PropertyValue::String(s.clone()))
        }
        "b" => {
            let Json::String(hex) = value else {
                return Err(storage_error("blob column carries a non-string"));
            };
            if hex.len() % 2 != 0 {
                return Err(storage_error(
                    "blob column carries an odd-length hex string",
                ));
            }
            let mut bytes = Vec::with_capacity(hex.len() / 2);
            for pair in hex.as_bytes().chunks(2) {
                let high = (pair[0] as char).to_digit(16);
                let low = (pair[1] as char).to_digit(16);
                let (Some(high), Some(low)) = (high, low) else {
                    return Err(storage_error(format!(
                        "blob column carries non-hex text: {}",
                        String::from_utf8_lossy(pair)
                    )));
                };
                bytes.push(((high << 4) | low) as u8);
            }
            Ok(PropertyValue::Blob(bytes))
        }
        "v" => {
            let Json::Array(items) = value else {
                return Err(storage_error("vector column carries a non-array"));
            };
            let mut components = Vec::with_capacity(items.len());
            for item in items {
                components.push(float_from_json(item)? as f32);
            }
            Ok(PropertyValue::Vector(components))
        }
        "q" => {
            let Json::Array(items) = value else {
                return Err(storage_error("sequence column carries a non-array"));
            };
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(decode(item)?);
            }
            Ok(PropertyValue::Sequence(values))
        }
        "d" => match value {
            Json::Null => Ok(PropertyValue::FixedDict(None)),
            Json::Array(items) => {
                let mut fields = Vec::with_capacity(items.len());
                for item in items {
                    let Json::Array(pair) = item else {
                        return Err(storage_error("fixed dict column carries a non-pair field"));
                    };
                    let [Json::String(name), tagged] = pair.as_slice() else {
                        return Err(storage_error(
                            "fixed dict column carries a malformed field pair",
                        ));
                    };
                    fields.push((name.clone(), decode(tagged)?));
                }
                Ok(PropertyValue::FixedDict(Some(fields)))
            }
            other => Err(storage_error(format!(
                "fixed dict column carries {other}, not null or an array"
            ))),
        },
        "m" => {
            let Json::Array(items) = value else {
                return Err(storage_error("mailbox column carries a non-array"));
            };
            if items.len() != 4 {
                return Err(storage_error(
                    "mailbox column carries an array that is not [id, ip, port, salt]",
                ));
            }
            let number = |json: &Json| -> Result<f64, DbError> {
                json.as_f64()
                    .ok_or_else(|| storage_error("mailbox column carries a non-number"))
            };
            Ok(PropertyValue::MailBox {
                id: number(&items[0])? as i32,
                ip: number(&items[1])? as u32,
                port: number(&items[2])? as u16,
                salt: number(&items[3])? as u16,
            })
        }
        other => Err(storage_error(format!(
            "nested column carries an unknown tag {other:?}"
        ))),
    }
}

/// Decodes JSONB text back into a value.
pub fn decode_from_str(text: &str) -> Result<PropertyValue, DbError> {
    let json: Json = serde_json::from_str(text)
        .map_err(|e| storage_error(format!("nested column carries bad JSON: {e}")))?;
    decode(&json)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `assert_eq!` with the one exception `PropertyValue`'s `PartialEq`
    /// carries from `f64`: a `NaN` never equals itself, so the comparison
    /// reads the non-finite markers instead.
    fn values_agree(value: &PropertyValue, back: &PropertyValue) -> bool {
        match (value, back) {
            (PropertyValue::Float(a), PropertyValue::Float(b)) => {
                (a.is_nan() && b.is_nan()) || a == b
            }
            (PropertyValue::Sequence(a), PropertyValue::Sequence(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| values_agree(x, y))
            }
            (PropertyValue::FixedDict(a), PropertyValue::FixedDict(b)) => match (a, b) {
                (None, None) => true,
                (Some(a), Some(b)) => {
                    a.len() == b.len()
                        && a.iter()
                            .zip(b)
                            .all(|((ka, va), (kb, vb))| ka == kb && values_agree(va, vb))
                }
                _ => false,
            },
            _ => value == back,
        }
    }

    fn round_trip(value: PropertyValue) {
        let text = encode_to_string(&value);
        let back = decode_from_str(&text).unwrap();
        assert!(
            values_agree(&value, &back),
            "text was {text}: {back:?} does not agree with {value:?}"
        );
    }

    #[test]
    fn every_simple_shape_round_trips() {
        round_trip(PropertyValue::Int(i64::MIN));
        round_trip(PropertyValue::Int(-1));
        round_trip(PropertyValue::Int(0));
        round_trip(PropertyValue::Int(i64::MAX));
        round_trip(PropertyValue::Uint(0));
        round_trip(PropertyValue::Uint(u64::MAX));
        round_trip(PropertyValue::Float(0.0));
        round_trip(PropertyValue::Float(-0.0));
        round_trip(PropertyValue::Float(1.5));
        round_trip(PropertyValue::Float(f64::MIN));
        round_trip(PropertyValue::Float(f64::MAX));
        round_trip(PropertyValue::Float(f64::MIN_POSITIVE));
        round_trip(PropertyValue::Float(std::f64::consts::PI));
    }

    #[test]
    fn non_finite_floats_travel_as_marker_strings() {
        round_trip(PropertyValue::Float(f64::NAN));
        round_trip(PropertyValue::Float(f64::INFINITY));
        round_trip(PropertyValue::Float(f64::NEG_INFINITY));
    }

    #[test]
    fn unicode_and_escaped_strings_round_trip() {
        // Quotes, backslashes, control bytes, astral-plane characters.
        round_trip(PropertyValue::String("伤害 π 🎮 \\ \" \n\t\u{1}".into()));
        round_trip(PropertyValue::String(String::new()));
        round_trip(PropertyValue::String("0️⃣ 汉字 😀 АБВ ﬁ".into()));
    }

    #[test]
    fn binary_round_trips_through_every_byte_value() {
        round_trip(PropertyValue::Blob(vec![]));
        round_trip(PropertyValue::Blob((0..=255u8).collect()));
        round_trip(PropertyValue::Blob(vec![
            0x00, 0xff, 0x0a, 0x0d, 0x1a, 0x00,
        ]));
    }

    #[test]
    fn vectors_round_trip_component_exact() {
        round_trip(PropertyValue::Vector(vec![]));
        round_trip(PropertyValue::Vector(vec![1.0, 2.5, -3.75]));
        round_trip(PropertyValue::Vector(vec![
            f32::MIN,
            f32::MAX,
            f32::MIN_POSITIVE,
        ]));
    }

    #[test]
    fn nested_sequences_and_dicts_round_trip_in_order() {
        round_trip(PropertyValue::Sequence(vec![]));
        round_trip(PropertyValue::Sequence(vec![
            PropertyValue::Int(2),
            PropertyValue::Uint(3),
            PropertyValue::Float(0.5),
            PropertyValue::String("four".into()),
        ]));
        round_trip(PropertyValue::Sequence(vec![PropertyValue::Sequence(
            vec![PropertyValue::Int(1)],
        )]));
        round_trip(PropertyValue::FixedDict(None));
        // Field order must survive even when it is not sorted.
        round_trip(PropertyValue::FixedDict(Some(vec![
            ("zebra".to_string(), PropertyValue::Int(1)),
            ("alpha".to_string(), PropertyValue::Float(2.0)),
            (
                "nested".to_string(),
                PropertyValue::Sequence(vec![
                    PropertyValue::Blob(vec![7]),
                    PropertyValue::Float(f64::NAN),
                ]),
            ),
        ])));
    }

    #[test]
    fn mailboxes_round_trip() {
        round_trip(PropertyValue::MailBox {
            id: -2147483648,
            ip: 3232235777,
            port: 65535,
            salt: 0,
        });
        round_trip(PropertyValue::MailBox {
            id: 2147483647,
            ip: 0,
            port: 0,
            salt: 65535,
        });
    }

    #[test]
    fn decode_rejects_untagged_and_malformed_values() {
        assert!(decode_from_str("7").is_err());
        assert!(decode_from_str("{}").is_err());
        assert!(decode_from_str("{\"x\":1}").is_err());
        assert!(decode_from_str("{\"b\":\"zz\"}").is_err());
        assert!(decode_from_str("{\"b\":\"0\"}").is_err());
        assert!(decode_from_str("{\"m\":[1,2]}").is_err());
        assert!(decode_from_str("{\"d\":3}").is_err());
        assert!(decode_from_str("not json").is_err());
    }

    #[test]
    fn decode_rejects_every_type_confused_leg() {
        // Floats that carry neither a number nor a marker string.
        assert!(decode_from_str("{\"f\":true}").is_err());
        assert!(decode_from_str("{\"f\":{\"x\":1}}").is_err());
        // Ints that carry a non-int.
        assert!(decode_from_str("{\"i\":\"5\"}").is_err());
        assert!(decode_from_str("{\"i\":1.5}").is_err());
        // Uints that carry a negative number.
        assert!(decode_from_str("{\"u\":-1}").is_err());
        // Strings and blobs that carry other shapes.
        assert!(decode_from_str("{\"s\":5}").is_err());
        assert!(decode_from_str("{\"b\":7}").is_err());
        // Arrays that carry other shapes.
        assert!(decode_from_str("{\"v\":\"no\"}").is_err());
        assert!(decode_from_str("{\"q\":1}").is_err());
        assert!(decode_from_str("{\"m\":\"no\"}").is_err());
        // A fixed dict whose fields are not [name, value] pairs.
        assert!(decode_from_str("{\"d\":[\"no\"]}").is_err());
        assert!(decode_from_str("{\"d\":[[5, 1]]}").is_err());
        assert!(decode_from_str("{\"d\":[[\"a\"]]}").is_err());
        // A mailbox with the wrong arity or a non-number member.
        assert!(decode_from_str("{\"m\":[1,2,3,\"x\"]}").is_err());
        // The nested members of those shapes fail the same way.
        assert!(decode_from_str("{\"q\":[{\"i\":\"x\"}]}").is_err());
        assert!(decode_from_str("{\"d\":[[\"a\", {\"v\":1}]]}").is_err());
    }

    #[test]
    fn values_agree_distinguishes_the_shapes_it_compares() {
        // The NaN exception reads the markers, not bitwise identity.
        assert!(values_agree(
            &PropertyValue::Float(f64::NAN),
            &PropertyValue::Float(f64::NAN)
        ));
        assert!(!values_agree(
            &PropertyValue::Float(f64::NAN),
            &PropertyValue::Float(0.0)
        ));
        // The containers compare only same-shaped pairs.
        assert!(!values_agree(
            &PropertyValue::FixedDict(None),
            &PropertyValue::FixedDict(Some(vec![]))
        ));
        assert!(!values_agree(
            &PropertyValue::Sequence(vec![PropertyValue::Int(1)]),
            &PropertyValue::Sequence(vec![])
        ));
    }

    #[test]
    fn tags_stay_distinct_between_int_and_uint() {
        // Int and Uint both fit BIGINT; the tags keep the shapes apart.
        assert_eq!(decode_from_str("{\"i\":5}").unwrap(), PropertyValue::Int(5));
        assert_eq!(
            decode_from_str("{\"u\":5}").unwrap(),
            PropertyValue::Uint(5)
        );
        assert_eq!(
            encode_to_string(&PropertyValue::Uint(u64::MAX)),
            format!("{{\"u\":{}}}", u64::MAX)
        );
    }
}
