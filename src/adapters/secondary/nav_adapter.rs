//! The navigation grid as a resource, and the cache that makes pathing cheap.
//!
//! The domain owns the algorithms ([`crate::domain::nav`], [`crate::domain::flow`]);
//! this module owns their lifetime in an app: one grid, a version that changes when
//! the grid does, and a bounded cache of solved fields that drops anything built
//! against an older layout.
//!
//! The version is the whole safety mechanism. A flow field is only valid for the
//! layout it was solved against, so a door opening must invalidate every field that
//! routed through it — and it must do so in the same frame, or NPCs walk into a wall
//! that was not there when they planned.

use crate::domain::flow::{FlowCache, FlowCacheError, FlowField, FlowLookup, FlowTarget};
use crate::domain::nav::{GridNavSource, NavBounds, NavCell, NavError, NavSource, NavVersion};
use bevy_ecs::prelude::*;
use bevy_math::Vec3;

/// The crowd's navigation grid.
#[derive(Resource, Debug)]
pub struct NavGrid {
    source: GridNavSource,
    rebuilds: u64,
}

impl NavGrid {
    /// A grid with every cell walkable.
    ///
    /// # Errors
    ///
    /// [`NavError`] for degenerate bounds.
    pub fn open(origin: Vec3, width: u16, height: u16, cell_size: f32) -> Result<Self, NavError> {
        Ok(Self {
            source: GridNavSource::open(origin, width, height, cell_size)?,
            rebuilds: 0,
        })
    }

    /// A grid from an explicit walkability bitmap.
    ///
    /// # Errors
    ///
    /// [`NavError::BadBitmap`] if the bitmap length does not match the bounds.
    pub fn from_bitmap(bounds: NavBounds, walkable: Vec<bool>) -> Result<Self, NavError> {
        Ok(Self {
            source: GridNavSource::from_bitmap(bounds, walkable)?,
            rebuilds: 0,
        })
    }

    /// The grid.
    #[must_use]
    pub const fn source(&self) -> &GridNavSource {
        &self.source
    }

    /// The grid, mutably. Changing it without calling [`NavGrid::bump`] leaves stale
    /// fields in the cache, which is why the setter is a method and not a field.
    pub fn source_mut(&mut self) -> &mut GridNavSource {
        &mut self.source
    }

    /// The current layout version, taken from the source so the two can never
    /// disagree. A wrapper that kept its own counter would hand the cache a version
    /// that matched nothing.
    #[must_use]
    pub fn version(&self) -> NavVersion {
        self.source.version()
    }

    /// How many times the layout has changed.
    #[must_use]
    pub const fn rebuilds(&self) -> u64 {
        self.rebuilds
    }

    /// Block a rectangle of cells and invalidate the cache.
    ///
    /// # Errors
    ///
    /// [`NavError::BadObstacle`] for an inverted rectangle.
    pub fn block(&mut self, min: NavCell, max: NavCell) -> Result<NavVersion, NavError> {
        self.source.add_obstacle(min, max)?;
        self.rebuilds = self.rebuilds.saturating_add(1);
        Ok(self.version())
    }

    /// Bytes the grid occupies.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.source.bytes()
    }
}

impl Default for NavGrid {
    fn default() -> Self {
        // A 128 x 128 metre town at one metre per cell: big enough to be useful and
        // small enough that a default costs 16 KiB.
        Self::open(Vec3::ZERO, 128, 128, 1.0).unwrap_or_else(|_| Self {
            source: GridNavSource::open(Vec3::ZERO, 1, 1, 1.0).unwrap_or_else(|_| unreachable!()),
            rebuilds: 0,
        })
    }
}

/// Cached flow fields, bounded by capacity.
#[derive(Resource, Debug)]
pub struct FlowFields {
    cache: FlowCache,
}

impl FlowFields {
    /// A cache holding at most `capacity` fields.
    ///
    /// # Errors
    ///
    /// [`FlowCacheError::ZeroCapacity`].
    pub fn new(capacity: usize) -> Result<Self, FlowCacheError> {
        Ok(Self {
            cache: FlowCache::new(capacity)?,
        })
    }

