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

/// `BIN_SECTION_MAGIC`: the container magic (`bin_section.cpp`).
pub const BIN_SECTION_MAGIC: u32 = 0x42a1_4e65;

/// A named view onto a binary block (`BinSection`): the tag plus the owned
/// bytes. The block may hold a BinSection *container* — child blobs followed
/// by an index table — which `introspect` parses lazily, exactly like the
/// reference's introspect-on-first-touch. New-format containers carry the
/// magic and 16 reserved bytes (preloadLen/version/modified) per index
/// entry; old-format containers (no magic, e.g. legacy `.primitives`) carry
/// neither. `save`/`asBinary` on a section whose children changed re-collects
/// the container in the new format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinSection {
    tag: String,
    block: BinaryBlock,
    /// Set once `introspect` ran (the reference's `introspected_`).
    introspected: bool,
    /// Child sections in container order; empty until introspected or
    /// populated by `new_section`.
    children: Vec<BinSection>,
    /// Set when children changed after introspection (the reference knows
    /// this via `introspected_` flips in `setBinary`/`newSection`).
    dirty: bool,
}

impl BinSection {
    pub fn new(tag: &str, block: BinaryBlock) -> Self {
        Self {
            tag: tag.to_string(),
            block,
            introspected: false,
            children: Vec::new(),
            dirty: false,
        }
    }

    /// `BinSection::sectionName`.
    pub fn section_name(&self) -> &str {
        &self.tag
    }

    /// `BinSection::asBinary` (raw face): the owned bytes, without
    /// re-collecting mutated children (use [`BinSection::collect_binary`]
    /// for the reference's introspect-then-recollect flow).
    pub fn as_binary(&self) -> &BinaryBlock {
        &self.block
    }

    /// `BinSection::setBinary`: replace the block and drop the parsed
    /// children.
    pub fn set_binary(&mut self, block: BinaryBlock) {
        self.block = block;
        self.introspected = false;
        self.children.clear();
        self.dirty = false;
    }

    /// `BinSection::introspect`: parse the container index out of the block.
    /// An already-introspected section is untouched; a dirty one is left
    /// alone too (the reference re-parses only after `setBinary`).
    pub fn introspect(&mut self) {
        if self.introspected {
            return;
        }
        self.introspected = true;
        self.children.clear();

        let len = self.block.len();
        if len < 3 * std::mem::size_of::<u32>() {
            return;
        }
        let data = self.block.data();

        let mut entry_data_offset = 0usize;
        // entryNameLenPad: 3 for the new format (names 4-byte padded), 0
        // for the old one.
        let entry_name_len_pad;
        if read_u32(data, 0) == BIN_SECTION_MAGIC {
            entry_data_offset = 4;
            entry_name_len_pad = 3;
        } else {
            entry_name_len_pad = 0;
        }

        let index_len = read_i32(data, len - 4);
        let mut offset = len as i64 - 4 - index_len as i64;

        // The index must sit inside the data area.
        if offset < entry_data_offset as i64 || offset >= (len - 4) as i64 {
            return;
        }

        let mut children = Vec::new();
        while offset <= (len - 3 * std::mem::size_of::<u32>()) as i64 {
            // The entry's data length; bit 31 flags the extended form.
            let mut entry_data_len = read_i32(data, offset as usize);
            offset += 4;
            if ((entry_data_len & (1 << 31)) != 0 || entry_name_len_pad != 0)
                && offset + 4 * std::mem::size_of::<u32>() as i64
                    <= (len - 2 * std::mem::size_of::<u32>()) as i64
            {
                // Skip the extended data (u32 preloadLen, u32 version,
                // u64 modified) and mask the flag off.
                entry_data_len &= !(1 << 31);
                offset += 4 * std::mem::size_of::<u32>() as i64;
            }
            let entry_name_len = read_i32(data, offset as usize);
            offset += 4;

            // The lengths must make sense (the reference's guards; failure
            // drops every child collected so far).
            if entry_data_len as u32 > 256 * 1024 * 1028
                || (entry_data_offset as i64 + entry_data_len as i64)
                    > (len as i64 - 4 - index_len as i64)
                || entry_name_len as u32 > 4096
                || offset + entry_name_len as i64 > (len - 4) as i64
            {
                self.children.clear();
                return;
            }

            let name = String::from_utf8_lossy(
                &data[offset as usize..(offset + entry_name_len as i64) as usize],
            )
            .into_owned();
            offset += ((entry_name_len + entry_name_len_pad) & !entry_name_len_pad) as i64;

            let blob = BinaryBlock::new(
                data[entry_data_offset..(entry_data_offset + entry_data_len as usize)].to_vec(),
            );
            // The child stays lazily parsed (nested containers introspect
            // on first access, like the reference's child BinSections).
            children.push(BinSection::new(&name, blob));

            entry_data_offset = (entry_data_offset + entry_data_len as usize + 3) & !3;
        }

        self.children = children;
    }

