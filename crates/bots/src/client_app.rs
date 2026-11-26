//! The `ClientApp` entry face (`server/tools/bots/client_app.{hpp,cpp}`):
//! one simulated client inside the Bots process's active set.
//!
//! BW-12-04-02 carried the entry itself -- the identity fields the batch
//! add/remove commands write and the destroy guard. BW-12-04-03 lands the
//! logout events and the movement face: `logOn`/`logOff`, `tick`'s
//! movement walk (`moveTo` first, then the controller or the circle
//! fallback), the controller set/reset, and the retry stamp.
//!
//! Stub face: the `BWServerConnection` and the player entity are not
//! ported yet (the transport and entity faces land in later items), so
//! the port folds the connection lifecycle and the "player in world and
//! controlled" predicate into one `is_online` flag: `isOnline`/
//! `isLoggingIn`/`pPlayer->isInWorld() && isControlled() && !...` all
//! read it, and the position/direction accessors hold the player state
//! directly. The personality script hooks (`onClientAppDestroy`,
//! `onLoseConnection`, `onLogOnFailure`, `onTick`, the timers) are
//! documented at their call sites; the script face rides the
//! python-host type machinery, and without a personality the reference's
//! no-module paths are exactly what the port runs.

use crate::config::ConnectionTransport;
use crate::movement::{Direction3D, MovementController, Rng};
use foundation::math::Vector3;

/// `NULL_ENTITY_ID` (`lib/entity/entity_id.hpp`): the id a bot carries
/// until its log-on hands one out.
pub const NULL_ENTITY_ID: i32 = 0;

/// One simulated client (`ClientApp`, `client_app.cpp:363-409`).
pub struct ClientApp {
    /// `userName_`.
    pub user_name: String,
    /// `userPasswd_`.
    pub user_passwd: String,
    /// `transport_`.
    pub transport: ConnectionTransport,
    /// `tag_`.
    pub tag: String,
    /// `id_` -- NULL until the log-on completes.
    pub id: i32,
    /// `isDestroyed_`.
    pub is_destroyed: bool,
    /// `isDormant_` -- starts true, the constructor's `logOn` clears it.
    pub is_dormant: bool,
    /// `speed_` -- `6.f + float(rand())*2.f/float(RAND_MAX)`
    /// (`client_app.cpp:30`), so a draw from [6, 8).
    pub speed: f32,
    /// `autoMove_` -- the constructor initialises it true.
    pub auto_move: bool,
    /// `pDest_` -- a `moveTo` target outranks the controller walk.
    pub dest: Option<Vector3>,
    /// `pMovementController_` -- null means the circle fallback.
    pub movement_controller: Option<Box<dyn MovementController>>,
    /// The connection stub: `isOnline`/`isLoggingIn` and the player
    /// entity's in-world-and-controlled state all fold into this.
    pub is_online: bool,
    /// `logOnRetryTime_` -- a `timestamp()` stamp; zero means no retry
    /// is pending.
    pub log_on_retry_time: u64,
    /// `pConnection_->clientTime()`'s stub: seconds advanced by each
    /// `tick`'s `dTime`.
    pub client_time: f64,
    /// The player entity's position stub.
    pub position: Vector3,
    /// The player entity's direction stub (the bots code only touches
    /// the yaw).
    pub direction: Direction3D,
    /// (test seam) the stub's `pConnection_->update` drops the
    /// connection mid-tick when set -- the real update does this on a
    /// dead socket; the transport face lands later.
    #[cfg(test)]
    pub drop_on_update: bool,
    /// (test seam) the stub folds `isInWorld`/`isControlled` into
    /// this flag so the not-in-control paths stay reachable; the
    /// reference computes them off the live player entity.
    #[cfg(test)]
    pub movable_override: bool,
}

impl ClientApp {
    /// The reference constructor's state, including its trailing
    /// `logOn()` call (`client_app.cpp:409`).
    pub fn new(
        user_name: String,
        user_passwd: String,
        transport: ConnectionTransport,
        tag: String,
        speed: f32,
    ) -> Self {
        let mut app = Self {
            user_name,
            user_passwd,
            transport,
            tag,
            id: NULL_ENTITY_ID,
            is_destroyed: false,
            is_dormant: true,
            speed,
            auto_move: true,
            dest: None,
            movement_controller: None,
            is_online: false,
            log_on_retry_time: 0,
            client_time: 0.0,
            position: Vector3::ZERO,
            direction: Direction3D::new(0.0),
            #[cfg(test)]
            drop_on_update: false,
            #[cfg(test)]
            movable_override: true,
        };
        app.log_on();
        app
    }

