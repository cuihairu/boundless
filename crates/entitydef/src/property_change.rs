//! Property changes: the encoded form of one property update — the flags
//! byte, the change path and the payload.
//!
//! The reference is `lib/entitydef/property_change.cpp` (+ `.hpp`): a nested
//! write such as `entity.myDict.ammo = 7` builds a [`SinglePropertyChange`] at
//! the leaf (`changeOwnedProperty`), walks up to the entity
//! (`getTopLevelOwner`, `IntermediatePropertyOwner`) recording the hops, and
//! the entity then streams it internally with `addToInternalStream`.
//!
//! | field | bytes (internal stream) |
//! |---|---|
//! | flags | one `uint8`: [`FLAG_IS_SLICE`] bit 0, [`FLAG_IS_NESTED`] bit 1 |
//! | path | `uint8` hop count, then one `int32` index per hop, leaf to root |
//! | extra bits (single) | `int32` leaf index |
//! | extra bits (slice) | `uint32` start index, `uint32` end index |
//! | payload | the leaf (or per-element) type's own `addToStream` form |
//!
//! The entity's own property index is not on this stream: in the reference it
//! travels in the message envelope (`rootIndex()` is read by the caller, and
//! `TopLevelPropertyOwner::setPropertyFromInternalStream` takes it as a
//! parameter).

use foundation::stream::BinaryStream;

use crate::data_type::DataType;
use crate::value::{PropertyError, PropertyValue, add_to_stream};

/// `PropertyChange::FLAG_IS_SLICE`.
pub const FLAG_IS_SLICE: u8 = 1 << 0;

/// `PropertyChange::FLAG_IS_NESTED`.
pub const FLAG_IS_NESTED: u8 = 1 << 1;

/// `PropertyChange`: the state shared by one property update. The path is the
/// child indexes ordered from the leaf to the root, stopping below the
/// entity's own property (whose index rides the message envelope).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PropertyChange {
    /// `ChangePath`: `(index, index_length)` per hop, leaf to root. The
    /// length is only read by the external (bit-packed) stream form.
    path: Vec<(i32, i32)>,
    is_nested_change: bool,
    root_index: i32,
}

impl PropertyChange {
    /// The constructor: an empty path, a flat change and no root index.
    pub fn new() -> Self {
        Self {
            path: Vec::new(),
            is_nested_change: false,
            root_index: -1,
        }
    }

    /// `addToPath`: one more hop, closest to the leaf first.
    pub fn add_to_path(&mut self, index: i32, index_length: i32) {
        self.path.push((index, index_length));
    }

    /// `isNestedChange( bool )` — the reference ignores its argument and
    /// always marks the change nested; so does this.
    pub fn set_is_nested_change(&mut self) {
        self.is_nested_change = true;
    }

    /// `isNestedChange()`.
    pub fn is_nested_change(&self) -> bool {
        self.is_nested_change
    }

    /// `rootIndex( int )`.
    pub fn set_root_index(&mut self, root_index: i32) {
        self.root_index = root_index;
    }

    /// `rootIndex()`: the envelope index of a nested change, or the
    /// subclass's own `getRootIndex()` for a flat one.
    pub fn root_index_or(&self, flat_index: i32) -> i32 {
        if self.is_nested_change {
            self.root_index
        } else {
            flat_index
        }
    }

    /// `addInternalFlags`.
    fn add_internal_flags(&self, stream: &mut BinaryStream, is_slice: bool) {
        let mut flags = 0u8;
        if is_slice {
            flags |= FLAG_IS_SLICE;
        }
        if self.is_nested_change {
            flags |= FLAG_IS_NESTED;
        }
        stream.write_u8(flags);
    }

    /// `writePathSimple`: the hop count then one `int32` per hop, leaf to
    /// root. The reference truncates a path longer than 255 hops to a byte
    /// without a word.
    fn write_path_simple(&self, stream: &mut BinaryStream) {
        stream.write_u8(self.path.len() as u8);
        for &(index, _) in &self.path {
            stream.write_i32(index);
        }
    }

    /// The path part of `addToInternalStream`: the simple path, then the
    /// subclass extra bits.
    fn add_simple_path_to_stream(
        &self,
        stream: &mut BinaryStream,
        extra_bits: impl FnOnce(&mut BinaryStream),
    ) {
        self.write_path_simple(stream);
        extra_bits(stream);
    }
}

