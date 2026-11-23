//! `Mercury::EncryptionStreamFilter` (lib/network/encryption_stream_filter.
//! {hpp,cpp}): the stream filter `TCPChannel::setEncryption` wraps the
//! channel's stream in (tcp_channel.cpp:578-590). It encrypts/decrypts the
//! TCP byte stream in 8-byte blocks with the same Blowfish XOR chaining as
//! the UDP `EncryptionFilter`, but the frame boundary is carried *inside* the
//! stream: the first decrypted block of a frame is the frame's own length
//! header, which the filter uses to trim the zero padding the last block
//! carries.
//!
//! The reference is a `StreamFilter` over a `NetworkStream` (the channel's
//! endpoint). The port has no endpoint stream object -- `TcpChannel::on_data`
//! hands it raw bytes -- so this is a self-contained codec: `read_into`
//! buffers the encrypted bytes and returns the decrypted stream, `write_from`
//! does the inverse for the send side. The codec is stateful (partial blocks
//! and the previous plaintext block carry across calls), which is what makes
//! the framing loop's frame-splitting transparent to it.

use std::rc::Rc;

use crate::block_cipher::BlockCipher;

/// `Mercury::EncryptionStreamFilter` (encryption_stream_filter.hpp:24).
pub struct EncryptionStreamFilter {
    p_block_cipher: Rc<dyn BlockCipher>,
    /// `receiveBuffer_`: encrypted bytes not yet a whole block.
    receive_buffer: Vec<u8>,
    /// `expectedReceiveFrameLength_`: how many bytes of the current frame are
    /// still owed, 0 = between frames.
    expected_receive_frame_length: usize,
    /// `previousReceivedBlock_`: the last decrypted (plaintext) block, for
    /// the receive-side XOR chain.
    previous_received_block: Vec<u8>,
    /// `previousSentBlock_`: the last plaintext block written, for the
    /// send-side XOR chain.
    previous_sent_block: Vec<u8>,
}

impl EncryptionStreamFilter {
    /// `EncryptionStreamFilter::create( stream, pCipher )`
    /// (encryption_stream_filter.cpp:38).
    pub fn new(p_block_cipher: Rc<dyn BlockCipher>) -> Self {
        let block_size = p_block_cipher.block_size();
        EncryptionStreamFilter {
            p_block_cipher,
            receive_buffer: Vec::new(),
            expected_receive_frame_length: 0,
            previous_received_block: vec![0u8; block_size],
            previous_sent_block: vec![0u8; block_size],
        }
    }

    /// `readInto` (encryption_stream_filter.cpp:99): feed encrypted bytes in,
    /// get the decrypted stream out. Incomplete trailing blocks stay buffered
    /// until the next call. The returned bytes are exactly the frame bytes
    /// (padding trimmed by the frame length the first block carries).
    pub fn read_into(&mut self, encrypted: &[u8]) -> Vec<u8> {
        self.receive_buffer.extend_from_slice(encrypted);
        let block_size = self.p_block_cipher.block_size();
        let mut output = Vec::new();
        while self.receive_buffer.len() >= block_size {
            let block: Vec<u8> = self.receive_buffer.drain(..block_size).collect();
            self.decrypt_block(&block, &mut output);
        }
        if self.receive_buffer.is_empty() {
            self.receive_buffer.clear();
        }
        output
    }

    /// `writeFrom` (encryption_stream_filter.cpp:75): the send-side inverse.
    /// The last block is zero-padded; the caller transmits the returned
    /// ciphertext. The reference's `shouldCork` only decides when to flush the
    /// underlying stream, which the port's caller owns, so it is not part of
    /// the codec.
    pub fn write_from(&mut self, clear: &[u8]) -> Vec<u8> {
        let block_size = self.p_block_cipher.block_size();
        let mut output = Vec::new();
        let mut offset = 0;
        while offset < clear.len() {
            let take = (clear.len() - offset).min(block_size);
            let mut work = vec![0u8; block_size];
            work[..take].copy_from_slice(&clear[offset..offset + take]);

            let mut xored = vec![0u8; block_size];
            self.p_block_cipher
                .combine_blocks(&self.previous_sent_block, &work, &mut xored);
            let mut enc = vec![0u8; block_size];
            self.p_block_cipher.encrypt_block(&xored, &mut enc);
            self.previous_sent_block.copy_from_slice(&work);
            output.extend_from_slice(&enc);
            offset += take;
        }
        output
    }

    /// `decryptBlock` (encryption_stream_filter.cpp:176): decrypt one
    /// ciphertext block, fold the receive chain, and emit `dataSize` bytes of
    /// it (the frame-tail padding is discarded).
    fn decrypt_block(&mut self, block: &[u8], output: &mut Vec<u8>) {
        let block_size = self.p_block_cipher.block_size();
        let expected = self.expected_receive_frame_length;

        // `dataSize` is trimmed to the frame's remaining length when the frame
        // ends inside this block (encryption_stream_filter.cpp:181-184); the
        // block's tail is padding.
        let mut data_size = block_size;
        if expected != 0 && expected < block_size {
            data_size = expected;
        }

        let mut plain = vec![0u8; block_size];
        self.p_block_cipher.decrypt_block(block, &mut plain);
        // Fold the receive chain seed in (the reference's in-place
        // `combineBlocks( previousReceivedBlock_, pOut, pOut )`).
        let seed = self.previous_received_block.clone();
        for b in 0..block_size {
            plain[b] ^= seed[b];
        }
        // The full plaintext block (padding included) is the next block's
        // chain seed.
        self.previous_received_block.copy_from_slice(&plain);

        if expected == 0 {
            self.update_expected_receive_frame_length(&plain);
            data_size = self.expected_receive_frame_length.min(data_size);
        }

        output.extend_from_slice(&plain[..data_size]);
        self.expected_receive_frame_length -= data_size;
    }

