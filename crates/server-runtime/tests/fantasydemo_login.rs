//! BW-14-01-02: the fantasydemo account login and character creation, run
//! in-process against the tracked fixture
//! (`docs/review/fixtures/fantasydemo_def`, BW-14-01-01).
//!
//! The reference chain, per face:
//!
//! * the client wires a `ClientServerProtocolVersion` (subpatch, patch,
//!   minor, major) plus `LogOnParams`; `LoginApp::login` reads the version,
//!   rejects a mismatch, then parses the params
//!   (`server/loginapp/loginapp.cpp:916-999`, `login_request.rs`);
//! * `LoginHandler::login` asks the billing system for the account's entity
//!   key (`server/dbapp/login_handler.cpp:69-76`); the XML logon map answers
//!   from `db.xml`'s `_BigWorldInfo/LogOnMapping` items
//!   (`lib/db_storage_xml/xml_billing_system.cpp:35-113`, plain
//!   case-sensitive password compare);
//! * the machine then loads the entity (`onGetEntityCompleted`), checks it
//!   out — reserving its base mailbox or routing to the re-logon path
//!   (`login_handler.cpp:235-307`) — and dispatches `createEntity` to the
//!   BaseAppMgr;
//! * an unknown user with `shouldAcceptUnknownUsers` creates a remembered
//!   default entity: `putEntity` assigns `dbID = ++maxID_`
//!   (`xml_database.cpp:626`), then `setEntityKeyForAccount` writes the
//!   mapping back before the dispatch (`login_handler.cpp:313-342`);
//! * the account's persistent `character` property is the account -> Avatar
//!   pointer the scripts resolve with `BigWorld.createBaseFromDB`
//!   (`examples/client_integration/python/simple/res/scripts/base/Account.py`);
//!   a new character row comes from a script-created base's `writeToDB`,
//!   which allocates its id the same way.
//!
//! The acceptance: every callback the `LoginEntityHandler` machine hands
//! out must agree with what the XML database then persists and re-loads.
//! The multi-process run (`app::run`) awaits the role runtimes; this
//! verifies the login chain over the ported faces, storage included.

use db::lookup::{EntityDefIndex, LookupDefs, get_database_id_from_name};
use db::storage::{
    BaseMailbox as StorageMailbox, CheckOut, EntityKey as StorageKey, EntityStorage,
};
use db::xml::{XmlDatabase, XmlKind, XmlPropDef, XmlTypeDef, parse_document};
use entitydef::constants;
use entitydef::data_type::{DataType, Kind};
use entitydef::diagnostics::Diagnostics;
use entitydef::entity_description::{DefResolver, FsResolver};
use entitydef::entity_map::{EntityDescriptionMap, MessageRanges};
use entitydef::flags::{BASE_DATA, CELL_DATA, ONLY_PERSISTENT_DATA};
use foundation::ids::{EntityTypeId, PENDING_DATABASE_ID};
use mercury::endpoint::Address;
use server_runtime::billing::{
    AccountResolution, BillingConfig, EntityKey as BillingKey, LogonMapBillingSystem,
};
use server_runtime::login_entity::{
    BaseMailbox as HandlerMailbox, EntityDBKey, EntityStep, LoginEntityHandler,
};
use server_runtime::loginapp::{
    ClientServerProtocolVersion, LOG_ON_PARAMS_PASS_THRU, LogOnParams, LogOnStatus,
    LoginParseOutcome, MAX_LOGIN_MESSAGE_SIZE, parse_login_request,
};

/// The tracked fantasydemo fixture res tree.
const FIXTURE_RES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/fantasydemo_def/res"
);

/// The fixture's `db.xml` bytes.
fn fixture_db_xml() -> String {
    std::fs::read_to_string(format!("{FIXTURE_RES}/scripts/db.xml")).expect("fixture db.xml")
}

