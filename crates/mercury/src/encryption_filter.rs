//! `Mercury::EncryptionFilter` (lib/network/encryption_filter.{hpp,cpp}): the
//! `PacketFilter` that encrypts the datagrams on a channel with a
//! non-modal block cipher. It is the filter `UDPChannel::setEncryption`
//! installs (udp_channel.cpp:675-687), so an encrypted client channel is the
//! filter the interface's found-channel `pFilter` arm hands every datagram to
//! (wire compatibility with the original client is the point of the layer).
//!
//! On the wire each datagram becomes a whole number of Blowfish blocks with a
//! `4-byte magic + 1-byte wastage` footer.
//!
//! `encrypt` (encryption_filter.cpp:271) pads `packetSize + 4` up to a block
//! multiple (`wastage` is computed so the total is a multiple, and is always at
//! least one), then XORs each block after the first against the previous plain
//! block before encrypting -- this is the anti-replay / anti-reassembly
//! chaining. `recv` (encryption_filter.cpp:126) decrypts in place, checks the
//! footer magic, validates the wastage, shrinks the footer off, then hands the
//! plaintext to the base `PacketFilter::recv` (the receive path).
//!
//! The stream API (`encryptStream`/`decryptStream`) is the same cipher in a
//! stream shape: zero-pad the clear stream to a block multiple, chain-encrypt
//! into the cipher stream. The port takes it over `Vec<u8>` rather than the
//! reference's `MemoryOStream`, because mercury's bundle streams are byte
//! vectors.

use std::rc::Rc;

use crate::block_cipher::BlockCipher;
use crate::endpoint::Address;
use crate::misc::Reason;
use crate::network_interface::NetworkInterface;
use crate::packet::Packet;
use crate::packet_filter::PacketFilter;

/// `MagicType ENCRYPTION_MAGIC = 0xdeadbeef` (encryption_filter.cpp:18).
pub const ENCRYPTION_MAGIC: u32 = 0xdeadbeef;

/// `MTU_ALLOWANCE` in `maxSpareSize` (encryption_filter.cpp:227): room for
/// networks that cannot carry full-sized UDP packets.
const MTU_ALLOWANCE: usize = 200;

/// `Mercury::EncryptionFilter` (encryption_filter.hpp:29): a `PacketFilter`
/// that uses a non-modal block cipher.
pub struct EncryptionFilter {
    p_block_cipher: Rc<dyn BlockCipher>,
}

impl EncryptionFilter {
    /// `EncryptionFilter::create( pCipher )` (encryption_filter.cpp:31).
    pub fn create(p_block_cipher: Rc<dyn BlockCipher>) -> Rc<EncryptionFilter> {
        Rc::new(EncryptionFilter { p_block_cipher })
    }

    /// `key()` (encryption_filter.hpp:36): the shared key.
    pub fn key(&self) -> &[u8] {
        self.p_block_cipher.key()
    }

    /// `maxSpareSize()` (encryption_filter.cpp:223): the block size plus the
    /// magic footer plus the MTU allowance.
    pub fn max_spare_size(&self) -> usize {
        self.p_block_cipher.block_size() + std::mem::size_of::<u32>() + MTU_ALLOWANCE
    }

    /// `encryptStream` (encryption_filter.cpp:248): pad `clear` in place up
    /// to a whole number of blocks, then chain-encrypt it into the returned
    /// cipher stream. The padded clear carries the zero padding at its tail,
    /// matching the reference's in-place `MemoryOStream` reserve.
    pub fn encrypt_stream(&self, clear: &mut Vec<u8>) -> Vec<u8> {
        let block_size = self.p_block_cipher.block_size();
        let remainder = clear.len() % block_size;
        if remainder != 0 {
            clear.resize(clear.len() + (block_size - remainder), 0);
        }
        self.chain_encrypt(clear)
    }

