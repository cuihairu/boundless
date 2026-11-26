//! OPT-04-01-03: the pre-generated codec evaluation.
//!
//! The interpretive codec walks the `Kind` tree on every call — the
//! per-`Kind` match and the payload match run for every value, and the
//! composites recurse through the generic entry point. A pre-generated
//! codec compiles that tree once (at definition load, the way a codegen
//! pass would at build time) into specialized closures whose kind is
//! baked in; the acceptance for this row is a **report of the
//! serialization cost** on both paths, not a change to the shipped
//! codec.
//!
//! The evaluation therefore lives in the test tree next to its
//! measurement harness (`entitydef_baseline.rs`) and keeps the value
//! vocabulary untouched: the generated closures take and return
//! `PropertyValue` exactly like the interpretive path, so the pair of
//! structural faces below is exact:
//!
//! 1. **Byte identity** — for every property of every entity in both
//!    fixtures, the generated encode produces the same bytes as
//!    `DataType::add_to_stream`, and both decoders return the parsed
//!    default from either encoding. A generated codec that changes the
//!    wire format cannot produce any timing row.
//! 2. **Cost report** — per property family, encode-only and
//!    decode-only nanoseconds, interpretive against generated, plus the
//!    one-off generation cost per fixture. Wall-clock numbers are
//!    recorded, never asserted tight (the baseline discipline).
//!
//! The three scalar-family helpers the closures need (`write_int`,
//! `truncate_integer`, `check_sequence_capacity`) are private to the
//! crate, so faithful local copies live here, pinned to the originals by
//! the byte-identity face.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use entitydef::data_type::{DataType, Kind};
use entitydef::diagnostics::Diagnostics;
use entitydef::entity_description::{DefResolver, FsResolver, clear_section_cache};
use entitydef::entity_map::{EntityDescriptionMap, MessageRanges};
use entitydef::value::{PropertyError, PropertyValue};
use foundation::stream::BinaryStream;

/// The repository root from the crate manifest (`crates/entitydef` up
/// two levels).
const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
/// Samples per family and face in the cost leg (the baseline's ROUNDS).
const ROUNDS: usize = 20_000;
/// The `SimpleStreamElement` truncation text, as in the crate codec.
const SIMPLE_STREAM_ELEMENT: &str = "SimpleStreamElement";

// ---------------------------------------------------------------------------
// The local copies of the crate's private scalar helpers
// ---------------------------------------------------------------------------

/// `value.rs::write_int`: the low `bytes` bytes of the 64-bit pattern,
/// little-endian.
fn write_int(stream: &mut BinaryStream, bytes: i32, bits: u64) {
    assert!(bytes > 0 && bytes <= 8, "integer byte width {bytes}");
    let mut pattern = [0u8; 8];
    pattern[..bytes as usize].copy_from_slice(&bits.to_le_bytes()[..bytes as usize]);
    let _ = stream.write_bytes(&pattern[..bytes as usize]);
}

/// `value.rs::truncate_integer`: mask to the `bytes`-wide two's
/// complement pattern, sign-extending it back for the signed metas.
fn truncate_integer(value: i64, bytes: i32, signed: bool) -> i64 {
    let bits = bytes as u32 * 8;
    if bits == 64 {
        return value;
    }
    let raw = value & ((1i64 << bits) - 1);
    if signed {
        (raw << (64 - bits)) >> (64 - bits)
    } else {
        raw
    }
}

