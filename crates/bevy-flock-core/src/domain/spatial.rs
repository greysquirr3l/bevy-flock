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
use std::time::{Duration, Instant};

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

/// Cells per block when the scan is blocked.
///
/// Chosen so the O(blocks) serial step stays a few hundred additions for any grid this crate will build: a
/// 421,876-cell grid gets 103 blocks, and the 1M-cell budget ceiling gets 256. Smaller blocks buy nothing —
/// the step is already sub-microsecond — and cost more `Vec` traffic in the block-total arrays.
const CELLS_PER_BLOCK: usize = 4_096;

/// Upper bound on blocks, so a caller overriding `with_blocks` cannot turn the "cheap" serial step into an
/// O(cells) loop with extra indirection.
const MAX_BLOCKS: usize = 4_096;

/// Cells below which the scan stays serial regardless of the block count requested.
///
/// Below this the O(blocks) step stops being negligible against O(cells), and a grid this small is not where
/// anyone's frame time goes. The override is clamped rather than honoured so a caller cannot make a small grid
/// slower and then file the regression against this crate (ADR 0055 §7).
const PARALLEL_MIN_CELLS: usize = 65_536;

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
    /// `1.0 / cell_size`, cached because the fill pass multiplies by it once per body per axis.
    inv_cell_size: f32,
    dims: [u32; 3],
    /// Prefix sums of *capacities*: cell `c`'s region is `cell_start[c]..cell_start[c + 1]`, and the bodies in it are
    /// the first `cell_len[c]` slots, leaving the rest as slack for arrivals (ADR 0064). The prefix sums used to be of
    /// counts, which made the two numbers the same thing and cost the ability to absorb an insert: a run that gained a
    /// body moved every run after it. `cell_start[c]..cell_start[c + 1]` is cell `c`'s region in
    /// `order`. Length is `cell_count() + 1`.
    ///
    /// Written by the scan and **not touched afterwards**: `place` advances [`SpatialGrid::cell_cursor`]
    /// instead, so the window table a caller reads is the one the scan produced rather than a leftover of the
    /// placement pass (ADR 0056).
    cell_start: Vec<u32>,
    /// How many bodies sit in each cell: the run at `cell_start[cell]` is `cell_len[cell]` long.
    ///
    /// The window table's entries are no longer one cell's run apart, because each cell owns `capacity_for(count)` slots
    /// and the difference is slack: room for the bodies that arrive next tick without moving any other cell's run
    /// (ADR 0064). So the run length is a separate number, and this is it. Four bytes per cell, and it is what lets a
    /// query slice `cell_start[cell]..cell_start[cell] + cell_len[cell]` with no indirection in between.
    cell_len: Vec<u32>,
    /// The write cursor `place` advances through each window: a copy of `cell_start`, reused every rebuild.
    ///
    /// It exists so that placement does not have to un-advance `cell_start` afterwards. The old shape walked the
    /// starts up to the ends and then shifted the whole array down by one cell, a serial 421k-step dependency
    /// chain measured at 0.70 ms of a 7.48 ms build; a `memcpy` over the same bytes is ~0.08 ms because it
    /// carries no dependency. Length is `cell_count() + 1`.
    cell_cursor: Vec<u32>,
    /// Indices into `entries`, grouped by cell.
    order: Vec<u32>,
    /// Insertion-ordered agents.
    entries: Vec<GridEntry>,
    /// How many threads a rebuild may use. See [`SpatialGrid::with_threads`].
    threads: usize,
    /// The slack each cell's run gets when it is laid out, as a percentage of the cell's population.
    ///
    /// See [`SpatialGrid::with_slack_percent`] and [`DEFAULT_SLACK_PERCENT`]. Read by the count pass, which is where
    /// counts become capacities, so it takes effect on the next rebuild rather than on the next query.
    slack_percent: u8,
    /// Spare slots every cell gets regardless of occupancy. See [`SpatialGrid::with_slack_floor`]. Read by the
    /// count pass beside the percentage, because a cell's spare slots are the larger of the two rules.
    slack_floor: u32,
    /// How many blocks the prefix sum is split into; 0 means derive it from the cell count.
    ///
    /// See [`SpatialGrid::with_blocks`]. Derived from the cell count and never from `threads`, because a
    /// decomposition that followed the worker count would make the output depend on the machine (ADR 0055 §6).
    blocks: usize,
    /// Reused per-block count totals for the blocked scan. See [`SpatialGrid::with_blocks`].
    block_totals: Vec<u32>,
    /// Reused per-block starting offsets for the blocked scan.
    ///
    /// Only the parallel path reads them: the serial rebuild scans the window table directly. Gated with
    /// the code that uses them rather than left to warn in a build that has no threads.
    #[cfg(feature = "threads")]
    block_bases: Vec<u32>,
    /// Whether the tables describe `entries` as they stand.
    ///
    /// Set when the count, scan and placement finish, cleared when the entry list is emptied. The incremental rebuild
    /// reads it instead of inferring "already built" from buffer lengths, because the constructor sizes the window table
    /// and an unbuilt empty grid would otherwise look built, with a cursor that had never been written and a table full
    /// of zeros that happens to be right by accident.
    built: bool,
    /// Every body that landed in a different cell, as `(body, the cell it came from)`.
    ///
    /// Filled by the diff that already runs every tick, and only up to [`PATCH_MAX_MOVERS`]: past that the rebuild is
    /// cheaper than the patch, so a longer list would be memory nothing reads. Every worker writes its own chunk of this
    /// buffer, which is why it is sized by workers and not by bodies, and the chunks are compacted into the front once
    /// the dispatch returns so a caller reads a contiguous list (`len == changed` exactly when it is complete).
    movers: Vec<(u32, u32)>,
    /// The cell each entry landed in, remembered between the counting and placement passes.
    ///
    /// `cell_of` is the only float work in a rebuild and it was being done twice per body: once to
    /// count, once to place. At a million bodies that was 18 of the 46 ms. Four bytes per body buys it
    /// back, and it is the same buffer every tick, so a steady frame does not allocate for it.
    cells: Vec<u32>,
}

/// What each pass of a rebuild cost.
///
/// Produced by [`SpatialGrid::build_measured`]. The numbers in `docs/dev/tuning/performance.md` come from this type
/// rather than from a profiler's attribution, because a profiler cannot separate `count` from `prefix` inside
/// an inlined rebuild — and separating exactly those two is the question phase 4 has to answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PassCosts {
    /// Clearing the cell counts and window starts: `fill_cells_*`. Partitions.
    pub clear: Duration,
    /// Counting bodies per cell and totalling each block's counts: `count_cells_*`. Partitions.
    ///
    /// The totals moved in here from the scan (ADR 0057), so this row is the one to watch if the fusion turns out
    /// to cost more than the read it removed.
    pub count: Duration,
    /// The block bases and the block-local scans: `prefix_bases` then the scan inside `scan_and_place_parallel`.
    ///
    /// Serial by construction in the bases (`O(blocks)` additions) and partitioned in the scans. In the fused
    /// dispatch (ADR 0059) there is no longer a pass boundary between the scan and the placement, so this is the
    /// longest worker's own scan and `place` is the rest of the same dispatch — the two still sum to it, and the
    /// spawn is counted here, where the scan's own dispatch used to be.
    pub prefix: Duration,
    /// Placing bodies into their windows and filling `order`: `place_cells_*`. Partitions.
    ///
    /// In the fused dispatch this is measured from the end of the longest scan to the end of the dispatch, so it
    /// carries the join as well as the placement.
    pub place: Duration,
    /// Blocks the prefix sum was split into: 1 means the serial reference ran.
    ///
    /// Reported rather than inferred, because "the blocked scan is actually used" is a claim a test can check
    /// and a reader cannot otherwise see.
    pub blocks: usize,
    /// Workers the rebuild actually used: not the configured count, and not the machine's.
    pub workers: usize,
    /// Bodies it placed.
    pub bodies: usize,
    /// Cells it scanned.
    pub cells: usize,
}

impl PassCosts {
    /// The sum of the four passes.
    ///
    /// Saturating rather than wrapping: a total that overflowed would read as a fast rebuild.
    #[must_use]
    pub fn total(&self) -> Duration {
        [self.clear, self.count, self.prefix, self.place]
            .into_iter()
            .fold(Duration::ZERO, |sum, pass| {
                sum.checked_add(pass).unwrap_or(Duration::MAX)
            })
    }

    /// The share of the rebuild that ran on a single thread.
    ///
    /// `prefix` is the only pass left that a single thread owns end to end — the bases are `O(blocks)` additions
    /// and the scans are partitioned — so this is the fraction a cheaper prefix could in principle recover, times
    /// the workers it could use. It read `prefix + seat` until ADR 0057 deleted the seat, which is worth knowing
    /// when comparing shares across that commit: the denominator lost a pass, so the same work reads higher.
    /// Zero when the rebuild took no measurable time.
    #[must_use]
    pub fn serial_share(&self) -> f32 {
        let total = self.total().as_secs_f64();
        if total <= 0.0 {
            return 0.0;
        }
        let serial = self.prefix.as_secs_f64();
        num::f64_to_f32(serial / total)
    }
}

/// The most bodies a tick may move and still be patched rather than rebuilt.
///
/// The patch's own bookkeeping is linear in the moves, and so is the chance that a run arrives full, so past a few
/// thousand moves the rebuild is both cheaper and simpler. At the sizes the probe measured, 4,096 moves at a million
/// bodies is 3.4 ms of shifting against a 15.7 ms rebuild — still a win, but the last decade of it.
const PATCH_MAX_MOVERS: usize = 4_096;

/// Where cell `cell`'s run is: `(start, len)`, checked against the cell's capacity.
///
/// `None` for a cell whose run would not fit inside its own region, which is the corruption a patch has to refuse
/// rather than propagate.
fn window_of(cell_start: &[u32], cell_len: &[u32], cell: u32) -> Option<(usize, usize)> {
    let start = usize::try_from(*cell_start.get(cell as usize)?).ok()?;
    let len = usize::try_from(*cell_len.get(cell as usize)?).ok()?;
    let next = usize::try_from(*cell_start.get((cell as usize).checked_add(1)?)?).ok()?;
    if start.checked_add(len)? > next {
        return None;
    }
    Some((start, len))
}

/// The slack a cell gets when it is laid out, as a percentage of its population.
///
/// A quarter is the measured default (ADR 0064): it absorbs the arrivals a tick puts into a cell, and it costs a quarter
/// again in `order` on top of the population, which at a million bodies is 1.7 MB of slack rather than 1.7 MB of exact
/// runs. A crowd whose arrivals cluster harder than that can raise it with
/// [`SpatialGrid::with_slack_percent`], at the cost of that memory and of the cache the runs used to share.
pub const DEFAULT_SLACK_PERCENT: u8 = 25;

