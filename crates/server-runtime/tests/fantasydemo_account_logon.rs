//! BW-14-01-02: run the account logon and the character creation over the
//! tracked FantasyDemo definitions and database.
//!
//! Acceptance: 回调与持久化状态吻合 — the callbacks and the persisted state
//! agree. The run joins the landed faces into the reference's `LoginHandler`
//! flow (`server/dbapp/login_handler.cpp`):
//!
//! * the XML billing system resolves the credentials against `db.xml`'s
//!   `LogOnMapping` (`XMLBillingSystem::getEntityKeyForAccount`),
//! * the account entity loads or is created through the storage contract,
//!   the created one from `DBApp::defaultEntityToStrm`'s port
//!   (`db::default_entity`),
//! * the online state is the checkout face (`bigworldLogOns`' XML
//!   counterpart, the `activeSet_`).
//!
//! The database under test is the fixture `db.xml` itself (BW-14-01-01
//! tracks it byte-identically): the `FantasyAccount` mapping row, the
//! stored `Account` entity (id 2, playerName `FantasyAccount`, character
//! `FantasyHero`) and the stored `Avatar` (id 1).
//!
//! The Mercury wire is not modelled, exactly as the reference's process
//! boundary: the run stops at the faces that cross it — the billing
//! callback (`LoginHandler::login`), the storage calls, and the
//! `BaseAppMgrInterface::createEntity` dispatch (`EntityStep`
//! ::DispatchCreateEntity). What the run asserts is that every callback
//! the flow produces matches the row the storage then serves.

use std::path::{Path, PathBuf};

use db::billing::{BillingConfig, BillingSystem, EntityKeyOutcome, SetEntityKeyOutcome};
use db::xml_database::XmlDatabase;
use db::{BaseMailbox, CheckOut, DefaultEntity, EntityKey, EntityStorage, create_new_entity};
use entitydef::data_type::TypeContext;
use entitydef::diagnostics::Diagnostics;
use entitydef::entity_description::{DefResolver, FsResolver};
use entitydef::entity_map::{EntityDescriptionMap, MessageRanges};
use entitydef::value::PropertyValue;
use mercury::endpoint::Address;
use resources::datasection;
use server_runtime::billing::{AccountResolution, EntityKey as FlowKey};
use server_runtime::login_entity::{EntityDBKey, EntityStep, LoginEntityHandler};
use server_runtime::loginapp::LogOnStatus;

/// The tracked FantasyDemo resource tree (BW-14-01-01).
fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the repository root")
        .join("docs/review/fixtures/fantasydemo_def/res")
}

/// `EntityDescriptionMapFactory::create` over the fixture res tree: the five
/// sample definitions load clean.
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

/// The fixture `db.xml` as a running database: the mapping row and the two
/// stored entities.
fn fixture_database(defs: EntityDescriptionMap) -> XmlDatabase {
    let xml = std::fs::read_to_string(fixture_root().join("scripts/db.xml"))
        .expect("the fixture db.xml reads");
    XmlDatabase::from_xml(&xml, defs).expect("the fixture database opens")
}

/// `DBApp::pBillingSystem()`: the XML billing system over the fixture's
/// `LogOnMapping` section, configured the way `dbapp_config.cpp` reads
/// `db.xml`'s `billingSystem/` child — absent in the demo database, so the
/// demo-config run keeps every default.
fn fixture_billing(
    defs: EntityDescriptionMap,
    database: &XmlDatabase,
    config: BillingConfig,
) -> db::billing::XmlBillingSystem {
    let xml = std::fs::read_to_string(fixture_root().join("scripts/db.xml"))
        .expect("the fixture db.xml reads");
    let mapping = log_on_mapping_fragment(&xml);
    db::billing::XmlBillingSystem::new(mapping, config, defs, database)
}

/// The `LogOnMapping` block of `db.xml`, as its own XML document: the
/// reference reads it out of the `<_BigWorldInfo>` child the database
/// writes, and the billing system keeps it as its own section.
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

