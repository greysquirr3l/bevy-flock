//! Navigation derived from the world's obstacles, staged a budget at a time (ADR 0046).
//!
//! A hand-authored [`NavSource`](crate::domain::nav::NavSource) is the right answer for a level
//! that never changes. It is the wrong answer for a game where a crate gets pushed into a
//! corridor, because the alternative is rebuilding the whole grid on the frame the crate lands.
//!
//! This module is the middle path:
//!
//! * **The footprint set is the input.** Obstacles are axis-aligned footprints in world space;
//!   the builder rasterises them into a walkability bitmap over [`NavBounds`].
//! * **Rebuilds are staged.** A rebuild spends at most [`ObstacleNavConfig::cells_per_tick`]
//!   cells of work a tick, so a 200×200 grid costs a fraction of a millisecond a frame instead
//!   of a hitch.
//! * **Publication is atomic.** Callers keep reading the last complete source while a new one is
//!   being staged, and the version increments exactly once, on the step that finishes the job.
//!   A half-applied maze is never visible, which is the difference between an agent re-routing
//!   and an agent walking into a wall that only exists in the staging buffer.
//!
//! The ECS half lives in `adapters::primary::obstacle_nav`; everything here is deterministic
//! arithmetic over plain data, so it is testable without a world.
//!
//! # Why not a navmesh rebuild
//!
//! A polygon rebuild has to re-triangulate and re-link, which cannot be sliced into "ten cells a
//! tick" without publishing a mesh that is partly old and partly new. Grid rebuilds can be
//! sliced, so they can be budgeted. A game that wants polygons keeps its own source and uses
//! [`crate::domain::navmesh`] for the mesh itself.

use bevy_math::Vec3;

use super::nav::{GridNavSource, NavBounds, NavCell, NavError, NavVersion};
use crate::domain::num;

/// An axis-aligned obstacle footprint, in world units.
///
/// Only the `x` and `z` extents matter: the grid is a floor plan, and something tall enough to
/// block a walker blocks its whole column. `y` is kept so a caller can author obstacles in the
/// same units as everything else and so a future layout with decks has somewhere to start.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ObstacleFootprint {
    /// The low corner.
    pub min: Vec3,
    /// The high corner.
    pub max: Vec3,
}

impl ObstacleFootprint {
    /// A footprint from two corners, normalised so either corner may be given first.
    ///
    /// Returns `None` for a footprint that covers no area, which is what a caller gets from a
    /// zero-sized crate and which should not silently block a cell.
    #[must_use]
    pub fn new(a: Vec3, b: Vec3) -> Option<Self> {
        let min = Vec3::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z));
        let max = Vec3::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z));
        if max.x <= min.x || max.z <= min.z {
            return None;
        }
        Some(Self { min, max })
    }

    /// A footprint centred on `centre` with the given extents.
    #[must_use]
    pub fn centred(centre: Vec3, extents: Vec3) -> Option<Self> {
        Self::new(centre - extents * 0.5, centre + extents * 0.5)
    }

    /// The cells this footprint covers, clamped to `bounds`.
    ///
    /// `None` when it covers no cell inside the bounds — an obstacle outside the level is not an
    /// error, it is simply not this grid's business.
    #[must_use]
    pub fn cells(&self, bounds: &NavBounds) -> Option<(NavCell, NavCell)> {
        let cell = bounds.cell_size;
        if !(cell.is_finite() && cell > 0.0) {
            return None;
        }
        let to_cell = |value: f32, origin: f32| {
            let relative = (value - origin) / cell;
            if relative.is_finite() { relative } else { -1.0 }
        };
        let low_x = to_cell(self.min.x, bounds.origin.x).floor();
        let low_z = to_cell(self.min.z, bounds.origin.z).floor();
        let high_x = to_cell(self.max.x, bounds.origin.x).ceil() - 1.0;
        let high_z = to_cell(self.max.z, bounds.origin.z).ceil() - 1.0;
        let last_x = f32::from(bounds.width.saturating_sub(1));
        let last_z = f32::from(bounds.height.saturating_sub(1));
        let clamp_x = |value: f32| value.clamp(0.0, last_x);
        let clamp_z = |value: f32| value.clamp(0.0, last_z);
        if high_x < 0.0 || high_z < 0.0 || low_x > last_x || low_z > last_z {
            return None;
        }
        let (min_x, min_z) = (
            num::f32_to_u16(clamp_x(low_x)),
            num::f32_to_u16(clamp_z(low_z)),
        );
        let (max_x, max_z) = (
            num::f32_to_u16(clamp_x(high_x)),
            num::f32_to_u16(clamp_z(high_z)),
        );
        let min = NavCell::new(min_x.min(max_x), min_z.min(max_z));
        let max = NavCell::new(min_x.max(max_x), min_z.max(max_z));
        Some((min, max))
    }
}

