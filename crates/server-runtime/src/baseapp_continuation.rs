//! BW-06-03-01 the BaseApp continuation address.
//!
//! A logged-on client does not talk to the LoginApp: the login reply tells it
//! which BaseApp to talk to, and the BaseApp's external address plus a session
//! key travel back through the server chain so the client can switch:
//!
//! * the BaseApp creates the Proxy (`EntityCreator::createBaseWithCellData`,
//!   `entity_creator.cpp`), and for a Proxy with a client address calls
//!   `Proxy::prepareForLogin`, which regenerates the session key and replies
//!   with the entity mailbox ref followed by that key;
//! * the reply is relayed BaseApp → BaseAppMgr → DBApp (`LoginHandler::handleMessage`)
//!   → LoginApp → client, so the client receives `LOGGED_ON`, the BaseApp
//!   address and the session key;
//! * the client switches in this order (`ServerConnection::logOnComplete`,
//!   `server_connection.cpp`): when the status is `LOGGED_ON` and the
//!   connection is still online, it sanity-checks that the reply's BaseApp
//!   address matches the BaseApp that answered, clears the entity id, sends an
//!   initial packet to the proxy to open a firewall hole on its side, and only
//!   then installs the server message handler. Anything else is a failure.
//!
//! The session key is the reference `Proxy::regenerateSessionKey`
//! (`server/baseapp/proxy.cpp`): a `uint32` of `timestamp()`, retried until
//! it is non-zero.

/// `SessionKey` (`lib/connection/server_connection.hpp`).
pub type SessionKey = u32;

/// One address pair, `(ip, port)`.
pub type Addr = crate::baseappmgr::Addr;

/// The reference `BasePtr` reply: `EntityMailBoxRef`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntityMailBoxRef {
    /// `id`.
    pub id: u32,
    /// `addr`.
    pub addr: Addr,
}

impl EntityMailBoxRef {
    /// Constructor.
    pub fn new(id: u32, addr: Addr) -> Self {
        Self { id, addr }
    }

    /// `ref.init()`: an empty (failed) reply.
    pub const NONE: Self = Self {
        id: 0,
        addr: (0, 0),
    };
}

/// `Proxy::regenerateSessionKey` (`proxy.cpp`): a `uint32` of `timestamp()`,
/// retried until non-zero.
pub fn regenerate_session_key(now: u64) -> SessionKey {
    let mut key = now as SessionKey;
    if key == 0 {
        key = 1;
    }
    key
}

/// The session side of one Proxy (`Proxy::prepareForLogin`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProxySession {
    client_addr: Option<Addr>,
    session_key: SessionKey,
}

impl ProxySession {
    /// Constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// `prepareForLogin`: the client address is recorded and the session key
    /// is regenerated; the key is what the client is told to present.
    pub fn prepare_for_login(&mut self, client_addr: Addr, now: u64) -> SessionKey {
        self.client_addr = Some(client_addr);
        self.session_key = regenerate_session_key(now);
        self.session_key
    }

    /// `clientAddr_`.
    pub fn client_addr(&self) -> Option<Addr> {
        self.client_addr
    }

    /// `sessionKey_`.
    pub fn session_key(&self) -> SessionKey {
        self.session_key
    }
}

/// The `createBaseWithCellData` reply: the entity mailbox ref, and the login
/// key when the base is a Proxy with a client address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContinuationReply {
    /// The proxy's mailbox ref (`EntityMailBoxRef`).
    pub entity_mailbox: EntityMailBoxRef,
    /// The session key, `None` when the base is not a Proxy with a client.
    pub session_key: Option<SessionKey>,
}

impl ContinuationReply {
    /// Whether the proxy was created (`ref.addr.ip != 0`).
    pub fn is_ok(&self) -> bool {
        self.entity_mailbox.addr.0 != 0
    }
}

/// The login reply the client receives once the chain is complete: the
/// continuation address and session key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoggedOnReply {
    /// The BaseApp external address the client switches to.
    pub server_addr: Addr,
    /// The session key.
    pub session_key: SessionKey,
}