/// `EntityDescriptionMapFactory::create` over the fixture res tree
/// (`bwentity_load.rs`'s load path).
fn fixture_defs() -> EntityDescriptionMap {
    let resolver = FsResolver::new(std::path::Path::new(FIXTURE_RES));
    let mut ctx = entitydef::data_type::TypeContext::default();
    let aliases_resolver = resolver.clone();
    ctx.init_aliases(move || aliases_resolver.open_section(constants::ALIASES_FILE));
    let entities = resolver
        .open_section(constants::ENTITIES_FILE)
        .expect("the fixture has scripts/entities.xml");
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
        "fixture defs parse failed: {:?}",
        diag.messages()
    );
    assert!(diag.is_empty(), "clean load: {:?}", diag.messages());
    map
}

/// The storage face of one fixture property's value
/// (`db::xml`'s `XmlKind`; the production entitydef adapter lands with the
/// DBApp storage face, so this maps what the fixture uses and fails loudly
/// on anything else).
fn storage_kind(data_type: &DataType) -> XmlKind {
    match data_type.kind() {
        Kind::Int { signed, .. } => XmlKind::Int { signed: *signed },
        Kind::String | Kind::UnicodeString => XmlKind::String,
        other => panic!(
            "the fixture needs only int and string storage, got {}",
            other.meta_name()
        ),
    }
}

/// The DBApp def face: per type, the persistent properties in the
/// `BASE_DATA | CELL_DATA | ONLY_PERSISTENT_DATA` projection order and the
/// identifier's index (`EntityDefs::getIdentifierProperty`).
fn storage_defs(defs: &EntityDescriptionMap) -> Vec<XmlTypeDef> {
    let mut out = Vec::new();
    for index in 0..defs.size() {
        let desc = defs
            .entity_description(index as EntityTypeId)
            .expect("index valid");
        let mut properties = Vec::new();
        desc.visit(
            BASE_DATA | CELL_DATA | ONLY_PERSISTENT_DATA,
            &mut |property| {
                properties.push(XmlPropDef {
                    name: property.name().to_string(),
                    kind: storage_kind(&property.data_type),
                });
                true
            },
        );
        let identifier = desc.identifier().map(|ident| {
            properties
                .iter()
                .position(|prop| prop.name == ident.name())
                .expect("the identifier is in the projection")
        });
        out.push(XmlTypeDef {
            name: desc.name.clone(),
            type_id: index as EntityTypeId,
            properties,
            identifier,
        });
    }
    out
}

/// The fixture database: `startup` + `initEntityMap` over the original
/// `db.xml` (`xml_database.cpp:100-292`).
fn fixture_database(defs: &EntityDescriptionMap) -> XmlDatabase {
    XmlDatabase::parse(&fixture_db_xml(), storage_defs(defs)).expect("the fixture db.xml parses")
}

/// `XMLBillingSystem::initLogOnMapping` (`xml_billing_system.cpp:35-68`):
/// seed the logon map from the fixture's `_BigWorldInfo/LogOnMapping`
/// items; an `entityID` of 0 resolves through `entityName`
/// (`findEntityByName`).
fn seeded_billing(defs: &EntityDescriptionMap, database: &XmlDatabase) -> LogonMapBillingSystem {
    let document = parse_document(&fixture_db_xml()).expect("the fixture db.xml parses");
    let items = document
        .find(db::xml::INFO_SECTION)
        .and_then(|info| info.find(db::xml::LOG_ON_MAPPING_SECTION))
        .expect("the fixture carries a LogOnMapping");
    let mut billing = LogonMapBillingSystem::new(BillingConfig::new());
    for item in &items.children {
        let name = item.find("logOnName").expect("logOnName").value.clone();
        let password = item.find("password").expect("password").value.clone();
        let type_name = item.find("type").expect("type").value.clone();
        let type_id = defs
            .name_to_index(&type_name)
            .expect("the mapping's type is a fixture entity") as EntityTypeId;
        let entity_id: i64 = item
            .find("entityID")
            .expect("entityID")
            .value
            .parse()
            .expect("a numeric entityID");
        let db_id = if entity_id == 0 {
            database
                .find_entity_by_name(type_id, &item.find("entityName").expect("entityName").value)
                .expect("the name resolves")
        } else {
            entity_id
        };
        billing.add_mapping(
            &name,
            BillingKey {
                type_id,
                db_id: db_id as u32,
            },
            &password,
        );
    }
    billing
}

