//! `TimeQueueT` + `TimerHandler` port (`lib/cstdmf/time_queue.hpp/.ipp`,
//! `lib/cstdmf/timer_handler.hpp`) and the `EventDispatcher` timer surface
//! (`lib/network/event_dispatcher.cpp`).
//!
//! The queue is cstdmf, so it lives in `foundation`; both event
//! dispatchers in this tree (server-runtime's bootstrap `Dispatcher` and
//! mercury's `EventDispatcher`) share it.
//!
//! Reference contract preserved:
//! - `add( startTime, interval, handler, user, name )` pushes a node and
//!   restores the heap; a recurrent timer carries its interval, a once-off
//!   carries 0 (`addTimerCommon` maps microseconds to stamps and zeroes
//!   the interval of once-offs);
//! - `process( now )` pops the front while it is due (`time <= now`) or
//!   cancelled, fires it (lateness tracked against `now`), and re-pushes
//!   it unless the callback cancelled it; a once-off cancels itself after
//!   firing (`interval == 0` auto-cancel);
//! - cancellation is legal from inside the callback (the
//!   PENDING/EXECUTING/CANCELLED state machine re-checks `isCancelled`
//!   after every callback) and from outside (a cancelled node is dropped
//!   without firing when it reaches the front);
//! - when cancelled nodes dominate the queue (`numCancelled * 2 > size`)
//!   they are purged on the spot, leaving the mid-processing once-off as
//!   the only possible leftover (`numCancelled == 0 or 1` after a purge);
//! - `clear` drains the queue, calling `onRelease` for still-pending
//!   timers only when asked, then resets the cancelled bookkeeping;
//! - the heap is the libstdc++ `push_heap`/`pop_heap` pair
//!   (`__adjust_heap` hole-walk + trailing sift-up), so the firing order
//!   of timers expiring in the same tick matches the reference build --
//!   including equal deadlines, where `std::push_heap` never bubbles an
//!   element above an equal parent.
//!
//! Flattening notes:
//! - `ReferenceCount` on nodes becomes `Rc`; the `numTimesRegistered_`
//!   counter (a destructor leak guard) is dropped -- `Rc` gives the same
//!   keep-alive guarantee;
//! - the node's `owner_` back-reference is a `Weak` to the shared queue
//!   state, so `TimerHandle::cancel` reaches `onCancel` exactly like the
//!   reference;
//! - `PriorityQueue` (std heap over `std::vector`) becomes a `Vec` plus
//!   hand-rolled heap primitives; `heapify` is `make_heap`,
//!   `unsafePopBack` is a plain `pop` (only used while destroying the
//!   queue);
//! - `std::partition` in `purgeCancelledNodes` is unstable, but the
//!   `make_heap` afterwards erases the order difference, so the port
//!   retains the survivors in place;
//! - `TimerHandler` methods take `&self` with interior mutability in the
//!   handlers: the reference runs `onRelease` re-entrantly on the handler
//!   whose `handleTimeout` is on the stack (self-cancelling timers), which
//!   a `&mut self` receiver would forbid;
//! - `TimeStamp` is the `TimeQueueT` template parameter; the two reference
//!   instantiations are exported as `TimeQueue` (u32 game ticks) and
//!   `TimeQueue64` (u64 stamps, the dispatcher's queue);
//! - `processStats` is instrumentation-only and not ported, and
//!   `prepareForShutdown` stays real-clock by construction (the injected
//!   clock is only swapped in tests).

use std::cell::RefCell;
use std::fmt;
use std::marker::PhantomData;
use std::ops::{Add, Sub};
use std::rc::{Rc, Weak};

/// The `TimeQueueT<TimeStamp>` template parameter: a stamp type with the
/// arithmetic the queue needs (u32 game ticks / u64 stamps).
pub trait Stamp: Copy + Default + PartialOrd + Add<Output = Self> + Sub<Output = Self> {}

impl Stamp for u32 {}
impl Stamp for u64 {}

/// `TimeQueueNode::State` (STATE_PENDING / STATE_EXECUTING /
/// STATE_CANCELLED).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NodeState {
    /// `STATE_PENDING`: queued for a future delivery.
    Pending,
    /// `STATE_EXECUTING`: the callback is on the stack.
    Executing,
    /// `STATE_CANCELLED`: will never fire again.
    Cancelled,
}

/// `TimeQueueNode` + `TimeQueueNodeT<TimeStamp>` merged: the per-timer
/// state, the handler/user pair, and the `owner_` back-reference.
struct Node<TS> {
    time: TS,
    interval: TS,
    /// The `name` given at `add` ("UnnamedTimer" default), reported by
    /// `getTimerInfo`.
    name: &'static str,
    state: NodeState,
    handler: Option<Rc<dyn TimerHandler<TS>>>,
    /// `pUserData` (`void *`, flattened to an integer token).
    user: usize,
    /// `owner_`: the queue the node belongs to (for `onCancel`).
    owner: Weak<RefCell<Inner<TS>>>,
}

/// `TimeQueueNode::isCancelled`.
fn is_cancelled<TS>(node: &Rc<RefCell<Node<TS>>>) -> bool {
    node.borrow().state == NodeState::Cancelled
}

/// The queue's shared state (`TimeQueueT` members). Held by an `Rc` so a
/// `TimerHandle` can reach the `onCancel` bookkeeping.
struct Inner<TS> {
    /// The `PriorityQueue`: a `Vec` with the libstdc++ heap invariant.
    queue: Vec<Rc<RefCell<Node<TS>>>>,
    /// Signed: `clear` momentarily drives it negative (it pre-decrements
    /// to offset `cancel`'s increment), exactly like the reference's
    /// `int numCancelled_`.
    num_cancelled: isize,
    /// `pProcessingNode_`: the node whose callback is on the stack.
    processing_node: Option<Rc<RefCell<Node<TS>>>>,
    last_process_time: TS,
    max_lateness: TS,
    num_late_timers: usize,
}

/// `TimerHandle`: a reference to a queued timer node; the empty handle is
/// the reference's default-constructed `TimerHandle()`.
#[derive(Clone, Default)]
pub struct TimerHandle<TS>(Option<Rc<RefCell<Node<TS>>>>);

impl<TS> PartialEq for TimerHandle<TS> {
    /// `operator==( TimerHandle, TimerHandle )`: same node pointer.
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl<TS> fmt::Debug for TimerHandle<TS> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            None => write!(formatter, "TimerHandle(unset)"),
            Some(_) => write!(formatter, "TimerHandle(set)"),
        }
    }
}

impl<TS: Stamp> TimerHandle<TS> {
    /// `isSet`.
    pub fn is_set(&self) -> bool {
        self.0.is_some()
    }

    /// `cancel()`: cancels the timer if the handle is set. The reference
    /// clears `pNode_` *before* touching the node, so the handle is unset
    /// as soon as the call is made and a second cancel is inert.
    pub fn cancel(&mut self) {
        if let Some(node) = self.0.take() {
            cancel_node(&node, true);
        }
    }

    /// `clearWithoutCancel`: detach the handle without cancelling.
    pub fn clear_without_cancel(&mut self) {
        self.0 = None;
    }
}

/// `TimerHandler`: receives timeouts. The `numTimesRegistered_` counter
/// and its destructor assert are a C++ leak guard flattened away -- `Rc`
/// keeps the handler alive while a timer references it. `onRelease` runs
/// when the timer is cancelled (including the once-off auto-cancel),
/// exactly like the reference's private `release` path.
pub trait TimerHandler<TS> {
    /// `handleTimeout`: the timer expired.
    fn handle_timeout(&self, handle: TimerHandle<TS>, user: usize);

    /// `onRelease`: the timer was cancelled and will never fire again.
    fn on_release(&self, _handle: TimerHandle<TS>, _user: usize) {}
}

/// Any closure as a `TimerHandler`.
pub struct TimerHandlerFn<TS, F: Fn(TimerHandle<TS>, usize)> {
    callback: F,
    /// The `name` argument (profiler labels in the reference).
    pub name: &'static str,
    _stamp: PhantomData<TS>,
}

impl<TS, F: Fn(TimerHandle<TS>, usize)> TimerHandlerFn<TS, F> {
    pub fn new(callback: F, name: &'static str) -> Self {
        Self {
            callback,
            name,
            _stamp: PhantomData,
        }
    }
}

impl<TS, F: Fn(TimerHandle<TS>, usize)> TimerHandler<TS> for TimerHandlerFn<TS, F> {
    fn handle_timeout(&self, handle: TimerHandle<TS>, user: usize) {
        (self.callback)(handle, user)
    }
}

