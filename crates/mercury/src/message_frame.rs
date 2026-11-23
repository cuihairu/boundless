//! Message frame encoding and decoding
//! (`lib/network/interface_element.cpp` `compressLength`/`expandLength`).
//!
//! BW-02-02-02: the wire form of one message is `[id][length][body]` --
//! the one-byte `MessageID`, then, for variable-length messages only, a
//! length field `length_param` bytes wide, then the body. Fixed-length
//! messages carry no length field and their body must equal the element's
//! declared size.
//!
//! BigWorld serialises little-endian over the network (`binary_stream.ipp`:
//! "BigWorld uses little-endian over the network, not big-endian which is
//! the standard network byte order"), so every length width is
//! little-endian: `BW_HTONS`/`BW_HTONL` are the identity on a little-endian
//! host and `BW_PACK3` packs the low three bytes (`binary_stream.ipp:155`).
//!
//! A body that cannot fit its inline length field is the reference's
//! oversize escape: the field is filled with `0xff` and the real length
//! follows as a 32-bit stash, which needs a multi-packet `Bundle` to
//! reassemble. That path is BW-02-03 (Packet/Bundle); here it surfaces as
//! [`FrameError::Oversize`] on encode and [`FrameError::EscapeMarker`] on
//! decode.

use crate::interface_element::{InterfaceElement, LengthStyle};

/// A message-frame encode or decode failure.
///
/// The reference returns `-1` and prints an `ERROR_MSG`/`CRITICAL_MSG`
/// diagnosis for each of these; the port keeps them as a typed error so the
/// two arms are tested rather than diagnosed at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// A fixed-length body does not match the element's declared size
    /// (`compressLength`'s "Fixed length message has wrong length").
    FixedLengthMismatch { expected: i32, got: usize },
    /// A variable-length body is too long for the inline length field. The
    /// reference escapes through `specialCompressLength`, which needs a
    /// multi-packet bundle (BW-02-03).
    Oversize { length: usize, width: i32 },
    /// The length field is the all-ones escape marker; the real length is a
    /// 32-bit stash to be reassembled from a bundle (BW-02-03).
    EscapeMarker,
    /// The variable-length width is not 1..=4.
    UnsupportedWidth(i32),
    /// A callback- or invalid-style element has no computable header.
    NotFramable,
    /// The buffer is too short for the declared header.
    Truncated,
    /// A variable-length frame's body does not match its declared length.
    BodyLengthMismatch { declared: usize, got: usize },
}

/// `compressLength` + the identifier: the wire bytes of one message.
///
/// On success the returned buffer is exactly `[id][length][body]`, with the
/// length in little-endian at the declared width (or absent for fixed
/// messages). The body must already be complete; a body that would need the
/// oversize escape is rejected ([`FrameError::Oversize`]).
pub fn encode_frame(element: &InterfaceElement, body: &[u8]) -> Result<Vec<u8>, FrameError> {
    match element.length_style() {
        LengthStyle::Fixed => {
            if body.len() != element.length_param() as usize {
                return Err(FrameError::FixedLengthMismatch {
                    expected: element.length_param(),
                    got: body.len(),
                });
            }
            let mut frame = Vec::with_capacity(1 + body.len());
            frame.push(element.id());
            frame.extend_from_slice(body);
            Ok(frame)
        }
        LengthStyle::Variable => {
            let width = element.length_param();
            let fits = match width {
                1 => body.len() < 0xff,
                2 => body.len() < 0xffff,
                3 => body.len() < 0xffffff,
                4 => body.len() <= i32::MAX as usize,
                _ => return Err(FrameError::UnsupportedWidth(width)),
            };
            if !fits {
                return Err(FrameError::Oversize {
                    length: body.len(),
                    width,
                });
            }

            let mut frame = Vec::with_capacity(1 + width as usize + body.len());
            frame.push(element.id());
            // Little-endian at the declared width: the low `width` bytes of
            // the length, `BW_PACK3` for width 3.
            frame.extend_from_slice(&(body.len() as u64).to_le_bytes()[..width as usize]);
            frame.extend_from_slice(body);
            Ok(frame)
        }
        LengthStyle::Callback | LengthStyle::Invalid => Err(FrameError::NotFramable),
    }
}