/// `value.rs::check_sequence_capacity`: a hostile count must not get to
/// allocate before the bytes are confirmed to be there.
fn check_sequence_capacity(
    element_size: i32,
    count: usize,
    remaining: i32,
) -> Result<(), PropertyError> {
    if element_size < 0 {
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
        return Err(PropertyError::ExpectedSequenceBytes {
            bytes: count * element_size as usize,
            remaining,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The generated codec
// ---------------------------------------------------------------------------

/// A kind-specialized encoder: the `Kind` is baked in at generation.
type Encoder = Box<dyn Fn(&PropertyValue, &mut BinaryStream) -> Result<(), PropertyError>>;
/// A kind-specialized decoder, same contract.
type Decoder = Box<dyn Fn(&mut BinaryStream) -> Result<PropertyValue, PropertyError>>;

/// The compiled codec for one `Kind` tree. The closures own clones of the
/// `Arc<DataType>` nodes and the field names they captured, so the value
/// is `'static` and moves freely — the shape a build-time codegen pass
/// would emit as plain functions.
struct GeneratedCodec {
    encode: Encoder,
    decode: Decoder,
}

/// Compiles a `Kind` tree into a `GeneratedCodec`, mirroring the
/// interpretive `add_to_stream` / `create_from_stream` semantics arm by
/// arm. Returns `None` for the kinds the interpretive stream path itself
/// rejects (`CLASS`, `UDO_REF`, `PYTHON`, `USER_TYPE`); the fixtures
/// never contain them, and the identity leg counts the occurrences.
fn generate(kind: &Kind) -> Option<GeneratedCodec> {
    match kind {
        Kind::Int { bytes, signed } => {
            let (bytes, signed) = (*bytes, *signed);
            Some(GeneratedCodec {
                encode: Box::new(move |value, stream| match value {
                    PropertyValue::Int(v) => {
                        write_int(stream, bytes, *v as u64);
                        Ok(())
                    }
                    PropertyValue::Uint(v) => {
                        write_int(stream, bytes, *v);
                        Ok(())
                    }
                    _ => Err(PropertyError::TypeMismatch { meta: "integer" }),
                }),
                decode: Box::new(move |stream| {
                    let raw = read_raw_integer(stream, bytes)?;
                    Ok(if signed {
                        PropertyValue::Int(truncate_integer(raw as i64, bytes, true))
                    } else {
                        PropertyValue::Uint(raw)
                    })
                }),
            })
        }
        Kind::Float32 => Some(GeneratedCodec {
            encode: Box::new(|value, stream| match value {
                PropertyValue::Float(v) => {
                    let _ = stream.write_f32(*v as f32);
                    Ok(())
                }
                _ => Err(PropertyError::TypeMismatch { meta: "float" }),
            }),
            decode: Box::new(|stream| {
                let v = stream
                    .read_f32()
                    .map_err(|_| PropertyError::StreamTruncated {
                        element: SIMPLE_STREAM_ELEMENT,
                    })?;
                Ok(PropertyValue::Float(v as f64))
            }),
        }),
        Kind::Float64 => Some(GeneratedCodec {
            encode: Box::new(|value, stream| match value {
                PropertyValue::Float(v) => {
                    let _ = stream.write_f64(*v);
                    Ok(())
                }
                _ => Err(PropertyError::TypeMismatch { meta: "float" }),
            }),
            decode: Box::new(|stream| {
                let v = stream
                    .read_f64()
                    .map_err(|_| PropertyError::StreamTruncated {
                        element: SIMPLE_STREAM_ELEMENT,
                    })?;
                Ok(PropertyValue::Float(v))
            }),
        }),
        Kind::String | Kind::UnicodeString => Some(GeneratedCodec {
            encode: Box::new(|value, stream| match value {
                PropertyValue::String(text) => {
                    stream.write_string(text).map(|_| ()).map_err(|_| {
                        PropertyError::StreamTruncated {
                            element: SIMPLE_STREAM_ELEMENT,
                        }
                    })
                }
                _ => Err(PropertyError::TypeMismatch { meta: "string" }),
            }),
            decode: Box::new(|stream| {
                let text = stream
                    .read_string()
                    .map_err(|_| PropertyError::StreamTruncated {
                        element: SIMPLE_STREAM_ELEMENT,
                    })?;
                Ok(PropertyValue::String(text))
            }),
        }),
        Kind::Blob => Some(GeneratedCodec {
            encode: Box::new(|value, stream| match value {
                PropertyValue::Blob(bytes) => stream.write_blob(bytes).map(|_| ()).map_err(|_| {
                    PropertyError::StreamTruncated {
                        element: "BlobStreamElement",
                    }
                }),
                _ => Err(PropertyError::TypeMismatch { meta: "blob" }),
            }),
            decode: Box::new(|stream| {
                stream
                    .read_blob()
                    .map_err(|_| PropertyError::StreamTruncated {
                        element: "BlobStreamElement",
                    })
                    .map(PropertyValue::Blob)
            }),
        }),
        Kind::Vector { elements } => {
            let elements = *elements;
            Some(GeneratedCodec {
                encode: Box::new(move |value, stream| match value {
                    PropertyValue::Vector(components) => {
                        for component in components.iter() {
                            let _ = stream.write_f32(*component);
                        }
                        Ok(())
                    }
                    _ => Err(PropertyError::TypeMismatch { meta: "vector" }),
                }),
                decode: Box::new(move |stream| {
                    let mut components = Vec::with_capacity(elements as usize);
                    for _ in 0..elements {
                        components.push(stream.read_f32().map_err(|_| {
                            PropertyError::StreamTruncated {
                                element: SIMPLE_STREAM_ELEMENT,
                            }
                        })?);
                    }
                    Ok(PropertyValue::Vector(components))
                }),
            })
        }
        Kind::Sequence { size, element, .. } => {
            let size = *size;
            // The element's stream size is a property of the element
            // type, so the generated codec resolves it once instead of
            // on every read.
            let element_size = element.stream_size();
            let inner = Rc::new(generate(element.kind())?);
            let decode_inner = Rc::clone(&inner);
            Some(GeneratedCodec {
                encode: Box::new(move |value, stream| match value {
                    PropertyValue::Sequence(elements) => {
                        if size != 0 && elements.len() != size as usize {
                            return Err(PropertyError::FixedSequenceSize {
                                size: elements.len() as i32,
                                expected: size,
                            });
                        }
                        if size == 0 {
                            let _ =
                                stream
                                    .write_packed_int(elements.len() as u32)
                                    .map_err(|_| PropertyError::StreamTruncated {
                                        element: SIMPLE_STREAM_ELEMENT,
                                    })?;
                        }
                        for element_value in elements.iter() {
                            (inner.encode)(element_value, stream)?;
                        }
                        Ok(())
                    }
                    _ => Err(PropertyError::TypeMismatch { meta: "sequence" }),
                }),
                decode: Box::new(move |stream| {
                    let count = if size == 0 {
                        stream
                            .read_packed_int()
                            .map_err(|_| PropertyError::MissingSequenceSize)?
                            as usize
                    } else {
                        size as usize
                    };
                    check_sequence_capacity(element_size, count, stream.remaining() as i32)?;
                    let mut values = Vec::with_capacity(count);
                    for _ in 0..count {
                        values.push((decode_inner.decode)(stream)?);
                    }
                    Ok(PropertyValue::Sequence(values))
                }),
            })
        }
        Kind::FixedDict {
            allow_none, fields, ..
        } => {
            let allow_none = *allow_none;
            let names: Vec<String> = fields.iter().map(|field| field.name.clone()).collect();
            // The compiled children are shared by both directions through
            // `Rc` (the closures move their captures, and a codec is not
            // `Clone`).
            let codecs: Vec<Rc<GeneratedCodec>> = fields
                .iter()
                .map(|field| generate(field.type_.kind()).map(Rc::new))
                .collect::<Option<_>>()?;
            let width = fields.len();
            let decode_codecs = codecs.clone();
            Some(GeneratedCodec {
                encode: Box::new(move |value, stream| match value {
                    PropertyValue::FixedDict(None) => {
                        if !allow_none {
                            return Err(PropertyError::TypeMismatch { meta: "fixed_dict" });
                        }
                        let _ = stream.write_u8(0);
                        Ok(())
                    }
                    PropertyValue::FixedDict(Some(values)) => {
                        if allow_none {
                            let _ = stream.write_u8(1);
                        }
                        for (codec, pair) in codecs.iter().zip(values.iter()) {
                            (codec.encode)(&pair.1, stream)?;
                        }
                        Ok(())
                    }
                    _ => Err(PropertyError::TypeMismatch { meta: "fixed_dict" }),
                }),
                decode: Box::new(move |stream| {
                    let is_none = if allow_none {
                        stream
                            .read_u8()
                            .map_err(|_| PropertyError::MissingNoneIndicator)?
                            == 0
                    } else {
                        false
                    };
                    if is_none {
                        return Ok(PropertyValue::FixedDict(None));
                    }
                    let mut values = Vec::with_capacity(width);
                    for (name, codec) in names.iter().zip(decode_codecs.iter()) {
                        values.push((name.clone(), (codec.decode)(stream)?));
                    }
                    Ok(PropertyValue::FixedDict(Some(values)))
                }),
            })
        }
        Kind::MailBox => Some(GeneratedCodec {
            encode: Box::new(|value, stream| match value {
                PropertyValue::MailBox { id, ip, port, salt } => {
                    let _ = stream.write_i32(*id);
                    let _ = stream.write_u32(*ip);
                    let _ = stream.write_u16(*port);
                    let _ = stream.write_u16(*salt);
                    Ok(())
                }
                _ => Err(PropertyError::TypeMismatch { meta: "mailbox" }),
            }),
            decode: Box::new(|stream| {
                let id = stream
                    .read_i32()
                    .map_err(|_| PropertyError::StreamTruncated {
                        element: SIMPLE_STREAM_ELEMENT,
                    })?;
                let ip = stream
                    .read_u32()
                    .map_err(|_| PropertyError::StreamTruncated {
                        element: SIMPLE_STREAM_ELEMENT,
                    })?;
                let port = stream
                    .read_u16()
                    .map_err(|_| PropertyError::StreamTruncated {
                        element: SIMPLE_STREAM_ELEMENT,
                    })?;
                let salt = stream
                    .read_u16()
                    .map_err(|_| PropertyError::StreamTruncated {
                        element: SIMPLE_STREAM_ELEMENT,
                    })?;
                Ok(PropertyValue::MailBox { id, ip, port, salt })
            }),
        }),
        // The interpretive stream path rejects these before any stream
        // work (`Unsupported(kind.meta_name())`), and the fixtures
        // contain none of them.
        Kind::Class { .. } | Kind::UdoRef | Kind::Python | Kind::UserType { .. } => None,
    }
}

/// The `read` of a fixed-width integer: the low `bytes` bytes,
/// little-endian, exactly as the crate's `read_integer`.
fn read_raw_integer(stream: &mut BinaryStream, bytes: i32) -> Result<u64, PropertyError> {
    assert!(bytes > 0 && bytes <= 8, "integer byte width {bytes}");
    let width = bytes as usize;
    let mut pattern = [0u8; 8];
    pattern[..width].copy_from_slice(&stream.read_blob_raw(width).map_err(|_| {
        PropertyError::StreamTruncated {
            element: SIMPLE_STREAM_ELEMENT,
        }
    })?);
    Ok(u64::from_le_bytes(pattern))
}

// ---------------------------------------------------------------------------
// The record sink (the baseline's shape)
// ---------------------------------------------------------------------------

/// Collected `metric<TAB>value` rows, printed and appended to
/// `$CODEC_GEN_OUT` when set.
#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: &str, value: impl std::fmt::Display) {
        self.0.push((metric.to_string(), value.to_string()));
    }

    fn flush(&self) {
        let mut text = String::new();
        for (k, v) in &self.0 {
            text.push_str(&format!("{k}\t{v}\n"));
        }
        print!("{text}");
        if let Ok(path) = std::env::var("CODEC_GEN_OUT") {
            std::fs::write(&path, &text).expect("write CODEC_GEN_OUT");
            println!("record: {path}");
        }
    }
}

/// The p50/p90 summary of a sample set, in the recorded unit.
fn summarize(samples: &mut [f64]) -> [f64; 2] {
    samples.sort_by(|a, b| a.total_cmp(b));
    let pick = |p: f64| {
        let idx = ((p * samples.len() as f64).ceil() as usize).clamp(1, samples.len()) - 1;
        samples[idx]
    };
    [pick(0.50), pick(0.90)]
}

fn put_summary(record: &mut Record, metric: &str, summary: [f64; 2]) {
    for (name, value) in ["p50", "p90"].iter().zip(summary) {
        record.put(&format!("{metric}_{name}"), format!("{value:.1}"));
    }
}

// ---------------------------------------------------------------------------
// The fixture walk (the baseline's factory path)
// ---------------------------------------------------------------------------

/// `EntityDescriptionMapFactory::create`, as in `entitydef_baseline.rs`.
fn factory_create(root: &std::path::Path) -> EntityDescriptionMap {
    let resolver = FsResolver::new(root);

    let mut ctx = entitydef::data_type::TypeContext::default();
    let aliases_resolver = resolver.clone();
    ctx.init_aliases(move || aliases_resolver.open_section(entitydef::constants::ALIASES_FILE));

    let entities = resolver
        .open_section(entitydef::constants::ENTITIES_FILE)
        .expect("the factory opens scripts/entities.xml");

    let mut map = EntityDescriptionMap::new();
    let mut diag = Diagnostics::new();
    assert!(
        map.parse(
            Some(&entities),
            &resolver,
            &ctx,
            &mut diag,
            &MessageRanges::default()
        ),
        "factory parse failed: {:?}",
        diag.messages()
    );
    assert!(diag.is_empty(), "clean load: {:?}", diag.messages());
    map
}

/// One property of one entity, with its parsed default and the compiled
/// codec — the unit both legs walk.
struct Sample {
    family: String,
    value: PropertyValue,
    data_type: Arc<DataType>,
    codec: GeneratedCodec,
}

/// Compiles every property of every entity in both fixtures. Panics on a
/// kind the interpretive stream path itself rejects (the baseline's
/// `.expect("addToStream")` would fail there too).
fn collect_samples() -> (Vec<Sample>, u128, usize) {
    let mut samples = Vec::new();
    let mut unsupported = 0usize;
    let mut build_us = 0u128;
    for fixture in [
        "docs/review/fixtures/full_def/res",
        "docs/review/fixtures/fantasydemo_def/res",
    ] {
        let root = std::path::Path::new(REPO_ROOT).join(fixture);
        let map = factory_create(&root);
        for index in 0..map.size() {
            let description = map.entity_description(index as u16).unwrap();
            for property in description.properties() {
                let kind = property.data_type.kind();
                // The generation window covers only the compile of the
                // kind tree; the sample materialisation around it (the
                // parsed default and the owned type clone) is untimed.
                let started = Instant::now();
                let codec = generate(kind);
                build_us += started.elapsed().as_micros();
                match codec {
                    Some(codec) => samples.push(Sample {
                        family: property.data_type.type_name(),
                        value: property.initial_value().expect("a parsed default"),
                        data_type: property.data_type.clone(),
                        codec,
                    }),
                    None => unsupported += 1,
                }
            }
        }
    }
    (samples, build_us, unsupported)
}

// ---------------------------------------------------------------------------
// The legs
// ---------------------------------------------------------------------------

/// Leg 1: byte identity. For every sample the generated encode produces
/// exactly the interpretive bytes, and both decoders return the parsed
/// default from either encoding.
#[test]
fn the_generated_codec_is_byte_identical_to_the_interpretive_one() {
    clear_section_cache();
    let (samples, _, unsupported) = collect_samples();
    // The interpretive baseline `.expect`s a successful `addToStream` on
    // every property, so a fixture kind outside the supported set is a
    // fixture change this evaluation must notice.
    assert_eq!(unsupported, 0, "every fixture kind is generatable");
    assert!(!samples.is_empty());
    for sample in &samples {
        let mut interpretive = BinaryStream::new();
        sample
            .data_type
            .add_to_stream(&sample.value, &mut interpretive)
            .expect("interpretive encode");
        let mut generated = BinaryStream::new();
        (sample.codec.encode)(&sample.value, &mut generated).expect("generated encode");
        assert_eq!(
            interpretive.as_bytes(),
            generated.as_bytes(),
            "{}: the generated codec writes the interpretive bytes",
            sample.family
        );

        let mut from_interpretive = BinaryStream::new();
        from_interpretive.write_bytes(interpretive.as_bytes());
        assert_eq!(
            (sample.codec.decode)(&mut from_interpretive).expect("generated decode"),
            sample.value,
            "{}: the generated decoder reads the interpretive bytes",
            sample.family
        );

        let mut from_generated = BinaryStream::new();
        from_generated.write_bytes(generated.as_bytes());
        assert_eq!(
            sample
                .data_type
                .create_from_stream(&mut from_generated)
                .expect("interpretive decode"),
            sample.value,
            "{}: the interpretive decoder reads the generated bytes",
            sample.family
        );
    }
}

/// Leg 2: the cost report. Per family, encode-only and decode-only
/// nanoseconds on both paths, plus the one-off generation cost.
#[test]
fn the_generated_codec_serialization_cost_is_reported() {
    clear_section_cache();
    let (samples, build_us, unsupported) = collect_samples();
    assert_eq!(unsupported, 0, "every fixture kind is generatable");

    // Family -> four sample sets: interpretive/generated × encode/decode.
    let mut by_family: Vec<(String, [Vec<f64>; 4])> = Vec::new();

    for sample in &samples {
        let family = sample.family.as_str();
        let sets = match by_family.iter_mut().find(|(name, _)| name == family) {
            Some((_, sets)) => sets,
            None => {
                by_family.push((family.to_string(), Default::default()));
                &mut by_family.last_mut().unwrap().1
            }
        };

        for _ in 0..ROUNDS {
            // Encode: the sink is constructed and dropped outside the
            // clock window; only the encode call is timed.
            let mut sink = BinaryStream::new();
            let started = Instant::now();
            sample
                .data_type
                .add_to_stream(&sample.value, &mut sink)
                .expect("interpretive encode");
            sets[0].push(started.elapsed().as_nanos() as f64);

            let mut sink = BinaryStream::new();
            let started = Instant::now();
            (sample.codec.encode)(&sample.value, &mut sink).expect("generated encode");
            sets[1].push(started.elapsed().as_nanos() as f64);

            // Decode: one pre-encoded buffer, `rewind` before each
            // window, only the decode call timed.
            let mut source = BinaryStream::new();
            sample
                .data_type
                .add_to_stream(&sample.value, &mut source)
                .expect("encode for the decode faces");

            source.rewind();
            let started = Instant::now();
            let read_back = sample
                .data_type
                .create_from_stream(&mut source)
                .expect("interpretive decode");
            sets[2].push(started.elapsed().as_nanos() as f64);
            assert_eq!(read_back, sample.value);

            source.rewind();
            let started = Instant::now();
            let read_back = (sample.codec.decode)(&mut source).expect("generated decode");
            sets[3].push(started.elapsed().as_nanos() as f64);
            assert_eq!(read_back, sample.value);
        }
    }

    let mut record = Record::default();
    record.put("ser_rounds_per_face", ROUNDS);
    record.put("ser_properties", samples.len());
    record.put("ser_unsupported_kinds", unsupported);
    record.put("ser_codec_generation_us", build_us);
    for (family, mut sets) in by_family {
        put_summary(
            &mut record,
            &format!("ser_{family}_interp_encode"),
            summarize(&mut sets[0]),
        );
        put_summary(
            &mut record,
            &format!("ser_{family}_gen_encode"),
            summarize(&mut sets[1]),
        );
        put_summary(
            &mut record,
            &format!("ser_{family}_interp_decode"),
            summarize(&mut sets[2]),
        );
        put_summary(
            &mut record,
            &format!("ser_{family}_gen_decode"),
            summarize(&mut sets[3]),
        );
    }
    record.flush();
}
