//! BW-06-04-01 the client's server discovery.
//!
//! `ServerFinder` (`lib/connection/server_finder.cpp`) finds the LoginApps on
//! the local network: it broadcasts a machine-guard `find` for server
//! components named "LoginInterface" to `PORT_MACHINED` and collects the
//! `ProcessStatsMessage` responses:
//!
//! * each response from a bwmachined instance reports the process's UDP port
//!   and uid; a `pid` of zero means no process was found and the response is
//!   ignored;
//! * the server address is the machined's address with the process's port;
//! * `MachineGuardResponseChecker`
//!   (`lib/network/machine_guard_response_checker.cpp`) decides when the
//!   discovery is done: a reply removes its source from the waiting set and
//!   adds the buddy address the packet announces, and discovery is done once
//!   at least one machined replied and nobody is outstanding;
//! * the two-second timeout ends the search regardless.
//!
//! The fixture-driven acceptance is that a canned set of responses yields the
//! same service list, in the order the responses arrive.

use std::collections::HashSet;

/// `PORT_MACHINED` (`network/portmap.hpp`).
pub const PORT_MACHINED: u16 = 20018;

/// `MachineGuardMessage::PROCESS_STATS_MESSAGE`.
pub const PROCESS_STATS_MESSAGE: u8 = 3;

/// `ProcessMessage::SERVER_COMPONENT`.
pub const SERVER_COMPONENT: u8 = 0;

/// `PARAM_USE_CATEGORY | PARAM_USE_NAME`: the find's filter.
pub const FIND_FILTER: u8 = 0x1 | 0x20;

/// The interface the find targets.
pub const LOGIN_INTERFACE_NAME: &str = "LoginInterface";

/// `ServerInfo` (`server_finder.hpp`): a found server's address and uid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerInfo {
    /// The LoginApp's address (the machined's ip with the process's port).
    pub addr: crate::baseappmgr::Addr,
    /// The server's uid.
    pub uid: u16,
}

/// The `ProcessStatsMessage` response fields the discovery reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessStatsMessage {
    /// The process id; zero means no process was found.
    pub pid: u16,
    /// The server's uid.
    pub uid: u16,
    /// The process's UDP port.
    pub port: u16,
}

/// One machined-guard reply: the source ip, the buddy address and the
/// messages it carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MGMResponse {
    /// The machined instance's ip.
    pub src_addr: u32,
    /// `packet.buddy_`: the buddy machined address (0 when none).
    pub buddy: u32,
    /// The `ProcessStatsMessage` responses.
    pub messages: Vec<ProcessStatsMessage>,
}

/// `ServerFinder::onMessage`: the server a response reports. A `pid` of zero
/// ("no process found") reports nothing.
pub fn server_from_message(src_addr: u32, msg: &ProcessStatsMessage) -> Option<ServerInfo> {
    if msg.pid == 0 {
        None
    } else {
        Some(ServerInfo {
            addr: (src_addr, msg.port),
            uid: msg.uid,
        })
    }
}

/// `MachineGuardResponseChecker`: decides when all broadcast replies are in.
#[derive(Debug, Default)]
pub struct MachineGuardResponseChecker {
    waiting_for: HashSet<u32>,
    received_from: HashSet<u32>,
}

impl MachineGuardResponseChecker {
    /// Constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// `receivedPacket`: note the reply and pick up the buddy it announces.
    pub fn received_packet(&mut self, src_addr: u32, buddy: u32) {
        self.received_from.insert(src_addr);
        self.waiting_for.remove(&src_addr);
        if buddy != 0 && !self.received_from.contains(&buddy) {
            self.waiting_for.insert(buddy);
        }
    }

    /// `isDone`.
    pub fn is_done(&self) -> bool {
        !self.received_from.is_empty() && self.waiting_for.is_empty()
    }
}

/// `ServerFinder`: the discovery state — the found servers and the reply
/// checker.
#[derive(Debug, Default)]
pub struct ServerFinder {
    checker: MachineGuardResponseChecker,
    servers: Vec<ServerInfo>,
    searching: bool,
}

impl ServerFinder {
    /// Constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// The servers found so far, in the order they arrived.
    pub fn servers(&self) -> &[ServerInfo] {
        &self.servers
    }

