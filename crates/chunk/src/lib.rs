//! BW-10-01 chunk identifiers, boundaries, portals and streaming bind.
//!
//! A port of `lib/chunk` (the server-relevant slices): chunk identifiers and
//! their grid coordinates, geometry mappings over a space directory, the
//! chunk load path and the boundary/portal graph that drives adjacent chunk
//! binding. [`chunk_editor`] carries the WorldEditor save path back onto the
//! same chunk face (`tools/worldeditor/world/editor_chunk_cache.cpp`).

pub mod boundary;
pub mod chunk;
pub mod chunk_editor;
pub mod identifier;
pub mod mapping;
pub mod space;

pub use boundary::{ChunkBoundary, Portal, PortalTarget, create_boundaries, form_boundaries};
pub use chunk::{Chunk, ChunkItem};
pub use chunk_editor::{
    ChunkLoader, ChunkSaveError, ChunkSaver, EditorChunkCache, UnsavedChunks,
    update_data_section_with_transform,
};
pub use identifier::{
    grid_from_chunk_name, is_outside_chunk, outside_chunk_id_to_grid, outside_chunk_identifier,
};
pub use mapping::{DEFAULT_GRID_SIZE, GeometryMapping, GridBounds};
pub use space::ChunkSpace;
