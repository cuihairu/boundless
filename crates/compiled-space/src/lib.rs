//! BW-10-02-01 the compiled space binary format ("space.bin").
//!
//! A port of `lib/compiled_space` (the reader slices the server needs):
//!
//! - the `BWTB` container: header, section table and section streams
//!   (`binary_format_types.hpp` / `binary_format.cpp`),
//! - the `BWST` string table (`string_table_types.hpp` / `string_table.cpp`),
//! - the `BWAL` asset list (`asset_list_types.hpp` / `asset_list.cpp`),
//! - the `BWCS` compiled space settings (`compiled_space_settings_types.hpp` /
//!   `compiled_space_settings.cpp`).
//!
//! Reference quirks are ported on purpose and cited at each site: the writer
//! never fills `Header::file_size` (so the reader never consults it), the
//! asset list entry has three alignment-padding bytes the writer leaves
//! uninitialized (so the reader neither checks nor zeroes them), and
//! `findAndOpenSection`'s wrong-version message prints its expected/got
//! arguments in swapped order.

pub mod asset_list;
pub mod binary_format;
pub mod loose_octree;
pub mod settings;
pub mod static_scene;
pub mod static_scene_model;
pub mod string_table;

pub use asset_list::{
    ASSET_TYPE_DATASECTION, ASSET_TYPE_EFFECT, ASSET_TYPE_MODEL, ASSET_TYPE_PRIMITIVE,
    ASSET_TYPE_TEXTURE, ASSET_TYPE_UNKNOWN, ASSET_TYPE_VISUAL, AssetList, AssetType,
    ENTRY_SIZE as ASSET_LIST_ENTRY_SIZE, Entry as AssetListEntry,
};
pub use binary_format::{
    BinaryFormat, FORMAT_MAGIC, FORMAT_VERSION, Header, MAX_SECTIONS, OpenError, SectionHeader,
    Stream,
};
pub use loose_octree::{
    CONFIGURATION_SIZE as OCTREE_CONFIGURATION_SIZE, Configuration as OctreeConfiguration,
    INDEX_MASK, INVALID_DATA_REFERENCE, INVALID_NODE, LEAF_REF_FLAG, NODE_BOUND_DATA_SIZE,
    NODE_CENTER_SIZE, NODE_SIZE, NUM_CHILDREN, Node as OctreeNode, NodeDataReference, ROOT_NODE,
    StaticLooseOctree, child_index_from_reference, is_leaf_ref,
};
pub use settings::CompiledSpaceSettings;
pub use static_scene::{
    DATA_SPAN_SIZE, DataSpan, FORMAT_MAGIC as STATIC_SCENE_FORMAT_MAGIC,
    FORMAT_VERSION as STATIC_SCENE_FORMAT_VERSION, STATIC_OBJECT_STATIC_MODEL, StaticScene,
    TYPE_HEADER_SIZE, TypeHeader,
};
pub use static_scene_model::{
    FLAG_CASTS_SHADOW, FLAG_IS_SHELL, FLAG_REFLECTION_VISIBLE,
    FORMAT_MAGIC as STATIC_SCENE_MODEL_FORMAT_MAGIC,
    FORMAT_VERSION as STATIC_SCENE_MODEL_FORMAT_VERSION, INDEX_RANGE_SIZE, IndexRange, MODEL_SIZE,
    Model, StaticSceneModelData,
};
pub use string_table::{Entry as StringTableEntry, INVALID_INDEX, StringTable};
