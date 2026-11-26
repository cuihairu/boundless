//! BW-05-03 the BaseAppMgr scheduling logic.
//!
//! The reference `server/baseappmgr/baseappmgr.cpp` registers and removes
//! BaseApps (`add`/`del`/`finishedInit`), picks the least-loaded BaseApp
//! for a login (`createEntity` via `findLeastLoadedApp`), gates logins when
//! every BaseApp is overloaded (`calculateOverloaded`), keeps the
//! authoritative copies of shared data and global bases and broadcasts them
//! to every BaseApp (`setSharedData`, `registerBaseGlobally`), and drives
//! the controlled shutdown stages (`controlledShutDown`).
//!
//! The Mercury traffic is not modelled here: the module records the state
//! transitions and returns the outbound messages the reference would send,
//! so the Mercury lane can drive the sockets when it lands. Addresses are
//! `(ip, port)` in the same network-order/host-order split the rest of the
//! workspace uses.

use std::collections::BTreeMap;

use crate::entity_creator::EntityMailBoxRef;
use crate::services_map::ServicesMap;
use crate::shared_data::{
    SharedDataMessage, SharedDataOutcome, SharedDataSync, read_shared_section,
};
use foundation::stream::{BinaryStream, StreamError};

/// `BaseAppMgrInterface::CreateEntityError::CREATE_ENTITY_ERROR_NO_BASEAPPS`.
pub const CREATE_ENTITY_ERROR_NO_BASEAPPS: u8 = 1;
/// `CREATE_ENTITY_ERROR_BASEAPPS_OVERLOADED`.
pub const CREATE_ENTITY_ERROR_BASEAPPS_OVERLOADED: u8 = 2;

/// `LoginConditionsConfig::minLoad`: the load above which a BaseApp counts
/// as overloaded.
pub const MIN_LOAD: f32 = 0.8;
/// `LoginConditionsConfig::minOverloadTolerancePeriod`, seconds.
pub const MIN_OVERLOAD_TOLERANCE_PERIOD_SECS: f32 = 5.0;
/// `LoginConditionsConfig::overloadLogins`.
pub const OVERLOAD_LOGINS: i32 = 10;
/// `BaseApp::addEntity`: the load estimate added for each new entity.
pub const BASE_APP_ENTITY_LOAD_STEP: f32 = 0.01;
/// `BaseAppMgrConfig::baseAppTimeout`, seconds, given to a BaseApp at
/// registration.
pub const BASE_APP_TIMEOUT_PERIOD: f32 = 5.0;
/// `ManagedAppSubSet::getNextAppID` masks the id into this many bits.
pub const BASE_APP_ID_MASK: u32 = 0x0FFF_FFFF;

/// `SHARED_DATA_TYPE_CELL_APP` (`lib/server/shared_data_type.hpp`).
pub const SHARED_DATA_TYPE_CELL_APP: u8 = 1;
/// `SHARED_DATA_TYPE_BASE_APP`.
pub const SHARED_DATA_TYPE_BASE_APP: u8 = 2;
/// `SHARED_DATA_TYPE_GLOBAL`.
pub const SHARED_DATA_TYPE_GLOBAL: u8 = 3;
/// `SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP`.
pub const SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP: u8 = 4;

/// `ShutDownStage` (`lib/server/common.hpp`): the one shared definition.
pub use crate::shutdown::ShutDownStage;

/// A host-order port and network-order ip pair.
pub type Addr = (u32, u16);

/// One managed BaseApp or ServiceApp (`server/baseappmgr/baseapp.hpp`).
#[derive(Debug, Clone, PartialEq)]
pub struct ManagedBaseApp {
    /// The address the CellAppMgr and other components talk to.
    addr_for_cells: Addr,
    /// The address clients connect to.
    addr_for_clients: Addr,
    id: u32,
    is_service_app: bool,
    is_retiring: bool,
    load: f32,
    num_bases: i32,
    num_proxies: i32,
}

impl ManagedBaseApp {
    pub fn new(
        addr_for_cells: Addr,
        addr_for_clients: Addr,
        id: u32,
        is_service_app: bool,
    ) -> Self {
        Self {
            addr_for_cells,
            addr_for_clients,
            id,
            is_service_app,
            is_retiring: false,
            load: 0.0,
            num_bases: 0,
            num_proxies: 0,
        }
    }

    pub fn addr_for_cells(&self) -> Addr {
        self.addr_for_cells
    }

    pub fn addr_for_clients(&self) -> Addr {
        self.addr_for_clients
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn is_service_app(&self) -> bool {
        self.is_service_app
    }

    pub fn is_retiring(&self) -> bool {
        self.is_retiring
    }

    pub fn mark_retiring(&mut self) {
        self.is_retiring = true;
    }

    pub fn load(&self) -> f32 {
        self.load
    }

    /// `BaseApp::updateLoad`.
    pub fn update_load(&mut self, load: f32, num_bases: i32, num_proxies: i32) {
        self.load = load;
        self.num_bases = num_bases;
        self.num_proxies = num_proxies;
    }

    /// `BaseApp::addEntity`: a rough load estimate bump for the new entity.
    pub fn add_entity(&mut self) {
        self.load += BASE_APP_ENTITY_LOAD_STEP;
    }
}

/// An `EntityMailBoxRef`-like global base entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalBaseMailbox {
    pub entity: u32,
    pub addr: Addr,
}

/// A message the reference sends to one BaseApp's internal interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToBaseApp {
    /// `BaseAppIntInterface::setSharedData`.
    SetSharedData {
        data_type: u8,
        key: String,
        value: String,
        original_src: Addr,
    },
    /// `BaseAppIntInterface::delSharedData`.
    DelSharedData {
        data_type: u8,
        key: String,
        original_src: Addr,
    },
    /// `BaseAppIntInterface::addGlobalBase`.
    AddGlobalBase {
        name: String,
        mailbox: GlobalBaseMailbox,
    },
    /// `BaseAppIntInterface::delGlobalBase`.
    DelGlobalBase { name: String },
    /// `BaseAppIntInterface::addServiceFragment`.
    AddServiceFragment {
        name: String,
        mailbox: EntityMailBoxRef,
    },
    /// `BaseAppIntInterface::delServiceFragment`.
    DelServiceFragment { name: String, addr: Addr },
    /// `BaseAppIntInterface::controlledShutDown`.
    ControlledShutDown {
        stage: ShutDownStage,
        shut_down_time: u32,
    },
}

/// A service-fragment update the BaseAppMgr forwards to the CellAppMgr
/// (`CellAppMgrInterface::addServiceFragment` / `delServiceFragment`,
/// `baseappmgr.cpp:2801` / `2841`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellAppMgrServiceMessage {
    /// `CellAppMgrInterface::addServiceFragment`.
    AddServiceFragment {
        name: String,
        mailbox: EntityMailBoxRef,
    },
    /// `CellAppMgrInterface::delServiceFragment`.
    DelServiceFragment { name: String, addr: Addr },
}

/// The outcome of a `registerServiceFragment` / `deregisterServiceFragment`
/// request: the manager's authoritative map change, the broadcasts to the
/// BaseApps, the forward to the CellAppMgr, and whether the server should
/// shut down because a service lost its last fragment.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceFragmentOutcome {
    /// The number of fragments the map changed by (1 on a register, the
    /// removed count on a deregister).
    pub removed: usize,
    /// Whether the service no longer has any fragments anywhere
    /// (`*pDidLoseService`).
    pub did_lose_service: bool,
    /// `BaseAppIntInterface::addServiceFragment` / `delServiceFragment` to
    /// every managed BaseApp and ServiceApp, including the sender.
    pub broadcasts: Vec<(Addr, ToBaseApp)>,
    /// The `CellAppMgrInterface` forward, if the app was registered.
    pub cell_app_mgr: Option<CellAppMgrServiceMessage>,
    /// Whether `Config::shutDownServerOnServiceDeath()` asks for a shutdown
    /// because a service lost its last fragment outside a controlled
    /// shutdown.
    pub shut_down_server: bool,
}

/// Why `BaseAppMgr::add` refused to register a BaseApp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferredReason {
    /// The CellAppMgr has not started yet.
    CellAppMgrNotEstablished,
    /// No init data has arrived from the DBApp Alpha.
    NoInitData,
    /// The DBApp hash is empty.
    DbAppsEmpty,
    /// The manager is shutting down; the reference lets the request time out.
    ShuttingDown,
}

/// `BaseAppMgr::initData` outcome: the initial game time from the DBApp
/// Alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitDataOutcome {
    /// Ignored: initialisation data was already received.
    Ignored,
    /// Accepted.
    Accepted {
        /// Whether the caller should `set_start_time(game_time)`: the
        /// manager's game time was still zero (`BaseAppMgr::initData` only
        /// adopts the DBApp time on the first start, not on recovery).
        should_set_start_time: bool,
    },
}

/// `BaseAppIntInterface::startupArgs` for one BaseApp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseAppStartup {
    /// The app's address for cells.
    pub addr: Addr,
    /// `args.bootstrap`: set on exactly one non-service BaseApp.
    pub bootstrap: bool,
    /// `args.didAutoLoadEntitiesFromDB`.
    pub did_auto_load_entities_from_db: bool,
}

/// `BaseAppMgr::startup` outcome: the DBApp Alpha told the manager to start
/// the rest of the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupOutcome {
    /// Already started: the request is ignored.
    AlreadyStarted,
    /// No BaseApp (ready or pending) to start: the reference CRITICALs.
    NoBaseApps,
    /// Started: every BaseApp was told to start, with the bootstrap flag on
    /// exactly one non-service BaseApp, and the CellAppMgr was told to
    /// start.
    Started {
        /// `BaseAppIntInterface::startup` per app.
        apps: Vec<BaseAppStartup>,
        /// Whether the `CellAppMgrInterface::startup` was forwarded.
        cell_app_mgr: bool,
    },
}

