//! BW-05-01-06 the machined UDP transport: a discovery client and the
//! machined daemon loop.
//!
//! Both directions run the `contracts::machined` packet codec over a real
//! `SOCK_DGRAM` socket, through `platform::socket` (the workspace's single
//! kernel socket boundary). The reference's own client path is
//! `lib/network/machined_utils.cpp`, which sends one `MachineGuardMessage`
//! per `MGMPacket` to `htons( PORT_MACHINED )` and waits on the same
//! socket for the reply.
//!
//! The daemon answers the discovery queries the client sends, mirrors the
//! reference `BWMachined::processMessage` dispatch for register/deregister
//! and listener registrations, and keeps the process registry and listener
//! list the queries read. The reference reads host stats from the platform
//! and answers whole-machine queries from them; here the caller owns the
//! snapshot, so the same dispatch works against a real daemon or a test
//! fixture.

use contracts::machined as mg;
use foundation::stream::BinaryStream;
use platform::socket;
use std::os::fd::RawFd;

/// `PORT_MACHINED` (`lib/network/portmap.hpp`), the machined UDP port.
pub const PORT_MACHINED: u16 = 20018;

/// A UDP transport failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// The socket could not be created.
    SocketCreation,
    /// The socket could not be bound.
    Bind,
    /// A packet could not be encoded.
    Encode,
    /// A packet could not be decoded.
    Decode,
    /// The packet could not be sent.
    Send,
    /// No reply arrived within the socket timeout.
    Timeout,
    /// The reply packet carried no message.
    NoReply,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SocketCreation => write!(f, "could not create the UDP socket"),
            Self::Bind => write!(f, "could not bind the UDP socket"),
            Self::Encode => write!(f, "could not encode the machine guard packet"),
            Self::Decode => write!(f, "could not decode the machine guard packet"),
            Self::Send => write!(f, "could not send the machine guard packet"),
            Self::Timeout => write!(f, "no reply arrived before the timeout"),
            Self::NoReply => write!(f, "the reply packet carried no message"),
        }
    }
}

impl std::error::Error for TransportError {}

/// The loopback address in the network order the socket layer takes.
fn loopback_ip() -> u32 {
    socket::inet_aton("127.0.0.1").expect("loopback is a valid dotted quad")
}

/// A discovery client: one UDP socket that sends a query and waits for the
/// reply, like the reference's temporary nub in `machined_utils.cpp`.
pub struct MachinedClient {
    fd: RawFd,
}

impl MachinedClient {
    /// Bind an ephemeral UDP socket on the loopback address.
    pub fn new() -> Result<Self, TransportError> {
        let fd = socket::create_socket(socket::SOCK_DGRAM);
        if fd < 0 {
            return Err(TransportError::SocketCreation);
        }
        if socket::bind(fd, socket::htons(0), loopback_ip()) < 0 {
            socket::close(fd);
            return Err(TransportError::Bind);
        }
        // Two seconds to wait for a reply.
        socket::set_timeout_option(fd, socket::SO_RCVTIMEO, 2);
        Ok(Self { fd })
    }

    /// Send one query message to `ip:port` and return the daemon's reply.
    pub fn query(
        &self,
        ip: u32,
        port: u16,
        message: &mg::MachineGuardMessage,
    ) -> Result<mg::MachineGuardMessage, TransportError> {
        let packet = mg::MgmPacket::new(vec![message.clone()]);
        let bytes = packet.to_bytes().map_err(|_| TransportError::Encode)?;
        if socket::sendto(self.fd, &bytes, socket::htons(port), ip) < 0 {
            return Err(TransportError::Send);
        }
        let mut buf = vec![0u8; mg::MgmPacket::MAX_SIZE];
        let (n, _, _) = socket::recvfrom(self.fd, &mut buf);
        if n <= 0 {
            return Err(TransportError::Timeout);
        }
        let reply =
            mg::MgmPacket::from_bytes(&buf[..n as usize]).map_err(|_| TransportError::Decode)?;
        reply
            .messages
            .into_iter()
            .next()
            .ok_or(TransportError::NoReply)
    }
}

impl Drop for MachinedClient {
    fn drop(&mut self) {
        socket::close(self.fd);
    }
}

/// The machined daemon: the bound UDP socket, the snapshot every query
/// answers from, and the listener registry.
pub struct MachinedDaemon {
    fd: RawFd,
    /// Host-order port the daemon answers on.
    port: u16,
    /// The network-order address the daemon answers on.
    ip: u32,
    pub snapshot: mg::MachinedSnapshot,
    pub listeners: mg::ListenerRegistry,
}

