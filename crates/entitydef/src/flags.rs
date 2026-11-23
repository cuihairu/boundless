//! `data_description.hpp`: the `EntityDataFlags` bit set, the
//! `s_entityDataFlagMappings` table and the data-domain / pass tables.
//!
//! These bit values sit in the wire format (property distribution decisions are
//! taken from them on both ends), so a mismatch here silently re-shapes every
//! replicated property. The constants are pinned verbatim against the reference.

/// `DATA_GHOSTED`: synchronised to ghost entities.
pub const GHOSTED: u32 = 0x01;
/// `DATA_OTHER_CLIENT`: sent to other clients.
pub const OTHER_CLIENT: u32 = 0x02;
/// `DATA_OWN_CLIENT`: sent to own client.
pub const OWN_CLIENT: u32 = 0x04;
/// `DATA_BASE`: sent to the base.
pub const BASE: u32 = 0x08;
/// `DATA_CLIENT_ONLY`: static client-side data only.
pub const CLIENT_ONLY: u32 = 0x10;
/// `DATA_PERSISTENT`: saved to the database.
pub const PERSISTENT: u32 = 0x20;
/// `DATA_EDITOR_ONLY`: only read and written by the editor.
pub const EDITOR_ONLY: u32 = 0x40;
/// `DATA_ID`: is an indexed column in the database.
pub const ID: u32 = 0x80;
/// `DATA_REPLAY`: is exposed for replay.
pub const REPLAY: u32 = 0x100;

/// `DATA_DISTRIBUTION_FLAGS`: the flags that describe where data lives.
///
/// Persistence and the database-id bits are deliberately excluded: they are
/// storage attributes, not distribution ones, and the reference masks them out
/// before hashing (`dataFlags_ & DATA_DISTRIBUTION_FLAGS`).
pub const DATA_DISTRIBUTION_FLAGS: u32 =
    GHOSTED | OTHER_CLIENT | OWN_CLIENT | BASE | CLIENT_ONLY | EDITOR_ONLY;

/// `DEFAULT_DATABASE_LENGTH`.
pub const DEFAULT_DATABASE_LENGTH: i32 = 65535;

/// `DataType::CLIENT_SAFE`, `CLIENT_UNSAFE`, `CLIENT_UNUSABLE`.
pub const CLIENT_SAFE: u32 = 0;
pub const CLIENT_UNSAFE: u32 = 0x1;
pub const CLIENT_UNUSABLE: u32 = 0x2;

/// `DatabaseIndexingType`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DatabaseIndexing {
    /// `DATABASE_INDEXING_NONE`.
    #[default]
    None,
    /// `DATABASE_INDEXING_UNIQUE`.
    Unique,
    /// `DATABASE_INDEXING_NON_UNIQUE`.
    NonUnique,
}

impl DatabaseIndexing {
    /// `isIndexed()`: anything but `NONE`.
    pub fn is_indexed(self) -> bool {
        self != Self::None
    }

    /// `isUnique()`.
    pub fn is_unique(self) -> bool {
        self == Self::Unique
    }
}

/// A bit set of property distribution flags (`EntityDataFlags`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntityDataFlags(u32);

impl EntityDataFlags {
    /// Wrap raw bits.
    pub const fn new(bits: u32) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    /// `DATA_DISTRIBUTION_FLAGS` mask applied, as the digest does.
    pub const fn distribution_bits(self) -> u32 {
        self.0 & DATA_DISTRIBUTION_FLAGS
    }

    /// `isGhostedData()`.
    pub const fn is_ghosted_data(self) -> bool {
        self.0 & GHOSTED != 0
    }

    /// `isOtherClientData()`.
    pub const fn is_other_client_data(self) -> bool {
        self.0 & OTHER_CLIENT != 0
    }

    /// `isOwnClientData()`.
    pub const fn is_own_client_data(self) -> bool {
        self.0 & OWN_CLIENT != 0
    }

    /// `isBaseData()`.
    pub const fn is_base_data(self) -> bool {
        self.0 & BASE != 0
    }

    /// `isCellData()`: not base data. At the moment data only lives on a base
    /// or a cell, never both.
    pub fn is_cell_data(self) -> bool {
        !self.is_base_data()
    }

    /// `isClientServerData()`: `OTHER_CLIENT | OWN_CLIENT | REPLAY`.
    pub const fn is_client_server_data(self) -> bool {
        self.0 & (OTHER_CLIENT | OWN_CLIENT | REPLAY) != 0
    }

    /// `isPersistent()`.
    pub const fn is_persistent(self) -> bool {
        self.0 & PERSISTENT != 0
    }