/// The reply `BaseAppMgr::add` streams back on success.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BaseAppInitData {
    pub id: u32,
    pub time: u32,
    pub is_ready: bool,
    /// `Config::baseAppTimeout`.
    pub timeout_period: f32,
}

/// The outcome of a `BaseAppMgr::add`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RegisterOutcome {
    /// The BaseApp is deferred: the reply is empty and the app waits.
    Deferred(DeferredReason),
    /// The BaseApp is registered with this init data.
    Registered(BaseAppInitData),
}

/// Why `createEntity` rejected a login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateEntityError {
    NoBaseApps,
    BaseAppsOverloaded,
}

/// The result of `createEntity`: the chosen BaseApp's client address, or a
/// rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateEntityOutcome {
    /// Forward the entity to the BaseApp at this client address.
    CreateAt(Addr),
    Rejected(CreateEntityError),
}

/// What a `controlledShutDown` step asked the manager to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// Nothing to do for this stage.
    None,
    /// Forward the `SHUTDOWN_REQUEST` to the CellAppMgr.
    ForwardRequestToCellAppMgr,
    /// No BaseApps are left: acknowledge the stage to the CellAppMgr.
    AckToCellAppMgr(ShutDownStage),
    /// Inform every BaseApp; they acknowledge when done.
    InformApps(Vec<(Addr, ToBaseApp)>),
    /// Begin the asynchronous `SHUTDOWN_DISCONNECT_PROXIES` stage.
    StartDisconnectProxies(Vec<Addr>),
    /// `SHUTDOWN_TRIGGER`: begin a controlled shutdown from the most senior
    /// process.
    TriggerServerShutdown,
}

/// The BaseAppMgr scheduling state. Mirrors `BaseAppMgr`.
#[derive(Debug, Clone)]
pub struct BaseAppScheduler {
    base_apps: Vec<ManagedBaseApp>,
    service_apps: Vec<ManagedBaseApp>,
    pending: Vec<ManagedBaseApp>,
    next_base_id: u32,
    next_service_id: u32,
    shared_base_app_data: BTreeMap<String, String>,
    shared_global_data: BTreeMap<String, String>,
    global_bases: BTreeMap<String, GlobalBaseMailbox>,
    /// `ServiceAppSubSet::services_`: the service-fragment map.
    services: ServicesMap,
    /// `Config::shutDownServerOnServiceDeath()`.
    shut_down_on_service_death: bool,
    best_base_app: Option<Addr>,
    has_init_data: bool,
    has_started: bool,
    cell_app_mgr_established: bool,
    db_apps_empty: bool,
    overload_start_ms: Option<u64>,
    logins_since_overload: i32,
    shut_down_stage: ShutDownStage,
}

impl Default for BaseAppScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl BaseAppScheduler {
    pub fn new() -> Self {
        Self {
            base_apps: Vec::new(),
            service_apps: Vec::new(),
            pending: Vec::new(),
            next_base_id: 0,
            next_service_id: 0,
            shared_base_app_data: BTreeMap::new(),
            shared_global_data: BTreeMap::new(),
            global_bases: BTreeMap::new(),
            services: ServicesMap::new(),
            shut_down_on_service_death: false,
            best_base_app: None,
            has_init_data: false,
            has_started: false,
            cell_app_mgr_established: false,
            db_apps_empty: true,
            overload_start_ms: None,
            logins_since_overload: 0,
            shut_down_stage: ShutDownStage::None,
        }
    }

    pub fn num_base_apps(&self) -> usize {
        self.base_apps.len()
    }

    pub fn num_service_apps(&self) -> usize {
        self.service_apps.len()
    }

    pub fn num_pending(&self) -> usize {
        self.pending.len()
    }

    pub fn shut_down_stage(&self) -> ShutDownStage {
        self.shut_down_stage
    }

    pub fn best_base_app(&self) -> Option<Addr> {
        self.best_base_app
    }

    pub fn set_cell_app_mgr_established(&mut self, established: bool) {
        self.cell_app_mgr_established = established;
    }

    pub fn set_has_init_data(&mut self, has: bool) {
        self.has_init_data = has;
    }

    pub fn set_db_apps_empty(&mut self, empty: bool) {
        self.db_apps_empty = empty;
    }

    pub fn set_has_started(&mut self, started: bool) {
        self.has_started = started;
    }

    /// `BaseAppMgr::initData`: the initial game time from the DBApp Alpha.
    /// Subsequent messages are ignored; the DBApp time is only adopted on
    /// the first start, not on recovery.
    pub fn init_data(&mut self, _game_time: u32, current_time: u32) -> InitDataOutcome {
        if self.has_init_data {
            return InitDataOutcome::Ignored;
        }
        self.has_init_data = true;
        InitDataOutcome::Accepted {
            should_set_start_time: current_time == 0,
        }
    }

    /// `BaseAppMgr::requestHasStarted`: the reply to the DBAppMgr's query.
    pub fn request_has_started(&self) -> bool {
        self.has_started
    }

    /// `BaseAppMgr::startup`: the DBApp Alpha tells the manager to start the
    /// rest of the server. Once the timer starts, `hasStarted_` is set, the
    /// CellAppMgr is told to start, and every BaseApp (ready then pending)
    /// is sent `BaseAppIntInterface::startup` with the bootstrap flag on
    /// exactly one non-service BaseApp.
    pub fn startup(&mut self, did_auto_load_entities_from_db: bool) -> StartupOutcome {
        if self.has_started {
            return StartupOutcome::AlreadyStarted;
        }

        if self.base_apps.is_empty() && self.service_apps.is_empty() && self.pending.is_empty() {
            return StartupOutcome::NoBaseApps;
        }

        // startTimer(): the game timer and the slave TimeKeeper come up.
        self.has_started = true;

        let mut apps = Vec::new();
        let mut bootstrap_used = false;
        for app in self
            .base_apps
            .iter()
            .chain(self.service_apps.iter())
            .chain(self.pending.iter())
        {
            let bootstrap = !bootstrap_used && !app.is_service_app;
            bootstrap_used |= bootstrap;
            apps.push(BaseAppStartup {
                addr: app.addr_for_cells,
                bootstrap,
                did_auto_load_entities_from_db,
            });
        }

        StartupOutcome::Started {
            apps,
            cell_app_mgr: self.cell_app_mgr_established,
        }
    }

    /// Find a ready app by its cells address.
    fn find_ready(&self, addr: Addr) -> Option<&ManagedBaseApp> {
        self.base_apps
            .iter()
            .chain(self.service_apps.iter())
            .find(|app| app.addr_for_cells == addr)
    }

    fn find_ready_mut(&mut self, addr: Addr) -> Option<&mut ManagedBaseApp> {
        self.base_apps
            .iter_mut()
            .chain(self.service_apps.iter_mut())
            .find(|app| app.addr_for_cells == addr)
    }

    fn subset_mut(&mut self, is_service_app: bool) -> &mut Vec<ManagedBaseApp> {
        if is_service_app {
            &mut self.service_apps
        } else {
            &mut self.base_apps
        }
    }

    fn subset(&self, is_service_app: bool) -> &[ManagedBaseApp] {
        if is_service_app {
            &self.service_apps
        } else {
            &self.base_apps
        }
    }

    /// `BaseAppMgr::add`: register a BaseApp, deferring until the CellAppMgr
    /// is up, the init data has arrived, and the DBApp hash is non-empty.
    /// While shutting down the reference lets the request time out.
    pub fn register(
        &mut self,
        addr_for_cells: Addr,
        addr_for_clients: Addr,
        is_service_app: bool,
        game_time: u32,
    ) -> RegisterOutcome {
        if !self.cell_app_mgr_established {
            return RegisterOutcome::Deferred(DeferredReason::CellAppMgrNotEstablished);
        }
        if !self.has_init_data {
            return RegisterOutcome::Deferred(DeferredReason::NoInitData);
        }
        if self.db_apps_empty {
            return RegisterOutcome::Deferred(DeferredReason::DbAppsEmpty);
        }
        if self.shut_down_stage != ShutDownStage::None {
            return RegisterOutcome::Deferred(DeferredReason::ShuttingDown);
        }

        let id = if is_service_app {
            self.next_service_id = (self.next_service_id + 1) & BASE_APP_ID_MASK;
            self.next_service_id
        } else {
            self.next_base_id = (self.next_base_id + 1) & BASE_APP_ID_MASK;
            self.next_base_id
        };

        let app = ManagedBaseApp::new(addr_for_cells, addr_for_clients, id, is_service_app);
        self.pending.push(app);

        RegisterOutcome::Registered(BaseAppInitData {
            id,
            time: game_time,
            is_ready: self.has_started,
            timeout_period: BASE_APP_TIMEOUT_PERIOD,
        })
    }

    /// `BaseAppMgr::finishedInit`: move the app from pending to ready and
    /// synchronise it with the current globals. Returns the messages the
    /// new app is sent.
    pub fn finished_init(&mut self, addr_for_cells: Addr) -> Vec<(Addr, ToBaseApp)> {
        let Some(index) = self
            .pending
            .iter()
            .position(|app| app.addr_for_cells == addr_for_cells)
        else {
            return Vec::new();
        };

        let app = self.pending.remove(index);
        let is_service_app = app.is_service_app;
        self.subset_mut(is_service_app).push(app);

        self.synchronize(addr_for_cells)
    }

