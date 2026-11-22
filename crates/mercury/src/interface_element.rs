//! `Mercury::InterfaceElement` and `InterfaceElementWithStats`
//! (`lib/network/interface_element.hpp/.cpp`).
//!
//! An element describes one message of an interface: its id, its name, and
//! how to read its header. Fixed-length messages carry their body size in
//! `lengthParam`; variable-length ones carry the number of bytes the body
//! length occupies; callback messages defer the decision to a handler.
//!
//! The framing side (`compressLength`, `expandLength`,
//! `unexpandLength`, and the `special*` helpers) lives in
//! [`crate::message_framing`]; this module owns the id and length tables the
//! framing reads.

use std::cell::Cell;
use std::rc::Rc;

use foundation::math::ema::AccumulatingEma;
use foundation::math::ema::Ema;

use crate::interfaces::InputMessageHandler;
use crate::misc::MessageID;

/// `FIXED_LENGTH_MESSAGE`: `lengthParam_` is the body size.
pub const FIXED_LENGTH_MESSAGE: i8 = 0;
/// `VARIABLE_LENGTH_MESSAGE`: `lengthParam_` is the size of the length
/// field.
pub const VARIABLE_LENGTH_MESSAGE: i8 = 1;
/// `CALLBACK_LENGTH_MESSAGE`: the handler decides the size.
pub const CALLBACK_LENGTH_MESSAGE: i8 = 2;
/// `INVALID_MESSAGE`: the element is not initialised.
pub const INVALID_MESSAGE: i8 = 3;

/// `INVALID_STREAM_SIZE` (`std::numeric_limits< int >::min()`): a callback
/// handler's answer meaning "this bundle is invalid".
pub const INVALID_STREAM_SIZE: i32 = i32::MIN;
/// `DEFAULT_VARIABLE_LENGTH_HEADER_SIZE`.
pub const DEFAULT_VARIABLE_LENGTH_HEADER_SIZE: u32 = 1;
/// `IDENTIFIER_SIZE`: one message id byte.
pub const IDENTIFIER_SIZE: usize = 1;
/// `REPLY_MESSAGE_IDENTIFIER`: the id the internal NUB handles for replies.
pub const REPLY_MESSAGE_IDENTIFIER: u8 = 0xFF;

/// `lengthStyle_` of an [`InterfaceElement`]. The reference stores a raw
/// `int8`, but only these four values exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i8)]
pub enum LengthStyle {
    /// `FIXED_LENGTH_MESSAGE`.
    Fixed = FIXED_LENGTH_MESSAGE,
    /// `VARIABLE_LENGTH_MESSAGE`.
    Variable = VARIABLE_LENGTH_MESSAGE,
    /// `CALLBACK_LENGTH_MESSAGE`.
    Callback = CALLBACK_LENGTH_MESSAGE,
    /// `INVALID_MESSAGE`.
    Invalid = INVALID_MESSAGE,
}

impl From<i8> for LengthStyle {
    fn from(value: i8) -> Self {
        match value {
            FIXED_LENGTH_MESSAGE => Self::Fixed,
            VARIABLE_LENGTH_MESSAGE => Self::Variable,
            CALLBACK_LENGTH_MESSAGE => Self::Callback,
            _ => Self::Invalid,
        }
    }
}

impl LengthStyle {
    /// The raw `int8` the reference compares against.
    pub fn as_i8(&self) -> i8 {
        *self as i8
    }
}

/// `InterfaceElement`: how one message's header is encoded.
#[derive(Clone)]
pub struct InterfaceElement {
    /// `id_`: the unique message id.
    id: MessageID,
    /// `lengthStyle_`.
    length_style: LengthStyle,
    /// `lengthParam_`: what it means depends on the style.
    length_param: i32,
    /// `name_`: the interface method name.
    name: &'static str,
    /// `pHandler_`.
    handler: Option<Rc<dyn InputMessageHandler>>,
    /// `shouldProcessEarly_`: `mutable` in the reference, so the flag is a
    /// cell here.
    should_process_early: Cell<bool>,
}

impl InterfaceElement {
    /// `InterfaceElement( name, id, lengthStyle, lengthParam, pHandler )`.
    pub fn new(
        name: &'static str,
        id: MessageID,
        length_style: i8,
        length_param: i32,
        handler: Option<Rc<dyn InputMessageHandler>>,
    ) -> Self {
        Self {
            id,
            length_style: LengthStyle::from(length_style),
            length_param,
            name,
            handler,
            should_process_early: Cell::new(false),
        }
    }

