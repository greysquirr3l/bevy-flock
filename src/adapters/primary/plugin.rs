//! The plugin, its configuration, and budgeted spawning.
//!
//! One entry point: [`FlockPlugin`]. It inserts the configuration, the
//! simulation clock and the spawn queue, adds the boids systems, and orders the
//! frame into named sets that a game can slot its own systems into.

use std::collections::VecDeque;

use bevy_app::{App, Plugin, Update};
use bevy_ecs::prelude::*;
use bevy_math::Vec3;
use bevy_transform::prelude::Transform;

use crate::adapters::primary::cognition::{
    ThinkSchedule, TierConfig, WeightConfig, assign_tiers, plan_thinks, tick_chosen,
};
use crate::adapters::primary::components::{
    LodTier, Npc, NpcBundle, NpcDesiredVelocity, NpcIdentity, NpcTemperament, NpcVelocity,
};
use crate::application::{SimClock, advance_clock};
use crate::boids::{BoidsConfig, BoidsPlugin};
use crate::domain::ids::{NpcId, SimSeed};
use crate::domain::population::{PopulationMode, PopulationSize, SlotCount};
use crate::domain::rng::RngStream;
use crate::domain::temperament::{Archetype, Temperament};
use crate::domain::tick::TicksPerSecond;

/// The ordered stages of a flock frame.
///
/// Systems outside this crate can join any set: `my_scoring.in_set(FlockSet::Cognition)`
/// runs in the right place without knowing the plugin's internal ordering.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlockSet {
    /// Entities are created from the spawn queue.
    Spawn,
    /// The spatial index is rebuilt and perception is refreshed.
    Perception,
    /// Brains decide what to do.
    Cognition,
    /// Desired velocities are computed.
    Steering,
    /// Velocities are integrated into positions.
    Integration,
    /// The logical clock advances.
    AdvanceTick,
}

/// A ready-made crowd configuration, tuned to feel like something.
///
/// Presets exist so a game can start from taste rather than from numbers. Each
/// one is a complete, valid configuration; tweak it afterwards with
/// [`FlockConfig::with_population`] and friends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrowdPreset {
    /// A quiet village: a few dozen people, slow, plenty of personal space.
    QuietVillage,
    /// A market at midday: a few hundred people milling with destinations.
    MarketplaceBustle,
    /// A city at large: two thousand people, dense, destination-driven.
    CityAmbient,
    /// A charging horde: tight spacing, high speed, aggression in the weights.
    BattleHorde,
}

impl CrowdPreset {
    /// Every preset, in increasing order of intensity.
    pub const ALL: [Self; 4] = [
        Self::QuietVillage,
        Self::MarketplaceBustle,
        Self::CityAmbient,
        Self::BattleHorde,
    ];

    /// How many NPCs this preset spawns.
    #[must_use]
    pub const fn population(self) -> u32 {
        match self {
            Self::QuietVillage => 48,
            Self::MarketplaceBustle => 256,
            Self::CityAmbient => 2_000,
            Self::BattleHorde => 1_500,
        }
    }

    /// A lowercase name for debug output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QuietVillage => "quiet-village",
            Self::MarketplaceBustle => "marketplace-bustle",
            Self::CityAmbient => "city-ambient",
            Self::BattleHorde => "battle-horde",
        }
    }

    /// The full configuration for this preset.
    #[must_use]
    pub fn config(self) -> FlockConfig {
        let boids = match self {
            Self::QuietVillage | Self::MarketplaceBustle => BoidsConfig::default(),
            Self::CityAmbient => BoidsConfig::default().with_max_acceleration(5.0),
            // A horde that cannot accelerate quickly reads as a stroll, not a
            // charge; the limit is the whole difference.
            Self::BattleHorde => BoidsConfig::default().with_max_acceleration(9.0),
        };
        FlockConfig {
            population: PopulationSize::clamped(self.population() as usize),
            mode: PopulationMode::Unique,
            seed: SimSeed::default(),
            ticks_per_second: TicksPerSecond::SIXTY,
            spawn_budget: 4_096,
            boids,
        }
    }
}

