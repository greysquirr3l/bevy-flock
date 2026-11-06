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
//! [`crate::FlockPlugin`] adds this plugin for you. Add it by hand only when a
//! game owns its own spawning and wants steering without the crowd layer — and
//! add it *before* `FlockPlugin`, which detects it and leaves its configuration
//! alone rather than panicking on a duplicate plugin.
//!
//! ```
//! use bevy_app::App;
//! use bevy_flock::boids::BoidsPlugin;
//! use bevy_flock::prelude::*;
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
use crate::adapters::primary::plugin::FlockSet;
use crate::application::SimClock;
use crate::application::{AvoidanceParams, Crowd, CrowdParams, CrowdReport, SteeringRoad};
use crate::domain::spatial::{GridEntry, Neighbour, SpatialGrid, SpatialGridConfig};
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
        })
    }

    /// The steering parameters.
    #[must_use]
    pub const fn params(&self) -> SteeringParams {
        self.params
    }

    /// The spatial grid parameters.
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
/// Added automatically by [`crate::FlockPlugin`]; usable on its own when a game
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
            .insert_resource(FlockSpatialGrid::default())
            .insert_resource(FlockSteerReport::default())
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
/// Reads positions and velocities, writes the grid. Runs first in the frame so
/// steering sees the world as it is now, not as it was last tick.
pub fn rebuild_spatial_grid(
    mut grid: ResMut<FlockSpatialGrid>,
    query: Query<(&Transform, &Npc, &NpcVelocity)>,
) {
    grid.0.clear();
    for (transform, npc, velocity) in &query {
        grid.0.insert(GridEntry {
            id: npc.0,
            position: transform.translation,
            velocity: velocity.0,
        });
    }
    grid.0.build();
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
    use crate::domain::temperament::Weight;
    use crate::testkit::OrDie;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(BoidsPlugin::default());
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
}
