//! `Mercury::SymmetricBlockCipher` (lib/network/symmetric_block_cipher.
//! {hpp,cpp}): Blowfish in ECB mode over 8-byte blocks, the cipher the session
//! encryption uses. The reference leans on OpenSSL (`BF_set_key` /
//! `BF_ecb_encrypt`); the port leans on the RustCrypto `blowfish` crate, which
//! reproduces the same standard Blowfish (the wire tests pin this with Eric
//! Young's published vectors).
//!
//! The key is symmetric and shared up front -- it rides the login data from
//! the app layer (`SymmetricBlockCipher::create( encryptionKey )`,
//! loginapp.cpp:1318 and baseapp's proxy.cpp:614), so the port's factory is
//! the known-key one. The reference's generated-key factory `create( size_t )`
//! (OpenSSL `RAND_bytes`) is not ported: no app-layer call site uses it.

use std::rc::Rc;

use blowfish::Blowfish;
use blowfish::cipher::generic_array::GenericArray;
use blowfish::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};

use crate::block_cipher::BlockCipher;

/// `SymmetricBlockCipher` (symmetric_block_cipher.hpp:22): a symmetric block
/// cipher with a shared private key (Blowfish, ECB).
pub struct SymmetricBlockCipher {
    key: Vec<u8>,
    bf: Blowfish,
}

impl SymmetricBlockCipher {
    /// `MIN_KEY_SIZE`: `32 / NETWORK_BITS_PER_BYTE` (4 bytes).
    pub const MIN_KEY_SIZE: usize = 4;

    /// `MAX_KEY_SIZE`: `448 / NETWORK_BITS_PER_BYTE` (56 bytes).
    pub const MAX_KEY_SIZE: usize = 56;

    /// `DEFAULT_KEY_SIZE`: `128 / NETWORK_BITS_PER_BYTE` (16 bytes).
    pub const DEFAULT_KEY_SIZE: usize = 16;

    /// `Impl::BLOCK_SIZE`: `64 / NETWORK_BITS_PER_BYTE` -- Blowfish's
    /// 64-bit block (symmetric_block_cipher.cpp).
    pub const BLOCK_SIZE: usize = 8;

    /// `SymmetricBlockCipher::create( const Key & keyString )`
    /// (symmetric_block_cipher.cpp:270): factory for a cipher with a known
    /// key. The key must be `MIN_KEY_SIZE`..=`MAX_KEY_SIZE` bytes, otherwise
    /// `None` (the reference returns a NULL `BlockCipherPtr` after its
    /// ERROR_MSG).
    pub fn create(key: &[u8]) -> Option<Rc<dyn BlockCipher>> {
        if key.len() < Self::MIN_KEY_SIZE || key.len() > Self::MAX_KEY_SIZE {
            eprintln!(
                "SymmetricBlockCipher::create: Requested invalid known key of invalid size: {}",
                key.len()
            );
            return None;
        }
        let bf =
            <Blowfish as KeyInit>::new_from_slice(key).expect("Blowfish takes 4..=56 byte keys");
        Some(Rc::new(SymmetricBlockCipher {
            key: key.to_vec(),
            bf,
        }))
    }
}

impl BlockCipher for SymmetricBlockCipher {
    /// `Impl::encryptBlock` (symmetric_block_cipher.cpp:107): an empty key
    /// refuses; otherwise one Blowfish ECB block.
    fn encrypt_block(&self, input: &[u8], output: &mut [u8]) -> bool {
        if self.key.is_empty() {
            return false;
        }
        let mut block = GenericArray::clone_from_slice(&input[..Self::BLOCK_SIZE]);
        self.bf.encrypt_block(&mut block);
        output[..Self::BLOCK_SIZE].copy_from_slice(&block);
        true
    }

