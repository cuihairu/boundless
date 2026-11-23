//! `ExposedMessageRange` and its method/property variants
//! (`lib/network/exposed_message_range.{hpp,cpp}`).
//!
//! BW-02-02-01: this range arithmetic is what turns a block of message ids
//! (allocated by `InterfaceMinder::addRange`) into an index space for the
//! methods and properties a client can reach, so a single 255-id wide
//! message can address arbitrarily many exposed methods.

use crate::misc::MessageId;

/// Manages a range of message ids, used when a single handler handles a
/// range of message ids -- for example property changes and method calls
/// sent from the server to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExposedMessageRange {
    first_msg_id: i32,
    last_msg_id: i32,
}

impl ExposedMessageRange {
    pub const fn new(first_msg_id: i32, last_msg_id: i32) -> Self {
        Self {
            first_msg_id,
            last_msg_id,
        }
    }

    pub const fn contains(&self, msg_id: MessageId) -> bool {
        (self.first_msg_id <= msg_id as i32) && (msg_id as i32 <= self.last_msg_id)
    }

    pub const fn num_slots(&self) -> i32 {
        self.last_msg_id - self.first_msg_id + 1
    }

    pub const fn first_msg_id(&self) -> i32 {
        self.first_msg_id
    }

    pub const fn last_msg_id(&self) -> i32 {
        self.last_msg_id
    }

    /// `simpleExposedIDFromMsgID`.
    pub const fn simple_exposed_id_from_msg_id(&self, msg_id: MessageId) -> i32 {
        msg_id as i32 - self.first_msg_id
    }
}

/// A range of message ids used for calling exposed methods.
///
/// A range with more exposed methods than slots keeps enough slots for
/// `numSubSlots` overflow ids: the overflow ids are packed 255 per
/// remaining slot, and only overflow ids carry a sub-message id byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExposedMethodMessageRange {
    range: ExposedMessageRange,
}

impl ExposedMethodMessageRange {
    pub const fn new(first_msg_id: i32, last_msg_id: i32) -> Self {
        Self {
            range: ExposedMessageRange::new(first_msg_id, last_msg_id),
        }
    }

    /// `contains`, `numSlots` and the exposed-id offsets all delegate to the
    /// underlying range.
    pub const fn contains(&self, msg_id: MessageId) -> bool {
        self.range.contains(msg_id)
    }

    pub const fn num_slots(&self) -> i32 {
        self.range.num_slots()
    }

    pub const fn first_msg_id(&self) -> i32 {
        self.range.first_msg_id()
    }

    pub const fn last_msg_id(&self) -> i32 {
        self.range.last_msg_id()
    }

    /// `msgIDFromExposedID`: the message id and (possibly) sub-message id
    /// for an exposed method index.
    ///
    /// The sub-message id is `-1` when the method does not overflow into a
    /// second level of ids, in which case no sub-index byte is written to the
    /// stream.
    #[must_use]
    pub const fn msg_id_from_exposed_id(&self, exposed_id: i32, num_exposed: i32) -> (i32, i32) {
        let num_remaining_slots = self.num_remaining_slots(num_exposed);

        if exposed_id < num_remaining_slots {
            (self.range.first_msg_id() + exposed_id, -1)
        } else {
            let overflow = exposed_id - num_remaining_slots;
            let first_overflow_slot = self.range.first_msg_id() + num_remaining_slots;
            (first_overflow_slot + overflow / 256, overflow % 256)
        }
    }

    /// `exposedIDFromMsgID`: the exposed method index for a message id.
    ///
    /// `sub_index` is the extra byte read from the stream when the message
    /// id lies in an overflow slot; `None` when the id is a plain slot. The
    /// reference reads the byte straight off `BinaryIStream`, which is not
    /// ported until BW-02-02-02.
    #[must_use]
    pub fn exposed_id_from_msg_id(
        &self,
        msg_id: MessageId,
        sub_index: Option<u8>,
        num_exposed: i32,
    ) -> i32 {
        let num_remaining_slots = self.num_remaining_slots(num_exposed);
        let offset = msg_id as i32 - self.range.first_msg_id();

        if offset < num_remaining_slots {
            return offset;
        }

        let sub_index = sub_index.expect("overflow message ids carry a sub-index byte");
        num_remaining_slots + 256 * (offset - num_remaining_slots) + sub_index as i32
    }

    /// `needsSubID`: whether the exposed method id needs a sub-message id
    /// when sending between client and server.
    #[must_use]
    pub const fn needs_sub_id(&self, exposed_id: i32, num_exposed: i32) -> bool {
        exposed_id >= self.num_remaining_slots(num_exposed)
    }

    /// The number of top-level slots (message ids) remaining when the range
    /// must handle `num_exposed` different method ids.
    const fn num_remaining_slots(&self, num_exposed: i32) -> i32 {
        self.range.num_slots() - self.num_sub_slots(num_exposed)
    }

    /// The number of message ids allocated to overflow indexes.
    const fn num_sub_slots(&self, num_exposed: i32) -> i32 {
        let num_excess = num_exposed - self.range.num_slots();
        if num_excess > 0 {
            num_excess / 255 + 1
        } else {
            0
        }
    }
}

/// A range of message ids used for sending property updates from the server
/// to the client. The mapping is a plain offset, so out-of-range ids report
/// `-1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExposedPropertyMessageRange {
    range: ExposedMessageRange,
}