    /// `decryptStream` (encryption_filter.cpp:273): the inverse of
    /// `encryptStream`. A cipher stream that is not a whole number of blocks
    /// is refused (`None`, the reference's `false`); the recovered plaintext
    /// keeps the zero padding that encryption added.
    pub fn decrypt_stream(&self, cipher: &[u8]) -> Option<Vec<u8>> {
        self.chain_decrypt(cipher)
    }

    /// `EncryptionFilter::send` (encryption_filter.cpp:49): the send-side
    /// transform -- build the footer (magic + wastage) past the packet's
    /// original end, then encrypt the whole thing. The port's signature
    /// returns the ready datagram instead of driving a `PacketSender`
    /// (mercury has no send path wired yet, so the caller would transmit
    /// `out.wire()`); see the deviation note in the review doc.
    pub fn encrypt_packet(&self, src: &Packet) -> Option<Packet> {
        if self.p_block_cipher.key().is_empty() {
            eprintln!("EncryptionFilter::encrypt_packet: Dropping packet due to invalid filter");
            return None;
        }

        let block_size = self.p_block_cipher.block_size();
        // `len = packetSize + sizeof(ENCRYPTION_MAGIC)` (encryption_filter.cpp:71).
        let mut len = src.total_size() + std::mem::size_of::<u32>();
        // `wastage = ((BLOCK_SIZE - ((len + 1) % BLOCK_SIZE)) % BLOCK_SIZE) + 1`
        // (encryption_filter.cpp:75-77): >= 1, and makes the total a block
        // multiple.
        let wastage = (block_size - ((len + 1) % block_size)) % block_size + 1;
        len += wastage;

        // Build the plaintext to encrypt: the original datagram followed by
        // the magic and the wastage byte at the very end.
        let mut plain = vec![0u8; len];
        plain[..src.total_size()].copy_from_slice(src.wire());
        let start_wastage = len - 1;
        let start_magic = start_wastage - std::mem::size_of::<u32>();
        plain[start_wastage] = wastage as u8;
        plain[start_magic..start_magic + 4].copy_from_slice(&ENCRYPTION_MAGIC.to_le_bytes());

        let cipher_stream = self.chain_encrypt(&plain);

        let mut out = Packet::new();
        out.buffer_mut()[..len].copy_from_slice(&cipher_stream);
        out.grow(len);
        Some(out)
    }

    /// The chained encrypt of one datagram/stream (encryption_filter.cpp:271).
    fn chain_encrypt(&self, clear: &[u8]) -> Vec<u8> {
        let block_size = self.p_block_cipher.block_size();
        let mut out = vec![0u8; clear.len()];
        let mut previous: Vec<u8> = vec![0u8; block_size];
        let mut have_previous = false;
        for (i, chunk) in clear.chunks_exact(block_size).enumerate() {
            let mut work = vec![0u8; block_size];
            if have_previous {
                // `combineBlocks( src + i, pPrevBlock, dest + i )`: XOR with
                // the previous PLAINTEXT block.
                for b in 0..block_size {
                    work[b] = chunk[b] ^ previous[b];
                }
            } else {
                work.copy_from_slice(chunk);
            }
            self.p_block_cipher
                .encrypt_block(&work, &mut out[i * block_size..i * block_size + block_size]);
            previous.copy_from_slice(chunk);
            have_previous = true;
        }
        out
    }

    /// The chained decrypt of one datagram/stream (encryption_filter.cpp:319).
    fn chain_decrypt(&self, cipher: &[u8]) -> Option<Vec<u8>> {
        let block_size = self.p_block_cipher.block_size();
        if !cipher.len().is_multiple_of(block_size) {
            return None;
        }
        let mut out = vec![0u8; cipher.len()];
        let mut previous: Vec<u8> = vec![0u8; block_size];
        let mut have_previous = false;
        for (i, chunk) in cipher.chunks_exact(block_size).enumerate() {
            let mut plain = vec![0u8; block_size];
            self.p_block_cipher.decrypt_block(chunk, &mut plain);
            if have_previous {
                // Inverse of the encrypt XOR: fold the previous plaintext
                // back off the decrypted block.
                for b in 0..block_size {
                    plain[b] ^= previous[b];
                }
            }
            out[i * block_size..i * block_size + block_size].copy_from_slice(&plain);
            previous.copy_from_slice(&plain);
            have_previous = true;
        }
        Some(out)
    }