    /// `isEditorOnly()`.
    pub const fn is_editor_only(self) -> bool {
        self.0 & EDITOR_ONLY != 0
    }

    /// `isIdentifier()`.
    pub const fn is_identifier(self) -> bool {
        self.0 & ID != 0
    }

    /// `isExposedForReplay()`.
    pub const fn is_exposed_for_replay(self) -> bool {
        self.0 & REPLAY != 0
    }

    /// `isOfType(flags)`: `(dataFlags_ & flags) == flags`.
    pub const fn is_of_type(self, other: EntityDataFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// `getDataFlagsAsStr()`: the `<Flags>` spelling for this distribution
    /// mask, if any (`NULL` when the bits are a custom combination).
    pub fn as_flag_name(self) -> Option<&'static str> {
        FLAG_MAPPINGS
            .iter()
            .find(|(_, flags)| *flags == self.distribution_bits())
            .map(|(name, _)| *name)
    }
}

impl std::ops::BitOr for EntityDataFlags {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// `s_entityDataFlagMappings`, in table order.
pub const FLAG_MAPPINGS: &[(&str, u32)] = &[
    ("CELL_PRIVATE", 0),
    ("CELL_PUBLIC", GHOSTED),
    ("OTHER_CLIENTS", GHOSTED | OTHER_CLIENT),
    ("OWN_CLIENT", OWN_CLIENT),
    ("BASE", BASE),
    ("BASE_AND_CLIENT", OWN_CLIENT | BASE),
    ("CELL_PUBLIC_AND_OWN", GHOSTED | OWN_CLIENT),
    ("ALL_CLIENTS", GHOSTED | OTHER_CLIENT | OWN_CLIENT),
    ("EDITOR_ONLY", EDITOR_ONLY),
];

/// `setEntityDataFlags`: look a `<Flags>` name up in the table.
pub fn set_entity_data_flags(name: &str) -> Option<EntityDataFlags> {
    FLAG_MAPPINGS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, flags)| EntityDataFlags::new(*flags))
}

// ---------------------------------------------------------------------------
// Data domains (`data_domain.hpp`) and the pass tables
// ---------------------------------------------------------------------------

/// `DataDomain::BASE_DATA`.
pub const BASE_DATA: u32 = 0x1;
/// `DataDomain::CLIENT_DATA`.
pub const CLIENT_DATA: u32 = 0x2;
/// `DataDomain::CELL_DATA`.
pub const CELL_DATA: u32 = 0x4;
/// `DataDomain::EXACT_MATCH`.
pub const EXACT_MATCH: u32 = 0x8;
/// `DataDomain::ONLY_OTHER_CLIENT_DATA`.
pub const ONLY_OTHER_CLIENT_DATA: u32 = 0x10;
/// `DataDomain::ONLY_PERSISTENT_DATA`.
pub const ONLY_PERSISTENT_DATA: u32 = 0x20;

/// `DataDomain::FROM_CELL_TO_CLIENT_DATA` (`CELL_DATA|CLIENT_DATA|EXACT_MATCH`).
pub const FROM_CELL_TO_CLIENT_DATA: u32 = CELL_DATA | CLIENT_DATA | EXACT_MATCH;
/// `DataDomain::FROM_BASE_TO_CLIENT_DATA` (`BASE_DATA|CLIENT_DATA|EXACT_MATCH`).
pub const FROM_BASE_TO_CLIENT_DATA: u32 = BASE_DATA | CLIENT_DATA | EXACT_MATCH;

/// `PASS_FILTER[NUM_PASSES][2]` (see `shouldConsiderData`).
pub const PASS_FILTER: [[bool; 2]; 4] =
    [[true, false], [true, true], [false, true], [false, false]];

/// `PASS_JUMPER[NUM_PASSES]` (see `shouldSkipPass`).
pub const PASS_JUMPER: [u32; 4] = [
    EXACT_MATCH | BASE_DATA,
    EXACT_MATCH | BASE_DATA | CLIENT_DATA,
    EXACT_MATCH | CELL_DATA | CLIENT_DATA,
    EXACT_MATCH | CELL_DATA,
];

