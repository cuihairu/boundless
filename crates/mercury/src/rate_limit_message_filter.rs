//! `RateLimitMessageFilter` (`server/baseapp/rate_limit_message_filter.
//! {hpp,cpp}`): the message filter that enforces
//! [`RateLimitConfig`](crate::rate_limit_config) on everything a client
//! sends upstream.
//!
//! `Proxy::attachToClient` (proxy.cpp:584) fits one of these onto every
//! client channel, so a client that sends faster than its configured
//! per-tick budgets allow has its messages buffered and replayed when the
//! budget allows again, and has messages dropped outright once the buffer
//! itself is full. The reference's per-tick budgets and the
//! buffered-queue ceilings live in [`crate::rate_limit_config`].
//!
//! The reference couples the filter to the baseapp's `Proxy`
//! (`pProxy_->clientAddr()`, `pProxy_->onFilterLimitsExceeded`) and to
//! `BaseApp::setBaseForCall`/`getProxyForCall` -- the guard that notices a
//! proxy swap mid-replay. The baseapp layer is not ported yet, so:
//! the proxy is the client address plus the proxy id the reference's log
//! lines carry ([`RateLimitMessageFilter::set_proxy`] rebinds both and
//! clears the queue, exactly as `setProxy` does), and
//! `onFilterLimitsExceeded` is the [`RateLimitMessageFilter::
//! set_on_limits_exceeded`] hook. The `getProxyForCall` guard reduces to
//! comparing the proxy id each replay iteration against the one the
//! replay started with -- a `set_proxy` during a replayed dispatch stops
//! the replay the same way, and logs the same notice.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use foundation::stream::BinaryStream;

use crate::endpoint::Address;
use crate::interfaces::InputMessageHandler;
use crate::message_filter::MessageFilter;
use crate::rate_limit_config::RateLimitConfig;
use crate::unpacked_message_header::UnpackedMessageHeader;

/// Warn bit flag (`WARN_MESSAGE_COUNT`): the client sent at least
/// `warnMessagesPerTick` messages since the last tick.
const WARN_MESSAGE_COUNT: u8 = 0x01;
/// Warn bit flag (`WARN_MESSAGE_SIZE`): the client sent at least
/// `warnBytesPerTick` bytes since the last tick.
const WARN_MESSAGE_SIZE: u8 = 0x02;
/// Warn bit flag (`WARN_MESSAGE_BUFFERED`): the buffer holds at least
/// `warnMessagesBuffered` messages.
const WARN_MESSAGE_BUFFERED: u8 = 0x04;
/// Warn bit flag (`WARN_BYTES_BUFFERED`): the buffer holds at least
/// `warnBytesBuffered` bytes.
const WARN_BYTES_BUFFERED: u8 = 0x08;

/// `Proxy::onFilterLimitsExceeded( srcAddr, pMsg )`: told when a message
/// could not be buffered and is about to be dropped. The baseapp proxy
/// counts these; the port's hook receives the same pair.
type LimitsExceededHook = Rc<dyn Fn(&Address, &BufferedMessage)>;

/// A buffered message instance (`BufferedMessage`): the header, the data
/// stream and the destination handler stored for deferred playback.
///
/// The reference notes application code can derive from it to associate
/// state or override the dispatch; the port keeps the one shape and the
/// virtual `dispatch` reduces to delivering to the stored handler on the
/// proxy's client address.
pub struct BufferedMessage {
    header: UnpackedMessageHeader,
    data: Vec<u8>,
    handler: Rc<dyn InputMessageHandler>,
}

impl BufferedMessage {
    /// `BufferedMessage( header, data, pHandler )`: copy the message out
    /// of the stream. The `MemoryOStream::transfer` the reference uses
    /// empties the source stream -- `finish()` here -- so the dispatch
    /// walk's "handler left bytes" check does not fire on a message the
    /// filter buffered.
    pub fn new(
        header: &UnpackedMessageHeader,
        data: &mut BinaryStream,
        handler: Rc<dyn InputMessageHandler>,
    ) -> Self {
        let body = data.unread_bytes().to_vec();
        data.finish();
        BufferedMessage {
            header: header.clone(),
            data: body,
            handler,
        }
    }