    /// `set`: the four header fields, leaving the handler alone.
    pub fn set(&mut self, name: &'static str, id: MessageID, length_style: i8, length_param: i32) {
        self.id = id;
        self.length_style = LengthStyle::from(length_style);
        self.length_param = length_param;
        self.name = name;
    }

    /// `updateLengthDetails`: ask the callback handler to size this message
    /// and fold the answer into the style. Returns false when the handler
    /// says the bundle is invalid and processing must stop.
    ///
    /// The reference passes `( networkInterface, srcAddr, id )` to the
    /// handler; see [`InputMessageHandler`].
    pub fn update_length_details(&mut self, handler: &dyn InputMessageHandler) -> bool {
        if self.length_style != LengthStyle::Callback {
            return true;
        }

        let stream_size = handler.get_message_stream_size(self.id);

        if stream_size == INVALID_STREAM_SIZE {
            return false;
        }

        if stream_size < 0 {
            self.length_style = LengthStyle::Variable;
            self.length_param = -stream_size;
        } else {
            self.length_style = LengthStyle::Fixed;
            self.length_param = stream_size;
        }

        true
    }

    /// `headerSize`: the bytes this message's header occupies, or -1 when
    /// the length is not yet known.
    pub fn header_size(&self) -> i32 {
        let mut header_len: i32 = IDENTIFIER_SIZE as i32;
        match self.length_style {
            LengthStyle::Fixed => {}
            LengthStyle::Variable => header_len += self.length_param,
            _ => header_len = -1,
        }
        header_len
    }

    /// `nominalBodySize`: never guesses for variable-length messages.
    pub fn nominal_body_size(&self) -> i32 {
        if self.length_style == LengthStyle::Fixed {
            self.length_param
        } else {
            0
        }
    }

    /// `streamSize`: negative for a variable message, whose magnitude is
    /// the length-header size.
    pub fn stream_size(&self) -> i32 {
        if self.length_style == LengthStyle::Variable {
            -self.length_param
        } else {
            self.length_param
        }
    }

    /// `canHandleLength( length, lengthHeaderSize )`: whether the length
    /// fits the presumed variable header.
    pub fn can_handle_length(length: i32, length_header_size: i32) -> bool {
        debug_assert!(
            length_header_size <= 4,
            "canHandleLength: lengthHeaderSize must be at most 4"
        );

        if length_header_size == 4 {
            // Prevent overflow below, true for all values of (int) length.
            return true;
        }

        length < ((1 << (8 * length_header_size)) - 1)
    }

    /// `canHandleLength( length )`: this element's own header.
    pub fn can_handle_length_for(&self, length: i32) -> bool {
        self.length_style != LengthStyle::Variable
            || self.length_param >= 4
            || Self::can_handle_length(length, self.length_param)
    }

    /// `id()`.
    pub fn id(&self) -> MessageID {
        self.id
    }

    /// `id( id )`.
    pub fn set_id(&mut self, id: MessageID) {
        self.id = id;
    }

    /// `lengthStyle()`.
    pub fn length_style(&self) -> i8 {
        self.length_style.as_i8()
    }

    /// `lengthParam()`.
    pub fn length_param(&self) -> i32 {
        self.length_param
    }

    /// `name()`.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// `c_str()`: `"name/id"`.
    ///
    /// The reference formats into a function-local `static char buf[ 256 ]`,
    /// so two calls share one buffer; a returned owned string has no such
    /// aliasing.
    pub fn c_str(&self) -> String {
        format!("{}/{}", self.name, self.id)
    }

    /// `pHandler()`.
    pub fn handler(&self) -> Option<Rc<dyn InputMessageHandler>> {
        self.handler.clone()
    }

    /// `pHandler( pHandler )`.
    pub fn set_handler(&mut self, handler: Option<Rc<dyn InputMessageHandler>>) {
        self.handler = handler;
    }

    /// Whether this message may be processed out of order (early).
    pub fn should_process_early(&self) -> bool {
        self.should_process_early.get()
    }

    /// Set whether this message may be processed out of order (early).
    pub fn set_should_process_early(&self, new_value: bool) {
        self.should_process_early.set(new_value);
    }

    /// `InterfaceElement::REPLY`: the reply element the internal NUB
    /// handles, 4 length bytes.
    ///
    /// The reference keeps this as a `static const` member; here it is a
    /// constructor because the element carries a shared handle rather
    /// than a raw handler pointer.
    pub fn reply() -> Self {
        Self::new(
            "Reply",
            REPLY_MESSAGE_IDENTIFIER,
            VARIABLE_LENGTH_MESSAGE,
            4,
            None,
        )
    }
}