/// A login request on the wire: the version, then the params
/// (`LoginApp::login`'s two reads).
fn wire_request(username: &str, password: &str) -> foundation::stream::BinaryStream {
    let mut stream = foundation::stream::BinaryStream::new();
    ClientServerProtocolVersion::current().write_to_stream(&mut stream);
    LogOnParams::with_credentials(username, password, "")
        .add_to_stream(&mut stream, LOG_ON_PARAMS_PASS_THRU);
    stream
}

/// The accepted parse, asserted once so each test reads flat.
fn accepted_params(stream: &mut foundation::stream::BinaryStream) -> LogOnParams {
    match parse_login_request(stream, MAX_LOGIN_MESSAGE_SIZE) {
        LoginParseOutcome::Accepted { params } => params,
        other => panic!("the fixture client's request is accepted, got {other:?}"),
    }
}

/// The BaseApp mailbox the DBApp reserves for a checked-out entity.
fn storage_mailbox(entity_id: i32) -> StorageMailbox {
    StorageMailbox {
        addr: Address::new(0x7f00_0001, 20000),
        entity_id,
    }
}

/// The storage's checkout holder as the handler face sees it
/// (`login_entity::BaseMailbox`).
fn handler_holder(holder: &StorageMailbox, type_id: EntityTypeId) -> HandlerMailbox {
    HandlerMailbox {
        type_id,
        id: holder.entity_id as u32,
        addr: (holder.addr.ip, holder.addr.port),
    }
}

/// `login_handler.cpp`'s key face over the storage's id width.
fn storage_key(type_id: EntityTypeId, db_id: i64) -> StorageKey {
    StorageKey { type_id, db_id }
}

/// A fixture client: wire in, accepted params out.
fn login(billing: &LogonMapBillingSystem, username: &str, password: &str) -> AccountResolution {
    let mut stream = wire_request(username, password);
    let params = accepted_params(&mut stream);
    billing.get_entity_key_for_account(params.username(), params.password())
}

/// The per-type lookup face `BigWorld.createBaseFromDB` reads
/// (`LookupDefs` from the same projection `storage_defs` walks).
fn lookup_defs(defs: &EntityDescriptionMap) -> LookupDefs {
    let mut out = LookupDefs::new();
    for index in 0..defs.size() {
        let desc = defs
            .entity_description(index as EntityTypeId)
            .expect("index valid");
        let mut properties = Vec::new();
        desc.visit(
            BASE_DATA | CELL_DATA | ONLY_PERSISTENT_DATA,
            &mut |property| {
                properties.push(property.name().to_string());
                true
            },
        );
        let identifier = desc
            .identifier()
            .and_then(|ident| properties.iter().position(|name| name == ident.name()));
        out.insert(
            index as EntityTypeId,
            EntityDefIndex {
                properties,
                identifier,
            },
        );
    }
    out
}

