//! Boids: the ECS side of flocking.
//!
//! Three systems, in order, once per frame:
//!
//! 1. [`rebuild_spatial_grid`] copies every NPC's position into the spatial index
//!    and rebuilds it. Rebuild, not incremental update: at crowd density a
//!    rebuild is cheaper and it cannot go stale.
//! 2. [`steer_flock`] asks each NPC for the velocity it wants, using only
//!    neighbours the index returned.
//! 3. [`integrate_flock`] accelerates each NPC toward that velocity under an
//!    acceleration limit and moves it.
//!
//! The split matters: steering is pure and testable without a world, and a game
//! that wants physics-driven NPCs replaces step 3 and keeps steps 1 and 2.
//!
//! # Using it
//!
//! [`FlockPlugin`](crate::adapters::primary::plugin::FlockPlugin) adds this plugin for you. Add it by hand only when a
//! game owns its own spawning and wants steering without the crowd layer — and
//! add it *before* `FlockPlugin`, which detects it and leaves its configuration
//! alone rather than panicking on a duplicate plugin.
//!
//! ```
//! use bevy_app::App;
//! use bevy_flock_core::domain::ids::NpcId;
//! use bevy_flock_ecs::boids::BoidsPlugin;
//! use bevy_flock_ecs::prelude::*;
//!
//! let mut app = App::new();
//! app.add_plugins(BoidsPlugin::default());
//! app.world_mut().spawn(NpcBundle::default_for(NpcId::unique(0)));
//! app.update();
//!
//! let grid = app.world().resource::<FlockSpatialGrid>();
//! assert_eq!(grid.0.len(), 1);
//! ```

use bevy_app::{App, Plugin, Update};
use bevy_ecs::prelude::*;
use bevy_math::Vec3;
use bevy_transform::prelude::Transform;

use crate::adapters::primary::components::{
    FlockMember, LodTier, Npc, NpcAvoidancePriority, NpcDesiredVelocity, NpcNeeds, NpcTarget,
    NpcTemperament, NpcVelocity,
};
use crate::adapters::primary::grid_slots::{GridSlots, NpcGridSlot};
use crate::adapters::primary::plugin::{FlockSet, FlockStats};
use crate::application::SimClock;
use crate::application::{AvoidanceParams, Crowd, CrowdParams, CrowdReport, SteeringRoad};
use crate::domain::spatial::{
    GridEntry, Neighbour, RebuildOutcome, SpatialGrid, SpatialGridConfig,
};
use crate::domain::steering::{AgentState, SteeringParams, desired_velocity};
use crate::domain::temperament::{Needs, Temperament, TemperamentDrive, WeightBias};
use crate::domain::tick::TicksPerSecond;
use crate::error::ConfigError;

/// How often unmet needs decay, in ticks.
///
/// [`Needs::decayed`] is linear in ticks, so decaying eight ticks in one pass is the same
/// arithmetic as eight single-tick passes — to within float rounding — for an eighth of the ECS
/// traffic. A crowd that ticks in bursts (an interval scheduler, a fast-forward) gets the right
/// answer rather than a stale one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeedsDecay {
    every: u32,
}

impl NeedsDecay {
    /// The shipped interval: eight ticks, which is imperceptible and cheap.
    pub const DEFAULT_INTERVAL: u32 = 8;

    /// Decay every tick.
    pub const EVERY_TICK: Self = Self { every: 1 };

    /// Decay every `every` ticks.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::ZeroBudget`] for zero, which would leave needs frozen while the
    /// documentation promised otherwise.
    pub const fn try_new(every: u32) -> Result<Self, ConfigError> {
        if every == 0 {
            return Err(ConfigError::ZeroBudget {
                field: "needs_decay",
            });
        }
        Ok(Self { every })
    }

    /// Decay every `every` ticks, with zero read as one.
    ///
    /// The forgiving form, for a knob a game exposes: a zero interval is a slip, not a request to
    /// stop time.
    #[must_use]
    pub const fn every(every: u32) -> Self {
        Self {
            every: if every == 0 { 1 } else { every },
        }
    }

    /// The interval, in ticks.
    #[must_use]
    pub const fn interval(self) -> u32 {
        self.every
    }
}

impl Default for NeedsDecay {
    fn default() -> Self {
        Self::every(Self::DEFAULT_INTERVAL)
    }
}

/// Configuration for the boids simulation.
///
/// Fields are private and construction is fallible, so the grid config can never
/// be in a state that makes [`SpatialGrid::new`] fail later.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct BoidsConfig {
    params: SteeringParams,
    grid: SpatialGridConfig,
    max_acceleration: f32,
    road: SteeringRoad,
    avoidance: Option<AvoidanceParams>,
    temperament: Option<TemperamentDrive>,
    needs: Option<NeedsDecay>,
    workers: usize,
}

impl BoidsConfig {
    /// Build a configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::InvalidDistance`] when the acceleration is not
    /// finite and positive, and propagates steering and grid validation.
    pub fn try_new(
        params: SteeringParams,
        grid: SpatialGridConfig,
        max_acceleration: f32,
    ) -> Result<Self, ConfigError> {
        if !max_acceleration.is_finite() || max_acceleration <= 0.0 {
            return Err(ConfigError::InvalidDistance {
                field: "max_acceleration",
                value: max_acceleration,
            });
        }
        params.validate()?;
        // Re-validate the grid through its own constructor so the stored value
        // is the type that already proved itself constructible.
        let grid = SpatialGridConfig::try_new(grid.min, grid.max, grid.cell_size)?;
        Ok(Self {
            params,
            grid,
            max_acceleration,
            road: SteeringRoad::Auto,
            avoidance: Some(AvoidanceParams::new()),
            temperament: Some(TemperamentDrive::default()),
            needs: Some(NeedsDecay::default()),
            workers: 0,
        })
    }

    /// The steering parameters.
    #[must_use]
    pub const fn params(&self) -> SteeringParams {
        self.params
    }

    /// Workers the per-tick rebuild may use.
    ///
    /// `0` means "as many as the machine has", resolved once per rebuild. A count larger than
    /// [`std::thread::available_parallelism`] is clamped, because oversubscription measured 25% *slower*
    /// on every phase of the sweep in `docs/dev/tuning/performance.md` — six workers on six cores beat eight every
    /// time, so a configuration cannot ask the framework to make that mistake.
    #[must_use]
    pub const fn workers(&self) -> usize {
        self.workers
    }