    /// `BinSection::countChildren`.
    pub fn count_children(&mut self) -> usize {
        self.introspect();
        self.children.len()
    }

    /// The parsed children in container order (the reference's
    /// `begin()`/`end()` section walk).
    pub fn children(&mut self) -> &[BinSection] {
        self.introspect();
        &self.children
    }

    /// `BinSection::findChild`: the child with `tag`, if present.
    pub fn find_child(&mut self, tag: &str) -> Option<&BinSection> {
        self.introspect();
        self.children.iter().find(|child| child.tag == tag)
    }

    /// `PrimitiveFile::readBinary` face: the child's raw block by tag.
    pub fn read_binary(&mut self, tag: &str) -> Option<BinaryBlock> {
        self.find_child(tag).map(|child| child.block.clone())
    }

    /// `BinSection::newSection`: append an empty child with `tag`.
    pub fn new_section(&mut self, tag: &str) -> &mut BinSection {
        self.introspect();
        self.dirty = true;
        self.children
            .push(BinSection::new(tag, BinaryBlock::default()));
        self.children.last_mut().unwrap()
    }

    /// `BinSection::recollect` + `asBinary`: the owned bytes, or the
    /// children re-collected into the new-format container when they were
    /// modified. Empty children collapse to a bare block.
    pub fn collect_binary(&mut self) -> BinaryBlock {
        self.introspect();
        if !self.dirty || self.children.is_empty() {
            return self.block.clone();
        }

        // Collect the child blobs (each asBinary; recursive containers are
        // written as their collected bytes).
        let child_blocks: Vec<Vec<u8>> = self
            .children
            .iter_mut()
            .map(|child| child.collect_binary().data().to_vec())
            .collect();

        let mut bsz = 4usize; // the magic
        for block in &child_blocks {
            bsz += (block.len() + 3) & !3;
        }

        // Build the index: {len, 4 reserved words, nameLen} + padded name.
        let mut index: Vec<u8> = Vec::new();
        for (child, block) in self.children.iter().zip(&child_blocks) {
            index.extend_from_slice(&(block.len() as u32).to_le_bytes());
            // u32 preloadLen, u32 version, u64 modified — all reserved zero.
            index.extend_from_slice(&[0u8; 16]);
            index.extend_from_slice(&(child.tag.len() as u32).to_le_bytes());
            index.extend_from_slice(child.tag.as_bytes());
            let pad = (4 - (child.tag.len() & 3)) & 3;
            index.resize(index.len() + pad, 0);
        }
        let index_len = index.len() as u32;
        index.extend_from_slice(&index_len.to_le_bytes());
        bsz += index.len();

        let mut out = Vec::with_capacity(bsz);
        out.extend_from_slice(&BIN_SECTION_MAGIC.to_le_bytes());
        for block in &child_blocks {
            out.extend_from_slice(block);
            let pad = (4 - (block.len() & 3)) & 3;
            out.resize(out.len() + pad, 0);
        }
        out.extend_from_slice(&index);

        self.block = BinaryBlock::new(out);
        self.dirty = false;
        self.block.clone()
    }

    /// `BinSection::save` against an in-memory sink: the whole block is
    /// written unchanged (mutated children are re-collected first, like the
    /// reference's saveChild → asBinary → recollect flow).
    pub fn save_bytes(&mut self) -> Vec<u8> {
        self.collect_binary().data().to_vec()
    }
}

/// Little-endian `u32` at `offset` (the reference reads host-order ints,
/// which are little-endian on the platforms this targets).
fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