/// Everything the plugin needs to know.
///
/// Every field is either a validated newtype or a validated config, so a
/// `FlockConfig` in hand is already a valid configuration.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct FlockConfig {
    /// How many NPCs to spawn at startup.
    pub population: PopulationSize,
    /// Whether identities are unique or drawn from a recycled pool.
    pub mode: PopulationMode,
    /// The world seed.
    pub seed: SimSeed,
    /// Simulation rate.
    pub ticks_per_second: TicksPerSecond,
    /// The most entities spawned in a single frame.
    pub spawn_budget: u32,
    /// Steering, grid and acceleration settings.
    pub boids: BoidsConfig,
}

impl FlockConfig {
    /// The configuration for a preset.
    #[must_use]
    pub fn preset(preset: CrowdPreset) -> Self {
        preset.config()
    }

    /// Change the population.
    #[must_use]
    pub const fn with_population(mut self, population: PopulationSize) -> Self {
        self.population = population;
        self
    }

    /// Change the spawn mode.
    #[must_use]
    pub const fn with_mode(mut self, mode: PopulationMode) -> Self {
        self.mode = mode;
        self
    }

    /// Change the world seed.
    #[must_use]
    pub const fn with_seed(mut self, seed: SimSeed) -> Self {
        self.seed = seed;
        self
    }

    /// Change the per-frame spawn budget.
    #[must_use]
    pub const fn with_spawn_budget(mut self, budget: u32) -> Self {
        self.spawn_budget = budget;
        self
    }
}

impl Default for FlockConfig {
    fn default() -> Self {
        Self::preset(CrowdPreset::MarketplaceBustle)
    }
}

/// A request to spawn NPCs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlockRequest {
    /// How many.
    pub count: PopulationSize,
    /// How identities are allocated.
    pub mode: PopulationMode,
    /// The seed that determines who these people are.
    pub seed: SimSeed,
}

impl FlockRequest {
    /// A request for `count` unique NPCs with the default seed.
    #[must_use]
    pub const fn new(count: PopulationSize) -> Self {
        Self {
            count,
            mode: PopulationMode::Unique,
            seed: SimSeed::new(0),
        }
    }

    /// A request from a plugin configuration.
    #[must_use]
    pub const fn from_config(config: &FlockConfig) -> Self {
        Self {
            count: config.population,
            mode: config.mode,
            seed: config.seed,
        }
    }

    /// Set the seed.
    #[must_use]
    pub const fn with_seed(mut self, seed: SimSeed) -> Self {
        self.seed = seed;
        self
    }

    /// Set the spawn mode.
    #[must_use]
    pub const fn with_mode(mut self, mode: PopulationMode) -> Self {
        self.mode = mode;
        self
    }
}

/// One batch of identities waiting to be spawned.
#[derive(Debug, Clone, Copy)]
struct SpawnBatch {
    next: u32,
    end: u32,
    mode: PopulationMode,
    seed: SimSeed,
}

/// Queued spawn work, drained against a per-frame budget.
///
/// Batches rather than materialised identities: a request for a million NPCs
/// costs one queue entry, and the identities are computed as they are handed out.
#[derive(Resource, Debug, Default)]
pub struct SpawnQueue {
    batches: VecDeque<SpawnBatch>,
}

impl SpawnQueue {
    /// Queue a request.
    pub fn push(&mut self, request: FlockRequest) {
        let count = request.count.as_u32();
        if count == 0 {
            return;
        }
        self.batches.push_back(SpawnBatch {
            next: 0,
            end: count,
            mode: request.mode,
            seed: request.seed,
        });
    }

    /// Take the next identity, or `None` when the queue is drained.
    pub fn pop(&mut self) -> Option<(NpcId, SimSeed)> {
        let batch = self.batches.front_mut()?;
        let id = match batch.mode {
            PopulationMode::Unique => NpcId::unique(batch.next),
            PopulationMode::Recycled(slots) => NpcId::recycled(batch.next % slots.get().max(1)),
        };
        let seed = batch.seed;
        batch.next += 1;
        if batch.next >= batch.end {
            self.batches.pop_front();
        }
        Some((id, seed))
    }

