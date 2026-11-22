//! `lib/network/network_interface.cpp`: the UDP interface an app listens
//! on. This item carries the listening-socket lifecycle
//! (`recreateListeningSocket`/`closeSocket`), the interface's registered
//! poller, and the routing seams the receiver draws on.
//!
//! Scope of BW-02-01-03: the interface owns an `EventPoller` as its
//! dispatcher stand-in -- the full `EventDispatcher` (timers, tasks,
//! attached children) is BW-02-01-05; `registerWithMachined` has no
//! machined to talk to; the `MF_SERVER` buffer sizing is BW-02-01-04; the
//! channel map, dead-channel bookkeeping and rate limiter arrive with
//! BW-02-03 / BW-02-05-04; the interface table (message dispatch) is
//! BW-02-02.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use platform::socket;

use crate::endpoint::{Address, Endpoint, INADDR_ANY, NO_SOCKET, NONE};
use crate::misc::Reason;
use crate::packet::Packet;
use crate::packet_receiver::{ErrorReporter, PacketReceiver, PacketSink, strerror};
use crate::poller::EventPoller;

/// `Mercury::NetworkInterfaceType` (network_interface.hpp).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterfaceType {
    /// The interface serving traffic between BigWorld processes.
    Internal,
    /// The interface facing untrusted clients.
    External,
}

/// The slice of `UDPChannel` the receiver's error paths touch; the channel
/// itself is BW-02-03.
pub trait ChannelRef {
    /// `c_str`: the channel's label for messages.
    fn c_str(&self) -> String;
    /// `isInternal`.
    fn is_internal(&self) -> bool;
    /// `hasRemoteFailed`.
    fn has_remote_failed(&self) -> bool;
    /// `setRemoteFailed`.
    fn set_remote_failed(&self);
    /// `addr()`: the channel's destination address, used by the request
    /// manager to check that a reply came from the right peer.
    fn addr(&self) -> Address;
}

/// `findChannel(addr)` as a settable seam: `None` until channels exist.
pub type ChannelFinder = dyn Fn(&Address) -> Option<Rc<dyn ChannelRef>>;

/// `Mercury::NetworkInterface` (network_interface.hpp:146): the socket, the
/// address it advertises, and the dispatcher it registers on. The setup
/// paths take the write borrows of the socket and address cells so they
/// keep the reference's callability after the interface is shared.
pub struct NetworkInterface {
    /// `udpSocket_`.
    udp_socket: RefCell<Endpoint>,
    /// `address_`.
    address: RefCell<Address>,
    /// `isExternal_`.
    is_external: bool,
    /// `verbosityLevel_`, defaulted to debug -- verbose.
    verbose: Cell<bool>,
    /// The child-dispatcher stand-in this interface registers its socket
    /// with. The reference builds an `EventDispatcher` here.
    poller: EventPoller,
    /// Where `processPacket` hands datagrams until channels exist.
    sink: RefCell<Option<Rc<dyn PacketSink>>>,
    /// `dispatcher().errorReporter()`, defaulting to the logging reporter.
    reporter: RefCell<Option<Rc<dyn ErrorReporter>>>,
    /// `findChannel`, settable until the channel map lands (BW-02-03).
    channel_finder: RefCell<Option<Rc<ChannelFinder>>>,
}

impl NetworkInterface {
    /// The reference constructor (network_interface.cpp:95): make the
    /// socket and receiver, then `recreateListeningSocket`. Attaching a
    /// main dispatcher and serving the reply interface element belong to
    /// their owners (BW-02-01-05 / BW-02-02).
    ///
    /// Returns the interface and its receiver: the receiver is the handler
    /// registered on the interface's poller.
    pub fn new(
        interface_type: InterfaceType,
        listening_port: u16,
        listening_interface: &str,
    ) -> (Rc<NetworkInterface>, Rc<PacketReceiver>) {
        let interface = NetworkInterface {
            udp_socket: RefCell::new(Endpoint::new()),
            address: RefCell::new(NONE),
            is_external: interface_type == InterfaceType::External,
            verbose: Cell::new(true),
            poller: EventPoller::create(),
            sink: RefCell::new(None),
            reporter: RefCell::new(None),
            channel_finder: RefCell::new(None),
        };
        let receiver = PacketReceiver::new();
        interface.recreate_listening_socket(&receiver, listening_port, listening_interface);
        let shared = Rc::new(interface);
        receiver.attach_interface(&shared);
        (shared, receiver)
    }

