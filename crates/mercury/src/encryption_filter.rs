//! `lib/network/encryption_filter.{hpp,cpp}` (BW-02-05-02): the per-channel
//! packet filter that encrypts and decrypts a channel's datagrams -- the
//! Mercury layer of session negotiation. The login handlers hand a
//! `BlockCipher` built from the session key to `UDPChannel::setEncryption`,
//! which installs one of these on the channel, and from then on every
//! packet in or out of the channel is encrypted.
//!
//! Framing (encryption_filter.cpp:63-118): outbound, the filter pads the
//! datagram up to a whole number of cipher blocks, writes a
//! `0xdeadbeef` magic and a one-byte wastage count into the trailing pad
//! bytes, and encrypts the lot; the base `send` then forwards the cipher
//! text. Inbound (encryption_filter.cpp:120-206) it decrypts in place,
//! checks the magic and the wastage sanity, and shrinks the datagram back
//! to its real length before the base `recv` takes the header walk. The
//! block chaining is a plaintext-feedback CBC the filter runs itself over
//! the cipher's ECB primitive, so rearranged blocks break the chain
//! (encryption_filter.cpp:305-384).
//!
//! Deviation: the reference's `send` writes the magic and wastage into the
//! spare room of the packet it was handed; the port's `send` takes the
//! packet by `&Rc<Packet>`, so it builds the framed bytes in a scratch
//! buffer instead. The original packet is left untouched either way, and
//! the bytes that reach the wire are identical.

use std::rc::Rc;

use crate::block_cipher::BlockCipher;
use crate::endpoint::Address;
use crate::misc::Reason;
use crate::network_interface::NetworkInterface;
use crate::packet::Packet;
use crate::packet_filter::PacketFilter;
use crate::packet_receiver::PacketReceiver;
use crate::packet_sender::PacketSender;
use crate::udp_channel::ChannelPtr;

/// `ENCRYPTION_MAGIC` (encryption_filter.cpp:26): written at the end of
/// the plaintext so a decrypted stream is recognised. Host-endian on the
/// wire, as the reference reads and writes it through a `MagicType` cast
/// (little-endian here).
const ENCRYPTION_MAGIC: u32 = 0xdeadbeef;

/// `sizeof( ENCRYPTION_MAGIC )`.
const ENCRYPTION_MAGIC_SIZE: usize = 4;

/// `MTU_ALLOWANCE` (encryption_filter.cpp:297): headroom for networks
/// whose packets cannot fill a UDP datagram, e.g. a VPN.
const MTU_ALLOWANCE: usize = 200;

/// `Mercury::EncryptionFilter` (encryption_filter.hpp:26): a `PacketFilter`
/// that encrypts through a non-modal block cipher.
pub struct EncryptionFilter {
    /// `pBlockCipher_`.
    cipher: Rc<dyn BlockCipher>,
}

impl EncryptionFilter {
    /// `EncryptionFilter::create` (encryption_filter.cpp:31).
    pub fn create(cipher: Rc<dyn BlockCipher>) -> Rc<EncryptionFilter> {
        Rc::new(Self { cipher })
    }

    /// `key()`.
    pub fn key(&self) -> &[u8] {
        self.cipher.key()
    }

    /// `encryptStream` (encryption_filter.cpp:210): pad `clear` up to a
    /// whole number of blocks in place, then append the cipher text to
    /// `cipher_stream`. The port takes plain byte buffers in place of the
    /// reference's `MemoryOStream`/`BinaryOStream` pair, which grow the
    /// same way.
    pub fn encrypt_stream(&self, clear: &mut Vec<u8>, cipher_stream: &mut Vec<u8>) {
        let block = self.cipher.block_size();
        let remainder = clear.len() % block;
        if remainder != 0 {
            clear.resize(clear.len() + (block - remainder), 0);
        }

        let mut cipher_text = vec![0u8; clear.len()];
        self.encrypt(clear, &mut cipher_text);
        cipher_stream.extend_from_slice(&cipher_text);
    }

