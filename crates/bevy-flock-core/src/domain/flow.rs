//! Flow fields: one solve per destination, one lookup per agent.
//!
//! A crowd of ten thousand walking to the same market square cannot afford ten
//! thousand searches. It can afford one: a Dijkstra over the coarse navigation
//! grid, stored as a direction per cell, sampled in constant time by every agent
//! that wants to know which way the square is.
//!
//! Three properties make the difference between this and a per-agent pathfinder:
//!
//! - **Integer costs.** Ties are broken by cell index, so the same obstacles
//!   produce the same field on every machine and every run.
//! - **Resumable solves.** A large field may take longer than a frame allows, so
//!   the solver advances a node budget per call and reports progress instead of
//!   blocking. A game that only calls it once gets the same answer, later.
//! - **Versioned caching.** A [`NavVersion`] stamp on every cached field means a
//!   door that opens invalidates the fields built against the old layout, instead
//!   of leaving a crowd walking through a wall that is no longer there.

use crate::domain::nav::{NavBounds, NavCell, NavSource, NavVersion};
use crate::domain::num;
use bevy_math::{Vec2, Vec3};
use smallvec::SmallVec;
use std::collections::BinaryHeap;

/// Bytes one cached field costs beyond its two `Vec`s, for the memory report.
pub const FLOW_FIELD_OVERHEAD_BYTES: usize = core::mem::size_of::<FlowField>();

/// Where a field is heading.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlowTarget {
    /// The cell whose centre is the goal.
    pub cell: NavCell,
    /// The exact point an agent should reach inside that cell.
    pub point: Vec3,
}

impl FlowTarget {
    /// A target at the centre of a cell.
    #[must_use]
    pub fn at_cell(source: &dyn NavSource, cell: NavCell) -> Self {
        Self {
            cell,
            point: source.center_of(cell),
        }
    }
}

/// A solved field: cost from every cell to the target, and a direction to step.
#[derive(Debug, Clone)]
pub struct FlowField {
    bounds: NavBounds,
    version: NavVersion,
    target: FlowTarget,
    cost: Vec<u32>,
    dir: Vec<Vec2>,
    /// The tag of the link each cell was reached through, or 0. Empty when the source had
    /// no links, so a game that never opens a door pays nothing for the feature.
    via: Vec<u16>,
    reachable: u32,
}

impl FlowField {
    /// The bounds this field was solved over.
    #[must_use]
    pub const fn bounds(&self) -> NavBounds {
        self.bounds
    }

    /// The target.
    #[must_use]
    pub const fn target(&self) -> FlowTarget {
        self.target
    }

    /// The navigation version this field was solved against.
    #[must_use]
    pub const fn version(&self) -> NavVersion {
        self.version
    }

    /// How many cells can reach the target.
    #[must_use]
    pub const fn reachable_cells(&self) -> u32 {
        self.reachable
    }

    /// The integer cost from a cell, or `None` if it cannot reach the target.
    #[must_use]
    pub fn cost_at(&self, cell: NavCell) -> Option<u32> {
        let raw = *self.cost.get(cell.index(self.bounds.width))?;
        (raw != u32::MAX).then_some(raw)
    }

    /// The stored direction for a cell, before sampling.
    #[must_use]
    pub fn direction_at_cell(&self, cell: NavCell) -> Vec2 {
        self.dir
            .get(cell.index(self.bounds.width))
            .copied()
            .unwrap_or(Vec2::ZERO)
    }

