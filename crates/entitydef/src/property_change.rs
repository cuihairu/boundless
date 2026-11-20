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

use crate::data_type::{DataType, Kind};
use crate::value::{
    PropertyError, PropertyValue, SIMPLE_STREAM_ELEMENT, add_to_stream, create_from_stream,
};

/// `PropertyChange::FLAG_IS_SLICE`.
pub const FLAG_IS_SLICE: u8 = 1 << 0;

/// `PropertyChange::FLAG_IS_NESTED`.
pub const FLAG_IS_NESTED: u8 = 1 << 1;

/// `PropertyChange`: the state shared by one property update. The path holds
/// the indexes of the owners strictly between the top-level property and the
/// leaf, in the order the reader will descend them (the hop nearest the
/// top-level property first); the entity's own property index rides the
/// message envelope instead. (The reference header comment claims the path is
/// "ordered from the leaf to the root", but `IntermediatePropertyOwner::
/// getTopLevelOwner` appends each hop after its recursion returns, so the
/// hop closest to the entity lands first — the mechanism and the reader both
/// use descent order, and the comment is wrong.)
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PropertyChange {
    /// `ChangePath`: `(index, index_length)` per hop, in reader descent
    /// order. The length is only read by the external (bit-packed) form.
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

    /// `addToPath`: one more hop, in reader descent order — the frame that
    /// recursion returns into first appends first, so the hop nearest the
    /// top-level property lands before the deeper ones.
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

// ---------------------------------------------------------------------------
// Reading an update slice back (`property_change_reader.cpp`)
// ---------------------------------------------------------------------------

/// `PropertyChangeReader`'s and both owners' reporting names.
pub(crate) const READER_SITE: &str = "PropertyChangeReader::readAndApply";
const ARRAY_GET_CHILD: &str = "PyArrayDataInstance::getChildPropertyOwner";
const FIXED_DICT_GET_CHILD: &str = "PyFixedDictDataInstance::getChildPropertyOwner";

/// One element of the Python change path (`getPyIndex` per hop).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathElement {
    /// `PyFixedDictDataInstance::getPyIndex`: the field name.
    Field(String),
    /// `PyArrayDataInstance::getPyIndex`: the integer index.
    Index(i32),
    /// `SlicePropertyChangeReader::apply`: the half-open replaced range,
    /// the end re-based on the size after the splice.
    Slice(i32, i32),
}

/// What applying an update slice reports to the setter callback: the old
/// value(s) (`*ppOldValue`) and the change path (`*ppChangePath`).
#[derive(Clone, Debug, PartialEq)]
pub struct AppliedChange {
    /// The value(s) the change replaced. A slice carries the replaced
    /// elements as a [`PropertyValue::Sequence`].
    pub old_value: PropertyValue,
    /// One element per hop, then the leaf's own index.
    pub path: Vec<PathElement>,
}

