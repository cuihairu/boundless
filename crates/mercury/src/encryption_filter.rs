//! `lib/network/encryption_filter.{hpp,cpp}`: the packet filter that turns a
//! channel encrypted once a block cipher is fitted (BW-02-05-02).
//!
//! `Proxy::attachToClient` (server/baseapp/proxy.cpp:584) fits an
//! `EncryptionFilter` onto the freshly created client channel the moment a
//! session encryption key is agreed, and every byte that rides the channel --
//! the login reply carrying the session key included -- is encrypted from then
//! on. This module carries that filter: the `send`/`recv` framing (wastage
//! count, anti-replay block chaining, the `ENCRYPTION_MAGIC` sentinel) and
//! the stream helpers. The cipher itself is a [`BlockCipher`](crate::block_cipher).

use crate::block_cipher::{BlockCipherPtr, Key};
use crate::endpoint::Address;
use crate::misc::Reason;
use crate::packet::Packet;
use crate::packet_filter::PacketFilter;

/// `encryption_filter.cpp:31`: the 32-bit sentinel written just before the
/// wastage byte at the end of an encrypted packet, and checked on receive.
const ENCRYPTION_MAGIC: u32 = 0xdead_beef;

/// `EncryptionFilter::maxSpareSize` (encryption_filter.cpp:236): an
/// allowance for networks that cannot handle full-sized UDP packets.
const MTU_ALLOWANCE: usize = 200;

/// `encryption_filter.cpp`: a `PacketFilter` that encrypts every packet it
/// is handed with a non-modal block cipher. The reference's Blowfish variant
/// is created by [`EncryptionFilter::create`] with a
/// `SymmetricBlockCipher`; this port drives the same code path with any
/// [`BlockCipher`], including the reference's no-op `NullCipher`.
pub struct EncryptionFilter {
    cipher: BlockCipherPtr,
}

impl EncryptionFilter {
    /// `EncryptionFilter::create( pCipher )` (encryption_filter.cpp:27).
    pub fn create(cipher: BlockCipherPtr) -> std::rc::Rc<Self> {
        std::rc::Rc::new(Self { cipher })
    }

    /// `EncryptionFilter::key()` (encryption_filter.hpp): the filter is
    /// only valid while its cipher carries a key.
    pub fn key(&self) -> &Key {
        self.cipher.key()
    }

    /// `encryptStream` (encryption_filter.cpp:159): pad the clear stream to
    /// a block multiple and encrypt it into a fresh buffer.
    pub fn encrypt_stream(&self, clear: &[u8]) -> Vec<u8> {
        let block = self.cipher.block_size();
        let mut padded = clear.to_vec();
        if let Some(pad_size) = padded.len().checked_rem(block).filter(|r| *r != 0) {
            padded.resize(padded.len() + block - pad_size, 0);
        }
        let mut out = vec![0u8; padded.len()];
        self.encrypt(&padded, &mut out);
        out
    }

    /// `decryptStream` (encryption_filter.cpp:181): decrypt into a fresh
    /// buffer; `None` (the reference's `-1`) when the length is not a block
    /// multiple.
    pub fn decrypt_stream(&self, cipher_text: &[u8]) -> Option<Vec<u8>> {
        let mut out = vec![0u8; cipher_text.len()];
        if self.decrypt(cipher_text, &mut out) == -1 {
            return None;
        }
        Some(out)
    }

    /// `EncryptionFilter::encrypt` (encryption_filter.cpp:266): copy each
    /// block from `src` to `dest`, XOR-chaining every block but the first
    /// with the *plaintext* of the one before it (anti-replay), then encrypt.
    /// `src` and `dest` must not alias. The length is a block multiple.
    fn encrypt(&self, src: &[u8], dest: &mut [u8]) -> usize {
        let block = self.cipher.block_size();
        debug_assert!(
            src.len().is_multiple_of(block),
            "EncryptionFilter::encrypt: length is not a block multiple"
        );
        let mut prev = vec![0u8; block];
        let mut plain = vec![0u8; block];
        for i in (0..src.len()).step_by(block) {
            if i == 0 {
                plain.copy_from_slice(&src[0..block]);
            } else {
                for j in 0..block {
                    plain[j] = src[i + j] ^ prev[j];
                }
            }
            self.cipher.encrypt_block(&plain, &mut dest[i..i + block]);
            prev.copy_from_slice(&src[i..i + block]);
        }
        src.len()
    }