    /// The direction an agent at `position` should move in.
    ///
    /// Bilinear between cell centres so a crowd does not move in visible 7-metre
    /// steps, and a direct seek inside the target cell so the last metre is a walk
    /// rather than a gradient with nothing left to descend.
    #[must_use]
    pub fn sample(&self, position: Vec3, source: &dyn NavSource) -> Vec2 {
        let Some(cell) = source.cell_of(position) else {
            return Vec2::ZERO;
        };
        if cell == self.target.cell {
            let to_target = self.target.point - position;
            let planar = Vec2::new(to_target.x, to_target.z);
            return planar.normalize_or_zero();
        }
        let size = self.bounds.cell_size;
        let local_x = (position.x - self.bounds.origin.x) / size - 0.5;
        let local_y = (position.z - self.bounds.origin.z) / size - 0.5;
        let base_x = local_x.floor();
        let base_y = local_y.floor();
        let fx = (local_x - base_x).clamp(0.0, 1.0);
        let fy = (local_y - base_y).clamp(0.0, 1.0);
        let at = |dx: f32, dy: f32| -> Vec2 {
            let x = base_x + dx;
            let y = base_y + dy;
            if x < 0.0 || y < 0.0 {
                return Vec2::ZERO;
            }
            let (x, y) = (num::f32_to_u16(x), num::f32_to_u16(y));
            if x >= self.bounds.width || y >= self.bounds.height {
                return Vec2::ZERO;
            }
            self.direction_at_cell(NavCell::new(x, y))
        };
        let blend = at(0.0, 0.0) * ((1.0 - fx) * (1.0 - fy))
            + at(1.0, 0.0) * (fx * (1.0 - fy))
            + at(0.0, 1.0) * ((1.0 - fx) * fy)
            + at(1.0, 1.0) * (fx * fy);
        blend.normalize_or_zero()
    }

    /// The tag of the link a cell was reached through, if any.
    ///
    /// This is how a behaviour tree learns that an agent's route uses the ladder rather
    /// than the stairs: `flow.link_tag_at(cell) == Some(LinkTag::new(1))` is a fact, not a
    /// guess, and it costs two bytes per cell only in a level that has links.
    #[must_use]
    pub fn link_tag_at(&self, cell: NavCell) -> Option<crate::domain::links::LinkTag> {
        let tag = *self.via.get(cell.index(self.bounds.width))?;
        (tag != 0).then_some(crate::domain::links::LinkTag(tag))
    }

    /// Whether this field followed any links.
    #[must_use]
    pub const fn has_links(&self) -> bool {
        !self.via.is_empty()
    }

    /// Bytes this field occupies.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        FLOW_FIELD_OVERHEAD_BYTES
            + self.cost.len() * core::mem::size_of::<u32>()
            + self.dir.len() * 8
            + self.via.len() * core::mem::size_of::<u16>()
    }

    /// Build the direction array from a completed cost field.
    fn finish(mut self, source: &dyn NavSource) -> Self {
        let width = self.bounds.width;
        let height = self.bounds.height;
        let mut reachable = 0_u32;
        for y in 0..height {
            for x in 0..width {
                let cell = NavCell::new(x, y);
                let index = cell.index(width);
                let Some(&own) = self.cost.get(index) else {
                    continue;
                };
                if own == u32::MAX {
                    continue;
                }
                reachable += 1;
                if cell == self.target.cell {
                    if let Some(slot) = self.dir.get_mut(index) {
                        *slot = Vec2::ZERO;
                    }
                    continue;
                }
                let mut sum = Vec2::ZERO;
                // Ordinary steps and links both descend the field: a route that takes the
                // ladder has to pull toward the ladder, or the desired velocity would walk
                // an agent at a wall it is supposed to climb.
                let mut nexts: SmallVec<[NavCell; 12]> = SmallVec::new();
                let mut neighbours: SmallVec<[(NavCell, u32); 8]> = SmallVec::new();
                source.neighbours(cell, &mut neighbours);
                nexts.extend(neighbours.iter().map(|(next, _)| *next));
                let mut links: SmallVec<[(NavCell, u32, crate::domain::links::LinkTag); 4]> =
                    SmallVec::new();
                source.links_leaving(cell, &mut links);
                nexts.extend(links.iter().map(|(next, _, _)| *next));
                // Record the tag on the cell that *takes* the link, which is where an agent
                // has to decide to climb rather than where it lands, and is the fact a tree
                // wants. The cheapest link that still descends the field wins.
                let mut best_link: Option<(u32, u16)> = None;
                for (next, _step, tag) in links.iter().copied() {
                    let Some(&next_cost) = self.cost.get(next.index(width)) else {
                        continue;
                    };
                    if next_cost == u32::MAX || next_cost >= own {
                        continue;
                    }
                    let drop = own - next_cost;
                    if best_link.is_none_or(|(best, _)| drop > best) {
                        best_link = Some((drop, tag.0));
                    }
                }
                if let Some((_, tag)) = best_link
                    && let Some(slot) = self.via.get_mut(index)
                {
                    *slot = tag;
                }
                for next in nexts {
                    let Some(&next_cost) = self.cost.get(next.index(width)) else {
                        continue;
                    };
                    if next_cost == u32::MAX || next_cost >= own {
                        continue;
                    }
                    // Descend the cost field: weight each edge by the drop it offers, so
                    // the direction follows the cheapest way down rather than the most
                    // geometrically direct one.
                    let delta = source.center_of(next) - source.center_of(cell);
                    let planar = Vec2::new(delta.x, delta.z);
                    let drop = f32::from(
                        u16::try_from((own - next_cost).min(u32::from(u16::MAX)))
                            .unwrap_or(u16::MAX),
                    );
                    sum += planar.normalize_or_zero() * drop;
                }
                if let Some(slot) = self.dir.get_mut(index) {
                    *slot = sum.normalize_or_zero();
                }
            }
        }
        self.reachable = reachable;
        self
    }
}

