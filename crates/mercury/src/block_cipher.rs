//! `Mercury::BlockCipher` (lib/network/block_cipher.{hpp,ipp}): the abstract
//! block-cipher seam the session encryption layers hang off.
//!
//! The reference has two concrete ciphers: `SymmetricBlockCipher` (Blowfish,
//! ECB) when OpenSSL is available, and `NullCipher` (the no-op identity, used
//! both when OpenSSL is disabled and in tests to lay the XOR chaining bare).
//! Both encrypt/decrypt a single `blockSize()`-sized block and expose the
//! shared `key()` for the filter to sanity-check.
//!
//! `combineBlocks` (block_cipher.ipp:18-42) XORs two blocks into a third; the
//! reference does it in `uint64` words where the platform allows, but byte-wise
//! XOR is the same result, so the port folds it into a plain loop.

/// `Mercury::BlockCipher` (block_cipher.hpp:22).
pub trait BlockCipher {
    /// `encryptBlock`: encrypt one block in place-of-destination. The caller
    /// guarantees both buffers are `blockSize()` bytes. False when the cipher
    /// has no key (the reference's `encryptBlock` returns false on an empty
    /// key string, symmetric_block_cipher.cpp).
    fn encrypt_block(&self, input: &[u8], output: &mut [u8]) -> bool;

    /// `decryptBlock`: the inverse of `encryptBlock`.
    fn decrypt_block(&self, input: &[u8], output: &mut [u8]) -> bool;

    /// `key()`: the shared key. Empty for `NullCipher`.
    fn key(&self) -> &[u8];

    /// `blockSize()`: the cipher's block size in bytes (8 for Blowfish).
    fn block_size(&self) -> usize;

    /// `combineBlocks` (block_cipher.ipp): XOR `in1` with `in2` into `out`,
    /// `blockSize()` bytes. `out` may alias either input.
    fn combine_blocks(&self, in1: &[u8], in2: &[u8], out: &mut [u8]) {
        for i in 0..self.block_size() {
            out[i] = in1[i] ^ in2[i];
        }
    }
}

/// `NullCipher` (block_cipher.hpp:70): the no-op cipher. It copies the block
/// through unchanged, carries an empty key (so the packet send/recv paths
/// refuse it, unlike the stream API), and reports a caller-chosen block size.
pub struct NullCipher {
    key: Vec<u8>,
    block_size: usize,
}

impl NullCipher {
    /// `NullCipher::create( blockSize )` (block_cipher.hpp:73).
    pub fn create(block_size: usize) -> std::rc::Rc<dyn BlockCipher> {
        std::rc::Rc::new(NullCipher {
            key: Vec::new(),
            block_size,
        })
    }
}

impl BlockCipher for NullCipher {
    fn encrypt_block(&self, input: &[u8], output: &mut [u8]) -> bool {
        output[..self.block_size].copy_from_slice(&input[..self.block_size]);
        true
    }

    fn decrypt_block(&self, input: &[u8], output: &mut [u8]) -> bool {
        output[..self.block_size].copy_from_slice(&input[..self.block_size]);
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

    #[test]
    fn null_cipher_copies_the_block_through() {
        let c = NullCipher::create(8);
        assert_eq!(c.block_size(), 8);
        assert!(c.key().is_empty());
        let input: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut out = [0u8; 8];
        assert!(c.encrypt_block(&input, &mut out));
        assert_eq!(out, input);
        assert!(c.decrypt_block(&input, &mut out));
        assert_eq!(out, input);
    }

    #[test]
    fn combine_blocks_is_a_bytewise_xor() {
        let c = NullCipher::create(8);
        let a: [u8; 8] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        let b: [u8; 8] = [0x0f, 0xf0, 0xaa, 0x55, 0x00, 0xff, 0x01, 0x80];
        let mut out = [0u8; 8];
        c.combine_blocks(&a, &b, &mut out);
        for i in 0..8 {
            assert_eq!(out[i], a[i] ^ b[i]);
        }
    }

    #[test]
    fn combine_blocks_allows_aliasing() {
        let c = NullCipher::create(8);
        let a: [u8; 8] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        let mut a_mut = a;
        // `out` aliases `in1`: XORing a with b into a itself.
        c.combine_blocks(&a, &[0xff; 8], &mut a_mut);
        for i in 0..8 {
            assert_eq!(a_mut[i], a[i] ^ 0xff);
        }
    }
}
