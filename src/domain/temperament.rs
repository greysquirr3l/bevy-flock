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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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

    /// A blend of two weights: `share` of `other`, the rest of `self`.
    ///
    /// This is how an archetype preset and an individual's own sample become one
    /// personality — see [`Temperament::generated`].
    #[must_use]
    pub fn blend(self, other: Self, share: f32) -> Self {
        let share = share.clamp(0.0, 1.0);
        Self::clamped(self.0.mul_add(1.0 - share, other.0 * share))
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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

    /// How this personality bends the steering weights.
    ///
    /// Every axis is a multiplier centred on `1.0`, so `gain` of zero returns
    /// [`WeightBias::NEUTRAL`] and the crowd behaves as one. The mapping is
    /// deliberately legible rather than fitted: the point is that two NPCs running the
    /// same behaviour code keep different distances, not that the numbers are optimal.
    ///
    /// - **separation** rises with neuroticism and falls with agreeableness: a jumpy,
    ///   disagreeable NPC keeps more room.
    /// - **alignment** rises with conscientiousness and agreeableness: a follower
    ///   matches the crowd it is in.
    /// - **cohesion** is [`Temperament::company_appetite`]: a gregarious NPC drifts
    ///   toward the middle of the group.
    /// - **seek** rises with industriousness and openness: a busy, curious NPC pushes
    ///   toward its target harder.
    /// - **arrive** is conscientiousness: a careful NPC slows earlier and smoother.
    #[must_use]
    pub fn steering_bias(&self, gain: Weight) -> WeightBias {
        let g = gain.get();
        let axis = |shape: f32| 1.0 + g * (2.0 * shape - 1.0);
        let room = self
            .neuroticism
            .0
            .mul_add(0.6, (1.0 - self.agreeableness.0) * 0.4);
        let follow = self
            .conscientiousness
            .0
            .mul_add(0.6, self.agreeableness.0 * 0.4);
        let purpose = self.industriousness.0.mul_add(0.6, self.openness.0 * 0.4);
        WeightBias {
            separation: axis(room),
            alignment: axis(follow),
            cohesion: axis(self.company_appetite().0),
            seek: axis(purpose),
            arrive: axis(self.conscientiousness.0),
        }
        .clamped()
    }

    /// The personality a generator gives an archetype: two thirds preset, one third
    /// individual.
    ///
    /// [`Temperament::archetype`] alone would make every guard the same guard, and
    /// [`Temperament::sample`] alone would make the archetype a costume — which is exactly what the
    /// spawner used to do, so a guard was no braver than an urchin. This is the same
    /// two-thirds/one-third blend `domain::generator` applies to archetype weights, so the two
    /// generators agree about what an archetype means.
    ///
    /// The individual is drawn first and in full, so a seed that produced a given personality
    /// before still draws the same numbers; only the blend on top of them is new.
    #[must_use]
    pub fn generated(archetype: Archetype, rng: &mut RngStream) -> Self {
        let individual = Self::sample(rng);
        Self::blend(Self::archetype(archetype), individual, 0.34)
    }

    /// Blend a preset with an individual: `share` of `individual`, the rest preset.
    #[must_use]
    pub fn blend(preset: Self, individual: Self, share: f32) -> Self {
        Self {
            openness: preset.openness.blend(individual.openness, share),
            conscientiousness: preset
                .conscientiousness
                .blend(individual.conscientiousness, share),
            extraversion: preset.extraversion.blend(individual.extraversion, share),
            agreeableness: preset.agreeableness.blend(individual.agreeableness, share),
            neuroticism: preset.neuroticism.blend(individual.neuroticism, share),
            bravery: preset.bravery.blend(individual.bravery, share),
            sociability: preset.sociability.blend(individual.sociability, share),
            industriousness: preset
                .industriousness
                .blend(individual.industriousness, share),
        }
    }
}