/// Reading an update slice back, with the reference's messages.
#[derive(Clone, Debug, PartialEq)]
pub enum ApplyError {
    /// `PropertyChangeReader::readAndApply: pOwner is NULL. %d/%d.\n` — a
    /// hop landed on a value that cannot own children.
    OwnerIsNull { hop: usize, size: usize },
    /// `PyArrayDataInstance::getChildPropertyOwner: Bad index %d. size =
    /// %zd\n` (and the FIXED_DICT twin) — a hop out of range.
    BadChildIndex {
        owner: &'static str,
        index: i32,
        size: usize,
    },
    /// The leaf index ran off the end of its owner. The reference indexes
    /// `fieldValues_[ref]` unchecked here, which is undefined; this port
    /// refuses instead.
    LeafIndexOutOfRange { index: i32, size: i32 },
    /// `PyArrayDataInstance::setOwnedSlice: Invalid values. startIndex =
    /// %d. endIndex = %d. values = %zd data.remainingLength = %d\n` — the
    /// reference prints this and carries on with the clamped indexes, as
    /// does this port; the variant only pins the message in tests.
    InvalidSlice {
        start: i32,
        end: i32,
        size: usize,
        remaining: usize,
    },
    /// `PyArrayDataInstance::setOwnedSlice: Unable to create slice\n`.
    UnableToCreateSlice,
    /// `PyArrayDataInstance::setSlice: slice assignment would create array
    /// of wrong size` (the reference literal has no trailing newline).
    WrongArraySize,
    /// `PropertyChangeReader::readAndApply: Old value is NULL\n` — a slice
    /// on an owner without `setOwnedSlice` (any owner but an array), which
    /// leaves the default base implementation returning NULL.
    OldValueIsNull,
    /// A flat slice on the internal stream: the reference asserts
    /// `MF_ASSERT( !isSlice )` before its flat branch.
    FlatSliceOnInternalStream,
    /// The stream or the payload failed underneath.
    Payload(PropertyError),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OwnerIsNull { hop, size } => {
                writeln!(f, "{READER_SITE}: pOwner is NULL. {hop}/{size}.")
            }
            Self::BadChildIndex { owner, index, size } => {
                writeln!(f, "{owner}: Bad index {index}. size = {size}")
            }
            Self::LeafIndexOutOfRange { index, size } => {
                write!(
                    f,
                    "Leaf index {index} out of range (owner has {size} children)"
                )
            }
            Self::InvalidSlice {
                start,
                end,
                size,
                remaining,
            } => writeln!(
                f,
                "PyArrayDataInstance::setOwnedSlice: Invalid values. \
                 startIndex = {start}. endIndex = {end}. values = {size} \
                 data.remainingLength = {remaining}"
            ),
            Self::UnableToCreateSlice => {
                writeln!(
                    f,
                    "PyArrayDataInstance::setOwnedSlice: Unable to create slice"
                )
            }
            Self::WrongArraySize => write!(
                f,
                "PyArrayDataInstance::setSlice: slice assignment would \
                 create array of wrong size"
            ),
            Self::OldValueIsNull => writeln!(f, "{READER_SITE}: Old value is NULL"),
            Self::FlatSliceOnInternalStream => write!(
                f,
                "TopLevelPropertyOwner::setPropertyFromInternalStream: a \
                 flat slice never arrives on the internal stream (MF_ASSERT( \
                 !isSlice ))"
            ),
            Self::Payload(err) => write!(f, "{err}"),
        }
    }
}

/// `readExtraBits`: the leaf index of a single change or the range of a
/// slice, kept apart so they cannot mix.
enum ExtraBits {
    Single(i32),
    Slice(i32, i32),
}

/// The read side's truncation, as the write side reports one.
fn stream_truncated() -> ApplyError {
    ApplyError::Payload(PropertyError::StreamTruncated {
        element: SIMPLE_STREAM_ELEMENT,
    })
}

/// `TopLevelPropertyOwner::setPropertyFromInternalStream` plus the reader
/// walk (`PropertyChangeReader::readSimplePathAndApply`): `root_type` and
/// `root_value` are the top-level property the envelope's index resolved to
/// (`getChildPropertyOwner( rootIndex )` on the reference side).
pub fn apply_internal_stream(
    root_type: &DataType,
    root_value: &mut PropertyValue,
    stream: &mut BinaryStream,
) -> Result<AppliedChange, ApplyError> {
    let flags = stream.read_u8().map_err(|_| stream_truncated())?;
    let is_slice = flags & FLAG_IS_SLICE != 0;
    let is_nested = flags & FLAG_IS_NESTED != 0;

    if !is_nested {
        if is_slice {
            return Err(ApplyError::FlatSliceOnInternalStream);
        }
        // The flat branch: `setOwnedProperty( rootIndex, stream )` on the
        // entity, which replaces the whole property and fills no path.
        let new_value =
            create_from_stream(root_type.kind(), stream).map_err(ApplyError::Payload)?;
        let old_value = std::mem::replace(root_value, new_value);
        return Ok(AppliedChange {
            old_value,
            path: Vec::new(),
        });
    }

    // `readSimplePathAndApply`: the hop count, then one `int32` per hop.
    let size = stream.read_u8().map_err(|_| stream_truncated())? as usize;
    let mut hops = Vec::with_capacity(size);
    for _ in 0..size {
        hops.push(stream.read_i32().map_err(|_| stream_truncated())?);
    }

    // `readExtraBits`, before the payload.
    let extra = if is_slice {
        let start = stream.read_u32().map_err(|_| stream_truncated())?;
        let end = stream.read_u32().map_err(|_| stream_truncated())?;
        ExtraBits::Slice(start as i32, end as i32)
    } else {
        ExtraBits::Single(stream.read_i32().map_err(|_| stream_truncated())?)
    };

    let mut path = Vec::new();
    let old_value = apply_at(
        root_type.kind(),
        root_value,
        &hops,
        extra,
        stream,
        &mut path,
    )?;
    Ok(AppliedChange { old_value, path })
}

