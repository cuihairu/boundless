//! The bot fleet (BW-17-01-02): batch add/remove and the tag-filtered
//! control commands run over the delivered `Bot` connection layer.
//!
//! Ported from `programming/bigworld/server/tools/bots/main_app.cpp`
//! (the `MainApp` fleet face) with the run defaults taken from
//! `bots_config.cpp`. The reference keeps the bots in a list and walks it
//! with the `clientTickIndex_` iterator; this port keeps a `Vec` and
//! mirrors every iterator adjustment with index arithmetic (see the
//! round-robin notes on `tick`, `del_bots` and `del_tagged_entities`).
//! The personality-script hook inside the tick (`BWPersonality.onTick`)
//! and the controller factory behind `update_movement` both arrive with
//! BW-17-01-03.

use crate::bot::{Bot, FD_PER_BOT, TICK_PERIOD, Transport};

/// `TICK_FRAGMENTS` (main_app.cpp:63): the bots are updated in this many
/// fragments per game tick; the reference builds with 1, so every bot is
/// visited every tick.
pub const TICK_FRAGMENTS: u32 = 1;

/// `MINI_TICK_PERIOD` (main_app.cpp:67): the slice of game time one fleet
/// tick represents (`0.1f / TICK_FRAGMENTS`).
pub const MINI_TICK_PERIOD: f64 = 0.1;

/// The per-run fleet settings, seeded with the `BotsConfig` defaults
/// (bots_config.cpp:17-31). The config reader belongs to BW-12-04; this
/// carries the in-memory values the fleet runs with.
#[derive(Debug, Clone, PartialEq)]
pub struct FleetSettings {
    /// The base login name (`username`, bots_config.cpp:17).
    pub username: String,
    /// The base login password (`password`, bots_config.cpp:18).
    pub password: String,
    /// The tag new bots are stamped with (`tag`, bots_config.cpp:20).
    pub tag: String,
    /// The transport new bots dial with (derived by `BotsConfig::postInit`,
    /// bots_config.cpp:56-67).
    pub transport: Transport,
    /// Whether `add_bot` appends the `_<n>_<appID>` suffix
    /// (`shouldUseRandomName`, bots_config.cpp:23).
    pub should_use_random_name: bool,
    /// The movement-controller type `update_movement` applies
    /// (bots_config.cpp:28).
    pub controller_type: String,
    /// The movement-controller data `update_movement` applies
    /// (bots_config.cpp:29).
    pub controller_data: String,
    /// Seconds to wait before retrying a refused log-on; zero disables the
    /// retry schedule (`logOnRetryPeriod`, bots_config.cpp:31).
    pub log_on_retry_period: f32,
}

impl Default for FleetSettings {
    fn default() -> Self {
        Self {
            username: "Bot".to_string(),
            password: String::new(),
            tag: "Default".to_string(),
            transport: Transport::Udp,
            should_use_random_name: true,
            controller_type: "Patrol".to_string(),
            controller_data: "server/bots/test.bwp".to_string(),
            log_on_retry_period: 0.0,
        }
    }
}

/// The `MainApp` fleet: the bots list, the log-on retry queue and the
/// round-robin tick cursor.
pub struct Fleet {
    settings: FleetSettings,
    /// `appID_` (main_app.hpp:173) — stamped into the random bot names.
    app_id: String,
    /// `nextBotID_` (main_app.hpp:174) — pre-incremented per random name.
    next_bot_id: i32,
    /// The environment sample `numFileDescriptorsOpen()` would report; the
    /// caller keeps it fresh between adds.
    fd_open: i32,
    /// `fileDescriptorLimit()`.
    fd_limit: i32,
    /// `bots_` (main_app.hpp:159) — additions push to the back, removals
    /// consume from the front.
    bots: Vec<Bot>,
    /// `botsForLogOnRetry_` (main_app.hpp:160) — bots parked outside the
    /// list until their scheduled retry comes due.
    bots_for_log_on_retry: Vec<Bot>,
    /// `clientTickIndex_` — the round-robin cursor into `bots`.
    client_tick_index: usize,
    /// The `static remainder` carried between tick passes
    /// (main_app.cpp:618-620).
    remainder: i32,
    /// `localTime_` (main_app.cpp:123), advanced by one `MINI_TICK_PERIOD`
    /// per pass (main_app.cpp:622).
    local_time: f64,
    /// The `static bool inTick` recursion guard (main_app.cpp:595-601).
    in_tick: bool,
}