/// One multiplier, clamped into the permitted range, with NaN read as neutral.
fn clean_multiplier(value: f32) -> f32 {
    if value.is_nan() {
        1.0
    } else {
        value.clamp(WeightBias::MIN, WeightBias::MAX)
    }
}

/// Per-agent multipliers on the steering weights.
///
/// A bias is *relative*: the weights a game configured stay the baseline, and this
/// scales them per agent, so tuning a crowd still works the way the documentation says.
/// Every field is centred on `1.0` and clamped into the range between [`WeightBias::MIN`] and
/// [`WeightBias::MAX`], which bounds what one personality can do to a frame.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeightBias {
    /// Multiplier on separation.
    pub separation: f32,
    /// Multiplier on alignment.
    pub alignment: f32,
    /// Multiplier on cohesion.
    pub cohesion: f32,
    /// Multiplier on seek.
    pub seek: f32,
    /// Multiplier on arrival.
    pub arrive: f32,
}

impl WeightBias {
    /// No change to any weight.
    pub const NEUTRAL: Self = Self {
        separation: 1.0,
        alignment: 1.0,
        cohesion: 1.0,
        seek: 1.0,
        arrive: 1.0,
    };

    /// The smallest multiplier any axis may take.
    pub const MIN: f32 = 0.25;

    /// The largest multiplier any axis may take.
    pub const MAX: f32 = 4.0;

    /// Every axis set to the same multiplier.
    #[must_use]
    pub const fn uniform(value: f32) -> Self {
        Self {
            separation: value,
            alignment: value,
            cohesion: value,
            seek: value,
            arrive: value,
        }
    }

    /// Clamp every axis into the permitted range, dropping NaN to `1.0`.
    ///
    /// NaN is neutral rather than extreme: a broken sample should leave the agent steering
    /// normally instead of freezing it in place.
    #[must_use]
    pub fn clamped(self) -> Self {
        Self {
            separation: clean_multiplier(self.separation),
            alignment: clean_multiplier(self.alignment),
            cohesion: clean_multiplier(self.cohesion),
            seek: clean_multiplier(self.seek),
            arrive: clean_multiplier(self.arrive),
        }
    }

    /// True when the bias leaves every weight alone, so a kernel can skip it.
    #[must_use]
    pub fn is_neutral(&self) -> bool {
        *self == Self::NEUTRAL
    }

    /// Compose two biases multiplicatively.
    #[must_use]
    pub fn combined(self, other: Self) -> Self {
        Self {
            separation: self.separation * other.separation,
            alignment: self.alignment * other.alignment,
            cohesion: self.cohesion * other.cohesion,
            seek: self.seek * other.seek,
            arrive: self.arrive * other.arrive,
        }
        .clamped()
    }

    /// The bias as the array the steering kernel consumes.
    #[must_use]
    pub const fn to_array(self) -> [f32; 5] {
        [
            self.separation,
            self.alignment,
            self.cohesion,
            self.seek,
            self.arrive,
        ]
    }

    /// A bias from the kernel's array form.
    #[must_use]
    pub const fn from_array(values: [f32; 5]) -> Self {
        Self {
            separation: values[0],
            alignment: values[1],
            cohesion: values[2],
            seek: values[3],
            arrive: values[4],
        }
    }

    /// Apply the bias to a `[separation, alignment, cohesion, seek, arrive]` weight set.
    #[must_use]
    pub fn apply(self, weights: [f32; 5]) -> [f32; 5] {
        let bias = self.to_array();
        let mut out = weights;
        for (weight, scale) in out.iter_mut().zip(bias) {
            *weight *= scale;
        }
        out
    }
}

/// How far temperament and needs are allowed to move a crowd's steering weights.
///
/// Both fields are [`Weight`]s, so the answer to "how much difference can one personality
/// make?" is a number in `0.0..=1.0` rather than an unbounded knob: `gain` of `0.5` keeps
/// every temperament multiplier inside `0.5..=1.5`.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TemperamentDrive {
    gain: Weight,
    needs: Weight,
}