/// An empty `billingSystem/` section: the reference reads a missing child as
/// every default (`BWConfig::get( path, default )`).
fn demo_config() -> BillingConfig {
    let empty = datasection::parse("", b"<root/>").expect("parse");
    BillingConfig::from_section(&empty)
}

/// The operator switch that enables the character-creation path
/// (`db.xml`'s `billingSystem/` block, absent from the demo database):
/// accept a user the mapping does not know and remember the created
/// entity, exactly the `shouldAcceptUnknownUsers`/`shouldRememberUnknownUsers`
/// pair.
fn creating_config(entity_type_for_unknown_users: &str) -> BillingConfig {
    BillingConfig {
        should_accept_unknown_users: true,
        should_remember_unknown_users: true,
        authenticate_via_base_entity: false,
        entity_type_for_unknown_users: entity_type_for_unknown_users.to_string(),
    }
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

/// `LoginHandler::login`: the billing system's callback, translated into
/// the flow's `AccountResolution` (`IGetEntityKeyForAccountHandler`).
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
        EntityKeyOutcome::LoadFromUsername {
            entity_type,
            username,
            should_create_new_on_load_failure,
            should_remember_new_on_load_failure,
            data_for_client,
            data_for_base_entity,
        } => AccountResolution::LoadFromUsername {
            entity_type,
            username,
            should_create_new_on_load_failure,
            should_remember_new_on_load_failure,
            data_for_client,
            data_for_base_entity,
        },
        EntityKeyOutcome::CreateNew {
            entity_type,
            should_remember,
            data_for_client,
            data_for_base_entity,
        } => AccountResolution::CreateNew {
            entity_type,
            should_remember,
            data_for_client,
            data_for_base_entity,
        },
        EntityKeyOutcome::Failure { status, error_msg } => AccountResolution::Failure {
            status: flow_status(status.value()),
            error_msg,
        },
    }
}

/// The status code both faces number identically
/// (`connection/log_on_status.hpp`).
fn flow_status(code: i32) -> LogOnStatus {
    match code {
        67 => LogOnStatus::LoginRejectedNoSuchUser,
        68 => LogOnStatus::LoginRejectedInvalidPassword,
        71 => LogOnStatus::LoginRejectedDbGeneralFailure,
        90 => LogOnStatus::LoginRejectedAuthServiceGeneralFailure,
        254 => LogOnStatus::LoginCustomDefinedError,
        _ => panic!("the run only produces the reference's login-handler statuses"),
    }
}

/// The fixture's entity type indexes (`entities.xml` order).
fn account_type(defs: &EntityDescriptionMap) -> u16 {
    defs.name_to_index("Account").expect("Account")
}

