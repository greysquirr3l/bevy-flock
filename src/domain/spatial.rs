//! A deterministic uniform grid for neighbour queries.
//!
//! Why hand-written rather than a crate: the crowd code queries neighbours tens
//! of thousands of times per frame, the query results feed simulation state, and
//! simulation state must be reproducible. A `HashMap`-backed index answers the
//! query just as fast and then destroys determinism, because iteration order
//! depends on hasher state that differs between runs and platforms.
//!
//! The design is a dense uniform grid with **a hard budget on the number of
//! cells**. That budget is the whole story, and it came from measurement, not
//! taste:
//!
//! - A textbook dense grid over the default 1 km volume with 4 m cells is 15.6
//!   million cells. Materialising it cost 60 MB and a 15 million step prefix sum
//!   *every tick*, which measured at 37 ms per frame for a crowd of **one**.
//! - A sorted-array grid (no per-cell storage at all) fixed that: 4.6 µs for one
//!   NPC. It then lost badly at scale — 78 ms at 10,000 agents, 953 ms at
//!   100,000 — because an 8 m query radius with 4 m cells touches 125 cells, and
//!   a binary search per cell is far more expensive than an array index.
//!
//! So: dense cells, because queries are the hot path, and a cell budget of
//! 2^20, because the fixed cost is what made the first version unusable. When a
//! volume would exceed the budget the cell size is coarsened until it fits. Query
//! results are identical either way — cell size only changes cost — and the
//! coarsening is reported through [`SpatialGrid::effective_cell_size`].
//!
//! Queries return results ordered by `(distance, NpcId)`, so two runs of the same
//! simulation see the same neighbours in the same order.

use bevy_math::Vec3;

use crate::domain::ids::NpcId;
use crate::domain::num;
use crate::error::ConfigError;

/// The most cells a grid will materialise.
///
/// The number is measured, not guessed. Rebuild cost is linear in cells and is
/// paid every tick, so a budget that is too generous shows up as a fixed cost on
/// every crowd, and one that is too tight shows up as more candidates per cell.
///
/// Both were measured:
///
/// | budget | 1 NPC | 1,000 | 10,000 | 100,000 |
/// | --- | --- | --- | --- | --- |
/// | 2^20 | 1.00 ms | 2.47 ms | 18.2 ms | 192 ms |
/// | 2^18 | 0.30 ms | 2.11 ms | 23.1 ms | 276 ms |
///
/// 2^18 is three times better for a crowd of one and 27% worse at the population
/// this crate actually targets. 2^20 is the choice, and the guidance that follows
/// from it is in the docs: size the volume to the playable area, not to the map.
pub const DEFAULT_MAX_CELLS: u64 = 1 << 20;

/// Construction parameters for a [`SpatialGrid`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpatialGridConfig {
    /// Minimum corner of the simulated volume.
    pub min: Vec3,
    /// Maximum corner of the simulated volume.
    pub max: Vec3,
    /// Requested edge length of one cubic cell.
    ///
    /// A *minimum*: if the volume would need more than [`SpatialGridConfig::max_cells`] cells at
    /// this size, the grid coarsens until it fits.
    pub cell_size: f32,
    /// The most cells the grid may allocate.
    ///
    /// Defaults to [`DEFAULT_MAX_CELLS`], which was measured against the rebuild cost (ADR 0003).
    /// This is the lever for ADR 0031: a finer grid gives the steering kernel more agents per
    /// bucket, at the price of more cells to clear and rebuild every tick. Raising it is a trade a
    /// game can make per volume — size the volume to the playable area before raising this.
    pub max_cells: u32,
}

impl SpatialGridConfig {
    /// Build and validate a configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::InvalidDistance`] when the cell size is not finite
    /// and positive, and the same error when the bounds are empty or inverted.
    pub fn try_new(min: Vec3, max: Vec3, cell_size: f32) -> Result<Self, ConfigError> {
        if !cell_size.is_finite() || cell_size <= 0.0 {
            return Err(ConfigError::InvalidDistance {
                field: "cell_size",
                value: cell_size,
            });
        }
        let extent = max - min;
        if !extent.is_finite() || extent.min_element() <= 0.0 {
            return Err(ConfigError::InvalidDistance {
                field: "bounds extent",
                value: extent.min_element(),
            });
        }
        Ok(Self {
            min,
            max,
            cell_size,
            max_cells: num::u64_to_u32(DEFAULT_MAX_CELLS),
        })
    }