/// The existing-account login: FantasyAccount/a walks
/// load -> check-out -> reserve -> dispatch, and every callback agrees
/// with the rows (`login_handler.cpp:69-407`).
#[test]
fn an_existing_account_login_walks_the_machine_and_matches_the_rows() {
    let defs = fixture_defs();
    let account_type = defs.name_to_index("Account").expect("Account") as EntityTypeId;
    let avatar_type = defs.name_to_index("Avatar").expect("Avatar") as EntityTypeId;
    let mut database = fixture_database(&defs);
    let billing = seeded_billing(&defs, &database);
    let mut handler = LoginEntityHandler::new();

    // The billing map answers with the fixture's Account key (entityID 2).
    let resolution = login(&billing, "FantasyAccount", "a");
    assert_eq!(
        resolution,
        AccountResolution::Success {
            entity_key: BillingKey {
                type_id: account_type,
                db_id: 2
            },
            data_for_client: String::new(),
            data_for_base_entity: String::new(),
        }
    );

    // `loadEntity(EntityDBKey(Account, 2))`, the bundle's dbID slot pinned.
    assert_eq!(
        handler.begin(&resolution, "FantasyAccount"),
        EntityStep::LoadByKey {
            key: EntityDBKey::new(account_type, 2, "")
        }
    );
    assert_eq!(handler.db_id_slot(), Some(2));

    // The row loads: the character pointer + playerName, nothing else is
    // persistent (`Account.def`). The projection order is the visit
    // order: the base-only `character` lands in the first pass, the
    // base+client `playerName` in the next.
    let record = database
        .get_entity(&storage_key(account_type, 2))
        .expect("get")
        .expect("the fixture account row");
    assert_eq!(record.type_id, account_type);
    assert_eq!(record.db_id, 2);
    assert_eq!(
        record.values,
        vec![
            entitydef::value::PropertyValue::String("FantasyHero".into()),
            entitydef::value::PropertyValue::String("FantasyAccount".into()),
        ]
    );
    let base_location = database
        .logon_of(&storage_key(account_type, 2))
        .expect("logon query");
    assert!(base_location.is_none(), "the fixture row starts offline");
    assert_eq!(
        handler.on_entity_loaded(
            true,
            EntityDBKey::new(account_type, 2, "FantasyAccount"),
            None,
        ),
        EntityStep::CheckOut
    );

    // `checkOutEntity`: the storage grant is free, the mailbox reserves.
    let grant = database
        .check_out(&storage_key(account_type, 2), storage_mailbox(1))
        .expect("checkout");
    assert_eq!(grant, CheckOut::Granted);
    assert_eq!(
        handler.check_out_entity(false),
        EntityStep::ReserveBaseMailbox {
            key: EntityDBKey::new(account_type, 2, "FantasyAccount")
        }
    );
    // The reservation lives in the database; the handler's base ref only
    // ever carries a pre-existing base (a fresh login dispatches with none).
    assert_eq!(
        handler.on_base_reserved(true),
        EntityStep::DispatchCreateEntity {
            key: EntityDBKey::new(account_type, 2, "FantasyAccount"),
            base_ref: None,
            db_id_slot: 2,
        }
    );

    // The character handoff: the account's `character` property resolves
    // the Avatar row by name (`createBaseFromDB`), whose persistent
    // projection is playerName + prop2 (both cell data, def order).
    let character = match &record.values[0] {
        entitydef::value::PropertyValue::String(name) => name.clone(),
        other => panic!("the character property is a string, got {other:?}"),
    };
    assert_eq!(character, "FantasyHero");
    assert_eq!(
        database.find_entity_by_name(avatar_type, &character),
        Some(1)
    );
    let avatar = database
        .get_entity(&storage_key(avatar_type, 1))
        .expect("get")
        .expect("the fixture avatar row");
    assert_eq!(
        avatar.values,
        vec![
            entitydef::value::PropertyValue::String("FantasyHero".into()),
            entitydef::value::PropertyValue::Int(2),
        ]
    );

    // A second login of the same account: the DB reports the live holder,
    // so the machine routes to the re-logon path instead of re-reserving
    // (`DBApp::onLogOnLoggedOnUser`, login_handler.cpp:286-307).
    let resolution = login(&billing, "FantasyAccount", "a");
    let mut second = LoginEntityHandler::new();
    assert!(matches!(
        second.begin(&resolution, "FantasyAccount"),
        EntityStep::LoadByKey { .. }
    ));
    let holder = database
        .logon_of(&storage_key(account_type, 2))
        .expect("logon query")
        .expect("the first login holds the checkout");
    assert_eq!(
        second.on_entity_loaded(
            true,
            EntityDBKey::new(account_type, 2, "FantasyAccount"),
            Some(handler_holder(&holder, account_type)),
        ),
        EntityStep::CheckOut
    );
    assert_eq!(
        second.check_out_entity(false),
        EntityStep::AlreadyLoggedOn {
            key: EntityDBKey::new(account_type, 2, "FantasyAccount"),
            existing_base: Some(handler_holder(&holder, account_type)),
        }
    );
    // And the storage agrees the second grant never took hold.
    assert!(matches!(
        database.check_out(&storage_key(account_type, 2), storage_mailbox(2)),
        Ok(CheckOut::Elsewhere(_))
    ));
}