    /// Set the worker count for the per-tick rebuild. `0` restores the machine default.
    #[must_use]
    pub const fn with_workers(mut self, workers: usize) -> Self {
        self.workers = workers;
        self
    }

    /// The spatial grid the perception pass builds every tick.
    #[must_use]
    pub const fn grid(&self) -> SpatialGridConfig {
        self.grid
    }

    /// Replace the grid configuration.
    ///
    /// The grid is where cell size and the cell budget live, so this is how a game picks the
    /// density its crowd is scanned at — see ADR 0031 for what the cell size costs and buys.
    #[must_use]
    pub const fn with_grid(mut self, grid: SpatialGridConfig) -> Self {
        self.grid = grid;
        self
    }

    /// The acceleration limit, in metres per second squared.
    #[must_use]
    pub const fn max_acceleration(&self) -> f32 {
        self.max_acceleration
    }

    /// Replace the steering parameters.
    ///
    /// # Errors
    ///
    /// Propagates [`SteeringParams::validate`].
    pub fn with_params(mut self, params: SteeringParams) -> Result<Self, ConfigError> {
        params.validate()?;
        self.params = params;
        Ok(self)
    }

    /// Choose where a frame is steered. `Auto` unless a game asks otherwise.
    ///
    /// `SteeringRoad::Threads` on a machine with cores to spare, `SteeringRoad::Scalar` for a
    /// replay somebody wants to step through one agent at a time.
    #[must_use]
    pub const fn with_road(mut self, road: SteeringRoad) -> Self {
        self.road = road;
        self
    }

    /// Where a frame is steered.
    #[must_use]
    pub const fn road(&self) -> SteeringRoad {
        self.road
    }

    /// Turn reciprocal avoidance on or off, and set its numbers (ADR 0045).
    ///
    /// On by default with [`AvoidanceParams::new`]: two agents walking at each other in a
    /// corridor should not pass through each other, and a crowd library where that needs
    /// discovering is a crowd library that ships the bug. `None` restores pure boids steering
    /// for a game that solves separation itself.
    #[must_use]
    pub const fn with_avoidance(mut self, avoidance: Option<AvoidanceParams>) -> Self {
        self.avoidance = avoidance;
        self
    }

    /// The avoidance settings, if the pass runs.
    #[must_use]
    pub const fn avoidance(&self) -> Option<AvoidanceParams> {
        self.avoidance
    }

    /// Let each NPC's temperament scale its own steering weights (ADR 0051).
    ///
    /// On by default: an NPC whose personality never reaches the simulation is a sampled number
    /// with a doc comment. `None` restores one weight set for the whole crowd, which is what a
    /// game that builds its own biases on [`crate::application::Crowd::set_bias`] wants.
    ///
    /// The configured weights stay the baseline and the bias multiplies them, so a game tuning
    /// `separation` still gets what the number says, per agent.
    #[must_use]
    pub const fn with_temperament(mut self, temperament: Option<TemperamentDrive>) -> Self {
        self.temperament = temperament;
        self
    }

    /// The temperament drive, if personalities steer.
    #[must_use]
    pub const fn temperament(&self) -> Option<TemperamentDrive> {
        self.temperament
    }

    /// How often unmet needs decay. `None` leaves needs to the game, which is what a game with
    /// its own economy wants.
    ///
    /// Decay is on by default because a need that never moves cannot influence anything: the
    /// bias would be a constant, and every NPC would read as content.
    #[must_use]
    pub const fn with_needs_decay(mut self, needs: Option<NeedsDecay>) -> Self {
        self.needs = needs;
        self
    }

    /// The needs decay interval, if the plugin decays them.
    #[must_use]
    pub const fn needs_decay(&self) -> Option<NeedsDecay> {
        self.needs
    }

    /// Change the acceleration limit.
    ///
    /// A non-finite or non-positive value is ignored and the previous limit is
    /// kept: this is a feel knob tuned while a game is running, and refusing the
    /// value is better than producing a crowd that teleports or freezes.
    #[must_use]
    pub fn with_max_acceleration(mut self, acceleration: f32) -> Self {
        if acceleration.is_finite() && acceleration > 0.0 {
            self.max_acceleration = acceleration;
        }
        self
    }
}

impl Default for BoidsConfig {
    fn default() -> Self {
        Self {
            // The defaults are validated by construction, so the `expect` this
            // would otherwise need at every call site does not exist.
            params: SteeringParams::human_scale(),
            grid: SpatialGridConfig::default(),
            max_acceleration: 4.0,
            road: SteeringRoad::Auto,
            avoidance: Some(AvoidanceParams::new()),
            temperament: Some(TemperamentDrive::default()),
            needs: Some(NeedsDecay::default()),
            workers: 0,
        }
    }
}

/// The simulation's spatial index.
#[derive(Resource, Debug)]
pub struct FlockSpatialGrid(pub SpatialGrid);

/// What the last steering pass cost and covered.
///
/// Published rather than discarded so a game can watch its own crowd: `isolated` climbing means
/// the population has thinned out, `pairs` climbing means it has clumped.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct FlockSteerReport(pub CrowdReport);

/// What the last rebuild of the spatial index did.
///
/// Published rather than discarded so a game can watch its own crowd: most ticks move no body
/// across a cell boundary, and a crowd that reports [`RebuildOutcome::Patched`] is a crowd that
/// is being simulated rather than re-sorted. A crowd that reports
/// [`RebuildOutcome::Rebuilt`] every tick is not: something in it is moving every body every
/// tick, or the population is changing every tick.
#[derive(Resource, Debug, Clone, Copy)]
pub struct FlockGridReport(pub RebuildOutcome);

impl Default for FlockGridReport {
    fn default() -> Self {
        Self(RebuildOutcome::Unchanged)
    }
}

/// The components steering reads and writes for one NPC.
///
/// A named alias rather than an inline tuple: the query appears in signatures,
/// docs and error messages, and a six-element tuple in all three is unreadable.
type SteerQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static Transform,
        &'static NpcVelocity,
        &'static mut NpcDesiredVelocity,
        Option<&'static NpcTarget>,
        Option<&'static FlockMember>,
        Option<&'static LodTier>,
        Option<&'static NpcAvoidancePriority>,
        Option<&'static NpcTemperament>,
        Option<&'static NpcNeeds>,
    ),