    /// `decryptStream` (encryption_filter.cpp:230): decrypt `cipher` into
    /// the tail of `clear`, growing it first. `false` if the cipher text is
    /// not a whole number of blocks -- the reference still grows the output
    /// stream before the failed attempt, and this port does the same.
    pub fn decrypt_stream(&self, cipher: &[u8], clear: &mut Vec<u8>) -> bool {
        let start = clear.len();
        clear.resize(start + cipher.len(), 0);
        self.decrypt(cipher, &mut clear[start..])
    }

    /// `encrypt( src, dest, length )` (encryption_filter.cpp:305): the
    /// plaintext-feedback chain -- each block is XORed with the previous
    /// plaintext block before the cipher, so blocks cannot be reassembled
    /// across packets. `src` and `dest` must not alias.
    fn encrypt(&self, src: &[u8], dest: &mut [u8]) {
        let block = self.cipher.block_size();
        debug_assert_eq!(src.len() % block, 0);
        debug_assert_eq!(src.len(), dest.len());

        let mut prev: Option<&[u8]> = None;
        for i in (0..src.len()).step_by(block) {
            let this = &src[i..i + block];
            let out = &mut dest[i..i + block];
            match prev {
                None => out.copy_from_slice(this),
                Some(previous) => self.cipher.combine_blocks(this, previous, out),
            }
            self.cipher.encrypt_block(out);
            prev = Some(this);
        }
    }

    /// `decrypt( src, dest, length )` (encryption_filter.cpp:355): the
    /// inverse of `encrypt`. `false` if the length is not a whole number of
    /// blocks. `src` and `dest` must not alias.
    fn decrypt(&self, src: &[u8], dest: &mut [u8]) -> bool {
        let block = self.cipher.block_size();
        if !src.len().is_multiple_of(block) {
            return false;
        }

        for i in (0..src.len()).step_by(block) {
            let out = &mut dest[i..i + block];
            out.copy_from_slice(&src[i..i + block]);
            self.cipher.decrypt_block(out);
        }
        // Fold the chain back: block i is XORed with the plaintext block
        // before it, in place, after every block is decrypted.
        for i in (block..dest.len()).step_by(block) {
            for k in 0..block {
                let previous = dest[i + k - block];
                dest[i + k] ^= previous;
            }
        }
        true
    }

    /// `decrypt( dest, dest, length )`: the receive path's in-place
    /// decrypt, the shape the reference uses for a packet it just read
    /// (encryption_filter.cpp:146).
    fn decrypt_in_place(&self, data: &mut [u8]) -> bool {
        let block = self.cipher.block_size();
        if !data.len().is_multiple_of(block) {
            return false;
        }

        for i in (0..data.len()).step_by(block) {
            let out = &mut data[i..i + block];
            self.cipher.decrypt_block(out);
        }
        for i in (block..data.len()).step_by(block) {
            for k in 0..block {
                let previous = data[i + k - block];
                data[i + k] ^= previous;
            }
        }
        true
    }
}

impl PacketFilter for EncryptionFilter {
    /// `maxSpareSize`: the trait hook the channel consults when a packet
    /// opens (packet_filter.hpp:61), so it must be the same value as the
    /// inherent accessor -- without this, `Rc<dyn PacketFilter>` sees the
    /// default 0 and `setEncryption` reserves no headroom.
    fn max_spare_size(&self) -> usize {
        self.cipher.block_size() + ENCRYPTION_MAGIC_SIZE + MTU_ALLOWANCE
    }

