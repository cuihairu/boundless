//! The bot connection layer (BW-17-01-01): one simulated client — its
//! connection phases, log on / log off semantics and the retry schedule.
//!
//! Ported from `programming/bigworld/server/tools/bots/client_app.{hpp,cpp}`
//! (the `ClientApp` connection face) and `bots_config.cpp` (the transport
//! derivation). The Mercury `BWServerConnection` underneath belongs to the
//! connection-model port; here the transport is abstracted the same way the
//! watcher forwarding port abstracts Mercury: the caller feeds the events the
//! server would deliver (`feed_log_on_result`, `feed_log_on_failure`,
//! `feed_logged_off`) and they are applied by the connection update inside
//! `tick`, exactly where `pConnection_->update(dTime)` processes them in the
//! reference — which is what makes the tick's `wasOnline` branch observe a
//! fresh disconnect.

use std::collections::VecDeque;

/// `NULL_ENTITY_ID` (connection/bw_entity.hpp): no player entity yet.
pub const NULL_ENTITY_ID: u32 = 0;

/// `TICK_FREQUENCY` (main_app.cpp:63): the bots process ticks at 10 Hz.
pub const TICK_FREQUENCY: u32 = 10;

/// `TICK_PERIOD` (main_app.cpp:66): one tick is a tenth of a second.
pub const TICK_PERIOD: f32 = 1.0 / TICK_FREQUENCY as f32;

/// `FD_PER_BOT` (main_app.cpp:74): the file-descriptor budget reserved per
/// bot when `addBotWithName` refuses to exceed the process limit.
pub const FD_PER_BOT: i32 = 10;

/// `ConnectionTransport` (connection/connection_transport.hpp) as derived by
/// `BotsConfig::postInit` (bots_config.cpp:56-67).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    /// `CONNECTION_TRANSPORT_UDP` — the default when TCP is not requested.
    #[default]
    Udp,
    /// `CONNECTION_TRANSPORT_TCP`
    Tcp,
    /// `CONNECTION_TRANSPORT_WEBSOCKETS`
    WebSockets,
}

/// `BotsConfig::postInit` (bots_config.cpp:56-67): TCP is only used when
/// requested, and websockets only on top of TCP.
pub fn transport_from_config(should_use_tcp: bool, should_use_web_sockets: bool) -> Transport {
    if should_use_tcp {
        if should_use_web_sockets {
            Transport::WebSockets
        } else {
            Transport::Tcp
        }
    } else {
        Transport::Udp
    }
}

/// The connection phase, as `BWConnection` reports it through
/// `isOnline()`/`isLoggingIn()` (client_app.hpp:267-271).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnState {
    /// Neither online nor logging in.
    #[default]
    Idle,
    /// `isLoggingIn()` — a log-on attempt is in flight.
    LoggingIn,
    /// `isOnline()` — the session is established.
    Online,
}

/// One server-delivered event, waiting for the next connection update.
#[derive(Debug, Clone, Copy)]
enum ConnEvent {
    /// The log-on reply carried an established session: the player entity
    /// and its space are assigned.
    LogOnResult(u32, u32),
    /// The log-on was refused; `Some(at)` schedules a retry for when the
    /// configured `logOnRetryPeriod` is non-zero.
    LogOnFailure(Option<f64>),
    /// The session ended; `Some(keep)` is the personality script's dormant
    /// decision.
    LoggedOff(Option<bool>),
}

/// A movement-controller request: the `type`/`data` pair the reference
/// feeds to `MainApp::createMovementController` (client_app.cpp:729-732).
/// The factory that interprets the pair lands with BW-17-01-03; until then
/// an accepted request is recorded verbatim so the fleet's tag-filtered
/// update is observable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerSpec {
    /// The controller type (e.g. `"Patrol"`, bots_config.cpp:28).
    pub kind: String,
    /// The controller data (e.g. `"server/bots/test.bwp"`,
    /// bots_config.cpp:29).
    pub data: String,
}

/// One simulated client: the `ClientApp` connection layer.
///
/// The default speed mirrors the constructor's `6 + rand()*2/RAND_MAX`
/// floor (client_app.cpp:387); the seeded 6..8 draw arrives with the
/// movement face (BW-17-01-03).
#[derive(Debug, Clone)]
pub struct Bot {
    conn_state: ConnState,
    is_dormant: bool,
    destroyed: bool,
    log_on_retry_time: f64,
    user_name: String,
    user_passwd: String,
    transport: Transport,
    tag: String,
    speed: f32,
    entity_id: u32,
    space_id: u32,
    controller: Option<ControllerSpec>,
    events: VecDeque<ConnEvent>,
}