/// The walk: descend the hops (`getChildPropertyOwner` + `getPyIndex` per
/// level), then apply the leaf.
fn apply_at(
    kind: &Kind,
    value: &mut PropertyValue,
    hops: &[i32],
    extra: ExtraBits,
    stream: &mut BinaryStream,
    path: &mut Vec<PathElement>,
) -> Result<PropertyValue, ApplyError> {
    let Some((&hop, rest)) = hops.split_first() else {
        // `SinglePropertyChangeReader::apply` or
        // `SlicePropertyChangeReader::apply` at the leaf owner.
        return match extra {
            ExtraBits::Single(leaf_index) => {
                set_owned_property(kind, value, leaf_index, stream, path)
            }
            ExtraBits::Slice(start, end) => set_owned_slice(kind, value, start, end, stream, path),
        };
    };
    let position = hops.len() - rest.len();
    let hop = hop as usize;
    match (kind, value) {
        (Kind::FixedDict { fields, .. }, PropertyValue::FixedDict(Some(children))) => {
            // `uint(ref) >= fieldValues_.size()`: a negative index casts to
            // a huge `usize` and fails the same bounds check.
            if hop >= children.len() {
                return Err(ApplyError::BadChildIndex {
                    owner: FIXED_DICT_GET_CHILD,
                    index: hop as i32,
                    size: children.len(),
                });
            }
            path.push(PathElement::Field(fields[hop].name.clone()));
            apply_at(
                fields[hop].type_.kind(),
                &mut children[hop].1,
                rest,
                extra,
                stream,
                path,
            )
        }
        (Kind::Sequence { element, .. }, PropertyValue::Sequence(children)) => {
            if hop >= children.len() {
                return Err(ApplyError::BadChildIndex {
                    owner: ARRAY_GET_CHILD,
                    index: hop as i32,
                    size: children.len(),
                });
            }
            path.push(PathElement::Index(hop as i32));
            apply_at(
                element.kind(),
                &mut children[hop],
                rest,
                extra,
                stream,
                path,
            )
        }
        _ => {
            // `getChildPropertyOwner` handed back NULL: the child cannot
            // own anything.
            Err(ApplyError::OwnerIsNull {
                hop: position,
                size: position + rest.len(),
            })
        }
    }
}

/// `PyFixedDictDataInstance::setOwnedProperty` / `PyArrayDataInstance::
/// setOwnedProperty` at the leaf owner: replace one child with the value on
/// the stream and report the previous one.
fn set_owned_property(
    kind: &Kind,
    value: &mut PropertyValue,
    leaf_index: i32,
    stream: &mut BinaryStream,
    path: &mut Vec<PathElement>,
) -> Result<PropertyValue, ApplyError> {
    match (kind, value) {
        (Kind::FixedDict { fields, .. }, PropertyValue::FixedDict(children)) => {
            // A `None` dict counts as zero fields, so any index is refused.
            let mut empty = Vec::new();
            let children = children.as_deref_mut().unwrap_or(&mut empty);
            let index = leaf_index as usize;
            if index >= children.len() {
                return Err(ApplyError::LeafIndexOutOfRange {
                    index: leaf_index,
                    size: children.len() as i32,
                });
            }
            let old = children[index].1.clone();
            children[index].1 = create_from_stream(fields[index].type_.kind(), stream)
                .map_err(ApplyError::Payload)?;
            path.push(PathElement::Field(fields[index].name.clone()));
            Ok(old)
        }
        (Kind::Sequence { element, .. }, PropertyValue::Sequence(children)) => {
            let index = leaf_index as usize;
            if index >= children.len() {
                return Err(ApplyError::LeafIndexOutOfRange {
                    index: leaf_index,
                    size: children.len() as i32,
                });
            }
            let old = children[index].clone();
            children[index] =
                create_from_stream(element.kind(), stream).map_err(ApplyError::Payload)?;
            path.push(PathElement::Index(leaf_index));
            Ok(old)
        }
        _ => {
            // The leaf owner turned out not to own anything; the walk
            // above would have reported `pOwner is NULL` first, so this
            // only guards malformed input.
            Err(ApplyError::OwnerIsNull { hop: 1, size: 1 })
        }
    }
}

