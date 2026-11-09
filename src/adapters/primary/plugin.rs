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
    LodTier, Npc, NpcBundle, NpcDesiredVelocity, NpcIdentity, NpcNeeds, NpcTemperament, NpcVelocity,
};
use crate::application::{SimClock, advance_clock};
use crate::boids::{BoidsConfig, BoidsPlugin};
use crate::domain::ids::{NpcId, SimSeed};
use crate::domain::num;
use crate::domain::population::{PopulationMode, PopulationSize, SlotCount};
use crate::domain::rng::RngStream;
use crate::domain::spawn::Placement;
use crate::domain::temperament::{Archetype, Needs, Temperament};
use crate::domain::tick::TicksPerSecond;
use crate::error::ConfigError;

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
            placement: Placement::default(),
            spawn_budget: 4_096,
            boids,
        }
    }
}

/// Everything the plugin needs to know.
///
/// Every field is either a validated newtype or a validated config, so a
/// `FlockConfig` in hand is already a valid configuration.
#[derive(Resource, Debug, Clone, PartialEq)]
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
    /// Where NPCs stand when they are spawned.
    ///
    /// Defaults to the golden-angle spiral, which is compact at any population. Use
    /// [`FlockConfig::with_placement`] rather than assigning the field: the setter validates the
    /// placement against the simulated volume, which is the difference between a crowd and a
    /// crowd piled into the grid's border cells.
    pub placement: Placement,
}

impl FlockConfig {
    /// The configuration for a preset.
    #[must_use]
    pub fn preset(preset: CrowdPreset) -> Self {
        preset.config()
    }

