//! The authenticateAccount handler
//! (`server/dbapp/authenticate_account_handler.{hpp,cpp}`).
//!
//! The loginapp relays each log-on attempt to the DBApp as an
//! `authenticateAccount` message; the DBApp runs it through its billing
//! system and replies over the Mercury reply bundle. One handler
//! instance exists per in-flight request and carries that request's
//! reply addressing, so any number of concurrent authentications can
//! share one billing system without their replies or the log-on mapping
//! polluting each other.
//!
//! The reference drops the billing data strings on the floor (its
//! `TODO: Send billingData back in the reply?`), so success replies
//! `uint8( LOGGED_ON ) << typeID << dbID` and nothing else; every
//! failure replies `uint8( status.value() ) << errorMsg` — the uint8
//! cast is where an out-of-range status value truncates on the wire.
//!
//! Layering: domain library (layer 2). The Mercury reply bundle is
//! modelled as the [`AuthenticateReply`] value; the interface layer
//! addresses and sends it.

use foundation::stream::BinaryStream;
use mercury::endpoint::Address;
use mercury::misc::ReplyID;

use crate::billing::{BillingSystem, EntityKeyOutcome};
use crate::log_on_status::{LogOnStatus, codes};
use crate::storage::EntityKey;

/// The reply to one authenticateAccount request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthenticateReply {
    /// The logon succeeded: `uint8( LOGGED_ON ) << ekey.typeID <<
    /// ekey.dbID`. The billing data strings are not sent (the
    /// reference's TODO).
    LoggedOn(EntityKey),
    /// The logon was refused: `uint8( status.value() ) << errorMsg`.
    Rejected(LogOnStatus, String),
}

impl AuthenticateReply {
    /// The reply bundle's payload, without the Mercury envelope.
    pub fn write_to(&self, stream: &mut BinaryStream) {
        match self {
            Self::LoggedOn(key) => {
                stream.write_u8(codes::LOGGED_ON as u8);
                stream.write_u16(key.type_id);
                stream.write_i64(key.db_id);
            }
            Self::Rejected(status, error_msg) => {
                // `uint8( status.value() )` — the wire truncation point.
                stream.write_u8(status.value() as u8);
                let _ = stream.write_string(error_msg);
            }
        }
    }
}

/// One authenticateAccount request in flight — the reference's
/// `AuthenticateAccountHandler`: one instance per request, carrying the
/// reply addressing so concurrent requests cannot write into each
/// other's reply bundles.
pub struct AuthenticateAccountHandler {
    reply_addr: Address,
    reply_id: ReplyID,
}

impl AuthenticateAccountHandler {
    /// Constructor: capture the reply addressing for this request only.
    pub fn new(reply_addr: Address, reply_id: ReplyID) -> Self {
        Self {
            reply_addr,
            reply_id,
        }
    }

    /// The loginapp address the reply bundle goes to.
    pub fn reply_addr(&self) -> Address {
        self.reply_addr
    }

    /// The Mercury reply id to start the reply bundle with.
    pub fn reply_id(&self) -> ReplyID {
        self.reply_id
    }

    /// Dispatch the billing outcome to the matching handler override
    /// (`onGetEntityKeyForAccountSuccess` / `...LoadFromUsername` /
    /// `...CreateNew` / `...Failure`).
    pub fn on_entity_key_outcome(self, outcome: EntityKeyOutcome) -> AuthenticateReply {
        match outcome {
            EntityKeyOutcome::Success { key, .. } => AuthenticateReply::LoggedOn(key),
            EntityKeyOutcome::LoadFromUsername { .. } => self.send_failure(
                LogOnStatus::new(codes::LOGIN_REJECTED_DB_GENERAL_FAILURE),
                "Authenticate via Base is configured".to_string(),
            ),
            EntityKeyOutcome::CreateNew { .. } => self.send_failure(
                LogOnStatus::new(codes::LOGIN_REJECTED_NO_SUCH_USER),
                // The reference's message, verbatim.
                "Requested to created a new user".to_string(),
            ),
            EntityKeyOutcome::Failure { status, error_msg } => self.send_failure(status, error_msg),
        }
    }

    /// `sendFailure`: the one reject path. The reference asserts the
    /// status is not `LOGGED_ON` — a success must never travel through
    /// it.
    fn send_failure(self, status: LogOnStatus, error_msg: String) -> AuthenticateReply {
        debug_assert_ne!(status.value(), codes::LOGGED_ON);
        AuthenticateReply::Rejected(status, error_msg)
    }
}

