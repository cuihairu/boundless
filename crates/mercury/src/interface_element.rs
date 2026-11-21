//! The Mercury message registry: `InterfaceElement` and `InterfaceMinder`
//! (`lib/network/interface_element.{hpp,cpp}`, `interface_minder.{hpp,cpp}`).
//!
//! BW-02-02-01 extracts the message ids and length tables that the reference
//! builds by macro expansion inside each `BEGIN_MERCURY_INTERFACE` block: a
//! message's id is its declaration order in the interface, its length style
//! is fixed or variable, and for variable messages `length_param` is the
//! byte width of the length field in the header. Handler registration (the
//! reference's `InputMessageHandler *`) arrives with BW-02-02-05.

use crate::misc::MessageId;

/// `FIXED_LENGTH_MESSAGE`: `length_param` is the number of bytes in the
/// message body, constant for all messages of this type.
pub const FIXED_LENGTH_MESSAGE: i8 = 0;
/// `VARIABLE_LENGTH_MESSAGE`: `length_param` is the number of bytes used to
/// store the message size.
pub const VARIABLE_LENGTH_MESSAGE: i8 = 1;
/// `CALLBACK_LENGTH_MESSAGE`: the length is decided by a callback
/// (`InputMessageHandler::getMessageStreamSize`).
pub const CALLBACK_LENGTH_MESSAGE: i8 = 2;
/// `INVALID_MESSAGE`: the element has not been initialised.
pub const INVALID_MESSAGE: i8 = 3;

/// `INVALID_STREAM_SIZE`: a stream-size callback return value meaning "the
/// message is invalid, discard it".
pub const INVALID_STREAM_SIZE: i32 = i32::MIN;

/// `DEFAULT_VARIABLE_LENGTH_HEADER_SIZE`.
pub const DEFAULT_VARIABLE_LENGTH_HEADER_SIZE: u32 = 1;

/// `REPLY_MESSAGE_IDENTIFIER`: the message id reserved for reply messages,
/// handled internally by the NUB and never handed to `InterfaceMinder::add`.
pub const REPLY_MESSAGE_IDENTIFIER: MessageId = 0xFF;

/// `IDENTIFIER_SIZE`: the byte width of the message identifier itself.
pub const IDENTIFIER_SIZE: i32 = 1;

/// A message's length style, the `lengthStyle` of an `InterfaceElement`.
///
/// The reference stores it as an `int8` holding one of the
/// `*_LENGTH_MESSAGE` constants, so the discriminants are pinned to those
/// values (`#[repr(i8)]`, same numbering).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i8)]
pub enum LengthStyle {
    /// `FIXED_LENGTH_MESSAGE` (0).
    Fixed = FIXED_LENGTH_MESSAGE,
    /// `VARIABLE_LENGTH_MESSAGE` (1).
    Variable = VARIABLE_LENGTH_MESSAGE,
    /// `CALLBACK_LENGTH_MESSAGE` (2).
    Callback = CALLBACK_LENGTH_MESSAGE,
    /// `INVALID_MESSAGE` (3).
    Invalid = INVALID_MESSAGE,
}

/// This structure describes a single message within an interface: how to
/// encode and decode the header of the message, including whether the
/// message is fixed or variable length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterfaceElement {
    /// Unique message ID.
    id: MessageId,
    /// Fixed or variable length.
    length_style: LengthStyle,
    /// This depends on `length_style`.
    length_param: i32,
    /// The name of the interface method.
    name: &'static str,
    /// Whether this message type can be processed out-of-order (early).
    should_process_early: bool,
}

impl InterfaceElement {
    /// The reference constructor. The port leaves the handler to BW-02-02-05.
    #[must_use]
    pub const fn new(
        name: &'static str,
        id: MessageId,
        length_style: LengthStyle,
        length_param: i32,
    ) -> Self {
        Self {
            id,
            length_style,
            length_param,
            name,
            should_process_early: false,
        }
    }

    /// The standard interface definition for a reply message, used
    /// internally by Mercury (`InterfaceElement::REPLY`).
    #[must_use]
    pub const fn reply() -> Self {
        Self::new("Reply", REPLY_MESSAGE_IDENTIFIER, LengthStyle::Variable, 4)
    }

    /// `id()`.
    #[must_use]
    pub const fn id(&self) -> MessageId {
        self.id
    }

    /// `lengthStyle()`.
    #[must_use]
    pub const fn length_style(&self) -> LengthStyle {
        self.length_style
    }

