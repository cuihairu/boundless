//! `SceneObjectFlags` (`lib/scene/scene_object_flags.{hpp,cpp}`).

/// `StorageType` (`scene_object_flags.hpp:20`).
pub type StorageType = u8;

pub const IS_DYNAMIC_OBJECT: StorageType = 1 << 0;
pub const IS_REFLECTION_VISIBLE: StorageType = 1 << 1;
pub const IS_CASTING_STATIC_SHADOW: StorageType = 1 << 2;
pub const IS_CASTING_DYNAMIC_SHADOW: StorageType = 1 << 3;
pub const IS_TERRAIN_OBJECT: StorageType = 1 << 4;

/// Five flag bits over a `u8`. Getters test the bit; setters use the
/// reference's clear-then-merge form
/// (`scene_object_flags.cpp:30-34` and each following pair).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SceneObjectFlags {
    flags: StorageType,
}

impl SceneObjectFlags {
    /// `SceneObjectFlags()` (`scene_object_flags.cpp:8-12`).
    pub fn new() -> Self {
        Self { flags: 0 }
    }

    /// `explicit SceneObjectFlags(StorageType flags)` (hpp:22-24).
    pub fn from_storage(flags: StorageType) -> Self {
        Self { flags }
    }

    pub fn storage(&self) -> StorageType {
        self.flags
    }

    fn set(&mut self, mask: StorageType, value: bool) {
        self.flags &= !mask;
        self.flags |= if value { mask } else { 0 };
    }

    pub fn is_dynamic(&self) -> bool {
        (self.flags & IS_DYNAMIC_OBJECT) != 0
    }

    pub fn set_is_dynamic(&mut self, value: bool) {
        self.set(IS_DYNAMIC_OBJECT, value);
    }

    pub fn is_reflection_visible(&self) -> bool {
        (self.flags & IS_REFLECTION_VISIBLE) != 0
    }

    pub fn set_is_reflection_visible(&mut self, value: bool) {
        self.set(IS_REFLECTION_VISIBLE, value);
    }

    pub fn is_casting_static_shadow(&self) -> bool {
        (self.flags & IS_CASTING_STATIC_SHADOW) != 0
    }

    pub fn set_is_casting_static_shadow(&mut self, value: bool) {
        self.set(IS_CASTING_STATIC_SHADOW, value);
    }

    pub fn is_casting_dynamic_shadow(&self) -> bool {
        (self.flags & IS_CASTING_DYNAMIC_SHADOW) != 0
    }

    pub fn set_is_casting_dynamic_shadow(&mut self, value: bool) {
        self.set(IS_CASTING_DYNAMIC_SHADOW, value);
    }

    pub fn is_terrain(&self) -> bool {
        (self.flags & IS_TERRAIN_OBJECT) != 0
    }

    pub fn set_is_terrain(&mut self, value: bool) {
        self.set(IS_TERRAIN_OBJECT, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_default_to_zero_and_round_trip_through_storage() {
        let default = SceneObjectFlags::new();
        assert_eq!(default.storage(), 0);
        assert!(!default.is_dynamic());
        assert!(!default.is_reflection_visible());
        assert!(!default.is_casting_static_shadow());
        assert!(!default.is_casting_dynamic_shadow());
        assert!(!default.is_terrain());

        let all = SceneObjectFlags::from_storage(
            IS_DYNAMIC_OBJECT
                | IS_REFLECTION_VISIBLE
                | IS_CASTING_STATIC_SHADOW
                | IS_CASTING_DYNAMIC_SHADOW
                | IS_TERRAIN_OBJECT,
        );
        assert!(all.is_dynamic());
        assert!(all.is_reflection_visible());
        assert!(all.is_casting_static_shadow());
        assert!(all.is_casting_dynamic_shadow());
        assert!(all.is_terrain());
        assert_eq!(all.storage(), 0b11111);
    }

    #[test]
    fn setters_clear_then_merge_each_bit() {
        let mut flags = SceneObjectFlags::new();
        flags.set_is_dynamic(true);
        flags.set_is_terrain(true);
        assert_eq!(flags.storage(), IS_DYNAMIC_OBJECT | IS_TERRAIN_OBJECT);

        // Setting an already-set bit keeps the storage identical.
        flags.set_is_dynamic(true);
        assert_eq!(flags.storage(), IS_DYNAMIC_OBJECT | IS_TERRAIN_OBJECT);

        // Clearing one flag leaves the rest (the `&= ~mask` arm).
        flags.set_is_dynamic(false);
        assert_eq!(flags.storage(), IS_TERRAIN_OBJECT);
        assert!(flags.is_terrain());
        assert!(!flags.is_dynamic());

        flags.set_is_terrain(false);
        assert_eq!(flags.storage(), 0);
    }
    #[test]
    fn remaining_setters_clear_then_merge_their_bits() {
        let mut flags = SceneObjectFlags::new();
        flags.set_is_reflection_visible(true);
        flags.set_is_casting_static_shadow(true);
        flags.set_is_casting_dynamic_shadow(true);
        assert_eq!(
            flags.storage(),
            IS_REFLECTION_VISIBLE | IS_CASTING_STATIC_SHADOW | IS_CASTING_DYNAMIC_SHADOW
        );
        assert!(flags.is_reflection_visible());
        assert!(flags.is_casting_static_shadow());
        assert!(flags.is_casting_dynamic_shadow());

        flags.set_is_reflection_visible(false);
        flags.set_is_casting_static_shadow(false);
        flags.set_is_casting_dynamic_shadow(false);
        assert_eq!(flags.storage(), 0);
    }
}