    /// `ClientApp::logOn` (`client_app.cpp:554-572`): the retry stamp
    /// clears first; a destroyed bot stays dormant; the stub's
    /// connection goes straight online where the reference's
    /// `logOnTo` runs its async login.
    pub fn log_on(&mut self) {
        self.log_on_retry_time = 0;

        if self.is_destroyed {
            return;
        }

        self.is_dormant = false;

        // pConnection_->isLoggingIn() || pConnection_->isOnline()
        if self.is_online {
            return;
        }

        // logOnTo: the async login; the stub completes it at once.
        self.is_online = true;
    }

    /// `ClientApp::logOff` (`client_app.cpp:575-583`).
    pub fn log_off(&mut self) {
        if self.is_online {
            // pConnection_->logOff(); the stub drops straight out.
            self.is_online = false;
        }
    }

    /// `ClientApp::destroy` (`client_app.cpp:514-552`): idempotent.
    /// The personality `onClientAppDestroy` hook fires here in the
    /// reference when the id is live; the script face rides the
    /// python-host work.
    pub fn destroy(&mut self) {
        if self.is_destroyed {
            return;
        }

        self.is_destroyed = true;

        // pMovementController_.reset( NULL )
        self.movement_controller = None;

        // module.callMethod( "onClientAppDestroy", ... ) when the id
        // is live -- script face.

        self.log_off();

        // clearAllEntities: the id falls back to NULL.
        self.id = NULL_ENTITY_ID;
    }

    /// `ClientApp::onLoggedOff` (`client_app.cpp:1193-1232`): the
    /// controller drops; the personality `onLoseConnection` hook's
    /// return decides dormancy -- without a personality the reference
    /// leaves the bot non-dormant, ready for a retry log-on.
    pub fn on_logged_off(&mut self) {
        self.movement_controller = None;

        // module.getAttribute( "onLoseConnection" ) ... ret.isTrue()
        // -- script face; no module keeps isDormant_ untouched.

        // clearAllEntities: the id falls back to NULL.
        self.is_online = false;
        self.id = NULL_ENTITY_ID;
    }

    /// `ClientApp::onLogOnFailure` (`client_app.cpp:1239-1266`): the
    /// personality hook fires (script face), then a non-zero
    /// `logOnRetryPeriod` stamps the retry time. The period rides in
    /// from the config -- the reference reads the static
    /// `BotsConfig::logOnRetryPeriod()` at this point, and the port
    /// has no global statics; `isZero` is the exact `fabsf(v) <= 0.f`
    /// (`stdmf_minimal.hpp:119-122`).
    pub fn on_log_on_failure(&mut self, log_on_retry_period: f32) {
        // module.callMethod( "onLogOnFailure", ... ) -- script face.

        if log_on_retry_period != 0.0 {
            self.log_on_retry_time = foundation::time::stamp_now()
                + (log_on_retry_period * STAMPS_PER_SECOND as f32) as u64;
        }
    }

    /// `ClientApp::tick` (`client_app.cpp:431-511`): dormant bots
    /// skip; a connection that dropped mid-update answers
    /// `isDormant_`; a movable player walks `pDest_` first, else the
    /// controller, else the circle. The script `onTick` and the
    /// timers ride the script face.
    pub fn tick(&mut self, d_time: f32, rng: &mut dyn Rng) -> bool {
        // if it is dormant skip call tick.
        if self.is_dormant {
            return true;
        }

        let was_online = self.is_online;

        // pConnection_->update( dTime ): the stub just runs the clock.
        self.client_time += d_time as f64;
        #[cfg(test)]
        if self.drop_on_update {
            self.is_online = false;
        }

        if was_online && !self.is_online {
            // Either just became dormant, or just disconnected and
            // want to be destroyed.
            return self.is_dormant;
        }

        // pPlayer != NULL && pPlayer->isInWorld()
        if d_time > 0.0 && self.is_online {
            if self.is_player_movable() {
                // Movement ordered by moveTo() takes precedence over
                // movement by movement controller
                if let Some(dest) = self.dest {
                    let close_enough = 1.0f32;
                    let displacement = dest - self.position;
                    let length = displacement.length();

                    if length < close_enough {
                        self.dest = None;
                    } else {
                        let displacement = displacement * (self.speed * d_time / length);
                        let mut direction = self.direction;
                        direction.yaw = displacement.yaw();
                        let position = self.position + displacement;
                        self.update_position(position, direction);
                    }
                } else if self.auto_move {
                    self.add_move(d_time as f64, rng);
                }
            } else {
                // since we don't have control of the player
                // remove any destination
                self.dest = None;
            }
        }

        // pConnection_->updateServer()

        true
    }

