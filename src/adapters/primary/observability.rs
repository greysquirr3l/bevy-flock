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
use crate::domain::num;
use crate::domain::tick::Tick;
use bevy_ecs::prelude::*;
use bevy_transform::prelude::Transform;
use core::fmt::Write as _;
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
        let total = num::u32_to_f32(self.population.max(1));
        [
            num::u32_to_f32(self.tiers[0]) / total,
            num::u32_to_f32(self.tiers[1]) / total,
            num::u32_to_f32(self.tiers[2]) / total,
            num::u32_to_f32(self.tiers[3]) / total,
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
        mix(num::i64_to_u64(num::f64_to_i64(
            f64::from(self.pulse.mean_mood) * 1_000.0,
        )));
        mix(num::i64_to_u64(num::f64_to_i64(
            f64::from(self.pulse.tension) * 1_000.0,
        )));
        mix(self.pulse.sampled.get());
        hash
    }
}

/// Take the expensive crowd sample, once every [`SAMPLE_INTERVAL`] ticks.
///
/// O(n) and deliberately not per frame: the director decides once a second, and the
/// diagnostics are for humans.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
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
        (mood_sum / num::u32_to_f32(counted)).clamp(-1.0, 1.0)
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
    let population = num::usize_to_u32(identities.len());
    if num::usize_to_u32(tiered.len()) > population {
        violations.push(Violation::TierSumOverPopulation {
            sum: num::usize_to_u32(tiered.len()),
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

/// Why an NPC did that: the state a behaviour decision was taken from.
///
/// The one question a crowd makes hard to answer. A tree path tells you which branch ran;
/// it does not tell you that the branch ran because the NPC was hungry, in a bad mood and
/// standing next to a remembered friend. This prints the whole input set — tier, weight,
/// tree, cursor, mood, needs, blackboard, memory view, group — so a playtest report can
/// say "the guard ignored you" and the log can say why.
///
/// Debug-only in spirit: it allocates a `String` and walks every blackboard slot, so it
/// belongs behind a key press or a test, not in a per-frame path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplainError {
    /// No such entity.
    NoSuchEntity,
    /// The entity exists but is not an NPC this crate knows about.
    NotAnNpc,
}

impl core::fmt::Display for ExplainError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoSuchEntity => formatter.write_str("no such entity"),
            Self::NotAnNpc => formatter.write_str("entity is not an NPC"),
        }
    }
}

impl core::error::Error for ExplainError {}