    /// `BaseAppMgr::synchronize`: stream the global bases and shared data to
    /// the newly-ready app.
    fn synchronize(&self, addr_for_cells: Addr) -> Vec<(Addr, ToBaseApp)> {
        let mut out = Vec::new();
        for (name, mailbox) in &self.global_bases {
            out.push((
                addr_for_cells,
                ToBaseApp::AddGlobalBase {
                    name: name.clone(),
                    mailbox: mailbox.clone(),
                },
            ));
        }
        for (key, value) in &self.shared_base_app_data {
            out.push((
                addr_for_cells,
                ToBaseApp::SetSharedData {
                    data_type: SHARED_DATA_TYPE_BASE_APP,
                    key: key.clone(),
                    value: value.clone(),
                    original_src: (0, 0),
                },
            ));
        }
        for (key, value) in &self.shared_global_data {
            out.push((
                addr_for_cells,
                ToBaseApp::SetSharedData {
                    data_type: SHARED_DATA_TYPE_GLOBAL,
                    key: key.clone(),
                    value: value.clone(),
                    original_src: (0, 0),
                },
            ));
        }
        out
    }

    /// The batched join-sync of the authoritative maps (base-app data
    /// first, then global data): the optimization form of the per-entry
    /// fill, one message instead of one per entry.
    pub fn join_sync(&self) -> SharedDataSync {
        SharedDataSync::from_maps(
            SHARED_DATA_TYPE_BASE_APP,
            &self.shared_base_app_data,
            SHARED_DATA_TYPE_GLOBAL,
            &self.shared_global_data,
        )
    }

    /// `BaseAppMgr::del` / `onBaseAppDeath`: remove a ready app.
    pub fn remove_app(&mut self, addr_for_cells: Addr) -> bool {
        let before = self.base_apps.len() + self.service_apps.len();
        self.base_apps
            .retain(|app| app.addr_for_cells != addr_for_cells);
        self.service_apps
            .retain(|app| app.addr_for_cells != addr_for_cells);
        let after = self.base_apps.len() + self.service_apps.len();
        if after != before {
            if self.best_base_app == Some(addr_for_cells) {
                self.best_base_app = None;
            }
            true
        } else {
            false
        }
    }

    /// `BaseAppMgr::informOfLoad`.
    pub fn inform_of_load(
        &mut self,
        addr_for_cells: Addr,
        load: f32,
        num_bases: i32,
        num_proxies: i32,
    ) -> bool {
        let Some(app) = self.find_ready_mut(addr_for_cells) else {
            return false;
        };
        app.update_load(load, num_bases, num_proxies);
        true
    }

    /// `ManagedAppSubSet::findLeastLoadedApp`: the least-loaded ready app
    /// that is not retiring. Ties keep the earlier app, as the reference's
    /// strict comparison does.
    pub fn least_loaded(&self, is_service_app: bool) -> Option<Addr> {
        let mut best_load = None;
        let mut best_addr = None;
        for app in self.subset(is_service_app) {
            if app.is_retiring {
                continue;
            }
            if best_load.is_none_or(|load| app.load < load) {
                best_load = Some(app.load);
                best_addr = Some(app.addr_for_cells);
            }
        }
        best_addr
    }

    pub fn min_load(&self, is_service_app: bool) -> f32 {
        self.subset(is_service_app)
            .iter()
            .map(|app| app.load)
            .fold(2.0, f32::min)
    }

    pub fn avg_load(&self, is_service_app: bool) -> f32 {
        let apps = self.subset(is_service_app);
        if apps.is_empty() {
            return 0.0;
        }
        apps.iter().map(|app| app.load).sum::<f32>() / apps.len() as f32
    }

    pub fn max_load(&self, is_service_app: bool) -> f32 {
        self.subset(is_service_app)
            .iter()
            .map(|app| app.load)
            .fold(0.0, f32::max)
    }

    /// `BaseAppMgr::calculateOverloaded`: rate-limit logins while every
    /// BaseApp is over `minLoad`; the overload sticks after the tolerance
    /// period or after `overloadLogins` logins have slipped through.
    fn calculate_overloaded(&mut self, are_overloaded: bool, now_ms: u64) -> bool {
        if are_overloaded {
            let tolerance_ms = (MIN_OVERLOAD_TOLERANCE_PERIOD_SECS * 1000.0) as u64;
            let start = *self.overload_start_ms.get_or_insert(now_ms);
            if now_ms - start > tolerance_ms || self.logins_since_overload >= OVERLOAD_LOGINS {
                return true;
            }
            self.logins_since_overload += 1;
            false
        } else {
            self.overload_start_ms = None;
            self.logins_since_overload = 0;
            false
        }
    }

    /// `BaseAppMgr::createEntity`: pick the least-loaded BaseApp for a
    /// login, gating on the overload state. On success the chosen app's
    /// load estimate is bumped (`addEntity`).
    pub fn create_entity(&mut self, now_ms: u64) -> CreateEntityOutcome {
        let Some(best_addr) = self.least_loaded(false) else {
            return CreateEntityOutcome::Rejected(CreateEntityError::NoBaseApps);
        };

        let best_load = self
            .find_ready(best_addr)
            .map(|app| app.load)
            .unwrap_or(0.0);

        if self.calculate_overloaded(best_load > MIN_LOAD, now_ms) {
            return CreateEntityOutcome::Rejected(CreateEntityError::BaseAppsOverloaded);
        }

        // The chosen app's client address is what the reply carries.
        let client_addr = self
            .find_ready(best_addr)
            .map(ManagedBaseApp::addr_for_clients)
            .unwrap_or(best_addr);

        if let Some(app) = self.find_ready_mut(best_addr) {
            app.add_entity();
        }

        CreateEntityOutcome::CreateAt(client_addr)
    }

    /// `BaseAppMgr::setSharedData` (`baseappmgr.cpp:2112`): store the
    /// authoritative copy of BASE_APP or GLOBAL data and broadcast to every
    /// BaseApp; a global request from a BaseApp (`GLOBAL_FROM_BASE_APP`) or a
    /// cell-data change (`CELL_APP`) is forwarded to the CellAppMgr instead,
    /// so the BaseApps are told only after the CellAppMgr relays it back.
    pub fn set_shared_data(
        &mut self,
        data_type: u8,
        key: &str,
        value: &str,
        original_src: Addr,
    ) -> SharedDataOutcome {
        match data_type {
            SHARED_DATA_TYPE_BASE_APP => {
                self.shared_base_app_data
                    .insert(key.to_string(), value.to_string());
                SharedDataOutcome {
                    base_apps: self.broadcast_set(data_type, key, value, original_src),
                    cell_app_mgr: None,
                }
            }
            SHARED_DATA_TYPE_GLOBAL => {
                // The CellAppMgr relay (or a direct global set): the manager
                // mirrors and tells the BaseApps.
                self.shared_global_data
                    .insert(key.to_string(), value.to_string());
                SharedDataOutcome {
                    base_apps: self.broadcast_set(data_type, key, value, original_src),
                    cell_app_mgr: None,
                }
            }
            SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP => SharedDataOutcome {
                // Forwarded to the CellAppMgr as global data; the BaseApps
                // wait for the relay.
                base_apps: Vec::new(),
                cell_app_mgr: Some(SharedDataMessage::Set {
                    data_type: SHARED_DATA_TYPE_GLOBAL,
                    key: key.to_string(),
                    value: value.to_string(),
                    original_src,
                }),
            },
            SHARED_DATA_TYPE_CELL_APP => SharedDataOutcome {
                base_apps: Vec::new(),
                cell_app_mgr: Some(SharedDataMessage::Set {
                    data_type,
                    key: key.to_string(),
                    value: value.to_string(),
                    original_src,
                }),
            },
            _ => SharedDataOutcome::default(),
        }
    }

    /// `BaseAppMgr::delSharedData` (`baseappmgr.cpp:2183`): erase the
    /// authoritative copy of BASE_APP or GLOBAL data and broadcast the delete
    /// to every BaseApp; a global delete from a BaseApp or a cell-data delete
    /// is forwarded to the CellAppMgr instead.
    pub fn del_shared_data(
        &mut self,
        data_type: u8,
        key: &str,
        original_src: Addr,
    ) -> SharedDataOutcome {
        match data_type {
            SHARED_DATA_TYPE_BASE_APP => {
                self.shared_base_app_data.remove(key);
                SharedDataOutcome {
                    base_apps: self.broadcast_del(data_type, key, original_src),
                    cell_app_mgr: None,
                }
            }
            SHARED_DATA_TYPE_GLOBAL => {
                self.shared_global_data.remove(key);
                SharedDataOutcome {
                    base_apps: self.broadcast_del(data_type, key, original_src),
                    cell_app_mgr: None,
                }
            }
            SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP => SharedDataOutcome {
                base_apps: Vec::new(),
                cell_app_mgr: Some(SharedDataMessage::Del {
                    data_type: SHARED_DATA_TYPE_GLOBAL,
                    key: key.to_string(),
                    original_src,
                }),
            },
            SHARED_DATA_TYPE_CELL_APP => SharedDataOutcome {
                base_apps: Vec::new(),
                cell_app_mgr: Some(SharedDataMessage::Del {
                    data_type,
                    key: key.to_string(),
                    original_src,
                }),
            },
            _ => SharedDataOutcome::default(),
        }
    }

