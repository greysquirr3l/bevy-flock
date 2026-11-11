//! One hundred thousand NPCs, headless, on one thread.
//!
//! Run with:
//!
//! ```text
//! cargo run --release --example horde_100k
//! ```
//!
//! What to look for:
//!
//! * the spawn is budgeted — the population arrives over several frames instead of
//!   stalling the first one;
//! * the spawn is deterministic — the same seed reports the same digest;
//! * the diagnostics report tells you the tier split without a profiler.
//!
//! This is the example to point at when someone asks whether the crate actually
//! scales, which is why it prints the digest: run it twice and diff.

// An example's job is to be read: it prints, it reports its own failures through the test kit,
// and it keeps its fixtures next to the thing it demonstrates.
use bevy_app::App;
use bevy_flock::adapters::primary::observability::Diagnostics;
use bevy_flock::prelude::*;
use bevy_flock::testkit::OrDie;
use std::time::Instant;

/// Frames the horde runs before the measurement starts, so the first-tick work is not counted.
const WARM_FRAMES: u32 = 120;

fn main() {
    let population = PopulationSize::try_new(100_000).or_die();
    let config = FlockConfig::default()
        .with_population(population)
        .with_spawn_budget(4_000);

    let mut app = App::new();
    app.add_plugins(FlockPlugin::new(config));

    let start = Instant::now();
    let mut frames = 0_u32;
    let mut spawn_frames = 0_u32;
    loop {
        app.update();
        frames += 1;
        let stats = *app.world().resource::<FlockStats>();
        if !stats.spawn_complete() {
            spawn_frames += 1;
        }
        if stats.spawn_complete() || frames > 10_000 {
            break;
        }
    }
    let spawn_elapsed = start.elapsed();

    let warm = Instant::now();
    for _ in 0..WARM_FRAMES {
        app.update();
    }
    let warm_elapsed = warm.elapsed();

    let stats = *app.world().resource::<FlockStats>();
    let diagnostics = app.world().resource::<Diagnostics>();
    println!("{}", diagnostics.report());
    println!("digest           0x{:016x}", diagnostics.digest());
    println!();
    println!(
        "spawned          {} NPCs in {spawn_frames} frames",
        stats.live
    );
    println!(
        "spawn wall       {:.2} s ({:.0} NPC/s)",
        spawn_elapsed.as_secs_f32(),
        f64::from(stats.live) / spawn_elapsed.as_secs_f64().max(1e-9)
    );
    println!(
        "steady frame     {:.2} ms over {WARM_FRAMES} frames",
        warm_elapsed.as_secs_f64() * 1_000.0 / f64::from(WARM_FRAMES)
    );
    println!(
        "index            {} patched, {} rebuilt, of {WARM_FRAMES} frames",
        stats.patched_ticks, stats.rebuilt_ticks
    );
    println!(
        "last outcome     {:?}",
        app.world().resource::<FlockGridReport>().0
    );
    println!();
    println!(
        "Note: this is the full-cognition path. For the ambient continuum — the\n\
         millions-of-NPCs path — see the band plugin and `docs/dev/tuning/performance.md`."
    );
}
