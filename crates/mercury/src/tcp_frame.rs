//! The TCP frame parser: the framing half of `TCPChannel::handleInput`
//! (`lib/network/tcp_channel.cpp:190-263`) -- the length-prefix loop with its
//! two readers (`readSmallFrameLength` :321, `readLargeFrameLength` :345) and
//! the post-frame cleanup `cleanUpAfterFrameReceive` (:384) folds into.
//!
//! A frame on the wire is a length prefix over the bundle's bytes:
//!
//! ```text
//! small: [ len:2 ][ payload:len ]                    len != 0xFFFF
//! large: [ 0xFFFF:2 ][ len:4 ][ payload:len ]
//! ```
//!
//! `doSend` (tcp_channel.cpp:429) writes the 0xFFFF escape when a bundle is
//! that big, so the prefix is either two or six bytes wide. Because TCP is a
//! stream, a segment can carry none of a prefix, one, several complete frames,
//! or a frame plus the head of the next; the parser keeps whatever it cannot
//! yet frame and asks for more, which is the whole content of the two state
//! members `frameLength_` and `frameHeaderLength_`:
//!
//! | frame length   | prefix width | meaning                                       |
//! | -------------- | -------------- | --------------------------------------------- |
//! | 0              | 0              | not even the small prefix is in hand          |
//! | 0              | 6              | the escape was read, the 4-byte length awaits |
//! | nonzero        | 2 or 6         | the length is known, payload bytes are owed   |
//!
//! After a frame is completed the parser recounts the two members and carries
//! the leftover bytes forward as the start of the next frame, exactly what
//! `cleanUpAfterFrameReceive` does by moving the stream's remainder into a
//! fresh buffer; it also bumps the channel's data-unit and byte counters there,
//! so both travel along.
//!
//! The length fields are written with the reference's `operator<<( uint16 )`
//! / `( uint32 )`, which land in host byte order on the x86-64 fixture host,
//! so they are read little-endian here, as with every other integer wire field
//! in this crate.
//!
//! Left with the channel layer: the connection header frame the server waits
//! for and the WebSockets sniff (`headerState_`, `readHeader`,
//! tcp_channel.cpp:194-315), the stream filter the read may pass through
//! (`pStreamFilter_`, :172-183), the `MemoryOStream` cap the accumulator is
//! grown to (`maxSegmentSize()`), and the dispatch of a finished frame's
//! bytes (`processReceivedFrame`, :360-378).

/// A small length prefix of this value is not a length at all: it announces a
/// four-byte length follows. `doSend` writes it (tcp_channel.cpp:431).
const SMALL_FRAME_ESCAPE: u16 = 0xFFFF;
/// `frameHeaderLength_` for a small frame.
const SMALL_FRAME_HEADER: usize = 2;
/// `frameHeaderLength_` once the escape was read: the escape itself plus the
/// 4-byte length it announces.
const LARGE_FRAME_HEADER: usize = SMALL_FRAME_HEADER + 4;

/// The receive-side framing state of one TCP connection.
pub struct TcpFrameParser {
    /// The bytes read off the socket, whether or not they are framed yet: the
    /// reference's `pFrameData_`.
    buffer: Vec<u8>,
    /// How far `buffer` is already consumed by finished frames. The reference
    /// compacts instead of tracking this; it pays the same shift once per
    /// feed rather than once per frame.
    read: usize,
    /// `frameLength_`: the payload bytes a frame owes, or 0 while the prefix is
    /// still in the air.
    frame_length: usize,
    /// `frameHeaderLength_`: 0, then 2 after the small prefix, then 6 when the
    /// escape was seen.
    frame_header_length: usize,
    /// `++numDataUnitsReceived_` in `cleanUpAfterFrameReceive`.
    num_frames_received: u64,
    /// `numBytesReceived_ += frameHeaderLength_ + frameLength_`.
    num_bytes_received: u64,
}

