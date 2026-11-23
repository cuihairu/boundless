//! BW-10-01-01 geometry mappings: the chunk directory bounds of one space.
//!
//! A port of `lib/chunk/geometry_mapping.cpp` (`GeometryMapping`) plus the
//! settings it reads from the space's `settings.xml`. The mapping's grid
//! bounds decide which outside chunks exist, and `gather_chunks` enumerates
//! the same chunk set the reference does: every cell of the local grid rect
//! plus every `*i.chunk` file found by scanning the directory tree.

use std::collections::BTreeSet;
use std::path::Path;

use resources::datasection::DataSection;

use crate::identifier::outside_chunk_identifier;

/// Reference `DEFAULT_GRID_RESOLUTION` (`base_chunk_space.hpp`).
pub const DEFAULT_GRID_SIZE: f32 = 100.0;

/// The local grid rectangle a mapping owns (`bounds` in `settings.xml`),
/// plus the derived world grid bounds after the mapping transform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridBounds {
    pub min_l_grid_x: i32,
    pub min_l_grid_y: i32,
    pub max_l_grid_x: i32,
    pub max_l_grid_y: i32,
    pub min_grid_x: i32,
    pub min_grid_y: i32,
    pub max_grid_x: i32,
    pub max_grid_y: i32,
}

/// `GeometryMapping` for one space directory: its path, grid size, bounds
/// and the `singleDir` flag that drops the hashed subdirectory prefix.
#[derive(Debug, Clone, PartialEq)]
pub struct GeometryMapping {
    pub path: String,
    pub grid_size: f32,
    pub bounds: GridBounds,
    pub single_dir: bool,
}

impl GeometryMapping {
    /// `GeometryMapping::GeometryMapping` (`geometry_mapping.cpp:156-236`)
    /// reads `chunkSize`, `bounds/*` and `singleDir` from `settings.xml`.
    /// For a pure-identity mapping the world grid bounds equal the local
    /// ones; `bounds_to_grid` applies the mapping transform when it is not
    /// the identity, which is where a rotated mapping's bounds differ.
    pub fn new(
        path: impl Into<String>,
        grid_size: f32,
        bounds: GridBounds,
        single_dir: bool,
    ) -> Self {
        Self {
            path: path.into(),
            grid_size,
            bounds,
            single_dir,
        }
    }

    /// `GeometryMapping::GeometryMapping` (`geometry_mapping.cpp:63-101`) +
    /// `getGridSize` (`geometry_mapping.cpp:486-488`): build the mapping from
    /// a space's `settings.xml` section, which the loader opens first. The
    /// world grid bounds equal the local bounds for the identity mapping;
    /// transformed mappings are BW-10-02+ scope.
    pub fn from_settings(path: impl Into<String>, settings: &DataSection) -> Self {
        let read_int = |p: &str| -> i32 {
            settings
                .open_section(p)
                .map_or(0, |section| section.as_int(0))
        };
        let grid_size = settings
            .open_section("chunkSize")
            .map_or(DEFAULT_GRID_SIZE, |section| {
                section.as_float(DEFAULT_GRID_SIZE)
            });
        let bounds = GridBounds {
            min_l_grid_x: read_int("bounds/minX"),
            min_l_grid_y: read_int("bounds/minY"),
            max_l_grid_x: read_int("bounds/maxX"),
            max_l_grid_y: read_int("bounds/maxY"),
            min_grid_x: read_int("bounds/minX"),
            min_grid_y: read_int("bounds/minY"),
            max_grid_x: read_int("bounds/maxX"),
            max_grid_y: read_int("bounds/maxY"),
        };
        let single_dir = settings
            .open_section("singleDir")
            .is_some_and(|section| section.as_bool(false));
        Self {
            path: path.into(),
            grid_size,
            bounds,
            single_dir,
        }
    }

    /// `GeometryMapping::inLocalBounds` (`geometry_mapping.cpp:294-298`):
    /// a cell is owned by this mapping when it lies in the local rect.
    pub fn in_local_bounds(&self, grid_x: i32, grid_z: i32) -> bool {
        (self.bounds.min_l_grid_x..=self.bounds.max_l_grid_x).contains(&grid_x)
            && (self.bounds.min_l_grid_y..=self.bounds.max_l_grid_y).contains(&grid_z)
    }

