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

/// BW-14-01-01: the fantasydemo entity definitions load from the tracked
/// fixture, and every one of the five sample classes agrees with the summary
/// captured from the stock `process_defs` (`fantasydemo_entitydef.json`).
#[test]
fn fantasydemo_loads_the_five_sample_definitions() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("a crates/ parent")
        .parent()
        .expect("the repository root");
    let map = factory_create(&repo_root.join("docs/review/fixtures/fantasydemo_def/res"));

    // The ClientServerEntities list is exactly the five sample classes, in the
    // order the factory assigns indexes (Account, Avatar, Guard, Space, ClientOnly).
    assert_eq!(map.size(), 5, "five sample definitions");
    for (name, index) in [
        ("Account", 0),
        ("Avatar", 1),
        ("Guard", 2),
        ("Space", 3),
        ("ClientOnly", 4),
    ] {
        assert_eq!(map.name_to_index(name), Some(index), "{name} index");
        assert_eq!(
            map.entity_description(index).expect("index valid").name,
            name,
            "index {index} round trips"
        );
    }
    // Whole-set digest agrees with the reference `process_defs` capture.
    check_navigation(&map, "fantasydemo_entitydef.json");

    // Account: base-only proxy — two persistent base properties, one client
    // method, no cell part.
    let account = map.entity_description(0).unwrap();
    assert!(account.can_be_on_base() && !account.can_be_on_cell() && account.can_be_on_client());
    assert!(account.is_persistent() && !account.is_service());
    assert_eq!(account.property_count(), 2);
    assert_eq!(account.client_server_property_count(), 1, "playerName only");
    assert_eq!(account.base().size(), 0);
    assert_eq!(account.cell().size(), 0);
    assert_eq!(account.client().size(), 1, "chatMessage");
    assert_eq!(account.client_index(), 0);
    assert!(account.identifier().is_none());
    for (i, (name, ty, persistent, identifier)) in [
        ("playerName", "STRING", true, false),
        ("character", "STRING", true, false),
    ]
    .iter()
    .enumerate()
    {
        let p = account.property(i).expect("property");
        assert_eq!(p.name(), *name);
        assert_eq!(p.data_type.type_name(), *ty);
        assert_eq!(p.data_flags.is_persistent(), *persistent);
        assert_eq!(p.data_flags.is_identifier(), *identifier);
    }

    // Avatar: player proxy — four properties, two client methods, one exposed
    // base method, all three halves present.
    let avatar = map.entity_description(1).unwrap();
    assert!(avatar.can_be_on_base() && avatar.can_be_on_cell() && avatar.can_be_on_client());
    assert!(avatar.is_persistent() && !avatar.is_service());
    assert_eq!(avatar.property_count(), 4);
    assert_eq!(
        avatar.client_server_property_count(),
        4,
        "all client-visible"
    );
    assert_eq!(avatar.base().size(), 1, "logOff");
    assert_eq!(avatar.cell().size(), 0);
    assert_eq!(avatar.client().size(), 2, "fromCell and cellTick");
    assert_eq!(avatar.client_index(), 1);
    assert_eq!(
        avatar.identifier().map(|p| p.name()),
        Some("playerName"),
        "the identifier property"
    );
    for (i, (name, ty, persistent, identifier)) in [
        ("playerName", "STRING", true, true),
        ("prop1", "INT32", false, false),
        ("prop2", "INT16", true, false),
        ("prop3", "INT8", false, false),
    ]
    .iter()
    .enumerate()
    {
        let p = avatar.property(i).expect("property");
        assert_eq!(p.name(), *name);
        assert_eq!(p.data_type.type_name(), *ty);
        assert_eq!(p.data_flags.is_persistent(), *persistent);
        assert_eq!(p.data_flags.is_identifier(), *identifier);
    }
    assert_eq!(avatar.base().internal_method(0).unwrap().name(), "logOff");
    assert_eq!(
        avatar.client().internal_method(0).unwrap().name(),
        "fromCell"
    );
    assert_eq!(
        avatar.client().internal_method(1).unwrap().name(),
        "cellTick"
    );

    // Guard: cell + client placeholder — one non-persistent client property.
    let guard = map.entity_description(2).unwrap();
    assert!(guard.can_be_on_base() && guard.can_be_on_cell() && guard.can_be_on_client());
    assert!(guard.is_persistent() && !guard.is_service());
    assert_eq!(guard.property_count(), 1);
    assert_eq!(guard.client_server_property_count(), 1, "name");
    assert_eq!(guard.base().size(), 0);
    assert_eq!(guard.cell().size(), 0);
    assert_eq!(guard.client().size(), 0);
    assert_eq!(guard.client_index(), 2);
    let name = guard.property(0).expect("name");
    assert_eq!(name.name(), "name");
    assert_eq!(name.data_type.type_name(), "STRING");
    assert!(!name.data_flags.is_persistent());

    // Space: the bootstrap space — no properties, no methods.
    let space = map.entity_description(3).unwrap();
    assert!(space.can_be_on_base() && space.can_be_on_cell() && space.can_be_on_client());
    assert!(space.is_persistent() && !space.is_service());
    assert_eq!(space.property_count(), 0);
    assert_eq!(space.base().size(), 0);
    assert_eq!(space.cell().size(), 0);
    assert_eq!(space.client().size(), 0);

    // ClientOnly: a client-only entity — two client properties, one client
    // method, no base or cell part.
    let client_only = map.entity_description(4).unwrap();
    assert!(
        !client_only.can_be_on_base()
            && !client_only.can_be_on_cell()
            && client_only.can_be_on_client()
    );
    assert!(!client_only.is_persistent() && !client_only.is_service());
    assert_eq!(client_only.property_count(), 2);
    assert_eq!(client_only.client_server_property_count(), 2);
    assert_eq!(client_only.base().size(), 0);
    assert_eq!(client_only.cell().size(), 0);
    assert_eq!(client_only.client().size(), 1, "method1");
    assert_eq!(client_only.client_index(), 4);
    assert_eq!(
        client_only.client().internal_method(0).unwrap().name(),
        "method1"
    );
    for (i, (name, ty, persistent)) in [("name", "STRING", false), ("prop1", "INT32", false)]
        .iter()
        .enumerate()
    {
        let p = client_only.property(i).expect("property");
        assert_eq!(p.name(), *name);
        assert_eq!(p.data_type.type_name(), *ty);
        assert_eq!(p.data_flags.is_persistent(), *persistent);
    }
}
