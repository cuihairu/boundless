//! Fixed and variable length message framing: `InterfaceElement::compressLength`,
//! `expandLength` and `unexpandLength`
//! (`lib/network/interface_element.cpp:201-596`).
//!
//! A message on the wire is
//!
//! ```text
//! request:   [ id:1 ][ length:lengthParam ][ replyId:4 ][ nextRequest:2 ][ body ]
//! non-request: [ id:1 ][ length:lengthParam ][ body ]
//! ```
//!
//! `lengthParam` is the body size for a fixed message and the width in bytes
//! of the length field for a variable one. Widths 1 to 3 carry the length in
//! little-endian bytes; a body that no longer fits those fields is written in
//! the special form -- the length field set to all ones, the true length as a
//! 4-byte little-endian value at the head of the body, and the displaced first
//! four body bytes appended to the end.
//!
//! The reference does all of this through `BundleDataPos`, which walks a chain
//! of packets. This port frames inside one contiguous buffer, which is the
//! single-packet case: a message longer than a packet does not arrive until the
//! channel layer of BW-02-03.

use crate::interface_element::InterfaceElement;
use crate::interface_element::{FIXED_LENGTH_MESSAGE, IDENTIFIER_SIZE, VARIABLE_LENGTH_MESSAGE};

/// The width of the `ReplyID` the request preamble carries. `ReplyID` is an
/// `int32` in the reference, not the one byte a reply id might look like.
pub const REPLY_ID_SIZE: usize = 4;
/// The width of the next-request link (`Packet::Offset`, a `uint16`).
pub const NEXT_REQUEST_OFFSET_SIZE: usize = 2;
/// Both of them: how far past the length field a request's body starts.
pub const REQUEST_PREAMBLE_SIZE: usize = REPLY_ID_SIZE + NEXT_REQUEST_OFFSET_SIZE;
/// The width of the length the special form carries at the head of the body.
pub const SPECIAL_LENGTH_SIZE: usize = 4;

/// The one thing framing can refuse to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// `compressLength`: a negative body length on a variable message.
    NegativeLength,
    /// `expandLength`: a 4-byte length field with the top bit set.
    Overflow,
    /// The length style is not one framing knows (`CALLBACK_LENGTH_MESSAGE`
    /// before it has been resolved, or `INVALID_MESSAGE`).
    UnknownLengthStyle,
    /// The frame, or the four spare bytes the special form needs, does not
    /// fit inside the buffer handed in.
    BufferTooSmall,
}

impl InterfaceElement {
    /// The offset of the first body byte inside a message frame: the id byte,
    /// the length field, and for a request the reply id and the next-request
    /// link.
    ///
    /// A fixed message has no length field, which is exactly what
    /// `headerSize()` reports as 1 for it.
    pub fn body_offset(&self, is_request: bool) -> usize {
        let length_field = if self.length_style() == VARIABLE_LENGTH_MESSAGE {
            self.length_param().max(0) as usize
        } else {
            0
        };

        IDENTIFIER_SIZE + length_field + usize::from(is_request) * REQUEST_PREAMBLE_SIZE
    }

    /// `compressLength( data, length, pBundle, isRequest )`: write `length`
    /// into the header at `data`.
    ///
    /// The reference returns 0 on success and -1 otherwise. Fixed messages
    /// write nothing: the reference logs a `CRITICAL_MSG` when the length does
    /// not match and still returns 0, so this does the same.
    ///
    /// For the special form the reference calls `pBundle->reserve( 4 )` first
    /// and then writes four bytes past the end of the body; here the caller
    /// hands in a buffer with four spare bytes and [`FrameError::BufferTooSmall`]
    /// plays the part of the reference's -1.
    pub fn compress_length(
        &self,
        data: &mut [u8],
        length: i32,
        is_request: bool,
    ) -> Result<(), FrameError> {
        match self.length_style() {
            FIXED_LENGTH_MESSAGE => {
                if length != self.length_param() {
                    // The reference logs
                    // "Fixed length message has wrong length" and returns 0.
                    debug_assert_eq!(
                        length,
                        self.length_param(),
                        "{}: fixed length message has wrong length {} instead of {}",
                        self.c_str(),
                        length,
                        self.length_param()
                    );
                }
                Ok(())
            }

            VARIABLE_LENGTH_MESSAGE => {
                if length < 0 {
                    return Err(FrameError::NegativeLength);
                }

                match self.length_param() {
                    1..=3 => {
                        if self.fits_length(length) {
                            self.write_length_field(data, length);
                        } else {
                            // Set the length field to all ones to mark the
                            // special situation, starting after the id byte.
                            for byte in &mut data[1..=self.length_param() as usize] {
                                *byte = 0xff;
                            }

                            return self.special_compress_length(data, length, is_request);
                        }
                    }
                    // Width 4 cannot overflow, so it always writes.
                    4 => self.write_length_field(data, length),
                    // The reference's default arm logs
                    // "Unsupported variable length width" and returns 0.
                    _ => {}
                }

                Ok(())
            }

            _ => Err(FrameError::UnknownLengthStyle),
        }
    }