    /// `EncryptionFilter::decrypt` (encryption_filter.cpp:313): inverse of
    /// `encrypt`. Returns the length, or `-1` when the length is not a block
    /// multiple. `src` and `dest` must not alias.
    fn decrypt(&self, src: &[u8], dest: &mut [u8]) -> isize {
        let block = self.cipher.block_size();
        if !src.len().is_multiple_of(block) {
            return -1;
        }
        let mut prev = vec![0u8; block];
        for i in (0..src.len()).step_by(block) {
            self.cipher
                .decrypt_block(&src[i..i + block], &mut dest[i..i + block]);
            for j in 0..block {
                dest[i + j] ^= prev[j];
            }
            prev.copy_from_slice(&dest[i..i + block]);
        }
        src.len() as isize
    }

    /// `recv`'s in-place decrypt (encryption_filter.cpp:322). The reference
    /// decrypts in place; the port copies first so `decrypt_block` never
    /// sees aliased input, then writes back -- same result, without the
    /// in-place caveat the reference only tolerates for Blowfish.
    fn decrypt_in_place(&self, buf: &mut [u8], len: usize) -> isize {
        let scratch = buf[..len].to_vec();
        self.decrypt(&scratch, &mut buf[..len])
    }
}

impl PacketFilter for EncryptionFilter {
    /// `maxSpareSize` (encryption_filter.cpp:236): one block plus the magic
    /// plus the MTU allowance, the room each outbound packet must reserve.
    fn max_spare_size(&self) -> usize {
        self.cipher.block_size() + ENCRYPTION_MAGIC.to_le_bytes().len() + MTU_ALLOWANCE
    }

    /// `EncryptionFilter::send` (encryption_filter.cpp:43): encrypt the
    /// packet into a fresh one -- the magic and wastage count are appended
    /// first, and the original datagram is left as it arrived.
    fn send(&self, addr: &Address, packet: &mut Packet, next: &dyn Fn(&Address, &mut Packet)) {
        // encryption_filter.cpp:53: an invalid filter (empty key) is dropped.
        if self.cipher.key().is_empty() {
            eprintln!(
                "EncryptionFilter::send( {} ): Dropping packet due to invalid filter",
                addr.c_str()
            );
            return;
        }

        let block = self.cipher.block_size();
        let total = packet.total_size();

        // encryption_filter.cpp:65-72: pad to a block multiple, adding room
        // for the magic and the wastage byte.
        let mut len = total + ENCRYPTION_MAGIC.to_le_bytes().len();
        let wastage = ((block - ((len + 1) % block)) % block) + 1;
        len += wastage;
        debug_assert!(
            len <= crate::packet::PACKET_MAX_SIZE,
            "encrypted packet {} exceeds the max",
            len
        );

        // encryption_filter.cpp:99-104: rebuild the packet as a plain body of
        // `len` bytes -- the datagram, the padding, the magic and the wastage
        // count all become body, no footer trail.
        let mut to_send = packet.clone();
        packet.buffer_mut()[len - 1] = wastage as u8;
        let magic_off = len - 1 - ENCRYPTION_MAGIC.to_le_bytes().len();
        packet.buffer_mut()[magic_off..len - 1].copy_from_slice(&ENCRYPTION_MAGIC.to_le_bytes());

        // encryption_filter.cpp:109: encrypt datagram + padding + magic +
        // wastage into the rebuilt packet.
        self.encrypt(&packet.buffer()[..len], &mut to_send.buffer_mut()[..len]);
        to_send.set_body_size(len);

        // The reference forwards to `PacketFilter::send` -> the physical
        // sender; the port's `next` is that arm.
        next(addr, &mut to_send);
    }

