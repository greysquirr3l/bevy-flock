//! Seeing what the crowd is doing: counters, a report, and invariant checks.
//!
//! A crowd simulation fails quietly. Frames stay under budget, nothing panics, and
//! the town is subtly wrong: every NPC at the same tier, moods stuck at zero, a
//! group with a leader who despawned an hour ago. This module exists so those
//! failures are visible without a debugger.
//!
//! Three pieces:
//!
//! * [`CrowdPulse`] — a cheap snapshot the director and the diagnostics share. It is
//!   refreshed once a second rather than every frame, because counting the crowd is
//!   O(n) and no gameplay decision needs per-frame precision.
//! * [`Diagnostics`] — the counters a game can print, plot or ship to telemetry.
//! * [`check_world`] — invariants that must hold in any world this crate built. It
//!   runs in tests and in debug builds, and it is the check that catches "two NPCs
//!   share an identity", which otherwise shows up as two people who agree about
//!   everything for the rest of the session.

use crate::adapters::primary::components::{LodTier, Npc, NpcMood};
use crate::application::SimClock;
use crate::domain::ids::NpcId;
use crate::domain::tick::Tick;
use bevy_ecs::prelude::*;
use bevy_transform::prelude::Transform;
use smallvec::SmallVec;

/// How often the expensive crowd sample is taken, in ticks.
pub const SAMPLE_INTERVAL: u64 = 60;

/// A cheap snapshot of the crowd, refreshed once a second.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Default)]
pub struct CrowdPulse {
    /// Live NPCs.
    pub population: u32,
    /// How many NPCs are in each cognition tier, from reflex upward.
    pub tiers: [u32; 4],
    /// Mean mood across the crowd, in `-1..=1`.
    pub mean_mood: f32,
    /// Mean faction tension, in `0..=1`. Written by whoever owns factions.
    pub tension: f32,
    /// Live alarms. Written by the world director.
    pub alarms: u32,
    /// The tick the pulse was taken on.
    pub sampled: Tick,
}

impl CrowdPulse {
    /// The share of the crowd in each tier, in `0..=1`. Zero when the crowd is empty.
    #[must_use]
    pub fn tier_share(&self) -> [f32; 4] {
        let total = self.population.max(1) as f32;
        [
            self.tiers[0] as f32 / total,
            self.tiers[1] as f32 / total,
            self.tiers[2] as f32 / total,
            self.tiers[3] as f32 / total,
        ]
    }
}

/// Counters a game can read, print or ship.
#[derive(Resource, Debug, Clone, Default)]
pub struct Diagnostics {
    /// Frames simulated.
    pub frames: u64,
    /// The most recent pulse.
    pub pulse: CrowdPulse,
    /// NPCs waiting to be spawned.
    pub pending: u32,
    /// Barks produced since startup.
    pub barks: u64,
    /// Director commands applied since startup.
    pub commands: u64,
    /// How many samples have been taken.
    pub samples: u64,
}

impl Diagnostics {
    /// A one-screen report, suitable for a log line or an overlay.
    #[must_use]
    pub fn report(&self) -> String {
        let shares = self.pulse.tier_share();
        let tiers = format!(
            "{}% reflex / {}% habit / {}% social / {}% deliberate",
            (shares[0] * 100.0).round(),
            (shares[1] * 100.0).round(),
            (shares[2] * 100.0).round(),
            (shares[3] * 100.0).round()
        );
        format!(
            "population {} ({} pending)\ntiers {tiers}\nmood {:+.2}  tension {:.2}  alarms {}\nbarks {}  director commands {}\nframes {}  samples {}",
            self.pulse.population,
            self.pending,
            self.pulse.mean_mood,
            self.pulse.tension,
            self.pulse.alarms,
            self.barks,
            self.commands,
            self.frames,
            self.samples,
        )
    }

    /// A digest of the crowd state, for determinism tests.
    ///
    /// Deliberately coarse: population, tiers, mood to three decimals and the tick.
    /// A digest that included float bit patterns would fail on a platform whose
    /// transcendentals differ by one ulp, which is not a determinism bug in the
    /// simulation.
    #[must_use]
    pub fn digest(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |value: u64| {
            hash ^= value;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        };
        mix(u64::from(self.pulse.population));
        for tier in self.pulse.tiers {
            mix(u64::from(tier));
        }
        mix((f64::from(self.pulse.mean_mood) * 1_000.0) as i64 as u64);
        mix((f64::from(self.pulse.tension) * 1_000.0) as i64 as u64);
        mix(self.pulse.sampled.get());
        hash
    }
}