impl Bot {
    /// A new `ClientApp`. Like the reference constructor
    /// (client_app.cpp:363-407) it starts dormant and immediately calls
    /// `logOn()`, which wakes it and starts the log-on attempt.
    pub fn new(name: &str, password: &str, transport: Transport, tag: &str) -> Self {
        let mut bot = Self {
            conn_state: ConnState::Idle,
            is_dormant: true,
            destroyed: false,
            log_on_retry_time: 0.0,
            user_name: name.to_string(),
            user_passwd: password.to_string(),
            transport,
            tag: tag.to_string(),
            speed: 6.0,
            entity_id: NULL_ENTITY_ID,
            space_id: 0,
            controller: None,
            events: VecDeque::new(),
        };
        bot.log_on();
        bot
    }

    /// `ClientApp::logOn` (client_app.cpp:554-572): clears the retry
    /// schedule, refuses when destroyed, wakes the bot, and does nothing if
    /// a log-on attempt is already running or the session is online.
    /// Otherwise the log-on attempt starts.
    pub fn log_on(&mut self) {
        self.log_on_retry_time = 0.0;

        if self.destroyed {
            return;
        }

        self.is_dormant = false;

        if matches!(self.conn_state, ConnState::LoggingIn | ConnState::Online) {
            return;
        }

        self.conn_state = ConnState::LoggingIn;
    }

    /// `ClientApp::logOff` (client_app.cpp:575-582): only an online session
    /// can log off. The end-of-session processing itself arrives with the
    /// delivered logged-off event.
    pub fn log_off(&mut self) {
        if self.conn_state == ConnState::Online {
            self.conn_state = ConnState::Idle;
        }
    }

    /// `ClientApp::dropConnection` (client_app.cpp:584-593): cut the
    /// transport without informing the server. No logged-off processing
    /// runs here — the dormant decision belongs to the delivered
    /// logged-off event, not to a local cut.
    pub fn drop_connection(&mut self) {
        if self.conn_state == ConnState::Online {
            self.conn_state = ConnState::Idle;
        }
    }

    /// Queue the server's acceptance of the log-on: the session becomes
    /// established and the player entity (with its space) assigned on the
    /// next tick's connection update.
    pub fn feed_log_on_result(&mut self, entity_id: u32, space_id: u32) {
        self.events
            .push_back(ConnEvent::LogOnResult(entity_id, space_id));
    }

    /// Queue a log-on refusal (`ClientApp::onLogOnFailure`,
    /// client_app.cpp:1235-1269). `Some(at)` schedules the retry for `at`
    /// (= now + `logOnRetryPeriod`); `None` means the period is zero and
    /// the bot is left to the fleet's mercy.
    pub fn feed_log_on_failure(&mut self, retry_at: Option<f64>) {
        self.events.push_back(ConnEvent::LogOnFailure(retry_at));
    }

    /// Queue the end of the session (`ClientApp::onLoggedOff`,
    /// client_app.cpp:1193-1235). The personality script decides whether
    /// the bot parks dormant for another attempt (`Some(true)`) or is
    /// destroyed (`Some(false)`); `None` (no script, no return value)
    /// leaves the flag untouched. The entities are cleared afterwards
    /// either way — the reference clears them *after* the callback so the
    /// callback can still find the bot.
    pub fn feed_logged_off(&mut self, script_keep_alive: Option<bool>) {
        self.events
            .push_back(ConnEvent::LoggedOff(script_keep_alive));
    }