    /// `recreateListeningSocket` (network_interface.cpp:339): discard the
    /// current socket and create a new one on the requested interface.
    /// True on success; a not-good interface afterwards means failure.
    pub fn recreate_listening_socket(
        &self,
        receiver: &Rc<PacketReceiver>,
        listening_port: u16,
        listening_interface: &str,
    ) -> bool {
        self.close_socket();

        // clear this unless it gets set otherwise
        *self.address.borrow_mut() = NONE;

        // make the socket
        self.udp_socket.borrow_mut().socket(socket::SOCK_DGRAM);
        if !self.udp_socket.borrow().good() {
            eprintln!("NetworkInterface::recreateListeningSocket: couldn't create a socket");
            return false;
        }

        // Reference order (network_interface.cpp:359): the descriptor is
        // registered before the bind, so a failed bind leaves the entry
        // behind -- ported as-is.
        self.poller.register_for_read(
            self.udp_socket.borrow().fileno(),
            receiver.clone(),
            "PacketReceiver",
        );

        // ask endpoint to parse the interface specification into a name
        let mut ifname = String::new();
        let ifaddr = resolve_listening_interface(
            |spec, name| {
                self.udp_socket
                    .borrow()
                    .find_indicated_interface(spec, name)
            },
            |name, addr| self.udp_socket.borrow().get_interface_address(name, addr),
            listening_interface,
            &mut ifname,
        );

        // now we know where to bind, so do so
        if self.udp_socket.borrow().bind(listening_port, ifaddr) != 0 {
            eprintln!(
                "NetworkInterface::recreateListeningSocket: Couldn't bind the socket to {} ({})",
                Address::new(ifaddr, listening_port).c_str(),
                strerror(last_bind_errno())
            );
            self.udp_socket.borrow_mut().close();
            self.udp_socket.borrow_mut().detach();
            return false;
        }

        // but for advertising it ask the socket for where it thinks it's bound
        {
            let mut port = 0u16;
            let mut ip = 0u32;
            self.udp_socket
                .borrow()
                .local_address(Some(&mut port), Some(&mut ip));
            *self.address.borrow_mut() = Address::new(ip, port);
        }

        if self.address.borrow().ip == 0 {
            // we're on INADDR_ANY, report the address of the
            // interface used by the default route then
            let ip = match resolve_default_route_address(
                |name| self.udp_socket.borrow().find_default_interface(name),
                |name, addr| self.udp_socket.borrow().get_interface_address(name, addr),
                &mut ifname,
            ) {
                None => {
                    self.udp_socket.borrow_mut().close();
                    self.udp_socket.borrow_mut().detach();
                    return false;
                }
                Some(ip) => ip,
            };
            self.address.borrow_mut().ip = ip;

            eprintln!(
                "NetworkInterface::recreateListeningSocket: \
                 bound to all interfaces with default route interface on {} ( {} )",
                ifname,
                self.address.borrow().c_str()
            );
        }

        eprintln!(
            "NetworkInterface::recreateListeningSocket: address {}",
            self.address.borrow().c_str()
        );

        self.udp_socket.borrow().set_nonblocking(true);
        self.udp_socket.borrow().enable_error_queue(true);

        // MF_SERVER (network_interface.cpp:449-462): the server build
        // asks for the process-wide wanted buffer sizes and warns with
        // what the kernel granted when it refuses. This port is a
        // server port, so the block is unconditional; the `__APPLE__`
        // SO_NOSIGPIPE line before it has no Linux counterpart.
        apply_server_buffer_sizes(
            &self.udp_socket.borrow(),
            crate::network_utils::buffer_sizes(),
        );

        // registerWithMachined has no machined to talk to.

        true
    }

