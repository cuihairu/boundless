//! BW-05-06-03 the cluster-wide staged shutdown broadcast.
//!
//! The reference cascade (`server/cellappmgr/shutdown_handler.cpp`,
//! `server/baseappmgr/baseappmgr.cpp`, `server/dbappmgr/dbappmgr.cpp`):
//!
//! ```text
//! operator / last CellApp death
//!   -> CellAppMgr::triggerControlledShutDown  (SHUTDOWN_TRIGGER -> BaseAppMgr)
//!   -> BaseAppMgr::controlledShutDownServer   (LoginApp, else DBApp alpha,
//!                                              else the BaseAppMgr itself)
//!   -> DBApp alpha -> DBAppMgr (SHUTDOWN_REQUEST -> BaseAppMgr)
//!   -> BaseAppMgr (SHUTDOWN_REQUEST -> CellAppMgr, once its channel is up)
//!   -> CellAppMgr::ShutDownHandler::start      (the Inform phase)
//!       INFORM -> BaseAppMgr + every CellApp, with the deadline tick
//!       every CellApp acks, the BaseApps ack via the BaseAppMgr
//!       when time reaches the deadline and everything acked
//!         -> PerformBaseAppsHandler (SHUTDOWN_PERFORM -> BaseAppMgr)
//!         -> BaseAppMgr: DISCONNECT_PROXIES then PERFORM to its apps
//!         -> BaseAppMgr acks SHUTDOWN_PERFORM to the CellAppMgr
//!   -> CellAppMgr (SHUTDOWN_PERFORM -> DBAppMgr, then shuts down the rest)
//!   -> DBAppMgr (SHUTDOWN_PERFORM -> every DBApp, then itself)
//! ```
//!
//! The module carries the CellAppMgr's `ShutDownHandler` (`pShutDownHandler_`)
//! and the `BaseAppMgr::controlledShutDownServer` senior-process choice. The
//! Mercury traffic is not modelled: the handler returns the outbound
//! [`ShutDownAction`]s the reference would send, so the Mercury lane can
//! drive the sockets when it lands. The BaseAppMgr and DBAppMgr stage
//! handlers live in their own modules (`baseappmgr.rs`, `dbappmgr.rs`).

use std::collections::BTreeSet;

use crate::shutdown::ShutDownStage;

/// A Mercury address as `(ip, port)` with `ip` in network byte order.
pub type Addr = (u32, u16);

/// What the shutdown cascade asks a process to send or do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutDownAction {
    /// `CellAppMgr::triggerControlledShutDown`: `SHUTDOWN_TRIGGER` to the
    /// BaseAppMgr.
    TriggerAtBaseAppMgr,
    /// `BaseAppMgrInterface::controlledShutDown` at `SHUTDOWN_INFORM` with
    /// the deadline tick.
    InformBaseAppMgr(u32),
    /// `CellAppInterface::controlledShutDown` at `SHUTDOWN_INFORM` with the
    /// deadline tick: `(cell app, shut down time)`.
    InformCellApp(Addr, u32),
    /// `BaseAppMgrInterface::controlledShutDown` at `SHUTDOWN_PERFORM`.
    PerformBaseAppMgr,
    /// `DBAppMgrInterface::controlledShutDown` at `SHUTDOWN_PERFORM`.
    PerformDbAppMgr,
    /// The CellAppMgr exits itself: `shutDown(shutDownOthers)`.
    ShutDownCellAppMgr(bool),
}

/// The `ShutDownHandler` state machine (`shutdown_handler.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutDownHandler {
    /// `InformHandler`: every BaseApp and CellApp is told the deadline, and
    /// the phase ends when time reaches it and everything acked.
    Inform {
        /// `baseAppsAcked_`.
        base_apps_acked: bool,
        /// `ackedCellApps_`, by CellApp address.
        acked_cell_apps: BTreeSet<Addr>,
        /// `mgr_.numCellApps()`: the number of CellApps to hear from. The
        /// reference re-reads the live count; a CellApp death during the
        /// phase is refused (`handleCellAppDeath` returns early while
        /// `isShuttingDown_`), so the count is stable for the phase.
        num_cell_apps: usize,
        /// `shutDownTime_`.
        shut_down_time: u32,
        /// `mgr_.time()`.
        time: u32,
    },
    /// `PerformBaseAppsHandler`: the BaseApps are told to shut down; the
    /// phase ends with their ack.
    Perform,
}