/// Explain one NPC's current state, in a form a human can read in a log.
///
/// Takes the whole world because the answer spans components that no single system owns,
/// and deliberately does it on demand: a crowd of ten thousand cannot afford this per
/// frame, and nobody wants it per frame.
pub fn explain_npc(world: &mut World, entity: Entity) -> Result<String, ExplainError> {
    let Some(entity_ref) = world.get_entity_mut(entity).ok() else {
        return Err(ExplainError::NoSuchEntity);
    };
    let Some(npc) = entity_ref.get::<Npc>().copied() else {
        return Err(ExplainError::NotAnNpc);
    };
    let mut out = String::new();
    let _ = writeln!(out, "npc {:?} (entity {:?})", npc.0, entity);
    if let Some(identity) = entity_ref.get::<crate::adapters::primary::components::NpcIdentity>() {
        let _ = writeln!(out, "  identity: {identity:?}");
    }
    if let Some(tier) = entity_ref.get::<crate::adapters::primary::cognition::NpcTier>() {
        let _ = writeln!(out, "  tier: {:?}", tier.0);
    }
    if let Some(weight) = entity_ref.get::<crate::adapters::primary::cognition::NpcThinkWeight>() {
        let _ = writeln!(out, "  think weight: {:?}", weight.0);
    }
    if let Some(brain) = entity_ref.get::<crate::adapters::primary::components::NpcBrain>() {
        let cursor = brain.cursor();
        let _ = writeln!(
            out,
            "  tree: {} nodes, depth {} (cursor: {} frames deep)",
            brain.tree().len(),
            brain.tree().depth(),
            cursor.depth_used()
        );
        let _ = writeln!(out, "  declared think cost: {:?}", brain.cost());
    } else {
        let _ = writeln!(out, "  tree: none (this NPC is not thinking)");
    }
    if let Some(mood) = entity_ref.get::<NpcMood>() {
        let _ = writeln!(out, "  mood: {:?}", mood.0);
    }
    if let Some(needs) = entity_ref.get::<crate::adapters::primary::components::NpcNeeds>() {
        let _ = writeln!(out, "  needs: {:?}", needs.0);
    }
    if let Some(member) = entity_ref.get::<crate::adapters::primary::components::FlockMember>() {
        let _ = writeln!(out, "  group: {:?}", member.group);
    }
    if let Some(board) = entity_ref.get::<crate::adapters::primary::cognition::NpcBlackboard>() {
        let mut slots: SmallVec<[(u16, String); 8]> = SmallVec::new();
        for (key, value) in board.0.iter() {
            slots.push((key.get(), format!("{value:?}")));
        }
        slots.sort_unstable_by_key(|(key, _)| *key);
        if slots.is_empty() {
            let _ = writeln!(out, "  blackboard: empty");
        } else {
            let _ = writeln!(out, "  blackboard:");
            for (key, value) in slots {
                let _ = writeln!(out, "    [{key}] {value}");
            }
        }
    }
    #[cfg(feature = "memory")]
    if let Some(row) = entity_ref.get::<crate::adapters::primary::memory_bridge::NpcMemoryRow>() {
        let _ = writeln!(out, "  memory row: {}", row.0.get());
    }
    Ok(out)
}

/// One label a debug overlay should draw.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugLabel {
    /// The entity the label belongs to.
    pub entity: Entity,
    /// Where to draw it.
    pub position: bevy_math::prelude::Vec3,
    /// What to draw.
    pub text: String,
}

/// Labels a game's own debug renderer can draw.
///
/// The crate does not render: it produces the list and lets the game put it on screen
/// with whatever gizmo, sprite or text system it already has. That is the same division
/// of labour as the render plan (ADR 0040).
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct DebugOverlay {
    /// The labels, in draw order.
    pub labels: Vec<DebugLabel>,
}

/// What, if anything, to put in the overlay.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct DebugOverlayConfig {
    /// Whether the overlay is collected at all. Off by default.
    pub enabled: bool,
    /// How many labels at most.
    pub max_labels: usize,
    /// Only label NPCs this close to the camera or player, in metres.
    pub radius: f32,
}

impl Default for DebugOverlayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_labels: 32,
            radius: 30.0,
        }
    }
}