    /// How many identities are still queued.
    #[must_use]
    pub fn pending(&self) -> u32 {
        self.batches
            .iter()
            .map(|batch| batch.end.saturating_sub(batch.next))
            .sum()
    }

    /// Whether the queue is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    /// Drop all queued work.
    pub fn clear(&mut self) {
        self.batches.clear();
    }
}

/// Live counts a game can render or assert on.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlockStats {
    /// NPCs currently alive.
    pub live: u32,
    /// Identities still queued for spawn.
    pub pending: u32,
    /// Total NPCs spawned since startup.
    pub spawned_total: u64,
}

impl FlockStats {
    /// NPCs currently alive.
    #[must_use]
    pub const fn population(&self) -> u32 {
        self.live
    }

    /// Whether the spawn queue has drained.
    #[must_use]
    pub const fn spawn_complete(&self) -> bool {
        self.pending == 0
    }
}

/// Adds the crowd simulation to an app.
///
/// ```
/// use bevy_app::App;
/// use bevy_flock::prelude::*;
///
/// let mut app = App::new();
/// app.add_plugins(FlockPlugin::default());
/// app.update();
/// assert!(app.flock_stats().population() > 0);
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct FlockPlugin {
    /// The configuration to install.
    pub config: FlockConfig,
}

impl FlockPlugin {
    /// A plugin with an explicit configuration.
    #[must_use]
    pub const fn new(config: FlockConfig) -> Self {
        Self { config }
    }
}

impl Plugin for FlockPlugin {
    fn build(&self, app: &mut App) {
        // The schedule's capacity is the configured population: an NPC beyond it is a
        // configuration bug the stats report rather than a panic on the frame a crowd
        // walks into view.
        let think_capacity = u32::try_from(self.config.population.get()).unwrap_or(u32::MAX);
        app.insert_resource(ThinkSchedule::new(
            think_capacity,
            ThinkSchedule::DEFAULT_BUDGET,
        ))
        .insert_resource(WeightConfig::default())
        .insert_resource(TierConfig::default())
        .insert_resource(self.config)
            .insert_resource(SimClock::new(self.config.ticks_per_second))
            .init_resource::<SpawnQueue>()
            .init_resource::<FlockStats>()
            .configure_sets(
                Update,
                (
                    FlockSet::Spawn,
                    FlockSet::Perception,
                    FlockSet::Cognition,
                    FlockSet::Steering,
                    FlockSet::Integration,
                    FlockSet::AdvanceTick,
                )
                    .chain(),
            )
            .add_systems(Update, drain_spawn_queue.in_set(FlockSet::Spawn))
            // Weights before planning (distance is a perception fact), planning before
            // ticking (the choice is the plan). Bevy is told the order rather than
            // trusted to infer it from the resource conflicts.
            .add_systems(Update, assign_tiers.in_set(FlockSet::Perception))
            .add_systems(
                Update,
                (plan_thinks, tick_chosen)
                    .chain()
                    .in_set(FlockSet::Cognition),
            )
            .add_systems(Update, advance_clock.in_set(FlockSet::AdvanceTick));

        // A game that added `BoidsPlugin` first (with its own steering config) is
        // respected: adding it twice would panic, and silently overriding the
        // config the game just registered would be worse than either.
        if !app.is_plugin_added::<BoidsPlugin>() {
            app.add_plugins(BoidsPlugin::new(self.config.boids));
        }

        // Queue the configured population. This happens in `build` rather than
        // on the first frame so a game that inspects the queue before the first
        // update sees the work it asked for.
        if let Some(mut queue) = app.world_mut().get_resource_mut::<SpawnQueue>() {
            queue.push(FlockRequest::from_config(&self.config));
        }
    }
}

