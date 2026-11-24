//! BW-13 asset pipeline: dependency discovery, conversion and compilation
//! (`tools/asset_pipeline`).
//!
//! Ported modules:
//! - [`asset_compiler`] — the concrete compiler and task queue
//!   (`compiler/asset_compiler.cpp`);
//! - [`asset_compiler_options`] — the command-line options that configure an
//!   [`asset_compiler`] (`compiler/asset_compiler_options.cpp`);
//! - [`batch_compiler`] — the command-line batch driver and its options
//!   (`tools/batch_compiler/batch_compiler.cpp`);
//! - [`hash64`] — the `Hash64` FNV-1a hash used for dependency and file
//!   hashes (`bw_hash64.cpp`);
//! - [`dependency`] — the six dependency kinds and their DataSection
//!   serialisation (`dependency/*.hpp|cpp`);
//! - [`dependency_list`] — the per-task input/output dependency list
//!   (`dependency_list.hpp|cpp`);
//! - [`conversion_task`] — the conversion record and its status machine
//!   (`conversion/conversion_task.hpp`);
//! - [`conversion_rule`] — the abstract rule that maps a source file to a
//!   task (`discovery/conversion_rule.hpp`);
//! - [`task_finder`] — the recursive root-task discovery walk
//!   (`discovery/task_finder.cpp`);
//! - [`content_addressable_cache`] — the hash-addressed file cache
//!   (`conversion/content_addressable_cache.cpp`);
//! - [`compiler`] — the compiler seam the pipeline drives
//!   (`compiler/compiler.hpp`);
//! - [`converter`] — the converter plugin interface
//!   (`conversion/converter.hpp`);
//! - [`converter_info`] — the converter registry record and flags
//!   (`conversion/converter_info.hpp|converter_creator.hpp`);
//! - [`converter_map`] — the id → converter registry
//!   (`conversion/converter_map.hpp`);
//! - [`hierarchical_config`] — the path-hierarchical rule set behind
//!   `asset_rules.xml` (`resmgr/hierarchical_config.cpp`);
//! - [`generic_conversion_rule`] — the `asset_rules.xml`-backed rule
//!   (`compiler/generic_conversion_rule.cpp`);
//! - [`task_processor`] — the single-threaded task driver
//!   (`conversion/task_processor.cpp`);
//! - [`jit_compiler`] — the on-demand compiler: reverse-dependency
//!   tracking, on-asset requests and modification re-queues
//!   (`tools/jit_compiler/jit_compiler.cpp`);
//! - [`plugin_loader`] — the plugin registry and loader that registers
//!   converters (`tools/plugin_system/plugin_loader.cpp`);
//! - [`task_store`] — the JIT compiler's task bookkeeping
//!   (`tools/jit_compiler/task_store.cpp`);
//! - [`vertex_format`] — vertex formats and the conversion matrix behind
//!   them (`moo/vertex_format.*`, `moo/vertex_format_conversions.*`);
//! - [`primitive_file_structs`] — the on-disk primitive headers
//!   (`moo/primitive_file_structs.hpp`);
//! - [`primitive_processor`] — the `.primitives` → `.processed` converter
//!   (`tools/asset_pipeline/converters/primitive_processor/`);
//! - [`space_converter`] — the space → `space.bin` converter and the
//!   compiled-space writer faces behind it
//!   (`tools/asset_pipeline/converters/space_converter/`,
//!   `lib/compiled_space/binary_writers/`);
//! - [`hierarchical_config_converter`] — the rule-tree baker
//!   (`tools/asset_pipeline/converters/hierarchical_config_converter/`);
//! - [`bsp_converter`] — the BSP collision-tree upgrader and its discovery
//!   rule (`tools/asset_pipeline/converters/bsp_converter/`);
//! - [`texture_detail_level`] — the per-texture conversion settings and the
//!   D3DFMT name table (`lib/moo/texture_detail_level*`,
//!   `format_name_map.cpp`);
//! - [`convert_texture_tool`] — the texture pixel-work seam and its
//!   device-free TGA→DDS face (`lib/moo/convert_texture_tool.*`);
//! - [`texture_converter`] — the texture → `.dds`/`.c.dds` converter
//!   (`tools/asset_pipeline/converters/texture_converter/`);
//! - [`texformat_converter`] — the `.texformat` normaliser
//!   (`tools/asset_pipeline/converters/texformat_converter/`);
//! - [`effect_converter`] — the `.fx` → `.fxo` converter behind the D3DX
//!   compile seam (`tools/asset_pipeline/converters/effect_converter/`);
//! - [`asset_processor`] — the `_AssetProcessor` tool functions behind the
//!   PyScript seam (`tools/assetprocessor/`);
//! - [`offline_processor`] — the offline tool's command surface, cluster
//!   sharding, chunk gathering, `.cdata` invalidation and its two
//!   commands (`tools/offline_processor/`, `lib/chunk/chunk_processor_manager.cpp`);
//! - [`res_packer`] — the release packer: the packer registry and its six
//!   packers, the `PackedSection::convert` face and both command-line
//!   modes (`tools/res_packer/`);
//! - [`resource_checker`] — the `VisualChecker` that judges a `.visual`
//!   against `visual_rules.xml` (size, triangle count, hierarchy depth,
//!   portals, hard points, texture names)
//!   (`tools/resourcechecker/visual_checker.*`).

pub mod asset_compiler;
pub mod asset_compiler_options;
pub mod asset_processor;
pub mod batch_compiler;
pub mod bsp_converter;
pub mod compiler;
pub mod content_addressable_cache;
pub mod conversion_rule;
pub mod conversion_task;
pub mod convert_texture_tool;
pub mod converter;
pub mod converter_info;
pub mod converter_map;
pub mod dependency;
pub mod dependency_list;
pub mod effect_converter;
pub mod generic_conversion_rule;
pub mod hash64;
pub mod hierarchical_config;
pub mod hierarchical_config_converter;
pub mod jit_compiler;
pub mod offline_processor;
pub mod plugin_loader;
pub mod primitive_file_structs;
pub mod primitive_processor;
pub mod res_packer;
pub mod resource_checker;
pub mod space_converter;
pub mod task_finder;
pub mod task_processor;
pub mod task_store;
pub mod texformat_converter;
pub mod texture_converter;
pub mod texture_detail_level;
pub mod vertex_format;
pub mod visual_processor;