/// An unknown user with `shouldAcceptUnknownUsers` creates a remembered
/// account: the put assigns `dbID = ++maxID_`, the mapping is written
/// back, and the reopened database agrees with every callback
/// (`login_handler.cpp:433-508`, `xml_database.cpp:626`).
#[test]
fn an_accepted_unknown_user_creates_a_remembered_account_the_rows_agree_with() {
    let defs = fixture_defs();
    let account_type = defs.name_to_index("Account").expect("Account") as EntityTypeId;
    let mut database = fixture_database(&defs);

    // `billingSystem/shouldAcceptUnknownUsers` +
    // `shouldRememberUnknownUsers`, the type resolved from the fixture
    // (`billing_system.cpp:14-52`).
    let config = BillingConfig {
        should_accept_unknown_users: true,
        should_remember_unknown_users: true,
        entity_type_for_unknown_users: account_type,
        ..BillingConfig::new()
    };
    let mut billing = LogonMapBillingSystem::new(config);
    let resolution = login(&billing, "NewUser", "pw");
    assert_eq!(
        resolution,
        AccountResolution::CreateNew {
            entity_type: account_type,
            should_remember: true,
            data_for_client: String::new(),
            data_for_base_entity: String::new(),
        }
    );

    // `createNewEntity(Account, remember)` — the default entity is the
    // identifier-only projection (`custom.cpp:18-40`), and remember
    // routes through `putEntity` with a pending id.
    let mut handler = LoginEntityHandler::new();
    assert_eq!(
        handler.begin(&resolution, "NewUser"),
        EntityStep::CreateEntity {
            type_id: account_type,
            should_remember: true,
            entity_name: "NewUser".to_string(),
        }
    );
    assert_eq!(
        handler.create_new_entity(account_type, true, true),
        EntityStep::PutEntity {
            key: EntityDBKey::new(account_type, 0, ""),
            entity_name: "NewUser".to_string(),
        }
    );

    // `putEntity` assigns the id past the fixture's maxID_ (2 -> 3). The
    // values follow the projection order: character, then playerName.
    let values = vec![
        entitydef::value::PropertyValue::String(String::new()),
        entitydef::value::PropertyValue::String("NewUser".into()),
    ];
    let db_id = database
        .put_entity(
            &storage_key(account_type, PENDING_DATABASE_ID),
            &values,
            false,
        )
        .expect("the create put");
    assert_eq!(db_id, 3);

    // The callbacks then pin the id and write the mapping back
    // (`onPutEntityComplete` -> `setEntityKeyForAccount` -> dispatch).
    assert_eq!(
        handler.on_entity_put(true, db_id as u32),
        EntityStep::SetEntityKeyForAccount {
            username: "NewUser".to_string(),
            key: BillingKey {
                type_id: account_type,
                db_id: 3
            },
        }
    );
    billing.add_mapping(
        "NewUser",
        BillingKey {
            type_id: account_type,
            db_id: 3,
        },
        "pw",
    );
    // The mapping is live immediately (`XMLBillingSystem::setEntityKeyForAccount`
    // always succeeds and updates the in-memory map).
    assert_eq!(
        billing.get_entity_key_for_account("NewUser", "pw"),
        AccountResolution::Success {
            entity_key: BillingKey {
                type_id: account_type,
                db_id: 3
            },
            data_for_client: String::new(),
            data_for_base_entity: String::new(),
        }
    );
    assert_eq!(
        handler.on_set_entity_key_for_account_success(),
        EntityStep::DispatchCreateEntity {
            key: EntityDBKey::new(account_type, 3, ""),
            base_ref: None,
            db_id_slot: 3,
        }
    );

    // The persisted state: reopen the save, everything the callbacks
    // reported is in the file, and the original rows are untouched.
    let reopened =
        XmlDatabase::parse(&database.to_xml(), storage_defs(&defs)).expect("the save parses");
    // The created account row is at the id the callbacks pinned. Account
    // carries no `<Identifier>` in the fixture def, so it resolves by
    // (type, id) alone — the name lookups stay `None`, which is why the
    // reference's `createBaseFromDB` refuses identifier-less types
    // (`entity_creator.cpp:794-801`) and the logon mapping carries the
    // `entityID` itself.
    assert_eq!(reopened.find_entity_by_name(account_type, "NewUser"), None);
    assert_eq!(
        reopened
            .get_entity(&storage_key(account_type, 3))
            .expect("get")
            .expect("the created row")
            .values,
        values
    );
    assert_eq!(
        reopened
            .get_entity(&storage_key(account_type, 2))
            .expect("get")
            .expect("the original account row")
            .values,
        vec![
            entitydef::value::PropertyValue::String("FantasyHero".into()),
            entitydef::value::PropertyValue::String("FantasyAccount".into()),
        ]
    );
    assert_eq!(
        reopened.find_entity_by_name(
            defs.name_to_index("Avatar").unwrap() as EntityTypeId,
            "FantasyHero"
        ),
        Some(1)
    );
    // The billing info section survives the save verbatim.
    let saved = reopened.to_xml();
    assert!(saved.contains("<LogOnMapping>"), "{saved}");
    assert!(saved.contains("FantasyAccount"), "{saved}");
    assert!(saved.contains("<databaseID>\t3\t</databaseID>"), "{saved}");
    assert!(reopened.auto_load_ids().is_empty());
}