>;

impl FlockSpatialGrid {
    /// Wrap a grid.
    #[must_use]
    pub const fn new(grid: SpatialGrid) -> Self {
        Self(grid)
    }

    /// A grid built from a configuration.
    ///
    /// A configuration that fails validation falls back to the degenerate one-cell grid rather than
    /// panicking: the plugin has no way to report an error to the game that configured it, and a
    /// grid that answers wrongly about one body beats a crash.
    #[must_use]
    pub fn from_config(config: SpatialGridConfig) -> Self {
        Self(SpatialGrid::new(config).unwrap_or_else(|_| SpatialGrid::empty()))
    }
}

impl Default for FlockSpatialGrid {
    fn default() -> Self {
        // A grid built from a validated config cannot fail; the degenerate
        // fallback exists so this type has no panic path at all.
        let grid =
            SpatialGrid::new(SpatialGridConfig::default()).unwrap_or_else(|_| SpatialGrid::empty());
        Self(grid)
    }
}

/// Adds the boids systems.
///
/// Added automatically by [`FlockPlugin`](crate::adapters::primary::plugin::FlockPlugin); usable on its own when a game
/// owns its own spawning and only wants steering.
#[derive(Debug, Clone, Copy, Default)]
pub struct BoidsPlugin {
    /// Steering, grid and acceleration settings.
    pub config: BoidsConfig,
}

impl BoidsPlugin {
    /// Plugins with an explicit configuration.
    #[must_use]
    pub const fn new(config: BoidsConfig) -> Self {
        Self { config }
    }
}

impl Plugin for BoidsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(self.config)
            .insert_resource(FlockSpatialGrid::from_config(self.config.grid))
            .insert_resource(FlockSteerReport::default())
            .insert_resource(FlockGridReport::default())
            .init_resource::<GridSlots>()
            .init_resource::<FlockStats>()
            .configure_sets(
                Update,
                (
                    FlockSet::Perception,
                    FlockSet::Cognition,
                    FlockSet::Steering,
                    FlockSet::Integration,
                )
                    .chain(),
            )
            .add_systems(Update, rebuild_spatial_grid.in_set(FlockSet::Perception))
            .add_systems(Update, steer_flock.in_set(FlockSet::Steering))
            .add_systems(
                Update,
                (integrate_flock, decay_needs)
                    .chain()
                    .in_set(FlockSet::Integration),
            );
    }
}

/// Copy every NPC into the spatial index and rebuild it.
///
/// Reads positions, velocities and slots; writes the grid's entry array and the window table it
/// indexes. Runs first in the frame so steering sees the world as it is now, not as it was last
/// tick.
///
/// The grid's incremental path needs the same body at the same entry index every tick, which a
/// query rebuilt from scratch cannot promise: the order a query yields is the archetype order, and
/// that changes when a body is spawned or despawned, so a body that changed archetype would look
/// to the patch like a body that crossed a cell boundary. Each NPC therefore owns a slot, and the
/// entry array is written by slot rather than appended to.
///
/// When the slots do not describe the population — a body the plugin never saw, a table that lost
/// one — the system falls back to the full rebuild it always did. A correct index is worth more
/// than a fast one.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn rebuild_spatial_grid(
    mut grid: ResMut<FlockSpatialGrid>,
    config: Res<BoidsConfig>,
    mut slots: ResMut<GridSlots>,
    mut report: ResMut<FlockGridReport>,
    mut stats: ResMut<FlockStats>,
    mut commands: Commands,
    mut removed: RemovedComponents<Npc>,
    mut query: Query<(
        Entity,
        &Transform,
        &Npc,
        &NpcVelocity,
        Option<&mut NpcGridSlot>,
    )>,
) {
    // Asking for more workers than the machine has costs throughput rather than buying it: the sweep in
    // `docs/dev/tuning/performance.md` measured eight workers on six cores at 25% slower than six. `0` means the
    // machine default, so the framework never oversubscribes unless someone insists by name.
    let available = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let workers = if config.workers() == 0 {
        available
    } else {
        config.workers().min(available)
    };
    if workers != grid.0.threads() {
        // `with_threads` takes the grid by value; the grid is the resource's own, so this is a move
        // rather than a copy of the cell array.
        let grid_owned =
            std::mem::replace(&mut grid.0, crate::domain::spatial::SpatialGrid::empty());
        grid.0 = grid_owned.with_threads(workers);
    }

    // A despawned body gives its slot back. The event arrives a frame after the despawn, so for one
    // tick the table still holds a body that is gone; the counts below then disagree, and the
    // fallback keeps the dead body out of the index.
    for entity in removed.read() {
        slots.release(entity);
    }
    // A slot freed in the middle leaves a gap, and the entry array cannot describe a gap, so the
    // body in the highest live slot moves down into it.
    let moved = slots.compact();

    let live = query.iter_mut().len();
    // The fast path needs the entry array to be the population's array: the same length, and each
    // body at its own slot. The table says how many bodies own a slot; when that is not the number
    // of bodies in the world, the table describes a different population, and nothing in it can be
    // diffed against the entries.
    let fast = grid.0.entries().len() == live && slots.live() == live && slots.dense();

    if fast {
        // The slots have to be one body each. A body whose component names a slot another body
        // owns would put two bodies at one index and lose one of them from the index, so the
        // duplicates are caught here and the tick falls back to a rebuild.
        let mut seen = vec![0_u64; live.div_ceil(u64::BITS as usize)];
        let mut duplicated = false;
        for (entity, transform, npc, velocity, slot) in &mut query {
            let Some(mut slot) = slot else {
                // A body the plugin never saw: the slot it would have had is a guess, so this tick
                // is a rebuild. The slot it is given lands next tick.
                duplicated = true;
                continue;
            };
            let index = match moved {
                Some(moved) if moved.entity == entity => moved.to,
                _ => slot.index(),
            };
            let Some(word) = seen.get_mut((index as usize) / u64::BITS as usize) else {
                duplicated = true;
                continue;
            };
            let bit = 1_u64 << (index as usize % u64::BITS as usize);
            if *word & bit != 0 {
                duplicated = true;
                continue;
            }
            *word |= bit;
            if let Some(entry) = grid.0.entries_mut().get_mut(index as usize) {
                *entry = GridEntry {
                    id: npc.0,
                    position: transform.translation,
                    velocity: velocity.0,
                };
            }
            // A body the table moved owns its new slot now, whatever its component still says.
            if index != slot.index() {
                *slot = NpcGridSlot(index);
            }
        }
        if !duplicated {
            let outcome = grid.0.build_incremental();
            record_outcome(&mut report, &mut stats, outcome);
            return;
        }
    }

    // The fallback: the whole index, rebuilt from the bodies in front of us. The rebuild goes
    // through the incremental path so the grid's cell memory describes the entries it just took:
    // the next tick can then patch instead of starting over.
    grid.0.clear();
    for (entity, transform, npc, velocity, slot) in &mut query {
        let entry = GridEntry {
            id: npc.0,
            position: transform.translation,
            velocity: velocity.0,
        };
        if let Some(mut slot) = slot {
            let index = match moved {
                Some(moved) if moved.entity == entity => moved.to,
                _ => slot.index(),
            };
            if index != slot.index() {
                *slot = NpcGridSlot(index);
            }
            if slots.slot_of(entity).is_none() {
                // The table has never seen this body, so the slot the component names is not the
                // one the table handed out. The table is given the body and the component is
                // corrected to match.
                let assigned = slots.allocate(entity);
                *slot = NpcGridSlot(assigned);
            }
        } else {
            // A body the plugin never saw has no slot yet. It gets one now, and the component
            // lands next tick, so this tick's index is a rebuild rather than a patch.
            let assigned = slots
                .slot_of(entity)
                .unwrap_or_else(|| slots.allocate(entity));
            commands.entity(entity).insert(NpcGridSlot(assigned));
        }
        grid.0.insert(entry);
    }
    let outcome = grid.0.build_incremental();
    record_outcome(&mut report, &mut stats, outcome);
}

