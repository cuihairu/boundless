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

pub use flood::{AdjGridElt, FloodChunk, FloodPhysics, FloodProgress, MAX_HEIGHTS, WaypointFlood};
