//! BW-05-05 the DBAppMgr discovery and the DBApp-Alpha role.
//!
//! The reference `server/dbappmgr/` tracks the registered DBApps
//! (`dbappmgr.cpp`, `dbapp.cpp`). Discovery is ID-based: every `addDBApp`
//! gets a fresh monotonically increasing `DBAppID` (`lastDBAppID_`), and the
//! DBApp with the smallest ID is the alpha (`DBAppMgr::alphaID`). The
//! current DBApp set is broadcast as a *hash* — a packed count followed by
//! `(id, address)` pairs, streamed by `DBApps::MapStreaming` — to the
//! BaseAppMgr, to every DBApp, and, whenever the alpha changed, to the
//! CellAppMgr (`setDBAppAlpha`) and each LoginApp (`notifyDBAppAlpha`).
//!
//! The alpha also carries the first-time-startup duty: while the server has
//! not started, the hash sent to the alpha sets
//! `shouldAlphaResetGameServerState` and moves the manager to
//! `STARTUP_STATE_INDETERMINATE` until the alpha reports
//! `serverHasStarted`. Additions are refused (zero-length reply) while
//! indeterminate, and an alpha that dies during the window defers to the
//! BaseAppMgr's answer to `requestHasStarted`. A recovering manager learns
//! its old IDs back via `recoverDBApp` / `recoverLoginApp`, so the topology
//! stays comparable across the restart.
//!
//! The Mercury traffic is not modelled here: the module records the state
//! transitions and collects the outbound messages the reference would send
//! into the [`Outbound`] outbox, so the Mercury lane can drive the sockets
//! when it lands. Addresses are `(ip, port)` with `ip` in network byte
//! order, matching the rest of the workspace.

use std::collections::{BTreeMap, BTreeSet};

use foundation::stream::{BinaryStream, StreamError};

use crate::shutdown::ShutDownStage;

/// A Mercury address as `(ip, port)` with `ip` in network byte order.
pub type Addr = (u32, u16);

/// `DBAppID`.
pub type DbAppId = u32;

/// `LoginAppID`.
pub type LoginAppId = u32;

/// `Mercury::Address::NONE` (`0.0.0.0:0`).
pub const NONE_ADDR: Addr = (0, 0);

/// `Mercury::REPLY_ID_NONE`.
pub const REPLY_ID_NONE: i32 = -1;

/// `DBAppMgr::ADD_WAIT_TIME_SECONDS`, in milliseconds: non-alpha DBApp
/// additions are batched for at least this long before the hash update goes
/// out, so DBApps that start together share one update.
pub const ADD_WAIT_TIME_MS: u64 = 1000;

/// The once-off `gatherLoginAppsTimer_`, in milliseconds: a recovering
/// manager that finds the server already started collects the old LoginApps
/// for this long before accepting new ones.
pub const GATHER_LOGIN_APPS_MS: u64 = 2000;

// -----------------------------------------------------------------------------
// Section: DBApp
// -----------------------------------------------------------------------------

/// One registered DBApp (`DBApp`, `dbapp.hpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbApp {
    /// `id_`.
    pub id: DbAppId,
    /// `channelOwner_.channel().addr()`.
    pub addr: Addr,
    /// `pendingReplyID_`: the `addDBApp` reply the manager holds back until
    /// the first hash update, `None` for `Mercury::REPLY_ID_NONE`. A
    /// recovered DBApp has none.
    pub pending_reply: Option<i32>,
}

// -----------------------------------------------------------------------------
// Section: startup state
// -----------------------------------------------------------------------------

/// `DBAppMgr::StartupState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StartupState {
    /// `STARTUP_STATE_NOT_STARTED`: no DBApp-Alpha has started the rest of
    /// the server yet.
    #[default]
    NotStarted,
    /// `STARTUP_STATE_STARTED`.
    Started,
    /// `STARTUP_STATE_INDETERMINATE`: waiting for the alpha to report
    /// `serverHasStarted`.
    Indeterminate,
}

// -----------------------------------------------------------------------------
// Section: outbound messages
// -----------------------------------------------------------------------------