/// A slack percentage that means something: 0 (exact runs) to 100 (a spare slot per body).
const fn clamp_slack(percent: u8) -> u8 {
    if percent > 100 { 100 } else { percent }
}
/// Spare slots every cell gets regardless of how many bodies it holds. See [`SpatialGrid::with_slack_floor`].
///
/// Two, because the failure a percentage cannot fix is a cell that was empty when the tick began: a quarter of zero is
/// zero, so a sparse grid gives an empty cell one slot and rebuilds the table the moment two bodies arrive in it.
pub const DEFAULT_SLACK_FLOOR: u32 = 2;
/// The largest floor worth laying out. Past this a cell's spare slots cost more than the rebuild they avoid.
pub const MAX_SLACK_FLOOR: u32 = 1_024;
/// A floor that means something: 0 (a percentage-only rule) to [`MAX_SLACK_FLOOR`].
const fn clamp_floor(floor: u32) -> u32 {
    if floor > MAX_SLACK_FLOOR {
        MAX_SLACK_FLOOR
    } else {
        floor
    }
}

/// Room one cell of `count` bodies gets in `order`: its occupants, its spare slots, and one more for the count itself.
///
/// The spare slots are the larger of two rules, and they fail differently. The percentage scales with a cell that is
/// already busy, so a crowd packed into one cell keeps absorbing arrivals as it thickens. The floor is absolute, so a
/// cell that is *empty* still has room for the two bodies that can land in it together, which no percentage can give
/// it. Spare slots are `max(percent of count, floor)`, so either rule alone can be asked for by zeroing the other, and
/// the pair is what `examples/patch_tail.rs` sweeps.
///
/// Saturating, like every other width bound in this file: a count that overflowed would be a million bodies over
/// `u32::MAX`, and the saturating answer is at least not a window that points backwards.
fn capacity_for(count: u32, slack_percent: u8, slack_floor: u32) -> u32 {
    let percent = (u32::from(slack_percent)).saturating_mul(count) / 100;
    let spare = if percent > slack_floor {
        percent
    } else {
        slack_floor
    };
    count.saturating_add(spare).saturating_add(1)
}
/// Convert one slice of the window table from counts to capacities, recording the occupancy on the way past.
///
/// `window` is `cell_start[lo + 1 ..= hi]`, so its slot `i` belongs to cell `lo + i`, and `lens` is `cell_len[lo..hi]`.
/// The two arrays are shortest-zipped, so a short `lens` (an empty grid) leaves the window alone rather than panicking.
fn counts_to_capacity(window: &mut [u32], lens: &mut [u32], slack_percent: u8, slack_floor: u32) {
    for (slot, len) in window.iter_mut().zip(lens.iter_mut()) {
        *len = *slot;
        *slot = capacity_for(*slot, slack_percent, slack_floor);
    }
}

/// The unsplit tails of the three output arrays, and where each has been split to.
///
/// The fused scan-and-place hands every worker three disjoint slices: the window table it scans, the cursor table it
/// scans and then advances, and the run of `order` those windows cover. Keeping the three tails together means the
/// bookkeeping — which index each one now begins at — lives in one place instead of three locals per worker, and the
/// slices come back with the arrays' lifetime rather than a borrow of the struct, which is what lets a thread hold
/// them while the next worker is cut.
///
/// The two tables are off by one from each other and that is the whole subtlety: the count for cell `c` lives at
/// `cell_start[c + 1]`, so a worker scans `cell_start[lo + 1 ..= hi]` while it seats `cell_cursor[lo .. hi]`. Same
/// length, different start, no overlap with either neighbour.
#[cfg(feature = "threads")]
struct Splits<'a> {
    /// What is left of `cell_start`.
    starts: &'a mut [u32],
    /// What is left of `cell_cursor`.
    cursors: &'a mut [u32],
    /// What is left of `order`.
    runs: &'a mut [u32],
    /// The array index `starts` now begins at.
    at: usize,
    /// The array index `cursors` now begins at.
    cursors_at: usize,
    /// The array index `runs` now begins at.
    runs_at: usize,
}

#[cfg(feature = "threads")]
impl<'a> Splits<'a> {
    /// Cut one worker's slices for cells `[lo, lo + span)` and order `[begin, end)`.
    ///
    /// `last` widens the cursor slice by one slot so the final worker can write `cell_cursor[cells]`, which the
    /// serial scan leaves holding the population and which the parity test compares.
    ///
    /// `None` if the arithmetic does not add up, which cannot happen while the block spans are monotonic — the
    /// caller turns it into an idle worker that still waits at the barrier rather than a short barrier and a hang.
    fn take(
        &mut self,
        lo: usize,
        span: usize,
        last: bool,
        begin: usize,
        end: usize,
    ) -> Option<(&'a mut [u32], &'a mut [u32], &'a mut [u32])> {
        let starts_head = (lo + 1).checked_sub(self.at)?;
        let starts = std::mem::take(&mut self.starts);
        let (_, after_starts) = starts.split_at_mut_checked(starts_head)?;
        let (window, after_window) = after_starts.split_at_mut_checked(span)?;
        self.starts = after_window;
        self.at = lo + 1 + span;
        let cursors_head = lo.checked_sub(self.cursors_at)?;
        let cursors = std::mem::take(&mut self.cursors);
        let (_, after_cursors) = cursors.split_at_mut_checked(cursors_head)?;
        let cursor_span = span + usize::from(last);
        let (cursor, after_cursor) = after_cursors.split_at_mut_checked(cursor_span)?;
        self.cursors = after_cursor;
        self.cursors_at = lo + cursor_span;
        let run = begin.checked_sub(self.runs_at)?;
        let runs = std::mem::take(&mut self.runs);
        let (_, runs_tail) = runs.split_at_mut_checked(run)?;
        let (out, after_runs) = runs_tail.split_at_mut_checked(end.checked_sub(begin)?)?;
        self.runs = after_runs;
        self.runs_at = end;
        Some((window, cursor, out))
    }
}

/// What [`SpatialGrid::build_incremental`] did with a tick's worth of movement.
///
/// The distinction is the point of the whole exercise: most ticks move no body across a cell boundary, and on those the
/// window table, the cursor and the order are still correct, so the rebuild ends after recomputing cell numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildOutcome {
    /// No body changed cell, so nothing was recounted, rescanned or replaced.
    ///
    /// A body that stays in its cell keeps its place in that cell's run, so the previous `order` still lists every body
    /// once and each run is still sorted by identity: the invariants hold without any work.
    Unchanged,
    /// At least one body moved between cells, so the tables were rebuilt end to end.
    Rebuilt {
        /// How many bodies sit in a different cell than they did at the previous rebuild.
        ///
        /// Equal to the population when the entry list itself changed length, because then every body is new to the
        /// table rather than moved within it.
        changed: usize,
    },
    /// Bodies moved between cells, and the table absorbed them without a rebuild.
    ///
    /// The runs are as if the table had been rebuilt: same cell, same order within the cell, same starts and lengths. What
    /// differs is the slack, which a rebuild would have laid out from a fresh count and a patch leaves where it was, so a
    /// patched grid and a rebuilt grid are equal run by run rather than byte for byte (ADR 0064).
    Patched {
        /// How many bodies sit in a different cell than they did at the previous rebuild.
        changed: usize,
    },
}

/// An invariant that a completed rebuild promises, named so a failure says which one broke.
///
/// See [`SpatialGrid::check_invariants`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridInvariant {
    /// `order` is not a permutation of the entry indices: one body is missing from it, or one appears twice.
    NotAPermutation,
    /// A cell's run is not ordered by [`NpcId`], so a query's tie-break would depend on when a body arrived.
    RunUnsorted,
    /// A cell cursor does not end where that cell's run ends, so the next placement would overwrite its neighbour.
    CursorMisplaced,
    /// The runs do not tile `order`: the first does not start at zero, or the last does not end at the population.
    PopulationMismatch,
}

impl core::fmt::Display for GridInvariant {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let what = match self {
            Self::NotAPermutation => "order is not a permutation of the entry indices",
            Self::RunUnsorted => "a cell run is not sorted by identity",
            Self::CursorMisplaced => "a cell cursor does not end at its run's end",
            Self::PopulationMismatch => "the cell runs do not add up to the population",
        };
        formatter.write_str(what)
    }
}