/// `SinglePropertyChange`: one value of an entity changing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SinglePropertyChange {
    /// The `PropertyChange` base.
    pub change: PropertyChange,
    leaf_index: i32,
    leaf_size: i32,
}

impl SinglePropertyChange {
    /// The constructor, as `changeOwnedProperty` builds it: the leaf index
    /// and the leaf owner's property count.
    pub fn new(leaf_index: i32, leaf_size: i32) -> Self {
        Self {
            change: PropertyChange::new(),
            leaf_index,
            leaf_size,
        }
    }

    /// `rootIndex()` (this subclass's `getRootIndex()` is `leafIndex_`).
    pub fn root_index(&self) -> i32 {
        self.change.root_index_or(self.leaf_index)
    }

    /// `addToInternalStream`: the flags, then (nested only) the simple path
    /// and the extra bits, then the payload.
    pub fn add_to_internal_stream(
        &self,
        leaf_type: &DataType,
        value: &PropertyValue,
        stream: &mut BinaryStream,
    ) -> Result<(), PropertyError> {
        self.change.add_internal_flags(stream, false);
        if self.change.is_nested_change() {
            self.change
                .add_simple_path_to_stream(stream, |stream| self.add_extra_bits(stream));
        }
        self.add_value_to_stream(leaf_type, value, stream)
    }

    /// `addExtraBits( BinaryOStream & )`.
    fn add_extra_bits(&self, stream: &mut BinaryStream) {
        stream.write_i32(self.leaf_index);
    }

    /// `addValueToStream`: the leaf type's own payload.
    fn add_value_to_stream(
        &self,
        leaf_type: &DataType,
        value: &PropertyValue,
        stream: &mut BinaryStream,
    ) -> Result<(), PropertyError> {
        add_to_stream(leaf_type.kind(), value, stream)
    }

    /// The external-stream `addExtraBits( BitWriter & )` reads
    /// `BitReader::bitsRequired( leafSize_ )` — the packed form it sizes is
    /// recorded here so the leaf size is not dead weight.
    pub fn leaf_size(&self) -> i32 {
        self.leaf_size
    }
}

/// `SlicePropertyChange`: a slice of an array replaced by new values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlicePropertyChange {
    /// The `PropertyChange` base.
    pub change: PropertyChange,
    start_index: u32,
    end_index: u32,
    original_leaf_size: u32,
}

impl SlicePropertyChange {
    /// The constructor, as `PyArrayDataInstance::setSlice` builds it. The
    /// index range is half-open: `end` is one greater than the last index.
    pub fn new(start_index: u32, end_index: u32, original_leaf_size: u32) -> Self {
        Self {
            change: PropertyChange::new(),
            start_index,
            end_index,
            original_leaf_size,
        }
    }

    /// `rootIndex()` (this subclass's `getRootIndex()` is 0).
    pub fn root_index(&self) -> i32 {
        self.change.root_index_or(0)
    }

    /// `addToInternalStream`: the flags, then (nested only) the simple path
    /// and the extra bits, then one payload per new value.
    pub fn add_to_internal_stream(
        &self,
        element_type: &DataType,
        new_values: &[PropertyValue],
        stream: &mut BinaryStream,
    ) -> Result<(), PropertyError> {
        self.change.add_internal_flags(stream, true);
        if self.change.is_nested_change() {
            self.change
                .add_simple_path_to_stream(stream, |stream| self.add_extra_bits(stream));
        }
        self.add_value_to_stream(element_type, new_values, stream)
    }

    /// `addExtraBits( BinaryOStream & )`.
    fn add_extra_bits(&self, stream: &mut BinaryStream) {
        stream.write_u32(self.start_index);
        stream.write_u32(self.end_index);
    }

    /// `addValueToStream`: one element payload per new value, in order.
    fn add_value_to_stream(
        &self,
        element_type: &DataType,
        new_values: &[PropertyValue],
        stream: &mut BinaryStream,
    ) -> Result<(), PropertyError> {
        for value in new_values {
            add_to_stream(element_type.kind(), value, stream)?;
        }
        Ok(())
    }