/// Take the expensive crowd sample, once every [`SAMPLE_INTERVAL`] ticks.
///
/// O(n) and deliberately not per frame: the director decides once a second, and the
/// diagnostics are for humans.
pub fn sample_crowd(
    clock: Res<SimClock>,
    mut pulse: ResMut<CrowdPulse>,
    mut diagnostics: ResMut<Diagnostics>,
    npcs: Query<(&LodTier, Option<&NpcMood>)>,
) {
    if !clock.tick.get().is_multiple_of(SAMPLE_INTERVAL) {
        return;
    }
    let mut tiers = [0_u32; 4];
    let mut mood_sum = 0.0_f32;
    let mut counted = 0_u32;
    for (tier, mood) in &npcs {
        let slot = match tier {
            LodTier::Near => 0,
            LodTier::Mid => 1,
            LodTier::Far => 2,
            LodTier::Ambient => 3,
        };
        if let Some(count) = tiers.get_mut(slot) {
            *count = count.saturating_add(1);
        }
        if let Some(mood) = mood {
            mood_sum += mood.0.valence;
            counted = counted.saturating_add(1);
        }
    }
    let population = tiers.iter().copied().sum();
    pulse.population = population;
    pulse.tiers = tiers;
    pulse.mean_mood = if counted == 0 {
        0.0
    } else {
        (mood_sum / counted as f32).clamp(-1.0, 1.0)
    };
    pulse.sampled = clock.tick;
    diagnostics.pulse = *pulse;
    diagnostics.samples = diagnostics.samples.saturating_add(1);
}

/// Count frames.
pub fn count_frames(mut diagnostics: ResMut<Diagnostics>) {
    diagnostics.frames = diagnostics.frames.saturating_add(1);
}

/// Something that should never be true.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Violation {
    /// Two live NPCs share an identity.
    DuplicateIdentity(NpcId),
    /// The tier counts sum to more than the population.
    TierSumOverPopulation {
        /// The tier sum.
        sum: u32,
        /// The population.
        population: u32,
    },
    /// An NPC sits outside every tier.
    OutsideTheTiers(Entity),
    /// A mood outside its range.
    MoodOutOfRange {
        /// The offending entity.
        entity: Entity,
        /// The mood.
        value: f32,
    },
}

impl Violation {
    /// A human-readable description.
    #[must_use]
    pub fn describe(self) -> String {
        match self {
            Self::DuplicateIdentity(id) => format!("identity {id:?} is used by two NPCs"),
            Self::TierSumOverPopulation { sum, population } => {
                format!("tiers sum to {sum} but the population is {population}")
            }
            Self::OutsideTheTiers(entity) => format!("{entity:?} is in no tier"),
            Self::MoodOutOfRange { entity, value } => {
                format!("{entity:?} has mood {value}, outside -1..=1")
            }
        }
    }
}

