//! Personality, needs and mood.
//!
//! The design goal is that two NPCs running the same behaviour code behave
//! differently because of who they are, not because of a special case written
//! for "merchants". Temperament is the only input to that difference, and it is
//! sampled deterministically from the NPC's own stream, so personality is part
//! of the save file rather than a hash of the name.

use core::fmt;

use crate::domain::rng::RngStream;
use crate::domain::tick::Tick;
use crate::error::ConfigError;

/// A probability-like scalar in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Weight(f32);

impl Weight {
    /// The lowest possible weight.
    pub const MIN: Self = Self(0.0);

    /// The highest possible weight.
    pub const MAX: Self = Self(1.0);

    /// A weight exactly in the middle.
    pub const MID: Self = Self(0.5);

    /// Validate a weight.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::WeightOutOfRange`] for NaN, infinity or a value
    /// outside `0.0..=1.0`.
    pub fn try_new(field: &'static str, value: f32) -> Result<Self, ConfigError> {
        if !(0.0..=1.0).contains(&value) {
            return Err(ConfigError::WeightOutOfRange { field, value });
        }
        Ok(Self(value))
    }

    /// Clamp any float into `0.0..=1.0`.
    ///
    /// NaN becomes [`Weight::MID`] rather than zero: a broken sample should not
    /// silently turn an NPC into a personality-less shell, and NaN has no
    /// meaningful ordering to clamp toward.
    #[must_use]
    pub fn clamped(value: f32) -> Self {
        if value.is_nan() {
            Self::MID
        } else {
            Self(value.clamp(0.0, 1.0))
        }
    }

    /// The raw value.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }

    /// Move toward `target` by `rate` (a fraction of the remaining distance).
    #[must_use]
    pub fn approach_toward(self, target: Self, rate: f32) -> Self {
        let delta = (target.0 - self.0) * rate.clamp(0.0, 1.0);
        Self::clamped(self.0 + delta)
    }
}

impl Default for Weight {
    fn default() -> Self {
        Self::MID
    }
}

impl fmt::Display for Weight {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2}", self.0)
    }
}

/// The eight axes that define who an NPC is.
///
/// The first five are the Big Five, because a model that designers already know
/// beats a model invented for this crate. The last three are the axes a game
/// actually asks for by name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Temperament {
    /// Openness to novelty: high means curious, low means conventional.
    pub openness: Weight,
    /// Conscientiousness: high means orderly and reliable.
    pub conscientiousness: Weight,
    /// Extraversion: high means talkative and gregarious.
    pub extraversion: Weight,
    /// Agreeableness: high means cooperative, low means hostile.
    pub agreeableness: Weight,
    /// Neuroticism: high means easily upset and quick to panic.
    pub neuroticism: Weight,
    /// Bravery: the willingness to approach a threat.
    pub bravery: Weight,
    /// Sociability: how much the NPC seeks company.
    pub sociability: Weight,
    /// Industriousness: how much the NPC prefers to be doing something.
    pub industriousness: Weight,
}

impl Temperament {
    /// A neutral personality, all axes at the midpoint.
    #[must_use]
    pub const fn balanced() -> Self {
        Self {
            openness: Weight::MID,
            conscientiousness: Weight::MID,
            extraversion: Weight::MID,
            agreeableness: Weight::MID,
            neuroticism: Weight::MID,
            bravery: Weight::MID,
            sociability: Weight::MID,
            industriousness: Weight::MID,
        }
    }

    /// Sample a personality from the NPC's own stream.
    ///
    /// Draws are independent and in a fixed order, so adding an axis later is a
    /// breaking change to every stored replay and is treated as such.
    #[must_use]
    pub fn sample(rng: &mut RngStream) -> Self {
        Self {
            openness: Weight::clamped(0.5 + rng.next_range_f32(-0.35, 0.35)),
            conscientiousness: Weight::clamped(0.5 + rng.next_range_f32(-0.35, 0.35)),
            extraversion: Weight::clamped(0.5 + rng.next_range_f32(-0.35, 0.35)),
            agreeableness: Weight::clamped(0.5 + rng.next_range_f32(-0.35, 0.35)),
            neuroticism: Weight::clamped(0.5 + rng.next_range_f32(-0.35, 0.35)),
            bravery: Weight::clamped(0.5 + rng.next_range_f32(-0.35, 0.35)),
            sociability: Weight::clamped(0.5 + rng.next_range_f32(-0.35, 0.35)),
            industriousness: Weight::clamped(0.5 + rng.next_range_f32(-0.35, 0.35)),
        }
    }