/// Note what the rebuild did, so a game can see whether its crowd patches or rebuilds.
const fn record_outcome(
    report: &mut FlockGridReport,
    stats: &mut FlockStats,
    outcome: RebuildOutcome,
) {
    match outcome {
        RebuildOutcome::Patched { .. } => {
            stats.patched_ticks = stats.patched_ticks.saturating_add(1);
        }
        RebuildOutcome::Rebuilt { .. } => {
            stats.rebuilt_ticks = stats.rebuilt_ticks.saturating_add(1);
        }
        RebuildOutcome::Unchanged => {}
    }
    report.0 = outcome;
}

/// What the steering pass reads from each NPC.
///
/// A named alias rather than a tuple at the parameter: eight elements inline is a type a reader has
/// to parse, and the compiler does not care either way.
type GatherQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static Transform,
        &'static NpcVelocity,
        Option<&'static NpcTarget>,
        Option<&'static LodTier>,
        Option<&'static NpcAvoidancePriority>,
        Option<&'static NpcTemperament>,
        Option<&'static NpcNeeds>,
    ),
>;

/// The bias an agent with no personality carries: every weight as configured.
const NEUTRAL_BIAS: [f32; 5] = [1.0; 5];

/// The weight scale one NPC's personality and unmet needs ask for.
///
/// A missing temperament reads as [`Temperament::balanced`] and a missing needs component as
/// [`Needs::satisfied`], so an NPC spawned without the full bundle still steers — with the
/// configured weights rather than not at all.
fn bias_of(
    temperament: Option<&NpcTemperament>,
    needs: Option<&NpcNeeds>,
    drive: TemperamentDrive,
) -> [f32; 5] {
    let balanced = Temperament::balanced();
    let satisfied = Needs::satisfied();
    let who = temperament.map_or(&balanced, |component| &component.0);
    let state = needs.map_or(&satisfied, |component| &component.0);
    drive.bias(who, state).to_array()
}

/// Decay every NPC's unmet needs, in bursts, scaled by temperament (ADR 0051).
///
/// Runs after integration, once per `interval` frames. `Needs::decayed` is linear in ticks, so a
/// burst is the same arithmetic as a tick-by-tick pass; the counter is a `Local`, so the system
/// allocates nothing and its state is part of the schedule rather than of the world.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn decay_needs(
    config: Res<BoidsConfig>,
    mut elapsed: Local<u32>,
    mut query: Query<(&NpcTemperament, &mut NpcNeeds)>,
) {
    let Some(decay) = config.needs else {
        return;
    };
    *elapsed = elapsed.saturating_add(1);
    if *elapsed < decay.interval() {
        return;
    }
    let ticks = u64::from(*elapsed);
    *elapsed = 0;
    for (temperament, mut needs) in &mut query {
        needs.0 = needs.0.decayed(&temperament.0, ticks).sanitised();
    }
}

/// Ask every NPC for the velocity it wants, through the cell-major kernel.
///
/// Gathers the crowd into arrays in grid-cell order, steers it with one pass per agent over the
/// cells in range, and writes [`NpcDesiredVelocity`] back. The buffers live in a `Local`, so a
/// steady frame allocates nothing; `tests/crowd_no_alloc.rs` holds that.
///
/// This is the path ADR 0026 records. The model is unchanged — the same weights, the same
/// priority truncation, the same arrival curve — but the loop is inverted, and the distance sort
/// and the per-agent `Vec<Neighbour>` are gone.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn steer_flock(
    grid: Res<FlockSpatialGrid>,
    config: Res<BoidsConfig>,
    mut crowd: Local<Crowd>,
    mut entities: Local<Vec<Entity>>,
    gather: GatherQuery,
    mut desired: Query<&mut NpcDesiredVelocity>,
    mut report: ResMut<FlockSteerReport>,
) {
    let params = CrowdParams::from_steering(&config.params);
    crowd.set_road(config.road);
    crowd.configure_from_grid(&grid.0);
    crowd.begin();
    entities.clear();
    let drive = config.temperament;
    for (entity, transform, velocity, target, tier, priority, temperament, needs) in &gather {
        // Ambient-tier NPCs are population mass: neighbours to everyone, steered by nobody.
        let steers = !matches!(tier, Some(LodTier::Ambient));
        entities.push(entity);
        crowd.push(
            [
                transform.translation.x,
                transform.translation.y,
                transform.translation.z,
            ],
            [velocity.0.x, velocity.0.y, velocity.0.z],
            target.map(|target| [target.0.x, target.0.y, target.0.z]),
            steers,
        );
        if let Some(priority) = priority {
            crowd.set_priority(entities.len() - 1, priority.get());
        }
        // Personality scales this agent's weights; agents without one keep the configured set, so
        // the buffer stays short for a crowd nobody gave a temperament to.
        if let Some(drive) = drive {
            let bias = bias_of(temperament, needs, drive);
            if bias != NEUTRAL_BIAS {
                crowd.set_bias(entities.len() - 1, bias);
            }
        }
    }

    let mut crowd_report = crowd.steer(&params);

    // One reciprocal pass over the same cell order, after every agent has said where it wants to
    // go: correcting during steering would make the result depend on who was processed first.
    // Ambient agents are in the buffers, so they are avoided without being steered.
    if let Some(avoidance) = config.avoidance {
        let avoided = crowd.avoid(&avoidance);
        crowd_report.avoided = avoided.adjusted;
        crowd_report.avoid_pairs = avoided.pairs;
    }
    report.0 = crowd_report;

    for (index, entity) in entities.iter().enumerate() {
        let Ok(mut slot) = desired.get_mut(*entity) else {
            continue;
        };
        slot.0 = crowd.desired(index).map_or(Vec3::ZERO, |desired| {
            Vec3::new(desired[0], desired[1], desired[2])
        });
    }
}