impl Fleet {
    /// A fleet for the given app id, capped at the process file-descriptor
    /// limit.
    pub fn new(app_id: &str, fd_limit: i32) -> Self {
        Self {
            settings: FleetSettings::default(),
            app_id: app_id.to_string(),
            next_bot_id: 0,
            fd_open: 0,
            fd_limit,
            bots: Vec::new(),
            bots_for_log_on_retry: Vec::new(),
            client_tick_index: 0,
            remainder: 0,
            local_time: 0.0,
            in_tick: false,
        }
    }

    /// The run settings.
    pub fn settings(&self) -> &FleetSettings {
        &self.settings
    }

    /// The run settings, mutable — the reference reads them fresh off the
    /// global `BotsConfig` on every use, and the tag is switched between
    /// batches exactly this way.
    pub fn settings_mut(&mut self) -> &mut FleetSettings {
        &mut self.settings
    }

    /// Refresh the file-descriptor sample `add_bot_with_name` budgets
    /// against.
    pub fn set_fd_open(&mut self, fd_open: i32) {
        self.fd_open = fd_open;
    }

    /// The number of live bots in the list (`bots_.size()`).
    pub fn len(&self) -> usize {
        self.bots.len()
    }

    /// Whether the list is empty.
    pub fn is_empty(&self) -> bool {
        self.bots.is_empty()
    }

    /// The live bots, in list order.
    pub fn bots(&self) -> &[Bot] {
        &self.bots
    }

    /// The live bots, mutable — the scenario driver feeds server-delivered
    /// events and drives bot faces through here, the way scripts hold and
    /// call the `ClientApp` objects directly.
    pub fn bots_mut(&mut self) -> &mut [Bot] {
        &mut self.bots
    }

    /// The bots parked in the log-on retry queue.
    pub fn retry_queue_len(&self) -> usize {
        self.bots_for_log_on_retry.len()
    }

    /// The fleet-local game time, advanced by `MINI_TICK_PERIOD` per pass
    /// (`localTime_`, main_app.cpp:123/622).
    pub fn local_time(&self) -> f64 {
        self.local_time
    }

    /// `MainApp::addBotWithName` (main_app.cpp:376-399): refuse when the
    /// file-descriptor budget (`fdOpen + FD_PER_BOT > fdLimit`) would be
    /// exceeded, otherwise create the bot with the settings' transport and
    /// tag and append it to the list. The reference logs the refusal and
    /// returns void; the port reports it with `false`.
    pub fn add_bot_with_name(&mut self, name: &str, password: &str) -> bool {
        if self.fd_open + FD_PER_BOT > self.fd_limit {
            return false;
        }
        self.bots.push(Bot::new(
            name,
            password,
            self.settings.transport,
            &self.settings.tag,
        ));
        true
    }

    /// `MainApp::addBot` (main_app.cpp:401-422): the settings' username,
    /// suffixed `_<n>_<appID>` with a pre-incremented counter when
    /// `should_use_random_name`.
    pub fn add_bot(&mut self) -> bool {
        let mut name = self.settings.username.clone();
        if self.settings.should_use_random_name {
            self.next_bot_id += 1;
            name.push_str(&format!("_{}_{}", self.next_bot_id, self.app_id));
        }
        let password = self.settings.password.clone();
        self.add_bot_with_name(&name, &password)
    }

    /// `MainApp::addBots` (main_app.cpp:424-439): add `num` bots.
    pub fn add_bots(&mut self, num: i32) {
        for _ in 0..num {
            self.add_bot();
        }
    }