/// `AuthenticateAccountHandler::handle`: run one authenticateAccount
/// request through the shared billing system. The client address is
/// always `Mercury::Address( 0, 0 )` — the loginapp, not the client,
/// talks to the DBApp — and the handler is fresh per request, so the
/// outcome carries this request's addressing and nothing else's.
pub fn handle(
    reply_addr: Address,
    reply_id: ReplyID,
    billing: &mut dyn BillingSystem,
    username: &str,
    password: &str,
) -> AuthenticateReply {
    let handler = AuthenticateAccountHandler::new(reply_addr, reply_id);
    let outcome = billing.get_entity_key_for_account(username, password, Address::new(0, 0));
    handler.on_entity_key_outcome(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::billing::{BillingConfig, XmlBillingSystem};
    use crate::xml_database::tests::defs;
    use crate::xml_database::{AutoLoadUpdate, XmlDatabase};
    use resources::datasection::parse;

    /// A system whose database holds two named Avatars (ids 1 and 2)
    /// with a log-on mapping row for each.
    fn fixture() -> XmlBillingSystem {
        let mut database = XmlDatabase::new(defs());
        let avatar = defs().name_to_index("Avatar").unwrap();
        for (name, id) in [("moe", 1), ("curly", 2)] {
            let mut stream = BinaryStream::new();
            let _ = stream.write_string(name);
            stream.write_i32(10);
            stream.write_i64(0);
            database
                .put_entity_stream(avatar, id, &mut stream, AutoLoadUpdate::Retain, true)
                .expect("put");
        }

        let section = parse(
            "root",
            b"<root> \
                 <item> <logOnName> moe </logOnName> <password> secret </password> \
                     <type> Avatar </type> <entityID> 1 </entityID> </item> \
                 <item> <logOnName> curly </logOnName> <password> curlypw </password> \
                     <type> Avatar </type> <entityID> 2 </entityID> </item> \
             </root>",
        )
        .expect("parse logon map");
        XmlBillingSystem::new(section, BillingConfig::default(), defs(), &database)
    }

    fn avatar_id() -> u16 {
        defs().name_to_index("Avatar").unwrap()
    }

    /// A legal account replies LOGGED_ON with the mapped key and
    /// nothing else — the data strings are dropped.
    #[test]
    fn a_legal_account_replies_the_mapped_key() {
        let mut system = fixture();

        let reply = handle(
            Address::new(0xC0A8_0101, 20_001),
            7,
            &mut system,
            "moe",
            "secret",
        );
        assert_eq!(
            reply,
            AuthenticateReply::LoggedOn(EntityKey {
                type_id: avatar_id(),
                db_id: 1
            })
        );

        // Wire bytes: uint8(LOGGED_ON) << uint16(typeID) << int64(dbID).
        let mut stream = BinaryStream::new();
        reply.write_to(&mut stream);
        let mut expected = BinaryStream::new();
        expected.write_u8(codes::LOGGED_ON as u8);
        expected.write_u16(avatar_id());
        expected.write_i64(1);
        assert_eq!(stream.as_bytes(), expected.as_bytes());
    }

    /// A wrong password and an unknown user reply their own status
    /// codes with the mapping's empty error message.
    #[test]
    fn rejections_reply_their_status_codes() {
        let mut system = fixture();

        assert_eq!(
            handle(Address::new(0, 0), 1, &mut system, "moe", "wrong"),
            AuthenticateReply::Rejected(
                LogOnStatus::new(codes::LOGIN_REJECTED_INVALID_PASSWORD),
                String::new()
            )
        );
        assert_eq!(
            handle(Address::new(0, 0), 2, &mut system, "stranger", "pw"),
            AuthenticateReply::Rejected(
                LogOnStatus::new(codes::LOGIN_REJECTED_NO_SUCH_USER),
                String::new()
            )
        );

        // Wire bytes of a rejection: uint8(status) << packed string.
        let reply = handle(Address::new(0, 0), 3, &mut system, "moe", "nope");
        let mut stream = BinaryStream::new();
        reply.write_to(&mut stream);
        let mut expected = BinaryStream::new();
        expected.write_u8(codes::LOGIN_REJECTED_INVALID_PASSWORD as u8);
        let _ = expected.write_string("");
        assert_eq!(stream.as_bytes(), expected.as_bytes());
    }

    /// The load-from-username outcome is refused as a base-entity
    /// authentication, and the create-new outcome is refused with the
    /// reference's message, verbatim typo included.
    #[test]
    fn base_and_create_outcomes_are_refused_with_the_reference_texts() {
        let mut base_system = {
            let section = parse("root", b"<root/>").expect("parse");
            let config = BillingConfig {
                authenticate_via_base_entity: true,
                entity_type_for_unknown_users: "Avatar".to_string(),
                ..BillingConfig::default()
            };
            XmlBillingSystem::new(section, config, defs(), &fixture_database())
        };
        assert_eq!(
            handle(Address::new(0, 0), 4, &mut base_system, "whoever", "pw"),
            AuthenticateReply::Rejected(
                LogOnStatus::new(codes::LOGIN_REJECTED_DB_GENERAL_FAILURE),
                "Authenticate via Base is configured".to_string()
            )
        );

        let mut create_system = {
            let section = parse("root", b"<root/>").expect("parse");
            let config = BillingConfig {
                should_accept_unknown_users: true,
                entity_type_for_unknown_users: "Avatar".to_string(),
                ..BillingConfig::default()
            };
            XmlBillingSystem::new(section, config, defs(), &fixture_database())
        };
        assert_eq!(
            handle(Address::new(0, 0), 5, &mut create_system, "stranger", "pw"),
            AuthenticateReply::Rejected(
                LogOnStatus::new(codes::LOGIN_REJECTED_NO_SUCH_USER),
                "Requested to created a new user".to_string()
            )
        );
    }

    /// Two in-flight requests keep their own reply addressing and their
    /// own outcomes; the shared log-on mapping is untouched by either
    /// — no new rows, no changed passwords.
    #[test]
    fn interleaved_requests_do_not_pollute_each_other_or_the_mapping() {
        let mut system = fixture();
        let loginapp_a = Address::new(0xC0A8_0101, 20_001);
        let loginapp_b = Address::new(0xC0A8_0102, 20_002);

        // Request A: legal. Request B: legal for the other account.
        // Interleave them and one wrong-password attempt.
        let reply_a = handle(loginapp_a, 100, &mut system, "moe", "secret");
        let reply_b = handle(loginapp_b, 200, &mut system, "curly", "curlypw");
        let reply_bad = handle(loginapp_a, 101, &mut system, "moe", "guess");

        assert_eq!(
            reply_a,
            AuthenticateReply::LoggedOn(EntityKey {
                type_id: avatar_id(),
                db_id: 1
            })
        );
        assert_eq!(
            reply_b,
            AuthenticateReply::LoggedOn(EntityKey {
                type_id: avatar_id(),
                db_id: 2
            })
        );
        assert_eq!(
            reply_bad,
            AuthenticateReply::Rejected(
                LogOnStatus::new(codes::LOGIN_REJECTED_INVALID_PASSWORD),
                String::new()
            )
        );

        // The per-request handlers carry their own addressing.
        let handler_a = AuthenticateAccountHandler::new(loginapp_a, 100);
        let handler_b = AuthenticateAccountHandler::new(loginapp_b, 200);
        assert_eq!(handler_a.reply_addr(), loginapp_a);
        assert_eq!(handler_a.reply_id(), 100);
        assert_eq!(handler_b.reply_addr(), loginapp_b);
        assert_eq!(handler_b.reply_id(), 200);
        assert_ne!(handler_a.reply_addr(), handler_b.reply_addr());

        // The log-on mapping is exactly as it started.
        let map = system.logon_map();
        assert_eq!(map.len(), 2);
        assert_eq!(map["moe"].password, "secret");
        assert_eq!(map["moe"].entity_key.db_id, 1);
        assert_eq!(map["curly"].password, "curlypw");
        assert_eq!(map["curly"].entity_key.db_id, 2);
    }

    /// Concurrent logons through one Python billing system each get a
    /// fresh response object — the first request's report cannot leak
    /// into the second's outcome.
    #[test]
    fn interleaved_py_logons_do_not_share_responses() {
        use crate::py_billing::PyBillingScript;
        use crate::py_billing::{PyBillingResponse, PyBillingSystem};

        let defs_map = defs();
        let avatar = defs_map.name_to_index("Avatar").unwrap();

        let mut call = 0;
        let mut system = PyBillingSystem::new(
            PyBillingScript {
                get_entity_key_for_account: Some(Box::new(
                    move |_, _, _, response: &mut PyBillingResponse| {
                        call += 1;
                        if call == 1 {
                            response.load_entity("Avatar", 1, "", "")
                        } else {
                            response.failure_no_such_user("second logon refused")
                        }
                    },
                )),
                set_entity_key_for_account: None,
            },
            &defs_map,
        );

        let first = handle(Address::new(0, 0), 1, &mut system, "moe", "pw");
        let second = handle(Address::new(0, 0), 2, &mut system, "curly", "pw");
        assert_eq!(
            first,
            AuthenticateReply::LoggedOn(EntityKey {
                type_id: avatar,
                db_id: 1
            })
        );
        assert_eq!(
            second,
            AuthenticateReply::Rejected(
                LogOnStatus::new(codes::LOGIN_REJECTED_NO_SUCH_USER),
                "second logon refused".to_string()
            )
        );
    }

    // -- helpers ---------------------------------------------------------

    /// The fixture's two-entity database on its own, for configs that
    /// skip the log-on mapping.
    fn fixture_database() -> XmlDatabase {
        let mut database = XmlDatabase::new(defs());
        let avatar = defs().name_to_index("Avatar").unwrap();
        for (name, id) in [("moe", 1), ("curly", 2)] {
            let mut stream = BinaryStream::new();
            let _ = stream.write_string(name);
            stream.write_i32(10);
            stream.write_i64(0);
            database
                .put_entity_stream(avatar, id, &mut stream, AutoLoadUpdate::Retain, true)
                .expect("put");
        }
        database
    }
}