    /// `send` (encryption_filter.cpp:53): frame the packet -- pad it up to
    /// a whole number of blocks, stamp the magic and wastage into the tail,
    /// encrypt it -- and forward the cipher text through the base.
    fn send(&self, sender: &mut PacketSender, addr: &Address, packet: &Rc<Packet>) -> Reason {
        if self.cipher.key().is_empty() {
            eprintln!(
                "EncryptionFilter::send: Dropping packet to {} due to invalid filter",
                addr.c_str()
            );
            return Reason::GeneralNetwork;
        }

        // `len`: the datagram, plus the magic footer, padded to a whole
        // number of blocks. `wastage` counts the trailing pad bytes
        // including the wastage byte itself (>= 1), so the last byte of
        // the framed data is always the wastage count.
        let block = self.cipher.block_size();
        let orig_len = packet.total_size();
        let len_with_magic = orig_len + ENCRYPTION_MAGIC_SIZE;
        let wastage = ((block - ((len_with_magic + 1) % block)) % block) + 1;
        let total_len = len_with_magic + wastage;
        let start_wastage = total_len - 1;
        let start_magic = start_wastage - ENCRYPTION_MAGIC_SIZE;

        let mut plain = vec![0u8; total_len];
        plain[..orig_len].copy_from_slice(packet.wire());
        plain[start_magic..start_magic + ENCRYPTION_MAGIC_SIZE]
            .copy_from_slice(&ENCRYPTION_MAGIC.to_le_bytes());
        plain[start_wastage] = wastage as u8;

        let mut cipher_text = vec![0u8; total_len];
        self.encrypt(&plain, &mut cipher_text);

        let mut to_send = Packet::new();
        to_send.grow(total_len);
        to_send.buffer_mut()[..total_len].copy_from_slice(&cipher_text);

        sender.basic_send_with_retries(addr, &Rc::new(to_send))
    }