/// The outbound messages the reference DBAppMgr would send, collected so
/// the Mercury lane can drive the sockets when it lands. The hash bytes are
/// exactly what the reference streams and can be read back with
/// [`decode_db_app_hash`].
#[derive(Debug, Clone, PartialEq)]
pub enum Outbound {
    /// `BaseAppMgrInterface::updateDBAppHash` with the streamed DBApp hash.
    UpdateDbAppHashToBaseAppMgr(Vec<u8>),
    /// `CellAppMgrInterface::setDBAppAlpha` with the alpha address.
    SetDbAppAlphaToCellAppMgr(Addr),
    /// `LoginIntInterface::notifyDBAppAlpha`: `(login app, alpha)`.
    NotifyDbAppAlpha(Addr, Addr),
    /// `DBAppInterface::updateDBAppHash` sent as the reply to this DBApp's
    /// `addDBApp` request: `(dbapp, reply id, own id, should alpha reset
    /// game server state, hash)`. The own ID rides along only on the reply,
    /// which is how a fresh DBApp learns its ID.
    DbAppHashAddReply(Addr, i32, DbAppId, bool, Vec<u8>),
    /// `DBAppInterface::updateDBAppHash` as a plain message: `(dbapp,
    /// should alpha reset game server state, hash)`.
    DbAppHashMessage(Addr, bool, Vec<u8>),
    /// `BaseAppMgrInterface::requestHasStarted`, sent after an alpha died
    /// before reporting `serverHasStarted`.
    RequestHasStarted,
    /// A zero-length reply telling the requester to try again later:
    /// `(addr, reply id)`.
    EmptyReply(Addr, i32),
    /// The `addLoginApp` reply: `(login app, reply id, login app id, DBApp
    /// alpha address)`.
    LoginAppAddReply(Addr, i32, LoginAppId, Addr),
    /// `BaseAppMgrInterface::controlledShutDown` at `SHUTDOWN_REQUEST`,
    /// forwarded by the manager's controlled shutdown.
    ShutDownRequestToBaseAppMgr(Addr),
    /// `DBAppInterface::controlledShutDown` at `SHUTDOWN_PERFORM`, to one
    /// DBApp.
    DbAppPerform(Addr),
}

/// `DBAppMgr::controlledShutDown` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbAppMgrShutDownOutcome {
    /// `SHUTDOWN_REQUEST`: the manager is now shutting down and the request
    /// is forwarded to the BaseAppMgr at the given address.
    ForwardRequestToBaseAppMgr(Option<Addr>),
    /// `SHUTDOWN_PERFORM`: every DBApp is told to shut down, then the
    /// manager itself.
    Perform {
        /// The DBApps the manager told, in ascending ID order.
        db_apps: Vec<Addr>,
    },
    /// A stage the manager does not handle.
    Unhandled(ShutDownStage),
}

/// `DBAppMgr::addLoginApp` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginAppAddOutcome {
    /// A non-request message; ignored.
    Ignored,
    /// Registered; the reply carries the ID and the alpha address.
    Accepted {
        /// `++lastLoginAppID_`.
        id: LoginAppId,
        /// The DBApp alpha address.
        alpha: Addr,
    },
    /// Zero-length reply; the LoginApp tries again later.
    Retry,
}

// -----------------------------------------------------------------------------
// Section: DBAppMgr
// -----------------------------------------------------------------------------

/// The DBAppMgr process state (`DBAppMgr`, `dbappmgr.cpp`).
#[derive(Debug, Default)]
pub struct DbAppMgr {
    /// `dbApps_`, keyed by `DBAppID`; iteration is ascending, so the first
    /// entry is the alpha.
    pub db_apps: BTreeMap<DbAppId, DbApp>,
    /// `addressMap_`: address to owning `DBAppID`.
    pub address_map: BTreeMap<Addr, DbAppId>,
    /// `dbAppAddStartWaitTime_`: when the current non-alpha batch opened,
    /// `None` for the reference's `TimeStamp(0)`.
    pub db_app_add_start_wait_ms: Option<u64>,
    /// `lastDBAppID_`.
    pub last_db_app_id: DbAppId,
    /// `loginApps_`.
    pub login_apps: BTreeSet<Addr>,
    /// `shouldAcceptLoginApps_`.
    pub should_accept_login_apps: bool,
    /// Whether the once-off `gatherLoginAppsTimer_` is still running.
    pub gather_login_apps_pending: bool,
    /// `lastLoginAppID_`.
    pub last_login_app_id: LoginAppId,
    /// `startupState_`.
    pub startup_state: StartupState,
    /// `isShuttingDown_`.
    pub is_shutting_down: bool,
    /// `baseAppMgr_` channel address.
    pub base_app_mgr_addr: Option<Addr>,
    /// `cellAppMgr_` channel address.
    pub cell_app_mgr_addr: Option<Addr>,
    /// The outbound messages accumulated so far, in order.
    pub outbox: Vec<Outbound>,
}

impl DbAppMgr {
    /// A fresh manager; `shouldAcceptLoginApps_` starts `true` as in the
    /// reference constructor.
    pub fn new() -> Self {
        Self {
            should_accept_login_apps: true,
            ..Self::default()
        }
    }

    /// `DBAppMgr::alphaID`: the smallest registered `DBAppID`.
    pub fn alpha_id(&self) -> Option<DbAppId> {
        self.db_apps.keys().next().copied()
    }