    /// `ClientApp::tick` (client_app.cpp:431-470): a dormant bot is
    /// skipped; the connection update applies the queued events; a bot
    /// that was online or logging in and is now neither has just
    /// disconnected — its return value is the dormant flag, which tells
    /// the fleet whether to keep or remove it. The movement part of the
    /// tick lands with BW-17-01-03.
    pub fn tick(&mut self, dtime: f32) -> bool {
        let _ = dtime;
        if self.is_dormant {
            return true;
        }

        let was_online = matches!(self.conn_state, ConnState::Online | ConnState::LoggingIn);

        // pConnection_->update(dTime): process everything the server
        // delivered since the last tick.
        while let Some(event) = self.events.pop_front() {
            match event {
                ConnEvent::LogOnResult(entity_id, space_id) => {
                    if self.conn_state == ConnState::LoggingIn {
                        self.conn_state = ConnState::Online;
                        self.entity_id = entity_id;
                        self.space_id = space_id;
                    }
                }
                ConnEvent::LogOnFailure(retry_at) => {
                    self.conn_state = ConnState::Idle;
                    if let Some(at) = retry_at {
                        self.log_on_retry_time = at;
                    }
                }
                ConnEvent::LoggedOff(script_keep_alive) => {
                    self.conn_state = ConnState::Idle;
                    if let Some(keep) = script_keep_alive {
                        self.is_dormant = keep;
                    }
                    self.clear_entities();
                }
            }
        }

        if was_online && !matches!(self.conn_state, ConnState::Online | ConnState::LoggingIn) {
            // Either just became dormant, or just disconnected and want to
            // be destroyed.
            return self.is_dormant;
        }

        true
    }

    /// `ClientApp::destroy` (client_app.cpp:514-550): idempotent — a
    /// destroyed bot refuses further work; the session is logged off and
    /// the entities cleared.
    pub fn destroy(&mut self) {
        if self.destroyed {
            return;
        }
        self.destroyed = true;
        self.log_off();
        self.clear_entities();
    }

    /// `BWConnection::clearAllEntities` — the player entity and its space
    /// go away, so `id()` returns `NULL_ENTITY_ID` again.
    pub fn clear_entities(&mut self) {
        self.entity_id = NULL_ENTITY_ID;
        self.space_id = 0;
    }

    /// The bot's tag (client_app.hpp:107).
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// The bot's login name (the `loginName` attribute).
    pub fn user_name(&self) -> &str {
        &self.user_name
    }

    /// The bot's password (the `loginPassword` attribute).
    pub fn user_passwd(&self) -> &str {
        &self.user_passwd
    }

    /// The derived transport (the `transport` attribute).
    pub const fn transport(&self) -> Transport {
        self.transport
    }

    /// The movement speed (the `speed` attribute).
    pub const fn speed(&self) -> f32 {
        self.speed
    }

    /// Whether the bot has been destroyed (the `isDestroyed` attribute).
    pub const fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    /// The `isDormant` lifecycle flag: dormant bots are skipped by ticks.
    pub const fn is_dormant(&self) -> bool {
        self.is_dormant
    }

    /// The scheduled retry time, `0.0` when nothing is scheduled (the
    /// `logOnRetryTime` accessor, client_app.hpp:131-132).
    pub const fn log_on_retry_time(&self) -> f64 {
        self.log_on_retry_time
    }

    /// The connection phase.
    pub const fn conn_state(&self) -> ConnState {
        self.conn_state
    }

    /// The player entity id, `NULL_ENTITY_ID` while logged off.
    pub const fn id(&self) -> u32 {
        self.entity_id
    }

    /// The space the player entity occupies.
    pub const fn space_id(&self) -> u32 {
        self.space_id
    }

    /// Whether the session is online (`isOnline` attribute).
    pub const fn is_online(&self) -> bool {
        matches!(self.conn_state, ConnState::Online)
    }

    /// Whether a log-on attempt is in flight (`isLoggingIn`).
    pub const fn is_logging_in(&self) -> bool {
        matches!(self.conn_state, ConnState::LoggingIn)
    }

    /// The `TICK_PERIOD` this bot's fleet ticks it with.
    pub const fn tick_period() -> f32 {
        TICK_PERIOD
    }

    /// `ClientApp::canSetMovementController` (client_app.cpp:697-711): a
    /// controller can only be set while the player entity is alive, in the
    /// world and client-controlled — here, an established session with an
    /// assigned player entity on an undestroyed bot.
    pub const fn can_set_movement_controller(&self) -> bool {
        !self.destroyed
            && matches!(self.conn_state, ConnState::Online)
            && self.entity_id != NULL_ENTITY_ID
    }

    /// `ClientApp::setMovementController` (client_app.cpp:713-742): on a
    /// bot that cannot take one the controller is left unchanged and the
    /// call fails. The accepted request is recorded verbatim — the factory
    /// that builds the controller (and validates the type) arrives with
    /// BW-17-01-03.
    pub fn set_movement_controller(&mut self, kind: &str, data: &str) -> bool {
        if !self.can_set_movement_controller() {
            return false;
        }
        self.controller = Some(ControllerSpec {
            kind: kind.to_string(),
            data: data.to_string(),
        });
        true
    }