/// `TimeQueueT<TimeStamp>`.
pub struct TimeQueueT<TS> {
    inner: Rc<RefCell<Inner<TS>>>,
}

/// `typedef TimeQueueT< uint32 > TimeQueue;` (game ticks).
pub type TimeQueue = TimeQueueT<u32>;
/// `typedef TimeQueueT< uint64 > TimeQueue64;` (stamps; the dispatcher's
/// queue).
pub type TimeQueue64 = TimeQueueT<u64>;

// ---------------------------------------------------------------------------
// Section: libstdc++ heap primitives
//
// The reference's same-tick firing order is the libstdc++ heap order, so
// the port implements `push_heap`/`pop_heap`/`make_heap` exactly (the
// hole-based `__adjust_heap` with the trailing sift-up) instead of relying
// on `std::collections::BinaryHeap`, whose tie behaviour differs.
// ---------------------------------------------------------------------------

/// The comparison time of heap slot `index`.
fn time_at<TS: Stamp>(queue: &[Rc<RefCell<Node<TS>>>], index: usize) -> TS {
    queue[index].borrow().time
}

/// `__push_heap( first, topIndex, holeIndex, value )`: sift the value up
/// while its parent compares greater. Equal keys never bubble: a newly
/// pushed equal-deadline timer stays below its ancestors.
fn push_heap_value<TS: Stamp>(
    queue: &mut [Rc<RefCell<Node<TS>>>],
    mut hole: usize,
    top: usize,
    value: Rc<RefCell<Node<TS>>>,
) {
    let value_time = value.borrow().time;
    while hole > top {
        let parent = (hole - 1) / 2;
        if time_at(queue, parent) > value_time {
            queue[hole] = Rc::clone(&queue[parent]);
            hole = parent;
        } else {
            break;
        }
    }
    queue[hole] = value;
}

/// `std::push_heap`: sift the freshly pushed last element up.
fn push_heap_last<TS: Stamp>(queue: &mut [Rc<RefCell<Node<TS>>>]) {
    let hole = queue.len() - 1;
    let value = Rc::clone(&queue[hole]);
    push_heap_value(queue, hole, 0, value);
}

/// `__adjust_heap( first, holeIndex, len, value )`: walk the hole down to
/// a leaf picking the greater child (ties prefer the left child), then
/// sift `value` up. When `len` is even and the hole lands on the last
/// single-child level, libstdc++ moves the hole past *both* the leaf and
/// its missing sibling before the sift-up.
fn adjust_heap<TS: Stamp>(
    queue: &mut [Rc<RefCell<Node<TS>>>],
    hole: usize,
    len: usize,
    value: Rc<RefCell<Node<TS>>>,
) {
    let top = hole;
    let mut hole = hole;
    let len = len as isize;
    let mut second_child = hole as isize;
    while second_child < (len - 1) / 2 {
        second_child = 2 * (second_child + 1);
        if time_at(queue, second_child as usize) > time_at(queue, second_child as usize - 1) {
            second_child -= 1;
        }
        queue[hole] = Rc::clone(&queue[second_child as usize]);
        hole = second_child as usize;
    }
    if len & 1 == 0 && second_child == (len - 2) / 2 {
        second_child = 2 * (second_child + 1);
        queue[hole] = Rc::clone(&queue[(second_child - 1) as usize]);
        hole = (second_child - 1) as usize;
    }
    push_heap_value(queue, hole, top, value);
}

/// `std::pop_heap`: swap the front with the last element, re-heapify the
/// shortened range, then hand back the old front (now at the end).
fn pop_heap_front<TS: Stamp>(queue: &mut Vec<Rc<RefCell<Node<TS>>>>) -> Rc<RefCell<Node<TS>>> {
    let len = queue.len();
    let value = Rc::clone(&queue[len - 1]);
    queue[len - 1] = Rc::clone(&queue[0]);
    adjust_heap(queue, 0, len - 1, value);
    queue.pop().expect("pop_heap on a non-empty queue")
}

/// `std::make_heap` (the reference's `PriorityQueue::heapify`): adjust
/// every non-leaf, from the last parent to the root.
fn make_heap<TS: Stamp>(queue: &mut [Rc<RefCell<Node<TS>>>]) {
    let len = queue.len();
    if len < 2 {
        return;
    }
    let mut parent = (len - 2) / 2;
    loop {
        let value = Rc::clone(&queue[parent]);
        adjust_heap(queue, parent, len, value);
        if parent == 0 {
            return;
        }
        parent -= 1;
    }
}

// ---------------------------------------------------------------------------
// Section: the queue
// ---------------------------------------------------------------------------

impl<TS> fmt::Debug for TimeQueueT<TS> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("TimeQueueT").finish_non_exhaustive()
    }
}

impl<TS: Stamp> Default for TimeQueueT<TS> {
    fn default() -> Self {
        Self::new()
    }
}