    /// `DBAppMgr::dbAppAlpha`.
    pub fn db_app_alpha(&self) -> Option<&DbApp> {
        self.db_apps.values().next()
    }

    /// The alpha's address, `None` when no DBApp is registered.
    pub fn alpha_addr(&self) -> Option<Addr> {
        self.db_app_alpha().map(|app| app.addr)
    }

    /// `DBAppMgr::findApp`.
    pub fn find_app(&self, addr: Addr) -> Option<&DbApp> {
        self.address_map
            .get(&addr)
            .and_then(|id| self.db_apps.get(id))
    }

    /// `DBAppMgr::controlledShutDown`: `SHUTDOWN_REQUEST` marks the manager
    /// as shutting down and forwards the request to the BaseAppMgr, which in
    /// turn reaches the CellAppMgr; `SHUTDOWN_PERFORM` tells every DBApp to
    /// shut down, after which the manager drains its channels and exits.
    /// Other stages are the reference's `ERROR_MSG` arm and are ignored.
    pub fn controlled_shut_down(&mut self, stage: ShutDownStage) -> DbAppMgrShutDownOutcome {
        match stage {
            ShutDownStage::Request => {
                // Make sure we no longer send to anonymous channels etc.
                self.is_shutting_down = true;
                let target = self.base_app_mgr_addr;
                if let Some(addr) = target {
                    self.outbox
                        .push(Outbound::ShutDownRequestToBaseAppMgr(addr));
                }
                DbAppMgrShutDownOutcome::ForwardRequestToBaseAppMgr(target)
            }
            ShutDownStage::Perform => {
                let db_apps: Vec<Addr> = self.db_apps.values().map(|app| app.addr).collect();
                for addr in &db_apps {
                    self.outbox.push(Outbound::DbAppPerform(*addr));
                }
                DbAppMgrShutDownOutcome::Perform { db_apps }
            }
            stage => DbAppMgrShutDownOutcome::Unhandled(stage),
        }
    }

    /// The tail of `DBAppMgr::init` once the BaseAppMgr start status is
    /// known: a recovering manager that finds the server already started
    /// waits for the old DBApps and LoginApps to re-register before
    /// accepting new LoginApps.
    pub fn startup(&mut self, has_started: bool, now_ms: u64) {
        if has_started {
            self.db_app_add_start_wait_ms = Some(now_ms);
            self.gather_login_apps_pending = true;
            self.should_accept_login_apps = false;
            self.startup_state = StartupState::Started;
        } else {
            self.startup_state = StartupState::NotStarted;
        }
    }

    /// `DBAppMgr::addDBApp`. `reply_id` is the add request's reply ID the
    /// reference parks in `DBApp::pendingReplyID_`.
    pub fn add_db_app(&mut self, addr: Addr, reply_id: Option<i32>, now_ms: u64) {
        let was_empty = self.db_apps.is_empty();

        if self.startup_state == StartupState::Indeterminate {
            // The alpha has not finished initialisation yet; the DBApp
            // retries its add.
            self.outbox.push(Outbound::EmptyReply(
                addr,
                reply_id.unwrap_or(REPLY_ID_NONE),
            ));
            return;
        }

        self.last_db_app_id += 1;
        let id = self.last_db_app_id;
        self.db_apps.insert(
            id,
            DbApp {
                id,
                addr,
                pending_reply: reply_id,
            },
        );
        self.address_map.insert(addr, id);

        if was_empty {
            // The first DBApp becomes the alpha; tell everyone immediately.
            self.send_db_app_hash_update(true);
        } else {
            // Batch non-alpha additions so DBApps starting together share
            // one hash update.
            self.db_app_add_start_wait_ms = Some(now_ms);
        }
    }

    /// `DBAppMgr::recoverDBApp`: an already-running DBApp re-registers with
    /// the ID it held before, so the topology keeps its IDs across a
    /// manager restart. No hash update goes out here.
    pub fn recover_db_app(&mut self, addr: Addr, id: DbAppId) {
        if id > self.last_db_app_id {
            self.last_db_app_id = id;
        }
        self.db_apps.insert(
            id,
            DbApp {
                id,
                addr,
                pending_reply: None,
            },
        );
        self.address_map.insert(addr, id);
    }