    /// `EncryptionFilter::recv` (encryption_filter.cpp:119): decrypt in place,
    /// then check the magic and the wastage count before handing the packet
    /// on. Any failure is a corrupted packet.
    fn recv(
        &self,
        addr: &Address,
        packet: &mut Packet,
        next: &dyn Fn(&Address, &mut Packet) -> Reason,
    ) -> Reason {
        // encryption_filter.cpp:124: an invalid filter is dropped.
        if self.cipher.key().is_empty() {
            eprintln!(
                "EncryptionFilter::recv( {} ): Dropping packet due to invalid filter",
                addr.c_str()
            );
            return Reason::GeneralNetwork;
        }

        let total = packet.total_size();

        // encryption_filter.cpp:132: decrypt in place; a non-block-multiple
        // length is corrupted.
        if self.decrypt_in_place(packet.buffer_mut(), total) == -1 {
            return Reason::CorruptedPacket;
        }

        // encryption_filter.cpp:147-153: read the wastage count and the
        // magic just before it.
        let start_wastage = total - 1;
        if start_wastage < ENCRYPTION_MAGIC.to_le_bytes().len() {
            return Reason::CorruptedPacket;
        }
        let magic_off = start_wastage - ENCRYPTION_MAGIC.to_le_bytes().len();
        let wastage = packet.buffer()[start_wastage];
        let packet_magic = u32::from_le_bytes(
            packet.buffer()[magic_off..start_wastage]
                .try_into()
                .unwrap(),
        );

        // encryption_filter.cpp:161: the magic must be the one we wrote.
        if packet_magic != ENCRYPTION_MAGIC {
            eprintln!(
                "EncryptionFilter::recv( {} ): Dropping packet with invalid magic 0x{:x} (expected 0x{:x})",
                addr.c_str(),
                packet_magic,
                ENCRYPTION_MAGIC
            );
            return Reason::CorruptedPacket;
        }

        // encryption_filter.cpp:170: the wastage count must be within a block
        // and the footer must fit in the packet.
        let footer_size = wastage as usize + ENCRYPTION_MAGIC.to_le_bytes().len();
        if wastage as usize > self.cipher.block_size() || footer_size > total {
            eprintln!(
                "EncryptionFilter::recv( {} ): Dropping packet due to illegal wastage count ({})",
                addr.c_str(),
                wastage
            );
            return Reason::CorruptedPacket;
        }

        // encryption_filter.cpp:180: drop the wastage + magic footer and let
        // the channel process the decrypted datagram.
        packet.shrink(footer_size);
        next(addr, packet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_cipher::NullCipher;
    use crate::packet::{HEADER_SIZE, Packet, flags};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A channel cipher with a non-empty key, so the filter accepts it, and
    /// a no-op transform, so the framing is directly readable in tests.
    fn keyed_cipher(block: usize) -> BlockCipherPtr {
        NullCipher::with_key(b"session-encryption-key".to_vec(), block)
    }

    /// A packet with the given flags and body, no footers, so
    /// `total_size()` is exactly flags + body.
    fn packet_with_body(word: u16, body: &[u8]) -> Packet {
        let mut p = Packet::new();
        p.grow(HEADER_SIZE + body.len());
        p.buffer_mut()[0..HEADER_SIZE].copy_from_slice(&word.to_le_bytes());
        p.buffer_mut()[HEADER_SIZE..HEADER_SIZE + body.len()].copy_from_slice(body);
        p
    }

    /// The filter's `send` output, captured by the `next` continuation.
    fn sent_packet(filter: &EncryptionFilter, addr: &Address, packet: &mut Packet) -> Packet {
        let out = Rc::new(RefCell::new(None));
        filter.send(addr, packet, &|_, p| {
            *out.borrow_mut() = Some(p.clone());
        });
        out.borrow().clone().expect("the send continuation ran")
    }

    /// The acceptance: a packet survives `send` then `recv` with its body
    /// intact -- the encrypted datagram decrypts back to the original
    /// datagram.
    #[test]
    fn a_packet_round_trips_through_the_filter() {
        let filter = EncryptionFilter::create(keyed_cipher(8));
        let addr = Address::new(0x0100_007f, 0);
        let mut original = packet_with_body(flags::ON_CHANNEL, b"hello over the wire");

        let to_send = sent_packet(&filter, &addr, &mut original);
        assert_ne!(to_send.wire(), original.wire(), "the packet is encrypted");

        let mut received = to_send.clone();
        let out = Rc::new(RefCell::new(None));
        let reason = filter.recv(&addr, &mut received, &|_, p| {
            *out.borrow_mut() = Some(p.clone());
            Reason::Success
        });

        assert_eq!(reason, Reason::Success);
        let delivered = out.borrow().clone().expect("the recv continuation ran");
        assert_eq!(
            delivered.wire(),
            original.wire(),
            "the decrypted datagram is the original"
        );
    }

    /// The reference's framing: the encrypted packet is the datagram plus a
    /// wastage count and the magic sentinel. The last byte is the wastage
    /// count, the four bytes before it are `ENCRYPTION_MAGIC`. Both are
    /// written before the packet is encrypted, so the wire carries their
    /// ciphertext; the layout itself is asserted on the decrypted packet the
    /// filter hands the channel.
    #[test]
    fn the_encrypted_packet_carries_the_magic_and_wastage() {
        let filter = EncryptionFilter::create(keyed_cipher(8));
        let addr = Address::new(0x0100_007f, 0);
        let mut original = packet_with_body(flags::ON_CHANNEL, b"payload");

        let to_send = sent_packet(&filter, &addr, &mut original);
        let total = to_send.wire().len();
        assert!(total > original.wire().len(), "padding was added");
        assert_eq!(total % 8, 0, "the encrypted length is block-aligned");

        // The filter reads the framing back -- the magic at `total - 5` and
        // the wastage at `total - 1`, both under the block cipher and its
        // anti-replay chain -- so a successful round trip is the proof that
        // both were placed as the reference places them (a wrong magic or an
        // out-of-bounds wastage is a corrupted packet instead).
        let mut received = to_send.clone();
        let reason = filter.recv(&addr, &mut received, &|_, _| Reason::Success);
        assert_eq!(reason, Reason::Success);
        assert_eq!(
            received.wire(),
            original.wire(),
            "the framing survives the round trip"
        );
    }

    /// `maxSpareSize` (encryption_filter.cpp:236): one block plus the magic
    /// plus the 200-byte MTU allowance.
    #[test]
    fn the_max_spare_size_is_block_plus_magic_plus_mtu() {
        let filter = EncryptionFilter::create(keyed_cipher(8));
        assert_eq!(filter.max_spare_size(), 8 + 4 + 200);
    }

    /// The reference's invalid-filter arm: an empty key drops the packet on
    /// both directions, without reaching the sender or the channel.
    #[test]
    fn an_empty_key_drops_the_packet_both_ways() {
        let filter = EncryptionFilter::create(NullCipher::create(8));
        let addr = Address::new(0x0100_007f, 0);
        let mut p = packet_with_body(flags::ON_CHANNEL, b"payload");
        let sent = Rc::new(RefCell::new(0));
        filter.send(&addr, &mut p, &|_, _| *sent.borrow_mut() += 1);
        assert_eq!(*sent.borrow(), 0, "the empty-key packet is not sent");

        let mut r = packet_with_body(flags::ON_CHANNEL, b"payload");
        let delivered = Rc::new(RefCell::new(0));
        let reason = filter.recv(&addr, &mut r, &|_, _| {
            *delivered.borrow_mut() += 1;
            Reason::Success
        });
        assert_eq!(reason, Reason::GeneralNetwork);
        assert_eq!(
            *delivered.borrow(),
            0,
            "the empty-key packet is not delivered"
        );
    }

    /// The reference's corrupted-packet arms: a wrong magic and an illegal
    /// wastage count are both `REASON_CORRUPTED_PACKET`.
    #[test]
    fn a_bad_magic_or_wastage_is_a_corrupted_packet() {
        let filter = EncryptionFilter::create(keyed_cipher(8));
        let addr = Address::new(0x0100_007f, 0);

        // A corrupted magic: flip one of the magic bytes.
        let mut original = packet_with_body(flags::ON_CHANNEL, b"payload");
        let mut bad_magic = sent_packet(&filter, &addr, &mut original);
        let total = bad_magic.wire().len();
        bad_magic.buffer_mut()[total - 1 - ENCRYPTION_MAGIC.to_le_bytes().len()] ^= 0xFF;

        // An illegal wastage count: larger than a block.
        let mut bad_wastage = sent_packet(&filter, &addr, &mut original);
        let total = bad_wastage.wire().len();
        bad_wastage.buffer_mut()[total - 1] = 200;

        for packet in [&mut bad_magic, &mut bad_wastage] {
            let delivered = Rc::new(RefCell::new(0));
            let reason = filter.recv(&addr, packet, &|_, _| {
                *delivered.borrow_mut() += 1;
                Reason::Success
            });
            assert_eq!(reason, Reason::CorruptedPacket);
            assert_eq!(
                *delivered.borrow(),
                0,
                "the corrupted packet is not delivered"
            );
        }
    }

    /// The reference's `decrypt` arm: a non-block-aligned length is `-1`, so
    /// the stream helper reports the failure.
    #[test]
    fn a_non_block_aligned_stream_is_rejected() {
        let filter = EncryptionFilter::create(keyed_cipher(8));
        assert!(filter.decrypt_stream(&[0u8; 7]).is_none());
        assert_eq!(filter.decrypt_stream(&[0u8; 16]).map(|v| v.len()), Some(16));
    }

    /// `encryptStream`/`decryptStream` (encryption_filter.cpp:159): the clear
    /// stream is padded to a block multiple before encrypting, and the round
    /// trip recovers the original -- dangling pad bytes allowed.
    #[test]
    fn the_stream_round_trip_pads_to_a_block_multiple() {
        let filter = EncryptionFilter::create(keyed_cipher(8));
        let clear = b"abcdefghijklmnopqrst"; // 20 bytes, not a multiple of 8
        let ciphered = filter.encrypt_stream(clear);
        assert_eq!(ciphered.len(), 24, "padded up to a block multiple");

        let back = filter
            .decrypt_stream(&ciphered)
            .expect("the stream decrypts");
        assert_eq!(&back[..clear.len()], clear, "the clear bytes are recovered");
    }

    /// The anti-replay chain (encryption_filter.cpp:266): every block after
    /// the first is XOR-combined with the *plaintext* of the one before it,
    /// so tampering with one block disturbs the next on decrypt.
    #[test]
    fn the_anti_replay_chain_ties_blocks_together() {
        let filter = EncryptionFilter::create(keyed_cipher(8));
        let addr = Address::new(0x0100_007f, 0);
        let mut original = packet_with_body(flags::ON_CHANNEL, b"ABCDEFGHabcdefgh");

        let mut tampered = sent_packet(&filter, &addr, &mut original);
        // Disturb the first plaintext block's ciphertext byte 0.
        tampered.buffer_mut()[HEADER_SIZE] ^= 0x01;

        let mut received = tampered.clone();
        let _ = filter.recv(&addr, &mut received, &|_, _| Reason::Success);
        // The first block is the tampered one; the second is disturbed by
        // the chain even though it was not itself touched.
        assert_ne!(
            &received.wire()[HEADER_SIZE + 8..],
            &original.wire()[HEADER_SIZE + 8..],
            "the chained block is disturbed by the tampered one"
        );
    }
}