impl<TS: Stamp> TimeQueueT<TS> {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(Inner {
                queue: Vec::new(),
                num_cancelled: 0,
                processing_node: None,
                last_process_time: TS::default(),
                max_lateness: TS::default(),
                num_late_timers: 0,
            })),
        }
    }

    /// `TimeQueueT::add`: queue the timer and return its handle.
    pub fn add(
        &self,
        start_time: TS,
        interval: TS,
        handler: Rc<dyn TimerHandler<TS>>,
        user: usize,
        name: &'static str,
    ) -> TimerHandle<TS> {
        let node = Rc::new(RefCell::new(Node {
            time: start_time,
            interval,
            name,
            state: NodeState::Pending,
            handler: Some(Rc::clone(&handler)),
            user,
            owner: Rc::downgrade(&self.inner),
        }));
        let mut inner = self.inner.borrow_mut();
        inner.queue.push(Rc::clone(&node));
        push_heap_last(&mut inner.queue);
        // The reference counts the registration on the handler here; the
        // Rc provides the same keep-alive guarantee.
        TimerHandle(Some(node))
    }

    /// `TimeQueueT::process( now )`: fire every due (or cancelled) timer
    /// and return the number fired. Callbacks may cancel, add and re-order
    /// timers -- every iteration re-reads the queue state, so a timer
    /// added mid-callback with a past deadline fires in the same pass.
    pub fn process(&self, now: TS) -> usize {
        let mut num_fired = 0usize;
        loop {
            // The front is due when its time has passed (or it was already
            // cancelled and just needs dropping).
            let due = {
                let inner = self.inner.borrow();
                match inner.queue.first() {
                    None => false,
                    Some(front) => {
                        let front = front.borrow();
                        front.time <= now || front.state == NodeState::Cancelled
                    }
                }
            };
            if !due {
                break;
            }

            // Pop the front and do the pre-callback bookkeeping in one
            // borrow window: nothing between these mutations can run
            // user code, and the callback that follows needs `inner`
            // unborrowed so it may re-enter the queue.
            let node = {
                let mut inner = self.inner.borrow_mut();
                let node = pop_heap_front(&mut inner.queue);
                if !is_cancelled(&node) {
                    num_fired += 1;
                    // The loop condition guarantees now >= time, so the
                    // subtraction never wraps.
                    let lateness = now - node.borrow().time;
                    if lateness > inner.max_lateness {
                        inner.max_lateness = lateness;
                    }
                    if lateness > TS::default() {
                        inner.num_late_timers += 1;
                    }
                }
                inner.processing_node = Some(Rc::clone(&node));
                node
            };

            if !is_cancelled(&node) {
                trigger_timer(&node);
            }

            // The second window: reschedule the live node (or settle
            // the cancelled count) and clear the processing slot
            // together.
            {
                let mut inner = self.inner.borrow_mut();
                if is_cancelled(&node) {
                    // The callback (or an earlier outside cancel) killed it.
                    assert!(
                        inner.num_cancelled > 0,
                        "cancelled node must still be counted"
                    );
                    inner.num_cancelled -= 1;
                } else {
                    // Still alive: schedule the next delivery.
                    inner.queue.push(Rc::clone(&node));
                    push_heap_last(&mut inner.queue);
                }
                inner.processing_node = None;
            }
        }
        let mut inner = self.inner.borrow_mut();
        inner.last_process_time = now;
        num_fired
    }

    /// `TimeQueueT::clear( shouldCallOnRelease )`: drain the queue. The
    /// `maxLoopCount` guard bounds how many *pending* nodes are cancelled
    /// properly; the count is pre-decremented before `cancel` so the
    /// `onCancel` increment nets out and the purge assert stays satisfied.
    pub fn clear(&self, should_call_on_release: bool) {
        let mut max_loop_count = self.inner.borrow().queue.len();
        loop {
            let node = {
                let mut inner = self.inner.borrow_mut();
                if inner.queue.is_empty() {
                    break;
                }
                // `unsafePopBack`: the queue is being destroyed, heap
                // order no longer matters.
                inner.queue.pop().expect("queue not empty")
            };
            if !is_cancelled(&node) && max_loop_count > 0 {
                max_loop_count -= 1;
                {
                    let mut inner = self.inner.borrow_mut();
                    inner.num_cancelled -= 1;
                }
                cancel_node(&node, should_call_on_release);
            } else if is_cancelled(&node) {
                let mut inner = self.inner.borrow_mut();
                inner.num_cancelled -= 1;
            }
            // The node's Rc drops here (`decRef`).
        }
    }

    /// `TimeQueueT::legal`: a handle is legal while it is the node
    /// currently being processed or still sits in the queue.
    pub fn legal(&self, handle: &TimerHandle<TS>) -> bool {
        let Some(node) = &handle.0 else {
            return false;
        };
        let inner = self.inner.borrow();
        if let Some(processing) = &inner.processing_node
            && Rc::ptr_eq(processing, node)
        {
            return true;
        }
        inner.queue.iter().any(|queued| Rc::ptr_eq(queued, node))
    }

    /// `TimeQueueT::nextExp( now )`: stamps until the next expiry, 0 when
    /// the queue is empty or the front is already overdue.
    pub fn next_exp(&self, now: TS) -> TS {
        let inner = self.inner.borrow();
        match inner.queue.first() {
            None => TS::default(),
            Some(front) => {
                let front_time = front.borrow().time;
                if now > front_time {
                    TS::default()
                } else {
                    front_time - now
                }
            }
        }
    }

    /// `size`.
    pub fn size(&self) -> usize {
        self.inner.borrow().queue.len()
    }

    /// `empty`.
    pub fn is_empty(&self) -> bool {
        self.inner.borrow().queue.is_empty()
    }

    /// `maxLateness`.
    pub fn max_lateness(&self) -> TS {
        self.inner.borrow().max_lateness
    }

    /// `numLateTimers`.
    pub fn num_late_timers(&self) -> usize {
        self.inner.borrow().num_late_timers
    }

    /// `resetLateness`.
    pub fn reset_lateness(&self) {
        let mut inner = self.inner.borrow_mut();
        inner.max_lateness = TS::default();
        inner.num_late_timers = 0;
    }

    /// `getLastProcessTime`.
    pub fn last_process_time(&self) -> TS {
        self.inner.borrow().last_process_time
    }

    /// `adjustBy`: shift every delivery by `delta` (a uniform shift keeps
    /// the heap order, so no heapify is needed).
    pub fn adjust_by(&self, delta: TS) {
        let inner = self.inner.borrow();
        for node in &inner.queue {
            let mut node = node.borrow_mut();
            node.time = node.time + delta;
        }
    }

    /// `TimeQueueT::timerDeliveryTime`: when the timer next fires. A timer
    /// whose callback is on the stack has not had its time advanced yet,
    /// so its next delivery is one interval ahead.
    pub fn timer_delivery_time(&self, handle: &TimerHandle<TS>) -> TS {
        let node = handle.0.as_ref().expect("delivery time of a set timer");
        let node = node.borrow();
        if node.state == NodeState::Executing {
            node.time + node.interval
        } else {
            node.time
        }
    }

    /// `TimeQueueT::timerIntervalTime`: the timer's repeat interval.
    pub fn timer_interval_time(&self, handle: &TimerHandle<TS>) -> TS {
        let node = handle.0.as_ref().expect("interval time of a set timer");
        node.borrow().interval
    }

    /// The mutable `timerIntervalTime` overload (`Node::intervalRef`):
    /// change the timer's repeat interval in place. The reference hands
    /// out `TIME_STAMP &` into the node; here the same observable effect
    /// is a setter. A timer's own callback may do this -- the new
    /// interval takes effect on the post-callback `time += interval`
    /// reschedule (the TimeKeeper tick-speedup primitive).
    pub fn set_timer_interval_time(&self, handle: &TimerHandle<TS>, interval: TS) {
        let node = handle.0.as_ref().expect("interval time of a set timer");
        node.borrow_mut().interval = interval;
    }

    /// `TimeQueueT::getTimerInfo`: the time, interval and name of a
    /// timer; nothing for an unset or a cancelled handle
    /// (`if (!pNode->isCancelled())` in the reference).
    pub fn get_timer_info(&self, handle: &TimerHandle<TS>) -> Option<(TS, TS, &'static str)> {
        let node = handle.0.as_ref()?;
        let node = node.borrow();
        if node.state == NodeState::Cancelled {
            return None;
        }
        Some((node.time, node.interval, node.name))
    }
}

/// `TimeQueueNodeT::triggerTimer`: run the callback, auto-cancel once-offs
/// (`interval == 0`), and schedule the next delivery of survivors.
fn trigger_timer<TS: Stamp>(node: &Rc<RefCell<Node<TS>>>) {
    let (handler, user) = {
        let mut node = node.borrow_mut();
        if node.state == NodeState::Cancelled {
            return;
        }
        node.state = NodeState::Executing;
        (node.handler.clone(), node.user)
    };
    let handle = TimerHandle(Some(Rc::clone(node)));
    // The reference runs this with the node in STATE_EXECUTING so a
    // self-cancel is recognised; the handler may freely cancel itself,
    // other timers, or add new ones.
    handler
        .expect("a pending node always has a handler")
        .handle_timeout(handle, user);

    let interval = node.borrow().interval;
    if interval == TS::default() && !is_cancelled(node) {
        // Once-off: cancel instead of rescheduling.
        cancel_node(node, true);
    }
    if !is_cancelled(node) {
        let mut node = node.borrow_mut();
        node.time = node.time + interval;
        node.state = NodeState::Pending;
    }
}

/// `TimeQueueNode::cancel`: mark cancelled, call `onRelease` when asked,
/// then notify the owning queue (`onCancel`), which purges when the
/// cancelled nodes dominate the queue.
fn cancel_node<TS: Stamp>(node: &Rc<RefCell<Node<TS>>>, should_call_on_release: bool) {
    if is_cancelled(node) {
        return;
    }
    {
        let mut guard = node.borrow_mut();
        // MF_ASSERT: only a pending or executing node can be cancelled.
        assert!(
            guard.state == NodeState::Pending || guard.state == NodeState::Executing,
            "cancelling a node in state {:?}",
            guard.state
        );
        guard.state = NodeState::Cancelled;
        let handler = guard.handler.take();
        let user = guard.user;
        drop(guard);
        if should_call_on_release && let Some(handler) = handler {
            handler.on_release(TimerHandle(Some(Rc::clone(node))), user);
        }
    }
    // `owner_.onCancel()`.
    let owner = node.borrow().owner.upgrade();
    if let Some(owner) = owner {
        let mut inner = owner.borrow_mut();
        inner.num_cancelled += 1;
        // More than half cancelled: flush them from the queue immediately.
        if inner.num_cancelled * 2 > inner.queue.len() as isize {
            purge_cancelled_nodes(&mut inner);
        }
    }
}

/// `TimeQueueT::purgeCancelledNodes`: drop the cancelled nodes when they
/// dominate the queue. Afterwards the count is 0 -- or 1, when the node
/// currently being processed cancelled itself mid-pass (it is not in the
/// queue any more, so the sweep cannot see it).
fn purge_cancelled_nodes<TS: Stamp>(inner: &mut Inner<TS>) {
    let num_purged = inner.queue.iter().filter(|node| is_cancelled(node)).count() as isize;
    // std::partition + erase; the heapify below erases the ordering
    // difference between partition and retain.
    inner.queue.retain(|node| !is_cancelled(node));
    inner.num_cancelled -= num_purged;
    assert!(
        inner.num_cancelled == 0 || inner.num_cancelled == 1,
        "numCancelled must be 0 or 1 after a purge, got {}",
        inner.num_cancelled
    );
    make_heap(&mut inner.queue);
}

// ---------------------------------------------------------------------------
// Section: dispatcher surface
// ---------------------------------------------------------------------------

/// `Mercury::EventDispatcher::ClockFn`: the stamp source driving timer
/// processing. `realClock` reads `timestamp()`; tests inject a pinned
/// clock.
pub type ClockFn = Box<dyn Fn() -> u64>;

/// `EventDispatcher::realClock`.
pub fn real_clock() -> u64 {
    crate::time::stamp_now()
}