impl TcpFrameParser {
    /// An empty parser, as a channel's is: nothing read, nothing owed.
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            read: 0,
            frame_length: 0,
            frame_header_length: 0,
            num_frames_received: 0,
            num_bytes_received: 0,
        }
    }

    /// `handleInput`'s framing loop, minus the socket read: absorb the bytes
    /// just delivered and hand back every complete frame.
    ///
    /// Anything still short of a frame -- a prefix split in two, a large
    /// escape waiting on its length, a payload that runs out of room -- stays
    /// buffered for the next feed, which is the short read. Several frames in
    /// one read come back as several, which is the coalesced read; the loop
    /// runs until nothing further frames.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();

        loop {
            let available = self.buffer.len() - self.read;

            if self.frame_length == 0 && self.frame_header_length == 0 {
                // readSmallFrameLength
                if available < SMALL_FRAME_HEADER {
                    break;
                }
                let small =
                    u16::from_le_bytes(self.buffer[self.read..self.read + 2].try_into().unwrap());
                self.read += SMALL_FRAME_HEADER;
                if small != SMALL_FRAME_ESCAPE {
                    self.frame_length = small as usize;
                    self.frame_header_length = SMALL_FRAME_HEADER;
                } else {
                    self.frame_header_length = LARGE_FRAME_HEADER;
                }
                continue;
            }

            if self.frame_length == 0 && self.frame_header_length != 0 {
                // readLargeFrameLength
                if available < 4 {
                    break;
                }
                let large =
                    u32::from_le_bytes(self.buffer[self.read..self.read + 4].try_into().unwrap());
                self.read += 4;
                self.frame_length = large as usize;
                continue;
            }

            // frame_length != 0
            if available < self.frame_length {
                break;
            }

            // processReceivedFrame, then cleanUpAfterFrameReceive
            let end = self.read + self.frame_length;
            frames.push(self.buffer[self.read..end].to_vec());
            self.num_bytes_received += (self.frame_header_length + self.frame_length) as u64;
            self.num_frames_received += 1;
            self.read = end;
            self.frame_length = 0;
            self.frame_header_length = 0;
        }

        if self.read > 0 {
            self.buffer.drain(..self.read);
            self.read = 0;
        }
        frames
    }

    /// The bytes read so far that are not part of a finished frame: a prefix in
    /// the air, or a payload still arriving. Bytes a length already read owes
    /// but the wire has not delivered are not here.
    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len() - self.read
    }

    /// `numDataUnitsReceived_`.
    pub fn num_frames_received(&self) -> u64 {
        self.num_frames_received
    }

    /// `numBytesReceived_`, prefix bytes included.
    pub fn num_bytes_received(&self) -> u64 {
        self.num_bytes_received
    }
}