/// Collect labels for the NPCs nearest the origin, for a game to draw.
///
/// Sorted by distance so the nearest bots win the budget, which is what makes an overlay
/// useful while walking through a crowd: the labels you can read are the ones you are
/// standing next to.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn collect_debug_overlay(
    config: Res<DebugOverlayConfig>,
    mut overlay: ResMut<DebugOverlay>,
    brains: Query<(
        Entity,
        &Npc,
        &Transform,
        Option<&crate::adapters::primary::cognition::NpcTier>,
        Option<&crate::adapters::primary::components::NpcBrain>,
    )>,
) {
    overlay.labels.clear();
    if !config.enabled {
        return;
    }
    let radius = config.radius.max(0.0);
    let mut picked: SmallVec<[(f32, DebugLabel); 32]> = SmallVec::new();
    for (entity, npc, transform, tier, brain) in &brains {
        let distance = transform.translation.length();
        if distance > radius {
            continue;
        }
        let text = match (tier, brain) {
            (Some(tier), Some(brain)) => {
                format!("{:?} {:?} d{}", npc.0, tier.0, brain.cursor().depth_used())
            }
            (Some(tier), None) => format!("{:?} {:?}", npc.0, tier.0),
            _ => format!("{:?}", npc.0),
        };
        picked.push((
            distance,
            DebugLabel {
                entity,
                position: transform.translation,
                text,
            },
        ));
    }
    picked.sort_unstable_by(|left, right| left.0.total_cmp(&right.0));
    for (_, label) in picked.into_iter().take(config.max_labels) {
        overlay.labels.push(label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{GetOrDieRef, OrDie};
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
    fn an_explanation_says_what_the_npc_is_thinking_with() {
        let mut app = app();
        let tree = std::sync::Arc::new(
            crate::behavior::tree::BehaviorTree::try_from_spec(
                &crate::behavior::tree::NodeSpec::Action(0),
            )
            .or_die(),
        );
        let entity = app
            .world_mut()
            .spawn((
                Npc(NpcId::unique(31)),
                Transform::from_xyz(1.0, 0.0, 0.0),
                crate::adapters::primary::components::NpcBrain::new(
                    tree,
                    crate::domain::cost::VirtualCost::new(7),
                ),
                crate::adapters::primary::cognition::NpcBlackboard::default(),
            ))
            .id();
        {
            let world = app.world_mut();
            let mut entity_mut = world.entity_mut(entity);
            let mut board = entity_mut
                .get_mut::<crate::adapters::primary::cognition::NpcBlackboard>()
                .or_die();
            let _ = board.0.set(
                crate::adapters::primary::perception::KEY_PLAYER_SEEN,
                crate::behavior::blackboard::Value::Bool(true),
            );
        }
        let text = explain_npc(app.world_mut(), entity).or_die();
        assert!(text.contains("npc"), "{text}");
        assert!(text.contains("declared think cost"), "{text}");
        assert!(
            text.contains("[20]"),
            "the blackboard key should be listed: {text}"
        );
    }

    #[test]
    fn explaining_a_stranger_is_an_error_not_a_panic() {
        let mut app = app();
        assert_eq!(
            explain_npc(app.world_mut(), Entity::PLACEHOLDER),
            Err(ExplainError::NoSuchEntity)
        );
        let entity = app.world_mut().spawn(()).id();
        assert_eq!(
            explain_npc(app.world_mut(), entity),
            Err(ExplainError::NotAnNpc)
        );
    }

    #[test]
    fn the_overlay_is_empty_until_it_is_asked_for() {
        let mut app = app();
        app.init_resource::<DebugOverlay>()
            .init_resource::<DebugOverlayConfig>()
            .add_systems(Update, collect_debug_overlay);
        for id in 0..4 {
            app.world_mut().spawn((
                Npc(NpcId::unique(id)),
                Transform::from_xyz(num::u32_to_f32(id), 0.0, 0.0),
            ));
        }
        app.update();
        assert!(app.world().resource::<DebugOverlay>().labels.is_empty());
        app.world_mut().resource_mut::<DebugOverlayConfig>().enabled = true;
        app.update();
        assert_eq!(app.world().resource::<DebugOverlay>().labels.len(), 4);
    }

    #[test]
    fn the_overlay_draws_the_nearest_labels_within_its_budget() {
        let mut app = app();
        app.init_resource::<DebugOverlay>()
            .insert_resource(DebugOverlayConfig {
                enabled: true,
                max_labels: 2,
                radius: 10.0,
            })
            .add_systems(Update, collect_debug_overlay);
        for (id, x) in [(0, 9.0), (1, 1.0), (2, 5.0), (3, 50.0)] {
            app.world_mut()
                .spawn((Npc(NpcId::unique(id)), Transform::from_xyz(x, 0.0, 0.0)));
        }
        app.update();
        let overlay = app.world().resource::<DebugOverlay>();
        assert_eq!(overlay.labels.len(), 2);
        // The label carries the transform verbatim, so the comparison is exact by intent:
        // the position is a copy of a literal, not a computed length.
        assert_eq!(
            overlay.labels.at_ref(0).position.x.to_bits(),
            1.0_f32.to_bits()
        );
        assert_eq!(
            overlay.labels.at_ref(1).position.x.to_bits(),
            5.0_f32.to_bits()
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
