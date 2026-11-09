//! The ECS half of obstacle-driven navigation (ADR 0046).
//!
//! A game marks the things that block walking with [`NavObstacle`], adds [`ObstacleNavPlugin`],
//! and reads [`ObstacleNavState::source`] when it wants a path. Nothing else is required: the
//! plugin collects the footprints, starts a rebuild when the set changes, and spends a bounded
//! slice of each frame finishing it.
//!
//! ```
//! use bevy_app::App;
//! use bevy_flock_ecs::prelude::*;
//! use bevy_math::Vec3;
//!
//! let mut app = App::new();
//! app.add_plugins(ObstacleNavPlugin::new(
//!     ObstacleNavSettings::new(Vec3::ZERO, 64, 64, 1.0).expect("a 64 m square"),
//! ));
//! app.world_mut().spawn(
//!     NavObstacle::new(Vec3::new(10.0, 0.0, 4.0), Vec3::new(12.0, 3.0, 20.0)).expect("a wall"),
//! );
//! app.update();
//! assert!(app.world().resource::<ObstacleNavState>().source().is_some());
//! ```
//!
//! The rebuild is deliberately not driven by change detection: an obstacle that moves every frame
//! (a pushed crate) would then restart the rebuild every frame and never publish. Instead the
//! footprint set is compared each tick, which costs one sort of a small list and gives the
//! staging a chance to finish.

use bevy_app::{App, Plugin, Update};
use bevy_ecs::prelude::*;
use bevy_math::Vec3;

use crate::adapters::primary::plugin::FlockSet;
use crate::domain::nav::{GridNavSource, NavError, NavVersion};
use crate::domain::obstacle_nav::{
    ObstacleFootprint, ObstacleNav, ObstacleNavConfig, RebuildProgress,
};

/// Something that blocks walking, as an axis-aligned footprint.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct NavObstacle(pub ObstacleFootprint);

impl NavObstacle {
    /// An obstacle between two corners. Either corner may be given first.
    ///
    /// Returns `None` for a footprint with no area, which is what a zero-sized prop gives and
    /// which must not silently block a cell.
    #[must_use]
    pub fn new(a: Vec3, b: Vec3) -> Option<Self> {
        ObstacleFootprint::new(a, b).map(Self)
    }

    /// A footprint centred on a point.
    #[must_use]
    pub fn centred(centre: Vec3, extents: Vec3) -> Option<Self> {
        ObstacleFootprint::centred(centre, extents).map(Self)
    }

    /// The footprint, for the builder.
    #[must_use]
    pub const fn footprint(self) -> ObstacleFootprint {
        self.0
    }
}

/// The grid an obstacle rebuild targets, and how much of a frame it may spend.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct ObstacleNavSettings(pub ObstacleNavConfig);

impl ObstacleNavSettings {
    /// Settings for a grid at `origin`, `width` by `height` cells of `cell_size` metres.
    ///
    /// # Errors
    ///
    /// Propagates [`crate::domain::nav::NavBounds::new`].
    pub fn new(origin: Vec3, width: u16, height: u16, cell_size: f32) -> Result<Self, NavError> {
        ObstacleNavConfig::try_new(origin, width, height, cell_size).map(Self)
    }

    /// Erode this many cells around every obstacle, for agent radius.
    #[must_use]
    pub const fn with_erosion(mut self, cells: u16) -> Self {
        self.0 = self.0.with_erosion(cells);
        self
    }

    /// Spend at most this many cells of a rebuild a tick.
    #[must_use]
    pub const fn with_budget(mut self, cells: u32) -> Self {
        self.0 = self.0.with_budget(cells);
        self
    }
}

impl Default for ObstacleNavSettings {
    fn default() -> Self {
        // 64 m square at a metre a cell: a village block. Validated by construction, so there is
        // no panic path here even though the constructor is fallible.
        Self(
            ObstacleNavConfig::try_new(Vec3::ZERO, 64, 64, 1.0)
                .unwrap_or_else(|_| unreachable!("a 64x64 grid at 1 m is valid")),
        )
    }
}

/// The published obstacle navigation source and the state of the rebuild that will replace it.
#[derive(Resource, Debug)]
pub struct ObstacleNavState {
    nav: ObstacleNav,
    progress: RebuildProgress,
}

impl ObstacleNavState {
    /// A state with nothing published yet.
    #[must_use]
    pub fn new(config: ObstacleNavConfig) -> Self {
        Self {
            nav: ObstacleNav::new(config),
            progress: RebuildProgress::default(),
        }
    }

    /// The last complete source. `None` until the first rebuild publishes.
    #[must_use]
    pub const fn source(&self) -> Option<&GridNavSource> {
        self.nav.source()
    }

    /// The version of the published source, for a flow-field cache.
    #[must_use]
    pub const fn version(&self) -> NavVersion {
        self.nav.version()
    }

    /// Whether a rebuild is in flight.
    #[must_use]
    pub const fn is_staging(&self) -> bool {
        self.nav.is_staging()
    }