impl Default for TcpFrameParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small frame prefixes are two little-endian bytes.
    fn small_prefix(len: usize) -> Vec<u8> {
        (len as u16).to_le_bytes().to_vec()
    }

    /// The 0xFFFF escape plus its four little-endian bytes.
    fn large_prefix(len: usize) -> Vec<u8> {
        let mut out = SMALL_FRAME_ESCAPE.to_le_bytes().to_vec();
        out.extend_from_slice(&(len as u32).to_le_bytes());
        out
    }

    /// A small frame around `payload`, ready to walk a socket byte for byte.
    fn small_frame(payload: &[u8]) -> Vec<u8> {
        let mut out = small_prefix(payload.len());
        out.extend_from_slice(payload);
        out
    }

    /// A large frame, as `doSend` writes one that is UINT16_MAX big or more.
    fn large_frame(payload: &[u8]) -> Vec<u8> {
        let mut out = large_prefix(payload.len());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn the_parser_starts_empty_and_owing_nothing() {
        let mut parser = TcpFrameParser::new();
        assert_eq!(parser.feed(b""), Vec::<Vec<u8>>::new());
        assert_eq!(parser.buffered_bytes(), 0);
        assert_eq!(parser.num_frames_received(), 0);
        assert_eq!(parser.num_bytes_received(), 0);
    }

    /// Short read: a frame whose length prefix, and then whose payload, each
    /// split across feeds arrives whole at the end.
    #[test]
    fn a_frame_split_byte_for_byte_arrives_whole() {
        let mut parser = TcpFrameParser::new();
        let wire = small_frame(b"abcdefgh");
        for byte in wire {
            let got = parser.feed(&[byte]);
            if got.is_empty() {
                // A prefix byte or a partial payload frames nothing.
                assert_eq!(parser.buffered_bytes(), parser.buffer.len());
            } else {
                assert_eq!(got, vec![b"abcdefgh".to_vec()]);
            }
        }
        assert_eq!(parser.num_frames_received(), 1);
        assert_eq!(
            parser.num_bytes_received(),
            10,
            "two prefix bytes and eight"
        );
        assert_eq!(parser.buffered_bytes(), 0, "nothing is owed");
    }

    /// Short read at the two points the reference waits: mid-prefix and
    /// mid-large-length.
    #[test]
    fn a_split_prefix_and_a_split_large_length_both_wait() {
        let mut parser = TcpFrameParser::new();
        // One of the two prefix bytes: neither length reader can act.
        let mut first = small_frame(b"body");
        let half = first.remove(0);
        assert_eq!(parser.feed(&[half]), Vec::<Vec<u8>>::new());
        assert_eq!(parser.buffered_bytes(), 1);

        // The escape and only two of its four length bytes. A fresh parser
        // keeps the first sub-case's buffered byte out of this one.
        let mut parser = TcpFrameParser::new();
        let mut second = large_frame(b"big");
        let head: Vec<u8> = second.drain(..4).collect();
        assert_eq!(parser.feed(&head), Vec::<Vec<u8>>::new());
        assert_eq!(
            parser.buffered_bytes(),
            2,
            "half the length, escape consumed"
        );

        parser.feed(&second);
        assert_eq!(parser.num_frames_received(), 1);
        assert_eq!(parser.num_bytes_received(), 6 + 3);
        assert_eq!(parser.buffered_bytes(), 0);
    }

    /// Coalesced read: three frames in one segment come back three, in order,
    /// with nothing buffered behind them.
    #[test]
    fn several_frames_in_one_read_come_back_in_order() {
        let mut parser = TcpFrameParser::new();
        let mut wire = Vec::new();
        wire.extend_from_slice(&small_frame(b"one"));
        wire.extend_from_slice(&small_frame(b"two three"));
        wire.extend_from_slice(&small_frame(b"four five six"));
        assert_eq!(
            parser.feed(&wire),
            vec![
                b"one".to_vec(),
                b"two three".to_vec(),
                b"four five six".to_vec()
            ]
        );
        assert_eq!(parser.num_frames_received(), 3);
        assert_eq!(
            parser.num_bytes_received(),
            (2 + 3) + (2 + 9) + (2 + 13),
            "every frame's prefix and payload counted"
        );
        assert_eq!(parser.buffered_bytes(), 0);
    }

    /// Coalesced read across a feed boundary: two complete frames plus the
    /// head of a third; the head waits, the two others do not.
    #[test]
    fn trailing_partial_data_waits_on_the_next_read() {
        let mut parser = TcpFrameParser::new();
        let mut wire = small_frame(b"first");
        wire.extend_from_slice(&small_frame(b"second"));
        let next = large_frame(b"third frame");
        wire.extend_from_slice(&next[..3]);
        assert_eq!(
            parser.feed(&wire),
            vec![b"first".to_vec(), b"second".to_vec()],
            "only the two complete frames come out"
        );
        assert_eq!(
            parser.buffered_bytes(),
            1,
            "the third frame's escape and length bytes are already consumed \
             into the parser's state"
        );
        assert_eq!(parser.num_frames_received(), 2);

        parser.feed(&next[3..]);
        assert_eq!(parser.feed(b""), Vec::<Vec<u8>>::new());
        assert_eq!(parser.num_frames_received(), 3);
        assert_eq!(parser.num_bytes_received(), (2 + 5) + (2 + 6) + (6 + 11));
    }

    /// A large frame crosses the escape boundary both ways: prefix split,
    /// length split, payload split.
    #[test]
    fn a_large_frame_survives_every_split() {
        let mut wire = large_frame(b"0123456789");
        let mut parser = TcpFrameParser::new();
        assert_eq!(parser.feed(&wire[..1]), Vec::<Vec<u8>>::new());
        wire.remove(0);
        assert_eq!(parser.feed(&wire[..2]), Vec::<Vec<u8>>::new());
        wire.drain(..2);
        assert_eq!(parser.feed(&wire[..1]), Vec::<Vec<u8>>::new());
        wire.remove(0);
        assert_eq!(parser.feed(&wire[..10]), Vec::<Vec<u8>>::new());
        wire.drain(..10);
        assert_eq!(parser.feed(&wire), vec![b"0123456789".to_vec()]);
        assert_eq!(parser.num_bytes_received(), 6 + 10);
    }

    /// A large length can address a bundle the small prefix cannot, and the
    /// frame still frames.
    #[test]
    fn a_large_frame_of_many_kilobytes_frames() {
        let payload: Vec<u8> = (0..3000).map(|i| (i % 251) as u8).collect();
        let mut parser = TcpFrameParser::new();
        assert_eq!(parser.feed(&large_frame(&payload)), vec![payload]);
        assert_eq!(parser.num_bytes_received(), 6 + 3000);
    }

    /// A small length of 0 is not an empty frame: `readSmallFrameLength` sets
    /// `frameLength_` to 0 and `frameHeaderLength_` to 2, so the very next
    /// branch in `handleInput`'s loop reads a large length instead
    /// (tcp_channel.cpp:236-244), exactly as it does here. A real bundle always
    /// carries its flags byte, so `doSend` never emits one.
    #[test]
    fn a_zero_small_length_is_read_as_a_waiting_large_length() {
        let mut parser = TcpFrameParser::new();
        assert_eq!(
            parser.feed(&small_prefix(0)),
            Vec::<Vec<u8>>::new(),
            "the parser stalls on a zero small length, as the reference does"
        );
        assert_eq!(
            parser.buffered_bytes(),
            0,
            "the prefix was consumed; only the large length is now owed"
        );

        // The four bytes that were a payload are instead read as the large
        // length: 7, which is then owed as payload.
        assert_eq!(parser.feed(&7u32.to_le_bytes()), Vec::<Vec<u8>>::new());
        assert_eq!(parser.buffered_bytes(), 0, "the length is consumed");
        parser.feed(b"seven!!");
        assert_eq!(parser.num_frames_received(), 1);
        assert_eq!(
            parser.num_bytes_received(),
            2 + 7,
            "the reference counts the two-byte small header even here, since \
             readLargeFrameLength never widens frameHeaderLength_"
        );
    }
}
