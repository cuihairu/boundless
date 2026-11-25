//! BW-14-02-01 the LoginApp's external interface: the `LoginInterface`
//! elements served on the role's own socket, and the `probe` message the
//! server-discovery path answers.
//!
//! The reference `LoginApp` opens a second `NetworkInterface` next to the
//! role loop's internal one (`extInterface_`, loginapp.cpp:114-124), on
//! the port `getExternalPort` reads from `loginApp/externalPorts/port`
//! (loginapp.cpp:107-117), and serves the three `LoginInterface` messages
//! on it (`lib/connection/login_interface.hpp`):
//!
//! - `login`, `MERCURY_VARIABLE_MESSAGE( login, 2 )` -- the login chain
//!   itself;
//! - `probe`, `MERCURY_EMPTY_MESSAGE( probe )` -- the discovery probe the
//!   client's server browser fires (`server_discovery.cpp:286-293`);
//! - `challengeResponse`, `MERCURY_VARIABLE_MESSAGE( challengeResponse,
//!   2 )` -- the client's answer to an issued challenge.
//!
//! The ids follow the declaration order, which is how the reference
//! `InterfaceMinder` hands them out (`interface_minder.cpp:26-40`,
//! `id = elements_.size()`): login 0, probe 1, challengeResponse 2. The
//! `2` in the `MERCURY_VARIABLE_MESSAGE` macro is the width of the length
//! field (`interface_macros.hpp`), not an id.
//!
//! The two login-chain faces are not wired to this interface yet (they
//! drive the challenge/billing items through `parse_login_request`), so
//! their ids stay unserved: an off-id bundle lands on the dispatch walk's
//! unhandled-id arm, the place an unserved id lands in the reference too.
//!
//! The reference constructor also owns the TCP server and the
//! prescribed-port loop (`bindToPrescribedPort` reads every
//! `<port>` entry and tries each, server_app.cpp:587-670); both arrive
//! with the login stream filter work, so this port keeps the single
//! `getExternalPort` bind -- whose default behaviour on an occupied port
//! matches the reference's `shouldShutDownIfPortUsed` (default true): no
//! random-port fallback, the init check refuses.

use std::cell::Cell;
use std::rc::{Rc, Weak};

use foundation::stream::BinaryStream;
use mercury::bundle_dispatch::dispatch_messages;
use mercury::endpoint::Address;
use mercury::event_dispatcher::EventDispatcher;
use mercury::interface_element::{FIXED_LENGTH_MESSAGE, VARIABLE_LENGTH_MESSAGE};
use mercury::interface_minder::InterfaceMinder;
use mercury::interface_table::InterfaceTable;
use mercury::interfaces::InputMessageHandler;
use mercury::misc::Reason;
use mercury::network_interface::{InterfaceType, NetworkInterface};
use mercury::packet::{HEADER_SIZE, Packet, flags};
use mercury::packet_receiver::PacketSink;
use mercury::udp_bundle::{ReliableType, UdpBundle};
use mercury::unpacked_message_header::UnpackedMessageHeader;
use platform::socket;
use resources::bwresource::current_uid;

use crate::config::BwConfig;
use crate::loginapp_login::{
    ChallengeResponseHandler, LoginFaces, LoginFacesConfig, LoginRequestHandler, NoLogOnForward,
};

/// `PORT_LOGIN` (`lib/network/portmap.hpp:7`), the login interface's
/// default external port.
pub const PORT_LOGIN: u16 = 20013;

/// `PROBE_KEY_HOST_NAME` (`login_interface.hpp:23`).
const PROBE_KEY_HOST_NAME: &str = "hostName";

/// `PROBE_KEY_OWNER_NAME` (`login_interface.hpp:24`).
const PROBE_KEY_OWNER_NAME: &str = "ownerName";

/// `PROBE_KEY_USERS_COUNT` (`login_interface.hpp:25`).
const PROBE_KEY_USERS_COUNT: &str = "usersCount";

/// The LoginApp options the external interface and the probe reply read,
/// each with the default its `BW_OPTION` declares
/// (`loginapp_config.cpp:25-26`, `external_app_config.cpp:19`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginAppConfig {
    /// `loginApp/externalPorts/port`, default `PORT_LOGIN`; `getExternalPort`
    /// reads the first value at this path (`BWConfig::get`, the multi-value
    /// `readInts` loop belongs to the unported prescribed-port helper).
    pub external_port: u32,
    /// `loginApp/externalInterface`, default `""`.
    pub external_interface: String,
    /// `loginApp/allowProbe`, default false.
    pub allow_probe: bool,
    /// `loginApp/logProbes`, default true.
    pub log_probes: bool,
    /// `loginApp/shouldOffsetExternalPortByUID`, default false; when set,
    /// `getExternalPort` adds `getUserId()` to the configured port.
    pub should_offset_external_port_by_uid: bool,
}

impl LoginAppConfig {
    /// The options as the config chain hands them over.
    pub fn from_config(config: &BwConfig) -> Self {
        Self {
            external_port: config.get_uint("loginApp/externalPorts/port", PORT_LOGIN as u32),
            external_interface: config.get_string("loginApp/externalInterface", ""),
            allow_probe: config.get_bool("loginApp/allowProbe", false),
            log_probes: config.get_bool("loginApp/logProbes", true),
            should_offset_external_port_by_uid: config
                .get_bool("loginApp/shouldOffsetExternalPortByUID", false),
        }
    }

