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
use crate::interface_table::InterfaceTable;
use crate::interfaces::InputMessageHandler;
use crate::irregular_channels::IrregularChannels;
use crate::misc::{CHANNEL_ID_NULL, Reason};
use crate::packet::Packet;
use crate::packet_filter::PacketFilter;
use crate::packet_receiver::{ErrorReporter, PacketReceiver, PacketSink, strerror};
use crate::packet_sender::{PacketSender, SendingStats};
use crate::poller::EventPoller;
use crate::recently_dead_channels::RecentlyDeadChannels;
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
    /// `pIrregularChannels_` (BW-02-03-04): the shared resend timer for
    /// the channels that do not send regularly.
    irregular: RefCell<IrregularChannels>,
    /// `pRequestManager_`.
    request_manager: RefCell<Option<Rc<RequestManager>>>,
    /// Where `processPacket` hands datagrams until channels exist.
    sink: RefCell<Option<Rc<dyn PacketSink>>>,
    /// `pInterfaceTable_` (network_interface.cpp:122): the message-id to
    /// handler map the receive dispatch draws on.
    interface_table: Rc<InterfaceTable>,
    /// `dispatcher().errorReporter()`, defaulting to the logging reporter.
    reporter: RefCell<Option<Rc<dyn ErrorReporter>>>,
    /// `findChannel`, settable until the channel map lands (BW-02-03).
    channel_finder: RefCell<Option<Rc<ChannelFinder>>>,
    /// This interface's own shared handle, so channel creation
    /// (`findChannel`'s anonymous arm) can hand out an `Rc` of it.
    self_weak: RefCell<Weak<NetworkInterface>>,
    /// `pPacketSender_` (network_interface.cpp:110): the socket write path
    /// (BW-02-03-06), created once this interface's own `Rc` exists.
    packet_sender: RefCell<PacketSender>,
    /// `pOffChannelFilter_` (network_interface.hpp:328, BW-02-05-01): the
    /// filter off-channel packets are handed to before the header walk.
    off_channel_filter: RefCell<Option<Rc<dyn PacketFilter>>>,
    /// `pRecentlyDeadChannels_` (network_interface.hpp:307, BW-02-05-01):
    /// external interfaces remember recently deregistered channel
    /// addresses, so their stragglers are dropped rather than read raw.
    /// `None` on an internal interface, like the reference's unset
    /// `pRecentlyDeadChannels_` on one (network_interface.cpp:165-170).
    recently_dead: RefCell<Option<Rc<RecentlyDeadChannels>>>,
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
        // `setMainDispatcher` creates the recently-dead set only for
        // external interfaces (network_interface.cpp:165-170); the port
        // has no main-dispatcher attach step (BW-02-01-05), so the
        // interface's own dispatcher is the one the timers arm on.
        let recently_dead = (interface_type == InterfaceType::External)
            .then(|| RecentlyDeadChannels::new(Rc::clone(&dispatcher)));

        let interface = NetworkInterface {
            udp_socket: RefCell::new(Endpoint::new()),
            address: RefCell::new(NONE),
            is_external: interface_type == InterfaceType::External,
            verbose: Cell::new(true),
            poller: dispatcher.poller(),
            dispatcher,
            channels: RefCell::new(ChannelMap::new()),
            condemned: RefCell::new(CondemnedChannels::new()),
            irregular: RefCell::new(IrregularChannels::new()),
            request_manager: RefCell::new(None),
            sink: RefCell::new(None),
            interface_table: Rc::new(InterfaceTable::new()),
            reporter: RefCell::new(None),
            channel_finder: RefCell::new(None),
            self_weak: RefCell::new(Weak::new()),
            packet_sender: RefCell::new(PacketSender::new(Weak::new())),
            off_channel_filter: RefCell::new(None),
            recently_dead: RefCell::new(recently_dead),
        };
        let shared = Rc::new(interface);
        *shared.self_weak.borrow_mut() = Rc::downgrade(&shared);
        *shared.packet_sender.borrow_mut() = PacketSender::new(Rc::downgrade(&shared));
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

    /// `interfaceTable()` (network_interface.cpp:743): the message-id to
    /// handler map the receive dispatch draws on.
    pub fn interface_table(&self) -> Rc<InterfaceTable> {
        Rc::clone(&self.interface_table)
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

    /// `deregisterChannel` (network_interface.cpp:662): drop the channel's
    /// address. A recently deregistered external channel is remembered for
    /// a while, so its leftover packets are dropped instead of being read
    /// raw (network_interface.cpp:499-501).
    pub fn deregister_channel(&self, channel: &ChannelPtr) -> bool {
        let (is_external, addr) = {
            let channel = channel.borrow();
            (channel.is_external(), channel.addr())
        };
        if is_external && let Some(recently_dead) = self.recently_dead.borrow().clone() {
            RecentlyDeadChannels::add(&recently_dead, addr);
        }
        self.channels.borrow_mut().remove(channel)
    }

    /// `pOffChannelFilter()` (network_interface.hpp:134): the filter
    /// off-channel packets are handed to.
    pub fn p_off_channel_filter(&self) -> Option<Rc<dyn PacketFilter>> {
        self.off_channel_filter.borrow().clone()
    }

    /// `pOffChannelFilter( pFilter )` (network_interface.hpp:135).
    pub fn set_p_off_channel_filter(&self, filter: Option<Rc<dyn PacketFilter>>) {
        *self.off_channel_filter.borrow_mut() = filter;
    }

    /// `isDead( addr )` (network_interface.cpp:531): whether the address
    /// is in the recently-dead set; always false without the set, exactly
    /// the reference's null `pRecentlyDeadChannels_`.
    pub fn is_dead(&self, addr: &Address) -> bool {
        self.recently_dead
            .borrow()
            .as_ref()
            .is_some_and(|recently_dead| recently_dead.is_dead(addr))
    }

    /// `setRequestManager`'s seam: the reply bookkeeping the channel
    /// deaths and failures report to. Setting one also registers it as
    /// the `InterfaceElement::REPLY` handler, as the reference's
    /// constructor does (network_interface.cpp:147).
    pub fn set_request_manager(&self, manager: Option<Rc<RequestManager>>) {
        if let Some(manager) = manager.as_ref() {
            self.interface_table.serve(
                &crate::interface_element::InterfaceElement::reply(),
                Some(manager.clone() as Rc<dyn InputMessageHandler>),
            );
        }
        *self.request_manager.borrow_mut() = manager;
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

    // -- Sending (BW-02-03-06) -------------------------------------------

    /// `addReplyOrdersTo( bundle, pChannel )` (network_interface.cpp:729):
    /// hand a finalised bundle's request reply orders to the manager.
    pub fn add_reply_orders_to(&self, bundle: &mut UdpBundle, channel: Option<&ChannelPtr>) {
        let manager = self.request_manager.borrow();
        let Some(manager) = manager.as_deref() else {
            // Bundle::addReplyOrdersTo returns early on a null manager.
            return;
        };
        let channel_addr = channel.map(|channel| channel.borrow().addr());
        bundle.add_reply_orders_to(manager, channel_addr);
    }

    /// `send( address, bundle, pChannel )` (network_interface.cpp:809):
    /// the bundle leaves through the packet sender.
    pub fn send(&self, address: &Address, bundle: &mut UdpBundle, channel: Option<&ChannelPtr>) {
        self.packet_sender
            .borrow_mut()
            .send(address, bundle, channel);
    }

    /// `sendPacket( address, pPacket, pChannel, isResend )`
    /// (network_interface.cpp:840).
    pub fn send_packet(
        &self,
        address: &Address,
        packet: &Rc<Packet>,
        channel: Option<&ChannelPtr>,
        is_resend: bool,
    ) {
        self.packet_sender
            .borrow_mut()
            .send_packet(address, packet, channel, is_resend);
    }

    /// `sendingStats()`: a snapshot of the outbound counters.
    pub fn sending_stats(&self) -> SendingStats {
        self.packet_sender.borrow().stats().clone()
    }

    /// `PacketSender::sendRescheduledPacket` reached from the resend timer:
    /// a delayed packet goes out through the interface's sender.
    pub(crate) fn send_rescheduled_packet(
        &self,
        addr: &Address,
        packet: &Rc<Packet>,
        channel: Option<&ChannelPtr>,
    ) {
        self.packet_sender
            .borrow_mut()
            .send_rescheduled_packet(addr, packet, channel);
    }

    /// `dropNextSend()` (network_interface.cpp:875): the next packet is
    /// counted and dropped.
    pub fn drop_next_send(&self) {
        self.packet_sender.borrow_mut().drop_next_send();
    }

    /// `setLossRatio( lossRatio )` (network_interface.cpp:866).
    pub fn set_loss_ratio(&self, loss_ratio: f32) {
        self.packet_sender
            .borrow_mut()
            .loss_mut()
            .set_loss_ratio(loss_ratio);
    }

    /// `setLatency( latencyMin, latencyMax )` (network_interface.cpp:854).
    pub fn set_latency(&self, latency_min: f32, latency_max: f32) {
        let mut sender = self.packet_sender.borrow_mut();
        let loss = sender.loss_mut();
        loss.set_min_latency(latency_min);
        loss.set_max_latency(latency_max);
    }

    /// `shouldUseChecksums()`.
    pub fn should_use_checksums(&self) -> bool {
        self.packet_sender.borrow().should_use_checksums()
    }

    /// `shouldUseChecksums( v )`.
    pub fn set_should_use_checksums(&self, v: bool) {
        self.packet_sender.borrow_mut().set_should_use_checksums(v);
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

    // -- Irregular channels (BW-02-03-04) ---------------------------------

    /// `pNetworkInterface_->irregularChannels().addIfNecessary( *this )`,
    /// as the channel's `isLocalRegular` setter and send path reach it.
    pub fn add_irregular_channel_if_necessary(&self, channel: &ChannelPtr) {
        self.irregular.borrow_mut().add_if_necessary(
            channel,
            &self.dispatcher,
            &self.self_weak.borrow(),
        );
    }

    /// `pNetworkInterface_->irregularChannels().delIfNecessary( *this )`,
    /// as `clearState` and `switchInterface` reach it.
    pub fn del_irregular_channel_if_necessary(&self, channel: &ChannelPtr) {
        self.irregular.borrow_mut().del_if_necessary(channel);
    }

    /// `setIrregularChannelsResendPeriod( seconds )`
    /// (network_interface.cpp:718): set the shared resend-check period;
    /// zero or less stops the monitoring.
    pub fn set_irregular_channels_resend_period(&self, seconds: f32) {
        self.irregular
            .borrow_mut()
            .set_period(seconds, &self.dispatcher, &self.self_weak.borrow());
    }

    /// `IrregularChannels::handleTimeout`: the shared timer's verdict --
    /// every member gets its resend check.
    pub fn check_irregular_channels(&self) {
        self.irregular.borrow_mut().check();
    }

    /// Test access to the irregular collection.
    #[cfg(test)]
    pub fn irregular_channels(&self) -> &RefCell<IrregularChannels> {
        &self.irregular
    }

    /// Drop everything irregular without destroying, for tests that must
    /// leave no resend timer behind.
    #[cfg(test)]
    pub fn irregular_clear_for_test(&self) {
        self.irregular.borrow_mut().clear();
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

    /// The loss and checksum knobs round-trip through the interface
    /// (network_interface.cpp:854-875).
    #[test]
    fn the_loss_and_checksum_knobs_round_trip() {
        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);

        interface.set_loss_ratio(0.25);
        assert_eq!(
            interface.packet_sender.borrow().loss().drop_per_million(),
            250_000
        );

        interface.set_latency(0.1, 0.2);
        assert_eq!(
            interface
                .packet_sender
                .borrow()
                .loss()
                .min_latency_million(),
            100
        );
        assert_eq!(
            interface
                .packet_sender
                .borrow()
                .loss()
                .max_latency_million(),
            200
        );

        assert!(!interface.should_use_checksums());
        interface.set_should_use_checksums(true);
        assert!(interface.should_use_checksums());
    }

    /// With no request manager, reply orders have nowhere to go and the
    /// bundle stays as it was (network_interface.cpp:729).
    #[test]
    fn reply_orders_without_a_manager_stop_quietly() {
        let (interface, _receiver) = loopback_interface(InterfaceType::Internal);
        let mut bundle = UdpBundle::new(0);
        bundle.start_message(
            &crate::interface_element::InterfaceElement::new(
                "m",
                1,
                crate::interface_element::FIXED_LENGTH_MESSAGE,
                4,
                None,
            ),
            crate::udp_bundle::ReliableType::Driver,
        );
        bundle.write_bytes(&7u32.to_le_bytes());
        bundle.finalise();

        interface.set_request_manager(None);
        interface.add_reply_orders_to(&mut bundle, None);
        assert!(
            !bundle.is_empty(),
            "the bundle is untouched without a manager"
        );
    }
}