/// How an obstacle grid is built and how much of a rebuild one tick may spend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObstacleNavConfig {
    /// The rectangle the level occupies.
    pub bounds: NavBounds,
    /// How many cells of clearance to erode around every obstacle, for agent radius.
    pub erode_cells: u16,
    /// The most cells one rebuild step may fill. 4,096 is a 64×64 block a frame.
    pub cells_per_tick: u32,
}

impl ObstacleNavConfig {
    /// A configuration for a grid, with a default budget of 4,096 cells a tick.
    ///
    /// # Errors
    ///
    /// Propagates [`NavBounds::new`].
    pub fn try_new(
        origin: Vec3,
        width: u16,
        height: u16,
        cell_size: f32,
    ) -> Result<Self, NavError> {
        Ok(Self {
            bounds: NavBounds::new(origin, width, height, cell_size)?,
            erode_cells: 0,
            cells_per_tick: 4_096,
        })
    }

    /// Erode this many cells around every obstacle.
    #[must_use]
    pub const fn with_erosion(mut self, cells: u16) -> Self {
        self.erode_cells = cells;
        self
    }

    /// Spend at most this many cells a tick on a rebuild.
    #[must_use]
    pub const fn with_budget(mut self, cells: u32) -> Self {
        self.cells_per_tick = cells;
        self
    }
}

/// What one [`ObstacleNav::step`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RebuildProgress {
    /// Cells filled this step.
    pub cells: u32,
    /// Cells still to fill, across every requested obstacle.
    pub remaining: u32,
    /// Whether this step finished a rebuild and published it.
    pub published: bool,
}

impl RebuildProgress {
    /// Whether a rebuild is in flight.
    #[must_use]
    pub const fn is_staging(self) -> bool {
        self.remaining > 0
    }
}

#[derive(Debug, Clone)]
struct Staging {
    walkable: Vec<bool>,
    requests: Vec<ObstacleFootprint>,
    /// Which request is being filled.
    obstacle: usize,
    /// How many rows of the current request are done.
    rows_done: u16,
    remaining: u32,
}

/// An obstacle-derived navigation source that rebuilds itself a budget at a time.
#[derive(Debug, Clone)]
pub struct ObstacleNav {
    config: ObstacleNavConfig,
    staged: Option<Staging>,
    published: Option<GridNavSource>,
    published_version: NavVersion,
    last_requests: Vec<ObstacleFootprint>,
    rebuilds: u32,
}

impl ObstacleNav {
    /// A builder with nothing published yet.
    #[must_use]
    pub fn new(config: ObstacleNavConfig) -> Self {
        Self {
            config,
            staged: None,
            published: None,
            published_version: NavVersion::default(),
            last_requests: Vec::new(),
            rebuilds: 0,
        }
    }

    /// The configuration.
    #[must_use]
    pub const fn config(&self) -> ObstacleNavConfig {
        self.config
    }

    /// The source callers should path against: the last *complete* rebuild.
    #[must_use]
    pub const fn source(&self) -> Option<&GridNavSource> {
        self.published.as_ref()
    }

    /// The version of the published source. Zero until the first rebuild commits.
    #[must_use]
    pub const fn version(&self) -> NavVersion {
        self.published_version
    }

    /// How many rebuilds have been published.
    #[must_use]
    pub const fn rebuilds(&self) -> u32 {
        self.rebuilds
    }

    /// Whether a rebuild is in flight.
    #[must_use]
    pub const fn is_staging(&self) -> bool {
        self.staged.is_some()
    }

