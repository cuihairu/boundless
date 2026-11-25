//! BW-10-03: the collision faces — world triangles, BSP trees and portal
//! pass-through (`lib/physics2/worldtri.hpp`, `lib/physics2/bsp.cpp`,
//! `lib/chunk/portal_chunk_item.cpp`).

pub mod bsp;
pub mod closest_triangle;
pub mod portal;
pub mod quad_tree;
pub mod sweep_helpers;
pub mod world_polygon;
pub mod world_triangle;

pub use bsp::{
    BSP_FILE_CUR_TOKEN, BSP_FILE_CUR_VERSION, BSP_FILE_INVALID_VERSION, BSP_FILE_LEGACY_TOKEN,
    BSP_FILE_LEGACY_VERSION, BSP_FILE_MAGIC, BSP_NODE_PACKED_SIZE, BSPNODE_TOLERANCE, BspLoadError,
    BspNode, BspTree, CollisionVisitor, OFFSET_32BIT_NULL, PLANE_EQ_SIZE, bsp_version,
};
pub use closest_triangle::ClosestTriangle;
pub use portal::{
    COLLIDE_AFTER, COLLIDE_ALL, COLLIDE_BEFORE, COLLIDE_NOT_IMPLEMENTED, COLLIDE_STOP,
    CollisionCallback, CollisionInterface, CollisionObstacle, CollisionState, Portal,
    PortalObstacle, PortalTarget, SweepParams, SweepShape, SweptShape, biggest_axis_of_difference,
    collide_into_object,
};
pub use quad_tree::{
    INVALID_NODE, QUAD_BL, QUAD_BR, QUAD_TL, QUAD_TR, QtCoord, QtRange, QuadTree, QuadTreeNode,
};
pub use sweep_helpers::{BspSweepVisitor, SpaceCollider, collide_into_object_point};
pub use world_polygon::WorldPolygon;
pub use world_triangle::{
    BW_TRIANGLE_NOCOLLIDE, TRIANGLE_BLENDED, TRIANGLE_CAMERANOCOLLIDE, TRIANGLE_COLLISIONFLAG_MASK,
    TRIANGLE_DOUBLESIDED, TRIANGLE_MATERIALKIND_MASK, TRIANGLE_MATERIALKIND_SHIFT,
    TRIANGLE_NOCOLLIDE, TRIANGLE_NOT_IN_BSP, TRIANGLE_PROJECTILENOCOLLIDE, TRIANGLE_TERRAIN,
    TRIANGLE_TRANSPARENT, TRIANGLE_WATER, WORLD_TRIANGLE_SIZE, WorldTriangle,
};
