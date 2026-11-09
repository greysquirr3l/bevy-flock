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
#[cfg(feature = "threads")]
use std::thread;

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

/// Bodies below this stay on the serial path.
///
/// A rebuild of a few thousand bodies takes a few hundred microseconds, which is the same order as
/// spawning a handful of threads; below this threshold the parallel path is slower and noisier.
const PARALLEL_MIN_BODIES: usize = 4_096;

/// Queries below this stay on the serial path, for the same reason: a batch of a few dozen radius
/// queries is tens of microseconds and a scope costs more than that.
const PARALLEL_MIN_QUERIES: usize = 64;

/// Default slots per query in a [`QueryArena`].
pub const DEFAULT_NEIGHBOUR_CAP: usize = 32;

/// How many workers a grid uses when nobody says otherwise.
///
/// ADR 0054 makes the count configuration rather than behaviour: every count produces the same cells in
/// the same order, so defaulting to the machine's parallelism is safe. A single-core target, or a build
/// without the `threads` feature, gets one.
fn default_threads() -> usize {
    if cfg!(feature = "threads") {
        return std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    }
    1
}

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

/// A flat, reusable result set for many neighbour queries at once.
///
/// Each query owns exactly [`QueryArena::cap`] contiguous slots, so query `q` writes
/// `q * cap .. (q + 1) * cap` and nothing else. That fixed stride is the whole design: the output
/// offset of every query is known before any work starts, so a batch can be partitioned by query
/// range with no gather step, no per-query `Vec`, and no ordering rule that a thread count could
/// violate.
///
/// Resizing happens once, in [`SpatialGrid::radius_batch`], against `queries * cap`; after that a
/// steady frame reuses the same two buffers.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryArena {
    /// `queries * cap` neighbour slots, in query order.
    slots: Vec<Neighbour>,
    /// How many slots each query filled.
    counts: Vec<u32>,
    /// Slots per query.
    cap: usize,
}

impl Default for QueryArena {
    fn default() -> Self {
        Self::new(DEFAULT_NEIGHBOUR_CAP)
    }
}

impl QueryArena {
    /// An empty arena holding up to `cap` neighbours per query.
    #[must_use]
    pub const fn new(cap: usize) -> Self {
        Self {
            slots: Vec::new(),
            counts: Vec::new(),
            cap,
        }
    }

    /// Slots reserved per query.
    #[must_use]
    pub const fn cap(&self) -> usize {
        self.cap
    }

