//! BW-06-03-02 the BaseApp's pending logins and session keys.
//!
//! When a Proxy is created for a client that has just logged in via the
//! LoginApp, the BaseApp stores it in `PendingLogins`
//! (`server/baseapp/pending_logins.*`) keyed by the session key the client was
//! told. The client's first BaseApp packet (`LoginHandler::login`,
//! `server/baseapp/login_handler.cpp`) presents that key to be taken out of the
//! set:
//!
//! * `PendingLogins::add` binds the proxy to a fresh login key and arms a
//!   thirty-second (`PENDING_LOGINS_TIMEOUT`) expiry measured in game ticks;
//! * a `login` with a key that is not pending is rejected — the reference
//!   breaks the bundle loop and replies nothing, so an invalid or expired
//!   ticket is silently dropped;
//! * `PendingLogins::tick` expires timed-out entries, and the caller is told
//!   to call `onClientDeath(CLIENT_DISCONNECT_TIMEOUT)` on the proxy;
//! * a successful `login` erases the entry, and the caller attaches the proxy
//!   to the client's channel (BW-06-03-03).
//!
//! The expiry is measured in game ticks: `now` is `BaseApp::time()`, a
//! `GameTime` that advances by one per game tick, so the timeout is
//! `PENDING_LOGINS_TIMEOUT * update_hertz` ticks.

use std::collections::{HashMap, VecDeque};

use crate::baseapp_continuation::SessionKey;
use crate::baseappmgr::Addr;
use crate::shutdown::GameTime;

/// `PENDING_LOGINS_TIMEOUT`: 30 seconds, in the comment's own words
/// "Could make this configurable".
pub const PENDING_LOGINS_TIMEOUT: u32 = 30;

/// `ClientDisconnectReason::CLIENT_DISCONNECT_TIMEOUT` (`proxy.hpp`).
pub const CLIENT_DISCONNECT_TIMEOUT: u8 = 3;

/// One pending login: the proxy that logged in via the LoginApp, and the
/// address the client used to contact it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingLogin {
    /// The proxy's entity id.
    pub proxy_id: u32,
    /// The address the client used to reach the LoginApp.
    pub login_app_addr: Addr,
}

/// A time-sorted expiry record in the timeout queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct QueueElement {
    expiry_time: GameTime,
    proxy_id: u32,
    login_key: SessionKey,
}

impl QueueElement {
    /// `hasExpired`. The reference compares with `==` because it is ticked
    /// once per game tick; `<=` is the same under sequential ticks and also
    /// fires if a tick is ever skipped past the expiry.
    fn has_expired(&self, now: GameTime) -> bool {
        now >= self.expiry_time
    }
}

/// `PendingLogins` (`pending_logins.hpp`): the set of proxies awaiting the
/// client's confirmation, keyed by session key, plus the timeout queue.
pub struct PendingLogins {
    container: HashMap<SessionKey, PendingLogin>,
    queue: VecDeque<QueueElement>,
    num_internal_logins: u32,
    num_external_logins: u32,
    /// `NATConfig::isInternalIP`; the default classifies every address as
    /// external.
    is_internal_ip: Box<dyn Fn(u32) -> bool>,
}

impl PendingLogins {
    /// Constructor. `is_internal_ip` is the `NATConfig` classifier used to
    /// count internal versus external logins.
    pub fn new(is_internal_ip: impl Fn(u32) -> bool + 'static) -> Self {
        Self {
            container: HashMap::new(),
            queue: VecDeque::new(),
            num_internal_logins: 0,
            num_external_logins: 0,
            is_internal_ip: Box::new(is_internal_ip),
        }
    }

    /// Constructor with the default (always external) classifier.
    pub fn new_default() -> Self {
        Self::new(|_| false)
    }

    /// `find`: the pending login for a session key, or `None`.
    pub fn find(&self, key: SessionKey) -> Option<&PendingLogin> {
        self.container.get(&key)
    }

    /// `add`: bind the proxy to `login_key` and arm its timeout. Any prior
    /// pending entry for the same proxy is removed first, so a proxy is only
    /// in the list once. Returns the key the client must present.
    pub fn add(
        &mut self,
        proxy_id: u32,
        login_key: SessionKey,
        login_app_addr: Addr,
        now: GameTime,
        update_hertz: u32,
    ) -> SessionKey {
        // Make sure the proxy is only in the pending list once (the reference
        // brute-forces this too).
        let prior: Vec<SessionKey> = self
            .container
            .iter()
            .filter(|(_, pending)| pending.proxy_id == proxy_id)
            .map(|(key, _)| *key)
            .collect();
        for key in prior {
            self.container.remove(&key);
        }

        self.container.insert(
            login_key,
            PendingLogin {
                proxy_id,
                login_app_addr,
            },
        );

        let expiry_time = now.saturating_add(PENDING_LOGINS_TIMEOUT.saturating_mul(update_hertz));
        self.queue.push_back(QueueElement {
            expiry_time,
            proxy_id,
            login_key,
        });

        login_key
    }

