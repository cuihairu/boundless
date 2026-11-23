//! The bot connection layer (BW-17-01-01) and its script behaviour face
//! (BW-17-01-03): one simulated client — its connection phases, log on /
//! log off semantics, the retry schedule, and the movement a script
//! drives.
//!
//! Ported from `programming/bigworld/server/tools/bots/client_app.{hpp,cpp}`
//! (the `ClientApp` connection and movement faces),
//! `movement_controller.hpp` (the controller/factory interfaces) and
//! `bots_config.cpp` (the transport derivation). The Mercury
//! `BWServerConnection` underneath belongs to the connection-model port;
//! here the transport is abstracted the same way the watcher forwarding
//! port abstracts Mercury: the caller feeds the events the server would
//! deliver (`feed_log_on_result`, `feed_log_on_failure`, `feed_logged_off`)
//! and they are applied by the connection update inside `tick`, exactly
//! where `pConnection_->update(dTime)` processes them in the reference —
//! which is what makes the tick's `wasOnline` branch observe a fresh
//! disconnect.
//!
//! The script boundary is inverted for Rust: Python calls `ClientApp`
//! methods directly from `onTick`; a Rust `Behaviour` cannot hold a
//! `&mut Bot` while the tick runs it, so `on_tick` receives a read-only
//! [`BotView`] and returns the [`Directive`]s it would have issued. They
//! are applied through the very same public faces (`move_to`,
//! `face_towards`, ...) *before* the tick advances the movement — the
//! reference order (`onTick` then the movement step) is preserved.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};

use foundation::math::vector::Vector3;

use crate::fleet::ControllerRegistry;

/// `NULL_ENTITY_ID` (connection/bw_entity.hpp): no player entity yet.
pub const NULL_ENTITY_ID: u32 = 0;

/// `TICK_FREQUENCY` (main_app.cpp:63): the bots process ticks at 10 Hz.
pub const TICK_FREQUENCY: u32 = 10;

/// `TICK_PERIOD` (main_app.cpp:66): one tick is a tenth of a second.
pub const TICK_PERIOD: f32 = 1.0 / TICK_FREQUENCY as f32;

/// `FD_PER_BOT` (main_app.cpp:74): the file-descriptor budget reserved per
/// bot when `addBotWithName` refuses to exceed the process limit.
pub const FD_PER_BOT: i32 = 10;

/// `MATH_PI` (`math/mathdef.hpp`): the reference builds with the plain
/// float constant.
pub const MATH_PI: f32 = std::f32::consts::PI;

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

/// `MovementController` (movement_controller.hpp:17-21): the interface
/// every movement controller implements. `speed` is in-out (controllers
/// may slow the bot down), `pos`/`yaw` are in-out and carry the next step.
pub trait MovementController {
    fn next_step(&mut self, speed: &mut f32, dtime: f32, pos: &mut Vector3, yaw: &mut f32) -> bool;
}

/// `MovementFactory` (movement_controller.hpp:26-32): creates the
/// controllers; registered by name in the fleet's
/// [`ControllerRegistry`](crate::fleet::ControllerRegistry).
pub trait MovementFactory: Sync {
    fn create(
        &self,
        data: &str,
        speed: f32,
        start_pos: &mut Vector3,
    ) -> Option<Box<dyn MovementController>>;
}

/// An order a script issues through the `ClientApp` faces (`PY_AUTO`
/// methods client_app.hpp:225-239 and the `PY_RW` attributes
/// client_app.hpp:217-223). The tick applies these through the very same
/// public methods a Python caller would hit.
#[derive(Debug, Clone, PartialEq)]
pub enum Directive {
    /// `moveTo( pos )`
    MoveTo(Vector3),
    /// `faceTowards( pos )`
    FaceTowards(Vector3),
    /// `snapTo( pos )`
    SnapTo(Vector3),
    /// `stop()`
    Stop,
    /// the `autoMove` attribute
    SetAutoMove(bool),
    /// the `speed` attribute
    SetSpeed(f32),
}

/// The read-only state a `Behaviour` sees for one tick — the `ClientApp`
/// attributes a Python `onTick` could read (client_app.hpp:208-223).
#[derive(Debug, Clone, PartialEq)]
pub struct BotView {
    /// the `id` attribute
    pub id: u32,
    /// the `spaceID` attribute
    pub space_id: u32,
    /// the `position` attribute
    pub position: Vector3,
    /// the `yaw` attribute
    pub yaw: f32,
    /// the `isMoving` attribute
    pub is_moving: bool,
    /// the `autoMove` attribute
    pub auto_move: bool,
    /// the `speed` attribute
    pub speed: f32,
    /// the client clock the movement runs on (`pConnection_->clientTime()`).
    pub client_time: f64,
}