/// The client-side result of `ServerConnection::logOnComplete`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientSwitch {
    /// The client continues to the BaseApp at the reply's address.
    ContinueToBaseApp(LoggedOnReply),
    /// `LOGGED_ON` but the connection is no longer online: `CANCELLED`.
    AlreadyLoggedOff,
    /// The login failed with the status.
    LoginFailed { status: u8 },
}

/// `ServerConnection::logOnComplete`: the client's ordered switch to the
/// BaseApp.
///
/// `status` is the login reply's `LogOnStatus`, `is_online` whether the
/// connection is still online, `winning_addr` the BaseApp that actually
/// answered, and `reply` the continuation reply the chain produced.
pub fn log_on_complete(
    status: u8,
    is_online: bool,
    winning_addr: Addr,
    reply: Option<LoggedOnReply>,
) -> ClientSwitch {
    if status == 1 {
        // LOGGED_ON
        if !is_online {
            return ClientSwitch::AlreadyLoggedOff;
        }
        let reply = match reply {
            Some(reply) => reply,
            None => return ClientSwitch::LoginFailed { status: 1 },
        };
        // Sanity check: the reply's BaseApp address must be the one that
        // answered.
        if reply.server_addr != winning_addr {
            return ClientSwitch::LoginFailed { status: 1 };
        }
        ClientSwitch::ContinueToBaseApp(reply)
    } else {
        ClientSwitch::LoginFailed { status }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_APP: Addr = (0x0a00_0001, 20000);
    const NOW: u64 = 1_000_000_000;

    #[test]
    fn a_session_key_is_never_zero() {
        assert_ne!(regenerate_session_key(0), 0);
        assert_eq!(regenerate_session_key(0), 1);
        assert_eq!(regenerate_session_key(NOW), (NOW as u32));
    }

    #[test]
    fn prepare_for_login_records_the_client_and_issues_a_key() {
        let mut session = ProxySession::new();
        let key = session.prepare_for_login(BASE_APP, NOW);
        assert_eq!(session.client_addr(), Some(BASE_APP));
        assert_eq!(session.session_key(), key);
        assert_ne!(key, 0);
        // A second preparation issues a different key.
        assert_ne!(session.prepare_for_login(BASE_APP, NOW + 1), key);
    }

    #[test]
    fn the_reply_carries_the_proxy_address_and_the_key() {
        let mut session = ProxySession::new();
        let key = session.prepare_for_login(BASE_APP, NOW);
        let reply = ContinuationReply {
            entity_mailbox: EntityMailBoxRef::new(7, BASE_APP),
            session_key: Some(key),
        };
        assert!(reply.is_ok());
        assert_eq!(reply.session_key, Some(key));

        // An empty ref is a failed creation.
        assert!(
            !ContinuationReply {
                entity_mailbox: EntityMailBoxRef::NONE,
                session_key: None,
            }
            .is_ok()
        );
    }

    #[test]
    fn a_logged_on_client_switches_to_the_baseapp() {
        let reply = LoggedOnReply {
            server_addr: BASE_APP,
            session_key: 0x1234_5678,
        };
        assert_eq!(
            log_on_complete(1, true, BASE_APP, Some(reply)),
            ClientSwitch::ContinueToBaseApp(reply)
        );
    }

    #[test]
    fn a_logged_on_client_that_is_already_off_is_cancelled() {
        let reply = LoggedOnReply {
            server_addr: BASE_APP,
            session_key: 1,
        };
        assert_eq!(
            log_on_complete(1, false, BASE_APP, Some(reply)),
            ClientSwitch::AlreadyLoggedOff
        );
    }

    #[test]
    fn a_mismatching_baseapp_address_fails() {
        let reply = LoggedOnReply {
            server_addr: BASE_APP,
            session_key: 1,
        };
        assert_eq!(
            log_on_complete(1, true, (0x0b00_0001, 20000), Some(reply)),
            ClientSwitch::LoginFailed { status: 1 }
        );
    }

    #[test]
    fn a_failed_login_status_passes_through() {
        // 68 is LOGIN_REJECTED_INVALID_PASSWORD.
        assert_eq!(
            log_on_complete(68, true, BASE_APP, None),
            ClientSwitch::LoginFailed { status: 68 }
        );
    }
}