/// The demo-config run: the mapped account logs in, its stored entity is
/// the payload, and the reserved checkout is what the storage then serves.
#[test]
fn a_mapped_logon_loads_the_stored_account_entity() {
    let defs = fixture_defs();
    let account = account_type(&defs);
    let mut database = fixture_database(fixture_defs());
    let mut billing = fixture_billing(fixture_defs(), &database, demo_config());

    // The callback: the mapping row (FantasyAccount/a → Account, 2).
    let outcome = billing.get_entity_key_for_account("FantasyAccount", "a", client_addr());
    assert_eq!(
        outcome,
        EntityKeyOutcome::Success {
            key: EntityKey {
                type_id: account,
                db_id: 2,
            },
            data_for_client: String::new(),
            data_for_base_entity: String::new(),
        }
    );

    // `LoginHandler::login` begins the entity step: load by key.
    let mut handler = LoginEntityHandler::new();
    let step = handler.begin(&resolution(outcome), "FantasyAccount");
    assert_eq!(
        step,
        EntityStep::LoadByKey {
            key: EntityDBKey::new(account, 2, ""),
        }
    );

    // `onGetEntityCompleted`: what the database serves is the stored
    // section — playerName `FantasyAccount`, character `FantasyHero` —
    // and the entity id the billing callback named.
    //
    // The values follow the definition's visit order rather than the
    // `.def`'s declaration order: `visit` walks its passes first, and
    // `character` is `BASE` only while `playerName` is `BASE_AND_CLIENT`,
    // so the base-only pass yields it before the base-and-client pass.
    let record = database
        .get_entity(&EntityKey {
            type_id: account,
            db_id: 2,
        })
        .expect("the stored account reads")
        .expect("the stored account exists");
    assert_eq!(record.db_id, 2);
    assert_eq!(
        record.values,
        vec![
            PropertyValue::String("FantasyHero".to_string()),
            PropertyValue::String("FantasyAccount".to_string()),
        ]
    );
    let step = handler.on_entity_loaded(true, EntityDBKey::new(account, 2, ""), None);
    assert_eq!(step, EntityStep::CheckOut);

    // `checkOutEntity`: not checked out, so the base mailbox is reserved.
    let step = handler.check_out_entity(false);
    assert_eq!(
        step,
        EntityStep::ReserveBaseMailbox {
            key: EntityDBKey::new(account, 2, ""),
        }
    );

    // `setBaseEntityLocation`: the logon record is persisted; the reserved
    // mailbox is what the storage then serves.
    let mailbox = base_mailbox(10_001);
    assert_eq!(
        database.check_out(
            &EntityKey {
                type_id: account,
                db_id: 2,
            },
            mailbox.clone(),
        ),
        Ok(CheckOut::Granted)
    );
    let step = handler.on_base_reserved(true);
    assert_eq!(
        step,
        EntityStep::DispatchCreateEntity {
            key: EntityDBKey::new(account, 2, ""),
            base_ref: None,
            db_id_slot: 2,
        }
    );
    assert_eq!(
        database.logon_of(&EntityKey {
            type_id: account,
            db_id: 2,
        }),
        Ok(Some(mailbox))
    );
}

/// A wrong password is rejected at the billing callback and nothing is
/// written: the rejection and the untouched database agree.
#[test]
fn a_wrong_password_is_rejected_without_touching_the_database() {
    let defs = fixture_defs();
    let account = account_type(&defs);
    let database = fixture_database(fixture_defs());
    let mut billing = fixture_billing(fixture_defs(), &database, demo_config());

    let outcome = billing.get_entity_key_for_account("FantasyAccount", "b", client_addr());
    assert_eq!(
        outcome,
        EntityKeyOutcome::Failure {
            status: db::log_on_status::LogOnStatus::new(
                db::log_on_status::codes::LOGIN_REJECTED_INVALID_PASSWORD,
            ),
            error_msg: String::new(),
        }
    );

    let mut handler = LoginEntityHandler::new();
    let step = handler.begin(&resolution(outcome), "FantasyAccount");
    assert_eq!(
        step,
        EntityStep::Reject {
            status: LogOnStatus::LoginRejectedInvalidPassword,
            message: "Invalid password.".to_string(),
        }
    );

    // The stored rows are untouched: still the two fixture entities, no
    // logon record.
    assert_eq!(database.max_id(), 2);
    assert!(
        database
            .get_entity(&EntityKey {
                type_id: account,
                db_id: 3,
            })
            .expect("the probe reads")
            .is_none()
    );
    assert_eq!(
        database.logon_of(&EntityKey {
            type_id: account,
            db_id: 2,
        }),
        Ok(None)
    );
}