impl ExposedPropertyMessageRange {
    pub const fn new(first_msg_id: i32, last_msg_id: i32) -> Self {
        Self {
            range: ExposedMessageRange::new(first_msg_id, last_msg_id),
        }
    }

    pub const fn contains(&self, msg_id: MessageId) -> bool {
        self.range.contains(msg_id)
    }

    pub const fn num_slots(&self) -> i32 {
        self.range.num_slots()
    }

    /// The exposed property id for a message id.
    #[must_use]
    pub const fn exposed_id_from_msg_id(&self, msg_id: MessageId) -> i32 {
        self.range.simple_exposed_id_from_msg_id(msg_id)
    }

    /// The message id associated with an exposed property id, or `-1` if the
    /// value does not fit in this range.
    #[must_use]
    pub const fn msg_id_from_exposed_id(&self, exposed_id: i32) -> i32 {
        if exposed_id < self.range.num_slots() {
            exposed_id + self.range.first_msg_id()
        } else {
            -1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference's own round-trip check
    /// (`lib/network/unit_test/test_exposed_message_range.cpp`): a range from
    /// id 10 to 20 holding 300 exposed methods, i.e. far more than the
    /// eleven slots available, so nearly all ids overflow.
    const START_MSG_ID: i32 = 10;
    const END_MSG_ID: i32 = 20;
    const NUM_EXPOSED: i32 = 300;

    #[test]
    fn the_method_range_round_trips_more_exposed_than_slots() {
        let range = ExposedMethodMessageRange::new(START_MSG_ID, END_MSG_ID);
        assert_eq!(range.num_slots(), END_MSG_ID - START_MSG_ID + 1);

        for exposed_id in 0..NUM_EXPOSED {
            let (msg_id, sub_msg_id) = range.msg_id_from_exposed_id(exposed_id, NUM_EXPOSED);

            assert!(START_MSG_ID <= msg_id);
            assert!(msg_id <= END_MSG_ID);

            let sub_index = if sub_msg_id != -1 {
                assert!(range.needs_sub_id(exposed_id, NUM_EXPOSED));
                Some(sub_msg_id as u8)
            } else {
                assert!(!range.needs_sub_id(exposed_id, NUM_EXPOSED));
                None
            };

            assert!(range.contains(msg_id as MessageId));
            assert_eq!(
                range.exposed_id_from_msg_id(msg_id as MessageId, sub_index, NUM_EXPOSED),
                exposed_id
            );
        }
    }

    #[test]
    fn the_method_range_fits_plain_slots_when_there_is_no_overflow() {
        // Five exposed methods over eleven slots: everything is a plain slot
        // and no sub-index byte is needed at all.
        let range = ExposedMethodMessageRange::new(10, 20);
        assert_eq!(range.num_slots(), 11);

        for exposed_id in 0..5i32 {
            let (msg_id, sub_msg_id) = range.msg_id_from_exposed_id(exposed_id, 5);
            assert_eq!(msg_id, START_MSG_ID + exposed_id);
            assert_eq!(sub_msg_id, -1);
            assert!(!range.needs_sub_id(exposed_id, 5));
            assert_eq!(
                range.exposed_id_from_msg_id(msg_id as MessageId, None, 5),
                exposed_id
            );
        }

        // A slot outside the range is not claimable.
        assert!(!range.contains(9));
        assert!(!range.contains(21));
    }

    #[test]
    fn the_property_range_maps_offsets_directly() {
        let range = ExposedPropertyMessageRange::new(10, 20);
        assert_eq!(range.num_slots(), 11);

        for exposed_id in 0..11i32 {
            let msg_id = range.msg_id_from_exposed_id(exposed_id);
            assert_eq!(msg_id, START_MSG_ID + exposed_id);
            assert_eq!(
                range.exposed_id_from_msg_id(msg_id as MessageId),
                exposed_id
            );
        }

        // Beyond the last slot the property range cannot represent an id.
        assert_eq!(range.msg_id_from_exposed_id(11), -1);
        assert_eq!(range.msg_id_from_exposed_id(12), -1);
    }

    #[test]
    fn the_overflow_uses_the_reference_255_per_slot_packing() {
        // Two overflow slots are needed for anything past 300+... the port
        // must match the reference's exact division, not a rounded one.
        let range = ExposedMethodMessageRange::new(START_MSG_ID, END_MSG_ID);

        // 11 slots: numSubSlots = (300-11)/255 + 1 = 2, leaving 9 slots
        // for plain ids, so the first nine methods take ids 10..18 and the
        // rest overflow from slot 19.
        let (plain_msg_id, plain_sub) = range.msg_id_from_exposed_id(8, NUM_EXPOSED);
        assert_eq!((plain_msg_id, plain_sub), (18, -1));

        let (overflow_msg_id, overflow_sub) = range.msg_id_from_exposed_id(9, NUM_EXPOSED);
        assert_eq!((overflow_msg_id, overflow_sub), (19, 0));

        // 256 per overflow slot in the wire arithmetic, not 255.
        let (msg_id_265, sub_265) = range.msg_id_from_exposed_id(9 + 256, NUM_EXPOSED);
        assert_eq!((msg_id_265, sub_265), (20, 0));
    }
}