/// Little-endian `i32` at `offset`.
fn read_i32(data: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
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
        let mut s = BinSection::new("icons.bin", BinaryBlock::new(vec![9, 8, 7]));
        assert_eq!(s.section_name(), "icons.bin");
        assert_eq!(s.as_binary().data(), &[9, 8, 7]);
        assert_eq!(s.save_bytes(), vec![9, 8, 7]);
    }

    /// A reference-style container (magic, padded blobs, reserved-word
    /// index entries, padded names) introspects into named children, and a
    /// re-collect round-trips byte-for-byte.
    #[test]
    fn container_introspects_and_round_trips() {
        // Build a new-format container by hand: two children ("alpha"
        // 3 bytes, "beta.list" 5 bytes), each padded to 4, then the index.
        let mut blob: Vec<u8> = BIN_SECTION_MAGIC.to_le_bytes().to_vec();
        let alpha = vec![1u8, 2, 3];
        let beta = vec![9u8; 5];
        blob.extend_from_slice(&alpha);
        blob.extend_from_slice(&[0]); // 3 -> pad to 4
        blob.extend_from_slice(&beta);
        blob.extend_from_slice(&[0, 0, 0]); // 5 -> pad to 8

        let mut index: Vec<u8> = Vec::new();
        for (name, block) in [("alpha", &alpha), ("beta.list", &beta)] {
            index.extend_from_slice(&(block.len() as u32).to_le_bytes());
            index.extend_from_slice(&[0u8; 16]);
            index.extend_from_slice(&(name.len() as u32).to_le_bytes());
            index.extend_from_slice(name.as_bytes());
            let pad = (4 - (name.len() & 3)) & 3;
            index.resize(index.len() + pad, 0);
        }
        let index_len = index.len() as u32;
        index.extend_from_slice(&index_len.to_le_bytes());
        blob.extend_from_slice(&index);

        let mut s = BinSection::new("container", BinaryBlock::new(blob.clone()));
        assert_eq!(s.count_children(), 2);
        assert_eq!(s.read_binary("alpha").unwrap().data(), &[1u8, 2, 3]);
        assert_eq!(s.read_binary("beta.list").unwrap().data(), &[9u8; 5]);
        assert!(s.read_binary("missing").is_none());

        // Unmodified: collect hands the block straight back.
        assert_eq!(s.collect_binary().data(), blob.as_slice());

        // Mutate a child and re-collect: the output parses back to the
        // same children.
        s.new_section("gamma")
            .set_binary(BinaryBlock::new(vec![7u8; 2]));
        let rebuilt = s.save_bytes();
        let mut again = BinSection::new("container", BinaryBlock::new(rebuilt));
        assert_eq!(again.count_children(), 3);
        assert_eq!(again.read_binary("alpha").unwrap().data(), &[1u8, 2, 3]);
        assert_eq!(again.read_binary("gamma").unwrap().data(), &[7u8; 2]);
    }

    /// `children()` hands the parsed children out in container order.
    #[test]
    fn children_are_listed_in_container_order() {
        let mut s = BinSection::new("container", BinaryBlock::default());
        s.new_section("zebra").set_binary(BinaryBlock::new(vec![1]));
        s.new_section("alpha").set_binary(BinaryBlock::new(vec![2]));
        let names: Vec<&str> = s.children().iter().map(|c| c.section_name()).collect();
        assert_eq!(names, ["zebra", "alpha"]);
        assert_eq!(s.children()[1].as_binary().data(), &[2]);
    }

    /// An old-format container (no magic, no reserved words, unpadded
    /// names — the legacy `.primitives` layout) also introspects.
    #[test]
    fn old_format_container_introspects() {
        // Directory entries {len, nameLen, name}, no name padding, no
        // magic; the data blocks themselves are 4-byte padded like the
        // reference's entryDataOffset walk.
        let mut blob: Vec<u8> = Vec::new();
        let vertices = vec![0x42u8, 0x4e]; // pretend VertexHeader bytes
        let list = vec![1u8, 2, 3, 4];
        blob.extend_from_slice(&vertices);
        blob.extend_from_slice(&[0, 0]); // 2 -> padded to 4
        blob.extend_from_slice(&list);

        let mut index: Vec<u8> = Vec::new();
        for (name, block) in [("vertices", &vertices), ("list", &list)] {
            index.extend_from_slice(&(block.len() as u32).to_le_bytes());
            index.extend_from_slice(&(name.len() as u32).to_le_bytes());
            index.extend_from_slice(name.as_bytes());
        }
        let index_len = index.len() as u32;
        index.extend_from_slice(&index_len.to_le_bytes());
        blob.extend_from_slice(&index);

        let mut s = BinSection::new("prim", BinaryBlock::new(blob));
        assert_eq!(s.count_children(), 2);
        assert_eq!(s.read_binary("vertices").unwrap().data(), &[0x42u8, 0x4e]);
        assert_eq!(s.read_binary("list").unwrap().data(), &[1u8, 2, 3, 4]);
    }

    /// A truncated or malformed index drops the children (the reference's
    /// guard clears the list and returns).
    #[test]
    fn a_malformed_index_has_no_children() {
        // Index length points outside the data area.
        let mut s = BinSection::new("bad", BinaryBlock::new(vec![0u8; 20]));
        s.introspect();
        assert_eq!(s.count_children(), 0);

        // Not enough bytes for even one directory entry.
        let mut tiny = BinSection::new("tiny", BinaryBlock::new(vec![1u8; 8]));
        tiny.introspect();
        assert_eq!(tiny.count_children(), 0);
    }
}