    /// `getExternalPort` (loginapp.cpp:107-117): the configured port, the
    /// uid offset folded in when the option asks, as the wire sees it --
    /// the reference returns `htons( static_cast< uint16 >( port ) )`.
    pub fn external_port(&self) -> u16 {
        self.external_port_for(current_uid())
    }

    /// [`LoginAppConfig::external_port`] over an explicit uid, so the
    /// offset arm is drivable without rewiring the environment.
    fn external_port_for(&self, uid: u32) -> u16 {
        let mut port = self.external_port;
        if self.should_offset_external_port_by_uid {
            port += uid;
        }
        // `static_cast< uint16 >( port )` truncates; the wire carries the
        // byte-swapped value.
        socket::htons(port as u16)
    }
}

/// Why the external interface refused to come up. The verbatim reference
/// ERROR lines are logged where they fire; this carries the refusal to
/// `main`'s exit path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindError {
    /// `!extInterface_.isGood()` (loginapp.cpp:199-206).
    ExternalInterface,
    /// `extInterface_.address().ip == 0` (loginapp.cpp:212-218).
    UdpPorts,
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExternalInterface => {
                write!(
                    f,
                    "unable to bind the external interface to any specified port"
                )
            }
            Self::UdpPorts => write!(f, "failed to open the external UDP port"),
        }
    }
}

impl std::error::Error for BindError {}

/// The bound external interface: the socket -- whose sink walks the
/// served table -- and the login count the probe reports.
pub struct LoginAppInterface {
    /// `extInterface_`.
    interface: Rc<NetworkInterface>,
    /// `gNumLogins` (loginapp.cpp:70): incremented by the login chain
    /// (loginapp.cpp:1329), reported by the probe (loginapp.cpp:1185).
    users_count: Rc<Cell<u32>>,
    /// The `login` / `challengeResponse` faces served over the same
    /// interface, sharing `gNumLogins` with the probe.
    faces: Rc<LoginFaces>,
}

impl LoginAppInterface {
    /// The socket, kept alive by the caller for the loop's lifetime.
    pub fn interface(&self) -> &Rc<NetworkInterface> {
        &self.interface
    }

    /// `gNumLogins`.
    pub fn users_count(&self) -> u32 {
        self.users_count.get()
    }

    /// The login faces, for the DBApp registration faces and the tests
    /// that drive them.
    pub fn faces(&self) -> &Rc<LoginFaces> {
        &self.faces
    }
}

/// The `LoginApp` constructor's external half (loginapp.cpp:114-135):
/// open the interface on the configured port and serve the elements onto
/// its dispatch table. A failed bind leaves the interface not-good,
/// exactly as the reference member initialisation leaves `extInterface_`;
/// [`check`] reports it at the `init` position.
///
/// `faces_config` is the `loginApp` section read twice by the reference:
/// once by the probe and the interface options, once more by the login
/// chain's gates (`LoginApp`'s `Config::` reads).
pub fn open(
    config: &LoginAppConfig,
    faces_config: LoginFacesConfig,
    dispatcher: Rc<EventDispatcher>,
) -> Rc<LoginAppInterface> {
    let (interface, _receiver) = NetworkInterface::new_on_dispatcher(
        InterfaceType::External,
        dispatcher,
        config.external_port(),
        &config.external_interface,
    );

    let table = Rc::new(InterfaceTable::new());
    let users_count = Rc::new(Cell::new(0));
    let faces = Rc::new(LoginFaces::new(
        faces_config,
        Rc::downgrade(&interface),
        Rc::clone(&users_count),
        Rc::new(NoLogOnForward),
    ));
    let probe = Rc::new(ProbeHandler {
        log_probes: config.log_probes,
        allow_probe: config.allow_probe,
        users_count: Rc::clone(&users_count),
        interface: Rc::downgrade(&interface),
    });
    let minder = login_minder(
        Some(Rc::new(LoginRequestHandler(Rc::clone(&faces)))),
        Some(probe),
        Some(Rc::new(ChallengeResponseHandler(Rc::clone(&faces)))),
    );
    minder.register_with_interface(&table);

    // The off-channel datagrams reach the table through this sink; the
    // interface keeps it until the interface itself dies.
    interface.set_packet_sink(Some(Rc::new(LoginInterfaceDispatch {
        table: Rc::clone(&table),
    })));

    Rc::new(LoginAppInterface {
        interface,
        users_count,
        faces,
    })
}

/// The `LoginApp::init` arms over the bound interface
/// (loginapp.cpp:199-214): log the reference's ERROR line and refuse.
/// The port's `run` calls it after the startup line, where the
/// reference's `LoginApp::init` runs after `ServerApp::init`'s.
pub fn check(app: &LoginAppInterface) -> Result<(), BindError> {
    check_interface(app.interface.is_good(), app.interface.address().ip)
}

