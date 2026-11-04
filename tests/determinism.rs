//! Determinism tests.
//!
//! The claim is precise, so the test is too: the same seed, the same population
//! and the same number of ticks produce the same world state, bit for bit, on
//! every run.
//!
//! The digest covers the whole of an NPC's canonical state — identity,
//! personality, position and velocity — ordered by [`NpcId`], never by iteration
//! order. Hashing only positions would pass while a seed quietly stopped
//! affecting who was generated, which is exactly the bug worth catching.

use bevy_app::App;
use bevy_flock::prelude::*;

/// An NPC's canonical state as raw bit patterns.
type Row = (u32, [u64; 12]);

fn rows(app: &App) -> Vec<Row> {
    let world = app.world();
    let mut rows: Vec<Row> = world
        .iter_entities()
        .filter_map(|entity| {
            let npc = entity.get::<Npc>()?;
            let transform = entity.get::<Transform>()?;
            let velocity = entity.get::<NpcVelocity>()?;
            let identity = entity.get::<NpcIdentity>()?;
            let temperament = entity.get::<NpcTemperament>()?.0;
            Some((
                npc.0.index(),
                [
                    identity.appearance_seed,
                    u64::from(identity.archetype as u8),
                    u64::from(temperament.openness.get().to_bits()),
                    u64::from(temperament.conscientiousness.get().to_bits()),
                    u64::from(temperament.extraversion.get().to_bits()),
                    u64::from(temperament.agreeableness.get().to_bits()),
                    u64::from(temperament.neuroticism.get().to_bits()),
                    u64::from(temperament.bravery.get().to_bits()),
                    u64::from(transform.translation.x.to_bits()),
                    u64::from(transform.translation.z.to_bits()),
                    u64::from(velocity.0.x.to_bits()),
                    u64::from(velocity.0.z.to_bits()),
                ],
            ))
        })
        .collect();
    rows.sort_by_key(|(index, _)| *index);
    rows
}

fn digest(app: &App) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for (index, values) in rows(app) {
        hash ^= u64::from(index);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        for value in values {
            // to_bits, not a float comparison: identical floats must produce
            // identical bits, and no epsilon can hide a divergence.
            hash ^= value;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

fn run(seed: u64, population: usize, ticks: usize) -> u64 {
    let mut app = App::new();
    let config = FlockConfig {
        population: PopulationSize::clamped(population),
        seed: SimSeed::new(seed),
        spawn_budget: 1_000_000,
        ..FlockConfig::preset(CrowdPreset::CityAmbient)
    };
    app.add_plugins(FlockPlugin::new(config));
    app.update();
    for _ in 0..ticks {
        app.update();
    }
    digest(&app)
}

#[test]
fn identical_seeds_produce_identical_worlds() {
    assert_eq!(run(0x1234_5678, 400, 60), run(0x1234_5678, 400, 60));
}

#[test]
fn different_seeds_produce_different_worlds() {
    assert_ne!(
        run(1, 200, 30),
        run(2, 200, 30),
        "a different seed must produce different people"
    );
}

#[test]
fn ticks_actually_change_the_world() {
    assert_ne!(run(7, 200, 2), run(7, 200, 40), "the crowd must move");
}

#[test]
fn digest_is_stable_across_reruns_in_one_process() {
    // Guards against dependence on allocation addresses or hash seeds, which are
    // stable within a process and would otherwise hide until CI.
    let digests: Vec<u64> = (0..3).map(|_| run(99, 150, 20)).collect();
    assert!(digests.windows(2).all(|pair| pair.first() == pair.last()));
}

#[test]
fn a_larger_population_keeps_the_shared_prefix_of_individuals() {
    // Stream isolation: an NPC's draws depend on the seed and its own identity,
    // so the first 100 NPCs of a 100-NPC world match the first 100 of a
    // 1,000-NPC world. If this fails, adding an NPC changed somebody else's fate.
    fn first_temperaments(count: usize, wanted: usize) -> Vec<[u32; 3]> {
        let mut app = App::new();
        let config = FlockConfig {
            population: PopulationSize::clamped(count),
            seed: SimSeed::new(0xABCD),
            spawn_budget: 1_000_000,
            ..FlockConfig::preset(CrowdPreset::QuietVillage)
        };
        app.add_plugins(FlockPlugin::new(config));
        app.update();

        let world = app.world();
        let mut rows: Vec<(u32, [u32; 3])> = world
            .iter_entities()
            .filter_map(|entity| {
                let npc = entity.get::<Npc>()?;
                let temperament = entity.get::<NpcTemperament>()?.0;
                Some((
                    npc.0.index(),
                    [
                        temperament.openness.get().to_bits(),
                        temperament.bravery.get().to_bits(),
                        temperament.agreeableness.get().to_bits(),
                    ],
                ))
            })
            .collect();
        rows.sort_by_key(|(index, _)| *index);
        rows.into_iter()
            .take(wanted)
            .map(|(_, values)| values)
            .collect()
    }

    assert_eq!(first_temperaments(100, 100), first_temperaments(1_000, 100));
}