/// A new character under an existing account: a script-created base's
/// `writeToDB` allocates the Avatar row, the account's `character`
/// pointer is rewritten to it, and the reopened database resolves the
/// name the way `createBaseFromDB` does (`base.cpp:272-300`,
/// `xml_database.cpp:626`).
#[test]
fn a_new_character_row_is_created_and_the_account_points_at_it() {
    let defs = fixture_defs();
    let account_type = defs.name_to_index("Account").expect("Account") as EntityTypeId;
    let avatar_type = defs.name_to_index("Avatar").expect("Avatar") as EntityTypeId;
    let mut database = fixture_database(&defs);

    // `BigWorld.createBaseLocally` + `Base.writeToDB`: the default
    // projection (playerName, prop2), pending id.
    let values = vec![
        entitydef::value::PropertyValue::String("FreshHero".into()),
        entitydef::value::PropertyValue::Int(2),
    ];
    let db_id = database
        .put_entity(
            &storage_key(avatar_type, PENDING_DATABASE_ID),
            &values,
            false,
        )
        .expect("the avatar create");
    assert_eq!(db_id, 3, "the id is the one past the fixture's maxID_");

    // `self.character = "FreshHero"` and `writeToDB`: the account row
    // updates in place, keeping its id and its name (the values follow
    // the projection order: character, then playerName).
    assert_eq!(
        database
            .put_entity(
                &storage_key(account_type, 2),
                &[
                    entitydef::value::PropertyValue::String("FreshHero".into()),
                    entitydef::value::PropertyValue::String("FantasyAccount".into()),
                ],
                false,
            )
            .expect("the account update"),
        2
    );

    // Persistence agrees: the reopened save resolves the new character by
    // name and the account points at it.
    let reopened =
        XmlDatabase::parse(&database.to_xml(), storage_defs(&defs)).expect("the save parses");
    assert_eq!(
        reopened.find_entity_by_name(avatar_type, "FreshHero"),
        Some(3)
    );
    assert_eq!(
        reopened
            .get_entity(&storage_key(avatar_type, 3))
            .expect("get")
            .expect("the new avatar row")
            .values,
        values
    );
    let account = reopened
        .get_entity(&storage_key(account_type, 2))
        .expect("get")
        .expect("the account row");
    assert_eq!(
        account.values[0],
        entitydef::value::PropertyValue::String("FreshHero".into())
    );
    // The account kept its own name through the update — the identifier
    // slot was written, not clobbered. (Account has no `<Identifier>`,
    // so the name maps stay empty for the type; the row resolves by id.)
    assert_eq!(
        account.values[1],
        entitydef::value::PropertyValue::String("FantasyAccount".into())
    );
    assert_eq!(
        reopened.find_entity_by_name(account_type, "FantasyAccount"),
        None
    );

    // The next run's `createBaseFromDB` lookup face resolves the same row.
    assert_eq!(
        get_database_id_from_name(&reopened, &lookup_defs(&defs), avatar_type, "FreshHero")
            .expect("the lookup"),
        3
    );
    // The Avatar's non-projection extras (position/direction/spaceID) are
    // preserved through the save.
    let saved = reopened.to_xml();
    assert!(saved.contains("<spaceID>"), "{saved}");
}

