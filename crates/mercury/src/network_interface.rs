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
use std::rc::{Rc, Weak};

use platform::socket;

use crate::channel_map::ChannelMap;
use crate::condemned_channels::CondemnedChannels;
use crate::endpoint::{Address, Endpoint, INADDR_ANY, NO_SOCKET, NONE};
use crate::event_dispatcher::EventDispatcher;
use crate::misc::{CHANNEL_ID_NULL, Reason, SeqNumAllocator};
use crate::packet::Packet;
use crate::packet::flags;
use crate::packet_filter::PacketFilter;
use crate::packet_receiver::{ErrorReporter, PacketReceiver, PacketSink, strerror};
use crate::packet_sender::{
    PacketLossParameters, RandStream, Reschedule, SendingStats, UDP_OVERHEAD, reschedule_decision,
    schedule_rescheduled_packet,
};
use crate::poller::EventPoller;
use crate::rate_limiter::RateLimiter;
use crate::request_manager::RequestManager;
use crate::udp_bundle::UdpBundle;
use crate::udp_channel::{
    ChannelPtr, DEFAULT_INACTIVITY_RESEND_DELAY, Traits as ChannelTraits, UdpChannel,
};

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
    /// `dispatcher_.pPoller_`: the poller this interface registers its
    /// socket on, shared with its dispatcher.
    poller: Rc<EventPoller>,
    /// `pDispatcher_`.
    dispatcher: Rc<EventDispatcher>,
    /// `channels_`: the live non-indexed channels (BW-02-03-02).
    channels: RefCell<ChannelMap>,
    /// The condemned channels waiting to be deleted.
    condemned: RefCell<CondemnedChannels>,
    /// `pRequestManager_`.
    request_manager: RefCell<Option<Rc<RequestManager>>>,
    /// Where `processPacket` hands datagrams until channels exist.
    sink: RefCell<Option<Rc<dyn PacketSink>>>,
    /// `dispatcher().errorReporter()`, defaulting to the logging reporter.
    reporter: RefCell<Option<Rc<dyn ErrorReporter>>>,
    /// `findChannel`, settable until the channel map lands (BW-02-03).
    channel_finder: RefCell<Option<Rc<ChannelFinder>>>,
    /// This interface's own shared handle, so channel creation
    /// (`findChannel`'s anonymous arm) can hand out an `Rc` of it.
    self_weak: RefCell<Weak<NetworkInterface>>,
    /// `seqNumAllocator_`: the off-channel sequence numbers this interface
    /// hands out (packet_sender.hpp).
    seq_num_allocator: RefCell<SeqNumAllocator>,
    /// `shouldUseChecksums_`: whether outbound packets carry a checksum
    /// footer (packet_sender.hpp). Off by default, as the reference's.
    should_use_checksums: Cell<bool>,
    /// The configured rate limiting (`rateLimitPeriod_`, the two per-period
    /// budgets and their over-budget averages, network_interface.cpp:99-105).
    /// It lives with the interface's dispatcher: its period timer is armed on
    /// it, and `process_packet`'s first arm asks it about every datagram.
    rate_limiter: Rc<RateLimiter>,
    /// `pOffChannelFilter_` (network_interface.hpp:328): the filter given
    /// first say over off-channel datagrams (`PacketReceiver::processPacket`,
    /// packet_receiver.cpp:493). `None` until one is installed; the BaseApp
    /// installs its `InitialConnectionFilter` here (BW-02-04-03).
    off_channel_filter: RefCell<Option<Rc<dyn PacketFilter>>>,
    /// `sendingStats_` (network_interface.hpp): the outbound counters the
    /// inline send path keeps.
    sending_stats: RefCell<SendingStats>,
    /// `pPacketLossParameters_` (network_interface.hpp:288): the
    /// artificial-loss and latency knobs the send path consults
    /// (BW-12-03-04).
    loss: RefCell<PacketLossParameters>,
    /// The interface's own `isDroppingNextSend_`, the one-shot the
    /// `dropNextSend` entries set.
    is_dropping_next_send: Cell<bool>,
    /// The interface's own draw of the reference's `rand()` stream, for
    /// the loss and latency decisions.
    rand: RandStream,
}

impl NetworkInterface {
    /// The reference constructor (network_interface.cpp:95): make the
    /// socket and receiver, then `recreateListeningSocket`. The
    /// dispatcher is this interface's own (real clock); the reference
    /// takes one from its owner, and the main-dispatcher coupling is the
    /// owner's business (BW-02-01-05 / BW-02-02).
    ///
    /// Returns the interface and its receiver: the receiver is the handler
    /// registered on the interface's poller.
    pub fn new(
        interface_type: InterfaceType,
        listening_port: u16,
        listening_interface: &str,
    ) -> (Rc<NetworkInterface>, Rc<PacketReceiver>) {
        let shared = Self::new_with_dispatcher(interface_type, Rc::new(EventDispatcher::new()));
        let receiver = PacketReceiver::new();
        shared.recreate_listening_socket(&receiver, listening_port, listening_interface);
        receiver.attach_interface(&shared);
        (shared, receiver)
    }