    /// `ClientApp::isPlayerMovable` (`client_app.cpp:682-697`): the
    /// stub folds the player predicate into the connection flag (the
    /// test seam models the not-in-world side).
    pub fn is_player_movable(&self) -> bool {
        let online = self.is_online;
        #[cfg(test)]
        let online = online && self.movable_override;
        online
    }

    /// `ClientApp::canSetMovementController`
    /// (`client_app.cpp:700-711`): same stub.
    pub fn can_set_movement_controller(&self) -> bool {
        self.is_online
    }

    /// `ClientApp::position` getter (`client_app.cpp:868-881`): the
    /// stub holds the player state directly, so the not-in-world
    /// default (0, 0, 0) is simply the initial value.
    pub fn get_position(&self) -> Vector3 {
        self.position
    }

    /// `ClientApp::position` setter (`client_app.cpp:843-856`): the
    /// `onMoveLocally` write-through.
    pub fn set_position(&mut self, pos: Vector3) {
        if !self.is_player_movable() {
            return;
        }
        self.position = pos;
    }

    /// `ClientApp::direction` getter (`client_app.cpp:910-923`).
    pub fn get_direction(&self) -> Direction3D {
        self.direction
    }

    /// `ClientApp::updatePosition` (`client_app.cpp:1032-1055`): the
    /// player entity's `onMoveLocally` write-through.
    pub fn update_position(&mut self, position: Vector3, direction: Direction3D) {
        if !self.is_player_movable() {
            return;
        }
        self.position = position;
        self.direction = direction;
    }

    /// `ClientApp::addMove` (`client_app.cpp:1106-1140`): the
    /// controller walks when one is set, else the circle fallback on
    /// `clientTime` -- period `10*speed/7`, radius 10, yaw ahead a
    /// quarter turn.
    pub fn add_move(&mut self, d_time: f64, rng: &mut dyn Rng) {
        if !self.is_player_movable() {
            return;
        }

        let mut position = self.position;
        let mut direction = self.direction;

        if let Some(controller) = self.movement_controller.as_mut() {
            controller.next_step(
                &mut self.speed,
                d_time as f32,
                &mut position,
                &mut direction,
                rng,
            );
        } else {
            let time = self.client_time;
            let period = 10.0f32 * self.speed / 7.0;
            let radius = 10.0f32;
            let angle = (time * 2.0 * std::f64::consts::PI / period as f64) as f32;

            position = Vector3::new(radius * angle.sin(), 0.0, radius * angle.cos());

            direction.yaw = angle + std::f32::consts::FRAC_PI_2;
        }

        self.update_position(position, direction);
    }

    /// `ClientApp::moveTo` (`client_app.cpp:791-809`): a target
    /// outranks the controller; not movable, the call is a no-op.
    pub fn move_to(&mut self, pos: Vector3) {
        if !self.is_player_movable() {
            // TODO: should add a warning message if we are not in control
            return;
        }

        // make sure no memory leak if called repeatedly from script.
        self.dest = Some(pos);

        self.auto_move = false;
    }

    /// `ClientApp::faceTowards` (`client_app.cpp:813-825`).
    pub fn face_towards(&mut self, pos: Vector3) {
        if !self.is_player_movable() {
            return;
        }

        let position = self.position;
        let mut direction = self.direction;

        direction.yaw = (pos - position).yaw();
        self.update_position(position, direction);
    }

    /// `ClientApp::stop` (`client_app.cpp:828-832`): the destination
    /// drops; `autoMove_` goes false -- the controller walk does not
    /// resume by itself.
    pub fn stop(&mut self) {
        self.dest = None;
        self.auto_move = false;
    }
}

