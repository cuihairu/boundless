//! BW-10-04 waypoint navmesh runtime (`lib/waypoint/`): the old-graph
//! (version 0 `worldNavmesh`) decode and the waypoint adjacency faces.
//!
//! The reference split lives across `chunk_waypoint.hpp/cpp` (the waypoint
//! polygon, its edge adjacency encoding and the centre calculation) and
//! `chunk_waypoint_set_data.hpp/cpp` (the per-set storage plus
//! `readWaypointSet`, the two-cursor binary decode). The chunk-loading
//! wrapper around them (`navmeshFactory`'s population registry and
//! `ChunkWaypointSet`) is the chunk lane's face and stays out of this port.

mod astar;
mod chunk_waypoint;
mod chunk_waypoint_set;
mod chunk_waypoint_set_data;
mod chunk_waypoint_set_state;
mod chunk_waypoint_state;
mod girth;
mod navloc;

pub use astar::{AStar, AStarState};
pub use chunk_waypoint::{
    ADJACENT_RANGE_END, ADJACENT_RANGE_START, ChunkWaypoint, ChunkWaypointVertexProvider, Edge,
    HEIGHT_RANGE_TOLERANCE, OTHER_CHUNK,
};
pub use chunk_waypoint_set::{EdgeLabel, Portal, WaypointNeighbourIterator};
pub use chunk_waypoint_set_data::{
    ChunkWaypointSetData, NAVPOLY_ELEMENT_SIZE, NavmeshDecodeError, parse_world_navmesh,
};
pub use chunk_waypoint_set_state::ChunkWPSetState;
pub use chunk_waypoint_state::{ChunkWaypointState, ChunkWaypointStateSearchData};
pub use girth::{GIRTH_EPSILON, NavigatorFindResult, almost_equal, find_by_girth};
pub use navloc::NavLoc;

/// `EdgeIndex` (`chunk_waypoint.hpp:25`).
pub type EdgeIndex = u16;
/// `WaypointIndex` (`chunk_waypoint.hpp:26`).
pub type WaypointIndex = i32;