impl ShutDownHandler {
    /// `ShutDownHandler::start`: begin the Inform phase and broadcast the
    /// `SHUTDOWN_INFORM` stage to the BaseAppMgr and every CellApp.
    fn start(
        current_time: u32,
        shutting_down_delay: u32,
        cell_apps: &[Addr],
    ) -> (Self, Vec<ShutDownAction>) {
        let shut_down_time = current_time.saturating_add(shutting_down_delay);
        let mut actions = vec![ShutDownAction::InformBaseAppMgr(shut_down_time)];
        for addr in cell_apps {
            actions.push(ShutDownAction::InformCellApp(*addr, shut_down_time));
        }
        (
            Self::Inform {
                base_apps_acked: false,
                acked_cell_apps: BTreeSet::new(),
                num_cell_apps: cell_apps.len(),
                shut_down_time,
                time: current_time,
            },
            actions,
        )
    }

    /// `isPaused()`: the Inform phase waits until `time == shutDownTime_`.
    fn is_paused(&self) -> bool {
        match self {
            Self::Inform {
                time,
                shut_down_time,
                ..
            } => time == shut_down_time,
            // The base class default: `true`.
            Self::Perform => true,
        }
    }

    /// `handleTimeout`'s shutdown path: every tick advances the clock, then
    /// runs the phase check when paused.
    fn on_tick(&mut self, current_time: u32) -> Vec<ShutDownAction> {
        if let Self::Inform { time, .. } = self {
            *time = current_time;
        }
        if self.is_paused() {
            self.check_status()
        } else {
            Vec::new()
        }
    }

    /// `InformHandler::checkStatus`: the Inform phase completes once the
    /// deadline is reached and every BaseApp and CellApp acked; the handler
    /// becomes the `PerformBaseAppsHandler` (`PerformBaseAppMgr` message).
    fn check_status(&mut self) -> Vec<ShutDownAction> {
        let Self::Inform {
            base_apps_acked,
            acked_cell_apps,
            num_cell_apps,
            shut_down_time,
            time,
        } = self
        else {
            return Vec::new();
        };
        if *time != *shut_down_time || !*base_apps_acked || acked_cell_apps.len() < *num_cell_apps {
            return Vec::new();
        }
        *self = Self::Perform;
        vec![ShutDownAction::PerformBaseAppMgr]
    }

    /// `ackBaseApps`: the BaseAppMgr's ack of the stage. `(actions, finished)`
    /// with `finished` meaning the handler is done and should be dropped
    /// (`setShutDownHandler(NULL)`).
    fn ack_base_apps(&mut self, _stage: ShutDownStage) -> (Vec<ShutDownAction>, bool) {
        match self {
            Self::Inform {
                base_apps_acked, ..
            } => {
                // The reference logs an error for a wrong stage but proceeds.
                *base_apps_acked = true;
                (self.check_status(), false)
            }
            Self::Perform => {
                // PerformBaseAppsHandler::ackBaseApps: tell the DBAppMgr to
                // perform, then the CellAppMgr shuts down the rest.
                (
                    vec![
                        ShutDownAction::PerformDbAppMgr,
                        ShutDownAction::ShutDownCellAppMgr(true),
                    ],
                    true,
                )
            }
        }
    }

    /// `InformHandler::ackCellApp`: a CellApp's ack of the stage.
    fn ack_cell_app(&mut self, addr: Addr, _stage: ShutDownStage) -> Vec<ShutDownAction> {
        match self {
            Self::Inform {
                acked_cell_apps, ..
            } => {
                acked_cell_apps.insert(addr);
                self.check_status()
            }
            // The reference's PerformBaseAppsHandler::ackCellApp is an
            // error arm; the CellApps are not involved in this stage.
            Self::Perform => Vec::new(),
        }
    }
}