/// The per-agent reference implementation, kept for the parity tests and for callers who need
/// neighbours they can inspect.
///
/// Not in the schedule: it materialises a sorted `Vec<Neighbour>` per agent, which is the cost
/// [`steer_flock`] exists to remove.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn steer_flock_reference(
    grid: Res<FlockSpatialGrid>,
    config: Res<BoidsConfig>,
    mut scratch: Local<Vec<Neighbour>>,
    mut query: SteerQuery,
) {
    let params = config.params;
    for (transform, velocity, mut desired, target, member, tier, _priority, temperament, needs) in
        &mut query
    {
        if matches!(tier, Some(LodTier::Ambient)) {
            desired.0 = Vec3::ZERO;
            continue;
        }
        let _ = member;
        // The kernel scales the weights per agent; the reference path has to do the same or the
        // parity tests would be comparing two different simulations.
        let mut params = params;
        if let Some(drive) = config.temperament {
            let bias = bias_of(temperament, needs, drive);
            if bias != NEUTRAL_BIAS {
                let scaled = WeightBias::from_array(bias).apply([
                    params.weights.separation,
                    params.weights.alignment,
                    params.weights.cohesion,
                    params.weights.seek,
                    params.weights.arrive,
                ]);
                params.weights = crate::domain::steering::SteeringWeights {
                    separation: scaled[0],
                    alignment: scaled[1],
                    cohesion: scaled[2],
                    seek: scaled[3],
                    arrive: scaled[4],
                };
            }
        }

        grid.0
            .radius(transform.translation, params.neighbour_radius, &mut scratch);
        let agent = AgentState {
            position: transform.translation,
            velocity: velocity.0,
            target: target.map(|target| target.0),
        };
        desired.0 = desired_velocity(agent, &scratch, &params);
    }
}