    /// `BaseAppMgr::recoverSharedDataFromStream` (`baseappmgr.cpp:1249`):
    /// restore the authoritative BASE_APP and GLOBAL maps from the recovery
    /// stream (each a count then count key/value pairs).
    pub fn recover_shared_data_from_stream(
        &mut self,
        stream: &mut BinaryStream,
    ) -> Result<(), StreamError> {
        self.shared_base_app_data = read_shared_section(stream)?;
        self.shared_global_data = read_shared_section(stream)?;
        Ok(())
    }

    fn broadcast_set(
        &self,
        data_type: u8,
        key: &str,
        value: &str,
        original_src: Addr,
    ) -> Vec<(Addr, ToBaseApp)> {
        self.all_ready_addrs()
            .into_iter()
            .map(|addr| {
                (
                    addr,
                    ToBaseApp::SetSharedData {
                        data_type,
                        key: key.to_string(),
                        value: value.to_string(),
                        original_src,
                    },
                )
            })
            .collect()
    }

    fn broadcast_del(
        &self,
        data_type: u8,
        key: &str,
        original_src: Addr,
    ) -> Vec<(Addr, ToBaseApp)> {
        self.all_ready_addrs()
            .into_iter()
            .map(|addr| {
                (
                    addr,
                    ToBaseApp::DelSharedData {
                        data_type,
                        key: key.to_string(),
                        original_src,
                    },
                )
            })
            .collect()
    }

    /// `BaseAppMgr::registerBaseGlobally`: insert the global base and
    /// broadcast to every BaseApp except the sender. Returns the success
    /// code (1 = registered, 0 = duplicate) and the messages.
    pub fn register_base_globally(
        &mut self,
        sender: Addr,
        name: &str,
        mailbox: GlobalBaseMailbox,
    ) -> (u8, Vec<(Addr, ToBaseApp)>) {
        if self
            .global_bases
            .insert(name.to_string(), mailbox.clone())
            .is_some()
        {
            return (0, Vec::new());
        }
        let out = self
            .all_ready_addrs()
            .into_iter()
            .filter(|addr| *addr != sender)
            .map(|addr| {
                (
                    addr,
                    ToBaseApp::AddGlobalBase {
                        name: name.to_string(),
                        mailbox: mailbox.clone(),
                    },
                )
            })
            .collect();
        (1, out)
    }

    /// `BaseAppMgr::deregisterBaseGlobally`.
    pub fn deregister_base_globally(&mut self, sender: Addr, name: &str) -> Vec<(Addr, ToBaseApp)> {
        if self.global_bases.remove(name).is_none() {
            return Vec::new();
        }
        self.all_ready_addrs()
            .into_iter()
            .filter(|addr| *addr != sender)
            .map(|addr| {
                (
                    addr,
                    ToBaseApp::DelGlobalBase {
                        name: name.to_string(),
                    },
                )
            })
            .collect()
    }

    /// `Config::shutDownServerOnServiceDeath`.
    pub fn set_shut_down_on_service_death(&mut self, enabled: bool) {
        self.shut_down_on_service_death = enabled;
    }

    /// The registered service names (`ServicesMap::getServiceNames`).
    pub fn service_names(&self) -> Vec<String> {
        self.services.service_names()
    }

    /// The authoritative BASE_APP shared data (`sharedBaseAppData_`).
    pub fn shared_base_app_data(&self) -> &BTreeMap<String, String> {
        &self.shared_base_app_data
    }

    /// The mirrored GLOBAL shared data (`sharedGlobalData_`).
    pub fn shared_global_data(&self) -> &BTreeMap<String, String> {
        &self.shared_global_data
    }

    /// The fragments of one service (`ServicesMap::getFragmentsForService`).
    pub fn fragments_for_service(&self, name: &str) -> Vec<EntityMailBoxRef> {
        self.services.fragments_for_service(name)
    }

    /// Choose a fragment of one service by `rand() % count`
    /// (`ServicesMap::chooseFragment`).
    pub fn choose_service_fragment(&self, name: &str, draw: usize) -> Option<EntityMailBoxRef> {
        self.services.choose_fragment(name, draw)
    }

    /// `BaseAppMgr::registerServiceFragment`
    /// (`ServiceAppSubSet::addServiceFragmentFromStream`, `baseappmgr.cpp:2755`):
    /// a ServiceApp registers one of its service entities as a fragment of a
    /// service. The sender must be a registered app; the fragment joins the
    /// authoritative map, every managed BaseApp and ServiceApp is told to add
    /// it, and the CellAppMgr is forwarded the update.
    pub fn register_service_fragment(
        &mut self,
        sender: Addr,
        name: &str,
        mailbox: EntityMailBoxRef,
    ) -> ServiceFragmentOutcome {
        if self.find_ready(sender).is_none() {
            // An unregistered app's message is dropped (ERROR_MSG and return).
            return ServiceFragmentOutcome {
                removed: 0,
                did_lose_service: false,
                broadcasts: Vec::new(),
                cell_app_mgr: None,
                shut_down_server: false,
            };
        }

        self.services.add_fragment(name, mailbox);
        let broadcasts = self
            .all_ready_addrs()
            .into_iter()
            .map(|addr| {
                (
                    addr,
                    ToBaseApp::AddServiceFragment {
                        name: name.to_string(),
                        mailbox,
                    },
                )
            })
            .collect();

        ServiceFragmentOutcome {
            removed: 1,
            did_lose_service: false,
            broadcasts,
            cell_app_mgr: Some(CellAppMgrServiceMessage::AddServiceFragment {
                name: name.to_string(),
                mailbox,
            }),
            shut_down_server: false,
        }
    }

    /// `BaseAppMgr::deregisterServiceFragment`
    /// (`ServiceAppSubSet::deregisterServiceFragment`, `baseappmgr.cpp:2812`):
    /// a ServiceApp deregisters one of its fragments. The fragment for the
    /// sender's address leaves the map, every managed app is told to delete
    /// it, and the CellAppMgr is forwarded the update. When the service loses
    /// its last fragment and `shutDownServerOnServiceDeath` is set (and the
    /// manager is not already shutting down), the server is to be shut down.
    pub fn deregister_service_fragment(
        &mut self,
        sender: Addr,
        name: &str,
    ) -> ServiceFragmentOutcome {
        let (removed, did_lose_service) = self.services.remove_fragment(name, sender);

        let broadcasts = self
            .all_ready_addrs()
            .into_iter()
            .map(|addr| {
                (
                    addr,
                    ToBaseApp::DelServiceFragment {
                        name: name.to_string(),
                        addr: sender,
                    },
                )
            })
            .collect();

        let shut_down_server = did_lose_service
            && self.shut_down_stage == ShutDownStage::None
            && self.shut_down_on_service_death;

        ServiceFragmentOutcome {
            removed,
            did_lose_service,
            broadcasts,
            cell_app_mgr: Some(CellAppMgrServiceMessage::DelServiceFragment {
                name: name.to_string(),
                addr: sender,
            }),
            shut_down_server,
        }
    }

    fn all_ready_addrs(&self) -> Vec<Addr> {
        self.base_apps
            .iter()
            .chain(self.service_apps.iter())
            .map(|app| app.addr_for_cells)
            .collect()
    }