    /// `GeometryMapping::inWorldBounds` (`geometry_mapping.cpp:304-309`):
    /// the world rect is what the streaming scan consults, since the scan
    /// works in the space's coordinate system.
    pub fn in_world_bounds(&self, grid_x: i32, grid_z: i32) -> bool {
        (self.bounds.min_grid_x..=self.bounds.max_grid_x).contains(&grid_x)
            && (self.bounds.min_grid_y..=self.bounds.max_grid_y).contains(&grid_z)
    }

    /// `GeometryMapping::outsideChunkIdentifier` (`geometry_mapping.cpp:344-353`):
    /// the identifier for the outside chunk containing the cell, or the empty
    /// string when the cell is outside the local bounds and the caller asked
    /// for the check.
    pub fn outside_chunk_identifier(&self, grid_x: i32, grid_z: i32, check_bounds: bool) -> String {
        if check_bounds && !self.in_local_bounds(grid_x, grid_z) {
            return String::new();
        }
        outside_chunk_identifier(grid_x, grid_z, self.single_dir)
    }

    /// `GeometryMapping::gatherChunks` (`geometry_mapping.cpp:470-484`): the
    /// set of every outside cell in the grid rect plus the internal chunk
    /// files found anywhere in the directory tree. Internal chunks are named
    /// `<xxxxxxxx>i.chunk` anywhere below the space root, and their
    /// identifier is the subdirectory prefix plus that name.
    pub fn gather_chunks(&self, root: &Path) -> BTreeSet<String> {
        let mut chunks = BTreeSet::new();
        for grid_z in self.bounds.min_grid_y..=self.bounds.max_grid_y {
            for grid_x in self.bounds.min_grid_x..=self.bounds.max_grid_x {
                chunks.insert(outside_chunk_identifier(grid_x, grid_z, self.single_dir));
            }
        }
        gather_internal_chunks(root, Path::new(""), &mut chunks);
        chunks
    }
}

/// `Util::gatherInternalChunks` (`geometry_mapping.cpp:422-447`): recurse
/// the directory tree collecting `*i.chunk` names as identifiers. A file
/// whose name is 15 characters and ends in `i.chunk` is an inside chunk
/// (`xxxxxxxxi.chunk`); the identifier is its subdirectory path plus name.
fn gather_internal_chunks(root: &Path, prefix: &Path, out: &mut BTreeSet<String>) {
    let Ok(dir) = std::fs::read_dir(root.join(prefix)) else {
        return;
    };
    let mut entries: Vec<_> = dir.flatten().map(|e| e.file_name()).collect();
    entries.sort();
    for entry in entries {
        let name = entry.to_string_lossy().into_owned();
        if name.len() == 15 && name.ends_with("i.chunk") {
            let mut identifier = prefix.to_string_lossy().into_owned();
            identifier.push_str(&name[..9]);
            out.insert(identifier);
            continue;
        }
        if name.len() == 15 && name.ends_with(".chunk") {
            continue;
        }
        let child = root.join(prefix).join(&name);
        if child.is_dir() {
            let child_prefix = Path::new(&name);
            let nested = if prefix.as_os_str().is_empty() {
                child_prefix.to_string_lossy().into_owned()
            } else {
                format!(
                    "{}/{}",
                    prefix.to_string_lossy(),
                    child_prefix.to_string_lossy()
                )
            };
            gather_internal_chunks(root, Path::new(&nested), out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(min_x: i32, min_z: i32, max_x: i32, max_z: i32) -> GeometryMapping {
        let bounds = GridBounds {
            min_l_grid_x: min_x,
            min_l_grid_y: min_z,
            max_l_grid_x: max_x,
            max_l_grid_y: max_z,
            min_grid_x: min_x,
            min_grid_y: min_z,
            max_grid_x: max_x,
            max_grid_y: max_z,
        };
        GeometryMapping::new("space_00010001/", DEFAULT_GRID_SIZE, bounds, false)
    }

    #[test]
    fn local_bounds_own_a_rect() {
        let m = mapping(-1, -1, 1, 1);
        assert!(m.in_local_bounds(-1, -1));
        assert!(m.in_local_bounds(1, 1));
        assert!(!m.in_local_bounds(-2, 0));
        assert!(!m.in_local_bounds(0, 2));
    }

    #[test]
    fn identifier_checks_bounds_on_request() {
        let m = mapping(0, 0, 1, 1);
        assert_eq!(m.outside_chunk_identifier(1, 1, true), "00010001o");
        assert_eq!(m.outside_chunk_identifier(2, 0, true), "");
        // check_bounds=false yields the identifier regardless of ownership.
        assert_eq!(m.outside_chunk_identifier(2, 0, false), "00020000o");
    }
}