    /// `lengthParam()`.
    #[must_use]
    pub const fn length_param(&self) -> i32 {
        self.length_param
    }

    /// `name()`.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// `headerSize()`: the number of bytes occupied by a header for this
    /// type of message -- the identifier plus, for variable length
    /// messages, the byte width of the length field. Anything that is not
    /// fixed or variable (callback or invalid) has no computable header.
    #[must_use]
    pub const fn header_size(&self) -> i32 {
        match self.length_style {
            LengthStyle::Fixed => IDENTIFIER_SIZE,
            LengthStyle::Variable => IDENTIFIER_SIZE + self.length_param,
            LengthStyle::Callback | LengthStyle::Invalid => -1,
        }
    }

    /// `nominalBodySize()`: the number of bytes nominally occupied by the
    /// body. Never guesses for variable-length messages.
    #[must_use]
    pub const fn nominal_body_size(&self) -> i32 {
        match self.length_style {
            LengthStyle::Fixed => self.length_param,
            _ => 0,
        }
    }

    /// `streamSize()`: negative for variable length messages (the reference
    /// encodes "this many bytes of length header" as a negative size).
    #[must_use]
    pub const fn stream_size(&self) -> i32 {
        match self.length_style {
            LengthStyle::Variable => -self.length_param,
            _ => self.length_param,
        }
    }

    /// Whether this message type can be processed out-of-order (early).
    #[must_use]
    pub const fn should_process_early(&self) -> bool {
        self.should_process_early
    }

    /// Set whether this message type can be processed out-of-order (early).
    pub fn set_should_process_early(&mut self, new_value: bool) {
        self.should_process_early = new_value;
    }

    /// The static `canHandleLength( length, lengthHeaderSize )`: whether the
    /// given length can be represented fully with the given variable length
    /// header size.
    ///
    /// A 4-byte header can hold any `i32` length, so it short-circuits
    /// before the shift below would overflow. Callers pass header sizes
    /// 0..=4 (`MF_ASSERT( lengthHeaderSize <= 4 )` in the reference).
    #[must_use]
    pub const fn header_can_handle_length(length: i32, length_header_size: i32) -> bool {
        if length_header_size == 4 {
            return true;
        }
        length < ((1 << (8 * length_header_size)) - 1)
    }

    /// The instance `canHandleLength( length )`: whether this element can
    /// express the given body length. Fixed messages always can, and a
    /// 4-byte variable header always can; other variable widths are bounded
    /// by their header size.
    #[must_use]
    pub const fn can_handle_length(&self, length: i32) -> bool {
        match self.length_style {
            LengthStyle::Variable => {
                self.length_param >= 4 || Self::header_can_handle_length(length, self.length_param)
            }
            _ => true,
        }
    }
}

/// `InterfaceMinder` manages a set of interface elements, numbering them
/// sequentially as they are added -- the port of the registry the reference
/// drives by macro expansion inside `BEGIN_MERCURY_INTERFACE` blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceMinder {
    elements: Vec<InterfaceElement>,
    name: &'static str,
}

impl InterfaceMinder {
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            elements: Vec::new(),
            name,
        }
    }

    /// `add`: registers one interface element (Mercury method). The element
    /// takes the id `elements_.size()`, so a message's id is its declaration
    /// order in the interface. Returns the assigned id.
    pub fn add(
        &mut self,
        name: &'static str,
        length_style: LengthStyle,
        length_param: i32,
    ) -> MessageId {
        let id = self.elements.len() as MessageId;
        self.elements
            .push(InterfaceElement::new(name, id, length_style, length_param));
        id
    }

    /// `addRange`: populates the interface with a range of the same
    /// interface element. `range_portion` specifies the proportion of the
    /// remaining range to consume; a value of x consumes 1/x.
    ///
    /// The caller passes the just-added base element's id (`METHOD_RANGE_MSG`
    /// expands to `gMinder.addRange( NAME, RANGE_PORTION )` right after
    /// `NAME` was added), so the range starts at `base_id + 1` and fills
    /// through `end_id` inclusive. Returns the last id of the range.
    pub fn add_range(&mut self, base_id: MessageId, range_portion: i32) -> MessageId {
        debug_assert_eq!(
            self.elements.last().map(InterfaceElement::id),
            Some(base_id),
            "addRange expects the just-added base element"
        );
        // A copy, so the mutable adds below do not alias `self.elements`.
        let base = *self.element(base_id);

        let start_id = self.elements.len();
        let end_id = start_id + ((254 - start_id) as i32 / range_portion) as usize;

        while self.elements.len() <= end_id {
            self.add(base.name(), base.length_style(), base.length_param());
        }

        end_id as MessageId
    }

    /// `interfaceElement( id )`: the element with the given id.
    #[must_use]
    pub fn element(&self, id: MessageId) -> &InterfaceElement {
        &self.elements[id as usize]
    }

    /// Mutable access to the element with the given id.
    pub fn element_mut(&mut self, id: MessageId) -> &mut InterfaceElement {
        &mut self.elements[id as usize]
    }

    /// Find an element by message name.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&InterfaceElement> {
        self.elements.iter().find(|element| element.name() == name)
    }

    /// The number of registered elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.elements.len()
    }

    /// Whether no element is registered yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// Iterate the registered elements in id order.
    pub fn iter(&self) -> std::slice::Iter<'_, InterfaceElement> {
        self.elements.iter()
    }

    /// The interface's name (used to register with machined).
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

