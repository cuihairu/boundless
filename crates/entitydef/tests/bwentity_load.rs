//! BW-04-04-04: the `lib/bwentity` contract — `EntityDescriptionMapFactory::
//! create(paths)` initializes the resource paths, opens
//! `scripts/entities.xml`, parses every definition into an
//! `EntityDescriptionMap` (`size`, `nameToIndex`, `entityDescription`), and
//! hands it to the caller for navigation. These tests drive the same load
//! path and then pin the result against the generated artifacts
//! (`docs/review/reference/*.json`) from BW-04-04-03.

use entitydef::constants;
use entitydef::data_type::TypeContext;
use entitydef::diagnostics::Diagnostics;
use entitydef::entity_description::{DefResolver, FsResolver};
use entitydef::entity_map::{EntityDescriptionMap, MessageRanges};

const ORACLE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/review/reference");

/// `EntityDescriptionMapFactory::create`: res paths in, navigable map out
/// (parse must succeed — the reference factory only logs a failure).
fn factory_create(root: &std::path::Path) -> EntityDescriptionMap {
    let resolver = FsResolver::new(root);

    let mut ctx = TypeContext::default();
    let aliases_resolver = resolver.clone();
    ctx.init_aliases(move || aliases_resolver.open_section(constants::ALIASES_FILE));

    let entities = resolver
        .open_section(constants::ENTITIES_FILE)
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

/// The digest line of a captured artifact, without pulling in a JSON parser.
fn oracle_digest(oracle_file: &str) -> String {
    let path = format!("{ORACLE_DIR}/{oracle_file}");
    let text = std::fs::read_to_string(&path).expect("oracle file readable");
    let marker = "\"digest\": \"";
    let start = text.find(marker).expect("digest in oracle") + marker.len();
    text[start..start + 32].to_string()
}

/// `nameToIndex` and `entityDescription` round trip for every entity, and the
/// digest/maxima agree with the artifact.
fn check_navigation(map: &EntityDescriptionMap, oracle_file: &str) {
    for (name, index) in map.name_to_index_entries() {
        assert_eq!(map.name_to_index(name), Some(index), "{name} resolves");
        assert_eq!(
            map.entity_description(index).expect("index valid").name,
            name,
            "index {index} round trips"
        );
    }

    assert_eq!(map.digest(), oracle_digest(oracle_file), "digest");
}

/// full_def: three entities across all three script halves, methods and
/// properties navigable exactly as `bwentity` consumers see them.
#[test]
fn factory_loads_the_full_def_scripts() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("a crates/ parent")
        .parent()
        .expect("the repository root");
    let map = factory_create(&repo_root.join("docs/review/fixtures/full_def/res"));

    assert_eq!(map.size(), 3);
    assert_eq!(map.name_to_index("Avatar"), Some(0));
    assert_eq!(map.name_to_index("Sensor"), Some(1));
    assert_eq!(map.name_to_index("Beacon"), Some(2));
    check_navigation(&map, "full_entitydef.json");

    let avatar = map.entity_description(0).unwrap();
    assert_eq!(avatar.base().size(), 2, "order and save");
    assert_eq!(avatar.cell().size(), 1, "teleport");
    assert_eq!(avatar.client().size(), 1, "ping");
    assert_eq!(avatar.client_index(), 0, "client-visible");
    assert_eq!(
        avatar.property_count(),
        7,
        "EDITOR_ONLY secret is not a runtime property"
    );
    assert!(
        !avatar.properties().iter().any(|p| p.name() == "secret"),
        "the EDITOR_ONLY property is dropped like the reference does"
    );
    let dbid = avatar
        .properties()
        .iter()
        .find(|p| p.name() == "dbid")
        .expect("dbid");
    assert!(dbid.data_flags.is_identifier());
    assert!(dbid.database_indexing.is_indexed() && dbid.database_indexing.is_unique());

    let sensor = map.entity_description(1).unwrap();
    assert_eq!(sensor.client_index(), 65535, "server-only sentinel");
    let calibrate = sensor.base().internal_method(0).unwrap();
    assert_eq!(calibrate.name(), "calibrate");
    assert!(calibrate.has_return_values());

    let beacon = map.entity_description(2).unwrap();
    assert_eq!(beacon.cell().size(), 1, "flash");
    assert_eq!(beacon.client().size(), 0, "beacon faces no client");
}

/// The reference client-integration example loads through the factory and its
/// digest matches the captured artifact (needs BW04_CI_RES).
#[test]
fn factory_loads_the_client_integration_example() {
    let Some(root) = std::env::var_os("BW04_CI_RES") else {
        eprintln!("skipping: set BW04_CI_RES to the client_integration res root");
        return;
    };
    let map = factory_create(std::path::Path::new(&root));
    assert!(map.size() > 0);
    check_navigation(&map, "client_integration_entitydef.json");
}

/// The fantasydemo tree loads through the factory and its digest matches the
/// captured artifact (needs BW04_FD_RES).
#[test]
fn factory_loads_the_fantasydemo_scripts() {
    let Some(root) = std::env::var_os("BW04_FD_RES") else {
        eprintln!("skipping: set BW04_FD_RES to the fantasydemo res root");
        return;
    };
    let map = factory_create(std::path::Path::new(&root));
    assert!(map.size() > 0);
    check_navigation(&map, "fantasydemo_entitydef.json");
}