/// The two `init` arms over the observable facts, split so a test can
/// drive each refusal without a broken host.
fn check_interface(is_good: bool, ip: u32) -> Result<(), BindError> {
    if !is_good {
        // The lowercase "app" typo is the reference's own
        // (loginapp.cpp:203-205).
        eprintln!("Loginapp::init: Unable to bind external interface to any specified ports.");
        return Err(BindError::ExternalInterface);
    }
    if ip == 0 {
        eprintln!(
            "LoginApp::init: Failed to open UDP ports. Maybe another process already owns it"
        );
        return Err(BindError::UdpPorts);
    }
    Ok(())
}

/// The `LoginInterface` elements (`login_interface.hpp`), in declaration
/// order -- which is the id order: `login` gets `LoginApp::login`'s face,
/// `probe` the handler that answers it, `challengeResponse` the face that
/// verifies the client's challenge answer.
fn login_minder(
    login: Option<Rc<dyn InputMessageHandler>>,
    probe: Option<Rc<dyn InputMessageHandler>>,
    challenge: Option<Rc<dyn InputMessageHandler>>,
) -> InterfaceMinder {
    let minder = InterfaceMinder::new("LoginInterface");
    minder.add("login", VARIABLE_LENGTH_MESSAGE, 2, login);
    minder.add("probe", FIXED_LENGTH_MESSAGE, 0, probe);
    minder.add("challengeResponse", VARIABLE_LENGTH_MESSAGE, 2, challenge);
    minder
}

/// `LoginApp::probe` (loginapp.cpp:1143-1193): answer the discovery
/// probe with the three string pairs the client's server browser reads
/// (`server_discovery.cpp:40-70`).
struct ProbeHandler {
    /// `Config::logProbes()`.
    log_probes: bool,
    /// `Config::allowProbe()`.
    allow_probe: bool,
    /// `gNumLogins`.
    users_count: Rc<Cell<u32>>,
    /// `extInterface_`, the interface the reply leaves on; weak so the
    /// table the interface serves cannot pin it.
    interface: Weak<NetworkInterface>,
}

impl InputMessageHandler for ProbeHandler {
    fn handle_message(
        &self,
        source: &Address,
        header: &mut UnpackedMessageHeader,
        _data: &mut BinaryStream,
    ) {
        if self.log_probes {
            eprintln!("LoginApp::probe: Got probe from {}", source.c_str());
        }

        if !self.allow_probe || header.length() != 0 {
            return;
        }

        let Some(interface) = self.interface.upgrade() else {
            return;
        };

        let mut bundle = UdpBundle::new(0);
        bundle.start_reply(header.reply_id(), ReliableType::No);

        // The reference streams the pairs straight into the bundle
        // (`bundle << key << value`, the packed-length string form); the
        // port's bundle takes raw bytes, so the pairs are built in a
        // memory stream and handed over whole.
        let mut body = BinaryStream::new();
        stream_pair(&mut body, PROBE_KEY_HOST_NAME, &hostname());
        stream_pair(&mut body, PROBE_KEY_OWNER_NAME, &owner_name());
        stream_pair(
            &mut body,
            PROBE_KEY_USERS_COUNT,
            &self.users_count.get().to_string(),
        );
        bundle.write_bytes(body.as_bytes());

        // `extInterface_.send( source, bundle )`: off-channel, no
        // channel to ride.
        interface.send(source, &mut bundle, None);
    }
}

/// `bundle << key << value` (`binary_stream.hpp:295-325`): the packed
/// length prefix and the raw bytes, for one probe pair.
fn stream_pair(body: &mut BinaryStream, key: &str, value: &str) {
    body.write_string(key)
        .expect("a memory stream cannot overflow");
    body.write_string(value)
        .expect("a memory stream cannot overflow");
}

/// `gethostname( buf, sizeof(buf) )` (loginapp.cpp:1170-1171): the
/// kernel's host name, into the reference's 256-byte buffer, whose last
/// byte the reference NUL-terminates whatever the call did
/// (`buf[sizeof(buf)-1]=0`) -- so a truncated name still reads, and a
/// failed call reads as empty.
fn hostname() -> String {
    let mut buf = vec![0u8; 256];
    unsafe {
        libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len());
    }
    buf[255] = 0;
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// `getpwuid( getUserId() )` (loginapp.cpp:1173-1181): the account name
/// the process runs under, or the reference's miss -- the empty string
/// plus the ERROR line (`loginapp.cpp:1179-1182`).
fn owner_name() -> String {
    owner_name_for(current_uid())
}

/// [`owner_name`] over an explicit uid, so the miss arm is drivable
/// without rewiring the environment.
fn owner_name_for(uid: u32) -> String {
    match crate::config::passwd_name(uid) {
        Some(name) => name,
        None => {
            eprintln!("LoginApp::probe: Process uid {uid} doesn't exist on this system!");
            String::new()
        }
    }
}

/// The off-channel dispatch sink over the served table. The on-channel
/// path strips its footers channel-side and hands the walk the
/// first-request offset it stashed (`udp_channel.rs:1104-1112`,
/// `3392-3408`); an off-channel datagram arrives with every footer still
/// on, so this sink strips the flags word and -- when the bundle carried
/// a request -- the first-request footer itself, with the same bounds
/// check the channel side applies.
struct LoginInterfaceDispatch {
    table: Rc<InterfaceTable>,
}