    /// `DBAppMgr::handleDBAppDeath`.
    pub fn handle_db_app_death(&mut self, addr: Addr) {
        let Some(&id) = self.address_map.get(&addr) else {
            // Informed about an unknown DBApp.
            return;
        };

        let was_alpha = self.alpha_addr() == Some(addr);

        self.db_apps.remove(&id);
        self.address_map.remove(&addr);

        if self.is_shutting_down {
            return;
        }

        if was_alpha && self.startup_state == StartupState::Indeterminate {
            // The alpha died before it could report `serverHasStarted`; ask
            // the BaseAppMgr whether it actually got that far. If not, the
            // next alpha retries the first-time initialisation.
            self.outbox.push(Outbound::RequestHasStarted);
            return;
        }

        if self.db_apps.is_empty() {
            // No DBApps left; the reference logs an error and leaves a TODO
            // to trigger a controlled shutdown here.
        }

        self.send_db_app_hash_update(was_alpha);
    }

    /// `DBAppMgr::sendDBAppHashUpdate`: the DBApp hash to every interested
    /// process, with the CellAppMgr and LoginApps notified only when the
    /// alpha changed.
    pub fn send_db_app_hash_update(&mut self, have_new_alpha: bool) {
        // Inform BaseAppMgr.
        self.send_db_app_hash_update_to_base_app_mgr();

        // Inform each DBApp; the first in ascending ID order is the alpha.
        let is_not_started = self.startup_state == StartupState::NotStarted;
        let hash = self.hash_bytes();
        for (index, db_app) in self.db_apps.values_mut().enumerate() {
            let should_alpha_reset = index == 0 && is_not_started;
            if let Some(reply) = db_app.pending_reply.take() {
                self.outbox.push(Outbound::DbAppHashAddReply(
                    db_app.addr,
                    reply,
                    db_app.id,
                    should_alpha_reset,
                    hash.clone(),
                ));
            } else {
                self.outbox.push(Outbound::DbAppHashMessage(
                    db_app.addr,
                    should_alpha_reset,
                    hash.clone(),
                ));
            }
            if should_alpha_reset {
                // Wait for this alpha to report `serverHasStarted` before
                // directing another reset.
                self.startup_state = StartupState::Indeterminate;
            }
        }

        if have_new_alpha {
            let new_alpha = self.alpha_addr().unwrap_or(NONE_ADDR);

            // Inform CellAppMgr.
            self.send_db_app_hash_update_to_cell_app_mgr();

            // Inform LoginApps.
            for &login in self.login_apps.iter() {
                self.outbox
                    .push(Outbound::NotifyDbAppAlpha(login, new_alpha));
            }
        }

        self.db_app_add_start_wait_ms = None;
    }

    /// `DBAppMgr::sendDBAppHashUpdateToBaseAppMgr`.
    pub fn send_db_app_hash_update_to_base_app_mgr(&mut self) {
        self.outbox
            .push(Outbound::UpdateDbAppHashToBaseAppMgr(self.hash_bytes()));
    }

    /// `DBAppMgr::sendDBAppHashUpdateToCellAppMgr`.
    pub fn send_db_app_hash_update_to_cell_app_mgr(&mut self) {
        let alpha = self.alpha_addr().unwrap_or(NONE_ADDR);
        self.outbox.push(Outbound::SetDbAppAlphaToCellAppMgr(alpha));
    }

    /// The DBApp hash the reference streams (`DBApps::MapStreaming`): a
    /// packed count followed by `(DBAppID, address)` pairs in ascending ID
    /// order.
    pub fn hash_bytes(&self) -> Vec<u8> {
        let mut stream = BinaryStream::new();
        let _ = stream.write_packed_int(self.db_apps.len() as u32);
        for db_app in self.db_apps.values() {
            stream.write_u32(db_app.id);
            stream.write_u32(db_app.addr.0);
            stream.write_u16(db_app.addr.1);
        }
        stream.as_bytes().to_vec()
    }

    /// `DBAppMgr::addLoginApp`.
    pub fn add_login_app(&mut self, addr: Addr, reply_id: Option<i32>) -> LoginAppAddOutcome {
        let Some(reply_id) = reply_id else {
            // Got a non-request message; ignoring.
            return LoginAppAddOutcome::Ignored;
        };

        if !self.should_accept_login_apps || self.db_apps.is_empty() {
            // Zero length reply: try again later.
            self.outbox.push(Outbound::EmptyReply(addr, reply_id));
            return LoginAppAddOutcome::Retry;
        }

        self.login_apps.insert(addr);
        self.last_login_app_id += 1;
        let id = self.last_login_app_id;
        let alpha = self.alpha_addr().unwrap_or(NONE_ADDR);
        self.outbox
            .push(Outbound::LoginAppAddReply(addr, reply_id, id, alpha));
        LoginAppAddOutcome::Accepted { id, alpha }
    }

    /// `DBAppMgr::recoverLoginApp`.
    pub fn recover_login_app(&mut self, addr: Addr, id: LoginAppId) {
        self.login_apps.insert(addr);
        if id > self.last_login_app_id {
            self.last_login_app_id = id;
        }
    }