/// An unknown user is rejected under the demo config, and the database
/// remembers nothing — no entity, no mapping row.
#[test]
fn an_unknown_user_is_rejected_under_the_demo_config() {
    let defs = fixture_defs();
    let account = account_type(&defs);
    let database = fixture_database(fixture_defs());
    let mut billing = fixture_billing(fixture_defs(), &database, demo_config());

    let outcome = billing.get_entity_key_for_account("Stranger", "whatever", client_addr());
    assert_eq!(
        outcome,
        EntityKeyOutcome::Failure {
            status: db::log_on_status::LogOnStatus::new(
                db::log_on_status::codes::LOGIN_REJECTED_NO_SUCH_USER,
            ),
            error_msg: String::new(),
        }
    );

    let mut handler = LoginEntityHandler::new();
    let step = handler.begin(&resolution(outcome.clone()), "Stranger");
    assert_eq!(
        step,
        EntityStep::Reject {
            status: LogOnStatus::LoginRejectedNoSuchUser,
            message: "Unknown user.".to_string(),
        }
    );

    assert_eq!(database.max_id(), 2);
    assert!(
        database
            .get_entity(&EntityKey {
                type_id: account,
                db_id: 3,
            })
            .expect("the probe reads")
            .is_none()
    );
    // The mapping still only knows FantasyAccount.
    assert_eq!(
        billing.get_entity_key_for_account("Stranger", "whatever", client_addr()),
        outcome.clone()
    );
}

/// The character creation: an accepted unknown user gets the type's default
/// entity, the id the put returned is what the callback carries, the
/// mapping row is remembered, and a second logon resolves — and loads —
/// exactly what creation persisted.
#[test]
fn an_accepted_unknown_user_creates_and_remembers_a_new_account() {
    let defs = fixture_defs();
    let account = account_type(&defs);
    let mut database = fixture_database(fixture_defs());
    let mut billing = fixture_billing(fixture_defs(), &database, creating_config("Account"));

    // The callback: create a new `Account` for the unknown user, remembered.
    let outcome = billing.get_entity_key_for_account("NewHero", "pw", client_addr());
    assert_eq!(
        outcome,
        EntityKeyOutcome::CreateNew {
            entity_type: account,
            should_remember: true,
            data_for_client: String::new(),
            data_for_base_entity: String::new(),
        }
    );

    let mut handler = LoginEntityHandler::new();
    let step = handler.begin(&resolution(outcome), "NewHero");
    assert_eq!(
        step,
        EntityStep::CreateEntity {
            type_id: account,
            should_remember: true,
            entity_name: "NewHero".to_string(),
        }
    );

    // `defaultEntityToStrm` (custom.cpp): the `Account` type has no
    // identifier, so the default entity is every column at its type
    // default.
    let account_desc = defs
        .entity_description(account)
        .expect("the Account description");
    assert!(create_new_entity(account_desc, "NewHero").is_empty());
    let defaults = DefaultEntity::for_type(account_desc, "NewHero");
    assert!(defaults.is_some(), "the Account defaults");
    let default_entity_ok = defaults.is_some();
    let default = defaults.expect("the Account defaults");
    assert!(!default.can_be_on_cell);

    // `createNewEntity` (`login_handler.cpp`): the default entity was
    // produced, so the createEntity bundle's dbID slot is prepared, and a
    // remembered entity is written before it is dispatched.
    let step = handler.create_new_entity(account, true, default_entity_ok);
    // The bundle key names nothing yet: the id is allocated by the put and
    // the name travels in `entity_name`.
    assert_eq!(
        step,
        EntityStep::PutEntity {
            key: EntityDBKey::new(account, 0, ""),
            entity_name: "NewHero".to_string(),
        }
    );

    // `putEntity` with dbID 0: the row is created and the id allocated
    // (`++maxID_` past the fixture's 2).
    let created = database
        .put_entity(
            &EntityKey {
                type_id: account,
                db_id: 0,
            },
            &default.values,
            false,
        )
        .expect("the default entity writes");
    assert_eq!(created, 3);
    let created_id = created as u32;

    // `onPutEntityComplete`: the callback now carries the allocated id.
    let step = handler.on_entity_put(true, created_id);
    assert_eq!(
        step,
        EntityStep::SetEntityKeyForAccount {
            username: "NewHero".to_string(),
            key: FlowKey::new(account, created_id),
        }
    );

    // `setEntityKeyForAccount`: the billing system remembers the mapping.
    assert_eq!(
        billing.set_entity_key_for_account(
            "NewHero",
            "pw",
            EntityKey {
                type_id: account,
                db_id: created,
            }
        ),
        SetEntityKeyOutcome::Success(EntityKey {
            type_id: account,
            db_id: created,
        })
    );
    let step = handler.on_set_entity_key_for_account_success();
    assert_eq!(
        step,
        EntityStep::DispatchCreateEntity {
            key: EntityDBKey::new(account, created_id, ""),
            base_ref: None,
            db_id_slot: created_id,
        }
    );

    // What the callbacks carried is what the database serves: the new row
    // holds the default values, and the remembered mapping resolves the
    // very same entity.
    let record = database
        .get_entity(&EntityKey {
            type_id: account,
            db_id: created,
        })
        .expect("the created account reads")
        .expect("the created account exists");
    assert_eq!(record.values, default.values);
    let outcome = billing.get_entity_key_for_account("NewHero", "pw", client_addr());
    assert_eq!(
        outcome,
        EntityKeyOutcome::Success {
            key: EntityKey {
                type_id: account,
                db_id: created,
            },
            data_for_client: String::new(),
            data_for_base_entity: String::new(),
        }
    );

    // And a second logon runs the mapped flow over the created entity:
    // load, check out, dispatch — with the created id in every slot.
    let mut handler = LoginEntityHandler::new();
    let step = handler.begin(&resolution(outcome), "NewHero");
    assert_eq!(
        step,
        EntityStep::LoadByKey {
            key: EntityDBKey::new(account, created_id, ""),
        }
    );
    let record = database
        .get_entity(&EntityKey {
            type_id: account,
            db_id: created,
        })
        .expect("the created account reads")
        .expect("the created account exists");
    assert_eq!(record.values, default.values);
    let step = handler.on_entity_loaded(true, EntityDBKey::new(account, created_id, ""), None);
    assert_eq!(step, EntityStep::CheckOut);
    let step = handler.check_out_entity(false);
    assert_eq!(
        step,
        EntityStep::ReserveBaseMailbox {
            key: EntityDBKey::new(account, created_id, ""),
        }
    );
    let mailbox = base_mailbox(10_002);
    assert_eq!(
        database.check_out(
            &EntityKey {
                type_id: account,
                db_id: created,
            },
            mailbox.clone(),
        ),
        Ok(CheckOut::Granted)
    );
    assert_eq!(
        handler.on_base_reserved(true),
        EntityStep::DispatchCreateEntity {
            key: EntityDBKey::new(account, created_id, ""),
            base_ref: None,
            db_id_slot: created_id,
        }
    );
    assert_eq!(
        database.logon_of(&EntityKey {
            type_id: account,
            db_id: created,
        }),
        Ok(Some(mailbox))
    );
}