    /// `size()`: the data size of the message.
    pub fn size(&self) -> usize {
        self.data.len()
    }

    /// `header()`.
    pub fn header(&self) -> &UnpackedMessageHeader {
        &self.header
    }

    /// The buffered body.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// `dispatch( pProxy )`: deliver to the stored handler on the proxy's
    /// client address.
    fn dispatch(&mut self, addr: &Address) {
        let mut stream = BinaryStream::new();
        stream.write_bytes(&self.data);
        self.handler
            .handle_message(addr, &mut self.header, &mut stream);
    }
}

/// The mutable half of the filter; every field is the reference field of
/// the same name.
struct RateLimitState {
    proxy_addr: Address,
    proxy_id: u32,
    warn_flags: u8,
    num_received_since_last_tick: usize,
    received_bytes_since_last_tick: usize,
    num_dispatched_since_last_tick: usize,
    dispatched_bytes_since_last_tick: usize,
    queue: VecDeque<BufferedMessage>,
    sum_buffered_sizes: usize,
}

/// All messages from a client get passed through an instance of this
/// class. It is responsible for enforcing rate limits on client messages,
/// buffering and playing back messages when limits are no longer
/// exceeded.
pub struct RateLimitMessageFilter {
    config: RateLimitConfig,
    state: RefCell<RateLimitState>,
    on_limits_exceeded: RefCell<Option<LimitsExceededHook>>,
}

impl RateLimitMessageFilter {
    /// `RateLimitMessageFilter( addr )`. The reference ignores the
    /// argument and binds the client through `setProxy`; the port seeds
    /// the proxy address with it so the filter is never unbound, which is
    /// the value `attachToClient` passes anyway (`pClientChannel_->addr()`).
    pub fn new(addr: Address, config: RateLimitConfig) -> Self {
        RateLimitMessageFilter {
            config,
            state: RefCell::new(RateLimitState {
                proxy_addr: addr,
                proxy_id: 0,
                warn_flags: 0,
                num_received_since_last_tick: 0,
                received_bytes_since_last_tick: 0,
                num_dispatched_since_last_tick: 0,
                dispatched_bytes_since_last_tick: 0,
                queue: VecDeque::new(),
                sum_buffered_sizes: 0,
            }),
            on_limits_exceeded: RefCell::new(None),
        }
    }

    /// `setProxy( pProxy )`: rebind the client. Messages buffered for the
    /// old client are not played back on the new one -- the queue is
    /// cleared first, which is the reference's mitigation for a filter
    /// handed to another client.
    pub fn set_proxy(&self, addr: Address, proxy_id: u32) {
        let mut st = self.state.borrow_mut();
        if !st.queue.is_empty() {
            // `clear` logs against the proxy it is clearing, before the
            // rebind.
            eprintln!(
                "RateLimitMessageFilter::clear: removing {} buffered messages for proxy {}",
                st.queue.len(),
                st.proxy_id
            );
        }
        st.queue.clear();
        st.sum_buffered_sizes = 0;
        st.proxy_addr = addr;
        st.proxy_id = proxy_id;
    }

    /// The `Proxy::onFilterLimitsExceeded` seam: the hook told when a
    /// message cannot be buffered and is dropped.
    pub fn set_on_limits_exceeded(&self, hook: Option<LimitsExceededHook>) {
        *self.on_limits_exceeded.borrow_mut() = hook;
    }