    /// Decrypt `data` in place, the reference's `decrypt` applied to the whole
    /// packet (encryption_filter.cpp:340). False when the length is not a
    /// block multiple.
    fn decrypt_in_place(&self, data: &mut [u8]) -> bool {
        let block_size = self.p_block_cipher.block_size();
        if !data.len().is_multiple_of(block_size) {
            return false;
        }
        let mut previous: Vec<u8> = vec![0u8; block_size];
        let mut have_previous = false;
        for chunk in data.chunks_exact_mut(block_size) {
            let mut plain = vec![0u8; block_size];
            self.p_block_cipher.decrypt_block(chunk, &mut plain);
            if have_previous {
                for b in 0..block_size {
                    plain[b] ^= previous[b];
                }
            }
            chunk.copy_from_slice(&plain);
            previous.copy_from_slice(&plain);
            have_previous = true;
        }
        true
    }

    /// `PacketFilter::recv` (encryption_filter.cpp:126): decrypt the datagram
    /// in place, validate the magic footer, then hand the plaintext to the
    /// receive path.
    fn decrypt_datagram(
        &self,
        interface: &NetworkInterface,
        addr: &Address,
        packet: &mut Packet,
    ) -> Reason {
        if self.p_block_cipher.key().is_empty() {
            if interface.is_verbose() {
                eprintln!(
                    "EncryptionFilter::recv: Dropping packet from {} due to invalid filter",
                    addr.c_str()
                );
            }
            return Reason::GeneralNetwork;
        }

        let total_size = packet.total_size();
        let block_size = self.p_block_cipher.block_size();
        let decrypted = {
            let data = &mut packet.buffer_mut()[..total_size];
            self.decrypt_in_place(data)
        };
        if !decrypted {
            interface
                .receive_stats()
                .borrow_mut()
                .inc_corrupted_packets();
            return Reason::CorruptedPacket;
        }

        let start_wastage = total_size - 1;
        if start_wastage < std::mem::size_of::<u32>() {
            interface
                .receive_stats()
                .borrow_mut()
                .inc_corrupted_packets();
            return Reason::CorruptedPacket;
        }
        let start_magic = start_wastage - std::mem::size_of::<u32>();
        let wastage = packet.buffer_mut()[start_wastage] as usize;
        let magic = u32::from_le_bytes(
            packet.buffer_mut()[start_magic..start_magic + 4]
                .try_into()
                .unwrap(),
        );
        if magic != ENCRYPTION_MAGIC {
            if interface.is_verbose() {
                eprintln!(
                    "EncryptionFilter::recv: Dropping packet with invalid magic {:#x} (expected {:#x})",
                    magic, ENCRYPTION_MAGIC
                );
            }
            interface
                .receive_stats()
                .borrow_mut()
                .inc_corrupted_packets();
            return Reason::CorruptedPacket;
        }

        let footer_size = wastage + std::mem::size_of::<u32>();
        if wastage > block_size || footer_size > total_size {
            if interface.is_verbose() {
                eprintln!(
                    "EncryptionFilter::recv: Dropping packet due to illegal wastage count ({})",
                    wastage
                );
            }
            interface
                .receive_stats()
                .borrow_mut()
                .inc_corrupted_packets();
            return Reason::CorruptedPacket;
        }

        packet.shrink(footer_size);
        Reason::Success
    }
}