impl<'a> IntoIterator for &'a InterfaceMinder {
    type Item = &'a InterfaceElement;
    type IntoIter = std::slice::Iter<'a, InterfaceElement>;

    fn into_iter(self) -> Self::IntoIter {
        self.elements.iter()
    }
}

/// Declare a Mercury interface the way the reference's
/// `BEGIN_MERCURY_INTERFACE` .. `END_MERCURY_INTERFACE` block does
/// (`lib/network/interface_macros.hpp`).
///
/// Each message listed is `gMinder.add( ... )` in declaration order, so its
/// id matches the reference numbering. Message tokens use the reference's
/// own spelling (`channelMsg`, not `channel_msg`) so the registry's names
/// compare equal to the original tables; the tokens only ever feed
/// `stringify!`, so no non-snake-case Rust identifier is created.
///
/// ```ignore
/// mercury_interface! {
///     /// `LoginInterface`, the external client-facing interface
///     /// (`lib/connection/login_interface.hpp`).
///     pub mod login_interface {
///         name: "LoginInterface",
///         login => (LengthStyle::Variable, 2),
///         probe => (LengthStyle::Fixed, 0),
///     }
/// }
/// ```
#[macro_export]
macro_rules! mercury_interface {
    (
        $(#[$doc:meta])*
        $vis:vis mod $module:ident {
            name: $name:literal,
            $( $message:ident => ( $style:expr , $param:expr ), )+
        }
    ) => {
        $(#[$doc])*
        $vis mod $module {
            use $crate::interface_element::{InterfaceMinder, LengthStyle};

            /// This interface's registry (`gMinder` in the reference), with
            /// every message added in declaration order so the ids match the
            /// reference's `InterfaceMinder::add` numbering.
            #[must_use]
            pub fn interface() -> InterfaceMinder {
                let mut minder = InterfaceMinder::new($name);
                $( minder.add(stringify!($message), $style, $param); )+
                minder
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    mercury_interface! {
        /// `CommonInterface`, the registry the reference's own unit tests
        /// use (`lib/network/unit_test/common_interface.hpp`).
        pub mod common_interface {
            name: "CommonInterface",
            // BW_COMMON_MSG( msg1 ): `Traits traits; uint32 seq; uint32
            // data;` -- a `#pragma pack(1)` struct of an enum (4) plus two
            // uint32s, so the fixed body is 12 bytes.
            msg1 => (LengthStyle::Fixed, 12),
            // BW_COMMON_VARIABLE_MSG( msgVar1, 1 ).
            msgVar1 => (LengthStyle::Variable, 1),
            // BW_COMMON_VARIABLE_MSG( msgVar2, 4 ).
            msgVar2 => (LengthStyle::Variable, 4),
            // BW_COMMON_MSG( disconnect ): `uint32 seq`.
            disconnect => (LengthStyle::Fixed, 4),
        }
    }

    mercury_interface! {
        /// `FragmentServerInterface`, an internal server-to-server registry
        /// (`lib/network/unit_test/test_fragment_interfaces.hpp`). Every
        /// message is `BW_VARLEN_MSG`, a `MERCURY_HANDLED_VARIABLE_MESSAGE`
        /// with a 2-byte length header.
        pub mod fragment_server_interface {
            name: "FragmentServerInterface",
            connect => (LengthStyle::Variable, 2),
            disconnect => (LengthStyle::Variable, 2),
            channelMsg => (LengthStyle::Variable, 2),
            onceOffMsg => (LengthStyle::Variable, 2),
        }
    }

    mercury_interface! {
        /// `LoginInterface`, the external client-facing login registry
        /// (`lib/connection/login_interface.hpp`): `login` and
        /// `challengeResponse` are `MERCURY_VARIABLE_MESSAGE( .., 2, .. )`
        /// and `probe` is `MERCURY_EMPTY_MESSAGE`.
        pub mod login_interface {
            name: "LoginInterface",
            login => (LengthStyle::Variable, 2),
            probe => (LengthStyle::Fixed, 0),
            challengeResponse => (LengthStyle::Variable, 2),
        }
    }

    /// `CommonInterface`'s id and length table, in declaration order: id,
    /// name, style, param, header, nominal body, stream size.
    const COMMON_TABLE: [(&str, LengthStyle, i32, i32, i32, i32); 4] = [
        ("msg1", LengthStyle::Fixed, 12, 1, 12, 12),
        ("msgVar1", LengthStyle::Variable, 1, 2, 0, -1),
        ("msgVar2", LengthStyle::Variable, 4, 5, 0, -4),
        ("disconnect", LengthStyle::Fixed, 4, 1, 4, 4),
    ];

    #[test]
    fn the_common_interface_matches_the_reference_registry() {
        let minder = common_interface::interface();
        assert_eq!(minder.len(), COMMON_TABLE.len());

        for (id, expected) in COMMON_TABLE.iter().enumerate() {
            let element = minder.element(id as MessageId);
            let (name, style, param, header, body, stream) = *expected;
            assert_eq!(element.id(), id as MessageId);
            assert_eq!(element.name(), name);
            assert_eq!(element.length_style(), style);
            assert_eq!(element.length_param(), param);
            assert_eq!(element.header_size(), header, "{}", name);
            assert_eq!(element.nominal_body_size(), body, "{}", name);
            assert_eq!(element.stream_size(), stream, "{}", name);
        }
    }

    #[test]
    fn the_internal_fragment_interface_matches_the_reference_registry() {
        let minder = fragment_server_interface::interface();
        assert_eq!(minder.len(), 4);
        assert_eq!(minder.name(), "FragmentServerInterface");

        // Every BW_VARLEN_MSG message is variable with a 2-byte length
        // header, numbered in declaration order.
        for (id, name) in ["connect", "disconnect", "channelMsg", "onceOffMsg"]
            .into_iter()
            .enumerate()
        {
            let element = minder.element(id as MessageId);
            assert_eq!(element.name(), name);
            assert_eq!(element.length_style(), LengthStyle::Variable);
            assert_eq!(element.length_param(), 2);
            assert_eq!(element.header_size(), 3);
            assert_eq!(element.stream_size(), -2);
        }

        assert_eq!(minder.find("channelMsg").map(InterfaceElement::id), Some(2));
    }

    #[test]
    fn the_external_login_interface_matches_the_reference_registry() {
        let minder = login_interface::interface();
        assert_eq!(minder.len(), 3);
        assert_eq!(minder.name(), "LoginInterface");

        // MERCURY_VARIABLE_MESSAGE( login, 2, .. )
        let login = minder.element(0);
        assert_eq!(login.name(), "login");
        assert_eq!(login.length_style(), LengthStyle::Variable);
        assert_eq!(login.length_param(), 2);
        assert_eq!(login.header_size(), 3);

        // MERCURY_EMPTY_MESSAGE( probe, .. ): fixed, zero-length body, so
        // the whole message is the one-byte identifier.
        let probe = minder.element(1);
        assert_eq!(probe.name(), "probe");
        assert_eq!(probe.length_style(), LengthStyle::Fixed);
        assert_eq!(probe.length_param(), 0);
        assert_eq!(probe.header_size(), 1);
        assert_eq!(probe.nominal_body_size(), 0);
        assert_eq!(probe.stream_size(), 0);

        // MERCURY_VARIABLE_MESSAGE( challengeResponse, 2, .. )
        let challenge = minder.element(2);
        assert_eq!(challenge.name(), "challengeResponse");
        assert_eq!(challenge.length_style(), LengthStyle::Variable);
        assert_eq!(challenge.length_param(), 2);
    }

    #[test]
    fn add_numbers_elements_sequentially_from_zero() {
        let mut minder = InterfaceMinder::new("Test");
        assert!(minder.is_empty());

        assert_eq!(minder.add("first", LengthStyle::Fixed, 8), 0);
        assert_eq!(minder.add("second", LengthStyle::Variable, 2), 1);
        assert_eq!(minder.add("third", LengthStyle::Callback, 0), 2);

        assert_eq!(minder.len(), 3);
        assert_eq!(minder.element(0).name(), "first");
        assert_eq!(minder.element(1).name(), "second");

        // The callback style has no computable header, and a negative
        // stream size is reserved for variable messages only.
        let third = minder.element(2);
        assert_eq!(third.header_size(), -1);
        assert_eq!(third.stream_size(), 0);
        assert_eq!(third.nominal_body_size(), 0);
    }

    #[test]
    fn add_range_consumes_the_remaining_range_as_the_reference_does() {
        // A base element takes id 0; addRange then fills
        // start_id..=start_id + (254 - start_id)/range_portion.
        let mut minder = InterfaceMinder::new("RangeTest");
        let base = minder.add("rangeMsg", LengthStyle::Variable, 2);
        assert_eq!(base, 0);

        // range_portion 2: end = 1 + (254 - 1)/2 = 1 + 126 = 127.
        let end = minder.add_range(base, 2);
        assert_eq!(end, 127);
        assert_eq!(minder.len(), 128);
        assert_eq!(minder.element(127).id(), 127);
        assert_eq!(minder.element(127).name(), "rangeMsg");
        assert_eq!(minder.element(127).length_param(), 2);

        // A second range starts after everything added so far.
        let base2 = minder.add("secondRange", LengthStyle::Fixed, 0);
        assert_eq!(base2, 128);
        // range_portion 4: start = 129, end = 129 + (254-129)/4 = 129 + 31.
        let end2 = minder.add_range(base2, 4);
        assert_eq!(end2, 160);
        assert_eq!(minder.len(), 161);
    }

    #[test]
    fn the_header_width_bounds_the_lengths_it_can_handle() {
        // Static canHandleLength: width N holds lengths below 2^(8N) - 1.
        assert!(InterfaceElement::header_can_handle_length(254, 1));
        assert!(!InterfaceElement::header_can_handle_length(255, 1));
        assert!(InterfaceElement::header_can_handle_length(65_534, 2));
        assert!(!InterfaceElement::header_can_handle_length(65_535, 2));
        // A 4-byte header never overflows, whatever the length.
        assert!(InterfaceElement::header_can_handle_length(i32::MAX, 4));

        // The element form: fixed messages always pass, variable messages
        // defer to their width, and a 4-byte variable width always passes.
        let fixed = InterfaceElement::new("f", 0, LengthStyle::Fixed, 0);
        assert!(fixed.can_handle_length(i32::MAX));
        let var1 = InterfaceElement::new("v1", 1, LengthStyle::Variable, 1);
        assert!(var1.can_handle_length(254));
        assert!(!var1.can_handle_length(255));
        let var4 = InterfaceElement::new("v4", 2, LengthStyle::Variable, 4);
        assert!(var4.can_handle_length(i32::MAX));
    }

    #[test]
    fn the_reply_element_is_variable_length_with_a_four_byte_width() {
        let reply = InterfaceElement::reply();
        assert_eq!(reply.id(), REPLY_MESSAGE_IDENTIFIER);
        assert_eq!(reply.name(), "Reply");
        assert_eq!(reply.length_style(), LengthStyle::Variable);
        assert_eq!(reply.length_param(), 4);
        assert_eq!(reply.header_size(), 5);
        assert!(reply.can_handle_length(i32::MAX));
    }

    #[test]
    fn the_length_style_discriminants_are_pinned_to_the_reference_constants() {
        assert_eq!(LengthStyle::Fixed as i8, FIXED_LENGTH_MESSAGE);
        assert_eq!(LengthStyle::Variable as i8, VARIABLE_LENGTH_MESSAGE);
        assert_eq!(LengthStyle::Callback as i8, CALLBACK_LENGTH_MESSAGE);
        assert_eq!(LengthStyle::Invalid as i8, INVALID_MESSAGE);
        assert_eq!(FIXED_LENGTH_MESSAGE, 0);
        assert_eq!(VARIABLE_LENGTH_MESSAGE, 1);
        assert_eq!(CALLBACK_LENGTH_MESSAGE, 2);
        assert_eq!(INVALID_MESSAGE, 3);
        assert_eq!(DEFAULT_VARIABLE_LENGTH_HEADER_SIZE, 1);
        assert_eq!(INVALID_STREAM_SIZE, i32::MIN);
    }

    #[test]
    fn elements_default_to_not_early_and_can_be_marked_early() {
        let mut element = InterfaceElement::new("early", 0, LengthStyle::Fixed, 0);
        assert!(!element.should_process_early());
        element.set_should_process_early(true);
        assert!(element.should_process_early());
    }
}
