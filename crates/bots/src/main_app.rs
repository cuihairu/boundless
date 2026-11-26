//! `MainApp` (`server/tools/bots/main_app.{hpp,cpp}`): the Bots tool
//! application. BW-12-04-02 carried the batch add/remove face -- the
//! active set (`bots_`) and the commands that mutate it.
//! BW-12-04-03 lands the controller factory, `updateMovement`, the
//! game tick slice (`handleGameTickTimeSlice`), the log-on retry
//! queue and the slow-add timer.
//!
//! Later items land the rest: the watcher command wiring rides
//! BW-12-01-04's protocol-2 stream decode; the CPython script face
//! (`BigWorld.addBots` &c, the `PyBots` mapping type) rides the
//! python-host type machinery -- the typed Rust backends
//! (`findApp`/`appsKeys`/`appsValues`/`appsItems`, the module
//! functions) are here.
//!
//! Reference translations:
//! - `bots_` is a `BW::list` walked with iterators; the port keeps a
//!   `VecDeque` plus a slot index for `clientTickIndex_`, preserving
//!   the cursor's survived-erasure semantics (`main_app.cpp:508-512`,
//!   `:558-562`): a cursor sitting on an erased slot rides forward to
//!   the element that follows it;
//! - the function statics `inTick` and `remainder` in
//!   `handleGameTickTimeSlice` (`main_app.cpp:596,623`) become fields
//!   -- one Bots process per `MainApp`;
//! - `MainApp::instance()` calls out of `BotAdder::handleTimeout`
//!   (`main_app.cpp:1221`) become a pending-adds queue the app
//!   drains right after the timer pass -- single-threaded, same net
//!   effect;
//! - `advanceTime()` is the cstdmf stamp clock's bookkeeping, which
//!   `foundation::time::stamp_now` keeps live, so the call is a
//!   no-op here; `condemnedInterfaces_.processOnce()` rides the
//!   mercury face (BW-12-01 lane).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use foundation::math::Vector3;
use foundation::time_queue::{DispatcherTimers, TimerHandle, TimerHandler};
use resources::bwresource::BwResource;

use crate::client_app::{ClientApp, NULL_ENTITY_ID};
use crate::config::BotsConfig;
use crate::movement::{
    Lcg, MovementController, MovementError, MovementErrorKind, MovementFactories, Rng,
};

/// `FD_PER_BOT` (`main_app.cpp:74`): the estimated fd spend of one
/// bot, kept safely over the real figure for the epoll-based
/// connection face.
pub const FD_PER_BOT: i32 = 10;

/// `TICK_FRAGMENTS` (`main_app.hpp`): how many slices a full sweep
/// over `bots_` is split into.
pub const TICK_FRAGMENTS: i32 = 1;

/// `TICK_TIMEOUT` (`main_app.hpp`): the game-tick timer period in
/// microseconds.
pub const TICK_TIMEOUT: i64 = 100000;

/// `TICK_PERIOD` (`main_app.hpp`): the `dTime` each bot is ticked
/// with.
pub const TICK_PERIOD: f32 = 0.1;

/// `MINI_TICK_PERIOD` (`main_app.hpp`): the `localTime_` advance per
/// slice.
pub const MINI_TICK_PERIOD: f64 = 0.1;

/// `MainApp` (`main_app.cpp:119-142` initialisers).
pub struct MainApp {
    /// The tool's option statics; the reference reads the
    /// `BotsConfig` statics directly (`main_app.cpp:403-405`).
    config: BotsConfig,
    /// `bots_` -- the active set, front = oldest (`push_back`,
    /// `main_app.cpp:394`).
    bots: VecDeque<ClientApp>,
    /// `appID_` -- "serverName:port", built in `init`
    /// (`main_app.cpp:258-264`).
    app_id: String,
    /// `nextBotID_` -- pre-incremented into each random name
    /// (`main_app.cpp:410`).
    next_bot_id: i32,
    /// `clientTickIndex_` -- the tick rotation cursor.
    client_tick_index: usize,
    /// The `rand()` stream the speeds and the controllers draw from
    /// (seeded like `srand( timestamp() )`, `main_app.cpp:141`).
    rng: Lcg,
    /// `s_pMovementFactories` (`main_app.cpp:734`) -- pre-populated
    /// with the three built-ins.
    factories: MovementFactories,
    /// The `BWResource::instance()` stand-in the factories load
    /// graphs through.
    resource: BwResource,
    /// `handleGameTickTimeSlice`'s `static bool inTick`
    /// (`main_app.cpp:596`).
    in_tick: bool,
    /// The slice's `static int remainder` (`main_app.cpp:623`).
    remainder: i32,
    /// `localTime_` -- `MINI_TICK_PERIOD` per slice.
    local_time: f64,
    /// `botsForLogOnRetry_` -- bots waiting for their retry stamp.
    bots_for_log_on_retry: Vec<ClientApp>,
    /// `mainDispatcher()`'s timer queue.
    main_timers: DispatcherTimers,
    /// The `BotAdder` bridge: its callbacks queue the batch sizes and
    /// the app drains them after the timer pass.
    pending_bot_adds: Rc<RefCell<Vec<i32>>>,
}

/// `ServerApp::numFileDescriptorsOpen` (`server_app.cpp:836-863`):
/// procfs holds one symlink per open fd; the listing counts "." and
/// ".." padding and is itself an open fd, so the reference starts at
/// -2 and the port counts real entries minus the reader. (These two
/// helpers stay here until a second app needs them; then they move
/// into the `ServerApp` port.)
pub fn num_file_descriptors_open() -> i32 {
    match std::fs::read_dir("/proc/self/fd") {
        Ok(entries) => entries.count() as i32 - 1,
        // The reference's tag says numFileDescriptorsLimit -- a
        // copy-paste slip preserved as-is.
        Err(err) => {
            eprintln!("ServerApp::numFileDescriptorsLimit: Error opening /proc/self/fd/: {err}.");
            -1
        }
    }
}