    /// `closeSocket` (network_interface.cpp:316): deregister the
    /// descriptor, close, and detach in case close failed.
    pub fn close_socket(&self) {
        let (good, fd) = {
            let udp_socket = self.udp_socket.borrow();
            (udp_socket.good(), udp_socket.fileno())
        };
        if good {
            // deregisterWithMachined has no machined to talk to.
            self.poller.deregister_for_read(fd);
            self.udp_socket.borrow_mut().close();
            self.udp_socket.borrow_mut().detach();
        }
    }

    /// `isGood` (network_interface.hpp:229).
    pub fn is_good(&self) -> bool {
        let udp_socket = self.udp_socket.borrow();
        udp_socket.fileno() != NO_SOCKET && !self.address.borrow().is_none()
    }

    /// `address()`.
    pub fn address(&self) -> Address {
        *self.address.borrow()
    }

    /// The listening socket, for the receiver's reads.
    pub fn udp_socket(&self) -> std::cell::Ref<'_, Endpoint> {
        self.udp_socket.borrow()
    }

    /// The poller this interface registers on.
    pub fn poller(&self) -> &EventPoller {
        &self.poller
    }

    /// `isExternal`.
    pub fn is_external(&self) -> bool {
        self.is_external
    }

    /// `isVerbose` -- `verbosityLevel_` defaults to debug.
    pub fn is_verbose(&self) -> bool {
        self.verbose.get()
    }

    /// `verbosityLevel_`, flattened to a bool.
    pub fn set_verbose(&self, verbose: bool) {
        self.verbose.set(verbose);
    }

    /// The `processPacket` routing seam: datagrams go to the sink until
    /// the channel map takes them over (BW-02-03).
    pub fn set_packet_sink(&self, sink: Option<Rc<dyn PacketSink>>) {
        *self.sink.borrow_mut() = sink;
    }

    /// `dispatcher().errorReporter()`, replaced by tests and by the
    /// dispatcher's own reporter later.
    pub fn set_error_reporter(&self, reporter: Option<Rc<dyn ErrorReporter>>) {
        *self.reporter.borrow_mut() = reporter;
    }

    /// `findChannel`, settable until the channel map lands (BW-02-03).
    pub fn set_channel_finder(&self, finder: Option<Rc<ChannelFinder>>) {
        *self.channel_finder.borrow_mut() = finder;
    }

    /// `findChannel(addr)` (network_interface.cpp:636): `None` until the
    /// channel map exists.
    pub fn find_channel(&self, addr: &Address) -> Option<Rc<dyn ChannelRef>> {
        self.channel_finder
            .borrow()
            .as_ref()
            .and_then(|finder| finder(addr))
    }

    /// `dispatcher().errorReporter()`: the registered reporter, or the
    /// logging default.
    pub fn error_reporter(&self) -> Rc<dyn ErrorReporter> {
        match self.reporter.borrow().as_ref() {
            Some(reporter) => reporter.clone(),
            None => Rc::new(crate::packet_receiver::LogReporter),
        }
    }

    /// The `PacketReceiver::processPacket` routing target: hand the packet
    /// to the sink, or accept it silently while there is none (the
    /// interface table that dispatches messages is BW-02-02).
    pub fn process_packet(&self, addr: &Address, packet: &mut Packet) -> Reason {
        match self.sink.borrow().as_ref() {
            Some(sink) => sink.process_packet(addr, packet),
            None => Reason::Success,
        }
    }
}