    /// Whether a search is in progress.
    pub fn is_searching(&self) -> bool {
        self.searching
    }

    /// The response checker (for the done test).
    pub fn checker(&self) -> &MachineGuardResponseChecker {
        &self.checker
    }

    /// `findServers`: begin a search. Returns false when a search is already
    /// in progress.
    pub fn begin_search(&mut self) -> bool {
        if self.searching {
            return false;
        }
        self.searching = true;
        self.servers.clear();
        self.checker = MachineGuardResponseChecker::new();
        true
    }

    /// `recv`/`onMessage`: feed one machined reply and return the servers it
    /// reported.
    pub fn on_response(&mut self, response: &MGMResponse) -> Vec<ServerInfo> {
        self.checker
            .received_packet(response.src_addr, response.buddy);
        let found: Vec<ServerInfo> = response
            .messages
            .iter()
            .filter_map(|msg| server_from_message(response.src_addr, msg))
            .collect();
        self.servers.extend(found.iter().copied());
        found
    }

    /// The search is done (the checker reports done, or the timeout fired);
    /// returns the servers found.
    pub fn finish(&mut self) -> Vec<ServerInfo> {
        self.searching = false;
        self.servers.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(pid: u16, uid: u16, port: u16) -> ProcessStatsMessage {
        ProcessStatsMessage { pid, uid, port }
    }

    #[test]
    fn a_found_process_reports_a_server() {
        let server = server_from_message(0x0a00_0001, &msg(123, 5, 20015)).unwrap();
        assert_eq!(server.addr, (0x0a00_0001, 20015));
        assert_eq!(server.uid, 5);
    }

    #[test]
    fn a_zero_pid_reports_no_server() {
        assert_eq!(server_from_message(0x0a00_0001, &msg(0, 5, 20015)), None);
    }

    #[test]
    fn the_fixture_produces_the_same_server_list() {
        let mut finder = ServerFinder::new();
        assert!(finder.begin_search());
        assert!(finder.is_searching());

        // The fixture: two machined replies, one with a live LoginApp and one
        // with a pid of zero, plus a second LoginApp on the same machine.
        let first = MGMResponse {
            src_addr: 0x0a00_0001,
            buddy: 0x0b00_0001,
            messages: vec![msg(100, 1, 20015), msg(200, 2, 20016)],
        };
        let second = MGMResponse {
            src_addr: 0x0b00_0001,
            buddy: 0,
            messages: vec![msg(0, 3, 20017)],
        };

        let found_first = finder.on_response(&first);
        assert_eq!(
            found_first,
            vec![
                ServerInfo {
                    addr: (0x0a00_0001, 20015),
                    uid: 1
                },
                ServerInfo {
                    addr: (0x0a00_0001, 20016),
                    uid: 2
                },
            ]
        );
        // The pid-zero reply reports nothing but ends the search (the buddy
        // answered).
        assert_eq!(finder.on_response(&second), Vec::<ServerInfo>::new());
        assert!(finder.checker().is_done());

        let servers = finder.finish();
        assert!(!finder.is_searching());
        assert_eq!(
            servers,
            vec![
                ServerInfo {
                    addr: (0x0a00_0001, 20015),
                    uid: 1
                },
                ServerInfo {
                    addr: (0x0a00_0001, 20016),
                    uid: 2
                },
            ]
        );
    }

    #[test]
    fn a_second_search_while_searching_is_refused() {
        let mut finder = ServerFinder::new();
        assert!(finder.begin_search());
        assert!(!finder.begin_search());
    }

    #[test]
    fn the_checker_waits_for_the_buddy() {
        let mut checker = MachineGuardResponseChecker::new();
        assert!(!checker.is_done());
        // A reply announces a buddy that still owes a reply.
        checker.received_packet(0x0a00_0001, 0x0b00_0001);
        assert!(!checker.is_done());
        // The buddy replies.
        checker.received_packet(0x0b00_0001, 0);
        assert!(checker.is_done());
    }

    #[test]
    fn the_checker_is_done_after_one_reply_with_no_buddy() {
        let mut checker = MachineGuardResponseChecker::new();
        checker.received_packet(0x0a00_0001, 0);
        assert!(checker.is_done());
    }
}
