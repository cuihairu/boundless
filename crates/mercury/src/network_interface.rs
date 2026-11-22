//! `lib/network/network_interface.cpp`: the UDP interface an app listens
//! on. This item carries the listening-socket lifecycle
//! (`recreateListeningSocket`/`closeSocket`), the interface's registered
//! poller, the child dispatcher, the interface table it serves with, and
//! the routing seams the receiver draws on.
//!
//! The interface owns its child `EventDispatcher`, its `InterfaceTable`,
//! the `RequestManager` served as the reply element's handler
//! (`network_interface.cpp:124-147`), and the channel registry with its
//! condemned and recently-dead recycling lanes (BW-02-03-02). It can
//! finalise the manager on shutdown. Still not owned here:
//! `registerWithMachined` has no machined to talk to; the `MF_SERVER`
//! buffer sizing is BW-02-01-04; the rate limiter arrives with
//! BW-02-05-04.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use platform::socket;

use crate::channel_map::ChannelMap;
use crate::condemned_channels::CondemnedChannels;
use crate::endpoint::{Address, Endpoint, INADDR_ANY, NO_SOCKET, NONE};
use crate::event_dispatcher::EventDispatcher;
use crate::interface_element::InterfaceElement;
use crate::interface_table::InterfaceTable;
use crate::interfaces::InputMessageHandler;
use crate::misc::{CHANNEL_ID_NULL, ChannelID, Reason};
use crate::packet::Packet;
use crate::packet_receiver::{ErrorReporter, PacketReceiver, PacketSink, strerror};
use crate::packet_sender::PacketSender;
use crate::poller::EventPoller;
use crate::recently_dead_channels::RecentlyDeadChannels;
use crate::request_manager::RequestManager;
use crate::udp_bundle::UDPBundle;
use crate::udp_channel::{ChannelTraits, UDPChannel};

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
    /// `pRequestManager_`: correlates replies to the requests that asked
    /// for them, and serves them as the handler for the reply message.
    /// Declared before the dispatcher so its drop cancels the request
    /// timers before the dispatcher's queue is inspected.
    request_manager: RefCell<Option<Rc<RequestManager>>>,
    /// `pChannelMap_` (BW-02-03-02): the registered channels, keyed by
    /// destination address.
    channel_map: RefCell<ChannelMap>,
    /// `pCondemnedChannels_` (BW-02-03-02): the recycling lane for
    /// condemned channels, with its recurring recycle timer.
    condemned_channels: Rc<CondemnedChannels>,
    /// `pRecentlyDeadChannels_` (BW-02-03-02): the addresses of recently
    /// deregistered external channels, `None` for internal interfaces. The
    /// reference builds it in `attach` when external; the port builds it in
    /// the constructor (the attach surface is BW-02-01-05) and documents the
    /// seam.
    recently_dead_channels: RefCell<Option<Rc<RecentlyDeadChannels>>>,
    /// `pPacketSender_`: the physical send half of the data plane, plus the
    /// off-channel sequence-number allocator it owns.
    packet_sender: Rc<PacketSender>,
    /// `pPacketReceiver_`: the read loop this interface hands datagrams to.
    /// The receiver holds only a `Weak` back here, so this ownership is the
    /// one direction of the pair.
    packet_receiver: RefCell<Option<Rc<PacketReceiver>>>,
    /// `pInterfaceTable_`: the message-id registry this socket serves.
    interface_table: Rc<InterfaceTable>,
    /// `pDispatcher_`: the child dispatcher the request timeouts and the
    /// reply element's handler are registered on. Declared after the
    /// channel collections so their timer handles are cancelled before the
    /// dispatcher's queue is inspected on drop.
    dispatcher: Rc<EventDispatcher>,
}

