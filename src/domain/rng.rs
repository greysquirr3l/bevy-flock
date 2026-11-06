//! Deterministic random number generation.
//!
//! The crate does not depend on `rand`. Two reasons:
//!
//! 1. A dependency on `rand` is a dependency the consumer must agree with; a
//!    crowd library that pins a `rand` version can force a game to upgrade or
//!    duplicate.
//! 2. Determinism is a documented guarantee, and the only way to keep it is to
//!    own the algorithm. [`RngStream`] implements PCG-XSH-RR 32/64, the reference
//!    `pcg32` generator, in about forty lines.
//!
//! Every NPC gets its own stream, derived from the world seed and the NPC's
//! identifier, so drawing a random number for one NPC never shifts another's
//! sequence.

use core::fmt;

use crate::domain::ids::{NpcId, SimSeed};

/// PCG32 multiplier, from the reference implementation.
const MULTIPLIER: u64 = 6_364_136_223_846_793_005;

/// A deterministic, seekable random stream.
///
/// The stream is `Clone`, so a system can hand each member of a group a copy
/// without sharing state, and `Debug`, so a failing test prints something
/// readable and reproducible.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RngStream {
    state: u64,
    increment: u64,
    draws: u64,
}

impl RngStream {
    /// Create a stream from a seed and a stream selector.
    ///
    /// Different selectors produce independent sequences for the same seed.
    #[must_use]
    pub const fn new(seed: SimSeed, selector: u64) -> Self {
        let increment = (selector << 1) | 1;
        // The reference implementation seeds by stepping once with a zero state,
        // adding the seed, then stepping again. Doing it inline keeps the
        // constructor `const`.
        let mut state = 0_u64;
        state = state.wrapping_mul(MULTIPLIER).wrapping_add(increment);
        state = state.wrapping_add(seed.get());
        state = state.wrapping_mul(MULTIPLIER).wrapping_add(increment);
        Self {
            state,
            increment,
            draws: 0,
        }
    }

    /// The stream assigned to one NPC.
    ///
    /// This is a pure function of the world seed and the NPC identifier, which
    /// is what makes adding a hundred NPCs to a world leave every existing
    /// NPC's future draws untouched.
    #[must_use]
    pub const fn for_npc(seed: SimSeed, npc: NpcId) -> Self {
        // Mix the identifier so that consecutive NPC ids do not produce
        // correlated streams: PCG streams differing only in the low increment
        // bits share structure.
        // `as` rather than `u64::from` because `From` is not yet callable from a
        // `const fn`, and this constructor must stay const for static presets.
        let selector = splitmix64(seed.get() ^ npc.to_bits() as u64).rotate_left(17);
        Self::new(seed, selector)
    }

    /// How many values this stream has produced.
    #[must_use]
    pub const fn draws(&self) -> u64 {
        self.draws
    }

    /// Rewind the draw counter without rewinding the sequence.
    ///
    /// Used by snapshot restoration, where the state is restored wholesale
    /// rather than by replaying draws.
    pub const fn set_draws(&mut self, draws: u64) {
        self.draws = draws;
    }

    /// The raw 64-bit state, for snapshotting.
    #[must_use]
    pub const fn state_bits(&self) -> (u64, u64) {
        (self.state, self.increment)
    }

    /// Rebuild a stream from [`RngStream::state_bits`].
    #[must_use]
    pub const fn from_state_bits(state: u64, increment: u64, draws: u64) -> Self {
        Self {
            state,
            increment: increment | 1,
            draws,
        }
    }