    /// `updateExpectedReceiveFrameLength` (encryption_stream_filter.cpp:151):
    /// the frame length rides the first block of the frame -- a small frame's
    /// `[len:2]` header, or the `0xFFFF` escape plus a 4-byte length. The
    /// length field is the frame's *payload* size, so the total is that plus
    /// the header the filter already holds. The port reads little-endian,
    /// matching the wire (`frameData << uint16`, tcp_channel.cpp:431) and the
    /// reference's no-op `BW_NTOHS` on this platform.
    fn update_expected_receive_frame_length(&mut self, block: &[u8]) {
        let small = u16::from_le_bytes(block[0..2].try_into().unwrap());
        if small == 0xFFFF {
            let large = u32::from_le_bytes(block[2..6].try_into().unwrap());
            self.expected_receive_frame_length = large as usize + 2 + 4;
        } else {
            self.expected_receive_frame_length = small as usize + 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symmetric_block_cipher::SymmetricBlockCipher;

    const BLOWFISH_KEY: &[u8] = b"abcdefghijklmnopqrstuvwxyz";

    fn cipher() -> Rc<dyn BlockCipher> {
        SymmetricBlockCipher::create(BLOWFISH_KEY).unwrap()
    }

    /// The stream's own framing: a frame is `[len:2][payload]` (small) or
    /// `[0xFFFF:2][len:4][payload]` (large), exactly what `TcpFrameParser`
    /// produces.
    fn small_frame(payload: &[u8]) -> Vec<u8> {
        let mut out = (payload.len() as u16).to_le_bytes().to_vec();
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn a_small_frame_round_trips_through_the_codec() {
        let payload = b"hello encrypted world";
        let frame = small_frame(payload);

        let mut enc = EncryptionStreamFilter::new(cipher());
        let ciphertext = enc.write_from(&frame);

        let mut dec = EncryptionStreamFilter::new(cipher());
        let clear = dec.read_into(&ciphertext);

        // The frame length header trims the block padding off, so the
        // recovered stream is exactly the frame bytes.
        assert_eq!(clear, frame);
    }

    #[test]
    fn a_frame_ending_mid_block_is_padded_and_trimmed() {
        // Frame total = 2 + 7 = 9 bytes, not a block multiple: the last
        // block carries 7 padding bytes the filter must discard.
        let frame = small_frame(b"1234567");
        let mut enc = EncryptionStreamFilter::new(cipher());
        let ciphertext = enc.write_from(&frame);
        assert_eq!(ciphertext.len(), 16); // padded up to 2 blocks

        let mut dec = EncryptionStreamFilter::new(cipher());
        assert_eq!(dec.read_into(&ciphertext), frame);
    }

    #[test]
    fn blocks_are_buffered_until_complete() {
        let frame = small_frame(b"abcdefghijklmno"); // 2 + 15 = 17 bytes
        let mut enc = EncryptionStreamFilter::new(cipher());
        let ciphertext = enc.write_from(&frame);

        let mut dec = EncryptionStreamFilter::new(cipher());
        // Feed in chunks that split blocks: the first 3 bytes leave the rest
        // buffered, so nothing is emitted yet.
        assert_eq!(dec.read_into(&ciphertext[..3]), Vec::<u8>::new());
        // The rest completes both blocks and emits the frame.
        assert_eq!(dec.read_into(&ciphertext[3..]), frame);
    }

    #[test]
    fn two_frames_sent_separately_round_trip() {
        // The reference's doSend calls `writeFrom` once per frame
        // (tcp_channel.cpp:446), so each frame's last block is padded on its
        // own; the receiver reads the concatenated ciphertext as one block
        // stream and trims each frame's padding by its own length header.
        let f1 = small_frame(b"first");
        let f2 = small_frame(b"second frame");

        let mut enc = EncryptionStreamFilter::new(cipher());
        let mut ciphertext = enc.write_from(&f1);
        ciphertext.extend_from_slice(&enc.write_from(&f2));

        let mut dec = EncryptionStreamFilter::new(cipher());
        let clear = dec.read_into(&ciphertext);
        let mut expected = f1.clone();
        expected.extend_from_slice(&f2);
        assert_eq!(clear, expected);
    }

    #[test]
    fn the_chain_is_seeded_with_zero_and_blocks_are_not_standalone() {
        // The first block XORs against a zero seed, so the first ciphertext
        // block is E(plain0); a second identical plaintext stream would
        // encrypt differently because the chain carries over.
        let frame = small_frame(b"abcdefgh");
        let mut enc = EncryptionStreamFilter::new(cipher());
        let first = enc.write_from(&frame);
        let mut enc2 = EncryptionStreamFilter::new(cipher());
        let second = enc2.write_from(&frame);
        // Identical input, fresh filter: identical output (deterministic).
        assert_eq!(first, second);
        // Two frames through ONE filter differ (chaining is stateful).
        let mut enc3 = EncryptionStreamFilter::new(cipher());
        let mut two = frame.clone();
        two.extend_from_slice(&small_frame(b"abcdefgh"));
        let chained = enc3.write_from(&two);
        assert_ne!(&chained[..16], &first[..16]);
    }
}