/// The CellAppMgr's shutdown machinery (`pShutDownHandler_`).
#[derive(Debug, Clone, Default)]
pub struct CellAppMgrShutDown {
    /// `pShutDownHandler_`, `None` when the manager is not shutting down.
    handler: Option<ShutDownHandler>,
}

impl CellAppMgrShutDown {
    pub fn new() -> Self {
        Self { handler: None }
    }

    /// `pShutDownHandler_` is up (the cascade is running). The CellAppMgr's
    /// own `isShuttingDown_` is a different flag, set only when the manager
    /// itself exits.
    pub fn has_handler(&self) -> bool {
        self.handler.is_some()
    }

    /// `CellAppMgr::controlledShutDown`: `SHUTDOWN_TRIGGER` is forwarded to
    /// the BaseAppMgr (`triggerControlledShutDown`); `SHUTDOWN_REQUEST`
    /// starts the Inform phase. Other stages are the reference's error arm.
    pub fn controlled_shut_down(
        &mut self,
        stage: ShutDownStage,
        current_time: u32,
        shutting_down_delay: u32,
        cell_apps: &[Addr],
    ) -> Vec<ShutDownAction> {
        match stage {
            ShutDownStage::Trigger => vec![ShutDownAction::TriggerAtBaseAppMgr],
            ShutDownStage::Request => {
                let (handler, actions) =
                    ShutDownHandler::start(current_time, shutting_down_delay, cell_apps);
                self.handler = Some(handler);
                actions
            }
            _ => Vec::new(),
        }
    }

    /// `CellAppMgr::ackBaseAppsShutDown`: the BaseAppMgr's stage ack.
    pub fn ack_base_apps_shut_down(&mut self, stage: ShutDownStage) -> Vec<ShutDownAction> {
        let (actions, finished) = match self.handler.as_mut() {
            Some(handler) => handler.ack_base_apps(stage),
            None => return Vec::new(),
        };
        if finished {
            // setShutDownHandler(NULL).
            self.handler = None;
        }
        actions
    }

    /// `InformHandler::ackCellApp`: a CellApp's stage ack.
    pub fn ack_cell_app_shut_down(
        &mut self,
        addr: Addr,
        stage: ShutDownStage,
    ) -> Vec<ShutDownAction> {
        match self.handler.as_mut() {
            Some(handler) => handler.ack_cell_app(addr, stage),
            None => Vec::new(),
        }
    }

    /// `handleTimeout`'s shutdown path: each tick, when paused, run the
    /// phase check.
    pub fn on_tick(&mut self, current_time: u32) -> Vec<ShutDownAction> {
        match self.handler.as_mut() {
            Some(handler) => handler.on_tick(current_time),
            None => Vec::new(),
        }
    }
}

/// `BaseAppMgr::controlledShutDownServer`: the most 'senior' process that
/// currently exists -- the LoginApp, then the DBApp alpha, then the
/// BaseAppMgr itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeniorShutDownTarget {
    /// Send `LoginIntInterface::controlledShutDown` to the LoginApp.
    LoginApp(Addr),
    /// Send `DBAppInterface::controlledShutDown` at `SHUTDOWN_REQUEST` to
    /// the alpha DBApp (which forwards to the DBAppMgr).
    DbAppAlpha(Addr),
    /// No LoginApp or DBApp is up: begin the shutdown here.
    BaseAppMgr,
}