    /// Whether a body length fits this element's length field.
    ///
    /// The reference's own test is `canHandleLength`, which is one short of
    /// the field's true capacity: `len < 0xff` for one byte, `len < 0xffff`
    /// for two, `len < 0xffffff` for three.
    fn fits_length(&self, length: i32) -> bool {
        Self::can_handle_length(length, self.length_param())
    }

    /// Write the length into the field just past the id byte.
    fn write_length_field(&self, data: &mut [u8], length: i32) {
        match self.length_param() {
            1 => data[1] = length as u8,
            2 => data[1..3].copy_from_slice(&(length as u16).to_le_bytes()),
            3 => {
                // `BW_PACK3`: the low three bytes, little-endian.
                data[1] = length as u8;
                data[2] = (length >> 8) as u8;
                data[3] = (length >> 16) as u8;
            }
            _ => data[1..5].copy_from_slice(&(length as u32).to_le_bytes()),
        }
    }

    /// `specialCompressLength`: move the first four body bytes to the end of
    /// the body and write the true length in little-endian at the head.
    ///
    /// The reference does this a byte at a time because the head may span
    /// packets; the head and the tail never overlap, a body this long is always
    /// well past four bytes, so a whole-word copy is the same result.
    fn special_compress_length(
        &self,
        data: &mut [u8],
        length: i32,
        is_request: bool,
    ) -> Result<(), FrameError> {
        let body = self.body_offset(is_request);
        let tail = body + length as usize;
        // `tailCheck.advance( size - 1 )`: the last byte must be inside.
        if body + SPECIAL_LENGTH_SIZE > data.len() || tail + SPECIAL_LENGTH_SIZE > data.len() {
            return Err(FrameError::BufferTooSmall);
        }

        // The bounds are checked above, so the four bytes are there.
        let displaced = [data[body], data[body + 1], data[body + 2], data[body + 3]];
        data[body..body + SPECIAL_LENGTH_SIZE].copy_from_slice(&length.to_le_bytes());
        data[tail..tail + SPECIAL_LENGTH_SIZE].copy_from_slice(&displaced);

        Ok(())
    }

    /// `expandLength( data, pPacket, isRequest )`: read the length out of the
    /// header at `data`, undoing the special form when it was used.
    ///
    /// The special form rewrites the frame, so the buffer is handed in
    /// mutably: afterwards the body begins at [`body_offset`](Self::body_offset)
    /// and runs for the returned number of bytes, with four stale bytes left
    /// at the end for the caller to shrink off.
    pub fn expand_length(&self, data: &mut [u8], is_request: bool) -> Result<i32, FrameError> {
        match self.length_style() {
            FIXED_LENGTH_MESSAGE => Ok(self.length_param()),

            VARIABLE_LENGTH_MESSAGE => {
                let width = self.length_param() as usize;
                if 1 + width > data.len() {
                    return Err(FrameError::BufferTooSmall);
                }

                // `pLen` points just past the message id byte.
                let len: u32 = match self.length_param() {
                    0 => 0,
                    1 => u8::from_le_bytes([data[1]]).into(),
                    2 => u16::from_le_bytes([data[1], data[2]]).into(),
                    3 => u32::from_le_bytes([data[1], data[2], data[3], 0]),
                    4 => u32::from_le_bytes([data[1], data[2], data[3], data[4]]),
                    // The reference's default arm logs
                    // "Unhandled variable message length" and leaves the
                    // length zero, so an unknown width expands to zero.
                    _ => 0,
                };

                // A 4-byte length field with the top bit set is negative as an
                // int, which the reference reports as an overflow.
                let len = len as i32;
                if len < 0 {
                    return Err(FrameError::Overflow);
                }

                if !self.can_handle_length_for(len) {
                    // No packet chain to walk, so the buffer must hold it.
                    return self.special_expand_length(data, is_request);
                }

                Ok(len)
            }

            _ => Err(FrameError::UnknownLengthStyle),
        }
    }