    /// `Impl::decryptBlock` (symmetric_block_cipher.cpp:126).
    fn decrypt_block(&self, input: &[u8], output: &mut [u8]) -> bool {
        if self.key.is_empty() {
            return false;
        }
        let mut block = GenericArray::clone_from_slice(&input[..Self::BLOCK_SIZE]);
        self.bf.decrypt_block(&mut block);
        output[..Self::BLOCK_SIZE].copy_from_slice(&block);
        true
    }

    /// `key()`: the shared key as created.
    fn key(&self) -> &[u8] {
        &self.key
    }

    /// `blockSize()`: 8 bytes.
    fn block_size(&self) -> usize {
        Self::BLOCK_SIZE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classic Eric Young Blowfish vector, as the reference's own unit
    /// test pins it (test_symmetric_block_cipher.cpp): key
    /// "abcdefghijklmnopqrstuvwxyz", plaintext "BLOWFISH" -> `32 4E D0 FE F4 13
    /// A2 03`. Verified independently against OpenSSL's Blowfish.
    const BLOWFISH_KEY: &[u8] = b"abcdefghijklmnopqrstuvwxyz";

    #[test]
    fn the_reference_known_answer_vector() {
        let cipher = SymmetricBlockCipher::create(BLOWFISH_KEY).unwrap();
        assert_eq!(cipher.block_size(), 8);

        let plaintext = b"BLOWFISH";
        let expected = [0x32, 0x4E, 0xD0, 0xFE, 0xF4, 0x13, 0xA2, 0x03];
        let mut ciphertext = [0u8; 8];
        let mut roundtrip = [0u8; 8];

        assert!(cipher.encrypt_block(plaintext, &mut ciphertext));
        assert_eq!(ciphertext, expected);

        assert!(cipher.decrypt_block(&ciphertext, &mut roundtrip));
        assert_eq!(&roundtrip, plaintext);
    }

    #[test]
    fn key_sizes_min_default_and_max_round_trip() {
        // test_symmetric_block_cipher.cpp's key_sizes: the boundaries of the
        // supported range.
        let key_sizes = [
            SymmetricBlockCipher::MIN_KEY_SIZE,
            SymmetricBlockCipher::DEFAULT_KEY_SIZE,
            SymmetricBlockCipher::MAX_KEY_SIZE,
        ];
        let block = [1u8, 2, 3, 4, 5, 6, 7, 8];
        for size in key_sizes {
            let cipher = SymmetricBlockCipher::create(&vec![7u8; size]).unwrap();
            assert_eq!(cipher.key().len(), size);
            let mut ciphertext = [0u8; 8];
            let mut roundtrip = [0u8; 8];
            assert!(cipher.encrypt_block(&block, &mut ciphertext));
            assert!(cipher.decrypt_block(&ciphertext, &mut roundtrip));
            assert_eq!(roundtrip, block);
        }
    }

    #[test]
    fn out_of_range_keys_are_refused() {
        // Below MIN and above MAX: the reference's factory returns NULL.
        assert!(SymmetricBlockCipher::create(&[1u8, 2, 3]).is_none());
        assert!(SymmetricBlockCipher::create(&[0u8; 57]).is_none());
    }

    #[test]
    fn ecb_is_deterministic_and_key_bound() {
        // test_symmetric_block_cipher.cpp's deterministic: two ciphers from
        // the same key agree, and a repeat encryption is identical (ECB).
        let key = b"0123456789abcdef0123456789abcdef";
        let a = SymmetricBlockCipher::create(key).unwrap();
        let b = SymmetricBlockCipher::create(key).unwrap();
        let block = [0xDE, 0xAD, 0xBE, 0xEF, 0xBA, 0xAD, 0xF0, 0x0D];
        let mut ct_a = [0u8; 8];
        let mut ct_b = [0u8; 8];
        let mut ct_again = [0u8; 8];
        assert!(a.encrypt_block(&block, &mut ct_a));
        assert!(b.encrypt_block(&block, &mut ct_b));
        assert_eq!(ct_a, ct_b);
        assert!(a.encrypt_block(&block, &mut ct_again));
        assert_eq!(ct_a, ct_again);
    }
}
