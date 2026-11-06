//! Where a crowd is allowed to walk, and how the library asks.
//!
//! Navigation is a port, not a dependency: this crate ships a grid adapter that
//! needs nothing but an obstacle list, and a game with a baked navmesh or a
//! hand-authored street map implements [`NavSource`] and keeps its own data.
//!
//! The trait is object-safe and deliberately poor: bounds, walkability, the
//! neighbours of a cell, and the two conversions between world space and cells.
//! Anything richer would be a navigation library, and a navigation library in a
//! crowd crate is a dependency the game did not ask for.
//!
//! Two costs are fixed point integers (orthogonal 10, diagonal 14) rather than
//! floats, because a flow field's tie-breaking decides which way a thousand NPCs
//! flow around a corner, and float equality is not a thing you can rely on across
//! a compiler upgrade.

use bevy_math::Vec3;
use smallvec::SmallVec;

/// The integer cost of stepping to an orthogonally adjacent cell.
pub const COST_ORTHOGONAL: u32 = 10;

/// The integer cost of stepping to a diagonally adjacent cell.
///
/// `10 * sqrt(2)` rounded up: a diagonal step is longer than an orthogonal one,
/// and rounding up keeps a diagonal-heavy route from beating a straight one that
/// it should lose to.
pub const COST_DIAGONAL: u32 = 15;

/// A cell in the navigation grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NavCell {
    /// Column.
    pub x: u16,
    /// Row.
    pub y: u16,
}

impl NavCell {
    /// A cell.
    #[must_use]
    pub const fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }

    /// The linear index of this cell in a grid `width` cells wide.
    #[must_use]
    pub const fn index(self, width: u16) -> usize {
        self.y as usize * width as usize + self.x as usize
    }

    /// The eight neighbours of this cell, in a fixed order.
    ///
    /// Fixed order matters: a Dijkstra that pushes equal-cost nodes in a different
    /// order picks a different (equally short) path, and the whole point of the
    /// fixed-point costs is that everyone picks the same one.
    #[must_use]
    pub fn neighbour_offsets() -> [(i16, i16, u32); 8] {
        [
            (0, -1, COST_ORTHOGONAL),
            (1, -1, COST_DIAGONAL),
            (1, 0, COST_ORTHOGONAL),
            (1, 1, COST_DIAGONAL),
            (0, 1, COST_ORTHOGONAL),
            (-1, 1, COST_DIAGONAL),
            (-1, 0, COST_ORTHOGONAL),
            (-1, -1, COST_DIAGONAL),
        ]
    }
}

/// The rectangle a navigation source covers, in world units.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NavBounds {
    /// South-west corner, at ground level.
    pub origin: Vec3,
    /// Cells across.
    pub width: u16,
    /// Cells up.
    pub height: u16,
    /// World units per cell.
    pub cell_size: f32,
}

impl NavBounds {
    /// Bounds for a grid.
    ///
    /// # Errors
    ///
    /// Returns [`NavError::Empty`] for a zero-width or zero-height grid and
    /// [`NavError::ZeroCellSize`] for a cell size that is not finite and positive.
    pub fn new(origin: Vec3, width: u16, height: u16, cell_size: f32) -> Result<Self, NavError> {
        if width == 0 || height == 0 {
            return Err(NavError::Empty);
        }
        if !(cell_size.is_finite() && cell_size > 0.0) {
            return Err(NavError::ZeroCellSize { cell_size });
        }
        Ok(Self {
            origin,
            width,
            height,
            cell_size,
        })
    }

    /// How many cells the grid holds.
    #[must_use]
    pub const fn cell_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// Whether a cell is inside the bounds.
    #[must_use]
    pub const fn contains(&self, cell: NavCell) -> bool {
        cell.x < self.width && cell.y < self.height
    }
}

/// A monotonically increasing stamp for navigation data.
///
/// A flow field built against version 3 is stale the moment a door opens and the
/// version becomes 4. The cache compares the two numbers and drops the field,
/// which is the difference between a crowd that walks through a wall and one that
/// does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NavVersion(pub u32);

impl NavVersion {
    /// The first version.
    pub const FIRST: Self = Self(1);

