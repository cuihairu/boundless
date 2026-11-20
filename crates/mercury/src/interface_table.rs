//! `Mercury::InterfaceTable` (`lib/network/interface_table.hpp/.cpp`).
//!
//! One table per socket: 256 slots indexed by message id. An interface
//! registers its elements into the table of the socket it serves, which
//! is the whole of the reference's internal-versus-external distinction —
//! the same message id can mean different things on the internal and the
//! external socket because each socket owns its own table.
//!
//! The machined registration (`registerWithMachined`) is not ported; the
//! name and id it would set are kept as plain fields so the later item
//! that owns machined can fill them in.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use foundation::time_queue::{TimerHandle, TimerHandler};

use crate::event_dispatcher::EventDispatcher;
use crate::interface_element::InterfaceElement;
use crate::interface_element::InterfaceElementWithStats;
use crate::interfaces::InputMessageHandler;
use crate::misc::MessageID;

/// The per-second statistics timer interval (`addTimer( 1000000, ... )`).
const STATS_INTERVAL_MICROS: i64 = 1_000_000;
/// The timer label the reference passes to `addTimer`.
const STATS_TIMER_NAME: &str = "InterfaceStats";

/// `InterfaceTable`: the socket's message-id to element map.
pub struct InterfaceTable {
    /// `name_`: the interface name registered with bwmachined, empty when
    /// not registered.
    name: RefCell<String>,
    /// `id_`: the registration id, e.g. cellapp01.
    id: Cell<i32>,
    /// `table_`: 256 entries, one per message id.
    table: RefCell<Vec<InterfaceElementWithStats>>,
    /// `statsTimerHandle_`.
    stats_timer: RefCell<TimerHandle<u64>>,
}

impl InterfaceTable {
    /// `InterfaceTable( dispatcher )` without the timer: the table slots
    /// exist, and [`attach`](Self::attach) wires the statistics timer.
    ///
    /// The reference registers `this` with the dispatcher straight from
    /// the constructor; here the shared handle must exist first, so the
    /// timer registration is a separate call.
    pub fn new() -> Self {
        Self {
            name: RefCell::new(String::new()),
            id: Cell::new(0),
            table: RefCell::new(vec![InterfaceElementWithStats::new(); 256]),
            stats_timer: RefCell::new(TimerHandle::default()),
        }
    }

    /// Register the one-second statistics timer on `dispatcher`
    /// (`dispatcher.addTimer( 1000000, this, NULL, "InterfaceStats" )`).
    pub fn attach(self: &Rc<Self>, dispatcher: &EventDispatcher) {
        let handle = dispatcher.add_timer(
            STATS_INTERVAL_MICROS,
            Rc::clone(self) as Rc<dyn TimerHandler<u64>>,
            0,
            STATS_TIMER_NAME,
        );
        *self.stats_timer.borrow_mut() = handle;
    }

    /// `serve`: make `ie` the element for its message id and give it the
    /// handler that will receive that message.
    pub fn serve(&self, ie: &InterfaceElement, handler: Option<Rc<dyn InputMessageHandler>>) {
        let mut table = self.table.borrow_mut();
        let entry = &mut table[ie.id() as usize];
        *entry.element_mut() = ie.clone();
        entry.element_mut().set_handler(handler);
    }

    /// `msgName`: the name of the message with this id.
    pub fn msg_name(&self, msg_id: MessageID) -> &'static str {
        self.table.borrow()[msg_id as usize].element().name()
    }

    /// `operator[]`: the element registered for a message id.
    pub fn element(&self, msg_id: MessageID) -> InterfaceElement {
        self.table.borrow()[msg_id as usize].element().clone()
    }

    /// The statistics entry for a message id.
    pub fn element_stats(&self, msg_id: MessageID) -> InterfaceElementWithStats {
        self.table.borrow()[msg_id as usize].clone()
    }

    /// The handler serving a message id.
    pub fn handler(&self, msg_id: MessageID) -> Option<Rc<dyn InputMessageHandler>> {
        self.table.borrow()[msg_id as usize].element().handler()
    }

    /// `handleTimeout`: tick every entry's statistics once.
    pub fn handle_stats_timeout(&self) {
        for entry in self.table.borrow_mut().iter_mut() {
            entry.tick();
        }
    }

    /// `name_`.
    pub fn name(&self) -> String {
        self.name.borrow().clone()
    }

    /// The name a machined registration would carry
    /// (`registerWithMachined( addr, name, id )`).
    pub fn set_name(&self, name: &str) {
        *self.name.borrow_mut() = name.to_string();
    }

    /// `id_`.
    pub fn id(&self) -> i32 {
        self.id.get()
    }

    /// The id a machined registration would carry.
    pub fn set_id(&self, id: i32) {
        self.id.set(id);
    }

    /// The table width, one slot per message id.
    pub fn len(&self) -> usize {
        self.table.borrow().len()
    }

    /// Always false: the reference allocates all 256 slots in the
    /// constructor, so the table is never empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for InterfaceTable {
    fn default() -> Self {
        Self::new()
    }
}

impl TimerHandler<u64> for InterfaceTable {
    /// `handleTimeout`.
    fn handle_timeout(&self, _handle: TimerHandle<u64>, _user: usize) {
        self.handle_stats_timeout();
    }
}