impl PacketFilter for EncryptionFilter {
    /// `EncryptionFilter::recv` (encryption_filter.cpp:126): the filter's
    /// verdict is the datagram's fate; a valid decrypt reaches the base
    /// `PacketFilter::recv` pass-through (the receive path).
    fn recv(&self, interface: &NetworkInterface, addr: &Address, packet: &mut Packet) -> Reason {
        let reason = self.decrypt_datagram(interface, addr, packet);
        if reason != Reason::Success {
            return reason;
        }
        interface.process_filtered_packet(addr, packet)
    }

    /// `maxSpareSize()` (encryption_filter.cpp:223).
    fn max_spare_size(&self) -> usize {
        EncryptionFilter::max_spare_size(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_cipher::NullCipher;
    use crate::endpoint::Address;
    use crate::event_dispatcher::EventDispatcher;
    use crate::misc::{CHANNEL_ID_NULL, Reason};
    use crate::network_interface::InterfaceType;
    use crate::packet::{Packet, flags};
    use crate::packet_receiver::tests_support::RecordingSink;
    use crate::symmetric_block_cipher::SymmetricBlockCipher;
    use crate::udp_channel::{
        DEFAULT_INACTIVITY_RESEND_DELAY, Traits as ChannelTraits, UdpChannel,
    };
    use platform::socket;
    use std::cell::Cell;
    use std::rc::Rc;

    /// The reference's own fixture (test_encryption_filter.cpp): a 26-byte
    /// Blowfish key and a two-block plaintext of visually distinct blocks.
    const BLOWFISH_KEY: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
    const PLAIN_TWO_BLOCKS: [u8; 16] = [
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
        0x00,
    ];

    fn cipher() -> Rc<dyn BlockCipher> {
        SymmetricBlockCipher::create(BLOWFISH_KEY).unwrap()
    }

    fn filter() -> Rc<EncryptionFilter> {
        EncryptionFilter::create(cipher())
    }

    /// A socketless interface with a pinned clock, as the network_interface
    /// tests drive it.
    fn test_interface() -> Rc<NetworkInterface> {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let clock_cell = Rc::clone(&clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || {
            clock_cell.get()
        })));
        NetworkInterface::new_with_dispatcher(InterfaceType::Internal, dispatcher)
    }

    fn wire(flags: u16, body: &[u8]) -> Packet {
        let mut packet = Packet::new();
        packet.set_flags(flags);
        packet.grow(2 + body.len());
        packet.buffer_mut()[2..2 + body.len()].copy_from_slice(body);
        packet
    }

    /// A valid on-channel datagram, as `ordered_wire` builds it.
    fn ordered_wire(seq: u32, msg: u8) -> Packet {
        let mut body = vec![msg];
        body.extend_from_slice(&seq.to_le_bytes());
        wire(
            flags::ON_CHANNEL | flags::IS_RELIABLE | flags::HAS_SEQUENCE_NUMBER,
            &body,
        )
    }