    /// Place NPCs with this strategy, validated against the simulated volume.
    ///
    /// # Errors
    ///
    /// Returns the placement's own complaint: a spacing or radius that is not finite and
    /// positive, a rectangle with its corners swapped, an empty list of positions, or an area
    /// that reaches outside the grid volume (see [`Placement::validate`]).
    pub fn with_placement(mut self, placement: Placement) -> Result<Self, ConfigError> {
        placement.validate(&self.boids.grid())?;
        self.placement = placement;
        Ok(self)
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
#[derive(Debug, Clone, Default)]
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

/// The `memory` bridge, as a plugin so [`FlockPlugin`] can include it conditionally.
///
/// Split out because the store's capacity is a game's decision, not the crowd's: a game
/// that wants forty thousand rows says so by adding this plugin with its own
/// [`MemoryConfig`] and capacity instead of accepting whatever the crowd happened to
/// pick. [`FlockPlugin`] adds the default when the feature is on.
#[cfg(feature = "memory")]
#[derive(Debug, Clone, Copy, Default)]
pub struct MemoryBridgePlugin;

#[cfg(feature = "memory")]
impl Plugin for MemoryBridgePlugin {
    fn build(&self, app: &mut App) {
        let population = app
            .world()
            .get_resource::<FlockConfig>()
            .map_or(1_024, |config| config.population.get());
        // Rows are 16 bytes per subject; a tenth of the crowd is a defensible default
        // for a game that never thinks about it, and the enrolment budget spreads the
        // rest over the ticks that follow.
        let capacity = num::usize_to_u32((population / 10).clamp(64, 16_384));
        crate::adapters::primary::memory_bridge::register(
            app,
            crate::adapters::primary::memory_bridge::MemoryConfig::default(),
            capacity,
            16,
        );
    }
}

/// Register the shell: the systems that turn simulation state into what an animation layer reads.
///
/// Kept out of [`Plugin::build`] so the schedule declaration in that function stays readable. The
/// orderings live here because they are the whole point of these two systems: a gesture is
/// adjudicated after the leaf that asked for it, and gaze is resolved after integration moved
/// whatever is being looked at (ADR 0052).
fn add_shell_systems(app: &mut App) {
    app
        // Gaze and gestures are decided here and posed by `bevy-flock-anim`; the numbers that
        // decide them are the crowd's, so they live with the crowd.
        .init_resource::<crate::adapters::primary::gaze::GazeConfig>()
        .init_resource::<crate::adapters::primary::gesture::GestureConfig>()
        .init_resource::<crate::adapters::primary::gesture::GestureChannel>()
        .add_systems(
            Update,
            // Leaves run inside `tick_chosen` and write their gesture requests to the blackboard,
            // so collection and adjudication follow it in the same tick: a leaf that asks on this
            // tick is answered on this tick.
            (
                plan_thinks,
                tick_chosen,
                crate::adapters::primary::gesture::collect_gesture_requests,
                crate::adapters::primary::gesture::advance_gestures,
            )
                .chain()
                .in_set(FlockSet::Cognition),
        )
        .add_systems(
            Update,
            (
                // Animation intent is derived from the velocity integration just wrote, so a game
                // that renders after `Integration` reads the same tick it drew.
                crate::adapters::primary::locomotion::derive_locomotion
                    .after(crate::boids::integrate_flock),
                // Gaze reads the transform of whatever entity cognition named, which moved this
                // tick, so it waits for the same system locomotion does.
                crate::adapters::primary::gaze::resolve_gaze
                    .after(crate::boids::integrate_flock),
            )
                .in_set(FlockSet::Integration),
        );
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
        .insert_resource(self.config.clone())
            .insert_resource(SimClock::new(self.config.ticks_per_second))
            .init_resource::<SpawnQueue>()
            .init_resource::<FlockStats>()
            .init_resource::<crate::adapters::primary::perception::PerceptionConfig>()
            .init_resource::<crate::adapters::primary::observability::DebugOverlay>()
            .init_resource::<crate::adapters::primary::observability::DebugOverlayConfig>()
            .init_resource::<crate::adapters::primary::grouping::Groups>()
            .init_resource::<crate::adapters::primary::grouping::NpcIndex>()
            .init_resource::<crate::adapters::primary::observability::CrowdPulse>()
            .init_resource::<crate::adapters::primary::observability::Diagnostics>()
            .init_resource::<crate::adapters::primary::world_director::AmbientMood>()
            .init_resource::<crate::adapters::primary::world_director::CrowdAlarms>()
            .init_resource::<crate::adapters::primary::world_director::CrowdDirector>()
            .init_resource::<crate::adapters::primary::render::CrowdView>()
            .init_resource::<crate::adapters::primary::render::RenderPlan>()
            .init_resource::<crate::adapters::primary::dialogue::Barks>()
            .init_resource::<crate::adapters::primary::dialogue::DialogHub>()
            .init_resource::<crate::adapters::primary::dialogue::DialogOutbox>()
            .init_resource::<crate::adapters::primary::perception::PerceptionConfig>()
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
            .add_systems(
                Update,
                (
                    drain_spawn_queue,
                    crate::adapters::primary::grouping::index_new_npcs,
                    crate::adapters::primary::grouping::reconcile_despawns,
                )
                    .chain()
                    .in_set(FlockSet::Spawn),
            )
            // Weights before planning (distance is a perception fact), planning before
            // ticking (the choice is the plan). Bevy is told the order rather than
            // trusted to infer it from the resource conflicts.
            .add_systems(
                Update,
                (
                    assign_tiers,
                    crate::adapters::primary::perception::observe_players,
                    crate::adapters::primary::perception::forget_the_absent,
                    crate::adapters::primary::observability::sample_crowd,
                    crate::adapters::primary::observability::count_frames,
                    crate::adapters::primary::observability::collect_debug_overlay,
                    crate::adapters::primary::dialogue::produce_barks,
                    crate::adapters::primary::dialogue::tick_dialog,
                    crate::adapters::primary::render::build_render_plan,
                )
                    .chain()
                    .in_set(FlockSet::Perception),
            )
            .add_systems(
                Update,
                (
                    crate::adapters::primary::world_director::run_world_director,
                    advance_clock,
                )
                    .chain()
                    .in_set(FlockSet::AdvanceTick),
            );

        add_shell_systems(app);

        // Memory is opt-in at build time as well as at compile time: the store's
        // capacity is the game's bill, so it comes with the config rather than with
        // the crowd. Defaults are chosen for a game that does not care.
        #[cfg(feature = "memory")]
        app.add_plugins(MemoryBridgePlugin);
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
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
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
        // The individual is drawn in full before the archetype, so a seed that produced a given
        // personality before still draws the same numbers; only the blend below is new.
        let individual = Temperament::sample(&mut rng);
        let archetype_index = rng.next_bounded(num::usize_to_u32(Archetype::ALL.len())) as usize;
        let archetype = Archetype::ALL
            .get(archetype_index)
            .copied()
            .unwrap_or(Archetype::Wanderer);
        // Two thirds preset, one third individual — the same blend `domain::generator` applies, so
        // an archetype means the same thing on both paths. Without this a guard was exactly as
        // brave as an urchin and the archetype was a costume (ADR 0051).
        let temperament = Temperament::blend(Temperament::archetype(archetype), individual, 0.34);
        // Position last: the spiral and an authored list draw nothing from the stream, so the
        // temperaments and archetypes above are the same whatever the placement is.
        let position = config.placement.position(id.index(), &mut rng);

        commands.spawn(NpcBundle {
            npc: Npc(id),
            transform: Transform::from_translation(position),
            identity: NpcIdentity {
                archetype,
                appearance_seed: u64::from(id.to_bits()).wrapping_mul(0x9E37_79B9_7F4A_7C15),
            },
            temperament: NpcTemperament(temperament),
            // Everyone starts content; the decay system and whatever satisfies needs take it from
            // here, so an NPC's state is a function of its life rather than of its spawn.
            needs: NpcNeeds(Needs::satisfied()),
            velocity: NpcVelocity(Vec3::ZERO),
            desired: NpcDesiredVelocity(Vec3::ZERO),
            locomotion: crate::adapters::primary::locomotion::Locomotion::default(),
            lod: LodTier::Near,
        });
        spawned += 1;
    }

    stats.live += spawned;
    stats.spawned_total += u64::from(spawned);
    stats.pending = queue.pending();
}

/// Where the `index`-th NPC starts under the default placement.
///
/// A golden-angle spiral with 1.6 m spacing: even coverage of a disc with no two NPCs stacked, so
/// a fresh crowd looks placed rather than dumped. Deterministic by construction, and identical to
/// [`Placement::spiral`] with [`crate::domain::spawn::DEFAULT_SPACING`].
///
/// Kept as a free function because it is the answer for the common case and callers should not
/// have to construct a placement to ask for it; a game that wants anything else passes a
/// [`Placement`] to [`FlockConfig::with_placement`].
#[must_use]
pub fn spawn_position(index: u32) -> Vec3 {
    // The spiral consumes no draws, so the stream is a formality; naming one keeps the call
    // honest about what `Placement::position` expects.
    let mut unused = RngStream::for_npc(SimSeed::new(0), NpcId::unique(0));
    Placement::default().position(index, &mut unused)
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
    use crate::testkit::OrDie;

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
            population: PopulationSize::try_new(4).or_die(),
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
            population: PopulationSize::try_new(1).or_die(),
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
            population: PopulationSize::try_new(1_000).or_die(),
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
                population: PopulationSize::try_new(20).or_die(),
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
        let slots = SlotCount::try_new(4).or_die();
        queue.push(
            FlockRequest::new(PopulationSize::try_new(8).or_die())
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
        queue.push(FlockRequest::new(PopulationSize::try_new(5).or_die()));
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