    /// A preset personality with the shape its name implies.
    #[must_use]
    pub const fn archetype(archetype: Archetype) -> Self {
        match archetype {
            Archetype::Guard => Self {
                conscientiousness: Weight(0.85),
                bravery: Weight(0.9),
                agreeableness: Weight(0.6),
                neuroticism: Weight(0.25),
                ..Self::balanced()
            },
            Archetype::Merchant => Self {
                extraversion: Weight(0.85),
                agreeableness: Weight(0.75),
                openness: Weight(0.65),
                bravery: Weight(0.35),
                ..Self::balanced()
            },
            Archetype::Urchin => Self {
                openness: Weight(0.85),
                bravery: Weight(0.7),
                conscientiousness: Weight(0.3),
                ..Self::balanced()
            },
            Archetype::Noble => Self {
                conscientiousness: Weight(0.8),
                neuroticism: Weight(0.15),
                agreeableness: Weight(0.4),
                industriousness: Weight(0.3),
                ..Self::balanced()
            },
            Archetype::Wanderer => Self {
                openness: Weight(0.9),
                sociability: Weight(0.35),
                industriousness: Weight(0.4),
                ..Self::balanced()
            },
        }
    }

    /// The five axes averaged, for the NPCs that need one number.
    #[must_use]
    pub fn average(&self) -> Weight {
        let sum = self.openness.0
            + self.conscientiousness.0
            + self.extraversion.0
            + self.agreeableness.0
            + self.neuroticism.0;
        Weight::clamped(sum / 5.0)
    }

    /// How likely this NPC is to flee rather than stand, given a threat.
    #[must_use]
    pub fn flight_bias(&self) -> Weight {
        Weight::clamped(self.neuroticism.0.mul_add(0.7, -self.bravery.0 * 0.7) + 0.5)
    }

    /// How much this NPC wants company right now.
    #[must_use]
    pub fn company_appetite(&self) -> Weight {
        Weight::clamped(self.sociability.0.mul_add(0.6, self.extraversion.0 * 0.4))
    }
}

/// A coarse personality preset a generator can weight against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Archetype {
    /// A soldier, watchman or gatekeeper.
    Guard,
    /// A trader or shopkeeper.
    Merchant,
    /// A street child, courier or pickpocket.
    Urchin,
    /// Someone with money and no intention of moving quickly.
    Noble,
    /// A traveller, mercenary or nobody in particular.
    Wanderer,
}

impl Archetype {
    /// Every archetype, in a fixed order suitable for weighted selection.
    pub const ALL: [Self; 5] = [
        Self::Guard,
        Self::Merchant,
        Self::Urchin,
        Self::Noble,
        Self::Wanderer,
    ];

    /// A lowercase name for debug output and save files.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Guard => "guard",
            Self::Merchant => "merchant",
            Self::Urchin => "urchin",
            Self::Noble => "noble",
            Self::Wanderer => "wanderer",
        }
    }
}

impl fmt::Display for Archetype {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The need channels every NPC has.
///
/// Fixed-size rather than a map: four floats fit in a cache line, and a need
/// system that allocates is a need system that costs more than the cognition it
/// feeds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Needs {
    /// Physical tiredness; low means exhausted.
    pub energy: f32,
    /// Fullness; low means hungry.
    pub satiety: f32,
    /// Social contact; low means lonely.
    pub company: f32,
    /// Perceived safety; low means frightened.
    pub safety: f32,
}

impl Needs {
    /// Needs that are entirely satisfied.
    #[must_use]
    pub const fn satisfied() -> Self {
        Self {
            energy: 1.0,
            satiety: 1.0,
            company: 1.0,
            safety: 1.0,
        }
    }

    /// Needs that are entirely unmet.
    #[must_use]
    pub const fn depleted() -> Self {
        Self {
            energy: 0.0,
            satiety: 0.0,
            company: 0.0,
            safety: 0.0,
        }
    }

    /// Advance one tick of decay, scaled by temperament.
    ///
    /// An extravert loses company faster than a hermit; a neurotic NPC loses its
    /// sense of safety faster than a stoic. The rates are per-tick fractions, not
    /// per-second, so decay is identical at any frame rate.
    #[must_use]
    pub fn decayed(self, temperament: &Temperament, ticks: u64) -> Self {
        let ticks = ticks as f32;
        let company_rate = 0.0004 * temperament.company_appetite().0.mul_add(1.0, 0.5);
        let safety_rate = 0.0002 * temperament.neuroticism.0.mul_add(1.0, 0.5);
        Self {
            energy: (self.energy - 0.0003 * ticks).max(0.0),
            satiety: (self.satiety - 0.0005 * ticks).max(0.0),
            company: (self.company - company_rate * ticks).max(0.0),
            safety: (self.safety - safety_rate * ticks).max(0.0),
        }
    }