    /// `MainApp::addBotsWithName` (main_app.cpp:441-501): add one bot per
    /// pregenerated username/password pair. The reference validates the
    /// Python sequence item by item and stops at the first invalid one
    /// with a `TypeError`; behind a typed slice that boundary cannot
    /// occur. Bots refused by the FD budget are skipped.
    pub fn add_bots_with_name(&mut self, credentials: &[(&str, &str)]) {
        for (name, password) in credentials {
            self.add_bot_with_name(name, password);
        }
    }

    /// `MainApp::delBots` (main_app.cpp:504-519): destroy up to `num`
    /// bots from the front of the list.
    ///
    /// Round-robin note: the reference advances `clientTickIndex_` only
    /// when it points at the front element being popped (iterator `++`
    /// before `pop_front`); with the Vec index the same effect is the
    /// mirror — a cursor not at the front follows its bot one slot left.
    pub fn del_bots(&mut self, num: i32) {
        let mut num = num;
        while num > 0 && !self.bots.is_empty() {
            num -= 1;
            if self.client_tick_index > 0 {
                self.client_tick_index -= 1;
            }
            let mut bot = self.bots.remove(0);
            bot.destroy();
        }
    }

    /// `MainApp::delTaggedEntities` (main_app.cpp:549-572): remove every
    /// bot whose tag matches. The reference moves each match to a
    /// `condemnedBots` list and lets it destruct when the list leaves
    /// scope; the port destroys on removal.
    ///
    /// Round-robin note: a match sitting before the cursor shifts the
    /// cursor's bot one slot left, so the index follows; a match at the
    /// cursor itself needs no adjustment (the bot after it slides into
    /// place, which is where the reference's incremented iterator points).
    pub fn del_tagged_entities(&mut self, tag: &str) {
        let mut index = 0;
        while index < self.bots.len() {
            if self.bots[index].tag() == tag {
                if index < self.client_tick_index {
                    self.client_tick_index -= 1;
                }
                let mut bot = self.bots.remove(index);
                bot.destroy();
            } else {
                index += 1;
            }
        }
    }

    /// `MainApp::updateMovement` (main_app.cpp:521-547): apply the
    /// settings' controller type/data to every bot matching the tag; an
    /// empty tag targets all bots. A bot that is not in the world refuses
    /// the controller and keeps the old one — the reference prints (and
    /// clears) the Python error, which has no counterpart here.
    pub fn update_movement(&mut self, tag: &str) {
        let controller_type = self.settings.controller_type.clone();
        let controller_data = self.settings.controller_data.clone();
        for bot in &mut self.bots {
            if tag.is_empty() || bot.tag() == tag {
                let _ = bot.set_movement_controller(&controller_type, &controller_data);
            }
        }
    }