impl TemperamentDrive {
    /// The shipped setting: half a swing from temperament, half from unmet needs.
    pub const DEFAULT: Self = Self {
        gain: Weight(0.5),
        needs: Weight(0.5),
    };

    /// Build a drive, validating both shares.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::WeightOutOfRange`] when either share is outside `0.0..=1.0`.
    pub fn try_new(gain: f32, needs: f32) -> Result<Self, ConfigError> {
        Ok(Self {
            gain: Weight::try_new("temperament_gain", gain)?,
            needs: Weight::try_new("needs_influence", needs)?,
        })
    }

    /// A drive with known-valid shares.
    #[must_use]
    pub const fn new(gain: Weight, needs: Weight) -> Self {
        Self { gain, needs }
    }

    /// A drive that changes nothing, for a game that wants uniform steering.
    #[must_use]
    pub const fn inert() -> Self {
        Self {
            gain: Weight::MIN,
            needs: Weight::MIN,
        }
    }

    /// The temperament share.
    #[must_use]
    pub const fn gain(self) -> Weight {
        self.gain
    }

    /// The needs share.
    #[must_use]
    pub const fn needs(self) -> Weight {
        self.needs
    }

    /// The bias one agent's personality and needs ask for.
    ///
    /// The two are composed multiplicatively and then clamped, so a frightened NPC with a
    /// gregarious personality both keeps its distance and still wants company.
    #[must_use]
    pub fn bias(self, temperament: &Temperament, needs: &Needs) -> WeightBias {
        self.temperament_bias(temperament)
            .combined(self.needs_bias(needs))
    }

    /// The bias the personality alone asks for.
    #[must_use]
    pub fn temperament_bias(self, temperament: &Temperament) -> WeightBias {
        if self.gain == Weight::MIN {
            return WeightBias::NEUTRAL;
        }
        temperament.steering_bias(self.gain)
    }

    /// The bias the unmet needs alone ask for.
    #[must_use]
    pub fn needs_bias(self, needs: &Needs) -> WeightBias {
        if self.needs == Weight::MIN {
            return WeightBias::NEUTRAL;
        }
        needs.steering_bias(self.needs)
    }
}

impl Default for TemperamentDrive {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A coarse personality preset a generator can weight against.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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

    /// How unmet this NPC's needs are, as one number: `1.0` is desperate, `0.0` is content.
    ///
    /// The worst channel rather than the mean: one unmet need is what changes behaviour, and
    /// averaging it away against three satisfied ones would hide exactly the case a game cares
    /// about.
    #[must_use]
    pub fn pressure(&self) -> Weight {
        let worst = self
            .energy
            .min(self.satiety)
            .min(self.company)
            .min(self.safety);
        Weight::clamped(1.0 - worst)
    }

