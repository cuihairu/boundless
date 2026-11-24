//! `mongodb/metadata.hpp/cpp`: the typed metadata stream the backend
//! renders into a BSON document.
//!
//! The reference keeps two builders alive — a binary one fed by the typed
//! `operator<<`s and a JSON one fed by `setJSON` — and `getBson` returns
//! whichever was filled last, the JSON always winning once set. The port
//! holds the same pair; every add becomes a no-op once the JSON landed
//! (`metadata.hpp:59-66`), and a failed `setJSON` leaves the earlier adds
//! standing (`metadata.cpp:57-72`).
//!
//! [`build_metadata_bson`] walks the same `mlvt` wire as the file backend's
//! `build_metadata_json` (`parseMessage`, `log_storage.cpp:940-1000`), but
//! fills the builders instead of re-rendering text.

use crate::file_logger::mlvt;
use crate::mongodb::bson::{self, Bson, BsonObj};
use crate::stream::MemoryIStream;

/// `MongoDBMetadataStream`.
#[derive(Clone, Default)]
pub struct MongoDBMetadataStream {
    binary_output: BsonObj,
    binary_output_json: BsonObj,
    json_added: bool,
}

impl MongoDBMetadataStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// `addInt` — a typed add is silently dropped once the JSON argument
    /// has landed.
    pub fn add_int(&mut self, key: &str, value: i32) {
        if !self.json_added {
            self.binary_output.append(key, Bson::Int(value));
        }
    }

    /// `addLongLong`.
    pub fn add_long_long(&mut self, key: &str, value: i64) {
        if !self.json_added {
            self.binary_output.append(key, Bson::Long(value));
        }
    }

    /// The unsigned `operator<<`: the builder stores it as an `int`.
    pub fn add_unsigned(&mut self, key: &str, value: u32) {
        self.add_int(key, value as i32);
    }

    /// The float `operator<<`: the builder widens it to a double.
    pub fn add_float(&mut self, key: &str, value: f32) {
        self.add_double(key, value as f64);
    }

    /// `addDouble`.
    pub fn add_double(&mut self, key: &str, value: f64) {
        if !self.json_added {
            self.binary_output.append(key, Bson::Double(value));
        }
    }

    /// `addString`.
    pub fn add_string(&mut self, key: &str, value: &str) {
        if !self.json_added {
            self.binary_output.append(key, Bson::Str(value.to_string()));
        }
    }

    /// `setJSON` (`metadata.cpp:57-72`): parse and splice the document into
    /// the JSON builder. `false` is a parse failure — prior adds survive and
    /// the wire walk continues with the next entry.
    pub fn set_json(&mut self, json: &str) -> bool {
        let Some(parsed) = bson::from_json(json) else {
            return false;
        };
        self.binary_output_json.append_elements(&parsed);
        self.json_added = true;
        true
    }

    /// `getBson` (`metadata.hpp:44-53`): the JSON document once it exists,
    /// the binary one otherwise.
    pub fn get_bson(&self) -> BsonObj {
        if self.json_added {
            self.binary_output_json.clone()
        } else {
            self.binary_output.clone()
        }
    }
}