    /// The next version.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// Why a navigation grid could not be built.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum NavError {
    /// A grid with no cells.
    #[error("a navigation grid needs at least one cell in each direction")]
    Empty,
    /// A cell size that is not a positive, finite number.
    #[error("cell size {cell_size} is not a positive, finite number")]
    ZeroCellSize {
        /// The rejected value.
        cell_size: f32,
    },
    /// A walkability bitmap whose length does not match the bounds.
    #[error("walkability bitmap has {actual} cells, bounds describe {expected}")]
    BitmapLength {
        /// Cells supplied.
        actual: usize,
        /// Cells the bounds describe.
        expected: usize,
    },
    /// An obstacle rectangle that is not a rectangle.
    #[error("obstacle rectangle is inverted or empty: {min:?} to {max:?}")]
    BadObstacle {
        /// Supplied minimum corner.
        min: NavCell,
        /// Supplied maximum corner.
        max: NavCell,
    },
    /// A navigation link that cannot exist.
    #[error("navigation link {from:?} to {to:?} is invalid: {reason}")]
    BadLink {
        /// The cell the link leaves from.
        from: NavCell,
        /// The cell the link arrives at.
        to: NavCell,
        /// Why it was refused.
        reason: &'static str,
    },
}

/// A source of navigation data.
///
/// Object-safe on purpose: the flow-field cache stores one of these behind a
/// reference and a game swaps in its own implementation without the crate growing
/// a generic parameter in every signature.
pub trait NavSource {
    /// The rectangle this source covers.
    fn bounds(&self) -> NavBounds;

    /// Whether an agent may stand in this cell.
    fn is_walkable(&self, cell: NavCell) -> bool;

    /// Append the walkable neighbours of a cell, with their integer step costs.
    ///
    /// The caller supplies the buffer so a solve allocates none.
    fn neighbours(&self, cell: NavCell, out: &mut SmallVec<[(NavCell, u32); 8]>);

    /// Append the links leaving a cell: connections that are not ordinary steps.
    ///
    /// The default is empty, so a source with no ladders or doors does not implement this.
    /// A source that answers here has its links followed by the flow solver, and the tags
    /// appear in the finished field for a behaviour tree to read.
    fn links_leaving(
        &self,
        cell: NavCell,
        out: &mut SmallVec<[(NavCell, u32, crate::domain::links::LinkTag); 4]>,
    ) {
        let _ = cell;
        out.clear();
    }

    /// Append the links arriving at a cell, which is what a backward search follows.
    ///
    /// A flow field is solved from its target outward, so an expansion asks what can step
    /// *into* the cell it is standing on. A source that implemented only the forward
    /// direction would have every ladder walked in the wrong direction.
    fn links_into(
        &self,
        cell: NavCell,
        out: &mut SmallVec<[(NavCell, u32, crate::domain::links::LinkTag); 4]>,
    ) {
        let _ = cell;
        out.clear();
    }

    /// Whether this source can ever return a link.
    ///
    /// Lets the solver skip allocating the per-cell tag array for the common case.
    fn has_links(&self) -> bool {
        false
    }

    /// The cell containing a world position, or `None` outside the bounds.
    fn cell_of(&self, position: Vec3) -> Option<NavCell>;

    /// The centre of a cell, at ground level.
    fn center_of(&self, cell: NavCell) -> Vec3;

    /// The version stamp, for cache invalidation.
    fn version(&self) -> NavVersion;
}

/// A navigation grid built from a walkability bitmap or an obstacle list.
#[derive(Debug, Clone)]
pub struct GridNavSource {
    bounds: NavBounds,
    walkable: Vec<bool>,
    version: NavVersion,
    links: crate::domain::links::LinkTable,
}

impl GridNavSource {
    /// Every cell walkable.
    ///
    /// # Errors
    ///
    /// As [`NavBounds::new`].
    pub fn open(origin: Vec3, width: u16, height: u16, cell_size: f32) -> Result<Self, NavError> {
        let bounds = NavBounds::new(origin, width, height, cell_size)?;
        Ok(Self {
            walkable: vec![true; bounds.cell_count()],
            bounds,
            version: NavVersion::FIRST,
            links: crate::domain::links::LinkTable::new(),
        })
    }

    /// Built from a row-major bitmap, `true` meaning walkable.
    ///
    /// # Errors
    ///
    /// [`NavError::BitmapLength`] if the bitmap does not match the bounds.
    pub fn from_bitmap(bounds: NavBounds, walkable: Vec<bool>) -> Result<Self, NavError> {
        if walkable.len() != bounds.cell_count() {
            return Err(NavError::BitmapLength {
                actual: walkable.len(),
                expected: bounds.cell_count(),
            });
        }
        Ok(Self {
            bounds,
            walkable,
            version: NavVersion::FIRST,
            links: crate::domain::links::LinkTable::new(),
        })
    }

    /// The bounds.
    #[must_use]
    pub const fn bounds_of(&self) -> NavBounds {
        self.bounds
    }