/// Spawn up to `spawn_budget` NPCs this frame.
///
/// Budgeted rather than all at once: a game asking for 100,000 NPCs gets them
/// over 25 frames at the default budget instead of one 400 ms hitch.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are taken by value by construction"
)]
pub fn drain_spawn_queue(
    config: Res<FlockConfig>,
    mut queue: ResMut<SpawnQueue>,
    mut stats: ResMut<FlockStats>,
    mut commands: Commands,
) {
    let budget = config.spawn_budget;
    let mut spawned = 0_u32;
    while spawned < budget {
        let Some((id, seed)) = queue.pop() else {
            break;
        };
        let mut rng = RngStream::for_npc(seed, id);
        let temperament = Temperament::sample(&mut rng);
        let archetype_index = rng.next_bounded(Archetype::ALL.len() as u32) as usize;
        let archetype = Archetype::ALL
            .get(archetype_index)
            .copied()
            .unwrap_or(Archetype::Wanderer);
        let position = spawn_position(id.index());

        commands.spawn(NpcBundle {
            npc: Npc(id),
            transform: Transform::from_translation(position),
            identity: NpcIdentity {
                archetype,
                appearance_seed: u64::from(id.to_bits()).wrapping_mul(0x9E37_79B9_7F4A_7C15),
            },
            temperament: NpcTemperament(temperament),
            velocity: NpcVelocity(Vec3::ZERO),
            desired: NpcDesiredVelocity(Vec3::ZERO),
            lod: LodTier::Near,
        });
        spawned += 1;
    }

    stats.live += spawned;
    stats.spawned_total += u64::from(spawned);
    stats.pending = queue.pending();
}

/// Where the `index`-th NPC starts.
///
/// A golden-angle spiral: even coverage of a disc with no two NPCs stacked, so a
/// fresh crowd looks placed rather than dumped. Deterministic by construction.
#[must_use]
pub fn spawn_position(index: u32) -> Vec3 {
    const GOLDEN_ANGLE: f32 = 2.399_963_2;
    const SPACING: f32 = 1.6;
    let ring = index as f32;
    let angle = ring * GOLDEN_ANGLE;
    let radius = SPACING * libm::sqrtf(ring);
    Vec3::new(libm::cosf(angle) * radius, 0.0, libm::sinf(angle) * radius)
}

/// Spawning conveniences on [`App`].
pub trait FlockCommands {
    /// Queue NPCs for budgeted spawning.
    fn spawn_flock(&mut self, request: FlockRequest) -> &mut Self;

    /// Current counts.
    fn flock_stats(&self) -> FlockStats;
}

impl FlockCommands for App {
    fn spawn_flock(&mut self, request: FlockRequest) -> &mut Self {
        let world = self.world_mut();
        if world.get_resource::<SpawnQueue>().is_none() {
            world.insert_resource(SpawnQueue::default());
        }
        if let Some(mut queue) = world.get_resource_mut::<SpawnQueue>() {
            queue.push(request);
        }
        self
    }

    fn flock_stats(&self) -> FlockStats {
        self.world()
            .get_resource::<FlockStats>()
            .copied()
            .unwrap_or_default()
    }
}

