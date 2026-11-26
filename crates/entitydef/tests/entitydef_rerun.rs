//! OPT-04-01-04: the rerun of the definition digests and the byte
//! fixtures, after the OPT-04-01 optimization section (the definition
//! parse cache and the codec evaluation).
//!
//! Acceptance: the artifacts are **identical**. Three legs:
//!
//! 1. **Definition digests** — every in-repo definition tree is loaded
//!    through the engine load path and its digest must equal the
//!    captured oracle (`docs/review/reference/*_entitydef.json`). The
//!    digests are recorded so the rerun leaves its own artifact.
//! 2. **Entity snapshot bytes** — the `Avatar` entity of the
//!    `fantasydemo_def` tree, serialized from the capture's fixed input
//!    section through the reference's `EntityDescription::addSectionToStream`
//!    pass order (`BASE_DATA|CELL_DATA|CLIENT_DATA`), must reproduce the
//!    15 captured bytes (`docs/review/fixtures/artifacts/entity-Avatar.snapshot.hex`).
//! 3. **Database row bytes** — the same entity through
//!    `BASE_DATA|CELL_DATA|ONLY_PERSISTENT_DATA` plus the
//!    `canBeOnCell` spatial suffix (`position`, `direction`, `spaceID`,
//!    the `XMLDatabase::getEntity` row formula) must reproduce the 38
//!    captured bytes (`database-Avatar.row.hex`).
//!
//! The pass order mirrors `EntityDescription::shouldConsiderData`
//! (`entity_description.cpp`): four passes — base-not-client,
//! base-and-client, cell-and-client, cell-not-client — each walking the
//! properties in declaration order, filtered by persistence and
//! other-client bits when the corresponding ONLY_* flag is set.

use std::io::Write as _;

use entitydef::data_description::DataDescription;
use entitydef::diagnostics::Diagnostics;
use entitydef::entity_description::{DefResolver, FsResolver, clear_section_cache};
use entitydef::entity_map::{EntityDescriptionMap, MessageRanges};
use foundation::stream::BinaryStream;
use resources::datasection;

/// The repository root from the crate manifest (`crates/entitydef` up
/// two levels).
const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

// ---------------------------------------------------------------------------
// The record sink (the baseline harness shape)
// ---------------------------------------------------------------------------