    /// `tick()`: update the filter for the next tick. Called periodically
    /// by the application -- `BaseApp::updateWeather`'s neighbourhood, the
    /// per-tick housekeeping slot.
    pub fn tick(&self) {
        {
            let mut st = self.state.borrow_mut();
            // Check if we are now over the receive warn limits.
            if (st.warn_flags & WARN_MESSAGE_COUNT) == 0
                && st.num_received_since_last_tick >= self.config.warn_messages_per_tick() as usize
            {
                eprintln!(
                    "RateLimitMessageFilter: Client {} has sent {} messages since last tick ({} bytes)",
                    st.proxy_addr.c_str(),
                    st.num_received_since_last_tick,
                    st.received_bytes_since_last_tick
                );
                st.warn_flags |= WARN_MESSAGE_COUNT;
            } else {
                st.warn_flags &= !WARN_MESSAGE_COUNT;
            }

            if (st.warn_flags & WARN_MESSAGE_SIZE) == 0
                && st.received_bytes_since_last_tick >= self.config.warn_bytes_per_tick() as usize
            {
                eprintln!(
                    "RateLimitMessageFilter: Client {} has sent {} bytes since last tick ({} messages)",
                    st.proxy_addr.c_str(),
                    st.received_bytes_since_last_tick,
                    st.num_received_since_last_tick
                );
                st.warn_flags |= WARN_MESSAGE_SIZE;
            } else {
                st.warn_flags &= !WARN_MESSAGE_SIZE;
            }
        }

        self.replay_any();

        let mut st = self.state.borrow_mut();
        // Check if we are still over our buffered warn limits.
        if (st.warn_flags & WARN_MESSAGE_COUNT) != 0
            && st.num_dispatched_since_last_tick < self.config.warn_messages_per_tick() as usize
        {
            st.warn_flags &= !WARN_MESSAGE_COUNT;
        }
        if (st.warn_flags & WARN_MESSAGE_SIZE) != 0
            && st.dispatched_bytes_since_last_tick < self.config.warn_bytes_per_tick() as usize
        {
            st.warn_flags &= !WARN_MESSAGE_SIZE;
        }

        // Reset our stats for the next tick.
        st.num_received_since_last_tick = 0;
        st.received_bytes_since_last_tick = 0;
        st.num_dispatched_since_last_tick = 0;
        st.dispatched_bytes_since_last_tick = 0;
    }

    /// `canSendNow( dataLen )`: whether a new incoming message of
    /// `data_len` bytes can be accepted -- false when the count or the
    /// size budget for this tick is used up.
    fn can_send_now(&self, st: &RateLimitState, data_len: usize) -> bool {
        st.num_dispatched_since_last_tick <= self.config.max_messages_per_tick() as usize
            && st.dispatched_bytes_since_last_tick + data_len
                <= self.config.max_bytes_per_tick() as usize
    }

    /// `buffer( pMsg )`: buffer the message for later handling. False when
    /// the buffered limits are exceeded -- the message is then told to the
    /// limits-exceeded hook and dropped.
    fn buffer(&self, msg: BufferedMessage, src_addr: &Address) -> bool {
        let mut st = self.state.borrow_mut();

        if st.queue.len() >= self.config.max_messages_buffered() as usize {
            eprintln!(
                "RateLimitMessageFilter({}): Could not buffer further messages, message count limit reached",
                st.proxy_id
            );
            self.notify_limits_exceeded(src_addr, &msg);
            return false;
        }
        if st.sum_buffered_sizes + msg.size() >= self.config.max_bytes_buffered() as usize {
            eprintln!(
                "RateLimitMessageFilter({}): Could not buffer further messages, message size limit reached",
                st.proxy_id
            );
            self.notify_limits_exceeded(src_addr, &msg);
            return false;
        }

        let size = msg.size();
        st.queue.push_back(msg);
        st.sum_buffered_sizes += size;

        if (st.warn_flags & WARN_MESSAGE_BUFFERED) == 0
            && st.queue.len() >= self.config.warn_messages_buffered() as usize
        {
            eprintln!(
                "RateLimitMessageFilter: {} messages pending in buffer from client {}",
                st.queue.len(),
                st.proxy_addr.c_str()
            );
            st.warn_flags |= WARN_MESSAGE_BUFFERED;
        }
        if (st.warn_flags & WARN_BYTES_BUFFERED) == 0
            && st.sum_buffered_sizes >= self.config.warn_bytes_buffered() as usize
        {
            eprintln!(
                "RateLimitMessageFilter: {} bytes pending in buffer from client {}",
                st.sum_buffered_sizes,
                st.proxy_addr.c_str()
            );
            st.warn_flags |= WARN_BYTES_BUFFERED;
        }
        true
    }

