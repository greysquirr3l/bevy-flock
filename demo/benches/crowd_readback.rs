//! What reading the crowd back out of the ECS costs, and what each part of it costs.
//!
//! The playground calls `Playground::bodies` once per rendered frame to move 2,000 entities. That is
//! a snapshot API used in a live loop, so the question is not whether it is correct — it is how much
//! of a frame it takes, and which part of it takes the time:
//!
//! * **`full`** is the call as it stands: build a `QueryState`, iterate, collect into
//!   `Vec<(u32, Body)>`, sort by identity, copy the bodies into a second `Vec`.
//! * **`into_reused`** is the path the window uses: `bodies_into` with a buffer the caller keeps, so
//!   the only work left is the iteration, the sort and the copy into that buffer.
//! * **`fresh_query`** is the same without the sort and the second copy, so the difference between it
//!   and `full` is the sort plus the output allocation.
//! * **`reused_query`** iterates through a `QueryState` built once in advance, so the difference
//!   between it and `fresh_query` is the cost of matching archetypes on every call.
//!
//! The three together say where the milliseconds are, which is what decides whether a fix is worth
//! its risk. Absolute numbers are this sandbox's: an ARM core, a debug-tuned profile, and a crowd
//! that is not animating. The ratios are the part that travels.
use std::hint::black_box;

use bevy_ecs::query::QueryState;
use bevy_flock::adapters::primary::components::{Npc, NpcIdentity, NpcVelocity};
use bevy_flock_demo::app::Playground;
use bevy_flock_demo::scene::Body;
use bevy_transform::prelude::Transform;
use criterion::{Criterion, criterion_group, criterion_main};

/// The seed the demo ships with, so the benchmark measures the crowd the artefacts show.
const SEED: u64 = 7;

/// The city's half-width, metres: the demo's default extent.
const EXTENT: f32 = 900.0;

/// Ticks before measuring: long enough that the crowd has dispersed and is walking.
const WARM_TICKS: u64 = 600;

/// The populations to measure: the playground's default, and the largest a live window is asked for.
const POPULATIONS: [u32; 2] = [2_000, 10_000];

/// The components a renderer reads a body from.
type Crowd<'w> = (&'w Npc, &'w NpcIdentity, &'w NpcVelocity, &'w Transform);

/// A playground, or the reason there is not one.
fn played(population: u32) -> Option<Playground> {
    let mut playground = match Playground::new(SEED, population, EXTENT) {
        Ok(playground) => playground,
        Err(error) => {
            eprintln!("skipping {population}: {error}");
            return None;
        }
    };
    playground.tick(WARM_TICKS);
    Some(playground)
}

/// The body an NPC component set describes, as the readback builds it.
fn body_of(identity: &NpcIdentity, velocity: &NpcVelocity, transform: &Transform) -> Body {
    let yaw = if velocity.0.length_squared() > 1e-6 {
        velocity.0.x.atan2(velocity.0.z)
    } else {
        0.0
    };
    Body {
        archetype: identity.archetype,
        position: transform.translation,
        yaw,
        appearance_seed: identity.appearance_seed,
    }
}

/// What a frame pays to read the crowd back, and where it pays it.
fn readback(criterion: &mut Criterion) {
    for population in POPULATIONS {
        let Some(mut playground) = played(population) else {
            continue;
        };
        let mut group = criterion.benchmark_group(format!("readback_{population}"));

        group.bench_function("full", |bencher| {
            bencher.iter(|| black_box(playground.bodies()));
        });

        // What a running renderer does: one buffer, refilled, allocated once and then never again.
        let mut reused: Vec<Body> = Vec::with_capacity(usize::try_from(population).unwrap_or(0));
        group.bench_function("into_reused", |bencher| {
            bencher.iter(|| {
                playground.bodies_into(&mut reused);
                black_box(&reused);
            });
        });

        group.bench_function("fresh_query", |bencher| {
            bencher.iter(|| {
                let world = playground.app.world_mut();
                let mut query = world.query::<Crowd>();
                let mut collected: Vec<(u32, Body)> = Vec::new();
                for (npc, identity, velocity, transform) in query.iter(world) {
                    collected.push((npc.0.to_bits(), body_of(identity, velocity, transform)));
                }
                black_box(collected);
            });
        });

        let mut query: QueryState<Crowd> = playground.app.world_mut().query();
        let mut collected: Vec<(u32, Body)> = Vec::new();
        group.bench_function("reused_query", |bencher| {
            bencher.iter(|| {
                collected.clear();
                let world = playground.app.world();
                for (npc, identity, velocity, transform) in query.iter(world) {
                    collected.push((npc.0.to_bits(), body_of(identity, velocity, transform)));
                }
                black_box(&collected);
            });
        });

        group.finish();
    }
}

criterion_group!(benches, readback);
criterion_main!(benches);
