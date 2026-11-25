//! OPT-02-01-04: the mixed-version rerun — the reference EL7 binaries and
//! the new Rust fabric in one deployment, requests over UDP and TCP.
//!
//! Five legs, all on loopback, one deployment each for the machined port:
//!
//! 1. `udp-new-to-original` — the new `MachinedClient` queries the
//!    reference `bwmachined2`: a pid query (pid 1 is running) and a
//!    whole-machine query, both answered by the original daemon.
//! 2. `udp-original-to-new` — the reference `message_logger` starts
//!    against the new `MachinedDaemon` on `PORT_MACHINED`: its watcher
//!    registration and its birth/death listener registrations are parsed
//!    and answered by the new daemon.
//! 3. `udp-discovery` — a fake component (version 2.7) is registered in
//!    the new daemon's registry; the original logger's discovery broadcast
//!    finds it and pushes the `logger/addTCP` watcher set to the
//!    component's own UDP socket. A second component at version 2.5 gets
//!    the `logger/add` UDP flavor, so the version gate on the original
//!    side is exercised too.
//! 4. `tcp-new-to-original` — the component connects TCP to the address
//!    the push carried and ships the register envelope (`108`) plus record
//!    envelopes (`107`), each framed as `[i32 LE length][bytes]`, exactly
//!    what `LoggerEndpoint::sendImmediate` writes. The original logger
//!    must accept the component and store the records in its mldb output.
//! 5. `udp-records` — the old-flavor component ships the same envelopes
//!    over UDP (datagrams, no length prefix) and the records land as well.
//!
//! The reference binaries are located by `$BW_MACHINED_BIN` /
//! `$BW_MESSAGE_LOGGER_BIN` or the standard EL7 tools path; the resource
//! root by `$BW_RES_PATH`. Run with `--ignored` on a machine that has
//! them:
//!
//! ```sh
//! EL7_MIXED_OUT=<absolute path> cargo test -p server-runtime \
//!     --test el7_mixed_deploy the_mixed_deploy_rerun_passes_all_legs \
//!     -- --ignored --exact
//! ```

#![cfg(target_os = "linux")]

use std::io::Write as _;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use contracts::machined as mg;
use contracts::machined::{
    ProcessMessage, message_id, process_category, process_param, process_type,
};
use message_logger::MESSAGE_LOGGER_VERSION;
use message_logger::component_message::ComponentMessage;
use message_logger::forwarding_handler::{ArgValue, ForwardingStringHandler, build_record};
use message_logger::message_header::MessageHeader;
use message_logger::message_priority;
use platform::socket;
use server_runtime::machined::{MachinedClient, MachinedDaemon, PORT_MACHINED};

const LOOPBACK: u32 = 0x0100_007f;
const DEFAULT_TOOLS: &str = "/home/cui/workspaces/BigWorld/game/bin/server/el7/tools";
const DEFAULT_RES: &str = "/home/cui/workspaces/BigWorld/game/res/bigworld";

/// `WATCHER_MSG_SET` (`lib/network/watcher_nub.hpp:65`).
const WATCHER_MSG_SET: i32 = 17;

fn binary(name: &str, env: &str) -> String {
    std::env::var(env)
        .ok()
        .unwrap_or(format!("{DEFAULT_TOOLS}/{name}"))
}

fn res_path() -> String {
    std::env::var("BW_RES_PATH").unwrap_or_else(|_| DEFAULT_RES.to_string())
}

fn uid() -> u16 {
    unsafe { libc::getuid() as u16 }
}

