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
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};

use foundation::math::ema::{AccumulatingEma, Ema};
use foundation::time_queue::{TimerHandle, TimerHandlerFn};
use platform::socket;

use crate::channel_map::ChannelMap;
use crate::condemned_channels::CondemnedChannels;
use crate::endpoint::{Address, Endpoint, INADDR_ANY, NO_SOCKET, NONE};
use crate::event_dispatcher::EventDispatcher;
use crate::fragmented_bundle::{FragmentedBundle, FragmentedBundles};
use crate::interface_table::InterfaceTable;
use crate::misc::{CHANNEL_ID_NULL, Reason, SEQ_NULL, SeqNumAllocator, seq_less_than};
use crate::packet::{Packet, flags};
use crate::packet_filter::PacketFilter;
use crate::packet_receiver::{ErrorReporter, PacketReceiver, PacketSink, strerror};
use crate::packet_receiver_stats::PacketReceiverStats;
use crate::poller::EventPoller;
use crate::recently_dead_channels::RecentlyDeadChannels;
use crate::request_manager::RequestManager;
use crate::udp_bundle::UdpBundle;
use crate::udp_channel::{
    AddToReceiveWindowResult, ChannelPtr, DEFAULT_INACTIVITY_RESEND_DELAY, FragmentResult,
    Traits as ChannelTraits, UdpChannel,
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
    /// `pRecentlyDeadChannels_` (network_interface.cpp:160-170): only an
    /// external interface has one.
    recently_dead: RefCell<Option<RecentlyDeadChannels>>,
    /// `rateLimitPeriod_` (network_interface.hpp:294): the rate-limiting
    /// window in seconds. A zero period turns the whole feature off, and
    /// a zero per-address limit turns that bucket off.
    rate_limit_period: Cell<f64>,
    /// `rateLimitTimer_` (hpp:295): fires once per period, sampling both
    /// averages and clearing the counts.
    rate_limit_timer: RefCell<TimerHandle<u64>>,
    /// `rateLimitPerIPAddress_` (hpp:297): the per-address limit.
    rate_limit_per_ip_address: Cell<u32>,
    /// `rateLimitedIPAddresses_` (hpp:298): datagram counts per address,
    /// keyed with the port zeroed.
    rate_limited_ip_addresses: RefCell<BTreeMap<Address, u32>>,
    /// `ipAddressRateLimitAverage_` (hpp:300).
    ip_address_rate_limit_average: RefCell<AccumulatingEma<u32>>,
    /// `rateLimitPerIPAddressPort_` (hpp:302): the per-full-address limit.
    rate_limit_per_ip_address_port: Cell<u32>,
    /// `rateLimitedIPAddressPorts_` (hpp:303): datagram counts per full
    /// address.
    rate_limited_ip_address_ports: RefCell<BTreeMap<Address, u32>>,
    /// `ipAddressPortRateLimitAverage_` (hpp:304).
    ip_address_port_rate_limit_average: RefCell<AccumulatingEma<u32>>,
    /// `pRequestManager_`.
    request_manager: RefCell<Option<Rc<RequestManager>>>,
    /// `pInterfaceTable_` (network_interface.hpp:326): the message table
    /// the receive paths dispatch through. `interfaceTable()`, like the
    /// reference, is the channel's way to reach the served handlers.
    interface_table: RefCell<Option<Rc<InterfaceTable>>>,
    /// Where `processPacket` hands datagrams until channels exist.
    sink: RefCell<Option<Rc<dyn PacketSink>>>,
    /// `dispatcher().errorReporter()`, defaulting to the logging reporter.
    reporter: RefCell<Option<Rc<dyn ErrorReporter>>>,
    /// `findChannel`, settable until the channel map lands (BW-02-03).
    channel_finder: RefCell<Option<Rc<ChannelFinder>>>,
    /// This interface's own shared handle, so channel creation
    /// (`findChannel`'s anonymous arm) can hand out an `Rc` of it.
    self_weak: RefCell<Weak<NetworkInterface>>,
    /// `seqNumAllocator_`: the sequence numbers off-channel (and
    /// unreliable on-channel) traffic uses.
    seq_num_allocator: RefCell<SeqNumAllocator>,
    /// `shouldUseChecksums()`: whether `send` stamps a checksum on every
    /// packet. Off by default; the reference reads a global flag.
    should_use_checksums: Cell<bool>,
    /// `stats_`: the receive-side counts the filtered path fills in.
    receive_stats: RefCell<PacketReceiverStats>,
    /// The off-channel fragment bundles mid-reassembly (`onceOffReceiver_`
    /// in the reference; the once-off receiver is BW-02-03-08, so the
    /// interface holds the map for now).
    fragmented_bundles: RefCell<FragmentedBundles>,
    /// `pOffChannelFilter_`: the connection filter gating every datagram
    /// that found no channel (BW-02-04-03). The reference's enabled
    /// instance is the BaseApp's `InitialConnectionFilter`.
    off_channel_filter: RefCell<Option<Rc<dyn PacketFilter>>>,
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
        let interface = NetworkInterface {
            udp_socket: RefCell::new(Endpoint::new()),
            address: RefCell::new(NONE),
            is_external: interface_type == InterfaceType::External,
            verbose: Cell::new(true),
            poller: dispatcher.poller(),
            dispatcher,
            channels: RefCell::new(ChannelMap::new()),
            condemned: RefCell::new(CondemnedChannels::new()),
            recently_dead: RefCell::new(
                (interface_type == InterfaceType::External).then(RecentlyDeadChannels::new),
            ),
            rate_limit_period: Cell::new(0.0),
            rate_limit_timer: RefCell::new(TimerHandle::default()),
            rate_limit_per_ip_address: Cell::new(0),
            rate_limited_ip_addresses: RefCell::new(BTreeMap::new()),
            ip_address_rate_limit_average: RefCell::new(AccumulatingEma::new(
                Ema::calculate_bias_from_num_samples(5.0, 0.95),
                0.0,
                0,
            )),
            rate_limit_per_ip_address_port: Cell::new(0),
            rate_limited_ip_address_ports: RefCell::new(BTreeMap::new()),
            ip_address_port_rate_limit_average: RefCell::new(AccumulatingEma::new(
                Ema::calculate_bias_from_num_samples(5.0, 0.95),
                0.0,
                0,
            )),
            request_manager: RefCell::new(None),
            interface_table: RefCell::new(None),
            sink: RefCell::new(None),
            reporter: RefCell::new(None),
            channel_finder: RefCell::new(None),
            self_weak: RefCell::new(Weak::new()),
            seq_num_allocator: RefCell::new(SeqNumAllocator::new(0)),
            should_use_checksums: Cell::new(false),
            receive_stats: RefCell::new(PacketReceiverStats::new()),
            fragmented_bundles: RefCell::new(FragmentedBundles::new()),
            off_channel_filter: RefCell::new(None),
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

    /// `pOffChannelFilter()`: the installed connection filter, if any.
    pub fn p_off_channel_filter(&self) -> Option<Rc<dyn PacketFilter>> {
        self.off_channel_filter.borrow().clone()
    }

    /// `pOffChannelFilter( pFilter )` (network_interface.hpp:135): install
    /// or clear the filter gating channel-less datagrams.
    pub fn set_p_off_channel_filter(&self, filter: Option<Rc<dyn PacketFilter>>) {
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

    /// `deregisterChannel` (network_interface.cpp:487-497): drop the
    /// channel's address, remembering it as recently dead when the
    /// interface has that memory at all.
    pub fn deregister_channel(&self, channel: &ChannelPtr) -> bool {
        let (external, addr) = {
            let channel = channel.borrow();
            (channel.is_external(), channel.addr())
        };
        if external && let Some(dead) = self.recently_dead.borrow_mut().as_mut() {
            dead.add(&addr, &self.dispatcher, &self.self_weak.borrow());
        }
        self.channels.borrow_mut().remove(channel)
    }

    /// `isDead( addr )` (network_interface.cpp:525-528): has this address
    /// lost a channel recently?
    pub fn is_dead(&self, addr: &Address) -> bool {
        self.recently_dead
            .borrow()
            .as_ref()
            .is_some_and(|dead| dead.is_dead(addr))
    }

    /// The arm `RecentlyDeadChannels::handleTimeout` fires through: the
    /// interface hands the expired handle back to its collection.
    pub(crate) fn forget_recently_dead(&self, handle: TimerHandle<u64>) {
        if let Some(dead) = self.recently_dead.borrow_mut().as_mut() {
            dead.forget_by_handle(handle);
        }
    }

    /// `rateLimitPeriod()` (network_interface.hpp:242): the window in
    /// seconds, or zero when rate limiting is off.
    pub fn rate_limit_period(&self) -> f64 {
        self.rate_limit_period.get()
    }

    /// `perIPAddressRateLimit()` (network_interface.hpp:245).
    pub fn per_ip_address_rate_limit(&self) -> u32 {
        self.rate_limit_per_ip_address.get()
    }

    /// `perIPAddressPortRateLimit()` (network_interface.hpp:252).
    pub fn per_ip_address_port_rate_limit(&self) -> u32 {
        self.rate_limit_per_ip_address_port.get()
    }

    /// `perIPAddressRateLimit( uint )` (network_interface.hpp:247).
    pub fn set_per_ip_address_rate_limit(&self, limit: u32) {
        self.rate_limit_per_ip_address.set(limit);
    }

    /// `perIPAddressPortRateLimit( uint )` (network_interface.hpp:257).
    pub fn set_per_ip_address_port_rate_limit(&self, limit: u32) {
        self.rate_limit_per_ip_address_port.set(limit);
    }

    /// `rateLimitPeriod( float )` (network_interface.cpp:936-951): cancel
    /// the period timer, clear the counters, then re-arm a recurring
    /// timer when the period is a positive number of seconds.
    pub fn set_rate_limit_period(&self, period: f64) {
        self.rate_limit_timer.borrow_mut().cancel();
        self.clear_rate_limit_counts();
        self.rate_limit_period.set(period);

        if period != 0.0 && period > 0.0 {
            let interface = self.self_weak.borrow().clone();
            let handler = Rc::new(TimerHandlerFn::new(
                move |_handle: TimerHandle<u64>, _user: usize| {
                    if let Some(interface) = interface.upgrade() {
                        interface.on_rate_limit_timeout();
                    }
                },
                "NetworkInterface::rateLimitPeriod",
            ));
            *self.rate_limit_timer.borrow_mut() = self.dispatcher.add_timer(
                (period * 1_000_000.0) as i64,
                handler,
                0,
                "NetworkInterface::rateLimitPeriod",
            );
        }
    }

    /// `clearRateLimitCounts` (network_interface.cpp:957-964).
    fn clear_rate_limit_counts(&self) {
        self.rate_limited_ip_addresses.borrow_mut().clear();
        self.rate_limited_ip_address_ports.borrow_mut().clear();
        *self.ip_address_rate_limit_average.borrow_mut().value_mut() = 0;
        *self
            .ip_address_port_rate_limit_average
            .borrow_mut()
            .value_mut() = 0;
    }

    /// `handleTimeout` (network_interface.cpp:540-546): the period has
    /// elapsed, so sample both accumulated counters into their weighted
    /// averages and reset them.
    fn on_rate_limit_timeout(&self) {
        self.ip_address_rate_limit_average.borrow_mut().sample(true);
        self.ip_address_port_rate_limit_average
            .borrow_mut()
            .sample(true);
        self.clear_rate_limit_counts();
    }

    /// `incrementAndCheckRateLimit( addr )`
    /// (network_interface.cpp:970-1004): count the datagram against the
    /// two limits and say whether the address has now gone over. A zero
    /// period means the feature is off and the counts stay at zero.
    pub fn increment_and_check_rate_limit(&self, addr: &Address) -> bool {
        if self.rate_limit_period.get() == 0.0 {
            return false;
        }

        let mut should_rate_limit = false;

        // Check and update the IP address.
        if self.rate_limit_per_ip_address.get() > 0 {
            let zeroed_port = Address::new(addr.ip, 0);
            let mut counts = self.rate_limited_ip_addresses.borrow_mut();
            let count = counts.entry(zeroed_port).or_insert(0);
            *count += 1;
            if *count > self.rate_limit_per_ip_address.get() {
                self.ip_address_rate_limit_average
                    .borrow_mut()
                    .increment_value();
                should_rate_limit = true;
            }
        }

        // Check and update the IP address and port pair.
        if self.rate_limit_per_ip_address_port.get() > 0 {
            let mut counts = self.rate_limited_ip_address_ports.borrow_mut();
            let count = counts.entry(*addr).or_insert(0);
            *count += 1;
            if *count > self.rate_limit_per_ip_address_port.get() {
                self.ip_address_port_rate_limit_average
                    .borrow_mut()
                    .increment_value();
                should_rate_limit = true;
            }
        }

        should_rate_limit
    }

    /// The two rate-limit averages, per address and per address-and-port,
    /// as the reference's `rateLimit/stats/*` watchers expose them
    /// (network_interface.cpp:1220-1228): per address first.
    pub fn rate_limit_averages(&self) -> (f32, f32) {
        (
            self.ip_address_rate_limit_average.borrow().average(),
            self.ip_address_port_rate_limit_average.borrow().average(),
        )
    }

    /// `setRequestManager`'s seam: the reply bookkeeping the channel
    /// deaths and failures report to.
    pub fn set_request_manager(&self, manager: Option<Rc<RequestManager>>) {
        *self.request_manager.borrow_mut() = manager;
    }

    /// `setInterfaceTable`'s seam: the message table the receive paths
    /// dispatch through. Nothing is wired until the owner (the baseapp
    /// side of the port) hands one over.
    pub fn set_interface_table(&self, table: Option<Rc<InterfaceTable>>) {
        *self.interface_table.borrow_mut() = table;
    }

    /// `interfaceTable()` (network_interface.hpp): the table the receive
    /// paths walk. The TCP channel reaches the served handlers through
    /// it, the same way the reference's `TCPBundleProcessor` does.
    pub fn interface_table(&self) -> Option<Rc<InterfaceTable>> {
        self.interface_table.borrow().clone()
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

    /// The `PacketReceiver::processPacket` routing target: find the
    /// packet's channel, then run the filtered path. The sink is where
    /// an ordered packet's messages are delivered -- the interface table
    /// that dispatches them is BW-02-02.
    ///
    /// `PacketReceiver::processPacket` (packet_receiver.cpp:445): the
    /// entry point for a new datagram. Rate limiting arrives with
    /// BW-02-05-04. A found channel first-passes the datagram through its
    /// own packet filter (`pChannel->pFilter() && !pChannel->hasRemoteFailed()`,
    /// packet_receiver.cpp:471-474) -- the filter's verdict is the
    /// result, and the base `PacketFilter::recv` is the pass-through to
    /// the receive path below. A datagram that finds no channel but whose
    /// address recently lost one (the recently-dead guard, :474-487) is
    /// dropped. Otherwise it meets the interface's connection filter
    /// (`pOffChannelFilter_`, :492) before the receive path.
    pub fn process_packet(&self, addr: &Address, packet: &mut Packet) -> Reason {
        // Rate limiting speaks before anything else
        // (packet_receiver.cpp:445-455): an address over its limit is
        // answered with REASON_GENERAL_NETWORK, and its datagram never
        // reaches a channel, even a registered one.
        if self.increment_and_check_rate_limit(addr) {
            if self.is_verbose() {
                eprintln!(
                    "PacketReceiver::processPacket( {} ): Ignoring packet from rate-limited address",
                    addr.c_str()
                );
            }
            return Reason::GeneralNetwork;
        }

        let create_anonymous = !self.is_external && packet.should_create_anonymous();

        if let Some(channel) = self.find_channel(*addr, create_anonymous) {
            // We update received times for addressed channels here.
            // Indexed channels are done in process_filtered_packet.
            UdpChannel::on_packet_received(&channel, packet.total_size() as i32);
            let (p_filter, remote_failed) = {
                let channel = channel.borrow();
                (channel.p_filter(), channel.has_remote_failed())
            };
            // The channel's own filter first-passes the datagram; a
            // failed remote skips it, matching packet_receiver.cpp:471-474.
            if let (Some(filter), false) = (p_filter, remote_failed) {
                return filter.recv(self, addr, packet);
            }
        } else if self.is_external && self.is_dead(addr) {
            // A recently dead channel's datagrams are dropped, not
            // processed raw (packet_receiver.cpp:474-487): the client's
            // session is over, so nothing it sends is worth parsing.
            if self.is_verbose() {
                eprintln!(
                    "PacketReceiver::processPacket( {} ): Ignoring incoming packet on recently dead channel",
                    addr.c_str()
                );
            }
            return Reason::Success;
        } else if let Some(filter) = self.p_off_channel_filter() {
            // The interface's connection filter decides what a channel-less
            // datagram is worth; the else-if is the reference's, so a
            // found channel skips the filter even when it has none.
            return filter.recv(self, addr, packet);
        }

        self.process_filtered_packet(addr, packet)
    }

    /// `shouldUseChecksums()` setter, for tests and config.
    pub fn set_should_use_checksums(&self, enabled: bool) {
        self.should_use_checksums.set(enabled);
    }

    /// The receive-side stats the filtered path counts into.
    pub fn receive_stats(&self) -> &RefCell<PacketReceiverStats> {
        &self.receive_stats
    }

    /// The off-channel fragment bundles this interface is mid-reassembly
    /// on (`onceOffReceiver_.fragmentedBundles()`).
    pub fn fragmented_bundles(&self) -> &RefCell<FragmentedBundles> {
        &self.fragmented_bundles
    }

    /// `PacketReceiver::processFilteredPacket` (packet_receiver.cpp:518):
    /// the header guards, the footer strip (checksum, piggybacks,
    /// indexed-channel id, cumulative ack, acks, sequence number), the
    /// channel receive window, and the ordered dispatch. `RETURN_FOR_
    /// CORRUPTED_PACKET` is the guard-and-return idiom throughout.
    pub fn process_filtered_packet(&self, addr: &Address, packet: &mut Packet) -> Reason {
        macro_rules! corrupted {
            () => {{
                self.receive_stats.borrow_mut().inc_corrupted_packets();
                return Reason::CorruptedPacket;
            }};
        }

        if packet.total_size() < 2 {
            if self.is_verbose() {
                eprintln!(
                    "PacketReceiver::processFilteredPacket( {} ): received undersized packet",
                    addr.c_str()
                );
            }
            corrupted!();
        }

        // Make sure we understand all the flags.
        if packet.flags() & !flags::KNOWN_FLAGS != 0 {
            if self.is_verbose() {
                eprintln!(
                    "PacketReceiver::processFilteredPacket( {} ): received packet with bad flags {:x}",
                    addr.c_str(),
                    packet.flags()
                );
            }
            corrupted!();
        }

        if !packet.has_flags(flags::ON_CHANNEL) {
            self.receive_stats.borrow_mut().inc_packets_off_channel();
        }

        // Don't allow FLAG_CREATE_CHANNEL on external interfaces.
        if self.is_external && packet.has_flags(flags::CREATE_CHANNEL) {
            if self.is_verbose() {
                eprintln!(
                    "PacketReceiver::processFilteredPacket( {} ): \
                     Got FLAG_CREATE_CHANNEL on external interface",
                    addr.c_str()
                );
            }
            corrupted!();
        }

        // Make sure there's something in the packet.
        if packet.total_size() <= 2 {
            if self.is_verbose() {
                eprintln!(
                    "PacketReceiver::processFilteredPacket( {} ): received undersize packet ({} bytes)",
                    addr.c_str(),
                    packet.total_size()
                );
            }
            corrupted!();
        }

        // Start stripping footers.
        if !packet.validate_checksum() {
            if self.is_verbose() {
                eprintln!(
                    "PacketReceiver::processFilteredPacket( {} ): Invalid checksum",
                    addr.c_str()
                );
            }
            corrupted!();
        }

        // Strip off piggybacks and process them immediately (i.e. before
        // the messages on this packet) as the piggybacks must be older
        // than this packet.
        let piggybacks_ok = packet.process_piggyback_packets(|inner| {
            self.process_filtered_packet(addr, inner) != Reason::CorruptedPacket
        });
        if !piggybacks_ok {
            if self.is_verbose() {
                eprintln!(
                    "PacketReceiver::processFilteredPacket( {} ): Corrupted piggyback packets",
                    addr.c_str()
                );
            }
            corrupted!();
        }

        // Strip off indexed channel ID if present. No indexed channel
        // finder is registered in this port yet, so the packet -- like
        // the reference's without a finder -- is corrupt.
        if packet.has_flags(flags::INDEXED_CHANNEL) {
            let id = match packet.strip_footer_u32() {
                Some(id) => id,
                None => {
                    if self.is_verbose() {
                        eprintln!(
                            "PacketReceiver::processFilteredPacket( {} ): \
                             Not enough data for indexed channel footer ({} bytes left)",
                            addr.c_str(),
                            packet.body_size()
                        );
                    }
                    corrupted!();
                }
            };
            let version = match packet.strip_footer_u32() {
                Some(version) => version,
                None => {
                    if self.is_verbose() {
                        eprintln!(
                            "PacketReceiver::processFilteredPacket( {} ): \
                             Not enough data for indexed channel footer ({} bytes left)",
                            addr.c_str(),
                            packet.body_size()
                        );
                    }
                    corrupted!();
                }
            };
            packet.set_channel_id(id as i32);
            packet.set_channel_version(version);

            if self.is_verbose() {
                eprintln!(
                    "PacketReceiver::processFilteredPacket( {} ): \
                     Got indexed channel packet with no finder registered",
                    addr.c_str()
                );
            }
            corrupted!();
        }

        // Find the channel the packet is on.
        let p_channel: Option<ChannelPtr> = if packet.has_flags(flags::ON_CHANNEL) {
            match self.find_channel(*addr, false) {
                Some(channel) => Some(channel),
                None => {
                    if self.is_verbose() {
                        eprintln!(
                            "PacketReceiver::processFilteredPacket( {} ): \
                             Dropping packet due to absence of local channel",
                            addr.c_str()
                        );
                    }
                    return Reason::GeneralNetwork;
                }
            }
        } else {
            None
        };

        if let Some(channel) = p_channel.as_ref() {
            let this = channel.borrow();

            if this.has_remote_failed() {
                if self.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): \
                         Dropping packet due to remote process failure",
                        this.c_str()
                    );
                }
                return Reason::GeneralNetwork;
            }

            // If the packet is out of date, drop it.
            if seq_less_than(packet.channel_version(), this.creation_version()) {
                if self.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): \
                         Dropping packet from old channel {} (v{} < v{})",
                        this.c_str(),
                        addr.c_str(),
                        packet.channel_version(),
                        this.creation_version()
                    );
                }
                return Reason::Success;
            }
        }

        let mut should_send_channel = false;

        // Cumulative ack: everything before a sequence has arrived.
        if packet.has_flags(flags::HAS_CUMULATIVE_ACK) {
            let channel = match p_channel.as_ref() {
                Some(channel) => channel.clone(),
                None => {
                    if self.is_verbose() {
                        eprintln!(
                            "PacketReceiver::processFilteredPacket( {} ): \
                             Cumulative ack was not on a channel.",
                            addr.c_str()
                        );
                    }
                    corrupted!();
                }
            };

            let end_seq = match packet.strip_footer_u32() {
                Some(end_seq) => end_seq,
                None => {
                    if self.is_verbose() {
                        eprintln!(
                            "PacketReceiver::processFilteredPacket( {} ): \
                             Not enough data for cumulative.",
                            channel.borrow().c_str()
                        );
                    }
                    corrupted!();
                }
            };

            if !UdpChannel::handle_cumulative_ack(&channel, end_seq) {
                corrupted!();
            }
        }

        // Strip and handle ACKs.
        if packet.has_flags(flags::HAS_ACKS) {
            let n_acks = match packet.strip_footer_u8() {
                Some(n_acks) => n_acks,
                None => {
                    if self.is_verbose() {
                        eprintln!(
                            "PacketReceiver::processFilteredPacket( {} ): \
                             Not enough data for ack count footer ({} bytes left)",
                            addr.c_str(),
                            packet.body_size()
                        );
                    }
                    corrupted!();
                }
            };

            if n_acks == 0 {
                if self.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): \
                         Packet with FLAG_HAS_ACKS had 0 acks",
                        addr.c_str()
                    );
                }
                corrupted!();
            }

            // The total size of all the ACKs on this packet.
            let ack_size = n_acks as usize * 4;

            // Check that we have enough footers to account for all of the
            // acks the packet claims to have.
            if packet.body_size() < ack_size {
                if self.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): \
                         Not enough footers for {} acks (have {} bytes but need {})",
                        addr.c_str(),
                        n_acks,
                        packet.body_size(),
                        ack_size
                    );
                }
                corrupted!();
            }

            // For each ACK that we receive, we no longer need to store
            // the corresponding packet.
            match p_channel.as_ref() {
                Some(channel) => {
                    for _ in 0..n_acks {
                        let seq = match packet.strip_footer_u32() {
                            Some(seq) => seq,
                            None => {
                                if self.is_verbose() {
                                    eprintln!(
                                        "PacketReceiver::processFilteredPacket( {} ): \
                                         Not enough data for acknowledgement footer ({} bytes left)",
                                        addr.c_str(),
                                        packet.body_size()
                                    );
                                }
                                corrupted!();
                            }
                        };

                        if !UdpChannel::handle_ack(channel, seq) {
                            if self.is_verbose() {
                                eprintln!(
                                    "PacketReceiver::processFilteredPacket( {} ): \
                                     handleAck() failed for #{}",
                                    addr.c_str(),
                                    seq
                                );
                            }
                            corrupted!();
                        }
                    }
                }
                None => {
                    // Once-off acks go to the once-off sender's resend
                    // timers; without one ported, shrink past them like
                    // the reference's no-channel warning arm.
                    packet.shrink(ack_size);
                }
            }
        }

        // Strip sequence number. Like the reference's
        // `p->stripFooter( p->seq() )`, the value goes back into the
        // packet so the reliability guard and receive window can read it.
        if packet.has_flags(flags::HAS_SEQUENCE_NUMBER) {
            match packet.strip_footer_u32() {
                Some(seq) => packet.set_seq(seq),
                None => {
                    if self.is_verbose() {
                        eprintln!(
                            "PacketReceiver::processFilteredPacket( {} ): \
                             Not enough data for sequence number footer ({} bytes left)",
                            addr.c_str(),
                            packet.body_size()
                        );
                    }
                    corrupted!();
                }
            }
        }

        // Now do something if it's reliable.
        if packet.has_flags(flags::IS_RELIABLE) {
            // First make sure it has a sequence number, so we can
            // address it.
            if packet.seq() == SEQ_NULL {
                if self.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): \
                         Dropping packet due to illegal request for \
                         reliability without related sequence number",
                        addr.c_str()
                    );
                }
                corrupted!();
            }

            // Should we be looking in a channel?
            if let Some(channel) = p_channel.as_ref() {
                let (result, ready) = {
                    let mut stats = self.receive_stats.borrow_mut();
                    UdpChannel::add_to_receive_window(channel, packet.clone(), *addr, &mut stats)
                };

                if !channel.borrow().is_local_regular() {
                    should_send_channel = true;
                }

                if result != AddToReceiveWindowResult::PacketIsNextInWindow {
                    // The packet is not corrupted, and has either already
                    // been received, or is too early and has been
                    // buffered. In either case, we send the ACK
                    // immediately, as long as the channel is established
                    // and is irregular.
                    if result != AddToReceiveWindowResult::PacketIsCorrupt
                        && channel.borrow().is_established()
                        && should_send_channel
                    {
                        self.push_pending_acks(channel);
                    }

                    return Reason::Success;
                }

                // The ordered run dispatches below.
                let mut oret = Reason::Success;
                for mut ordered in ready {
                    if channel.borrow().is_condemned() {
                        break;
                    }
                    let reason = self.process_ordered_packet(addr, &mut ordered, Some(channel));
                    if oret == Reason::Success {
                        oret = reason;
                    }
                }

                // If this bundle was delivered to a channel and there are
                // still ACKs to send, do it now.
                if !channel.borrow().is_destroyed()
                    && should_send_channel
                    && channel.borrow().is_established()
                    && channel.borrow().has_acks()
                {
                    self.push_pending_acks(channel);
                }

                return oret;
            }

            // If the packet is not on a channel (i.e. FLAG_ON_CHANNEL is
            // not set), it must be once-off reliable.
            //
            // Don't allow incoming once-off-reliable traffic on external
            // interfaces since this is a potential DOS vulnerability.
            if self.is_external {
                if self.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): \
                         Dropping deprecated once-off-reliable packet",
                        addr.c_str()
                    );
                }
                return Reason::Success;
            }

            // Send back the ack for this packet.
            let mut back_bundle = UdpBundle::new(0);
            back_bundle.add_ack(packet.seq());
            self.send_bundle(*addr, &mut back_bundle, None);

            // The once-off receiver's dedup window is the once-off
            // sender/receiver pair (BW-02-03-08); the ordered walk
            // delivers the messages below either way.
        }

        // Push this packet (frequently one) through processOrderedPacket.
        let mut oret = Reason::Success;
        let reason = self.process_ordered_packet(addr, packet, p_channel.as_ref());
        if oret == Reason::Success {
            oret = reason;
        }

        // If this bundle was delivered to a channel and there are still
        // ACKs to send, do it now.
        if let Some(channel) = p_channel.as_ref()
            && !channel.borrow().is_destroyed()
            && should_send_channel
            && channel.borrow().is_established()
            && channel.borrow().has_acks()
        {
            self.push_pending_acks(channel);
        }

        oret
    }

    /// `PacketReceiver::processOrderedPacket` (packet_receiver.cpp:1170):
    /// a packet after the reliable-order footers are off it -- strip the
    /// first request offset, join fragments into bundles, and hand the
    /// ordered run's messages to the sink (the interface-table
    /// stand-in).
    fn process_ordered_packet(
        &self,
        addr: &Address,
        packet: &mut Packet,
        p_channel: Option<&ChannelPtr>,
    ) -> Reason {
        macro_rules! corrupted {
            () => {{
                self.receive_stats.borrow_mut().inc_corrupted_packets();
                return Reason::CorruptedPacket;
            }};
        }

        // Strip first request offset.
        if packet.has_flags(flags::HAS_REQUESTS) {
            match packet.strip_footer_u16() {
                Some(first_request_offset) => {
                    if first_request_offset as usize > packet.msg_end_offset() {
                        if self.is_verbose() {
                            eprintln!(
                                "PacketReceiver::processOrderedPacket( {} ): \
                                 First request offset out of bounds (got offset of {} bytes, {} bytes left)",
                                addr.c_str(),
                                first_request_offset,
                                packet.msg_end_offset()
                            );
                        }
                        corrupted!();
                    }
                }
                None => {
                    if self.is_verbose() {
                        eprintln!(
                            "PacketReceiver::processOrderedPacket( {} ): \
                             Not enough data for first request offset ({} bytes left)",
                            addr.c_str(),
                            packet.body_size()
                        );
                    }
                    corrupted!();
                }
            }
        }

        let is_on_channel = p_channel.is_some() && packet.has_flags(flags::IS_RELIABLE);

        // Strip fragment footers.
        if !packet.has_flags(flags::IS_FRAGMENT)
            && packet.has_flags(flags::HAS_SEQUENCE_NUMBER)
            && is_on_channel
            && p_channel
                .map(|channel| channel.borrow().p_fragments().is_some())
                .unwrap_or(false)
        {
            // Make sure that for incoming non-fragment packets with
            // sequence numbers, we are not expecting them to be fragments.
            if self.is_verbose() {
                eprintln!(
                    "PacketReceiver::processOrderedPacket( {} ): \
                     got non-fragment packet, when expecting fragment (#{})",
                    addr.c_str(),
                    packet.seq()
                );
            }
            corrupted!();
        }

        if packet.has_flags(flags::IS_FRAGMENT) {
            if !packet.strip_frag_info() {
                if self.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processOrderedPacket( {} ): \
                         Invalid fragment footers",
                        addr.c_str()
                    );
                }
                corrupted!();
            }

            let now = self.dispatcher.now();

            if is_on_channel {
                let channel = p_channel.expect("is_on_channel implies a channel");
                let mut this = channel.borrow_mut();
                match this.process_fragment(packet.clone(), self.is_external, &addr.c_str(), now) {
                    FragmentResult::Waiting => return Reason::Success,
                    FragmentResult::Corrupt => corrupted!(),
                    FragmentResult::Complete(chain) => {
                        // The bundle is complete: dispatch its ordered
                        // chain, the reference's `pChain` walk.
                        let mut oret = Reason::Success;
                        for mut ordered in chain {
                            let reason = self.dispatch_ordered(addr, &mut ordered);
                            if oret == Reason::Success {
                                oret = reason;
                            }
                        }
                        return oret;
                    }
                }
            }

            // Off-channel fragments live in the once-off receiver's
            // bundle map, keyed by (fragBegin, addr).
            let key = (packet.frag_begin(), *addr);
            let source_str = addr.c_str();

            // If the previous fragment is really old, then this must be
            // some failed bundle that has not been resent and has been
            // given up on, so we get rid of it now.
            {
                let mut bundles = self.fragmented_bundles.borrow_mut();
                let stale = bundles
                    .get(&key)
                    .map(|bundle| bundle.borrow().is_old(now))
                    .unwrap_or(false);
                if stale {
                    if self.is_verbose() {
                        eprintln!(
                            "PacketReceiver::processOrderedPacket( {} ): \
                             Discarding abandoned stale overlapping fragmented bundle",
                            source_str
                        );
                    }
                    bundles.remove(&key);
                }
            }

            let mut bundles = self.fragmented_bundles.borrow_mut();
            let is_new = !bundles.contains_key(&key);
            if is_new {
                // If this is on a channel, it must be the first packet in
                // the bundle, since channel traffic is ordered; off
                // channel, the first fragment seen opens the bundle.
                bundles.insert(
                    key,
                    std::rc::Rc::new(std::cell::RefCell::new(FragmentedBundle::new(
                        packet.clone(),
                        now,
                    ))),
                );
                return Reason::Success;
            }

            let bundle = bundles.get(&key).expect("checked above").clone();
            drop(bundles);
            let complete = {
                let mut fragments = bundle.borrow_mut();
                if !fragments.add_packet(packet.clone(), self.is_external, &source_str, now) {
                    corrupted!();
                }
                fragments.is_complete()
            };

            if !complete {
                return Reason::Success;
            }

            // The bundle is now complete, so dispatch the whole chain
            // below.
            let chain = bundle.borrow_mut().take_chain();
            self.fragmented_bundles.borrow_mut().remove(&key);

            let mut oret = Reason::Success;
            for mut ordered in chain {
                let reason = self.dispatch_ordered(addr, &mut ordered);
                if oret == Reason::Success {
                    oret = reason;
                }
            }
            return oret;
        }

        self.dispatch_ordered(addr, packet)
    }

    /// The terminal delivery of one ordered packet: its messages go to
    /// the sink, the interface-table stand-in (`dispatchMessages`'s
    /// handler walk is BW-02-02's seam).
    fn dispatch_ordered(&self, addr: &Address, packet: &mut Packet) -> Reason {
        match self.sink.borrow().as_ref() {
            Some(sink) => sink.process_packet(addr, packet),
            None => Reason::Success,
        }
    }

    /// `NetworkInterface::send( addr, bundle, pChannel )`: prepare the
    /// bundle's packets and put each on the wire. `channel` is the
    /// channel the bundle rides on, or `None` for off-channel (once-off)
    /// traffic. The send window's overflow is honoured: only the packets
    /// up to the first overflow go out.
    pub fn send_bundle(&self, addr: Address, bundle: &mut UdpBundle, channel: Option<&ChannelPtr>) {
        let first_overflow = bundle.prepare_packets(
            channel,
            &mut self.seq_num_allocator.borrow_mut(),
            self.should_use_checksums.get(),
        );

        let num_packets = bundle.num_data_units();
        let last = first_overflow.unwrap_or(num_packets);
        for index in 0..last {
            self.send_packet(addr, bundle.packet_wire(index));
        }
    }

    /// `NetworkInterface::sendPacket( addr, pPacket, pChannel )`
    /// (network_interface.cpp): one packet on the wire. The channel is
    /// informational here; the resends that need its version refresh do
    /// it before calling in.
    pub fn send_packet(&self, addr: Address, data: &[u8]) {
        self.udp_socket.borrow().sendto_address(data, addr);
    }

    /// `pChannel->send( &emptyBundle )` (packet_receiver.cpp:1046): push
    /// a channel's pending acks out immediately on an external empty
    /// bundle -- the `isSendingOwnBundle == false` arm of `doSend`, so
    /// the acks ride out even though the channel's own bundle is empty.
    fn push_pending_acks(&self, channel: &ChannelPtr) {
        let mut empty = UdpBundle::new(0);
        UdpChannel::send(channel, Some(&mut empty));
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

impl Drop for NetworkInterface {
    /// The period timer is the interface's own, and nothing needs it once
    /// the interface goes. The reference only cancels it in
    /// `rateLimitPeriod( float )`; cancelling here as well keeps the
    /// recurring timer from firing into a dead interface.
    fn drop(&mut self) {
        self.rate_limit_timer.borrow_mut().cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::initial_connection_filter::InitialConnectionFilter;
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

    /// An interface with no sink accepts dispatched packets silently;
    /// with one, it hands the ordered packet's messages over (the
    /// terminal delivery of `processPacket`). The datagram is a valid
    /// header-only packet so it clears the guards and reaches the sink.
    #[test]
    fn the_sink_receives_what_the_interface_processes() {
        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);

        let mut packet = crate::packet::Packet::new();
        packet.grow(2); // flags word
        packet.grow(1); // one body byte
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

    // ------------------------------------------------------------------
    // The wired bidirectional exchange (BW-02-03-07)
    // ------------------------------------------------------------------

    /// A loopback interface on a pinned clock: real socket, cell clock,
    /// so the resend timers are driven deterministically.
    fn pinned_loopback(clock: Rc<std::cell::Cell<u64>>) -> Rc<NetworkInterface> {
        let cell = Rc::clone(&clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || cell.get())));
        let interface = NetworkInterface::new_with_dispatcher(InterfaceType::Internal, dispatcher);
        let receiver = PacketReceiver::new();
        interface.recreate_listening_socket(&receiver, 0, "127.0.0.1");
        receiver.attach_interface(&interface);
        interface
    }

    /// A reliable message element, variable length like an app's.
    fn payload_element() -> crate::interface_element::InterfaceElement {
        crate::interface_element::InterfaceElement::new(
            "payload",
            7,
            crate::interface_element::VARIABLE_LENGTH_MESSAGE,
            8,
            None,
        )
    }

    /// The acceptance: both directions exchange over real loopback
    /// sockets, acks ride the opposite direction's traffic, and a timed
    /// resend is counted exactly once on the losing channel while the
    /// receive window delivers the packet exactly once.
    #[test]
    fn both_directions_exchange_and_retransmissions_are_counted() {
        let clock_a = Rc::new(std::cell::Cell::new(1_000_000u64));
        let clock_b = Rc::new(std::cell::Cell::new(1_000_000u64));
        let interface_a = pinned_loopback(clock_a.clone());
        let interface_b = pinned_loopback(clock_b.clone());
        interface_a.set_should_use_checksums(true);
        interface_b.set_should_use_checksums(true);

        let sink_a = Rc::new(RecordingSink::default());
        let sink_b = Rc::new(RecordingSink::default());
        interface_a.set_packet_sink(Some(sink_a.clone()));
        interface_b.set_packet_sink(Some(sink_b.clone()));

        let addr_a = interface_a.address();
        let addr_b = interface_b.address();

        // One channel each way, both registered with their interfaces.
        // A's channel is irregular (the remote is on a lossy link), so
        // its resends come from the timers rather than newer acks.
        let channel_a = UdpChannel::new(
            &interface_a,
            addr_b,
            ChannelTraits::Internal,
            0.001,
            CHANNEL_ID_NULL,
        );
        let channel_b = UdpChannel::new(
            &interface_b,
            addr_a,
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::set_is_remote_regular(&channel_a, false);

        let payload = |bytes: &'static [u8]| -> UdpBundle {
            let mut bundle = UdpBundle::new(0);
            bundle.start_message(&payload_element(), crate::udp_bundle::ReliableType::Driver);
            bundle.write_bytes(bytes);
            bundle.finalise();
            bundle
        };

        // A -> B: a reliable message goes out, and A's channel files it
        // in the send window.
        let mut ping = payload(b"ping");
        UdpChannel::send(&channel_a, Some(&mut ping));
        assert_eq!(channel_a.borrow().oldest_unacked_seq(), 0);
        assert_eq!(channel_a.borrow().num_packets_resent(), 0);

        // B drains its socket: delivered once, to B and not A.
        assert!(
            interface_b.poller().process_pending_events(0.05) >= 1,
            "B's socket saw the ping"
        );
        {
            let packets = sink_b.packets.borrow();
            assert_eq!(packets.len(), 1);
            assert!(
                String::from_utf8_lossy(&packets[0].1).contains("ping"),
                "B got the ping, got {:?}",
                packets[0].1
            );
        }
        assert!(sink_a.packets.borrow().is_empty());

        // B -> A: the reply's outbound bundle carries B's ack for the
        // ping, and A's channel acks its unacked slot.
        let mut pong = payload(b"pong");
        UdpChannel::send(&channel_b, Some(&mut pong));
        assert!(
            interface_a.poller().process_pending_events(0.05) >= 1,
            "A's socket saw the pong"
        );
        {
            let packets = sink_a.packets.borrow();
            assert_eq!(packets.len(), 1);
            assert!(
                String::from_utf8_lossy(&packets[0].1).contains("pong"),
                "A got the pong, got {:?}",
                packets[0].1
            );
        }
        assert!(
            !channel_a.borrow().has_unacked_packets(),
            "B's ack cleared A's send window"
        );

        // A -> B again, but B never drains its socket: from A's side the
        // packet is lost in transit.
        let mut gone = payload(b"gone");
        UdpChannel::send(&channel_a, Some(&mut gone));
        assert_eq!(channel_a.borrow().oldest_unacked_seq(), 1);

        // Past the resend period (twice the 100ms internal RTT) the
        // channel's check pushes the packet out again and counts it.
        clock_a.set(1_000_000 + 200_001);
        UdpChannel::check_resend_timers(&channel_a);
        assert_eq!(
            channel_a.borrow().num_packets_resent(),
            1,
            "the one timed resend is counted"
        );

        // B drains both copies: the first is delivered, the resent copy
        // is a duplicate the receive window refuses.
        assert!(interface_b.poller().process_pending_events(0.05) >= 1);
        {
            let packets = sink_b.packets.borrow();
            let bodies: Vec<String> = packets
                .iter()
                .map(|(_, data)| String::from_utf8_lossy(data).to_string())
                .collect();
            assert_eq!(bodies.len(), 2, "ping then gone: {:?}", bodies);
            assert!(
                bodies[1].contains("gone"),
                "the resent packet is delivered, got {:?}",
                bodies
            );
            assert_eq!(
                bodies.iter().filter(|b| b.contains("gone")).count(),
                1,
                "the duplicate copy is not delivered again"
            );
        }
        assert!(sink_a.packets.borrow().len() == 1, "A saw only the pong");
    }

    // ------------------------------------------------------------------
    // The receive-path guards (BW-02-03-07): every header guard and
    // footer-strip failure the reference reports as a corrupted packet,
    // the two silent drops, the once-off arms, and the ordered fragment
    // branches, all driven through the wired path.
    // ------------------------------------------------------------------

    /// A datagram as the receiver hands it over: the flags word, then the
    /// body with whatever footers the sender packed at its back.
    fn wire(flags: u16, body: &[u8]) -> Packet {
        let mut packet = Packet::new();
        packet.set_flags(flags);
        packet.grow(2 + body.len());
        packet.buffer_mut()[2..2 + body.len()].copy_from_slice(body);
        packet
    }

    /// Little-endian u32 footer bytes.
    fn le32(value: u32) -> [u8; 4] {
        value.to_le_bytes()
    }

    /// A fragment datagram: a message byte, the fragment footers
    /// (fragBegin then fragEnd), then the sequence footer. The strip
    /// order is the reverse of the pack, so fragEnd sits at the back.
    fn fragment_wire(seq: u32, frag_begin: u32, frag_end: u32, msg: u8) -> Packet {
        let mut body = vec![msg];
        body.extend_from_slice(&le32(frag_begin));
        body.extend_from_slice(&le32(frag_end));
        body.extend_from_slice(&le32(seq));
        wire(
            flags::ON_CHANNEL
                | flags::IS_RELIABLE
                | flags::IS_FRAGMENT
                | flags::HAS_SEQUENCE_NUMBER,
            &body,
        )
    }

    /// A whole reliable datagram on a channel: a message byte then the
    /// sequence footer.
    fn ordered_wire(seq: u32, msg: u8) -> Packet {
        let mut body = vec![msg];
        body.extend_from_slice(&le32(seq));
        wire(
            flags::ON_CHANNEL | flags::IS_RELIABLE | flags::HAS_SEQUENCE_NUMBER,
            &body,
        )
    }

    /// The guard channel of a socketless interface, registered at `port`.
    fn guard_channel(
        port: u16,
    ) -> (
        Rc<NetworkInterface>,
        Rc<NetworkInterface>,
        ChannelPtr,
        Address,
        Rc<Cell<u64>>,
    ) {
        let (interface, clock) = channel_interface(InterfaceType::Internal);
        interface.set_verbose(true);
        let addr = Address::new(LOOPBACK, socket::htons(port));
        let channel = UdpChannel::new(
            &interface,
            addr,
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        (interface.clone(), interface, channel, addr, clock)
    }

    /// The size and known-flags guards, the bad checksum and the bad
    /// piggyback footer all report a corrupted packet.
    #[test]
    fn the_receive_guards_report_corruption() {
        let (interface, _clock) = channel_interface(InterfaceType::Internal);
        interface.set_verbose(true);
        let addr = Address::new(LOOPBACK, socket::htons(9101));

        // Under two bytes: not even a flags word.
        let mut undersized = Packet::new();
        undersized.grow(1);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut undersized),
            Reason::CorruptedPacket
        );

        // A flag this build does not understand.
        let mut bad_flags = wire(0x0800, &[0]);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut bad_flags),
            Reason::CorruptedPacket
        );

        // A bare flags word is too small to hold a message.
        let mut empty = wire(0, &[]);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut empty),
            Reason::CorruptedPacket
        );

        // A checksum that no longer matches its bytes.
        let mut bad_checksum = wire(flags::HAS_CHECKSUM, &[0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut bad_checksum),
            Reason::CorruptedPacket
        );

        // A piggyback footer too short to peel.
        let mut bad_piggyback = wire(flags::HAS_PIGGYBACKS, &[0]);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut bad_piggyback),
            Reason::CorruptedPacket
        );

        assert_eq!(
            interface.receive_stats().borrow().num_corrupted_packets(),
            5,
            "every guard counted one corruption"
        );
    }

    /// The indexed-channel guards: a short id footer, a short version
    /// footer, and -- with both present -- the no-finder corruption the
    /// reference reports while indexed lookup is unported.
    #[test]
    fn the_indexed_channel_guards_report_corruption() {
        let (interface, _clock) = channel_interface(InterfaceType::Internal);
        interface.set_verbose(true);
        let addr = Address::new(LOOPBACK, socket::htons(9102));

        let mut short_id = wire(flags::INDEXED_CHANNEL, &[0, 0, 0]);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut short_id),
            Reason::CorruptedPacket
        );

        let mut short_version = wire(flags::INDEXED_CHANNEL, &le32(7));
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut short_version),
            Reason::CorruptedPacket
        );

        let mut indexed = wire(flags::INDEXED_CHANNEL, &[le32(7), le32(9)].concat());
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut indexed),
            Reason::CorruptedPacket
        );
    }

    /// The silent drops: an on-channel packet for an address no channel
    /// is registered at, and one whose channel has a remote failure.
    #[test]
    fn the_channel_guards_drop_silently() {
        let (interface, _clock) = channel_interface(InterfaceType::Internal);
        interface.set_verbose(true);
        let addr = Address::new(LOOPBACK, socket::htons(9103));

        let mut no_channel = wire(flags::ON_CHANNEL, &[0, 0, 0]);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut no_channel),
            Reason::GeneralNetwork
        );

        let failed = UdpChannel::new(
            &interface,
            addr,
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::set_remote_failed(&failed);
        let mut remote = wire(flags::ON_CHANNEL, &[0, 0, 0]);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut remote),
            Reason::GeneralNetwork
        );
    }

    /// The acknowledgement guards: a cumulative ack with no channel, a
    /// short cumulative footer, a cumulative ack from the future, a short
    /// ack count footer, a zero count, a count whose footers are short,
    /// and an acknowledgement outside the ring.
    #[test]
    fn the_ack_guards_report_corruption() {
        let (interface, _interface2, channel, addr, _clock) = guard_channel(9104);

        let mut off_channel = wire(flags::HAS_CUMULATIVE_ACK, &[0, 0, 0, 0, 0]);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut off_channel),
            Reason::CorruptedPacket
        );

        let mut short_cumulative = wire(flags::ON_CHANNEL | flags::HAS_CUMULATIVE_ACK, &[0, 0]);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut short_cumulative),
            Reason::CorruptedPacket
        );

        // File a packet in the window, then ack from the future.
        let mut filed = Packet::new();
        filed.grow(2);
        filed.grow(1);
        filed.set_seq(0);
        UdpChannel::add_resend_timer(&channel, 0, Rc::new(filed), &[]);
        let mut future_ack = wire(flags::ON_CHANNEL | flags::HAS_CUMULATIVE_ACK, &le32(5));
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut future_ack),
            Reason::CorruptedPacket
        );

        let mut short_count = wire(flags::ON_CHANNEL | flags::HAS_ACKS, &[]);
        short_count.reserve_footer(1);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut short_count),
            Reason::CorruptedPacket
        );

        let mut no_acks = wire(flags::ON_CHANNEL | flags::HAS_ACKS, &[0]);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut no_acks),
            Reason::CorruptedPacket
        );

        let mut short_footers = wire(flags::ON_CHANNEL | flags::HAS_ACKS, &[2, 0, 0, 0]);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut short_footers),
            Reason::CorruptedPacket
        );

        // An acknowledgement past the 28-bit ring.
        let mut out_of_ring = wire(flags::ON_CHANNEL | flags::HAS_ACKS, &[1, 0, 0, 0, 0x10]);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut out_of_ring),
            Reason::CorruptedPacket
        );

        assert_eq!(
            interface.receive_stats().borrow().num_corrupted_packets(),
            7,
            "every ack guard counted one corruption"
        );
    }

    /// A reliable request without a sequence number is corrupt; a once-off
    /// reliable packet gets its acknowledgement back on an internal
    /// interface and is quietly dropped on an external one.
    #[test]
    fn the_reliability_guards_run_their_once_off_arms() {
        let (interface, _clock) = channel_interface(InterfaceType::Internal);
        interface.set_verbose(true);
        let addr = Address::new(LOOPBACK, socket::htons(9105));

        let mut no_sequence = wire(flags::IS_RELIABLE, &[0]);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut no_sequence),
            Reason::CorruptedPacket
        );

        let mut once_off = wire(flags::IS_RELIABLE | flags::HAS_SEQUENCE_NUMBER, &le32(3));
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut once_off),
            Reason::Success
        );

        let (external, _) = channel_interface(InterfaceType::External);
        external.set_verbose(true);
        let mut refused = wire(flags::IS_RELIABLE | flags::HAS_SEQUENCE_NUMBER, &le32(3));
        assert_eq!(
            external.process_filtered_packet(&addr, &mut refused),
            Reason::Success
        );
    }

    /// The request footer guards: an offset pointing past the message,
    /// and a footer too short to hold the offset.
    #[test]
    fn the_request_footer_guards_report_corruption() {
        let (interface, _clock) = channel_interface(InterfaceType::Internal);
        interface.set_verbose(true);
        let addr = Address::new(LOOPBACK, socket::htons(9106));

        let mut out_of_bounds = wire(flags::HAS_REQUESTS, &[0, 0, 0, 0xFF, 0xFF]);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut out_of_bounds),
            Reason::CorruptedPacket
        );

        let mut short_offset = wire(flags::HAS_REQUESTS, &[0]);
        assert_eq!(
            interface.process_filtered_packet(&addr, &mut short_offset),
            Reason::CorruptedPacket
        );
    }

    /// A fresh on-channel interface registered at `port`, with the sink
    /// attached.
    fn on_channel_interface(port: u16) -> (Rc<NetworkInterface>, ChannelPtr, Address) {
        let (interface, _clock) = channel_interface(InterfaceType::Internal);
        interface.set_verbose(true);
        let addr = Address::new(LOOPBACK, socket::htons(port));
        let channel = UdpChannel::new(
            &interface,
            addr,
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        (interface, channel, addr)
    }

    /// A two-fragment bundle on a channel reassembles and delivers in
    /// order.
    #[test]
    fn the_on_channel_fragments_reassemble_in_order() {
        let (interface, _channel, addr) = on_channel_interface(9107);
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        let mut first = fragment_wire(0, 0, 1, 0xAA);
        assert_eq!(interface.process_packet(&addr, &mut first), Reason::Success);
        assert!(
            sink.packets.borrow().is_empty(),
            "the bundle is not whole yet"
        );

        let mut second = fragment_wire(1, 0, 1, 0xBB);
        assert_eq!(
            interface.process_packet(&addr, &mut second),
            Reason::Success
        );
        let packets = sink.packets.borrow();
        assert_eq!(packets.len(), 2, "the completed chain delivers both");
        assert_eq!(packets[0].1[2], 0xAA, "the first fragment's message");
        assert_eq!(packets[1].1[2], 0xBB, "the second fragment's message");
    }

    /// A whole packet arriving while a fragment is still expected is the
    /// reference's `non-fragment packet, when expecting fragment` bit,
    /// and it consumes the sequence like the reference's window does.
    #[test]
    fn a_whole_packet_while_a_fragment_is_expected_is_corrupt() {
        let (interface, _channel, addr) = on_channel_interface(9117);
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        let mut first = fragment_wire(0, 0, 1, 0xAA);
        assert_eq!(interface.process_packet(&addr, &mut first), Reason::Success);

        let mut whole = ordered_wire(1, 0xCC);
        assert_eq!(
            interface.process_packet(&addr, &mut whole),
            Reason::CorruptedPacket
        );
        assert!(sink.packets.borrow().is_empty());
    }

    /// The on-channel fragment guards, each on its own channel since a
    /// corruption consumes the sequence: bad footers, a bundle that
    /// starts mid-way, and a fragment of a different bundle.
    #[test]
    fn the_on_channel_fragment_guards_report_corruption() {
        // Bad footers: the caller's strip fails on a fragment too short
        // to hold them.
        let (interface, channel, addr) = on_channel_interface(9108);
        let mut bad_footers = wire(
            flags::ON_CHANNEL
                | flags::IS_RELIABLE
                | flags::IS_FRAGMENT
                | flags::HAS_SEQUENCE_NUMBER,
            &[0, 0, 0, 0, 0],
        );
        assert_eq!(
            interface.process_packet(&addr, &mut bad_footers),
            Reason::CorruptedPacket
        );
        assert!(channel.borrow().p_fragments().is_none());

        // A bundle that starts mid-way: the channel has already consumed
        // seq 0 as a whole packet, so the first fragment it sees (seq 1
        // of a bundle beginning at 0) is not its head.
        let (interface, channel, addr) = on_channel_interface(9118);
        let mut whole = ordered_wire(0, 0x99);
        assert_eq!(interface.process_packet(&addr, &mut whole), Reason::Success);
        let mut mid_stream = fragment_wire(1, 0, 2, 0xBB);
        assert_eq!(
            interface.process_packet(&addr, &mut mid_stream),
            Reason::CorruptedPacket
        );
        assert!(channel.borrow().p_fragments().is_none());

        // A fragment of a different bundle cannot join the open one.
        let (interface, channel, addr) = on_channel_interface(9128);
        let mut head = fragment_wire(0, 0, 2, 0xAA);
        assert_eq!(interface.process_packet(&addr, &mut head), Reason::Success);
        let mut mismatched = fragment_wire(1, 1, 3, 0xCC);
        assert_eq!(
            interface.process_packet(&addr, &mut mismatched),
            Reason::CorruptedPacket
        );
        assert!(
            channel
                .borrow()
                .p_fragments()
                .is_some_and(|f| f.chain_length() == 1),
            "the bundle keeps the chain it had"
        );
    }

    /// Off-channel fragments reassemble in the once-off receiver's map:
    /// the head opens the bundle, later fragments fill it, the completed
    /// chain delivers in order, a mangled fragment is corrupt, and a
    /// bundle the source gave up on is swept when it is revisited.
    #[test]
    fn the_off_channel_fragments_reassemble_and_stale_bundles_reclaim() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let cell = Rc::clone(&clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || cell.get())));
        let interface = NetworkInterface::new_with_dispatcher(InterfaceType::Internal, dispatcher);
        interface.set_verbose(true);
        let addr = Address::new(LOOPBACK, socket::htons(9109));
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        let off_fragment = |seq: u32, frag_begin: u32, frag_end: u32, msg: u8| {
            let mut body = vec![msg];
            body.extend_from_slice(&le32(frag_begin));
            body.extend_from_slice(&le32(frag_end));
            body.extend_from_slice(&le32(seq));
            wire(
                flags::IS_RELIABLE | flags::IS_FRAGMENT | flags::HAS_SEQUENCE_NUMBER,
                &body,
            )
        };

        // The head opens the bundle; nothing delivers yet.
        let mut head = off_fragment(0, 0, 1, 0xAA);
        assert_eq!(interface.process_packet(&addr, &mut head), Reason::Success);
        assert!(sink.packets.borrow().is_empty());
        assert_eq!(interface.fragmented_bundles().borrow().len(), 1);

        // A fragment of a different bundle cannot join: mangled. Same
        // head key, a fragEnd that disagrees with the open bundle.
        let mut mangled = off_fragment(1, 0, 2, 0xCC);
        assert_eq!(
            interface.process_packet(&addr, &mut mangled),
            Reason::CorruptedPacket
        );

        // The last fragment completes the bundle and the chain delivers.
        let mut tail = off_fragment(1, 0, 1, 0xBB);
        assert_eq!(interface.process_packet(&addr, &mut tail), Reason::Success);
        {
            let packets = sink.packets.borrow();
            assert_eq!(packets.len(), 2, "the completed chain delivers both");
            assert_eq!(packets[0].1[2], 0xAA);
            assert_eq!(packets[1].1[2], 0xBB);
        }
        assert!(
            interface.fragmented_bundles().borrow().is_empty(),
            "the finished bundle left the map"
        );

        // A bundle left too long is swept when its source shows up again.
        let mut abandoned = off_fragment(0, 0, 1, 0xDD);
        assert_eq!(
            interface.process_packet(&addr, &mut abandoned),
            Reason::Success
        );
        clock.set(1_000_000 + 11_000_000);
        let mut revisit = off_fragment(1, 0, 1, 0xEE);
        assert_eq!(
            interface.process_packet(&addr, &mut revisit),
            Reason::Success
        );
        assert_eq!(
            interface.fragmented_bundles().borrow().len(),
            1,
            "the stale bundle was replaced, not joined"
        );
        assert_eq!(
            interface
                .receive_stats()
                .borrow()
                .num_packets_received_off_channel(),
            5,
            "every off-channel datagram is counted, the rejected one included"
        );
    }

    /// A locally irregular channel pushes the acknowledgements it owes
    /// straight after the ordered delivery.
    #[test]
    fn a_locally_irregular_channel_pushes_its_acks() {
        let (interface, _clock) = channel_interface(InterfaceType::Internal);
        interface.set_verbose(true);
        let addr = Address::new(LOOPBACK, socket::htons(9110));
        let channel = UdpChannel::new(
            &interface,
            addr,
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::set_is_local_regular(&channel, false);

        let mut packet = ordered_wire(0, 0x41);
        assert_eq!(
            interface.process_packet(&addr, &mut packet),
            Reason::Success
        );
        assert!(
            !channel.borrow().has_acks(),
            "the owed acknowledgement went straight out"
        );
    }

    // ------------------------------------------------------------------
    // The connection filter (BW-02-04-03): the reference-enabled
    // InitialConnectionFilter gates channel-less datagrams; the allow
    // and the deny arms each match, and a found channel skips it.
    // ------------------------------------------------------------------

    /// The `baseAppLogin` message id the filter is built around.
    const BASE_APP_LOGIN_ID: u8 = 0x0D;

    /// A `baseAppLogin` request as it arrives off-channel: the flags
    /// word, the message frame (a fixed-length message carries no length
    /// prefix), then the request footer -- the reply id, the last
    /// request's link slot, and the packet's firstRequestOffset, which is
    /// the very back. 16 bytes in all: exactly the wire size the
    /// reference's filter accepts.
    fn login_wire(login_id: u8) -> Packet {
        let mut body = vec![login_id];
        body.extend_from_slice(&0u32.to_le_bytes()); // SessionKey key
        body.push(1); // numAttempts
        body.extend_from_slice(&0u32.to_le_bytes()); // ReplyID
        body.extend_from_slice(&0u16.to_le_bytes()); // last request link
        body.extend_from_slice(&(crate::packet::HEADER_SIZE as u16).to_le_bytes()); // firstRequestOffset
        wire(flags::HAS_REQUESTS, &body)
    }

    /// A socketless external interface with the sink attached and the
    /// connection filter enabled, as a BaseApp's external interface is.
    fn filtered_interface(port: u16) -> (Rc<NetworkInterface>, Rc<RecordingSink>, Address) {
        let (interface, _clock) = channel_interface(InterfaceType::External);
        interface.set_verbose(true);
        interface.set_p_off_channel_filter(Some(Rc::new(InitialConnectionFilter::new(
            BASE_APP_LOGIN_ID,
        ))));
        let addr = Address::new(LOOPBACK, socket::htons(port));
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));
        (interface, sink, addr)
    }

    /// The one shape the filter admits: a `baseAppLogin` request reaches
    /// the receive path and is dispatched, exactly what the base
    /// `PacketFilter::recv` pass-through does for an allowed packet.
    #[test]
    fn a_base_app_login_request_passes_the_connection_filter() {
        let (interface, sink, addr) = filtered_interface(9111);

        let mut login = login_wire(BASE_APP_LOGIN_ID);
        assert_eq!(interface.process_packet(&addr, &mut login), Reason::Success);
        let packets = sink.packets.borrow();
        assert_eq!(packets.len(), 1, "the allowed request was dispatched");
        // The sink sees the packet after the firstRequestOffset strip:
        // flags, the message frame, and the request footer.
        assert_eq!(packets[0].1.len(), 14);
        assert_eq!(&packets[0].1[2..3], &[BASE_APP_LOGIN_ID]);
    }

    /// Every other shape is dropped with `REASON_GENERAL_NETWORK` before
    /// the receive path runs: wrong flags, wrong size (either side), and
    /// the wrong message id each deny.
    #[test]
    fn the_connection_filter_drops_everything_but_the_login() {
        let (interface, sink, addr) = filtered_interface(9112);

        // Wrong flags: the login shape plus any other bit is noise.
        let mut flagged = login_wire(BASE_APP_LOGIN_ID);
        flagged.set_flags(flags::HAS_REQUESTS | flags::HAS_ACKS);
        assert_eq!(
            interface.process_packet(&addr, &mut flagged),
            Reason::GeneralNetwork
        );

        // Wrong size: one byte short and one byte long of the 16.
        let mut short = login_wire(BASE_APP_LOGIN_ID);
        short.shrink(1);
        assert_eq!(
            interface.process_packet(&addr, &mut short),
            Reason::GeneralNetwork
        );
        let mut long = login_wire(BASE_APP_LOGIN_ID);
        long.grow(1);
        assert_eq!(
            interface.process_packet(&addr, &mut long),
            Reason::GeneralNetwork
        );

        // Wrong message id: a well-shaped request naming another message.
        let mut other = login_wire(BASE_APP_LOGIN_ID + 1);
        assert_eq!(
            interface.process_packet(&addr, &mut other),
            Reason::GeneralNetwork
        );

        assert!(
            sink.packets.borrow().is_empty(),
            "nothing the filter denied reached the dispatch"
        );
    }

    /// The filter arm is the reference's else-if: a datagram that finds
    /// its channel is not the filter's business, login-shaped or not.
    #[test]
    fn a_channelled_datagram_bypasses_the_connection_filter() {
        let (interface, _clock) = channel_interface(InterfaceType::Internal);
        interface.set_verbose(true);
        let addr = Address::new(LOOPBACK, socket::htons(9113));
        UdpChannel::new(
            &interface,
            addr,
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        interface.set_p_off_channel_filter(Some(Rc::new(InitialConnectionFilter::new(
            BASE_APP_LOGIN_ID,
        ))));
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        // An on-channel datagram of no login shape whatsoever: the filter
        // would deny it, but it never sees it.
        let mut ordered = ordered_wire(0, 0x41);
        assert_eq!(
            interface.process_packet(&addr, &mut ordered),
            Reason::Success
        );
        assert_eq!(sink.packets.borrow().len(), 1, "it went straight through");
    }

    /// A packet filter that denies every datagram with its own reason, so
    /// whichever filter spoke is recognisable by the reason that came back.
    struct DenyingFilter(Reason);

    impl crate::packet_filter::PacketFilter for DenyingFilter {
        fn recv(
            &self,
            _interface: &NetworkInterface,
            _addr: &Address,
            _packet: &mut Packet,
        ) -> Reason {
            self.0
        }
    }

    /// A channel's own packet filter first-passes the datagram found for
    /// it (packet_receiver.cpp:471-474): the filter's verdict is the
    /// receive path's result, and the datagram never reaches the
    /// validation guards or the dispatch.
    #[test]
    fn a_channel_filter_decides_a_found_channel() {
        let (interface, _clock) = channel_interface(InterfaceType::Internal);
        interface.set_verbose(true);
        let addr = Address::new(LOOPBACK, socket::htons(9115));
        let channel = UdpChannel::new(
            &interface,
            addr,
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::set_p_filter(&channel, Some(Rc::new(DenyingFilter(Reason::NoSuchPort))));
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        let mut ordered = ordered_wire(0, 0x41);
        assert_eq!(
            interface.process_packet(&addr, &mut ordered),
            Reason::NoSuchPort,
            "the channel's filter spoke"
        );
        assert!(
            sink.packets.borrow().is_empty(),
            "nothing a channel filter denied reached the dispatch"
        );
    }

    /// The channel's filter is skipped once its remote has failed
    /// (`pChannel->pFilter() && !pChannel->hasRemoteFailed()`), and the
    /// datagram runs the validation and the dispatch as usual.
    #[test]
    fn a_failed_channel_skips_its_filter() {
        let (interface, _clock) = channel_interface(InterfaceType::Internal);
        interface.set_verbose(true);
        let addr = Address::new(LOOPBACK, socket::htons(9116));
        let channel = UdpChannel::new(
            &interface,
            addr,
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::set_p_filter(&channel, Some(Rc::new(DenyingFilter(Reason::NoSuchPort))));
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        let handle = interface
            .find_channel_ref(&addr)
            .expect("the channel is registered");
        handle.set_remote_failed();

        let mut ordered = ordered_wire(0, 0x41);
        assert_eq!(
            interface.process_packet(&addr, &mut ordered),
            Reason::GeneralNetwork,
            "the filter was skipped, and the failed-remote guard in the \
             receive path dropped the packet instead (packet_receiver.cpp:700)"
        );
        assert!(
            sink.packets.borrow().is_empty(),
            "a failed channel's packet is dropped in the receive path"
        );
    }

    /// The channel filter sits before the interface's connection filter:
    /// a datagram that finds its channel never meets `pOffChannelFilter_`,
    /// the reference's else-if.
    #[test]
    fn a_channel_filter_sits_before_the_off_channel_filter() {
        let (interface, _clock) = channel_interface(InterfaceType::Internal);
        interface.set_verbose(true);
        let addr = Address::new(LOOPBACK, socket::htons(9117));
        let channel = UdpChannel::new(
            &interface,
            addr,
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::set_p_filter(&channel, Some(Rc::new(DenyingFilter(Reason::NoSuchPort))));
        interface.set_p_off_channel_filter(Some(Rc::new(InitialConnectionFilter::new(
            BASE_APP_LOGIN_ID,
        ))));
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        // A datagram the connection filter would deny: the channel's
        // filter speaks first, so its reason is what comes back.
        let mut ordered = ordered_wire(0, 0x41);
        assert_eq!(
            interface.process_packet(&addr, &mut ordered),
            Reason::NoSuchPort,
            "the channel's filter spoke before the connection filter could"
        );
        assert!(sink.packets.borrow().is_empty());
    }

    /// Deregistering a channel on the external interface remembers its
    /// address as recently dead (network_interface.cpp:493-501): the
    /// session the address held is gone, but the address is not yet
    /// cleared.
    #[test]
    fn a_deregistered_external_channel_becomes_recently_dead() {
        let (interface, _clock) = channel_interface(InterfaceType::External);
        let addr = Address::new(LOOPBACK, socket::htons(9121));
        let channel = UdpChannel::new(
            &interface,
            addr,
            ChannelTraits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        assert!(!interface.is_dead(&addr), "a live channel is not dead");
        interface.deregister_channel(&channel);
        assert!(
            interface.is_dead(&addr),
            "a deregistered external channel is remembered"
        );
    }

    /// `pRecentlyDeadChannels_` only exists on external interfaces
    /// (network_interface.cpp:160-170): an internal deregistration
    /// forgets nothing.
    #[test]
    fn an_internal_interface_keeps_no_recently_dead() {
        let (interface, _clock) = channel_interface(InterfaceType::Internal);
        let addr = Address::new(LOOPBACK, socket::htons(9122));
        let channel = UdpChannel::new(
            &interface,
            addr,
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        interface.deregister_channel(&channel);
        assert!(!interface.is_dead(&addr));
    }

    /// BW-02-05-03 acceptance: an auth failure tears the session down,
    /// and the address stays remembered, so a follow-up datagram finds no
    /// channel and is dropped -- the guard sits before the connection
    /// filter, so nothing is re-processed raw and no session lingers.
    #[test]
    fn a_failed_handshake_leaves_no_residual_session() {
        let (interface, _clock) = channel_interface(InterfaceType::External);
        interface.set_verbose(true);
        interface.set_p_off_channel_filter(Some(Rc::new(InitialConnectionFilter::new(
            BASE_APP_LOGIN_ID,
        ))));
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        // The client's session channel, and the app tearing it down when
        // the login fails.
        let addr = Address::new(LOOPBACK, socket::htons(9123));
        let channel = UdpChannel::new(
            &interface,
            addr,
            ChannelTraits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::destroy(&channel);

        // A retry from the same address: no channel to find, so the
        // recently-dead guard answers.
        let mut retry = wire(flags::ON_CHANNEL, &[0x41]);
        assert_eq!(
            interface.process_packet(&addr, &mut retry),
            Reason::Success,
            "the dead guard drops the datagram"
        );
        assert!(
            sink.packets.borrow().is_empty(),
            "nothing a dead address sent reached the dispatch"
        );
    }

    /// The guard is a short memory, not a ban: once the 60-second timer
    /// has run, the address is forgotten and its datagrams go back to the
    /// connection filter (packet_receiver.cpp:474-487).
    #[test]
    fn the_recently_dead_guard_expires_after_a_minute() {
        let (interface, clock) = channel_interface(InterfaceType::External);
        let addr = Address::new(LOOPBACK, socket::htons(9124));
        let channel = UdpChannel::new(
            &interface,
            addr,
            ChannelTraits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        interface.deregister_channel(&channel);
        assert!(interface.is_dead(&addr));

        clock.set(1_000_000 + 60_000_000);
        interface.dispatcher().process_timers();
        assert!(!interface.is_dead(&addr), "the address was forgotten");

        // A datagram from it now meets the connection filter again.
        interface.set_p_off_channel_filter(Some(Rc::new(InitialConnectionFilter::new(
            BASE_APP_LOGIN_ID,
        ))));
        let mut noise = wire(flags::ON_CHANNEL, &[0x41]);
        assert_eq!(
            interface.process_packet(&addr, &mut noise),
            Reason::GeneralNetwork,
            "the filter speaks once the guard has expired"
        );
    }

    /// Rate limiting starts off: zero period, no per-address limits, and
    /// no datagram refused for rate (network_interface.cpp:99-105).
    #[test]
    fn rate_limiting_starts_disabled() {
        let (interface, sink, addr) = filtered_interface(9131);

        assert_eq!(interface.rate_limit_period(), 0.0);
        assert_eq!(interface.per_ip_address_rate_limit(), 0);
        assert_eq!(interface.per_ip_address_port_rate_limit(), 0);

        for _ in 0..8 {
            let mut login = login_wire(BASE_APP_LOGIN_ID);
            assert_eq!(
                interface.process_packet(&addr, &mut login),
                Reason::Success,
                "nothing is rate-limited while the period is zero"
            );
        }
        assert_eq!(sink.packets.borrow().len(), 8);
        assert_eq!(interface.rate_limit_averages(), (0.0, 0.0));
    }

    /// The per-address limit counts every port of the address: three
    /// datagrams pass, the fourth meets `REASON_GENERAL_NETWORK` before
    /// the connection filter ever sees it
    /// (network_interface.cpp:980-989).
    #[test]
    fn the_per_ip_address_limit_drops_over_the_boundary() {
        let (interface, sink, _addr) = filtered_interface(9131);
        interface.set_rate_limit_period(1.0);
        interface.set_per_ip_address_rate_limit(3);

        for port in 9132..=9134 {
            let addr = Address::new(LOOPBACK, socket::htons(port));
            let mut login = login_wire(BASE_APP_LOGIN_ID);
            assert_eq!(
                interface.process_packet(&addr, &mut login),
                Reason::Success,
                "the limit is still met"
            );
        }
        let addr = Address::new(LOOPBACK, socket::htons(9135));
        let mut over = login_wire(BASE_APP_LOGIN_ID);
        assert_eq!(
            interface.process_packet(&addr, &mut over),
            Reason::GeneralNetwork,
            "the fourth datagram from the same address is dropped"
        );
        assert_eq!(sink.packets.borrow().len(), 3);
    }

    /// The per-address-and-port limit is a separate bucket per full
    /// address, so a different port starts again from zero
    /// (network_interface.cpp:993-1002).
    #[test]
    fn the_per_ip_address_port_limit_counts_each_port_separately() {
        let (interface, _sink, _addr) = filtered_interface(9132);
        interface.set_rate_limit_period(1.0);
        interface.set_per_ip_address_port_rate_limit(2);

        let addr = Address::new(LOOPBACK, socket::htons(9136));
        let mut first = login_wire(BASE_APP_LOGIN_ID);
        assert_eq!(interface.process_packet(&addr, &mut first), Reason::Success);
        let mut second = login_wire(BASE_APP_LOGIN_ID);
        assert_eq!(
            interface.process_packet(&addr, &mut second),
            Reason::Success
        );
        let mut third = login_wire(BASE_APP_LOGIN_ID);
        assert_eq!(
            interface.process_packet(&addr, &mut third),
            Reason::GeneralNetwork,
            "the third from one port is over its limit"
        );

        let other = Address::new(LOOPBACK, socket::htons(9137));
        let mut fresh = login_wire(BASE_APP_LOGIN_ID);
        assert_eq!(
            interface.process_packet(&other, &mut fresh),
            Reason::Success,
            "the other port keeps its own bucket"
        );
    }

    /// When the period elapses the counters sample into their averages
    /// and reset, so the address can send again
    /// (network_interface.cpp:540-546).
    #[test]
    fn the_period_timer_samples_the_averages_and_clears_the_counts() {
        let (interface, clock) = channel_interface(InterfaceType::External);
        let dispatcher = interface.dispatcher();
        interface.set_verbose(true);
        let addr = Address::new(LOOPBACK, socket::htons(9141));
        interface.set_p_off_channel_filter(Some(Rc::new(InitialConnectionFilter::new(
            BASE_APP_LOGIN_ID,
        ))));
        interface.set_rate_limit_period(1.0);
        interface.set_per_ip_address_port_rate_limit(1);

        let mut first = login_wire(BASE_APP_LOGIN_ID);
        assert_eq!(interface.process_packet(&addr, &mut first), Reason::Success);
        let mut second = login_wire(BASE_APP_LOGIN_ID);
        assert_eq!(
            interface.process_packet(&addr, &mut second),
            Reason::GeneralNetwork,
            "one per period"
        );
        assert_eq!(interface.rate_limit_averages(), (0.0, 0.0));

        // One period later the counts are sampled and cleared.
        clock.set(1_000_000 + 1_000_001);
        dispatcher.process_timers();
        let bias = Ema::calculate_bias_from_num_samples(5.0, 0.95);
        assert_eq!(
            interface.rate_limit_averages(),
            (0.0, (1.0 - bias) * 1.0),
            "the address-port bucket saw exactly one over-limit datagram"
        );
        let mut third = login_wire(BASE_APP_LOGIN_ID);
        assert_eq!(
            interface.process_packet(&addr, &mut third),
            Reason::Success,
            "the reset counters admit the address again"
        );
    }

    /// Both limits count at once and each keeps its own average
    /// (network_interface.cpp:978-1002): the per-address one trips first
    /// here, so only its bucket accumulates.
    #[test]
    fn both_limits_trip_and_accumulate_separately() {
        let (interface, clock) = channel_interface(InterfaceType::External);
        let dispatcher = interface.dispatcher();
        interface.set_p_off_channel_filter(Some(Rc::new(InitialConnectionFilter::new(
            BASE_APP_LOGIN_ID,
        ))));
        interface.set_rate_limit_period(1.0);
        interface.set_per_ip_address_rate_limit(1);
        interface.set_per_ip_address_port_rate_limit(1);

        let first_addr = Address::new(LOOPBACK, socket::htons(9138));
        let mut login = login_wire(BASE_APP_LOGIN_ID);
        assert_eq!(
            interface.process_packet(&first_addr, &mut login),
            Reason::Success
        );
        let second_addr = Address::new(LOOPBACK, socket::htons(9139));
        let mut login = login_wire(BASE_APP_LOGIN_ID);
        assert_eq!(
            interface.process_packet(&second_addr, &mut login),
            Reason::GeneralNetwork,
            "the second address of the IP is over the per-address limit"
        );

        clock.set(1_000_000 + 1_000_001);
        dispatcher.process_timers();
        let bias = Ema::calculate_bias_from_num_samples(5.0, 0.95);
        assert_eq!(
            interface.rate_limit_averages(),
            ((1.0 - bias) * 1.0, 0.0),
            "only the per-address bucket went over"
        );
    }

    /// The rate check precedes the channel lookup, so a datagram for a
    /// registered channel is dropped rather than dispatched
    /// (packet_receiver.cpp:445-455).
    #[test]
    fn a_channelled_address_is_limited_before_its_channel_is_found() {
        let (interface, _clock) = channel_interface(InterfaceType::Internal);
        interface.set_verbose(true);
        let addr = Address::new(LOOPBACK, socket::htons(9139));
        UdpChannel::new(
            &interface,
            addr,
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));
        interface.set_rate_limit_period(1.0);
        interface.set_per_ip_address_port_rate_limit(1);

        let mut first = ordered_wire(0, 0x41);
        assert_eq!(
            interface.process_packet(&addr, &mut first),
            Reason::Success,
            "the first reaches the channel"
        );
        let mut second = ordered_wire(1, 0x42);
        assert_eq!(
            interface.process_packet(&addr, &mut second),
            Reason::GeneralNetwork,
            "the limit speaks before the channel lookup"
        );
        assert_eq!(
            sink.packets.borrow().len(),
            1,
            "the second never dispatched"
        );
    }

    /// `rateLimitPeriod( 0 )` cancels the period timer and turns the
    /// limits off again (network_interface.cpp:936-951).
    #[test]
    fn setting_the_period_to_zero_disables_and_cancels() {
        let (interface, clock) = channel_interface(InterfaceType::External);
        interface.set_verbose(true);
        interface.set_p_off_channel_filter(Some(Rc::new(InitialConnectionFilter::new(
            BASE_APP_LOGIN_ID,
        ))));
        let addr = Address::new(LOOPBACK, socket::htons(9140));
        interface.set_rate_limit_period(1.0);
        interface.set_per_ip_address_port_rate_limit(1);
        let mut first = login_wire(BASE_APP_LOGIN_ID);
        assert_eq!(interface.process_packet(&addr, &mut first), Reason::Success);

        interface.set_rate_limit_period(0.0);
        let mut second = login_wire(BASE_APP_LOGIN_ID);
        assert_eq!(
            interface.process_packet(&addr, &mut second),
            Reason::Success,
            "no period, no limit"
        );
        clock.set(1_000_000 + 1_000_001);
        assert_eq!(
            interface.dispatcher().process_timers(),
            0,
            "the period timer was cancelled"
        );
    }

    /// The bare `PacketFilter` is the pass-through: with no override
    /// installed, the same login request runs the base `recv`'s
    /// `processFilteredPacket` by itself.
    #[test]
    fn an_installed_filter_without_an_override_passes_through() {
        struct NoFilter;

        impl crate::packet_filter::PacketFilter for NoFilter {}

        let (interface, _clock) = channel_interface(InterfaceType::External);
        interface.set_verbose(true);
        interface.set_p_off_channel_filter(Some(Rc::new(NoFilter)));
        let addr = Address::new(LOOPBACK, socket::htons(9114));
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        let mut login = login_wire(BASE_APP_LOGIN_ID);
        assert_eq!(interface.process_packet(&addr, &mut login), Reason::Success);
        assert_eq!(sink.packets.borrow().len(), 1);
    }
}
