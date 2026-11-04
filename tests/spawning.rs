//! Population-scale integration tests.
//!
//! These exercise the plugin the way a game does — add plugin, ask for a crowd,
//! update — and assert the properties the pitch claims: a population of one
//! works, ten thousand works, recycled pools reuse identities, and the same seed
//! produces the same people.

#![expect(
    clippy::expect_used,
    reason = "integration tests assert their own fixtures"
)]

use bevy_app::App;
use bevy_flock::prelude::*;

fn app_with(population: usize, mode: PopulationMode) -> App {
    let mut app = App::new();
    let config = FlockConfig {
        population: PopulationSize::clamped(population),
        mode,
        spawn_budget: 1_000_000,
        ..FlockConfig::preset(CrowdPreset::MarketplaceBustle)
    };
    app.add_plugins(FlockPlugin::new(config));
    app.update();
    app
}

#[test]
fn one_npc_is_a_valid_population() {
    let app = app_with(1, PopulationMode::Unique);
    assert_eq!(app.flock_stats().population(), 1);
}

#[test]
fn ten_thousand_unique_npcs_exist_and_are_distinct() {
    let app = app_with(10_000, PopulationMode::Unique);
    assert_eq!(app.flock_stats().population(), 10_000);

    let world = app.world();
    let mut ids: Vec<u32> = world
        .iter_entities()
        .filter_map(|entity| entity.get::<Npc>().map(|npc| npc.0.index()))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 10_000, "identities must be unique");
}

#[test]
fn the_same_seed_produces_the_same_crowd() {
    fn snapshot() -> Vec<(u32, [f32; 3])> {
        let app = app_with(500, PopulationMode::Unique);
        let world = app.world();
        let mut rows: Vec<(u32, [f32; 3])> = world
            .iter_entities()
            .filter_map(|entity| {
                let npc = entity.get::<Npc>()?;
                let temperament = entity.get::<NpcTemperament>()?;
                Some((
                    npc.0.index(),
                    [
                        temperament.0.openness.get(),
                        temperament.0.neuroticism.get(),
                        temperament.0.industriousness.get(),
                    ],
                ))
            })
            .collect();
        rows.sort_by_key(|(index, _)| *index);
        rows
    }
    assert_eq!(snapshot(), snapshot());
}

#[test]
fn recycled_pool_reuses_slot_identities() {
    let mode = PopulationMode::Recycled(SlotCount::try_new(64).expect("64 is non-zero"));
    let app = app_with(64, mode);
    assert_eq!(app.flock_stats().population(), 64);

    let ids: Vec<u32> = app
        .world()
        .iter_entities()
        .filter_map(|entity| entity.get::<Npc>().map(|npc| npc.0.index()))
        .collect();
    assert!(ids.iter().all(|index| *index < 64));
    assert!(
        ids.iter()
            .all(|index| NpcId::recycled(*index).is_recycled())
    );
}

#[test]
fn spawn_queue_drains_across_frames_when_budgeted() {
    let mut app = App::new();
    let config = FlockConfig {
        population: PopulationSize::clamped(1_000),
        spawn_budget: 128,
        ..FlockConfig::preset(CrowdPreset::QuietVillage)
    };
    app.add_plugins(FlockPlugin::new(config));

    app.update();
    assert_eq!(app.flock_stats().population(), 128);
    assert!(!app.flock_stats().spawn_complete());

    for _ in 0..7 {
        app.update();
    }
    assert_eq!(app.flock_stats().population(), 1_000);
    assert!(app.flock_stats().spawn_complete());
}

#[test]
fn clock_advances_one_tick_per_update() {
    let mut app = app_with(1, PopulationMode::Unique);
    for expected in 2..6 {
        app.update();
        assert_eq!(
            app.world()
                .resource::<bevy_flock::application::SimClock>()
                .tick
                .get(),
            expected
        );
    }
}
