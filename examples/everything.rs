//! The whole adoption path in one file, in the order a new project needs it.
//!
//! ```toml
//! [dependencies]
//! bevy-flock = { version = "0.1", features = ["full"] }
//! ```
//!
//! Five things happen here, and each one is a section below:
//!
//! 1. Add one plugin. It installs steering, the spatial grid, perception, dialogue, gesture and gaze
//!    limits, obstacle navigation and the continuum, and populates the resources they need.
//! 2. Ask for a crowd. Spawning is budgeted, so a request is queued and spent over frames rather than
//!    arriving as one 400 ms hitch.
//! 3. Run frames. The simulation advances on a fixed logical tick, not on wall-clock time.
//! 4. Read the numbers the engine already keeps: population, chosen thinkers, tier counts.
//! 5. Save what is worth saving. The snapshot carries the format version and the subject registry, so
//!    a load can tell whether the people in it still mean what they meant.
//!
//! Run it with:
//!
//! ```text
//! cargo run --release --example everything --features full
//! ```
//!
//! Nothing here renders. A crowd is simulated first and drawn second, and the drawing is a separate
//! crate's problem (`bevy-flock-anim` for skinned NPCs, your own renderer for everything else).

use bevy_app::App;
use bevy_flock::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. One plugin. The preset picks a population, a temperament mix and a spatial budget that
    //    suit the scene; `with_population` overrides the count without touching the rest.
    const START: usize = 2_000;
    const EXTRA: usize = 500;

    let preset = CrowdPreset::MarketplaceBustle;
    let population = PopulationSize::try_new(START)?;
    let config = FlockConfig::preset(preset).with_population(population);

    let mut app = App::new();
    app.add_plugins(FlockPlugin::new(config));

    // 2. Ask for more people than the startup crowd. The queue spends `spawn_budget` per frame.
    let extra = PopulationSize::try_new(EXTRA)?;
    app.spawn_flock(FlockRequest::new(extra));
    // The count the queue should reach. Derived from the two constants rather than written out, so
    // editing one of them cannot leave the loop waiting for a number nobody asked for.
    let target = u32::try_from(START + EXTRA)?;

    // 3. Run frames until the queue drains. A fixed number here because the example has no window;
    //    a game runs `app.update()` from its own loop.
    let mut frames = 0_u32;
    while app.flock_stats().population() < target && frames < 240 {
        app.update();
        frames += 1;
    }

    // 4. Read what the engine already counts. These are the numbers to put on a debug overlay
    //    before you tune anything.
    let stats = app.flock_stats();
    println!("population      {}", stats.population());
    println!("frames to spawn {frames}");
    println!("spawn complete  {}", stats.spawn_complete());

    // 5. Save. A snapshot of social memory plus the registry that names its subjects. An empty crowd
    //    of memories is a valid save: memory is only written when an interaction leaf enrols someone,
    //    which is why a fresh crowd round-trips to the same empty file.
    let store = app.world().resource::<MemoryStore>();
    let archive = MemoryArchive::new(2_500);
    let registry = bevy_flock::domain::saves::SubjectRegistry::new();
    let now = app.world().resource::<SimClock>().tick;
    let snapshot = MemorySnapshot::capture(store, &archive, now, 1_800, &registry);
    let bytes = snapshot.to_bytes();
    let reloaded = MemorySnapshot::from_bytes(&bytes)?;
    println!(
        "snapshot        {} bytes, {} records",
        bytes.len(),
        reloaded.records.len()
    );
    assert_eq!(reloaded.records.len(), snapshot.records.len());

    println!("\nnext: site/src/content/docs/guides/ for memory, saving, scaling and behaviour");
    Ok(())
}
