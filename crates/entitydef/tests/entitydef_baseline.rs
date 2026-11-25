//! OPT-04-01-01 / OPT-04-01-02 / OPT-04-01-03: the EntityDef baseline
//! measurements — startup and codec cost, cold against warm, the same
//! harness serving every OPT-04-01 comparison.
//!
//! The acceptance saves baselines for the entity-definitions startup and
//! encode/decode cost against the tracked fixtures the way the engine
//! loads them (`EntityDescriptionMapFactory::create`: res paths in,
//! `scripts/entities.xml` opened, every definition parsed). Three legs:
//!
//! 1. **Startup** — a cold `factory_create` of each fixture tree, timed;
//!    the parse must succeed, the diagnostics must be clean, and the
//!    definition digest must equal the captured oracle, so a broken load
//!    cannot produce a timing row at all. The digest string itself is
//!    recorded, not asserted: the fixture is pinned separately by the
//!    `bwentity_load` contract tests.
//! 2. **Codec** — every property of every entity in the fixtures,
//!    round-tripped through `DataType::addToStream` /
//!    `createFromStream` from its parsed default: the bytes go out, come
//!    back equal, and the per-family nanoseconds are recorded. This is
//!    the interpretive codec (per-`Kind` dispatch) any pre-generated
//!    codec must match byte for byte.
//! 3. **Warm start** — the same fixture loaded a second time in the same
//!    process, so the operating-system page cache is warm and the number
//!    measures the parse, not the file reads.
//!
//! Structural assertions run on every execution. Wall-clock numbers are
//! **recorded, never asserted tight** — raw milliseconds and nanoseconds
//! are machine-dependent, as with the network and Python-bridge
//! baselines. The record sink writes `metric<TAB>value` rows to stdout
//! and, when `DEF_BASELINE_OUT` is set, appends to that path in the
//! network/load fixture format.

use std::io::Write as _;
use std::time::Instant;

use entitydef::data_type::TypeContext;
use entitydef::diagnostics::Diagnostics;
use entitydef::entity_description::{DefResolver, FsResolver, clear_section_cache};
use entitydef::entity_map::{EntityDescriptionMap, MessageRanges};
use foundation::stream::BinaryStream;

/// The repository root from the crate manifest (`crates/entitydef` up
/// two levels).
const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
/// Round-trips per property family in the codec leg.
const ROUNDS: usize = 20_000;

// ---------------------------------------------------------------------------
// The record sink
// ---------------------------------------------------------------------------