/// `expandLength` + the identifier: the body of one message.
///
/// `frame` is `[id][length][body]` as [`encode_frame`] produced it. The
/// identifier is not checked here (the caller matched it to `element` via
/// the interface table); the length field is read little-endian and the
/// body slice is the rest of the buffer.
pub fn body_from_frame<'a>(
    element: &InterfaceElement,
    frame: &'a [u8],
) -> Result<&'a [u8], FrameError> {
    match element.length_style() {
        LengthStyle::Fixed => {
            if frame.is_empty() {
                return Err(FrameError::Truncated);
            }
            let body = &frame[1..];
            if body.len() != element.length_param() as usize {
                return Err(FrameError::BodyLengthMismatch {
                    declared: element.length_param() as usize,
                    got: body.len(),
                });
            }
            Ok(body)
        }
        LengthStyle::Variable => {
            let width = element.length_param();
            if !(1..=4).contains(&width) {
                return Err(FrameError::UnsupportedWidth(width));
            }
            let header = 1 + width as usize;
            if frame.len() < header {
                return Err(FrameError::Truncated);
            }

            let length_field = &frame[1..header];
            // All-ones is the oversize escape marker for any width below 4
            // (`expandLength`: `!canHandleLength( len )`).
            if width < 4 && length_field.iter().all(|&byte| byte == 0xff) {
                return Err(FrameError::EscapeMarker);
            }

            let mut length = 0usize;
            for (i, byte) in length_field.iter().enumerate() {
                length |= (*byte as usize) << (8 * i);
            }

            let body = &frame[header..];
            if body.len() != length {
                return Err(FrameError::BodyLengthMismatch {
                    declared: length,
                    got: body.len(),
                });
            }
            Ok(body)
        }
        LengthStyle::Callback | LengthStyle::Invalid => Err(FrameError::NotFramable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed, empty-body message (`MERCURY_EMPTY_MESSAGE`): the whole
    /// frame is the one-byte identifier.
    const PROBE: (u8, LengthStyle, i32) = (1, LengthStyle::Fixed, 0);

    /// A fixed message with a 4-byte body (`disconnect`, `uint32 seq`).
    const FIXED4: (u8, LengthStyle, i32) = (3, LengthStyle::Fixed, 4);

    /// A variable message with a 2-byte length header (`login`).
    const VAR2: (u8, LengthStyle, i32) = (0, LengthStyle::Variable, 2);

    /// A variable message with a 3-byte length header.
    const VAR3: (u8, LengthStyle, i32) = (4, LengthStyle::Variable, 3);

    /// A variable message with a 4-byte length header.
    const VAR4: (u8, LengthStyle, i32) = (5, LengthStyle::Variable, 4);

    fn element((id, style, param): (u8, LengthStyle, i32)) -> InterfaceElement {
        InterfaceElement::new("msg", id, style, param)
    }

    #[test]
    fn a_fixed_empty_frame_is_just_the_identifier() {
        let probe = element(PROBE);
        let frame = encode_frame(&probe, &[]).unwrap();
        assert_eq!(frame, [0x01]);
        assert_eq!(body_from_frame(&probe, &frame).unwrap(), &[] as &[u8]);
    }

    #[test]
    fn a_fixed_frame_carries_the_body_verbatim() {
        let msg = element(FIXED4);
        let payload = [0xAA, 0xBB, 0xCC, 0xDD];
        let frame = encode_frame(&msg, &payload).unwrap();
        assert_eq!(frame, [0x03, 0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(body_from_frame(&msg, &frame).unwrap(), &payload);
    }

    #[test]
    fn a_fixed_body_must_match_the_declared_length() {
        let probe = element(PROBE);
        assert_eq!(
            encode_frame(&probe, &[0x01]),
            Err(FrameError::FixedLengthMismatch {
                expected: 0,
                got: 1
            })
        );
    }

    #[test]
    fn a_variable_length_is_little_endian_at_its_width() {
        // The reference's own golden bytes (`test_tcp_bundle.cpp`): a
        // width-2 message with a 258-byte payload stores `0x0102` as the
        // two little-endian bytes `02 01`.
        let msg = element(VAR2);
        let payload = vec![0x77; 258];
        let frame = encode_frame(&msg, &payload).unwrap();
        assert_eq!(&frame[..4], &[0x00, 0x02, 0x01, 0x77]);
        assert_eq!(body_from_frame(&msg, &frame).unwrap(), &payload[..]);

        // The identifier precedes the length: [id][len 02 01][payload..].
        assert_eq!(frame[0], 0x00);
    }

    #[test]
    fn a_width_3_length_is_pack3_little_endian() {
        // `TCPBundle_variableLengthWidth3Pack3`: 300 = 0x012C packs as the
        // three little-endian bytes `2C 01 00`.
        let msg = element(VAR3);
        let payload = vec![0x66; 300];
        let frame = encode_frame(&msg, &payload).unwrap();
        assert_eq!(&frame[..5], &[0x04, 0x2C, 0x01, 0x00, 0x66]);
        assert_eq!(body_from_frame(&msg, &frame).unwrap(), &payload[..]);
    }

    #[test]
    fn a_width_4_length_is_a_little_endian_u32() {
        let msg = element(VAR4);
        let payload = [0x01, 0x02, 0x03, 0x04];
        let frame = encode_frame(&msg, &payload).unwrap();
        assert_eq!(&frame[..5], &[0x05, 0x04, 0x00, 0x00, 0x00]);
        assert_eq!(body_from_frame(&msg, &frame).unwrap(), &payload);
    }

    #[test]
    fn the_round_trip_preserves_arbitrary_bodies() {
        for (id, style, param) in [
            PROBE,
            FIXED4,
            VAR2,
            VAR3,
            VAR4,
            (6, LengthStyle::Variable, 1),
        ] {
            let msg = element((id, style, param));
            let body: Vec<u8> = match style {
                LengthStyle::Fixed => vec![0x5A; param as usize],
                _ => (0..200).map(|i| (i * 7) as u8).collect(),
            };
            let frame = encode_frame(&msg, &body).unwrap();
            assert_eq!(body_from_frame(&msg, &frame).unwrap(), &body[..], "id {id}");
        }
    }

    #[test]
    fn an_oversize_body_is_rejected_on_encode() {
        // Width 1 holds lengths below 0xff; 255 is the escape marker.
        let msg = element((7, LengthStyle::Variable, 1));
        assert_eq!(
            encode_frame(&msg, &vec![0x99; 300]),
            Err(FrameError::Oversize {
                length: 300,
                width: 1
            })
        );
        assert!(encode_frame(&msg, &vec![0x99; 254]).is_ok());

        // Width 2 holds lengths below 0xffff.
        let msg2 = element(VAR2);
        assert!(encode_frame(&msg2, &vec![0x00; 0xfffe]).is_ok());
        assert_eq!(
            encode_frame(&msg2, &vec![0x00; 0xffff]),
            Err(FrameError::Oversize {
                length: 0xffff,
                width: 2
            })
        );
    }

    #[test]
    fn an_escape_marker_is_reported_on_decode() {
        let msg = element(VAR2);
        // The all-ones length field marks the oversize escape.
        assert_eq!(
            body_from_frame(&msg, &[0x00, 0xff, 0xff]),
            Err(FrameError::EscapeMarker)
        );
    }

    #[test]
    fn a_truncated_frame_is_rejected() {
        let msg = element(VAR2);
        assert_eq!(body_from_frame(&msg, &[0x00]), Err(FrameError::Truncated));
        assert_eq!(body_from_frame(&msg, &[]), Err(FrameError::Truncated));
    }

    #[test]
    fn a_body_shorter_than_its_length_is_rejected() {
        let msg = element(VAR2);
        // Declares 4 bytes but carries only 2.
        let frame = [0x00, 0x04, 0x00, 0x01, 0x02];
        assert_eq!(
            body_from_frame(&msg, &frame),
            Err(FrameError::BodyLengthMismatch {
                declared: 4,
                got: 2
            })
        );
    }

    #[test]
    fn unsupported_widths_and_styles_are_rejected() {
        let width0 = element((8, LengthStyle::Variable, 0));
        assert_eq!(
            encode_frame(&width0, &[]),
            Err(FrameError::UnsupportedWidth(0))
        );

        let callback = element((9, LengthStyle::Callback, 0));
        assert_eq!(encode_frame(&callback, &[]), Err(FrameError::NotFramable));
        assert_eq!(
            body_from_frame(&callback, &[]),
            Err(FrameError::NotFramable)
        );
    }
}
