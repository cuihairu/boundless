//! `MainApp` (`server/tools/bots/main_app.{hpp,cpp}`): the Bots tool
//! application. BW-12-04-02 carries the batch add/remove face -- the
//! active set (`bots_`), the commands that mutate it
//! (`addBot`/`addBots`/`addBotsWithName`/`delBots`/`delTaggedEntities`)
//! and the fd head-room guard.
//!
//! Later items land the rest: the watcher command wiring rides
//! BW-12-01-04's protocol-2 stream decode, the script face
//! (`BigWorld.addBots` &c, `PyBots`) and the tick loop ride
//! BW-12-04-03.
//!
//! Reference translation, `bots_` is a `BW::list` walked with
//! iterators; the port keeps a `VecDeque` plus a slot index for
//! `clientTickIndex_`, preserving the cursor's survived-erasure
//! semantics (`main_app.cpp:508-512`, `:558-562`): a cursor sitting on
//! an erased slot rides forward to the element that follows it.

use std::collections::VecDeque;

use crate::client_app::ClientApp;
use crate::config::BotsConfig;

/// `FD_PER_BOT` (`main_app.cpp:74`): the estimated fd spend of one
/// bot, kept safely over the real figure for the epoll-based
/// connection face.
pub const FD_PER_BOT: i32 = 10;

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
    /// `clientTickIndex_` -- the tick rotation cursor. The tick loop
    /// itself lands with BW-12-04-03; the del commands already keep
    /// the slot valid.
    client_tick_index: usize,
    /// The `rand()` stand-in the speeds draw from (seeded like
    /// `srand( timestamp() )`, `main_app.cpp:141`).
    rng: u64,
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
            rng: seed,
        }
    }

    /// The speed draw: `6 + rand()*2/RAND_MAX`
    /// (`client_app.cpp:30`). The port keeps a small LCG -- the
    /// acceptance here pins the active set (names/tags/order), not
    /// the C library's rand stream.
    fn next_speed(&mut self) -> f32 {
        self.rng = self
            .rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let sample = (self.rng >> 33) as f32 / u32::MAX as f32;
        6.0 + sample * 2.0
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
            app.bots().iter().cloned().collect::<Vec<ClientApp>>()
        };
        let first = run();
        let second = run();
        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].user_name, "Bot_2_S:1");
        assert_eq!(first[0].tag, "Default");
        assert_eq!(first[1].user_name, "extra");
        assert_eq!(first[1].tag, "Named");
        for bot in &first {
            assert!((6.0..8.0).contains(&bot.speed));
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
}