fn wait_until(secs: u64, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Byte-substring search over a file, format-agnostic.
fn file_contains(dir: &std::path::Path, file: &str, needle: &str) -> bool {
    std::fs::read(dir.join(file))
        .map(|bytes| {
            let needle = needle.as_bytes();
            bytes.windows(needle.len()).any(|window| window == needle)
        })
        .unwrap_or(false)
}

fn send_all(fd: RawFd, mut data: &[u8]) {
    while !data.is_empty() {
        let n = socket::send(fd, data);
        assert!(n > 0, "tcp send failed: {n}");
        data = &data[n as usize..];
    }
}

/// One framed packet: `[i32 LE length][bytes]` — the
/// `LoggerEndpoint::sendImmediate` wire shape (`logger_endpoint.cpp:501-516`).
fn framed(payload: &[u8]) -> Vec<u8> {
    let mut frame = (payload.len() as i32).to_le_bytes().to_vec();
    frame.extend_from_slice(payload);
    frame
}

/// Parses the `logger/add` watcher set the reference logger pushes at a
/// component: `WatcherDataMsg { int32 message; int32 count; char string[] }`
/// (`watcher_nub.hpp:44-48`), then the path and the address, both NUL
/// terminated. `None` when the datagram is not a well-formed watcher set.
fn parse_logger_add(datagram: &[u8]) -> Option<(String, String)> {
    if datagram.len() < 8 {
        return None;
    }
    let message = i32::from_le_bytes(datagram[0..4].try_into().unwrap());
    if message != WATCHER_MSG_SET {
        return None;
    }
    let count = i32::from_le_bytes(datagram[4..8].try_into().unwrap());
    if count != 1 {
        return None;
    }

    let mut off = 8;
    let read_c_str = |bytes: &[u8], off: &mut usize| -> String {
        let start = *off;
        while *off < bytes.len() && bytes[*off] != 0 {
            *off += 1;
        }
        let text = String::from_utf8_lossy(&bytes[start..*off]).into_owned();
        *off += 1; // the terminator
        text
    };
    let path = read_c_str(datagram, &mut off);
    let value = read_c_str(datagram, &mut off);
    Some((path, value))
}

/// Splits `1.2.3.4:port` into the network-order address and the port.
fn parse_address(value: &str) -> (u32, u16) {
    let (ip, port) = value
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("address without a port: {value}"));
    (
        socket::inet_aton(ip).unwrap_or_else(|| panic!("bad ip: {ip}")),
        port.parse().unwrap_or_else(|_| panic!("bad port: {port}")),
    )
}

/// The record payload a component ships for one log line
/// (`LoggerMessageForwarder::parseAndSend` shape): transport id, header,
/// format string, encoded arguments.
fn record_payload(marker: &str, value: i64) -> Vec<u8> {
    let handler = ForwardingStringHandler::new(&format!("{marker} %d"), false);
    assert!(handler.is_ok(), "format string rejected");
    build_record(
        &MessageHeader {
            component_priority: 0,
            message_priority: message_priority::INFO,
            message_source: message_logger::MessageSource::Cpp,
            category: String::new(),
        },
        &handler,
        &[ArgValue::Int(value)],
    )
}

/// The register envelope a component announces itself with
/// (`LoggerComponentMessage`, `logger_message_forwarder.cpp:540-553`).
fn register_payload(component_name: &str) -> Vec<u8> {
    ComponentMessage {
        version: MESSAGE_LOGGER_VERSION,
        logger_id: String::new(),
        uid: uid(),
        pid: std::process::id(),
        component_name: component_name.to_string(),
    }
    .to_register_envelope()
}

/// A bound loopback UDP endpoint standing in for a component's watcher nub.
fn component_socket() -> (RawFd, u16) {
    let fd = socket::create_socket(socket::SOCK_DGRAM);
    assert!(fd >= 0, "component socket failed");
    // Bound to INADDR_ANY, not loopback: the logger attaches to the source
    // IP the machined reply carried, which on this host is the interface
    // address — a real component's nub is reachable on it too.
    assert_eq!(socket::bind(fd, socket::htons(0), 0), 0);
    socket::set_timeout_option(fd, socket::SO_RCVTIMEO, 1);
    let (port, _) = socket::local_address(fd).expect("component local address");
    (fd, socket::ntohs(port))
}

/// Receives one datagram within the socket timeout.
fn recv_one(fd: RawFd) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 2048];
    let (n, _, _) = socket::recvfrom(fd, &mut buf);
    (n > 0).then(|| buf[..n as usize].to_vec())
}