    /// `erase`: remove the pending login and count it.
    pub fn erase(&mut self, key: SessionKey) -> Option<PendingLogin> {
        let pending = self.container.remove(&key)?;
        self.count(pending.login_app_addr);
        Some(pending)
    }

    /// Count an address toward the internal or external login tally.
    fn count(&mut self, addr: Addr) {
        if (self.is_internal_ip)(addr.0) {
            self.num_internal_logins += 1;
        } else {
            self.num_external_logins += 1;
        }
    }

    /// `tick`: expire timed-out entries still pending. The returned entries
    /// tell the caller which proxies to `onClientDeath(CLIENT_DISCONNECT_TIMEOUT)`;
    /// the entry is removed here.
    pub fn tick(&mut self, now: GameTime) -> Vec<ExpiredLogin> {
        let mut expired = Vec::new();
        while let Some(front) = self.queue.front().copied() {
            if !front.has_expired(now) {
                break;
            }
            self.queue.pop_front();
            if let Some(pending) = self.container.remove(&front.login_key) {
                let is_first_login = self.num_internal_logins == 0 && self.num_external_logins == 0;
                let is_first_external_login = self.num_external_logins == 0
                    && !(self.is_internal_ip)(pending.login_app_addr.0);
                self.count(pending.login_app_addr);
                expired.push(ExpiredLogin {
                    proxy_id: front.proxy_id,
                    client_addr: pending.login_app_addr,
                    is_first_login,
                    is_first_external_login,
                });
            }
            // Otherwise the login already succeeded and the queue entry is
            // stale; just pop it.
        }
        expired
    }

    /// `LoginHandler::login` (`login_handler.cpp`): the client presents the
    /// session key to take its proxy out of the pending set.
    ///
    /// `proxy_destroyed` is whether the proxy has already been destroyed, and
    /// `has_channel` whether the client's address already has a channel (a
    /// login collision).
    pub fn login(
        &mut self,
        key: SessionKey,
        proxy_destroyed: bool,
        has_channel: bool,
        num_attempts: u32,
    ) -> LoginDecision {
        let Some(pending) = self.find(key).copied() else {
            // No pending login for this key: break the bundle loop, reply
            // nothing. The ticket is invalid or already expired.
            return LoginDecision::InvalidTicket;
        };
        if proxy_destroyed {
            return LoginDecision::ProxyDestroyed;
        }
        if has_channel {
            return LoginDecision::ChannelCollision;
        }
        self.erase(key);
        LoginDecision::AttachClient {
            proxy_id: pending.proxy_id,
            login_app_addr: pending.login_app_addr,
            num_attempts,
        }
    }
}

/// A pending login that timed out (`PendingLogins::tick`): the caller calls
/// `onClientDeath(CLIENT_DISCONNECT_TIMEOUT)` for the proxy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpiredLogin {
    /// The proxy to disconnect.
    pub proxy_id: u32,
    /// The address the client used to reach the LoginApp.
    pub client_addr: Addr,
    /// `isFirstLogin`: no login has been recorded at all.
    pub is_first_login: bool,
    /// `isFirstExternalLogin`: no external login has been recorded.
    pub is_first_external_login: bool,
}