/// Build a recycled population mode, rejecting an empty pool.
///
/// # Errors
///
/// Returns [`crate::error::PopulationError::NoSlots`] when `slots` is zero.
pub fn recycled_mode(slots: u32) -> Result<PopulationMode, crate::error::PopulationError> {
    Ok(PopulationMode::Recycled(SlotCount::try_new(slots)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with(config: FlockConfig) -> App {
        let mut app = App::new();
        app.add_plugins(FlockPlugin::new(config));
        app
    }

    #[test]
    fn default_plugin_spawns_the_marketplace_preset() {
        let mut app = App::new();
        app.add_plugins(FlockPlugin::default());
        app.update();
        assert_eq!(
            app.flock_stats().population(),
            CrowdPreset::MarketplaceBustle.population()
        );
    }

    #[test]
    fn a_pre_added_boids_plugin_does_not_panic_or_double_up() {
        // Adding a plugin twice panics in Bevy, so the flock plugin has to notice.
        // The steering systems must also run exactly once: two integrations per
        // frame would double every velocity.
        let mut app = App::new();
        app.add_plugins(BoidsPlugin::default());
        app.add_plugins(FlockPlugin::new(FlockConfig {
            population: PopulationSize::try_new(4).expect("4 is valid"),
            spawn_budget: 64,
            ..FlockConfig::default()
        }));
        app.update();
        app.update();

        assert_eq!(app.flock_stats().population(), 4);
        let clock = app.world().resource::<SimClock>();
        assert_eq!(clock.tick.get(), 2, "the clock advances once per frame");
    }

    #[test]
    fn single_npc_population_works() {
        let mut app = app_with(FlockConfig {
            population: PopulationSize::try_new(1).expect("1 is valid"),
            ..FlockConfig::default()
        });
        app.update();
        assert_eq!(app.flock_stats().population(), 1);
    }

    #[test]
    fn zero_population_cannot_be_configured() {
        assert!(PopulationSize::try_new(0).is_err());
    }

    #[test]
    fn spawn_budget_spreads_work_across_frames() {
        let mut app = app_with(FlockConfig {
            population: PopulationSize::try_new(1_000).expect("valid"),
            spawn_budget: 100,
            ..FlockConfig::default()
        });
        app.update();
        assert_eq!(app.flock_stats().population(), 100);
        assert!(!app.flock_stats().spawn_complete());
        for _ in 0..9 {
            app.update();
        }
        assert_eq!(app.flock_stats().population(), 1_000);
        assert!(app.flock_stats().spawn_complete());
    }

    #[test]
    fn identical_seeds_generate_identical_individuals() {
        fn temperaments() -> Vec<[f32; 3]> {
            let mut app = app_with(FlockConfig {
                population: PopulationSize::try_new(20).expect("valid"),
                ..FlockConfig::default()
            });
            app.update();
            let mut collected: Vec<(u32, [f32; 3])> = app
                .world_mut()
                .query::<(&Npc, &NpcTemperament)>()
                .iter(app.world())
                .map(|(npc, temperament)| {
                    (
                        npc.0.index(),
                        [
                            temperament.0.openness.get(),
                            temperament.0.bravery.get(),
                            temperament.0.neuroticism.get(),
                        ],
                    )
                })
                .collect();
            collected.sort_by_key(|(index, _)| *index);
            collected.into_iter().map(|(_, values)| values).collect()
        }
        assert_eq!(temperaments(), temperaments());
    }

    #[test]
    fn recycled_requests_reuse_slot_identities() {
        let mut queue = SpawnQueue::default();
        let slots = SlotCount::try_new(4).expect("4 is non-zero");
        queue.push(
            FlockRequest::new(PopulationSize::try_new(8).expect("valid"))
                .with_mode(PopulationMode::Recycled(slots)),
        );
        let ids: Vec<u32> = (0..8)
            .filter_map(|_| queue.pop().map(|(id, _)| id.index()))
            .collect();
        assert_eq!(ids, vec![0, 1, 2, 3, 0, 1, 2, 3]);
        assert!(queue.is_empty());
    }

    #[test]
    fn pending_counts_shrink_as_the_queue_drains() {
        let mut queue = SpawnQueue::default();
        queue.push(FlockRequest::new(
            PopulationSize::try_new(5).expect("valid"),
        ));
        assert_eq!(queue.pending(), 5);
        queue.pop();
        assert_eq!(queue.pending(), 4);
        queue.clear();
        assert_eq!(queue.pending(), 0);
    }

    #[test]
    fn spawn_positions_are_distinct_and_deterministic() {
        let first = spawn_position(0);
        assert_eq!(first, spawn_position(0));
        assert_ne!(spawn_position(1), spawn_position(2));
        assert!(spawn_position(100).length() > spawn_position(1).length());
    }

    #[test]
    fn recycled_mode_rejects_an_empty_pool() {
        assert!(recycled_mode(0).is_err());
        assert!(recycled_mode(1).is_ok());
    }

    #[test]
    fn clock_advances_once_per_frame() {
        let mut app = app_with(FlockConfig::default());
        app.update();
        app.update();
        assert_eq!(app.world().resource::<SimClock>().tick.get(), 2);
    }
}