/// How a resumable solve is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolveProgress {
    /// Still working; this many nodes have been expanded in total.
    Running {
        /// Nodes expanded so far.
        expanded: u32,
    },
    /// Finished.
    Complete,
}

/// A solve in progress, advanced a node budget at a time.
#[derive(Debug, Clone)]
pub struct FlowSolver {
    bounds: NavBounds,
    version: NavVersion,
    target: FlowTarget,
    cost: Vec<u32>,
    /// Link tags per cell, empty when the source has no links.
    via: Vec<u16>,
    heap: BinaryHeap<std::cmp::Reverse<(u32, u32)>>,
    expanded: u32,
    started: bool,
}

impl FlowSolver {
    /// Begin a solve toward a target.
    #[must_use]
    pub fn new(source: &dyn NavSource, target: FlowTarget) -> Self {
        let bounds = source.bounds();
        let mut cost = vec![u32::MAX; bounds.cell_count()];
        if let Some(slot) = cost.get_mut(target.cell.index(bounds.width)) {
            *slot = 0;
        }
        let mut heap = BinaryHeap::new();
        heap.push(std::cmp::Reverse((
            0,
            num::usize_to_u32(target.cell.index(bounds.width)),
        )));
        let via = if source.has_links() {
            vec![0_u16; bounds.cell_count()]
        } else {
            Vec::new()
        };
        Self {
            bounds,
            version: source.version(),
            target,
            cost,
            via,
            heap,
            expanded: 0,
            started: false,
        }
    }

    /// Whether the solve has finished.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.heap.is_empty() && self.started
    }

    /// Nodes expanded so far.
    #[must_use]
    pub const fn expanded(&self) -> u32 {
        self.expanded
    }

    /// Advance the solve, expanding at most `budget` nodes.
    ///
    /// `budget` is a node count rather than a duration: a duration would make the
    /// solve depend on how loaded the machine was, and the whole point of the
    /// integer costs is that the answer does not.
    pub fn step(&mut self, source: &dyn NavSource, budget: u32) -> SolveProgress {
        self.started = true;
        let width = self.bounds.width;
        let mut spent = 0;
        while spent < budget {
            let Some(std::cmp::Reverse((cost, index))) = self.heap.pop() else {
                return SolveProgress::Complete;
            };
            let index = index as usize;
            let Some(&known) = self.cost.get(index) else {
                continue;
            };
            if cost > known {
                // A stale heap entry for a cell that has since been improved.
                continue;
            }
            spent += 1;
            self.expanded = self.expanded.saturating_add(1);
            let cell = NavCell::new(
                num::usize_to_u16(index % width as usize),
                num::usize_to_u16(index / width as usize),
            );
            let mut neighbours: SmallVec<[(NavCell, u32); 8]> = SmallVec::new();
            source.neighbours(cell, &mut neighbours);
            for (next, step) in neighbours {
                self.relax(next, step, cost);
            }
            let mut links: SmallVec<[(NavCell, u32, crate::domain::links::LinkTag); 4]> =
                SmallVec::new();
            // Backward: the search grows from the target, so it crosses a link in reverse
            // and charges its cost to the cell on the far side.
            source.links_into(cell, &mut links);
            for (next, step, _tag) in links {
                self.relax(next, step, cost);
            }
        }
        if self.heap.is_empty() {
            SolveProgress::Complete
        } else {
            SolveProgress::Running {
                expanded: self.expanded,
            }
        }
    }

    /// Lower the cost of a cell if this route is cheaper.
    fn relax(&mut self, next: NavCell, step: u32, index_cost: u32) {
        let next_index = next.index(self.bounds.width);
        let candidate = index_cost.saturating_add(step);
        let Some(slot) = self.cost.get_mut(next_index) else {
            return;
        };
        if candidate < *slot {
            *slot = candidate;
            self.heap.push(std::cmp::Reverse((
                candidate,
                num::usize_to_u32(next_index),
            )));
        }
    }

    /// Finish the solve in one call.
    #[must_use]
    pub fn solve(mut self, source: &dyn NavSource) -> FlowField {
        while !self.heap.is_empty() {
            let _ = self.step(source, u32::MAX / 2);
        }
        self.into_field(source)
    }

    /// Turn a completed solve into a field.
    #[must_use]
    pub fn into_field(self, source: &dyn NavSource) -> FlowField {
        FlowField {
            bounds: self.bounds,
            version: self.version,
            target: self.target,
            cost: self.cost,
            dir: vec![Vec2::ZERO; self.bounds.cell_count()],
            via: self.via,
            reachable: 0,
        }
        .finish(source)
    }
}