    /// Raise or lower the cell budget.
    ///
    /// The grid coarsens to fit this many cells, so a larger budget is what lets a finer
    /// [`SpatialGridConfig::cell_size`] survive. ADR 0031 measured 2^21 as the point where a
    /// one-kilometre volume keeps 9 m cells.
    #[must_use]
    pub const fn with_cell_budget(mut self, max_cells: u32) -> Self {
        self.max_cells = max_cells;
        self
    }

    /// Dimensions this configuration needs at a given cell size.
    fn dims_for(&self, cell_size: f32) -> [u32; 3] {
        let extent = self.max - self.min;
        let per_axis = extent / cell_size;
        [
            num::f32_to_u32(per_axis.x.ceil().max(1.0)),
            num::f32_to_u32(per_axis.y.ceil().max(1.0)),
            num::f32_to_u32(per_axis.z.ceil().max(1.0)),
        ]
    }
}

impl Default for SpatialGridConfig {
    /// A 1 km cube with 4 m cells: a village, a stadium or a city block.
    fn default() -> Self {
        Self {
            min: Vec3::splat(-500.0),
            max: Vec3::splat(500.0),
            cell_size: 4.0,
            max_cells: num::u64_to_u32(DEFAULT_MAX_CELLS),
        }
    }
}

/// One agent as the grid sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridEntry {
    /// Stable identity, used as the tie-break so query order is deterministic.
    pub id: NpcId,
    /// World position at the moment the grid was built.
    pub position: Vec3,
    /// World velocity at the moment the grid was built.
    pub velocity: Vec3,
}

/// A neighbour returned by a query.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Neighbour {
    /// Identity of the neighbour.
    pub id: NpcId,
    /// Its position.
    pub position: Vec3,
    /// Its velocity.
    pub velocity: Vec3,
    /// Squared distance from the query centre. Squared because every consumer
    /// compares distances and none of them needs the square root.
    pub distance_squared: f32,
}

/// A uniform grid over a fixed volume.
///
/// The grid is rebuilt from current positions each tick. Rebuilding is cheaper
/// than incremental updates at crowd densities, and it removes a whole class of
/// "stale entry" bug.
#[derive(Debug, Clone)]
pub struct SpatialGrid {
    config: SpatialGridConfig,
    cell_size: f32,
    dims: [u32; 3],
    /// Prefix sums: `cell_start[c]..cell_start[c + 1]` is cell `c`'s window in
    /// `order`. Length is `cell_count() + 1`.
    cell_start: Vec<u32>,
    /// Indices into `entries`, grouped by cell.
    order: Vec<u32>,
    /// Insertion-ordered agents.
    entries: Vec<GridEntry>,
    /// Reused fill cursor so rebuilding does not allocate.
    cursor: Vec<u32>,
}

impl SpatialGrid {
    /// Build an empty grid.
    ///
    /// # Errors
    ///
    /// Propagates [`SpatialGridConfig::try_new`] validation. A volume whose cell
    /// ids would exceed `u32::MAX` even after coarsening is also rejected.
    pub fn new(config: SpatialGridConfig) -> Result<Self, ConfigError> {
        let mut cell_size = config.cell_size;
        let mut dims = config.dims_for(cell_size);
        let budget = u64::from(config.max_cells.max(1));
        // Coarsen until the grid fits the budget. Each step multiplies the cell
        // size, so the loop terminates at a single cell in a handful of rounds.
        while cell_count(dims) > budget {
            cell_size *= 1.5;
            if !cell_size.is_finite() {
                return Err(ConfigError::InvalidDistance {
                    field: "cell_size after coarsening",
                    value: cell_size,
                });
            }
            dims = config.dims_for(cell_size);
        }
        let total = cell_count(dims);
        if total == 0 || total > u64::from(u32::MAX) {
            return Err(ConfigError::InvalidDistance {
                field: "cell count",
                value: cell_size,
            });
        }
        let cells = num::u64_to_usize(total);
        Ok(Self {
            config,
            cell_size,
            dims,
            cell_start: vec![0; cells + 1],
            order: Vec::new(),
            entries: Vec::new(),
            cursor: vec![0; cells],
        })
    }