    /// Ask for the world's obstacles. Starts a rebuild only if the set changed.
    ///
    /// The requests are sorted into a canonical order, so two callers that hand over the same
    /// obstacles in a different order get no rebuild and the same published source: a query's
    /// iteration order must not be able to cost a frame.
    ///
    /// Returns whether a rebuild started. A rebuild already in flight is abandoned if the set
    /// changed again — the newer world wins, and the abandoned work is at most one tick old.
    pub fn request(&mut self, obstacles: &[ObstacleFootprint]) -> bool {
        let mut requests: Vec<ObstacleFootprint> = obstacles.to_vec();
        requests.sort_by(|a, b| {
            (a.min.x, a.min.z, a.max.x, a.max.z)
                .partial_cmp(&(b.min.x, b.min.z, b.max.x, b.max.z))
                .unwrap_or(core::cmp::Ordering::Equal)
        });
        requests.dedup();
        if requests == self.last_requests && (self.published.is_some() || self.staged.is_some()) {
            return false;
        }
        self.last_requests.clone_from(&requests);
        let remaining = requests
            .iter()
            .filter_map(|request| request.cells(&self.config.bounds))
            .map(|(min, max)| (u32::from(max.x - min.x) + 1) * (u32::from(max.y - min.y) + 1))
            .sum();
        self.staged = Some(Staging {
            walkable: vec![true; self.config.bounds.cell_count()],
            requests,
            obstacle: 0,
            rows_done: 0,
            remaining,
        });
        if remaining == 0 {
            // An empty level still needs a published source, and there is nothing to rasterise:
            // commit now rather than spending a tick staging nothing.
            self.commit();
        }
        true
    }

    /// Spend at most the configured budget finishing the rebuild.
    ///
    /// Work is done a whole row at a time. A row is never half filled, because a half-filled row
    /// is a gap that exists for exactly one frame — and a path through a wall that is only there
    /// for a frame is still a bug an agent can walk into.
    pub fn step(&mut self) -> RebuildProgress {
        let Some(staging) = self.staged.as_mut() else {
            return RebuildProgress::default();
        };
        let mut budget = self.config.cells_per_tick.max(1);
        let mut filled = 0_u32;
        while budget > 0 {
            let Some(request) = staging.requests.get(staging.obstacle).copied() else {
                break;
            };
            let Some((min, max)) = request.cells(&self.config.bounds) else {
                staging.obstacle += 1;
                staging.rows_done = 0;
                continue;
            };
            let width = u32::from(max.x - min.x) + 1;
            let height = u32::from(max.y - min.y) + 1;
            if u32::from(staging.rows_done) >= height {
                staging.obstacle += 1;
                staging.rows_done = 0;
                continue;
            }
            let rows_left = height - u32::from(staging.rows_done);
            // At least one row, even when a single row is wider than the whole budget: a budget
            // that cannot fill one row would never make progress at all.
            let rows = (budget / width).min(rows_left).max(1);
            let end = u32::from(staging.rows_done) + rows;
            for offset in u32::from(staging.rows_done)..end {
                let row = usize::from(min.y) * usize::from(self.config.bounds.width)
                    + usize::try_from(offset).unwrap_or(0) * usize::from(self.config.bounds.width);
                for x in min.x..=max.x {
                    if let Some(cell) = staging.walkable.get_mut(row + usize::from(x)) {
                        *cell = false;
                    }
                }
            }
            let spent = (end - u32::from(staging.rows_done)) * width;
            filled += spent;
            staging.remaining = staging.remaining.saturating_sub(spent);
            budget = budget.saturating_sub(spent);
            staging.rows_done = u16::try_from(end).unwrap_or(max.y);
            if u32::from(staging.rows_done) >= height {
                staging.obstacle += 1;
                staging.rows_done = 0;
            }
        }
        let remaining = staging.remaining;
        if staging.obstacle >= staging.requests.len() {
            self.commit();
            return RebuildProgress {
                cells: filled,
                remaining: 0,
                published: true,
            };
        }
        RebuildProgress {
            cells: filled,
            remaining,
            published: false,
        }
    }

    /// Build the published source from the finished bitmap and bump the version once.
    fn commit(&mut self) {
        let Some(staging) = self.staged.take() else {
            return;
        };
        let mut walkable = staging.walkable;
        if self.config.erode_cells > 0 {
            // Erosion is done on the bitmap rather than through `GridNavSource::eroded` so the
            // published version is the one the caller asked for, not a second derived copy.
            walkable = erode(&walkable, self.config.bounds, self.config.erode_cells);
        }
        let Ok(source) = GridNavSource::from_bitmap(self.config.bounds, walkable) else {
            return;
        };
        self.published = Some(source);
        self.published_version = if self.published_version == NavVersion::default() {
            NavVersion::FIRST
        } else {
            self.published_version.next()
        };
        self.rebuilds = self.rebuilds.saturating_add(1);
    }