/// Build a field in one call.
#[must_use]
pub fn solve(source: &dyn NavSource, target: FlowTarget) -> FlowField {
    FlowSolver::new(source, target).solve(source)
}

/// A cache key: where the field goes and which layout it was solved against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FlowKey {
    /// The target cell.
    pub cell: NavCell,
    /// The navigation version.
    pub version: NavVersion,
}

/// Where a lookup came from, for the stats snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowLookup {
    /// The field was already cached.
    Hit,
    /// The field was built and cached.
    Rebuilt,
    /// The field was built but could not be kept: the cache is full of newer work.
    RebuiltUncached,
}

/// An LRU cache of solved fields.
///
/// Bounded by construction: `capacity` fields, each one `cell_count` costs and the
/// same number of directions. A city with four destinations and a cache of four
/// costs eight bytes per cell per destination, which is the number the memory
/// report quotes.
#[derive(Debug, Clone)]
pub struct FlowCache {
    capacity: usize,
    entries: Vec<CacheEntry>,
    clock: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    key: FlowKey,
    stamp: u64,
    field: FlowField,
}

impl FlowCache {
    /// A cache holding at most `capacity` fields.
    ///
    /// # Errors
    ///
    /// Returns [`FlowCacheError::ZeroCapacity`] for a capacity of zero: a cache
    /// that can hold nothing is a bug waiting to be mistaken for a policy.
    pub fn new(capacity: usize) -> Result<Self, FlowCacheError> {
        if capacity == 0 {
            return Err(FlowCacheError::ZeroCapacity);
        }
        Ok(Self {
            capacity,
            entries: Vec::with_capacity(capacity.min(64)),
            clock: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
        })
    }

    /// How many fields the cache may hold.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// How many fields it currently holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Cache hits since construction.
    #[must_use]
    pub const fn hits(&self) -> u64 {
        self.hits
    }

    /// Cache misses since construction.
    #[must_use]
    pub const fn misses(&self) -> u64 {
        self.misses
    }