impl MachinedDaemon {
    /// Bind the daemon socket. `port` 0 picks an ephemeral port for tests.
    pub fn bind(
        port: u16,
        ip: u32,
        snapshot: mg::MachinedSnapshot,
    ) -> Result<Self, TransportError> {
        let fd = socket::create_socket(socket::SOCK_DGRAM);
        if fd < 0 {
            return Err(TransportError::SocketCreation);
        }
        if socket::bind(fd, socket::htons(port), ip) < 0 {
            socket::close(fd);
            return Err(TransportError::Bind);
        }
        // A short receive timeout so `step` returns and the caller can
        // re-check its loop rather than blocking forever.
        socket::set_timeout_option(fd, socket::SO_RCVTIMEO, 1);
        let (bound_port, bound_ip) = socket::local_address(fd).ok_or(TransportError::Bind)?;
        Ok(Self {
            fd,
            port: socket::ntohs(bound_port),
            ip: bound_ip,
            snapshot,
            listeners: mg::ListenerRegistry::new(),
        })
    }

    /// The address the daemon answers on, as a host-order port and the
    /// network-order address.
    pub fn address(&self) -> (u16, u32) {
        (self.port, self.ip)
    }

    /// Receive one packet, dispatch every message, and send the replies
    /// back to the sender. Returns the reply bytes sent, if any.
    pub fn step(&mut self) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; mg::MgmPacket::MAX_SIZE];
        let (n, from_port, from_ip) = socket::recvfrom(self.fd, &mut buf);
        if n <= 0 {
            return None;
        }
        let packet = mg::MgmPacket::from_bytes(&buf[..n as usize]).ok()?;
        let mut replies = Vec::new();
        for message in packet.messages {
            if let Some(reply) = self.dispatch(message, from_ip) {
                replies.push(reply);
            }
        }
        if replies.is_empty() {
            return None;
        }
        let bytes = mg::MgmPacket::new(replies).to_bytes().ok()?;
        socket::sendto(self.fd, &bytes, from_port, from_ip);
        Some(bytes)
    }

    /// Dispatch one message the way the reference `BWMachined::processMessage`
    /// does: register/deregister mutate the registry and confirm, listener
    /// registrations join the lists, and discovery queries are answered from
    /// the snapshot. Unhandled message types return no reply.
    fn dispatch(
        &mut self,
        message: mg::MachineGuardMessage,
        from_ip: u32,
    ) -> Option<mg::MachineGuardMessage> {
        match &message {
            mg::MachineGuardMessage::Process(pm) => {
                let confirmation = self.snapshot.registry.handle_command(pm)?;
                Some(mg::MachineGuardMessage::Process(confirmation))
            }
            mg::MachineGuardMessage::Listener(lm) => {
                let addr = from_ip.to_le_bytes();
                if self.listeners.add(lm, addr) {
                    Some(message)
                } else {
                    None
                }
            }
            _ => {
                let reply = mg::answer_query(&self.snapshot, &message);
                if let mg::MachineGuardMessage::Unknown(unknown) = &reply
                    && unknown.header.flags & mg::flags::MESSAGE_NOT_UNDERSTOOD != 0
                {
                    return None;
                }
                Some(reply)
            }
        }
    }
}

impl Drop for MachinedDaemon {
    fn drop(&mut self) {
        socket::close(self.fd);
    }
}