    /// A one-cell degenerate grid.
    ///
    /// Exists so types that hold a grid have no panic path. A grid built from a
    /// validated [`SpatialGridConfig`] cannot fail, so this is only reachable
    /// from a hand-constructed config; it still answers queries correctly.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            config: SpatialGridConfig {
                min: Vec3::splat(-1.0),
                max: Vec3::splat(1.0),
                cell_size: 2.0,
                max_cells: 1,
            },
            cell_size: 2.0,
            dims: [1, 1, 1],
            cell_start: vec![0, 0],
            order: Vec::new(),
            entries: Vec::new(),
            cursor: vec![0],
        }
    }

    /// The configuration this grid was asked for.
    #[must_use]
    pub const fn config(&self) -> SpatialGridConfig {
        self.config
    }

    /// The cell size actually in use.
    ///
    /// Equal to `config().cell_size` unless the volume needed more than
    /// [`MAX_CELLS`] cells, in which case the grid coarsened to stay inside the
    /// budget.
    #[must_use]
    pub const fn effective_cell_size(&self) -> f32 {
        self.cell_size
    }

    /// Number of cells along each axis.
    #[must_use]
    pub const fn dims(&self) -> [u32; 3] {
        self.dims
    }

    /// Total number of cells, never more than [`MAX_CELLS`].
    #[must_use]
    pub fn cell_count(&self) -> usize {
        num::u64_to_usize(cell_count(self.dims))
    }

    /// Number of agents currently in the grid.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the grid is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clear the grid, keeping its allocations.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    /// The agents in insertion order.
    #[must_use]
    pub fn entries(&self) -> &[GridEntry] {
        &self.entries
    }

    /// Insert an agent.
    ///
    /// Insertion order is the order [`SpatialGrid::entries`] reports, and it is
    /// reproduced exactly by a replay that spawns in the same order.
    pub fn insert(&mut self, entry: GridEntry) {
        self.entries.push(entry);
    }

    /// Insert every agent from an iterator of `(id, position, velocity)`.
    pub fn extend_from<'a, I>(&mut self, items: I)
    where
        I: IntoIterator<Item = (NpcId, Vec3, Vec3)> + 'a,
    {
        for (id, position, velocity) in items {
            self.insert(GridEntry {
                id,
                position,
                velocity,
            });
        }
    }

    /// Cell index containing `position`, clamped into the grid.
    ///
    /// Positions outside the configured volume are clamped rather than rejected:
    /// an NPC that walks off the edge of a navmesh should still be findable by
    /// anything querying near the boundary, and silently dropping it would make
    /// it invisible to its own flock.
    #[must_use]
    pub fn cell_of(&self, position: Vec3) -> u32 {
        let local = (position - self.config.min) / self.cell_size;
        let x = clamp_axis(local.x, self.dims[0]);
        let y = clamp_axis(local.y, self.dims[1]);
        let z = clamp_axis(local.z, self.dims[2]);
        (z * self.dims[1] + y) * self.dims[0] + x
    }

    /// Centre of a cell.
    #[must_use]
    pub fn cell_center(&self, cell: u32) -> Vec3 {
        let x = cell % self.dims[0];
        let y = (cell / self.dims[0]) % self.dims[1];
        let z = cell / (self.dims[0] * self.dims[1]);
        let offset = Vec3::new(num::u32_to_f32(x), num::u32_to_f32(y), num::u32_to_f32(z))
            + Vec3::splat(0.5);
        self.config.min + offset * self.cell_size
    }

    /// Group the entries by cell. Call once per tick, after all inserts.
    ///
    /// Cost is `O(cells + n)`: one counting pass over agents, one prefix sum over
    /// the (budgeted) cell array, one placement pass. No sorting, no allocation.
    pub fn build(&mut self) {
        let cells = self.cell_count();
        self.cell_start.fill(0);
        for entry in &self.entries {
            let cell = self.cell_of(entry.position) as usize;
            if let Some(slot) = self.cell_start.get_mut(cell + 1) {
                *slot += 1;
            }
        }
        for index in 0..cells {
            let previous = self.cell_start.get(index).copied().unwrap_or(0);
            if let Some(current) = self.cell_start.get_mut(index + 1) {
                *current += previous;
            }
        }

        self.order.clear();
        self.order.resize(self.entries.len(), 0);
        self.cursor.clear();
        self.cursor
            .extend_from_slice(self.cell_start.get(..cells).unwrap_or(&[]));

        for (index, entry) in self.entries.iter().enumerate() {
            let cell = self.cell_of(entry.position) as usize;
            let target = match self.cursor.get(cell).copied() {
                Some(target) => target as usize,
                None => continue,
            };
            if let Some(slot) = self.order.get_mut(target) {
                *slot = num::usize_to_u32(index);
            }
            if let Some(cursor) = self.cursor.get_mut(cell) {
                *cursor += 1;
            }
        }
    }

    /// The entry indices in one cell.
    fn cell_window(&self, cell: u32) -> &[u32] {
        let start = self.cell_start.get(cell as usize).copied().unwrap_or(0) as usize;
        let end = self.cell_start.get(cell as usize + 1).copied().unwrap_or(0) as usize;
        self.order.get(start..end).unwrap_or(&[])
    }

    /// Append every agent within `radius` of `centre`, ordered by distance then
    /// identifier.
    ///
    /// `out` is cleared first, so a caller can reuse one buffer for every query
    /// and never allocate in a frame.
    pub fn radius(&self, centre: Vec3, radius: f32, out: &mut Vec<Neighbour>) {
        out.clear();
        if radius <= 0.0 || !radius.is_finite() {
            return;
        }
        let radius_squared = radius * radius;
        let span = num::f32_to_i64((radius / self.cell_size).ceil());
        let bounds = self.cell_bounds(centre, span);

        for z in bounds.min_z..=bounds.max_z {
            for y in bounds.min_y..=bounds.max_y {
                for x in bounds.min_x..=bounds.max_x {
                    let cell = num::i64_to_u32(
                        (z * i64::from(self.dims[1]) + y) * i64::from(self.dims[0]) + x,
                    );
                    for index in self.cell_window(cell) {
                        let Some(entry) = self.entries.get(*index as usize) else {
                            continue;
                        };
                        let distance_squared = entry.position.distance_squared(centre);
                        if distance_squared <= radius_squared {
                            out.push(Neighbour {
                                id: entry.id,
                                position: entry.position,
                                velocity: entry.velocity,
                                distance_squared,
                            });
                        }
                    }
                }
            }
        }

        out.sort_unstable_by(|left, right| {
            left.distance_squared
                .total_cmp(&right.distance_squared)
                .then_with(|| left.id.cmp(&right.id))
        });
    }

    /// Append the `k` nearest agents, ordered by distance then identifier.
    ///
    /// The search starts one cell wide and doubles until it has `k` candidates
    /// or covers the whole grid, so a sparse query is cheap and a dense one is
    /// still correct.
    pub fn nearest(&self, centre: Vec3, k: usize, out: &mut Vec<Neighbour>) {
        out.clear();
        if k == 0 || self.is_empty() {
            return;
        }
        let mut span = 1_i64;
        let max_span = i64::from(self.dims.iter().copied().max().unwrap_or(1));
        loop {
            let bounds = self.cell_bounds(centre, span);
            out.clear();
            for z in bounds.min_z..=bounds.max_z {
                for y in bounds.min_y..=bounds.max_y {
                    for x in bounds.min_x..=bounds.max_x {
                        if x < 0
                            || y < 0
                            || z < 0
                            || x >= i64::from(self.dims[0])
                            || y >= i64::from(self.dims[1])
                            || z >= i64::from(self.dims[2])
                        {
                            continue;
                        }
                        let cell = num::i64_to_u32(
                            (z * i64::from(self.dims[1]) + y) * i64::from(self.dims[0]) + x,
                        );
                        for index in self.cell_window(cell) {
                            let Some(entry) = self.entries.get(*index as usize) else {
                                continue;
                            };
                            out.push(Neighbour {
                                id: entry.id,
                                position: entry.position,
                                velocity: entry.velocity,
                                distance_squared: entry.position.distance_squared(centre),
                            });
                        }
                    }
                }
            }
            if out.len() >= k || span >= max_span {
                break;
            }
            span *= 2;
        }

        out.sort_unstable_by(|left, right| {
            left.distance_squared
                .total_cmp(&right.distance_squared)
                .then_with(|| left.id.cmp(&right.id))
        });
        out.truncate(k);
    }

    /// Inclusive cell bounds for a query, clipped to the grid.
    fn cell_bounds(&self, centre: Vec3, span: i64) -> CellBounds {
        let local = (centre - self.config.min) / self.cell_size;
        // `floor`, not `ceil`: the centre cell must be the cell `cell_of` would
        // report, or the window sits one cell to the side — and on a negative
        // coordinate that window is clipped away entirely.
        let cx = num::f32_to_i64(local.x.floor());
        let cy = num::f32_to_i64(local.y.floor());
        let cz = num::f32_to_i64(local.z.floor());
        let clamp_axis = |value: i64, axis: usize| {
            let max = i64::from(self.dims.get(axis).copied().unwrap_or(1)) - 1;
            value.clamp(0, max)
        };
        CellBounds {
            min_x: clamp_axis(cx - span, 0),
            min_y: clamp_axis(cy - span, 1),
            min_z: clamp_axis(cz - span, 2),
            max_x: clamp_axis(cx + span, 0),
            max_y: clamp_axis(cy + span, 1),
            max_z: clamp_axis(cz + span, 2),
        }
    }
}

