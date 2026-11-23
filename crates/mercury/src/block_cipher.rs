//! `lib/network/block_cipher.{hpp,ipp}`: the abstract block cipher behind
//! `EncryptionFilter`, and the reference's no-op `NullCipher`.
//!
//! A `BlockCipher` is what makes an `EncryptionFilter` (BW-02-05-02) able to
//! turn a plain channel into an encrypted one: the filter owns a
//! `BlockCipherPtr` and calls `encrypt_block`/`decrypt_block` on every block
//! of every packet. The reference's `SymmetricBlockCipher` is real Blowfish;
//! this port ships the cipher seam plus the reference's faithful, non-crypto
//! `NullCipher` (block_cipher.hpp) and drives the filter with a keyed no-op
//! cipher in tests, so the whole negotiate-then-encrypt path runs without the
//! Blowfish dependency.

use std::rc::Rc;

/// `BlockCipher::Key`: the shared private key (a `BW::string`, i.e. bytes).
pub type Key = Vec<u8>;

/// `BlockCipherPtr`: the reference's `SmartPointer<BlockCipher>`, here an
/// `Rc<dyn BlockCipher>` (the filter is not `Send`/`Sync`, matching the
/// reference's single-threaded `SmartPointer` use on a channel).
pub type BlockCipherPtr = Rc<dyn BlockCipher>;

/// `lib/network/block_cipher.hpp`: an abstract block cipher. `encrypt_block`
/// and `decrypt_block` transform one block in place; `combine_blocks` (the
/// reference's `BlockCipher::combineBlocks`, block_cipher.ipp) XORs two
/// blocks into a third, which may alias either input. The `EncryptionFilter`
/// uses it to chain blocks across a packet (anti-replay).
pub trait BlockCipher {
    /// `encryptBlock( pInput, pOutput )`.
    fn encrypt_block(&self, input: &[u8], output: &mut [u8]) -> bool;

    /// `decryptBlock( pInput, pOutput )`: the inverse of `encrypt_block`.
    fn decrypt_block(&self, input: &[u8], output: &mut [u8]) -> bool;

    /// `key()`: the key used. An empty key makes the `EncryptionFilter`
    /// drop every packet (`key().empty()` in encryption_filter.cpp).
    fn key(&self) -> &Key;

    /// `blockSize()`: the cipher's block width in bytes.
    fn block_size(&self) -> usize;

    /// `combineBlocks( pIn1, pIn2, pOut )` (block_cipher.ipp): XOR two blocks
    /// into `pOut`, which may alias either input. The reference optimises the
    /// loop to 64-bit words on platforms that allow it; the byte-wise branch
    /// is the portable reference path and is what this port keeps, since the
    /// result is identical.
    fn combine_blocks(&self, in1: &[u8], in2: &[u8], out: &mut [u8]) {
        let n = self
            .block_size()
            .min(in1.len())
            .min(in2.len())
            .min(out.len());
        for i in 0..n {
            out[i] = in1[i] ^ in2[i];
        }
    }

    /// `readableKey()` (block_cipher.ipp): the key rendered as a spaced
    /// uppercase hex string.
    fn readable_key(&self) -> String {
        self.key()
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// `lib/network/block_cipher.hpp`: the reference's no-op block cipher.
/// Both directions memcpy one block, so the channel it encrypts is a plain
/// copy -- a faithful, non-crypto cipher available in the reference, useful
/// as a stand-in when only the filter's framing matters (the stream tests
/// of the reference, and the tests here).
pub struct NullCipher {
    key: Key,
    block_size: usize,
}

impl NullCipher {
    /// `NullCipher::create( blockSize )`: a no-op cipher with an empty key.
    pub fn create(block_size: usize) -> BlockCipherPtr {
        Rc::new(Self::new(block_size))
    }

    /// `NullCipher( blockSize )`: the constructor's default block size is 8
    /// (block_cipher.hpp), same as the reference's default.
    fn new(block_size: usize) -> Self {
        Self {
            key: Vec::new(),
            block_size,
        }
    }

    /// A no-op cipher with a key: unlike `create`, its key is non-empty, so
    /// the `EncryptionFilter` accepts it. The transforms are still plain
    /// copies (the key is not used to scramble), which is exactly what makes
    /// it a readable stand-in for a real keyed cipher in tests.
    pub fn with_key(key: Key, block_size: usize) -> BlockCipherPtr {
        Rc::new(Self { key, block_size })
    }
}

impl BlockCipher for NullCipher {
    fn encrypt_block(&self, input: &[u8], output: &mut [u8]) -> bool {
        let n = self.block_size.min(input.len()).min(output.len());
        output[..n].copy_from_slice(&input[..n]);
        true
    }

    fn decrypt_block(&self, input: &[u8], output: &mut [u8]) -> bool {
        self.encrypt_block(input, output)
    }

    fn key(&self) -> &Key {
        &self.key
    }

    fn block_size(&self) -> usize {
        self.block_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference's `NullCipher::create` (block_cipher.hpp) has an empty key
    /// and a default block size of 8.
    #[test]
    fn the_null_cipher_has_no_key_and_eight_byte_blocks() {
        let cipher = NullCipher::create(8);
        assert!(cipher.key().is_empty(), "the no-op cipher carries no key");
        assert_eq!(cipher.block_size(), 8);
    }

    /// `create( blockSize )`: the explicit block size.
    #[test]
    fn the_null_cipher_takes_the_requested_block_size() {
        let cipher = NullCipher::create(16);
        assert_eq!(cipher.block_size(), 16);
    }

    /// `encrypt_block`/`decrypt_block` are both plain copies, so a round trip
    /// is the identity -- the no-op cipher leaves data untouched.
    #[test]
    fn the_null_cipher_round_trips_as_the_identity() {
        let cipher = NullCipher::create(4);
        let plain = [0xAA, 0xBB, 0xCC, 0xDD];
        let mut ciphered = [0u8; 4];

        assert!(cipher.encrypt_block(&plain, &mut ciphered));
        assert_eq!(ciphered, plain, "encrypt is a copy");

        let mut cleared = [0u8; 4];
        assert!(cipher.decrypt_block(&ciphered, &mut cleared));
        assert_eq!(cleared, plain, "decrypt is a copy");
    }

    /// `combineBlocks` XORs two blocks into a third. The reference allows the
    /// destination to alias an input; the port's `&mut`/`&` signature cannot
    /// express that, and the filter never needs it (its decrypt keeps a copy
    /// of the previous block), so the non-aliasing form is the one tested.
    #[test]
    fn the_combine_blocks_xor_into_a_third_block() {
        let cipher = NullCipher::create(4);
        let a = [0xFF, 0xFF, 0x00, 0x00];
        let b = [0x00, 0x0F, 0xF0, 0xFF];
        let mut out = [0u8; 4];
        cipher.combine_blocks(&a, &b, &mut out);
        assert_eq!(out, [0xFF, 0xF0, 0xF0, 0xFF]);
    }

    /// `readableKey` renders the key as spaced uppercase hex.
    #[test]
    fn the_key_renders_as_spaced_uppercase_hex() {
        let cipher = NullCipher::with_key(vec![0xde, 0xad, 0xbe, 0xef], 8);
        assert_eq!(cipher.readable_key(), "DE AD BE EF");
    }
}