impl NetworkInterface {
    /// The reference constructor (network_interface.cpp:95): make the
    /// dispatcher, interface table and request manager, the socket and
    /// receiver, then `recreateListeningSocket` and serve the request
    /// manager as the reply element's handler. Attaching a main
    /// dispatcher belongs to BW-02-01-05's attach surface.
    ///
    /// Returns the interface and its receiver: the receiver is the handler
    /// registered on the interface's poller.
    pub fn new(
        interface_type: InterfaceType,
        listening_port: u16,
        listening_interface: &str,
    ) -> (Rc<NetworkInterface>, Rc<PacketReceiver>) {
        // The reference builds the child dispatcher, the interface table
        // on it, and the request manager for the reply element
        // (network_interface.cpp:124-146).
        let dispatcher = Rc::new(EventDispatcher::new());
        let interface_table = Rc::new(InterfaceTable::new());
        let request_manager = Rc::new(RequestManager::new(
            interface_type == InterfaceType::External,
            Rc::clone(&dispatcher),
        ));
        // The reference defers the recently-dead collection to `attach`
        // (network_interface.cpp:166-168); the port builds it here because
        // the main-dispatcher attach surface is BW-02-01-05.
        let recently_dead_channels = if interface_type == InterfaceType::External {
            Some(RecentlyDeadChannels::new(&dispatcher))
        } else {
            None
        };
        let interface = NetworkInterface {
            udp_socket: RefCell::new(Endpoint::new()),
            address: RefCell::new(NONE),
            is_external: interface_type == InterfaceType::External,
            verbose: Cell::new(true),
            poller: EventPoller::create(),
            sink: RefCell::new(None),
            reporter: RefCell::new(None),
            channel_finder: RefCell::new(None),
            request_manager: RefCell::new(Some(Rc::clone(&request_manager))),
            channel_map: RefCell::new(ChannelMap::new()),
            condemned_channels: Rc::new(CondemnedChannels::new()),
            recently_dead_channels: RefCell::new(recently_dead_channels),
            packet_sender: Rc::new(PacketSender::new()),
            packet_receiver: RefCell::new(None),
            interface_table,
            dispatcher,
        };
        let receiver = PacketReceiver::new();
        *interface.packet_receiver.borrow_mut() = Some(Rc::clone(&receiver));
        interface.recreate_listening_socket(&receiver, listening_port, listening_interface);

        // And put our request manager in as the reply handler
        // (network_interface.cpp:147).
        interface.interface_table.serve(
            &InterfaceElement::reply(),
            Some(Rc::clone(&request_manager) as Rc<dyn InputMessageHandler>),
        );

        let shared = Rc::new(interface);
        receiver.attach_interface(&shared);
        (shared, receiver)
    }

    /// `interfaceTable()`.
    pub fn interface_table(&self) -> &Rc<InterfaceTable> {
        &self.interface_table
    }

    /// `requestManager()`: the request manager this interface serves
    /// replies with. `None` once [`finalise_request_manager`](Self::finalise_request_manager)
    /// has torn it down.
    pub fn request_manager(&self) -> Option<Rc<RequestManager>> {
        self.request_manager.borrow().clone()
    }

    /// `dispatcher()`.
    pub fn dispatcher(&self) -> Rc<EventDispatcher> {
        Rc::clone(&self.dispatcher)
    }

    /// `finaliseRequestManager` (network_interface.cpp:189): deregister the
    /// reply element and destroy the request manager, so no
    /// `ReplyMessageHandler` is cancelled while the server is in a bad
    /// state (for example after the App has been destroyed). Dropping the
    /// manager fails whatever is still pending.
    pub fn finalise_request_manager(&self) {
        self.interface_table.serve(&InterfaceElement::reply(), None);
        *self.request_manager.borrow_mut() = None;
    }