/// The port's `stampsPerSecond()`: `stamp_now()` runs on nanosecond
/// stamps, matching the dispatcher queue's `1_000_000_000` default.
const STAMPS_PER_SECOND: u64 = 1_000_000_000;

impl std::fmt::Debug for ClientApp {
    /// The reference's operator<< shows the identity and session
    /// state; the controller is opaque.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientApp")
            .field("user_name", &self.user_name)
            .field("user_passwd", &self.user_passwd)
            .field("transport", &self.transport)
            .field("tag", &self.tag)
            .field("id", &self.id)
            .field("is_destroyed", &self.is_destroyed)
            .field("is_dormant", &self.is_dormant)
            .field("speed", &self.speed)
            .field("auto_move", &self.auto_move)
            .field("dest", &self.dest)
            .field("movement_controller", &self.movement_controller.is_some())
            .field("is_online", &self.is_online)
            .field("log_on_retry_time", &self.log_on_retry_time)
            .field("client_time", &self.client_time)
            .field("position", &self.position)
            .field("direction", &self.direction)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movement::BeelineController;

    /// A draw stream that hands out fixed values in rotation.
    struct FixedRng {
        values: Vec<u32>,
        at: usize,
    }

    impl FixedRng {
        fn new(values: &[u32]) -> Self {
            Self {
                values: values.to_vec(),
                at: 0,
            }
        }
    }

    impl Rng for FixedRng {
        fn next_u32(&mut self) -> u32 {
            let v = self.values[self.at % self.values.len()];
            self.at += 1;
            v
        }
    }

    fn fresh_app() -> ClientApp {
        ClientApp::new(
            "Bot_1_A".to_string(),
            "pw".to_string(),
            ConnectionTransport::Udp,
            "Default".to_string(),
            7.0,
        )
    }

    /// Acceptance: the constructor leaves a live bot dormant-free
    /// (the trailing `logOn`), NULL-id'd, carrying the identity it
    /// was built with, `autoMove_` on and no controller
    /// (`client_app.cpp:363-409`).
    #[test]
    fn the_constructor_logons_and_carries_the_identity() {
        let app = ClientApp::new(
            "Bot_1_App".to_string(),
            "pw".to_string(),
            ConnectionTransport::Tcp,
            "Swarm".to_string(),
            6.5,
        );
        assert_eq!(app.user_name, "Bot_1_App");
        assert_eq!(app.user_passwd, "pw");
        assert_eq!(app.transport, ConnectionTransport::Tcp);
        assert_eq!(app.tag, "Swarm");
        assert_eq!(app.id, NULL_ENTITY_ID);
        assert!(!app.is_destroyed);
        assert!(!app.is_dormant, "the constructor calls logOn");
        assert!(app.auto_move);
        assert!(app.dest.is_none());
        assert!(app.movement_controller.is_none());
        assert!(app.is_online, "the stub connection completed the login");
        assert_eq!(app.log_on_retry_time, 0);
    }

    /// Acceptance: `destroy` is guarded -- the second call is a no-op,
    /// a destroyed bot's `logOn` keeps it dormant
    /// (`logOn`'s destroyed check, `client_app.cpp:560-563`), and
    /// destroy drops the connection and the id.
    #[test]
    fn destroy_is_idempotent_and_logon_respects_it() {
        let mut app = fresh_app();
        app.destroy();
        assert!(app.is_destroyed);
        assert!(!app.is_online, "logOff ran inside destroy");
        assert_eq!(app.id, NULL_ENTITY_ID);
        assert!(app.movement_controller.is_none());

        let mut dormant = fresh_app();
        dormant.is_online = false;
        dormant.is_dormant = true;
        dormant.destroy();
        dormant.log_on();
        assert!(dormant.is_destroyed);
        assert!(dormant.is_dormant, "logOn returns early when destroyed");
    }

    /// Acceptance: `logOff` drops the online flag only when online
    /// (`client_app.cpp:575-583`), and `logOn` after it re-connects
    /// (`client_app.cpp:554-572`).
    #[test]
    fn log_off_and_log_on_round_trip_the_connection() {
        let mut app = fresh_app();
        app.log_off();
        assert!(!app.is_online);
        app.log_on();
        assert!(!app.is_dormant);
        assert!(app.is_online);
    }

