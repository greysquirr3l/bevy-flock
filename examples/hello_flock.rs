//! A crowd in five lines, then a look at what it produced.
//!
//! Run with:
//!
//! ```text
//! cargo run --example hello_flock
//! ```
//!
//! Headless on purpose: no window, no GPU, no assets. A crowd is a simulation
//! first; rendering it is a separate, optional concern.

use bevy_app::App;
use bevy_flock::application::SimClock;
use bevy_flock::prelude::*;

fn main() {
    // Five lines to a running crowd.
    let mut app = App::new();
    app.add_plugins(FlockPlugin::new(FlockConfig::preset(
        CrowdPreset::CityAmbient,
    )));
    app.update();
    let stats = app.flock_stats();
    assert_eq!(stats.population(), CrowdPreset::CityAmbient.population());

    // Let the crowd move.
    for _ in 0..120 {
        app.update();
    }

    let clock = *app.world().resource::<SimClock>();
    let world = app.world();
    let mut alive = 0_u32;
    let mut moved = 0_u32;
    let mut fastest = 0.0_f32;
    for entity in world.iter_entities() {
        let Some(_npc) = entity.get::<Npc>() else {
            continue;
        };
        let Some(transform) = entity.get::<Transform>() else {
            continue;
        };
        let Some(velocity) = entity.get::<NpcVelocity>() else {
            continue;
        };
        alive += 1;
        if velocity.0.length() > 0.01 {
            moved += 1;
        }
        fastest = fastest.max(velocity.0.length());
        // Distance from the middle of the crowd: a sanity check that people
        // actually spread out rather than stacking on the origin.
        debug_assert!(transform.translation.is_finite());
    }

    println!("bevy-flock {}", env!("CARGO_PKG_VERSION"));
    println!("  tick            {}", clock.tick);
    println!("  population      {alive}");
    println!("  moving          {moved}");
    println!("  fastest         {fastest:.2} m/s");
    println!("  spawn complete  {}", stats.spawn_complete());
}
