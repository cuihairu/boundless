//! BW-10-05 waypoint generation, following the reference in
//! `lib/waypoint_generator/`.
//!
//! The flood stage (`WaypointFlood`) samples a collision scene on a
//! regular grid: each grid point keeps up to [`MAX_HEIGHTS`] height
//! layers, and every layer carries an 8-direction adjacency map whose
//! 4-bit entries name the height layer of the walkable neighbour in that
//! direction. The physics and chunk seams the reference consumes
//! (`IPhysics`, `Chunk::owns` + space bounds) become [`FloodPhysics`] and
//! [`FloodChunk`].

pub mod flood;
pub mod generator;
pub mod view;

pub use flood::{AdjGridElt, FloodChunk, FloodPhysics, FloodProgress, MAX_HEIGHTS, WaypointFlood};
pub use generator::WaypointGenerator;
pub use view::{
    ADJ_ANOTHER_CHUNK, NAV_POLY_EDGE_ELT_SIZE, NAV_POLY_ELT_SIZE, NAV_POLY_SET_ELT_SIZE, NavVertex,
    NavView, as_binary, save_out_records,
};
