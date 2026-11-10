//! The 1.0 path promise, in one file.
//!
//! Every other test in this crate exercises behaviour. This one exercises *names*, because a published
//! path is a compatibility promise in a way that a function body is not: renaming `bevy_flock::domain` to
//! `bevy_flock::sim` breaks every adopter at once and no behavioural test would notice.
//!
//! Three things are asserted, all of them at compile time:
//!
//! 1. the documented module paths resolve (`domain`, `behavior`, `application`, `adapters`, `boids`, `anim`);
//! 2. a name reached through the prelude is the *same type* as the one reached through its module path,
//!    which is what makes `use bevy_flock::prelude::*` safe to recommend;
//! 3. the facade paths are the crate paths they re-export, so a facade type is not a second, accidentally
//!    distinct type from the one a `bevy-flock-core` consumer names.
//!
//! The core half runs with `--no-default-features` as well, because that is the configuration a headless
//! server builds and its paths are just as promised as the engine ones.
//!
//! Nothing here imports `bevy_app`, `bevy_ecs` or `bevy_math` directly. That is deliberate: an adopter who
//! runs `cargo add bevy-flock` has exactly one dependency, so the prelude has to supply `App` and `Vec3`
//! itself. The engine half of this file fails to compile if that stops being true.

use bevy_flock::application::SimClock;
use bevy_flock::application::schedule::ThinkScheduler;
use bevy_flock::behavior::Blackboard;
use bevy_flock::domain::num;
use bevy_flock::domain::spatial::{SpatialGrid, SpatialGridConfig};
use bevy_flock::domain::steering::SteeringParams;
use bevy_flock::domain::tick::Tick;

/// Proves a type path resolves without needing a value of it.
const fn resolves<T>() {}

#[test]
fn the_facade_core_paths_are_the_core_crate_types() {
    resolves::<SpatialGrid>();
    resolves::<SteeringParams>();
    resolves::<Blackboard>();

    // Identity for the ones with a cheap constructor: if the facade ever declared a second
    // `SpatialGridConfig`, the assignment to the core path would stop compiling, which is the point.
    let config: SpatialGridConfig = SpatialGridConfig::default();
    let _: bevy_flock_core::domain::spatial::SpatialGridConfig = config;

    let params = SteeringParams::default();
    let _: bevy_flock_core::domain::steering::SteeringParams = params;

    let tick = Tick::new(0);
    let _: bevy_flock_core::domain::tick::Tick = tick;

    let clock = SimClock::default();
    let _: bevy_flock_core::application::SimClock = clock;

    let blackboard = Blackboard::default();
    let _: bevy_flock_core::behavior::Blackboard = blackboard;

    let scheduler = ThinkScheduler::new(4);
    let _: bevy_flock_core::application::schedule::ThinkScheduler = scheduler;

    // The deterministic numeric helpers are part of the promise: a replay that calls them must reach the
    // same code through either crate.
    let _: u32 = num::f32_to_u32(0.5);
}

#[cfg(feature = "ecs")]
mod engine {
    use bevy_flock::adapters::primary::plugin::FlockPlugin as AdapterPlugin;
    use bevy_flock::boids::BoidsPlugin;
    use bevy_flock::prelude::*;

    /// The first block in the README, with no import of its own beyond the prelude.
    ///
    /// It lives here rather than in a doctest because doctests of this crate run with its
    /// dev-dependencies available, which is exactly the difference that once let the README claim a
    /// one-dependency install while naming `bevy_app` directly.
    #[test]
    fn the_readme_block_compiles_and_runs() {
        let mut app = App::new();
        app.add_plugins(FlockPlugin::new(FlockConfig::preset(
            CrowdPreset::CityAmbient,
        )));
        app.update();
    }

    #[test]
    fn the_prelude_and_the_module_paths_name_one_plugin() {
        fn takes_adapter_plugin(plugin: AdapterPlugin) -> AdapterPlugin {
            plugin
        }
        let from_prelude: FlockPlugin = takes_adapter_plugin(FlockPlugin::default());
        let _ = from_prelude;
    }

    #[test]
    fn the_facade_ecs_paths_are_the_ecs_crate_types() {
        let _: bevy_flock_ecs::adapters::primary::plugin::FlockPlugin = FlockPlugin::default();
        let _: bevy_flock_ecs::boids::BoidsPlugin = BoidsPlugin::default();
        // `App` reaches an adopter through the prelude, not through a direct `bevy_app` dependency.
        let app: App = App::new();
        let _: bevy_flock_ecs::prelude::App = app;
    }

    #[cfg(feature = "anim")]
    #[test]
    fn the_facade_anim_paths_are_the_anim_crate_types() {
        let _: bevy_flock_anim::AnimPlugin = bevy_flock::anim::AnimPlugin;
    }
}