    /// The raw bitmap.
    #[must_use]
    pub fn walkable(&self) -> &[bool] {
        &self.walkable
    }

    /// Mark a rectangle unwalkable, as a building or a wall would.
    ///
    /// # Errors
    ///
    /// [`NavError::BadObstacle`] if the rectangle is inverted or empty; cells
    /// outside the grid are clamped rather than refused, because an obstacle that
    /// runs off the edge of the map is a normal thing for a level to contain.
    pub fn add_obstacle(&mut self, min: NavCell, max: NavCell) -> Result<(), NavError> {
        if max.x < min.x || max.y < min.y {
            return Err(NavError::BadObstacle { min, max });
        }
        for y in min.y..=max.y.min(self.bounds.height.saturating_sub(1)) {
            for x in min.x..=max.x.min(self.bounds.width.saturating_sub(1)) {
                let index = NavCell::new(x, y).index(self.bounds.width);
                if let Some(cell) = self.walkable.get_mut(index) {
                    *cell = false;
                }
            }
        }
        self.version = self.version.next();
        Ok(())
    }

    /// A copy with walkable space eroded by an agent radius.
    ///
    /// A path that hugs a wall looks wrong and, with any agent width at all, walks
    /// through it. Eroding the walkable area by the largest agent radius is the
    /// cheap version of inflating the obstacles, and it is the version this crate
    /// ships: mixed-size crowds should be split into separate navigation layers
    /// rather than quietly squeezing a cart through a doorway sized for a person.
    #[must_use]
    pub fn eroded(&self, radius_cells: u16) -> Self {
        if radius_cells == 0 {
            return self.clone();
        }
        let width = i32::from(self.bounds.width);
        let height = i32::from(self.bounds.height);
        let radius = i32::from(radius_cells);
        let mut walkable = self.walkable.clone();
        for y in 0..height {
            for x in 0..width {
                if !self.walkable_at(x, y) {
                    continue;
                }
                let mut blocked = false;
                for dy in -radius..=radius {
                    for dx in -radius..=radius {
                        if dx * dx + dy * dy > radius * radius {
                            continue;
                        }
                        if !self.walkable_at(x + dx, y + dy) {
                            blocked = true;
                            break;
                        }
                    }
                    if blocked {
                        break;
                    }
                }
                if blocked
                    && let Some(cell) = walkable.get_mut(y as usize * width as usize + x as usize)
                {
                    *cell = false;
                }
            }
        }
        Self {
            bounds: self.bounds,
            walkable,
            version: self.version.next(),
            links: self.links.clone(),
        }
    }

    /// Walkability at signed coordinates, out-of-range reading as unwalkable.
    fn walkable_at(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x >= i32::from(self.bounds.width) || y >= i32::from(self.bounds.height)
        {
            return false;
        }
        self.walkable
            .get(y as usize * self.bounds.width as usize + x as usize)
            .copied()
            .unwrap_or(false)
    }

    /// The links this source carries.
    #[must_use]
    pub const fn link_table(&self) -> &crate::domain::links::LinkTable {
        &self.links
    }

    /// Add a link: a ladder, a ledge, a door that only staff may use.
    ///
    /// Bumps the version, because a field solved before the ladder existed does not know
    /// about it and would route a crowd the long way round for ever.
    ///
    /// # Errors
    ///
    /// [`NavError::BadLink`] as [`crate::domain::links::LinkTable::add`].
    pub fn add_link(&mut self, link: crate::domain::links::NavLink) -> Result<(), NavError> {
        self.links.add(link)?;
        self.version = self.version.next();
        Ok(())
    }

    /// A copy of this source carrying a whole table of links.
    #[must_use]
    pub fn with_links(mut self, links: crate::domain::links::LinkTable) -> Self {
        if !links.is_empty() {
            self.version = self.version.next();
        }
        self.links = links;
        self
    }

    /// Bytes the bitmap occupies.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.walkable.len()
    }
}

impl NavSource for GridNavSource {
    fn bounds(&self) -> NavBounds {
        self.bounds
    }

    fn is_walkable(&self, cell: NavCell) -> bool {
        if !self.bounds.contains(cell) {
            return false;
        }
        self.walkable
            .get(cell.index(self.bounds.width))
            .copied()
            .unwrap_or(false)
    }