    /// How this NPC's unmet needs bend the steering weights.
    ///
    /// - **cohesion** rises with loneliness: people drift toward company.
    /// - **separation** rises with fear: a frightened NPC gives everyone more room.
    /// - **seek** rises with hunger and falls with fatigue, so the hungry press on and the tired
    ///   drag.
    /// - **arrive** rises with fear: a frightened NPC slows sooner rather than running into
    ///   whatever it was heading for.
    ///
    /// Unlike [`Temperament::steering_bias`], this is not symmetric around `1.0`: needs only ever
    /// have one direction, and the clamp stops a starving crowd from becoming a different crowd.
    #[must_use]
    pub fn steering_bias(&self, influence: Weight) -> WeightBias {
        let i = influence.get();
        let hunger = 1.0 - self.satiety.clamp(0.0, 1.0);
        let fatigue = 1.0 - self.energy.clamp(0.0, 1.0);
        let lonely = 1.0 - self.company.clamp(0.0, 1.0);
        let frightened = 1.0 - self.safety.clamp(0.0, 1.0);
        WeightBias {
            separation: (1.0 + i * frightened).min(WeightBias::MAX),
            alignment: 1.0,
            cohesion: (1.0 + i * lonely).min(WeightBias::MAX),
            seek: (1.0 + i * (hunger - fatigue)).clamp(WeightBias::MIN, WeightBias::MAX),
            arrive: (1.0 + i * frightened).min(WeightBias::MAX),
        }
        .clamped()
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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

    /// A stream with a fixed seed, for the tests that need two different people.
    fn stream(seed: u64) -> RngStream {
        RngStream::for_npc(SimSeed::new(seed), crate::domain::ids::NpcId::unique(0))
    }

    #[test]
    fn a_zero_gain_leaves_every_weight_alone() {
        let mut rng = stream(1);
        for _ in 0..8 {
            let who = Temperament::sample(&mut rng);
            assert_eq!(who.steering_bias(Weight::MIN), WeightBias::NEUTRAL);
            assert!(who.steering_bias(Weight::MIN).is_neutral());
        }
    }

    #[test]
    fn a_jumpy_disagreeable_person_keeps_more_room() {
        let jumpy = Temperament {
            neuroticism: Weight(0.95),
            agreeableness: Weight(0.05),
            ..Temperament::balanced()
        };
        let easy = Temperament {
            neuroticism: Weight(0.05),
            agreeableness: Weight(0.95),
            ..Temperament::balanced()
        };
        let gain = Weight(0.5);
        assert!(jumpy.steering_bias(gain).separation > 1.4);
        assert!(easy.steering_bias(gain).separation < 0.65);
        // The rest of the axes stay inside the gain's band even though only one moved far.
        for bias in [jumpy.steering_bias(gain), easy.steering_bias(gain)] {
            assert!(bias.separation <= 1.5 && bias.separation >= 0.5);
        }
    }

    #[test]
    fn a_generous_person_drifts_with_the_crowd() {
        let gregarious = Temperament {
            sociability: Weight(1.0),
            extraversion: Weight(1.0),
            ..Temperament::balanced()
        };
        let hermit = Temperament {
            sociability: Weight(0.0),
            extraversion: Weight(0.0),
            ..Temperament::balanced()
        };
        let gain = Weight(0.5);
        assert!(gregarious.steering_bias(gain).cohesion > 1.4);
        assert!(hermit.steering_bias(gain).cohesion < 0.6);
    }

    #[test]
    fn every_bias_stays_inside_its_bounds() {
        let extreme = Temperament {
            openness: Weight(1.0),
            conscientiousness: Weight(1.0),
            extraversion: Weight(1.0),
            agreeableness: Weight(0.0),
            neuroticism: Weight(1.0),
            bravery: Weight(0.0),
            sociability: Weight(1.0),
            industriousness: Weight(1.0),
        };
        let bias = extreme.steering_bias(Weight::MAX);
        for axis in bias.to_array() {
            assert!((WeightBias::MIN..=WeightBias::MAX).contains(&axis));
        }
        // A NaN multiplier reads as neutral rather than extreme.
        let broken = WeightBias::uniform(f32::NAN).clamped();
        assert!(broken.is_neutral());
    }

    #[test]
    fn needs_only_point_one_way() {
        let full = Needs::satisfied();
        assert!(full.steering_bias(Weight(0.5)).is_neutral());
        let lonely = Needs {
            company: 0.0,
            ..Needs::satisfied()
        };
        let hungry = Needs {
            satiety: 0.0,
            ..Needs::satisfied()
        };
        let frightened = Needs {
            safety: 0.0,
            ..Needs::satisfied()
        };
        let influence = Weight(0.5);
        assert!(lonely.steering_bias(influence).cohesion > 1.4);
        assert!(hungry.steering_bias(influence).seek > 1.4);
        assert!(frightened.steering_bias(influence).separation > 1.4);
        // Hunger presses on, fatigue drags: the two cancel when both are unmet.
        let exhausted = Needs {
            satiety: 0.0,
            energy: 0.0,
            ..Needs::satisfied()
        };
        assert!((exhausted.steering_bias(influence).seek - 1.0).abs() < 1e-6);
    }

    #[test]
    fn pressure_reports_the_worst_channel() {
        assert_eq!(Needs::satisfied().pressure(), Weight::MIN);
        assert_eq!(Needs::depleted().pressure(), Weight::MAX);
        let one_bad = Needs {
            safety: 0.25,
            ..Needs::satisfied()
        };
        assert!((one_bad.pressure().get() - 0.75).abs() < 1e-6);
    }

    #[test]
    fn the_drive_composes_temperament_and_needs() {
        let mut rng = stream(7);
        let who = Temperament::sample(&mut rng);
        let needs = Needs {
            company: 0.1,
            ..Needs::satisfied()
        };
        let drive = TemperamentDrive::default();
        assert_eq!(
            drive.bias(&who, &needs),
            drive
                .temperament_bias(&who)
                .combined(drive.needs_bias(&needs))
        );
        assert_eq!(
            drive.bias(&who, &needs).alignment,
            drive.temperament_bias(&who).alignment
        );
        // An inert drive is indistinguishable from no personality at all.
        assert!(TemperamentDrive::inert().bias(&who, &needs).is_neutral());
    }

    #[test]
    fn a_bias_multiplies_the_configured_weights() {
        let bias = WeightBias {
            separation: 2.0,
            alignment: 0.5,
            cohesion: 1.0,
            seek: 1.5,
            arrive: 1.0,
        };
        assert_eq!(
            bias.apply([0.9, 0.4, 0.3, 0.7, 0.6]),
            [1.8, 0.2, 0.3, 1.05, 0.6]
        );
        assert_eq!(WeightBias::from_array(bias.to_array()), bias);
        // Composing twice is a product, not a sum.
        assert_eq!(bias.combined(bias).separation, WeightBias::MAX);
    }

    #[test]
    fn an_archetype_survives_the_blend_into_an_individual() {
        let mut guard_rng = stream(11);
        let mut other_guard_rng = stream(11);
        let guard = Temperament::generated(Archetype::Guard, &mut guard_rng);
        let wanderer = Temperament::generated(Archetype::Wanderer, &mut stream(12));
        // A guard is braver and steadier than nobody-in-particular: the preset carries.
        assert!(guard.bravery.get() > wanderer.bravery.get() * 0.8);
        assert!(guard.neuroticism < wanderer.neuroticism);
        // Two guards from different people are not the same person.
        let same_person = Temperament::generated(Archetype::Guard, &mut other_guard_rng);
        assert_eq!(
            guard, same_person,
            "the same seed must generate the same guard"
        );
        let different = Temperament::generated(Archetype::Guard, &mut stream(99));
        assert_ne!(guard, different);
        // The individual is still two thirds of the noise rather than a clone of the preset.
        assert_ne!(different.bravery, guard.bravery);
    }

    #[test]
    fn a_burst_of_decay_matches_single_ticks() {
        let who = Temperament {
            sociability: Weight(0.9),
            extraversion: Weight(0.8),
            neuroticism: Weight(0.7),
            ..Temperament::balanced()
        };
        let mut burst = Needs::satisfied().decayed(&who, 8);
        let mut single = Needs::satisfied();
        for _ in 0..8 {
            single = single.decayed(&who, 1);
        }
        assert!((burst.energy - single.energy).abs() < 1e-6);
        assert!((burst.company - single.company).abs() < 1e-6);
        assert!((burst.safety - single.safety).abs() < 1e-6);
        // A gregarious NPC loses company faster than a stoic one; that is the whole point.
        let stoic = Temperament {
            sociability: Weight(0.0),
            extraversion: Weight(0.0),
            ..who
        };
        burst = Needs::satisfied().decayed(&who, 600);
        let calm = Needs::satisfied().decayed(&stoic, 600);
        assert!(burst.company < calm.company);
    }
}