    /// Satisfy one channel by an absolute amount.
    #[must_use]
    pub fn satisfied_by(self, channel: NeedChannel, amount: f32) -> Self {
        let amount = amount.max(0.0);
        match channel {
            NeedChannel::Energy => Self {
                energy: (self.energy + amount).min(1.0),
                ..self
            },
            NeedChannel::Satiety => Self {
                satiety: (self.satiety + amount).min(1.0),
                ..self
            },
            NeedChannel::Company => Self {
                company: (self.company + amount).min(1.0),
                ..self
            },
            NeedChannel::Safety => Self {
                safety: (self.safety + amount).min(1.0),
                ..self
            },
        }
    }

    /// The most pressing unmet need, if any is below `threshold`.
    #[must_use]
    pub fn most_pressing(&self, threshold: f32) -> Option<NeedChannel> {
        let mut worst: Option<(NeedChannel, f32)> = None;
        for (channel, value) in [
            (NeedChannel::Energy, self.energy),
            (NeedChannel::Satiety, self.satiety),
            (NeedChannel::Company, self.company),
            (NeedChannel::Safety, self.safety),
        ] {
            if value >= threshold {
                continue;
            }
            let better = worst.is_none_or(|(_, current)| value < current);
            if better {
                worst = Some((channel, value));
            }
        }
        worst.map(|(channel, _)| channel)
    }

    /// Clamp every channel into `0.0..=1.0`, dropping NaN to zero.
    #[must_use]
    pub fn sanitised(self) -> Self {
        fn clean(value: f32) -> f32 {
            if value.is_nan() {
                0.0
            } else {
                value.clamp(0.0, 1.0)
            }
        }
        Self {
            energy: clean(self.energy),
            satiety: clean(self.satiety),
            company: clean(self.company),
            safety: clean(self.safety),
        }
    }
}

impl Default for Needs {
    fn default() -> Self {
        Self::satisfied()
    }
}

/// One need channel, addressable without a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NeedChannel {
    /// Tiredness.
    Energy,
    /// Hunger.
    Satiety,
    /// Loneliness.
    Company,
    /// Fear.
    Safety,
}

impl NeedChannel {
    /// Every channel, in the fixed order used by tie-breaks.
    pub const ALL: [Self; 4] = [Self::Energy, Self::Satiety, Self::Company, Self::Safety];

    /// A lowercase name for debug output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Energy => "energy",
            Self::Satiety => "satiety",
            Self::Company => "company",
            Self::Safety => "safety",
        }
    }
}

/// Emotional state, expressed on two axes rather than as a label.
///
/// Two continuous axes compose (a happy crowd under threat becomes tense, not
/// undefined), whereas a mood enum forces every consumer to enumerate
/// transitions by hand.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mood {
    /// How positive the NPC feels, in `-1.0..=1.0`.
    pub valence: f32,
    /// How activated the NPC feels, in `0.0..=1.0`.
    pub arousal: f32,
    /// The tick this mood was last updated on, for staggered decay.
    pub updated: Tick,
}

impl Mood {
    /// A calm, neutral mood.
    #[must_use]
    pub const fn neutral(at: Tick) -> Self {
        Self {
            valence: 0.0,
            arousal: 0.3,
            updated: at,
        }
    }

    /// Apply an event impulse, then decay toward neutral.
    ///
    /// Impulses are applied immediately and decay is a fraction of the remaining
    /// distance per tick, so a frightened NPC calms down over a predictable
    /// window instead of snapping back.
    #[must_use]
    pub fn step(self, at: Tick, impulse: (f32, f32), temperament: &Temperament) -> Self {
        let ticks = at.since(self.updated).max(1) as f32;
        let stability = 1.0 - temperament.neuroticism.0.mul_add(0.5, 0.0);
        let valence_decay = (1.0 - 0.002 * stability * ticks).clamp(0.0, 1.0);
        let arousal_decay = (1.0 - 0.004 * stability * ticks).clamp(0.0, 1.0);
        Self {
            valence: ((self.valence * valence_decay) + impulse.0).clamp(-1.0, 1.0),
            arousal: ((self.arousal * arousal_decay) + impulse.1).clamp(0.0, 1.0),
            updated: at,
        }
    }

    /// Whether the NPC reads as frightened: negative and highly activated.
    #[must_use]
    pub fn is_distressed(&self) -> bool {
        self.valence < -0.25 && self.arousal > 0.6
    }
}