    /// How much work the last step did and how much is left.
    #[must_use]
    pub const fn progress(&self) -> RebuildProgress {
        self.progress
    }

    /// How many rebuilds have published.
    #[must_use]
    pub const fn rebuilds(&self) -> u32 {
        self.nav.rebuilds()
    }

    /// The builder itself, for tools and tests that want to drive it directly.
    #[must_use]
    pub const fn nav(&self) -> &ObstacleNav {
        &self.nav
    }
}

/// Adds obstacle-driven navigation.
#[derive(Debug, Clone, Copy, Default)]
pub struct ObstacleNavPlugin {
    /// The grid and the rebuild budget.
    pub settings: ObstacleNavSettings,
}

impl ObstacleNavPlugin {
    /// A plugin with explicit settings.
    #[must_use]
    pub const fn new(settings: ObstacleNavSettings) -> Self {
        Self { settings }
    }
}

impl Plugin for ObstacleNavPlugin {
    fn build(&self, app: &mut App) {
        let config = self.settings.0;
        app.insert_resource(self.settings)
            .insert_resource(ObstacleNavState::new(config))
            .add_systems(Update, rebuild_obstacle_nav.in_set(FlockSet::Perception));
    }
}

/// Collect the world's obstacles and spend a slice of the frame on the rebuild.
///
/// Runs in [`FlockSet::Perception`], before anything that paths, so a version published this tick
/// is visible to this tick's steering.
pub fn rebuild_obstacle_nav(
    mut state: ResMut<ObstacleNavState>,
    query: Query<&NavObstacle>,
    mut footprints: Local<Vec<ObstacleFootprint>>,
) {
    footprints.clear();
    footprints.extend(query.iter().map(|obstacle| obstacle.0));
    state.nav.request(&footprints);
    state.progress = state.nav.step();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::nav::{NavCell, NavSource};
    use crate::testkit::OrDie;

    fn app(settings: ObstacleNavSettings) -> App {
        let mut app = App::new();
        app.add_plugins(ObstacleNavPlugin::new(settings));
        app
    }

    fn settings() -> ObstacleNavSettings {
        ObstacleNavSettings::new(Vec3::ZERO, 32, 32, 1.0)
            .or_die()
            .with_budget(4_096)
    }

    #[test]
    fn a_spawned_obstacle_is_rasterised_and_published() {
        let mut app = app(settings());
        app.world_mut()
            .spawn(NavObstacle::new(Vec3::new(8.0, 0.0, 0.0), Vec3::new(10.0, 2.0, 32.0)).or_die());
        app.update();
        let state = app.world().resource::<ObstacleNavState>();
        let source = state.source().or_die();
        assert!(!source.is_walkable(NavCell::new(9, 4)));
        assert!(source.is_walkable(NavCell::new(4, 4)));
        assert_eq!(state.version(), NavVersion::FIRST);
    }

    #[test]
    fn an_unchanged_set_does_not_rebuild_next_frame() {
        let mut app = app(settings());
        app.world_mut()
            .spawn(NavObstacle::new(Vec3::new(8.0, 0.0, 0.0), Vec3::new(10.0, 2.0, 32.0)).or_die());
        app.update();
        let version = app.world().resource::<ObstacleNavState>().version();
        app.update();
        let state = app.world().resource::<ObstacleNavState>();
        assert_eq!(state.version(), version);
        assert_eq!(state.rebuilds(), 1);
    }

    #[test]
    fn despawning_the_obstacle_reopens_the_grid() {
        let mut app = app(settings());
        let wall = app
            .world_mut()
            .spawn(NavObstacle::new(Vec3::new(8.0, 0.0, 0.0), Vec3::new(10.0, 2.0, 32.0)).or_die())
            .id();
        app.update();
        assert!(
            !app.world()
                .resource::<ObstacleNavState>()
                .source()
                .or_die()
                .is_walkable(NavCell::new(9, 4))
        );
        app.world_mut().despawn(wall);
        app.update();
        let state = app.world().resource::<ObstacleNavState>();
        assert!(state.source().or_die().is_walkable(NavCell::new(9, 4)));
        assert_eq!(state.version(), NavVersion::FIRST.next());
    }

    #[test]
    fn a_tiny_budget_spreads_the_rebuild_over_frames() {
        let mut app = app(settings().with_budget(16));
        app.world_mut()
            .spawn(NavObstacle::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(32.0, 2.0, 32.0)).or_die());
        app.update();
        let state = app.world().resource::<ObstacleNavState>();
        assert!(state.is_staging(), "1,024 cells cannot be done in 16");
        assert!(state.source().is_none(), "nothing is published mid-rebuild");
        for _ in 0..200 {
            app.update();
        }
        let state = app.world().resource::<ObstacleNavState>();
        assert!(!state.is_staging());
        assert!(state.source().is_some());
    }

    #[test]
    fn a_zero_sized_prop_is_not_an_obstacle() {
        assert!(NavObstacle::new(Vec3::ZERO, Vec3::new(0.0, 2.0, 2.0)).is_none());
    }
}