/// Accelerate each NPC toward its desired velocity and move it.
///
/// Reads [`NpcDesiredVelocity`]; writes [`NpcVelocity`] and `Transform`. The
/// acceleration limit is what stops a crowd from snapping direction and reading
/// as a particle system.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn integrate_flock(
    clock: Option<Res<SimClock>>,
    config: Res<BoidsConfig>,
    mut query: Query<(&mut Transform, &mut NpcVelocity, &NpcDesiredVelocity)>,
) {
    let dt = clock.map_or_else(
        || TicksPerSecond::SIXTY.delta_seconds(),
        |clock| clock.delta_seconds(),
    );
    let max_delta_v = config.max_acceleration * dt;

    for (mut transform, mut velocity, desired) in &mut query {
        let change = (desired.0 - velocity.0).clamp_length_max(max_delta_v);
        velocity.0 += change;
        transform.translation += velocity.0 * dt;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::primary::components::NpcBundle;
    use crate::domain::ids::NpcId;
    use crate::domain::num;
    use crate::domain::spatial::{PATCH_MAX_MOVERS, RebuildCause};
    use crate::domain::temperament::Weight;
    use crate::testkit::OrDie;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(BoidsPlugin::default());
        app
    }

    /// A grid with four-metre cells.
    ///
    /// The default grid is a one-kilometre cube, and a budget of a million cells coarsens it to
    /// thirteen and a half metres, so a body that moves four metres has not crossed a boundary at
    /// all. The tests that move bodies across a boundary need the boundary where they say it is.
    fn four_metre_grid() -> SpatialGridConfig {
        SpatialGridConfig::try_new(Vec3::splat(-100.0), Vec3::splat(100.0), 4.0).or_die()
    }

    /// An app whose spatial index has four-metre cells.
    fn app_with_four_metre_cells() -> App {
        let mut app = App::new();
        app.add_plugins(BoidsPlugin::new(
            BoidsConfig::default().with_grid(four_metre_grid()),
        ));
        app
    }

    #[test]
    fn grid_reflects_spawned_entities() {
        let mut app = app();
        for index in 0..10_u32 {
            app.world_mut()
                .spawn(NpcBundle::default_for(NpcId::unique(index)));
        }
        app.update();
        assert_eq!(app.world().resource::<FlockSpatialGrid>().0.len(), 10);
    }

    #[test]
    fn separation_pushes_neighbours_apart() {
        let mut app = app();
        // Two agents nearly on top of each other, both with a target.
        let left = app
            .world_mut()
            .spawn(NpcBundle::default_for(NpcId::unique(0)))
            .insert(NpcTarget(Vec3::new(50.0, 0.0, 0.0)))
            .id();
        let right = app
            .world_mut()
            .spawn(NpcBundle::default_for(NpcId::unique(1)))
            .insert(NpcTarget(Vec3::new(50.0, 0.0, 0.0)))
            .id();
        app.world_mut()
            .get_mut::<Transform>(right)
            .or_die()
            .translation = Vec3::new(0.1, 0.0, 0.0);

        for _ in 0..120 {
            app.update();
        }

        let left_position = app.world().get::<Transform>(left).or_die().translation;
        let right_position = app.world().get::<Transform>(right).or_die().translation;
        assert!(
            left_position.distance(right_position) > 0.4,
            "agents did not separate: {left_position} vs {right_position}"
        );
    }

    #[test]
    fn ambient_tier_agents_are_not_steered() {
        let mut app = app();
        let entity = app
            .world_mut()
            .spawn(NpcBundle::default_for(NpcId::unique(0)))
            .insert(NpcTarget(Vec3::new(100.0, 0.0, 0.0)))
            .insert(LodTier::Ambient)
            .id();
        app.update();
        let desired = app.world().get::<NpcDesiredVelocity>(entity).or_die();
        assert_eq!(desired.0, Vec3::ZERO);
    }

    #[test]
    fn acceleration_is_limited() {
        let mut app = app();
        let entity = app
            .world_mut()
            .spawn(NpcBundle::default_for(NpcId::unique(0)))
            .insert(NpcTarget(Vec3::new(100.0, 0.0, 0.0)))
            .id();
        app.update();
        let velocity = app.world().get::<NpcVelocity>(entity).or_die().0;
        let config = *app.world().resource::<BoidsConfig>();
        let limit = config.max_acceleration() * TicksPerSecond::SIXTY.delta_seconds();
        assert!(
            velocity.length() <= limit + 1e-4,
            "velocity {} exceeded the per-tick limit {limit}",
            velocity.length()
        );
    }

    /// Two NPCs walking at each other down a corridor, with the soft separation weight off so
    /// steering alone cannot keep them apart. Returns their closest approach in metres.
    fn head_on(avoidance: Option<AvoidanceParams>) -> (f32, bool) {
        let mut params = SteeringParams::human_scale();
        params.weights.separation = 0.0;
        let config = BoidsConfig::default()
            .with_params(params)
            .or_die()
            .with_avoidance(avoidance);
        let mut app = App::new();
        app.add_plugins(BoidsPlugin::new(config));
        let left = app
            .world_mut()
            .spawn(
                NpcBundle::default_for(NpcId::unique(0)).with_position(Vec3::new(-6.0, 0.0, 0.0)),
            )
            .insert(NpcTarget(Vec3::new(9.0, 0.0, 0.0)))
            .id();
        let right = app
            .world_mut()
            .spawn(NpcBundle::default_for(NpcId::unique(1)).with_position(Vec3::new(6.0, 0.0, 0.0)))
            .insert(NpcTarget(Vec3::new(-9.0, 0.0, 0.0)))
            .id();
        let mut closest = f32::MAX;
        for _ in 0..360 {
            app.update();
            let left = app.world().get::<Transform>(left).or_die().translation;
            let right = app.world().get::<Transform>(right).or_die().translation;
            closest = closest.min(left.distance(right));
        }
        let left = app.world().get::<Transform>(left).or_die().translation;
        let right = app.world().get::<Transform>(right).or_die().translation;
        (closest, left.x < right.x)
    }

    #[test]
    fn avoidance_stops_two_agents_walking_through_each_other() {
        let (without, _) = head_on(None);
        assert!(
            without < 0.5,
            "the fixture must overlap without avoidance, else it proves nothing (got {without})"
        );
        let (with, kept_order) = head_on(Some(AvoidanceParams::new()));
        assert!(
            with >= 0.7,
            "the separation floor must hold for the whole approach (closest {with})"
        );
        assert!(kept_order, "they must not have swapped sides");
    }

    #[test]
    fn avoidance_priority_decides_who_yields() {
        // A cart (4.0) against a pedestrian (1.0), head-on: the pedestrian does the stepping
        // aside, so the cart keeps far more of its speed along the corridor.
        let mut params = SteeringParams::human_scale();
        params.weights.separation = 0.0;
        let config = BoidsConfig::default()
            .with_params(params)
            .or_die()
            .with_avoidance(Some(AvoidanceParams::new()));
        let mut app = App::new();
        app.add_plugins(BoidsPlugin::new(config));
        let cart = app
            .world_mut()
            .spawn(
                NpcBundle::default_for(NpcId::unique(0)).with_position(Vec3::new(-1.2, 0.0, 0.0)),
            )
            .insert(NpcTarget(Vec3::new(9.0, 0.0, 0.0)))
            .insert(NpcAvoidancePriority::new(4.0))
            .id();
        let person = app
            .world_mut()
            .spawn(NpcBundle::default_for(NpcId::unique(1)).with_position(Vec3::new(0.0, 0.0, 0.0)))
            .insert(NpcTarget(Vec3::new(-9.0, 0.0, 0.0)))
            .insert(NpcAvoidancePriority::new(1.0))
            .id();
        app.update();
        let cart = app.world().get::<NpcDesiredVelocity>(cart).or_die().0.x;
        let person = app.world().get::<NpcDesiredVelocity>(person).or_die().0.x;
        assert!(
            cart > -person,
            "the cart must keep more of its speed than the pedestrian: cart {cart}, person {person}"
        );
    }

    #[test]
    fn avoidance_is_reported() {
        let mut app = App::new();
        app.add_plugins(BoidsPlugin::default());
        let left = app
            .world_mut()
            .spawn(
                NpcBundle::default_for(NpcId::unique(0)).with_position(Vec3::new(-1.2, 0.0, 0.0)),
            )
            .insert(NpcTarget(Vec3::new(9.0, 0.0, 0.0)))
            .id();
        app.world_mut()
            .spawn(NpcBundle::default_for(NpcId::unique(1)).with_position(Vec3::new(0.0, 0.0, 0.0)))
            .insert(NpcTarget(Vec3::new(-9.0, 0.0, 0.0)));
        app.update();
        assert!(app.world().get::<Transform>(left).is_some());
        let report = app.world().resource::<FlockSteerReport>().0;
        assert_eq!(report.steered, 2);
        assert!(report.avoid_pairs > 0, "the pair must be examined");
        assert!(report.avoided > 0, "at least one agent must be corrected");
    }

    #[test]
    fn simulation_is_deterministic_across_runs() {
        fn run() -> Vec<Vec3> {
            let mut app = app();
            for index in 0..25_u32 {
                let angle = num::u32_to_f32(index) * 0.25;
                let position = Vec3::new(libm::cosf(angle), 0.0, libm::sinf(angle)) * 3.0;
                app.world_mut()
                    .spawn(NpcBundle::default_for(NpcId::unique(index)).with_position(position))
                    .insert(NpcTarget(Vec3::new(20.0, 0.0, 0.0)));
            }
            for _ in 0..30 {
                app.update();
            }
            let mut positions: Vec<(u32, Vec3)> = app
                .world()
                .iter_entities()
                .filter_map(|entity| {
                    let npc = entity.get::<Npc>()?;
                    let transform = entity.get::<Transform>()?;
                    Some((npc.0.index(), transform.translation))
                })
                .collect();
            positions.sort_by_key(|(index, _)| *index);
            positions
                .into_iter()
                .map(|(_, position)| position)
                .collect()
        }

        assert_eq!(
            run(),
            run(),
            "the same world must produce the same positions"
        );
    }

    /// Three agents in a row: the left one has the group to its right, so cohesion points +x.
    fn row_app(config: BoidsConfig) -> App {
        let mut app = App::new();
        app.add_plugins(BoidsPlugin::new(config));
        for (index, x) in [0.0_f32, 2.0, 4.0].into_iter().enumerate() {
            let entity = app
                .world_mut()
                .spawn(NpcBundle::default_for(NpcId::unique(num::usize_to_u32(
                    index,
                ))))
                .id();
            app.world_mut()
                .get_mut::<Transform>(entity)
                .or_die()
                .translation = Vec3::new(x, 0.0, 0.0);
        }
        app
    }

    /// The desired velocity the first agent was given this frame.
    fn first_desired(app: &mut App) -> Vec3 {
        app.update();
        let mut query = app.world_mut().query::<(&Npc, &NpcDesiredVelocity)>();
        query
            .iter(app.world())
            .find(|(npc, _)| npc.0.index() == 0)
            .map(|(_, desired)| desired.0)
            .or_die()
    }

    #[test]
    fn a_gregarious_person_drifts_toward_the_group() {
        let gregarious = Temperament {
            sociability: Weight::MAX,
            extraversion: Weight::MAX,
            ..Temperament::balanced()
        };
        let hermit = Temperament {
            sociability: Weight::MIN,
            extraversion: Weight::MIN,
            ..Temperament::balanced()
        };
        let mut with_company = row_app(BoidsConfig::default());
        let mut alone = row_app(BoidsConfig::default());
        // The same fixture in both apps, and a different person in each: any difference in the
        // step is the personality's.
        for (app, temperament) in [(&mut with_company, gregarious), (&mut alone, hermit)] {
            let entity = app
                .world_mut()
                .query_filtered::<Entity, bevy_ecs::prelude::With<Npc>>()
                .iter(app.world())
                .next()
                .or_die();
            *app.world_mut().get_mut::<NpcTemperament>(entity).or_die() =
                NpcTemperament(temperament);
        }
        let drawn_in = first_desired(&mut with_company).x;
        let uninterested = first_desired(&mut alone).x;
        assert!(
            drawn_in > uninterested + 0.01,
            "a gregarious agent must drift toward the group harder than a hermit: {drawn_in} vs {uninterested}"
        );
    }

    #[test]
    fn a_lonely_person_drifts_toward_the_group() {
        let mut app = row_app(BoidsConfig::default());
        let entity = app
            .world_mut()
            .query_filtered::<Entity, bevy_ecs::prelude::With<Npc>>()
            .iter(app.world())
            .next()
            .or_die();
        let before = first_desired(&mut app).x;
        *app.world_mut().get_mut::<NpcNeeds>(entity).or_die() = NpcNeeds(Needs {
            company: 0.0,
            ..Needs::satisfied()
        });
        let after = first_desired(&mut app).x;
        assert!(
            after > before + 0.01,
            "an unmet need for company must pull the agent in: {before} vs {after}"
        );
    }

    #[test]
    fn a_crowd_without_a_drive_steers_uniformly() {
        let config = BoidsConfig::default().with_temperament(None);
        let mut a = row_app(config);
        let mut b = row_app(config);
        for (app, temperament) in [
            (
                &mut a,
                Temperament {
                    sociability: Weight::MAX,
                    ..Temperament::balanced()
                },
            ),
            (
                &mut b,
                Temperament {
                    sociability: Weight::MIN,
                    ..Temperament::balanced()
                },
            ),
        ] {
            let entity = app
                .world_mut()
                .query_filtered::<Entity, bevy_ecs::prelude::With<Npc>>()
                .iter(app.world())
                .next()
                .or_die();
            *app.world_mut().get_mut::<NpcTemperament>(entity).or_die() =
                NpcTemperament(temperament);
        }
        assert_eq!(
            first_desired(&mut a),
            first_desired(&mut b),
            "with the drive off, personality must not reach the steering"
        );
    }

    #[test]
    fn needs_decay_on_their_interval_and_no_faster() {
        let config = BoidsConfig::default()
            .with_temperament(None)
            .with_needs_decay(Some(NeedsDecay::every(4)));
        let mut app = row_app(config);
        let entity = app
            .world_mut()
            .query_filtered::<Entity, bevy_ecs::prelude::With<Npc>>()
            .iter(app.world())
            .next()
            .or_die();
        let start = Needs::satisfied();
        *app.world_mut().get_mut::<NpcNeeds>(entity).or_die() = NpcNeeds(start);
        for _ in 0..3 {
            app.update();
        }
        assert_eq!(
            app.world().get::<NpcNeeds>(entity).or_die().0,
            start,
            "decay must not run before the interval elapses"
        );
        app.update();
        let decayed = app.world().get::<NpcNeeds>(entity).or_die().0;
        assert!(
            decayed.satiety < start.satiety,
            "the fourth frame decays the burst"
        );
    }

    #[test]
    fn the_ecs_rebuild_is_identical_at_any_worker_count() {
        // The sweep found that the rebuild scales, but a framework is not a benchmark: the wiring has to
        // produce the same grid in a running App as it does on the bench fixture. Two thousand bodies is
        // over `PARALLEL_MIN_BODIES`, so the parallel path is genuinely taken rather than skipped.
        let digests: Vec<u32> = [1_usize, 2, 3, 8]
            .into_iter()
            .map(|workers| {
                let mut app = app();
                app.world_mut()
                    .resource_mut::<BoidsConfig>()
                    .clone_from(&BoidsConfig::default().with_workers(workers));
                for index in 0..2_000_u32 {
                    let step = crate::domain::num::u32_to_f32(index);
                    let radius = 3.0 + (step * 0.37).rem_euclid(20.0);
                    let angle = step * 0.618_034;
                    app.world_mut().spawn(
                        NpcBundle::default_for(NpcId::unique(index)).with_position(Vec3::new(
                            radius * libm::cosf(angle),
                            0.0,
                            radius * libm::sinf(angle),
                        )),
                    );
                }
                app.update();
                let grid = app.world().resource::<FlockSpatialGrid>();
                crate::testkit::spatial_digest(
                    &grid.0,
                    &[Vec3::ZERO, Vec3::new(9.0, 0.0, 4.0)],
                    12.0,
                )
            })
            .collect();
        assert!(
            digests.windows(2).all(|pair| pair.first() == pair.get(1)),
            "the ECS rebuild must not depend on its worker count, got {digests:?}"
        );
    }

    #[test]
    fn decay_follows_the_person_and_not_the_clock_alone() {
        let config = BoidsConfig::default().with_needs_decay(Some(NeedsDecay::EVERY_TICK));
        let mut app = row_app(config);
        let entities: Vec<Entity> = app
            .world_mut()
            .query_filtered::<Entity, bevy_ecs::prelude::With<Npc>>()
            .iter(app.world())
            .collect();
        let gregarious = Temperament {
            sociability: Weight::MAX,
            extraversion: Weight::MAX,
            ..Temperament::balanced()
        };
        let hermit = Temperament {
            sociability: Weight::MIN,
            extraversion: Weight::MIN,
            ..Temperament::balanced()
        };
        for (entity, temperament) in entities.iter().zip([gregarious, hermit, gregarious]) {
            *app.world_mut().get_mut::<NpcTemperament>(*entity).or_die() =
                NpcTemperament(temperament);
            *app.world_mut().get_mut::<NpcNeeds>(*entity).or_die() = NpcNeeds(Needs::satisfied());
        }
        for _ in 0..600 {
            app.update();
        }
        let company: Vec<f32> = entities
            .iter()
            .map(|entity| app.world().get::<NpcNeeds>(*entity).or_die().0.company)
            .collect();
        let first = company.first().or_die();
        let second = company.get(1).or_die();
        assert!(
            first < second,
            "the gregarious agent must lose company faster: {company:?}"
        );
    }

    /// A crowd of bodies that stands still: ambient tier agents are not steered, so the only
    /// thing that can move them is the test.
    fn standing_crowd(app: &mut App, count: u32) -> Vec<Entity> {
        (0..count)
            .map(|index| {
                app.world_mut()
                    .spawn(
                        NpcBundle::default_for(NpcId::unique(index))
                            .with_position(Vec3::new(5.0, 5.0, 5.0))
                            .with_lod(LodTier::Ambient),
                    )
                    .id()
            })
            .collect()
    }

    /// The index the same bodies would get if it were built from scratch.
    fn reference_grid(app: &App, entities: &[Entity], config: SpatialGridConfig) -> SpatialGrid {
        let mut grid = SpatialGrid::new(config).or_die();
        for entity in entities {
            let transform = app.world().get::<Transform>(*entity).or_die();
            let npc = app.world().get::<Npc>(*entity).or_die();
            let velocity = app.world().get::<NpcVelocity>(*entity).or_die();
            grid.insert(GridEntry {
                id: npc.0,
                position: transform.translation,
                velocity: velocity.0,
            });
        }
        grid.build();
        grid
    }

    /// What the last rebuild did.
    fn outcome(app: &App) -> RebuildOutcome {
        app.world().resource::<FlockGridReport>().0
    }

    #[test]
    fn a_still_crowd_does_not_rebuild_the_index() {
        let mut app = app();
        standing_crowd(&mut app, 10);
        // The first tick hands out the slots and rebuilds; the second is the tick the index can
        // patch, and a crowd that stands still has nothing to patch.
        app.update();
        app.update();
        assert_eq!(outcome(&app), RebuildOutcome::Unchanged);
        assert_eq!(app.world().resource::<FlockStats>().rebuilt_ticks, 1);
        assert_eq!(app.world().resource::<FlockStats>().patched_ticks, 0);
    }

    #[test]
    fn bodies_crossing_a_cell_boundary_are_patched() {
        let mut app = app_with_four_metre_cells();
        let entities = standing_crowd(&mut app, 10);
        app.update();
        // The cells are four metres wide and start at -100, so x = 5 and x = 9 are in different
        // cells: every body crosses one.
        for entity in &entities {
            app.world_mut()
                .get_mut::<Transform>(*entity)
                .or_die()
                .translation = Vec3::new(9.0, 5.0, 5.0);
        }
        app.update();
        assert_eq!(outcome(&app), RebuildOutcome::Patched { changed: 10 });
        // The patched index has to answer exactly like an index built from scratch.
        let probes = [Vec3::new(9.0, 5.0, 5.0)];
        let patched = crate::testkit::spatial_digest(
            &app.world().resource::<FlockSpatialGrid>().0,
            &probes,
            12.0,
        );
        let reference = crate::testkit::spatial_digest(
            &reference_grid(&app, &entities, four_metre_grid()),
            &probes,
            12.0,
        );
        assert_eq!(patched, reference);
    }

    #[test]
    fn a_crowd_that_moves_everywhere_is_rebuilt() {
        let mut app = app_with_four_metre_cells();
        // More movers than the patch can absorb, so the index has to be counted again.
        let entities = standing_crowd(&mut app, u32::try_from(PATCH_MAX_MOVERS + 1).or_die());
        app.update();
        for entity in &entities {
            app.world_mut()
                .get_mut::<Transform>(*entity)
                .or_die()
                .translation = Vec3::new(9.0, 5.0, 5.0);
        }
        app.update();
        assert_eq!(
            outcome(&app),
            RebuildOutcome::Rebuilt {
                changed: entities.len(),
                cause: RebuildCause::TooManyMovers,
            }
        );
    }

    #[test]
    fn a_spawn_and_a_despawn_fall_back_to_a_rebuild() {
        let mut app = app();
        let entities = standing_crowd(&mut app, 10);
        app.update();
        // One body leaves and one arrives: the table describes a different population, so there is
        // nothing in it that can be diffed against the entries.
        app.world_mut().despawn(entities.first().copied().or_die());
        app.world_mut().spawn(
            NpcBundle::default_for(NpcId::unique(999)).with_position(Vec3::new(5.0, 5.0, 5.0)),
        );
        app.update();
        assert_eq!(
            outcome(&app),
            RebuildOutcome::Rebuilt {
                changed: 10,
                cause: RebuildCause::Population,
            }
        );
    }
}