/// `TimeStamp::fromSeconds` for the `addTimerCommon` conversion:
/// microseconds to stamps.
fn stamps_from_microseconds(microseconds: i64, stamps_per_second: u64) -> u64 {
    ((microseconds as f64) / 1_000_000.0 * (stamps_per_second as f64)) as u64
}

/// The `EventDispatcher` timer surface: the dispatcher owns a
/// `TimeQueue64` and a `ClockFn`, exactly like the reference
/// (`pTimeQueue_`, `clock_`, `numTimerCalls_`).
pub struct DispatcherTimers {
    /// `pTimeQueue_` (always a `TimeQueue64`).
    pub time_queue: TimeQueue64,
    /// `clock_` (injectable in this tree for virtual-clock tests).
    clock: ClockFn,
    /// `stampsPerSecond()` for the microseconds conversion; nanosecond
    /// stamps match `stamp_now`.
    stamps_per_second: u64,
    /// `numTimerCalls_`.
    num_timer_calls: usize,
}

impl DispatcherTimers {
    /// The constructor pair: `EventDispatcher()` uses `realClock`, the
    /// `ClockFn` overload injects a clock for tests.
    pub fn new() -> Self {
        Self::with_clock(Box::new(real_clock))
    }

    pub fn with_clock(clock: ClockFn) -> Self {
        Self {
            time_queue: TimeQueue64::new(),
            clock,
            stamps_per_second: 1_000_000_000,
            num_timer_calls: 0,
        }
    }

    /// `EventDispatcher::addTimer( microseconds, handler, arg, name )`.
    pub fn add_timer(
        &mut self,
        microseconds: i64,
        handler: Rc<dyn TimerHandler<u64>>,
        user: usize,
        name: &'static str,
    ) -> TimerHandle<u64> {
        self.add_timer_common(microseconds, handler, user, true, name)
    }

    /// `EventDispatcher::addOnceOffTimer`.
    pub fn add_once_off_timer(
        &mut self,
        microseconds: i64,
        handler: Rc<dyn TimerHandler<u64>>,
        user: usize,
        name: &'static str,
    ) -> TimerHandle<u64> {
        self.add_timer_common(microseconds, handler, user, false, name)
    }

    /// `EventDispatcher::addTimerCommon`: non-positive delays return an
    /// unset handle; recurrent timers repeat on their interval, once-offs
    /// carry 0.
    fn add_timer_common(
        &mut self,
        microseconds: i64,
        handler: Rc<dyn TimerHandler<u64>>,
        user: usize,
        recurrent: bool,
        name: &'static str,
    ) -> TimerHandle<u64> {
        // MF_ASSERT( handler ) is unrepresentable: the Rc is never null.
        if microseconds <= 0 {
            return TimerHandle::default();
        }
        let interval = stamps_from_microseconds(microseconds, self.stamps_per_second);
        self.time_queue.add(
            (self.clock)() + interval,
            if recurrent { interval } else { 0 },
            handler,
            user,
            name,
        )
    }

    /// `EventDispatcher::processTimers`: run the queue on the injected
    /// clock and accumulate `numTimerCalls_`. Returns the number fired.
    pub fn process_timers(&mut self) -> usize {
        let num_fired = self.time_queue.process((self.clock)());
        self.num_timer_calls += num_fired;
        num_fired
    }

    /// `numTimerCalls`.
    pub fn num_timer_calls(&self) -> usize {
        self.num_timer_calls
    }

    /// `stampsPerSecondD()`: the nanosecond-stamp conversion factor the
    /// dispatcher's `calculateWait` divides by.
    pub fn stamps_per_second(&self) -> u64 {
        self.stamps_per_second
    }

    /// `nextExp( clock_() ) / stampsPerSecondD()`: the stamp of the
    /// earliest pending timer, in seconds, measured against the clock the
    /// timers were built with (absent when the queue is empty).
    pub fn next_exp_seconds(&self) -> f64 {
        self.time_queue.next_exp((self.clock)()) as f64 / self.stamps_per_second as f64
    }

    /// `EventDispatcher::timerDeliveryTime`.
    pub fn timer_delivery_time(&self, handle: &TimerHandle<u64>) -> u64 {
        self.time_queue.timer_delivery_time(handle)
    }

    /// `EventDispatcher::timerIntervalTime`.
    pub fn timer_interval_time(&self, handle: &TimerHandle<u64>) -> u64 {
        self.time_queue.timer_interval_time(handle)
    }

    /// The mutable `EventDispatcher::timerIntervalTime` overload.
    pub fn set_timer_interval_time(&self, handle: &TimerHandle<u64>, interval: u64) {
        self.time_queue.set_timer_interval_time(handle, interval);
    }

    /// `EventDispatcher::maxTimerLateness`: the worst observed delivery
    /// delay -- the event-loop stall signal.
    pub fn max_timer_lateness(&self) -> u64 {
        self.time_queue.max_lateness()
    }

    /// `EventDispatcher::numLateTimers`.
    pub fn num_late_timers(&self) -> usize {
        self.time_queue.num_late_timers()
    }

    /// `EventDispatcher::resetTimerLateness`.
    pub fn reset_timer_lateness(&mut self) {
        self.time_queue.reset_lateness();
    }
}

impl Default for DispatcherTimers {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A recording handler: pushes "fired"/"released" into the shared log.
    struct Recorder {
        log: Rc<RefCell<Vec<&'static str>>>,
    }

    impl TimerHandler<u64> for Recorder {
        fn handle_timeout(&self, _handle: TimerHandle<u64>, _user: usize) {
            self.log.borrow_mut().push("fired");
        }

        fn on_release(&self, _handle: TimerHandle<u64>, _user: usize) {
            self.log.borrow_mut().push("released");
        }
    }