    /// `specialExpandLength`: read the four length bytes at the head of the
    /// body, move the last four body bytes back to the head, and report the
    /// length.
    fn special_expand_length(&self, data: &mut [u8], is_request: bool) -> Result<i32, FrameError> {
        let body = self.body_offset(is_request);
        if body + SPECIAL_LENGTH_SIZE > data.len() {
            return Err(FrameError::BufferTooSmall);
        }

        // The reference accumulates the four bytes least significant first,
        // which is the little-endian read of the head.
        let len = i32::from_le_bytes([data[body], data[body + 1], data[body + 2], data[body + 3]]);
        let tail = body + len as usize;
        // `tailCheck.advance( size - 1 )`: the displaced bytes must be inside.
        if len < 0 || tail + SPECIAL_LENGTH_SIZE > data.len() {
            return Err(FrameError::BufferTooSmall);
        }

        let displaced = [data[tail], data[tail + 1], data[tail + 2], data[tail + 3]];
        data[body..body + SPECIAL_LENGTH_SIZE].copy_from_slice(&displaced);

        Ok(len)
    }

    /// `unexpandLength( data, length, pPacket, isRequest )`: the inverse of
    /// [`expand_length`](Self::expand_length), used on the sending side when a
    /// message has to be re-framed.
    ///
    /// Only variable messages with a width that can overflow are touched, and
    /// the length field must already hold all ones.
    pub fn unexpand_length(
        &self,
        data: &mut [u8],
        length: i32,
        is_request: bool,
    ) -> Result<(), FrameError> {
        if self.length_style() != VARIABLE_LENGTH_MESSAGE {
            return Ok(());
        }

        let special = match self.length_param() {
            1 => length >= 0xff,
            2 => length >= 0xffff,
            3 => length >= 0xffffff,
            _ => false,
        };

        if special {
            self.special_compress_length(data, length, is_request)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-byte length field, as `login` in the reference's
    /// `LoginInterface` declares.
    fn variable(width: i32) -> InterfaceElement {
        InterfaceElement::new("variable", 3, VARIABLE_LENGTH_MESSAGE, width, None)
    }

    /// A fixed message of eight body bytes, as `probe` looks when the
    /// interface declares it fixed.
    fn fixed(size: i32) -> InterfaceElement {
        InterfaceElement::new("fixed", 4, FIXED_LENGTH_MESSAGE, size, None)
    }

    /// `compressLength` writes the body length just past the id byte, in
    /// little-endian, for every width the reference supports. The id byte is
    /// untouched.
    #[test]
    fn the_length_lands_just_past_the_id_byte() {
        let cases: [(i32, i32, &[u8]); 4] = [
            (1, 200, &[200u8]),
            (2, 65000, &[232, 253]),
            (3, 1_600_000, &[0, 106, 24]),
            (4, 2_000_000_000, &[0, 148, 53, 119]),
        ];
        for (width, length, field) in cases {
            let element = variable(width);
            let mut frame = [0u8; 16];
            frame[0] = 7;
            assert_eq!(element.compress_length(&mut frame, length, false), Ok(()));
            assert_eq!(frame[0], 7, "the id byte is not written");
            assert_eq!(&frame[1..1 + width as usize], field);
        }
    }

    /// The byte order is the reference's `BW_HTONS`/`BW_HTONL`, which are
    /// little-endian on this host.
    #[test]
    fn the_length_field_is_little_endian() {
        let element = variable(2);
        let mut frame = [0u8; 8];
        assert_eq!(element.compress_length(&mut frame, 0x0102, false), Ok(()));
        assert_eq!(&frame[1..3], &[0x02, 0x01]);

        assert_eq!(element.expand_length(&mut frame, false), Ok(0x0102));
    }

    /// A fixed message writes nothing and answers with its declared size.
    #[test]
    fn a_fixed_message_needs_no_length_field() {
        let element = fixed(8);
        let mut frame = [0u8; 12];
        frame[0] = 4;
        assert_eq!(element.compress_length(&mut frame, 8, false), Ok(()));
        assert_eq!(
            element.expand_length(&mut frame, false),
            Ok(8),
            "nominalBodySize is the answer"
        );
        assert_eq!(element.body_offset(false), 1, "id byte only");
        assert_eq!(element.body_offset(true), 7, "plus the request preamble");
    }

    /// `expandLength` reads back what `compressLength` wrote, including the
    /// three-byte `BW_PACK3`/`BW_UNPACK3` pair.
    #[test]
    fn the_compress_and_expand_are_inverses() {
        // Each width gets a length that fits it, so the round trip stays in
        // the normal form.
        let cases: [(i32, i32); 4] = [(1, 200), (2, 65000), (3, 1_600_000), (4, 2_000_000_000)];
        for (width, length) in cases {
            let element = variable(width);
            let mut frame = [0u8; 16];
            frame[0] = 11;
            assert_eq!(element.compress_length(&mut frame, length, false), Ok(()));
            assert_eq!(element.expand_length(&mut frame, false), Ok(length));
        }
    }

    /// A request's body starts past the reply id and the next-request link.
    #[test]
    fn a_request_frame_carries_the_reply_preamble() {
        let element = variable(2);
        assert_eq!(REQUEST_PREAMBLE_SIZE, 6, "4-byte reply id plus 2-byte link");
        assert_eq!(element.body_offset(true), 1 + 2 + 6);

        let mut frame = [0u8; 20];
        assert_eq!(element.compress_length(&mut frame, 300, true), Ok(()));
        assert_eq!(&frame[1..3], &[44, 1]);
        // The preamble itself is not part of the body length.
        assert_eq!(element.expand_length(&mut frame, true), Ok(300));
    }

    /// A body that no longer fits the length field takes the special form:
    /// the field is all ones, the true length is a little-endian u32 at the
    /// head of the body, and the displaced bytes are appended.
    #[test]
    fn an_oversize_body_takes_the_special_form() {
        let element = variable(1);
        // 255 bytes is exactly the first length a one-byte field refuses.
        let length = 255i32;
        let body = element.body_offset(false);
        let mut frame = [0u8; 8 + 255 + 4];
        frame[0] = 9;
        for (i, byte) in frame[body..body + length as usize].iter_mut().enumerate() {
            *byte = (i + 1) as u8;
        }

        assert_eq!(element.compress_length(&mut frame, length, false), Ok(()));

        assert_eq!(frame[1], 0xff, "the length field is all ones");
        assert_eq!(
            &frame[body..body + SPECIAL_LENGTH_SIZE],
            &255i32.to_le_bytes(),
            "the true length at the head"
        );
        assert_eq!(
            &frame[body + length as usize..body + length as usize + SPECIAL_LENGTH_SIZE],
            &[1, 2, 3, 4],
            "the displaced bytes at the tail"
        );

        // Expanding undoes it and reports the same length.
        assert_eq!(element.expand_length(&mut frame, false), Ok(length));
        assert_eq!(&frame[body..body + SPECIAL_LENGTH_SIZE], &[1, 2, 3, 4]);
        // The tail is left as it was: the reference overwrites the head with
        // the tail bytes and never restores them, leaving four stale bytes for
        // the caller to shrink off the body.
        assert_eq!(
            frame[body + length as usize..body + length as usize + SPECIAL_LENGTH_SIZE],
            [1, 2, 3, 4]
        );
    }

    /// The two-byte and three-byte widths take the special form at their own
    /// thresholds, and `unexpandLength` re-creates it on the sending side.
    #[test]
    fn the_special_form_starts_at_each_widths_threshold() {
        let mut frame = [0u8; 64 + 0xffff + 4];
        frame[0] = 1;
        let two = variable(2);
        let length = 0xffffi32;
        assert_eq!(two.compress_length(&mut frame, length, false), Ok(()));
        assert_eq!(frame[1], 0xff);
        assert_eq!(frame[2], 0xff);
        assert_eq!(&frame[3..7], &length.to_le_bytes());
        assert_eq!(two.expand_length(&mut frame, false), Ok(length));

        // A non-oversize length needs no special form, so unexpandLength leaves
        // the frame alone.
        let mut small = [0u8; 12];
        assert_eq!(two.unexpand_length(&mut small, 100, false), Ok(()));
        assert_eq!(small, [0; 12]);

        let mut special = [0u8; 64 + 0xffff + 4];
        special[0] = 1;
        special[1] = 0xff;
        special[2] = 0xff;
        for (i, byte) in special[3..3 + 0xffff].iter_mut().enumerate() {
            *byte = (i & 0xff) as u8;
        }
        assert_eq!(two.unexpand_length(&mut special, length, false), Ok(()));
        assert_eq!(&special[3..7], &length.to_le_bytes());

        // Widths 2 and 3 are the only ones that can overflow: width 4 always
        // fits, so no threshold applies.
        let four = variable(4);
        let mut wide = [0u8; 16];
        assert_eq!(four.compress_length(&mut wide, 0xffff, false), Ok(()));
        // The field is the value itself, all ones included: no special form.
        assert_eq!(&wide[1..5], &[0xff, 0xff, 0, 0]);
        assert_eq!(four.expand_length(&mut wide, false), Ok(0xffff));
    }

    /// The reference's thresholds are one short of the field's true capacity:
    /// `len < 0xff` for one byte, `len < 0xffff` for two, `len < 0xffffff` for
    /// three, matching `canHandleLength`.
    #[test]
    fn the_thresholds_match_can_handle_length() {
        for (width, fits) in [(1i32, 254i32), (2, 65534), (3, 0xfffffe)] {
            let element = variable(width);
            assert!(element.can_handle_length_for(fits));
            assert!(!element.can_handle_length_for(fits + 1));

            let mut frame = [0u8; 16];
            assert_eq!(element.compress_length(&mut frame, fits, false), Ok(()));
            assert_eq!(element.expand_length(&mut frame, false), Ok(fits));
        }
    }

    /// A negative body length is the overflow the reference reports.
    #[test]
    fn a_negative_length_is_refused() {
        let element = variable(2);
        let mut frame = [0u8; 8];
        assert_eq!(
            element.compress_length(&mut frame, -1, false),
            Err(FrameError::NegativeLength)
        );
    }

    /// A four-byte length field with the top bit set overflows the int the
    /// reference returns.
    #[test]
    fn a_four_byte_length_can_overflow() {
        let element = variable(4);
        let mut frame = [0u8; 8];
        frame[1..5].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            element.expand_length(&mut frame, false),
            Err(FrameError::Overflow)
        );
    }

    /// An unresolvable length style is not frameable.
    #[test]
    fn a_callback_element_is_not_frameable() {
        let element = InterfaceElement::new("callback", 6, 2, 0, None);
        let mut frame = [0u8; 8];
        assert_eq!(
            element.compress_length(&mut frame, 4, false),
            Err(FrameError::UnknownLengthStyle)
        );
        assert_eq!(
            element.expand_length(&mut frame, false),
            Err(FrameError::UnknownLengthStyle)
        );

        // The inverse does nothing for it.
        assert_eq!(element.unexpand_length(&mut frame, 4, false), Ok(()));
    }

    /// A frame shorter than its length field cannot be expanded.
    #[test]
    fn a_short_frame_is_too_small() {
        let element = variable(2);
        let mut frame = [0u8; 1];
        assert_eq!(
            element.expand_length(&mut frame, false),
            Err(FrameError::BufferTooSmall)
        );

        // And the special form needs room for its four spare bytes.
        let mut special = [0u8; 10];
        assert_eq!(
            element.unexpand_length(&mut special, 0xffff, false),
            Err(FrameError::BufferTooSmall)
        );

        // Expanding a special frame needs the head inside the buffer, and
        // separately the tail the length points at.
        let one = variable(1);
        let mut headless = [0u8; 3];
        headless[1] = 0xff;
        assert_eq!(
            one.expand_length(&mut headless, false),
            Err(FrameError::BufferTooSmall)
        );

        // The head holds the true length, so a small body makes the tail
        // the byte past the buffer.
        let mut short_tail = [0u8; 20];
        short_tail[1] = 0xff;
        short_tail[2..6].copy_from_slice(&255i32.to_le_bytes());
        assert_eq!(
            one.expand_length(&mut short_tail, false),
            Err(FrameError::BufferTooSmall)
        );
    }

    /// A whole message, compressed and then expanded, is byte identical to
    /// what the reference would have put on the wire: id, length field,
    /// payload.
    #[test]
    fn a_golden_variable_message_round_trips() {
        let element = variable(2);
        let payload: [u8; 6] = [0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe];

        let mut frame = [0u8; 3 + 6];
        frame[0] = element.id();
        frame[3..].copy_from_slice(&payload);
        assert_eq!(
            element.compress_length(&mut frame, payload.len() as i32, false),
            Ok(())
        );
        assert_eq!(
            frame,
            [element.id(), 6, 0, 0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe]
        );

        assert_eq!(element.expand_length(&mut frame, false), Ok(6));
        assert_eq!(&frame[3..], &payload[..]);
    }

    /// `unexpandLength` recreates the special form at each width's own
    /// threshold, and leaves a width-4 frame alone because it cannot overflow.
    #[test]
    fn unexpand_marks_the_special_form_per_width() {
        // Every width arm is reached; a small frame simply lacks the four
        // spare bytes the special form wants, which is the error unexpand
        // reports.
        let cases: [(i32, i32); 4] = [(1, 255), (2, 0xffff), (3, 0xfffffff), (4, 0xffff)];
        for (width, length) in cases {
            let element = variable(width);
            let mut frame = [0u8; 8];
            frame[0] = 3;
            for byte in &mut frame[1..=width as usize] {
                *byte = 0xff;
            }

            let result = element.unexpand_length(&mut frame, length, false);
            if width == 4 {
                assert_eq!(result, Ok(()), "width 4 never needs the form");
            } else {
                assert_eq!(result, Err(FrameError::BufferTooSmall));
            }
        }

        // Width 4 alone: an untouched field stays untouched.
        let four = variable(4);
        let mut wide = [0u8; 16];
        wide[0] = 5;
        assert_eq!(four.unexpand_length(&mut wide, 0xffff, false), Ok(()));
        assert_eq!(&wide[1..5], &[0; 4], "nothing is written");

        // A zero-width element has no field at all: compress writes nothing
        // and expand answers zero.
        let none = variable(0);
        let mut frame = [0u8; 8];
        assert_eq!(none.compress_length(&mut frame, 4, false), Ok(()));
        assert_eq!(none.expand_length(&mut frame, false), Ok(0));
        assert_eq!(frame, [0; 8]);
    }

    /// A fixed message whose length does not match is a programmer error in
    /// the reference: it logs and trips its assert.
    #[test]
    #[should_panic(expected = "fixed length message has wrong length")]
    fn a_mismatching_fixed_length_trips_the_reference_assert() {
        let element = fixed(8);
        let mut frame = [0u8; 4];
        element.compress_length(&mut frame, 9, false).unwrap();
    }

    /// A length field that is not 1 to 4 bytes wide is the reference's
    /// default arm: compress writes nothing and still reports success, and
    /// expand answers zero.
    #[test]
    fn a_width_outside_one_to_four_writes_nothing() {
        let element = variable(5);
        let mut frame = [0u8; 8];
        assert_eq!(element.compress_length(&mut frame, 10, false), Ok(()));
        assert_eq!(frame, [0; 8], "the reference logs and returns 0");
        assert_eq!(element.expand_length(&mut frame, false), Ok(0));
    }
}