    /// `pProxy_->onFilterLimitsExceeded( srcAddr, pMsg )`: told when the
    /// message cannot be buffered and is dropped.
    fn notify_limits_exceeded(&self, src_addr: &Address, msg: &BufferedMessage) {
        if let Some(hook) = self.on_limits_exceeded.borrow().as_ref() {
            hook(src_addr, msg);
        }
    }

    /// `dispatch( header, data, pHandler )`: dispatch a message now,
    /// counting it against this tick's budget.
    fn dispatch_now(
        &self,
        header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
        handler: &Rc<dyn InputMessageHandler>,
    ) {
        let addr = {
            let mut st = self.state.borrow_mut();
            st.num_dispatched_since_last_tick += 1;
            st.dispatched_bytes_since_last_tick += data.remaining();
            st.proxy_addr
        };
        // `pHandler->handleMessage( pProxy_->clientAddr(), header, data )`.
        handler.handle_message(&addr, header, data);
    }

    /// `replayAny()`: replay buffered messages that are eligible to be
    /// dispatched now. Called by `tick` -- and so once a tick -- with the
    /// *current* tick's dispatch counters, before they reset: a tick that
    /// follows over-limit traffic has no budget left and replays nothing.
    fn replay_any(&self) {
        let replaying_id = self.state.borrow().proxy_id;

        loop {
            // `while (pMsg && getProxyForCall(false) == pProxy &&
            // canSendNow( pMsg->size() ))`: the front guard borrows once
            // and releases before the dispatch, so a replayed handler can
            // rebind the proxy mid-replay without a borrow conflict.
            let should_dispatch = {
                let st = self.state.borrow();
                let Some(size) = st.queue.front().map(|m| m.size()) else {
                    break;
                };
                if st.proxy_id != replaying_id {
                    break;
                }
                self.can_send_now(&st, size)
            };
            if !should_dispatch {
                break;
            }

            // `pop_front()`: the sum tracks the queue before the dispatch.
            let mut msg = {
                let mut st = self.state.borrow_mut();
                let size = st.queue.front().expect("front checked above").size();
                st.sum_buffered_sizes -= size;
                st.queue.pop_front().expect("front checked above")
            };
            let addr = {
                let mut st = self.state.borrow_mut();
                st.num_dispatched_since_last_tick += 1;
                st.dispatched_bytes_since_last_tick += msg.size();
                st.proxy_addr
            };
            msg.dispatch(&addr);
        }

        if self.state.borrow().proxy_id != replaying_id {
            eprintln!(
                "RateLimitMessageFilter: current proxy was changed while replaying rate-limited messages for proxy {}",
                replaying_id
            );
        }

        let mut st = self.state.borrow_mut();
        if st.queue.len() < self.config.warn_messages_buffered() as usize {
            st.warn_flags &= !WARN_MESSAGE_BUFFERED;
        }
        if st.sum_buffered_sizes < self.config.warn_bytes_buffered() as usize {
            st.warn_flags &= !WARN_BYTES_BUFFERED;
        }
    }

    /// The number of messages waiting in the buffer.
    pub fn num_buffered(&self) -> usize {
        self.state.borrow().queue.len()
    }

    /// The total size of the messages waiting in the buffer.
    pub fn sum_buffered_sizes(&self) -> usize {
        self.state.borrow().sum_buffered_sizes
    }
}