impl PacketSink for LoginInterfaceDispatch {
    fn process_packet(&self, addr: &Address, packet: &mut Packet) -> Reason {
        let first_request = if packet.has_flags(flags::HAS_REQUESTS) {
            match packet.strip_footer_u16() {
                // The footer value counts from the flags word; the walk
                // translates it against the body slice itself.
                Some(offset) if offset as usize <= packet.msg_end_offset() => Some(offset as usize),
                _ => return Reason::CorruptedPacket,
            }
        } else {
            None
        };
        let body = &mut packet.data_mut()[HEADER_SIZE..];
        dispatch_messages(&self.table, addr, body, first_request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use mercury::interface_element::REPLY_MESSAGE_IDENTIFIER;

    const LOOPBACK: u32 = 0x0100_007f;

    /// A `std` client socket bound on the loopback, as the discovery
    /// probe's sender stands on the wire.
    fn client_socket() -> std::net::UdpSocket {
        let client = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind the client");
        client
            .set_read_timeout(Some(std::time::Duration::from_millis(300)))
            .expect("arm the client timeout");
        client
    }

    /// The client socket's address in Mercury terms.
    fn client_address(client: &std::net::UdpSocket) -> Address {
        let addr = client.local_addr().expect("client address");
        Address::new(
            socket::inet_aton("127.0.0.1").expect("loopback parses"),
            socket::htons(addr.port()),
        )
    }

    /// Try to read one datagram; `None` after the timeout.
    fn recv(client: &std::net::UdpSocket) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; 2048];
        match client.recv_from(&mut buf) {
            Ok((n, _)) => Some(buf[..n].to_vec()),
            Err(_) => None,
        }
    }

    /// The probe request as `ServerDiscovery::MachineInfoCallback`'s
    /// `startRequest` puts it on the wire (server_discovery.cpp:286-293,
    /// `udp_bundle.rs`'s request framing): the flags word with
    /// `HAS_REQUESTS`, the fixed-length probe's one id byte (no length
    /// field -- the length is the element's own 0), the reply id and
    /// next-request link a request carries, and the first-request footer
    /// the send fills with the frame's packet-absolute offset. The reply
    /// id stays zero, what a manager-less send leaves on the wire.
    fn probe_request_datagram() -> Vec<u8> {
        let mut datagram = Vec::new();
        datagram.extend_from_slice(&flags::HAS_REQUESTS.to_le_bytes());
        datagram.push(1); // the probe element's id
        datagram.extend_from_slice(&0i32.to_le_bytes()); // the reply id
        datagram.extend_from_slice(&0u16.to_le_bytes()); // no next request
        datagram.extend_from_slice(&(HEADER_SIZE as u16).to_le_bytes());
        datagram
    }

    /// The string pairs a probe reply carries, read the way the client's
    /// reply handler does (`server_discovery.cpp:44-52`).
    fn probe_pairs(reply: &[u8]) -> Vec<(String, String)> {
        // [flags 2][reply id byte][4-byte length][4-byte reply id][body].
        assert_eq!(reply[0], 0, "an off-channel reply carries no flags");
        assert_eq!(reply[1], 0);
        assert_eq!(
            reply[2], REPLY_MESSAGE_IDENTIFIER,
            "the reply element's id byte"
        );
        let length = u32::from_le_bytes(reply[3..7].try_into().unwrap()) as usize;
        let reply_id = i32::from_le_bytes(reply[7..11].try_into().unwrap());
        assert_eq!(
            reply_id, 0,
            "the id the manager-less request carried on the wire"
        );
        // The length counts the four reply-id bytes with the body.
        let mut body = BinaryStream::from_slice(&reply[11..11 + length - 4]);
        let mut pairs = Vec::new();
        while body.remaining() != 0 {
            let key = body.read_string().expect("a key reads");
            let value = body.read_string().expect("a value reads");
            pairs.push((key, value));
        }
        pairs
    }

    /// The reply frame's body: `[flags 2][reply id byte][4-byte length]
    /// [4-byte reply id][body]`, the length counting the reply id with
    /// the body. Answers `(reply id, body)`, the body copied out.
    fn reply_frame(reply: &[u8]) -> (i32, Vec<u8>) {
        assert_eq!(
            u16::from_le_bytes(reply[0..2].try_into().unwrap()),
            0,
            "an off-channel reply carries no flags"
        );
        assert_eq!(reply[2], REPLY_MESSAGE_IDENTIFIER, "the reply id byte");
        let length = u32::from_le_bytes(reply[3..7].try_into().unwrap()) as usize;
        let reply_id = i32::from_le_bytes(reply[7..11].try_into().unwrap());
        (reply_id, reply[11..11 + length - 4].to_vec())
    }

    // -- config --------------------------------------------------------

    /// The defaults are the reference's `BW_OPTION` defaults
    /// (`loginapp_config.cpp:25-26`, `portmap.hpp:7`).
    #[test]
    fn defaults_match_the_reference() {
        let config = LoginAppConfig::from_config(&BwConfig::new());
        assert_eq!(config.external_port, PORT_LOGIN as u32);
        assert_eq!(config.external_interface, "");
        assert!(!config.allow_probe);
        assert!(config.log_probes);
        assert!(!config.should_offset_external_port_by_uid);
        // And `getExternalPort` hands the default to the wire.
        assert_eq!(config.external_port(), socket::htons(PORT_LOGIN));
    }