    /// `MainApp::handleGameTickTimeSlice` (main_app.cpp:593-682): tick the
    /// list round-robin in `TICK_FRAGMENTS` fragments, park freshly
    /// disconnected bots with a retry schedule in the retry queue, return
    /// due retries to the list with `logOn()`, and advance the game time.
    /// `now` is the caller's monotonic stamp, in the same domain the
    /// delivered retry times were computed in.
    ///
    /// The reference guards against re-entrant calls because a bot's tick
    /// can pump the connection and reach the timer handler again; that
    /// call chain is structurally absent behind the event-feed
    /// abstraction, but the guard is kept for parity. The personality
    /// script's `onTick` hook (main_app.cpp:605-613) arrives with
    /// BW-17-01-03.
    pub fn tick(&mut self, now: f64) {
        if self.in_tick {
            return;
        }
        self.in_tick = true;

        let number_to_update = (self.bots.len() as i32 + self.remainder) / TICK_FRAGMENTS as i32;
        self.remainder = (self.bots.len() as i32 + self.remainder) % TICK_FRAGMENTS as i32;

        self.local_time += MINI_TICK_PERIOD;

        let mut updated = 0;
        while updated < number_to_update && !self.bots.is_empty() {
            if self.client_tick_index >= self.bots.len() {
                self.client_tick_index = 0;
            }
            let index = self.client_tick_index;
            self.client_tick_index += 1;

            let keep = self.bots[index].tick(TICK_PERIOD);
            if !keep && !self.bots[index].is_destroyed() {
                if self.bots[index].log_on_retry_time() != 0.0 {
                    let bot = self.bots.remove(index);
                    self.bots_for_log_on_retry.push(bot);
                } else {
                    self.bots.remove(index);
                }
                // The cursor already moved past the removed bot; the Vec
                // shift moves the next bot into its slot, so follow it.
                self.client_tick_index -= 1;
            }
            updated += 1;
        }

        let mut retry_index = 0;
        while retry_index < self.bots_for_log_on_retry.len() {
            if self.bots_for_log_on_retry[retry_index].log_on_retry_time() < now {
                let mut bot = self.bots_for_log_on_retry.remove(retry_index);
                bot.log_on();
                self.bots.push(bot);
            } else {
                retry_index += 1;
            }
        }

        self.in_tick = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::{ConnState, NULL_ENTITY_ID};

    /// bots_config.cpp:17-31 — the settings a fresh fleet runs with.
    #[test]
    fn fleet_defaults_match_the_reference_config() {
        let settings = FleetSettings::default();
        assert_eq!(settings.username, "Bot");
        assert_eq!(settings.password, "");
        assert_eq!(settings.tag, "Default");
        assert_eq!(settings.transport, Transport::Udp);
        assert!(settings.should_use_random_name);
        assert_eq!(settings.controller_type, "Patrol");
        assert_eq!(settings.controller_data, "server/bots/test.bwp");
        assert_eq!(settings.log_on_retry_period, 0.0);

        let fleet = Fleet::new("0", 100);
        assert_eq!(fleet.settings(), &FleetSettings::default());
    }

    /// main_app.cpp:376-399 — a named bot carries the given credentials
    /// and the settings' tag and transport, and starts logging in.
    #[test]
    fn named_bots_carry_the_given_credentials() {
        let mut fleet = Fleet::new("0", 100);
        assert!(fleet.is_empty());
        assert!(fleet.add_bot_with_name("Alice", "pw"));
        assert_eq!(fleet.len(), 1);
        let bot = &fleet.bots()[0];
        assert_eq!(bot.user_name(), "Alice");
        assert_eq!(bot.user_passwd(), "pw");
        assert_eq!(bot.tag(), "Default");
        assert_eq!(bot.transport(), Transport::Udp);
        assert_eq!(bot.conn_state(), ConnState::LoggingIn);
    }

    /// main_app.cpp:401-422 — random names are the username suffixed
    /// `_<n>_<appID>` with a pre-incremented counter that persists across
    /// batches.
    #[test]
    fn random_names_append_the_counter_and_app_id() {
        let mut fleet = Fleet::new("0", 100);
        fleet.add_bots(2);
        assert_eq!(fleet.bots()[0].user_name(), "Bot_1_0");
        assert_eq!(fleet.bots()[1].user_name(), "Bot_2_0");
        fleet.add_bot();
        assert_eq!(fleet.bots()[2].user_name(), "Bot_3_0");
    }

    /// main_app.cpp:401-422 — with `shouldUseRandomName` off the bare
    /// username is used for every bot.
    #[test]
    fn without_random_names_the_username_is_bare() {
        let mut fleet = Fleet::new("7", 100);
        fleet.settings_mut().should_use_random_name = false;
        fleet.add_bots(2);
        assert_eq!(fleet.bots()[0].user_name(), "Bot");
        assert_eq!(fleet.bots()[1].user_name(), "Bot");
    }

    /// main_app.cpp:376-399 — the FD budget refuses exactly at
    /// `fdOpen + FD_PER_BOT > fdLimit`, leaving the list untouched.
    #[test]
    fn the_fd_budget_refuses_at_the_limit() {
        let mut fleet = Fleet::new("0", 20);
        fleet.set_fd_open(11);
        assert!(!fleet.add_bot_with_name("Alice", "pw"));
        assert!(fleet.is_empty());

        fleet.set_fd_open(10);
        assert!(fleet.add_bot_with_name("Alice", "pw"));
        assert_eq!(fleet.len(), 1);
    }

    /// main_app.cpp:504-519 — removals consume from the front and stop at
    /// the empty list without underflowing.
    #[test]
    fn del_bots_consumes_the_front() {
        let mut fleet = Fleet::new("0", 100);
        fleet.add_bots_with_name(&[("A", "p"), ("B", "p"), ("C", "p")]);
        fleet.del_bots(2);
        assert_eq!(fleet.len(), 1);
        assert_eq!(fleet.bots()[0].user_name(), "C");
        fleet.del_bots(9);
        assert!(fleet.is_empty());
    }

    /// main_app.cpp:504-519 — after a full round-robin pass the cursor
    /// sits at the end of the list; the front removal follows it one slot
    /// left and the next pass still visits every remaining bot.
    #[test]
    fn del_bots_keeps_the_round_robin_aligned() {
        let mut fleet = Fleet::new("0", 100);
        fleet.add_bots_with_name(&[("A", "p"), ("B", "p"), ("C", "p")]);
        for (id, bot) in fleet.bots_mut().iter_mut().enumerate() {
            bot.feed_log_on_result(id as u32 + 1, 1);
        }
        fleet.tick(0.0);

        fleet.del_bots(1);
        assert_eq!(fleet.bots()[0].user_name(), "B");
        fleet.tick(0.1);
        assert!(fleet.bots().iter().all(|bot| bot.is_online()));
    }

    /// main_app.cpp:549-572 — only the matching tag is removed, in list
    /// order; an unknown tag removes nothing.
    #[test]
    fn del_tagged_entities_takes_only_matching_tags() {
        let mut fleet = Fleet::new("0", 100);
        fleet.add_bots_with_name(&[("A", "p"), ("B", "p")]);
        fleet.settings_mut().tag = "Raid".to_string();
        fleet.add_bots_with_name(&[("C", "p"), ("D", "p")]);

        fleet.del_tagged_entities("Raid");
        assert_eq!(fleet.len(), 2);
        assert_eq!(fleet.bots()[0].user_name(), "A");
        assert_eq!(fleet.bots()[1].user_name(), "B");

        fleet.del_tagged_entities("Ghost");
        assert_eq!(fleet.len(), 2);
    }

    /// main_app.cpp:549-572 — removals before the cursor shift it along
    /// with their bots; the pass after the purge still visits everyone.
    #[test]
    fn del_tagged_entities_keeps_the_round_robin_aligned() {
        let mut fleet = Fleet::new("0", 100);
        fleet.add_bots_with_name(&[("A", "p"), ("B", "p")]);
        fleet.settings_mut().tag = "Raid".to_string();
        fleet.add_bots_with_name(&[("C", "p"), ("D", "p")]);
        for (id, bot) in fleet.bots_mut().iter_mut().enumerate() {
            bot.feed_log_on_result(id as u32 + 1, 1);
        }
        fleet.tick(0.0);

        fleet.del_tagged_entities("Raid");
        fleet.tick(0.1);
        assert!(fleet.bots().iter().all(|bot| bot.is_online()));
    }

    /// main_app.cpp:593-682 — one pass visits every bot (TICK_FRAGMENTS
    /// is 1), and a bot removed mid-pass does not skip the bot after it:
    /// the Vec cursor follows the shift.
    #[test]
    fn the_pass_never_skips_after_an_in_loop_removal() {
        let mut fleet = Fleet::new("0", 100);
        fleet.add_bots_with_name(&[("A", "p"), ("B", "p"), ("C", "p")]);
        for (id, bot) in fleet.bots_mut().iter_mut().enumerate() {
            bot.feed_log_on_result(id as u32 + 1, 1);
        }
        fleet.tick(0.0);
        assert!(fleet.bots().iter().all(|bot| bot.is_online()));

        // B disconnects and the script declines to park it dormant: the
        // pass destroys it mid-round and must still reach C.
        fleet.bots_mut()[1].feed_logged_off(Some(false));
        fleet.tick(0.1);
        assert_eq!(fleet.len(), 2);
        assert_eq!(fleet.bots()[0].id(), 1);
        assert_eq!(fleet.bots()[1].id(), 3);
        assert!(fleet.bots()[1].is_online());
    }

    /// main_app.cpp:636-657 — a bot whose log-on was refused with a
    /// scheduled retry leaves the list into the retry queue and returns
    /// with `logOn()` once its retry time has come due.
    #[test]
    fn a_retry_scheduled_bot_relogs_on_when_due() {
        let mut fleet = Fleet::new("0", 100);
        fleet.add_bot();
        fleet.bots_mut()[0].feed_log_on_failure(Some(5.0));

        fleet.tick(0.0);
        assert!(fleet.is_empty());
        assert_eq!(fleet.retry_queue_len(), 1);

        // Not yet due.
        fleet.tick(4.0);
        assert_eq!(fleet.retry_queue_len(), 1);

        fleet.tick(5.5);
        assert_eq!(fleet.len(), 1);
        assert_eq!(fleet.retry_queue_len(), 0);
        let bot = &fleet.bots()[0];
        assert_eq!(bot.conn_state(), ConnState::LoggingIn);
        assert_eq!(bot.log_on_retry_time(), 0.0);
    }

    /// main_app.cpp:626-634 — a fresh disconnect with no retry scheduled
    /// (a zero `logOnRetryPeriod`) drops the bot without queueing it.
    #[test]
    fn a_zero_retry_failure_drops_the_bot() {
        let mut fleet = Fleet::new("0", 100);
        fleet.add_bot();
        fleet.bots_mut()[0].feed_log_on_failure(None);

        fleet.tick(0.0);
        assert!(fleet.is_empty());
        assert_eq!(fleet.retry_queue_len(), 0);
        assert_eq!(fleet.local_time(), MINI_TICK_PERIOD);
    }

    /// main_app.cpp:595-601 — the re-entrance guard refuses a nested
    /// tick outright.
    #[test]
    fn the_recursion_guard_refuses_reentrant_ticks() {
        let mut fleet = Fleet::new("0", 100);
        fleet.add_bot();
        fleet.bots_mut()[0].feed_log_on_result(3, 1);

        fleet.in_tick = true;
        fleet.tick(0.0);
        assert_eq!(fleet.bots()[0].conn_state(), ConnState::LoggingIn);
        assert_eq!(fleet.local_time(), 0.0);

        fleet.in_tick = false;
        fleet.tick(0.0);
        assert!(fleet.bots()[0].is_online());
    }

    /// main_app.cpp:622 — each pass advances the game time by one
    /// MINI_TICK_PERIOD.
    #[test]
    fn each_pass_advances_the_local_time() {
        let mut fleet = Fleet::new("0", 100);
        fleet.tick(0.0);
        fleet.tick(0.1);
        assert!((fleet.local_time() - 0.2).abs() < 1e-9);
    }

    /// main_app.cpp:521-547 — the acceptance face: a tag-filtered command
    /// touches only the matching bots, an empty tag targets everyone, and
    /// a bot not in the world refuses without losing its old controller.
    #[test]
    fn update_movement_scopes_to_the_tag() {
        let mut fleet = Fleet::new("0", 100);
        fleet.add_bots_with_name(&[("A", "p")]);
        fleet.settings_mut().tag = "Raid".to_string();
        fleet.add_bots_with_name(&[("C", "p")]);
        fleet.bots_mut()[0].feed_log_on_result(1, 1);
        fleet.bots_mut()[1].feed_log_on_result(2, 1);
        fleet.tick(0.0);

        fleet.update_movement("Raid");
        assert!(fleet.bots()[0].controller().is_none());
        let spec = fleet.bots()[1].controller().expect("raid bot updated");
        assert_eq!(spec.kind, "Patrol");
        assert_eq!(spec.data, "server/bots/test.bwp");

        // The empty tag targets every bot.
        fleet.update_movement("");
        assert!(fleet.bots()[0].controller().is_some());

        // C logs off; a refused update leaves its controller unchanged.
        fleet.bots_mut()[1].feed_logged_off(Some(true));
        fleet.tick(0.1);
        fleet.update_movement("Raid");
        assert_eq!(
            fleet.bots()[1].controller().map(|spec| spec.kind.as_str()),
            Some("Patrol")
        );
        assert_eq!(fleet.bots()[1].id(), NULL_ENTITY_ID);
    }
}