    /// How many queries the last batch answered.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.counts.len()
    }

    /// Whether the last batch answered no queries.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }

    /// The neighbour counts of the last batch, one per query.
    #[must_use]
    pub fn counts(&self) -> &[u32] {
        &self.counts
    }

    /// How many neighbours query `query` has, or zero if the batch did not reach it.
    #[must_use]
    pub fn count(&self, query: usize) -> u32 {
        self.counts.get(query).copied().unwrap_or(0)
    }

    /// The neighbours of query `query`, nearest first, ties broken by identifier.
    ///
    /// Slots past [`QueryArena::count`] are stale from an earlier batch and are not returned.
    #[must_use]
    pub fn neighbours(&self, query: usize) -> &[Neighbour] {
        let filled = match self.counts.get(query).copied() {
            Some(filled) => filled as usize,
            None => return &[],
        };
        let Some(start) = query.checked_mul(self.cap) else {
            return &[];
        };
        let Some(end) = start.checked_add(filled.min(self.cap)) else {
            return &[];
        };
        self.slots.get(start..end).unwrap_or(&[])
    }

    /// Size the buffers for `queries` queries. Allocates only when they must grow.
    fn begin(&mut self, queries: usize) {
        let slots = queries.saturating_mul(self.cap);
        self.slots.clear();
        self.slots.resize(slots, EMPTY_NEIGHBOUR);
        self.counts.clear();
        self.counts.resize(queries, 0);
    }
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
    /// How many threads a rebuild may use. See [`SpatialGrid::with_threads`].
    threads: usize,
    /// The cell each entry landed in, remembered between the counting and placement passes.
    ///
    /// `cell_of` is the only float work in a rebuild and it was being done twice per body: once to
    /// count, once to place. At a million bodies that was 18 of the 46 ms. Four bytes per body buys it
    /// back, and it is the same buffer every tick, so a steady frame does not allocate for it.
    cells: Vec<u32>,
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
            threads: default_threads(),
            cells: Vec::new(),
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
            threads: 1,
            cells: Vec::new(),
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
        cell_of_parts(self.config.min, self.cell_size, self.dims, position)
    }

    /// How many threads a rebuild may use.
    #[must_use]
    pub const fn threads(&self) -> usize {
        self.threads
    }

    /// Set how many threads a rebuild may use.
    ///
    /// This is configuration, not behaviour: the resulting cell windows and the order within them are
    /// identical at every count (ADR 0054). One means the serial path, which is what a single-core
    /// target gets, and what a build without the `threads` feature gets regardless of what is set here.
    #[must_use]
    pub const fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads;
        self
    }

    /// Threads a rebuild will actually use for this grid's size.
    fn worker_count(&self) -> usize {
        if !cfg!(feature = "threads") || self.entries.len() < PARALLEL_MIN_BODIES {
            return 1;
        }
        self.threads.max(1).min(self.cell_count().max(1))
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
    /// Cost is `O(cells + n)` on one thread: one pass computing each body's cell, one counting pass,
    /// one prefix sum over the (budgeted) cell array, one placement pass. No sorting, and the four
    /// buffers are reused every tick.
    ///
    /// The buffers are not the whole story on the parallel path: `std::thread::scope` boxes one work
    /// packet per worker, so a parallel rebuild makes a handful of allocator calls per tick (22 for six
    /// workers on this machine, measured). A frame that must not touch the allocator at all should call
    /// [`SpatialGrid::with_threads`] with `1`, or keep a worker pool alive across ticks, which is
    /// recorded as an open follow-up in plan.phase3.toml.
    ///
    /// With [`SpatialGrid::with_threads`] above one, the three `O(n)` passes are partitioned across
    /// workers and the prefix sum stays serial. The result is byte-identical at every thread count,
    /// which is the whole point of ADR 0054: the build partitions by *cell range* rather than by arrival
    /// order, so no body's placement depends on which worker got there first.
    pub fn build(&mut self) {
        let workers = self.worker_count();
        if workers <= 1 {
            self.build_serial();
        } else {
            self.build_parallel(workers);
        }
    }

    /// The single-threaded rebuild, and the reference every parallel result is compared against.
    fn build_serial(&mut self) {
        self.fill_cells_serial();
        self.count_cells_serial();
        self.prefix_cells();
        self.place_cells_serial();
        self.restore_cell_start();
    }

    /// Compute each body's cell. `cell_of` is the only float work in a rebuild, and it is done once per
    /// body into a reused buffer so the placement pass reads four bytes instead of recomputing.
    fn fill_cells_serial(&mut self) {
        let (min, cell_size, dims) = (self.config.min, self.cell_size, self.dims);
        let Self { cells, entries, .. } = self;
        cells.clear();
        cells.resize(entries.len(), 0);
        for (slot, entry) in cells.iter_mut().zip(entries.iter()) {
            *slot = cell_of_parts(min, cell_size, dims, entry.position);
        }
    }

    /// Count how many bodies land in each cell, in `cell_start[cell + 1]`.
    fn count_cells_serial(&mut self) {
        self.cell_start.fill(0);
        for cell in &self.cells {
            if let Some(slot) = self.cell_start.get_mut(*cell as usize + 1) {
                *slot += 1;
            }
        }
    }

    /// Turn counts into prefix sums, so `cell_start[cell]` is where that cell's window begins.
    fn prefix_cells(&mut self) {
        let cells = self.cell_count();
        for index in 0..cells {
            let previous = self.cell_start.get(index).copied().unwrap_or(0);
            if let Some(current) = self.cell_start.get_mut(index + 1) {
                *current += previous;
            }
        }
    }

    /// Place body indices into `order`, using `cell_start` itself as the fill cursor.
    ///
    /// Afterwards `cell_start[cell]` holds where cell `cell + 1` begins, which
    /// [`SpatialGrid::restore_cell_start`] turns back into the array a query reads.
    fn place_cells_serial(&mut self) {
        self.order.clear();
        self.order.resize(self.entries.len(), 0);
        for (index, cell) in self.cells.iter().enumerate() {
            let cell = *cell as usize;
            let target = match self.cell_start.get(cell).copied() {
                Some(target) => target as usize,
                None => continue,
            };
            if let Some(slot) = self.order.get_mut(target) {
                *slot = num::usize_to_u32(index);
            }
            if let Some(cursor) = self.cell_start.get_mut(cell) {
                *cursor += 1;
            }
        }
    }

    /// Shift the advanced cursors back by one cell, descending because each step reads the value it is
    /// about to overwrite. One sequential pass replaces a copy of the whole array, and it is the same
    /// pass at any thread count, so it stays serial.
    fn restore_cell_start(&mut self) {
        let cells = self.cell_count();
        for index in (1..=cells).rev() {
            let previous = self.cell_start.get(index - 1).copied().unwrap_or(0);
            if let Some(slot) = self.cell_start.get_mut(index) {
                *slot = previous;
            }
        }
        if let Some(first) = self.cell_start.get_mut(0) {
            *first = 0;
        }
    }

    /// The partitioned rebuild.
    ///
    /// Passes one and three are partitioned by *body* and by *cell range* respectively; pass two is
    /// partitioned by cell range, which means every worker reads the whole cell list. That is
    /// `O(workers * n)` sequential reads against `O(n)` writes, and it is the price of not keeping a
    /// per-worker histogram: at a million bodies and six workers it is 24 MB of reads that are mostly
    /// in cache, versus six histograms of 2.3 MB each to allocate, clear and merge.
    #[cfg(feature = "threads")]
    fn build_parallel(&mut self, workers: usize) {
        self.fill_cells_parallel(workers);
        self.count_cells_parallel(workers);
        self.prefix_cells();
        self.place_cells_parallel(workers);
        self.restore_cell_start();
    }

    /// Pass one, partitioned by body: each worker writes its own slice of the cell list.
    #[cfg(feature = "threads")]
    fn fill_cells_parallel(&mut self, workers: usize) {
        let (min, cell_size, dims) = (self.config.min, self.cell_size, self.dims);
        let Self { cells, entries, .. } = self;
        cells.clear();
        cells.resize(entries.len(), 0);
        let chunk = entries.len().div_ceil(workers);
        if chunk == 0 {
            return;
        }
        thread::scope(|scope| {
            for (slots, slice) in cells.chunks_mut(chunk).zip(entries.chunks(chunk)) {
                scope.spawn(move || {
                    for (slot, entry) in slots.iter_mut().zip(slice) {
                        *slot = cell_of_parts(min, cell_size, dims, entry.position);
                    }
                });
            }
        });
    }

    /// Pass two, partitioned by cell range.
    ///
    /// A worker owns cells `[lo, hi)`, zeroes exactly those counters and adds only the bodies that
    /// belong to them, so the counters need no merge afterwards — which is also why there is no
    /// per-worker histogram to allocate, clear or reconcile.
    ///
    /// Every worker reads the whole cell list: `O(workers * n)` sequential reads against `O(n)` writes.
    /// At a million bodies and six workers that is 24 MB of mostly-cached reads, cheaper than six
    /// histograms of 2.3 MB each.
    #[cfg(feature = "threads")]
    fn count_cells_parallel(&mut self, workers: usize) {
        let cells_in_grid = self.cell_count();
        let Self {
            cell_start, cells, ..
        } = self;
        let cell_list: &[u32] = cells;
        let per = cells_in_grid.div_ceil(workers);
        thread::scope(|scope| {
            let mut rest: &mut [u32] = cell_start.as_mut_slice();
            let mut at = 0_usize;
            for index in 0..workers {
                let lo = index * per;
                let hi = ((index + 1) * per).min(cells_in_grid);
                if lo >= hi {
                    continue;
                }
                let Some(head) = (lo + 1).checked_sub(at) else {
                    continue;
                };
                let current = std::mem::take(&mut rest);
                let Some((_, tail)) = current.split_at_mut_checked(head) else {
                    continue;
                };
                let Some((window, tail_rest)) = tail.split_at_mut_checked(hi - lo) else {
                    continue;
                };
                rest = tail_rest;
                at = hi + 1;
                scope.spawn(move || {
                    window.fill(0);
                    for cell in cell_list {
                        let cell = *cell as usize;
                        if cell < lo || cell >= hi {
                            continue;
                        }
                        if let Some(slot) = window.get_mut(cell - lo) {
                            *slot += 1;
                        }
                    }
                });
            }
        });
    }

    /// Pass three, partitioned by cell range.
    ///
    /// A range's bodies occupy a contiguous run of `order` — that is what the prefix sum bought — so
    /// each worker writes a disjoint slice and no two workers ever alias. The run's boundaries are read
    /// from the cursor array before it is split, at a point where the values at and beyond `lo` are
    /// still untouched by anyone.
    #[cfg(feature = "threads")]
    fn place_cells_parallel(&mut self, workers: usize) {
        let cells_in_grid = self.cell_count();
        let bodies = self.entries.len();
        let Self {
            cell_start,
            order,
            cells,
            ..
        } = self;
        let cell_list: &[u32] = cells;
        order.clear();
        order.resize(bodies, 0);
        let per = cells_in_grid.div_ceil(workers);
        thread::scope(|scope| {
            let mut cursor_rest: &mut [u32] = cell_start.as_mut_slice();
            let mut order_rest: &mut [u32] = order.as_mut_slice();
            let mut cursor_at = 0_usize;
            let mut order_at = 0_usize;
            for index in 0..workers {
                let lo = index * per;
                let hi = ((index + 1) * per).min(cells_in_grid);
                if lo >= hi {
                    continue;
                }
                let begin = cursor_rest.first().copied().unwrap_or(0) as usize;
                let end = cursor_rest.get(hi - lo).copied().unwrap_or(0) as usize;
                let remaining = std::mem::take(&mut cursor_rest);
                let runs_remaining = std::mem::take(&mut order_rest);
                let (Some(head), Some(run)) = (
                    lo.checked_sub(cursor_at),
                    begin.checked_sub(order_at).filter(|_| end >= begin),
                ) else {
                    continue;
                };
                let Some((_, tail)) = remaining.split_at_mut_checked(head) else {
                    continue;
                };
                let Some((cursor, tail_rest)) = tail.split_at_mut_checked(hi - lo) else {
                    continue;
                };
                cursor_rest = tail_rest;
                cursor_at = hi;
                let Some((_, runs)) = runs_remaining.split_at_mut_checked(run) else {
                    continue;
                };
                let Some((out, runs_rest)) = runs.split_at_mut_checked(end - begin) else {
                    continue;
                };
                order_rest = runs_rest;
                order_at = end;
                scope.spawn(move || {
                    for (body, cell) in cell_list.iter().enumerate() {
                        let cell = *cell as usize;
                        if cell < lo || cell >= hi {
                            continue;
                        }
                        let Some(slot) = cursor.get_mut(cell - lo) else {
                            continue;
                        };
                        let target = *slot as usize;
                        *slot += 1;
                        if let Some(out) = target
                            .checked_sub(begin)
                            .and_then(|local| out.get_mut(local))
                        {
                            *out = num::usize_to_u32(body);
                        }
                    }
                });
            }
        });
    }

    /// Without the `threads` feature there is nothing to partition onto.
    #[cfg(not(feature = "threads"))]
    fn build_parallel(&mut self, _workers: usize) {
        self.build_serial();
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

    /// Answer the same radius query for many centres at once, in one reusable buffer.
    ///
    /// This is the shape a crowd needs: every agent asks one question per tick. Each query owns
    /// exactly [`QueryArena::cap`] slots, so a worker filling its own range of queries writes its own
    /// slice of the arena and the result is identical at every thread count — the same contract the
    /// build keeps (ADR 0054).
    ///
    /// A query that finds more than `cap` neighbours keeps the nearest `cap`, ordered exactly as
    /// [`SpatialGrid::radius`] would order them, so truncation is a prefix of the unbounded answer
    /// rather than an arbitrary subset.
    pub fn radius_batch(&self, centres: &[Vec3], radius: f32, arena: &mut QueryArena) {
        arena.begin(centres.len());
        if radius <= 0.0 || !radius.is_finite() || centres.is_empty() {
            return;
        }
        let radius_squared = radius * radius;
        let span = num::f32_to_i64((radius / self.cell_size).ceil());
        let workers = self.batch_workers(centres.len());
        if workers <= 1 {
            self.radius_batch_serial(centres, radius_squared, span, arena);
            return;
        }
        self.radius_batch_parallel(centres, radius_squared, span, arena, workers);
    }

    /// Every query on one thread.
    fn radius_batch_serial(
        &self,
        centres: &[Vec3],
        radius_squared: f32,
        span: i64,
        arena: &mut QueryArena,
    ) {
        let cap = arena.cap;
        for (query, centre) in centres.iter().enumerate() {
            let Some(window) = query
                .checked_mul(cap)
                .and_then(|start| Some(start..start.checked_add(cap)?))
                .and_then(|range| arena.slots.get_mut(range))
            else {
                continue;
            };
            let filled = fill_batch_query(self, *centre, radius_squared, span, window);
            if let Some(count) = arena.counts.get_mut(query) {
                *count = filled;
            }
        }
    }

    /// Every query, partitioned by query range.
    ///
    /// Chunking by query (never by cell) is what keeps the output offsets fixed: a worker's queries
    /// occupy a contiguous run of the arena, so the slices it writes are disjoint by construction and
    /// the arena needs no lock, no atomic cursor and no second pass.
    #[cfg(feature = "threads")]
    fn radius_batch_parallel(
        &self,
        centres: &[Vec3],
        radius_squared: f32,
        span: i64,
        arena: &mut QueryArena,
        workers: usize,
    ) {
        let cap = arena.cap;
        if cap == 0 || centres.is_empty() {
            return;
        }
        let per = centres.len().div_ceil(workers);
        let grid = self;
        let QueryArena { slots, counts, .. } = arena;
        thread::scope(|scope| {
            let chunks = centres
                .chunks(per)
                .zip(slots.chunks_mut(per * cap).zip(counts.chunks_mut(per)));
            for (queries, (slot_chunk, count_chunk)) in chunks {
                scope.spawn(move || {
                    for (offset, centre) in queries.iter().enumerate() {
                        let Some(window) = offset
                            .checked_mul(cap)
                            .and_then(|start| Some(start..start.checked_add(cap)?))
                            .and_then(|range| slot_chunk.get_mut(range))
                        else {
                            continue;
                        };
                        let filled = fill_batch_query(grid, *centre, radius_squared, span, window);
                        if let Some(count) = count_chunk.get_mut(offset) {
                            *count = filled;
                        }
                    }
                });
            }
        });
    }

    /// Without the `threads` feature there is nothing to partition onto.
    #[cfg(not(feature = "threads"))]
    fn radius_batch_parallel(
        &self,
        centres: &[Vec3],
        radius_squared: f32,
        span: i64,
        arena: &mut QueryArena,
        _workers: usize,
    ) {
        self.radius_batch_serial(centres, radius_squared, span, arena);
    }

    /// Queries a batch of this size may spread across.
    fn batch_workers(&self, queries: usize) -> usize {
        if !cfg!(feature = "threads") || queries < PARALLEL_MIN_QUERIES {
            return 1;
        }
        self.threads.max(1).min(queries)
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
/// A slot that has never been filled.
///
/// The arena is sized by resizing rather than by growing per query, which needs some value to fill
/// with. This one is never observable: [`QueryArena::neighbours`] slices to the count each query
/// wrote, so no caller can read a slot that was not filled by this batch.
const EMPTY_NEIGHBOUR: Neighbour = Neighbour {
    id: NpcId::unique(0),
    position: Vec3::ZERO,
    velocity: Vec3::ZERO,
    distance_squared: f32::INFINITY,
};

/// Fill one query's slots with the nearest `slots.len()` agents within `radius_squared`.
///
/// Returns how many slots were filled. The bounded set is maintained without a heap: a candidate
/// either lands in a free slot or replaces the current worst, and the worst is only recomputed when
/// a replacement happens — which becomes rarer as the bound tightens. The final sort orders by
/// distance then identifier, the same rule the unbounded [`SpatialGrid::radius`] uses, so truncating
/// its answer to `cap` yields exactly this.
fn fill_batch_query(
    grid: &SpatialGrid,
    centre: Vec3,
    radius_squared: f32,
    span: i64,
    slots: &mut [Neighbour],
) -> u32 {
    let cap = slots.len();
    if cap == 0 {
        return 0;
    }
    let mut filled = 0_usize;
    let mut worst = 0_usize;
    let bounds = grid.cell_bounds(centre, span);
    for z in bounds.min_z..=bounds.max_z {
        for y in bounds.min_y..=bounds.max_y {
            for x in bounds.min_x..=bounds.max_x {
                let cell = num::i64_to_u32(
                    (z * i64::from(grid.dims[1]) + y) * i64::from(grid.dims[0]) + x,
                );
                for index in grid.cell_window(cell) {
                    let Some(entry) = grid.entries.get(*index as usize) else {
                        continue;
                    };
                    let distance_squared = entry.position.distance_squared(centre);
                    if distance_squared > radius_squared {
                        continue;
                    }
                    let candidate = Neighbour {
                        id: entry.id,
                        position: entry.position,
                        velocity: entry.velocity,
                        distance_squared,
                    };
                    if filled < cap {
                        if let Some(slot) = slots.get_mut(filled) {
                            *slot = candidate;
                        }
                        filled += 1;
                        // The new slot is the worst one if it is farther than the current worst;
                        // `worst` still points at a previous element here, never at itself.
                        if let Some(current) = slots.get(worst)
                            && !nearer(&candidate, current)
                        {
                            worst = filled - 1;
                        }
                        continue;
                    }
                    let Some(current_worst) = slots.get(worst).copied() else {
                        continue;
                    };
                    if !nearer(&candidate, &current_worst) {
                        continue;
                    }
                    if let Some(slot) = slots.get_mut(worst) {
                        *slot = candidate;
                    }
                    worst = slots
                        .iter()
                        .enumerate()
                        .max_by(|left, right| {
                            left.1
                                .distance_squared
                                .total_cmp(&right.1.distance_squared)
                                .then_with(|| left.1.id.cmp(&right.1.id))
                        })
                        .map_or(0, |(index, _)| index);
                }
            }
        }
    }
    if let Some(window) = slots.get_mut(..filled) {
        window.sort_unstable_by(|left, right| {
            left.distance_squared
                .total_cmp(&right.distance_squared)
                .then_with(|| left.id.cmp(&right.id))
        });
    }
    num::usize_to_u32(filled)
}

/// Whether `candidate` should displace `current` in a bounded set: nearer first, then lower identity.
fn nearer(candidate: &Neighbour, current: &Neighbour) -> bool {
    candidate
        .distance_squared
        .total_cmp(&current.distance_squared)
        .then_with(|| candidate.id.cmp(&current.id))
        .is_lt()
}

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
fn cell_of_parts(min: Vec3, cell_size: f32, dims: [u32; 3], position: Vec3) -> u32 {
    let local = (position - min) / cell_size;
    let x = clamp_axis(local.x, dims[0]);
    let y = clamp_axis(local.y, dims[1]);
    let z = clamp_axis(local.z, dims[2]);
    (z * dims[1] + y) * dims[0] + x
}

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
    fn every_cell_window_holds_exactly_the_entries_that_belong_to_it() {
        // The build uses `cell_start` as its own fill cursor and shifts it back afterwards. If that
        // shift is off by one, windows overlap or land on the wrong cell and a query silently returns
        // the wrong neighbours, so the invariant is checked directly rather than through a query.
        let mut grid = SpatialGrid::new(SpatialGridConfig::default()).or_die();
        grid.extend_from((0..500).map(|index| {
            let spread = num::u32_to_f32(num::usize_to_u32(index) * 7919 % 977);
            (
                NpcId::unique(num::usize_to_u32(index)),
                Vec3::new(spread, spread * 0.5, spread * 1.25),
                Vec3::ZERO,
            )
        }));
        grid.build();
        let mut total = 0_usize;
        for cell in 0..num::usize_to_u32(grid.cell_count()) {
            let window = grid.cell_window(cell);
            for index in window {
                let entry = grid.entries().get(*index as usize).or_die();
                assert_eq!(
                    grid.cell_of(entry.position),
                    cell,
                    "entry in the wrong cell"
                );
            }
            total += window.len();
        }
        assert_eq!(total, grid.len(), "every entry appears in exactly one cell");
    }

    #[test]
    fn every_thread_count_builds_identical_windows() {
        // ADR 0054 invariant 7: the thread count is configuration and cannot change behaviour. This
        // compares the raw arrays rather than query results, because a query test would pass while two
        // cells' windows were swapped — it only looks at one cell at a time.
        let build = |threads: usize| {
            let config =
                SpatialGridConfig::try_new(Vec3::splat(-400.0), Vec3::splat(400.0), 6.0).or_die();
            let mut grid = SpatialGrid::new(config).or_die().with_threads(threads);
            grid.extend_from((0..6_000).map(|index| {
                let step = num::u32_to_f32(num::usize_to_u32(index));
                (
                    NpcId::unique(num::usize_to_u32(index)),
                    Vec3::new(
                        (step * 0.37).sin() * 380.0,
                        (step * 0.11).cos() * 40.0,
                        (step * 0.23).sin() * 380.0,
                    ),
                    Vec3::ZERO,
                )
            }));
            grid.build();
            grid
        };
        let serial = build(1);
        assert_eq!(
            serial.worker_count(),
            1,
            "the reference must be the serial path"
        );
        for threads in [2, 3, 8] {
            let parallel = build(threads);
            assert!(
                parallel.worker_count() > 1,
                "threads = {threads} must actually partition"
            );
            assert_eq!(
                parallel.cell_start, serial.cell_start,
                "prefix sums differ at {threads} threads"
            );
            assert_eq!(
                parallel.order, serial.order,
                "placement order differs at {threads} threads"
            );
        }
    }

    /// Bodies on a deterministic spiral, spread across the whole volume.
    fn batch_fixture(count: u32) -> SpatialGrid {
        let config =
            SpatialGridConfig::try_new(Vec3::splat(-200.0), Vec3::splat(200.0), 5.0).or_die();
        let mut grid = SpatialGrid::new(config).or_die();
        grid.extend_from((0..count).map(|index| {
            let step = num::u32_to_f32(index);
            let ring = (step * 0.7).sin() * 180.0;
            (
                NpcId::unique(index),
                Vec3::new(
                    ring,
                    (step * 0.13).cos() * 20.0,
                    (step * 0.31).sin() * 180.0,
                ),
                Vec3::new(step * 0.01, 0.0, -step * 0.01),
            )
        }));
        grid.build();
        grid
    }

    fn batch_centres(count: usize) -> Vec<Vec3> {
        (0..count)
            .map(|index| {
                let step = num::u32_to_f32(num::usize_to_u32(index));
                Vec3::new(
                    (step * 1.7).sin() * 150.0,
                    (step * 0.9).cos() * 12.0,
                    (step * 2.3).cos() * 150.0,
                )
            })
            .collect()
    }

    #[test]
    fn a_batch_is_the_truncated_single_query_answer() {
        // The bounded set has to be the nearest `cap` by (distance, identity), not merely "some cap of
        // them": a consumer comparing a batch against a single query must get a prefix, not a sample.
        let grid = batch_fixture(10_000);
        let centres = batch_centres(400);
        let radius = 14.0;
        let mut arena = QueryArena::new(32);
        grid.radius_batch(&centres, radius, &mut arena);
        assert_eq!(arena.len(), centres.len());
        let mut single = Vec::new();
        for (query, centre) in centres.iter().enumerate() {
            grid.radius(*centre, radius, &mut single);
            let expected = single.len().min(arena.cap());
            assert_eq!(
                arena.count(query) as usize,
                expected,
                "query {query} counted a different number of neighbours"
            );
            for (position, neighbour) in arena.neighbours(query).iter().enumerate() {
                let reference = single.get(position).or_die();
                assert_eq!(
                    neighbour.id, reference.id,
                    "query {query} neighbour {position} differs"
                );
                assert!(
                    neighbour
                        .distance_squared
                        .total_cmp(&reference.distance_squared)
                        .is_eq(),
                    "query {query} neighbour {position} has a different distance"
                );
                assert_eq!(neighbour.position, reference.position);
            }
        }
    }

    #[test]
    fn a_batch_is_identical_at_every_thread_count() {
        let grid = batch_fixture(10_000);
        let centres = batch_centres(500);
        let run = |threads: usize| {
            let mut arena = QueryArena::new(32);
            grid.clone()
                .with_threads(threads)
                .radius_batch(&centres, 14.0, &mut arena);
            arena
        };
        let serial = run(1);
        assert!(!serial.is_empty());
        for threads in [2, 3, 8] {
            let parallel = run(threads);
            assert_eq!(
                parallel.counts(),
                serial.counts(),
                "counts differ at {threads} threads"
            );
            for query in 0..centres.len() {
                assert_eq!(
                    parallel.neighbours(query),
                    serial.neighbours(query),
                    "query {query} differs at {threads} threads"
                );
            }
        }
    }

    #[test]
    fn a_batch_with_no_room_returns_nothing_rather_than_panicking() {
        let grid = batch_fixture(1_000);
        let mut arena = QueryArena::new(0);
        grid.radius_batch(&batch_centres(4), 20.0, &mut arena);
        assert_eq!(arena.count(0), 0);
        assert!(arena.neighbours(0).is_empty());
        // A query index past the batch is answered with an empty slice, not a panic.
        assert_eq!(arena.count(99), 0);
        assert!(arena.neighbours(99).is_empty());
    }

    #[test]
    fn the_spatial_digest_notices_an_insertion_ordered_reduction() {
        // The parity harness compares digests, so the digest has to notice the mistake a parallel
        // partition can actually make: reducing into the window array in insertion order instead of cell
        // order. Every body is still present, and every window still holds *a* body, which is why a test
        // that only counted bodies, or compared the set of identifiers, would pass.
        let grid = batch_fixture(2_000);
        let probes = batch_centres(16);
        let correct = crate::testkit::spatial_digest(&grid, &probes, 14.0);
        // Stable on the same grid, or the comparison below would be noise rather than evidence.
        assert_eq!(
            correct,
            crate::testkit::spatial_digest(&grid, &probes, 14.0)
        );

        let mut wrong = grid.clone();
        for (slot, value) in wrong.order.iter_mut().enumerate() {
            *value = num::usize_to_u32(slot);
        }
        let broken = crate::testkit::spatial_digest(&wrong, &probes, 14.0);
        assert_ne!(
            correct, broken,
            "the digest must notice a reduction that ignored cell order"
        );
        // The identities are all still there: the difference is ordering, which is the point.
        assert_eq!(wrong.len(), grid.len());
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