    /// Draw the next 32 bits.
    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old.wrapping_mul(MULTIPLIER).wrapping_add(self.increment);
        self.draws = self.draws.wrapping_add(1);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rotation = (old >> 59) as u32;
        xorshifted.rotate_right(rotation)
    }

    /// Draw the next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        let low = u64::from(self.next_u32());
        let high = u64::from(self.next_u32());
        (high << 32) | low
    }

    /// Draw a value in `0..bound` without modulo bias.
    ///
    /// Uses Lemire's multiply-shift with the standard rejection so the
    /// distribution is exactly uniform for any bound.
    pub fn next_bounded(&mut self, bound: u32) -> u32 {
        if bound <= 1 {
            return 0;
        }
        let mut product = u64::from(self.next_u32()) * u64::from(bound);
        let mut low = product as u32;
        if low < bound {
            let threshold = bound.wrapping_neg() % bound;
            while low < threshold {
                product = u64::from(self.next_u32()) * u64::from(bound);
                low = product as u32;
            }
        }
        (product >> 32) as u32
    }

    /// Draw a value in `[0, 1)`.
    ///
    /// Built by constructing a float in `[1, 2)` from raw bits and subtracting
    /// one, so the result never depends on platform float conversion rules.
    pub fn next_unit_f32(&mut self) -> f32 {
        let bits = (self.next_u32() >> 9) | 0x3F80_0000;
        f32::from_bits(bits) - 1.0
    }

    /// Draw a value in `[low, high)`.
    pub fn next_range_f32(&mut self, low: f32, high: f32) -> f32 {
        (high - low).mul_add(self.next_unit_f32(), low).min(high)
    }

    /// Draw a value in `[low, high)` for integers.
    pub fn next_range_u32(&mut self, low: u32, high: u32) -> u32 {
        if high <= low {
            return low;
        }
        low.saturating_add(self.next_bounded(high.saturating_sub(low)))
    }

    /// Draw a boolean with probability `p`.
    pub fn next_bool(&mut self, p: f32) -> bool {
        self.next_unit_f32() < p
    }

    /// Shuffle a slice in place.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        if items.len() < 2 {
            return;
        }
        for index in (1..items.len()).rev() {
            let bound = (index + 1) as u32;
            let swap_with = self.next_bounded(bound) as usize;
            items.swap(index, swap_with);
        }
    }

    /// Pick an index from a slice of weights.
    ///
    /// Returns `None` when every weight is zero or the slice is empty, so the
    /// caller decides the fallback rather than receiving a silent `0`.
    pub fn choose_weighted(&mut self, weights: &[f32]) -> Option<usize> {
        let total: f32 = weights.iter().copied().filter(|w| *w > 0.0).sum();
        if total <= 0.0 {
            return None;
        }
        let mut remaining = self.next_range_f32(0.0, total);
        for (index, weight) in weights.iter().copied().enumerate() {
            if weight <= 0.0 {
                continue;
            }
            if remaining < weight {
                return Some(index);
            }
            remaining -= weight;
        }
        // Floating-point round-off can land exactly on the total; the last
        // positively weighted item is the correct answer in that case.
        weights.iter().rposition(|w| *w > 0.0)
    }

    /// Split off an independent child stream.
    #[must_use]
    pub fn split(&mut self) -> Self {
        let seed = SimSeed::new(self.next_u64());
        Self::new(seed, self.next_u64())
    }
}

impl fmt::Display for RngStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rng(0x{:016x}/0x{:016x})", self.state, self.increment)
    }
}