/// `NUM_PASSES`.
pub const NUM_PASSES: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_values_match_the_reference_enum() {
        assert_eq!(GHOSTED, 0x01);
        assert_eq!(OTHER_CLIENT, 0x02);
        assert_eq!(OWN_CLIENT, 0x04);
        assert_eq!(BASE, 0x08);
        assert_eq!(CLIENT_ONLY, 0x10);
        assert_eq!(PERSISTENT, 0x20);
        assert_eq!(EDITOR_ONLY, 0x40);
        assert_eq!(ID, 0x80);
        assert_eq!(REPLAY, 0x100);
        assert_eq!(DATA_DISTRIBUTION_FLAGS, 0x5F);
        assert_eq!(DEFAULT_DATABASE_LENGTH, 65535);
    }

    #[test]
    fn distribution_mask_drops_persistence_and_id_bits() {
        let flags = EntityDataFlags::new(GHOSTED | OTHER_CLIENT | PERSISTENT | ID | REPLAY);
        assert_eq!(
            flags.distribution_bits(),
            GHOSTED | OTHER_CLIENT,
            "the digest masks out PERSISTENT and ID"
        );
    }

    #[test]
    fn predicates_follow_the_reference_bit_tests() {
        let cell_public = EntityDataFlags::new(GHOSTED);
        assert!(cell_public.is_ghosted_data());
        assert!(!cell_public.is_other_client_data());
        assert!(cell_public.is_cell_data());

        let base = EntityDataFlags::new(BASE);
        assert!(base.is_base_data());
        assert!(!base.is_cell_data(), "isCellData() == !isBaseData()");
        assert!(base.is_of_type(EntityDataFlags::new(BASE)));

        let replay_only = EntityDataFlags::new(REPLAY);
        assert!(replay_only.is_client_server_data());
        assert!(replay_only.is_exposed_for_replay());

        let cell_private = EntityDataFlags::new(0);
        assert!(!cell_private.is_client_server_data());
    }

    #[test]
    fn flag_table_is_in_reference_order_and_is_lookupable() {
        assert_eq!(FLAG_MAPPINGS.len(), 9);
        assert_eq!(FLAG_MAPPINGS[0], ("CELL_PRIVATE", 0));
        assert_eq!(
            set_entity_data_flags("ALL_CLIENTS"),
            Some(EntityDataFlags::new(GHOSTED | OTHER_CLIENT | OWN_CLIENT))
        );
        assert_eq!(
            set_entity_data_flags("BASE_AND_CLIENT"),
            Some(EntityDataFlags::new(OWN_CLIENT | BASE))
        );
        assert_eq!(
            set_entity_data_flags("EDITOR_ONLY"),
            Some(EntityDataFlags::new(EDITOR_ONLY))
        );
        assert_eq!(set_entity_data_flags("NONSENSE"), None);
    }

    #[test]
    fn flag_names_roundtrip_through_the_distribution_mask() {
        assert_eq!(
            EntityDataFlags::new(GHOSTED).as_flag_name(),
            Some("CELL_PUBLIC")
        );
        assert_eq!(
            EntityDataFlags::new(GHOSTED | OTHER_CLIENT | REPLAY).as_flag_name(),
            Some("OTHER_CLIENTS")
        );
        // A combination with no table entry has no name.
        assert_eq!(EntityDataFlags::new(GHOSTED | BASE).as_flag_name(), None);
    }

    #[test]
    fn database_indexing_predicates() {
        assert!(!DatabaseIndexing::None.is_indexed());
        assert!(!DatabaseIndexing::None.is_unique());
        assert!(DatabaseIndexing::Unique.is_indexed());
        assert!(DatabaseIndexing::Unique.is_unique());
        assert!(DatabaseIndexing::NonUnique.is_indexed());
        assert!(!DatabaseIndexing::NonUnique.is_unique());
    }

    #[test]
    fn domain_and_pass_tables_match_the_reference() {
        assert_eq!(BASE_DATA, 0x1);
        assert_eq!(CLIENT_DATA, 0x2);
        assert_eq!(CELL_DATA, 0x4);
        assert_eq!(EXACT_MATCH, 0x8);
        assert_eq!(ONLY_OTHER_CLIENT_DATA, 0x10);
        assert_eq!(ONLY_PERSISTENT_DATA, 0x20);
        assert_eq!(FROM_CELL_TO_CLIENT_DATA, 0xE);
        assert_eq!(FROM_BASE_TO_CLIENT_DATA, 0xB);

        assert_eq!(NUM_PASSES, 4);
        assert_eq!(
            PASS_FILTER,
            [[true, false], [true, true], [false, true], [false, false]]
        );
        assert_eq!(
            PASS_JUMPER,
            [
                EXACT_MATCH | BASE_DATA,
                EXACT_MATCH | BASE_DATA | CLIENT_DATA,
                EXACT_MATCH | CELL_DATA | CLIENT_DATA,
                EXACT_MATCH | CELL_DATA,
            ]
        );
    }
}