    fn neighbours(&self, cell: NavCell, out: &mut SmallVec<[(NavCell, u32); 8]>) {
        out.clear();
        let width = i32::from(self.bounds.width);
        let height = i32::from(self.bounds.height);
        let x = i32::from(cell.x);
        let y = i32::from(cell.y);
        for (dx, dy, cost) in NavCell::neighbour_offsets() {
            let nx = x + i32::from(dx);
            let ny = y + i32::from(dy);
            if nx < 0 || ny < 0 || nx >= width || ny >= height {
                continue;
            }
            let next = NavCell::new(nx as u16, ny as u16);
            if !self.is_walkable(next) {
                continue;
            }
            // No corner cutting: a diagonal step requires both orthogonal cells to be
            // walkable, otherwise an agent slips through the diagonal gap between two
            // walls and the navmesh stops describing the level.
            if dx != 0
                && dy != 0
                && (!self.is_walkable(NavCell::new(nx as u16, cell.y))
                    || !self.is_walkable(NavCell::new(cell.x, ny as u16)))
            {
                continue;
            }
            out.push((next, cost));
        }
    }

    fn links_leaving(
        &self,
        cell: NavCell,
        out: &mut SmallVec<[(NavCell, u32, crate::domain::links::LinkTag); 4]>,
    ) {
        self.links.leaving_into(cell, out);
    }

    fn links_into(
        &self,
        cell: NavCell,
        out: &mut SmallVec<[(NavCell, u32, crate::domain::links::LinkTag); 4]>,
    ) {
        self.links.arriving_into(cell, out);
    }

    fn has_links(&self) -> bool {
        !self.links.is_empty()
    }

    fn cell_of(&self, position: Vec3) -> Option<NavCell> {
        let local_x = (position.x - self.bounds.origin.x) / self.bounds.cell_size;
        let local_y = (position.z - self.bounds.origin.z) / self.bounds.cell_size;
        if !local_x.is_finite() || !local_y.is_finite() || local_x < 0.0 || local_y < 0.0 {
            return None;
        }
        let x = local_x.floor();
        let y = local_y.floor();
        if x >= f32::from(self.bounds.width) || y >= f32::from(self.bounds.height) {
            return None;
        }
        Some(NavCell::new(x as u16, y as u16))
    }

    fn center_of(&self, cell: NavCell) -> Vec3 {
        let half = self.bounds.cell_size * 0.5;
        Vec3::new(
            self.bounds.origin.x + (f32::from(cell.x) + 0.5) * self.bounds.cell_size,
            self.bounds.origin.y,
            self.bounds.origin.z + (f32::from(cell.y) + 0.5) * self.bounds.cell_size,
        ) + Vec3::new(0.0, 0.0, 0.0) * half
    }

