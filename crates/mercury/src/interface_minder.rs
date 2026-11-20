//! `Mercury::InterfaceMinder` (`lib/network/interface_minder.hpp/.cpp/.ipp`).
//!
//! One instance per interface. It owns the ordered list of
//! [`InterfaceElement`]s and hands out message ids in declaration order,
//! which is exactly how the reference builds its registry: each message in
//! an interface contributes one element in the order it is written.

use std::cell::RefCell;
use std::rc::Rc;

use crate::interface_element::InterfaceElement;
use crate::interface_table::InterfaceTable;
use crate::interfaces::InputMessageHandler;
use crate::misc::MessageID;

/// The highest message id an interface may use: `254 - startID` is what
/// the reference divides the free space by when it splits a range.
pub const MAX_MESSAGE_ID: usize = 254;

/// `InterfaceMinder`: the per-interface element list.
///
/// The reference keeps the list in a `BW::vector`, reserving 256 in the
/// constructor.
pub struct InterfaceMinder {
    /// `name_`.
    name: &'static str,
    /// `elements_`.
    elements: RefCell<Vec<InterfaceElement>>,
}

impl InterfaceMinder {
    /// `InterfaceMinder( name )`: reserves room for 256 elements.
    pub fn new(name: &'static str) -> Self {
        // The reference reserves 256 in its constructor.
        let elements: Vec<InterfaceElement> = Vec::with_capacity(256);
        Self {
            name,
            elements: RefCell::new(elements),
        }
    }

    /// The interface name, e.g. `"CellAppInterface"`.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// `add`: take the next message id and append the element. Returns the
    /// id the element got.
    pub fn add(
        &self,
        name: &'static str,
        length_style: i8,
        length_param: i32,
        handler: Option<Rc<dyn InputMessageHandler>>,
    ) -> MessageID {
        let id = self.elements.borrow().len() as MessageID;
        self.elements.borrow_mut().push(InterfaceElement::new(
            name,
            id,
            length_style,
            length_param,
            handler,
        ));
        id
    }

    /// `addRange( msgID, rangePortion )`: split the ids left after the
    /// given message into `rangePortion` blocks and return the id at the
    /// end of the first block. Every id in the block is a copy of the
    /// given element, so all of them reach the same handler.
    ///
    /// `msgID` must be the last element's id: the reference
    /// `MF_ASSERT( ie.id() == elements_.back().id() )`.
    pub fn add_range(&self, msg_id: MessageID, range_portion: usize) -> MessageID {
        let (base, start_id) = {
            let elements = self.elements.borrow();
            let last = elements.last().expect("addRange: needs a declared element");
            debug_assert_eq!(
                last.id(),
                msg_id,
                "addRange: msgID must be the last element"
            );
            (last.clone(), elements.len())
        };
        let end_id = start_id + (MAX_MESSAGE_ID - start_id) / range_portion;

        while self.elements.borrow().len() <= end_id {
            self.add(
                base.name(),
                base.length_style(),
                base.length_param(),
                base.handler(),
            );
        }
        end_id as MessageID
    }

    /// `interfaceElement( id )`: the element for a message id.
    pub fn interface_element(&self, id: MessageID) -> InterfaceElement {
        self.elements.borrow()[id as usize].clone()
    }

    /// The handler registered for an element index, if any.
    ///
    /// The reference indexes `elements_[ index ]` without a bound, so an
    /// out-of-range index is undefined behaviour there; this port answers
    /// "no handler" instead of reading past the end.
    pub fn handler(&self, index: usize) -> Option<Rc<dyn InputMessageHandler>> {
        self.elements.borrow().get(index).and_then(|e| e.handler())
    }

    /// Set the handler registered for an element index.
    pub fn set_handler(&self, index: usize, handler: Option<Rc<dyn InputMessageHandler>>) {
        if let Some(element) = self.elements.borrow_mut().get_mut(index) {
            element.set_handler(handler);
        }
    }

    /// How many messages the interface declares.
    pub fn len(&self) -> usize {
        self.elements.borrow().len()
    }