    /// The recorder fixture pair: its handler and the shared log.
    type RecorderParts = (Rc<dyn TimerHandler<u64>>, Rc<RefCell<Vec<&'static str>>>);

    fn recorder() -> RecorderParts {
        let log = Rc::new(RefCell::new(Vec::new()));
        (
            Rc::new(Recorder {
                log: Rc::clone(&log),
            }),
            log,
        )
    }

    /// A named once-off at `time` recording its `name`.
    fn once_off(
        queue: &TimeQueue64,
        time: u64,
        name: &'static str,
        log: &Rc<RefCell<Vec<&'static str>>>,
    ) -> TimerHandle<u64> {
        let log = Rc::clone(log);
        queue.add(
            time,
            0,
            Rc::new(TimerHandlerFn::new(
                move |_, _| log.borrow_mut().push(name),
                name,
            )),
            0,
            name,
        )
    }

    /// The acceptance case: four timers expiring in the same tick fire in
    /// the reference build's order. The order falls out of the libstdc++
    /// heap (equal-deadline pushes never bubble past an equal parent;
    /// `pop_heap` swaps the front behind the sifted value), verified
    /// against a stock libstdc++ `std::push_heap`/`std::pop_heap` probe:
    /// four equal timers A, B, C, D pop as A, C, B, D. This pins the port
    /// to the libstdc++ heap rather than to Rust's `BinaryHeap` tie
    /// behaviour. The same probe gives mixed deadlines W(10) X(5) Y(7)
    /// Z(10) V(10) popping as X, Y, V, W, Z.
    #[test]
    fn same_tick_timers_fire_in_the_reference_heap_order() {
        let queue = TimeQueue64::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut handles = Vec::new();
        for name in ["A", "B", "C", "D"] {
            handles.push(once_off(&queue, 100, name, &log));
        }

        assert_eq!(queue.process(100), 4);
        assert_eq!(*log.borrow(), ["A", "C", "B", "D"]);
        assert!(queue.is_empty());
        for handle in &handles {
            assert!(!queue.legal(handle));
        }

        // The mixed-deadline probe sequence, as part of the same pin.
        let queue = TimeQueue64::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        once_off(&queue, 10, "W", &log);
        once_off(&queue, 5, "X", &log);
        once_off(&queue, 7, "Y", &log);
        once_off(&queue, 10, "Z", &log);
        once_off(&queue, 10, "V", &log);
        assert_eq!(queue.process(10), 5);
        assert_eq!(*log.borrow(), ["X", "Y", "V", "W", "Z"]);
    }

    /// Distinct deadlines fire earliest-first inside one pass, and a
    /// recurrent timer that comes due again inside the same pass fires
    /// again (the loop re-checks the re-pushed node against `now`).
    #[test]
    fn distinct_deadlines_fire_in_order_and_recurrent_timers_repeat_within_a_pass() {
        let queue = TimeQueue64::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        once_off(&queue, 10, "late", &log);
        once_off(&queue, 5, "early", &log);
        once_off(&queue, 7, "middle", &log);

        assert_eq!(queue.process(10), 3);
        assert_eq!(*log.borrow(), ["early", "middle", "late"]);

        // A recurrent timer due at 0 with interval 3 fires at 0, 3, 6, 9
        // within a single process( 9 ).
        let hits = Rc::new(Cell::new(0u64));
        let hits_for_handler = Rc::clone(&hits);
        queue.add(
            0,
            3,
            Rc::new(TimerHandlerFn::new(
                move |_, _| hits_for_handler.set(hits_for_handler.get() + 1),
                "recurrent",
            )),
            0,
            "recurrent",
        );
        assert_eq!(queue.process(9), 4);
        assert_eq!(hits.get(), 4);
    }

    /// A once-off fires exactly once: the `interval == 0` auto-cancel
    /// releases the handler, drops the node and leaves the queue empty.
    #[test]
    fn a_once_off_fires_exactly_once() {
        let queue = TimeQueue64::new();
        let (handler, log) = recorder();
        let handle = queue.add(50, 0, handler, 7, "once");
        assert!(queue.legal(&handle));

        assert_eq!(queue.process(50), 1);
        assert_eq!(*log.borrow(), ["fired", "released"]);
        assert_eq!(queue.size(), 0);
        assert!(!queue.legal(&handle));

        // Nothing left to fire.
        assert_eq!(queue.process(1000), 0);
    }

    /// A recurrent timer keeps firing on its interval until cancelled,
    /// and its delivery time advances by the interval after each fire.
    #[test]
    fn a_recurrent_timer_repeats_until_cancelled() {
        let queue = TimeQueue64::new();
        let (handler, log) = recorder();
        let mut handle = queue.add(0, 10, handler, 0, "tick");
        assert_eq!(queue.timer_interval_time(&handle), 10);

        assert_eq!(queue.process(25), 3);
        assert_eq!(queue.timer_delivery_time(&handle), 30);

        handle.cancel();
        assert_eq!(*log.borrow(), ["fired", "fired", "fired", "released"]);
        assert_eq!(queue.process(100), 0);
    }

    /// Cancelling from inside the callback is legal: the node is already
    /// popped, `legal` still accepts it while it is being processed, and
    /// the re-check after `triggerTimer` sees CANCELLED -- it fires exactly
    /// once.
    #[test]
    fn a_timer_that_cancels_itself_inside_the_callback_fires_once() {
        struct SelfCanceller {
            log: Rc<RefCell<Vec<&'static str>>>,
            queue: Rc<RefCell<TimeQueue64>>,
        }
        impl TimerHandler<u64> for SelfCanceller {
            fn handle_timeout(&self, handle: TimerHandle<u64>, _user: usize) {
                self.log.borrow_mut().push("fired");
                assert!(self.queue.borrow().legal(&handle));
                let mut handle = handle;
                handle.cancel();
            }
        }
        let log = Rc::new(RefCell::new(Vec::new()));
        let queue = Rc::new(RefCell::new(TimeQueue64::new()));
        let handler = Rc::new(SelfCanceller {
            log: Rc::clone(&log),
            queue: Rc::clone(&queue),
        });
        let handle: TimerHandle<u64> = queue.borrow().add(
            0,
            5,
            Rc::clone(&handler) as Rc<dyn TimerHandler<u64>>,
            0,
            "self",
        );

        assert_eq!(queue.borrow().process(100), 1);
        assert_eq!(*log.borrow(), ["fired"]);
        assert!(!queue.borrow().legal(&handle));
    }

    /// A timer cancelled from outside while pending is dropped without
    /// firing when it reaches the front -- and a cancelled node that is
    /// not the front waits until it is due.
    #[test]
    fn an_outside_cancelled_timer_is_skipped_when_due() {
        let queue = TimeQueue64::new();
        let (handler, log) = recorder();
        let early = queue.add(5, 0, handler, 0, "early");
        let (doomed_handler, doomed_log) = recorder();
        let mut doomed = queue.add(10, 0, doomed_handler, 0, "doomed");

        doomed.cancel();
        assert_eq!(*doomed_log.borrow(), ["released"]);

        // At 5 only the early timer is due; the cancelled one waits. The
        // early timer is a once-off, so it also releases itself.
        assert_eq!(queue.process(5), 1);
        assert_eq!(*log.borrow(), ["fired", "released"]);

        // At 10 the cancelled node is due: dropped, not fired.
        assert_eq!(queue.process(10), 0);
        assert_eq!(*doomed_log.borrow(), ["released"]);
        assert!(!queue.legal(&early));
        assert!(!queue.legal(&doomed));
    }

    /// A cancelled front node is dropped without firing at all.
    #[test]
    fn a_cancelled_front_node_never_fires() {
        let queue = TimeQueue64::new();
        let (handler, log) = recorder();
        let mut handle = queue.add(10, 0, handler, 0, "doomed");
        handle.cancel();

        assert_eq!(queue.process(10), 0);
        assert_eq!(*log.borrow(), ["released"]);
        assert!(queue.is_empty());
    }

    /// A timer added from inside a callback is processed in the same pass
    /// when its deadline has already passed; one added for the future
    /// waits for the next pass.
    #[test]
    fn timers_added_inside_a_callback_respect_the_queue_order() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let queue = Rc::new(RefCell::new(TimeQueue64::new()));

        let adder_log = Rc::clone(&log);
        let adder_queue = Rc::clone(&queue);
        queue.borrow().add(
            5,
            0,
            Rc::new(TimerHandlerFn::new(
                move |_, _| {
                    adder_log.borrow_mut().push("adder");
                    let same_pass_log = Rc::clone(&adder_log);
                    // Due immediately: fires later in this same pass.
                    adder_queue.borrow().add(
                        5,
                        0,
                        Rc::new(TimerHandlerFn::new(
                            move |_, _| same_pass_log.borrow_mut().push("same-pass"),
                            "same-pass",
                        )),
                        0,
                        "same-pass",
                    );
                    let next_pass_queue = Rc::clone(&adder_queue);
                    let next_pass_log = Rc::clone(&adder_log);
                    // In the future: waits for the next pass.
                    next_pass_queue.borrow().add(
                        500,
                        0,
                        Rc::new(TimerHandlerFn::new(
                            move |_, _| next_pass_log.borrow_mut().push("next-pass"),
                            "next-pass",
                        )),
                        0,
                        "next-pass",
                    );
                },
                "adder",
            )),
            0,
            "adder",
        );

        assert_eq!(queue.borrow().process(5), 2);
        assert_eq!(*log.borrow(), ["adder", "same-pass"]);
        assert_eq!(queue.borrow().process(500), 1);
        assert_eq!(*log.borrow(), ["adder", "same-pass", "next-pass"]);
    }

    /// Lateness is measured against the delivery time: overdue timers
    /// count and update the maximum, on-time ones do not.
    #[test]
    fn lateness_tracks_overdue_deliveries() {
        let queue = TimeQueue64::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        once_off(&queue, 10, "t10", &log);
        once_off(&queue, 20, "t20", &log);
        once_off(&queue, 5, "t5", &log);
        once_off(&queue, 30, "t30", &log);

        assert_eq!(queue.process(30), 4);
        // Delivery times 10, 20, 5 are late (by 15, 10, 25); 30 is on time.
        assert_eq!(queue.max_lateness(), 25);
        assert_eq!(queue.num_late_timers(), 3);

        queue.reset_lateness();
        assert_eq!(queue.max_lateness(), 0);
        assert_eq!(queue.num_late_timers(), 0);
    }

    /// When cancelled nodes dominate the queue (`numCancelled * 2 >
    /// size`) they are purged on the spot; the surviving timers keep the
    /// right order.
    #[test]
    fn dominating_cancels_purge_the_queue() {
        let queue = TimeQueue64::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut handles = Vec::new();
        for (time, name) in [
            (1u64, "a"),
            (2, "b"),
            (3, "c"),
            (4, "d"),
            (5, "e"),
            (6, "f"),
        ] {
            handles.push(once_off(&queue, time, name, &log));
        }
        // Cancel four of six: the fourth cancel sees 4 * 2 > 2 and purges.
        for handle in &mut handles[1..5] {
            handle.cancel();
        }
        assert_eq!(queue.size(), 2);

        assert_eq!(queue.process(6), 2);
        assert_eq!(*log.borrow(), ["a", "f"]);
    }