    /// A `loginApp` section overrides each default, and the uid offset
    /// extends the configured port the way `getExternalPort` does
    /// (loginapp.cpp:109-113).
    #[test]
    fn a_configured_login_app_section_overrides_the_defaults() {
        static TEMP_COUNTER: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);
        let tag = TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("loginapp-config-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(root.join("server")).expect("mkdir the config root");
        std::fs::write(
            root.join("server/bw.xml"),
            "<root>\n\
             \t<loginApp>\n\
             \t\t<externalPorts>\n\
             \t\t\t<port>21114</port>\n\
             \t\t\t<port>21115</port>\n\
             \t\t</externalPorts>\n\
             \t\t<externalInterface>eth9</externalInterface>\n\
             \t\t<allowProbe>true</allowProbe>\n\
             \t\t<logProbes>false</logProbes>\n\
             \t\t<shouldOffsetExternalPortByUID>true</shouldOffsetExternalPortByUID>\n\
             \t</loginApp>\n\
             </root>\n",
        )
        .expect("write the fixture bw.xml");
        let resource = resources::bwresource::BwResource::init_paths(&[&root.to_string_lossy()]);
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, Some("user"));

        let login = LoginAppConfig::from_config(&config);
        assert_eq!(login.external_port, 21114, "the first port entry wins");
        assert_eq!(login.external_interface, "eth9");
        assert!(login.allow_probe);
        assert!(!login.log_probes);
        assert!(login.should_offset_external_port_by_uid);

        // `port += getUserId()` over an explicit uid (loginapp.cpp:111).
        assert_eq!(
            login.external_port_for(20),
            socket::htons(21114 + 20),
            "the uid offset extends the configured port"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The external port is handed to the wire byte-swapped, the
    /// reference's `htons( static_cast< uint16 >( port ) )`.
    #[test]
    fn the_external_port_reaches_the_wire_swapped() {
        let config = LoginAppConfig {
            external_port: 21114,
            external_interface: String::new(),
            allow_probe: false,
            log_probes: true,
            should_offset_external_port_by_uid: false,
        };
        assert_eq!(config.external_port(), socket::htons(21114));
    }

    // -- elements ------------------------------------------------------

    /// The three elements take ids 0, 1, 2 in declaration order, with
    /// the styles the reference declares (`login_interface.hpp`); a
    /// minder built without the login-chain handlers serves only the
    /// probe.
    #[test]
    fn the_login_interface_elements_match_the_reference() {
        let probe: Rc<dyn InputMessageHandler> = Rc::new(ProbeHandler {
            log_probes: true,
            allow_probe: false,
            users_count: Rc::new(Cell::new(0)),
            interface: Weak::new(),
        });
        let minder = login_minder(None, Some(probe), None);
        assert_eq!(minder.len(), 3, "login, probe, challengeResponse");

        let login = minder.interface_element(0);
        assert_eq!(login.name(), "login");
        assert_eq!(login.length_style(), VARIABLE_LENGTH_MESSAGE);
        assert_eq!(login.length_param(), 2);
        assert!(login.handler().is_none(), "no handler was supplied");

        let probe_element = minder.interface_element(1);
        assert_eq!(probe_element.name(), "probe");
        assert_eq!(probe_element.length_style(), FIXED_LENGTH_MESSAGE);
        assert_eq!(probe_element.length_param(), 0);
        assert!(probe_element.handler().is_some(), "the probe is served");

        let challenge = minder.interface_element(2);
        assert_eq!(challenge.name(), "challengeResponse");
        assert_eq!(challenge.length_style(), VARIABLE_LENGTH_MESSAGE);
        assert_eq!(challenge.length_param(), 2);
        assert!(
            challenge.handler().is_none(),
            "the challengeResponse face is unported"
        );
    }

    // -- init arms -----------------------------------------------------

    /// The `init` arms refuse with the mapped error, each behind its
    /// observable fact (loginapp.cpp:199-218).
    #[test]
    fn the_init_arms_refuse_behind_their_facts() {
        assert_eq!(
            check_interface(false, 0x0100_007f),
            Err(BindError::ExternalInterface),
            "a not-good interface refuses first"
        );
        assert_eq!(
            check_interface(true, 0),
            Err(BindError::UdpPorts),
            "a bound interface without an address refuses second"
        );
        assert_eq!(check_interface(true, 0x0100_007f), Ok(()));
    }

    /// The refusals name themselves, for `main`'s exit line.
    #[test]
    fn the_bind_error_names_itself() {
        assert_eq!(
            BindError::ExternalInterface.to_string(),
            "unable to bind the external interface to any specified port"
        );
        assert_eq!(
            BindError::UdpPorts.to_string(),
            "failed to open the external UDP port"
        );
    }

    /// An occupied port leaves the interface not-good and `check`
    /// refuses -- the reference's `shouldShutDownIfPortUsed` default
    /// takes the same shape: no random-port fallback.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_occupied_port_refuses_at_the_init_arms() {
        use mercury::endpoint::Endpoint;

        // Occupy an ephemeral port, then ask for it by host port.
        let mut blocker = Endpoint::new();
        blocker.socket(socket::SOCK_DGRAM);
        assert_eq!(blocker.bind(socket::htons(0), LOOPBACK), 0);
        let mut bound_port = 0u16;
        let mut bound_ip = 0u32;
        assert_eq!(
            blocker.local_address(Some(&mut bound_port), Some(&mut bound_ip)),
            0,
            "the blocker is bound"
        );

        let config = LoginAppConfig {
            external_port: socket::ntohs(bound_port) as u32,
            external_interface: "127.0.0.1".to_string(),
            allow_probe: false,
            log_probes: true,
            should_offset_external_port_by_uid: false,
        };
        assert_ne!(bound_ip, 0);
        let login = open(
            &config,
            crate::loginapp_login::LoginFacesConfig::new(),
            Rc::new(EventDispatcher::new()),
        );
        assert!(!login.interface().is_good(), "the bind failed");
        assert_eq!(check(&login), Err(BindError::ExternalInterface));
    }