    /// `DBAppMgr::handleLoginAppDeath`.
    pub fn handle_login_app_death(&mut self, addr: Addr) {
        self.login_apps.remove(&addr);
    }

    /// `DBAppMgr::handleBaseAppMgrBirth`: a revived BaseAppMgr is re-sent
    /// the current hash.
    pub fn handle_base_app_mgr_birth(&mut self, addr: Addr) {
        self.base_app_mgr_addr = Some(addr);
        self.send_db_app_hash_update_to_base_app_mgr();
    }

    /// `DBAppMgr::handleCellAppMgrBirth`: a revived CellAppMgr is re-sent
    /// the current alpha.
    pub fn handle_cell_app_mgr_birth(&mut self, addr: Addr) {
        self.cell_app_mgr_addr = Some(addr);
        self.send_db_app_hash_update_to_cell_app_mgr();
    }

    /// `DBAppMgr::serverHasStarted`: the alpha reported the outcome of the
    /// first-time startup (or the BaseAppMgr answered `requestHasStarted`
    /// after an alpha died mid-initialisation).
    pub fn server_has_started(&mut self, has_started: bool) {
        debug_assert_eq!(self.startup_state, StartupState::Indeterminate);

        self.startup_state = if has_started {
            StartupState::Started
        } else {
            StartupState::NotStarted
        };

        if !self.db_apps.is_empty() {
            // DBApps registered since the dead alpha first did; the new
            // alpha retries the first-time initialisation if `!has_started`.
            self.send_db_app_hash_update(true);
        }
    }

    /// The `TIMEOUT_GATHER_LOGIN_APPS` expiry: the recovering manager now
    /// accepts new LoginApps.
    pub fn handle_gather_login_apps_timeout(&mut self) {
        self.gather_login_apps_pending = false;
        self.should_accept_login_apps = true;
    }

    /// `DBAppMgr::onStartOfTick`: flush a batched non-alpha addition once
    /// `ADD_WAIT_TIME_SECONDS` have passed.
    pub fn on_start_of_tick(&mut self, now_ms: u64) {
        let Some(start) = self.db_app_add_start_wait_ms else {
            return;
        };
        if now_ms.saturating_sub(start) >= ADD_WAIT_TIME_MS {
            self.send_db_app_hash_update(false);
        }
    }
}