/// Collected `metric<TAB>value` rows, printed and written to
/// `$DEF_RERUN_OUT` when set.
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
        if let Ok(path) = std::env::var("DEF_RERUN_OUT") {
            std::fs::write(&path, &text).expect("write DEF_RERUN_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

// ---------------------------------------------------------------------------
// The load path (the baseline harness factory)
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
    // The `entity_index` tree ships no `alias.xml`, so the load carries
    // that one warning there (`differential.rs` tolerates the same);
    // the digest equality below is the acceptance gate, not the
    // diagnostic channel.
    map
}

/// The digest line of a captured oracle artifact, without pulling in a
/// JSON parser (the `entitydef_baseline` pattern).
fn oracle_digest(oracle_file: &str) -> String {
    let path = format!("{REPO_ROOT}/docs/review/reference/{oracle_file}");
    let text = std::fs::read_to_string(&path).expect("oracle file readable");
    let marker = "\"digest\": \"";
    let start = text.find(marker).expect("digest in oracle") + marker.len();
    text[start..start + 32].to_string()
}

// ---------------------------------------------------------------------------
// Leg 1: the definition digests
// ---------------------------------------------------------------------------

fn rerun_digests(record: &mut Record) {
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
        (
            "service",
            "docs/review/fixtures/service_def/res",
            "service_entitydef.json",
        ),
        (
            "entity_index",
            "docs/review/fixtures/entity_index/res",
            "entity_index_entitydef.json",
        ),
    ] {
        let root = std::path::Path::new(REPO_ROOT).join(fixture);
        let map = factory_create(&root);
        let digest = map.digest().to_owned();
        // The acceptance: identical to the captured artifact.
        assert_eq!(
            digest,
            oracle_digest(oracle),
            "{name}: the rerun digest equals the oracle"
        );
        record.put(&format!("rerun_{name}_digest"), digest);
        record.put(&format!("rerun_{name}_digest_match"), 1);
    }
    // The fifth tree lives outside this checkout and rides an env path,
    // exactly as `differential.rs` reads it; recorded as skipped so the
    // rerun artifact says so honestly.
    match std::env::var("BW04_CI_RES") {
        Ok(path) => {
            let map = factory_create(&std::path::PathBuf::from(path));
            let digest = map.digest().to_owned();
            assert_eq!(
                digest,
                oracle_digest("client_integration_entitydef.json"),
                "client_integration: the rerun digest equals the oracle"
            );
            record.put("rerun_client_integration_digest", digest);
            record.put("rerun_client_integration_digest_match", 1);
        }
        Err(_) => {
            record.put(
                "rerun_client_integration_digest_match",
                "skipped: no BW04_CI_RES",
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Legs 2 and 3: the Avatar byte fixtures
// ---------------------------------------------------------------------------

/// The capture's fixed input value section (`capture_artifacts.cpp`):
/// the properties plus the spatial data the row formula reads.
const VALUE_XML: &str = "<root>\n\
     <playerName>Admiral</playerName>\n\
     <prop1>100</prop1>\n\
     <prop2>200</prop2>\n\
     <prop3>300</prop3>\n\
     <position>1 2 3</position>\n\
     <direction>0 0 0</direction>\n\
     <spaceID>7</spaceID>\n\
     </root>\n";

/// `EntityDescription::shouldConsiderData`: the four-pass filter. The
/// ONLY_* faces degrade to booleans here — the capture never sets
/// `ONLY_OTHER_CLIENT_DATA`, and the harness keeps the parameter so the
/// filter reads exactly like the reference.
fn should_consider(
    pass: usize,
    prop: &DataDescription,
    only_persistent: bool,
    only_other_client: bool,
) -> bool {
    const PASS_FILTER: [[bool; 2]; 4] = [
        [true, false],  // base and not client
        [true, true],   // base and client
        [false, true],  // cell and client
        [false, false], // cell and not client
    ];
    PASS_FILTER[pass][0] == prop.is_base_data()
        && PASS_FILTER[pass][1] == prop.is_client_server_data()
        && (prop.is_other_client_data() || !only_other_client)
        && (prop.data_flags.is_persistent() || !only_persistent)
}

/// The captured hex line of a byte fixture, lower-case, whitespace-free.
fn artifact_hex(name: &str) -> String {
    let path = format!("{REPO_ROOT}/docs/review/fixtures/artifacts/{name}");
    std::fs::read_to_string(&path)
        .expect("byte fixture readable")
        .trim()
        .to_string()
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `EntityDescription::addSectionToStream`: the four passes over the
/// declaration order, each considered property taken from the input
/// section (the capture's `FromSection` visitor: a missing child stands
/// in as the property default — every property is present here).
fn add_section_to_stream(
    properties: &[&DataDescription],
    values: &datasection::DataSection,
    stream: &mut BinaryStream,
    only_persistent: bool,
) {
    for pass in 0..4usize {
        for prop in properties.iter() {
            if !should_consider(pass, prop, only_persistent, false) {
                continue;
            }
            let value = match values.find_child(prop.name()) {
                Some(child) => prop.data_type.value_from_section(child),
                None => prop.initial_value(),
            }
            .expect("the fixture value converts");
            prop.data_type
                .add_to_stream(&value, stream)
                .expect("the fixture value encodes");
        }
    }
}

fn rerun_avatar_fixtures(record: &mut Record) {
    let root = std::path::Path::new(REPO_ROOT).join("docs/review/fixtures/fantasydemo_def/res");
    let map = factory_create(&root);
    let avatar = (0..map.size())
        .map(|index| map.entity_description(index as u16).unwrap())
        .find(|description| description.name == "Avatar")
        .expect("the fantasydemo tree defines Avatar");

    let values = datasection::parse("values", VALUE_XML.as_bytes()).expect("fixed input parses");
    let properties: Vec<&DataDescription> = avatar.properties().iter().collect();

    // Leg 2: the entity snapshot — all data domains.
    let mut snapshot = BinaryStream::new();
    add_section_to_stream(&properties, &values, &mut snapshot, false);
    let snapshot_hex = hex_of(snapshot.as_bytes());
    let snapshot_oracle = artifact_hex("entity-Avatar.snapshot.hex");
    assert_eq!(
        snapshot_hex, snapshot_oracle,
        "the rerun snapshot equals the captured 15 bytes"
    );
    record.put("rerun_avatar_snapshot_hex", snapshot_hex);
    record.put("rerun_avatar_snapshot_bytes", snapshot.as_bytes().len());
    record.put("rerun_avatar_snapshot_match", 1);

    // Leg 3: the database row — the persistent projection, then the
    // spatial suffix when the entity can be on a cell
    // (`XMLDatabase::getEntity`).
    let mut row = BinaryStream::new();
    add_section_to_stream(&properties, &values, &mut row, true);
    if avatar.can_be_on_cell() {
        for field in ["position", "direction"] {
            let child = values.find_child(field).expect("spatial child present");
            for token in child.as_string().split_whitespace() {
                let _ = row.write_f32(token.parse::<f32>().expect("float component"));
            }
        }
        let space_id: i32 = values
            .read_string("spaceID", "")
            .trim()
            .parse()
            .expect("spaceID integer");
        let _ = row.write_i32(space_id);
    }
    let row_hex = hex_of(row.as_bytes());
    let row_oracle = artifact_hex("database-Avatar.row.hex");
    assert_eq!(
        row_hex, row_oracle,
        "the rerun row equals the captured 38 bytes"
    );
    record.put("rerun_avatar_row_hex", row_hex);
    record.put("rerun_avatar_row_bytes", row.as_bytes().len());
    record.put("rerun_avatar_row_match", 1);
}

/// Runs every leg in fixed order and leaves the rerun artifact behind.
#[test]
fn the_definition_digest_and_byte_fixtures_rerun_identically() {
    let mut record = Record::default();
    clear_section_cache();
    rerun_digests(&mut record);
    rerun_avatar_fixtures(&mut record);
    record.flush();
}