    /// The external-stream `addExtraBits( BitWriter & )` sizes its two
    /// indexes with `bitsRequired( originalLeafSize_ + 1 )` — recorded here
    /// so the original size is not dead weight.
    pub fn original_leaf_size(&self) -> u32 {
        self.original_leaf_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_xml(xml: &str) -> resources::datasection::DataSection {
        resources::datasection::parse("test", xml.as_bytes()).expect("valid fixture xml")
    }

    fn build_type_(xml: &str) -> DataType {
        let mut diag = crate::diagnostics::Diagnostics::new();
        let ctx = crate::data_type::TypeContext::default();
        let section = parse_xml(xml);
        crate::data_type::build_type(&ctx, Some(&section), &mut diag).unwrap()
    }

    fn dict_type() -> DataType {
        build_type_(
            "<a> FIXED_DICT <Properties> \
             <ammo> <Type> UINT8 </Type> </ammo> \
             <label> <Type> STRING </Type> </label> \
             </Properties> </a>",
        )
    }

    #[test]
    fn a_flat_change_writes_its_flags_then_payload_only() {
        // `TopLevelPropertyOwner::setPropertyFromInternalStream` reads the
        // root index from the envelope, so a non-nested single change is the
        // flags byte and the leaf payload, nothing else.
        let uint8 = build_type_("<a> UINT8 </a>");
        let change = SinglePropertyChange::new(0, 1);
        let mut stream = BinaryStream::new();
        change
            .add_to_internal_stream(&uint8, &PropertyValue::Uint(7), &mut stream)
            .unwrap();
        assert_eq!(stream.as_bytes(), &[0, 7]);
    }

    #[test]
    fn a_fixed_dict_subfield_change_is_nested_with_an_empty_path() {
        // `entity.myDict.ammo = 7`, `myDict` the entity's 6th property: the
        // FIXED_DICT instance builds the change with the *field* index and
        // its own field count, then `getTopLevelOwner` reaches the entity,
        // whose index goes to the root index and NOT to the path.
        let dict = dict_type();
        let field = dict.field_type(0).expect("field 0");
        let mut change = SinglePropertyChange::new(0, 2);
        change.change.set_is_nested_change();
        change.change.set_root_index(6);
        let mut stream = BinaryStream::new();
        change
            .add_to_internal_stream(&field, &PropertyValue::Uint(7), &mut stream)
            .unwrap();
        assert_eq!(stream.as_bytes(), &[2, 0, 0, 0, 0, 0, 7]);
    }

    #[test]
    fn a_deeper_change_records_the_hops_leaf_to_root() {
        // `entity.arr[4].dict.ammo = 7`: the ARRAY instance is an
        // intermediate owner, so its ref lands on the path while the entity's
        // property index stays in the envelope.
        let array_of_dict = build_type_(
            "<a> ARRAY <size> 10 </size> \
             <of> FIXED_DICT <Properties> \
             <ammo> <Type> UINT8 </Type> </ammo> \
             </Properties> </of> </a>",
        );
        let element = array_of_dict.element_type().expect("element");
        let field = element.field_type(0).expect("field 0");
        let mut change = SinglePropertyChange::new(0, 1);
        change.change.set_is_nested_change();
        change.change.set_root_index(6);
        // Leaf to root: the dict sits at position 4 of the array.
        change.change.add_to_path(4, 10);
        let mut stream = BinaryStream::new();
        change
            .add_to_internal_stream(&field, &PropertyValue::Uint(7), &mut stream)
            .unwrap();
        assert_eq!(stream.as_bytes(), &[2, 1, 4, 0, 0, 0, 0, 0, 0, 0, 7]);
    }

    #[test]
    fn a_nested_slice_writes_start_end_then_one_payload_per_value() {
        // `entity.arr[2..5] = [9, 8]` on a UINT8 array: `SlicePropertyChange`
        // adds its two `uint32` indexes as the extra bits, then one element
        // payload per new value.
        let uint8_array = build_type_("<a> ARRAY <size> 10 </size> <of> UINT8 </of> </a>");
        let element = uint8_array.element_type().expect("element");
        let mut change = SlicePropertyChange::new(2, 5, 10);
        change.change.set_is_nested_change();
        change.change.set_root_index(3);
        change.change.add_to_path(1, 10);
        let mut stream = BinaryStream::new();
        change
            .add_to_internal_stream(
                &element,
                &[PropertyValue::Uint(9), PropertyValue::Uint(8)],
                &mut stream,
            )
            .unwrap();
        assert_eq!(
            stream.as_bytes(),
            &[3, 1, 1, 0, 0, 0, 2, 0, 0, 0, 5, 0, 0, 0, 9, 8]
        );
    }

    #[test]
    fn a_flat_slice_of_dicts_streams_each_full_dict() {
        // A slice of a FIXED_DICT array streams each new dict in full, in the
        // dict type's own field order.
        let array_of_dict = build_type_(
            "<a> ARRAY <size> 4 </size> \
             <of> FIXED_DICT <Properties> \
             <ammo> <Type> UINT8 </Type> </ammo> \
             <label> <Type> STRING </Type> </label> \
             </Properties> </of> </a>",
        );
        let element = array_of_dict.element_type().expect("element");
        let change = SlicePropertyChange::new(0, 1, 4);
        let mut stream = BinaryStream::new();
        change
            .add_to_internal_stream(
                &element,
                &[PropertyValue::FixedDict(Some(vec![
                    ("ammo".to_string(), PropertyValue::Uint(7)),
                    ("label".to_string(), PropertyValue::String("x".into())),
                ]))],
                &mut stream,
            )
            .unwrap();
        // Flat: no start/end indexes, just the flags and the payload.
        assert_eq!(stream.as_bytes(), &[1, 7, 1, b'x']);
    }

    #[test]
    fn a_flat_slice_change_carries_the_slice_flags_only() {
        // `addToInternalStream` only writes the extra bits on the nested
        // path, so a flat slice is the flags byte and the payloads alone --
        // which is why the flat branch of
        // `setPropertyFromInternalStream` can `MF_ASSERT( !isSlice )`.
        let uint8_array = build_type_("<a> ARRAY <size> 10 </size> <of> UINT8 </of> </a>");
        let element = uint8_array.element_type().expect("element");
        let change = SlicePropertyChange::new(1, 2, 10);
        let mut stream = BinaryStream::new();
        change
            .add_to_internal_stream(&element, &[PropertyValue::Uint(5)], &mut stream)
            .unwrap();
        assert_eq!(stream.as_bytes(), &[1, 5]);
    }

    #[test]
    fn the_payload_of_a_single_change_is_the_leaf_types_own_form() {
        // `SinglePropertyChange::addValueToStream` hands the *field* type to
        // `addToStream`, so a STRING subfield is the packed length form even
        // though the owning dict type would never write it bare.
        let dict = dict_type();
        let field = dict.field_type(1).expect("field 1");
        let mut change = SinglePropertyChange::new(1, 2);
        change.change.set_is_nested_change();
        change.change.set_root_index(6);
        let mut stream = BinaryStream::new();
        change
            .add_to_internal_stream(&field, &PropertyValue::String("xy".into()), &mut stream)
            .unwrap();
        assert_eq!(stream.as_bytes(), &[2, 0, 1, 0, 0, 0, 2, b'x', b'y']);
    }

    #[test]
    fn a_change_remembers_what_the_external_stream_will_size_with() {
        // The external `addExtraBits( BitWriter & )` sizes the single leaf
        // index with `bitsRequired( leafSize_ )` and the slice indexes with
        // `bitsRequired( originalLeafSize_ + 1 )`, so both sizes ride the
        // constructors even though the internal stream never reads them.
        let single = SinglePropertyChange::new(5, 8);
        assert_eq!(single.leaf_size(), 8);
        let slice = SlicePropertyChange::new(2, 5, 10);
        assert_eq!(slice.original_leaf_size(), 10);
    }

    #[test]
    fn the_root_index_follows_the_reference_virtual_dispatch() {
        // A flat single change reports its leaf index, a flat slice change
        // reports 0, and a nested change reports the envelope index whatever
        // the subclass.
        let mut single = SinglePropertyChange::new(5, 8);
        assert_eq!(single.root_index(), 5);
        single.change.set_root_index(2);
        assert_eq!(single.root_index(), 5);
        single.change.set_is_nested_change();
        assert_eq!(single.root_index(), 2);

        let mut slice = SlicePropertyChange::new(1, 2, 8);
        assert_eq!(slice.root_index(), 0);
        slice.change.set_root_index(4);
        slice.change.set_is_nested_change();
        assert_eq!(slice.root_index(), 4);
    }
}