impl Default for Mood {
    fn default() -> Self {
        Self::neutral(Tick::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::SimSeed;

    #[test]
    fn weights_reject_out_of_range_values() {
        assert!(Weight::try_new("x", -0.1).is_err());
        assert!(Weight::try_new("x", 1.1).is_err());
        assert!(Weight::try_new("x", f32::NAN).is_err());
        assert!(Weight::try_new("x", 0.5).is_ok());
    }

    #[test]
    fn clamped_nan_becomes_mid_not_zero() {
        assert_eq!(Weight::clamped(f32::NAN), Weight::MID);
        assert_eq!(Weight::clamped(-3.0), Weight::MIN);
        assert_eq!(Weight::clamped(3.0), Weight::MAX);
    }

    #[test]
    fn sampled_temperaments_stay_in_range() {
        let mut rng = RngStream::new(SimSeed::new(1), 1);
        for _ in 0..1_000 {
            let temperament = Temperament::sample(&mut rng);
            for weight in [
                temperament.openness,
                temperament.conscientiousness,
                temperament.extraversion,
                temperament.agreeableness,
                temperament.neuroticism,
                temperament.bravery,
                temperament.sociability,
                temperament.industriousness,
            ] {
                assert!((0.0..=1.0).contains(&weight.get()), "got {weight}");
            }
        }
    }

    #[test]
    fn sampling_is_deterministic_for_a_seed() {
        let mut a = RngStream::new(SimSeed::new(99), 9);
        let mut b = RngStream::new(SimSeed::new(99), 9);
        assert_eq!(Temperament::sample(&mut a), Temperament::sample(&mut b));
    }

    #[test]
    fn archetypes_differ_in_the_way_their_names_imply() {
        let guard = Temperament::archetype(Archetype::Guard);
        let urchin = Temperament::archetype(Archetype::Urchin);
        assert!(guard.bravery.get() > urchin.bravery.get());
        assert!(urchin.openness.get() > guard.openness.get());
    }

    #[test]
    fn flight_bias_tracks_neuroticism_and_bravery() {
        let bold = Temperament {
            neuroticism: Weight::MIN,
            bravery: Weight::MAX,
            ..Temperament::balanced()
        };
        let timid = Temperament {
            neuroticism: Weight::MAX,
            bravery: Weight::MIN,
            ..Temperament::balanced()
        };
        assert!(timid.flight_bias().get() > bold.flight_bias().get());
    }

    #[test]
    fn needs_decay_and_never_leave_the_unit_interval() {
        let temperament = Temperament::balanced();
        let mut needs = Needs::satisfied();
        for tick in 0..10_000 {
            needs = needs.decayed(&temperament, 1);
            assert!((0.0..=1.0).contains(&needs.energy), "tick {tick}");
            assert!((0.0..=1.0).contains(&needs.satiety), "tick {tick}");
            assert!((0.0..=1.0).contains(&needs.company), "tick {tick}");
            assert!((0.0..=1.0).contains(&needs.safety), "tick {tick}");
        }
        assert!(needs.energy < 1.0, "needs must actually decay");
    }

    #[test]
    fn decay_is_monotonic_without_satisfaction() {
        let temperament = Temperament::balanced();
        let mut needs = Needs::satisfied();
        let mut previous = needs.energy;
        for _ in 0..100 {
            needs = needs.decayed(&temperament, 1);
            assert!(needs.energy <= previous);
            previous = needs.energy;
        }
    }

    #[test]
    fn satisfying_caps_at_one() {
        let needs = Needs::depleted().satisfied_by(NeedChannel::Energy, 5.0);
        assert!((needs.energy - 1.0).abs() < f32::EPSILON);
        assert!(needs.satiety.abs() < f32::EPSILON);
    }

    #[test]
    fn most_pressing_finds_the_lowest_channel() {
        let needs = Needs {
            energy: 0.8,
            satiety: 0.1,
            company: 0.4,
            safety: 0.9,
        };
        assert_eq!(needs.most_pressing(0.5), Some(NeedChannel::Satiety));
        assert_eq!(needs.most_pressing(0.05), None);
    }

    #[test]
    fn sanitise_removes_nan() {
        let needs = Needs {
            energy: f32::NAN,
            satiety: 5.0,
            company: -1.0,
            safety: 0.5,
        }
        .sanitised();
        assert!(needs.energy.abs() < f32::EPSILON);
        assert!((needs.satiety - 1.0).abs() < f32::EPSILON);
        assert!(needs.company.abs() < f32::EPSILON);
    }

    #[test]
    fn mood_decays_toward_neutral() {
        let temperament = Temperament::balanced();
        let mut mood = Mood::neutral(Tick::ZERO);
        mood = mood.step(Tick::new(1), (-1.0, 1.0), &temperament);
        let peak = mood.valence;
        for tick in 2..200 {
            mood = mood.step(Tick::new(tick), (0.0, 0.0), &temperament);
        }
        assert!(mood.valence > peak, "mood must recover toward neutral");
        assert!((-1.0..=1.0).contains(&mood.valence));
        assert!((0.0..=1.0).contains(&mood.arousal));
    }

    #[test]
    fn distress_is_detectable() {
        let frightened = Mood {
            valence: -0.8,
            arousal: 0.9,
            updated: Tick::ZERO,
        };
        assert!(frightened.is_distressed());
        assert!(!Mood::neutral(Tick::ZERO).is_distressed());
    }
}
