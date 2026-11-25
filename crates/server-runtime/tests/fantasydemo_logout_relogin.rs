//! BW-14-01-05 run the logout and the re-login over the tracked FantasyDemo
//! definitions and database.
//!
//! Acceptance: 读回此前角色位置及属性 — after the character exits, the next
//! login reads back exactly the row the exit persisted. The reference
//! logout is `Base::destroy(deleteFromDB, writeToDB, logOffFromDB)`
//! (`base.cpp:1488`) deciding `WriteDBFlags` (`lib/server/writedb.hpp`) and
//! `Base::writeToDB` (`base.cpp:1991`) normalising them; what the flags do
//! to the storage is `putEntity` (persist the row), `removeLogOnRecord`
//! (release the checkout) and `delEntity` (a deletion). The re-login is the
//! BW-14-01-02 flow again: the mapping row still resolves, the entity loads,
//! the checkout is granted again.
//!
//! The database under test is the fixture `db.xml` itself: the
//! `FantasyAccount` mapping row, the stored `Account` (id 2) and the stored
//! `Avatar` (id 1). The persistent values are what the record contract
//! carries; the pose (`position`/`direction`/`spaceID`) stays in the row's
//! section and an update never clobbers it, which is why the re-login reads
//! the exit's row unchanged.

use db::billing::{BillingConfig, BillingSystem, EntityKeyOutcome};
use db::xml_database::XmlDatabase;
use db::{BaseMailbox, CheckOut, DbError, EntityKey, EntityStorage};
use entitydef::data_type::TypeContext;
use entitydef::diagnostics::Diagnostics;
use entitydef::entity_description::{DefResolver, FsResolver};
use entitydef::entity_map::{EntityDescriptionMap, MessageRanges};
use entitydef::value::PropertyValue;
use mercury::endpoint::Address;
use resources::datasection;
use server_runtime::base_write_db::{
    CellDataArg, WRITE_BASE_CELL_DATA, WRITE_BASE_DATA, WRITE_DELETE_FROM_DB, WRITE_LOG_OFF,
    plan_write_to_db,
};
use server_runtime::billing::{AccountResolution, EntityKey as FlowKey};
use server_runtime::login_entity::{EntityDBKey, EntityStep, LoginEntityHandler};

/// The tracked FantasyDemo resource tree (BW-14-01-01).
fn fixture_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("the repository root")
        .join("docs/review/fixtures/fantasydemo_def/res")
}

/// `EntityDescriptionMapFactory::create` over the fixture res tree.
fn fixture_defs() -> EntityDescriptionMap {
    let resolver = FsResolver::new(fixture_root());

    let mut ctx = TypeContext::default();
    let aliases = FsResolver::new(fixture_root());
    ctx.init_aliases(move || aliases.open_section(entitydef::constants::ALIASES_FILE));

    let entities = resolver
        .open_section(entitydef::constants::ENTITIES_FILE)
        .expect("the fixture opens scripts/entities.xml");

    let mut map = EntityDescriptionMap::default();
    let mut diag = Diagnostics::new();
    assert!(
        map.parse(
            Some(&entities),
            &resolver,
            &ctx,
            &mut diag,
            &MessageRanges::default(),
        ),
        "fixture defs parse failed: {:?}",
        diag.messages()
    );
    map
}

/// The fixture `db.xml` as a running database.
fn fixture_database(defs: EntityDescriptionMap) -> XmlDatabase {
    let xml = std::fs::read_to_string(fixture_root().join("scripts/db.xml"))
        .expect("the fixture db.xml reads");
    XmlDatabase::from_xml(&xml, defs).expect("the fixture database opens")
}

/// The XML billing system over the fixture's `LogOnMapping` section, under
/// the demo defaults (the database carries no `billingSystem/` block).
fn fixture_billing(
    defs: EntityDescriptionMap,
    database: &XmlDatabase,
) -> db::billing::XmlBillingSystem {
    let xml = std::fs::read_to_string(fixture_root().join("scripts/db.xml"))
        .expect("the fixture db.xml reads");
    let mapping = log_on_mapping_fragment(&xml);
    let empty = datasection::parse("", b"<root/>").expect("parse");
    db::billing::XmlBillingSystem::new(mapping, BillingConfig::from_section(&empty), defs, database)
}

/// The `LogOnMapping` block of `db.xml`, as its own XML document.
fn log_on_mapping_fragment(db_xml: &str) -> resources::datasection::DataSection {
    let start = db_xml
        .find("<LogOnMapping>")
        .expect("the fixture db.xml carries a LogOnMapping block");
    let end = db_xml
        .find("</LogOnMapping>")
        .expect("the LogOnMapping block closes")
        + "</LogOnMapping>".len();
    let fragment = db_xml
        .as_bytes()
        .get(start..end)
        .expect("the LogOnMapping block is in range");
    datasection::parse("", fragment).expect("the LogOnMapping block parses")
}