/// A helper to encode a message for the wire, for tests that want the raw
/// bytes without opening a socket.
pub fn encode(message: &mg::MachineGuardMessage) -> Vec<u8> {
    let mut stream = BinaryStream::new();
    message
        .write(&mut stream)
        .expect("encode a machine guard message");
    stream.as_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use contracts::machined::{
        Header, MachinedRegistry, ProcessMessage, message_id, process_param,
    };

    fn base_snapshot() -> mg::MachinedSnapshot {
        mg::MachinedSnapshot {
            hostname: "host".to_string(),
            cpu_speed: 2000,
            cpu_loads: vec![10],
            io_wait_time: 0,
            mem: 50,
            interfaces: Vec::new(),
            in_discards: 0,
            out_discards: 0,
            version: 1,
            platform_info: "Linux".to_string(),
            tags: Vec::new(),
            registry: MachinedRegistry::new(),
            user: mg::UserMessage::default(),
        }
    }

    fn pid_query(pid: u16) -> mg::MachineGuardMessage {
        mg::MachineGuardMessage::Pid(mg::PidMessage {
            header: Header {
                message: message_id::PID,
                flags: 0,
                seq: 7,
            },
            pid,
            running: 0,
        })
    }

    /// A register command for a process on an ephemeral-ish port.
    fn register_command(pid: u16, port: u16, name: &str) -> mg::MachineGuardMessage {
        let mut pm = ProcessMessage {
            pid,
            port,
            id: 1,
            name: name.to_string(),
            ..ProcessMessage::default()
        };
        pm.param = process_param::IS_MSGTYPE | contracts::machined::process_type::REGISTER;
        mg::MachineGuardMessage::Process(pm)
    }

    /// The daemon answers a discovery query from a client over a real UDP
    /// socket: the register command is applied, and an immediately-following
    /// pid query reports the process running.
    #[test]
    fn daemon_answers_client_over_udp() {
        let mut daemon = MachinedDaemon::bind(0, loopback_ip(), base_snapshot()).unwrap();
        let (port, _) = daemon.address();
        let client = MachinedClient::new().unwrap();

        // The daemon blocks on recv, the client blocks on the reply, so the
        // daemon runs on its own thread.
        let handle = std::thread::spawn(move || {
            // Three quiet timeouts let the client's two queries through.
            for _ in 0..4 {
                daemon.step();
            }
        });

        let register = register_command(1234, 40001, "cellapp01");
        client
            .query(loopback_ip(), port, &register)
            .expect("register");
        let reply = client
            .query(loopback_ip(), port, &pid_query(1234))
            .expect("pid query");
        handle.join().unwrap();

        let mg::MachineGuardMessage::Pid(pid) = reply else {
            panic!("expected a pid reply, got {reply:?}");
        };
        assert_eq!(pid.pid, 1234);
        assert_eq!(pid.running, 1);
    }

    /// The client encodes a message the reference codec would parse, and the
    /// daemon reads the wire bytes back into the same message.
    #[test]
    fn wire_round_trip_through_the_daemon() {
        let mut daemon = MachinedDaemon::bind(0, loopback_ip(), base_snapshot()).unwrap();
        let (port, _) = daemon.address();
        let client = MachinedClient::new().unwrap();

        let handle = std::thread::spawn(move || {
            for _ in 0..3 {
                daemon.step();
            }
        });

        let query = pid_query(99);
        let reply = client.query(loopback_ip(), port, &query).expect("reply");
        handle.join().unwrap();
        let mg::MachineGuardMessage::Pid(pid) = reply else {
            panic!("expected a pid reply, got {reply:?}");
        };
        // No such pid is running.
        assert_eq!(pid.running, 0);
    }

    /// Direction 2 of BW-05-01-06: the new client discovers the reference
    /// `bwmachined2` daemon over real UDP. The daemon must be started first;
    /// run with `--ignored` on a machine that has the reference tools.
    ///
    /// The binary is located by `$BW_MACHINED_BIN` or the standard BigWorld
    /// tools path.
    #[test]
    #[ignore = "requires the reference bwmachined2 binary"]
    fn client_discovers_via_reference_daemon() {
        let binary = std::env::var("BW_MACHINED_BIN").ok().or_else(|| {
            let path = "/home/cui/workspaces/BigWorld/game/bin/server/el7/tools/bwmachined2";
            std::path::Path::new(path)
                .exists()
                .then(|| path.to_string())
        });
        let binary = binary.expect("set BW_MACHINED_BIN to the reference bwmachined2");

        // A fresh pidfile so a stale one does not block the bind.
        let pidfile =
            std::env::temp_dir().join(format!("bwmachined2-interop-{}.pid", std::process::id()));
        let mut child = std::process::Command::new(&binary)
            .args(["-f", "-p"])
            .arg(&pidfile)
            .spawn()
            .expect("spawn the reference daemon");

        // Give it a moment to bind UDP 20018.
        std::thread::sleep(std::time::Duration::from_millis(700));

        let client = MachinedClient::new().expect("client socket");
        // pid 1 (init) is always running, so the reference reports it live.
        let reply = client
            .query(loopback_ip(), PORT_MACHINED, &pid_query(1))
            .expect("reply from the reference daemon");
        let mg::MachineGuardMessage::Pid(pid) = reply else {
            panic!("expected a pid reply, got {reply:?}");
        };
        assert_eq!(pid.pid, 1);
        assert_eq!(pid.running, 1);

        // A whole-machine query is answered from the host it is running on.
        let whole = mg::MachineGuardMessage::WholeMachine(mg::WholeMachineMessage {
            header: Header {
                message: message_id::WHOLE_MACHINE,
                flags: 0,
                seq: 11,
            },
            ..mg::WholeMachineMessage::default()
        });
        let reply = client
            .query(loopback_ip(), PORT_MACHINED, &whole)
            .expect("whole-machine reply");
        assert!(matches!(reply, mg::MachineGuardMessage::WholeMachine(_)));

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&pidfile);
    }
}