    /// Fields evicted since construction.
    #[must_use]
    pub const fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Bytes the cached fields occupy.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.entries.iter().map(|entry| entry.field.bytes()).sum()
    }

    /// Drop every field built against an older navigation version.
    pub fn invalidate_stale(&mut self, version: NavVersion) -> usize {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.key.version == version);
        before - self.entries.len()
    }

    /// Look up a field, or build and cache it.
    ///
    /// # Errors
    ///
    /// As [`FlowCache::new`] at construction; lookups themselves cannot fail.
    pub fn field_for(
        &mut self,
        source: &dyn NavSource,
        target: FlowTarget,
    ) -> (FlowLookup, &FlowField) {
        self.clock = self.clock.wrapping_add(1);
        let key = FlowKey {
            cell: target.cell,
            version: source.version(),
        };
        // Every mutation happens first and exactly one borrow is taken at the end. Returning a
        // reference derived from `&mut self` and then touching the cache again in the same body
        // does not borrow-check, even on a path that returns: the returned reference is good for
        // the whole `&mut self` lifetime.
        let hit = self.entries.iter().position(|entry| entry.key == key);
        let (lookup, position) = if let Some(position) = hit {
            self.hits += 1;
            let clock = self.clock;
            if let Some(entry) = self.entries.get_mut(position) {
                entry.stamp = clock;
            }
            (FlowLookup::Hit, position)
        } else {
            self.misses += 1;
            let field = solve(source, target);
            if self.entries.len() >= self.capacity {
                // Evict the least recently used entry: the oldest stamp.
                if let Some((oldest, _)) = self
                    .entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, entry)| entry.stamp)
                {
                    self.entries.swap_remove(oldest);
                    self.evictions += 1;
                }
            }
            let clock = self.clock;
            self.entries.push(CacheEntry {
                key,
                stamp: clock,
                field,
            });
            (FlowLookup::Rebuilt, self.entries.len().saturating_sub(1))
        };
        let Some(entry) = self.entries.get(position) else {
            unreachable!("the cache entry was either found or just pushed");
        };
        (lookup, &entry.field)
    }
}