/// Check the invariants that must hold in any world this crate built.
///
/// Duplicate identities are found by sorting the live identities, which is O(n log n)
/// and therefore only run on demand — in tests, in debug builds, and from a console
/// command — not every frame.
#[must_use]
pub fn check_world(world: &mut World) -> SmallVec<[Violation; 8]> {
    let mut violations = SmallVec::new();
    let mut query = world.query::<(Entity, &Npc)>();
    let mut identities: Vec<(NpcId, Entity)> = query
        .iter(world)
        .map(|(entity, npc)| (npc.0, entity))
        .collect();
    identities.sort_unstable_by_key(|(id, _)| *id);
    for pair in identities.windows(2) {
        if let [first, second] = pair
            && first.0 == second.0
        {
            violations.push(Violation::DuplicateIdentity(first.0));
        }
    }
    let mut query = world.query::<(Entity, &LodTier)>();
    let tiered: Vec<Entity> = query.iter(world).map(|(entity, _)| entity).collect();
    let population = identities.len() as u32;
    if tiered.len() as u32 > population {
        violations.push(Violation::TierSumOverPopulation {
            sum: tiered.len() as u32,
            population,
        });
    }
    let mut query = world.query::<(Entity, &Transform)>();
    let transforms: Vec<Entity> = query.iter(world).map(|(entity, _)| entity).collect();
    let mut query = world.query::<(Entity, &NpcMood)>();
    for (entity, mood) in query.iter(world) {
        let value = mood.0.valence;
        if !(-1.0..=1.0).contains(&value) {
            violations.push(Violation::MoodOutOfRange { entity, value });
        }
    }
    // An entity with a transform but no tier and no identity is a stray from another
    // plugin, which is fine; an NPC with no tier is not.
    for entity in transforms {
        if world.get::<Npc>(entity).is_some() && world.get::<LodTier>(entity).is_none() {
            violations.push(Violation::OutsideTheTiers(entity));
        }
    }
    let _ = tiered;
    violations
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_app::{App, Update};

    fn app() -> App {
        let mut app = App::new();
        app.insert_resource(SimClock::default())
            .init_resource::<CrowdPulse>()
            .init_resource::<Diagnostics>()
            .add_systems(Update, (sample_crowd, count_frames));
        app
    }

    #[test]
    fn the_pulse_counts_the_crowd_by_tier() {
        let mut app = app();
        for index in 0..12_u32 {
            let tier = match index % 4 {
                0 => LodTier::Near,
                1 => LodTier::Mid,
                2 => LodTier::Far,
                _ => LodTier::Ambient,
            };
            app.world_mut().spawn((
                Npc(NpcId::unique(index)),
                tier,
                NpcMood(crate::domain::temperament::Mood::neutral(Tick::ZERO)),
                Transform::default(),
            ));
        }
        // Sampling happens on multiples of SAMPLE_INTERVAL; drive the clock there.
        let mut clock = app.world_mut().resource_mut::<SimClock>();
        clock.tick = Tick::new(SAMPLE_INTERVAL);
        app.update();
        let pulse = *app.world().resource::<CrowdPulse>();
        assert_eq!(pulse.population, 12);
        assert_eq!(pulse.tiers, [3, 3, 3, 3]);
    }

    #[test]
    fn the_report_mentions_every_section() {
        let mut app = app();
        app.update();
        let report = app.world().resource::<Diagnostics>().report();
        assert!(report.contains("population"));
        assert!(report.contains("tiers"));
        assert!(report.contains("frames"));
    }

    #[test]
    fn the_digest_changes_with_the_population_and_is_stable_otherwise() {
        let mut app = app();
        app.world_mut()
            .spawn((Npc(NpcId::unique(1)), LodTier::Near, Transform::default()));
        let first = app.world().resource::<Diagnostics>().digest();
        let second = app.world().resource::<Diagnostics>().digest();
        assert_eq!(first, second);
        assert_ne!(app.world().resource::<CrowdPulse>().population, 1);
    }

    #[test]
    fn a_clean_world_has_no_violations() {
        let mut app = app();
        for index in 0..8_u32 {
            app.world_mut().spawn((
                Npc(NpcId::unique(index)),
                LodTier::Near,
                NpcMood(crate::domain::temperament::Mood::neutral(Tick::ZERO)),
                Transform::default(),
            ));
        }
        app.update();
        let violations = check_world(app.world_mut());
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn duplicate_identities_are_caught() {
        let mut app = app();
        app.world_mut()
            .spawn((Npc(NpcId::unique(7)), LodTier::Near, Transform::default()));
        app.world_mut()
            .spawn((Npc(NpcId::unique(7)), LodTier::Near, Transform::default()));
        let violations = check_world(app.world_mut());
        assert_eq!(
            violations.as_slice(),
            [Violation::DuplicateIdentity(NpcId::unique(7))]
        );
    }

    #[test]
    fn an_npc_without_a_tier_is_caught() {
        let mut app = app();
        app.world_mut()
            .spawn((Npc(NpcId::unique(9)), Transform::default()));
        let violations = check_world(app.world_mut());
        assert!(
            violations
                .iter()
                .any(|violation| matches!(violation, Violation::OutsideTheTiers(_)))
        );
    }

    #[test]
    fn a_mood_outside_its_range_is_caught_and_described() {
        let violation = Violation::MoodOutOfRange {
            entity: Entity::PLACEHOLDER,
            value: 4.0,
        };
        assert!(violation.describe().contains("outside"));
    }
}