/// `ServerApp::fileDescriptorLimit` (`server_app.cpp:816-833`):
/// `getrlimit(RLIMIT_NOFILE)`'s soft limit, -1 on failure.
pub fn file_descriptor_limit() -> i32 {
    unsafe {
        let mut nofile = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut nofile) == -1 {
            // The reference's tag says raiseFileDescriptorLimit -- a
            // copy-paste slip preserved as-is.
            eprintln!(
                "ServerApp::raiseFileDescriptorLimit: Error checking limit of maximum file descriptor: {}.",
                std::io::Error::last_os_error()
            );
            return -1;
        }
        nofile.rlim_cur as i32
    }
}

impl MainApp {
    /// `MainApp( ... )` with the wallclock seed
    /// (`srand( timestamp() )`, `main_app.cpp:141`).
    pub fn new(config: BotsConfig, app_id: String) -> Self {
        let seed = unsafe { libc::time(std::ptr::null_mut()) } as u64;
        Self::with_seed(config, app_id, seed)
    }

    /// The constructor with an explicit seed, for reproducible
    /// active sets.
    pub fn with_seed(config: BotsConfig, app_id: String, seed: u64) -> Self {
        Self {
            config,
            bots: VecDeque::new(),
            app_id,
            next_bot_id: 0,
            client_tick_index: 0,
            rng: Lcg::new(seed),
            factories: MovementFactories::new(),
            resource: BwResource::new(),
            in_tick: false,
            remainder: 0,
            local_time: 0.0,
            bots_for_log_on_retry: Vec::new(),
            main_timers: DispatcherTimers::new(),
            pending_bot_adds: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// The speed draw: `6 + rand()*2/RAND_MAX`
    /// (`client_app.cpp:30`) -- one draw off the process stream the
    /// controllers also use.
    fn next_speed(&mut self) -> f32 {
        6.0 + self.rng.unit_rand() * 2.0
    }

    /// The active set, front = oldest.
    pub fn bots(&self) -> &VecDeque<ClientApp> {
        &self.bots
    }

    /// `MF_WATCH( "numBots", bots_, &Bots::size )`
    /// (`main_app.cpp:1059`).
    pub fn num_bots(&self) -> usize {
        self.bots.len()
    }

    /// The tick rotation cursor, for the tick loop.
    pub fn client_tick_index(&self) -> usize {
        self.client_tick_index
    }

    /// The option statics -- the `MF_WATCH( "tag", ... )` runtime
    /// knob (`main_app.cpp:1055`) is a live write into them.
    pub fn config_mut(&mut self) -> &mut BotsConfig {
        &mut self.config
    }

    /// `MainApp::addBot` (`main_app.cpp:401-418`): one bot off the
    /// current defaults; `shouldUseRandomName` appends
    /// `_` + pre-incremented id + `_` + appID.
    pub fn add_bot(&mut self) {
        let mut bname = self.config.username.clone();
        let bpass = self.config.password.clone();
        if self.config.should_use_random_name {
            self.next_bot_id += 1;
            bname.push_str(&format!("_{}_{}", self.next_bot_id, self.app_id));
        }
        self.add_bot_with_name(&bname, &bpass);
    }

    /// `MainApp::addBots` (`main_app.cpp:424-430`).
    pub fn add_bots(&mut self, num: i32) {
        for _ in 0..num {
            self.add_bot();
        }
    }

    /// `MainApp::addBotWithName` (`main_app.cpp:376-395`): the fd
    /// head-room guard first, then the entry at the set's back.
    pub fn add_bot_with_name(&mut self, name: &str, password: &str) {
        let fd_open = num_file_descriptors_open();
        let fd_limit = file_descriptor_limit();
        self.add_bot_with_name_checked(name, password, fd_open, fd_limit);
    }

    /// The guard with the fd numbers injected, so tests can pin the
    /// boundary without touching the process limit.
    fn add_bot_with_name_checked(
        &mut self,
        name: &str,
        password: &str,
        fd_open: i32,
        fd_limit: i32,
    ) {
        if (fd_open + FD_PER_BOT) > fd_limit {
            eprintln!(
                "MainApp::addBotWithName: Failed to add {}. File \
                 descriptors limit will be exceeded: per bot={}, \
                 opened={}, limit={}",
                name, FD_PER_BOT, fd_open, fd_limit
            );
            return;
        }
        let speed = self.next_speed();
        self.bots.push_back(ClientApp::new(
            name.to_string(),
            password.to_string(),
            self.config.transport,
            self.config.tag.clone(),
            speed,
        ));
    }

    /// `MainApp::addBotsWithName` (`main_app.cpp:441-498`) over
    /// validated pairs. The PyObjectPtr overload's sequence/tuple
    /// validation raises the TypeErrors on the script face
    /// (BW-12-04-03); the batch face over pairs is here.
    pub fn add_bots_with_name(&mut self, credentials: &[(&str, &str)]) {
        for (name, password) in credentials {
            self.add_bot_with_name(name, password);
        }
    }

    /// `MainApp::delBots` (`main_app.cpp:504-516`): consume from the
    /// front, destroying each. The cursor translation: the reference
    /// bumps `clientTickIndex_` when it sits on the front, then pops;
    /// in slot terms every index steps down one, and a cursor on the
    /// front stays put -- its slot now holds the following element.
    pub fn del_bots(&mut self, num: i32) {
        let mut num = num;
        while num > 0 && !self.bots.is_empty() {
            num -= 1;
            if self.client_tick_index > 0 {
                self.client_tick_index -= 1;
            }
            let front = self.bots.front_mut().expect("loop guard holds");
            front.destroy();
            self.bots.pop_front();
        }
    }

    /// `MainApp::delTaggedEntities` (`main_app.cpp:549-567`): erase
    /// every entry whose tag matches, forward walk. A cursor sitting
    /// on a condemned slot rides the erase to the following element.
    pub fn del_tagged_entities(&mut self, tag: &str) {
        let mut index = 0;
        while index < self.bots.len() {
            if self.bots[index].tag == tag {
                if self.client_tick_index > index {
                    self.client_tick_index -= 1;
                }
                self.bots.remove(index);
            } else {
                index += 1;
            }
        }
    }

    /// The resource instance stand-in -- the factories' graph caches
    /// load through it.
    pub fn resource_mut(&mut self) -> &mut BwResource {
        &mut self.resource
    }

    /// `MainApp::localTime()`.
    pub fn local_time(&self) -> f64 {
        self.local_time
    }

    /// `MainApp::createDefaultMovementController`
    /// (`main_app.cpp:744-749`): the config's controller strings.
    pub fn create_default_movement_controller(
        &mut self,
        speed: &mut f32,
        position: &mut Vector3,
        rng: &mut dyn Rng,
    ) -> Result<Option<Box<dyn MovementController>>, MovementError> {
        let Self {
            config,
            factories,
            resource,
            ..
        } = self;
        create_movement_controller(
            factories,
            resource,
            &config.controller_type,
            &config.controller_data,
            speed,
            position,
            rng,
        )
    }

    /// `ClientApp::setDefaultMovementController` over this app's
    /// registry (`client_app.cpp:733-755`): the canSet guard, the
    /// create, the position writeback (even on failure), then the
    /// error gate. The bot is addressed by its slot -- the reference's
    /// `ClientApp` method reaches `MainApp::instance()` for the
    /// factories, which the port cannot alias.
    pub fn set_default_movement_controller(
        &mut self,
        index: usize,
        rng: &mut dyn Rng,
    ) -> Result<bool, MovementError> {
        let Self {
            bots,
            factories,
            config,
            resource,
            ..
        } = self;
        let bot = bots.get_mut(index).expect("bot index in range");
        set_bot_movement_controller(
            factories,
            resource,
            &config.controller_type,
            &config.controller_data,
            bot,
            rng,
        )
    }

    /// `ClientApp::setMovementController` over this app's registry
    /// (`client_app.cpp:764-789`).
    pub fn set_movement_controller(
        &mut self,
        index: usize,
        controller_type: &str,
        controller_data: &str,
        rng: &mut dyn Rng,
    ) -> Result<bool, MovementError> {
        let Self {
            bots,
            factories,
            resource,
            ..
        } = self;
        let bot = bots.get_mut(index).expect("bot index in range");
        set_bot_movement_controller(
            factories,
            resource,
            controller_type,
            controller_data,
            bot,
            rng,
        )
    }

    /// `MainApp::updateMovement` (`main_app.cpp:524-543`): the empty
    /// tag means all bots; a failed set prints the error
    /// (`PyErr_Print()`).
    pub fn update_movement(&mut self, tag: &str, rng: &mut dyn Rng) {
        let Self {
            bots,
            factories,
            config,
            resource,
            ..
        } = self;
        for bot in bots.iter_mut() {
            if !(tag.is_empty() || bot.tag == tag) {
                continue;
            }
            if let Err(err) = set_bot_movement_controller(
                factories,
                resource,
                &config.controller_type,
                &config.controller_data,
                bot,
                rng,
            ) {
                eprintln!("{err:?}");
            }
        }
    }

    /// `MainApp::handleGameTickTimeSlice` (`main_app.cpp:593-671`):
    /// the re-entrancy guard, the fragment rotation over the active
    /// set, the retry queue, and `advanceTime`/`condemnedInterfaces`
    /// (both no-ops/deferred here -- see the module doc).
    pub fn handle_game_tick_time_slice(&mut self, rng: &mut dyn Rng) {
        if self.in_tick {
            // This can occur because the tick method of ClientApp can
            // processInput on the ServerConnection.
            eprintln!("MainApp::handleTimeout: Called recursively");
            return;
        }

        self.in_tick = true;

        // personality.callMethod( "onTick", ... ) -- script face.

        let number_to_update = (self.bots.len() as i32 + self.remainder) / TICK_FRAGMENTS;
        self.remainder = (self.bots.len() as i32 + self.remainder) % TICK_FRAGMENTS;

        self.local_time += MINI_TICK_PERIOD;

        let mut number_to_update = number_to_update;
        while number_to_update > 0 && !self.bots.is_empty() {
            number_to_update -= 1;
            if self.client_tick_index >= self.bots.len() {
                self.client_tick_index = 0;
            }
            // The cursor pre-advances: it already points at the next
            // bot while the current one ticks.
            let index = self.client_tick_index;
            self.client_tick_index += 1;

            if !self.bots[index].tick(TICK_PERIOD, rng) {
                // tick may have called destroyBots(), which could
                // invalidate the current iterator and free the bot;
                // the value move plays the ClientAppPtr's part.
                if !self.bots[index].is_destroyed {
                    if self.bots[index].log_on_retry_time != 0 {
                        let bot = self.bots.remove(index).expect("index in range");
                        self.bots_for_log_on_retry.push(bot);
                    } else {
                        self.bots.remove(index);
                    }
                    // The erase shifted the pre-advanced cursor's
                    // element down one slot.
                    if self.client_tick_index > 0 {
                        self.client_tick_index -= 1;
                    }
                }
            }
        }

        let now = foundation::time::stamp_now();

        let mut index = 0;
        while index < self.bots_for_log_on_retry.len() {
            if self.bots_for_log_on_retry[index].log_on_retry_time < now {
                let mut bot = self.bots_for_log_on_retry.remove(index);
                bot.log_on();
                self.bots.push_back(bot);
            } else {
                index += 1;
            }
        }

        // advanceTime(); condemnedInterfaces_.processOnce();

        self.in_tick = false;
    }

    /// The `mainDispatcher()` timer pass plus the `BotAdder` drain
    /// (`main_app.cpp:1221`'s re-entrant `addBots`, flattened). The
    /// pass runs on the timers' clock, like `processEvents` does.
    pub fn process_main_timers(&mut self) {
        self.main_timers.process_timers();
        let pending = std::mem::take(&mut *self.pending_bot_adds.borrow_mut());
        for count in pending {
            self.add_bots(count);
        }
    }

    /// The constructor with an injected timer clock, for the
    /// `BotAdder` tests.
    pub fn with_timers_clock(
        config: BotsConfig,
        app_id: String,
        seed: u64,
        clock: foundation::time_queue::ClockFn,
    ) -> Self {
        let mut app = Self::with_seed(config, app_id, seed);
        app.main_timers = DispatcherTimers::with_clock(clock);
        app
    }

    /// `addBotsSlowly` (`main_app.cpp:1232-1239`): the BotAdder
    /// trickle timer.
    pub fn add_bots_slowly(&mut self, count: i32, period: f32, per_tick: i32) {
        let adder = Rc::new(BotAdder {
            remaining: Cell::new(count),
            per_tick,
            pending: Rc::clone(&self.pending_bot_adds),
        });
        self.main_timers
            .add_timer((period * 1_000_000.0) as i64, adder, 0, "BotAdder");
    }

    /// `MainApp::findApp` (`main_app.cpp:843-868`): a NULL id never
    /// finds anything -- "cannot find dormant bots by ID".
    pub fn find_app(&self, id: i32) -> Option<&ClientApp> {
        if id == NULL_ENTITY_ID {
            return None;
        }
        self.bots.iter().find(|bot| bot.id == id)
    }

    /// `MainApp::appsKeys` (`main_app.cpp:873-890`): the live ids.
    pub fn apps_keys(&self) -> Vec<i32> {
        self.bots
            .iter()
            .filter(|bot| bot.id != NULL_ENTITY_ID)
            .map(|bot| bot.id)
            .collect()
    }

    /// `MainApp::appsValues` (`main_app.cpp:895-911`).
    pub fn apps_values(&self) -> Vec<&ClientApp> {
        self.bots
            .iter()
            .filter(|bot| bot.id != NULL_ENTITY_ID)
            .collect()
    }

    /// `MainApp::appsItems` (`main_app.cpp:916-935`).
    pub fn apps_items(&self) -> Vec<(i32, &ClientApp)> {
        self.bots
            .iter()
            .filter(|bot| bot.id != NULL_ENTITY_ID)
            .map(|bot| (bot.id, bot))
            .collect()
    }
}

/// `MainApp::createMovementController` (`main_app.cpp:752-807`): the
/// "None" early-out, then the factory lookup, then the TypeError.
pub fn create_movement_controller(
    factories: &mut MovementFactories,
    resource: &BwResource,
    controller_type: &str,
    controller_data: &str,
    speed: &mut f32,
    position: &mut Vector3,
    rng: &mut dyn Rng,
) -> Result<Option<Box<dyn MovementController>>, MovementError> {
    if controller_type == "None" {
        return Ok(None);
    }

    // The bots-extensions/*.so dlopen face (`main_app.cpp:755-782`)
    // is not ported: this tree has no dynamic-library controller ABI,
    // so a `.so:`-prefixed type reaches the lookup unstripped and
    // fails there.

    if let Some(factory) = factories.get(controller_type) {
        return factory
            .borrow_mut()
            .create(resource, controller_data, speed, position, rng);
    }

    Err(MovementError {
        kind: MovementErrorKind::TypeError,
        message: format!("No such controller type '{controller_type}'"),
    })
}

/// `ClientApp::setMovementController`'s body over an app's registry
/// (`client_app.cpp:764-789`): the canSet guard answers false with
/// no error; the create runs; the position writeback happens before
/// the error gate (the reference's `this->position( position )` is
/// unconditional), so a failed patrol-with-snap still snaps.
fn set_bot_movement_controller(
    factories: &mut MovementFactories,
    resource: &BwResource,
    controller_type: &str,
    controller_data: &str,
    bot: &mut ClientApp,
    rng: &mut dyn Rng,
) -> Result<bool, MovementError> {
    if !bot.can_set_movement_controller() {
        return Ok(false);
    }

    let mut position = bot.get_position();
    let mut speed = bot.speed;

    let created = create_movement_controller(
        factories,
        resource,
        controller_type,
        controller_data,
        &mut speed,
        &mut position,
        rng,
    );

    bot.speed = speed;
    bot.set_position(position);

    match created {
        Ok(controller) => {
            bot.movement_controller = controller;
            Ok(true)
        }
        Err(err) => Err(err),
    }
}

/// `BotAdder` (`main_app.cpp:1188-1229`): the trickle timer. The
/// `MainApp::instance()` re-entry becomes the pending-adds queue.
struct BotAdder {
    remaining: Cell<i32>,
    per_tick: i32,
    pending: Rc<RefCell<Vec<i32>>>,
}

impl TimerHandler<u64> for BotAdder {
    fn handle_timeout(&self, mut handle: TimerHandle<u64>, _user: usize) {
        let remaining = self.remaining.get();
        self.pending.borrow_mut().push(remaining.min(self.per_tick));
        self.remaining.set(remaining - self.per_tick);

        if self.remaining.get() <= 0 {
            // timer_.cancel(); delete this -- the Rc drops with the
            // cancelled node.
            handle.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConnectionTransport;

    fn config_for(tag: &str, random_names: bool) -> BotsConfig {
        let mut config = BotsConfig::new();
        config.username = "Bot".to_string();
        config.tag = tag.to_string();
        config.should_use_random_name = random_names;
        config
    }

    /// Places the tick cursor; the real driver is the tick loop
    /// (BW-12-04-03).
    #[cfg(test)]
    impl MainApp {
        fn set_client_tick_index(&mut self, index: usize) {
            self.client_tick_index = index;
        }
    }

    /// Acceptance: the random-name scheme is `_` + pre-incremented
    /// id + `_` + appID appended to the configured username
    /// (`main_app.cpp:403-415`), tags and transport ride the config
    /// statics, and the constructor's logOn cleared dormancy
    /// (`client_app.cpp:9-37`).
    #[test]
    fn random_names_follow_the_reference_scheme() {
        let app_id = "Test:20933".to_string();
        let mut app = MainApp::with_seed(config_for("Default", true), app_id.clone(), 7);
        app.add_bots(2);
        assert_eq!(app.num_bots(), 2);
        let names: Vec<&str> = app
            .bots()
            .iter()
            .map(|bot| bot.user_name.as_str())
            .collect();
        assert_eq!(names, vec!["Bot_1_Test:20933", "Bot_2_Test:20933"]);
        for bot in app.bots() {
            assert_eq!(bot.tag, "Default");
            assert_eq!(bot.transport, ConnectionTransport::Udp);
            assert_eq!(bot.id, 0);
            assert!(!bot.is_dormant);
        }
    }

    /// Acceptance: with `shouldUseRandomName` off the plain username
    /// is used as-is, duplicates included (`main_app.cpp:403-405`).
    #[test]
    fn random_names_off_keeps_the_plain_username() {
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.add_bots(3);
        let names: Vec<&str> = app
            .bots()
            .iter()
            .map(|bot| bot.user_name.as_str())
            .collect();
        assert_eq!(names, vec!["Bot", "Bot", "Bot"]);
    }

    /// Acceptance: each credential pair becomes a bot, in order,
    /// with the config's tag (`main_app.cpp:492-493`).
    #[test]
    fn add_bots_with_name_creates_each_credential_in_order() {
        let mut app = MainApp::with_seed(config_for("Named", false), "A".into(), 7);
        app.add_bots_with_name(&[("u1", "p1"), ("u2", "p2")]);
        let got: Vec<(String, String)> = app
            .bots()
            .iter()
            .map(|bot| (bot.user_name.clone(), bot.user_passwd.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("u1".to_string(), "p1".to_string()),
                ("u2".to_string(), "p2".to_string())
            ]
        );
        for bot in app.bots() {
            assert_eq!(bot.tag, "Named");
        }
    }

    /// Acceptance: `delBots` consumes from the front and stops at the
    /// empty set (`main_app.cpp:504-516`: the loop condition counts
    /// down and checks `!bots_.empty()`), non-positive counts are
    /// no-ops.
    #[test]
    fn del_bots_consumes_the_front_and_stops_at_empty() {
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.add_bots_with_name(&[("u1", "p"), ("u2", "p"), ("u3", "p")]);
        app.del_bots(2);
        let names: Vec<&str> = app
            .bots()
            .iter()
            .map(|bot| bot.user_name.as_str())
            .collect();
        assert_eq!(names, vec!["u3"]);
        app.del_bots(5);
        assert_eq!(app.num_bots(), 0);
        app.del_bots(0);
        app.del_bots(-1);
        assert_eq!(app.num_bots(), 0);
    }

    /// Acceptance: the cursor translation of
    /// `if (iter == clientTickIndex_) ++clientTickIndex_`
    /// (`main_app.cpp:508-512`): on the front it stays on slot 0
    /// (riding forward to the next element); past the front it steps
    /// down one; at the set's end it stays at the end.
    #[test]
    fn del_bots_keeps_the_tick_cursor_on_its_element() {
        // Cursor on the front: after the pop it sits on the follower,
        // which is now slot 0.
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.add_bots_with_name(&[("u1", "p"), ("u2", "p"), ("u3", "p")]);
        app.set_client_tick_index(0);
        app.del_bots(1);
        assert_eq!(app.client_tick_index(), 0);
        assert_eq!(app.num_bots(), 2);

        // Cursor past the front: the erase steps it down one, keeping
        // it on the same element.
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.add_bots_with_name(&[("u1", "p"), ("u2", "p"), ("u3", "p")]);
        app.set_client_tick_index(2);
        app.del_bots(1);
        assert_eq!(app.client_tick_index(), 1);

        // Cursor at the end: stays at the (new) end.
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.add_bots_with_name(&[("u1", "p"), ("u2", "p"), ("u3", "p")]);
        app.set_client_tick_index(3);
        app.del_bots(1);
        assert_eq!(app.client_tick_index(), 2);
        assert_eq!(app.num_bots(), 2);
    }

    /// Acceptance: `delTaggedEntities` removes only the matching tag,
    /// order preserved; the empty tag matches literally-empty tags
    /// only -- no "empty means all" rule exists here
    /// (`main_app.cpp:549-567`; that rule is `updateMovement`'s).
    #[test]
    fn del_tagged_entities_removes_only_the_matching_tag() {
        let mut app = MainApp::with_seed(config_for("A", false), "A".into(), 7);
        app.add_bots_with_name(&[("u1", "p"), ("u2", "p")]);
        app.config_mut().tag = "B".to_string();
        app.add_bots_with_name(&[("u3", "p"), ("u4", "p")]);
        app.del_tagged_entities("A");
        let names: Vec<&str> = app
            .bots()
            .iter()
            .map(|bot| bot.user_name.as_str())
            .collect();
        assert_eq!(names, vec!["u3", "u4"]);

        // The empty tag is an ordinary tag.
        app.del_tagged_entities("");
        assert_eq!(app.num_bots(), 2);
        app.config_mut().tag = String::new();
        app.add_bot_with_name("u5", "p");
        assert_eq!(app.num_bots(), 3);
        app.del_tagged_entities("");
        assert_eq!(app.num_bots(), 2);
    }

    /// Acceptance: the cursor rides an erasure it sits on to the
    /// following element (`main_app.cpp:558-562`) -- end position
    /// exactly, whether the walk erases before or at it.
    #[test]
    fn del_tagged_entities_keeps_the_tick_cursor_valid() {
        // Cursor on the second bot (tagged): the walk erases both A
        // bots; the cursor ends on the element that followed it.
        let mut app = MainApp::with_seed(config_for("A", false), "A".into(), 7);
        app.add_bots_with_name(&[("u1", "p"), ("u2", "p")]);
        app.config_mut().tag = "B".to_string();
        app.add_bots_with_name(&[("u3", "p")]);
        app.set_client_tick_index(1);
        app.del_tagged_entities("A");
        assert_eq!(app.client_tick_index(), 0);
        let names: Vec<&str> = app
            .bots()
            .iter()
            .map(|bot| bot.user_name.as_str())
            .collect();
        assert_eq!(names, vec!["u3"]);

        // Cursor at the end when the walk erases the tail: it stays
        // at the (new) end.
        let mut app = MainApp::with_seed(config_for("A", false), "A".into(), 7);
        app.add_bots_with_name(&[("u1", "p")]);
        app.set_client_tick_index(1);
        app.del_tagged_entities("A");
        assert_eq!(app.client_tick_index(), 0);
        assert_eq!(app.num_bots(), 0);
    }

    /// Acceptance: the fd guard rejects the bot when the head room is
    /// short -- `(opened + FD_PER_BOT) > limit` with FD_PER_BOT = 10
    /// (`main_app.cpp:74`, `:379-388`) -- and accepts right at the
    /// boundary.
    #[test]
    fn the_fd_guard_rejects_a_bot_when_the_head_room_is_short() {
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        // 91 + 10 > 100: rejected.
        app.add_bot_with_name_checked("swarm", "pw", 91, 100);
        assert_eq!(app.num_bots(), 0);
        // 90 + 10 > 100 is false: accepted.
        app.add_bot_with_name_checked("swarm", "pw", 90, 100);
        assert_eq!(app.num_bots(), 1);
        // An erroring helper (-1 on both sides) still refuses: -1+10
        // > -1.
        app.add_bot_with_name_checked("swarm", "pw", -1, -1);
        assert_eq!(app.num_bots(), 1);
    }

    /// Acceptance: identical command scripts drive the active set to
    /// identical states -- names, tags, order and speeds (the same
    /// seed) line up (`main_app.cpp:401-430`, `:441-516`).
    #[test]
    fn same_commands_produce_the_same_active_set() {
        let run = || {
            let mut app = MainApp::with_seed(config_for("Default", true), "S:1".into(), 42);
            app.add_bots(2);
            app.config_mut().tag = "Named".to_string();
            app.add_bots_with_name(&[("extra", "pw")]);
            app.del_bots(1);
            app.config_mut().tag = "Temp".to_string();
            app.add_bots(2);
            app.del_tagged_entities("Temp");
            // The controller is not comparable; the identity fields
            // are the active set.
            app.bots()
                .iter()
                .map(|bot| {
                    (
                        bot.user_name.clone(),
                        bot.tag.clone(),
                        bot.id,
                        bot.is_dormant,
                        bot.speed,
                    )
                })
                .collect::<Vec<_>>()
        };
        let first = run();
        let second = run();
        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].0, "Bot_2_S:1");
        assert_eq!(first[0].1, "Default");
        assert_eq!(first[1].0, "extra");
        assert_eq!(first[1].1, "Named");
        for bot in &first {
            assert!((6.0..8.0).contains(&bot.4));
        }
    }

    /// Acceptance: the helpers answer the live process's real
    /// numbers (`server_app.cpp:816-863`) -- the listing reads, the
    /// soft limit is positive.
    #[test]
    fn the_fd_helpers_read_the_live_process() {
        let opened = num_file_descriptors_open();
        assert!(opened > 0, "the test process has fds open: {opened}");
        let limit = file_descriptor_limit();
        assert!(limit > 0, "the soft nofile limit is real: {limit}");
    }

    /// Acceptance: the wallclock-seeded constructor starts the same
    /// empty set (`main_app.cpp:119-142`); the seed only feeds the
    /// speed draws.
    #[test]
    fn the_wallclock_constructor_starts_empty() {
        let app = MainApp::new(config_for("Default", true), "W:1".into());
        assert_eq!(app.num_bots(), 0);
        assert_eq!(app.client_tick_index(), 0);
    }

    /// A rotating fixed draw stream for the movement tests.
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

    impl crate::movement::Rng for FixedRng {
        fn next_u32(&mut self) -> u32 {
            let v = self.values[self.at % self.values.len()];
            self.at += 1;
            v
        }
    }

    /// Acceptance: `updateMovement` with the empty tag re-aims every
    /// bot at the config's controller (`main_app.cpp:524-543`); a
    /// non-empty tag only the matching ones; the "None" type clears
    /// the controller without an error (`main_app.cpp:752-755`).
    #[test]
    fn update_movement_aims_the_bots_at_the_config_controller() {
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.config_mut().controller_type = "Beeline".to_string();
        app.config_mut().controller_data = "10,0,0".to_string();
        app.add_bots_with_name(&[("u1", "p"), ("u2", "p")]);
        let mut rng = FixedRng::new(&[0]);

        app.update_movement("", &mut rng);
        for bot in app.bots() {
            assert!(bot.movement_controller.is_some());
        }

        // The "None" type: the controllers drop, no error.
        app.config_mut().controller_type = "None".to_string();
        app.update_movement("", &mut rng);
        for bot in app.bots() {
            assert!(bot.movement_controller.is_none());
        }
    }

    /// Acceptance: `updateMovement` with a tag only touches the
    /// matching bots (`main_app.cpp:527-528`).
    #[test]
    fn update_movement_with_a_tag_only_touches_the_matching() {
        let mut app = MainApp::with_seed(config_for("A", false), "A".into(), 7);
        app.config_mut().controller_type = "Beeline".to_string();
        app.config_mut().controller_data = "10,0,0".to_string();
        app.add_bots_with_name(&[("a1", "p")]);
        app.config_mut().tag = "B".to_string();
        app.add_bots_with_name(&[("b1", "p")]);
        let mut rng = FixedRng::new(&[0]);

        app.update_movement("A", &mut rng);
        assert!(app.bots()[0].movement_controller.is_some());
        assert!(app.bots()[1].movement_controller.is_none());
    }

    /// Acceptance: an unknown controller type raises the exact
    /// TypeError (`main_app.cpp:798-803`) -- `No such controller
    /// type '%s'`, no trailing newline.
    #[test]
    fn an_unknown_controller_type_raises_the_reference_type_error() {
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.add_bots_with_name(&[("u1", "p")]);
        let mut rng = FixedRng::new(&[0]);

        let err = app
            .set_movement_controller(0, "Wat", "", &mut rng)
            .unwrap_err();
        assert_eq!(err.kind, crate::movement::MovementErrorKind::TypeError);
        assert_eq!(err.message, "No such controller type 'Wat'");

        // The controller is left unchanged on failure
        // (`client_app.cpp:760-762`).
        assert!(app.bots()[0].movement_controller.is_none());
    }

    /// Acceptance: a failed set still writes the position back
    /// (`client_app.cpp:769-777` -- the writeback precedes the error
    /// gate), so a patrol-with-snap that raises still snaps the bot.
    #[test]
    fn a_failed_patrol_set_still_snaps_the_position() {
        let dir = std::env::temp_dir().join(format!("boundless_bots_snap_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            format!("{}/graphs.snap", dir.display()),
            "<root><nodes>\
             <node><name>a</name><pos>0 0 0</pos><radius>0</radius>\
             <edges><edge>b</edge></edges></node>\
             <node><name>b</name><pos>100 0 0</pos><radius>0</radius>\
             <edges><edge>a</edge></edges></node></nodes></root>",
        )
        .unwrap();

        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        *app.resource_mut() = BwResource::init_paths(&[dir.to_string_lossy().as_ref()]);
        app.add_bots_with_name(&[("u1", "p")]);
        let mut rng = FixedRng::new(&[0, 1073741823]);

        // The unknown argument raises, but the snap already moved the
        // start onto the node path.
        let err = app
            .set_movement_controller(0, "Patrol", "graphs.snap,snap,wat", &mut rng)
            .unwrap_err();
        assert_eq!(err.kind, crate::movement::MovementErrorKind::ValueError);
        assert_eq!(app.bots()[0].get_position(), Vector3::new(25.0, 0.0, 0.0));
    }

    /// Acceptance: the default controller comes from the config
    /// strings (`main_app.cpp:744-749`) and the set walks the
    /// recurrence from the bot's position.
    #[test]
    fn the_default_controller_comes_from_the_config() {
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.config_mut().controller_type = "Beeline".to_string();
        app.config_mut().controller_data = "10,0,0".to_string();
        app.add_bots_with_name(&[("u1", "p")]);
        let mut rng = FixedRng::new(&[0]);

        assert!(app.set_default_movement_controller(0, &mut rng).unwrap());

        // One slice: the bot walks the beeline toward (10, 0, 0) --
        // the recurrence advances the exact fraction of the
        // destination vector.
        let speed = app.bots()[0].speed;
        app.handle_game_tick_time_slice(&mut rng);
        let dest = Vector3::new(10.0, 0.0, 0.0);
        assert_eq!(
            app.bots()[0].get_position(),
            dest * (speed * TICK_PERIOD / dest.length())
        );
    }

    /// Acceptance: `createDefaultMovementController` rides the config
    /// strings through the shared builder (`main_app.cpp:744-749`).
    #[test]
    fn the_default_controller_builder_rides_the_config() {
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.config_mut().controller_type = "Beeline".to_string();
        app.config_mut().controller_data = "10,0,0".to_string();
        let mut rng = FixedRng::new(&[0]);
        let mut speed = 7.0f32;
        let mut pos = Vector3::ZERO;
        let controller = app
            .create_default_movement_controller(&mut speed, &mut pos, &mut rng)
            .unwrap();
        assert!(controller.is_some());
    }

    /// Acceptance: `updateMovement` prints a failed set through
    /// PyErr_Print (`main_app.cpp:524-543`) -- an unknown controller
    /// type leaves the bots controller-less.
    #[test]
    fn update_movement_prints_a_failed_set() {
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.config_mut().controller_type = "Ghost".to_string();
        app.config_mut().controller_data = String::new();
        app.add_bots_with_name(&[("u1", "p")]);
        let mut rng = FixedRng::new(&[0]);

        app.update_movement("", &mut rng);
        assert!(app.bots()[0].movement_controller.is_none());
    }

    /// Acceptance: a mid-rotation erase pulls the pre-advanced cursor
    /// back one slot (`main_app.cpp:621-644`'s erase arm).
    #[test]
    fn the_erase_path_pulls_back_the_cursor() {
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.add_bots_with_name(&[("u1", "p"), ("u2", "p")]);
        // The second bot's connection drops mid-tick and it holds no
        // retry stamp: erased, not queued.
        app.bots[1].drop_on_update = true;
        let mut rng = FixedRng::new(&[0]);

        app.handle_game_tick_time_slice(&mut rng);

        assert_eq!(app.bots.len(), 1);
        assert_eq!(app.client_tick_index, 1, "cursor pulled back from 2");
    }

    /// Acceptance: `setBotMovementController` gates on control -- an
    /// uncontrolled bot answers Ok(false) with no error; a controlled
    /// one gets the controller.
    #[test]
    fn set_bot_movement_controller_gates_on_control() {
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.add_bots_with_name(&[("u1", "p")]);
        let mut rng = FixedRng::new(&[0]);

        // Not controlled (the stub: offline): Ok(false).
        app.bots[0].is_online = false;
        let ok = set_bot_movement_controller(
            &mut app.factories,
            &app.resource,
            "Beeline",
            "10,0,0",
            &mut app.bots[0],
            &mut rng,
        )
        .unwrap();
        assert!(!ok);
        assert!(app.bots[0].movement_controller.is_none());

        // Controlled: the controller lands.
        app.bots[0].is_online = true;
        let ok = set_bot_movement_controller(
            &mut app.factories,
            &app.resource,
            "Beeline",
            "10,0,0",
            &mut app.bots[0],
            &mut rng,
        )
        .unwrap();
        assert!(ok);
        assert!(app.bots[0].movement_controller.is_some());
    }

    /// Acceptance: the tick slice rotates over the whole set each
    /// pass (`main_app.cpp:621-644` with `TICK_FRAGMENTS` 1), the
    /// cursor pre-advances and wraps, `localTime_` grows by
    /// `MINI_TICK_PERIOD` and `remainder` accumulates the modulo.
    #[test]
    fn the_tick_slice_rotates_over_the_active_set() {
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.add_bots_with_name(&[("u1", "p"), ("u2", "p"), ("u3", "p")]);
        let mut rng = FixedRng::new(&[0]);

        app.handle_game_tick_time_slice(&mut rng);
        assert_eq!(app.local_time(), MINI_TICK_PERIOD);
        for bot in app.bots() {
            assert_eq!(bot.client_time, TICK_PERIOD as f64, "every bot ticked");
        }
        assert_eq!(app.client_tick_index(), 3, "the cursor ran off the end");

        app.handle_game_tick_time_slice(&mut rng);
        assert_eq!(app.local_time(), 2.0 * MINI_TICK_PERIOD);
        assert_eq!(app.client_tick_index(), 3, "wrapped back to 0 then to 3");
        for bot in app.bots() {
            assert_eq!(bot.client_time, 2.0 * TICK_PERIOD as f64);
        }
    }

    /// Acceptance: a bot whose connection dropped mid-tick is
    /// erased; with a retry stamp it goes through the retry queue and
    /// comes back re-logged-on (`main_app.cpp:645-663`).
    #[test]
    fn a_dropped_bot_retries_through_the_retry_queue() {
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.add_bots_with_name(&[("u1", "p"), ("u2", "p")]);
        // A future retry stamp on the first bot.
        app.bots[0].log_on_retry_time = foundation::time::stamp_now() + 10_000_000_000;
        app.bots[0].drop_on_update = true;
        let mut rng = FixedRng::new(&[0]);

        app.handle_game_tick_time_slice(&mut rng);
        assert_eq!(app.num_bots(), 1, "the dropped bot left the set");
        assert_eq!(app.bots_for_log_on_retry.len(), 1, "and queued for retry");
        assert_eq!(app.client_tick_index(), 1, "the erase shifted the cursor");
        assert_eq!(app.bots()[0].user_name, "u2");

        // The stamp is in the future: the retry loop leaves it be.
        app.handle_game_tick_time_slice(&mut rng);
        assert_eq!(app.num_bots(), 1);
        assert_eq!(app.bots_for_log_on_retry.len(), 1);

        // Once the stamp passes, the bot re-logs-on and rejoins at
        // the back.
        app.bots_for_log_on_retry[0].log_on_retry_time = 1;
        app.handle_game_tick_time_slice(&mut rng);
        assert_eq!(app.bots_for_log_on_retry.len(), 0);
        assert_eq!(app.num_bots(), 2);
        assert_eq!(app.bots()[1].user_name, "u1", "rejoined at the back");
        assert!(app.bots()[1].is_online, "log_on ran on the way back in");
        assert!(!app.bots()[1].is_dormant);
    }

    /// Acceptance: a dropped bot with no retry stamp is erased for
    /// good (`main_app.cpp:648-656` -- the stamp is zero, so no
    /// retry push).
    #[test]
    fn a_dropped_bot_without_a_stamp_is_erased() {
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.add_bots_with_name(&[("u1", "p")]);
        app.bots[0].drop_on_update = true;
        let mut rng = FixedRng::new(&[0]);

        app.handle_game_tick_time_slice(&mut rng);
        assert_eq!(app.num_bots(), 0);
        assert_eq!(app.bots_for_log_on_retry.len(), 0);
        assert_eq!(app.client_tick_index(), 0);
    }

    /// Acceptance: the re-entrancy guard refuses the recursive call
    /// with the reference warning (`main_app.cpp:596-605`) and leaves
    /// the set untouched.
    #[test]
    fn the_recursive_tick_call_is_refused() {
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.add_bots_with_name(&[("u1", "p")]);
        let mut rng = FixedRng::new(&[0]);

        app.in_tick = true;
        app.handle_game_tick_time_slice(&mut rng);
        assert_eq!(app.num_bots(), 1, "the recursive call did nothing");
        assert_eq!(app.bots()[0].client_time, 0.0);
        assert_eq!(app.local_time(), 0.0);
    }

    /// Acceptance: `addBotsSlowly` trickles the batch through the
    /// BotAdder timer -- `min(remaining, perTick)` per pass, and the
    /// timer cancels itself when the batch is done
    /// (`main_app.cpp:1188-1240`).
    #[test]
    fn add_bots_slowly_trickles_the_batch() {
        let tick = std::rc::Rc::new(std::cell::Cell::new(0u64));
        let clock_tick = std::rc::Rc::clone(&tick);
        let mut app = MainApp::with_timers_clock(
            config_for("Default", true),
            "A".into(),
            7,
            Box::new(move || clock_tick.get()),
        );
        app.add_bots_slowly(5, 0.1, 2);

        // Nothing before the first period elapses.
        app.process_main_timers();
        assert_eq!(app.num_bots(), 0);

        tick.set(tick.get() + 100_000_000);
        app.process_main_timers();
        assert_eq!(app.num_bots(), 2);

        tick.set(tick.get() + 100_000_000);
        app.process_main_timers();
        assert_eq!(app.num_bots(), 4);

        // The tail batch is min(remaining, perTick) = 1.
        tick.set(tick.get() + 100_000_000);
        app.process_main_timers();
        assert_eq!(app.num_bots(), 5);

        // The timer cancelled itself.
        tick.set(tick.get() + 100_000_000);
        app.process_main_timers();
        assert_eq!(app.num_bots(), 5);
    }

    /// Acceptance: the `PyBots` backends (`main_app.cpp:843-935`) --
    /// a NULL id never finds ("cannot find dormant bots by ID") and
    /// the keys/values/items lists skip the dormant ids.
    #[test]
    fn the_pybots_backends_skip_the_dormant_ids() {
        let mut app = MainApp::with_seed(config_for("Default", false), "A".into(), 7);
        app.add_bots_with_name(&[("u1", "p"), ("u2", "p"), ("u3", "p")]);
        app.bots[0].id = 101;
        app.bots[1].id = NULL_ENTITY_ID; // dormant
        app.bots[2].id = 103;

        assert!(app.find_app(101).is_some());
        assert!(app.find_app(103).is_some());
        assert!(app.find_app(NULL_ENTITY_ID).is_none());
        assert!(app.find_app(999).is_none());
        assert_eq!(app.find_app(101).unwrap().user_name, "u1");

        assert_eq!(app.apps_keys(), vec![101, 103]);
        let values: Vec<&str> = app
            .apps_values()
            .iter()
            .map(|bot| bot.user_name.as_str())
            .collect();
        assert_eq!(values, vec!["u1", "u3"]);
        let items = app.apps_items();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0, 101);
        assert_eq!(items[0].1.user_name, "u1");
        assert_eq!(items[1].0, 103);
    }
}