    /// The channel-side constructor: no socket, only the dispatcher, the
    /// channel map and the condemned list -- what the channel machinery
    /// actually touches. Tests drive it with a pinned clock.
    pub fn new_with_dispatcher(
        interface_type: InterfaceType,
        dispatcher: Rc<EventDispatcher>,
    ) -> Rc<NetworkInterface> {
        // The rate limiting rides on the interface's own dispatcher: its
        // period timer is armed there, so it must exist before the
        // interface is put together.
        let rate_limiter = RateLimiter::new(Rc::clone(&dispatcher));
        let interface = NetworkInterface {
            udp_socket: RefCell::new(Endpoint::new()),
            address: RefCell::new(NONE),
            is_external: interface_type == InterfaceType::External,
            verbose: Cell::new(true),
            poller: dispatcher.poller(),
            dispatcher,
            channels: RefCell::new(ChannelMap::new()),
            condemned: RefCell::new(CondemnedChannels::new()),
            request_manager: RefCell::new(None),
            sink: RefCell::new(None),
            reporter: RefCell::new(None),
            channel_finder: RefCell::new(None),
            self_weak: RefCell::new(Weak::new()),
            seq_num_allocator: RefCell::new(SeqNumAllocator::new(1)),
            should_use_checksums: Cell::new(false),
            rate_limiter,
            off_channel_filter: RefCell::new(None),
            sending_stats: RefCell::new(SendingStats::default()),
            loss: RefCell::new(PacketLossParameters::new()),
            is_dropping_next_send: Cell::new(false),
            rand: RandStream::new(),
        };
        let shared = Rc::new(interface);
        *shared.self_weak.borrow_mut() = Rc::downgrade(&shared);
        shared
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

    /// The poller this interface registers on -- its dispatcher's.
    pub fn poller(&self) -> Rc<EventPoller> {
        Rc::clone(&self.poller)
    }

    /// `dispatcher()`.
    pub fn dispatcher(&self) -> Rc<EventDispatcher> {
        Rc::clone(&self.dispatcher)
    }

    /// This interface as a shared handle, through the self weak cell.
    pub(crate) fn self_rc(&self) -> Option<Rc<NetworkInterface>> {
        self.self_weak.borrow().upgrade()
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

    /// `pOffChannelFilter` (network_interface.hpp:135): install the filter
    /// that gets first say over off-channel datagrams. The BaseApp wires
    /// its `InitialConnectionFilter` here (baseapp.cpp:251).
    pub fn set_off_channel_filter(&self, filter: Option<Rc<dyn PacketFilter>>) {
        *self.off_channel_filter.borrow_mut() = filter;
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

    /// `findChannel(addr)` as the error paths see it: the test seam when
    /// one is installed, else the live channel map.
    pub fn find_channel_ref(&self, addr: &Address) -> Option<Rc<dyn ChannelRef>> {
        if let Some(finder) = self.channel_finder.borrow().as_ref() {
            return finder(addr);
        }
        self.find_channel(*addr, false).map(|channel| {
            // `Rc<ChannelRefHandle>` coerces at a typed binding -- `as`
            // has no unsized cast.
            let handle: Rc<dyn ChannelRef> = Rc::new(crate::udp_channel::ChannelRefHandle(channel));
            handle
        })
    }

    /// `NetworkInterface::findChannel( addr, createAnonymous )`
    /// (network_interface.cpp:636) over `ChannelMap::find`
    /// (channel_map.cpp:64): an anonymous channel is built to order on an
    /// internal interface; external ones refuse the shortcut outright.
    pub fn find_channel(&self, addr: Address, create_anonymous: bool) -> Option<ChannelPtr> {
        assert!(
            !create_anonymous || !self.is_external,
            "NetworkInterface::findChannel: MF_ASSERT( !createAnonymous || !isExternal() )"
        );

        if addr.ip == 0 {
            assert!(
                !create_anonymous,
                "ChannelMap::find: MF_ASSERT( !createAnonymous )"
            );
            return None;
        }

        if let Some(channel) = self.channels.borrow().find(addr) {
            assert!(
                channel.borrow().id() == CHANNEL_ID_NULL,
                "ChannelMap::find: MF_ASSERT( pChannel->id() == CHANNEL_ID_NULL )"
            );
            return Some(channel);
        }

        if create_anonymous {
            let interface = self
                .self_rc()
                .expect("NetworkInterface::findChannel: the interface holds its own handle");
            let channel = UdpChannel::new(
                &interface,
                addr,
                ChannelTraits::Internal,
                DEFAULT_INACTIVITY_RESEND_DELAY,
                CHANNEL_ID_NULL,
            );
            UdpChannel::set_is_anonymous(&channel, true);
            Some(channel)
        } else {
            None
        }
    }

    /// `registerChannel` (network_interface.cpp:651): file the channel
    /// under its address.
    pub fn register_channel(&self, channel: &ChannelPtr) -> bool {
        self.channels.borrow_mut().add(channel)
    }

    /// `deregisterChannel` (network_interface.cpp:662): drop the
    /// channel's address. Recently-dead bookkeeping for external
    /// channels is BW-02-05.
    pub fn deregister_channel(&self, channel: &ChannelPtr) -> bool {
        self.channels.borrow_mut().remove(channel)
    }

    /// `setRequestManager`'s seam: the reply bookkeeping the channel
    /// deaths and failures report to.
    pub fn set_request_manager(&self, manager: Option<Rc<RequestManager>>) {
        *self.request_manager.borrow_mut() = manager;
    }

    /// The registered request manager, if any.
    pub fn request_manager(&self) -> Option<Rc<RequestManager>> {
        self.request_manager.borrow().clone()
    }

    /// `onChannelGone` (network_interface.hpp): a channel has died -- any
    /// requests waiting on it fail with `ChannelLost`.
    pub fn on_channel_gone(&self, addr: &Address) {
        if let Some(manager) = self.request_manager.borrow().as_ref() {
            manager.cancel_requests_for_channel(addr);
        }
    }

    /// `cancelRequestsFor( addr )` (network_interface.hpp): a channel's
    /// remote has failed -- same fate for its requests.
    pub fn cancel_requests_for(&self, addr: &Address) {
        if let Some(manager) = self.request_manager.borrow().as_ref() {
            manager.cancel_requests_for_channel(addr);
        }
    }

    /// `delAnonymousChannel` (network_interface.cpp:672): anonymous
    /// channels are cleaned up through here when their remote fails;
    /// anything else is a bookkeeping error to report.
    pub fn del_anonymous(&self, addr: &Address) {
        let channel = match self.find_channel(*addr, false) {
            Some(channel) => channel,
            None => {
                eprintln!(
                    "NetworkInterface::delAnonymousChannel: Tried to delete an anonymous channel that doesn't exist ({addr})"
                );
                return;
            }
        };

        if !channel.borrow().is_anonymous() {
            eprintln!(
                "NetworkInterface::delAnonymousChannel: Tried to delete a channel that is not anonymous ({addr})"
            );
            return;
        }

        UdpChannel::condemn(&channel);
    }

    /// `onAddressDead` (network_interface.cpp:684): an ICMP-style death
    /// for an address fails its anonymous channel -- nobody else holds
    /// it, so nobody else would clean it up.
    pub fn on_address_dead(&self, addr: Address) {
        if let Some(channel) = self.find_channel(addr, false)
            && channel.borrow().is_anonymous()
        {
            UdpChannel::set_remote_failed(&channel);
        }
    }

    /// `addCondemnedChannel`: the channel handed itself over with
    /// `condemn`; the interface supplies the dispatcher and its own
    /// handle for the sweep timer.
    pub fn add_condemned_channel(&self, channel: &ChannelPtr) {
        self.condemned
            .borrow_mut()
            .add(channel, &self.dispatcher, &self.self_weak.borrow());
    }

    /// `deleteFinishedChannels`: one sweep of the condemned collections.
    pub fn delete_finished_channels(&self) {
        let now = self.dispatcher.now();
        self.condemned.borrow_mut().delete_finished_channels(now);
    }

    /// `destroyOwnedChannels` (network_interface.cpp:704): destroy every
    /// channel the interface itself owns -- the anonymous and condemned
    /// ones. Explicitly held channels survive with a warning.
    pub fn destroy_owned_channels(&self) {
        let owned: Vec<ChannelPtr> = {
            let map = self.channels.borrow();
            let mut owned = Vec::new();
            for channel in map.iter() {
                if channel.borrow().is_owned_by_interface() {
                    owned.push(channel.clone());
                } else {
                    eprintln!(
                        "ChannelMap::destroyOwnedChannels: Channel to {} is still registered",
                        channel.borrow().c_str()
                    );
                }
            }
            owned
        };

        // Destroyed outside the map borrow: `destroy` deregisters through
        // this same map.
        for channel in owned {
            UdpChannel::destroy(&channel);
        }
    }

    /// Test access to the condemned collections.
    #[cfg(test)]
    pub fn condemned_channels(&self) -> &RefCell<CondemnedChannels> {
        &self.condemned
    }

    /// How many channels the condemned collections hold, for tests.
    #[cfg(test)]
    pub fn condemned_size(&self) -> usize {
        self.condemned.borrow().size()
    }

    /// Drop everything condemned without destroying, for tests that must
    /// leave no sweep timer behind.
    #[cfg(test)]
    pub fn condemned_clear_for_test(&self) {
        self.condemned.borrow_mut().clear();
    }

    /// `dispatcher().errorReporter()`: the registered reporter, or the
    /// logging default.
    pub fn error_reporter(&self) -> Rc<dyn ErrorReporter> {
        match self.reporter.borrow().as_ref() {
            Some(reporter) => reporter.clone(),
            None => Rc::new(crate::packet_receiver::LogReporter),
        }
    }

    /// The `PacketReceiver::processPacket` routing target: on-channel
    /// packets with a live channel go to that channel's receive dispatch,
    /// `rateLimitPeriod()` (network_interface.cpp:929): the configured
    /// rate-limiting period in seconds. Zero means no rate limiting.
    pub fn rate_limit_period(&self) -> f32 {
        self.rate_limiter.rate_limit_period()
    }

    /// `rateLimitPeriod( period )` (network_interface.cpp:936): the budget
    /// period, in seconds. Setting it clears the running counters, and a
    /// positive period arms the recurring timer that samples the over-budget
    /// counts and resets them; zero leaves rate limiting off.
    pub fn set_rate_limit_period(&self, period: f32) {
        RateLimiter::set_rate_limit_period(&self.rate_limiter, period);
    }

    /// `perIPAddressRateLimit()` (network_interface.hpp:245): the budget
    /// every source IP may spend per period, counted with the port zeroed.
    pub fn per_ip_address_rate_limit(&self) -> u32 {
        self.rate_limiter.per_ip_address_rate_limit()
    }

    /// `perIPAddressRateLimit( limit )` (network_interface.hpp:247).
    pub fn set_per_ip_address_rate_limit(&self, limit_per_period: u32) {
        self.rate_limiter
            .set_per_ip_address_rate_limit(limit_per_period);
    }

    /// `perIPAddressPortRateLimit()` (network_interface.hpp:252): the budget
    /// every source address may spend per period.
    pub fn per_ip_address_port_rate_limit(&self) -> u32 {
        self.rate_limiter.per_ip_address_port_rate_limit()
    }

    /// `perIPAddressPortRateLimit( limit )` (network_interface.hpp:257).
    pub fn set_per_ip_address_port_rate_limit(&self, limit_per_period: u32) {
        self.rate_limiter
            .set_per_ip_address_port_rate_limit(limit_per_period);
    }

    /// `rateLimit/stats/perIPAddressAverage` and
    /// `.../perIPAddressPortAverage` (network_interface.cpp:1207-1228): the
    /// two running averages of the over-budget counts, the per-IP one first.
    pub fn rate_limit_averages(&self) -> (f32, f32) {
        (
            self.rate_limiter.ip_address_rate_limit_average(),
            self.rate_limiter.ip_address_port_rate_limit_average(),
        )
    }

    /// The `PacketReceiver::processPacket` routing target: on-channel
    /// packets with a live channel go to that channel's receive dispatch,
    /// everything else to the interface's off-channel filter (when one is
    /// installed) and then the sink (the interface table that dispatches
    /// messages is BW-02-02). A channel-less on-channel packet falls to
    /// the sink rather than the reference's `REASON_GENERAL_NETWORK`
    /// drop: the sink is the message-dispatch seam, and the channel map's
    /// condemned-handling covers the destroyed-channel case elsewhere.
    pub fn process_packet(&self, addr: &Address, packet: &mut Packet) -> Reason {
        // `processPacket` (packet_receiver.cpp:445-455): the rate limiting
        // speaks before anything else, and it does not distinguish internal
        // from external interfaces -- a rate-limited address is answered
        // with REASON_GENERAL_NETWORK, and its datagram never reaches a
        // channel, even a registered one.
        if self.rate_limiter.increment_and_check_rate_limit(addr) {
            if self.verbose.get() {
                eprintln!(
                    "PacketReceiver::processPacket( {} ): Ignoring packet from rate-limited address",
                    addr.c_str()
                );
            }
            return Reason::GeneralNetwork;
        }
        if packet.has_flags(flags::ON_CHANNEL)
            && let Some(channel) = self.find_channel(*addr, false)
        {
            // `processPacket` (packet_receiver.cpp:471-474): the channel's
            // filter gets first say over the packet, unless the remote has
            // failed; its continuation is the channel's own dispatch.
            let filter = channel.borrow().p_filter();
            let remote_failed = channel.borrow().has_remote_failed();
            if let Some(filter) = filter
                && !remote_failed
            {
                return filter.recv(addr, packet, &|addr, packet| {
                    UdpChannel::process_packet(&channel, addr, packet)
                });
            }
            return UdpChannel::process_packet(&channel, addr, packet);
        }
        // `networkInterface_.pOffChannelFilter()` (packet_receiver.cpp:493):
        // the interface's off-channel filter gets first say over stray
        // datagrams; with none installed they go straight to the sink.
        // This half of `processFilteredPacket` stays as loose as the rest
        // of it here: the size and flag checks run in the on-channel
        // dispatch, and the checksum with them.
        if let Some(filter) = self.off_channel_filter.borrow().as_ref() {
            return filter.recv(addr, packet, &|addr, packet| {
                self.deliver_packet(addr, packet)
            });
        }
        self.deliver_packet(addr, packet)
    }

    /// The message-delivery seam: hand a footer-stripped packet to the
    /// sink. The on-channel dispatch routes here after stripping its
    /// footers; the off-channel entry routes straight here. Accept the
    /// packet silently while there is no sink.
    pub fn deliver_packet(&self, addr: &Address, packet: &mut Packet) -> Reason {
        match self.sink.borrow().as_ref() {
            Some(sink) => sink.process_packet(addr, packet),
            None => Reason::Success,
        }
    }

    /// `NetworkInterface::send` + `PacketSender::send`
    /// (network_interface.cpp:809, packet_sender.cpp:61): finalise the
    /// bundle, fill the footers, and hand each packet to the socket. The
    /// channel is the bundle's owner (reliable, on-channel); off-channel
    /// bundles pass `None`. Each packet runs the reference's
    /// `rescheduleSend` decision first (BW-12-03-04): artificial loss
    /// drops it, artificial latency holds it for a once-off timer, and
    /// only a clear packet reaches the filter and the socket.
    pub fn send(&self, addr: &Address, bundle: &mut UdpBundle, channel: Option<&ChannelPtr>) {
        if !bundle.is_finalised() {
            bundle.finalise();
            if let Some(manager) = self.request_manager() {
                bundle.add_reply_orders_to(&manager, Some(*addr));
            }
        }

        let first_overflow = bundle.prepare_packets(
            &mut self.seq_num_allocator.borrow_mut(),
            self.should_use_checksums.get(),
            channel,
        );

        // `sendRescheduledPacket` (packet_sender.cpp:241-258): the owning
        // channel's filter, when fitted, gets each packet before the
        // physical send; the base `send` forwards straight to it.
        let filter = channel.and_then(|c| c.borrow().p_filter());

        let count = bundle.num_data_units();
        for index in 0..count {
            if Some(index) == first_overflow {
                break;
            }
            if self.reschedule_packet(addr, bundle.packet(index), channel) {
                continue;
            }
            match filter.as_ref() {
                Some(filter) => filter.send(addr, bundle.packet_mut(index), &|addr, packet| {
                    self.send_packet(addr, packet)
                }),
                None => self.send_packet(addr, bundle.packet(index)),
            }
        }
    }

    /// `PacketSender::rescheduleSend` (packet_sender.cpp:158) on the
    /// interface's own knobs: `true` when the packet was dropped (counted
    /// as sent) or held for the once-off delay timer, `false` when it may
    /// go out now.
    fn reschedule_packet(
        &self,
        addr: &Address,
        packet: &Packet,
        channel: Option<&ChannelPtr>,
    ) -> bool {
        let loss = self.loss.borrow();
        let forced = self.is_dropping_next_send.replace(false);
        let decision = reschedule_decision(&loss, &self.rand, forced);
        drop(loss);

        match decision {
            Reschedule::Dropped => {
                // The reference counts a dropped packet as sent
                // (packet_sender.cpp:194).
                let mut stats = self.sending_stats.borrow_mut();
                stats.num_bytes_sent += packet.total_size() as u64 + UDP_OVERHEAD as u64;
                stats.num_packets_sent += 1;
                true
            }
            Reschedule::Delayed { latency_millis } => match self.self_rc() {
                Some(me) => {
                    schedule_rescheduled_packet(
                        &me,
                        *addr,
                        Rc::new(packet.clone()),
                        channel.cloned(),
                        latency_millis,
                    );
                    true
                }
                // The reference's timer holds a plain reference to its
                // interface; the port needs an owning handle, so without
                // one the packet goes out now rather than never.
                None => false,
            },
            Reschedule::Now => false,
        }
    }

    /// `NetworkInterface::setLossRatio` (network_interface.cpp:866): the
    /// artificial-drop ratio, 0..1.
    pub fn set_loss_ratio(&self, loss_ratio: f32) {
        self.loss.borrow_mut().set_loss_ratio(loss_ratio);
    }

    /// `NetworkInterface::setLatency` (network_interface.cpp:854): the
    /// artificial-latency window, in seconds.
    pub fn set_latency(&self, latency_min: f32, latency_max: f32) {
        let mut loss = self.loss.borrow_mut();
        loss.set_min_latency(latency_min);
        loss.set_max_latency(latency_max);
    }

    /// `NetworkInterface::dropNextSend` (network_interface.cpp:875): the
    /// next packet this interface would send is counted and dropped,
    /// exactly once.
    pub fn drop_next_send(&self) {
        self.is_dropping_next_send.set(true);
    }

    /// `NetworkInterface::hasArtificialLossOrLatency`
    /// (network_interface.cpp:1046): whether any knob is live.
    pub fn has_artificial_loss_or_latency(&self) -> bool {
        self.loss.borrow().has_artificial_loss_or_latency()
    }

    /// `pPacketLossParameters_`, read-only: what
    /// `PacketLossParameters::apply` pushed and the send path consults.
    pub fn packet_loss_parameters(&self) -> std::cell::Ref<'_, PacketLossParameters> {
        self.loss.borrow()
    }

    /// `sendingStats_`, read-only: the inline send path's counters.
    pub fn sending_stats(&self) -> std::cell::Ref<'_, SendingStats> {
        self.sending_stats.borrow()
    }

    /// `PacketSender::sendRescheduledPacket` (packet_sender.cpp:241) on
    /// the interface: the owning channel's filter, when fitted, gets the
    /// packet before the physical send; without one it goes straight out.
    /// The [`RescheduledSender`] timer lands here.
    pub(crate) fn send_rescheduled_packet(
        &self,
        addr: &Address,
        packet: &std::rc::Rc<Packet>,
        channel: Option<&ChannelPtr>,
    ) {
        let filter = channel.and_then(|c| c.borrow().p_filter());
        match filter {
            Some(filter) => {
                let mut owned = packet.as_ref().clone();
                filter.send(addr, &mut owned, &|addr, packet| {
                    self.send_packet(addr, packet)
                });
            }
            None => self.send_packet(addr, packet),
        }
    }

    /// `PacketSender::sendPacket`'s physical arm: the datagram goes out on
    /// this interface's socket, counted into this interface's stats the
    /// way `basicSendSingleTry` counts (packet_sender.cpp:347-390). No
    /// result is trusted beyond the counters -- the packet may never
    /// arrive.
    pub fn send_packet(&self, addr: &Address, packet: &Packet) {
        let socket = self.udp_socket.borrow();
        let len = socket.sendto_address(packet.wire(), *addr);
        drop(socket);
        if len == packet.total_size() as i32 {
            let mut stats = self.sending_stats.borrow_mut();
            stats.num_bytes_sent += len as u64 + UDP_OVERHEAD as u64;
            stats.num_packets_sent += 1;
        } else {
            self.sending_stats.borrow_mut().num_failed_packet_send += 1;
            eprintln!(
                "PacketSender::sendPacket( {} ): failed to send {} bytes to {} (sent {})",
                self.c_str(),
                packet.wire().len(),
                addr.c_str(),
                len
            );
        }
    }

    /// `c_str`: the interface's label for messages.
    pub fn c_str(&self) -> String {
        self.address.borrow().c_str()
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

    /// An interface with the off-channel filter installed consults it for
    /// stray datagrams: the admitted request reaches the sink, and a
    /// mismatching packet is dropped with `REASON_GENERAL_NETWORK` before
    /// it gets there. Without a filter the stray goes straight through
    /// (initial_connection_filter.cpp:42-66, packet_receiver.cpp:493).
    #[test]
    fn the_off_channel_filter_gates_stray_datagrams() {
        let (interface, _receiver) = loopback_interface(InterfaceType::External);
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        let filter = Rc::new(crate::packet_filter::InitialConnectionFilter::new(0));
        interface.set_off_channel_filter(Some(filter));
        let addr = Address::new(LOOPBACK, socket::htons(1));

        // The admitted request goes through.
        let mut allowed = crate::packet_filter::tests_support::base_app_login_packet(0);
        assert_eq!(
            interface.process_packet(&addr, &mut allowed),
            Reason::Success
        );
        assert_eq!(sink.packets.borrow().len(), 1);

        // A mismatching packet is dropped before the sink.
        let mut denied = crate::packet_filter::tests_support::base_app_login_packet(5);
        assert_eq!(
            interface.process_packet(&addr, &mut denied),
            Reason::GeneralNetwork
        );
        assert_eq!(
            sink.packets.borrow().len(),
            1,
            "the denied packet is not delivered"
        );

        // With no filter installed, stray datagrams go straight to the
        // sink as before.
        interface.set_off_channel_filter(None);
        let mut unfiltered = crate::packet_filter::tests_support::base_app_login_packet(5);
        assert_eq!(
            interface.process_packet(&addr, &mut unfiltered),
            Reason::Success
        );
        assert_eq!(sink.packets.borrow().len(), 2);
    }

    /// A pinned-clock interface with no socket, for the channel-map
    /// paths. The dispatcher's clock is the shared cell.
    fn channel_interface(
        interface_type: InterfaceType,
    ) -> (Rc<NetworkInterface>, Rc<std::cell::Cell<u64>>) {
        let clock = Rc::new(std::cell::Cell::new(1_000_000u64));
        let clock_cell = Rc::clone(&clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || {
            clock_cell.get()
        })));
        (
            NetworkInterface::new_with_dispatcher(interface_type, dispatcher),
            clock,
        )
    }