/// Inclusive cell bounds for a query.
struct CellBounds {
    min_x: i64,
    min_y: i64,
    min_z: i64,
    max_x: i64,
    max_y: i64,
    max_z: i64,
}

/// Total number of cells in an id space.
fn cell_count(dims: [u32; 3]) -> u64 {
    u64::from(dims[0]) * u64::from(dims[1]) * u64::from(dims[2])
}

/// Clamp a local coordinate to a cell index on one axis.
fn clamp_axis(value: f32, cells: u32) -> u32 {
    let max = cells.saturating_sub(1);
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    let floored = num::f32_to_u32(value);
    floored.min(max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::OrDie;

    #[test]
    fn a_larger_budget_keeps_a_finer_cell() {
        // ADR 0031: the default budget coarsens a 1 km volume to 13.5 m because 9 m needs 1.4M
        // cells. Raising the budget is what lets the finer cell survive.
        let coarse =
            SpatialGridConfig::try_new(Vec3::splat(-500.0), Vec3::splat(500.0), 9.0).or_die();
        let grid = SpatialGrid::new(coarse).or_die();
        assert!(
            (grid.effective_cell_size() - 13.5).abs() < 0.01,
            "the default budget should coarsen 9 m to 13.5 m, got {}",
            grid.effective_cell_size()
        );

        let fine = coarse.with_cell_budget(1 << 21);
        let grid = SpatialGrid::new(fine).or_die();
        assert!(
            (grid.effective_cell_size() - 9.0).abs() < 0.01,
            "2^21 cells should keep 9 m, got {}",
            grid.effective_cell_size()
        );
    }

    #[test]
    fn a_small_budget_coarsens_instead_of_failing() {
        let config = SpatialGridConfig::default().with_cell_budget(64);
        let grid = SpatialGrid::new(config).or_die();
        let dims = grid.dims();
        assert!(u64::from(dims[0]) * u64::from(dims[1]) * u64::from(dims[2]) <= 64);
    }

    use crate::domain::ids::SimSeed;
    use crate::domain::rng::RngStream;

    fn grid() -> SpatialGrid {
        SpatialGrid::new(SpatialGridConfig::default()).or_die()
    }

    fn brute_force(entries: &[GridEntry], centre: Vec3, radius: f32) -> Vec<Neighbour> {
        let radius_squared = radius * radius;
        let mut found: Vec<Neighbour> = entries
            .iter()
            .filter_map(|entry| {
                let distance_squared = entry.position.distance_squared(centre);
                (distance_squared <= radius_squared).then_some(Neighbour {
                    id: entry.id,
                    position: entry.position,
                    velocity: entry.velocity,
                    distance_squared,
                })
            })
            .collect();
        found.sort_unstable_by(|left, right| {
            left.distance_squared
                .total_cmp(&right.distance_squared)
                .then_with(|| left.id.cmp(&right.id))
        });
        found
    }

    #[test]
    fn radius_query_matches_brute_force_including_order() {
        let mut rng = RngStream::new(SimSeed::new(0xA11CE), 1);
        let mut grid = grid();
        for index in 0..200_u32 {
            let position = Vec3::new(
                rng.next_range_f32(-100.0, 100.0),
                rng.next_range_f32(-100.0, 100.0),
                rng.next_range_f32(-100.0, 100.0),
            );
            grid.insert(GridEntry {
                id: NpcId::unique(index),
                position,
                velocity: Vec3::ZERO,
            });
        }
        grid.build();

        let mut out = Vec::new();
        for _ in 0..50 {
            let centre = Vec3::new(
                rng.next_range_f32(-100.0, 100.0),
                rng.next_range_f32(-100.0, 100.0),
                rng.next_range_f32(-100.0, 100.0),
            );
            let radius = rng.next_range_f32(1.0, 40.0);
            grid.radius(centre, radius, &mut out);
            let expected = brute_force(grid.entries(), centre, radius);
            assert_eq!(out.len(), expected.len(), "radius {radius} at {centre}");
            for (found, want) in out.iter().zip(expected.iter()) {
                assert_eq!(found.id, want.id);
                assert!(
                    (found.distance_squared - want.distance_squared).abs() < 1e-3,
                    "{} vs {}",
                    found.distance_squared,
                    want.distance_squared
                );
            }
        }
    }

    #[test]
    fn nearest_matches_brute_force_prefix() {
        let mut rng = RngStream::new(SimSeed::new(0xBEE5), 2);
        let mut grid = grid();
        for index in 0..500_u32 {
            grid.insert(GridEntry {
                id: NpcId::unique(index),
                position: Vec3::new(
                    rng.next_range_f32(-50.0, 50.0),
                    rng.next_range_f32(-50.0, 50.0),
                    rng.next_range_f32(-50.0, 50.0),
                ),
                velocity: Vec3::ZERO,
            });
        }
        grid.build();

        let mut out = Vec::new();
        let mut expected = Vec::new();
        let centre = Vec3::new(3.0, -7.0, 11.0);
        grid.nearest(centre, 12, &mut out);
        grid.radius(centre, 500.0, &mut expected);
        assert_eq!(out.len(), 12);
        for (found, want) in out.iter().zip(expected.iter()) {
            assert_eq!(found.id, want.id);
        }
    }

    #[test]
    fn agents_on_a_cell_border_are_found_exactly_once() {
        let mut grid = grid();
        let cell = grid.effective_cell_size();
        grid.insert(GridEntry {
            id: NpcId::unique(1),
            position: Vec3::new(0.0, 0.0, 0.0),
            velocity: Vec3::ZERO,
        });
        grid.insert(GridEntry {
            id: NpcId::unique(2),
            position: Vec3::new(cell, cell, cell),
            velocity: Vec3::ZERO,
        });
        grid.build();

        let mut out = Vec::new();
        grid.radius(Vec3::new(0.0, 0.0, 0.0), cell * 2.0, &mut out);
        assert_eq!(out.len(), 2);
        let mut ids: Vec<u32> = out.iter().map(|n| n.id.index()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn empty_and_single_agent_grids_behave() {
        let mut grid = grid();
        grid.build();
        let mut out = Vec::new();
        grid.radius(Vec3::ZERO, 10.0, &mut out);
        assert_eq!(out.len(), 0);
        grid.nearest(Vec3::ZERO, 5, &mut out);
        assert_eq!(out.len(), 0);

        grid.insert(GridEntry {
            id: NpcId::unique(0),
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
        });
        grid.build();
        grid.nearest(Vec3::ZERO, 5, &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn positions_outside_the_volume_are_clamped_not_dropped() {
        let mut grid = grid();
        grid.insert(GridEntry {
            id: NpcId::unique(1),
            position: Vec3::splat(10_000.0),
            velocity: Vec3::ZERO,
        });
        grid.build();
        let mut out = Vec::new();
        grid.radius(Vec3::splat(10_000.0), 1.0, &mut out);
        assert_eq!(out.len(), 1, "an out-of-bounds agent stays visible");
    }

    #[test]
    fn rebuilding_reuses_allocations() {
        let mut grid = grid();
        for index in 0..1_000_u32 {
            grid.insert(GridEntry {
                id: NpcId::unique(index),
                position: Vec3::splat(num::u32_to_f32(index)),
                velocity: Vec3::ZERO,
            });
        }
        grid.build();
        grid.clear();
        for index in 0..1_000_u32 {
            grid.insert(GridEntry {
                id: NpcId::unique(index),
                position: Vec3::splat(num::u32_to_f32(index)),
                velocity: Vec3::ZERO,
            });
        }
        grid.build();
        assert_eq!(grid.len(), 1_000);
    }

    #[test]
    fn a_huge_volume_is_coarsened_instead_of_materialised() {
        // The regression that made the first version unusable: a 10 km cube with
        // 2 m cells is 125 billion cells. The grid must coarsen to fit its budget
        // rather than trying to allocate the volume.
        let config =
            SpatialGridConfig::try_new(Vec3::splat(-5_000.0), Vec3::splat(5_000.0), 2.0).or_die();
        let grid = SpatialGrid::new(config).or_die();
        assert!(grid.cell_count() as u64 <= DEFAULT_MAX_CELLS);
        assert!(
            grid.effective_cell_size() > config.cell_size,
            "the cell size must have coarsened"
        );

        let mut grid = grid;
        for index in 0..3_u32 {
            grid.insert(GridEntry {
                id: NpcId::unique(index),
                position: Vec3::new(num::u32_to_f32(index) * 10.0, 0.0, 0.0),
                velocity: Vec3::ZERO,
            });
        }
        grid.build();
        let mut out = Vec::new();
        grid.radius(Vec3::ZERO, 25.0, &mut out);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn the_default_volume_fits_the_cell_budget() {
        let grid = grid();
        assert!(grid.cell_count() as u64 <= DEFAULT_MAX_CELLS);
    }

    #[test]
    fn invalid_configurations_are_rejected() {
        let min = Vec3::splat(-10.0);
        let max = Vec3::splat(10.0);
        assert!(SpatialGridConfig::try_new(min, max, 0.0).is_err());
        assert!(SpatialGridConfig::try_new(min, max, f32::NAN).is_err());
        assert!(SpatialGridConfig::try_new(max, min, 1.0).is_err());
        assert!(SpatialGridConfig::try_new(min, max, 1.0).is_ok());
    }

    #[test]
    fn cell_center_is_inside_its_cell() {
        let grid = grid();
        for cell in [0_u32, 5, 100, 1_000] {
            let centre = grid.cell_center(cell);
            assert_eq!(grid.cell_of(centre), cell);
        }
    }

    #[test]
    fn build_is_stable_when_positions_do_not_change() {
        let mut rng = RngStream::new(SimSeed::new(5), 5);
        let mut grid = grid();
        for index in 0..300_u32 {
            grid.insert(GridEntry {
                id: NpcId::unique(index),
                position: Vec3::new(
                    rng.next_range_f32(-20.0, 20.0),
                    0.0,
                    rng.next_range_f32(-20.0, 20.0),
                ),
                velocity: Vec3::ZERO,
            });
        }
        grid.build();
        let mut first = Vec::new();
        grid.radius(Vec3::ZERO, 30.0, &mut first);

        grid.build();
        let mut second = Vec::new();
        grid.radius(Vec3::ZERO, 30.0, &mut second);

        assert_eq!(first.len(), second.len());
        for (a, b) in first.iter().zip(second.iter()) {
            assert_eq!(a.id, b.id, "identical input must give identical order");
        }
    }
}