/// Decodes the DBApp hash the reference streams: a packed count followed by
/// `(DBAppID, address)` pairs.
pub fn decode_db_app_hash(bytes: &[u8]) -> Result<Vec<(DbAppId, Addr)>, StreamError> {
    let mut stream = BinaryStream::from_slice(bytes);
    let count = stream.read_packed_int()?;
    let mut apps = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let id = stream.read_u32()?;
        let ip = stream.read_u32()?;
        let port = stream.read_u16()?;
        apps.push((id, (ip, port)));
    }
    Ok(apps)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A1: Addr = (0x7f00_0001, 4000);
    const A2: Addr = (0x7f00_0002, 4000);
    const A3: Addr = (0x7f00_0003, 4000);
    const A4: Addr = (0x7f00_0004, 4000);
    const L1: Addr = (0x7f00_0004, 2000);
    const L2: Addr = (0x7f00_0005, 2000);
    const B: Addr = (0x7f00_0006, 100);
    const C: Addr = (0x7f00_0007, 100);

    /// Three DBApps registered in a fixed order with the server already
    /// started, so later steps start from a settled topology.
    fn started_mgr() -> DbAppMgr {
        let mut mgr = DbAppMgr::new();
        // The first DBApp is told to reset the game server state, then
        // reports success.
        mgr.add_db_app(A1, Some(1), 0);
        assert_eq!(mgr.startup_state, StartupState::Indeterminate);
        mgr.server_has_started(true);
        assert_eq!(mgr.startup_state, StartupState::Started);
        // The next two are batched and flushed once the wait window passes.
        mgr.add_db_app(A2, Some(2), 10);
        mgr.add_db_app(A3, Some(3), 10);
        mgr.on_start_of_tick(10 + ADD_WAIT_TIME_MS);
        mgr.outbox.clear();
        mgr
    }

    // BW-05-05-01: a fixed join order picks the smallest ID as alpha.
    #[test]
    fn alpha_is_smallest_id_in_join_order() {
        let mut mgr = DbAppMgr::new();
        assert_eq!(mgr.alpha_id(), None);
        assert_eq!(mgr.alpha_addr(), None);

        mgr.add_db_app(A1, Some(1), 0);
        assert_eq!(mgr.alpha_id(), Some(1));
        assert_eq!(mgr.alpha_addr(), Some(A1));
        mgr.server_has_started(true);

        mgr.add_db_app(A2, Some(2), 10);
        mgr.add_db_app(A3, Some(3), 10);
        mgr.on_start_of_tick(10 + ADD_WAIT_TIME_MS);

        assert_eq!(mgr.alpha_id(), Some(1));
        assert_eq!(mgr.alpha_addr(), Some(A1));
        assert_eq!(mgr.last_db_app_id, 3);
        assert_eq!(mgr.find_app(A2).map(|app| app.id), Some(2));
        assert_eq!(mgr.find_app(A3).map(|app| app.id), Some(3));
        assert_eq!(mgr.find_app((0x7f00_0009, 9)), None);
    }

    // BW-05-05-02: the hash update lets every peer read the current set.
    #[test]
    fn hash_update_forwards_current_topology() {
        let mut mgr = DbAppMgr::new();
        mgr.add_db_app(A1, Some(7), 0);
        mgr.server_has_started(true);
        mgr.recover_login_app(L1, 1);
        mgr.recover_login_app(L2, 2);
        mgr.outbox.clear();

        mgr.add_db_app(A2, Some(8), 1000);
        // Batched: nothing goes out until the wait window passes.
        assert!(mgr.outbox.is_empty());
        mgr.on_start_of_tick(1000 + ADD_WAIT_TIME_MS);

        // The BaseAppMgr reads the whole set.
        let base_hash = match &mgr.outbox[0] {
            Outbound::UpdateDbAppHashToBaseAppMgr(hash) => decode_db_app_hash(hash).unwrap(),
            other => panic!("unexpected outbound: {other:?}"),
        };
        assert_eq!(base_hash, vec![(1, A1), (2, A2)]);

        // A settled DBApp gets a plain message; the fresh one its add reply.
        match &mgr.outbox[1] {
            Outbound::DbAppHashMessage(addr, reset, hash) => {
                assert_eq!(*addr, A1);
                assert!(!reset);
                assert_eq!(decode_db_app_hash(hash).unwrap(), vec![(1, A1), (2, A2)]);
            }
            other => panic!("unexpected outbound: {other:?}"),
        }
        match &mgr.outbox[2] {
            Outbound::DbAppHashAddReply(addr, reply, id, reset, hash) => {
                assert_eq!(*addr, A2);
                assert_eq!(*reply, 8);
                assert_eq!(*id, 2);
                assert!(!reset);
                assert_eq!(decode_db_app_hash(hash).unwrap(), vec![(1, A1), (2, A2)]);
            }
            other => panic!("unexpected outbound: {other:?}"),
        }

        // No alpha change: CellAppMgr and LoginApps stay quiet.
        assert_eq!(mgr.outbox.len(), 3);
    }

    // BW-05-05-02: the batch waits a full wait window before flushing.
    #[test]
    fn batch_not_flushed_before_wait_window() {
        let mut mgr = started_mgr();
        mgr.outbox.clear();

        mgr.add_db_app(A4, Some(9), 2000);
        mgr.on_start_of_tick(2000 + ADD_WAIT_TIME_MS - 1);
        assert!(mgr.outbox.is_empty());

        mgr.on_start_of_tick(2000 + ADD_WAIT_TIME_MS);
        assert_eq!(mgr.outbox.len(), 5); // base + 3 messages + 1 reply
        match &mgr.outbox[4] {
            Outbound::DbAppHashAddReply(addr, reply, id, _, hash) => {
                assert_eq!(*addr, A4);
                assert_eq!(*reply, 9);
                assert_eq!(*id, 4);
                assert_eq!(
                    decode_db_app_hash(hash).unwrap(),
                    vec![(1, A1), (2, A2), (3, A3), (4, A4)]
                );
            }
            other => panic!("unexpected outbound: {other:?}"),
        }
    }

    // BW-05-05-03: an alpha death promotes the next smallest ID and notifies
    // the CellAppMgr and every LoginApp.
    #[test]
    fn alpha_death_promotes_next_and_notifies() {
        let mut mgr = started_mgr();
        mgr.recover_login_app(L1, 1);
        mgr.recover_login_app(L2, 2);
        mgr.outbox.clear();

        mgr.handle_db_app_death(A1);

        assert_eq!(mgr.alpha_id(), Some(2));
        assert_eq!(mgr.alpha_addr(), Some(A2));
        assert_eq!(mgr.find_app(A1), None);

        match &mgr.outbox[0] {
            Outbound::UpdateDbAppHashToBaseAppMgr(hash) => {
                assert_eq!(decode_db_app_hash(hash).unwrap(), vec![(2, A2), (3, A3)]);
            }
            other => panic!("unexpected outbound: {other:?}"),
        }
        assert!(
            matches!(&mgr.outbox[1], Outbound::DbAppHashMessage(a, _, h) if *a == A2 && decode_db_app_hash(h).unwrap() == vec![(2, A2), (3, A3)])
        );
        assert!(matches!(&mgr.outbox[2], Outbound::DbAppHashMessage(a, _, _) if *a == A3));
        assert_eq!(mgr.outbox[3], Outbound::SetDbAppAlphaToCellAppMgr(A2));
        assert_eq!(mgr.outbox[4], Outbound::NotifyDbAppAlpha(L1, A2));
        assert_eq!(mgr.outbox[5], Outbound::NotifyDbAppAlpha(L2, A2));
        assert_eq!(mgr.outbox.len(), 6);
    }

    // BW-05-05-03: a non-alpha death keeps the alpha and stays quiet.
    #[test]
    fn non_alpha_death_no_alpha_notify() {
        let mut mgr = started_mgr();
        mgr.recover_login_app(L1, 1);
        mgr.outbox.clear();

        mgr.handle_db_app_death(A3);

        assert_eq!(mgr.alpha_id(), Some(1));
        assert_eq!(mgr.outbox.len(), 3); // base + 2 DBApps, no alpha notify
        assert!(matches!(
            &mgr.outbox[0],
            Outbound::UpdateDbAppHashToBaseAppMgr(_)
        ));
        assert!(matches!(&mgr.outbox[1], Outbound::DbAppHashMessage(a, _, _) if *a == A1));
        assert!(matches!(&mgr.outbox[2], Outbound::DbAppHashMessage(a, _, _) if *a == A2));
    }

    // BW-05-05-03: login-app death removes it from the notify set, and a
    // revived manager gets the current topology / alpha re-sent.
    #[test]
    fn login_app_death_and_manager_births() {
        let mut mgr = started_mgr();
        mgr.recover_login_app(L1, 1);
        mgr.recover_login_app(L2, 2);
        mgr.outbox.clear();

        mgr.handle_login_app_death(L1);
        mgr.handle_db_app_death(A1);
        assert!(mgr.outbox.contains(&Outbound::NotifyDbAppAlpha(L2, A2)));
        assert!(!mgr.outbox.contains(&Outbound::NotifyDbAppAlpha(L1, A2)));

        mgr.outbox.clear();
        mgr.handle_base_app_mgr_birth(B);
        assert!(
            matches!(&mgr.outbox[0], Outbound::UpdateDbAppHashToBaseAppMgr(h) if decode_db_app_hash(h).unwrap() == vec![(2, A2), (3, A3)])
        );

        mgr.outbox.clear();
        mgr.handle_cell_app_mgr_birth(C);
        assert_eq!(mgr.outbox, vec![Outbound::SetDbAppAlphaToCellAppMgr(A2)]);
    }

    // BW-05-05-04: the alpha carries the first-time-startup duty; additions
    // are refused while indeterminate and a dead alpha defers to the
    // BaseAppMgr.
    #[test]
    fn indeterminate_window_defers_to_baseappmgr() {
        let mut mgr = DbAppMgr::new();

        // The first DBApp is told to reset the game server state.
        mgr.add_db_app(A1, Some(9), 0);
        assert_eq!(mgr.startup_state, StartupState::Indeterminate);
        assert_eq!(
            mgr.outbox[1],
            Outbound::DbAppHashAddReply(A1, 9, 1, true, mgr.hash_bytes())
        );

        // A second add during the window is refused with an empty reply.
        mgr.outbox.clear();
        mgr.add_db_app(A2, Some(10), 1);
        assert_eq!(mgr.outbox, vec![Outbound::EmptyReply(A2, 10)]);
        assert_eq!(mgr.alpha_id(), Some(1));
        assert_eq!(mgr.find_app(A2), None);

        // The alpha dies before reporting: ask the BaseAppMgr instead.
        mgr.outbox.clear();
        mgr.handle_db_app_death(A1);
        assert_eq!(mgr.outbox, vec![Outbound::RequestHasStarted]);
        assert_eq!(mgr.alpha_id(), None);

        // The BaseAppMgr says it never started: back to NotStarted.
        mgr.server_has_started(false);
        assert_eq!(mgr.startup_state, StartupState::NotStarted);

        // A fresh DBApp becomes the new alpha and retries the reset.
        mgr.add_db_app(A2, Some(11), 2);
        assert_eq!(mgr.startup_state, StartupState::Indeterminate);
        assert_eq!(mgr.alpha_id(), Some(2));

        // It reports success: the server is up.
        mgr.server_has_started(true);
        assert_eq!(mgr.startup_state, StartupState::Started);
    }

    // BW-05-05-04: recovered DBApps keep their IDs, so the topology stays
    // comparable across a manager restart.
    #[test]
    fn recover_db_app_keeps_ids() {
        let mut mgr = DbAppMgr::new();
        mgr.recover_db_app(A1, 7);
        mgr.recover_db_app(A2, 3);

        assert_eq!(mgr.last_db_app_id, 7);
        assert_eq!(mgr.alpha_id(), Some(3));
        assert_eq!(mgr.alpha_addr(), Some(A2));
        assert_eq!(mgr.find_app(A1).map(|app| app.id), Some(7));
        assert_eq!(mgr.find_app(A2).map(|app| app.id), Some(3));
        assert!(mgr.outbox.is_empty()); // recovery sends nothing

        // The recovered set reads back out of a fresh hash, in ID order.
        assert_eq!(
            decode_db_app_hash(&mgr.hash_bytes()).unwrap(),
            vec![(3, A2), (7, A1)]
        );
    }

    // BW-05-05-04: the pending add reply is consumed by the first hash
    // update; later updates go out as plain messages.
    #[test]
    fn pending_reply_consumed_once() {
        let mut mgr = DbAppMgr::new();
        mgr.add_db_app(A1, Some(5), 0);
        assert!(matches!(&mgr.outbox[1], Outbound::DbAppHashAddReply(..)));

        mgr.server_has_started(true);
        // The second update goes out as a plain message, not a reply.
        assert!(matches!(
            &mgr.outbox[4],
            Outbound::DbAppHashMessage(a, _, _) if *a == A1
        ));
        assert_eq!(mgr.db_app_alpha().unwrap().pending_reply, None);
    }

    // BW-05-05-02: the login-app registration reply carries the ID and the
    // current alpha; it is refused while the manager gathers.
    #[test]
    fn login_app_registration_and_gather() {
        let mut mgr = DbAppMgr::new();
        // While the server has not started / no DBApps, the LoginApp retries.
        assert_eq!(mgr.add_login_app(L1, Some(1)), LoginAppAddOutcome::Retry);

        // A recovering manager that found the server started gathers first.
        mgr.startup(true, 0);
        assert_eq!(mgr.add_login_app(L1, Some(2)), LoginAppAddOutcome::Retry);
        assert!(mgr.gather_login_apps_pending);
        mgr.handle_gather_login_apps_timeout();
        assert!(!mgr.gather_login_apps_pending);
        assert!(mgr.should_accept_login_apps);

        // A DBApp alpha exists now, so the add succeeds with id and alpha.
        mgr.recover_db_app(A1, 3);
        mgr.outbox.clear();
        let outcome = mgr.add_login_app(L1, Some(4));
        assert_eq!(outcome, LoginAppAddOutcome::Accepted { id: 1, alpha: A1 });
        assert_eq!(mgr.outbox, vec![Outbound::LoginAppAddReply(L1, 4, 1, A1)]);
    }

    /// `SHUTDOWN_REQUEST` marks the manager as shutting down and forwards
    /// the request to the BaseAppMgr; with no BaseAppMgr address nothing goes
    /// out.
    #[test]
    fn request_marks_shutting_down_and_forwards() {
        let mut mgr = DbAppMgr::new();
        assert_eq!(
            mgr.controlled_shut_down(ShutDownStage::Request),
            DbAppMgrShutDownOutcome::ForwardRequestToBaseAppMgr(None)
        );
        assert!(mgr.is_shutting_down);
        assert!(mgr.outbox.is_empty());

        let mut mgr = DbAppMgr::new();
        mgr.base_app_mgr_addr = Some(B);
        mgr.outbox.clear();
        assert_eq!(
            mgr.controlled_shut_down(ShutDownStage::Request),
            DbAppMgrShutDownOutcome::ForwardRequestToBaseAppMgr(Some(B))
        );
        assert_eq!(mgr.outbox, vec![Outbound::ShutDownRequestToBaseAppMgr(B)]);
    }

    /// `SHUTDOWN_PERFORM` tells every registered DBApp to shut down; other
    /// stages are ignored.
    #[test]
    fn perform_tells_every_db_app() {
        let mut mgr = DbAppMgr::new();
        mgr.base_app_mgr_addr = Some(B);
        mgr.recover_db_app(A1, 3);
        mgr.recover_db_app(A2, 4);
        mgr.outbox.clear();

        assert_eq!(
            mgr.controlled_shut_down(ShutDownStage::Perform),
            DbAppMgrShutDownOutcome::Perform {
                db_apps: vec![A1, A2],
            }
        );
        assert_eq!(
            mgr.outbox,
            vec![Outbound::DbAppPerform(A1), Outbound::DbAppPerform(A2)]
        );
        assert!(!mgr.is_shutting_down);

        // A stage the manager does not handle is the reference's error arm.
        assert_eq!(
            mgr.controlled_shut_down(ShutDownStage::Inform),
            DbAppMgrShutDownOutcome::Unhandled(ShutDownStage::Inform)
        );
    }
}