    /// `prepareForShutdown` (network_interface.cpp:178): the reference also
    /// prepares its main dispatcher, which the port does not hold yet.
    pub fn prepare_for_shutdown(&self) {
        self.finalise_request_manager();
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

    /// `send( address, bundle, channel )` (network_interface.cpp:809): push
    /// a bundle through the physical sender.
    pub fn send(&self, address: &Address, bundle: &mut UDPBundle, channel: Option<&UDPChannel>) {
        let socket = self.udp_socket.borrow();
        self.packet_sender.send(&socket, address, bundle, channel);
    }

    /// `sendPacket( address, packet, channel, isResend )`
    /// (network_interface.cpp:840): push a single packet. The reference's
    /// channel slot feeds the sender's packet filter and once-off timer,
    /// both seams, so the port takes none.
    pub fn send_packet(&self, address: &Address, packet: &Rc<RefCell<Packet>>, is_resend: bool) {
        let socket = self.udp_socket.borrow();
        self.packet_sender
            .send_packet(&socket, address, packet, is_resend);
    }

    /// `dropNextSend()`: arrange for the next sent packet to be dropped.
    pub fn drop_next_send(&self) {
        self.packet_sender.drop_next_send();
    }

    /// `packetSender()`: the physical sender and its statistics.
    pub fn packet_sender(&self) -> Rc<PacketSender> {
        Rc::clone(&self.packet_sender)
    }

    /// `cancelRequestsFor( bundle, reason )` (network_interface.cpp:860):
    /// fail the request orders a failed bundle carries. With no request
    /// manager yet it is a no-op, matching the reply-correlation seam.
    pub fn cancel_requests_for(&self, bundle: &mut UDPBundle, reason: Reason) {
        if let Some(manager) = self.request_manager.borrow().as_ref() {
            bundle.cancel_requests(Some(manager), reason);
        }
    }

    /// `dispatcher().errorReporter()`, replaced by tests and by the
    /// dispatcher's own reporter later.
    pub fn set_error_reporter(&self, reporter: Option<Rc<dyn ErrorReporter>>) {
        *self.reporter.borrow_mut() = reporter;
    }

    /// `registerChannelFinder` (network_interface.cpp:595): the indexed
    /// channel lookup, consulted before the channel map by
    /// [`find_channel`](Self::find_channel).
    pub fn set_channel_finder(&self, finder: Option<Rc<ChannelFinder>>) {
        *self.channel_finder.borrow_mut() = finder;
    }

    /// `findChannel(addr)` (network_interface.cpp:636): a settable finder
    /// first (the indexed-channel seam), then the registered channel map.
    /// `createAnonymous` stays false here: the anonymous-creation half is
    /// [`find_or_create_channel`](Self::find_or_create_channel).
    pub fn find_channel(&self, addr: &Address) -> Option<Rc<dyn ChannelRef>> {
        if let Some(channel) = self
            .channel_finder
            .borrow()
            .as_ref()
            .and_then(|finder| finder(addr))
        {
            return Some(channel);
        }
        self.channel_map
            .borrow()
            .find(addr)
            .map(|channel| -> Rc<dyn ChannelRef> { channel })
    }

    /// `findChannel( addr )` restricted to the registered channel map:
    /// what [`UDPChannel::get`](crate::udp_channel::UDPChannel::get) looks
    /// up when it claims an existing anonymous channel.
    pub fn find_channel_owned(&self, addr: &Address) -> Option<Rc<UDPChannel>> {
        self.channel_map.borrow().find(addr)
    }

    /// `findChannel( addr, createAnonymous )` with the anonymous-creation
    /// half (channel_map.cpp:105-142): `createAnonymous` makes a new
    /// internal channel when none is registered. Only the non-creating arm
    /// may see an external interface (channel_map.cpp:124).
    pub fn find_or_create_channel(
        self: &Rc<Self>,
        addr: &Address,
        create_anonymous: bool,
    ) -> Option<Rc<UDPChannel>> {
        if !create_anonymous {
            return self.channel_map.borrow().find(addr);
        }

        debug_assert!(
            !self.is_external,
            "findChannel: cannot create an anonymous channel on an external interface"
        );
        if let Some(channel) = self.channel_map.borrow().find(addr) {
            return Some(channel);
        }
        if addr.ip == 0 {
            return None;
        }

        let channel = UDPChannel::new(self, *addr, ChannelTraits::Internal, CHANNEL_ID_NULL);
        channel.set_anonymous(true);
        eprintln!(
            "ChannelMap::find: Creating anonymous channel to {}",
            addr.c_str()
        );
        Some(channel)
    }

    /// `registerChannel` (network_interface.cpp:485): add a channel to the
    /// map. False when another channel is already registered there.
    pub fn register_channel(&self, channel: &Rc<UDPChannel>) -> bool {
        self.channel_map.borrow_mut().add(channel)
    }

    /// `deregisterChannel` (network_interface.cpp:497): remember the
    /// address of an external channel as recently dead, then drop it from
    /// the map.
    pub fn deregister_channel(&self, channel: &Rc<UDPChannel>) -> bool {
        if channel.is_external()
            && let Some(dead) = self.recently_dead_channels.borrow().as_ref()
        {
            dead.add(channel.addr());
        }
        self.channel_map.borrow_mut().remove(channel)
    }

    /// `condemnedChannels()` (network_interface.hpp:141).
    pub fn condemned_channels(&self) -> &Rc<CondemnedChannels> {
        &self.condemned_channels
    }

    /// `condemnedChannels().add( channel )` (udp_channel.cpp:412), with the
    /// now stamp threaded in from this interface's dispatcher clock.
    pub fn condemned_channels_add(&self, channel: Rc<UDPChannel>) {
        let now = self.dispatcher().now();
        self.condemned_channels.add(channel, now);
    }

    /// `delAnonymousChannel( addr )` (network_interface.cpp:711):
    /// condemn the anonymous channel to the address, if there is one.
    pub fn del_anonymous_channel(&self, addr: &Address) {
        let channel = {
            // Do not hold the map borrow across `condemn` -- the condemn
            // path deregisters through this same map.
            self.channel_map.borrow().find(addr)
        };
        let Some(channel) = channel else {
            eprintln!(
                "ChannelMap::delAnonymous: Couldn't find channel for address {}",
                addr.c_str()
            );
            return;
        };
        if !channel.is_anonymous() {
            eprintln!(
                "ChannelMap::delAnonymous: UDPChannel to {} is not anonymous!",
                addr.c_str()
            );
            return;
        }
        channel.condemn();
    }

    /// `destroyOwnedChannels()` (channel_map.cpp:12): destroy every channel
    /// the interface owns, warning about the registered ones it does not.
    pub fn destroy_owned_channels(&self) {
        let (owned, unowned) = {
            let map = self.channel_map.borrow();
            (map.owned_channels(), map.unowned_channels())
        };
        for channel in &owned {
            channel.destroy();
        }
        for (_addr, channel) in &unowned {
            eprintln!(
                "ChannelMap::destroyOwnedChannels: Channel to {} is still registered",
                channel.c_str()
            );
        }
    }

    /// `deleteFinishedChannels()` (network_interface.cpp:617): recycle the
    /// condemned channels that are finished. True when none are left.
    pub fn delete_finished_channels(&self) -> bool {
        let now = self.dispatcher().now();
        self.condemned_channels.delete_finished_channels(now)
    }

    /// `hasUnackedPackets()` (network_interface.cpp:606): any registered or
    /// condemned channel still has outstanding packets. The once-off sender
    /// is the BW-02-03-03/04 seam.
    pub fn has_unacked_packets(&self) -> bool {
        self.channel_map.borrow().has_unacked_packets()
            || self.condemned_channels.has_unacked_packets()
    }

    /// `findCondemnedChannel( channelID )` (network_interface.cpp:585).
    pub fn find_condemned_channel(&self, channel_id: ChannelID) -> Option<Rc<UDPChannel>> {
        self.condemned_channels.find(channel_id)
    }

    /// `isDead( addr )` (network_interface.cpp:531): the address was
    /// recently deregistered on an external interface.
    pub fn is_dead(&self, addr: &Address) -> bool {
        self.recently_dead_channels
            .borrow()
            .as_ref()
            .map(|dead| dead.is_dead(addr))
            .unwrap_or(false)
    }

    /// `onAddressDead( addr )` (network_interface.cpp:514): clean up the
    /// anonymous channel to the dead address. The once-off sender's own
    /// cleanup is the BW-02-03-03/04 seam.
    pub fn on_address_dead(&self, addr: &Address) {
        if let Some(channel) = self.find_channel_owned(addr)
            && channel.is_anonymous()
        {
            channel.set_remote_failed();
        }
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
    /// to the sink, or to the receiver's channel routing when there is none.
    /// Takes the shared handle so the receiver's `findChannel` can create an
    /// anonymous channel.
    pub fn process_packet(self: &Rc<Self>, addr: &Address, packet: Rc<RefCell<Packet>>) -> Reason {
        if let Some(sink) = self.sink.borrow().as_ref() {
            return sink.process_packet(addr, packet);
        }
        match self.packet_receiver.borrow().as_ref() {
            Some(receiver) => receiver.process_packet(self, addr, packet),
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

    /// BW-02-02-05: the constructor serves the request manager as the
    /// reply element's handler (`network_interface.cpp:147`), and
    /// `finaliseRequestManager` deregisters it and drops the manager.
    #[test]
    fn the_request_manager_is_served_as_the_reply_handler() {
        use crate::interface_element::REPLY_MESSAGE_IDENTIFIER;

        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);

        assert!(
            interface
                .interface_table()
                .handler(REPLY_MESSAGE_IDENTIFIER)
                .is_some(),
            "the reply element is served"
        );
        assert!(interface.request_manager().is_some());
        // The child dispatcher is the one the request manager armed itself on.
        assert_eq!(interface.dispatcher().num_timer_calls(), 0);

        // The reference prepares its manager through `prepareForShutdown`.
        interface.prepare_for_shutdown();

        assert!(
            interface
                .interface_table()
                .handler(REPLY_MESSAGE_IDENTIFIER)
                .is_none(),
            "finalising deregisters the reply handler"
        );
        assert!(interface.request_manager().is_none());

        // A second call has nothing left to tear down, and is a no-op.
        interface.finalise_request_manager();
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

    /// The routing entry of `processPacket`: a packet goes to the sink when
    /// there is one, otherwise to the receiver's channel routing. An empty
    /// packet is corrupt in that routing, so the no-sink arm records nothing.
    #[test]
    fn the_sink_receives_what_the_interface_processes() {
        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);

        let packet = crate::packet::Packet::new();
        let addr = Address::new(LOOPBACK, socket::htons(1234));

        // No sink yet: the real receiver routes it, and an empty packet is
        // corrupt there, so it is not dispatched.
        assert_eq!(
            interface.process_packet(&addr, Rc::new(RefCell::new(packet))),
            Reason::CorruptedPacket
        );

        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));
        assert_eq!(
            interface.process_packet(&addr, Rc::new(RefCell::new(crate::packet::Packet::new()))),
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
