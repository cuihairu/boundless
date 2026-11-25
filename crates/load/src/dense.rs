//! The dense AOI scenario: pack the requested bots on a tight grid and
//! record the density and per-bot visible counts as the run's expectations.
//!
//! The load side owns the layout and the record; the AOI arbitration itself
//! is the cell seat's (BW-08). Visibility here is the recorded expectation a
//! live run checks the server's enter/leave feeds against: two targets see
//! each other when their distance is within the AOI radius, counted on the
//! packed grid. The layout is a pure function of the bot count and spacing,
//! so the recorded expectations reproduce bit for bit.

use crate::scenario::Scenario;

/// The dense AOI run's record: the packed layout and what each bot should
/// see.
#[derive(Debug, Clone, PartialEq)]
pub struct DenseAoiLedger {
    /// Grid spacing between neighbouring spawn points.
    pub spacing: f32,
    /// The AOI radius the visible counts were recorded under.
    pub aoi_radius: f32,
    /// Spawned targets per unit area of the packed grid.
    pub density: f32,
    /// Spawn positions `(x, z)` in grid order.
    pub positions: Vec<(f32, f32)>,
    /// Visible targets per bot, in grid order (self excluded).
    pub visible_counts: Vec<u32>,
}

/// Lay out `count` bots on a square grid with the given spacing and record
/// the density and per-bot visible counts under the AOI radius.
pub fn run_dense_aoi(scenario: &Scenario, spacing: f32, aoi_radius: f32) -> DenseAoiLedger {
    let count = scenario.concurrency.max(1) as usize;
    let side = (count as f64).sqrt().ceil() as usize;
    let extent = side as f32 * spacing;

    let mut positions = Vec::with_capacity(count);
    for index in 0..count {
        let x = (index % side) as f32 * spacing;
        let z = (index / side) as f32 * spacing;
        positions.push((x, z));
    }

    let radius_sq = aoi_radius * aoi_radius;
    let visible_counts = positions
        .iter()
        .map(|&(x, z)| {
            positions
                .iter()
                .filter(|&&(ox, oz)| {
                    let dx = ox - x;
                    let dz = oz - z;
                    dx * dx + dz * dz <= radius_sq
                })
                .count() as u32
                - 1 // self
        })
        .collect();

    DenseAoiLedger {
        spacing,
        aoi_radius,
        density: count as f32 / (extent * extent),
        positions,
        visible_counts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::AccountPool;
    use std::time::Duration;

    fn scenario(concurrency: u64) -> Scenario {
        Scenario {
            name: "dense-aoi".to_string(),
            fixed_seed: 20,
            account_pool: AccountPool::Generated {
                prefix: "Bot".to_string(),
                count: concurrency.max(1),
                password: String::new(),
            },
            concurrency,
            ramp_rate: 0,
            warmup: Duration::from_secs(1),
            duration: Duration::from_secs(9),
        }
    }

    /// The grid packs the requested count at the recorded density: a 3×3
    /// grid at unit spacing holds 9 targets over 9 square units.
    #[test]
    fn the_grid_packs_the_requested_density() {
        let ledger = run_dense_aoi(&scenario(9), 1.0, 20.0);
        assert_eq!(ledger.positions.len(), 9);
        assert!((ledger.density - 1.0).abs() < 1e-5);
        assert_eq!(ledger.positions[0], (0.0, 0.0));
        assert_eq!(ledger.positions[1], (1.0, 0.0));
        assert_eq!(ledger.positions[3], (0.0, 1.0));
        assert_eq!(ledger.positions[8], (2.0, 2.0));
    }

    /// The visible counts follow the AOI radius: on the 3×3 unit grid with
    /// radius 1.5, corners see 3, edges 5, the centre 8.
    #[test]
    fn the_visible_counts_follow_the_aoi_radius() {
        let ledger = run_dense_aoi(&scenario(9), 1.0, 1.5);
        assert_eq!(ledger.visible_counts, vec![3, 5, 3, 5, 8, 5, 3, 5, 3]);
    }

    /// A sparse packing empties the view: double the spacing pushes every
    /// neighbour past the radius, and the density drops four-fold.
    #[test]
    fn a_sparse_packing_empties_the_view() {
        let dense = run_dense_aoi(&scenario(9), 1.0, 1.5);
        let sparse = run_dense_aoi(&scenario(9), 2.0, 1.5);
        assert!(sparse.visible_counts.iter().all(|&c| c == 0));
        assert!((sparse.density - dense.density / 4.0).abs() < 1e-5);
    }

    /// The layout is a pure function of the inputs: equal runs record equal
    /// ledgers, and a non-square count still lands on the same grid.
    #[test]
    fn the_layout_is_repeatable_for_any_count() {
        let nine = scenario(9);
        assert_eq!(
            run_dense_aoi(&nine, 1.0, 1.5),
            run_dense_aoi(&nine, 1.0, 1.5)
        );
        let five = run_dense_aoi(&scenario(5), 1.0, 20.0);
        assert_eq!(five.positions.len(), 5);
        assert_eq!(five.visible_counts, vec![4, 4, 4, 4, 4]);
        // 5 targets on a 3×3 grid: 5 over 9 square units.
        assert!((five.density - 5.0 / 9.0).abs() < 1e-5);
    }
}