    // -- probe handler -------------------------------------------------

    /// A probe the default config allows nothing of: `allowProbe`
    /// defaults false, so the handler swallows the message and the
    /// sender reads silence (loginapp.cpp:1165).
    #[test]
    fn a_disallowed_probe_gets_no_reply() {
        let client = client_socket();
        let handler = ProbeHandler {
            log_probes: false,
            allow_probe: false,
            users_count: Rc::new(Cell::new(3)),
            interface: Weak::new(),
        };
        let mut header = UnpackedMessageHeader::new(1);
        header.set_length(0);
        handler.handle_message(
            &client_address(&client),
            &mut header,
            &mut BinaryStream::new(),
        );
        assert!(recv(&client).is_none(), "no reply without allowProbe");
    }

    /// A probe carrying a body is swallowed the same way
    /// (`header.length != 0`, loginapp.cpp:1165).
    #[test]
    fn a_probe_with_a_body_gets_no_reply() {
        let client = client_socket();
        let handler = ProbeHandler {
            log_probes: false,
            allow_probe: true,
            users_count: Rc::new(Cell::new(3)),
            interface: Weak::new(),
        };
        let mut header = UnpackedMessageHeader::new(1);
        header.set_length(1);
        handler.handle_message(
            &client_address(&client),
            &mut header,
            &mut BinaryStream::new(),
        );
        assert!(recv(&client).is_none(), "a body disqualifies the probe");
    }

    /// A live probe is answered on the handler's own interface: the
    /// reply frame is `[flags][reply id byte][length][reply id][body]`
    /// and the body is the three string pairs, with `usersCount` the
    /// shared `gNumLogins` (loginapp.cpp:1185).
    #[test]
    fn an_allowed_probe_is_answered_with_the_three_pairs() {
        let client = client_socket();
        let interface = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1").0;
        let users_count = Rc::new(Cell::new(7));
        let handler = ProbeHandler {
            log_probes: false,
            allow_probe: true,
            users_count: Rc::clone(&users_count),
            interface: Rc::downgrade(&interface),
        };
        let mut header = UnpackedMessageHeader::new(1);
        header.set_length(0);
        header.set_reply_id(0);
        handler.handle_message(
            &client_address(&client),
            &mut header,
            &mut BinaryStream::new(),
        );

        let reply = recv(&client).expect("the probe is answered");
        let pairs = probe_pairs(&reply);
        assert_eq!(pairs.len(), 3, "hostName, ownerName, usersCount");
        assert_eq!(pairs[0].0, PROBE_KEY_HOST_NAME);
        assert_eq!(pairs[1].0, PROBE_KEY_OWNER_NAME);
        assert_eq!(pairs[2].0, PROBE_KEY_USERS_COUNT);
        assert_eq!(pairs[2].1, "7", "gNumLogins is reported");
    }

    /// An interface already gone cannot answer: the handler's weak
    /// upgrade fails and the probe is dropped silently.
    #[test]
    fn a_probe_after_the_interface_is_gone_gets_no_reply() {
        let client = client_socket();
        let interface = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1").0;
        let handler = ProbeHandler {
            log_probes: false,
            allow_probe: true,
            users_count: Rc::new(Cell::new(0)),
            interface: Rc::downgrade(&interface),
        };
        drop(interface);
        let mut header = UnpackedMessageHeader::new(1);
        header.set_length(0);
        handler.handle_message(
            &client_address(&client),
            &mut header,
            &mut BinaryStream::new(),
        );
        assert!(recv(&client).is_none(), "nothing to send with");
    }

    /// The owner name comes from the passwd entry of the running uid;
    /// a uid with no entry takes the reference's miss -- the empty
    /// string (loginapp.cpp:1176-1182).
    #[test]
    fn the_owner_name_misses_an_unknown_uid() {
        assert_eq!(owner_name_for(4_000_000_000), "");
    }

    /// The owner name resolves the running account on hosts with an
    /// `/etc/passwd`; exotic hosts keep the graceful empty string.
    #[test]
    fn the_owner_name_resolves_the_running_account() {
        let name = owner_name_for(current_uid());
        // Either the running account's name or the miss-marker.
        assert!(
            name.is_empty() || !name.contains(':'),
            "an account name, not a passwd line"
        );
    }

