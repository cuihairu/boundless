//! Binary blocks and binary-backed sections, following
//! `resmgr/binary_block.{hpp,cpp}` and `resmgr/bin_section.cpp`.
//!
//! `BinaryBlock` is the reference-counted raw byte holder of the reference;
//! ownership is native in Rust, so only the externally observable parts
//! remain: the bytes, the length and the comparison. Compression
//! (`BinaryBlock::compress`/`decompress`) arrives with BW-01-02-04 and is
//! intentionally absent here.

/// A block of binary data (`BinaryBlock`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BinaryBlock {
    data: Vec<u8>,
}

impl BinaryBlock {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }

    /// Raw bytes (`BinaryBlock::data`).
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Length in bytes (`BinaryBlock::len`, the reference narrows to int).
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// `BinaryBlock::compare`: bytewise comparison over the shorter length,
    /// then by length.
    pub fn compare(&self, other: &Self) -> std::cmp::Ordering {
        self.data.cmp(&other.data)
    }
}

impl AsRef<[u8]> for BinaryBlock {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

/// A named view onto a binary block (`BinSection`): the tag plus the owned
/// bytes. `save` writes the bytes verbatim; `asBinary` hands them back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinSection {
    tag: String,
    block: BinaryBlock,
}

impl BinSection {
    pub fn new(tag: &str, block: BinaryBlock) -> Self {
        Self {
            tag: tag.to_string(),
            block,
        }
    }

    /// `BinSection::sectionName`.
    pub fn section_name(&self) -> &str {
        &self.tag
    }

    /// `BinSection::asBinary`.
    pub fn as_binary(&self) -> &BinaryBlock {
        &self.block
    }

    /// `BinSection::save` against an in-memory sink: the whole block is
    /// written unchanged.
    pub fn save_bytes(&self) -> Vec<u8> {
        self.block.data().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_as_ref_hands_out_the_bytes() {
        let b = BinaryBlock::new(vec![4, 5, 6]);
        assert_eq!(b.as_ref(), &[4u8, 5, 6]);
    }

    #[test]
    fn block_len_and_data_match_the_bytes() {
        let b = BinaryBlock::new(vec![1, 2, 3]);
        assert_eq!(b.len(), 3);
        assert_eq!(b.data(), &[1, 2, 3]);
        assert!(!b.is_empty());
        assert!(BinaryBlock::default().is_empty());
    }

    #[test]
    fn compare_is_bytewise_then_by_length() {
        assert_eq!(
            BinaryBlock::new(b"ab".to_vec()).compare(&BinaryBlock::new(b"ab".to_vec())),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            BinaryBlock::new(b"ab".to_vec()).compare(&BinaryBlock::new(b"ac".to_vec())),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            BinaryBlock::new(b"ab".to_vec()).compare(&BinaryBlock::new(b"abc".to_vec())),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn bin_section_saves_bytes_unchanged() {
        let s = BinSection::new("icons.bin", BinaryBlock::new(vec![9, 8, 7]));
        assert_eq!(s.section_name(), "icons.bin");
        assert_eq!(s.as_binary().data(), &[9, 8, 7]);
        assert_eq!(s.save_bytes(), vec![9, 8, 7]);
    }
}