/// The client address a logon attempt arrives from.
fn client_addr() -> Address {
    Address::new(0x0100_0002, 0x1f90)
}

/// The base mailbox a checkout reserves for the entity.
fn base_mailbox(entity_id: i32) -> BaseMailbox {
    BaseMailbox {
        addr: Address::new(0x0100_0001, 0x2710),
        entity_id,
    }
}

/// `LoginHandler::login`: the billing callback, translated into the flow's
/// `AccountResolution`.
fn resolution(outcome: EntityKeyOutcome) -> AccountResolution {
    match outcome {
        EntityKeyOutcome::Success {
            key,
            data_for_client,
            data_for_base_entity,
        } => AccountResolution::Success {
            entity_key: FlowKey::new(key.type_id, key.db_id as u32),
            data_for_client,
            data_for_base_entity,
        },
        _ => panic!("the demo run only resolves mapped successes"),
    }
}

/// The fixture's entity type indexes (`entities.xml` order).
fn type_of(defs: &EntityDescriptionMap, name: &str) -> u16 {
    defs.name_to_index(name).expect(name)
}

/// The logon half of the flow, as BW-14-01-02 drives it: the mapping
/// resolves, the account entity loads, the checkout is granted and the
/// createEntity dispatch carries the loaded id.
fn log_on(database: &mut XmlDatabase, defs: &EntityDescriptionMap, mailbox: BaseMailbox) -> u16 {
    let account = type_of(defs, "Account");
    let mut billing = fixture_billing(fixture_defs(), database);
    let outcome = billing.get_entity_key_for_account("FantasyAccount", "a", client_addr());
    let EntityKeyOutcome::Success { key, .. } = outcome else {
        panic!("the mapping row resolves");
    };
    let row_id = key.db_id;
    let flow_id = key.db_id as u32;

    let mut handler = LoginEntityHandler::new();
    let step = handler.begin(&resolution(outcome), "FantasyAccount");
    let EntityStep::LoadByKey { key } = step else {
        panic!("a mapped login loads by key");
    };
    assert_eq!(key, EntityDBKey::new(account, flow_id, ""));

    let record = database
        .get_entity(&EntityKey {
            type_id: account,
            db_id: row_id,
        })
        .expect("the account row reads")
        .expect("the account row exists");

    let step = handler.on_entity_loaded(true, EntityDBKey::new(account, flow_id, ""), None);
    assert_eq!(step, EntityStep::CheckOut);
    let step = handler.check_out_entity(false);
    let EntityStep::ReserveBaseMailbox { key: reserved } = step else {
        panic!("a free entity reserves its mailbox");
    };
    let granted = database.check_out(
        &EntityKey {
            type_id: account,
            db_id: row_id,
        },
        mailbox.clone(),
    );
    assert_eq!(granted, Ok(CheckOut::Granted));
    let step = handler.on_base_reserved(true);
    let EntityStep::DispatchCreateEntity { db_id_slot, .. } = step else {
        panic!("the reserved checkout dispatches the createEntity");
    };
    assert_eq!(db_id_slot, flow_id);
    assert_eq!(
        database
            .logon_of(&EntityKey {
                type_id: account,
                db_id: row_id,
            })
            .expect("the logon record reads"),
        Some(mailbox)
    );

    assert_eq!(reserved.db_id, flow_id);
    let _ = record;
    account
}

/// The plain logout: `Base::destroy(deleteFromDB = false, writeToDB = true,
/// logOffFromDB = true)` issues `WRITE_LOG_OFF | WRITE_BASE_CELL_DATA`, the
/// row is persisted and the checkout is released.
fn log_out(database: &mut XmlDatabase, defs: &EntityDescriptionMap, entity: &str, db_id: i64) {
    let type_id = type_of(defs, entity);
    let desc = defs.entity_description(type_id).expect("the description");

    let plan = plan_write_to_db(
        WRITE_LOG_OFF | WRITE_BASE_CELL_DATA,
        db_id,
        desc.can_be_on_cell(),
        desc.can_be_on_cell(),
        false,
        true,
    )
    .expect("the logout writes");
    assert_eq!(plan.db_id, db_id);
    assert!(
        plan.call_on_write_to_db,
        "onWriteToDB runs before the write"
    );
    assert!(!plan.should_backup, "a destroy never forces a backup");

    let before = database
        .get_entity(&EntityKey { type_id, db_id })
        .expect("the row reads")
        .expect("the row exists");

    // `WRITE_BASE_DATA`: the row is written back.
    let written = database.put_entity(&EntityKey { type_id, db_id }, &before.values, false);
    assert_eq!(written, Ok(db_id));
    // `WRITE_LOG_OFF`: the checkout is released.
    database
        .check_in(&EntityKey { type_id, db_id })
        .expect("the logon record is removed");
    assert_eq!(
        database
            .logon_of(&EntityKey { type_id, db_id })
            .expect("the logon record reads"),
        None
    );
    // What the exit persisted is what the row then serves.
    let after = database
        .get_entity(&EntityKey { type_id, db_id })
        .expect("the row reads")
        .expect("the row survives the logout");
    assert_eq!(after.values, before.values);
}