    /// `gethostname`'s answer reads back: the host name as the kernel
    /// reports it, never past the reference's 256-byte buffer.
    #[test]
    fn the_hostname_reads_back() {
        let name = hostname();
        assert!(name.len() < 256, "the buffer bounds the name");
        let from_uname = {
            let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
            unsafe {
                assert_eq!(libc::uname(&mut uts), 0);
            }
            let nodename = &uts.nodename;
            let bytes: &[u8] = unsafe {
                std::slice::from_raw_parts(nodename.as_ptr() as *const u8, nodename.len())
            };
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            String::from_utf8_lossy(&bytes[..end]).into_owned()
        };
        assert_eq!(name, from_uname, "gethostname answers what uname(2) sees");
    }

    // -- the dispatch sink ---------------------------------------------

    /// A plain datagram naming an unserved id lands on the discard arm:
    /// the two login-chain faces are unserved, and the walk reports
    /// `REASON_NONEXISTENT_ENTRY` the way it does for any unserved id.
    #[test]
    fn an_unserved_login_id_takes_the_discard_arm() {
        let minder = login_minder(None, None, None);
        let table = Rc::new(InterfaceTable::new());
        minder.register_with_interface(&table);
        let sink = LoginInterfaceDispatch {
            table: Rc::clone(&table),
        };
        let addr = Address::new(LOOPBACK, socket::htons(1234));

        for (id, name) in [(0u8, "login"), (2u8, "challengeResponse")] {
            let mut packet = Packet::new();
            packet.set_body_size(HEADER_SIZE + 1);
            packet.buffer_mut()[HEADER_SIZE] = id;
            assert_eq!(
                sink.process_packet(&addr, &mut packet),
                Reason::NonexistentEntry,
                "{name} is unserved and discards the bundle"
            );
        }
    }

    /// A request footer that does not fit, or names an offset past the
    /// body, is a corrupted packet (the channel-side strip's bounds
    /// check, udp_channel.rs:1104-1112).
    #[test]
    fn a_request_footer_that_does_not_fit_is_corrupted() {
        let minder = login_minder(None, None, None);
        let table = Rc::new(InterfaceTable::new());
        minder.register_with_interface(&table);
        let sink = LoginInterfaceDispatch {
            table: Rc::clone(&table),
        };
        let addr = Address::new(LOOPBACK, socket::htons(1234));

        // Three bytes is too short for the footer strip.
        let mut short = Packet::new();
        short.set_body_size(HEADER_SIZE + 1);
        short.buffer_mut()[HEADER_SIZE] = 1;
        short.set_flags(flags::HAS_REQUESTS);
        assert_eq!(
            sink.process_packet(&addr, &mut short),
            Reason::CorruptedPacket,
            "no room for the footer"
        );

        // A footer naming an offset past the body is refused too.
        let mut wide = Packet::new();
        wide.set_body_size(HEADER_SIZE + 2);
        wide.buffer_mut()[HEADER_SIZE..HEADER_SIZE + 2].copy_from_slice(&300u16.to_le_bytes());
        wide.set_flags(flags::HAS_REQUESTS);
        assert_eq!(
            sink.process_packet(&addr, &mut wide),
            Reason::CorruptedPacket,
            "the offset points past the body"
        );
    }

    // -- the round trip ------------------------------------------------

    /// The acceptance: the discovery probe, sent the way the reference
    /// client sends it, over a real datagram path into the bound
    /// interface and back -- `open` + `check` bring the interface up,
    /// the sink strips the request footer, the walk delivers to the
    /// probe handler, and the reply leaves on the interface's socket.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_probe_round_trips_over_a_real_datagram_path() {
        let config = LoginAppConfig {
            external_port: 0,
            external_interface: "127.0.0.1".to_string(),
            allow_probe: true,
            log_probes: true,
            should_offset_external_port_by_uid: false,
        };
        let dispatcher = Rc::new(EventDispatcher::new());
        let login = open(
            &config,
            crate::loginapp_login::LoginFacesConfig::new(),
            Rc::clone(&dispatcher),
        );
        check(&login).expect("the interface binds on an ephemeral port");

        let host_port = socket::ntohs(login.interface().address().port);
        let client = client_socket();
        let datagram = probe_request_datagram();
        client
            .send_to(&datagram, ("127.0.0.1", host_port))
            .expect("send the probe");

        // One poller pass carries the datagram through the receiver,
        // the sink and the handler.
        assert!(
            dispatcher.poller().process_pending_events(0.1) >= 1,
            "the probe surfaced through the poller"
        );