/// `BaseAppMgr::controlledShutDownServer`: try the LoginApp first, then the
/// DBApp alpha, then the BaseAppMgr itself.
pub fn controlled_shut_down_server(
    login_app: Option<Addr>,
    db_app_alpha: Option<Addr>,
) -> SeniorShutDownTarget {
    if let Some(addr) = login_app {
        SeniorShutDownTarget::LoginApp(addr)
    } else if let Some(addr) = db_app_alpha {
        SeniorShutDownTarget::DbAppAlpha(addr)
    } else {
        SeniorShutDownTarget::BaseAppMgr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell_addr(i: u16) -> Addr {
        (0x7f000001, 6000 + i)
    }

    fn base_addr() -> Addr {
        (0x7f000001, 5000)
    }

    fn alpha_addr() -> Addr {
        (0x7f000001, 4000)
    }

    /// `SHUTDOWN_TRIGGER` is forwarded to the BaseAppMgr.
    #[test]
    fn trigger_forwards_to_the_base_app_mgr() {
        let mut mgr = CellAppMgrShutDown::new();
        assert_eq!(
            mgr.controlled_shut_down(ShutDownStage::Trigger, 100, 0, &[]),
            vec![ShutDownAction::TriggerAtBaseAppMgr]
        );
        // The trigger does not start the handler.
        assert!(!mgr.has_handler());
    }

    /// `SHUTDOWN_REQUEST` starts the Inform phase, which broadcasts the
    /// deadline to the BaseAppMgr and every CellApp.
    #[test]
    fn request_broadcasts_the_inform_stage() {
        let cell_apps = [cell_addr(0), cell_addr(1)];
        let mut mgr = CellAppMgrShutDown::new();
        assert_eq!(
            mgr.controlled_shut_down(ShutDownStage::Request, 100, 10, &cell_apps),
            vec![
                ShutDownAction::InformBaseAppMgr(110),
                ShutDownAction::InformCellApp(cell_addr(0), 110),
                ShutDownAction::InformCellApp(cell_addr(1), 110),
            ]
        );
        assert!(mgr.has_handler());
    }

    /// The Inform phase needs the deadline, the BaseApps' ack and every
    /// CellApp's ack before it moves on.
    #[test]
    fn the_inform_phase_waits_for_time_and_acks() {
        let cell_app = cell_addr(0);
        let mut mgr = CellAppMgrShutDown::new();
        mgr.controlled_shut_down(ShutDownStage::Request, 100, 10, &[cell_app]);

        // Both acks arrive, but the deadline is not reached.
        assert_eq!(mgr.ack_base_apps_shut_down(ShutDownStage::Inform), vec![]);
        assert_eq!(
            mgr.ack_cell_app_shut_down(cell_app, ShutDownStage::Inform),
            vec![]
        );
        // A tick before the deadline changes nothing.
        assert_eq!(mgr.on_tick(109), vec![]);

        // At the deadline with everything acked the handler tells the
        // BaseAppMgr to perform.
        assert_eq!(mgr.on_tick(110), vec![ShutDownAction::PerformBaseAppMgr]);
    }

    /// The BaseApps' ack alone cannot finish the Inform phase: the deadline
    /// must also be reached.
    #[test]
    fn base_apps_ack_is_not_enough_before_the_deadline() {
        let cell_app = cell_addr(0);
        let mut mgr = CellAppMgrShutDown::new();
        mgr.controlled_shut_down(ShutDownStage::Request, 100, 10, &[cell_app]);
        mgr.ack_cell_app_shut_down(cell_app, ShutDownStage::Inform);
        assert_eq!(mgr.ack_base_apps_shut_down(ShutDownStage::Inform), vec![]);
        assert_eq!(mgr.on_tick(110), vec![ShutDownAction::PerformBaseAppMgr]);
    }

    /// Every CellApp must ack: a missing ack keeps the Inform phase open.
    #[test]
    fn a_missing_cell_app_ack_keeps_the_inform_phase_open() {
        let mut mgr = CellAppMgrShutDown::new();
        mgr.controlled_shut_down(
            ShutDownStage::Request,
            100,
            10,
            &[cell_addr(0), cell_addr(1)],
        );
        mgr.ack_cell_app_shut_down(cell_addr(0), ShutDownStage::Inform);
        mgr.ack_base_apps_shut_down(ShutDownStage::Inform);
        // At the deadline, one CellApp ack is still missing.
        assert_eq!(mgr.on_tick(110), vec![]);
        // The last ack completes the phase (the time is already there).
        assert_eq!(
            mgr.ack_cell_app_shut_down(cell_addr(1), ShutDownStage::Inform),
            vec![ShutDownAction::PerformBaseAppMgr]
        );
    }

    /// The BaseApps' ack of the Perform phase tells the DBAppMgr to perform
    /// and the CellAppMgr to shut down the rest; the handler is then done.
    #[test]
    fn perform_ack_tells_db_app_mgr_and_shuts_down() {
        let cell_app = cell_addr(0);
        let mut mgr = CellAppMgrShutDown::new();
        mgr.controlled_shut_down(ShutDownStage::Request, 100, 10, &[cell_app]);
        mgr.ack_base_apps_shut_down(ShutDownStage::Inform);
        mgr.ack_cell_app_shut_down(cell_app, ShutDownStage::Inform);
        mgr.on_tick(110); // -> Perform phase

        assert_eq!(
            mgr.ack_base_apps_shut_down(ShutDownStage::Perform),
            vec![
                ShutDownAction::PerformDbAppMgr,
                ShutDownAction::ShutDownCellAppMgr(true),
            ]
        );
        // The handler is dropped (setShutDownHandler(NULL)).
        assert!(!mgr.has_handler());
        // A late ack is ignored.
        assert_eq!(mgr.ack_base_apps_shut_down(ShutDownStage::Perform), vec![]);
    }

    /// With no CellApps and a zero delay the deadline is already reached, so
    /// the BaseApps' ack alone completes the Inform phase.
    #[test]
    fn an_empty_cluster_finishes_the_inform_phase_immediately() {
        let mut mgr = CellAppMgrShutDown::new();
        mgr.controlled_shut_down(ShutDownStage::Request, 100, 0, &[]);
        assert_eq!(
            mgr.ack_base_apps_shut_down(ShutDownStage::Inform),
            vec![ShutDownAction::PerformBaseAppMgr]
        );
        // The handler is already in the Perform phase.
        assert_eq!(mgr.on_tick(100), vec![]);
    }

    /// Stages the CellAppMgr does not handle leave the handler untouched.
    #[test]
    fn unhandled_stages_are_ignored() {
        let mut mgr = CellAppMgrShutDown::new();
        assert_eq!(
            mgr.controlled_shut_down(ShutDownStage::None, 100, 0, &[]),
            vec![]
        );
        assert_eq!(
            mgr.controlled_shut_down(ShutDownStage::DisconnectProxies, 100, 0, &[]),
            vec![]
        );
        assert_eq!(
            mgr.controlled_shut_down(ShutDownStage::Perform, 100, 0, &[]),
            vec![]
        );
        assert!(!mgr.has_handler());
    }

    /// The senior-process order: LoginApp, then the DBApp alpha, then the
    /// BaseAppMgr itself.
    #[test]
    fn the_senior_process_order_matches_the_reference() {
        let login = (0x7f000001, 2000);
        let alpha = alpha_addr();
        assert_eq!(
            controlled_shut_down_server(Some(login), Some(alpha)),
            SeniorShutDownTarget::LoginApp(login)
        );
        assert_eq!(
            controlled_shut_down_server(None, Some(alpha)),
            SeniorShutDownTarget::DbAppAlpha(alpha)
        );
        assert_eq!(
            controlled_shut_down_server(None, None),
            SeniorShutDownTarget::BaseAppMgr
        );
    }

    /// The whole cascade walks the reference exit order: the CellAppMgr's
    /// trigger reaches the BaseAppMgr, the senior-process choice routes via
    /// the DBApp alpha and the DBAppMgr, the request comes back to the
    /// CellAppMgr and the Inform/Perform phases end with the DBAppMgr told to
    /// perform and the CellAppMgr shutting the rest down.
    #[test]
    fn the_cascade_walks_the_reference_exit_order() {
        use crate::baseappmgr::{BaseAppScheduler, ShutdownOutcome};
        use crate::dbappmgr::{DbAppMgr, DbAppMgrShutDownOutcome};

        let base_app = base_addr();
        let alpha = alpha_addr();
        let cell_app = cell_addr(0);

        // 1. The operator tells the CellAppMgr to shut down: it forwards the
        //    trigger to the BaseAppMgr.
        let mut cmgr = CellAppMgrShutDown::new();
        assert_eq!(
            cmgr.controlled_shut_down(ShutDownStage::Trigger, 100, 0, &[cell_app]),
            vec![ShutDownAction::TriggerAtBaseAppMgr]
        );

        // 2. BaseAppMgr::controlledShutDownServer: no LoginApp, the DBApp
        //    alpha is up.
        assert_eq!(
            controlled_shut_down_server(None, Some(alpha)),
            SeniorShutDownTarget::DbAppAlpha(alpha)
        );

        // 3. The DBApp alpha forwards to the DBAppMgr, which forwards the
        //    request to the BaseAppMgr.
        let mut dmgr = DbAppMgr::new();
        dmgr.base_app_mgr_addr = Some(base_app);
        dmgr.add_db_app(alpha, None, 0);
        assert_eq!(
            dmgr.controlled_shut_down(ShutDownStage::Request),
            DbAppMgrShutDownOutcome::ForwardRequestToBaseAppMgr(Some(base_app))
        );
        assert!(dmgr.is_shutting_down);

        // 4. The BaseAppMgr forwards the request to the CellAppMgr once its
        //    channel is established.
        let mut bamgr = BaseAppScheduler::new();
        bamgr.set_cell_app_mgr_established(true);
        assert_eq!(
            bamgr.controlled_shut_down(ShutDownStage::Request, 0),
            ShutdownOutcome::ForwardRequestToCellAppMgr
        );

        // 5. The CellAppMgr starts the Inform phase.
        assert_eq!(
            cmgr.controlled_shut_down(ShutDownStage::Request, 100, 10, &[cell_app]),
            vec![
                ShutDownAction::InformBaseAppMgr(110),
                ShutDownAction::InformCellApp(cell_app, 110),
            ]
        );

        // 6. The BaseAppMgr informs its apps; with none registered it acks
        //    the stage, and the CellApp acks.
        assert_eq!(
            bamgr.controlled_shut_down(ShutDownStage::Inform, 110),
            ShutdownOutcome::AckToCellAppMgr(ShutDownStage::Inform)
        );
        assert_eq!(cmgr.ack_base_apps_shut_down(ShutDownStage::Inform), vec![]);
        assert_eq!(
            cmgr.ack_cell_app_shut_down(cell_app, ShutDownStage::Inform),
            vec![]
        );

        // 7. At the deadline the CellAppMgr tells the BaseAppMgr to perform.
        assert_eq!(cmgr.on_tick(110), vec![ShutDownAction::PerformBaseAppMgr]);

        // 8. The BaseAppMgr runs the DISCONNECT_PROXIES stage (no apps).
        assert_eq!(
            bamgr.controlled_shut_down(ShutDownStage::Perform, 0),
            ShutdownOutcome::StartDisconnectProxies(vec![])
        );

        // 9. The BaseAppMgr acks the Perform stage; the CellAppMgr tells the
        //    DBAppMgr to perform and shuts the rest down.
        assert_eq!(
            cmgr.ack_base_apps_shut_down(ShutDownStage::Perform),
            vec![
                ShutDownAction::PerformDbAppMgr,
                ShutDownAction::ShutDownCellAppMgr(true),
            ]
        );

        // 10. The DBAppMgr tells its DBApps to shut down and exits.
        assert_eq!(
            dmgr.controlled_shut_down(ShutDownStage::Perform),
            DbAppMgrShutDownOutcome::Perform {
                db_apps: vec![alpha],
            }
        );
    }
}
