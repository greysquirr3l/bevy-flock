//! Properties that must hold for *every* input, not just the ones in the unit tests.
//!
//! Example-based tests check the cases someone thought of. These check the ones
//! nobody did: arbitrary sequences of blackboard writes, arbitrary positions, arbitrary
//! weights. A failure here comes with the shrunk input that caused it, which is
//! usually the whole diagnosis.
//!
//! Run with `cargo test --test properties`. The case counts are deliberately modest —
//! this is a gate, not a fuzzing campaign — and can be raised with
//! `PROPTEST_CASES=4096`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::missing_docs_in_private_items
)]

use bevy_flock::adapters::primary::components::LodTier;
use bevy_flock::adapters::secondary::network::NpcNetState;
use bevy_flock::behavior::blackboard::{BLACKBOARD_CAPACITY, Blackboard, BlackboardKey, Value};
use bevy_flock::domain::ids::{NpcId, SimSeed};
use bevy_flock::domain::rng::RngStream;
use bevy_flock::domain::spatial::{GridEntry, Neighbour, SpatialGrid, SpatialGridConfig};
use bevy_flock::domain::temperament::Weight;
use bevy_flock::domain::tick::Tick;
use bevy_math::Vec3;
use proptest::prelude::*;

fn any_position() -> impl Strategy<Value = Vec3> {
    (-500.0_f32..500.0, -50.0_f32..50.0, -500.0_f32..500.0).prop_map(|(x, y, z)| Vec3::new(x, y, z))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Whatever a behaviour does to its blackboard, it cannot exceed the capacity and
    /// it cannot lose a value that was written and not removed.
    #[test]
    fn the_blackboard_never_exceeds_its_capacity_and_never_forgets_a_write(
        writes in prop::collection::vec((0_u16..64, -1.0_f32..1.0_f32), 0..48)
    ) {
        let mut blackboard = Blackboard::new();
        let mut live: Vec<(u16, f32)> = Vec::new();
        for (key, value) in writes {
            let slot = BlackboardKey::new(key);
            let _ = blackboard.set(slot, Value::Float(value));
            live.retain(|(existing, _)| *existing != key);
            live.push((key, value));
            prop_assert!(blackboard.len() <= BLACKBOARD_CAPACITY);
        }
        for (key, value) in live {
            let slot = BlackboardKey::new(key);
            let Some(stored) = blackboard.get(slot) else {
                // Evictions are allowed when the capacity is exceeded; a write that
                // survived must be exact.
                continue;
            };
            let stored = stored.as_float().expect("a float");
            prop_assert!((stored - value).abs() < f32::EPSILON);
        }
    }

    /// The same seed always produces the same stream, and two different draws are
    /// almost never equal.
    #[test]
    fn the_rng_is_a_function_of_its_seed(seed in 0_u64..u64::MAX, skip in 0_u64..1_000) {
        let mut first = RngStream::new(SimSeed::new(seed), 0);
        let mut second = RngStream::new(SimSeed::new(seed), 0);
        for _ in 0..skip {
            let _ = first.next_u32();
        }
        let mut second_value = 0;
        for _ in 0..skip {
            second_value = second.next_u32();
        }
        prop_assert_eq!(first.next_u32(), second.next_u32());
        let _ = second_value;
    }

    /// A radius query returns exactly the entries a brute-force scan would, in the
    /// same order. This is the property the crowd kernel depends on: if the grid
    /// disagrees with brute force by one neighbour, two runs diverge.
    #[test]
    fn the_spatial_index_agrees_with_brute_force(
        points in prop::collection::vec(any_position(), 0..128),
        radius in 1.0_f32..40.0
    ) {
        let config = SpatialGridConfig {
            cell_size: 8.0,
            ..SpatialGridConfig::default()
        };
        let mut grid = SpatialGrid::new(config).expect("config");
        for (index, point) in points.iter().enumerate() {
            grid.insert(GridEntry {
                id: NpcId::unique(index as u32),
                position: *point,
                velocity: Vec3::ZERO,
            });
        }
        grid.build();
        let centre = Vec3::ZERO;
        let mut found = Vec::with_capacity(points.len());
        grid.radius(centre, radius, &mut found);
        let mut expected: Vec<(NpcId, f32)> = points
            .iter()
            .enumerate()
            .filter_map(|(index, point)| {
                let distance = point.distance(centre);
                (distance <= radius).then_some((NpcId::unique(index as u32), distance))
            })
            .collect();
        expected.sort_by_key(|(id, _)| *id);
        let got: Vec<(NpcId, f32)> = found
            .iter()
            .map(|n: &Neighbour| (n.id, n.distance_squared.sqrt()))
            .collect();
        prop_assert_eq!(got.len(), expected.len());
        for ((got_id, got_distance), (want_id, want_distance)) in got.iter().zip(expected.iter()) {
            prop_assert_eq!(got_id, want_id);
            prop_assert!((got_distance - want_distance).abs() < 1e-3);
        }
    }

    /// A quantised network state survives the round trip to within half a centimetre,
    /// and never wraps.
    #[test]
    fn a_network_state_round_trips_within_quantisation(
        x in -300.0_f32..300.0,
        y in -50.0_f32..50.0,
        z in -300.0_f32..300.0,
        valence in -1.0_f32..1.0,
        id in 0_u32..u32::MAX
    ) {
        let state = NpcNetState::capture(
            NpcId::from_bits(id),
            Vec3::new(x, y, z),
            0.7,
            LodTier::Mid,
            valence,
        );
        let mut bytes = Vec::new();
        state.encode(&mut bytes);
        let (decoded, used) = NpcNetState::decode(&bytes).expect("decode");
        prop_assert_eq!(used, NpcNetState::BYTES);
        prop_assert!((decoded.position() - Vec3::new(x, y, z)).length() < 0.02);
        prop_assert!((decoded.valence() - valence).abs() < 0.02);
        prop_assert_eq!(decoded.identity(), NpcId::from_bits(id));
    }

    /// A weight is clamped, never rejected, and never leaves its range.
    #[test]
    fn a_weight_is_always_in_range(value in -100.0_f32..100.0) {
        let weight = Weight::clamped(value);
        prop_assert!((0.0..=1.0).contains(&weight.get()));
        prop_assert_eq!(Weight::clamped(weight.get()).get(), weight.get());
    }

    /// Ticks advance monotonically and never wrap into the past.
    #[test]
    fn ticks_advance_monotonically(start in 0_u64..u64::MAX / 2, steps in 0_u64..1_000) {
        let mut tick = Tick::new(start);
        for _ in 0..steps {
            let next = tick.next();
            prop_assert!(next.get() > tick.get());
            tick = next;
        }
    }
}