/// The registry view the pump thread publishes for the test thread.
#[derive(Default, Clone)]
struct FabricView {
    /// `(name, pid, port)` of every registered process.
    processes: Vec<(String, u32, u16)>,
    listeners: usize,
    steps: usize,
}

/// Runs the daemon until `stop` is set, publishing the registry view.
fn pump(mut daemon: MachinedDaemon, stop: Arc<AtomicBool>) -> Arc<Mutex<FabricView>> {
    let view = Arc::new(Mutex::new(FabricView::default()));
    let handle = view.clone();
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            daemon.step();
            let mut view = handle.lock().unwrap();
            view.steps += 1;
            view.processes = daemon
                .snapshot
                .registry
                .processes()
                .iter()
                .map(|entry| {
                    (
                        entry.process.name.clone(),
                        entry.process.pid,
                        entry.process.port,
                    )
                })
                .collect();
            view.listeners = daemon.listeners.len();
        }
    });
    view
}

fn base_snapshot() -> mg::MachinedSnapshot {
    mg::MachinedSnapshot {
        hostname: "mixed-deploy-host".to_string(),
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
        registry: mg::MachinedRegistry::new(),
        user: mg::UserMessage::default(),
    }
}

/// A register command the way every server process announces itself to
/// machined (`machined_utils.cpp registerWithMachined`): the watcher nub
/// category, the owning user, the nub port, and the app versions.
fn register_command(
    pid: u32,
    port: u16,
    name: &str,
    major: u16,
    minor: u16,
) -> mg::MachineGuardMessage {
    mg::MachineGuardMessage::Process(ProcessMessage {
        header: mg::Header {
            message: message_id::PROCESS,
            flags: 0,
            seq: 0,
        },
        param: process_param::IS_MSGTYPE | process_type::REGISTER,
        category: process_category::WATCHER_NUB,
        uid: uid(),
        pid,
        port,
        id: 1,
        name: name.to_string(),
        major_version: major,
        minor_version: minor,
        ..ProcessMessage::default()
    })
}

/// A pid query the way `machined_utils.cpp` builds it.
fn pid_query(pid: u32) -> mg::MachineGuardMessage {
    mg::MachineGuardMessage::Pid(mg::PidMessage {
        header: mg::Header {
            message: message_id::PID,
            flags: 0,
            seq: 7,
        },
        pid,
        running: 0,
    })
}

/// Owns a reference binary for the duration of a leg and kills it when the
/// scope ends, failure or not — a leftover reference daemon would keep port
/// 20018 and poison the reruns, and a leftover logger would keep its sockets.
struct KillOnDrop(std::process::Child);

impl KillOnDrop {
    fn child(&mut self) -> &mut std::process::Child {
        &mut self.0
    }