/// The interface-spec resolution of `recreateListeningSocket`
/// (network_interface.cpp:362-399): turn the spec into an interface name
/// and its IPv4 address, warning through the fallbacks. Split from the
/// socket dance so the arms are drivable with injected queries. Returns
/// the address to bind.
fn resolve_listening_interface(
    find_indicated: impl Fn(&str, &mut String) -> i32,
    address_of: impl Fn(&str, &mut u32) -> i32,
    listening_interface: &str,
    ifname: &mut String,
) -> u32 {
    let mut ifaddr = INADDR_ANY;

    // Querying bwmachined for the internal interface (`USE_BWMACHINED`)
    // is out of scope: no machined daemon lives in this tree, so such a
    // spec falls to the parse arm and warns like any unparseable spec.

    if find_indicated(listening_interface, ifname) == 0 {
        eprintln!(
            "NetworkInterface::recreateListeningSocket: Creating on interface '{}' (= {})",
            listening_interface, ifname
        );
        if address_of(ifname, &mut ifaddr) != 0 {
            eprintln!(
                "NetworkInterface::recreateListeningSocket: \
                 Couldn't get addr of interface {} so using all interfaces",
                ifname
            );
        }
    } else if !listening_interface.is_empty() {
        eprintln!(
            "NetworkInterface::recreateListeningSocket: \
             Couldn't parse interface spec '{}' so using all interfaces",
            listening_interface
        );
    }

    ifaddr
}

/// The `MF_SERVER` buffer block of `recreateListeningSocket`
/// (network_interface.cpp:449-462): request the wanted sizes, warn with
/// the granted ones when the kernel caps them. Split so tests can drive
/// both arms; returns whether each request was honoured.
fn apply_server_buffer_sizes(udp_socket: &Endpoint, wanted: (i32, i32)) -> (bool, bool) {
    let (recv_wanted, send_wanted) = wanted;

    let received = udp_socket.set_buffer_size(socket::SO_RCVBUF, recv_wanted);
    if !received {
        eprintln!(
            "NetworkInterface::recreateListeningSocket: \
             Operating with a receive buffer of only {} bytes (instead of {})",
            udp_socket.get_buffer_size(socket::SO_RCVBUF),
            recv_wanted
        );
    }

    let sent = udp_socket.set_buffer_size(socket::SO_SNDBUF, send_wanted);
    if !sent {
        eprintln!(
            "NetworkInterface::recreateListeningSocket: \
             Operating with a send buffer of only {} bytes (instead of {})",
            udp_socket.get_buffer_size(socket::SO_SNDBUF),
            send_wanted
        );
    }

    (received, sent)
}

/// The INADDR_ANY branch of `recreateListeningSocket`
/// (network_interface.cpp:414-435): name the interface the default route
/// leaves by and its address. Split from the socket dance so both
/// failure arms are drivable with injected queries. `None` on failure.
fn resolve_default_route_address(
    find_default: impl Fn(&mut String) -> i32,
    address_of: impl Fn(&str, &mut u32) -> i32,
    ifname: &mut String,
) -> Option<u32> {
    let mut ip = 0u32;
    if find_default(ifname) != 0 || address_of(ifname, &mut ip) != 0 {
        eprintln!(
            "NetworkInterface::recreateListeningSocket: \
             Couldn't determine ip addr of default interface"
        );
        return None;
    }
    Some(ip)
}