/// The exit and the re-login: the character row is written back, both
/// logon records are gone, and the next login reads the exit's rows back.
#[test]
fn a_logout_persists_the_character_and_the_relogin_reads_it_back() {
    let defs = fixture_defs();
    let mut database = fixture_database(fixture_defs());

    // The Avatar's persistent columns, in the definition's visit order:
    // `playerName` (`ALL_CLIENTS`, persistent) and `prop2` (`ALL_CLIENTS`,
    // persistent); `prop1`/`prop3` are not persistent and the base-only
    // pass yields nothing. The pose (`position`/`direction`/`spaceID`)
    // stays with the row's section, outside the record.
    let avatar = type_of(&defs, "Avatar");
    let avatar_before = database
        .get_entity(&EntityKey {
            type_id: avatar,
            db_id: 1,
        })
        .expect("the avatar row reads")
        .expect("the avatar row exists");
    assert_eq!(
        avatar_before.values,
        vec![
            PropertyValue::String("FantasyHero".to_string()),
            PropertyValue::Int(2),
        ]
    );

    // The first login: the account is online.
    let account = log_on(&mut database, &defs, base_mailbox(10_001));

    // The exit: both bases are destroyed and written back. The Avatar is
    // cell-capable, so the write keeps its cell data; the Account is
    // base-only and carries none.
    log_out(&mut database, &defs, "Avatar", 1);
    log_out(&mut database, &defs, "Account", 2);

    // The account base's flags lose the cell data it cannot carry
    // (`base.cpp:2018-2022`).
    let desc = defs
        .entity_description(account)
        .expect("the Account description");
    let plan = plan_write_to_db(
        WRITE_LOG_OFF | WRITE_BASE_CELL_DATA,
        2,
        desc.can_be_on_cell(),
        false,
        false,
        true,
    )
    .expect("the account logout writes");
    assert_eq!(plan.flags, WRITE_LOG_OFF | WRITE_BASE_DATA);
    assert_eq!(plan.cell_data, CellDataArg::NoneValue);

    // The re-login: the mapping resolves again, the same account loads,
    // the checkout is granted again, and the character row is exactly the
    // exit's.
    let mailbox = base_mailbox(10_002);
    let _ = log_on(&mut database, &defs, mailbox);

    let avatar_after = database
        .get_entity(&EntityKey {
            type_id: avatar,
            db_id: 1,
        })
        .expect("the avatar row reads")
        .expect("the avatar row survives");
    assert_eq!(avatar_after.values, avatar_before.values);
    assert_eq!(avatar_after.db_id, avatar_before.db_id);
}

/// The deletion logout: `destroy(deleteFromDB = true)` turns the write into
/// `WRITE_DELETE_FROM_DB | WRITE_LOG_OFF`, the row is removed, the logon
/// record with it, and nothing is left to load.
#[test]
fn a_delete_from_db_logout_removes_the_row() {
    let defs = fixture_defs();
    let mut database = fixture_database(fixture_defs());
    let avatar = type_of(&defs, "Avatar");

    let plan = plan_write_to_db(
        WRITE_DELETE_FROM_DB | WRITE_BASE_CELL_DATA,
        1,
        true,
        true,
        false,
        true,
    )
    .expect("the deletion writes");
    assert_eq!(plan.flags, WRITE_DELETE_FROM_DB | WRITE_LOG_OFF);
    assert!(
        !plan.call_on_write_to_db,
        "the data flags are gone, so no onWriteToDB"
    );

    let key = EntityKey {
        type_id: avatar,
        db_id: 1,
    };
    // `WRITE_DELETE_FROM_DB` deletes the row, `WRITE_LOG_OFF` drops the
    // record.
    EntityStorage::del_entity(&mut database, &key).expect("the row deletes");
    database.check_in(&key).expect("the record is removed");
    assert_eq!(database.get_entity(&key), Ok(None));
    assert_eq!(database.logon_of(&key), Ok(None));
    // A checkout needs a row; the deleted entity cannot come online.
    assert_eq!(
        database.check_out(&key, base_mailbox(10_003)),
        Err(DbError::NotFound)
    );
}