    /// `BaseAppMgr::controlledShutDown`: advance the shutdown sequence.
    pub fn controlled_shut_down(
        &mut self,
        stage: ShutDownStage,
        shut_down_time: u32,
    ) -> ShutdownOutcome {
        match stage {
            ShutDownStage::Request => {
                if self.cell_app_mgr_established {
                    ShutdownOutcome::ForwardRequestToCellAppMgr
                } else {
                    ShutdownOutcome::None
                }
            }
            ShutDownStage::Inform => {
                self.shut_down_stage = ShutDownStage::Inform;
                if self.all_ready_addrs().is_empty() {
                    ShutdownOutcome::AckToCellAppMgr(ShutDownStage::Inform)
                } else {
                    let apps = self
                        .all_ready_addrs()
                        .into_iter()
                        .map(|addr| {
                            (
                                addr,
                                ToBaseApp::ControlledShutDown {
                                    stage: ShutDownStage::Inform,
                                    shut_down_time,
                                },
                            )
                        })
                        .collect();
                    ShutdownOutcome::InformApps(apps)
                }
            }
            ShutDownStage::Perform => {
                ShutdownOutcome::StartDisconnectProxies(self.all_ready_addrs())
            }
            ShutDownStage::Trigger => ShutdownOutcome::TriggerServerShutdown,
            ShutDownStage::None | ShutDownStage::DisconnectProxies => ShutdownOutcome::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell_addr(i: u16) -> Addr {
        (1, 1000 + i)
    }

    fn client_addr(i: u16) -> Addr {
        (1, 2000 + i)
    }

    /// A scheduler ready to accept registrations.
    fn ready_scheduler() -> BaseAppScheduler {
        let mut scheduler = BaseAppScheduler::new();
        scheduler.set_cell_app_mgr_established(true);
        scheduler.set_has_init_data(true);
        scheduler.set_db_apps_empty(false);
        scheduler
    }

    /// Register and finish the init of `count` BaseApps, returning their
    /// cells addresses.
    fn add_base_apps(scheduler: &mut BaseAppScheduler, count: u16) -> Vec<Addr> {
        let mut addrs = Vec::new();
        for i in 0..count {
            let addr = cell_addr(i);
            assert!(matches!(
                scheduler.register(addr, client_addr(i), false, 10),
                RegisterOutcome::Registered(_)
            ));
            scheduler.finished_init(addr);
            addrs.push(addr);
        }
        addrs
    }

    // ------------------------------------------------------------------
    // BW-05-03-01 登记与移除 BaseApp
    // ------------------------------------------------------------------

    /// The synchronize fill streams base-app data ahead of global data
    /// (`baseappmgr.cpp:1414-1430`), one `setSharedData` message per
    /// entry, exactly the batched join-sync's expansion.
    #[test]
    fn the_synchronize_fill_carries_both_maps() {
        let mut scheduler = ready_scheduler();
        scheduler.set_shared_data(SHARED_DATA_TYPE_BASE_APP, "b", "1", (0, 0));
        scheduler.set_shared_data(SHARED_DATA_TYPE_GLOBAL, "g", "2", (0, 0));

        let addr = cell_addr(0);
        assert!(matches!(
            scheduler.register(addr, client_addr(0), false, 10),
            RegisterOutcome::Registered(_)
        ));
        let fill = scheduler.finished_init(addr);
        let shared: Vec<_> = fill
            .into_iter()
            .filter_map(|(_, m)| match m {
                ToBaseApp::SetSharedData { data_type, key, .. } => Some((data_type, key)),
                _ => None,
            })
            .collect();
        assert_eq!(shared.len(), 2);
        assert_eq!(shared[0], (SHARED_DATA_TYPE_BASE_APP, "b".to_string()));
        assert_eq!(shared[1], (SHARED_DATA_TYPE_GLOBAL, "g".to_string()));
        assert_eq!(shared.len(), scheduler.join_sync().entries.len());
    }

    /// Registration parks the app in the pending set; `finishedInit` moves
    /// it to the ready set.
    #[test]
    fn registration_moves_pending_to_ready() {
        let mut scheduler = ready_scheduler();
        assert_eq!(scheduler.num_pending(), 0, "no apps yet");

        let addr = cell_addr(0);
        let outcome = scheduler.register(addr, client_addr(0), false, 10);
        assert_eq!(
            outcome,
            RegisterOutcome::Registered(BaseAppInitData {
                id: 1,
                time: 10,
                is_ready: false,
                timeout_period: BASE_APP_TIMEOUT_PERIOD,
            })
        );
        assert_eq!(scheduler.num_pending(), 1);
        assert_eq!(scheduler.num_base_apps(), 0);

        scheduler.finished_init(addr);
        assert_eq!(scheduler.num_pending(), 0);
        assert_eq!(scheduler.num_base_apps(), 1);
    }

    /// The manager defers registration until the CellAppMgr is up, the init
    /// data has arrived and the DBApp hash is non-empty.
    #[test]
    fn registration_is_deferred_until_the_cluster_is_up() {
        let mut scheduler = BaseAppScheduler::new();
        let addr = cell_addr(0);

        assert_eq!(
            scheduler.register(addr, client_addr(0), false, 10),
            RegisterOutcome::Deferred(DeferredReason::CellAppMgrNotEstablished)
        );

        scheduler.set_cell_app_mgr_established(true);
        assert_eq!(
            scheduler.register(addr, client_addr(0), false, 10),
            RegisterOutcome::Deferred(DeferredReason::NoInitData)
        );

        scheduler.set_has_init_data(true);
        assert_eq!(
            scheduler.register(addr, client_addr(0), false, 10),
            RegisterOutcome::Deferred(DeferredReason::DbAppsEmpty)
        );

        scheduler.set_db_apps_empty(false);
        assert!(matches!(
            scheduler.register(addr, client_addr(0), false, 10),
            RegisterOutcome::Registered(_)
        ));
    }

    /// During a controlled shutdown the manager lets the request time out.
    #[test]
    fn registration_is_refused_while_shutting_down() {
        let mut scheduler = ready_scheduler();
        scheduler.controlled_shut_down(ShutDownStage::Inform, 0);

        assert_eq!(
            scheduler.register(cell_addr(0), client_addr(0), false, 10),
            RegisterOutcome::Deferred(DeferredReason::ShuttingDown)
        );
    }

    /// BaseApps and ServiceApps get their own id sequences.
    #[test]
    fn ids_are_allocated_per_subset() {
        let mut scheduler = ready_scheduler();
        let base = cell_addr(0);
        let service = cell_addr(1);

        let RegisterOutcome::Registered(base_data) =
            scheduler.register(base, client_addr(0), false, 10)
        else {
            panic!("base app must register");
        };
        let RegisterOutcome::Registered(service_data) =
            scheduler.register(service, client_addr(1), true, 10)
        else {
            panic!("service app must register");
        };
        assert_eq!(base_data.id, 1);
        assert_eq!(service_data.id, 1);
    }

    /// The id wraps at the reference's arbitrary 28-bit limit.
    #[test]
    fn id_wraps_at_the_reference_limit() {
        let mut scheduler = ready_scheduler();
        scheduler.next_base_id = BASE_APP_ID_MASK;
        let RegisterOutcome::Registered(data) =
            scheduler.register(cell_addr(0), client_addr(0), false, 10)
        else {
            panic!("must register");
        };
        assert_eq!(data.id, 0);
    }

    /// Removal keeps the counts consistent and rejects unknown addresses.
    #[test]
    fn remove_app_keeps_counts_consistent() {
        let mut scheduler = ready_scheduler();
        let addrs = add_base_apps(&mut scheduler, 2);
        assert_eq!(scheduler.num_base_apps(), 2);

        assert!(scheduler.remove_app(addrs[0]));
        assert_eq!(scheduler.num_base_apps(), 1);
        assert!(!scheduler.remove_app(addrs[0]), "already removed");
        assert!(scheduler.remove_app(addrs[1]));
        assert_eq!(scheduler.num_base_apps(), 0);
    }

    /// Removing an app also drops it from the load selection.
    #[test]
    fn removal_drops_the_app_from_selection() {
        let mut scheduler = ready_scheduler();
        let addrs = add_base_apps(&mut scheduler, 2);
        scheduler.inform_of_load(addrs[0], 0.1, 1, 0);
        scheduler.inform_of_load(addrs[1], 0.6, 1, 0);
        assert_eq!(scheduler.least_loaded(false), Some(addrs[0]));

        scheduler.remove_app(addrs[0]);
        assert_eq!(scheduler.least_loaded(false), Some(addrs[1]));
    }

    // ------------------------------------------------------------------
    // BW-05-03-02 接收负载报告并挑选创建目标
    // ------------------------------------------------------------------

    /// A load report updates the app's fields; an unknown app is reported.
    #[test]
    fn load_report_updates_the_app() {
        let mut scheduler = ready_scheduler();
        let addrs = add_base_apps(&mut scheduler, 1);

        assert!(scheduler.inform_of_load(addrs[0], 0.42, 7, 3));
        assert_eq!(scheduler.min_load(false), 0.42);
        assert_eq!(scheduler.max_load(false), 0.42);
        assert_eq!(scheduler.avg_load(false), 0.42);
        assert!(!scheduler.inform_of_load(cell_addr(99), 0.1, 1, 1));
    }

    /// A retiring app is never the least-loaded choice.
    #[test]
    fn least_loaded_skips_retiring_apps() {
        let mut scheduler = ready_scheduler();
        let addrs = add_base_apps(&mut scheduler, 2);
        scheduler.inform_of_load(addrs[0], 0.1, 1, 0);
        scheduler.inform_of_load(addrs[1], 0.9, 1, 0);

        // The lightest app retires; the choice moves to the heavier one.
        let app = scheduler
            .base_apps
            .iter_mut()
            .find(|app| app.addr_for_cells == addrs[0])
            .unwrap();
        app.mark_retiring();

        assert_eq!(scheduler.least_loaded(false), Some(addrs[1]));
    }

    /// A three-instance fixture: the manager creates at the least-loaded
    /// app's client address.
    #[test]
    fn create_entity_picks_the_least_loaded() {
        let mut scheduler = ready_scheduler();
        let addrs = add_base_apps(&mut scheduler, 3);
        scheduler.inform_of_load(addrs[0], 0.4, 1, 0);
        scheduler.inform_of_load(addrs[1], 0.1, 1, 0);
        scheduler.inform_of_load(addrs[2], 0.7, 1, 0);

        assert_eq!(
            scheduler.create_entity(0),
            CreateEntityOutcome::CreateAt(client_addr(1))
        );
    }

    /// The chosen app's load estimate is bumped for the new entity.
    #[test]
    fn create_entity_bumps_the_chosen_load() {
        let mut scheduler = ready_scheduler();
        let addrs = add_base_apps(&mut scheduler, 2);
        scheduler.inform_of_load(addrs[0], 0.5, 1, 0);
        scheduler.inform_of_load(addrs[1], 0.6, 1, 0);

        let _ = scheduler.create_entity(0);
        assert!(
            (scheduler.min_load(false) - 0.51).abs() < 1e-6,
            "the chosen app's load estimate grew by {}",
            BASE_APP_ENTITY_LOAD_STEP
        );
    }

    /// ServiceApps form their own subset for load selection.
    #[test]
    fn service_apps_are_a_separate_subset() {
        let mut scheduler = ready_scheduler();
        let base = add_base_apps(&mut scheduler, 1);
        let service = cell_addr(1);
        scheduler.register(service, client_addr(1), true, 10);
        scheduler.finished_init(service);
        scheduler.inform_of_load(base[0], 0.2, 1, 0);
        scheduler.inform_of_load(service, 0.9, 1, 0);

        assert_eq!(scheduler.least_loaded(false), Some(base[0]));
        assert_eq!(scheduler.least_loaded(true), Some(service));
    }

    // ------------------------------------------------------------------
    // BW-05-03-03 处理登录条件失败与回退
    // ------------------------------------------------------------------

    /// With no ready BaseApp the login is rejected outright.
    #[test]
    fn no_base_apps_rejects_the_login() {
        let mut scheduler = ready_scheduler();
        assert_eq!(
            scheduler.create_entity(0),
            CreateEntityOutcome::Rejected(CreateEntityError::NoBaseApps)
        );
    }

    /// An overloaded fleet lets `overloadLogins` logins through, then
    /// rejects until the load drops.
    #[test]
    fn overload_is_rate_limited_then_rejected() {
        let mut scheduler = ready_scheduler();
        let addrs = add_base_apps(&mut scheduler, 1);
        scheduler.inform_of_load(addrs[0], 0.9, 1, 0); // above MIN_LOAD

        for _ in 0..OVERLOAD_LOGINS {
            assert_eq!(
                scheduler.create_entity(0),
                CreateEntityOutcome::CreateAt(client_addr(0)),
                "logins slip through while under the tolerance"
            );
        }
        assert_eq!(
            scheduler.create_entity(0),
            CreateEntityOutcome::Rejected(CreateEntityError::BaseAppsOverloaded),
            "the overload sticks after the allowed logins"
        );
    }

    /// The overload is also reached once the tolerance period elapses,
    /// whichever comes first.
    #[test]
    fn overload_after_the_tolerance_period() {
        let mut scheduler = ready_scheduler();
        let addrs = add_base_apps(&mut scheduler, 1);
        scheduler.inform_of_load(addrs[0], 0.9, 1, 0);

        // One login at t=0 starts the window.
        assert_eq!(
            scheduler.create_entity(0),
            CreateEntityOutcome::CreateAt(client_addr(0))
        );
        // Past the tolerance period the next login is rejected.
        let later = (MIN_OVERLOAD_TOLERANCE_PERIOD_SECS * 1000.0) as u64 + 1;
        assert_eq!(
            scheduler.create_entity(later),
            CreateEntityOutcome::Rejected(CreateEntityError::BaseAppsOverloaded)
        );
    }

    /// Once the best load is back under the threshold the counters clear
    /// and logins resume.
    #[test]
    fn overload_counters_reset_when_load_drops() {
        let mut scheduler = ready_scheduler();
        let addrs = add_base_apps(&mut scheduler, 1);
        scheduler.inform_of_load(addrs[0], 0.9, 1, 0);
        let _ = scheduler.create_entity(0);
        let _ = scheduler.create_entity(0);

        scheduler.inform_of_load(addrs[0], 0.3, 1, 0);
        assert_eq!(
            scheduler.create_entity(0),
            CreateEntityOutcome::CreateAt(client_addr(0))
        );
        // The window did not carry over.
        let _ = scheduler.create_entity(0);
        assert_eq!(
            scheduler.create_entity(0),
            CreateEntityOutcome::CreateAt(client_addr(0))
        );
    }

    /// A fleet exactly at the threshold is not overloaded (`>` is strict).
    #[test]
    fn fleet_at_the_threshold_is_not_overloaded() {
        let mut scheduler = ready_scheduler();
        let addrs = add_base_apps(&mut scheduler, 1);

        // Keep the reported load pinned to the threshold so the load estimate
        // bumps from accepted logins do not push it over.
        for _ in 0..100 {
            scheduler.inform_of_load(addrs[0], MIN_LOAD, 1, 0);
            assert_eq!(
                scheduler.create_entity(0),
                CreateEntityOutcome::CreateAt(client_addr(0))
            );
        }
    }

    // ------------------------------------------------------------------
    // BW-05-03-04 转发全局状态
    // ------------------------------------------------------------------

    /// Shared data set on the manager reaches every BaseApp with the same
    /// payload.
    #[test]
    fn shared_data_reaches_every_base_app() {
        let mut scheduler = ready_scheduler();
        let addrs = add_base_apps(&mut scheduler, 2);

        let out = scheduler.set_shared_data(SHARED_DATA_TYPE_BASE_APP, "k", "v", (5, 1));
        assert_eq!(out.base_apps.len(), 2);
        assert_eq!(out.cell_app_mgr, None);
        for (to, message) in &out.base_apps {
            assert!(addrs.contains(to), "every app is notified");
            assert_eq!(
                message,
                &ToBaseApp::SetSharedData {
                    data_type: SHARED_DATA_TYPE_BASE_APP,
                    key: "k".to_string(),
                    value: "v".to_string(),
                    original_src: (5, 1),
                }
            );
        }
    }

    /// The manager keeps its authoritative copy so a late BaseApp is
    /// synchronised on `finishedInit`.
    #[test]
    fn shared_data_is_kept_for_new_apps() {
        let mut scheduler = ready_scheduler();
        scheduler.set_shared_data(SHARED_DATA_TYPE_BASE_APP, "k", "v", (0, 0));

        let late = cell_addr(1);
        scheduler.register(late, client_addr(1), false, 10);
        let out = scheduler.finished_init(late);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, late);
        assert_eq!(
            out[0].1,
            ToBaseApp::SetSharedData {
                data_type: SHARED_DATA_TYPE_BASE_APP,
                key: "k".to_string(),
                value: "v".to_string(),
                original_src: (0, 0),
            }
        );
    }

    // ------------------------------------------------------------------
    // BW-07-03-04 传播 SharedData 更新和删除
    // ------------------------------------------------------------------

    use crate::cellappmgr::{AddAppOutcome, CellAppMgr};
    use crate::shared_data::SharedDataMessage;

    /// A CellAppMgr with two ready CellApps and a registered BaseAppMgr.
    fn cam_with_two_cell_apps() -> (CellAppMgr, Vec<Addr>) {
        let mut cam = CellAppMgr::new();
        cam.set_base_app((0x7f000001, 100));
        cam.set_db_app_alpha((0x7f000001, 200));
        cam.allow_new_cell_apps = true;
        let addrs = [(0x7f000001, 3000), (0x7f000001, 4000)];
        for (index, addr) in addrs.iter().enumerate() {
            assert!(matches!(
                cam.add_app(*addr, 1000 + index as u16, 0),
                AddAppOutcome::Accepted { .. }
            ));
        }
        (cam, addrs.to_vec())
    }

    /// A global set from a BaseApp crosses to the CellAppMgr, reaches every
    /// CellApp, relays back to the BaseAppMgr and then reaches every BaseApp
    /// — in that order (`baseappmgr.cpp:2146`, `cellappmgr.cpp:1965/1972`).
    #[test]
    fn global_set_flows_base_to_cell_and_back_in_order() {
        let mut bam = ready_scheduler();
        let base_apps = add_base_apps(&mut bam, 2);
        let sender = base_apps[0];
        let (mut cam, cell_apps) = cam_with_two_cell_apps();

        // Event 1: the BaseAppMgr forwards the global set to the CellAppMgr;
        // the BaseApps are not told yet.
        let out = bam.set_shared_data(SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP, "k", "v", sender);
        assert!(out.base_apps.is_empty(), "BaseApps wait for the relay");
        let SharedDataMessage::Set {
            data_type,
            key,
            value,
            original_src,
        } = out.cell_app_mgr.expect("forward")
        else {
            panic!("expected a forward");
        };
        assert_eq!(data_type, SHARED_DATA_TYPE_GLOBAL);
        assert_eq!(key, "k");
        assert_eq!(value, "v");
        assert_eq!(original_src, sender);

        // Event 2: the CellAppMgr stores the authoritative copy, broadcasts to
        // every CellApp, and relays to the BaseAppMgr.
        let cam_out = cam.set_shared_data(data_type, &key, &value, original_src);
        assert_eq!(cam.shared_global_data().get("k"), Some(&"v".to_string()));
        assert_eq!(cam_out.cell_apps.len(), 2);
        for (addr, message) in &cam_out.cell_apps {
            assert!(cell_apps.contains(addr), "every CellApp is notified");
            assert_eq!(
                message,
                &SharedDataMessage::Set {
                    data_type: SHARED_DATA_TYPE_GLOBAL,
                    key: "k".to_string(),
                    value: "v".to_string(),
                    original_src: sender,
                }
            );
        }
        let relay = cam_out.base_app_mgr.expect("relay");
        assert_eq!(
            relay,
            SharedDataMessage::Set {
                data_type: SHARED_DATA_TYPE_GLOBAL,
                key: "k".to_string(),
                value: "v".to_string(),
                original_src: sender,
            }
        );

        // Event 3: the relay reaches the BaseAppMgr, which stores and
        // broadcasts to every BaseApp.
        let out = bam.set_shared_data(SHARED_DATA_TYPE_GLOBAL, "k", "v", sender);
        assert_eq!(bam.shared_global_data().get("k"), Some(&"v".to_string()));
        assert_eq!(out.base_apps.len(), 2);
        assert!(out.cell_app_mgr.is_none());
        for (addr, message) in &out.base_apps {
            assert!(base_apps.contains(addr), "every BaseApp is notified");
            assert_eq!(
                message,
                &ToBaseApp::SetSharedData {
                    data_type: SHARED_DATA_TYPE_GLOBAL,
                    key: "k".to_string(),
                    value: "v".to_string(),
                    original_src: sender,
                }
            );
        }
    }

    /// BASE_APP data is the BaseAppMgr's alone: it never reaches the
    /// CellAppMgr or the CellApps.
    #[test]
    fn base_app_set_stays_on_base_apps() {
        let mut bam = ready_scheduler();
        let base_apps = add_base_apps(&mut bam, 1);
        let (mut cam, _cell_apps) = cam_with_two_cell_apps();

        let out = bam.set_shared_data(SHARED_DATA_TYPE_BASE_APP, "k", "v", base_apps[0]);
        assert_eq!(out.base_apps.len(), 1);
        assert_eq!(out.cell_app_mgr, None);

        // The CellAppMgr sees nothing for BASE_APP data unless told; a CELL_APP
        // set it does relay stays among the CellApps.
        let cam_out = cam.set_shared_data(SHARED_DATA_TYPE_CELL_APP, "c", "x", (0x0a, 1));
        assert_eq!(cam.shared_cell_app_data().get("c"), Some(&"x".to_string()));
        assert_eq!(cam_out.cell_apps.len(), 2);
        assert_eq!(cam_out.base_app_mgr, None, "cell data stays among CellApps");
    }

    /// The delete path mirrors the set path: a global delete crosses to the
    /// CellAppMgr, reaches every CellApp, relays back and reaches every
    /// BaseApp, and both authoritative copies are erased.
    #[test]
    fn del_shared_data_follows_the_same_order() {
        let mut bam = ready_scheduler();
        let base_apps = add_base_apps(&mut bam, 1);
        let sender = base_apps[0];
        let (mut cam, _cell_apps) = cam_with_two_cell_apps();
        bam.set_shared_data(SHARED_DATA_TYPE_GLOBAL, "k", "v", sender);
        cam.set_shared_data(SHARED_DATA_TYPE_GLOBAL, "k", "v", sender);

        // Event 1: forward the delete to the CellAppMgr.
        let out = bam.del_shared_data(SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP, "k", sender);
        assert!(out.base_apps.is_empty());
        assert_eq!(
            out.cell_app_mgr,
            Some(SharedDataMessage::Del {
                data_type: SHARED_DATA_TYPE_GLOBAL,
                key: "k".to_string(),
                original_src: sender,
            })
        );

        // Event 2: the CellAppMgr erases, broadcasts the delete to every
        // CellApp, and relays to the BaseAppMgr.
        let cam_out = cam.del_shared_data(SHARED_DATA_TYPE_GLOBAL, "k", sender);
        assert!(cam.shared_global_data().get("k").is_none());
        assert_eq!(cam_out.cell_apps.len(), 2);
        for (_, message) in &cam_out.cell_apps {
            assert_eq!(
                message,
                &SharedDataMessage::Del {
                    data_type: SHARED_DATA_TYPE_GLOBAL,
                    key: "k".to_string(),
                    original_src: sender,
                }
            );
        }
        assert_eq!(
            cam_out.base_app_mgr,
            Some(SharedDataMessage::Del {
                data_type: SHARED_DATA_TYPE_GLOBAL,
                key: "k".to_string(),
                original_src: sender,
            })
        );

        // Event 3: the relay erases the BaseAppMgr copy and broadcasts the
        // delete to every BaseApp.
        let out = bam.del_shared_data(SHARED_DATA_TYPE_GLOBAL, "k", sender);
        assert!(bam.shared_global_data().get("k").is_none());
        assert_eq!(out.base_apps.len(), 1);
        assert_eq!(
            out.base_apps[0].1,
            ToBaseApp::DelSharedData {
                data_type: SHARED_DATA_TYPE_GLOBAL,
                key: "k".to_string(),
                original_src: sender,
            }
        );
    }

    /// `recoverSharedDataFromStream` restores both authoritative maps from the
    /// recovery stream (count, then count key/value pairs, per section).
    #[test]
    fn recovery_restores_the_authoritative_maps() {
        let mut stream = BinaryStream::new();
        stream.write_u32(2);
        stream.write_string("a").unwrap();
        stream.write_string("1").unwrap();
        stream.write_string("b").unwrap();
        stream.write_string("2").unwrap();
        stream.write_u32(1);
        stream.write_string("g").unwrap();
        stream.write_string("9").unwrap();

        let mut bam = ready_scheduler();
        bam.recover_shared_data_from_stream(&mut stream)
            .expect("recover");
        assert_eq!(bam.shared_base_app_data().get("a"), Some(&"1".to_string()));
        assert_eq!(bam.shared_base_app_data().get("b"), Some(&"2".to_string()));
        assert_eq!(bam.shared_global_data().get("g"), Some(&"9".to_string()));
    }

    /// A global base is broadcast to every other app; a duplicate is
    /// refused with a zero success code.
    #[test]
    fn global_base_broadcast_excludes_the_sender() {
        let mut scheduler = ready_scheduler();
        let addrs = add_base_apps(&mut scheduler, 2);
        let mailbox = GlobalBaseMailbox {
            entity: 7,
            addr: (2, 30000),
        };

        let (code, out) = scheduler.register_base_globally(addrs[0], "portal", mailbox.clone());
        assert_eq!(code, 1);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, addrs[1], "the sender is excluded");
        assert_eq!(
            out[0].1,
            ToBaseApp::AddGlobalBase {
                name: "portal".to_string(),
                mailbox: mailbox.clone(),
            }
        );

        // A second registration of the same name fails.
        let (code, out) = scheduler.register_base_globally(addrs[1], "portal", mailbox.clone());
        assert_eq!(code, 0);
        assert!(out.is_empty());
    }