/// `SplitMix64`, used only to derive stream selectors from identifiers.
const fn splitmix64(state: u64) -> u64 {
    let mut z = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::GetOrDieMut;

    /// A direct transcription of the reference `pcg32` from pcg-random.org,
    /// written independently of [`RngStream`] so the test cannot pass by sharing
    /// a bug with the implementation.
    struct ReferencePcg32 {
        state: u64,
        inc: u64,
    }

    impl ReferencePcg32 {
        fn seeded(seed: u64, stream: u64) -> Self {
            let mut rng = Self {
                state: 0,
                inc: (stream << 1) | 1,
            };
            rng.next();
            rng.state = rng.state.wrapping_add(seed);
            rng.next();
            rng
        }

        fn next(&mut self) -> u32 {
            let oldstate = self.state;
            self.state = oldstate
                .wrapping_mul(6_364_136_223_846_793_005_u64)
                .wrapping_add(self.inc);
            // The reference applies `>> 27` after the xor, before the truncation.
            let xorshifted = ((oldstate >> 18) ^ oldstate) >> 27;
            let xorshifted = xorshifted as u32;
            let rot = (oldstate >> 59) as u32;
            (xorshifted >> rot) | (xorshifted << ((!rot).wrapping_add(1) & 31))
        }
    }

    #[test]
    fn matches_the_reference_generator_for_ten_thousand_draws() {
        let mut ours = RngStream::new(SimSeed::new(42), 7);
        let mut reference = ReferencePcg32::seeded(42, 7);
        for draw in 0..10_000 {
            assert_eq!(ours.next_u32(), reference.next(), "diverged at draw {draw}");
        }
    }

    #[test]
    fn golden_vector_is_stable() {
        // Regenerate deliberately, never to make a test pass: a change here
        // means the crate's determinism guarantee changed and every stored
        // replay is invalid.
        let mut rng = RngStream::new(SimSeed::new(0), 0);
        let observed: Vec<u32> = (0..8).map(|_| rng.next_u32()).collect();
        assert_eq!(
            observed,
            vec![
                3_837_872_008,
                932_996_374,
                1_548_399_547,
                1_612_522_464,
                473_443_212,
                3_522_865_942,
                1_734_871_597,
                2_449_558_126,
            ],
            "the RNG golden vector moved; that breaks every stored replay"
        );
    }

    #[test]
    fn bounded_draws_stay_in_range() {
        let mut rng = RngStream::new(SimSeed::new(9), 1);
        for _ in 0..100_000 {
            let value = rng.next_bounded(10);
            assert!(value < 10);
        }
        assert_eq!(rng.next_bounded(0), 0);
        assert_eq!(rng.next_bounded(1), 0);
    }

    #[test]
    fn bounded_draws_are_uniform_within_one_percent() {
        const DRAWS: u32 = 1_000_000;
        let mut rng = RngStream::new(SimSeed::new(0xBEEF), 3);
        let mut buckets = [0_u32; 10];
        for _ in 0..DRAWS {
            *buckets.at_mut(rng.next_bounded(10) as usize) += 1;
        }
        let expected = DRAWS / 10;
        let tolerance = expected / 100;
        for (index, count) in buckets.iter().enumerate() {
            let delta = count.abs_diff(expected);
            assert!(
                delta <= tolerance,
                "bucket {index} had {count}, expected {expected} +/- {tolerance}"
            );
        }
    }

    #[test]
    fn unit_draws_are_in_the_half_open_unit_interval() {
        let mut rng = RngStream::new(SimSeed::new(3), 5);
        for _ in 0..1_000_000 {
            let value = rng.next_unit_f32();
            assert!((0.0..1.0).contains(&value), "got {value}");
        }
    }

    #[test]
    fn range_draws_respect_bounds_and_degenerate_ranges() {
        let mut rng = RngStream::new(SimSeed::new(11), 2);
        for _ in 0..100_000 {
            let value = rng.next_range_f32(-5.0, 5.0);
            assert!((-5.0..=5.0).contains(&value), "got {value}");
        }
        assert!((rng.next_range_f32(3.0, 3.0) - 3.0).abs() < f32::EPSILON);
    }

    #[test]
    fn per_npc_streams_are_independent() {
        let seed = SimSeed::new(1234);
        let mut a = RngStream::for_npc(seed, NpcId::unique(5));
        let mut b = RngStream::for_npc(seed, NpcId::unique(6));

        let a_draws: Vec<u32> = (0..16)
            .map(|_| {
                let value = a.next_u32();
                // Interleave draws from the other stream; they must not affect
                // this one.
                b.next_u32();
                value
            })
            .collect();

        let mut a_again = RngStream::for_npc(seed, NpcId::unique(5));
        let solo: Vec<u32> = (0..16).map(|_| a_again.next_u32()).collect();
        assert_eq!(a_draws, solo);
    }

    #[test]
    fn per_npc_streams_differ_between_neighbouring_ids() {
        let seed = SimSeed::new(7);
        let mut a = RngStream::for_npc(seed, NpcId::unique(0));
        let mut b = RngStream::for_npc(seed, NpcId::unique(1));
        let a_first = a.next_u32();
        let b_first = b.next_u32();
        assert_ne!(a_first, b_first);
    }

    #[test]
    fn shuffle_is_a_permutation_and_is_deterministic() {
        let mut rng = RngStream::new(SimSeed::new(5), 5);
        let mut items: Vec<u32> = (0..64).collect();
        rng.shuffle(&mut items);
        let mut sorted = items.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..64).collect::<Vec<_>>());

        let mut rng_again = RngStream::new(SimSeed::new(5), 5);
        let mut again: Vec<u32> = (0..64).collect();
        rng_again.shuffle(&mut again);
        assert_eq!(items, again);
    }

    #[test]
    fn shuffle_handles_degenerate_slices() {
        let mut rng = RngStream::new(SimSeed::new(1), 1);
        let mut empty: [u8; 0] = [];
        rng.shuffle(&mut empty);
        let mut single = [1_u8];
        rng.shuffle(&mut single);
        assert_eq!(single, [1]);
    }

    #[test]
    fn weighted_choice_respects_zero_weights_and_empty_input() {
        let mut rng = RngStream::new(SimSeed::new(2), 2);
        assert_eq!(rng.choose_weighted(&[]), None);
        assert_eq!(rng.choose_weighted(&[0.0, 0.0]), None);
        let picked = rng.choose_weighted(&[0.0, 1.0, 0.0]);
        assert_eq!(picked, Some(1));
    }

    #[test]
    fn weighted_choice_favours_heavier_options() {
        let mut rng = RngStream::new(SimSeed::new(4), 4);
        let mut counts = [0_u32; 2];
        for _ in 0..100_000 {
            match rng.choose_weighted(&[0.9, 0.1]) {
                Some(index) => *counts.at_mut(index) += 1,
                None => unreachable!("weights were positive"),
            }
        }
        assert!(counts[0] > counts[1] * 4, "counts were {counts:?}");
    }

    #[test]
    fn state_round_trip_resumes_the_sequence() {
        let mut rng = RngStream::new(SimSeed::new(88), 12);
        for _ in 0..37 {
            rng.next_u32();
        }
        let (state, increment) = rng.state_bits();
        let draws = rng.draws();
        let expected = rng.next_u32();

        let mut restored = RngStream::from_state_bits(state, increment, draws);
        assert_eq!(restored.next_u32(), expected);
    }

    #[test]
    fn split_produces_an_independent_stream() {
        let mut parent = RngStream::new(SimSeed::new(1), 1);
        let mut child = parent.split();
        let child_first = child.next_u32();
        let parent_next = parent.next_u32();
        assert_ne!(child_first, parent_next);
    }
}