    fn id(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "requires the reference EL7 binaries"]
fn the_mixed_deploy_rerun_passes_all_legs() {
    let scratch = std::env::temp_dir().join(format!("el7-mixed-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("scratch dir");
    let logdir = scratch.join("mldb");
    std::fs::create_dir_all(&logdir).expect("mldb dir");

    let mut rec: Vec<(String, String)> = Vec::new();

    // Poison guard: nothing else may own the broadcast port while the legs
    // run. A foreign daemon answering a query on 20018 would steal the
    // logger's discovery answers (and a foreign bwmachined2's more specific
    // broadcast bind steals the datagrams), so fail fast instead.
    {
        let probe = socket::create_socket(socket::SOCK_DGRAM);
        assert!(probe >= 0, "poison probe socket failed");
        socket::set_bool_option(probe, socket::SOL_SOCKET, socket::SO_BROADCAST, true);
        socket::set_timeout_option(probe, socket::SO_RCVTIMEO, 2);
        let whole = mg::MachineGuardMessage::WholeMachine(mg::WholeMachineMessage {
            header: mg::Header {
                message: message_id::WHOLE_MACHINE,
                flags: 0,
                seq: 11,
            },
            ..mg::WholeMachineMessage::default()
        });
        let probe_bytes = mg::MgmPacket::new(vec![whole])
            .to_bytes()
            .expect("encode poison probe");
        let mut foreign = Vec::new();
        for destination in [LOOPBACK, u32::from_be_bytes([255, 255, 255, 255])] {
            if socket::sendto(
                probe,
                &probe_bytes,
                socket::htons(PORT_MACHINED),
                destination,
            ) < 0
            {
                continue;
            }
            let mut buf = vec![0u8; 2048];
            let (n, _, _) = socket::recvfrom(probe, &mut buf);
            if n > 0 {
                foreign.push(
                    buf[..n as usize]
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>(),
                );
            }
        }
        socket::close(probe);
        assert!(
            foreign.is_empty(),
            "a foreign machined is already answering on port {PORT_MACHINED} ({foreign:?}); \
             kill the leftover and rerun",
        );
    }

    // ------------------------------------------------------------------
    // Leg 1: new client -> original bwmachined2 (UDP)
    // ------------------------------------------------------------------
    let pidfile = scratch.join("bwmachined2.pid");
    let mut machined = KillOnDrop(
        std::process::Command::new(binary("bwmachined2", "BW_MACHINED_BIN"))
            .arg("-f")
            .arg("-p")
            .arg(&pidfile)
            .current_dir(&scratch)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the reference bwmachined2"),
    );
    std::thread::sleep(Duration::from_millis(700));
    assert!(
        machined.child().try_wait().unwrap().is_none(),
        "the reference bwmachined2 exited at startup (is port 20018 already taken?)"
    );

    let client = MachinedClient::new().expect("client socket");
    let pid_reply = wait_until(12, || {
        matches!(
            client.query(LOOPBACK, PORT_MACHINED, &pid_query(1)),
            Ok(mg::MachineGuardMessage::Pid(pid)) if pid.running == 1
        )
    });
    assert!(
        pid_reply,
        "leg 1: the original daemon did not answer the pid query"
    );

    let whole = mg::MachineGuardMessage::WholeMachine(mg::WholeMachineMessage {
        header: mg::Header {
            message: message_id::WHOLE_MACHINE,
            flags: 0,
            seq: 11,
        },
        ..mg::WholeMachineMessage::default()
    });
    let whole_reply = client
        .query(LOOPBACK, PORT_MACHINED, &whole)
        .expect("leg 1: whole-machine reply");
    assert!(matches!(
        whole_reply,
        mg::MachineGuardMessage::WholeMachine(_)
    ));
    rec.push(("leg1_pid_query_running".into(), "1".into()));
    rec.push(("leg1_whole_machine_reply".into(), "1".into()));

    drop(machined);
    let _ = std::fs::remove_file(&pidfile);

    // ------------------------------------------------------------------
    // Legs 2-5: the original logger against the new machined fabric
    // ------------------------------------------------------------------
    let daemon = MachinedDaemon::bind(PORT_MACHINED, 0, base_snapshot())
        .expect("bind the new machined daemon (is port 20018 still taken?)");
    let stop = Arc::new(AtomicBool::new(false));
    let view = pump(daemon, stop.clone());

    // Two fake components: 2.7 gets the TCP attach, 2.5 the UDP attach —
    // the original logger's FindHandler version gate
    // (`logger.cpp SHOULD_USE_TCP` / `SHOULD_USE_UDP`).
    let tcp_component = component_socket();
    let udp_component = component_socket();
    let reg_client = MachinedClient::new().expect("register client");
    let register = |name: &str, port: u16, major: u16, minor: u16, tag: u16| {
        let pid = std::process::id().wrapping_add(u32::from(tag));
        let reply = reg_client
            .query(
                LOOPBACK,
                PORT_MACHINED,
                &register_command(pid, port, name, major, minor),
            )
            .expect("register accepted by the new daemon");
        assert!(matches!(reply, mg::MachineGuardMessage::Process(_)));
        pid
    };
    let _tcp_pid = register("cellapp01", tcp_component.1, 2, 7, 1);
    let _udp_pid = register("baseapp01", udp_component.1, 2, 5, 2);

    // The original logger. Its startup talks to the new daemon: the
    // watcher registration, the birth and death listener registrations,
    // then the discovery broadcast.
    let config = format!(
        "[message_logger]\ngroups=\n\n[mldb]\nsegment_size=32\nlogdir={}\n",
        logdir.display()
    );
    let config_path = scratch.join("message_logger.conf");
    std::fs::write(&config_path, &config).expect("write the logger config");
    let stderr_path = scratch.join("logger.stderr");
    let stderr = std::fs::File::create(&stderr_path).expect("logger stderr file");
    let mut logger = KillOnDrop(
        std::process::Command::new(binary("message_logger", "BW_MESSAGE_LOGGER_BIN"))
            .arg("-v")
            .arg("-c")
            .arg(&config_path)
            .arg(&logdir)
            .current_dir(&logdir)
            .env("BW_RES_PATH", res_path())
            .stderr(stderr)
            .spawn()
            .expect("spawn the reference message_logger"),
    );

    let assert_alive = |logger: &mut KillOnDrop, where_: &str| {
        if let Some(status) = logger.child().try_wait().expect("try_wait") {
            let tail = std::fs::read_to_string(&stderr_path).unwrap_or_default();
            panic!("the reference logger exited early ({where_}): {status}\n{tail}");
        }
    };

    // Leg 2: the logger registered itself with the new daemon and left its
    // birth and death listeners.
    let logger_pid = logger.id();
    let registered = wait_until(10, || {
        view.lock()
            .unwrap()
            .processes
            .iter()
            .any(|(name, pid, _)| name == "message_logger" && *pid == logger_pid)
    });
    assert_alive(&mut logger, "registering with the new machined");
    assert!(
        registered,
        "leg 2: the original logger is not in the new daemon's registry: {:?}",
        view.lock().unwrap().processes
    );
    let listeners = wait_until(5, || view.lock().unwrap().listeners >= 2);
    assert_alive(&mut logger, "registering the birth/death listeners");
    assert!(listeners, "leg 2: the birth/death listeners did not arrive");

    // Leg 3: the discovery broadcast finds both fake components and the
    // attach pushes arrive on their sockets.
    let mut tcp_add = None;
    let mut udp_add = None;
    let deadline = Instant::now() + Duration::from_secs(15);
    while (tcp_add.is_none() || udp_add.is_none()) && Instant::now() < deadline {
        if tcp_add.is_none() {
            tcp_add = recv_one(tcp_component.0)
                .map(|bytes| parse_logger_add(&bytes).map(|parsed| (bytes, parsed)));
        }
        if udp_add.is_none() {
            udp_add = recv_one(udp_component.0)
                .map(|bytes| parse_logger_add(&bytes).map(|parsed| (bytes, parsed)));
        }
    }
    assert_alive(&mut logger, "discovering the components");
    let Some((tcp_bytes, (tcp_path, tcp_value))) =
        tcp_add.expect("leg 3: no attach push for the 2.7 component")
    else {
        panic!("leg 3: the attach push for the 2.7 component was not a watcher set");
    };
    let Some((udp_bytes, (udp_path, udp_value))) =
        udp_add.expect("leg 3: no attach push for the 2.5 component")
    else {
        panic!("leg 3: the attach push for the 2.5 component was not a watcher set");
    };
    assert_eq!(
        tcp_path, "logger/addTCP",
        "the 2.7 component gets the TCP flavor"
    );
    assert_eq!(
        udp_path, "logger/add",
        "the 2.5 component gets the UDP flavor"
    );
    rec.push(("leg3_addtcp_path".into(), tcp_path));
    rec.push(("leg3_add_path".into(), udp_path));
    rec.push((
        "leg3_push_bytes".into(),
        format!("tcp={}/udp={}", tcp_bytes.len(), udp_bytes.len()),
    ));

    // Leg 4: connect TCP to the pushed address and ship the register plus
    // the records, framed.
    let (ip, port) = parse_address(&tcp_value);
    let tcp_fd = socket::create_socket(socket::SOCK_STREAM);
    assert!(tcp_fd >= 0, "tcp socket failed");
    assert_eq!(
        socket::connect(tcp_fd, socket::htons(port), ip),
        0,
        "leg 4: the logger refused the TCP connection"
    );
    let register = register_payload("cellapp01");
    let records: Vec<Vec<u8>> = (0..4)
        .map(|line| record_payload("mixed deploy leg four line", line))
        .collect();
    let mut shipped = framed(&register);
    for payload in &records {
        shipped.extend(framed(payload));
    }
    send_all(tcp_fd, &shipped);
    rec.push(("leg4_register_bytes".into(), register.len().to_string()));
    rec.push(("leg4_record_frames".into(), records.len().to_string()));
    rec.push((
        "leg4_record_bytes_each".into(),
        records[0].len().to_string(),
    ));

    // Leg 5: the 2.5 component ships the same envelopes as plain datagrams,
    // to the UDP endpoint the `logger/add` push advertised.
    let (udp_ip, udp_port) = parse_address(&udp_value);
    let udp_register = register_payload("baseapp01");
    let udp_records: Vec<Vec<u8>> = (0..3)
        .map(|line| record_payload("mixed deploy leg five line", line))
        .collect();
    for payload in std::iter::once(&udp_register).chain(udp_records.iter()) {
        let n = socket::sendto(udp_component.0, payload, socket::htons(udp_port), udp_ip);
        assert_eq!(n as usize, payload.len(), "leg 5: udp sendto short");
    }
    rec.push(("leg5_record_frames".into(), udp_records.len().to_string()));

    // The original logger stores the component and the strings in its mldb
    // output. The flush cycle is periodic, so poll for a while.
    let component_stored = wait_until(20, || {
        file_contains(&logdir, "component_names", "cellapp01")
    });
    let string_stored = wait_until(20, || {
        file_contains(&logdir, "strings", "mixed deploy leg four line")
    });
    assert_alive(&mut logger, "storing the records");
    assert!(
        component_stored,
        "leg 4: the original logger never stored the component: {:?}",
        std::fs::read_dir(&logdir).map(|entries| entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect::<Vec<_>>())
    );
    assert!(
        string_stored,
        "leg 4: the TCP records never reached the strings table"
    );
    let udp_string_stored = wait_until(20, || {
        file_contains(&logdir, "strings", "mixed deploy leg five line")
    });
    assert!(
        udp_string_stored,
        "leg 5: the UDP records never reached the strings table"
    );
    let strings_bytes = std::fs::metadata(logdir.join("strings"))
        .map(|m| m.len())
        .unwrap_or(0);
    rec.push(("mldb_component_found".into(), "1".into()));
    rec.push(("mldb_tcp_strings_found".into(), "1".into()));
    rec.push(("mldb_udp_strings_found".into(), "1".into()));
    rec.push(("mldb_strings_bytes".into(), strings_bytes.to_string()));

    drop(logger);
    socket::close(tcp_fd);
    socket::close(tcp_component.0);
    socket::close(udp_component.0);
    stop.store(true, Ordering::Relaxed);

    rec.push(("fabric_registry_entries".into(), {
        let view = view.lock().unwrap();
        view.processes.len().to_string()
    }));
    rec.push((
        "protocol_version".into(),
        MESSAGE_LOGGER_VERSION.to_string(),
    ));
    rec.push(("component_versions".into(), "2.7/tcp,2.5/udp".into()));

    if let Ok(path) = std::env::var("EL7_MIXED_OUT") {
        let mut text = String::new();
        for (key, value) in &rec {
            text.push_str(key);
            text.push('\t');
            text.push_str(value);
            text.push('\n');
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .expect("write EL7_MIXED_OUT");
        println!("record: {path}");
    }
    let _ = std::io::stdout().flush();
}

/// Non-Linux platforms: the reference binaries are EL7 Linux artifacts.
#[cfg(not(target_os = "linux"))]
#[test]
fn the_reference_binaries_are_linux_artifacts() {
    println!(
        "the reference deployment is linux-only; platform is {}",
        std::env::consts::OS
    );
}
