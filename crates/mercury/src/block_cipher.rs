//! `lib/network/block_cipher.{hpp,ipp}` (BW-02-05-02): the abstract block
//! cipher `EncryptionFilter` encrypts with, and its no-op implementation.
//!
//! A block cipher transforms one block of `blockSize()` bytes at a time.
//! The base also supplies the XOR helper `combineBlocks` that
//! `EncryptionFilter` uses to chain blocks; the reference implements it on
//! the base so every cipher shares it (block_cipher.ipp).
//!
//! Deviation: the reference's `encryptBlock`/`decryptBlock` take an input
//! and an output buffer that may alias, and every call site passes them
//! aliased (`encryptBlock( dest + i, dest + i )`); the port folds the two
//! into one in-place `&mut [u8]` argument, which is the only shape the
//! reference actually invokes.

use std::rc::Rc;

/// `BlockCipher::Key`: the raw key bytes.
pub type Key = Vec<u8>;

/// `Mercury::BlockCipher` (block_cipher.hpp:15): a block cipher with a
/// shared private key.
pub trait BlockCipher {
    /// `encryptBlock`: encrypt one block in place, honouring `blockSize()`.
    fn encrypt_block(&self, block: &mut [u8]) -> bool;

    /// `decryptBlock`: decrypt one block in place, honouring `blockSize()`.
    fn decrypt_block(&self, block: &mut [u8]) -> bool;

    /// `key()`: the key bytes. An empty key means the cipher is unusable --
    /// `EncryptionFilter` refuses to send or receive through it.
    fn key(&self) -> &[u8];

    /// `blockSize()`: the block size in bytes.
    fn block_size(&self) -> usize;

    /// `combineBlocks` (block_cipher.ipp): XOR two blocks into a
    /// destination. `out` may point at either input.
    fn combine_blocks(&self, in1: &[u8], in2: &[u8], out: &mut [u8]) {
        for i in 0..self.block_size() {
            out[i] = in1[i] ^ in2[i];
        }
    }
}

/// `Mercury::NullCipher` (block_cipher.hpp:84): a no-op cipher. Its key is
/// empty, so the packet send/recv paths refuse it; only the stream API is
/// usable, which is what the reference's tests use to lay the block chain
/// bare.
pub struct NullCipher {
    key: Key,
    block_size: usize,
}

impl NullCipher {
    /// `NullCipher::create( blockSize )`.
    pub fn create(block_size: usize) -> Rc<dyn BlockCipher> {
        Rc::new(Self {
            key: Key::new(),
            block_size,
        })
    }
}

impl BlockCipher for NullCipher {
    fn encrypt_block(&self, block: &mut [u8]) -> bool {
        // The identity: the reference copies `blockSize_` bytes onto
        // themselves.
        let _ = block;
        true
    }

    fn decrypt_block(&self, block: &mut [u8]) -> bool {
        let _ = block;
        true
    }

    fn key(&self) -> &[u8] {
        &self.key
    }

    fn block_size(&self) -> usize {
        self.block_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The null cipher is the identity and claims the requested block size.
    #[test]
    fn the_null_cipher_copies_blocks_through() {
        let cipher = NullCipher::create(8);
        assert_eq!(cipher.block_size(), 8);
        assert!(cipher.key().is_empty(), "the null cipher has no key");

        let mut block = *b"abcdefgh";
        assert!(cipher.encrypt_block(&mut block));
        assert_eq!(&block, b"abcdefgh", "encryption is the identity");
        assert!(cipher.decrypt_block(&mut block));
        assert_eq!(&block, b"abcdefgh", "decryption is the identity");
    }

    /// `combineBlocks` XORs the two blocks and honours the configured
    /// block size, reading that many bytes from each input (block_cipher.ipp
    /// reads `blockSize_`). The reference also allows the destination to
    /// alias one input; the port's `EncryptionFilter` never calls it that
    /// way, so only the non-aliased shape is exercised.
    #[test]
    fn combine_blocks_is_xor() {
        let cipher = NullCipher::create(8);
        let a = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let b = [0x0fu8, 0x0e, 0x0d, 0x0c, 0x0b, 0x0a, 0x09, 0x08];

        let mut out = [0u8; 8];
        cipher.combine_blocks(&a, &b, &mut out);
        assert_eq!(out, [0x0e, 0x0c, 0x0e, 0x08, 0x0e, 0x0c, 0x0e, 0x00]);

        // A larger configured block size XORs that many bytes.
        let big = NullCipher::create(16);
        let a16 = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10,
        ];
        let b16 = [
            0x0f, 0x0e, 0x0d, 0x0c, 0x0b, 0x0a, 0x09, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02,
            0x01, 0x00,
        ];
        let mut out16 = [0u8; 16];
        big.combine_blocks(&a16, &b16, &mut out16);
        assert_eq!(
            out16,
            [
                0x0e, 0x0c, 0x0e, 0x08, 0x0e, 0x0c, 0x0e, 0x00, 0x0e, 0x0c, 0x0e, 0x08, 0x0e, 0x0c,
                0x0e, 0x10
            ]
        );
    }
}