    /// Deregistering a global base broadcasts the removal.
    #[test]
    fn deregister_removes_and_broadcasts() {
        let mut scheduler = ready_scheduler();
        let addrs = add_base_apps(&mut scheduler, 2);
        scheduler.register_base_globally(
            addrs[0],
            "portal",
            GlobalBaseMailbox {
                entity: 7,
                addr: (2, 30000),
            },
        );

        let out = scheduler.deregister_base_globally(addrs[0], "portal");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, addrs[1]);
        assert_eq!(
            out[0].1,
            ToBaseApp::DelGlobalBase {
                name: "portal".to_string()
            }
        );
    }

    // ------------------------------------------------------------------
    // BW-05-03-05 协调关服
    // ------------------------------------------------------------------

    /// `SHUTDOWN_INFORM` tells every BaseApp; with none left it acks the
    /// CellAppMgr immediately.
    #[test]
    fn inform_stage_informs_all_apps() {
        let mut scheduler = ready_scheduler();
        let addrs = add_base_apps(&mut scheduler, 2);

        let out = scheduler.controlled_shut_down(ShutDownStage::Inform, 42);
        let ShutdownOutcome::InformApps(apps) = out else {
            panic!("expected to inform the apps, got {out:?}");
        };
        assert_eq!(apps.len(), 2);
        for (to, message) in &apps {
            assert!(addrs.contains(to));
            assert_eq!(
                message,
                &ToBaseApp::ControlledShutDown {
                    stage: ShutDownStage::Inform,
                    shut_down_time: 42,
                }
            );
        }
        assert_eq!(scheduler.shut_down_stage(), ShutDownStage::Inform);
    }

    #[test]
    fn inform_stage_with_no_apps_acks_immediately() {
        let mut scheduler = ready_scheduler();
        assert_eq!(
            scheduler.controlled_shut_down(ShutDownStage::Inform, 0),
            ShutdownOutcome::AckToCellAppMgr(ShutDownStage::Inform)
        );
    }

    /// `SHUTDOWN_PERFORM` begins the proxy-disconnect stage for every app.
    #[test]
    fn perform_stage_starts_disconnect_proxies() {
        let mut scheduler = ready_scheduler();
        let addrs = add_base_apps(&mut scheduler, 2);
        let out = scheduler.controlled_shut_down(ShutDownStage::Perform, 0);
        assert_eq!(out, ShutdownOutcome::StartDisconnectProxies(addrs));
    }

    /// `SHUTDOWN_REQUEST` forwards to the CellAppMgr once it is established.
    #[test]
    fn request_stage_forwards_to_cell_app_mgr() {
        let mut scheduler = ready_scheduler();
        assert_eq!(
            scheduler.controlled_shut_down(ShutDownStage::Request, 0),
            ShutdownOutcome::ForwardRequestToCellAppMgr
        );

        scheduler.set_cell_app_mgr_established(false);
        assert_eq!(
            scheduler.controlled_shut_down(ShutDownStage::Request, 0),
            ShutdownOutcome::None
        );
    }

    /// `SHUTDOWN_TRIGGER` starts the whole server shutdown.
    #[test]
    fn trigger_starts_the_server_shutdown() {
        let mut scheduler = ready_scheduler();
        assert_eq!(
            scheduler.controlled_shut_down(ShutDownStage::Trigger, 0),
            ShutdownOutcome::TriggerServerShutdown
        );
    }

    /// `initData` is taken once and only adopts the DBApp time on the first
    /// start.
    #[test]
    fn init_data_taken_once_and_start_time_guarded() {
        let mut scheduler = BaseAppScheduler::new();
        assert_eq!(
            scheduler.init_data(100, 0),
            InitDataOutcome::Accepted {
                should_set_start_time: true
            }
        );
        assert_eq!(scheduler.init_data(200, 0), InitDataOutcome::Ignored);

        // A recovery start adopts the time only when the clock is still 0.
        let mut scheduler = BaseAppScheduler::new();
        assert_eq!(
            scheduler.init_data(300, 0),
            InitDataOutcome::Accepted {
                should_set_start_time: true
            }
        );
        let mut scheduler = BaseAppScheduler::new();
        assert_eq!(
            scheduler.init_data(300, 50),
            InitDataOutcome::Accepted {
                should_set_start_time: false
            }
        );
    }

    /// `startup` sets `hasStarted_`, forwards the CellAppMgr start, and puts
    /// the bootstrap flag on exactly one non-service BaseApp.
    #[test]
    fn startup_sets_ready_and_bootstraps_one() {
        let mut scheduler = ready_scheduler();
        let base = add_base_apps(&mut scheduler, 2);
        assert!(!scheduler.request_has_started());

        let outcome = scheduler.startup(false);
        let StartupOutcome::Started { apps, cell_app_mgr } = outcome else {
            panic!("unexpected startup outcome: {outcome:?}");
        };
        assert!(cell_app_mgr);
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].addr, base[0]);
        assert!(apps[0].bootstrap);
        assert!(!apps[1].bootstrap);
        assert!(!apps[1].did_auto_load_entities_from_db);

        assert!(scheduler.request_has_started());
        assert_eq!(scheduler.startup(true), StartupOutcome::AlreadyStarted);

        // A BaseApp registered after the start is told it is ready.
        let late = cell_addr(9);
        assert!(matches!(
            scheduler.register(late, client_addr(9), false, 10),
            RegisterOutcome::Registered(BaseAppInitData { is_ready: true, .. })
        ));
    }

    /// `startup` refuses when no BaseApp has registered yet.
    #[test]
    fn startup_refused_without_base_apps() {
        let mut scheduler = ready_scheduler();
        assert_eq!(scheduler.startup(false), StartupOutcome::NoBaseApps);
        assert!(!scheduler.request_has_started());
    }

    /// The bootstrap flag never lands on a service app.
    #[test]
    fn bootstrap_skips_service_apps() {
        let mut scheduler = ready_scheduler();
        let base = add_base_apps(&mut scheduler, 1);
        let service = cell_addr(100);
        assert!(matches!(
            scheduler.register(service, client_addr(100), true, 10),
            RegisterOutcome::Registered(_)
        ));
        scheduler.finished_init(service);

        let outcome = scheduler.startup(true);
        let StartupOutcome::Started { apps, .. } = outcome else {
            panic!("unexpected startup outcome: {outcome:?}");
        };
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].addr, base[0]);
        assert!(apps[0].bootstrap);
        assert!(apps[0].did_auto_load_entities_from_db);
        assert_eq!(apps[1].addr, service);
        assert!(!apps[1].bootstrap);
    }

    // ------------------------------------------------------------------
    // BW-07-03-02 启动并注册 Service
    // ------------------------------------------------------------------

    use crate::entity_creator::MailBoxComponent;

    /// Register and finish the init of one BaseApp and one ServiceApp.
    fn scheduler_with_service_and_base() -> (BaseAppScheduler, Addr, Addr) {
        let mut scheduler = ready_scheduler();
        let base = cell_addr(1);
        let service = cell_addr(2);
        assert!(matches!(
            scheduler.register(base, client_addr(1), false, 10),
            RegisterOutcome::Registered(_)
        ));
        scheduler.finished_init(base);
        assert!(matches!(
            scheduler.register(service, client_addr(2), true, 10),
            RegisterOutcome::Registered(_)
        ));
        scheduler.finished_init(service);
        (scheduler, base, service)
    }

    /// A service entity's mailbox on `service`: the address is the ServiceApp
    /// that hosts it (`MF_ASSERT_DEV(srcAddr == mailBox.addr)`).
    fn service_mailbox(id: u32, service: Addr) -> EntityMailBoxRef {
        EntityMailBoxRef::init(id, service, MailBoxComponent::Base, 1)
    }

    /// The same configuration (the same fragments registered in the same
    /// order) yields the same available services on the manager.
    #[test]
    fn the_same_configuration_yields_the_same_available_services() {
        fn run() -> BaseAppScheduler {
            let (mut scheduler, base, service) = scheduler_with_service_and_base();
            // One ServiceApp registers two fragments of one service and one
            // of another.
            scheduler.register_service_fragment(service, "A", service_mailbox(1, service));
            scheduler.register_service_fragment(service, "B", service_mailbox(2, service));
            scheduler.register_service_fragment(service, "A", service_mailbox(3, service));
            assert_eq!(scheduler.fragments_for_service("A").len(), 2);
            assert_eq!(scheduler.service_names().len(), 2);
            let _ = base;
            scheduler
        }
        let first = run();
        let second = run();
        assert_eq!(first.service_names(), second.service_names());
        assert_eq!(
            first.service_names(),
            vec!["A".to_string(), "B".to_string()]
        );
        assert_eq!(
            first.fragments_for_service("A"),
            second.fragments_for_service("A")
        );
        assert_eq!(
            first.choose_service_fragment("A", 0),
            second.choose_service_fragment("A", 0)
        );
    }

    /// A registered fragment is broadcast to every managed app, including the
    /// sender, and forwarded to the CellAppMgr.
    #[test]
    fn a_registered_fragment_reaches_every_app_and_the_cell_app_mgr() {
        let (mut scheduler, base, service) = scheduler_with_service_and_base();
        let mailbox = service_mailbox(42, service);
        let outcome = scheduler.register_service_fragment(service, "DBAppMgr", mailbox);

        assert_eq!(outcome.removed, 1);
        assert!(!outcome.did_lose_service);
        // Every ready app is told to add the fragment, including the sender.
        let mut addrs: Vec<_> = outcome.broadcasts.iter().map(|(addr, _)| *addr).collect();
        addrs.sort();
        assert_eq!(addrs, vec![base, service]);
        for (_addr, message) in &outcome.broadcasts {
            assert_eq!(
                message,
                &ToBaseApp::AddServiceFragment {
                    name: "DBAppMgr".to_string(),
                    mailbox,
                }
            );
        }
        assert_eq!(
            outcome.cell_app_mgr,
            Some(CellAppMgrServiceMessage::AddServiceFragment {
                name: "DBAppMgr".to_string(),
                mailbox,
            })
        );
        assert!(!outcome.shut_down_server);

        // The service is available on the manager.
        assert_eq!(scheduler.service_names(), vec!["DBAppMgr".to_string()]);
        assert_eq!(scheduler.fragments_for_service("DBAppMgr"), vec![mailbox]);
        assert_eq!(
            scheduler.choose_service_fragment("DBAppMgr", 0),
            Some(mailbox)
        );
    }

    /// A message from an app the manager does not know is dropped: no map
    /// change, no broadcasts, no CellAppMgr forward.
    #[test]
    fn an_unregistered_app_cannot_register_a_fragment() {
        let (mut scheduler, _base, _service) = scheduler_with_service_and_base();
        let stranger = cell_addr(99);
        let outcome =
            scheduler.register_service_fragment(stranger, "A", service_mailbox(1, stranger));
        assert_eq!(outcome.removed, 0);
        assert!(outcome.broadcasts.is_empty());
        assert_eq!(outcome.cell_app_mgr, None);
        assert_eq!(scheduler.service_names(), Vec::<String>::new());
    }

    /// A deregistered fragment leaves the map, reaches every app and the
    /// CellAppMgr; losing the last fragment may ask for a shutdown.
    #[test]
    fn a_deregistered_fragment_leaves_the_map() {
        let (mut scheduler, base, service) = scheduler_with_service_and_base();
        scheduler.register_service_fragment(service, "A", service_mailbox(1, service));

        let outcome = scheduler.deregister_service_fragment(service, "A");
        assert_eq!(outcome.removed, 1);
        assert!(outcome.did_lose_service);
        let mut addrs: Vec<_> = outcome.broadcasts.iter().map(|(addr, _)| *addr).collect();
        addrs.sort();
        assert_eq!(addrs, vec![base, service]);
        for (_addr, message) in &outcome.broadcasts {
            assert_eq!(
                message,
                &ToBaseApp::DelServiceFragment {
                    name: "A".to_string(),
                    addr: service,
                }
            );
        }
        assert_eq!(
            outcome.cell_app_mgr,
            Some(CellAppMgrServiceMessage::DelServiceFragment {
                name: "A".to_string(),
                addr: service,
            })
        );

        // The service is gone from the manager.
        assert_eq!(scheduler.service_names(), Vec::<String>::new());
        assert!(scheduler.fragments_for_service("A").is_empty());
    }

    /// A service that keeps another fragment is not lost, and no shutdown is
    /// asked for.
    #[test]
    fn a_service_with_a_remaining_fragment_is_not_lost() {
        let (mut scheduler, _base, service) = scheduler_with_service_and_base();
        let peer = cell_addr(3);
        assert!(matches!(
            scheduler.register(peer, client_addr(3), true, 10),
            RegisterOutcome::Registered(_)
        ));
        scheduler.finished_init(peer);

        scheduler.register_service_fragment(service, "A", service_mailbox(1, service));
        scheduler.register_service_fragment(peer, "A", service_mailbox(2, peer));

        let outcome = scheduler.deregister_service_fragment(service, "A");
        assert_eq!(outcome.removed, 1);
        assert!(!outcome.did_lose_service);
        assert!(!outcome.shut_down_server);
        // The service is still available on the peer.
        assert_eq!(
            scheduler.fragments_for_service("A"),
            vec![service_mailbox(2, peer)]
        );
    }

    /// `shutDownServerOnServiceDeath` asks for a shutdown only when a service
    /// loses its last fragment outside a controlled shutdown.
    #[test]
    fn losing_the_last_fragment_may_shut_the_server_down() {
        let (mut scheduler, _base, service) = scheduler_with_service_and_base();
        scheduler.set_shut_down_on_service_death(true);
        scheduler.register_service_fragment(service, "A", service_mailbox(1, service));

        let outcome = scheduler.deregister_service_fragment(service, "A");
        assert!(outcome.did_lose_service);
        assert!(outcome.shut_down_server);

        // Without the config the same loss does not shut the server down.
        let (mut scheduler, _base, service) = scheduler_with_service_and_base();
        scheduler.register_service_fragment(service, "A", service_mailbox(1, service));
        let outcome = scheduler.deregister_service_fragment(service, "A");
        assert!(outcome.did_lose_service);
        assert!(!outcome.shut_down_server);
    }
}