/// A remembered creation whose put failed rejects the login with the
/// reference's database-failure text — and the callback carries no entity,
/// because nothing was written.
#[test]
fn a_failed_creation_rejects_the_login() {
    let defs = fixture_defs();
    let account = account_type(&defs);
    let mut database = fixture_database(fixture_defs());
    let mut billing = fixture_billing(fixture_defs(), &database, creating_config("Account"));

    let outcome = billing.get_entity_key_for_account("NewHero", "pw", client_addr());
    let mut handler = LoginEntityHandler::new();
    let step = handler.begin(&resolution(outcome), "NewHero");
    assert_eq!(
        step,
        EntityStep::CreateEntity {
            type_id: account,
            should_remember: true,
            entity_name: "NewHero".to_string(),
        }
    );

    // The default entity is fine, but the write fails (an unknown type is
    // the reference's `putEntity` → `isOK=false`).
    let write_failed = database.put_entity(
        &EntityKey {
            type_id: 0xfefe,
            db_id: 0,
        },
        &[],
        false,
    );

    let step = handler.on_entity_put(false, 0);
    assert_eq!(
        step,
        EntityStep::Reject {
            status: LogOnStatus::LoginRejectedDbGeneralFailure,
            message: "Unexpected database failure.".to_string(),
        }
    );
    assert!(write_failed.is_err());
    // Nothing was written: the database still ends at the fixture's 2.
    assert_eq!(database.max_id(), 2);
}