/// The typed metadata on the wire, walked into a BSON document. An
/// `mlvt::JSON_STRING` entry overrides everything added before it; a JSON
/// that fails to parse is skipped and the walk carries on (`log_storage.cpp
/// parseMessage`), an unknown type stores an empty string under its key.
pub fn build_metadata_bson(input: &mut MemoryIStream) -> BsonObj {
    let mut stream = MongoDBMetadataStream::new();

    loop {
        if input.remaining() == 0 {
            break;
        }
        let Some(tag) = input.read_u32() else {
            break;
        };
        if tag != mlvt::STRING {
            break;
        }
        let key = input.read_string();
        let Some(tag) = input.read_u32() else {
            break;
        };
        match tag {
            mlvt::JSON_STRING => {
                let raw = input.read_string();
                if stream.set_json(&raw) {
                    return stream.get_bson();
                }
            }
            mlvt::INT => {
                let Some(v) = input.read_u32() else {
                    break;
                };
                stream.add_int(&key, v as i32);
            }
            mlvt::LONG_LONG_INT => {
                let Some(v) = input.read_u64() else {
                    break;
                };
                stream.add_long_long(&key, v as i64);
            }
            mlvt::UNSIGNED_INT => {
                let Some(v) = input.read_u32() else {
                    break;
                };
                stream.add_unsigned(&key, v);
            }
            mlvt::STRING => {
                let v = input.read_string();
                stream.add_string(&key, &v);
            }
            mlvt::FLOAT => {
                let Some(v) = input.read_u32() else {
                    break;
                };
                stream.add_float(&key, f32::from_bits(v));
            }
            mlvt::DOUBLE => {
                let Some(v) = input.read_u64() else {
                    break;
                };
                stream.add_double(&key, f64::from_bits(v));
            }
            _ => {
                stream.add_string(&key, "");
            }
        }
    }

    stream.get_bson()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::MemoryOStream;

    /// Encode one wire entry: the key tag, the key, the value tag, the
    /// value — exactly what the reference's metadata stream emits.
    fn put_entry_u32(out: &mut MemoryOStream, key: &str, tag: u32, value: u32) {
        out.write_u32(mlvt::STRING);
        out.write_string(key);
        out.write_u32(tag);
        out.write_u32(value);
    }

    fn put_entry_string(out: &mut MemoryOStream, key: &str, tag: u32, value: &str) {
        out.write_u32(mlvt::STRING);
        out.write_string(key);
        out.write_u32(tag);
        out.write_string(value);
    }

    fn put_entry_u64(out: &mut MemoryOStream, key: &str, tag: u32, value: u64) {
        out.write_u32(mlvt::STRING);
        out.write_string(key);
        out.write_u32(tag);
        out.write_u64(value);
    }

    fn wire(build: impl FnOnce(&mut MemoryOStream)) -> Vec<u8> {
        let mut out = MemoryOStream::new();
        build(&mut out);
        out.data().to_vec()
    }

    #[test]
    fn typed_adds_land_in_the_binary_builder() {
        let wire_bytes = wire(|out| {
            put_entry_u32(out, "count", mlvt::INT, 7);
            put_entry_u32(out, "ident", mlvt::UNSIGNED_INT, u32::MAX);
            put_entry_u64(out, "ticks", mlvt::LONG_LONG_INT, i64::MAX as u64);
            put_entry_string(out, "msg", mlvt::STRING, "hello");
            put_entry_u32(out, "ratio", mlvt::FLOAT, 1.5f32.to_bits());
            put_entry_u64(out, "ratio", mlvt::DOUBLE, 2.5f64.to_bits());
        });

        let mut input = MemoryIStream::new(&wire_bytes);
        let doc = build_metadata_bson(&mut input);
        assert_eq!(doc.get_int("count"), 7);
        assert_eq!(doc.get_int("ident"), -1, "unsigned wraps into the int slot");
        assert_eq!(doc.fields()[2].1, Bson::Long(i64::MAX));
        assert_eq!(doc.get_string("msg"), "hello");
        assert_eq!(doc.fields()[4].1, Bson::Double(1.5), "float widens");
        assert_eq!(doc.fields()[5].1, Bson::Double(2.5));
    }

    #[test]
    fn a_json_argument_overrides_all_previous_adds() {
        let wire_bytes = wire(|out| {
            put_entry_u32(out, "dropped", mlvt::INT, 1);
            put_entry_string(out, "", mlvt::JSON_STRING, r#"{"a": 2}"#);
        });

        let mut input = MemoryIStream::new(&wire_bytes);
        let doc = build_metadata_bson(&mut input);
        assert!(!doc.has_field("dropped"));
        assert_eq!(doc.get_int("a"), 2);
    }

    #[test]
    fn a_failed_json_argument_is_skipped_and_the_walk_continues() {
        let wire_bytes = wire(|out| {
            put_entry_u32(out, "kept", mlvt::INT, 3);
            put_entry_string(out, "", mlvt::JSON_STRING, "{not json");
            put_entry_string(out, "late", mlvt::STRING, "still read");
        });

        let mut input = MemoryIStream::new(&wire_bytes);
        let doc = build_metadata_bson(&mut input);
        assert_eq!(doc.get_int("kept"), 3, "adds before the failed JSON stand");
        assert_eq!(doc.get_string("late"), "still read");
    }

    #[test]
    fn an_unknown_type_stores_an_empty_string() {
        let wire_bytes = wire(|out| {
            put_entry_u32(out, "weird", b'z' as u32, 0);
        });

        let mut input = MemoryIStream::new(&wire_bytes);
        let doc = build_metadata_bson(&mut input);
        assert_eq!(doc.get_string("weird"), "");
    }

    #[test]
    fn a_getbson_on_an_empty_stream_is_an_empty_document() {
        assert!(MongoDBMetadataStream::new().get_bson().is_empty());
    }

    #[test]
    fn set_json_failure_keeps_binary_output_as_getbson() {
        let mut stream = MongoDBMetadataStream::new();
        stream.add_int("n", 4);
        assert!(!stream.set_json("{bad"));
        assert_eq!(stream.get_bson().get_int("n"), 4);

        assert!(stream.set_json(r#"{"m": 5}"#));
        assert_eq!(stream.get_bson().get_int("m"), 5);
        assert!(!stream.get_bson().has_field("n"), "JSON replaces the lot");
    }

    #[test]
    fn adds_after_set_json_are_dropped() {
        let mut stream = MongoDBMetadataStream::new();
        assert!(stream.set_json(r#"{"only": true}"#));
        stream.add_int("late", 1);
        let doc = stream.get_bson();
        assert!(doc.has_field("only"));
        assert!(!doc.has_field("late"));
    }

    #[test]
    fn an_empty_wire_yields_an_empty_document() {
        let bytes: Vec<u8> = Vec::new();
        let mut input = MemoryIStream::new(&bytes);
        assert!(build_metadata_bson(&mut input).is_empty());
    }

    #[test]
    fn a_non_string_key_tag_terminates_the_walk() {
        let bytes = {
            let mut out = MemoryOStream::new();
            out.write_u32(mlvt::INT);
            out.data().to_vec()
        };
        let mut input = MemoryIStream::new(&bytes);
        assert!(build_metadata_bson(&mut input).is_empty());
    }

    #[test]
    fn wire_documents_cross_the_bson_round_trip() {
        let wire_bytes = wire(|out| {
            put_entry_u32(out, "hp", mlvt::INT, 42);
        });

        let mut input = MemoryIStream::new(&wire_bytes);
        let doc = build_metadata_bson(&mut input);
        let decoded = BsonObj::decode(&doc.encode()).expect("wire bytes decode");
        assert_eq!(decoded.get_int("hp"), 42);
    }
}