impl MessageFilter for RateLimitMessageFilter {
    /// `filterMessage`: filter an incoming message and decide whether to
    /// handle it now or buffer for handling later, based on message
    /// dispatch statistics for the source address.
    fn filter_message(
        &self,
        src_addr: &Address,
        header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
        handler: &Rc<dyn InputMessageHandler>,
    ) {
        // The identity check and the counting run in one short borrow; the
        // decision is taken before any handler runs.
        let buffer_it = {
            let mut st = self.state.borrow_mut();
            if *src_addr != st.proxy_addr {
                // We've probably been given to another client from the
                // last message in the bundle. We can't play these messages
                // for the old proxy on the new proxy.
                eprintln!(
                    "RateLimitMessageFilter::filterMessage: Dropping message from {} bound for proxy {}",
                    src_addr.c_str(),
                    st.proxy_id
                );
                return;
            }
            st.num_received_since_last_tick += 1;
            st.received_bytes_since_last_tick += data.remaining();
            !(st.queue.is_empty() && self.can_send_now(&st, data.remaining()))
        };

        if buffer_it {
            let msg = BufferedMessage::new(header, data, Rc::clone(handler));
            self.buffer(msg, src_addr);
        } else {
            self.dispatch_now(header, data, handler);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interfaces::InputMessageHandler;
    use std::cell::RefCell;

    /// A sink that records every delivery's body, and can be armed to
    /// rebind the filter on its next delivery (the proxy-swap arm of a
    /// replayed dispatch).
    struct Sink {
        bodies: RefCell<Vec<Vec<u8>>>,
        armed: std::cell::Cell<bool>,
        filter: RefCell<Option<Rc<RateLimitMessageFilter>>>,
    }

    impl Sink {
        fn new() -> Rc<Sink> {
            Rc::new(Sink {
                bodies: RefCell::new(Vec::new()),
                armed: std::cell::Cell::new(false),
                filter: RefCell::new(None),
            })
        }

        fn bodies(&self) -> Vec<Vec<u8>> {
            self.bodies.borrow().clone()
        }

        /// Arm the swap: the next delivery rebinds `filter` to another
        /// client, the way a replayed handler can move the filter to a new
        /// proxy.
        fn arm_swap(&self, filter: Rc<RateLimitMessageFilter>) {
            *self.filter.borrow_mut() = Some(filter);
            self.armed.set(true);
        }
    }

    impl InputMessageHandler for Sink {
        fn handle_message(
            &self,
            _source: &Address,
            _header: &mut UnpackedMessageHeader,
            data: &mut BinaryStream,
        ) {
            self.bodies.borrow_mut().push(data.unread_bytes().to_vec());
            data.finish();
            if self.armed.get() {
                self.armed.set(false);
                if let Some(filter) = self.filter.borrow().as_ref() {
                    filter.set_proxy(other(), 7);
                }
            }
        }
    }

    /// A client address, and a second one for the wrong-client arm.
    fn client() -> Address {
        Address::new(0x7f000001, 7001)
    }

    fn other() -> Address {
        Address::new(0x7f000002, 7002)
    }

    /// A filter whose per-tick count budget is `max_messages` messages and
    /// whose byte budget is generous. `hertz = 1` makes the per-tick
    /// budgets the per-second ones handed in.
    fn filter_with(max_messages: u32, max_bytes: u32) -> Rc<RateLimitMessageFilter> {
        let config = RateLimitConfig::from_per_second(
            0,
            max_messages,
            0,
            max_bytes,
            u32::MAX,
            u32::MAX,
            u32::MAX,
            u32::MAX,
            1,
        );
        Rc::new(RateLimitMessageFilter::new(client(), config))
    }

    /// Hand the filter one message of `len` bytes and report whether it
    /// was dispatched (`true`) or buffered (`false`).
    fn offer(filter: &RateLimitMessageFilter, sink: &Rc<Sink>, body: &[u8]) -> bool {
        let before = sink.bodies().len();
        let mut stream = BinaryStream::new();
        stream.write_bytes(body);
        filter.filter_message(
            &client(),
            &mut UnpackedMessageHeader::new(3),
            &mut stream,
            &(Rc::clone(sink) as Rc<dyn InputMessageHandler>),
        );
        sink.bodies().len() > before
    }

    /// The count boundary: `canSendNow` compares the pre-dispatch count
    /// with `<=`, so the first `max + 1` messages dispatch and the one
    /// after the boundary is the first buffered -- exactly the reference's
    /// arithmetic.
    #[test]
    fn the_count_boundary_dispatches_one_past_the_limit_then_buffers() {
        let sink = Sink::new();
        let filter = filter_with(2, u32::MAX);

        assert!(offer(&filter, &sink, b"a"));
        assert!(offer(&filter, &sink, b"b"));
        assert!(
            offer(&filter, &sink, b"c"),
            "the boundary message dispatches"
        );
        assert!(
            !offer(&filter, &sink, b"d"),
            "the first over-limit message buffers"
        );

        assert_eq!(
            sink.bodies(),
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]
        );
        assert_eq!(filter.num_buffered(), 1);
    }

    /// The byte boundary: `dispatchedBytes + dataLen <= maxBytes`, so the
    /// first message that would push the budget past the ceiling buffers.
    #[test]
    fn the_byte_boundary_buffers_the_message_that_does_not_fit() {
        let sink = Sink::new();
        let filter = filter_with(u32::MAX, 10);

        assert!(offer(&filter, &sink, &[0; 4]));
        assert!(offer(&filter, &sink, &[0; 4]), "4 + 4 still fits 10");
        assert!(!offer(&filter, &sink, &[0; 4]), "8 + 4 does not");

        assert_eq!(sink.bodies().len(), 2);
        assert_eq!(filter.sum_buffered_sizes(), 4);
    }

    /// A tick replays what the budget allows and keeps the rest: with the
    /// counters reset, the queue drains in order until `canSendNow` says
    /// stop -- the same `<=` boundary governs the replay.
    #[test]
    fn a_tick_replays_what_the_budget_allows_and_keeps_the_rest() {
        let sink = Sink::new();
        // A zero count budget dispatches exactly one message per tick (the
        // `<=` boundary: the pre-dispatch count 0 is allowed) and buffers
        // everything after it.
        let filter = filter_with(0, u32::MAX);

        assert!(offer(&filter, &sink, b"one"));
        assert!(!offer(&filter, &sink, b"two"));
        assert!(!offer(&filter, &sink, b"three"));
        assert!(!offer(&filter, &sink, b"four"));
        assert_eq!(sink.bodies(), vec![b"one".to_vec()]);
        assert_eq!(filter.num_buffered(), 3);

        // The spent tick replays nothing; each fresh tick drains exactly
        // the one message the boundary allows, keeping the rest.
        filter.tick();
        assert_eq!(sink.bodies().len(), 1, "nothing replayed on the spent tick");
        filter.tick();
        assert_eq!(sink.bodies().len(), 2, "one message drained");
        assert_eq!(filter.num_buffered(), 2);
        filter.tick();
        assert_eq!(sink.bodies().len(), 3);
        assert_eq!(filter.num_buffered(), 1);
        filter.tick();
        assert_eq!(sink.bodies().len(), 4);
        assert_eq!(filter.num_buffered(), 0);
    }

    /// A tick that follows over-limit traffic has no budget left (the
    /// replay runs before the counters reset), so it replays nothing;
    /// the buffered messages wait for the next tick.
    #[test]
    fn a_tick_following_over_limit_traffic_replays_nothing_yet() {
        let sink = Sink::new();
        let filter = filter_with(2, u32::MAX);

        for body in [b"a".as_slice(), b"b", b"c", b"d", b"e"] {
            offer(&filter, &sink, body);
        }
        // Three dispatched (the `<=` boundary), two buffered.
        assert_eq!(sink.bodies().len(), 3);
        assert_eq!(filter.num_buffered(), 2);

        // This tick's budget is spent: the replay cannot run.
        filter.tick();
        assert_eq!(sink.bodies().len(), 3, "nothing replayed on the spent tick");
        assert_eq!(filter.num_buffered(), 2);

        // The next tick's fresh budget replays up to the boundary (three
        // again: two buffered, then the first replay stops the budget).
        filter.tick();
        assert_eq!(sink.bodies().len(), 5, "both buffered messages replayed");
        assert_eq!(filter.num_buffered(), 0);
    }

    /// The buffered count ceiling: a message that does not fit the queue's
    /// message cap is not buffered -- it is told to the limits-exceeded
    /// hook and dropped, never dispatched later.
    #[test]
    fn a_message_over_the_buffered_count_cap_is_told_to_the_hook_and_dropped() {
        let sink = Sink::new();
        // A zero byte budget accepts nothing straight through, which is
        // what fills the queue to its cap.
        let config =
            RateLimitConfig::from_per_second(0, u32::MAX, 0, 0, u32::MAX, 2, u32::MAX, u32::MAX, 1);
        let filter = Rc::new(RateLimitMessageFilter::new(client(), config));
        let dropped: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
        let drop_hook = Rc::clone(&dropped);
        filter.set_on_limits_exceeded(Some(Rc::new(move |_: &Address, msg: &BufferedMessage| {
            drop_hook.borrow_mut().push(msg.data().to_vec());
        })));

        offer(&filter, &sink, b"one");
        offer(&filter, &sink, b"two");
        offer(&filter, &sink, b"three");
        assert_eq!(
            filter.num_buffered(),
            2,
            "the refused message is not queued"
        );
        assert_eq!(dropped.borrow().clone(), vec![b"three".to_vec()]);

        // The drop is final: nothing delivered, the two the queue took are
        // still waiting -- the refused message is gone, not deferred.
        filter.tick();
        assert_eq!(filter.num_buffered(), 2);
        assert!(sink.bodies().is_empty());
    }

    /// The buffered byte ceiling uses `>=`: a message that would land the
    /// buffered sum exactly on the cap is refused too.
    #[test]
    fn a_message_that_would_land_on_the_byte_cap_is_refused() {
        let sink = Sink::new();
        // A zero byte budget accepts nothing straight through, which is
        // what fills the buffer to its byte cap.
        let config = RateLimitConfig::from_per_second(
            0,
            u32::MAX,
            0,
            0,
            u32::MAX,
            u32::MAX,
            u32::MAX,
            10,
            1,
        );
        let filter = Rc::new(RateLimitMessageFilter::new(client(), config));
        let dropped: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
        let drop_hook = Rc::clone(&dropped);
        filter.set_on_limits_exceeded(Some(Rc::new(move |_: &Address, _: &BufferedMessage| {
            *drop_hook.borrow_mut() += 1;
        })));

        offer(&filter, &sink, &[0; 4]);
        assert_eq!(filter.num_buffered(), 1);
        assert_eq!(filter.sum_buffered_sizes(), 4);

        // 4 + 6 >= 10: refused, not buffered.
        offer(&filter, &sink, &[0; 6]);
        assert_eq!(*dropped.borrow(), 1, "the over-cap message is refused");
        assert_eq!(
            filter.num_buffered(),
            1,
            "the refused message is not queued"
        );
        assert_eq!(filter.sum_buffered_sizes(), 4);

        // 4 + 5 < 10: still under the cap, so it buffers.
        offer(&filter, &sink, &[0; 5]);
        assert_eq!(filter.sum_buffered_sizes(), 9);
        assert_eq!(filter.num_buffered(), 2);
        assert_eq!(
            *dropped.borrow(),
            1,
            "only the over-cap message was refused"
        );
    }

    /// A message from any other source address is dropped outright: it is
    /// counted nowhere and reaches neither the handler nor the buffer.
    #[test]
    fn a_message_from_another_client_is_dropped_outright() {
        let sink = Sink::new();
        let filter = filter_with(u32::MAX, u32::MAX);

        let mut stream = BinaryStream::new();
        stream.write_bytes(b"rogue");
        filter.filter_message(
            &other(),
            &mut UnpackedMessageHeader::new(3),
            &mut stream,
            &(Rc::clone(&sink) as Rc<dyn InputMessageHandler>),
        );

        assert!(sink.bodies().is_empty());
        assert_eq!(filter.num_buffered(), 0);
        // The drop is not counted against the receive budget either: the
        // next legitimate message still fits a fresh tick.
        assert!(offer(&filter, &sink, b"good"));
    }

    /// `setProxy` clears whatever is buffered for the old client and
    /// rebinds the address the filter serves: the new client's messages
    /// are accepted, the old one's are dropped.
    #[test]
    fn setting_the_proxy_clears_the_buffer_and_rebinds_the_address() {
        let sink = Sink::new();
        // A zero byte budget accepts nothing straight through, which is
        // what fills the queue.
        let filter = filter_with(0, 0);

        offer(&filter, &sink, b"stale one");
        offer(&filter, &sink, b"stale two");
        assert_eq!(filter.num_buffered(), 2);

        filter.set_proxy(other(), 42);

        assert_eq!(
            filter.num_buffered(),
            0,
            "the old client's queue is cleared"
        );
        assert_eq!(filter.sum_buffered_sizes(), 0);

        // The filter still serves the new address: a message from it is
        // accepted...
        let mut stream = BinaryStream::new();
        stream.write_bytes(b"fresh");
        filter.filter_message(
            &other(),
            &mut UnpackedMessageHeader::new(3),
            &mut stream,
            &(Rc::clone(&sink) as Rc<dyn InputMessageHandler>),
        );
        assert_eq!(filter.num_buffered(), 1);

        // ...and one from the old address is dropped outright.
        let mut stream = BinaryStream::new();
        stream.write_bytes(b"rogue");
        filter.filter_message(
            &client(),
            &mut UnpackedMessageHeader::new(3),
            &mut stream,
            &(Rc::clone(&sink) as Rc<dyn InputMessageHandler>),
        );
        assert_eq!(
            filter.num_buffered(),
            1,
            "the old client's message is dropped"
        );
    }

    /// The replay-swap guard: a replayed dispatch that rebinds the proxy
    /// stops the replay and logs the notice, leaving the rest of the
    /// buffer -- now cleared by the rebind -- alone.
    #[test]
    fn a_proxy_swap_during_replay_stops_the_replay() {
        let sink = Sink::new();
        let filter = filter_with(1, u32::MAX);

        // alpha and beta dispatch; gamma and delta buffer.
        for body in [b"alpha".as_slice(), b"beta", b"gamma", b"delta"] {
            offer(&filter, &sink, body);
        }
        assert_eq!(filter.num_buffered(), 2);

        // Arm the swap: the next delivery rebinds the proxy, the way a
        // replayed handler can move the filter to a new proxy.
        sink.arm_swap(Rc::clone(&filter));

        // The spent tick replays nothing; the fresh tick drains the first
        // buffered message, whose delivery rebinds the proxy mid-replay.
        filter.tick();
        assert_eq!(sink.bodies().len(), 2, "nothing replayed on the spent tick");
        filter.tick();
        assert_eq!(sink.bodies().len(), 3, "only the first message replayed");
        assert_eq!(filter.num_buffered(), 0, "the rebind cleared the rest");
    }

    /// The buffered messages replay in wire order, bodies intact, after
    /// the dispatches that fit went through -- the buffer preserves the
    /// client's ordering across the rate limit.
    #[test]
    fn the_replay_delivers_in_order_with_the_original_bodies() {
        let sink = Sink::new();
        let filter = filter_with(1, u32::MAX);

        // alpha and beta fit the count boundary (max + 1 = 2); gamma and
        // delta buffer.
        for body in [b"alpha".as_slice(), b"beta", b"gamma", b"delta"] {
            offer(&filter, &sink, body);
        }
        assert_eq!(sink.bodies(), vec![b"alpha".to_vec(), b"beta".to_vec()]);

        // The spent tick replays nothing; the fresh one drains both.
        filter.tick();
        assert_eq!(sink.bodies().len(), 2);
        filter.tick();
        assert_eq!(
            sink.bodies(),
            vec![
                b"alpha".to_vec(),
                b"beta".to_vec(),
                b"gamma".to_vec(),
                b"delta".to_vec()
            ]
        );
    }

    /// A buffered message carries a copy of the header it was offered
    /// with, and buffering consumed the source stream (the
    /// `MemoryOStream::transfer` semantics the dispatch walk relies on).
    #[test]
    fn a_buffered_message_copies_its_header_and_consumes_the_stream() {
        let sink = Sink::new();
        let filter = filter_with(1, u32::MAX);

        // The first two dispatch (the `<=` boundary); the third buffers.
        assert!(offer(&filter, &sink, b"one"));
        assert!(offer(&filter, &sink, b"two"));

        let mut header = UnpackedMessageHeader::new(9);
        header.set_reply_id(1234);
        let mut stream = BinaryStream::new();
        stream.write_bytes(b"body");
        filter.filter_message(
            &client(),
            &mut header,
            &mut stream,
            &(Rc::clone(&sink) as Rc<dyn InputMessageHandler>),
        );

        assert_eq!(stream.remaining(), 0, "the source stream was consumed");
        assert_eq!(filter.num_buffered(), 1);
        assert_eq!(
            sink.bodies().len(),
            2,
            "the buffered message did not dispatch"
        );
    }
}