/// The `LoginHandler::login` decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginDecision {
    /// The key is not pending: the ticket is invalid or expired. The reference
    /// breaks the bundle loop and replies nothing.
    InvalidTicket,
    /// The proxy has been destroyed; the login is dropped without a reply.
    ProxyDestroyed,
    /// The client's address already has a channel; the login is dropped.
    ChannelCollision,
    /// The pending entry matched and was erased; the caller attaches the proxy
    /// to the client's channel.
    AttachClient {
        /// The proxy to attach.
        proxy_id: u32,
        /// The address the client used to reach the LoginApp (for the login
        /// statistics).
        login_app_addr: Addr,
        /// The client's attempt number (for the login statistics).
        num_attempts: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROXY_ID: u32 = 7;
    const KEY: SessionKey = 0x1234_5678;
    const LOGIN_APP: Addr = (0x0a00_0001, 20000);
    const HERTZ: u32 = 20;
    const NOW: GameTime = 100;

    fn pending() -> PendingLogins {
        PendingLogins::new_default()
    }

    #[test]
    fn add_binds_a_key_and_finds_it_again() {
        let mut pending = pending();
        let key = pending.add(PROXY_ID, KEY, LOGIN_APP, NOW, HERTZ);
        assert_eq!(key, KEY);
        assert_eq!(
            pending.find(key),
            Some(&PendingLogin {
                proxy_id: PROXY_ID,
                login_app_addr: LOGIN_APP,
            })
        );
    }

    #[test]
    fn a_proxy_is_only_pending_once() {
        let mut pending = pending();
        pending.add(PROXY_ID, KEY, LOGIN_APP, NOW, HERTZ);
        // The same proxy added again displaces its earlier entry.
        pending.add(PROXY_ID, KEY + 1, (0x0a00_0002, 20001), NOW, HERTZ);
        assert_eq!(pending.find(KEY), None);
        assert_eq!(pending.find(KEY + 1).unwrap().proxy_id, PROXY_ID);
    }

    #[test]
    fn an_unknown_key_is_an_invalid_ticket() {
        let mut pending = pending();
        assert_eq!(
            pending.login(KEY, false, false, 1),
            LoginDecision::InvalidTicket
        );
    }

    #[test]
    fn a_matched_key_attaches_the_proxy() {
        let mut pending = pending();
        pending.add(PROXY_ID, KEY, LOGIN_APP, NOW, HERTZ);
        assert_eq!(
            pending.login(KEY, false, false, 3),
            LoginDecision::AttachClient {
                proxy_id: PROXY_ID,
                login_app_addr: LOGIN_APP,
                num_attempts: 3,
            }
        );
        // The entry was erased.
        assert_eq!(pending.find(KEY), None);
    }

    #[test]
    fn a_destroyed_proxy_is_dropped() {
        let mut pending = pending();
        pending.add(PROXY_ID, KEY, LOGIN_APP, NOW, HERTZ);
        assert_eq!(
            pending.login(KEY, true, false, 1),
            LoginDecision::ProxyDestroyed
        );
        // The entry survives for a later attempt.
        assert_eq!(pending.find(KEY).unwrap().proxy_id, PROXY_ID);
    }

    #[test]
    fn a_channel_collision_is_dropped() {
        let mut pending = pending();
        pending.add(PROXY_ID, KEY, LOGIN_APP, NOW, HERTZ);
        assert_eq!(
            pending.login(KEY, false, true, 1),
            LoginDecision::ChannelCollision
        );
        assert_eq!(pending.find(KEY).unwrap().proxy_id, PROXY_ID);
    }

    #[test]
    fn an_expired_ticket_is_removed_and_reported() {
        let mut pending = pending();
        pending.add(PROXY_ID, KEY, LOGIN_APP, NOW, HERTZ);
        let expiry = NOW + PENDING_LOGINS_TIMEOUT * HERTZ;
        // Before the timeout nothing expires.
        assert!(pending.tick(expiry - 1).is_empty());
        // At the timeout the entry expires and is told to die.
        let expired = pending.tick(expiry);
        assert_eq!(
            expired,
            vec![ExpiredLogin {
                proxy_id: PROXY_ID,
                client_addr: LOGIN_APP,
                is_first_login: true,
                is_first_external_login: true,
            }]
        );
        assert_eq!(pending.find(KEY), None);
        // The ticket is now invalid.
        assert_eq!(
            pending.login(KEY, false, false, 1),
            LoginDecision::InvalidTicket
        );
    }

    #[test]
    fn a_successful_login_leaves_a_stale_queue_entry() {
        let mut pending = pending();
        pending.add(PROXY_ID, KEY, LOGIN_APP, NOW, HERTZ);
        pending.login(KEY, false, false, 1);
        // The queue entry is stale: ticking to the expiry reports nothing.
        let expiry = NOW + PENDING_LOGINS_TIMEOUT * HERTZ;
        assert!(pending.tick(expiry).is_empty());
    }

    #[test]
    fn internal_addresses_are_counted_separately() {
        let mut pending = PendingLogins::new(|ip| ip == 0x0a00_0001);
        pending.add(PROXY_ID, KEY, LOGIN_APP, NOW, HERTZ);
        let expired = pending.tick(NOW + PENDING_LOGINS_TIMEOUT * HERTZ);
        // The address is classified as internal, so it is not the first
        // external login, but it is the first login overall.
        assert!(expired[0].is_first_login);
        assert!(!expired[0].is_first_external_login);
    }

    #[test]
    fn erase_reports_the_removed_entry() {
        let mut pending = pending();
        pending.add(PROXY_ID, KEY, LOGIN_APP, NOW, HERTZ);
        assert_eq!(
            pending.erase(KEY),
            Some(PendingLogin {
                proxy_id: PROXY_ID,
                login_app_addr: LOGIN_APP,
            })
        );
        assert_eq!(pending.erase(KEY), None);
    }
}