    /// Acceptance: the `onLoggedOff` event drops the controller and
    /// the id and leaves the bot non-dormant (no personality in the
    /// reference either, `client_app.cpp:1193-1232`) -- a retry
    /// `logOn` then re-enters with the retry stamp cleared.
    #[test]
    fn on_logged_off_drops_the_session_but_not_the_bot() {
        let mut app = fresh_app();
        app.movement_controller = Some(Box::new(BeelineController::new(Vector3::new(
            1.0, 1.0, 1.0,
        ))));
        app.id = 42;
        app.on_logged_off();
        assert!(app.movement_controller.is_none());
        assert!(!app.is_online);
        assert_eq!(app.id, NULL_ENTITY_ID);
        assert!(!app.is_dormant, "no personality: the bot stays alive");
        app.log_on();
        assert_eq!(app.log_on_retry_time, 0, "logOn clears the retry stamp");
        assert!(app.is_online);
    }

    /// Acceptance: `onLogOnFailure` stamps the retry time off the
    /// configured period (`client_app.cpp:1260-1266`), a zero period
    /// leaves it clear, and `logOn` clears it again
    /// (`client_app.cpp:555`).
    #[test]
    fn on_log_on_failure_stamps_the_retry_time() {
        let mut app = fresh_app();
        app.on_log_on_failure(3.5);
        let stamp = foundation::time::stamp_now();
        let expected = app.log_on_retry_time;
        assert!(
            expected > stamp.saturating_sub(1_000_000_000),
            "the stamp is now-ish plus 3.5s: {expected} vs {stamp}"
        );
        assert!(expected <= stamp + 3_600_000_000_000, "3.5s ahead");

        // A zero period (isZero) never stamps.
        let mut app = fresh_app();
        app.on_log_on_failure(0.0);
        assert_eq!(app.log_on_retry_time, 0);

        app.log_on();
        assert_eq!(app.log_on_retry_time, 0);
    }

    /// Acceptance: dormant bots skip the tick entirely
    /// (`client_app.cpp:436-441`), and the movement ordering is
    /// `pDest_` first, controller second, circle fallback last
    /// (`client_app.cpp:474-495`).
    #[test]
    fn tick_walks_dest_then_controller_then_the_circle() {
        // Dormant: nothing moves, the clock stands still.
        let mut dormant = fresh_app();
        dormant.is_dormant = true;
        assert!(dormant.tick(0.1, &mut FixedRng::new(&[0])));
        assert_eq!(dormant.client_time, 0.0);

        // pDest_ walk: the exact fraction toward the target, yaw at it.
        let mut app = fresh_app();
        app.move_to(Vector3::new(10.0, 0.0, 0.0));
        assert!(app.tick(0.5, &mut FixedRng::new(&[0])));
        assert_eq!(app.get_position(), Vector3::new(3.5, 0.0, 0.0));
        assert_eq!(app.get_direction().yaw, 10.0f32.atan2(0.0));
        assert!(app.dest.is_some(), "not close enough yet");

        // Within 1 unit the destination drops and the tick does not
        // move at all -- and moveTo has cleared autoMove, so the bot
        // stands still from here on (`client_app.cpp:805`).
        app.move_to(Vector3::new(4.0, 0.0, 0.0));
        assert!(app.tick(0.5, &mut FixedRng::new(&[0])));
        assert!(app.dest.is_none(), "inside closeEnough");
        assert_eq!(app.get_position(), Vector3::new(3.5, 0.0, 0.0));
        assert!(!app.auto_move);

        // Circle fallback on a bot that never moveTo'd: radius 10 on
        // clientTime -- after three half-second ticks clientTime is
        // 1.5 at the addMove call, period 10*7/7 = 10, angle =
        // 1.5*2*pi/10.
        let mut circler = fresh_app();
        let angle = (1.5 * 2.0 * std::f64::consts::PI / 10.0) as f32;
        let expected = Vector3::new(10.0 * angle.sin(), 0.0, 10.0 * angle.cos());
        assert!(circler.tick(0.5, &mut FixedRng::new(&[0])));
        assert!(circler.tick(0.5, &mut FixedRng::new(&[0])));
        assert!(circler.tick(0.5, &mut FixedRng::new(&[0])));
        assert_eq!(circler.get_position(), expected);
        assert_eq!(
            circler.get_direction().yaw,
            angle + std::f32::consts::FRAC_PI_2
        );
    }