/// Why a cache could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FlowCacheError {
    /// A capacity of zero.
    #[error("a flow-field cache needs room for at least one field")]
    ZeroCapacity,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::nav::{GridNavSource, NavBounds};
    use crate::testkit::{OrDie, OrDieErr};

    fn corridor() -> GridNavSource {
        // 32 x 8, wall along row 4 with a gap at x = 24, so a field has to bend.
        let mut grid = GridNavSource::open(Vec3::ZERO, 32, 8, 1.0).or_die();
        grid.add_obstacle(NavCell::new(0, 4), NavCell::new(23, 4))
            .or_die();
        grid
    }

    #[test]
    fn a_straight_run_points_at_the_target() {
        let grid = GridNavSource::open(Vec3::ZERO, 16, 16, 1.0).or_die();
        let target = FlowTarget::at_cell(&grid, NavCell::new(12, 8));
        let field = solve(&grid, target);
        let here = grid.center_of(NavCell::new(2, 8));
        let direction = field.sample(here, &grid);
        assert!(direction.x > 0.9, "expected eastward, got {direction:?}");
        assert!(direction.y.abs() < 0.1);
    }

    #[test]
    fn a_field_bends_around_a_wall() {
        let grid = corridor();
        let target = FlowTarget::at_cell(&grid, NavCell::new(30, 6));
        let field = solve(&grid, target);
        let behind_the_wall = grid.center_of(NavCell::new(4, 2));
        let direction = field.sample(behind_the_wall, &grid);
        assert!(direction.x > 0.0, "the route to the gap is eastward");
    }

    #[test]
    fn inside_the_target_cell_the_agent_seeks_the_point() {
        let grid = GridNavSource::open(Vec3::ZERO, 8, 8, 1.0).or_die();
        let target = FlowTarget {
            cell: NavCell::new(4, 4),
            point: grid.center_of(NavCell::new(4, 4)),
        };
        let field = solve(&grid, target);
        let near = Vec3::new(4.1, 0.0, 4.1);
        let direction = field.sample(near, &grid);
        assert!(
            direction.x > 0.0 && direction.y > 0.0,
            "should walk into the centre"
        );
    }

    #[test]
    fn unreachable_cells_have_no_direction() {
        let mut grid = GridNavSource::open(Vec3::ZERO, 8, 8, 1.0).or_die();
        // Wall off a corner entirely.
        grid.add_obstacle(NavCell::new(6, 0), NavCell::new(6, 7))
            .or_die();
        grid.add_obstacle(NavCell::new(0, 6), NavCell::new(7, 6))
            .or_die();
        let target = FlowTarget::at_cell(&grid, NavCell::new(2, 2));
        let field = solve(&grid, target);
        let trapped = NavCell::new(7, 7);
        assert_eq!(field.cost_at(trapped), None);
        assert_eq!(field.direction_at_cell(trapped), Vec2::ZERO);
    }

    #[test]
    fn the_solve_is_deterministic() {
        let grid = corridor();
        let target = FlowTarget::at_cell(&grid, NavCell::new(30, 2));
        let a = solve(&grid, target);
        let b = solve(&grid, target);
        assert_eq!(a.cost, b.cost);
        assert_eq!(a.dir, b.dir);
    }

    #[test]
    fn a_resumable_solve_matches_a_single_call() {
        let grid = corridor();
        let target = FlowTarget::at_cell(&grid, NavCell::new(30, 6));
        let one_shot = solve(&grid, target);
        let mut solver = FlowSolver::new(&grid, target);
        let mut rounds = 0;
        loop {
            rounds += 1;
            match solver.step(&grid, 16) {
                SolveProgress::Complete => break,
                SolveProgress::Running { expanded: _ } => {
                    assert!(rounds < 1_000, "solve never finished");
                }
            }
        }
        let staged = solver.into_field(&grid);
        assert!(rounds > 1, "the budget never split the solve");
        assert_eq!(one_shot.cost, staged.cost);
        assert_eq!(one_shot.dir, staged.dir);
    }

    #[test]
    fn the_cache_hits_then_rebuilds_only_when_the_target_changes() {
        let grid = corridor();
        let mut cache = FlowCache::new(2).or_die();
        let a = FlowTarget::at_cell(&grid, NavCell::new(30, 2));
        assert_eq!(cache.field_for(&grid, a).0, FlowLookup::Rebuilt);
        assert_eq!(cache.field_for(&grid, a).0, FlowLookup::Hit);
        assert_eq!(cache.hits(), 1);
        assert_eq!(cache.misses(), 1);
    }

    #[test]
    fn ten_thousand_rebuilds_stay_inside_the_budget() {
        let grid = corridor();
        let mut cache = FlowCache::new(4).or_die();
        let cell_count = grid.bounds().cell_count();
        let budget = 4 * (cell_count * 12 + FLOW_FIELD_OVERHEAD_BYTES);
        for step in 0..10_000_u32 {
            let x = (step % 30) as u16;
            let target = FlowTarget::at_cell(&grid, NavCell::new(x, 2));
            let _ = cache.field_for(&grid, target);
            assert!(cache.len() <= 4);
        }
        assert!(
            cache.bytes() <= budget,
            "cache grew to {} bytes",
            cache.bytes()
        );
        assert!(cache.evictions() > 0, "nothing was ever evicted");
    }

    #[test]
    fn a_rebuild_drops_fields_built_against_the_old_layout() {
        let mut grid = corridor();
        let mut cache = FlowCache::new(4).or_die();
        let target = FlowTarget::at_cell(&grid, NavCell::new(30, 2));
        let _ = cache.field_for(&grid, target);
        grid.add_obstacle(NavCell::new(10, 0), NavCell::new(10, 3))
            .or_die();
        assert_eq!(cache.invalidate_stale(grid.version()), 1);
        assert!(cache.is_empty());
    }

    #[test]
    fn a_cache_needs_room_for_one_field() {
        assert_eq!(FlowCache::new(0).or_die_err(), FlowCacheError::ZeroCapacity);
    }

    #[test]
    fn the_memory_report_adds_up() {
        let grid = GridNavSource::open(Vec3::ZERO, 64, 64, 1.0).or_die();
        let field = solve(&grid, FlowTarget::at_cell(&grid, NavCell::new(32, 32)));
        let expected = FLOW_FIELD_OVERHEAD_BYTES + 64 * 64 * 4 + 64 * 64 * 8;
        assert_eq!(field.bytes(), expected);
        assert_eq!(field.reachable_cells(), 64 * 64);
    }

    #[test]
    fn bounds_are_carried_through() {
        let bounds = NavBounds::new(Vec3::ZERO, 4, 4, 2.0).or_die();
        let grid = GridNavSource::from_bitmap(bounds, vec![true; 16]).or_die();
        let field = solve(&grid, FlowTarget::at_cell(&grid, NavCell::new(0, 0)));
        assert_eq!(field.bounds(), bounds);
    }
}