/// `PyArrayDataInstance::setOwnedSlice` + `setSlice` at the leaf owner.
/// Every other owner keeps the base `setOwnedSlice`, which returns NULL and
/// so reports no old value.
fn set_owned_slice(
    kind: &Kind,
    value: &mut PropertyValue,
    start: i32,
    end: i32,
    stream: &mut BinaryStream,
    path: &mut Vec<PathElement>,
) -> Result<PropertyValue, ApplyError> {
    let (element_kind, size_limit, children) = match (kind, value) {
        (Kind::Sequence { element, size, .. }, PropertyValue::Sequence(children)) => {
            (element.as_ref(), *size, children)
        }
        _ => return Err(ApplyError::OldValueIsNull),
    };

    let orig_size = children.len();
    // `endIndex < startIndex` makes the reference print an error and carry
    // on; the clamps below give the same outcome, so there is nothing to
    // observe here.

    // Read the new values first ("Read until stream is empty"), then splice.
    let mut new_values = Vec::new();
    while stream.remaining() > 0 {
        match create_from_stream(element_kind.kind(), stream) {
            Ok(new_value) => new_values.push(new_value),
            Err(_) => return Err(ApplyError::UnableToCreateSlice),
        }
    }

    // put indices in range (slices don't generate index errors)
    let start = start.clamp(0, orig_size as i32) as usize;
    let end = end.clamp(start as i32, orig_size as i32) as usize;

    // make sure the sequence will still be the right size
    if size_limit != 0 && end - start != new_values.len() {
        return Err(ApplyError::WrongArraySize);
    }

    let old: Vec<PropertyValue> = children.drain(start..end).collect();
    children.splice(start..start, new_values);
    let new_size = children.len();
    // `SlicePropertyChangeReader::apply`: the end re-based on the splice.
    path.push(PathElement::Slice(
        start as i32,
        // Signed in the reference: a shrinking slice moves the end back.
        end as i32 + (new_size as i64 - orig_size as i64) as i32,
    ));
    Ok(PropertyValue::Sequence(old))
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
    fn a_deeper_change_records_the_hops_in_descent_order() {
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
        // Reader descent order: the dict sits at position 4 of the array.
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

    fn encode_subfield_change(
        dict: &DataType,
        field: usize,
        value: PropertyValue,
        root: i32,
    ) -> BinaryStream {
        let field_type = dict.field_type(field).expect("field").clone();
        let mut change = SinglePropertyChange::new(field as i32, 2);
        change.change.set_is_nested_change();
        change.change.set_root_index(root);
        let mut stream = BinaryStream::new();
        change
            .add_to_internal_stream(&field_type, &value, &mut stream)
            .unwrap();
        stream
    }

    fn dict_value(ammo: u64, label: &str) -> PropertyValue {
        PropertyValue::FixedDict(Some(vec![
            ("ammo".to_string(), PropertyValue::Uint(ammo)),
            ("label".to_string(), PropertyValue::String(label.into())),
        ]))
    }

    fn slots_dict_type() -> DataType {
        build_type_(
            "<a> FIXED_DICT <Properties> \
             <slots> <Type> ARRAY <of> UINT8 </of> </Type> </slots> \
             </Properties> </a>",
        )
    }

    #[test]
    fn an_applied_dict_subfield_change_touches_only_the_target_field() {
        // The BW-04-02-05 encoding of `entity.myDict.ammo = 7` received by
        // `apply_internal_stream`: only `ammo` moves, `label` keeps its
        // value, and the notification carries the old `ammo` and the leaf's
        // Python index (the field name).
        let dict = dict_type();
        let mut stream = encode_subfield_change(&dict, 0, PropertyValue::Uint(7), 6);
        let mut value = dict_value(5, "hi");
        let applied = apply_internal_stream(&dict, &mut value, &mut stream).unwrap();
        assert_eq!(value, dict_value(7, "hi"));
        assert_eq!(applied.old_value, PropertyValue::Uint(5));
        assert_eq!(applied.path, vec![PathElement::Field("ammo".into())]);
    }

    #[test]
    fn an_applied_deep_change_leaves_the_sibling_owners_alone() {
        // `entity.arr[4].dict.ammo = 7`: element 4's dict moves, element 3's
        // stays as it was, and the path records the descent.
        let array_of_dict = build_type_(
            "<a> ARRAY <size> 10 </size> \
             <of> FIXED_DICT <Properties> \
             <ammo> <Type> UINT8 </Type> </ammo> \
             </Properties> </of> </a>",
        );
        let element = array_of_dict.element_type().expect("element").clone();
        let field = element.field_type(0).expect("field").clone();
        let mut change = SinglePropertyChange::new(0, 1);
        change.change.set_is_nested_change();
        change.change.set_root_index(6);
        change.change.add_to_path(4, 10);
        let mut stream = BinaryStream::new();
        change
            .add_to_internal_stream(&field, &PropertyValue::Uint(7), &mut stream)
            .unwrap();

        let dict = |ammo| {
            PropertyValue::FixedDict(Some(vec![("ammo".to_string(), PropertyValue::Uint(ammo))]))
        };
        let mut value = PropertyValue::Sequence(vec![dict(0), dict(1), dict(2), dict(3), dict(4)]);
        let applied = apply_internal_stream(&array_of_dict, &mut value, &mut stream).unwrap();
        assert_eq!(
            value,
            PropertyValue::Sequence(vec![dict(0), dict(1), dict(2), dict(3), dict(7)])
        );
        // The old value is the leaf's previous value, not the dict.
        assert_eq!(applied.old_value, PropertyValue::Uint(4));
        assert_eq!(
            applied.path,
            vec![PathElement::Index(4), PathElement::Field("ammo".into())]
        );
    }

    #[test]
    fn an_applied_slice_splices_and_rebases_the_path_end() {
        // A variable array slice `[1..4) = [9]` shrinks the list, and the
        // path element reports the range with the end re-based on the new
        // size: `end + newSize - oldSize` = 4 + 3 - 5 = 2.
        let slots = slots_dict_type();
        let mut change = SlicePropertyChange::new(1, 4, 5);
        change.change.set_is_nested_change();
        change.change.set_root_index(6);
        change.change.add_to_path(0, 1);
        let mut stream = BinaryStream::new();
        let element = slots
            .field_type(0)
            .and_then(|array| array.element_type())
            .expect("element")
            .clone();
        change
            .add_to_internal_stream(&element, &[PropertyValue::Uint(9)], &mut stream)
            .unwrap();

        let mut value = PropertyValue::FixedDict(Some(vec![(
            "slots".to_string(),
            PropertyValue::Sequence(vec![
                PropertyValue::Uint(1),
                PropertyValue::Uint(2),
                PropertyValue::Uint(3),
                PropertyValue::Uint(4),
                PropertyValue::Uint(5),
            ]),
        )]));
        let applied = apply_internal_stream(&slots, &mut value, &mut stream).unwrap();
        assert_eq!(
            value,
            PropertyValue::FixedDict(Some(vec![(
                "slots".to_string(),
                PropertyValue::Sequence(vec![
                    PropertyValue::Uint(1),
                    PropertyValue::Uint(9),
                    PropertyValue::Uint(5)
                ])
            )]))
        );
        assert_eq!(
            applied.old_value,
            PropertyValue::Sequence(vec![
                PropertyValue::Uint(2),
                PropertyValue::Uint(3),
                PropertyValue::Uint(4)
            ])
        );
        assert_eq!(
            applied.path,
            vec![PathElement::Field("slots".into()), PathElement::Slice(1, 2)]
        );
    }

    #[test]
    fn a_fixed_size_slice_keeps_the_array_size_or_fails() {
        let slots = slots_dict_type();
        // Change `<of>` to a fixed size by rebuilding with <size>: matching
        // counts splice, mismatching counts report the reference warning.
        let fixed = build_type_(
            "<a> FIXED_DICT <Properties> \
             <slots> <Type> ARRAY <size> 5 </size> <of> UINT8 </of> </Type> </slots> \
             </Properties> </a>",
        );
        let mut value = PropertyValue::FixedDict(Some(vec![(
            "slots".to_string(),
            PropertyValue::Sequence(vec![
                PropertyValue::Uint(1),
                PropertyValue::Uint(2),
                PropertyValue::Uint(3),
                PropertyValue::Uint(4),
                PropertyValue::Uint(5),
            ]),
        )]));
        // Matching: three values for [1..4).
        let mut change = SlicePropertyChange::new(1, 4, 5);
        change.change.set_is_nested_change();
        change.change.set_root_index(6);
        change.change.add_to_path(0, 1);
        let mut stream = BinaryStream::new();
        change
            .add_to_internal_stream(
                &fixed
                    .field_type(0)
                    .and_then(|array| array.element_type())
                    .expect("element"),
                &[
                    PropertyValue::Uint(9),
                    PropertyValue::Uint(8),
                    PropertyValue::Uint(7),
                ],
                &mut stream,
            )
            .unwrap();
        let applied = apply_internal_stream(&fixed, &mut value, &mut stream).unwrap();
        assert_eq!(
            value,
            PropertyValue::FixedDict(Some(vec![(
                "slots".to_string(),
                PropertyValue::Sequence(vec![
                    PropertyValue::Uint(1),
                    PropertyValue::Uint(9),
                    PropertyValue::Uint(8),
                    PropertyValue::Uint(7),
                    PropertyValue::Uint(5)
                ])
            )]))
        );
        assert_eq!(applied.path[1], PathElement::Slice(1, 4));

        // Mismatching: two values for [1..4).
        let mut change = SlicePropertyChange::new(1, 4, 5);
        change.change.set_is_nested_change();
        change.change.set_root_index(6);
        change.change.add_to_path(0, 1);
        let mut stream = BinaryStream::new();
        change
            .add_to_internal_stream(
                &slots
                    .field_type(0)
                    .and_then(|array| array.element_type())
                    .expect("element"),
                &[PropertyValue::Uint(9), PropertyValue::Uint(8)],
                &mut stream,
            )
            .unwrap();
        let mut value = PropertyValue::FixedDict(Some(vec![(
            "slots".to_string(),
            PropertyValue::Sequence(vec![
                PropertyValue::Uint(1),
                PropertyValue::Uint(2),
                PropertyValue::Uint(3),
                PropertyValue::Uint(4),
                PropertyValue::Uint(5),
            ]),
        )]));
        assert_eq!(
            apply_internal_stream(&fixed, &mut value, &mut stream),
            Err(ApplyError::WrongArraySize)
        );
        assert_eq!(
            ApplyError::WrongArraySize.to_string(),
            "PyArrayDataInstance::setSlice: slice assignment would \
             create array of wrong size"
        );
    }

    #[test]
    fn a_flat_change_replaces_the_whole_property() {
        let uint8 = build_type_("<a> UINT8 </a>");
        let mut value = PropertyValue::Uint(5);
        let mut stream = BinaryStream::new();
        let change = SinglePropertyChange::new(0, 1);
        change
            .add_to_internal_stream(&uint8, &PropertyValue::Uint(7), &mut stream)
            .unwrap();
        let applied = apply_internal_stream(&uint8, &mut value, &mut stream).unwrap();
        assert_eq!(value, PropertyValue::Uint(7));
        assert_eq!(applied.old_value, PropertyValue::Uint(5));
        // The flat branch fills no change path.
        assert!(applied.path.is_empty());
    }

    #[test]
    fn bad_hops_report_the_reference_messages() {
        // A hop onto a scalar child: `getChildPropertyOwner` returns NULL.
        let dict = dict_type();
        let mut change = SinglePropertyChange::new(0, 2);
        change.change.set_is_nested_change();
        change.change.set_root_index(6);
        change.change.add_to_path(0, 2); // "ammo" is a UINT8, no children
        let field_type = dict.field_type(0).expect("field").clone();
        let mut stream = BinaryStream::new();
        change
            .add_to_internal_stream(&field_type, &PropertyValue::Uint(7), &mut stream)
            .unwrap();
        let mut value = dict_value(5, "hi");
        let err = apply_internal_stream(&dict, &mut value, &mut stream).unwrap_err();
        assert_eq!(err, ApplyError::OwnerIsNull { hop: 1, size: 1 });
        assert_eq!(
            err.to_string(),
            "PropertyChangeReader::readAndApply: pOwner is NULL. 1/1.\n"
        );

        // A hop out of range: bounds check with the reference's message.
        let mut change = SinglePropertyChange::new(0, 2);
        change.change.set_is_nested_change();
        change.change.set_root_index(6);
        change.change.add_to_path(9, 2);
        let field_type = dict.field_type(0).expect("field").clone();
        let mut stream = BinaryStream::new();
        change
            .add_to_internal_stream(&field_type, &PropertyValue::Uint(7), &mut stream)
            .unwrap();
        let mut value = dict_value(5, "hi");
        let err = apply_internal_stream(&dict, &mut value, &mut stream).unwrap_err();
        assert_eq!(
            err,
            ApplyError::BadChildIndex {
                owner: FIXED_DICT_GET_CHILD,
                index: 9,
                size: 2
            }
        );
        assert_eq!(
            err.to_string(),
            "PyFixedDictDataInstance::getChildPropertyOwner: Bad index 9. size = 2\n"
        );
    }

    #[test]
    fn a_leaf_index_off_the_end_is_refused() {
        let dict = dict_type();
        let mut stream = encode_subfield_change(&dict, 0, PropertyValue::Uint(7), 6);
        let mut value = PropertyValue::FixedDict(None);
        assert_eq!(
            apply_internal_stream(&dict, &mut value, &mut stream),
            Err(ApplyError::LeafIndexOutOfRange { index: 0, size: 0 })
        );
    }

    #[test]
    fn a_slice_on_an_owner_without_set_owned_slice_reports_null() {
        let slots = slots_dict_type();
        let mut change = SlicePropertyChange::new(0, 1, 1);
        change.change.set_is_nested_change();
        change.change.set_root_index(6);
        change.change.add_to_path(0, 1);
        let mut stream = BinaryStream::new();
        let element = slots
            .field_type(0)
            .and_then(|array| array.element_type())
            .expect("element")
            .clone();
        change
            .add_to_internal_stream(&element, &[PropertyValue::Uint(9)], &mut stream)
            .unwrap();
        let mut value = PropertyValue::FixedDict(Some(vec![(
            "slots".to_string(),
            PropertyValue::FixedDict(None),
        )]));
        let err = apply_internal_stream(&slots, &mut value, &mut stream).unwrap_err();
        assert_eq!(err, ApplyError::OldValueIsNull);
        assert_eq!(
            err.to_string(),
            "PropertyChangeReader::readAndApply: Old value is NULL\n"
        );
    }

    #[test]
    fn a_truncated_slice_reports_the_stream_element_message() {
        let slots = slots_dict_type();
        let mut stream = BinaryStream::new();
        stream.write_u8(FLAG_IS_SLICE | FLAG_IS_NESTED);
        stream.write_u8(1);
        stream.write_i32(0);
        stream.write_u32(0);
        // The end index and the payload are missing.
        let mut value = PropertyValue::FixedDict(Some(vec![(
            "slots".to_string(),
            PropertyValue::Sequence(vec![PropertyValue::Uint(1)]),
        )]));
        let err = apply_internal_stream(&slots, &mut value, &mut stream).unwrap_err();
        assert_eq!(
            err,
            ApplyError::Payload(PropertyError::StreamTruncated {
                element: SIMPLE_STREAM_ELEMENT
            })
        );
    }

    #[test]
    fn the_apply_error_texts_pin_their_reference_literals() {
        assert_eq!(
            ApplyError::InvalidSlice {
                start: 4,
                end: 2,
                size: 5,
                remaining: 3
            }
            .to_string(),
            "PyArrayDataInstance::setOwnedSlice: Invalid values. \
             startIndex = 4. endIndex = 2. values = 5 \
             data.remainingLength = 3\n"
        );
        assert_eq!(
            ApplyError::UnableToCreateSlice.to_string(),
            "PyArrayDataInstance::setOwnedSlice: Unable to create slice\n"
        );
        assert_eq!(
            ApplyError::BadChildIndex {
                owner: ARRAY_GET_CHILD,
                index: 7,
                size: 3
            }
            .to_string(),
            "PyArrayDataInstance::getChildPropertyOwner: Bad index 7. size = 3\n"
        );
        assert!(
            ApplyError::FlatSliceOnInternalStream
                .to_string()
                .contains("MF_ASSERT( !isSlice )")
        );
    }

    #[test]
    fn a_nested_single_change_can_retune_one_array_element() {
        // `entity.grid[0][1] = 9` on an ARRAY-of-ARRAY: the leaf owner is an
        // array and `setOwnedProperty` swaps one element.
        let grid = build_type_(
            "<a> ARRAY <size> 2 </size> <of> ARRAY <size> 2 </size> <of> UINT8 </of> </of> </a>",
        );
        let inner = grid.element_type().expect("inner").clone();
        let leaf_type = inner.element_type().expect("leaf").clone();
        let mut change = SinglePropertyChange::new(1, 2);
        change.change.set_is_nested_change();
        change.change.set_root_index(3);
        change.change.add_to_path(0, 2);
        let mut stream = BinaryStream::new();
        change
            .add_to_internal_stream(&leaf_type, &PropertyValue::Uint(9), &mut stream)
            .unwrap();

        let row =
            |a, b| PropertyValue::Sequence(vec![PropertyValue::Uint(a), PropertyValue::Uint(b)]);
        let mut value = PropertyValue::Sequence(vec![row(1, 2), row(3, 4)]);
        let applied = apply_internal_stream(&grid, &mut value, &mut stream).unwrap();
        assert_eq!(value, PropertyValue::Sequence(vec![row(1, 9), row(3, 4)]));
        assert_eq!(applied.old_value, PropertyValue::Uint(2));
        assert_eq!(
            applied.path,
            vec![PathElement::Index(0), PathElement::Index(1)]
        );
    }

    #[test]
    fn an_array_hop_off_the_end_reports_the_array_message() {
        let grid = build_type_("<a> ARRAY <size> 5 </size> <of> UINT8 </of> </a>");
        let element = grid.element_type().expect("element").clone();
        let mut change = SinglePropertyChange::new(0, 5);
        change.change.set_is_nested_change();
        change.change.set_root_index(6);
        change.change.add_to_path(99, 5);
        let mut stream = BinaryStream::new();
        change
            .add_to_internal_stream(&element, &PropertyValue::Uint(7), &mut stream)
            .unwrap();
        let mut value = PropertyValue::Sequence(vec![PropertyValue::Uint(0); 5]);
        let err = apply_internal_stream(&grid, &mut value, &mut stream).unwrap_err();
        assert_eq!(
            err,
            ApplyError::BadChildIndex {
                owner: ARRAY_GET_CHILD,
                index: 99,
                size: 5
            }
        );
        assert_eq!(
            err.to_string(),
            "PyArrayDataInstance::getChildPropertyOwner: Bad index 99. size = 5\n"
        );
    }

    #[test]
    fn a_flat_slice_is_refused_on_the_internal_stream() {
        let uint8 = build_type_("<a> UINT8 </a>");
        let mut stream = BinaryStream::new();
        stream.write_u8(FLAG_IS_SLICE);
        let mut value = PropertyValue::Uint(0);
        assert_eq!(
            apply_internal_stream(&uint8, &mut value, &mut stream),
            Err(ApplyError::FlatSliceOnInternalStream)
        );
    }

    #[test]
    fn an_unparseable_slice_payload_reports_the_slice_message() {
        // A BLOB element whose length runs past the stream: the reference
        // prints "Unable to create slice" and reports no old value.
        let blobs = build_type_("<a> ARRAY <of> BLOB </of> </a>");
        let mut stream = BinaryStream::new();
        stream.write_u8(FLAG_IS_SLICE | FLAG_IS_NESTED);
        stream.write_u8(0);
        stream.write_u32(0);
        stream.write_u32(1);
        let _ = stream.write_packed_int(5);
        let _ = stream.write_blob(&[1, 2]);
        let mut value = PropertyValue::Sequence(vec![PropertyValue::Blob(vec![0; 5])]);
        let err = apply_internal_stream(&blobs, &mut value, &mut stream).unwrap_err();
        assert_eq!(err, ApplyError::UnableToCreateSlice);
        assert_eq!(
            err.to_string(),
            "PyArrayDataInstance::setOwnedSlice: Unable to create slice\n"
        );
    }

    #[test]
    fn the_remaining_apply_errors_display_their_texts() {
        assert_eq!(
            ApplyError::LeafIndexOutOfRange { index: 3, size: 2 }.to_string(),
            "Leaf index 3 out of range (owner has 2 children)"
        );
        let inner = PropertyError::TypeMismatch { meta: "mailbox" };
        assert_eq!(
            ApplyError::Payload(inner.clone()).to_string(),
            inner.to_string()
        );
    }

    #[test]
    fn a_nested_change_at_a_scalar_root_and_a_leaf_off_the_end_are_refused() {
        // A nested path that arrives for a scalar property: the walk cannot
        // descend — `getChildPropertyOwner` hands back NULL, and the value
        // is left untouched.
        let uint8 = build_type_("<a> UINT8 </a>");
        let mut stream = BinaryStream::new();
        stream.write_u8(FLAG_IS_NESTED);
        stream.write_u8(1);
        stream.write_i32(0);
        stream.write_i32(0);
        stream.write_u8(7);
        let mut value = PropertyValue::Uint(0);
        assert_eq!(
            apply_internal_stream(&uint8, &mut value, &mut stream),
            Err(ApplyError::OwnerIsNull { hop: 1, size: 1 })
        );
        assert_eq!(value, PropertyValue::Uint(0));

        // A leaf index past the end of an array owner: the reference would
        // index `values_[ref]` unchecked; this port refuses.
        let grid = build_type_(
            "<a> ARRAY <size> 2 </size> \
             <of> ARRAY <size> 1 </size> <of> UINT8 </of> </of> </a>",
        );
        let inner = grid.element_type().expect("inner").clone();
        let leaf = inner.element_type().expect("leaf").clone();
        let mut change = SinglePropertyChange::new(9, 2);
        change.change.set_is_nested_change();
        change.change.set_root_index(6);
        change.change.add_to_path(0, 2);
        let mut stream = BinaryStream::new();
        change
            .add_to_internal_stream(&leaf, &PropertyValue::Uint(7), &mut stream)
            .unwrap();
        let mut value = PropertyValue::Sequence(vec![
            PropertyValue::Sequence(vec![PropertyValue::Uint(0)]),
            PropertyValue::Sequence(vec![PropertyValue::Uint(1)]),
        ]);
        assert_eq!(
            apply_internal_stream(&grid, &mut value, &mut stream),
            Err(ApplyError::LeafIndexOutOfRange { index: 9, size: 1 })
        );
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