impl std::error::Error for GridInvariant {}

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
            inv_cell_size: 1.0 / cell_size,
            dims,
            cell_start: vec![0; cells + 1],
            cell_len: vec![0; cells],
            cell_cursor: Vec::new(),
            movers: Vec::new(),
            built: false,
            order: Vec::new(),
            entries: Vec::new(),
            threads: default_threads(),
            slack_percent: DEFAULT_SLACK_PERCENT,
            slack_floor: DEFAULT_SLACK_FLOOR,
            blocks: 0,
            block_totals: Vec::new(),
            #[cfg(feature = "threads")]
            block_bases: Vec::new(),
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
            inv_cell_size: 0.5,
            dims: [1, 1, 1],
            cell_start: vec![0, 0],
            cell_len: vec![0],
            cell_cursor: Vec::new(),
            movers: Vec::new(),
            built: false,
            order: Vec::new(),
            entries: Vec::new(),
            threads: 1,
            slack_percent: DEFAULT_SLACK_PERCENT,
            slack_floor: DEFAULT_SLACK_FLOOR,
            blocks: 0,
            block_totals: Vec::new(),
            #[cfg(feature = "threads")]
            block_bases: Vec::new(),
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
    /// `MAX_CELLS` cells, in which case the grid coarsened to stay inside the
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

    /// Total number of cells, never more than `MAX_CELLS`.
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
        self.built = false;
    }

    /// The agents in insertion order.
    #[must_use]
    pub fn entries(&self) -> &[GridEntry] {
        &self.entries
    }
    /// The agents, for the code that moves them between rebuilds.
    ///
    /// Positions and velocities are the supported mutations: they are what a physics or steering step writes, and the
    /// next [`SpatialGrid::build_incremental`] picks them up. Changing an `id` is allowed by the type and means calling
    /// [`SpatialGrid::build`] instead, because the identity is the sort key a cell's run is ordered by.
    pub fn entries_mut(&mut self) -> &mut [GridEntry] {
        &mut self.entries
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
        cell_of_parts(self.config.min, self.inv_cell_size, self.dims, position)
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
    /// Set how much slack each cell's run gets when the table is laid out, as a percentage of its population.
    ///
    /// The default is [`DEFAULT_SLACK_PERCENT`], a quarter. The slack is what a patch spends: a cell absorbs arrivals up
    /// to that share of its occupancy before the table has to be laid out again. Raising it trades memory in `order` (the
    /// slack is unused slots) for a smaller chance of that fallback, which is the knob a crowd with clustered arrivals
    /// wants; lowering it buys the memory back at the cost of more rebuilding.
    ///
    /// Clamped to 0..=100, because a slack above one spare slot per body would double the table for no patch the two
    /// phase move can use. Takes effect on the next rebuild, not on the next query.
    #[must_use]
    pub const fn with_slack_percent(mut self, slack_percent: u8) -> Self {
        self.slack_percent = clamp_slack(slack_percent);
        self
    }
    /// The slack this grid lays its runs out with. See [`SpatialGrid::with_slack_percent`].
    #[must_use]
    pub const fn slack_percent(&self) -> u8 {
        self.slack_percent
    }
    /// Set the spare slots every cell gets regardless of how many bodies it holds, clamped to [`MAX_SLACK_FLOOR`].
    ///
    /// The percentage rule cannot help a cell that is empty, because a percentage of zero is zero: a sparse grid gives
    /// an empty cell one slot, and the first tick in which two bodies arrive in the same one has to lay the table out
    /// again. The floor covers that case, and it is the expensive half of the rule because it is paid per cell rather
    /// than per body: at 611,066 cells a floor of two is 1.2 million slots, which is why the default is the smallest
    /// floor that absorbs a pair rather than a rounder number.
    ///
    /// A cell's spare slots are the larger of this and the percentage, so `with_slack_floor(0)` restores the
    /// percentage-only rule exactly. Takes effect on the next rebuild, not on the next query.
    #[must_use]
    pub const fn with_slack_floor(mut self, slack_floor: u32) -> Self {
        self.slack_floor = clamp_floor(slack_floor);
        self
    }
    /// The floor this grid lays its runs out with. See [`SpatialGrid::with_slack_floor`].
    #[must_use]
    pub const fn slack_floor(&self) -> u32 {
        self.slack_floor
    }
    /// Slots in the run table, which is the population plus the slack every cell was given.
    ///
    /// This is the array a query walks and the memory the slack costs, so it is the number to read next to a latency
    /// measurement rather than a count of bodies.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.order.len()
    }

    /// Split the prefix sum into `blocks` blocks.
    ///
    /// ADR 0055: this changes what a rebuild costs and never what it produces. `0` — the default — derives the
    /// count from the cell count, and `1` routes to the serial scan, which is the reference the byte-equality
    /// test compares against. Values above `MAX_BLOCKS` and grids under `PARALLEL_MIN_CELLS` are clamped,
    /// so a caller cannot ask for a slower scan by accident.
    #[must_use]
    pub const fn with_blocks(mut self, blocks: usize) -> Self {
        self.blocks = blocks;
        self
    }

    /// Blocks the prefix sum will actually be split into.
    #[must_use]
    pub fn block_count(&self) -> usize {
        derived_blocks(self.cell_count(), self.blocks)
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
    /// The buffers are not the whole story on the parallel path: `std::thread::scope` boxes one work packet per
    /// worker, and `examples/spawn_cost.rs` prices a six-worker dispatch at 95 allocator calls. A parallel rebuild
    /// makes three of those — clear, count and place — so a frame that must not touch the allocator at all should
    /// call [`SpatialGrid::with_threads`] with `1`, or wait for a worker pool that stays alive across ticks, which
    /// is phase 5's `persistent-worker-pool` task. The number is quoted from the harness rather than from an older
    /// measurement of this comment: the previous figure here was 22, and the harness disagrees.
    ///
    /// With [`SpatialGrid::with_threads`] above one, the three `O(n)` passes are partitioned across
    /// workers and the prefix sum stays serial. The result is byte-identical at every thread count,
    /// which is the whole point of ADR 0054: the build partitions by *cell range* rather than by arrival
    /// order, so no body's placement depends on which worker got there first.
    pub fn build(&mut self) {
        let _ = self.build_measured();
    }

    /// Rebuild, reporting what each pass cost.
    ///
    /// Five `Instant::now()` pairs against a rebuild that takes 8 ms at a million bodies is 0.01%, so this is
    /// not behind a feature flag: the inventory in `docs/dev/tuning/performance.md` has to be reproducible by anyone who
    /// checks the tree out, and a diagnostic that exists only in one profile is a claim rather than a
    /// measurement. [`SpatialGrid::build`] is this function with the costs dropped.
    pub fn build_measured(&mut self) -> PassCosts {
        let workers = self.worker_count();
        let mut costs = PassCosts::default();
        let start = Instant::now();
        if workers <= 1 {
            self.fill_cells_serial();
        } else {
            self.fill_cells_parallel(workers);
        }
        costs.clear = start.elapsed();
        self.build_tables(&mut costs);
        costs
    }
    /// Count, scan and place: everything a rebuild does once it knows each body's cell.
    ///
    /// Split out of [`SpatialGrid::build_measured`] so the incremental path can run the same passes over a cell table it
    /// has already recomputed, instead of keeping a second copy of this dispatch to drift out of step with the first.
    fn build_tables(&mut self, costs: &mut PassCosts) {
        let workers = self.worker_count();
        let blocks = self.block_count();
        let start = Instant::now();
        if workers <= 1 {
            self.count_cells_serial();
        } else {
            self.count_cells_parallel(workers);
        }
        costs.count = start.elapsed();
        // The scan is the serial tail this phase exists to attack, so it is timed on its own rather than folded
        // into the count that derives its totals.
        costs.blocks = blocks;
        if workers > 1 && blocks > 1 {
            // One dispatch for the scan and the placement (ADR 0059): nothing else touches either array between
            // them, so paying two spawns for them was paying twice for one hand-off. `prefix` carries the spawn,
            // where the scan's own dispatch used to be counted, which keeps the rows comparable with the serial path.
            let (prefix, place) = self.scan_and_place_parallel(workers, blocks);
            costs.prefix = prefix;
            costs.place = place;
        } else if workers > 1 {
            // One block: the scan has nothing to split, but the placement still does.
            let start = Instant::now();
            self.prefix_cells_serial();
            costs.prefix = start.elapsed();
            costs.place = self.place_cells_parallel(workers);
        } else {
            let start = Instant::now();
            self.prefix_cells_serial();
            costs.prefix = start.elapsed();
            // No seat pass: the block-local scan wrote the cursor alongside the window table (ADR 0057), so
            // placement starts from a primed cursor and the copy that used to sit here has nothing left to do.
            let start = Instant::now();
            self.place_cells_serial();
            costs.place = start.elapsed();
        }
        costs.workers = workers;
        costs.bodies = self.entries.len();
        costs.cells = self.cell_start.len();
        self.built = true;
    }
    /// Rebuild only if a body actually changed cell.
    ///
    /// The cheap tick. Cell numbers are recomputed for every body, which is the work `fill` already did, and the result
    /// is compared against the table the previous rebuild left behind. A body that stayed in its cell keeps its slot in
    /// that cell's run, so the old table still describes the new positions; a body that moved between cells makes the
    /// old runs wrong, which is the only case that pays for [`SpatialGrid::build_tables`].
    ///
    /// The comparison costs nothing extra: `fill` was writing every slot unconditionally, so this writes a slot only
    /// when it changes and counts the writes it made.
    ///
    /// `O(n)` when nothing moved, `O(n)` plus the three table passes when something did. It does not yet *patch* a handful
    /// of moves without rebuilding: that needs per-cell arrival and departure counts, and is the next step of ADR 0063
    /// P2, verified per ADR 0064.
    pub fn build_incremental(&mut self) -> RebuildOutcome {
        // A grid with no table yet, one whose entry list changed length, or one whose buffers were sized for another
        // population has nothing worth keeping. The length test is against the *capacity* total, not the population:
        // the array is longer than the bodies by the slack, and comparing it to `entries.len()` would rebuild every tick
        // for a table that is perfectly reusable.
        let cells_in_grid = self.cell_count();
        let capacity = self.cell_start.get(cells_in_grid).copied().unwrap_or(0) as usize;
        let reusable = self.built
            && self.cell_len.len() == cells_in_grid
            && self.cell_start.len() == cells_in_grid.saturating_add(1)
            && self.cell_cursor.len() == self.cell_start.len()
            && self.order.len() == capacity
            && self.cells.len() == self.entries.len();
        let changed = self.refill_cells();
        if reusable && changed == 0 {
            return RebuildOutcome::Unchanged;
        }
        // A handful of moves is cheaper to move than to recount: no fill, no count, no scan, just the two runs each body
        // touches. `movers.len() == changed` is the test that the list is complete rather than truncated.
        // A handful of moves is cheaper to move than to recount: no fill, no count, no scan, just the two runs each body
        // touches. `movers.len() == changed` is the test that the list is complete rather than truncated, and the patch
        // itself refuses when a run has no room, which is the one case only a fresh count can fix.
        let patched = reusable
            && changed <= PATCH_MAX_MOVERS
            && self.movers.len() == changed
            && self.patch_movers();
        if patched {
            return RebuildOutcome::Patched { changed };
        }
        let mut costs = PassCosts::default();
        self.build_tables(&mut costs);
        RebuildOutcome::Rebuilt { changed }
    }

    /// Move the bodies that changed cell, in place, without rebuilding anything.
    ///
    /// Two phases, every removal before any insertion. A body leaving a cell frees the slot an arrival into that same
    /// cell may need, so interleaving the two would let a full cell refuse an arrival a later departure made room for.
    ///
    /// Returns `false` when a run had no room left in its region, which the caller answers with a rebuild. Abandoning a
    /// half-applied patch is safe precisely because [`SpatialGrid::build_tables`] recomputes every table from the entries:
    /// the work is wasted, the result is not wrong.
    ///
    /// `O(moves)` twice, plus the shift of the run tails, which is why only a handful of moves may take this path.
    fn patch_movers(&mut self) -> bool {
        let Self {
            cell_start,
            cell_len,
            cell_cursor,
            order,
            entries,
            cells,
            movers,
            ..
        } = self;
        for (body, old) in movers.iter().copied() {
            let Some((start, len)) = window_of(cell_start, cell_len, old) else {
                return false;
            };
            let Some(run) = order.get(start..start.saturating_add(len)) else {
                return false;
            };
            let Some(at) = run.iter().position(|value| *value == body) else {
                return false;
            };
            // Close the gap inside the run only: the slots after it belong to the next cell.
            order.copy_within(
                start.saturating_add(at).saturating_add(1)..start.saturating_add(len),
                start.saturating_add(at),
            );
            let (Some(len_slot), Some(cursor)) = (
                cell_len.get_mut(old as usize),
                cell_cursor.get_mut(old as usize),
            ) else {
                return false;
            };
            *len_slot = len_slot.saturating_sub(1);
            *cursor = cursor.saturating_sub(1);
        }
        for (body, _) in movers.iter().copied() {
            let Some(new) = cells.get(body as usize).copied() else {
                return false;
            };
            let Some((start, len)) = window_of(cell_start, cell_len, new) else {
                return false;
            };
            let Some(next) = cell_start
                .get((new as usize).saturating_add(1))
                .copied()
                .map(|value| value as usize)
            else {
                return false;
            };
            if start.saturating_add(len) >= next {
                // The region is full. A rebuild lays the slack out again from the new counts.
                return false;
            }
            let Some(target) = entries.get(body as usize) else {
                return false;
            };
            let at = order
                .get(start..start.saturating_add(len))
                .map_or(0, |run| {
                    run.partition_point(|value| {
                        entries
                            .get(*value as usize)
                            .is_some_and(|other| other.id < target.id)
                    })
                });
            // Push the run's tail up by one inside the region, then drop the body into the hole. The run stays sorted by
            // identity, which is the tie-break a query relies on.
            order.copy_within(
                start.saturating_add(at)..start.saturating_add(len),
                start.saturating_add(at).saturating_add(1),
            );
            let Some(slot) = order.get_mut(start.saturating_add(at)) else {
                return false;
            };
            *slot = body;
            let (Some(len_slot), Some(cursor)) = (
                cell_len.get_mut(new as usize),
                cell_cursor.get_mut(new as usize),
            ) else {
                return false;
            };
            *len_slot = len_slot.saturating_add(1);
            *cursor = cursor.saturating_add(1);
        }
        true
    }
    /// Recompute every body's cell, returning how many of them landed somewhere new.
    ///
    /// The buffer is the one the previous rebuild used, so an unchanged cell number is a slot left alone rather than a
    /// slot rewritten with the value it already held. At a million bodies that is most of the traffic the pass would
    /// otherwise generate, on a tick that may be about to do nothing else at all.
    fn refill_cells(&mut self) -> usize {
        #[cfg(feature = "threads")]
        if self.worker_count() > 1 && self.entries.len() >= PARALLEL_MIN_BODIES {
            return self.refill_cells_parallel();
        }
        let (min, inv, dims) = (self.config.min, self.inv_cell_size, self.dims);
        let Self {
            cells,
            entries,
            movers,
            ..
        } = self;
        movers.clear();
        if cells.len() != entries.len() {
            cells.clear();
            cells.resize(entries.len(), 0);
            for (slot, entry) in cells.iter_mut().zip(entries.iter()) {
                *slot = cell_of_parts(min, inv, dims, entry.position);
            }
            return entries.len();
        }
        let mut changed = 0_usize;
        for (index, (slot, entry)) in cells.iter_mut().zip(entries.iter()).enumerate() {
            let cell = cell_of_parts(min, inv, dims, entry.position);
            if *slot != cell {
                // The old cell has to be read here, because the next statement is what forgets it.
                if movers.len() < PATCH_MAX_MOVERS {
                    movers.push((num::usize_to_u32(index), *slot));
                }
                *slot = cell;
                changed = changed.saturating_add(1);
            }
        }
        changed
    }
    /// The same diff, one body range per worker, each reporting how many moves it saw and which ones.
    ///
    /// One slot per worker rather than a shared counter, so the total is the same whatever order the workers finish in,
    /// and the pass allocates nothing: each worker writes the moves it finds into its own chunk of `movers`, and the chunks
    /// are compacted into the front afterwards. The old cell is read before the slot is overwritten, because that write is
    /// what forgets it.
    ///
    /// A worker whose chunk fills stops recording and says so. A tick with more moves than that is rebuilt rather than
    /// patched, and a half-recorded list must not be mistaken for a complete one.
    #[cfg(feature = "threads")]
    fn refill_cells_parallel(&mut self) -> usize {
        /// Workers are bounded by cores in practice; a longer list than this stops recording a count.
        const SAMPLES: usize = 32;
        let (min, inv, dims) = (self.config.min, self.inv_cell_size, self.dims);
        let workers = self.worker_count().min(SAMPLES);
        self.movers.clear();
        self.movers
            .resize(workers.saturating_mul(PATCH_MAX_MOVERS), (0, 0));
        let Self {
            cells,
            entries,
            movers,
            ..
        } = self;
        let bodies = entries.len();
        if cells.len() != bodies {
            cells.clear();
            cells.resize(bodies, 0);
            movers.clear();
            for (slot, entry) in cells.iter_mut().zip(entries.iter()) {
                *slot = cell_of_parts(min, inv, dims, entry.position);
            }
            return bodies;
        }
        let chunk = bodies.div_ceil(workers.max(1));
        if chunk == 0 {
            movers.clear();
            return 0;
        }
        let counts: [core::sync::atomic::AtomicUsize; SAMPLES] =
            core::array::from_fn(|_| core::sync::atomic::AtomicUsize::new(0));
        let wrote: [core::sync::atomic::AtomicUsize; SAMPLES] =
            core::array::from_fn(|_| core::sync::atomic::AtomicUsize::new(0));
        let mut base = 0_usize;
        thread::scope(|scope| {
            let mut siblings: &[core::sync::atomic::AtomicUsize] = counts.as_slice();
            let mut writers: &[core::sync::atomic::AtomicUsize] = wrote.as_slice();
            for ((slots, slice), room) in cells
                .chunks_mut(chunk)
                .zip(entries.chunks(chunk))
                .zip(movers.chunks_mut(PATCH_MAX_MOVERS))
            {
                let (sample, rest) = match siblings.split_first() {
                    Some((first, rest)) => (Some(first), rest),
                    None => (None, siblings),
                };
                siblings = rest;
                let (writer, rest) = match writers.split_first() {
                    Some((first, rest)) => (Some(first), rest),
                    None => (None, writers),
                };
                writers = rest;
                let first_body = base;
                base = base.saturating_add(slice.len());
                scope.spawn(move || {
                    let mut changed = 0_usize;
                    let mut collected = 0_usize;
                    let mut full = false;
                    for (offset, (slot, entry)) in slots.iter_mut().zip(slice).enumerate() {
                        let cell = cell_of_parts(min, inv, dims, entry.position);
                        if *slot != cell {
                            if let Some(pair) = room.get_mut(collected) {
                                *pair =
                                    (num::usize_to_u32(first_body.saturating_add(offset)), *slot);
                                collected = collected.saturating_add(1);
                            } else {
                                full = true;
                            }
                            *slot = cell;
                            changed = changed.saturating_add(1);
                        }
                    }
                    if let Some(slot) = sample {
                        slot.store(changed, core::sync::atomic::Ordering::Relaxed);
                    }
                    if let Some(slot) = writer {
                        slot.store(
                            if full { usize::MAX } else { collected },
                            core::sync::atomic::Ordering::Relaxed,
                        );
                    }
                });
            }
        });
        let changed = counts
            .iter()
            .map(|slot| slot.load(core::sync::atomic::Ordering::Relaxed))
            .fold(0_usize, usize::saturating_add);
        if wrote
            .iter()
            .any(|slot| slot.load(core::sync::atomic::Ordering::Relaxed) == usize::MAX)
        {
            self.movers.clear();
            return changed;
        }
        let mut write_at = 0_usize;
        for (index, slot) in wrote.iter().enumerate() {
            let count = slot.load(core::sync::atomic::Ordering::Relaxed);
            if count == 0 {
                continue;
            }
            let from = index.saturating_mul(PATCH_MAX_MOVERS);
            if from != write_at {
                self.movers
                    .copy_within(from..from.saturating_add(count), write_at);
            }
            write_at = write_at.saturating_add(count);
        }
        self.movers.truncate(write_at);
        changed
    }
    /// Check the four things a completed rebuild promises, in `O(n)` plus one bitset.
    ///
    /// Allocates, so it is a debugging and test tool rather than something to call in a frame. It exists because the
    /// incremental path's failures are quiet: a body left out of `order` is a neighbour that stops being found, and a
    /// run left unsorted changes a query's tie-break without changing how many neighbours it returns.
    ///
    /// # Errors
    ///
    /// The first invariant that fails, named by [`GridInvariant`].
    pub fn check_invariants(&self) -> Result<(), GridInvariant> {
        let bodies = self.entries.len();
        let cells = self.cell_count();
        if self.cell_start.len() != cells.saturating_add(1) || self.cell_len.len() != cells {
            return Err(GridInvariant::PopulationMismatch);
        }
        // The array is as long as the capacities add up to. With slack, that is longer than the population, and the
        // difference is the room a patch has to spend.
        let capacity_total = self.cell_start.get(cells).copied().unwrap_or(0);
        if self.order.len() != capacity_total as usize
            || self.cell_start.first().copied() != Some(0)
        {
            return Err(GridInvariant::PopulationMismatch);
        }
        let mut seen = vec![false; bodies];
        let mut occupied = 0_u32;
        for cell in 0..cells {
            let start = self.cell_start.get(cell).copied().unwrap_or(0);
            let next = self
                .cell_start
                .get(cell.saturating_add(1))
                .copied()
                .unwrap_or(0);
            let len = self.cell_len.get(cell).copied().unwrap_or(0);
            if next < start || len > next.saturating_sub(start) {
                // A run longer than its own region would write into the next cell's bodies.
                return Err(GridInvariant::PopulationMismatch);
            }
            if self.cell_cursor.get(cell).copied() != Some(start.saturating_add(len)) {
                return Err(GridInvariant::CursorMisplaced);
            }
            occupied = occupied.saturating_add(len);
            let Some(run) = self
                .order
                .get(start as usize..start.saturating_add(len) as usize)
            else {
                return Err(GridInvariant::PopulationMismatch);
            };
            for pair in run.windows(2) {
                let (Some(first), Some(second)) = (
                    self.entries.get(pair[0] as usize),
                    self.entries.get(pair[1] as usize),
                ) else {
                    return Err(GridInvariant::NotAPermutation);
                };
                if first.id > second.id {
                    return Err(GridInvariant::RunUnsorted);
                }
            }
            for index in run {
                match usize::try_from(*index)
                    .ok()
                    .and_then(|slot| seen.get_mut(slot))
                {
                    Some(flag) if !*flag => *flag = true,
                    _ => return Err(GridInvariant::NotAPermutation),
                }
            }
        }
        if occupied != num::usize_to_u32(bodies) {
            return Err(GridInvariant::PopulationMismatch);
        }
        Ok(())
    }
    /// Compute each body's cell. `cell_of` is the only float work in a rebuild, and it is done once per
    /// body into a reused buffer so the placement pass reads four bytes instead of recomputing.
    fn fill_cells_serial(&mut self) {
        let (min, inv, dims) = (self.config.min, self.inv_cell_size, self.dims);
        let Self { cells, entries, .. } = self;
        cells.clear();
        cells.resize(entries.len(), 0);
        for (slot, entry) in cells.iter_mut().zip(entries.iter()) {
            *slot = cell_of_parts(min, inv, dims, entry.position);
        }
    }

    /// Zero the window table and make sure the cursor table is the same length.
    ///
    /// The cursor is sized here, not in the scan, because `place` reads it whether or not the prefix sum ran with
    /// blocks: a grid whose cell count changed between rebuilds must not be left with a short cursor. The resize is
    /// a no-op once a grid is warm, which matters because this runs every rebuild.
    ///
    /// The cursor is *not* cleared. Every slot the scan writes it writes unconditionally, from index 1 up, and what
    /// `place` left there is exactly what that overwrites — so a `fill` here would be a second 1.7 MB write per
    /// rebuild to prepare memory that is about to be written anyway. Index 0 is the one slot the scans skip (it is
    /// the empty prefix, not a cell), and each of them sets it.
    fn reset_tables(&mut self) {
        self.cell_start.fill(0);
        self.cell_len.fill(0);
        if self.cell_cursor.len() != self.cell_start.len() {
            self.cell_cursor.clear();
            self.cell_cursor.resize(self.cell_start.len(), 0);
        }
    }

    /// Sum each block's counts out of the window table, on one thread.
    ///
    /// The serial counterpart of what each worker does for its own blocks in [`Self::count_cells_parallel`]: the
    /// same numbers, read back from the array instead of kept in a register. A serial rebuild is the reference the
    /// byte-equality test compares against, so it may be the slower of the two (ADR 0057) — but not here: a rebuild
    /// with one worker scans serially too, so it wants no totals at all, and [`Self::needs_block_totals_for`] is
    /// what both call sites consult to agree on that.
    fn total_blocks_from_window(&mut self) {
        let blocks = self.block_count();
        self.block_totals.clear();
        self.block_totals.resize(blocks, 0);
        let cells = self.cell_count();
        let Self {
            cell_start,
            block_totals,
            ..
        } = self;
        for (index, slot) in block_totals.iter_mut().enumerate() {
            let (first, len) = block_span(index, cells, blocks);
            *slot = cell_start
                .get(first + 1..first + 1 + len)
                .map_or(0, |window| window.iter().copied().sum());
        }
    }

    /// Count how many bodies land in each cell, in `cell_start[cell + 1]`, and total them per block.
    ///
    /// The totals are computed here rather than in the scan because this pass already holds the counts, and the
    /// scan used to read all 1.7 MB of them back on one thread to recover numbers this pass had just written
    /// (ADR 0057).
    fn count_cells_serial(&mut self) {
        self.reset_tables();
        for cell in &self.cells {
            if let Some(slot) = self.cell_start.get_mut(*cell as usize + 1) {
                *slot += 1;
            }
        }
        // Counts become capacities here, because this is the last moment the counts exist on their own: the prefix
        // scan below sums whatever it is given, and giving it capacities is what leaves room between the runs.
        let cells_in_grid = self.cell_count();
        let slack = self.slack_percent;
        let floor = self.slack_floor;
        let Self {
            cell_start,
            cell_len,
            ..
        } = self;
        if let (Some(window), Some(lens)) =
            (cell_start.get_mut(1..), cell_len.get_mut(..cells_in_grid))
        {
            counts_to_capacity(window, lens, slack, floor);
        }
        if self.needs_block_totals_for(self.worker_count()) {
            self.total_blocks_from_window();
        } else {
            self.block_totals.clear();
        }
    }

    /// Turn the block totals into bases, and report the population they add up to.
    ///
    /// This is all that is left of the old `prefix`: `O(blocks)` additions, no scan. The scans moved into the
    /// dispatch that places the bodies, because between the two of them nothing else touches the window table
    /// (ADR 0059) — so what used to be two spawns is one, and this is the serial step between the count that
    /// produced the totals and the scan that consumes them.
    ///
    /// Saturating rather than wrapping, for the same reason `PassCosts::total` saturates: a sum that overflowed
    /// would shift every window after it and still add up to the population.
    #[cfg(feature = "threads")]
    fn prefix_bases(&mut self) -> u32 {
        let blocks = self.block_count();
        self.block_bases.clear();
        self.block_bases.resize(blocks, 0);
        let mut running = 0_u32;
        for (index, total) in self.block_totals.iter().enumerate() {
            if let Some(slot) = self.block_bases.get_mut(index) {
                *slot = running;
            }
            running = running.saturating_add(*total);
        }
        running
    }

    /// Whether the scan that is about to run needs per-block totals.
    ///
    /// Only a blocked scan does. A single worker takes the serial scan whatever the block count says, which is
    /// why `count` asks this before summing anything: totals that nothing reads cost a full pass over the window
    /// table, and on an otherwise serial rebuild that is the difference between a wash and a regression.
    fn needs_block_totals_for(&self, workers: usize) -> bool {
        self.block_count() > 1 && workers > 1
    }

    /// The reference scan: one thread, in place, over the whole cell array.
    ///
    /// Counts live at `cell_start[cell + 1]`, so this rotates them into `cell_start[cell + 1] == sum of the
    /// counts at and below cell`, which leaves `cell_start[cell]` holding where cell `cell`'s window begins.
    /// `place` fills that window upwards from its start and never reads back through this array. The blocked
    /// scan below must reproduce this array byte for byte, which is why this stays in the tree rather than
    /// being replaced.
    ///
    /// It also writes `cell_cursor[p]` from the value it just computed, so the two tables are equal the moment
    /// this returns and `place` has a cursor to advance without a copy (ADR 0057).
    fn prefix_cells_serial(&mut self) {
        let Self {
            cell_start,
            cell_cursor,
            ..
        } = self;
        // Index 0 is the empty prefix and the start of cell 0's window: always zero, and the one slot the walk
        // below does not touch.
        if let Some(first) = cell_cursor.get_mut(0) {
            *first = 0;
        }
        let (Some(windows), Some(cursor_head)) =
            (cell_start.get_mut(1..), cell_cursor.get_mut(1..))
        else {
            return;
        };
        let mut running = 0_u32;
        for (slot, cursor) in windows.iter_mut().zip(cursor_head.iter_mut()) {
            *slot += running;
            running = *slot;
            *cursor = running;
        }
    }

    /// Fill each window from its start, in body order, on one thread.
    ///
    /// The reference the parallel path is compared against byte for byte, and the path a rebuild takes when it has
    /// one worker. It advances `cell_cursor` rather than `cell_start`, so the scan's output survives placement and
    /// nothing has to shift it back (ADR 0056).
    fn place_cells_serial(&mut self) {
        // The array is as long as the capacities add up to, not as long as the population: the difference is the slack a
        // patch spends, and placement still writes only the first `cell_len[cell]` slots of each region.
        let total = self.cell_start.get(self.cell_count()).copied().unwrap_or(0);
        self.order.clear();
        self.order.resize(total as usize, 0);
        for (index, cell) in self.cells.iter().enumerate() {
            let cell = *cell as usize;
            let target = match self.cell_cursor.get(cell).copied() {
                Some(target) => target as usize,
                None => continue,
            };
            if let Some(slot) = self.order.get_mut(target) {
                *slot = num::usize_to_u32(index);
            }
            if let Some(cursor) = self.cell_cursor.get_mut(cell) {
                *cursor += 1;
            }
        }
    }

    /// Rotate one worker's counts into window starts, and seat its placement cursor in the same walk.
    ///
    /// `window` is `cell_start[lo + 1 ..= hi]`, where the counts of cells `lo..hi` live, and `cursor` is
    /// `cell_cursor[lo .. hi]`, where their window starts belong. Both are the same length, and the walk sits them one
    /// slot apart: cell `lo + i`'s start is the running total *before* its own count is added, so the cursor slot is
    /// written first and the window slot accumulates from the same register. That is the whole of ADR 0057's fusion —
    /// seating the cursor costs a store rather than a second pass over the window table.
    ///
    /// `population` is `Some` only for the last worker: `cell_cursor[cells]` holds the population in the serial scan
    /// and nothing in this walk produces it, because no cell owns it.
    #[cfg(feature = "threads")]
    fn scan_cells(window: &mut [u32], cursor: &mut [u32], base: u32, population: Option<u32>) {
        let span = window.len();
        let mut running = base;
        for (slot, seated) in window.iter_mut().zip(cursor.iter_mut()) {
            *seated = running;
            *slot = slot.saturating_add(running);
            running = *slot;
        }
        if let Some((slot, population)) = cursor.get_mut(span).zip(population) {
            *slot = population;
        }
    }

    /// Advance one worker's cursor slots into its run of `order`, in body order.
    #[cfg(feature = "threads")]
    fn place_run(
        cell_list: &[u32],
        cursor: &mut [u32],
        out: &mut [u32],
        lo: usize,
        hi: usize,
        begin: usize,
    ) {
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
            if let Some(written) = target
                .checked_sub(begin)
                .and_then(|local| out.get_mut(local))
            {
                *written = num::usize_to_u32(body);
            }
        }
    }

    /// The block scans and the placement in one dispatch (ADR 0059).
    ///
    /// The two passes are adjacent in what they touch: the scan writes `cell_start[c]` and `cell_cursor[c]` for its own
    /// cells, and placement then advances those same cursor slots and fills `order`. Nothing else reads or writes either
    /// array in between, so paying two `thread::scope` spawns for them was paying twice for one hand-off — 0.53 ms each
    /// on the phase 5 harness.
    ///
    /// **No barrier, and that is the point.** A worker fills the windows of its own cells `[lo, hi)`, and every start in
    /// that range depends only on the counts of cells below `hi` — which the worker has just scanned itself, on top of
    /// the base `prefix_bases` gave it. So the second pass has nothing to wait for. An earlier shape of this function
    /// did wait, on the theory that a worker cannot know the blocks before it are scanned; it cannot, but it does not
    /// need to, because its base is already the sum of them. The barrier cost 0.6 ms — more than the dispatch it
    /// replaced — and the measurement is what caught it.
    ///
    /// Every worker owns exactly the cells of its blocks, `[lo, hi)`, and reads `cell_start[lo + 1 ..= hi]` where the
    /// counts of those cells live. That off-by-one is the whole subtlety: a slice that reached one cell too far would
    /// read — or overwrite — the neighbour's first count.
    ///
    /// Placement keeps a block-aligned range here rather than the cell-aligned one [`SpatialGrid::place_cells_parallel`]
    /// uses, because the worker holding that slice of the cursor table has to be the worker that advances it.
    ///
    /// Returns `(prefix, place)`. The two phases no longer have a boundary between them, so `prefix` is the longest
    /// worker's own scan and `place` is the rest of the dispatch: what the attribution loses in exactness it keeps in
    /// comparability, since both rows still sum to the dispatch.
    // Without the `threads` feature there is exactly one worker, so the dispatch in `build_measured` never reaches
    // these. They exist so that dispatch reads the same on both sides of the feature instead of duplicating every
    // call site behind `cfg` and having the two drift apart.
    #[cfg(not(feature = "threads"))]
    fn fill_cells_parallel(&mut self, _workers: usize) {
        self.fill_cells_serial();
    }

    #[cfg(not(feature = "threads"))]
    fn count_cells_parallel(&mut self, _workers: usize) {
        self.count_cells_serial();
    }

    #[cfg(not(feature = "threads"))]
    fn scan_and_place_parallel(&mut self, _workers: usize, _blocks: usize) -> (Duration, Duration) {
        let start = Instant::now();
        self.prefix_cells_serial();
        let prefix = start.elapsed();
        let place = Instant::now();
        self.place_cells_serial();
        (prefix, place.elapsed())
    }

    #[cfg(feature = "threads")]
    fn scan_and_place_parallel(&mut self, workers: usize, blocks: usize) -> (Duration, Duration) {
        /// Workers are bounded by cores in practice; a longer list than this simply stops recording samples.
        const SAMPLES: usize = 32;
        let cells_in_grid = self.cell_count();
        let total = self.prefix_bases();
        let capacity = total as usize;
        let Self {
            cell_start,
            cell_cursor,
            order,
            cells,
            block_bases,
            ..
        } = self;
        let cell_list: &[u32] = cells.as_slice();
        let bases: &[u32] = block_bases.as_slice();
        order.clear();
        order.resize(total as usize, 0);
        let per = blocks.div_ceil(workers.max(1)).max(1);
        let samples: [core::sync::atomic::AtomicU64; SAMPLES] =
            core::array::from_fn(|_| core::sync::atomic::AtomicU64::new(0));
        let started = Instant::now();
        thread::scope(|scope| {
            let mut splits = Splits {
                starts: cell_start.as_mut_slice(),
                cursors: cell_cursor.as_mut_slice(),
                runs: order.as_mut_slice(),
                at: 0,
                cursors_at: 0,
                runs_at: 0,
            };
            for worker in 0..workers.max(1) {
                let lo_block = worker * per;
                let hi_block = (lo_block + per).min(blocks);
                let lo = block_span(lo_block, cells_in_grid, blocks).0;
                let hi = block_span(hi_block, cells_in_grid, blocks).0;
                let base = bases.get(lo_block).copied().unwrap_or(0);
                // The window a worker owns runs from its base to the next block's base, which is a *capacity* offset:
                // the table is longer than the population by the slack a patch spends, and clamping to the population
                // here would hand the last workers a short slice and silently drop their bodies.
                let end = usize::try_from(bases.get(hi_block).copied().unwrap_or(total))
                    .unwrap_or(capacity)
                    .min(capacity);
                let begin = usize::try_from(base).unwrap_or(0).min(end);
                let span_cells = hi.saturating_sub(lo);
                let last = hi == cells_in_grid;
                let payload = splits.take(lo, span_cells, last, begin, end);
                let sample = samples.get(worker);
                scope.spawn(move || {
                    let Some((window, cursor, out)) = payload else {
                        return;
                    };
                    let scanned = Instant::now();
                    Self::scan_cells(
                        window,
                        &mut *cursor,
                        base,
                        last.then(|| num::usize_to_u32(end)),
                    );
                    if let Some(slot) = sample {
                        slot.store(
                            u64::try_from(scanned.elapsed().as_nanos()).unwrap_or(u64::MAX),
                            core::sync::atomic::Ordering::Relaxed,
                        );
                    }
                    Self::place_run(cell_list, cursor, out, lo, hi, begin);
                });
            }
        });
        let finished = Instant::now();
        let longest_scan = samples
            .iter()
            .map(|slot| slot.load(core::sync::atomic::Ordering::Relaxed))
            .max()
            .unwrap_or(0);
        debug_assert_eq!(
            cell_start.get(cells_in_grid).copied(),
            Some(total),
            "the fused scan left the window table's total wrong"
        );
        let scanned = Duration::from_nanos(longest_scan);
        (
            scanned,
            finished.duration_since(started).saturating_sub(scanned),
        )
    }

    /// Place the bodies into their windows, one worker per cell range.
    ///
    /// The cursor is already primed by the block-local scan that produced the window starts (ADR 0057), so this
    /// dispatch copies nothing: it reads `cell_cursor`, advances what it owns, and writes its own run of `order`.
    /// Before that fusion the copy ran here, and before *that* it was a serial pass of its own — which is the
    /// shape ADR 0056 measured, and why the copy is gone rather than moved again.
    ///
    /// Each worker takes a contiguous cell range and the run of `order` that range's windows cover. The two
    /// arrays are split before anything spawns, so no worker can reach another's cells, and placement order
    /// within a cell is the body order the serial path uses, which is what keeps the output byte-identical.
    ///
    /// Returns the wall time of the whole dispatch, spawn included, so the figure is comparable with `clear` and
    /// `count`, which are timed the same way.
    #[cfg(not(feature = "threads"))]
    fn place_cells_parallel(&mut self, _workers: usize) -> Duration {
        let start = Instant::now();
        self.place_cells_serial();
        start.elapsed()
    }

    #[cfg(feature = "threads")]
    fn place_cells_parallel(&mut self, workers: usize) -> Duration {
        let cells_in_grid = self.cell_count();
        let Self {
            cell_start,
            cell_cursor,
            order,
            cells,
            ..
        } = self;
        let starts: &[u32] = cell_start.as_slice();
        let cell_list: &[u32] = cells.as_slice();
        order.clear();
        let capacity = cell_start.get(cells_in_grid).copied().unwrap_or(0) as usize;
        order.resize(capacity, 0);
        let per = cells_in_grid.div_ceil(workers);
        let started = Instant::now();
        thread::scope(|scope| {
            let mut cursor_rest: &mut [u32] = cell_cursor.as_mut_slice();
            let mut order_rest: &mut [u32] = order.as_mut_slice();
            let mut cursor_at = 0_usize;
            let mut order_at = 0_usize;
            for index in 0..workers {
                let lo = index * per;
                let hi = ((index + 1) * per).min(cells_in_grid);
                if lo >= hi {
                    continue;
                }
                let begin = starts.get(lo).copied().unwrap_or(0) as usize;
                let end = starts.get(hi).copied().unwrap_or(0) as usize;
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
        started.elapsed()
    }

    /// Pass one, partitioned by body: each worker writes its own slice of the cell list.
    #[cfg(feature = "threads")]
    fn fill_cells_parallel(&mut self, workers: usize) {
        let (min, inv, dims) = (self.config.min, self.inv_cell_size, self.dims);
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
                        *slot = cell_of_parts(min, inv, dims, entry.position);
                    }
                });
            }
        });
    }

    /// Count how many bodies land in each cell, one slice of the window table per worker.
    ///
    /// Workers are sliced by *block range*, not by equal cell counts, so each block's total has exactly one
    /// writer and the accumulate needs no atomics — which is also what keeps the output independent of which
    /// worker finishes first (ADR 0057).
    ///
    /// Each worker totals its own blocks from the window it has just written. That is a second walk over its own
    /// slice, a few hundred kilobytes hot in L2, which measured cheaper than keeping a block slot in the inner
    /// loop: the pass is memory-bound and the inner loop already reads every cell index to filter for its range.
    #[cfg(feature = "threads")]
    fn count_cells_parallel(&mut self, workers: usize) {
        let slack_percent = self.slack_percent;
        let slack_floor = self.slack_floor;
        let cells_in_grid = self.cell_count();
        let blocks = self.block_count();
        self.reset_tables();
        self.block_totals.clear();
        self.block_totals.resize(blocks, 0);

        let Self {
            cell_start,
            cell_len,
            cells,
            block_totals,
            ..
        } = self;
        let cell_list: &[u32] = cells;
        let per = blocks.div_ceil(workers.max(1)).max(1);
        thread::scope(|scope| {
            let mut rest: &mut [u32] = cell_start.as_mut_slice();
            let mut lens_rest: &mut [u32] = cell_len.as_mut_slice();
            let mut totals_rest: &mut [u32] = block_totals.as_mut_slice();
            let mut at = 0_usize;
            let mut lens_at = 0_usize;
            let mut totals_at = 0_usize;
            let mut lo_block = 0_usize;
            for _ in 0..workers {
                let hi_block = (lo_block + per).min(blocks);
                if lo_block >= hi_block {
                    break;
                }
                let lo = block_span(lo_block, cells_in_grid, blocks).0;
                let hi = block_span(hi_block, cells_in_grid, blocks).0;
                let take = hi.saturating_sub(lo);
                let Some(head) = (lo + 1).checked_sub(at) else {
                    lo_block = hi_block;
                    continue;
                };
                let current = std::mem::take(&mut rest);
                let Some((_, tail)) = current.split_at_mut_checked(head) else {
                    lo_block = hi_block;
                    continue;
                };
                let Some((window, tail_rest)) = tail.split_at_mut_checked(take) else {
                    lo_block = hi_block;
                    continue;
                };
                rest = tail_rest;
                at = hi + 1;
                // The occupancy of the cells this worker owns, so it can convert its own window to capacities and
                // total them without a second pass over the array.
                let lens_now = std::mem::take(&mut lens_rest);
                let Some((_, lens_skip)) =
                    lens_now.split_at_mut_checked(lo.saturating_sub(lens_at))
                else {
                    lo_block = hi_block;
                    continue;
                };
                let Some((lens, lens_after)) = lens_skip.split_at_mut_checked(take) else {
                    lo_block = hi_block;
                    continue;
                };
                lens_rest = lens_after;
                lens_at = hi;
                let totals_now = std::mem::take(&mut totals_rest);
                let Some((_, totals_skip)) =
                    totals_now.split_at_mut_checked(lo_block.saturating_sub(totals_at))
                else {
                    lo_block = hi_block;
                    continue;
                };
                let Some((totals, totals_after)) =
                    totals_skip.split_at_mut_checked(hi_block - lo_block)
                else {
                    lo_block = hi_block;
                    continue;
                };
                totals_rest = totals_after;
                totals_at = hi_block;
                let from = lo_block;
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
                    // Counts become capacities before anything is totalled, so the totals are of the slots the
                    // prefix scan will actually hand out. This is why the conversion lives in the count pass and not
                    // after it: doing it later would stale every total and cost a second pass over the array.
                    counts_to_capacity(window, lens, slack_percent, slack_floor);
                    // The totals this worker owns, out of the window it just filled: the counts are in L2, and
                    // no other thread can write these slots, so the result cannot depend on the schedule.
                    let mut running_window: &mut [u32] = window;
                    for (offset, slot) in totals.iter_mut().enumerate() {
                        let (_, len) = block_span(from + offset, cells_in_grid, blocks);
                        let take = len.min(running_window.len());
                        let (piece, remainder) = running_window.split_at_mut(take);
                        running_window = remainder;
                        *slot = piece.iter().copied().sum();
                    }
                });
                lo_block = hi_block;
            }
        });
    }

    /// The entry indices in one cell.
    fn cell_window(&self, cell: u32) -> &[u32] {
        let start = self.cell_start.get(cell as usize).copied().unwrap_or(0) as usize;
        let len = self.cell_len.get(cell as usize).copied().unwrap_or(0) as usize;
        self.order
            .get(start..start.saturating_add(len))
            .unwrap_or(&[])
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
/// The cells covered by one block, as `(first cell, count)`.
///
/// The first `cells % blocks` blocks take one extra cell (ADR 0055 §2). Two implementers who differ on that
/// produce different `block_base` and therefore different `cell_start`, so the rule lives here rather than in
/// each caller.
/// How many blocks a grid of `cells` cells is split into, with `requested` as the caller's override.
///
/// A function of the cell count and nothing else (ADR 0055 §6): the worker count never enters, so a grid built on
/// six cores decomposes exactly as one built on sixteen and the bytes match. `0` derives the count from
/// [`CELLS_PER_BLOCK`]; anything else is clamped rather than honoured, so a caller cannot ask for a split that is
/// slower than the serial scan or one whose blocks outnumber its cells.
fn derived_blocks(cells: usize, requested: usize) -> usize {
    if cells < PARALLEL_MIN_CELLS {
        return 1;
    }
    let asked = if requested == 0 {
        cells.div_ceil(CELLS_PER_BLOCK)
    } else {
        requested
    };
    asked.clamp(1, MAX_BLOCKS).min(cells)
}

fn block_span(index: usize, cells: usize, blocks: usize) -> (usize, usize) {
    let blocks = blocks.max(1);
    let base = cells / blocks;
    let remainder = cells % blocks;
    let extra = index.min(remainder);
    let len = base + usize::from(index < remainder);
    (index * base + extra, len)
}

fn cell_count(dims: [u32; 3]) -> u64 {
    u64::from(dims[0]) * u64::from(dims[1]) * u64::from(dims[2])
}

/// Clamp a local coordinate to a cell index on one axis.
/// Which cell a position falls into: the map every pass agrees on.
///
/// `inv_cell_size` is `1.0 / cell_size`, so this is three multiplies rather than three divisions. When the cell size is a
/// power of two the reciprocal is exact and the two are identical; for any other size they can differ by one unit in the
/// last place, which only matters for a position sitting exactly on a cell boundary, where either cell is defensible.
/// That is the trade ADR 0060 asked for: the division was most of this pass, and this pass was most of the rebuild.
fn cell_of_parts(min: Vec3, inv_cell_size: f32, dims: [u32; 3], position: Vec3) -> u32 {
    let local = (position - min) * inv_cell_size;
    let x = clamp_axis(local.x, dims[0]);
    let y = clamp_axis(local.y, dims[1]);
    let z = clamp_axis(local.z, dims[2]);
    (z * dims[1] + y) * dims[0] + x
}

/// Truncate a local coordinate to a cell index, without a branch per axis.
///
/// A coordinate that is not finite collapses to cell zero: a body with a NaN position would otherwise index a cell that
/// does not exist, and the pass has to stay total. Everything else is lifted to zero with one `max` and truncated by
/// `f32_to_u32`, which is a saturating convert — no compare-and-branch on the hot path, and the same answer as the
/// compare-and-clamp it replaces, because truncation is monotonic and the conversion saturates rather than wrapping.
fn clamp_axis(value: f32, cells: u32) -> u32 {
    if !value.is_finite() {
        return 0;
    }
    num::f32_to_u32(value.max(0.0)).min(cells.saturating_sub(1))
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
    /// The map every pass agrees on: boundaries, out-of-range coordinates and non-finite ones.
    ///
    /// The fill pass multiplies by a cached reciprocal rather than dividing (ADR 0060), so this also pins what that
    /// means at the edges: a coordinate below the volume, above it, NaN or infinite lands somewhere defined rather than
    /// somewhere undefined, and the fill pass has to agree with `cell_of` for every one of them.
    fn cell_of_is_clamped_and_agrees_with_the_fill_pass() {
        let config = SpatialGridConfig::try_new(Vec3::splat(-8.0), Vec3::splat(8.0), 8.0).or_die();
        let mut grid = SpatialGrid::new(config).or_die();
        assert_eq!(grid.dims, [2, 2, 2]);
        let cases: [(Vec3, u32, &str); 8] = [
            (Vec3::splat(-8.0), 0, "the volume's own minimum"),
            (Vec3::new(-1.0, -1.0, -1.0), 0, "just inside the low half"),
            (
                Vec3::new(0.0, 0.0, 0.0),
                7,
                "the boundary itself, in the high half",
            ),
            (Vec3::splat(7.999), 7, "just inside the volume"),
            (Vec3::splat(1_000.0), 7, "far past the maximum"),
            (Vec3::splat(f32::NEG_INFINITY), 0, "negative infinity"),
            (Vec3::splat(f32::INFINITY), 0, "positive infinity"),
            (Vec3::splat(f32::NAN), 0, "not a number"),
        ];
        for (position, expected, what) in cases {
            assert_eq!(grid.cell_of(position), expected, "{what}");
        }
        grid.extend_from(cases.iter().enumerate().map(|(index, (position, ..))| {
            (
                NpcId::unique(num::usize_to_u32(index)),
                *position,
                Vec3::ZERO,
            )
        }));
        grid.build();
        for (body, (position, expected, what)) in cases.iter().enumerate() {
            assert_eq!(grid.cell_of(*position), *expected, "{what}");
            assert_eq!(
                grid.cells.get(body).copied(),
                Some(*expected),
                "the fill pass disagrees with cell_of at {what}"
            );
        }
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
            // The cursor is seated by the same walk that produces the starts (ADR 0057), and each worker writes the
            // slots for its own cells, so a boundary slot written by the wrong worker would show up here rather than
            // as a placement that is merely a little wrong.
            assert_eq!(
                parallel.cell_cursor, serial.cell_cursor,
                "the cursor table differs at {threads} threads"
            );
            assert_eq!(
                parallel.order, serial.order,
                "placement order differs at {threads} threads"
            );
        }
    }

    /// A byte view of an array, for tests that compare arrays as bytes rather than element by element.
    fn as_bytes(values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_ne_bytes())
            .collect()
    }

    #[test]
    fn placement_leaves_the_window_table_alone() {
        // ADR 0056. The old shape let `place` walk `cell_start` up to the window ends and then shifted the whole
        // array back down, so the table a caller read was a repaired copy of a buffer placement had been writing
        // to. This test is the postcondition that shape could not state: after a rebuild, every entry of
        // `cell_start` is the number of bodies in the cells before it, computed here from the bodies alone.
        let config =
            SpatialGridConfig::try_new(Vec3::splat(-100.0), Vec3::splat(100.0), 4.0).or_die();
        let mut grid = SpatialGrid::new(config).or_die();
        let positions: Vec<Vec3> = (0..2_000)
            .map(|index| {
                let step = num::u32_to_f32(num::usize_to_u32(index));
                Vec3::new(
                    (step * 0.7).sin() * 90.0,
                    (step * 0.3).cos() * 90.0,
                    (step * 0.11).sin() * 90.0,
                )
            })
            .collect();
        grid.extend_from(positions.iter().enumerate().map(|(index, position)| {
            (
                NpcId::unique(num::usize_to_u32(index)),
                *position,
                Vec3::ZERO,
            )
        }));
        grid.build();

        // The counts, as the cell list alone can produce them.
        let mut counts = vec![0_u32; grid.cell_count()];
        for position in &positions {
            let cell = grid.cell_of(*position);
            if let Some(slot) = counts.get_mut(cell as usize) {
                *slot += 1;
            }
        }
        assert_eq!(
            grid.cell_len, counts,
            "the occupancy is not what the cell list says it is"
        );
        // And the window table is the scan's output over *capacities*: each cell owns its occupants plus the slack a
        // patch spends, so the offsets are a prefix of capacities rather than of counts (ADR 0064).
        let mut expected = vec![0_u32; grid.cell_count() + 1];
        for (cell, count) in counts.iter().enumerate() {
            let below = expected.get(cell).copied().unwrap_or(0);
            if let Some(slot) = expected.get_mut(cell + 1) {
                *slot = below.saturating_add(capacity_for(
                    *count,
                    grid.slack_percent(),
                    grid.slack_floor(),
                ));
            }
        }
        assert_eq!(
            grid.cell_start, expected,
            "the window table is not the scan's output, so placement is still writing through it"
        );
        // And the cursor is the scratch that absorbed the advance: each run ends where its window starts, plus its
        // occupancy, which is the value the old shape left in `cell_start` and then had to shift back out.
        for cell in 0..grid.cell_count() {
            let start = grid.cell_start.get(cell).copied().unwrap_or(0);
            let len = grid.cell_len.get(cell).copied().unwrap_or(0);
            assert_eq!(
                grid.cell_cursor.get(cell).copied(),
                Some(start.saturating_add(len)),
                "cursor for cell {cell} did not finish at the run's end"
            );
        }
    }

    #[test]
    fn the_slack_rule_keeps_every_run_at_least_as_wide_as_its_bodies() {
        // The rule the patch spends, checked as arithmetic: a run always has room for the bodies in it, its spare slots
        // are the larger of the percentage and the floor, and an empty cell still owns one slot of its own plus the
        // floor, because that floor is the only thing standing between it and a rebuild on the next pair of arrivals.
        for percent in [0_u8, 10, 25, 50, 100] {
            for floor in [0_u32, 1, 2, 4] {
                for count in [0_u32, 1, 2, 3, 4, 5, 17, 1_000, 640_000] {
                    let capacity = capacity_for(count, percent, floor);
                    assert!(
                        capacity > count,
                        "{percent}%/{floor}: {count} bodies would fill {capacity} slots with no room to move"
                    );
                    let spare = capacity - count - 1;
                    let percent_slots = u32::from(percent).saturating_mul(count) / 100;
                    assert_eq!(
                        spare,
                        percent_slots.max(floor),
                        "{percent}%/{floor}: {count} bodies got {spare} spare slots"
                    );
                }
            }
        }
        // The percentage-only rule is still reachable, which is what makes this an addition rather than a change.
        assert_eq!(capacity_for(100, 25, 0), 126);
        assert_eq!(capacity_for(100, 50, 0), 151);
        // An empty cell: a percentage of zero is zero, so only the floor can make room for the pair that lands together.
        assert_eq!(capacity_for(0, 50, 0), 1);
        assert_eq!(capacity_for(0, 50, 1), 2);
        assert_eq!(capacity_for(0, 50, DEFAULT_SLACK_FLOOR), 3);
        // The floor dominates only while the percentage is smaller, which is the sparse end of the grid.
        assert_eq!(
            capacity_for(4, 25, 2),
            7,
            "1 percent slot yields to a floor of 2"
        );
        assert_eq!(capacity_for(400, 25, 2), 501, "100 percent slots win");
        // Both setters clamp rather than letting a number mean nothing.
        assert_eq!(clamp_slack(255), 100);
        assert_eq!(clamp_slack(50), 50);
        assert_eq!(clamp_floor(0), 0);
        assert_eq!(clamp_floor(u32::MAX), MAX_SLACK_FLOOR);
    }
    #[test]
    fn the_slack_is_read_at_build_time_and_defaults_to_a_quarter_plus_a_pair() {
        let config =
            SpatialGridConfig::try_new(Vec3::splat(-50.0), Vec3::splat(50.0), 4.0).or_die();
        let grid = SpatialGrid::new(config).or_die();
        assert_eq!(grid.slack_percent(), DEFAULT_SLACK_PERCENT);
        assert_eq!(grid.slack_floor(), DEFAULT_SLACK_FLOOR);
        let tuned = SpatialGrid::new(config)
            .or_die()
            .with_slack_percent(50)
            .with_slack_floor(0);
        assert_eq!(tuned.slack_percent(), 50);
        assert_eq!(tuned.slack_floor(), 0);
        assert_eq!(
            SpatialGrid::new(config)
                .or_die()
                .with_slack_percent(200)
                .with_slack_floor(u32::MAX)
                .slack_percent(),
            100
        );
        assert_eq!(
            SpatialGrid::new(config)
                .or_die()
                .with_slack_floor(u32::MAX)
                .slack_floor(),
            MAX_SLACK_FLOOR
        );
    }
    #[test]
    fn blocks_partition_the_cells_without_gaps_or_overlap() {
        // The remainder rule from ADR 0055 §2, checked as arithmetic rather than trusted: blocks must tile
        // the cell array exactly, and the first `cells % blocks` of them must be the long ones.
        for cells in [1_usize, 3, 100, 4_096, 421_876] {
            for blocks in [1_usize, 2, 7, 128, 1_024, 4_096] {
                if blocks > cells {
                    continue;
                }
                let short = cells / blocks;
                let long = cells.div_ceil(blocks);
                let mut next = 0;
                for block in 0..blocks {
                    let (first, len) = block_span(block, cells, blocks);
                    assert_eq!(first, next, "block {block} of {blocks} starts late");
                    assert!(
                        len == short || len == long,
                        "block {block} of {blocks} has {len} cells, expected {short} or {long}"
                    );
                    if block < cells % blocks {
                        assert_eq!(len, long, "the long blocks must come first");
                    }
                    next += len;
                }
                assert_eq!(next, cells, "blocks left cells uncovered");
            }
        }
    }

    #[test]
    fn the_fused_totals_match_a_recount_of_the_cell_list() {
        // ADR 0057: `count` now owns the per-block totals, and the scan reads them instead of recomputing them.
        // Those numbers come out of a partition — worker `w` sums the window it just filled — so they are the
        // part most likely to be quietly wrong: a block boundary off by one sums the right cells into the wrong
        // block, and every window after it shifts while the total still comes out right. Recounted here from the
        // cell list itself, one histogram then one window sum per block, which is the arithmetic the parallel
        // path replaced rather than a re-run of it.
        for threads in [1_usize, 6] {
            for blocks in [128_usize, 1_024, MAX_BLOCKS] {
                let config =
                    SpatialGridConfig::try_new(Vec3::splat(-400.0), Vec3::splat(400.0), 6.0)
                        .or_die();
                let mut grid = SpatialGrid::new(config)
                    .or_die()
                    .with_threads(threads)
                    .with_blocks(blocks);
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
                grid.build_measured();
                assert_eq!(
                    grid.block_count(),
                    blocks,
                    "the split was clamped, so this is not the case under test"
                );
                let cells = grid.cell_count();
                let mut counted = vec![0_u32; cells + 1];
                for cell in &grid.cells {
                    if let Some(slot) = counted.get_mut(*cell as usize + 1) {
                        *slot += 1;
                    }
                }
                if threads == 1 {
                    // A single worker takes the serial scan, which reads the counts in place and never asks
                    // where a block begins — so there are no totals to check, and their absence is the point:
                    // summing them would have cost this rebuild a pass it does not use.
                    assert!(
                        grid.block_totals.is_empty(),
                        "threads = 1, blocks = {blocks}: a serial scan was given totals it never reads"
                    );
                    continue;
                }
                assert_eq!(
                    grid.block_totals.len(),
                    blocks,
                    "threads = {threads}, blocks = {blocks}: wrong number of totals"
                );
                // The totals are sums of *capacities*, not of counts: the scan hands out slots from the capacity prefix,
                // so a total of counts would be a base pointing into the wrong cell (ADR 0064).
                for slot in counted.iter_mut().skip(1) {
                    *slot = capacity_for(*slot, grid.slack_percent(), grid.slack_floor());
                }
                for block in 0..blocks {
                    let (first, len) = block_span(block, cells, blocks);
                    let want: u32 = counted
                        .get(first + 1..first + 1 + len)
                        .map_or(0, |window| window.iter().copied().sum());
                    assert_eq!(
                        grid.block_totals.get(block).copied(),
                        Some(want),
                        "threads = {threads}, blocks = {blocks}, block = {block}: fused total is wrong"
                    );
                }
                let total: u32 = grid.block_totals.iter().copied().sum();
                assert_eq!(
                    total,
                    grid.cell_start.get(cells).copied().unwrap_or(0),
                    "threads = {threads}, blocks = {blocks}: totals do not add up to the table"
                );
                let occupied: u32 = grid.cell_len.iter().copied().sum();
                assert_eq!(
                    occupied,
                    num::usize_to_u32(grid.entries.len()),
                    "threads = {threads}, blocks = {blocks}: the occupancy is not the population"
                );
            }
        }
    }
    #[test]
    fn the_block_count_derives_from_the_cells_and_not_the_workers() {
        // ADR 0055 §6. If this ever fails, the decomposition has started depending on the machine, and the
        // byte-equality test starts passing for the wrong reason.
        let config =
            SpatialGridConfig::try_new(Vec3::splat(-400.0), Vec3::splat(400.0), 9.0).or_die();
        let blocks_for = |threads: usize, blocks: usize| {
            SpatialGrid::new(config)
                .or_die()
                .with_threads(threads)
                .with_blocks(blocks)
                .block_count()
        };
        let cells = SpatialGrid::new(config).or_die().cell_count();
        assert!(cells >= PARALLEL_MIN_CELLS);
        let derived = blocks_for(1, 0);
        assert_eq!(derived, cells.div_ceil(CELLS_PER_BLOCK));
        for threads in [2, 3, 8, 64] {
            assert_eq!(blocks_for(threads, 0), derived, "threads = {threads}");
        }
        assert_eq!(blocks_for(8, 7), 7);
        assert_eq!(blocks_for(8, usize::MAX), MAX_BLOCKS);
    }

    #[test]
    fn the_block_constant_is_pinned_by_its_bytes_and_its_table() {
        // Phase 5 T04 swept the constant and kept it (docs/dev/tuning/performance.md §*Picking CELLS_PER_BLOCK*), so these
        // numbers are literal on purpose: editing `CELLS_PER_BLOCK` has to fail *here*, and the failure has to send
        // the reader back to the sweep. A test written as `cells.div_ceil(CELLS_PER_BLOCK)` would move with the
        // constant and prove only that it is self-consistent.
        assert_eq!(
            CELLS_PER_BLOCK, 4_096,
            "the sweep chose this value; re-run it before changing it"
        );
        assert_eq!(
            CELLS_PER_BLOCK * core::mem::size_of::<u32>(),
            16_384,
            "one block's window slice must stay L1-sized: the cursor slice beside it is the same again"
        );
        // Cells per block -> blocks, at the points where the rule bends: the parallel floor, the sweep's own
        // fixture, a round million, and the point where MAX_BLOCKS takes over from the derived count.
        for (cells, blocks) in [
            (65_535_usize, 1),
            (65_536, 16),
            (131_072, 32),
            (421_876, 103),
            (1_000_000, 245),
            (16_777_216, MAX_BLOCKS),
            (64_000_000, MAX_BLOCKS),
        ] {
            assert_eq!(
                derived_blocks(cells, 0),
                blocks,
                "{cells} cells derived {} blocks, expected {blocks}",
                derived_blocks(cells, 0)
            );
        }
        // And the override is clamped, not trusted, at both walls.
        assert_eq!(derived_blocks(65_536, usize::MAX), MAX_BLOCKS);
        assert_eq!(derived_blocks(421_876, 1), 1);
        assert_eq!(derived_blocks(421_876, 200_000), MAX_BLOCKS);
    }
    #[test]
    fn small_grids_keep_the_serial_scan_whatever_the_caller_asks_for() {
        // ADR 0055 §7: the override is clamped, not honoured, so a caller cannot make a small grid slower and
        // then report the regression against this crate.
        let config =
            SpatialGridConfig::try_new(Vec3::splat(-40.0), Vec3::splat(40.0), 2.0).or_die();
        let mut grid = SpatialGrid::new(config).or_die().with_blocks(4_096);
        assert!(grid.cell_count() < PARALLEL_MIN_CELLS);
        assert_eq!(grid.block_count(), 1);
        grid.extend_from((0..32).map(|index| {
            let step = num::u32_to_f32(num::usize_to_u32(index));
            (
                NpcId::unique(num::usize_to_u32(index)),
                Vec3::new(step * 1.5, 0.0, step * -0.75),
                Vec3::ZERO,
            )
        }));
        assert_eq!(
            grid.build_measured().blocks,
            1,
            "a small grid must stay serial"
        );
    }

    #[test]
    fn every_block_count_builds_identical_windows() {
        // ADR 0055: `blocks` changes what the scan costs and never what it produces. Compared as bytes rather
        // than element by element, because element-wise equality would pass while the padding between two
        // arrays differed, and `(workers x blocks)` is the matrix the ADR asks for.
        let build = |threads: usize, blocks: usize| {
            let config =
                SpatialGridConfig::try_new(Vec3::splat(-400.0), Vec3::splat(400.0), 6.0).or_die();
            let mut grid = SpatialGrid::new(config)
                .or_die()
                .with_threads(threads)
                .with_blocks(blocks);
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
            let costs = grid.build_measured();
            (grid, costs)
        };

        let (reference, reference_costs) = build(1, 1);
        assert_eq!(
            reference_costs.blocks, 1,
            "blocks = 1 must route to the serial reference, or this test compares the blocked scan with itself"
        );
        let want_start = as_bytes(&reference.cell_start);
        let want_order = as_bytes(&reference.order);
        for threads in [1, 2, 3, 8] {
            for blocks in [1, 128, 1_024, MAX_BLOCKS] {
                let (grid, costs) = build(threads, blocks);
                assert_eq!(
                    costs.blocks, blocks,
                    "threads = {threads}, blocks = {blocks}: the requested split was clamped"
                );
                assert_eq!(
                    as_bytes(&grid.cell_start),
                    want_start,
                    "prefix sums differ at {threads} threads and {blocks} blocks"
                );
                assert_eq!(
                    as_bytes(&grid.order),
                    want_order,
                    "placement order differs at {threads} threads and {blocks} blocks"
                );
            }
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

#[cfg(test)]
#[path = "spatial_incremental.rs"]
mod incremental_tests;
