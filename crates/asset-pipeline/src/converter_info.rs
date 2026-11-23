//! `ConverterInfo`/`ConverterCreator` port
//! (`conversion/converter_info.hpp`, `conversion/converter_creator.hpp`): the
//! registry entry describing one converter and how to instantiate it.
//!
//! Reference quirks preserved:
//! - `typeId_` is unique per converter type and indexes the registry;
//! - `flags_` is the OR of the `CONVERTER_FLAGS` bits, defaulting to
//!   `THREAD_SAFE | CACHE_DEPENDENCIES | CACHE_CONVERSION`;
//! - `creator_` is a plain function pointer taking the params string (the
//!   reference's `ConverterCreator`, replaced here by a boxed `Converter`).

use crate::converter::Converter;

/// `ConverterCreator`: builds a converter instance from its params string.
pub type ConverterCreator = fn(params: &str) -> Box<dyn Converter>;

/// `ConverterInfo::CONVERTER_FLAGS`.
pub struct ConverterFlags;

impl ConverterFlags {
    pub const THREAD_SAFE: u32 = 1 << 0;
    pub const CACHE_DEPENDENCIES: u32 = 1 << 1;
    pub const CACHE_CONVERSION: u32 = 1 << 2;
    pub const UPGRADE_CONVERSION: u32 = 1 << 3;
    pub const DEFAULT_FLAGS: u32 =
        Self::THREAD_SAFE | Self::CACHE_DEPENDENCIES | Self::CACHE_CONVERSION;
}

/// `ConverterInfo`: the registry record for one converter.
#[derive(Clone)]
pub struct ConverterInfo {
    pub name: String,
    pub type_id: u64,
    pub version: String,
    pub flags: u32,
    pub creator: ConverterCreator,
}

impl ConverterInfo {
    pub fn new(
        name: impl Into<String>,
        type_id: u64,
        version: impl Into<String>,
        flags: u32,
        creator: ConverterCreator,
    ) -> Self {
        Self {
            name: name.into(),
            type_id,
            version: version.into(),
            flags,
            creator,
        }
    }

    /// `getFlags`-style helpers used by the processor.
    pub fn is_thread_safe(&self) -> bool {
        self.flags & ConverterFlags::THREAD_SAFE != 0
    }
    pub fn cache_dependencies(&self) -> bool {
        self.flags & ConverterFlags::CACHE_DEPENDENCIES != 0
    }
    pub fn cache_conversion(&self) -> bool {
        self.flags & ConverterFlags::CACHE_CONVERSION != 0
    }
    pub fn upgrade_conversion(&self) -> bool {
        self.flags & ConverterFlags::UPGRADE_CONVERSION != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default flags are thread-safe with dependency and conversion
    /// caching enabled.
    #[test]
    fn default_flags_match_reference() {
        assert_eq!(ConverterFlags::DEFAULT_FLAGS, 0b111);
        const {
            assert!(ConverterFlags::DEFAULT_FLAGS & ConverterFlags::THREAD_SAFE != 0);
            assert!(ConverterFlags::DEFAULT_FLAGS & ConverterFlags::CACHE_DEPENDENCIES != 0);
            assert!(ConverterFlags::DEFAULT_FLAGS & ConverterFlags::CACHE_CONVERSION != 0);
            assert!(ConverterFlags::DEFAULT_FLAGS & ConverterFlags::UPGRADE_CONVERSION == 0);
        }
    }

    /// The four flags occupy the low four bits (the reference's enum values).
    #[test]
    fn flag_values_match_reference() {
        assert_eq!(ConverterFlags::THREAD_SAFE, 1);
        assert_eq!(ConverterFlags::CACHE_DEPENDENCIES, 2);
        assert_eq!(ConverterFlags::CACHE_CONVERSION, 4);
        assert_eq!(ConverterFlags::UPGRADE_CONVERSION, 8);
    }

    /// `DEFAULT_FLAGS` also equals the `EXPERIMENTAL_MASK`-complemented set.
    #[test]
    fn experimental_mask_clears_cache_bits() {
        let experimental = !(ConverterFlags::CACHE_CONVERSION | ConverterFlags::CACHE_DEPENDENCIES);
        assert_eq!(
            ConverterFlags::DEFAULT_FLAGS & experimental,
            ConverterFlags::THREAD_SAFE
        );
    }
}