impl Drop for InterfaceTable {
    /// `~InterfaceTable`: `statsTimerHandle_.cancel()`.
    fn drop(&mut self) {
        self.stats_timer.borrow_mut().cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_element::{FIXED_LENGTH_MESSAGE, VARIABLE_LENGTH_MESSAGE};
    use crate::interface_minder::InterfaceMinder;

    struct CountingHandler;

    impl InputMessageHandler for CountingHandler {}

    /// The table is the socket's registry: serving puts the element at its
    /// message id with the handler the caller chose, and `msgName` answers
    /// by id afterwards.
    #[test]
    fn serve_puts_an_element_at_its_id() {
        let table = InterfaceTable::new();
        assert_eq!(table.len(), 256);
        // All 256 slots are allocated up front, so the table is never empty.
        assert!(!table.is_empty());
        assert_eq!(table.msg_name(0), "");

        let element = InterfaceElement::new("shutdown", 3, FIXED_LENGTH_MESSAGE, 0, None);
        table.serve(&element, Some(Rc::new(CountingHandler)));

        assert_eq!(table.msg_name(3), "shutdown");
        assert_eq!(table.element(3).length_style(), FIXED_LENGTH_MESSAGE);
        assert!(table.handler(3).is_some());
        assert!(table.handler(2).is_none(), "the other slots are untouched");
    }

    /// The internal and the external socket each own a table, so the same
    /// message id can carry different elements on each — this is the
    /// reference's whole internal/external distinction.
    #[test]
    fn each_socket_owns_its_own_table() {
        let internal = InterfaceMinder::new("CellAppInterface");
        internal.add("shutdown", FIXED_LENGTH_MESSAGE, 0, None);
        internal.add("handover", FIXED_LENGTH_MESSAGE, 16, None);

        let external = InterfaceMinder::new("BaseAppExtInterface");
        external.add("authenticate", VARIABLE_LENGTH_MESSAGE, 2, None);

        let internal_table = InterfaceTable::new();
        let external_table = InterfaceTable::new();
        internal.register_with_interface(&internal_table);
        external.register_with_interface(&external_table);

        assert_eq!(internal_table.msg_name(0), "shutdown");
        assert_eq!(external_table.msg_name(0), "authenticate");
        assert_eq!(internal_table.msg_name(1), "handover");
        assert_eq!(
            external_table.msg_name(1),
            "",
            "id 1 is free on the external socket"
        );
    }

    /// The one-second statistics timer ticks every entry's averages.
    #[test]
    fn the_statistics_timer_ticks_every_entry() {
        let table = InterfaceTable::new();
        {
            let mut entry = table.table.borrow_mut();
            entry[5].message_received(10);
            entry[200].message_received(3);
        }

        assert_eq!(table.element_stats(5).num_bytes_received(), 10);
        assert_eq!(table.element_stats(5).avg_bytes_received_per_second(), 0.0);

        table.handle_stats_timeout();

        let bias = InterfaceElementWithStats::average_bias();
        assert_eq!(
            table.element_stats(5).avg_bytes_received_per_second(),
            10.0 * (1.0 - bias)
        );
        assert_eq!(
            table.element_stats(200).avg_messages_received_per_second(),
            1.0 * (1.0 - bias)
        );
        assert_eq!(table.element_stats(6).avg_bytes_received_per_second(), 0.0);
    }

    /// A pinned-clock dispatcher drives the table's timer like the
    /// reference's `addTimer( 1000000, this, ... )`: the averages move
    /// only when a second of stamps has gone by.
    #[test]
    fn the_timer_is_registered_with_the_dispatcher() {
        use std::cell::Cell;

        let now = Rc::new(Cell::new(0u64));
        let clock = Rc::clone(&now);
        let dispatcher = EventDispatcher::with_clock(Box::new(move || clock.get()));

        let table = Rc::new(InterfaceTable::new());
        table.attach(&dispatcher);
        assert!(table.stats_timer.borrow().is_set());

        {
            let mut entry = table.table.borrow_mut();
            entry[7].message_received(4);
        }

        // Half a second in: the timer is not due yet, nothing ticks.
        now.set(500_000);
        dispatcher.process_once(false);
        assert_eq!(
            table.element_stats(7).avg_bytes_received_per_second(),
            0.0,
            "one tick a second, not one tick a pass"
        );

        // A whole second in: the statistics tick runs.
        now.set(1_000_000);
        dispatcher.process_once(false);
        let bias = InterfaceElementWithStats::average_bias();
        assert_eq!(
            table.element_stats(7).avg_bytes_received_per_second(),
            4.0 * (1.0 - bias)
        );
        assert_eq!(table.element_stats(7).num_messages_received(), 1);
    }

    /// The name and id a machined registration would set are plain fields.
    #[test]
    fn the_machined_name_and_id_are_plain_fields() {
        let table = InterfaceTable::default();
        assert_eq!(table.name(), "");
        assert_eq!(table.id(), 0);

        table.set_name("CellAppInterface");
        table.set_id(3);
        assert_eq!(table.name(), "CellAppInterface");
        assert_eq!(table.id(), 3);
    }
}