/// A wrong password and an unknown user under the default config are
/// rejected with the reference messages and touch no rows
/// (`login_handler.cpp:182-229`, `xml_billing_system.cpp:74-113`).
#[test]
fn wrong_password_and_unknown_user_are_rejected_without_touching_the_rows() {
    let defs = fixture_defs();
    let database = fixture_database(&defs);
    let billing = seeded_billing(&defs, &database);
    let before = database.to_xml();

    // The mapping answers; the password does not match it.
    let resolution = login(&billing, "FantasyAccount", "wrong");
    assert_eq!(
        resolution,
        AccountResolution::Failure {
            status: LogOnStatus::LoginRejectedInvalidPassword,
            error_msg: String::new(),
        }
    );
    let mut handler = LoginEntityHandler::new();
    assert_eq!(
        handler.begin(&resolution, "FantasyAccount"),
        EntityStep::Reject {
            status: LogOnStatus::LoginRejectedInvalidPassword,
            message: "Invalid password.".to_string(),
        }
    );

    // No mapping, unknown users not accepted.
    let resolution = login(&billing, "Nobody", "pw");
    assert_eq!(
        resolution,
        AccountResolution::Failure {
            status: LogOnStatus::LoginRejectedNoSuchUser,
            error_msg: String::new(),
        }
    );
    let mut handler = LoginEntityHandler::new();
    assert_eq!(
        handler.begin(&resolution, "Nobody"),
        EntityStep::Reject {
            status: LogOnStatus::LoginRejectedNoSuchUser,
            message: "Unknown user.".to_string(),
        }
    );

    // Neither rejection wrote anything: the save is byte-identical and
    // the row count is still the fixture's two.
    assert_eq!(database.to_xml(), before);
    assert_eq!(
        EntityStorage::all_entities(&database)
            .expect("all entities")
            .len(),
        2
    );
}

/// The wire leg alone: the fixture client's request parses, a wrong
/// version is refused before the credentials are touched, and a truncated
/// stream is malformed (`loginapp.cpp:916-967`).
#[test]
fn the_wire_login_request_yields_the_fixture_credentials() {
    let mut stream = wire_request("FantasyAccount", "a");
    let params = accepted_params(&mut stream);
    assert_eq!(params.username(), "FantasyAccount");
    assert_eq!(params.password(), "a");

    let mut stream = foundation::stream::BinaryStream::new();
    ClientServerProtocolVersion::new(1, 9, 0, 0).write_to_stream(&mut stream);
    LogOnParams::with_credentials("FantasyAccount", "a", "")
        .add_to_stream(&mut stream, LOG_ON_PARAMS_PASS_THRU);
    assert_eq!(
        parse_login_request(&mut stream, MAX_LOGIN_MESSAGE_SIZE),
        LoginParseOutcome::BadProtocolVersion
    );

    assert_eq!(
        parse_login_request(
            &mut foundation::stream::BinaryStream::from_slice(&[0, 0, 9, 2, 1]),
            MAX_LOGIN_MESSAGE_SIZE
        ),
        LoginParseOutcome::MalformedRequest
    );
}