/// Collected `metric<TAB>value` rows. Printed to stdout on flush and
/// written to `$DEF_BASELINE_OUT` when set (the network/load fixture
/// format).
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
        if let Ok(path) = std::env::var("DEF_BASELINE_OUT") {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .expect("write DEF_BASELINE_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

/// The p50/p90/p99/max/min summary of a sample set, in the recorded unit.
fn summarize(samples: &mut [f64]) -> [f64; 5] {
    samples.sort_by(|a, b| a.total_cmp(b));
    let pick = |p: f64| {
        let idx = ((p * samples.len() as f64).ceil() as usize).clamp(1, samples.len()) - 1;
        samples[idx]
    };
    [
        pick(0.50),
        pick(0.90),
        pick(0.99),
        samples[samples.len() - 1],
        samples[0],
    ]
}

/// Records one family's summary under `prefix_{p50,p90,p99,max,min}`.
fn put_summary(record: &mut Record, prefix: &str, summary: [f64; 5]) {
    let names = ["p50", "p90", "p99", "max", "min"];
    for (name, value) in names.iter().zip(summary) {
        record.put(&format!("{prefix}_{name}"), format!("{value:.1}"));
    }
}

// ---------------------------------------------------------------------------
// The factory (the load path under measurement)
// ---------------------------------------------------------------------------

/// `EntityDescriptionMapFactory::create`: res paths in, navigable map out.
/// Mirrors `crates/entitydef/tests/bwentity_load.rs`.
fn factory_create(root: &std::path::Path) -> EntityDescriptionMap {
    let resolver = FsResolver::new(root);

    let mut ctx = TypeContext::default();
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

/// The digest line of a captured artifact, without pulling in a JSON
/// parser (the `bwentity_load` pattern).
fn oracle_digest(oracle_file: &str) -> String {
    let path = format!("{REPO_ROOT}/docs/review/reference/{oracle_file}");
    let text = std::fs::read_to_string(&path).expect("oracle file readable");
    let marker = "\"digest\": \"";
    let start = text.find(marker).expect("digest in oracle") + marker.len();
    text[start..start + 32].to_string()
}

// ---------------------------------------------------------------------------
// The legs
// ---------------------------------------------------------------------------

/// Leg 1: a cold startup of each fixture tree — the full factory create,
/// file reads and XML parse included, exactly as a process pays it.
fn startup_leg(record: &mut Record) {
    for (name, fixture, oracle) in [
        (
            "full",
            "docs/review/fixtures/full_def/res",
            "full_entitydef.json",
        ),
        (
            "fantasydemo",
            "docs/review/fixtures/fantasydemo_def/res",
            "fantasydemo_entitydef.json",
        ),
    ] {
        let root = std::path::Path::new(REPO_ROOT).join(fixture);
        let started = Instant::now();
        let map = factory_create(&root);
        let elapsed = started.elapsed();
        assert!(map.size() > 0, "{name}: at least one entity");
        // The digest equals the captured artifact: the fixture is pinned
        // independently by the `bwentity_load` contract tests; recorded
        // here so a load that silently changed shape shows in the record.
        assert_eq!(
            map.digest(),
            oracle_digest(oracle),
            "{name}: pinned fixture"
        );
        record.put(&format!("startup_{name}_entities"), map.size());
        record.put(&format!("startup_{name}_digest"), map.digest());
        record.put(
            &format!("startup_{name}_ms"),
            format!("{:.3}", elapsed.as_secs_f64() * 1000.0),
        );
    }
}

/// Leg 2: the codec — every property of every entity, round-tripped
/// through `DataType::addToStream` / `createFromStream` from its parsed
/// default. The value comes out of the stream equal to what went in; the
/// per-type nanoseconds are recorded (one sample per round-trip, the
/// round-trip itself inside the clock window).
fn codec_leg(record: &mut Record) {
    // Family name -> per-round-trip samples.
    let mut by_family: Vec<(String, Vec<f64>)> = Vec::new();

    for fixture in [
        "docs/review/fixtures/full_def/res",
        "docs/review/fixtures/fantasydemo_def/res",
    ] {
        let root = std::path::Path::new(REPO_ROOT).join(fixture);
        let map = factory_create(&root);
        for index in 0..map.size() {
            let description = map.entity_description(index as u16).unwrap();
            for property in description.properties() {
                let value = property.initial_value().expect("a parsed default");
                let family = property.data_type.type_name();
                let samples = match by_family.iter_mut().find(|(name, _)| name == &family) {
                    Some((_, samples)) => samples,
                    None => {
                        by_family.push((family, Vec::new()));
                        &mut by_family.last_mut().unwrap().1
                    }
                };
                for _ in 0..ROUNDS {
                    let started = Instant::now();
                    let mut stream = BinaryStream::new();
                    property
                        .data_type
                        .add_to_stream(&value, &mut stream)
                        .expect("addToStream");
                    let read_back = property
                        .data_type
                        .create_from_stream(&mut stream)
                        .expect("createFromStream");
                    samples.push(started.elapsed().as_nanos() as f64);
                    assert_eq!(read_back, value, "the round-trip returns its input");
                }
            }
        }
    }

    record.put("codec_round_trips", ROUNDS);
    for (family, mut samples) in by_family {
        put_summary(record, &format!("codec_{family}"), summarize(&mut samples));
    }
}

/// Leg 3: the warm start — the same fixture loaded a second time in the
/// same process with the section cache already populated by the earlier
/// legs, so the number measures the cached reload (tree clones plus map
/// assembly, no file reads, no XML parse). The digest must match the
/// fresh load.
fn warm_leg(record: &mut Record) {
    let root = std::path::Path::new(REPO_ROOT).join("docs/review/fixtures/fantasydemo_def/res");
    let cold_digest = {
        let cold = factory_create(&root);
        cold.digest().to_owned()
    };
    let started = Instant::now();
    let map = factory_create(&root);
    let elapsed = started.elapsed();
    assert_eq!(
        map.digest(),
        cold_digest,
        "the warm load equals the cold one"
    );
    record.put(
        "startup_warm_fantasydemo_ms",
        format!("{:.3}", elapsed.as_secs_f64() * 1000.0),
    );
}

/// Runs every leg in fixed order. The section cache starts cleared so
/// the startup leg measures a cache-cold load and the warm leg measures
/// exactly the cached reload — the cold/warm pair the OPT-04-01-02
/// comparison reads.
#[test]
fn the_entitydef_baseline_runs_all_legs() {
    let mut record = Record::default();
    clear_section_cache();
    startup_leg(&mut record);
    codec_leg(&mut record);
    warm_leg(&mut record);
    record.flush();
}