    /// The field towards a target, built on demand.
    pub fn field_for(
        &mut self,
        source: &dyn NavSource,
        target: FlowTarget,
    ) -> (FlowLookup, &FlowField) {
        self.cache.field_for(source, target)
    }

    /// Drop fields built against an older layout.
    pub fn invalidate_stale(&mut self, version: NavVersion) -> usize {
        self.cache.invalidate_stale(version)
    }

    /// Hits, misses and evictions since startup.
    #[must_use]
    pub fn stats(&self) -> (u64, u64, u64) {
        (
            self.cache.hits(),
            self.cache.misses(),
            self.cache.evictions(),
        )
    }

    /// Bytes the cache occupies.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.cache.bytes()
    }

    /// How many fields are cached.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Whether nothing is cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }
}

impl Default for FlowFields {
    fn default() -> Self {
        Self::new(4).unwrap_or_else(|_| Self {
            cache: FlowCache::new(1).unwrap_or_else(|_| unreachable!()),
        })
    }
}

/// Drop cached fields whenever the layout changed.
pub fn invalidate_stale_fields(nav: Res<NavGrid>, mut flows: ResMut<FlowFields>) {
    let _ = flows.invalidate_stale(nav.version());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::OrDie;
    use bevy_app::{App, Update};

    #[test]
    fn a_field_is_built_once_and_then_hit() {
        let nav = NavGrid::open(Vec3::ZERO, 32, 32, 1.0).or_die();
        let mut flows = FlowFields::new(2).or_die();
        let target = FlowTarget::at_cell(nav.source(), NavCell::new(30, 30));
        let (first, _) = flows.field_for(nav.source(), target);
        let (second, _) = flows.field_for(nav.source(), target);
        assert_eq!(first, FlowLookup::Rebuilt);
        assert_eq!(second, FlowLookup::Hit);
        assert_eq!(flows.stats(), (1, 1, 0));
    }

    #[test]
    fn blocking_a_cell_invalidates_the_cache() {
        let mut nav = NavGrid::open(Vec3::ZERO, 16, 16, 1.0).or_die();
        let mut flows = FlowFields::new(2).or_die();
        let target = FlowTarget::at_cell(nav.source(), NavCell::new(15, 15));
        let _ = flows.field_for(nav.source(), target);
        assert_eq!(flows.len(), 1);
        let version = nav.block(NavCell::new(4, 4), NavCell::new(5, 5)).or_die();
        assert_eq!(version, nav.version());
        let removed = flows.invalidate_stale(version);
        assert_eq!(removed, 1);
        assert!(flows.is_empty());
        let (lookup, _) = flows.field_for(nav.source(), target);
        assert_eq!(lookup, FlowLookup::Rebuilt);
    }

    #[test]
    fn the_invalidation_system_runs_with_the_plugin_style_resources() {
        let mut app = App::new();
        app.init_resource::<FlowFields>()
            .insert_resource(NavGrid::open(Vec3::ZERO, 8, 8, 1.0).or_die())
            .add_systems(Update, invalidate_stale_fields);
        app.update();
        assert!(app.world().resource::<FlowFields>().is_empty());
    }

    #[test]
    fn the_cache_is_bounded() {
        let nav = NavGrid::open(Vec3::ZERO, 8, 8, 1.0).or_die();
        let mut flows = FlowFields::new(1).or_die();
        let first = FlowTarget::at_cell(nav.source(), NavCell::new(7, 7));
        let second = FlowTarget::at_cell(nav.source(), NavCell::new(0, 7));
        let _ = flows.field_for(nav.source(), first);
        let _ = flows.field_for(nav.source(), second);
        assert_eq!(flows.len(), 1);
        assert_eq!(flows.stats().2, 1);
    }

    #[test]
    fn the_default_grid_is_usable() {
        let nav = NavGrid::default();
        assert!(nav.source().is_walkable(NavCell::new(10, 10)));
        assert!(nav.bytes() > 0);
    }
}