    /// `findChannelRef` wraps a live channel, and the receiver's error
    /// paths run through it: an internal remote failure reports to the
    /// request manager, an anonymous one cleans itself up, and the two
    /// `delAnonymousChannel` bookkeeping errors are voiced.
    #[test]
    fn the_error_paths_run_through_the_live_map() {
        let (interface, _clock) = channel_interface(InterfaceType::Internal);
        let address = Address::new(LOOPBACK, socket::htons(7001));

        let channel = UdpChannel::new(
            &interface,
            address,
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        // The live map answers, wrapped for the receiver.
        let handle = interface
            .find_channel_ref(&address)
            .expect("the registered channel is found");
        assert_eq!(handle.c_str(), channel.borrow().c_str());
        assert!(handle.is_internal());
        assert!(!handle.has_remote_failed());

        // An internal failure cancels its requests through the manager.
        let manager = Rc::new(RequestManager::new(false, interface.dispatcher()));
        interface.set_request_manager(Some(manager.clone()));
        handle.set_remote_failed();
        assert!(handle.has_remote_failed());
        assert_eq!(manager.num_pending(), 0);

        // Not anonymous: `delAnonymousChannel` reports the mistake and
        // leaves the channel alone.
        interface.del_anonymous(&address);
        assert!(!channel.borrow().is_destroyed());

        // An anonymous channel at a fresh address is condemned and,
        // nothing unacked, destroyed -- delAnonymousChannel's real work.
        // The first channel still holds the old address, so the anonymous
        // build needs its own.
        let anonymous_address = Address::new(LOOPBACK, socket::htons(7002));
        let anonymous = interface
            .find_channel(anonymous_address, true)
            .expect("an anonymous channel is built");
        assert!(anonymous.borrow().is_anonymous());
        interface.del_anonymous(&anonymous_address);
        assert!(anonymous.borrow().is_destroyed());

        // Nobody is registered there any more: the not-found arm.
        interface.del_anonymous(&anonymous_address);

        // `onAddressDead` on an address with no channel is silent; on the
        // anonymous one it fails the channel.
        interface.on_address_dead(Address::new(LOOPBACK, socket::htons(7004)));
        let fresh_address = Address::new(LOOPBACK, socket::htons(7005));
        let fresh = interface
            .find_channel(fresh_address, true)
            .expect("another anonymous channel");
        interface.on_address_dead(fresh_address);
        assert!(fresh.borrow().has_remote_failed());
        assert!(fresh.borrow().is_destroyed(), "failed and anonymous: gone");
    }

    /// `destroyOwnedChannels` destroys what the interface owns -- the
    /// anonymous channels -- and warns over the explicitly held ones,
    /// which survive registered.
    #[test]
    fn destroy_owned_channels_takes_the_anonymous_and_leaves_the_held() {
        let (interface, _clock) = channel_interface(InterfaceType::Internal);
        let held_address = Address::new(LOOPBACK, socket::htons(7010));
        let owned_address = Address::new(LOOPBACK, socket::htons(7011));

        let held = UdpChannel::new(
            &interface,
            held_address,
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        let anonymous = interface
            .find_channel(owned_address, true)
            .expect("an anonymous channel");
        assert!(anonymous.borrow().is_anonymous());

        interface.destroy_owned_channels();

        assert!(!held.borrow().is_destroyed(), "the held channel survives");
        assert!(interface.find_channel(held_address, false).is_some());
        assert!(
            anonymous.borrow().is_destroyed(),
            "the anonymous one is gone"
        );
        assert!(interface.find_channel(owned_address, false).is_none());
        assert_eq!(interface.condemned_size(), 0);
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

    // -- The channel filter and the checksum (BW-02-05-01) ---------------

    /// A filter that records what it is asked about and hands everything
    /// on, so the order and the pass-through are observable.
    #[derive(Default)]
    struct RecordingFilter {
        recv: std::cell::Cell<usize>,
        sends: std::cell::Cell<usize>,
        saw_recv: std::cell::RefCell<Vec<u8>>,
        saw_send: std::cell::RefCell<Vec<u8>>,
    }

    impl PacketFilter for RecordingFilter {
        fn recv(
            &self,
            addr: &Address,
            packet: &mut Packet,
            next: &dyn Fn(&Address, &mut Packet) -> Reason,
        ) -> Reason {
            self.recv.set(self.recv.get() + 1);
            self.saw_recv.borrow_mut().extend_from_slice(packet.wire());
            next(addr, packet)
        }

        fn send(&self, addr: &Address, packet: &mut Packet, next: &dyn Fn(&Address, &mut Packet)) {
            self.sends.set(self.sends.get() + 1);
            self.saw_send.borrow_mut().extend_from_slice(packet.wire());
            next(addr, packet)
        }
    }

    /// An on-channel reliable datagram as it comes off the socket: flags,
    /// body, the sequence footer, and -- when asked for -- the checksum
    /// footer above it. `corrupt` leaves the stored sum a byte wrong.
    fn on_channel_packet(body: &[u8], seq: u32, checksum: Option<bool>) -> Packet {
        let mut word = flags::ON_CHANNEL | flags::IS_RELIABLE | flags::HAS_SEQUENCE_NUMBER;
        if checksum.is_some() {
            word |= flags::HAS_CHECKSUM;
        }
        let mut wire = Vec::new();
        wire.extend_from_slice(&word.to_le_bytes());
        wire.extend_from_slice(body);
        wire.extend_from_slice(&seq.to_le_bytes());
        if checksum.is_some() {
            wire.extend_from_slice(&[0u8; 4]);
        }

        let mut packet = Packet::new();
        packet.grow(wire.len());
        packet.buffer_mut()[..wire.len()].copy_from_slice(&wire);
        if let Some(corrupt) = checksum {
            let at = packet.msg_end_offset() - 4;
            packet.write_checksum(at);
            if corrupt {
                packet.buffer_mut()[at] ^= 0xff;
            }
        }
        packet
    }

    /// A channel the filter can be fitted on, registered at its own address.
    fn filtered_channel<F: PacketFilter + 'static>(
        interface: &Rc<NetworkInterface>,
        port: u16,
        filter: Rc<F>,
    ) -> ChannelPtr {
        let channel = UdpChannel::new(
            interface,
            Address::new(LOOPBACK, port),
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        channel
            .borrow_mut()
            .set_p_filter(Some(filter as Rc<dyn PacketFilter>));
        channel
    }

    /// The acceptance: a fitted channel filter gets first say over an
    /// incoming packet, and the one it forwards reaches the channel's own
    /// dispatch and, with it, the sink.
    #[test]
    fn the_channel_filter_sits_between_the_socket_and_the_channel() {
        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        let filter = Rc::new(RecordingFilter::default());
        let channel = filtered_channel(&interface, socket::htons(7101), filter.clone());
        let addr = channel.borrow().addr();

        let mut packet = on_channel_packet(b"ABCD", 0, None);
        assert_eq!(
            interface.process_packet(&addr, &mut packet),
            Reason::Success
        );

        assert_eq!(filter.recv.get(), 1, "the filter saw the packet");
        assert_eq!(
            sink.packets.borrow().len(),
            1,
            "the packet reached the sink"
        );
    }

    /// The acceptance: a checksummed packet that fails validates as
    /// corrupted and never reaches the sink, while one that validates
    /// goes through the channel as before. The filter saw both -- it sits
    /// ahead of the checksum, exactly as `processPacket` orders it.
    #[test]
    fn a_checksummed_packet_is_validated_before_the_channel() {
        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        let filter = Rc::new(RecordingFilter::default());
        let channel = filtered_channel(&interface, socket::htons(7102), filter.clone());
        let addr = channel.borrow().addr();

        let mut corrupt = on_channel_packet(b"ABCD", 0, Some(true));
        assert_eq!(
            interface.process_packet(&addr, &mut corrupt),
            Reason::CorruptedPacket,
            "a failed checksum is a corrupted packet"
        );
        assert_eq!(filter.recv.get(), 1, "the filter saw it first");
        assert_eq!(
            sink.packets.borrow().len(),
            0,
            "the corrupt packet is not delivered"
        );

        let mut sound = on_channel_packet(b"ABCD", 0, Some(false));
        assert_eq!(interface.process_packet(&addr, &mut sound), Reason::Success);
        assert_eq!(sink.packets.borrow().len(), 1, "the valid one is delivered");
    }

    /// A filter that decides a packet's fate instead of forwarding it.
    struct DenyingFilter;

    impl PacketFilter for DenyingFilter {
        fn recv(
            &self,
            _addr: &Address,
            _packet: &mut Packet,
            _next: &dyn Fn(&Address, &mut Packet) -> Reason,
        ) -> Reason {
            Reason::GeneralNetwork
        }
    }

    /// The acceptance: the filter's verdict is the packet's -- a filter
    /// that denies keeps the continuation (and so the sink) from running.
    #[test]
    fn a_denying_channel_filter_keeps_the_packet_out() {
        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        let channel = filtered_channel(&interface, socket::htons(7103), Rc::new(DenyingFilter));
        let addr = channel.borrow().addr();

        let mut packet = on_channel_packet(b"ABCD", 0, None);
        assert_eq!(
            interface.process_packet(&addr, &mut packet),
            Reason::GeneralNetwork,
            "the filter's reason is the packet's"
        );
        assert_eq!(
            sink.packets.borrow().len(),
            0,
            "a denied packet never reaches the sink"
        );
    }

    /// The acceptance: a fitted channel filter rides every packet out,
    /// seeing it before it leaves the socket -- a fresh send and a resend
    /// alike -- and what it saw is what went over the wire.
    #[test]
    fn a_channel_filter_rides_each_outgoing_packet() {
        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);

        // The channel's own address, held by a listener, so the datagrams
        // have somewhere to land.
        let mut listener = Endpoint::new();
        listener.socket(socket::SOCK_DGRAM);
        listener.bind(0, LOOPBACK);
        let port = listener.local().port;

        let filter = Rc::new(RecordingFilter::default());
        let channel = filtered_channel(&interface, port, filter.clone());

        UdpChannel::send_bundle(&channel, &mut UdpBundle::new(0));

        assert_eq!(filter.sends.get(), 1, "the filter saw the send");
        let mut seen = [0u8; 16];
        assert!(
            listener.recvfrom_address(&mut seen, &mut Address::default()) > 0,
            "the packet left the interface"
        );
        let saw = filter.saw_send.borrow();
        assert_eq!(
            &seen[..saw.len()],
            &saw[..],
            "the wire bytes are the ones the filter saw"
        );
    }

    /// The BW-02-05-02 acceptance at the transport layer: once the session
    /// cipher is agreed (`setEncryption`), the channel's outgoing bundle is
    /// encrypted -- the datagram on the wire is not the plaintext -- and a
    /// peer holding the same cipher accepts and decrypts it back (the legal
    /// handshake, both ways).
    #[test]
    fn an_agreed_cipher_encrypts_the_channel_and_the_peer_decrypts() {
        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);

        // The peer's socket, so the encrypted datagram has somewhere to land.
        let mut listener = Endpoint::new();
        listener.socket(socket::SOCK_DGRAM);
        listener.bind(0, LOOPBACK);
        let port = listener.local().port;

        let cipher =
            crate::block_cipher::NullCipher::with_key(b"session-encryption-key".to_vec(), 8);
        let channel = UdpChannel::new(
            &interface,
            Address::new(LOOPBACK, port),
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        channel.borrow_mut().set_encryption(Some(cipher.clone()));

        UdpChannel::send_bundle(&channel, &mut UdpBundle::new(0));

        let mut seen = [0u8; 256];
        let len = listener.recvfrom_address(&mut seen, &mut Address::default());
        assert!(len > 0, "the encrypted packet left the interface");
        let len = len as usize;

        // A peer holding the same cipher reconstructs the datagram and its
        // filter accepts it: magic, wastage and block chaining all check out.
        let peer = crate::encryption_filter::EncryptionFilter::create(cipher);
        let mut packet = Packet::new();
        packet.grow(len);
        packet.buffer_mut()[..len].copy_from_slice(&seen[..len]);
        let delivered = Rc::new(RefCell::new(None));
        let reason = peer.recv(&Address::new(LOOPBACK, port), &mut packet, &|_, p| {
            *delivered.borrow_mut() = Some(p.clone());
            Reason::Success
        });

        assert_eq!(
            reason,
            Reason::Success,
            "the peer accepts the encrypted packet"
        );
        let plaintext = delivered.borrow().clone().expect("the peer decrypted");
        assert_ne!(
            plaintext.wire(),
            &seen[..len],
            "the wire really carried ciphertext"
        );
    }

    /// The BW-02-05-03 acceptance at the transport layer: an authentication
    /// failure -- a packet encrypted with a key other than the session's --
    /// is rejected with a failure status, nothing is delivered, and no new
    /// session is left established.
    #[test]
    fn a_wrong_key_authentication_leaves_no_residual_session() {
        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        // The established session's channel, fitted with the session cipher.
        let session = crate::block_cipher::KeyedXorCipher::create(b"session-key".to_vec(), 8);
        let channel = UdpChannel::new(
            &interface,
            Address::new(LOOPBACK, socket::htons(7104)),
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        channel.borrow_mut().set_encryption(Some(session));
        let channel_addr = channel.borrow().addr();

        // A peer encrypts a login packet with a *different* key -- a wrong
        // authentication attempt against the established session. The
        // channel's filter decrypts it with the session cipher and finds
        // garbage where the magic should be.
        let wrong = crate::block_cipher::KeyedXorCipher::create(b"wrong-key".to_vec(), 8);
        let peer = crate::encryption_filter::EncryptionFilter::create(wrong);
        let mut plain = on_channel_packet(b"login", 0, None);
        let encrypted = Rc::new(RefCell::new(None));
        peer.send(&channel_addr, &mut plain, &|_, p| {
            *encrypted.borrow_mut() = Some(p.clone());
        });
        let mut encrypted = encrypted.borrow_mut().take().expect("the peer encrypted");

        assert_eq!(
            interface.process_packet(&channel_addr, &mut encrypted),
            Reason::CorruptedPacket,
            "the wrong-key packet is rejected"
        );
        assert_eq!(
            sink.packets.borrow().len(),
            0,
            "the failed authentication delivers nothing"
        );
        // No residual session: the established channel is still registered
        // and still the same one -- the failed attempt neither destroyed the
        // session nor created a new one.
        let after = interface
            .find_channel(channel_addr, false)
            .expect("the established session is still registered");
        assert!(
            Rc::ptr_eq(&channel, &after),
            "the failed attempt left no residual new session"
        );
    }

    /// An interface for the rate-limiting paths: a pinned clock, so the
    /// period timer can be advanced, a dispatcher, so those timers can be
    /// run, and a recording sink, so a delivered packet is observable.
    fn limited_interface() -> (
        Rc<NetworkInterface>,
        Rc<EventDispatcher>,
        Rc<std::cell::Cell<u64>>,
        Rc<RecordingSink>,
    ) {
        let clock = Rc::new(std::cell::Cell::new(1_000_000u64));
        let clock_cell = Rc::clone(&clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || {
            clock_cell.get()
        })));
        let dispatcher_ref = Rc::clone(&dispatcher);
        let interface = NetworkInterface::new_with_dispatcher(InterfaceType::External, dispatcher);
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));
        (interface, dispatcher_ref, clock, sink)
    }

    /// The five-sample bias the reference arms both averages with
    /// (network_interface.cpp:99-105).
    fn rate_limit_bias() -> f32 {
        foundation::math::ema::Ema::calculate_bias_from_num_samples(5.0, 0.95)
    }

    /// Rate limiting starts off: with no period and no budgets every
    /// datagram is accepted and both averages sit at zero
    /// (network_interface.cpp:970, the `isZero` guard).
    #[test]
    fn rate_limiting_starts_disabled() {
        let (interface, _dispatcher, _clock, sink) = limited_interface();

        assert_eq!(interface.rate_limit_period(), 0.0);
        assert_eq!(interface.per_ip_address_rate_limit(), 0);
        assert_eq!(interface.per_ip_address_port_rate_limit(), 0);

        let addr = Address::new(LOOPBACK, socket::htons(9));
        for _ in 0..8 {
            let mut packet = Packet::new();
            assert_eq!(
                interface.process_packet(&addr, &mut packet),
                Reason::Success
            );
        }
        assert_eq!(sink.packets.borrow().len(), 8);
        assert_eq!(interface.rate_limit_averages(), (0.0, 0.0));
    }

    /// The acceptance boundary: a source IP may spend exactly its budget in
    /// one period, and the packet that crosses it is answered with
    /// REASON_GENERAL_NETWORK. The reference compares `++count > limit`, so
    /// exactly `limit` packets pass (network_interface.cpp:970-988).
    #[test]
    fn the_per_ip_address_limit_drops_the_packet_over_the_boundary() {
        let (interface, _dispatcher, _clock, sink) = limited_interface();
        interface.set_rate_limit_period(1.0);
        interface.set_per_ip_address_rate_limit(3);

        let ports = [9u16, 10, 11, 12];
        for (index, port) in ports.iter().enumerate() {
            let mut packet = Packet::new();
            let addr = Address::new(LOOPBACK, socket::htons(*port));
            let reason = interface.process_packet(&addr, &mut packet);
            if index < 3 {
                assert_eq!(reason, Reason::Success, "port {index} is inside the budget");
            } else {
                assert_eq!(
                    reason,
                    Reason::GeneralNetwork,
                    "the fourth packet crosses the budget"
                );
            }
        }
        assert_eq!(
            sink.packets.borrow().len(),
            3,
            "only the three inside packets reached the sink"
        );
        // Nothing has been sampled yet: the average folds in when the
        // period expires.
        assert_eq!(interface.rate_limit_averages(), (0.0, 0.0));
    }

    /// The per-address budget counts each source address separately: one
    /// address crossing its budget does not touch another address's
    /// (network_interface.cpp:990-1003).
    #[test]
    fn the_per_ip_address_port_limit_counts_each_address_separately() {
        let (interface, _dispatcher, _clock, _sink) = limited_interface();
        interface.set_rate_limit_period(1.0);
        interface.set_per_ip_address_port_rate_limit(2);

        let busy = Address::new(LOOPBACK, socket::htons(9));
        for index in 0..3 {
            let mut packet = Packet::new();
            let reason = interface.process_packet(&busy, &mut packet);
            if index < 2 {
                assert_eq!(reason, Reason::Success, "index {index} is inside");
            } else {
                assert_eq!(reason, Reason::GeneralNetwork, "the third is over");
            }
        }

        let other = Address::new(LOOPBACK, socket::htons(10));
        let mut packet = Packet::new();
        assert_eq!(
            interface.process_packet(&other, &mut packet),
            Reason::Success,
            "the other address keeps its own budget"
        );
    }

    /// When the period expires the counters are cleared and the refused
    /// address is admitted again, and the over-budget counts of the period
    /// that just ended are folded into the per-IP average
    /// (network_interface.cpp:540-546).
    #[test]
    fn the_period_expiry_clears_the_counts_and_samples_the_average() {
        let (interface, dispatcher, clock, _sink) = limited_interface();
        interface.set_rate_limit_period(1.0);
        interface.set_per_ip_address_rate_limit(1);

        let addr = Address::new(LOOPBACK, socket::htons(9));
        let mut first = Packet::new();
        assert_eq!(interface.process_packet(&addr, &mut first), Reason::Success);
        let mut second = Packet::new();
        assert_eq!(
            interface.process_packet(&addr, &mut second),
            Reason::GeneralNetwork
        );
        let mut third = Packet::new();
        assert_eq!(
            interface.process_packet(&addr, &mut third),
            Reason::GeneralNetwork
        );

        clock.set(clock.get() + 1_000_000);
        dispatcher.process_timers();

        let mut fresh = Packet::new();
        assert_eq!(
            interface.process_packet(&addr, &mut fresh),
            Reason::Success,
            "the cleared budget admits the address again"
        );
        assert_eq!(
            interface.rate_limit_averages().0,
            (1.0 - rate_limit_bias()) * 2.0,
            "the two over-budget counts of the period were sampled"
        );
    }

    /// Rate limiting speaks before the channel lookup: a datagram from a
    /// rate-limited address never reaches even a channel that is
    /// registered for it (packet_receiver.cpp:445-455 precedes the channel
    /// arm at :471).
    #[test]
    fn a_channelled_address_is_limited_before_its_channel_is_found() {
        let (interface, _dispatcher, _clock, _sink) = limited_interface();
        interface.set_rate_limit_period(1.0);
        interface.set_per_ip_address_rate_limit(1);

        let port = socket::htons(7788);
        let address = Address::new(LOOPBACK, port);
        let _channel = UdpChannel::new(
            &interface,
            address,
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        let mut first = Packet::new();
        first.set_flags(flags::ON_CHANNEL);
        assert_ne!(
            interface.process_packet(&address, &mut first),
            Reason::GeneralNetwork,
            "the first packet is inside the budget"
        );
        let mut second = Packet::new();
        second.set_flags(flags::ON_CHANNEL);
        assert_eq!(
            interface.process_packet(&address, &mut second),
            Reason::GeneralNetwork,
            "the second is refused before the channel is found"
        );
    }

    /// BW-12-03-04 at the interface level: the knobs are entries a
    /// fixture calls, a dropped packet never reaches the wire, and a held
    /// packet rides the once-off timer of a pinned clock. Two internal
    /// interfaces share one dispatcher so the peer's sink observes the
    /// wire truth.
    #[test]
    fn the_interface_knobs_drop_delay_and_clear_for_fixtures() {
        let clock = Rc::new(std::cell::Cell::new(1_000_000u64));
        let handle = Rc::clone(&clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || handle.get())));

        // The peer: a sink watching what arrives.
        let peer =
            NetworkInterface::new_with_dispatcher(InterfaceType::Internal, Rc::clone(&dispatcher));
        let peer_receiver = PacketReceiver::new();
        assert!(peer.recreate_listening_socket(&peer_receiver, 0, "127.0.0.1"));
        peer_receiver.attach_interface(&peer);
        let sink = Rc::new(RecordingSink::default());
        peer.set_packet_sink(Some(sink.clone()));

        // The sender, on the same pinned clock so the delay timer is
        // driven by the fixture, not by wall time.
        let sender =
            NetworkInterface::new_with_dispatcher(InterfaceType::Internal, Rc::clone(&dispatcher));
        let sender_receiver = PacketReceiver::new();
        assert!(sender.recreate_listening_socket(&sender_receiver, 0, "127.0.0.1"));
        sender_receiver.attach_interface(&sender);
        let peer_addr = peer.address();

        // One fixed-length message, one datagram.
        let send_one = || {
            let mut bundle = UdpBundle::new(0);
            let element = crate::interface_element::InterfaceElement::new(
                "m",
                1,
                crate::interface_element::FIXED_LENGTH_MESSAGE,
                4,
                None,
            );
            bundle.start_message(&element, crate::udp_bundle::ReliableType::Driver);
            bundle.write_bytes(&7u32.to_le_bytes());
            sender.send(&peer_addr, &mut bundle, None);
        };

        // A plain send lands on the next pump.
        send_one();
        assert_eq!(sender.sending_stats().num_packets_sent, 1);
        dispatcher.process_once(false);
        assert_eq!(sink.packets.borrow().len(), 1, "the packet arrived");

        // A full loss ratio counts every packet on the sender and puts
        // nothing on the wire.
        sender.set_loss_ratio(1.0);
        send_one();
        send_one();
        dispatcher.process_once(false);
        assert_eq!(
            sender.sending_stats().num_packets_sent,
            3,
            "drops count as sends"
        );
        assert_eq!(sink.packets.borrow().len(), 1, "the wire saw nothing");

        // Back to clean; the one-shot drops exactly the next packet.
        sender.set_loss_ratio(0.0);
        sender.drop_next_send();
        send_one();
        send_one();
        send_one();
        dispatcher.process_once(false);
        assert_eq!(
            sender.sending_stats().num_packets_sent,
            6,
            "three sent, one forced drop counted, two resent... no: two more sent"
        );
        assert_eq!(
            sink.packets.borrow().len(),
            3,
            "the second and third bundle arrived"
        );

        // A latency window holds the packet on a once-off timer; the
        // pinned clock fires it exactly when the fixture says so.
        sender.set_latency(0.5, 0.5);
        send_one();
        dispatcher.process_once(false);
        assert_eq!(
            sink.packets.borrow().len(),
            3,
            "held before the clock moved"
        );
        clock.set(clock.get() + 600_000);
        dispatcher.process_once(false);
        assert_eq!(
            sink.packets.borrow().len(),
            4,
            "delivered when the clock did"
        );
        assert!(sender.has_artificial_loss_or_latency());
        sender.set_latency(0.0, 0.0);
        assert!(!sender.has_artificial_loss_or_latency());
    }

    /// The channel one-shot forwards to the interface exactly once
    /// (udp_channel.cpp:774-778): the first bundle after
    /// [`UdpChannel::drop_next_send`] vanishes, the next goes out.
    #[test]
    fn the_channel_one_shot_forces_the_interface_drop_once() {
        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);

        // The peer's socket, so the datagrams have somewhere to land.
        // Non-blocking: the drop assertions below read an empty socket.
        let mut listener = Endpoint::new();
        listener.socket(socket::SOCK_DGRAM);
        listener.bind(0, LOOPBACK);
        listener.set_nonblocking(true);
        let port = listener.local().port;

        let channel = UdpChannel::new(
            &interface,
            Address::new(LOOPBACK, port),
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        channel.borrow().drop_next_send();
        UdpChannel::send_bundle(&channel, &mut UdpBundle::new(0));
        let mut seen = [0u8; 16];
        assert!(
            listener.recvfrom_address(&mut seen, &mut Address::default()) <= 0,
            "the forced drop kept the bundle off the wire"
        );
        assert_eq!(
            interface.sending_stats().num_packets_sent,
            1,
            "the drop counted as a send"
        );

        // The one-shot cleared: the next bundle goes out untouched.
        UdpChannel::send_bundle(&channel, &mut UdpBundle::new(0));
        assert!(
            listener.recvfrom_address(&mut seen, &mut Address::default()) > 0,
            "the second bundle went out"
        );
        assert_eq!(interface.sending_stats().num_packets_sent, 2);
    }
}