    /// A channel registered at `port` with the session encryption installed.
    fn encrypted_channel(
        interface: &Rc<NetworkInterface>,
        port: u16,
        cipher: Option<Rc<dyn BlockCipher>>,
    ) -> crate::udp_channel::ChannelPtr {
        let addr = Address::new(0x0100_007f, socket::htons(port));
        let channel = UdpChannel::new(
            interface,
            addr,
            ChannelTraits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::set_encryption(&channel, cipher);
        channel
    }

    // -- the stream API, byte-exact against the reference oracle ----------

    /// test_encryption_filter.cpp's `streamRoundTrip`: 20 bytes (not a block
    /// multiple) pad to 24 in place, the cipher stream is exactly the padded
    /// length, and the recovered plaintext carries the zero tail.
    #[test]
    fn the_reference_stream_round_trip() {
        let f = filter();
        let mut clear = PLAIN_TWO_BLOCKS.to_vec();
        clear.extend_from_slice(b"four");

        let cipher_stream = f.encrypt_stream(&mut clear);
        assert_eq!(clear.len(), 24);
        assert_eq!(cipher_stream.len(), 24);

        // The wire bytes OpenSSL's Blowfish (the reference's) produce for
        // this exact clear stream -- the interop proof.
        assert_eq!(
            cipher_stream,
            [
                0xef, 0x51, 0x74, 0x6f, 0x6c, 0x3a, 0x38, 0x35, 0x2d, 0x32, 0x2d, 0x8e, 0x89, 0xd9,
                0xd7, 0x47, 0xc4, 0x32, 0x1a, 0xc0, 0xa1, 0x04, 0x40, 0x7a
            ]
        );

        let recovered = f.decrypt_stream(&cipher_stream).unwrap();
        assert_eq!(recovered.len(), 24);
        assert_eq!(&recovered[..16], &PLAIN_TWO_BLOCKS);
        assert_eq!(&recovered[16..20], b"four");
        assert_eq!(&recovered[20..], &[0u8; 4]);
    }

    /// `streamRoundTripExactMultiple`: a whole number of blocks needs no
    /// padding; the cipher text is the same length and the round trip exact.
    #[test]
    fn the_reference_exact_multiple_stream_round_trip() {
        let f = filter();
        let mut clear = PLAIN_TWO_BLOCKS.to_vec();
        let cipher_stream = f.encrypt_stream(&mut clear);
        assert_eq!(clear.len(), 16);
        assert_eq!(cipher_stream.len(), 16);
        assert_eq!(
            cipher_stream,
            [
                0xef, 0x51, 0x74, 0x6f, 0x6c, 0x3a, 0x38, 0x35, 0x2d, 0x32, 0x2d, 0x8e, 0x89, 0xd9,
                0xd7, 0x47
            ]
        );
        assert_eq!(f.decrypt_stream(&cipher_stream).unwrap(), PLAIN_TWO_BLOCKS);
    }

    /// `nullCipherRevealsChain`: with the identity cipher the chained XOR is
    /// laid bare -- block 0 passes through, every later block is the plain
    /// block XORed with the one before.
    #[test]
    fn the_reference_null_cipher_reveals_the_chain() {
        let f = EncryptionFilter::create(NullCipher::create(8));
        assert!(f.key().is_empty());
        let mut clear = PLAIN_TWO_BLOCKS.to_vec();
        let cipher_stream = f.encrypt_stream(&mut clear);
        assert_eq!(cipher_stream.len(), 16);

        // Block 0 copied through.
        assert_eq!(&cipher_stream[..8], &PLAIN_TWO_BLOCKS[..8]);
        // Block 1 is plaintext block 1 XOR plaintext block 0.
        for i in 0..8 {
            assert_eq!(
                cipher_stream[8 + i],
                PLAIN_TWO_BLOCKS[8 + i] ^ PLAIN_TWO_BLOCKS[i]
            );
        }
        // And decrypting folds the chain back.
        assert_eq!(f.decrypt_stream(&cipher_stream).unwrap(), PLAIN_TWO_BLOCKS);
    }

    /// `decryptStreamRejectsNonMultiple`: a cipher text that is not a whole
    /// number of blocks is refused.
    #[test]
    fn the_reference_decrypt_stream_rejects_a_non_multiple() {
        let f = filter();
        assert!(f.decrypt_stream(&[0xa5u8; 10]).is_none());
    }

    /// `blockRearrangementBreaksChain`: swapping two cipher blocks corrupts
    /// exactly the leading block; the chain self-heals after one block.
    #[test]
    fn the_reference_block_rearrangement_breaks_the_chain() {
        let f = filter();
        let mut clear = PLAIN_TWO_BLOCKS.to_vec();
        let mut cipher_stream = f.encrypt_stream(&mut clear);
        cipher_stream.swap(0, 8);
        cipher_stream.swap(1, 9);
        cipher_stream.swap(2, 10);
        cipher_stream.swap(3, 11);
        cipher_stream.swap(4, 12);
        cipher_stream.swap(5, 13);
        cipher_stream.swap(6, 14);
        cipher_stream.swap(7, 15);

        let recovered = f.decrypt_stream(&cipher_stream).unwrap();
        // The leading block is garbled.
        assert_ne!(&recovered[..8], &PLAIN_TWO_BLOCKS[..8]);
        // The chain has self-healed: the second block is exactly P1.
        assert_eq!(&recovered[8..], &PLAIN_TWO_BLOCKS[8..]);
        // Overall the plaintext is not recovered.
        assert_ne!(recovered, PLAIN_TWO_BLOCKS);
    }

    /// `maxSpareSize`: block size + 4-byte magic + 200-byte MTU allowance.
    #[test]
    fn the_reference_max_spare_size() {
        assert_eq!(filter().max_spare_size(), 212);
        let null = EncryptionFilter::create(NullCipher::create(16));
        assert_eq!(null.max_spare_size(), 220);
    }

    /// `key()` reports the cipher's key.
    #[test]
    fn the_filter_reports_the_shared_key() {
        assert_eq!(filter().key(), BLOWFISH_KEY);
    }

    // -- the packet path, through the channel filter ----------------------

    /// A datagram encrypted for a channel decrypts back to the plaintext the
    /// receive path dispatches: the same datagram sent bare to a channel
    /// without session encryption dispatches the very same packet.
    #[test]
    fn an_encrypted_datagram_reaches_the_dispatch_through_the_channel_filter() {
        let interface = test_interface();
        let cipher = cipher();
        let enc_addr = Address::new(0x0100_007f, socket::htons(9201));
        let bare_addr = Address::new(0x0100_007f, socket::htons(9206));
        let _encrypted = encrypted_channel(&interface, 9201, Some(cipher.clone()));
        let _bare = encrypted_channel(&interface, 9206, None);
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        let mut encrypted = EncryptionFilter::create(cipher)
            .encrypt_packet(&ordered_wire(0, 0x41))
            .unwrap();
        assert_eq!(
            interface.process_packet(&enc_addr, &mut encrypted),
            Reason::Success
        );
        assert_eq!(
            sink.packets.borrow().len(),
            1,
            "the datagram reached the dispatch"
        );

        // The identical plaintext, bare, to the reference channel.
        let mut bare = ordered_wire(0, 0x41);
        assert_eq!(
            interface.process_packet(&bare_addr, &mut bare),
            Reason::Success
        );
        assert_eq!(sink.packets.borrow().len(), 2);
        assert_eq!(
            sink.packets.borrow()[1].1,
            sink.packets.borrow()[0].1,
            "the encrypted datagram dispatched exactly what the bare one did"
        );
    }

    /// The wire datagram is a whole number of blocks, and the plaintext it
    /// carries ends in the reference's footer: the 4-byte magic (little-
    /// endian) with the wastage byte behind it, accounting for exactly the
    /// padding that block-aligned the buffer.
    #[test]
    fn the_encrypted_wire_is_block_aligned_with_a_magic_footer() {
        let f = filter();
        let plain = ordered_wire(0, 0x41);
        let encrypted = f.encrypt_packet(&plain).unwrap();
        let wire = encrypted.wire();
        assert_eq!(
            wire.len() % 8,
            0,
            "the datagram is a whole number of blocks"
        );

        // The footer rides inside the encrypted stream; decode it back out.
        let decoded = f.decrypt_stream(wire).unwrap();
        let start_magic = decoded.len() - 1 - 4;
        assert_eq!(
            u32::from_le_bytes(decoded[start_magic..start_magic + 4].try_into().unwrap()),
            ENCRYPTION_MAGIC
        );
        let wastage = decoded[decoded.len() - 1] as usize;
        assert!((1..=8).contains(&wastage));
        assert_eq!(decoded.len(), plain.total_size() + 4 + wastage);
    }

    /// A datagram without the magic footer is corrupted and never reaches the
    /// dispatch.
    #[test]
    fn a_datagram_with_a_bad_magic_is_corrupted() {
        let interface = test_interface();
        let addr = Address::new(0x0100_007f, socket::htons(9202));
        let cipher = cipher();
        encrypted_channel(&interface, 9202, Some(cipher.clone()));
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        let mut encrypted = EncryptionFilter::create(cipher)
            .encrypt_packet(&ordered_wire(0, 0x41))
            .unwrap();
        // Corrupt the magic.
        let magic_at = encrypted.wire().len() - 1 - 4;
        encrypted.buffer_mut()[magic_at] ^= 0xff;

        assert_eq!(
            interface.process_packet(&addr, &mut encrypted),
            Reason::CorruptedPacket
        );
        assert!(sink.packets.borrow().is_empty());
    }

    /// A datagram whose length is not a whole number of blocks is corrupted.
    #[test]
    fn a_datagram_of_a_non_block_length_is_corrupted() {
        let interface = test_interface();
        let addr = Address::new(0x0100_007f, socket::htons(9203));
        encrypted_channel(&interface, 9203, Some(cipher()));
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        let mut bad = Packet::new();
        bad.grow(10); // 10 is not a block multiple.
        assert_eq!(
            interface.process_packet(&addr, &mut bad),
            Reason::CorruptedPacket
        );
        assert!(sink.packets.borrow().is_empty());
    }

    /// A datagram whose wastage count is illegal is corrupted.
    #[test]
    fn a_datagram_with_an_illegal_wastage_is_corrupted() {
        let interface = test_interface();
        let addr = Address::new(0x0100_007f, socket::htons(9204));
        let cipher = cipher();
        encrypted_channel(&interface, 9204, Some(cipher.clone()));
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        let mut encrypted = EncryptionFilter::create(cipher)
            .encrypt_packet(&ordered_wire(0, 0x41))
            .unwrap();
        // Wastage beyond the block size.
        let last = encrypted.wire().len() - 1;
        encrypted.buffer_mut()[last] = 9;

        assert_eq!(
            interface.process_packet(&addr, &mut encrypted),
            Reason::CorruptedPacket
        );
        assert!(sink.packets.borrow().is_empty());
    }

    /// A cipher with an empty key refuses the datagram outright (the
    /// reference's `key().empty()` bail).
    #[test]
    fn an_invalid_filter_drops_the_packet() {
        let interface = test_interface();
        let addr = Address::new(0x0100_007f, socket::htons(9205));
        encrypted_channel(&interface, 9205, Some(NullCipher::create(8)));
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));

        let mut plain = ordered_wire(0, 0x41);
        assert_eq!(
            interface.process_packet(&addr, &mut plain),
            Reason::GeneralNetwork
        );
        assert!(sink.packets.borrow().is_empty());
    }

    /// The send-side transform refuses an empty key, as `send` does.
    #[test]
    fn encrypt_packet_refuses_an_empty_key() {
        let f = EncryptionFilter::create(NullCipher::create(8));
        assert!(f.encrypt_packet(&ordered_wire(0, 0x41)).is_none());
    }

    /// Encrypting a packet and decrypting the stream recovers the plaintext:
    /// the packet wire and the stream API share the cipher and the chaining.
    #[test]
    fn an_encrypted_packet_stream_decrypts_back() {
        let plain = ordered_wire(7, 0x55);
        let encrypted = filter().encrypt_packet(&plain).unwrap();
        let recovered = filter().decrypt_stream(encrypted.wire()).unwrap();
        // The packet's own bytes, with the footer bytes still on the tail.
        assert_eq!(&recovered[..plain.total_size()], plain.wire());
    }
}