    /// `clear( true )` releases every still-pending timer; already
    /// cancelled ones are not released twice. `clear( false )` releases
    /// nothing.
    #[test]
    fn clear_releases_pending_timers_per_the_flag() {
        let queue = TimeQueue64::new();
        let (handler, log) = recorder();
        queue.add(1, 0, Rc::clone(&handler), 0, "pending1");
        queue.add(2, 0, handler, 0, "pending2");
        let (doomed_handler, doomed_log) = recorder();
        let mut doomed = queue.add(3, 0, doomed_handler, 0, "pre-cancelled");
        doomed.cancel();

        queue.clear(true);
        // pending1 and pending2 released at clear; the pre-cancelled one
        // was released at its cancel and not again.
        assert_eq!(*log.borrow(), ["released", "released"]);
        assert_eq!(*doomed_log.borrow(), ["released"]);
        assert!(queue.is_empty());

        // clear( false ) leaves handlers alone.
        let (quiet_handler, quiet_log) = recorder();
        queue.add(4, 0, quiet_handler, 0, "quiet");
        queue.clear(false);
        assert_eq!(*quiet_log.borrow(), Vec::<&'static str>::new());
        assert!(queue.is_empty());
    }

    /// `nextExp` reports the time to the next expiry, 0 when empty or
    /// overdue.
    #[test]
    fn next_exp_reports_time_to_the_next_expiry() {
        let queue = TimeQueue64::new();
        assert_eq!(queue.next_exp(100), 0);

        let log = Rc::new(RefCell::new(Vec::new()));
        once_off(&queue, 150, "soon", &log);
        once_off(&queue, 300, "later", &log);
        assert_eq!(queue.next_exp(100), 50);
        assert_eq!(queue.next_exp(150), 0);
        assert_eq!(queue.next_exp(200), 0);
    }

    /// `adjustBy` shifts every delivery uniformly.
    #[test]
    fn adjust_by_shifts_every_delivery() {
        let queue = TimeQueue64::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        once_off(&queue, 10, "a", &log);
        once_off(&queue, 20, "b", &log);

        queue.adjust_by(100);
        assert_eq!(queue.next_exp(0), 110);
        assert_eq!(queue.process(110), 1);
        assert_eq!(*log.borrow(), ["a"]);
        assert_eq!(queue.process(120), 1);
        assert_eq!(*log.borrow(), ["a", "b"]);
    }

    /// `getTimerInfo` reports time, interval and name; `deliveryTime` of
    /// an executing recurrent timer counts the pending interval.
    #[test]
    fn timer_info_and_delivery_time_match_the_reference() {
        let queue = Rc::new(RefCell::new(TimeQueue64::new()));
        let log = Rc::new(RefCell::new(Vec::new()));

        let log_for_handler = Rc::clone(&log);
        let queue_for_handler = Rc::clone(&queue);
        let handle: TimerHandle<u64> = queue.borrow().add(
            100,
            50,
            Rc::new(TimerHandlerFn::new(
                move |handle: TimerHandle<u64>, _| {
                    log_for_handler.borrow_mut().push("fired");
                    // While the callback is on the stack the node has not
                    // had its time advanced: next delivery = time +
                    // interval.
                    assert_eq!(queue_for_handler.borrow().timer_delivery_time(&handle), 150);
                    assert_eq!(queue_for_handler.borrow().timer_interval_time(&handle), 50);
                },
                "info",
            )),
            9,
            "info",
        );
        assert_eq!(
            queue.borrow().get_timer_info(&handle),
            Some((100, 50, "info"))
        );
        assert_eq!(queue.borrow().timer_delivery_time(&handle), 100);

        assert_eq!(queue.borrow().process(100), 1);
        // After the pass the time has advanced.
        assert_eq!(queue.borrow().timer_delivery_time(&handle), 150);
        assert_eq!(
            queue.borrow().get_timer_info(&handle),
            Some((150, 50, "info"))
        );
        assert_eq!(*log.borrow(), ["fired"]);

        // Unset handles report nothing.
        assert_eq!(
            queue
                .borrow()
                .get_timer_info(&TimerHandle::<u64>::default()),
            None
        );
    }

    /// `lastProcessTime` records the `now` of the last pass.
    #[test]
    fn last_process_time_tracks_the_last_pass() {
        let queue = TimeQueue64::new();
        assert_eq!(queue.last_process_time(), 0);
        let log = Rc::new(RefCell::new(Vec::new()));
        once_off(&queue, 5, "a", &log);
        queue.process(7);
        assert_eq!(queue.last_process_time(), 7);
    }

    /// The u32 instantiation (`TimeQueue`, game ticks) processes the same
    /// way.
    #[test]
    fn the_u32_game_tick_queue_processes_the_same_way() {
        let queue = TimeQueue::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        let log_for_handler = Rc::clone(&log);
        let handle = queue.add(
            10u32,
            5,
            Rc::new(TimerHandlerFn::new(
                move |_, _| log_for_handler.borrow_mut().push("tick"),
                "game-tick",
            )),
            0,
            "game-tick",
        );
        assert_eq!(queue.process(19), 2);
        assert_eq!(*log.borrow(), ["tick", "tick"]);
        assert_eq!(queue.next_exp(20), 0);
        assert_eq!(queue.timer_delivery_time(&handle), 20);
    }

    /// An unset handle cancels as a no-op and never looks legal.
    #[test]
    fn unset_handles_are_inert() {
        let queue = TimeQueue64::new();
        let mut handle = TimerHandle::<u64>::default();
        assert!(!handle.is_set());
        assert!(!queue.legal(&handle));
        handle.cancel(); // no-op
        assert_eq!(queue.size(), 0);
    }

    /// `real_clock` is monotonic and moves forward.
    #[test]
    fn real_clock_returns_increasing_stamps() {
        let first = real_clock();
        let second = real_clock();
        assert!(second >= first);
    }

    /// Non-positive `addTimer` delays produce an unset handle, exactly
    /// like the reference.
    #[test]
    fn non_positive_timer_delays_return_unset_handles() {
        let mut timers = DispatcherTimers::with_clock(Box::new(|| 0));
        let (handler, log) = recorder();
        for delay in [0i64, -5] {
            let handle = timers.add_once_off_timer(delay, Rc::clone(&handler), 0, "unset");
            assert!(!handle.is_set());
            assert_eq!(timers.time_queue.size(), 0);
        }
        timers.process_timers();
        assert_eq!(*log.borrow(), Vec::<&'static str>::new());
        assert_eq!(timers.num_timer_calls(), 0);
    }

    /// The lateness surface: a stalled event loop shows up in
    /// `maxTimerLateness` / `numLateTimers`, resets cleanly, and the
    /// recurrent timer reschedules on its interval.
    #[test]
    fn the_lateness_surface_reports_the_event_loop_stall() {
        let clock = Rc::new(Cell::new(1_000_000_000u64));
        let clock_for_closure = Rc::clone(&clock);
        let mut timers = DispatcherTimers::with_clock(Box::new(move || clock_for_closure.get()));

        let hits = Rc::new(Cell::new(0u64));
        let hits_for_handler = Rc::clone(&hits);
        timers.add_timer(
            100_000,
            Rc::new(TimerHandlerFn::new(
                move |_, _| hits_for_handler.set(hits_for_handler.get() + 1),
                "recurrent",
            )),
            0,
            "recurrent",
        );

        // The timer due at 1.1s only runs when the (virtual) event loop
        // gets back control at 1.5s; it then replays every missed
        // interval within the same pass (1.2s, 1.3s, 1.4s, 1.5s).
        clock.set(1_500_000_000);
        assert_eq!(timers.process_timers(), 5);
        assert_eq!(timers.max_timer_lateness(), 400_000_000);
        assert_eq!(timers.num_late_timers(), 4);

        timers.reset_timer_lateness();
        assert_eq!(timers.max_timer_lateness(), 0);
        assert_eq!(timers.num_late_timers(), 0);
        assert_eq!(timers.num_timer_calls(), 5);

        // The last fire rescheduled onto the interval grid.
        assert_eq!(timers.time_queue.next_exp(1_500_000_000), 100_000_000);
    }

    /// `timerDeliveryTime` / `timerIntervalTime` surface through the
    /// dispatcher.
    #[test]
    fn the_dispatcher_timer_query_surface_matches_the_queue() {
        let clock = Rc::new(Cell::new(1_000_000_000u64));
        let mut timers = DispatcherTimers::with_clock(Box::new(move || clock.get()));

        let (handler, _log) = recorder();
        let handle = timers.add_timer(250_000, handler, 0, "queried");
        assert_eq!(timers.timer_delivery_time(&handle), 1_250_000_000);
        assert_eq!(timers.timer_interval_time(&handle), 250_000_000);
        assert_eq!(
            timers.time_queue.get_timer_info(&handle),
            Some((1_250_000_000, 250_000_000, "queried"))
        );
    }

    /// `DispatcherTimers::default()` matches `new()`.
    #[test]
    fn dispatcher_timers_default_matches_new() {
        let timers = DispatcherTimers::default();
        assert_eq!(timers.num_timer_calls(), 0);
        assert!(timers.time_queue.is_empty());
        assert_eq!(timers.max_timer_lateness(), 0);
    }

    /// `clearWithoutCancel` detaches a handle without touching the timer.
    #[test]
    fn clear_without_cancel_detaches_the_handle() {
        let queue = TimeQueue64::new();
        let (handler, log) = recorder();
        let mut handle = queue.add(10, 0, handler, 0, "kept");
        handle.clear_without_cancel();
        assert!(!handle.is_set());

        assert_eq!(queue.process(10), 1);
        assert_eq!(*log.borrow(), ["fired", "released"]);
    }

    /// Timer handles compare by node identity, like the reference's
    /// `operator==`; their debug form mirrors set state.
    #[test]
    fn timer_handles_compare_by_identity() {
        let queue = TimeQueue64::new();
        let (handler, _log) = recorder();
        let a = queue.add(1, 0, Rc::clone(&handler), 0, "a");
        let b = queue.add(2, 0, handler, 0, "b");

        assert_eq!(a, a.clone());
        assert_ne!(a, b);
        assert_ne!(a, TimerHandle::<u64>::default());
        assert_eq!(TimerHandle::<u64>::default(), TimerHandle::<u64>::default());

        assert_eq!(format!("{a:?}"), "TimerHandle(set)");
        assert_eq!(
            format!("{:?}", TimerHandle::<u64>::default()),
            "TimerHandle(unset)"
        );
    }

    /// `TimeQueueT::default()` matches `new()`.
    #[test]
    fn the_queue_default_matches_new() {
        let queue = TimeQueue64::default();
        assert!(queue.is_empty());
        assert_eq!(queue.size(), 0);
    }

    /// The `__adjust_heap` walk picks the left child when the right one
    /// is not strictly later, and the right child when it is: pushing
    /// 12, 10, 5, 7 builds [5, 7, 10, 12] and the first pop walks the
    /// hole past children 7 and 10 (the right child is strictly later,
    /// so the left one moves up).
    #[test]
    fn the_heap_walk_picks_the_earlier_child() {
        let queue = TimeQueue64::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        once_off(&queue, 12, "n12", &log);
        once_off(&queue, 10, "n10", &log);
        once_off(&queue, 5, "n5", &log);
        once_off(&queue, 7, "n7", &log);

        assert_eq!(queue.process(12), 4);
        assert_eq!(*log.borrow(), ["n5", "n7", "n10", "n12"]);
    }

    /// A purge with more than two survivors reheaps across several
    /// parents (`make_heap` walks every non-leaf). Cancelled nodes stay
    /// in the queue until the purge, so the trip condition counts them:
    /// 6 cancels of 10 give 12 > 10.
    #[test]
    fn a_purge_reheaps_the_survivors() {
        let queue = TimeQueue64::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut handles = Vec::new();
        for (time, name) in [
            (1u64, "a"),
            (2, "b"),
            (3, "c"),
            (4, "d"),
            (5, "e"),
            (6, "f"),
            (7, "g"),
            (8, "h"),
            (9, "i"),
            (10, "j"),
        ] {
            handles.push(once_off(&queue, time, name, &log));
        }
        // Six cancels leave four survivors; the sixth trips the purge
        // (6 * 2 > 10, cancelled nodes still counted in the length) and
        // `make_heap` walks two parent levels.
        for handle in &mut handles[1..7] {
            handle.cancel();
        }
        assert_eq!(queue.size(), 4);

        assert_eq!(queue.process(10), 4);
        assert_eq!(*log.borrow(), ["a", "h", "i", "j"]);
    }

    /// The defensive guards keep the reference's shape: a cancelled node
    /// never triggers, a second cancel is inert because the handle was
    /// unset first (`pNode_ = NULL` before the node is touched), and a
    /// copy taken before the cancel still points at the node but cannot
    /// make it fire.
    #[test]
    fn the_cancelled_guards_never_double_fire() {
        let queue = TimeQueue64::new();
        let (handler, log) = recorder();
        let mut handle = queue.add(5, 0, handler, 0, "guarded");
        let mut probe = handle.clone();

        handle.cancel(); // releases
        assert!(!handle.is_set());
        handle.cancel(); // unset: fully inert
        // triggerTimer's cancelled guard: the node can no longer fire.
        trigger_timer(probe.0.as_ref().expect("the copy keeps the node"));
        // Cancelling through the stale copy: the node's cancelled guard
        // makes it inert, so the handler is never released twice.
        probe.cancel();

        assert_eq!(*log.borrow(), ["released"]);
    }

    /// Cancelling after the queue was dropped is safe: the node still
    /// releases, the (gone) owner bookkeeping is skipped.
    #[test]
    fn cancelling_after_the_queue_is_dropped_is_safe() {
        let (mut handle, log) = {
            let queue = TimeQueue64::new();
            let (handler, log) = recorder();
            let handle = queue.add(1, 0, handler, 0, "late");
            (handle, log)
        };
        handle.cancel();
        assert_eq!(*log.borrow(), ["released"]);
    }

    /// Cancelling from inside another timer's callback: the victim never
    /// fires and its `onRelease` runs synchronously inside the canceller's
    /// callback. The victim was already in the queue, so the purge trips
    /// during processing.
    #[test]
    fn a_callback_can_cancel_another_pending_timer() {
        struct CrossCanceller {
            log: Rc<RefCell<Vec<&'static str>>>,
            victim: RefCell<Option<TimerHandle<u64>>>,
        }
        impl TimerHandler<u64> for CrossCanceller {
            fn handle_timeout(&self, _handle: TimerHandle<u64>, _user: usize) {
                self.log.borrow_mut().push("canceller");
                let mut victim = self.victim.borrow_mut().take().expect("victim handle set");
                victim.cancel();
                self.log.borrow_mut().push("victim-released");
            }
        }
        let log = Rc::new(RefCell::new(Vec::new()));
        let queue = TimeQueue64::new();

        let canceller = Rc::new(CrossCanceller {
            log: Rc::clone(&log),
            victim: RefCell::new(None),
        });
        queue.add(
            5,
            0,
            Rc::clone(&canceller) as Rc<dyn TimerHandler<u64>>,
            0,
            "canceller",
        );
        let (victim_handler, victim_recorder) = recorder();
        let victim = queue.add(10, 0, victim_handler, 0, "victim");
        *canceller.victim.borrow_mut() = Some(victim);

        assert_eq!(queue.process(5), 1);
        assert_eq!(*log.borrow(), ["canceller", "victim-released"]);
        assert_eq!(*victim_recorder.borrow(), ["released"]);

        // The victim's due time passes without it firing.
        assert_eq!(queue.process(10), 0);
        assert_eq!(*victim_recorder.borrow(), ["released"]);
    }

    /// The TimeKeeper re-registration pattern: read `timerDeliveryTime`
    /// before cancelling, then re-add at exactly that time. The old
    /// handle never fires again and the replacement keeps the phase --
    /// re-registered at 15, not at "late now + interval".
    #[test]
    fn a_callback_can_reregister_at_the_original_delivery_time() {
        struct Reregistrar {
            log: Rc<RefCell<Vec<&'static str>>>,
            queue: Rc<RefCell<TimeQueue64>>,
            me: RefCell<Option<Rc<dyn TimerHandler<u64>>>>,
            handle: RefCell<TimerHandle<u64>>,
        }
        impl TimerHandler<u64> for Reregistrar {
            fn handle_timeout(&self, handle: TimerHandle<u64>, _user: usize) {
                self.log.borrow_mut().push("fired");
                let delivery = self.queue.borrow().timer_delivery_time(&handle);
                let me = Rc::clone(self.me.borrow().as_ref().expect("self set"));
                let fresh = self.queue.borrow().add(delivery, 5, me, 0, "reregistered");
                let mut old = self.handle.borrow_mut();
                old.cancel();
                *old = fresh;
                self.log.borrow_mut().push("reregistered");
            }
        }
        let log = Rc::new(RefCell::new(Vec::new()));
        let queue = Rc::new(RefCell::new(TimeQueue64::new()));
        let reg = Rc::new(Reregistrar {
            log: Rc::clone(&log),
            queue: Rc::clone(&queue),
            me: RefCell::new(None),
            handle: RefCell::new(TimerHandle::default()),
        });
        *reg.me.borrow_mut() = Some(Rc::clone(&reg) as Rc<dyn TimerHandler<u64>>);
        *reg.handle.borrow_mut() = queue.borrow().add(
            10,
            5,
            Rc::clone(&reg) as Rc<dyn TimerHandler<u64>>,
            0,
            "original",
        );

        // Delivered two stamps late: the replacement goes in at the
        // original phase 15, not at the late 12 + 5.
        assert_eq!(queue.borrow().process(12), 1);
        assert_eq!(*log.borrow(), ["fired", "reregistered"]);
        assert_eq!(queue.borrow().process(15), 1);
        assert_eq!(queue.borrow().process(17), 0, "the phase stayed at 20");
        assert_eq!(queue.borrow().next_exp(17), 3);
        assert_eq!(queue.borrow().process(20), 1);
        assert_eq!(
            *log.borrow(),
            [
                "fired",
                "reregistered",
                "fired",
                "reregistered",
                "fired",
                "reregistered"
            ]
        );
    }

    /// The `scheduleSyncCheck` discipline: a second schedule request while
    /// the check timer is still armed is refused ("existing timer ID"),
    /// so timers never stack; the slot is rearmed only after it fired and
    /// was cleared.
    #[test]
    fn a_reregister_guard_does_not_stack_timers() {
        struct SyncCheck {
            log: Rc<RefCell<Vec<&'static str>>>,
            queue: Rc<RefCell<TimeQueue64>>,
            me: RefCell<Option<Rc<dyn TimerHandler<u64>>>>,
            sync_check: RefCell<TimerHandle<u64>>,
        }
        impl TimerHandler<u64> for SyncCheck {
            fn handle_timeout(&self, _handle: TimerHandle<u64>, user: usize) {
                if user == 1 {
                    // The scheduled check: detach the slot, then do the
                    // work (the reference cancels the handle first).
                    self.sync_check.borrow_mut().clear_without_cancel();
                    self.log.borrow_mut().push("check");
                    return;
                }
                if self.sync_check.borrow().is_set() {
                    self.log.borrow_mut().push("busy");
                    return;
                }
                let me = Rc::clone(self.me.borrow().as_ref().expect("self set"));
                // scheduleSyncCheck arms relative to the current pass
                // (the reference derives deliveryIn from the clock).
                let when = self.queue.borrow().last_process_time() + 10;
                *self.sync_check.borrow_mut() = self.queue.borrow().add(when, 0, me, 1, "check");
                self.log.borrow_mut().push("scheduled");
            }
        }
        let log = Rc::new(RefCell::new(Vec::new()));
        let queue = Rc::new(RefCell::new(TimeQueue64::new()));
        let sync = Rc::new(SyncCheck {
            log: Rc::clone(&log),
            queue: Rc::clone(&queue),
            me: RefCell::new(None),
            sync_check: RefCell::new(TimerHandle::default()),
        });
        *sync.me.borrow_mut() = Some(Rc::clone(&sync) as Rc<dyn TimerHandler<u64>>);
        let request = |queue: &Rc<RefCell<TimeQueue64>>,
                       sync: &Rc<SyncCheck>,
                       time: u64,
                       name: &'static str| {
            queue.borrow().add(
                time,
                0,
                Rc::clone(sync) as Rc<dyn TimerHandler<u64>>,
                0,
                name,
            );
        };

        request(&queue, &sync, 0, "request-1");
        request(&queue, &sync, 2, "request-2");
        request(&queue, &sync, 12, "request-3");

        assert_eq!(queue.borrow().process(2), 2);
        assert_eq!(*log.borrow(), ["scheduled", "busy"]);
        assert_eq!(queue.borrow().process(10), 1);
        assert_eq!(*log.borrow(), ["scheduled", "busy", "check"]);
        // The slot was cleared by the check, so request-3 rearms it.
        assert_eq!(queue.borrow().process(12), 1);
        assert_eq!(*log.borrow(), ["scheduled", "busy", "check", "scheduled"]);
        assert_eq!(queue.borrow().process(22), 1);
        assert_eq!(
            *log.borrow(),
            ["scheduled", "busy", "check", "scheduled", "check"]
        );
    }

    /// The mutable `timerIntervalTime` (`Node::intervalRef`): a timer may
    /// change its own interval inside the callback -- the post-callback
    /// `time += interval` reschedule uses the new value -- and revert to
    /// nominal afterwards (the TimeKeeper shortening/reverting dance).
    #[test]
    fn an_interval_can_be_changed_in_its_own_callback() {
        struct PaceSelf {
            queue: Rc<RefCell<TimeQueue64>>,
            fires: Cell<usize>,
        }
        impl TimerHandler<u64> for PaceSelf {
            fn handle_timeout(&self, handle: TimerHandle<u64>, _user: usize) {
                self.fires.set(self.fires.get() + 1);
                let queue = self.queue.borrow();
                if self.fires.get() == 2 {
                    // We are slow: shorten the tick interval.
                    queue.set_timer_interval_time(&handle, 3);
                } else if self.fires.get() == 3 {
                    // Sped up enough: revert to nominal.
                    queue.set_timer_interval_time(&handle, 5);
                }
            }
        }
        let queue = Rc::new(RefCell::new(TimeQueue64::new()));
        queue.borrow().add(
            10,
            5,
            Rc::new(PaceSelf {
                queue: Rc::clone(&queue),
                fires: Cell::new(0),
            }),
            0,
            "tracking",
        );

        assert_eq!(queue.borrow().process(10), 1);
        assert_eq!(queue.borrow().process(14), 0, "nominal spacing kept");
        assert_eq!(queue.borrow().process(15), 1);
        assert_eq!(queue.borrow().process(17), 0, "next delivery at 18");
        assert_eq!(queue.borrow().process(18), 1);
        assert_eq!(queue.borrow().process(22), 0, "nominal spacing restored");
        assert_eq!(queue.borrow().process(23), 1);
        assert_eq!(queue.borrow().process(27), 0, "next delivery at 28");
        assert_eq!(queue.borrow().process(28), 1);
    }

    /// The mutable interval surface is reachable through the dispatcher
    /// timers and takes effect on the next reschedule (the TimeKeeper
    /// master-adjusts-the-tracking-timer path).
    #[test]
    fn the_dispatcher_interval_surface_mutates_the_reschedule() {
        let now = Rc::new(Cell::new(1_000_000_000u64));
        let now_for_clock = Rc::clone(&now);
        let mut timers = DispatcherTimers::with_clock(Box::new(move || now_for_clock.get()));
        let fired = Rc::new(Cell::new(0));
        let fired_for_handler = Rc::clone(&fired);
        let handler = Rc::new(TimerHandlerFn::new(
            move |_, _| fired_for_handler.set(fired_for_handler.get() + 1),
            "tracking",
        ));
        let handle = timers.add_timer(500_000, handler, 0, "tracking");

        assert_eq!(timers.timer_interval_time(&handle), 500_000_000);
        // Shorten the interval like inputMasterReading does for a slow
        // app; the first delivery is unaffected (only the reschedule is).
        timers.set_timer_interval_time(&handle, 450_000_000);
        assert_eq!(timers.timer_interval_time(&handle), 450_000_000);
        assert_eq!(timers.timer_delivery_time(&handle), 1_500_000_000);

        assert_eq!(timers.process_timers(), 0, "not due yet at 1s");
        now.set(1_500_000_000);
        assert_eq!(timers.process_timers(), 1);
        assert_eq!(fired.get(), 1);
        // The reschedule used the shortened interval.
        assert_eq!(timers.timer_delivery_time(&handle), 1_950_000_000);
    }

    /// `getTimerInfo` reports nothing for a cancelled or unset handle
    /// (`if (!pNode->isCancelled())` in the reference); a set handle
    /// reports time, interval and name.
    #[test]
    fn timer_info_of_a_cancelled_or_unset_handle_is_none() {
        let queue = TimeQueue64::new();
        let (handler, log) = recorder();
        let mut handle = queue.add(10, 5, handler, 0, "info");
        assert_eq!(queue.get_timer_info(&handle), Some((10, 5, "info")));

        // A copy taken before the cancel still reaches the node; the
        // cancelled check in getTimerInfo turns it into nothing.
        let probe = handle.clone();
        handle.cancel();
        assert_eq!(queue.get_timer_info(&probe), None, "cancelled: no info");
        assert_eq!(queue.get_timer_info(&handle), None, "unset: no info");

        let unset = TimerHandle::<u64>::default();
        assert_eq!(queue.get_timer_info(&unset), None);
        assert_eq!(*log.borrow(), ["released"]);
    }
}