    /// The recorded movement-controller request, `None` until one is
    /// accepted (`pMovementController_`).
    pub fn controller(&self) -> Option<&ControllerSpec> {
        self.controller.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UDP: Transport = Transport::Udp;

    /// client_app.cpp:363-407 — the constructor starts dormant and calls
    /// `logOn()`, which wakes the bot and starts the attempt; the tag and
    /// credentials are kept verbatim.
    #[test]
    fn a_new_bot_wakes_and_starts_logging_in() {
        let bot = Bot::new("Bot_1_app0", "pw", UDP, "Default");
        assert!(!bot.is_dormant());
        assert_eq!(bot.conn_state(), ConnState::LoggingIn);
        assert_eq!(bot.user_name(), "Bot_1_app0");
        assert_eq!(bot.user_passwd(), "pw");
        assert_eq!(bot.tag(), "Default");
        assert_eq!(bot.transport(), UDP);
        assert_eq!(bot.id(), NULL_ENTITY_ID);
        assert_eq!(bot.space_id(), 0);
        assert!(!bot.is_destroyed());
        assert_eq!(bot.log_on_retry_time(), 0.0);
    }

    /// client_app.cpp:554-572 — `logOn` refuses when destroyed or when an
    /// attempt/session is already running.
    #[test]
    fn log_on_is_noop_while_destroyed_or_already_connecting() {
        // A destroyed bot refuses to restart even from idle.
        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_failure(None);
        assert!(!bot.tick(TICK_PERIOD)); // the failed attempt surfaces
        assert_eq!(bot.conn_state(), ConnState::Idle);
        bot.destroy();
        bot.log_on();
        assert!(bot.is_destroyed());
        assert_eq!(bot.conn_state(), ConnState::Idle);

        // An online bot does not re-log-on.
        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_result(7, 3);
        bot.tick(TICK_PERIOD);
        assert_eq!(bot.conn_state(), ConnState::Online);
        bot.log_on();
        assert_eq!(bot.conn_state(), ConnState::Online);

        // A bot already logging in does not restart the attempt.
        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.log_on();
        assert_eq!(bot.conn_state(), ConnState::LoggingIn);
    }

    /// A delivered acceptance establishes the session and assigns the
    /// player entity and space; a refusal schedules a retry only when the
    /// configured period is non-zero (client_app.cpp:1261-1266).
    #[test]
    fn delivered_results_move_the_connection_state() {
        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_result(42, 9);
        bot.tick(TICK_PERIOD);
        assert!(bot.is_online());
        assert_eq!(bot.id(), 42);
        assert_eq!(bot.space_id(), 9);

        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_failure(None);
        bot.tick(TICK_PERIOD);
        assert_eq!(bot.log_on_retry_time(), 0.0);

        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_failure(Some(102.5));
        bot.tick(TICK_PERIOD);
        assert_eq!(bot.log_on_retry_time(), 102.5);
    }

    /// client_app.cpp:575-582 — only an online session can log off.
    #[test]
    fn log_off_only_takes_an_online_bot_offline() {
        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.log_off();
        assert_eq!(bot.conn_state(), ConnState::LoggingIn);

        bot.feed_log_on_result(1, 1);
        bot.tick(TICK_PERIOD);
        bot.log_off();
        assert!(!bot.is_online());
        assert_eq!(bot.conn_state(), ConnState::Idle);

        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_failure(None);
        bot.tick(TICK_PERIOD);
        bot.log_off();
        assert_eq!(bot.conn_state(), ConnState::Idle);
    }

    /// client_app.cpp:1193-1235 — the logged-off processing takes the
    /// dormant decision from the script and then clears the entities.
    #[test]
    fn losing_the_connection_leaves_the_dormant_decision_to_script() {
        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_result(5, 2);
        bot.feed_logged_off(Some(true));
        assert!(bot.tick(TICK_PERIOD));
        assert!(bot.is_dormant());
        assert_eq!(bot.id(), NULL_ENTITY_ID);
        assert_eq!(bot.space_id(), 0);

        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_result(5, 2);
        bot.feed_logged_off(Some(false));
        assert!(!bot.tick(TICK_PERIOD));
        assert!(!bot.is_dormant());
        assert_eq!(bot.id(), NULL_ENTITY_ID);

        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_result(5, 2);
        bot.feed_logged_off(None);
        // No script decision: the flag stays false, so the bot is
        // destroyed after the tick reports the fresh disconnect.
        assert!(!bot.tick(TICK_PERIOD));
        assert_eq!(bot.id(), NULL_ENTITY_ID);
    }

    /// client_app.cpp:431-470 — dormant bots are skipped; an established
    /// or in-flight session ticks through.
    #[test]
    fn tick_skips_dormant_bots() {
        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.is_dormant = true;
        assert!(bot.tick(TICK_PERIOD));

        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_result(3, 1);
        bot.tick(TICK_PERIOD);
        assert!(bot.tick(TICK_PERIOD));

        let mut bot = Bot::new("b", "p", UDP, "t");
        assert!(bot.tick(TICK_PERIOD));
    }

    /// client_app.cpp:584-593 — a dropped connection ends the session
    /// without the logged-off processing; only an online session drops.
    #[test]
    fn drop_connection_cuts_without_telling_the_server() {
        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.drop_connection();
        assert_eq!(bot.conn_state(), ConnState::LoggingIn);

        bot.feed_log_on_result(8, 4);
        bot.tick(TICK_PERIOD);
        bot.drop_connection();
        assert!(!bot.is_online());
        // No logged-off processing ran: the player entity is untouched.
        assert_eq!(bot.id(), 8);
    }

    /// client_app.cpp:514-550 — destroy is idempotent, logs the session
    /// off and clears the entities.
    #[test]
    fn destroy_is_idempotent_and_ends_the_session() {
        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_result(6, 2);
        bot.tick(TICK_PERIOD);
        bot.destroy();
        assert!(bot.is_destroyed());
        assert!(!bot.is_online());
        assert_eq!(bot.id(), NULL_ENTITY_ID);

        bot.destroy();
        assert!(bot.is_destroyed());
        bot.log_on();
        assert_eq!(bot.conn_state(), ConnState::Idle);
    }

    /// bots_config.cpp:56-67 — the transport follows the config flags.
    #[test]
    fn transport_follows_the_config_flags() {
        assert_eq!(transport_from_config(false, false), Transport::Udp);
        assert_eq!(transport_from_config(false, true), Transport::Udp);
        assert_eq!(transport_from_config(true, false), Transport::Tcp);
        assert_eq!(transport_from_config(true, true), Transport::WebSockets);
    }

    /// A log-on acceptance only lands on an in-flight attempt; the
    /// speed and logging-in accessors report the live fields.
    #[test]
    fn a_result_off_ignored_and_the_accessors_report() {
        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_failure(None);
        bot.tick(TICK_PERIOD);
        assert_eq!(bot.conn_state(), ConnState::Idle);
        bot.feed_log_on_result(11, 5);
        bot.tick(TICK_PERIOD);
        assert_eq!(bot.conn_state(), ConnState::Idle);
        assert_eq!(bot.id(), NULL_ENTITY_ID);
        assert!(!bot.is_logging_in());

        let bot = Bot::new("b", "p", UDP, "t");
        assert!(bot.is_logging_in());
        assert_eq!(bot.speed(), 6.0);
    }

    /// main_app.cpp:62-74 — the tick and budget constants the fleet runs
    /// with.
    #[test]
    fn the_fleet_constants_match_the_reference() {
        assert_eq!(TICK_FREQUENCY, 10);
        assert_eq!(Bot::tick_period(), 0.1);
        assert_eq!(FD_PER_BOT, 10);
    }

    /// client_app.cpp:697-742 — a controller only lands on a bot whose
    /// player entity is alive in the world; a refused request leaves the
    /// slot unchanged.
    #[test]
    fn a_controller_only_lands_on_a_bot_in_the_world() {
        let mut bot = Bot::new("b", "p", UDP, "t");
        assert!(!bot.can_set_movement_controller());
        assert!(!bot.set_movement_controller("Patrol", "server/bots/test.bwp"));
        assert!(bot.controller().is_none());

        bot.feed_log_on_result(4, 2);
        bot.tick(TICK_PERIOD);
        assert!(bot.can_set_movement_controller());
        assert!(bot.set_movement_controller("Patrol", "server/bots/test.bwp"));
        let spec = bot.controller().expect("controller recorded");
        assert_eq!(spec.kind, "Patrol");
        assert_eq!(spec.data, "server/bots/test.bwp");

        bot.destroy();
        assert!(!bot.can_set_movement_controller());
    }
}