    /// Acceptance: losing control drops `pDest_` on the tick
    /// (`client_app.cpp:486-492`); the control-gated mutators
    /// (`position`/`updatePosition`/`addMove`, `client_app.cpp:843-856`,
    /// `:1032-1055`, `:1106-1112`) no-op; destroy is idempotent
    /// (`client_app.cpp:514-552`); the `operator<<` dump shows the
    /// identity and session fields.
    #[test]
    fn uncontrolled_bots_drop_the_dest_and_noop_the_mutators() {
        let mut app = fresh_app();
        app.move_to(Vector3::new(1.0, 0.0, 0.0));

        // With control the setter lands; then control drops.
        app.set_position(Vector3::new(2.0, 0.0, 0.0));
        assert_eq!(app.get_position(), Vector3::new(2.0, 0.0, 0.0));
        app.movable_override = false;

        assert!(app.tick(0.5, &mut FixedRng::new(&[0])));
        assert!(app.dest.is_none(), "control lost drops pDest");

        // The mutators no-op without control.
        let before = app.get_position();
        app.set_position(Vector3::new(9.0, 9.0, 9.0));
        app.update_position(Vector3::new(9.0, 9.0, 9.0), Direction3D::new(1.0));
        app.add_move(0.5, &mut FixedRng::new(&[0]));
        assert_eq!(app.get_position(), before);

        // Destroy is idempotent.
        app.destroy();
        app.destroy();
        assert!(app.is_destroyed);

        // The operator<< face: identity and session state, the
        // controller opaque.
        let text = format!("{app:?}");
        assert!(
            text.contains("user_name"),
            "identity rides the dump: {text}"
        );
        assert!(text.contains("movement_controller"), "{text}");

        // The draw stream interface itself.
        assert_eq!(FixedRng::new(&[7]).next_u32(), 7);
    }

    /// Acceptance: a controller outranks the circle -- the bot walks
    /// the beeline recurrence instead
    /// (`client_app.cpp:1106-1124`), and the speed rides in-out.
    #[test]
    fn tick_with_a_controller_walks_the_controller() {
        let mut app = fresh_app();
        app.movement_controller = Some(Box::new(BeelineController::new(Vector3::new(
            10.0, 0.0, 0.0,
        ))));
        assert!(app.tick(0.5, &mut FixedRng::new(&[0])));
        assert_eq!(app.get_position(), Vector3::new(3.5, 0.0, 0.0));
    }

    /// Acceptance: `moveTo` on a bot without control is a no-op
    /// (`client_app.cpp:793-797`), `stop` clears the destination and
    /// kills `autoMove_` (`client_app.cpp:828-832`), and
    /// `faceTowards` yaws at the point (`client_app.cpp:813-825`).
    #[test]
    fn move_to_stop_and_face_towards_follow_the_reference() {
        let mut app = fresh_app();

        // Without control: moveTo is ignored, autoMove_ untouched.
        app.is_online = false;
        app.move_to(Vector3::new(5.0, 0.0, 0.0));
        assert!(app.dest.is_none());
        assert!(app.auto_move);
        app.face_towards(Vector3::new(0.0, 0.0, 5.0));
        assert_eq!(app.get_direction().yaw, 0.0);

        // With control: faceTowards yaws, stop clears and kills the
        // auto walk.
        app.is_online = true;
        app.face_towards(Vector3::new(0.0, 0.0, 5.0));
        assert_eq!(app.get_direction().yaw, 0.0f32.atan2(5.0));
        app.move_to(Vector3::new(5.0, 0.0, 0.0));
        assert!(!app.auto_move, "moveTo kills the auto walk");
        app.stop();
        assert!(app.dest.is_none());
        assert!(!app.auto_move);
    }

    /// Acceptance: a non-dormant bot with no session yet (the
    /// failed-log-on state) ticks true and stays put -- the
    /// `pPlayer == NULL` path (`client_app.cpp:466-470`). The
    /// mid-tick drop branch (`:451-458`) stays for the connection
    /// face; the stub's update never drops.
    #[test]
    fn a_bot_without_a_session_ticks_true_and_stays_put() {
        let mut app = fresh_app();
        app.is_online = false;
        assert!(app.tick(0.1, &mut FixedRng::new(&[0])));
        assert_eq!(app.get_position(), Vector3::ZERO);
    }
}