/// The `errno` after the failed bind.
fn last_bind_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet_receiver::tests_support::{RecordingReporter, RecordingSink};

    /// `127.0.0.1` as the repo stores it.
    const LOOPBACK: u32 = 0x0100_007f;

    /// An interface bound to the loopback, port picked by the kernel.
    fn loopback_interface(
        interface_type: InterfaceType,
    ) -> (Rc<NetworkInterface>, Rc<PacketReceiver>) {
        NetworkInterface::new(interface_type, 0, "127.0.0.1")
    }

    /// The four arms of the spec resolution
    /// (network_interface.cpp:376-399).
    #[test]
    fn interface_specs_resolve_to_bind_addresses() {
        let mut ifname = String::new();

        // A parseable spec: the name and its address.
        let addr = resolve_listening_interface(
            |_spec, name| {
                *name = "lo".to_string();
                0
            },
            |name, out| {
                assert_eq!(name, "lo");
                *out = LOOPBACK;
                0
            },
            "127.0.0.1",
            &mut ifname,
        );
        assert_eq!(addr, LOOPBACK);
        assert_eq!(ifname, "lo");

        // Found but addressless: fall back to all interfaces. The same
        // refusing query doubles for the two fallback arms below, where
        // it must never be called.
        let consulted = std::cell::Cell::new(false);
        let addressless = |_name: &str, _out: &mut u32| -> i32 {
            consulted.set(true);
            -1
        };
        let addr = resolve_listening_interface(
            |_spec, name| {
                *name = "lo".to_string();
                0
            },
            addressless,
            "127.0.0.1",
            &mut ifname,
        );
        assert_eq!(addr, INADDR_ANY);
        assert!(consulted.get(), "the address was queried");

        // Unparseable non-empty spec: warn and use all interfaces.
        consulted.set(false);
        let addr =
            resolve_listening_interface(|_spec, _name| -2, addressless, "not-a-spec", &mut ifname);
        assert_eq!(addr, INADDR_ANY);
        assert!(!consulted.get(), "no name means no address query");

        // Empty spec: the same fallback, silently.
        let addr = resolve_listening_interface(|_spec, _name| -2, addressless, "", &mut ifname);
        assert_eq!(addr, INADDR_ANY);
        assert!(!consulted.get(), "no name means no address query");
    }

    /// The INADDR_ANY branch: the default-route interface is found and
    /// its address used; either query failing warns and refuses
    /// (network_interface.cpp:414-435).
    #[test]
    fn the_default_route_resolves_to_an_address() {
        let mut ifname = String::new();

        let ip = resolve_default_route_address(
            |name| {
                *name = "ens18".to_string();
                0
            },
            |name, addr| {
                assert_eq!(name, "ens18");
                *addr = 0x0100_00ac;
                0
            },
            &mut ifname,
        );
        assert_eq!(ip, Some(0x0100_00ac));

        assert_eq!(
            resolve_default_route_address(
                |_name| -1,
                |_name, _addr| panic!("no name"),
                &mut ifname
            ),
            None,
            "no default route refuses"
        );
        assert_eq!(
            resolve_default_route_address(
                |name| {
                    *name = "ens18".to_string();
                    0
                },
                |_name, _addr| -1,
                &mut ifname
            ),
            None,
            "an addressless interface refuses"
        );
    }

    /// An all-interfaces bind advertises the default route's address
    /// (network_interface.cpp:414-437, success arm against the real
    /// routing table).
    #[test]
    fn an_any_bind_advertises_the_default_route() {
        let (interface, _receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "");

        assert!(interface.is_good());
        assert_ne!(
            interface.address().ip,
            INADDR_ANY,
            "the advertised ip is the default route's, not 0"
        );
        assert_ne!(interface.address().port, 0);
        let fd = interface.udp_socket().fileno();
        assert!(interface.poller().is_registered(fd, true));
    }

    /// An internal interface binds where it is told, registers its
    /// receiver, and reports itself good.
    #[test]
    fn an_interface_listens_where_it_is_told() {
        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);

        assert!(interface.is_good());
        assert!(!interface.is_external());
        assert!(interface.is_verbose());
        assert_eq!(interface.address().ip, LOOPBACK);
        assert_ne!(interface.address().port, 0);

        // The receiver is registered on the interface's poller under the
        // socket's descriptor.
        let fd = interface.udp_socket().fileno();
        assert!(interface.poller().is_registered(fd, true));
        assert!(!interface.poller().is_registered(fd, false));
    }

    /// `InterfaceType::External` flags the interface, and the verbosity
    /// flag is settable.
    #[test]
    fn an_external_interface_reports_itself() {
        let (interface, _receiver) = loopback_interface(InterfaceType::External);

        assert!(interface.is_good());
        assert!(interface.is_external());
        interface.set_verbose(false);
        assert!(!interface.is_verbose());
        interface.set_verbose(true);
        assert!(interface.is_verbose());
    }

    /// A port someone already holds cannot be bound: the interface is
    /// left not-good with no address (network_interface.cpp:402-410).
    #[test]
    fn a_failed_bind_leaves_the_interface_not_good() {
        // Hold the port so the interface's bind fails.
        let mut blocker = Endpoint::new();
        blocker.socket(socket::SOCK_DGRAM);
        assert_eq!(blocker.bind(39_757, LOOPBACK), 0);

        let (interface, _receiver) =
            NetworkInterface::new(InterfaceType::Internal, 39_757, "127.0.0.1");

        assert!(!interface.is_good());
        assert!(interface.address().is_none());
    }

    /// `closeSocket` deregisters the descriptor and detaches the socket
    /// (network_interface.cpp:316-327).
    #[test]
    fn close_socket_deregisters_and_detaches() {
        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);
        let fd = interface.udp_socket().fileno();
        assert!(interface.poller().is_registered(fd, true));

        interface.close_socket();

        assert!(!interface.is_good());
        assert!(!interface.poller().is_registered(fd, true));
        assert_eq!(interface.udp_socket().fileno(), NO_SOCKET);
    }

    /// The server buffer block: recreate asks for the process-wide
    /// wanted sizes and the kernel honours them when they sit inside the
    /// maxima (network_interface.cpp:449-462, success arm).
    #[test]
    fn recreate_applies_the_wanted_buffer_sizes() {
        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);
        let (recv_wanted, send_wanted) = crate::network_utils::buffer_sizes();

        assert!(
            interface.udp_socket().get_buffer_size(socket::SO_RCVBUF) >= recv_wanted,
            "the receive buffer was raised to the wanted size"
        );
        assert!(
            interface.udp_socket().get_buffer_size(socket::SO_SNDBUF) >= send_wanted,
            "the send buffer was raised to the wanted size"
        );
    }

    /// An impossible request is refused and warned about: the kernel
    /// caps the buffer below the wanted size
    /// (network_interface.cpp:450-461, warning arm).
    #[test]
    fn impossible_buffer_requests_warn_and_fall_back() {
        let mut ep = Endpoint::new();
        ep.socket(socket::SOCK_DGRAM);

        assert_eq!(
            apply_server_buffer_sizes(&ep, (1 << 30, 1 << 30)),
            (false, false),
            "1 GiB is beyond the kernel maxima"
        );
        // A reachable request passes quietly.
        assert_eq!(apply_server_buffer_sizes(&ep, (4096, 4096)), (true, true));
    }

    /// `recreateListeningSocket` replaces the socket: the interface stays
    /// good, and the poller tracks the new descriptor.
    #[test]
    fn recreate_replaces_the_socket() {
        let (interface, receiver) = loopback_interface(InterfaceType::Internal);
        assert!(interface.is_good());

        assert!(interface.recreate_listening_socket(&receiver, 0, "127.0.0.1"));

        assert!(interface.is_good());
        assert_ne!(interface.address().port, 0);
        let fd = interface.udp_socket().fileno();
        assert!(interface.poller().is_registered(fd, true));
    }

    /// An interface with no sink accepts packets silently; with one, it
    /// hands them over (the routing entry of `processPacket`).
    #[test]
    fn the_sink_receives_what_the_interface_processes() {
        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);

        let mut packet = crate::packet::Packet::new();
        let addr = Address::new(LOOPBACK, socket::htons(1234));

        // No sink yet: accepted, nothing recorded.
        assert_eq!(
            interface.process_packet(&addr, &mut packet),
            Reason::Success
        );

        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));
        assert_eq!(
            interface.process_packet(&addr, &mut packet),
            Reason::Success
        );
        assert_eq!(sink.packets.borrow().len(), 1);
        assert_eq!(sink.packets.borrow()[0].0, addr);
    }

    /// The default reporter logs; a registered reporter receives instead.
    #[test]
    fn the_error_reporter_is_replaceable() {
        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);

        // The default is the logging reporter: calling it only checks it
        // does not panic.
        interface.error_reporter().report_exception(
            Reason::NoSuchPort,
            Some(&Address::new(LOOPBACK, socket::htons(1))),
        );

        let reporter = Rc::new(RecordingReporter::default());
        interface.set_error_reporter(Some(reporter.clone()));
        interface
            .error_reporter()
            .report_exception(Reason::Inactivity, None);
        assert_eq!(*reporter.reports.borrow(), vec![(Reason::Inactivity, None)]);
    }
}