/// The per-bot script entry (`pyPlayer.callMethod( "onTick", serverTime )`,
/// client_app.cpp:458-463): runs inside the tick while the player is in
/// the world, before the movement step.
pub trait Behaviour {
    fn on_tick(&mut self, view: &BotView) -> Vec<Directive>;
}

/// One pending script timer (`TimerRec`, client_app.hpp:161-201): fires
/// during the first tick whose client clock passed `start_time +
/// interval`, optionally repeating from the then-current clock.
struct TimerRec {
    id: i32,
    interval: f32,
    start_time: f32,
    func: Box<dyn FnMut() -> Vec<Directive>>,
    repeat: bool,
}

impl TimerRec {
    /// `finTime()` (client_app.hpp:188).
    fn fin_time(&self) -> f32 {
        self.start_time + self.interval
    }

    /// `restart()` (client_app.hpp:191).
    fn restart(&mut self, game_time: f32) {
        self.start_time = game_time;
    }
}

/// `operator<` returns `finTime() >= other.finTime()` (client_app.hpp:176-179)
/// so the reference's `priority_queue` pops the earliest deadline first;
/// the port inverts `total_cmp` the same way, breaking ties by the lower
/// id (the reference leaves ties to heap order).
impl Ord for TimerRec {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .fin_time()
            .total_cmp(&self.fin_time())
            .then_with(|| other.id.cmp(&self.id))
    }
}

impl PartialOrd for TimerRec {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for TimerRec {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for TimerRec {}

/// One simulated client: the `ClientApp` connection layer and the
/// movement a script drives.
///
/// A bare bot runs at the speed floor 6.0; the fleet draws each bot's
/// speed per the constructor's `6 + rand()*2/RAND_MAX`
/// (client_app.cpp:387) through its seeded draw (BW-17-02-04's
/// determinism face).
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
    events: VecDeque<ConnEvent>,
    /// The player entity's position (`position()`, client_app.cpp:862-875).
    position: Vector3,
    /// The direction the player faces; only the yaw is driven
    /// (`is2DPosition` is true on every move, client_app.cpp:1043).
    yaw: f32,
    /// The `autoMove` attribute — the constructor starts it `true`
    /// (client_app.cpp:385).
    auto_move: bool,
    /// `pDest_`: a `moveTo` destination; movement to it takes precedence
    /// over the controller (client_app.cpp:471-485).
    dest: Option<Vector3>,
    /// `pConnection_->clientTime()`: advanced by the connection update
    /// every tick.
    client_time: f64,
    /// `pMovementController_` (client_app.hpp:154): reset by
    /// `destroy`/`onLoggedOff` (client_app.cpp:525/1197).
    movement: Option<Box<dyn MovementController>>,
    /// The installed script (the player entity's Python class here).
    behaviour: Option<Box<dyn Behaviour>>,
    /// `timerRecs_` (client_app.hpp:203).
    timers: BinaryHeap<TimerRec>,
    /// `deletedTimerRecs_` (client_app.hpp:204).
    deleted_timers: Vec<i32>,
    /// The `ID_TICKER` counter (client_app.hpp:194), started at 1.
    next_timer_id: i32,
    /// The `shouldUseScripts` gate (bots_config.cpp:24, read per tick at
    /// client_app.cpp:457).
    scripts_enabled: bool,
}

impl Bot {
    /// A new `ClientApp`. Like the reference constructor
    /// (client_app.cpp:363-407) it starts dormant and immediately calls
    /// `logOn()`, which wakes it and starts the log-on attempt. The speed
    /// stays at the draw's floor — the fleet performs the seeded draw.
    pub fn new(name: &str, password: &str, transport: Transport, tag: &str) -> Self {
        Self::with_speed(name, password, transport, tag, 6.0)
    }