impl std::fmt::Debug for InterfaceElement {
    /// The handler is a shared handle without a stable identity, so it is
    /// reported only by presence.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InterfaceElement")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("length_style", &self.length_style.as_i8())
            .field("length_param", &self.length_param)
            .field("has_handler", &self.handler.is_some())
            .finish()
    }
}

impl std::fmt::Debug for InterfaceElementWithStats {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InterfaceElementWithStats")
            .field("element", &self.element)
            .field("num_messages_received", &self.num_messages_received)
            .field("num_bytes_received", &self.num_bytes_received)
            .field("max_bytes_received", &self.max_bytes_received)
            .finish()
    }
}

/// `InterfaceElementWithStats`: an element plus the receive counters the
/// interface table reports once a second.
#[derive(Clone)]
pub struct InterfaceElementWithStats {
    /// `maxBytesReceived_`.
    max_bytes_received: u32,
    /// `numBytesReceived_`.
    num_bytes_received: u32,
    /// `numMessagesReceived_`.
    num_messages_received: u32,
    /// `avgBytesReceivedPerSecond_`.
    avg_bytes_received_per_second: AccumulatingEma<u32>,
    /// `avgMessagesReceivedPerSecond_`.
    avg_messages_received_per_second: AccumulatingEma<u32>,
    /// The element itself.
    element: InterfaceElement,
}

impl InterfaceElementWithStats {
    /// `InterfaceElementWithStats()`: zeroed counters, both averages
    /// starting from the bias the reference derives from 5 samples.
    pub fn new() -> Self {
        let bias = Self::average_bias();
        Self {
            max_bytes_received: 0,
            num_bytes_received: 0,
            num_messages_received: 0,
            avg_bytes_received_per_second: AccumulatingEma::new(bias, AVERAGE_INITIAL, 0),
            avg_messages_received_per_second: AccumulatingEma::new(bias, AVERAGE_INITIAL, 0),
            element: InterfaceElement::new("", 0, INVALID_MESSAGE, 0, None),
        }
    }

    /// `AVERAGE_BIAS`.
    pub fn average_bias() -> f32 {
        Ema::calculate_bias_from_num_samples(5.0, DEFAULT_WEIGHTING)
    }

    /// The element this entry describes.
    pub fn element(&self) -> &InterfaceElement {
        &self.element
    }

    /// `element_mut`: the table writes through to the element.
    pub fn element_mut(&mut self) -> &mut InterfaceElement {
        &mut self.element
    }

    /// `tick`: called once a second by the interface table.
    pub fn tick(&mut self) {
        self.avg_bytes_received_per_second.sample(true);
        self.avg_messages_received_per_second.sample(true);
    }

    /// `messageReceived`: the counters the reference bumps per message.
    ///
    /// The reference reaches this only through `stopProfile`, which is
    /// watcher-gated; it is exposed so the statistics are observable.
    pub fn message_received(&mut self, msg_len: u32) {
        self.num_messages_received += 1;
        self.avg_messages_received_per_second.increment_value();
        self.num_bytes_received += msg_len;
        self.avg_bytes_received_per_second.add_to_value(msg_len);
        if msg_len > self.max_bytes_received {
            self.max_bytes_received = msg_len;
        }
    }

    /// `maxBytesReceived()`.
    pub fn max_bytes_received(&self) -> u32 {
        self.max_bytes_received
    }

    /// `numBytesReceived()`.
    pub fn num_bytes_received(&self) -> u32 {
        self.num_bytes_received
    }

    /// `numMessagesReceived()`.
    pub fn num_messages_received(&self) -> u32 {
        self.num_messages_received
    }

    /// `avgMessagesReceivedPerSecond()`.
    pub fn avg_messages_received_per_second(&self) -> f32 {
        self.avg_messages_received_per_second.average()
    }

    /// `avgBytesReceivedPerSecond()`.
    pub fn avg_bytes_received_per_second(&self) -> f32 {
        self.avg_bytes_received_per_second.average()
    }

    /// `avgMessageLength()`.
    pub fn avg_message_length(&self) -> f32 {
        if self.num_messages_received != 0 {
            self.num_bytes_received as f32 / self.num_messages_received as f32
        } else {
            0.0
        }
    }
}