    /// Whether the interface declares no messages.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `registerWithInterface`: copy every element into the socket's
    /// interface table, keeping each element's handler.
    pub fn register_with_interface(&self, table: &InterfaceTable) {
        for element in self.elements.borrow().iter() {
            let handler = element.handler();
            table.serve(element, handler);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_element::{
        CALLBACK_LENGTH_MESSAGE, FIXED_LENGTH_MESSAGE, VARIABLE_LENGTH_MESSAGE,
    };

    struct NullHandler;

    impl InputMessageHandler for NullHandler {}

    fn login_minder() -> InterfaceMinder {
        let minder = InterfaceMinder::new("LoginInterface");
        minder.add(
            "login",
            VARIABLE_LENGTH_MESSAGE,
            2,
            Some(Rc::new(NullHandler)),
        );
        minder.add("probe", FIXED_LENGTH_MESSAGE, 0, Some(Rc::new(NullHandler)));
        minder.add(
            "challengeResponse",
            VARIABLE_LENGTH_MESSAGE,
            2,
            Some(Rc::new(NullHandler)),
        );
        minder
    }

    /// The three `LoginInterface` messages take ids 0, 1 and 2 in
    /// declaration order, with the lengths the reference declares them with.
    #[test]
    fn the_login_interface_ids_match_the_reference() {
        let minder = login_minder();
        assert_eq!(minder.len(), 3);

        let login = minder.interface_element(0);
        assert_eq!(login.name(), "login");
        assert_eq!(login.length_style(), VARIABLE_LENGTH_MESSAGE);
        assert_eq!(login.length_param(), 2);
        assert!(login.handler().is_some());

        let probe = minder.interface_element(1);
        assert_eq!(probe.name(), "probe");
        assert_eq!(probe.length_style(), FIXED_LENGTH_MESSAGE);
        assert_eq!(probe.length_param(), 0);

        let challenge = minder.interface_element(2);
        assert_eq!(challenge.name(), "challengeResponse");
        assert_eq!(challenge.length_style(), VARIABLE_LENGTH_MESSAGE);
        assert_eq!(challenge.length_param(), 2);
    }

    /// `addRange` gives the first block exactly its share of the free ids,
    /// every id in the block copying the base element.
    #[test]
    fn a_range_covers_the_first_block_of_the_free_ids() {
        let minder = login_minder();
        let end = minder.add_range(2, 2);

        // (254 - 3) / 2 = 125 ids after the three declared ones.
        assert_eq!(end, 3 + 125);
        assert_eq!(minder.len(), end as usize + 1);

        let first = minder.interface_element(3);
        assert_eq!(
            first.name(),
            "challengeResponse",
            "a copy of the base element"
        );
        assert_eq!(first.length_style(), VARIABLE_LENGTH_MESSAGE);
        assert_eq!(first.length_param(), 2);
        assert!(first.handler().is_some(), "the handler carries across");

        let last = minder.interface_element(end);
        assert_eq!(last.name(), "challengeResponse");
    }

    /// The handler of an element is reachable by index, and can be replaced
    /// or removed.
    #[test]
    fn the_handler_is_reachable_by_element_index() {
        let minder = login_minder();
        assert!(minder.handler(0).is_some());
        assert!(minder.handler(3).is_none(), "no such element");

        minder.set_handler(1, None);
        assert!(minder.handler(1).is_none());
        assert!(minder.handler(0).is_some(), "the others are untouched");

        minder.set_handler(4, Some(Rc::new(NullHandler)));
        assert!(minder.handler(4).is_none(), "out of range is inert");
    }

    /// `registerWithInterface` copies every element into the table, so the
    /// table answers by message id.
    #[test]
    fn registering_copies_every_element_into_the_table() {
        let minder = login_minder();
        let table = crate::interface_table::InterfaceTable::new();

        assert_eq!(table.len(), 256, "the reference's table_ is sized 256");
        assert_eq!(table.msg_name(0), "", "an empty table has no name");

        minder.register_with_interface(&table);

        assert_eq!(table.msg_name(0), "login");
        assert_eq!(table.msg_name(1), "probe");
        assert_eq!(table.msg_name(2), "challengeResponse");
        assert_eq!(table.element(0).length_param(), 2);
        assert_eq!(table.element(1).length_param(), 0);
        assert!(table.handler(0).is_some());
    }

    /// An element the interface has not declared is the uninitialised
    /// default: `INVALID_MESSAGE` style and an empty name.
    #[test]
    fn an_undeclared_message_is_the_default_element() {
        let minder = login_minder();
        let table = crate::interface_table::InterfaceTable::new();
        minder.register_with_interface(&table);

        let undeclared = table.element(9);
        assert_eq!(undeclared.name(), "");
        assert_eq!(
            undeclared.length_style(),
            crate::interface_element::INVALID_MESSAGE
        );
        assert!(undeclared.handler().is_none());
    }

    /// The callback length style is reachable through the minder too.
    #[test]
    fn a_callback_message_takes_the_next_id() {
        let minder = InterfaceMinder::new("Callback");
        assert_eq!(minder.add("sized", CALLBACK_LENGTH_MESSAGE, 0, None), 0);
        assert_eq!(
            minder.interface_element(0).length_style(),
            CALLBACK_LENGTH_MESSAGE
        );
        assert_eq!(minder.name(), "Callback");
    }

    /// A minder with no declared messages is empty and keeps its name.
    #[test]
    fn a_minder_without_messages_is_empty() {
        let minder = InterfaceMinder::new("EmptyInterface");
        assert!(minder.is_empty());
        assert_eq!(minder.len(), 0);
        assert_eq!(minder.name(), "EmptyInterface");

        minder.add("first", FIXED_LENGTH_MESSAGE, 0, None);
        assert!(!minder.is_empty());
    }

    /// The extracted table of an interface, as `( name, lengthStyle,
    /// lengthParam )`.
    fn extracted(minder: &InterfaceMinder) -> Vec<(&str, i8, i32)> {
        (0..minder.len() as u8)
            .map(|id| minder.interface_element(id))
            .map(|e| (e.name(), e.length_style(), e.length_param()))
            .collect()
    }

    /// `lib/connection/login_interface.hpp` declares its three messages in
    /// this order, so the extracted table is this table: `login` and
    /// `challengeResponse` are 2-byte variable messages, `probe` is an empty
    /// (fixed, length 0) one.
    #[test]
    fn the_login_interface_matches_the_reference_registry() {
        let minder = login_minder();
        assert_eq!(
            extracted(&minder),
            vec![
                ("login", VARIABLE_LENGTH_MESSAGE, 2),
                ("probe", FIXED_LENGTH_MESSAGE, 0),
                ("challengeResponse", VARIABLE_LENGTH_MESSAGE, 2),
            ]
        );
    }

    /// `server/reviver/reviver_interface.hpp` expands
    /// `BW_REVIVER_MSGS( COMPONENT )` five times, each expansion adding
    /// `Birth` then `Death` as fixed messages of `sizeof( Mercury::Address )`
    /// bytes. The components come out in declaration order.
    #[test]
    fn the_reviver_interface_matches_the_reference_registry() {
        let address_size = std::mem::size_of::<crate::endpoint::Address>() as i32;
        assert_eq!(address_size, 8, "uint32 ip + uint16 port + uint16 salt");

        let minder = InterfaceMinder::new("ReviverInterface");
        for (birth, death) in [
            ("handleCellAppMgrBirth", "handleCellAppMgrDeath"),
            ("handleBaseAppMgrBirth", "handleBaseAppMgrDeath"),
            ("handleDBAppMgrBirth", "handleDBAppMgrDeath"),
            ("handleDBAppBirth", "handleDBAppDeath"),
            ("handleLoginBirth", "handleLoginDeath"),
        ] {
            minder.add(birth, FIXED_LENGTH_MESSAGE, address_size, None);
            minder.add(death, FIXED_LENGTH_MESSAGE, address_size, None);
        }

        let expected = [
            ("handleCellAppMgrBirth", FIXED_LENGTH_MESSAGE, address_size),
            ("handleCellAppMgrDeath", FIXED_LENGTH_MESSAGE, address_size),
            ("handleBaseAppMgrBirth", FIXED_LENGTH_MESSAGE, address_size),
            ("handleBaseAppMgrDeath", FIXED_LENGTH_MESSAGE, address_size),
            ("handleDBAppMgrBirth", FIXED_LENGTH_MESSAGE, address_size),
            ("handleDBAppMgrDeath", FIXED_LENGTH_MESSAGE, address_size),
            ("handleDBAppBirth", FIXED_LENGTH_MESSAGE, address_size),
            ("handleDBAppDeath", FIXED_LENGTH_MESSAGE, address_size),
            ("handleLoginBirth", FIXED_LENGTH_MESSAGE, address_size),
            ("handleLoginDeath", FIXED_LENGTH_MESSAGE, address_size),
        ];

        assert_eq!(minder.len(), 10, "five components times two messages");
        assert_eq!(extracted(&minder), expected);
    }
}