        let reply = recv(&client).expect("the probe is answered");
        let pairs = probe_pairs(&reply);
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0].0, PROBE_KEY_HOST_NAME);
        assert_eq!(pairs[1].0, PROBE_KEY_OWNER_NAME);
        assert_eq!(pairs[2].0, PROBE_KEY_USERS_COUNT);
        assert_eq!(pairs[2].1, "0", "gNumLogins before any login");

        // The counter the login chain bumps is the probe's answer: the
        // faces share the cell with the probe handler.
        assert_eq!(login.users_count(), login.faces().users_count());
    }

    /// A variable-length request datagram: the flags word with
    /// `HAS_REQUESTS`, the element's id byte and two-byte length field,
    /// the request preamble the walk reads right after the header
    /// (`bundle_dispatch.rs:253-270`), the body, and the first-request
    /// footer the receiver strips off the packet's tail.
    fn login_request_datagram(element_id: u8, payload: &[u8]) -> Vec<u8> {
        let mut datagram = Vec::new();
        datagram.extend_from_slice(&flags::HAS_REQUESTS.to_le_bytes());
        datagram.push(element_id);
        datagram.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        datagram.extend_from_slice(&7i32.to_le_bytes()); // the reply id
        datagram.extend_from_slice(&0u16.to_le_bytes()); // no next request
        datagram.extend_from_slice(payload);
        datagram.extend_from_slice(&(HEADER_SIZE as u16).to_le_bytes());
        datagram
    }

    /// The `login` element reaches its face over the real datagram path:
    /// the sink strips the request footer, the walk delivers to the login
    /// handler, and the refusal carries the request's reply id back
    /// (`allowLogin` defaults false, so the reply is the bare 82).
    #[cfg(target_os = "linux")]
    #[test]
    fn the_login_element_reaches_its_face_over_the_datagram_path() {
        let config = LoginAppConfig {
            external_port: 0,
            external_interface: "127.0.0.1".to_string(),
            allow_probe: false,
            log_probes: true,
            should_offset_external_port_by_uid: false,
        };
        let mut faces_config = crate::loginapp_login::LoginFacesConfig::new();
        faces_config.gate.verbose_login_failures = true;
        let dispatcher = Rc::new(EventDispatcher::new());
        let login = open(&config, faces_config, Rc::clone(&dispatcher));
        check(&login).expect("the interface binds on an ephemeral port");

        let host_port = socket::ntohs(login.interface().address().port);
        let client = client_socket();
        client
            .send_to(
                &login_request_datagram(0, &[1, 2, 3, 4]),
                ("127.0.0.1", host_port),
            )
            .expect("send the login");

        assert!(
            dispatcher.poller().process_pending_events(0.1) >= 1,
            "the login surfaced through the poller"
        );
        let reply = recv(&client).expect("the refusal replies");
        let (reply_id, body) = reply_frame(&reply);
        assert_eq!(reply_id, 7, "the request's reply id");
        assert_eq!(
            body[0],
            crate::loginapp::LogOnStatus::LoginRejectedLoginsNotAllowed as u8,
            "`allowLogin` false refuses with the bare status"
        );
        assert_eq!(login.faces().num_login_failures(), 1);
        assert_eq!(
            login.faces().num_login_attempts(),
            0,
            "refused at the gate, before the version read"
        );
    }

    /// The `challengeResponse` element reaches its face the same way: an
    /// answer to an outstanding delay challenge clears it and stays
    /// silent -- the reference drains on every arm of
    /// `challengeResponse` (loginapp.cpp:1195-1243). The cleared state is
    /// the discriminator: a corrupted or unserved datagram would leave
    /// the challenge standing.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_challenge_response_element_reaches_its_face_over_the_datagram_path() {
        let config = LoginAppConfig {
            external_port: 0,
            external_interface: "127.0.0.1".to_string(),
            allow_probe: false,
            log_probes: true,
            should_offset_external_port_by_uid: false,
        };
        let dispatcher = Rc::new(EventDispatcher::new());
        let login = open(
            &config,
            crate::loginapp_login::LoginFacesConfig::new(),
            Rc::clone(&dispatcher),
        );
        check(&login).expect("the interface binds on an ephemeral port");

        let host_port = socket::ntohs(login.interface().address().port);
        let client = client_socket();
        let addr = client_address(&client);
        // An outstanding delay challenge for this client, issued the way
        // the login face would have issued it.
        login.faces().with_requests(|requests| {
            let entry = requests.entry(crate::login_request::ServerAddress {
                ip: addr.ip,
                port: addr.port,
                salt: 0,
            });
            entry
                .challenge_state_mut()
                .get_or_insert_with(Default::default)
                .set_login_challenge(
                    crate::login_challenge::CHALLENGE_DELAY.to_string(),
                    Box::new(crate::login_challenge::DelayChallenge::new(0.001)),
                );
        });

        // The calculation duration then the echoed duration, the answer
        // `DelayChallenge::read_response_from_stream` accepts.
        let mut body = BinaryStream::new();
        body.write_f32(0.001);
        body.write_f32(0.001);
        client
            .send_to(
                &login_request_datagram(2, body.as_bytes()),
                ("127.0.0.1", host_port),
            )
            .expect("send the challenge response");

        assert!(
            dispatcher.poller().process_pending_events(0.1) >= 1,
            "the answer surfaced through the poller"
        );
        assert!(recv(&client).is_none(), "a verified answer draws no reply");
        login.faces().with_requests(|requests| {
            let entry = requests
                .find_mut(crate::login_request::ServerAddress {
                    ip: addr.ip,
                    port: addr.port,
                    salt: 0,
                })
                .expect("the request stays cached");
            let state = entry
                .challenge_state()
                .as_ref()
                .expect("the challenge state stays");
            assert!(state.pending().is_none(), "the challenge cleared");
            assert!(!state.did_fail_challenge());
        });
        assert_eq!(login.faces().num_login_failures(), 0);
        assert_eq!(login.faces().users_count(), 0);
    }
}