impl Default for InterfaceElementWithStats {
    fn default() -> Self {
        Self::new()
    }
}

/// The initial average the statistics start from (`AccumulatingEMA`'s
/// default `initialAverage`).
const AVERAGE_INITIAL: f32 = 0.0;
/// `EMA::calculateBiasFromNumSamples`'s default weighting.
const DEFAULT_WEIGHTING: f32 = 0.95;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::{Address, NONE};
    use crate::unpacked_message_header::UnpackedMessageHeader;
    use foundation::stream::BinaryStream;

    /// A handler that reports a fixed or a variable stream size.
    struct SizingHandler {
        stream_size: i32,
    }

    impl InputMessageHandler for SizingHandler {
        fn handle_message(
            &self,
            _source: &Address,
            _header: &mut UnpackedMessageHeader,
            _data: &mut BinaryStream,
        ) {
        }

        fn get_message_stream_size(&self, _msg_id: MessageID) -> i32 {
            self.stream_size
        }
    }

    fn callback_element(handler: i32) -> InterfaceElement {
        InterfaceElement::new(
            "callback",
            7,
            CALLBACK_LENGTH_MESSAGE,
            0,
            Some(Rc::new(SizingHandler {
                stream_size: handler,
            })),
        )
    }

    /// The length-style constants keep the reference's values because they
    /// appear in the wire-facing tables.
    #[test]
    fn the_length_styles_keep_the_reference_values() {
        assert_eq!(FIXED_LENGTH_MESSAGE, 0);
        assert_eq!(VARIABLE_LENGTH_MESSAGE, 1);
        assert_eq!(CALLBACK_LENGTH_MESSAGE, 2);
        assert_eq!(INVALID_MESSAGE, 3);
        assert_eq!(DEFAULT_VARIABLE_LENGTH_HEADER_SIZE, 1);
        assert_eq!(IDENTIFIER_SIZE, 1);
        assert_eq!(REPLY_MESSAGE_IDENTIFIER, 0xFF);
        assert_eq!(INVALID_STREAM_SIZE, i32::MIN);

        assert_eq!(LengthStyle::from(0), LengthStyle::Fixed);
        assert_eq!(LengthStyle::from(1), LengthStyle::Variable);
        assert_eq!(LengthStyle::from(2), LengthStyle::Callback);
        assert_eq!(LengthStyle::from(3), LengthStyle::Invalid);
        assert_eq!(
            LengthStyle::from(9),
            LengthStyle::Invalid,
            "unknown values are invalid"
        );
        assert_eq!(LengthStyle::Fixed.as_i8(), 0);
    }

    /// Non-callback messages pass `updateLengthDetails` straight through.
    #[test]
    fn a_non_callback_message_needs_no_update() {
        let mut element = InterfaceElement::new(
            "probe",
            1,
            FIXED_LENGTH_MESSAGE,
            0,
            Some(Rc::new(SizingHandler { stream_size: 9 })),
        );
        assert!(element.update_length_details(&SizingHandler { stream_size: 9 }));
        assert_eq!(element.length_style(), FIXED_LENGTH_MESSAGE, "untouched");
        assert_eq!(element.length_param(), 0);
    }

    /// The sizing stub's delivery half is a no-op: the element only needs
    /// the sizing half, so both halves of the handler are exercised.
    #[test]
    fn a_sizing_handler_also_delivers() {
        let handler = SizingHandler { stream_size: 9 };
        let source = NONE;
        let mut header = UnpackedMessageHeader::default();
        let mut data = BinaryStream::new();
        InputMessageHandler::handle_message(&handler, &source, &mut header, &mut data);
    }

    /// A callback answer sizes the message: negative means variable, with
    /// the magnitude as the length-header size; non-negative means fixed.
    #[test]
    fn a_callback_answer_becomes_the_length() {
        let mut negative = callback_element(-2);
        assert!(negative.update_length_details(&SizingHandler { stream_size: -2 }));
        assert_eq!(negative.length_style(), VARIABLE_LENGTH_MESSAGE);
        assert_eq!(negative.length_param(), 2, "magnitude of the answer");
        assert_eq!(negative.stream_size(), -2);

        let mut positive = callback_element(64);
        assert!(positive.update_length_details(&SizingHandler { stream_size: 64 }));
        assert_eq!(positive.length_style(), FIXED_LENGTH_MESSAGE);
        assert_eq!(positive.length_param(), 64);
        assert_eq!(positive.stream_size(), 64);
    }

    /// `INVALID_STREAM_SIZE` rejects the bundle.
    #[test]
    fn an_invalid_stream_size_rejects_the_bundle() {
        let mut element = callback_element(INVALID_STREAM_SIZE);
        assert!(!element.update_length_details(&SizingHandler {
            stream_size: INVALID_STREAM_SIZE
        }));
        assert_eq!(
            element.length_style(),
            CALLBACK_LENGTH_MESSAGE,
            "nothing is written"
        );
    }

    /// `headerSize` is one id byte plus the length bytes for variable
    /// messages, and -1 while the length is still unknown.
    #[test]
    fn the_header_size_depends_on_the_style() {
        let fixed = InterfaceElement::new("m", 0, FIXED_LENGTH_MESSAGE, 12, None);
        assert_eq!(fixed.header_size(), 1);

        let variable = InterfaceElement::new("m", 0, VARIABLE_LENGTH_MESSAGE, 4, None);
        assert_eq!(variable.header_size(), 5);

        let callback = InterfaceElement::new("m", 0, CALLBACK_LENGTH_MESSAGE, 0, None);
        assert_eq!(callback.header_size(), -1);

        let invalid = InterfaceElement::new("m", 0, INVALID_MESSAGE, 0, None);
        assert_eq!(invalid.header_size(), -1, "the reference's default arm");
    }

    /// `nominalBodySize` refuses to guess for variable messages.
    #[test]
    fn the_nominal_body_size_never_guesses() {
        assert_eq!(
            InterfaceElement::new("m", 0, FIXED_LENGTH_MESSAGE, 12, None).nominal_body_size(),
            12
        );
        assert_eq!(
            InterfaceElement::new("m", 0, VARIABLE_LENGTH_MESSAGE, 2, None).nominal_body_size(),
            0
        );
        assert_eq!(
            InterfaceElement::new("m", 0, CALLBACK_LENGTH_MESSAGE, 0, None).nominal_body_size(),
            0
        );
    }

    /// The stream-size sign carries the style.
    #[test]
    fn the_stream_size_is_signed_by_the_style() {
        assert_eq!(
            InterfaceElement::new("m", 0, FIXED_LENGTH_MESSAGE, 8, None).stream_size(),
            8
        );
        assert_eq!(
            InterfaceElement::new("m", 0, VARIABLE_LENGTH_MESSAGE, 2, None).stream_size(),
            -2
        );
    }

    /// `canHandleLength` admits everything a 4-byte header can hold, and
    /// otherwise the length must fit the smaller field.
    #[test]
    fn the_length_fits_the_header_it_is_given() {
        assert!(
            InterfaceElement::can_handle_length(i32::MAX, 4),
            "4 bytes hold any int"
        );
        assert!(InterfaceElement::can_handle_length(i32::MIN, 4));

        assert!(InterfaceElement::can_handle_length(254, 1));
        assert!(
            !InterfaceElement::can_handle_length(255, 1),
            "2^8 - 1 is the bound"
        );
        assert!(InterfaceElement::can_handle_length(65534, 2));
        assert!(!InterfaceElement::can_handle_length(65535, 2));
        assert!(
            InterfaceElement::can_handle_length(-1, 1),
            "negative lengths fit"
        );

        let variable = InterfaceElement::new("m", 0, VARIABLE_LENGTH_MESSAGE, 2, None);
        assert!(variable.can_handle_length_for(65534));
        assert!(!variable.can_handle_length_for(65535));
        // A 4-byte length header holds every int: the reference short-circuits
        // before computing.
        let big = InterfaceElement::new("m", 0, VARIABLE_LENGTH_MESSAGE, 4, None);
        assert!(big.can_handle_length_for(i32::MIN));
        // Non-variable messages never limit the length.
        let fixed = InterfaceElement::new("m", 0, FIXED_LENGTH_MESSAGE, 0, None);
        assert!(fixed.can_handle_length_for(999_999));
    }

    /// `set` rewrites the header fields and leaves the handler alone;
    /// `c_str` names the message as `name/id`.
    #[test]
    fn set_rewrites_the_header_fields() {
        let mut element = InterfaceElement::new(
            "old",
            3,
            FIXED_LENGTH_MESSAGE,
            0,
            Some(Rc::new(SizingHandler { stream_size: 1 })),
        );
        element.set("new", 9, VARIABLE_LENGTH_MESSAGE, 2);

        assert_eq!(element.name(), "new");
        assert_eq!(element.id(), 9);
        assert_eq!(element.length_style(), VARIABLE_LENGTH_MESSAGE);
        assert_eq!(element.length_param(), 2);
        assert!(element.handler().is_some(), "the handler survives");
        assert_eq!(element.c_str(), "new/9");

        element.set_id(10);
        assert_eq!(element.c_str(), "new/10");
        element.set_should_process_early(true);
        assert!(element.should_process_early());
    }

    /// The reply element is the one Mercury handles internally: variable
    /// with 4 length bytes, id 0xFF.
    #[test]
    fn the_reply_element_matches_the_reference() {
        let reply = InterfaceElement::reply();
        assert_eq!(reply.name(), "Reply");
        assert_eq!(reply.id(), REPLY_MESSAGE_IDENTIFIER);
        assert_eq!(reply.length_style(), VARIABLE_LENGTH_MESSAGE);
        assert_eq!(reply.length_param(), 4);
        assert_eq!(reply.header_size(), 5);
        assert_eq!(reply.stream_size(), -4);
        assert!(reply.handler().is_none());
    }

    /// The receive counters and their per-second averages.
    #[test]
    fn the_statistics_count_and_average_the_traffic() {
        let mut entry = InterfaceElementWithStats::new();
        assert_eq!(entry.num_messages_received(), 0);
        assert_eq!(entry.avg_message_length(), 0.0, "no messages yet");

        entry.message_received(4);
        entry.message_received(8);
        assert_eq!(entry.num_messages_received(), 2);
        assert_eq!(entry.num_bytes_received(), 12);
        assert_eq!(entry.max_bytes_received(), 8);
        // A smaller message does not displace the largest one seen.
        entry.message_received(2);
        assert_eq!(entry.max_bytes_received(), 8);
        assert_eq!(entry.num_messages_received(), 3);
        assert_eq!(entry.num_bytes_received(), 14);
        assert_eq!(entry.avg_message_length(), 14.0 / 3.0);
        // Nothing sampled yet, so the averages are still their initial 0.
        assert_eq!(entry.avg_bytes_received_per_second(), 0.0);

        let bias = InterfaceElementWithStats::average_bias();
        entry.tick();
        // A fresh average starts at 0, so the first sample is only the
        // non-biased part of the accumulated total.
        assert_eq!(entry.avg_bytes_received_per_second(), 14.0 * (1.0 - bias));
        assert_eq!(entry.avg_messages_received_per_second(), 3.0 * (1.0 - bias));

        // Sampled and reset: the next tick sees nothing new, so the
        // average decays by the bias.
        let previous = entry.avg_bytes_received_per_second();
        entry.tick();
        assert_eq!(entry.avg_bytes_received_per_second(), previous * bias);
    }

    /// The bias the averages start from is the 5-sample default weighting.
    #[test]
    fn the_average_bias_comes_from_five_samples() {
        let bias = InterfaceElementWithStats::average_bias();
        assert!((bias - 0.54928f32).abs() < 1e-6, "{bias}");

        let defaulted = InterfaceElementWithStats::default();
        assert_eq!(defaulted.max_bytes_received(), 0);
        assert_eq!(defaulted.element().length_style(), INVALID_MESSAGE);
    }

    /// Both elements format for the diagnostics that name them, reporting
    /// the handler only by presence because a shared handle has no stable
    /// identity.
    #[test]
    fn the_elements_report_their_header_fields() {
        let element = InterfaceElement::new(
            "probe",
            4,
            FIXED_LENGTH_MESSAGE,
            0,
            Some(Rc::new(SizingHandler { stream_size: 0 })),
        );
        assert_eq!(
            format!("{element:?}"),
            "InterfaceElement { id: 4, name: \"probe\", length_style: 0, \
             length_param: 0, has_handler: true }"
        );

        let bare = InterfaceElement::new("reply", 5, VARIABLE_LENGTH_MESSAGE, 2, None);
        assert_eq!(
            format!("{bare:?}"),
            "InterfaceElement { id: 5, name: \"reply\", length_style: 1, \
             length_param: 2, has_handler: false }"
        );

        let mut entry = InterfaceElementWithStats::new();
        *entry.element_mut() = element;
        entry.message_received(6);
        assert_eq!(
            format!("{entry:?}"),
            "InterfaceElementWithStats { element: InterfaceElement { id: 4, \
             name: \"probe\", length_style: 0, length_param: 0, has_handler: true }, \
             num_messages_received: 1, num_bytes_received: 6, max_bytes_received: 6 }"
        );
    }
}