    /// `recv` (encryption_filter.cpp:120): decrypt in place, check the
    /// magic and wastage, shrink back to the real length, and hand the
    /// plaintext to the header walk.
    fn recv(
        &self,
        receiver: &PacketReceiver,
        interface: &Rc<NetworkInterface>,
        addr: &Address,
        packet: &mut Packet,
        channel: Option<ChannelPtr>,
    ) -> Reason {
        if self.cipher.key().is_empty() {
            if interface.is_verbose() {
                eprintln!(
                    "EncryptionFilter::recv: Dropping packet from {} due to invalid filter",
                    addr.c_str()
                );
            }
            return Reason::GeneralNetwork;
        }

        let total_len = packet.total_size();
        if !self.decrypt_in_place(&mut packet.buffer_mut()[..total_len]) {
            receiver.inc_corrupted_packets();
            return Reason::CorruptedPacket;
        }

        let start_wastage = total_len - 1;
        if start_wastage < ENCRYPTION_MAGIC_SIZE {
            receiver.inc_corrupted_packets();
            return Reason::CorruptedPacket;
        }

        let start_magic = start_wastage - ENCRYPTION_MAGIC_SIZE;
        let wastage = packet.buffer_mut()[start_wastage];
        let mut magic_bytes = [0u8; ENCRYPTION_MAGIC_SIZE];
        magic_bytes.copy_from_slice(
            &packet.buffer_mut()[start_magic..start_magic + ENCRYPTION_MAGIC_SIZE],
        );
        let packet_magic = u32::from_le_bytes(magic_bytes);

        if packet_magic != ENCRYPTION_MAGIC {
            if interface.is_verbose() {
                eprintln!(
                    "EncryptionFilter::recv: Dropping packet with invalid magic 0x{:x} (expected 0x{:x})",
                    packet_magic, ENCRYPTION_MAGIC
                );
            }
            receiver.inc_corrupted_packets();
            return Reason::CorruptedPacket;
        }

        let footer_size = wastage as usize + ENCRYPTION_MAGIC_SIZE;
        if wastage as usize > self.cipher.block_size() || footer_size > total_len {
            if interface.is_verbose() {
                eprintln!(
                    "EncryptionFilter::recv: Dropping packet from {} due to illegal wastage count ({})",
                    addr.c_str(),
                    wastage
                );
            }
            receiver.inc_corrupted_packets();
            return Reason::CorruptedPacket;
        }

        packet.shrink(footer_size);

        receiver.process_filtered_packet(interface, addr, packet, channel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::Address;
    use crate::network_interface::InterfaceType;
    use crate::symmetric_block_cipher::SymmetricBlockCipher;
    use crate::udp_channel::Traits as ChannelTraits;
    use crate::udp_channel::{ChannelPtr, UdpChannel};
    use platform::socket;

    const LOOPBACK: u32 = 0x0100_007f;
    const BLOWFISH_KEY: &[u8] = b"abcdefghijklmnopqrstuvwxyz";

    /// The reference's two visually distinct blocks
    /// (test_encryption_filter.cpp:22): 0x11..0x88 then 0x99..0xff,0x00.
    const PLAIN_TWO_BLOCKS: [u8; 16] = [
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
        0x00,
    ];

    fn blowfish() -> Rc<EncryptionFilter> {
        EncryptionFilter::create(
            SymmetricBlockCipher::create_with_key(BLOWFISH_KEY).expect("key in range"),
        )
    }

    // ------------------------------------------------------------------
    // Stream API (the reference's test_encryption_filter.cpp)
    // ------------------------------------------------------------------

    /// 20 bytes is not a whole number of blocks: the clear stream pads in
    /// place to 24, the cipher text is exactly the padded length, and the
    /// round trip recovers the plaintext with a zero tail.
    #[test]
    fn stream_round_trip_pads_to_a_block() {
        let filter = blowfish();

        let mut clear = PLAIN_TWO_BLOCKS.to_vec();
        clear.extend_from_slice(b"four");
        assert_eq!(clear.len(), 20);

        let mut cipher = Vec::new();
        filter.encrypt_stream(&mut clear, &mut cipher);
        assert_eq!(clear.len(), 24, "the clear stream is padded in place");
        assert_eq!(cipher.len(), 24, "the cipher text is the padded length");

        let mut recovered = Vec::new();
        assert!(filter.decrypt_stream(&cipher, &mut recovered));
        assert_eq!(recovered.len(), 24);
        assert_eq!(&recovered[..16], &PLAIN_TWO_BLOCKS);
        assert_eq!(&recovered[20..], &[0u8; 4], "the padding decrypts to zeros");
    }

    /// An exact multiple needs no padding and the round trip is exact.
    #[test]
    fn stream_round_trip_exact_multiple() {
        let filter = blowfish();

        let mut clear = PLAIN_TWO_BLOCKS.to_vec();
        let mut cipher = Vec::new();
        filter.encrypt_stream(&mut clear, &mut cipher);
        assert_eq!(clear.len(), 16);
        assert_eq!(cipher.len(), 16);

        let mut recovered = Vec::new();
        assert!(filter.decrypt_stream(&cipher, &mut recovered));
        assert_eq!(recovered, PLAIN_TWO_BLOCKS);
    }

    /// The null cipher lays the block chain bare: the first block passes
    /// through and every later block is the plaintext XORed with the one
    /// before it (the reference's `EncryptionFilter_nullCipherRevealsChain`).
    #[test]
    fn null_cipher_reveals_the_chain() {
        let filter = EncryptionFilter::create(crate::block_cipher::NullCipher::create(8));
        assert!(filter.key().is_empty(), "the null cipher has no key");

        let mut clear = PLAIN_TWO_BLOCKS.to_vec();
        let mut cipher = Vec::new();
        filter.encrypt_stream(&mut clear, &mut cipher);

        assert_eq!(
            &cipher[..8],
            &PLAIN_TWO_BLOCKS[..8],
            "block 0 is copied through"
        );
        let mut expected_second = [0u8; 8];
        for i in 0..8 {
            expected_second[i] = PLAIN_TWO_BLOCKS[8 + i] ^ PLAIN_TWO_BLOCKS[i];
        }
        assert_eq!(&cipher[8..], &expected_second, "block 1 is P1 XOR P0");

        let mut recovered = Vec::new();
        assert!(filter.decrypt_stream(&cipher, &mut recovered));
        assert_eq!(
            recovered, PLAIN_TWO_BLOCKS,
            "decrypting folds the chain back"
        );
    }

    /// A cipher text that is not a whole number of blocks is refused, but
    /// the output stream still grows (the reference's
    /// `EncryptionFilter_decryptStreamRejectsNonMultiple`).
    #[test]
    fn decrypt_stream_rejects_a_non_multiple() {
        let filter = blowfish();

        let cipher = [0xa5u8; 10];
        let mut recovered = Vec::new();
        assert!(!filter.decrypt_stream(&cipher, &mut recovered));
        assert_eq!(
            recovered.len(),
            10,
            "the reserve happens before the attempt"
        );
    }

    /// Swapping two cipher blocks garbles only the leading block of the
    /// moved region; the chain self-heals after one block (the reference's
    /// `EncryptionFilter_blockRearrangementBreaksChain`).
    #[test]
    fn block_rearrangement_breaks_the_chain() {
        let filter = blowfish();

        let mut clear = PLAIN_TWO_BLOCKS.to_vec();
        let mut cipher = Vec::new();
        filter.encrypt_stream(&mut clear, &mut cipher);

        let (head, tail) = cipher.split_at_mut(8);
        head.swap_with_slice(&mut tail[..8]);

        let mut recovered = Vec::new();
        assert!(filter.decrypt_stream(&cipher, &mut recovered));
        assert_ne!(
            &recovered[..8],
            &PLAIN_TWO_BLOCKS[..8],
            "the leading block is garbled"
        );
        assert_eq!(
            &recovered[8..],
            &PLAIN_TWO_BLOCKS[8..],
            "the chain self-heals"
        );
        assert_ne!(recovered, PLAIN_TWO_BLOCKS);
    }

    /// `maxSpareSize`: block size + 4-byte magic + 200-byte MTU allowance,
    /// scaling with the cipher's block size (the reference's
    /// `EncryptionFilter_maxSpareSize`).
    #[test]
    fn max_spare_size() {
        assert_eq!(blowfish().max_spare_size(), 212);
        let null16 = EncryptionFilter::create(crate::block_cipher::NullCipher::create(16));
        assert_eq!(null16.max_spare_size(), 220);
    }

    // ------------------------------------------------------------------
    // Packet send/recv path (the framing `setEncryption` installs)
    // ------------------------------------------------------------------

    /// A channel with an encryption filter sends cipher text: the datagram
    /// that leaves is padded up to a block and decrypts back to the
    /// original bytes with the right length.
    #[test]
    fn the_send_path_encrypts_the_datagram() {
        let (interface, _receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let channel = external_channel(&interface, addr);
        let filter = blowfish();
        UdpChannel::set_p_filter(&channel, Some(filter.clone() as Rc<dyn PacketFilter>));

        // 14-byte body: not a whole number of 8-byte blocks.
        let mut packet = Packet::new();
        packet.grow(14);
        packet.buffer_mut()[..14].copy_from_slice(b"hello world!!!");
        interface.send_packet(&addr, &Rc::new(packet), Some(&channel), false);

        // The datagram that reached the wire is 14 + 4 (magic) + 6
        // (wastage) = 24 bytes, a whole number of blocks.
        assert_eq!(interface.sending_stats().num_packets_sent, 1);
    }

    /// `recv` decrypts a framed datagram back to its plaintext and shrinks
    /// it to the real length. Built by hand with the filter's own framing,
    /// then run through the receive path.
    #[test]
    fn recv_decrypts_and_restores_the_length() {
        let (interface, receiver) = NetworkInterface::new(InterfaceType::External, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let filter = blowfish();

        let plain = b"hello world!!!"; // 14 bytes
        let total = 14 + ENCRYPTION_MAGIC_SIZE + 6; // wastage = 6
        let mut framed = vec![0u8; total];
        framed[..14].copy_from_slice(plain);
        let start_wastage = total - 1;
        let start_magic = start_wastage - ENCRYPTION_MAGIC_SIZE;
        framed[start_magic..start_magic + ENCRYPTION_MAGIC_SIZE]
            .copy_from_slice(&ENCRYPTION_MAGIC.to_le_bytes());
        framed[start_wastage] = (total - 14 - ENCRYPTION_MAGIC_SIZE) as u8;
        let mut cipher_text = vec![0u8; total];
        filter.encrypt(&framed, &mut cipher_text);

        let mut packet = Packet::new();
        packet.grow(total);
        packet.buffer_mut()[..total].copy_from_slice(&cipher_text);

        let reason = filter.recv(&receiver, &interface, &addr, &mut packet, None);
        assert_eq!(
            reason,
            Reason::CorruptedPacket,
            "the walk refuses the short body"
        );
        assert_eq!(packet.total_size(), 14, "the footer is stripped");
        assert_eq!(&packet.buffer_mut()[..14], plain);
    }

    /// A corrupted magic makes the receive path drop the packet and count
    /// it, the reference's `REASON_CORRUPTED_PACKET` (encryption_filter.cpp:
    /// 175-188).
    #[test]
    fn bad_magic_is_corrupted() {
        let (interface, receiver) = NetworkInterface::new(InterfaceType::External, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let filter = blowfish();

        // One whole block of cipher text decrypts fine but has a bad magic.
        let framed = vec![0u8; 8];
        let mut cipher_text = vec![0u8; 8];
        filter.encrypt(&framed, &mut cipher_text);
        cipher_text[0] ^= 0xff; // keep it a block, break the plaintext

        let mut packet = Packet::new();
        packet.grow(8);
        packet.buffer_mut()[..8].copy_from_slice(&cipher_text);

        let reason = filter.recv(&receiver, &interface, &addr, &mut packet, None);
        assert_eq!(reason, Reason::CorruptedPacket);
        assert_eq!(receiver.stats().num_corrupted_packets_received, 1);
    }

    /// A datagram that is not a whole number of blocks is refused before
    /// any magic check (encryption_filter.cpp:147-160).
    #[test]
    fn non_multiple_datagram_is_corrupted() {
        let (interface, receiver) = NetworkInterface::new(InterfaceType::External, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let filter = blowfish();

        let mut packet = Packet::new();
        packet.grow(10);
        packet.buffer_mut()[..10].copy_from_slice(&[0xabu8; 10]);

        let reason = filter.recv(&receiver, &interface, &addr, &mut packet, None);
        assert_eq!(reason, Reason::CorruptedPacket);
        assert_eq!(receiver.stats().num_corrupted_packets_received, 1);
    }

    /// An empty-key filter refuses every packet with
    /// `REASON_GENERAL_NETWORK` (encryption_filter.cpp:64-77): this is what
    /// the reference's no-OpenSSL build amounts to.
    #[test]
    fn an_empty_key_filter_drops_packets() {
        let filter = EncryptionFilter::create(crate::block_cipher::NullCipher::create(8));

        let (interface, _receiver) = NetworkInterface::new(InterfaceType::External, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let mut packet = Packet::new();
        packet.grow(8);

        let reason = filter.recv(&_receiver, &interface, &addr, &mut packet, None);
        assert_eq!(reason, Reason::GeneralNetwork);
    }

    /// `set_encryption` installs a filter when given a cipher and clears
    /// the channel's filter when given `None`, matching
    /// `UDPChannel::setEncryption` (udp_channel.cpp:670-687).
    #[test]
    fn set_encryption_installs_and_clears() {
        let (interface, _receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let channel = external_channel(&interface, addr);
        assert!(UdpChannel::p_filter(&channel).is_none());

        let cipher = SymmetricBlockCipher::create_with_key(BLOWFISH_KEY).unwrap();
        UdpChannel::set_encryption(&channel, Some(cipher));
        let installed = UdpChannel::p_filter(&channel).expect("a filter is installed");
        assert_eq!(
            installed.max_spare_size(),
            212,
            "the encryption filter claims the Blowfish spare room"
        );

        UdpChannel::set_encryption(&channel, None);
        assert!(
            UdpChannel::p_filter(&channel).is_none(),
            "a null cipher clears the filter"
        );
    }

    fn external_channel(interface: &Rc<NetworkInterface>, addr: Address) -> ChannelPtr {
        UdpChannel::new(
            interface,
            addr,
            ChannelTraits::External,
            crate::udp_channel::DEFAULT_INACTIVITY_RESEND_DELAY,
            crate::misc::CHANNEL_ID_NULL,
        )
    }
}