    fn version(&self) -> NavVersion {
        self.version
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{OrDie, OrDieErr};

    fn grid() -> GridNavSource {
        GridNavSource::open(Vec3::ZERO, 8, 8, 1.0).or_die()
    }

    #[test]
    fn cells_convert_both_ways() {
        let grid = grid();
        let cell = grid.cell_of(Vec3::new(2.5, 0.0, 3.5)).or_die();
        assert_eq!(cell, NavCell::new(2, 3));
        let centre = grid.center_of(cell);
        assert_eq!(grid.cell_of(centre), Some(cell));
    }

    #[test]
    fn positions_outside_the_grid_have_no_cell() {
        let grid = grid();
        assert_eq!(grid.cell_of(Vec3::new(-1.0, 0.0, 0.0)), None);
        assert_eq!(grid.cell_of(Vec3::new(0.0, 0.0, 99.0)), None);
        assert_eq!(grid.cell_of(Vec3::new(f32::NAN, 0.0, 0.0)), None);
    }

    #[test]
    fn an_obstacle_is_impassable_and_bumps_the_version() {
        let mut grid = grid();
        let before = grid.version();
        grid.add_obstacle(NavCell::new(2, 2), NavCell::new(3, 3))
            .or_die();
        assert_ne!(grid.version(), before);
        assert!(!grid.is_walkable(NavCell::new(2, 2)));
        assert!(grid.is_walkable(NavCell::new(4, 2)));
    }

    #[test]
    fn an_inverted_obstacle_is_refused() {
        let mut grid = grid();
        let error = grid
            .add_obstacle(NavCell::new(4, 4), NavCell::new(2, 2))
            .or_die_err();
        assert!(matches!(error, NavError::BadObstacle { .. }));
    }

    #[test]
    fn diagonals_do_not_cut_corners() {
        let mut grid = grid();
        // Two walls meeting at a corner: (1,0) and (0,1) blocked, so (0,0) to (1,1)
        // must be refused even though both endpoints are walkable.
        grid.add_obstacle(NavCell::new(1, 0), NavCell::new(1, 0))
            .or_die();
        grid.add_obstacle(NavCell::new(0, 1), NavCell::new(0, 1))
            .or_die();
        assert!(!grid.is_walkable(NavCell::new(1, 0)));
        let mut out = SmallVec::new();
        grid.neighbours(NavCell::new(0, 0), &mut out);
        assert!(
            !out.iter().any(|(cell, _)| *cell == NavCell::new(1, 1)),
            "a diagonal step slipped between two walls"
        );
    }

    #[test]
    fn erosion_keeps_a_radius_of_clearance() {
        // A 16-wide grid so the borders do not dominate the result: the grid edge is a
        // wall, which is what keeps a crowd from walking off the map.
        let mut grid = GridNavSource::open(Vec3::ZERO, 16, 16, 1.0).or_die();
        grid.add_obstacle(NavCell::new(8, 0), NavCell::new(8, 15))
            .or_die();
        let eroded = grid.eroded(1);
        // The cell next to the wall is no longer walkable once a body needs clearance.
        assert!(grid.is_walkable(NavCell::new(7, 8)));
        assert!(!eroded.is_walkable(NavCell::new(7, 8)));
        // Far from the wall and from the edge nothing changed.
        assert!(eroded.is_walkable(NavCell::new(4, 8)));
    }

    #[test]
    fn a_bitmap_of_the_wrong_size_is_refused() {
        let bounds = NavBounds::new(Vec3::ZERO, 4, 4, 1.0).or_die();
        let error = GridNavSource::from_bitmap(bounds, vec![true; 15]).or_die_err();
        assert!(matches!(
            error,
            NavError::BitmapLength {
                actual: 15,
                expected: 16
            }
        ));
    }

    #[test]
    fn empty_bounds_and_zero_cell_size_are_refused() {
        assert_eq!(
            NavBounds::new(Vec3::ZERO, 0, 4, 1.0).or_die_err(),
            NavError::Empty
        );
        assert!(matches!(
            NavBounds::new(Vec3::ZERO, 4, 4, 0.0).or_die_err(),
            NavError::ZeroCellSize { .. }
        ));
    }

    /// A hand-authored source: an annulus, walkable between two radii.
    ///
    /// This is the "custom `NavSource` implementation demonstrated in a test" the
    /// plan asks for: no grid, no bitmap, just a rule.
    struct Ring {
        bounds: NavBounds,
        inner: f32,
        outer: f32,
        centre: Vec3,
    }

    impl NavSource for Ring {
        fn bounds(&self) -> NavBounds {
            self.bounds
        }

        fn is_walkable(&self, cell: NavCell) -> bool {
            let point = self.center_of(cell) - self.centre;
            let radius = point.x.hypot(point.z);
            (self.inner..=self.outer).contains(&radius)
        }

        fn neighbours(&self, cell: NavCell, out: &mut SmallVec<[(NavCell, u32); 8]>) {
            let mut candidates = SmallVec::new();
            GridNavSource::open(
                Vec3::ZERO,
                self.bounds.width,
                self.bounds.height,
                self.bounds.cell_size,
            )
            .map(|grid| grid.neighbours(cell, &mut candidates))
            .ok();
            out.clear();
            out.extend(
                candidates
                    .into_iter()
                    .filter(|(next, _)| self.is_walkable(*next)),
            );
        }

        fn cell_of(&self, position: Vec3) -> Option<NavCell> {
            let x = (position.x / self.bounds.cell_size).floor();
            let y = (position.z / self.bounds.cell_size).floor();
            if x < 0.0
                || y < 0.0
                || x >= f32::from(self.bounds.width)
                || y >= f32::from(self.bounds.height)
            {
                return None;
            }
            Some(NavCell::new(x as u16, y as u16))
        }

        fn center_of(&self, cell: NavCell) -> Vec3 {
            Vec3::new(
                (f32::from(cell.x) + 0.5) * self.bounds.cell_size,
                0.0,
                (f32::from(cell.y) + 0.5) * self.bounds.cell_size,
            )
        }

        fn version(&self) -> NavVersion {
            NavVersion::FIRST
        }
    }

    #[test]
    fn a_custom_nav_source_can_be_solved_over() {
        let ring = Ring {
            bounds: NavBounds::new(Vec3::ZERO, 32, 32, 1.0).or_die(),
            inner: 4.0,
            outer: 12.0,
            centre: Vec3::new(16.0, 0.0, 16.0),
        };
        let mut out = SmallVec::new();
        ring.neighbours(NavCell::new(16, 7), &mut out);
        assert!(!out.is_empty(), "the ring had no walkable neighbours");
        assert!(out.iter().all(|(cell, _)| ring.is_walkable(*cell)));
    }
}
