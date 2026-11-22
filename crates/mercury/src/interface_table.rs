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
use crate::misc::{MessageID, Reason};

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

    /// The `UDPBundleProcessor::dispatchMessages` resolution arm
    /// (`udp_bundle_processor.cpp:71-98`): look up the element registered
    /// for `msg_id` and validate that it can be delivered.
    ///
    /// Returns `Err(REASON_NONEXISTENT_ENTRY)` when no handler serves the
    /// id -- the reference discards the bundle in that case -- and
    /// `Err(REASON_CORRUPTED_PACKET)` when the element cannot size itself
    /// for this handler (a callback reporting `INVALID_STREAM_SIZE`).
    /// Otherwise `Ok` carries the element, with any callback length folded
    /// into its style, and the handler to call.
    ///
    /// The caller unpacks the message with the returned element's header
    /// and then delivers it to the returned handler, in the reference's
    /// order.
    pub fn resolve_message(
        &self,
        msg_id: MessageID,
    ) -> Result<(InterfaceElement, Rc<dyn InputMessageHandler>), Reason> {
        let element = self.element(msg_id);
        let Some(handler) = element.handler() else {
            eprintln!(
                "InterfaceTable::resolveMessage: Discarding bundle after hitting unhandled message ID {msg_id}"
            );
            return Err(Reason::NonexistentEntry);
        };

        // The reference works on a copy (`InterfaceElement updatedIE = ie;`)
        // so a callback sizing does not stick to the registered element.
        let mut updated = element;
        if !updated.update_length_details(&*handler) {
            eprintln!(
                "InterfaceTable::resolveMessage: Discarding bundle after failure to update length details for message ID {msg_id}"
            );
            return Err(Reason::CorruptedPacket);
        }

        Ok((updated, handler))
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
    use std::cell::Cell;

    use crate::endpoint::{Address, NONE};
    use crate::interface_element::{
        CALLBACK_LENGTH_MESSAGE, FIXED_LENGTH_MESSAGE, INVALID_STREAM_SIZE,
        REPLY_MESSAGE_IDENTIFIER, VARIABLE_LENGTH_MESSAGE,
    };
    use crate::interface_minder::InterfaceMinder;
    use crate::unpacked_message_header::UnpackedMessageHeader;
    use foundation::stream::BinaryStream;

    struct CountingHandler {
        delivered: Rc<Cell<usize>>,
    }

    impl CountingHandler {
        fn new() -> Rc<Self> {
            Rc::new(Self {
                delivered: Rc::new(Cell::new(0)),
            })
        }
    }

    impl InputMessageHandler for CountingHandler {
        fn handle_message(
            &self,
            _source: &Address,
            _header: &mut UnpackedMessageHeader,
            _data: &mut BinaryStream,
        ) {
            self.delivered.set(self.delivered.get() + 1);
        }
    }

    /// A callback handler answering a fixed stream size, or
    /// `INVALID_STREAM_SIZE` to reject the bundle.
    struct SizingHandler(i32);

    impl InputMessageHandler for SizingHandler {
        fn handle_message(
            &self,
            _source: &Address,
            _header: &mut UnpackedMessageHeader,
            _data: &mut BinaryStream,
        ) {
        }

        fn get_message_stream_size(&self, _msg_id: MessageID) -> i32 {
            self.0
        }
    }

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
        table.serve(&element, Some(CountingHandler::new()));

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

    /// An unserved message id resolves to `REASON_NONEXISTENT_ENTRY`: the
    /// reference's dispatch loop discards the bundle after hitting an
    /// unhandled message id (`udp_bundle_processor.cpp:72-85`).
    #[test]
    fn an_unserved_message_id_is_a_nonexistent_entry() {
        let table = InterfaceTable::new();

        assert!(matches!(
            table.resolve_message(9),
            Err(Reason::NonexistentEntry)
        ));
        // The reserved reply id starts unserved too.
        assert!(matches!(
            table.resolve_message(REPLY_MESSAGE_IDENTIFIER),
            Err(Reason::NonexistentEntry)
        ));
    }

    /// A callback handler answering `INVALID_STREAM_SIZE` resolves to
    /// `REASON_CORRUPTED_PACKET`: the reference's loop fails
    /// `updateLengthDetails` and discards the bundle
    /// (`udp_bundle_processor.cpp:89-98`).
    #[test]
    fn a_callback_that_rejects_the_bundle_is_a_corrupted_packet() {
        let table = InterfaceTable::new();
        let element = InterfaceElement::new("callback", 7, CALLBACK_LENGTH_MESSAGE, 0, None);
        table.serve(&element, Some(Rc::new(SizingHandler(INVALID_STREAM_SIZE))));

        assert!(matches!(
            table.resolve_message(7),
            Err(Reason::CorruptedPacket)
        ));
    }

    /// A served message resolves to its handler with the callback length
    /// folded in, and delivering to the returned handler is the caller's
    /// half of the reference's dispatch loop.
    #[test]
    fn a_served_message_resolves_and_delivers() {
        let table = InterfaceTable::new();
        let source = NONE;

        // A callback handler answering 12 folds the element into a fixed
        // 12-byte message, exactly as `updateLengthDetails` does in the
        // reference loop's copy.
        let element = InterfaceElement::new("callback", 7, CALLBACK_LENGTH_MESSAGE, 0, None);
        table.serve(&element, Some(Rc::new(SizingHandler(12))));

        let (updated, handler) = table.resolve_message(7).expect("served id resolves");
        assert_eq!(updated.length_style(), FIXED_LENGTH_MESSAGE);
        assert_eq!(updated.length_param(), 12);

        // The registered element keeps its callback style: the fold only
        // feeds this delivery, matching `InterfaceElement updatedIE = ie;`.
        assert_eq!(table.element(7).length_style(), CALLBACK_LENGTH_MESSAGE);

        let mut header = UnpackedMessageHeader {
            identifier: 7,
            length: 12,
            ..Default::default()
        };
        let mut data = BinaryStream::new();
        data.write_bytes(&[0; 12]);
        handler.handle_message(&source, &mut header, &mut data);

        // A fixed message resolves unchanged and delivers to its handler.
        let counter = CountingHandler::new();
        let element = InterfaceElement::new("shutdown", 3, FIXED_LENGTH_MESSAGE, 0, None);
        table.serve(&element, Some(counter.clone()));
        let (updated, handler) = table.resolve_message(3).expect("served id resolves");
        assert_eq!(updated.length_style(), FIXED_LENGTH_MESSAGE);

        let mut header = UnpackedMessageHeader {
            identifier: 3,
            length: 0,
            ..Default::default()
        };
        let mut data = BinaryStream::new();
        handler.handle_message(&source, &mut header, &mut data);
        assert_eq!(counter.delivered.get(), 1);
    }
}