    /// The constructor with the drawn speed (`6 + rand()*2/RAND_MAX`,
    /// client_app.cpp:387 — the fleet's draw lands in [6, 8)).
    pub fn with_speed(
        name: &str,
        password: &str,
        transport: Transport,
        tag: &str,
        speed: f32,
    ) -> Self {
        let mut bot = Self {
            conn_state: ConnState::Idle,
            is_dormant: true,
            destroyed: false,
            log_on_retry_time: 0.0,
            user_name: name.to_string(),
            user_passwd: password.to_string(),
            transport,
            tag: tag.to_string(),
            speed,
            entity_id: NULL_ENTITY_ID,
            space_id: 0,
            events: VecDeque::new(),
            position: Vector3::new(0.0, 0.0, 0.0),
            yaw: 0.0,
            auto_move: true,
            dest: None,
            client_time: 0.0,
            movement: None,
            behaviour: None,
            timers: BinaryHeap::new(),
            deleted_timers: Vec::new(),
            next_timer_id: 1,
            scripts_enabled: true,
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
    /// leaves the flag untouched. The controller is reset first
    /// (client_app.cpp:1197) and the entities are cleared afterwards — the
    /// reference clears them *after* the callback so the callback can
    /// still find the bot.
    pub fn feed_logged_off(&mut self, script_keep_alive: Option<bool>) {
        self.events
            .push_back(ConnEvent::LoggedOff(script_keep_alive));
    }

    /// `ClientApp::tick` (client_app.cpp:431-503): a dormant bot is
    /// skipped; the connection update applies the queued events and
    /// advances the client clock; a bot that was online or logging in and
    /// is now neither has just disconnected — its return value is the
    /// dormant flag, which tells the fleet whether to keep or remove it.
    /// Then, while the player is in the world, the script runs (directives
    /// then timers) and the movement advances: a `moveTo` destination
    /// first, else `autoMove` via the controller or the circle walk.
    pub fn tick(&mut self, dtime: f32) -> bool {
        if self.is_dormant {
            return true;
        }

        let was_online = matches!(self.conn_state, ConnState::Online | ConnState::LoggingIn);

        // pConnection_->update(dTime): process everything the server
        // delivered since the last tick; the client clock advances with
        // the update.
        self.client_time += dtime as f64;
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

        // pConnection_->updateServer(): the outgoing flush belongs to the
        // connection seat.

        if dtime > 0.0 && self.in_world() {
            if self.scripts_enabled {
                // pyPlayer.callMethod( "onTick", serverTime ) then
                // processTimers() (client_app.cpp:458-466).
                let view = self.view();
                let directives = match self.behaviour.as_mut() {
                    Some(behaviour) => behaviour.on_tick(&view),
                    None => Vec::new(),
                };
                self.apply_directives(directives);
                self.process_timers();
            }

            if self.can_set_movement_controller() {
                // Movement ordered by moveTo() takes precedence over
                // movement by movement controller (client_app.cpp:470-496).
                let dest = self.dest;
                if let Some(dest) = dest {
                    const CLOSE_ENOUGH: f32 = 1.0;
                    let displacement = dest - self.position;
                    let length = displacement.length();
                    if length < CLOSE_ENOUGH {
                        self.dest = None;
                    } else {
                        let step = displacement * (self.speed * dtime / length);
                        let yaw = step.yaw();
                        self.update_position(self.position + step, yaw);
                    }
                } else if self.auto_move {
                    self.add_move(dtime);
                }
            } else {
                // Since we don't have control of the player, remove any
                // destination (client_app.cpp:498-501).
                self.dest = None;
            }
        }

        true
    }

    /// `ClientApp::destroy` (client_app.cpp:514-550): idempotent — a
    /// destroyed bot refuses further work; the session is logged off, the
    /// controller reset (client_app.cpp:525) and the entities cleared.
    pub fn destroy(&mut self) {
        if self.destroyed {
            return;
        }
        self.destroyed = true;
        self.log_off();
        self.clear_entities();
    }

    /// `BWConnection::clearAllEntities` plus the controller reset both
    /// `destroy` (client_app.cpp:525) and `onLoggedOff`
    /// (client_app.cpp:1197) perform: the player entity and its space go
    /// away, so `id()` returns `NULL_ENTITY_ID` again and the movement
    /// falls back to nothing.
    pub fn clear_entities(&mut self) {
        self.entity_id = NULL_ENTITY_ID;
        self.space_id = 0;
        self.movement = None;
    }

    /// Whether the player entity exists and is in the world — the tick's
    /// `dTime > 0 && pPlayer != NULL && pPlayer->isInWorld()` gate
    /// (client_app.cpp:456).
    pub const fn in_world(&self) -> bool {
        matches!(self.conn_state, ConnState::Online) && self.entity_id != NULL_ENTITY_ID
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

    /// `ClientApp::setMovementController` (client_app.cpp:713-742): the
    /// type/data pair resolves through the fleet's registry — the factory
    /// may adjust the position on the way in, and the write-back runs
    /// through the guarded setter (client_app.cpp:742-747). The `"None"`
    /// type clears the controller and still succeeds; an unknown type
    /// fails and leaves the old controller in place (the reference raises
    /// a `TypeError`, main_app.cpp:800-803).
    pub fn set_movement_controller(
        &mut self,
        kind: &str,
        data: &str,
        registry: &ControllerRegistry,
    ) -> bool {
        if !self.can_set_movement_controller() {
            return false;
        }

        let mut position = self.position();
        match registry.create(kind, data, self.speed, &mut position) {
            Ok(controller) => {
                self.set_position(position);
                self.movement = controller;
                true
            }
            Err(_) => false,
        }
    }

    /// Whether a movement controller is installed (`pMovementController_`).
    pub const fn controller_installed(&self) -> bool {
        self.movement.is_some()
    }

    /// Install the script (`set_behaviour`): the per-bot `onTick` entry.
    pub fn set_behaviour(&mut self, behaviour: Box<dyn Behaviour>) {
        self.behaviour = Some(behaviour);
    }

    /// The `shouldUseScripts` gate (bots_config.cpp:24).
    pub fn set_scripts_enabled(&mut self, enabled: bool) {
        self.scripts_enabled = enabled;
    }

    /// The read-only view the script's `on_tick` receives.
    pub fn view(&self) -> BotView {
        BotView {
            id: self.entity_id,
            space_id: self.space_id,
            position: self.position(),
            yaw: self.yaw(),
            is_moving: self.is_moving(),
            auto_move: self.auto_move,
            speed: self.speed,
            client_time: self.client_time,
        }
    }

    /// Apply the directives a script or timer returned, through the same
    /// faces a Python caller would hit.
    fn apply_directives(&mut self, directives: Vec<Directive>) {
        for directive in directives {
            match directive {
                Directive::MoveTo(pos) => self.move_to(pos),
                Directive::FaceTowards(pos) => self.face_towards(pos),
                Directive::SnapTo(pos) => self.snap_to(pos),
                Directive::Stop => self.stop(),
                Directive::SetAutoMove(on) => self.auto_move = on,
                Directive::SetSpeed(speed) => self.speed = speed,
            }
        }
    }

    /// `ClientApp::moveTo` (client_app.cpp:795-811): a bot in control of
    /// its player heads for the point; `moveTo` movement takes precedence
    /// over the controller.
    pub fn move_to(&mut self, pos: Vector3) {
        if !self.can_set_movement_controller() {
            return;
        }
        self.dest = Some(pos);
        self.auto_move = false;
    }

    /// `ClientApp::faceTowards` (client_app.cpp:813-825): turn the yaw
    /// toward the point without stepping.
    pub fn face_towards(&mut self, pos: Vector3) {
        if !self.can_set_movement_controller() {
            return;
        }
        self.yaw = (pos - self.position).yaw();
    }

    /// `ClientApp::stop` (client_app.cpp:827-832): clear the destination
    /// and the auto-move flag.
    pub fn stop(&mut self) {
        self.dest = None;
        self.auto_move = false;
    }

    /// `ClientApp::snapTo` (client_app.hpp:104): teleport, via the guarded
    /// position setter.
    pub fn snap_to(&mut self, pos: Vector3) {
        self.set_position(pos);
    }

    /// `ClientApp::position` setter (client_app.cpp:841-860): a bot in
    /// control of its player moves its player entity locally.
    pub fn set_position(&mut self, pos: Vector3) {
        if !self.can_set_movement_controller() {
            return;
        }
        self.position = pos;
    }

    /// `ClientApp::position` getter (client_app.cpp:862-875): the origin
    /// until the player is in the world.
    pub fn position(&self) -> Vector3 {
        if !self.in_world() {
            return Vector3::new(0.0, 0.0, 0.0);
        }
        self.position
    }

    /// `ClientApp::yaw` setter via the direction face: turn in place.
    pub fn set_yaw(&mut self, yaw: f32) {
        if !self.can_set_movement_controller() {
            return;
        }
        self.yaw = yaw;
    }

    /// `ClientApp::direction` getter's yaw (client_app.cpp:898-911): zero
    /// until the player is in the world.
    pub const fn yaw(&self) -> f32 {
        self.yaw
    }

    /// `isMoving` (client_app.hpp:105): a destination is outstanding.
    pub const fn is_moving(&self) -> bool {
        self.dest.is_some()
    }

    /// The `autoMove` attribute (client_app.hpp:223).
    pub const fn auto_move(&self) -> bool {
        self.auto_move
    }

    /// `ClientApp::addMove` (client_app.cpp:1106-1134): with a controller,
    /// take its next step; without one, walk the circle — period
    /// `10*speed/7`, radius 10, position `(10 sin θ, 0, 10 cos θ)`, yaw
    /// `θ + π/2`, with `θ` off the client clock.
    fn add_move(&mut self, dtime: f32) {
        if !self.can_set_movement_controller() {
            return;
        }

        let mut position = self.position;
        let mut yaw = self.yaw;

        if let Some(controller) = self.movement.as_mut() {
            controller.next_step(&mut self.speed, dtime, &mut position, &mut yaw);
        } else {
            let period = 10.0f32 * self.speed / 7.0;
            let angle = (self.client_time * 2.0 * (MATH_PI as f64) / period as f64) as f32;
            position = Vector3::new(10.0 * angle.sin(), 0.0, 10.0 * angle.cos());
            yaw = angle + MATH_PI / 2.0;
        }

        self.update_position(position, yaw);
    }

    /// `ClientApp::updatePosition` (client_app.cpp:1032-1043): commit a
    /// local move while the player is under control.
    fn update_position(&mut self, position: Vector3, yaw: f32) {
        if !self.can_set_movement_controller() {
            return;
        }
        self.position = position;
        self.yaw = yaw;
    }

    /// `ClientApp::addTimer` (client_app.cpp:1062-1074): a destroyed bot
    /// fails with the negative id; otherwise the timer fires during the
    /// first tick whose client clock passed the deadline.
    pub fn add_timer(
        &mut self,
        interval: f32,
        func: Box<dyn FnMut() -> Vec<Directive>>,
        repeat: bool,
    ) -> i32 {
        if self.destroyed {
            return -1;
        }
        let id = self.next_timer_id;
        self.next_timer_id += 1;
        self.timers.push(TimerRec {
            id,
            interval,
            start_time: self.client_time as f32,
            func,
            repeat,
        });
        id
    }

    /// `ClientApp::delTimer`: the timer is skipped when its turn comes.
    pub fn del_timer(&mut self, id: i32) {
        self.deleted_timers.push(id);
    }

    /// `ClientApp::processTimers` (client_app.cpp:1146-1183): pop every
    /// elapsed timer (skipping the deleted ones), run its callback and
    /// apply what it ordered; repeating timers restart from the current
    /// client clock.
    fn process_timers(&mut self) {
        let now = self.client_time as f32;
        loop {
            let due = matches!(&self.timers.peek(), Some(rec) if rec.fin_time() <= now);
            if !due {
                return;
            }
            let mut rec = self.timers.pop().expect("peeked timer");
            if let Some(index) = self.deleted_timers.iter().position(|&id| id == rec.id) {
                self.deleted_timers.remove(index);
                continue;
            }
            let directives = (rec.func)();
            self.apply_directives(directives);
            if rec.repeat {
                rec.restart(now);
                self.timers.push(rec);
            }
        }
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

    /// The client clock the movement and timers run on
    /// (`pConnection_->clientTime()`).
    pub const fn client_time(&self) -> f64 {
        self.client_time
    }

    /// The `TICK_PERIOD` this bot's fleet ticks it with.
    pub const fn tick_period() -> f32 {
        TICK_PERIOD
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UDP: Transport = Transport::Udp;

    /// A stub controller that plants the player at a fixed spot so a test
    /// can tell controller movement from the circle walk.
    struct StubController;

    impl MovementController for StubController {
        fn next_step(
            &mut self,
            _speed: &mut f32,
            _dtime: f32,
            pos: &mut Vector3,
            _yaw: &mut f32,
        ) -> bool {
            *pos = Vector3::new(7.0, 0.0, 7.0);
            true
        }
    }

    /// A stub factory serving [`StubController`] (movement_controller.hpp:26-32).
    struct StubFactory;

    impl MovementFactory for StubFactory {
        fn create(
            &self,
            _data: &str,
            _speed: f32,
            _start_pos: &mut Vector3,
        ) -> Option<Box<dyn MovementController>> {
            Some(Box::new(StubController))
        }
    }

    fn stub_registry() -> ControllerRegistry {
        let mut registry = ControllerRegistry::new();
        registry.register("Stub", Box::new(StubFactory));
        registry
    }

    /// client_app.cpp:363-407 — the constructor starts dormant and calls
    /// `logOn()`, which wakes the bot and starts the attempt; the tag and
    /// credentials are kept verbatim; autoMove starts true and the player
    /// stands at the origin.
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
        assert_eq!(bot.speed(), 6.0);
        assert!(bot.auto_move());
        assert_eq!(bot.position(), Vector3::new(0.0, 0.0, 0.0));
        assert_eq!(bot.yaw(), 0.0);
        assert!(!bot.in_world());
        assert!(!bot.controller_installed());
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

    /// client_app.cpp:1193-1235 — the logged-off processing resets the
    /// controller (client_app.cpp:1197), takes the dormant decision from
    /// the script and then clears the entities.
    #[test]
    fn losing_the_connection_leaves_the_dormant_decision_to_script() {
        let registry = stub_registry();

        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_result(5, 2);
        bot.tick(TICK_PERIOD);
        assert!(bot.set_movement_controller("Stub", "", &registry));
        bot.feed_logged_off(Some(true));
        assert!(bot.tick(TICK_PERIOD));
        assert!(bot.is_dormant());
        assert_eq!(bot.id(), NULL_ENTITY_ID);
        assert_eq!(bot.space_id(), 0);
        assert!(!bot.controller_installed());

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
        assert_eq!(bot.client_time(), 0.0);

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
    /// off, resets the controller and clears the entities.
    #[test]
    fn destroy_is_idempotent_and_ends_the_session() {
        let registry = stub_registry();

        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_result(6, 2);
        bot.tick(TICK_PERIOD);
        assert!(bot.set_movement_controller("Stub", "", &registry));
        bot.destroy();
        assert!(bot.is_destroyed());
        assert!(!bot.is_online());
        assert_eq!(bot.id(), NULL_ENTITY_ID);
        assert!(!bot.controller_installed());

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

        let mut bot = Bot::new("b", "p", UDP, "t");
        assert!(bot.is_logging_in());
        bot.set_scripts_enabled(false);
        bot.feed_log_on_result(2, 2);
        bot.tick(TICK_PERIOD);
        // Scripts disabled: the bot still comes online, and the movement
        // step is not gated on the script flag — the auto-move advances.
        assert!(bot.is_online());
        assert!(bot.position() != Vector3::new(0.0, 0.0, 0.0));
    }

    /// main_app.cpp:62-74 — the tick and budget constants the fleet runs
    /// with.
    #[test]
    fn the_fleet_constants_match_the_reference() {
        assert_eq!(TICK_FREQUENCY, 10);
        assert_eq!(Bot::tick_period(), 0.1);
        assert_eq!(FD_PER_BOT, 10);
    }

    /// client_app.cpp:697-742 + main_app.cpp:753-803 — the controller
    /// request resolves through the registry: a registered type installs
    /// its controller, `"None"` clears it (main_app.cpp:758-759), an
    /// unknown type fails and leaves the old one (main_app.cpp:800-803),
    /// and a bot not in the world refuses outright.
    #[test]
    fn a_controller_only_lands_on_a_bot_in_the_world() {
        let registry = stub_registry();

        let mut bot = Bot::new("b", "p", UDP, "t");
        assert!(!bot.can_set_movement_controller());
        assert!(!bot.set_movement_controller("Stub", "", &registry));
        assert!(!bot.controller_installed());

        bot.feed_log_on_result(4, 2);
        bot.tick(TICK_PERIOD);
        assert!(bot.can_set_movement_controller());
        assert!(bot.set_movement_controller("Stub", "", &registry));
        assert!(bot.controller_installed());

        // An unknown type fails and leaves the old controller.
        assert!(!bot.set_movement_controller("Patrol", "", &registry));
        assert!(bot.controller_installed());

        // "None" is the documented way to run without a controller.
        assert!(bot.set_movement_controller("None", "", &registry));
        assert!(!bot.controller_installed());

        bot.destroy();
        assert!(!bot.can_set_movement_controller());
    }

    /// client_app.cpp:1106-1134 — with no controller the auto-move walks
    /// the circle: after one tick the client clock is 0.1 and the position
    /// sits on radius 10 at θ = t·2π/period, yaw θ + π/2. With a
    /// controller installed, its next step stands in for the circle.
    #[test]
    fn the_auto_move_walks_the_circle() {
        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_result(1, 1);
        bot.tick(TICK_PERIOD);

        let period = 10.0f32 * 6.0 / 7.0;
        let angle = (TICK_PERIOD as f64 * 2.0 * (MATH_PI as f64) / period as f64) as f32;
        let expected = Vector3::new(10.0 * angle.sin(), 0.0, 10.0 * angle.cos());
        let got = bot.position();
        assert!((got.x - expected.x).abs() < 1e-4);
        assert!((got.y - expected.y).abs() < 1e-4);
        assert!((got.z - expected.z).abs() < 1e-4);
        assert!((bot.yaw() - (angle + MATH_PI / 2.0)).abs() < 1e-4);
        assert!((bot.client_time() - TICK_PERIOD as f64).abs() < 1e-12);

        // With a controller, the auto-move takes its next step instead.
        let registry = stub_registry();
        assert!(bot.set_movement_controller("Stub", "", &registry));
        bot.tick(TICK_PERIOD);
        assert_eq!(bot.position(), Vector3::new(7.0, 0.0, 7.0));
    }

    /// client_app.cpp:470-485 — a moveTo destination outranks the
    /// auto-move: the bot steps `speed*dTime` per tick toward it, faces
    /// the travel direction, and stops once within the 1.0 close-enough
    /// radius (the final partial step is not taken).
    #[test]
    fn move_to_takes_precedence_and_arrives() {
        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_result(1, 1);
        bot.tick(TICK_PERIOD); // online
        bot.snap_to(Vector3::new(0.0, 0.0, 0.0)); // undo the first circle step

        let dest = Vector3::new(3.0, 0.0, 4.0);
        bot.move_to(dest);
        assert!(bot.is_moving());
        assert!(!bot.auto_move());

        // The origin sits 5.0 away; six 0.6 steps leave 1.4, the seventh
        // leaves 0.8, and the eighth tick finds it close enough.
        for _ in 0..6 {
            bot.tick(TICK_PERIOD);
            assert!(bot.is_moving());
        }
        assert!((bot.position().distance(&dest) - 1.4).abs() < 1e-4);
        bot.tick(TICK_PERIOD);
        assert!((bot.position().distance(&dest) - 0.8).abs() < 1e-4);

        bot.tick(TICK_PERIOD);
        assert!(!bot.is_moving());
        assert!((bot.position().distance(&dest) - 0.8).abs() < 1e-4);

        // The yaw followed the travel direction (toward (3,0,4)).
        assert!((bot.yaw() - 3.0f32.atan2(4.0)).abs() < 1e-4);
    }

    /// client_app.cpp:813-832 + client_app.hpp:104 — faceTowards turns
    /// without stepping; snapTo teleports; stop clears the destination and
    /// the auto-move flag.
    #[test]
    fn face_snap_stop_drive_the_player_directly() {
        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_result(1, 1);
        bot.tick(TICK_PERIOD);
        bot.stop();
        bot.snap_to(Vector3::new(0.0, 0.0, 0.0)); // undo the circle step

        bot.face_towards(Vector3::new(0.0, 0.0, -5.0));
        assert!((bot.yaw() - std::f32::consts::PI).abs() < 1e-5);
        assert_eq!(bot.position(), Vector3::new(0.0, 0.0, 0.0));

        // The direct yaw setter turns in place as well.
        bot.set_yaw(0.5);
        assert_eq!(bot.yaw(), 0.5);

        bot.snap_to(Vector3::new(2.0, 0.0, 3.0));
        assert_eq!(bot.position(), Vector3::new(2.0, 0.0, 3.0));

        bot.move_to(Vector3::new(9.0, 0.0, 9.0));
        assert!(bot.is_moving());
        bot.stop();
        assert!(!bot.is_moving());
        assert!(!bot.auto_move());
    }

    /// client_app.cpp:795-860 — every scripted face refuses while the bot
    /// is not in control of an in-world player, and the getters fall back
    /// to the zero defaults.
    #[test]
    fn an_offline_bot_refuses_the_script_faces() {
        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.move_to(Vector3::new(1.0, 1.0, 1.0));
        bot.face_towards(Vector3::new(1.0, 1.0, 1.0));
        bot.snap_to(Vector3::new(1.0, 1.0, 1.0));
        bot.set_position(Vector3::new(1.0, 1.0, 1.0));
        bot.set_yaw(1.0);
        assert!(!bot.is_moving());
        assert_eq!(bot.position(), Vector3::new(0.0, 0.0, 0.0));
        assert_eq!(bot.yaw(), 0.0);
        assert!(!bot.in_world());

        // Logging in is not enough: the player must be in the world.
        bot.feed_log_on_result(3, 1);
        bot.tick(TICK_PERIOD);
        assert!(bot.in_world());
        bot.snap_to(Vector3::new(1.0, 1.0, 1.0));
        assert_eq!(bot.position(), Vector3::new(1.0, 1.0, 1.0));
    }

    /// The acceptance face: the script's directives are applied before the
    /// movement step, so the tick that ran the script already advances the
    /// order it issued (client_app.cpp:456-496).
    #[test]
    fn the_script_entry_drives_the_movement() {
        struct ScriptBot {
            ticks: u32,
        }
        impl Behaviour for ScriptBot {
            fn on_tick(&mut self, _view: &BotView) -> Vec<Directive> {
                self.ticks += 1;
                match self.ticks {
                    1..=2 => vec![Directive::MoveTo(Vector3::new(6.0, 0.0, 0.0))],
                    3 => vec![Directive::FaceTowards(Vector3::new(0.0, 0.0, -6.0))],
                    4 => vec![Directive::Stop],
                    5 => vec![Directive::SnapTo(Vector3::new(1.0, 0.0, 0.0))],
                    6 => vec![Directive::SetAutoMove(true)],
                    _ => vec![Directive::Stop],
                }
            }
        }

        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_result(1, 1);
        bot.tick(TICK_PERIOD); // online
        bot.stop();
        bot.snap_to(Vector3::new(0.0, 0.0, 0.0)); // undo the first circle step

        bot.set_behaviour(Box::new(ScriptBot { ticks: 0 }));
        bot.tick(TICK_PERIOD);
        assert!(bot.is_moving());
        // The first 0.6 step toward (6,0,0) happened in the same tick.
        assert!((bot.position().x - 0.6).abs() < 1e-5);

        bot.tick(TICK_PERIOD);
        assert!((bot.position().x - 1.2).abs() < 1e-5);
        // The travel direction is +x, whose yaw is atan2(x, z) = π/2.
        assert!((bot.yaw() - MATH_PI / 2.0).abs() < 1e-5);

        // The face order turns the bot, but the same tick's pending move
        // then overwrites the yaw with the travel direction — directives
        // land before the movement step, exactly as the reference runs them.
        bot.tick(TICK_PERIOD);
        assert!((bot.position().x - 1.8).abs() < 1e-5);
        assert!((bot.yaw() - MATH_PI / 2.0).abs() < 1e-5);

        // The fourth tick stops the bot in place.
        bot.tick(TICK_PERIOD);
        assert!(!bot.is_moving());
        assert!(!bot.auto_move());

        // A snap teleports the idle player without any drift afterwards.
        bot.tick(TICK_PERIOD);
        assert!((bot.position().x - 1.0).abs() < 1e-5);

        // Auto-move rejoins the idle circle at radius 10 from anywhere.
        bot.tick(TICK_PERIOD);
        assert!(bot.auto_move());
        let p = bot.position();
        assert!((p.x * p.x + p.z * p.z - 100.0).abs() < 1e-2);

        // The final tick stops the bot for good.
        bot.tick(TICK_PERIOD);
        assert!(!bot.is_moving());
        assert!(!bot.auto_move());
    }

    /// client_app.cpp:1062-1090 + 1146-1183 — timers fire on the client
    /// clock, apply their directives, repeat from the current clock, and
    /// deleted ids are skipped; a destroyed bot refuses new timers.
    #[test]
    fn timers_fire_on_the_client_clock() {
        let mut bot = Bot::new("b", "p", UDP, "t");
        bot.feed_log_on_result(1, 1);
        bot.tick(TICK_PERIOD); // client clock 0.1

        let fired = std::rc::Rc::new(std::cell::RefCell::new(0));
        let fired_for_timer = fired.clone();
        let id = bot.add_timer(
            0.3,
            Box::new(move || {
                *fired_for_timer.borrow_mut() += 1;
                vec![Directive::SetSpeed(7.5)]
            }),
            true,
        );
        assert_eq!(id, 1);

        // A second, earlier, non-repeating timer; ordering two deadlines
        // is what drives the heap comparisons. A same-deadline twin makes
        // the two deadlines collide, exercising the heap's id tie-break.
        let once = std::rc::Rc::new(std::cell::RefCell::new(0));
        let once_for_timer = once.clone();
        let once_id = bot.add_timer(
            0.1,
            Box::new(move || {
                *once_for_timer.borrow_mut() += 1;
                vec![]
            }),
            false,
        );
        assert_eq!(once_id, 2);
        let tie = std::rc::Rc::new(std::cell::RefCell::new(0));
        let tie_for_timer = tie.clone();
        let tie_id = bot.add_timer(
            0.1,
            Box::new(move || {
                *tie_for_timer.borrow_mut() += 1;
                vec![]
            }),
            false,
        );
        assert_eq!(tie_id, 3);

        bot.tick(TICK_PERIOD); // 0.2 — the once timers fire and are gone
        assert_eq!(*once.borrow(), 1);
        assert_eq!(*tie.borrow(), 1);
        assert_eq!(*fired.borrow(), 0);
        bot.tick(TICK_PERIOD); // 0.3 — not due
        assert_eq!(*fired.borrow(), 0);
        bot.tick(TICK_PERIOD); // 0.4 — fires, restarts from 0.4
        assert_eq!(*fired.borrow(), 1);
        assert_eq!(bot.speed(), 7.5);
        assert_eq!(*once.borrow(), 1); // the once timer never came back

        bot.del_timer(id);
        bot.tick(TICK_PERIOD); // 0.5 — not due
        bot.tick(TICK_PERIOD); // 0.6 — not due
        bot.tick(TICK_PERIOD); // 0.7 — around the deadline
        bot.tick(TICK_PERIOD); // 0.8 — past it for sure: due but deleted
        assert_eq!(*fired.borrow(), 1);

        bot.destroy();
        assert_eq!(bot.add_timer(0.1, Box::new(Vec::new), false), -1);
    }
}
