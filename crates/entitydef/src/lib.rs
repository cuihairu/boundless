//! BW-04 entity definitions and data contracts.
//!
//! A port of `lib/entitydef` (plus the small `lib/network` slices it needs):
//! definitions parse out of a BigWorld `res/scripts` tree into fully indexed
//! entity descriptions, and the digest over those descriptions pins the wire
//! contract between processes.

pub mod cli;
pub mod constants;
pub mod data_description;
pub mod data_type;
pub mod diagnostics;
pub mod digest;
pub mod dump;
pub mod entity_description;
pub mod entity_map;
pub mod flags;
pub mod lod;
pub mod member;
pub mod method;
pub mod property_change;
pub mod value;
pub mod volatile;

pub use entity_description::{
    DefResolver, DistributionDecider, EntityDescription, EntityTypeID, INVALID_ENTITY_TYPE_ID,
};
pub use entity_map::{EntityDescriptionMap, MessageRanges};