    /// Drive a rebuild to completion, one budgeted step at a time. For tests and tools.
    pub fn finish(&mut self) -> u32 {
        let mut steps = 0;
        while self.is_staging() && steps < 1_000_000 {
            self.step();
            steps += 1;
        }
        steps
    }
}

/// Clear every cell within `radius` of a blocked cell.
fn erode(walkable: &[bool], bounds: NavBounds, radius: u16) -> Vec<bool> {
    let width = usize::from(bounds.width);
    let height = usize::from(bounds.height);
    let mut out = walkable.to_vec();
    let radius = i32::from(radius);
    for z in 0..height {
        for x in 0..width {
            if walkable.get(z * width + x).copied().unwrap_or(false) {
                continue;
            }
            let (x, z) = (num::usize_to_i32(x), num::usize_to_i32(z));
            for dz in -radius..=radius {
                for dx in -radius..=radius {
                    if dx * dx + dz * dz > radius * radius {
                        continue;
                    }
                    let (nx, nz) = (x + dx, z + dz);
                    if nx < 0
                        || nz < 0
                        || nx >= num::usize_to_i32(width)
                        || nz >= num::usize_to_i32(height)
                    {
                        continue;
                    }
                    let index = num::i32_to_usize(nz) * width + num::i32_to_usize(nx);
                    if let Some(cell) = out.get_mut(index) {
                        *cell = false;
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::flow::{FlowTarget, solve};
    use crate::domain::nav::NavSource;
    use crate::testkit::OrDie;

    fn config() -> ObstacleNavConfig {
        ObstacleNavConfig::try_new(Vec3::ZERO, 40, 40, 1.0).or_die()
    }

    /// A wall down the middle of the grid with a gap at one end: the corridor case.
    fn wall_with_gap() -> Vec<ObstacleFootprint> {
        vec![ObstacleFootprint::new(Vec3::new(19.0, 0.0, 0.0), Vec3::new(21.0, 0.0, 30.0)).or_die()]
    }

    #[test]
    fn footprints_normalise_and_reject_degenerate_input() {
        let forward = ObstacleFootprint::new(Vec3::ZERO, Vec3::new(2.0, 1.0, 3.0)).or_die();
        let backward = ObstacleFootprint::new(Vec3::new(2.0, 1.0, 3.0), Vec3::ZERO).or_die();
        assert_eq!(forward, backward);
        assert!(ObstacleFootprint::new(Vec3::ZERO, Vec3::new(0.0, 1.0, 3.0)).is_none());
    }

    #[test]
    fn a_footprint_maps_to_the_cells_it_covers() {
        let bounds = config().bounds;
        let footprint =
            ObstacleFootprint::new(Vec3::new(1.0, 0.0, 2.0), Vec3::new(3.0, 0.0, 5.0)).or_die();
        let (min, max) = footprint.cells(&bounds).or_die();
        assert_eq!((min.x, min.y, max.x, max.y), (1, 2, 2, 4));
    }

    #[test]
    fn the_first_rebuild_publishes_and_bumps_the_version_once() {
        let mut nav = ObstacleNav::new(config());
        assert_eq!(nav.version(), NavVersion::default());
        assert!(nav.request(&wall_with_gap()));
        let steps = nav.finish();
        assert_eq!(steps, 1, "a wall inside one budget is one step");
        assert_eq!(nav.version(), NavVersion::FIRST);
        assert_eq!(nav.rebuilds(), 1);
        let source = nav.source().or_die();
        assert!(!source.is_walkable(NavCell::new(20, 10)), "the wall blocks");
        assert!(
            source.is_walkable(NavCell::new(20, 35)),
            "the gap stays open"
        );
    }

    #[test]
    fn the_same_obstacles_do_not_rebuild() {
        let mut nav = ObstacleNav::new(config());
        nav.request(&wall_with_gap());
        nav.finish();
        let version = nav.version();
        assert!(
            !nav.request(&wall_with_gap()),
            "unchanged set must not rebuild"
        );
        assert!(nav.request(&[]), "a request that clears it is a change");
        nav.finish();
        assert_ne!(nav.version(), version, "clearing the wall is a new version");
    }

    #[test]
    fn request_order_does_not_start_a_rebuild() {
        let mut nav = ObstacleNav::new(config());
        let mut obstacles = wall_with_gap();
        obstacles.push(
            ObstacleFootprint::new(Vec3::new(2.0, 0.0, 2.0), Vec3::new(4.0, 0.0, 4.0)).or_die(),
        );
        nav.request(&obstacles);
        nav.finish();
        obstacles.reverse();
        assert!(!nav.request(&obstacles), "order is not a change");
    }

    #[test]
    fn a_blocked_corridor_is_routed_around_after_the_rebuild_publishes() {
        let mut nav = ObstacleNav::new(config().with_budget(64));
        nav.request(&[]);
        nav.finish();
        let source = nav.source().or_die();
        let open = solve(source, FlowTarget::at_cell(source, NavCell::new(35, 20)));
        let open_cost = open.cost_at(NavCell::new(5, 20)).or_die();

        nav.request(&wall_with_gap());
        nav.finish();
        let source = nav.source().or_die();
        assert!(!source.is_walkable(NavCell::new(20, 10)), "the wall blocks");
        let field = solve(source, FlowTarget::at_cell(source, NavCell::new(35, 20)));
        let detour = field.cost_at(NavCell::new(5, 20)).or_die();
        assert!(
            detour > open_cost,
            "the detour must cost more than the straight line: {detour} vs {open_cost}"
        );
    }

    #[test]
    fn a_removed_obstacle_opens_the_route_again() {
        let mut nav = ObstacleNav::new(config().with_budget(32));
        nav.request(&wall_with_gap());
        nav.finish();
        let source = nav.source().or_die();
        // The gap is at the far end, so the straight line is blocked but a route exists.
        assert!(!source.is_walkable(NavCell::new(20, 10)));
        let _ = solve(source, FlowTarget::at_cell(source, NavCell::new(35, 20)));

        nav.request(&[]);
        nav.finish();
        let source = nav.source().or_die();
        assert!(source.is_walkable(NavCell::new(20, 10)), "the wall is gone");
        let field = solve(source, FlowTarget::at_cell(source, NavCell::new(35, 20)));
        assert!(
            field.cost_at(NavCell::new(20, 10)).is_some(),
            "the route through the old wall is open again"
        );
    }

    #[test]
    fn a_staged_rebuild_never_exposes_a_half_built_grid() {
        // One cell a tick: the wall takes 61 steps, so the staging window is long enough to see.
        let mut nav = ObstacleNav::new(config().with_budget(1));
        nav.request(&[]);
        nav.finish();
        let before = nav.version();
        nav.request(&wall_with_gap());
        for _ in 0..10 {
            let progress = nav.step();
            assert!(progress.is_staging());
            let source = nav.source().or_die();
            assert!(
                source.is_walkable(NavCell::new(20, 10)),
                "a staged wall must not be visible"
            );
            assert_eq!(nav.version(), before, "the version moves once, at the end");
        }
        let steps = nav.finish();
        assert!(steps > 1, "the rebuild really was staged");
        assert!(!nav.source().or_die().is_walkable(NavCell::new(20, 10)));
        assert_eq!(nav.version(), before.next());
    }

    #[test]
    fn erosion_widens_the_gap_a_walker_needs() {
        let mut nav = ObstacleNav::new(config().with_erosion(2));
        nav.request(&wall_with_gap());
        nav.finish();
        let source = nav.source().or_die();
        assert!(!source.is_walkable(NavCell::new(20, 10)), "still blocked");
        assert!(
            !source.is_walkable(NavCell::new(22, 10)),
            "two cells of clearance are eroded on each side"
        );
        assert!(source.is_walkable(NavCell::new(30, 10)));
    }

    #[test]
    fn a_small_budget_still_finishes_and_is_deterministic() {
        let run = || {
            let mut nav = ObstacleNav::new(config().with_budget(7));
            nav.request(&wall_with_gap());
            let steps = nav.finish();
            let cells: Vec<bool> = (0..40 * 40)
                .map(|index| {
                    let x = u16::try_from(index % 40).or_die();
                    let z = u16::try_from(index / 40).or_die();
                    nav.source().or_die().is_walkable(NavCell::new(x, z))
                })
                .collect();
            (steps, cells)
        };
        let (steps_a, cells_a) = run();
        let (steps_b, cells_b) = run();
        assert_eq!(steps_a, steps_b);
        assert_eq!(cells_a, cells_b);
    }
}
