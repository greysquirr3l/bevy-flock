//! The crowd steering kernel: cell-major arrays instead of entities.
//!
//! The ECS path asks the spatial grid for each NPC's neighbours, which materialises a
//! `Vec<Neighbour>` per agent and *sorts it by distance*. At crowd density that sort and the
//! three separate passes over the buffer cost more than the arithmetic they feed: measured on
//! 10,000 agents at the default preset, steering was 17.0 ms of an 18.6 ms frame.
//!
//! This module is the same model with the loop inverted, which is the layout ADR 0023 chose for
//! the band:
//!
//! 1. **Agents live in arrays**, gathered once per frame and ordered by grid cell, so agents that
//!    stand near each other are adjacent in memory and share one neighbour scan.
//! 2. **One scan, not three.** Separation, alignment and cohesion accumulate in a single pass,
//!    because they read the same neighbour.
//! 3. **No sort.** Cell order is deterministic by construction; the distance sort existed to make
//!    the old path reproducible, and sums do not need it.
//!
//! The model is unchanged: [`desired_for`] is [`crate::domain::steering::desired_velocity`]'s
//! arithmetic term for term, including the priority-weighted truncation, the arrival curve and
//! the jitter floor, and a test holds the two within float noise of each other.
//!
//! The kernel is **three-dimensional**. ADR 0026 shipped it planar, with the vertical cell derived
//! once from a plane height; ADR 0028 removed that assumption, so a crowd can climb stairs, stand
//! on a balcony or fly, and the bucket ids are the grid's own on all three axes. A crowd that
//! happens to be flat pays for one extra cell layer in the scan and nothing else.
//!
//! One behaviour does change from the pre-ADR-0026 path, deliberately. The grid returns an agent
//! as its own nearest neighbour, and the old loop averaged that zero-distance self into the
//! alignment and cohesion sums, diluting both by `1/n`. The kernel excludes the agent, which is
//! what the equations always meant: a test pins the old bias so the difference stays visible
//! rather than unexplained.

#![expect(
    clippy::suboptimal_flops,
    reason = "Keep explicit multiply/add rounding for deterministic simulation arithmetic."
)]

use bevy_math::Vec3;
use bevy_tasks::{ComputeTaskPool, TaskPool};
#[cfg(feature = "simd")]
use wide::f32x4;

use crate::application::avoidance::{self, AvoidBody, AvoidReport, AvoidanceParams};
use crate::domain::num;
use crate::domain::spatial::{SpatialGrid, SpatialGridConfig};
use crate::domain::steering::{SteeringParams, accumulate};

/// The steering model, flattened for a kernel that has no ECS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrowdParams {
    /// Neighbourhood radius.
    pub radius: f32,
    /// Radius inside which agents push apart.
    pub separation: f32,
    /// Speed cap.
    pub max_speed: f32,
    /// Radius at which an agent starts slowing for its target.
    pub arrival: f32,
    /// Distance at which an agent is considered arrived.
    pub stop: f32,
    /// Speed below which the result snaps to zero.
    pub jitter: f32,
    /// Priority weights: separation, alignment, cohesion, seek, arrive.
    pub weights: [f32; 5],
}

impl CrowdParams {
    /// Take the model from the ECS configuration, so both paths steer identically.
    #[must_use]
    pub const fn from_steering(params: &SteeringParams) -> Self {
        Self {
            radius: params.neighbour_radius,
            separation: params.separation_radius,
            max_speed: params.max_speed,
            arrival: params.arrival_radius,
            stop: params.stop_radius,
            jitter: params.jitter_threshold,
            weights: [
                params.weights.separation,
                params.weights.alignment,
                params.weights.cohesion,
                params.weights.seek,
                params.weights.arrive,
            ],
        }
    }

    /// Whether the numbers can produce a sane step.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.radius > 0.0
            && self.radius.is_finite()
            && self.separation > 0.0
            && self.separation <= self.radius
            && self.max_speed > 0.0
            && self.max_speed.is_finite()
            && self.jitter >= 0.0
    }
}

/// How a frame is cut up: one thread, four lanes, or the task pool.
///
/// All three compute the same numbers — the tests hold them to bit equality — so this chooses
/// *where* the loop runs, never what it calculates. `Auto` is what ships: the pool when there
/// are agents enough to fill it, the wide road otherwise (ADR 0044).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SteeringRoad {
    /// Threads if the crowd and the pool are worth it, else the wide road, else the scalar one.
    #[default]
    Auto,
    /// The transposed four-agents-per-block kernel (ADR 0030): one thread, four lanes.
    Wide,
    /// The scalar scan, sliced across the ECS compute pool.
    Threads,
    /// The reference loop on the calling thread, where a replay debugger can follow it.
    Scalar,
}

/// What one steering pass cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CrowdReport {
    /// Agents whose desired velocity was written.
    pub steered: u32,
    /// Agents with no neighbour in range.
    pub isolated: u32,
    /// Neighbour pairs examined: the honest measure of the work done.
    pub pairs: u64,
    /// Cells that held at least one agent.
    pub cells: u32,
    /// Agents the avoidance pass adjusted. Zero when it is not configured.
    pub avoided: u32,
    /// Neighbour pairs the avoidance pass examined.
    pub avoid_pairs: u64,
}

/// Agents as arrays, bucketed by the spatial grid's cells.
///
/// Every buffer is allocated once and cleared, never freed: a steady frame does no allocator
/// work, which `tests/crowd_no_alloc.rs` holds it to.
#[derive(Debug, Clone, Default)]
pub struct Crowd {
    positions: Vec<[f32; 3]>,
    velocities: Vec<[f32; 3]>,
    targets: Vec<[f32; 3]>,
    /// Bit 0: this agent has a target. Bit 1: this agent is steered at all.
    flags: Vec<u8>,
    desired: Vec<[f32; 3]>,
    cells: Vec<u32>,
    /// Agent indices in cell-major order.
    order: Vec<u32>,
    /// First slot in `order` for a cell, and how many agents it holds.
    start: Vec<u32>,
    counts: Vec<u32>,
    /// Insertion cursor per cell while the order is being built.
    cursor: Vec<u32>,
    /// Cells written this frame; the next `begin` resets exactly these.
    touched: Vec<u32>,
    dims: [u32; 3],
    min: [f32; 3],
    cell_size: f32,
    /// Force every frame onto the calling thread. Off by default; on is what a replay
    /// debugger and a single-core CI runner want.
    sequential: bool,
    /// Where the loop runs; `Auto` unless a game asks otherwise.
    road: SteeringRoad,
    /// How much each agent yields in the avoidance pass, 1.0 by default (ADR 0045).
    priorities: Vec<f32>,
    /// Scratch for the avoidance pass, so it writes a second buffer rather than in place.
    corrected: Vec<[f32; 3]>,
    /// Per-agent multipliers on the steering weights, short when only some agents have one
    /// (ADR 0051). Empty is the fast path: the configured weights are used as they are.
    bias: Vec<[f32; 5]>,
    /// Cell-major component arrays, built by the same sort that produces `order` (ADR 0030).
    #[cfg(feature = "simd")]
    soa: Soa,
}

/// One row per agent, in cell order, so four agents of a cell load without a gather.
///
/// The kernel started with seven parallel `Vec<f32>`s, which is the textbook transposed layout:
/// four lanes gather one component each, and a broadcast neighbour reads seven separate addresses.
/// The broadcast turned out to be the hot side — the scan visits every neighbour, while the gather
/// runs once per block — so the row is the unit now. A neighbour is one bounds check and one 28-byte
/// load from a contiguous line instead of seven checks and seven lines, and `pack_soa` writes one
/// store instead of seven.
#[cfg(feature = "simd")]
#[derive(Debug, Clone, Default)]
struct Soa {
    /// Position, velocity and identity per agent, in cell-major order.
    rows: Vec<SoaRow>,
}

/// One agent's row of the transposed buffers, read out for a broadcast.
#[cfg(feature = "simd")]
#[derive(Debug, Clone, Copy, Default)]
struct SoaRow {
    /// Position on each axis.
    position: [f32; 3],
    /// Velocity on each axis.
    velocity: [f32; 3],
    /// The agent's identity, encoded as an `f32` so it can ride in a vector.
    agent: f32,
}

/// A block's four lane positions, and the identities that ride with them.
#[cfg(feature = "simd")]
#[derive(Debug, Clone, Copy)]
struct BlockLanes {
    /// X of each lane.
    px: f32x4,
    /// Y of each lane.
    py: f32x4,
    /// Z of each lane.
    pz: f32x4,
    /// Identity of each lane, as an `f32`.
    identities: f32x4,
}

/// A block's four lanes of sums, still in vectors.
#[cfg(feature = "simd")]
#[derive(Debug, Clone, Copy)]
struct BlockAccumulators {
    /// How many neighbours each lane kept.
    counts: f32x4,
    /// Sum of the neighbours' positions, per axis.
    sum_p: (f32x4, f32x4, f32x4),
    /// Sum of the neighbours' velocities, per axis.
    sum_v: (f32x4, f32x4, f32x4),
    /// Sum of the inverse-distance pushes, per axis.
    sep: (f32x4, f32x4, f32x4),
}

#[cfg(feature = "simd")]
impl BlockAccumulators {
    /// Unpack the ten vectors into four per-lane records.
    ///
    /// Literal indices, not a lane loop: each record is a register-sized value the compiler keeps
    /// in place either way, and the literal says so.
    #[inline]
    const fn lanes(&self) -> [NeighbourSums; 4] {
        let count = self.counts.to_array();
        // Tuple fields, not a destructure: three sibling bindings one letter apart are what
        // `similar_names` exists to reject, and `.0` costs nothing.
        let sum_x = self.sum_p.0.to_array();
        let sum_y = self.sum_p.1.to_array();
        let sum_z = self.sum_p.2.to_array();
        let head_x = self.sum_v.0.to_array();
        let head_y = self.sum_v.1.to_array();
        let head_z = self.sum_v.2.to_array();
        let away_x = self.sep.0.to_array();
        let away_y = self.sep.1.to_array();
        let away_z = self.sep.2.to_array();
        [
            NeighbourSums {
                count: num::f32_to_u32(count[0]),
                sum_p: [sum_x[0], sum_y[0], sum_z[0]],
                sum_v: [head_x[0], head_y[0], head_z[0]],
                sep: [away_x[0], away_y[0], away_z[0]],
            },
            NeighbourSums {
                count: num::f32_to_u32(count[1]),
                sum_p: [sum_x[1], sum_y[1], sum_z[1]],
                sum_v: [head_x[1], head_y[1], head_z[1]],
                sep: [away_x[1], away_y[1], away_z[1]],
            },
            NeighbourSums {
                count: num::f32_to_u32(count[2]),
                sum_p: [sum_x[2], sum_y[2], sum_z[2]],
                sum_v: [head_x[2], head_y[2], head_z[2]],
                sep: [away_x[2], away_y[2], away_z[2]],
            },
            NeighbourSums {
                count: num::f32_to_u32(count[3]),
                sum_p: [sum_x[3], sum_y[3], sum_z[3]],
                sum_v: [head_x[3], head_y[3], head_z[3]],
                sep: [away_x[3], away_y[3], away_z[3]],
            },
        ]
    }
}

const HAS_TARGET: u8 = 1;
const STEERS: u8 = 2;

impl Crowd {
    /// Size the buckets from the grid the ECS side uses, so both see the same neighbours.
    pub fn configure(&mut self, config: &SpatialGridConfig, cell_size: f32, dims: [u32; 3]) {
        self.dims = dims;
        self.min = [config.min.x, config.min.y, config.min.z];
        self.cell_size = cell_size;
        let cells = (dims[0] as usize) * (dims[1] as usize) * (dims[2] as usize);
        if self.start.len() != cells {
            self.start = vec![0; cells];
            self.counts = vec![0; cells];
            self.cursor = vec![0; cells];
        }
        // Occupancy is reset here rather than merely forgotten: a configuration change between
        // frames must not leave inflated counts behind, which is exactly the leak this method
        // had when it cleared the list and not the numbers it stood for.
        for cell in self.touched.drain(..) {
            if let Some(count) = self.counts.get_mut(cell as usize) {
                *count = 0;
            }
        }
    }

    /// Take the bucket geometry from a built grid.
    pub fn configure_from_grid(&mut self, grid: &SpatialGrid) {
        let config = grid.config();
        self.configure(&config, grid.effective_cell_size(), grid.dims());
    }

    /// Choose where the loop runs. `Auto` unless a game asks otherwise.
    ///
    /// Useful for reproducing a reported frame time on a fixed road, and for a host that would
    /// rather keep its cores for something else.
    pub const fn set_road(&mut self, road: SteeringRoad) {
        self.road = road;
    }

    /// The road this crowd steers on.
    #[must_use]
    pub const fn road(&self) -> SteeringRoad {
        self.road
    }

    /// Force every frame onto one thread (`true`) or let it use the pool (`false`).
    ///
    /// The result is identical either way — `a_parallel_frame_matches_the_scalar_one` holds
    /// that — so this is a diagnostic switch, not a performance one.
    pub const fn set_sequential(&mut self, sequential: bool) {
        self.sequential = sequential;
    }

    /// Whether this crowd steers on one thread.
    #[must_use]
    pub const fn is_sequential(&self) -> bool {
        self.sequential
    }

    /// How many agents are held.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.positions.len()
    }

    /// Whether the crowd is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// The cells that held an agent during the last pass: the occupancy signal.
    #[must_use]
    pub fn occupied_cells(&self) -> &[u32] {
        &self.touched
    }

    /// Forget last frame's agents, keeping the allocations.
    pub fn begin(&mut self) {
        self.reset_counts();
        self.positions.clear();
        self.velocities.clear();
        self.targets.clear();
        self.flags.clear();
        self.desired.clear();
        self.priorities.clear();
        self.bias.clear();
        self.cells.clear();
        self.order.clear();
    }

    /// Scale one agent's steering weights.
    ///
    /// The array is `[separation, alignment, cohesion, seek, arrive]`, each a multiplier centred on
    /// `1.0`; the configured weights stay the baseline, so a game's tuning still means what the
    /// documentation says. [`crate::domain::temperament::Temperament::steering_bias`] produces one
    /// from a personality, and [`crate::domain::temperament::WeightBias::to_array`] converts it.
    ///
    /// Agents without a bias use the configured weights untouched, so a crowd where nobody has a
    /// personality pays nothing for the feature. NaN and out-of-range multipliers are clamped
    /// rather than rejected: this is a per-frame setter, and a crowd that freezes because one
    /// number was wrong is worse than one that steers a little oddly for a tick.
    pub fn set_bias(&mut self, index: usize, bias: [f32; 5]) {
        let clamped = bias.map(|value| {
            if value.is_nan() {
                1.0
            } else {
                value.clamp(0.25, 4.0)
            }
        });
        if index >= self.bias.len() {
            self.bias.resize(index + 1, [1.0; 5]);
        }
        if let Some(slot) = self.bias.get_mut(index) {
            *slot = clamped;
        }
    }

    /// The bias one agent carries, or `None` when it steers with the configured weights.
    #[must_use]
    pub fn bias(&self, index: usize) -> Option<[f32; 5]> {
        self.bias.get(index).copied()
    }

    /// The weights one agent actually steers with.
    #[must_use]
    pub fn weights_for(&self, index: usize, params: &CrowdParams) -> [f32; 5] {
        resolve_weights(&self.bias, index, params)
    }

    /// Add one agent.
    ///
    /// `steers` is false for ambient-tier agents: they are neighbours to everyone else, but
    /// their own desired velocity is not this kernel's business.
    pub fn push(
        &mut self,
        position: [f32; 3],
        velocity: [f32; 3],
        target: Option<[f32; 3]>,
        steers: bool,
    ) {
        let mut flags = 0_u8;
        if target.is_some() {
            flags |= HAS_TARGET;
        }
        if steers {
            flags |= STEERS;
        }
        self.positions.push(position);
        self.velocities.push(velocity);
        self.targets.push(target.unwrap_or([0.0; 3]));
        self.flags.push(flags);
        self.desired.push([0.0; 3]);
        self.priorities.push(1.0);
        self.cells.push(self.cell_of(position));
    }

    /// The grid cell an agent at `position` falls in, by the grid's own arithmetic.
    ///
    /// Same floor-and-clamp, same axis order, same flattening as `SpatialGrid::cell_of`: the two
    /// must agree or the kernel and the ECS would disagree about who is next to whom.
    #[must_use]
    pub fn cell_of(&self, position: [f32; 3]) -> u32 {
        let x = clamp_axis((position[0] - self.min[0]) / self.cell_size, self.dims[0]);
        let y = clamp_axis((position[1] - self.min[1]) / self.cell_size, self.dims[1]);
        let z = clamp_axis((position[2] - self.min[2]) / self.cell_size, self.dims[2]);
        (z * self.dims[1] + y) * self.dims[0] + x
    }

    /// Set how much this agent yields in the avoidance pass.
    ///
    /// Above 1.0 is a guard who expects the crowd to move; below is a child who is moved around.
    /// Ignored unless the value is finite and positive, so a broken number cannot make an agent
    /// yield infinitely.
    pub fn set_priority(&mut self, index: usize, priority: f32) {
        if let Some(slot) = self.priorities.get_mut(index)
            && priority.is_finite()
            && priority > 0.0
        {
            *slot = priority;
        }
    }

    /// An agent's avoidance priority, or the default when it has not been set.
    #[must_use]
    pub fn priority(&self, index: usize, params: &AvoidanceParams) -> f32 {
        self.priorities
            .get(index)
            .copied()
            .unwrap_or(params.priority)
    }

    /// Correct the frame's desired velocities so no pair shares space (ADR 0045).
    ///
    /// Runs after [`Crowd::steer`] and reads the neighbour set through the same cell order, so it
    /// costs a second pass over pairs the grid already has in cache. The correction is applied to
    /// the desired velocity, never to the position: an agent that has already overlapped is
    /// separated over the horizon rather than snapped apart.
    pub fn avoid(&mut self, params: &AvoidanceParams) -> AvoidReport {
        match self.road {
            SteeringRoad::Scalar | SteeringRoad::Wide => self.avoid_scalar(params),
            SteeringRoad::Threads => self.avoid_parallel(params),
            // `Auto` takes the pool exactly when steering would have: a crowd too small to pay
            // for the split is a crowd whose avoidance should not pay for it either.
            SteeringRoad::Auto => {
                if pool_for(self.positions.len(), self.sequential).is_some() {
                    self.avoid_parallel(params)
                } else {
                    self.avoid_scalar(params)
                }
            }
        }
    }

    /// The avoidance pass, one agent at a time: the definition (ADR 0045).
    pub fn avoid_scalar(&mut self, params: &AvoidanceParams) -> AvoidReport {
        if !params.is_usable() || self.is_empty() {
            return AvoidReport::default();
        }
        self.order_by_cell();
        let mut report = AvoidReport::default();
        let desired = std::mem::take(&mut self.desired);
        // Written into a second buffer, never in place: an agent's neighbour must be judged on
        // the velocity it intended at the start of the pass, or whoever is processed second
        // under-corrects and walks through the first.
        let mut corrected = std::mem::take(&mut self.corrected);
        corrected.clear();
        corrected.extend_from_slice(&desired);
        {
            let view = self.view();
            let priorities = &self.priorities;
            for (index, out) in corrected.iter_mut().enumerate() {
                avoid_agent(&view, &desired, priorities, params, index, out, &mut report);
            }
        }
        self.desired = corrected;
        self.corrected = desired;
        report
    }

    /// The same pass over the ECS compute pool (ADR 0044's road, applied to avoidance).
    ///
    /// Every agent reads the pre-pass snapshot and writes only its own slot of a second buffer,
    /// so the split cannot change a float: `a_parallel_pass_matches_the_scalar_one` holds the two
    /// to bit equality. Falls back to [`Crowd::avoid_scalar`] when there is no pool to use.
    pub fn avoid_parallel(&mut self, params: &AvoidanceParams) -> AvoidReport {
        let Some(pool) = pool_for(self.positions.len(), self.sequential) else {
            return self.avoid_scalar(params);
        };
        if !params.is_usable() || self.is_empty() {
            return AvoidReport::default();
        }
        self.order_by_cell();
        let desired = std::mem::take(&mut self.desired);
        let mut corrected = std::mem::take(&mut self.corrected);
        corrected.clear();
        corrected.extend_from_slice(&desired);
        let chunk = corrected
            .len()
            .div_ceil(pool.thread_num() * 8)
            .max(MIN_PARALLEL_CHUNK);
        let priorities = &self.priorities;
        // A shared reference, not the `Vec`: each task borrows the snapshot for the length of the
        // scope, and only the slice of `corrected` it was handed is writable.
        let snapshot: &[[f32; 3]] = &desired;
        let avoid_params = *params;
        let parts = {
            let view = self.view();
            pool.scope(|scope| {
                for (chunk_index, slice) in corrected.chunks_mut(chunk).enumerate() {
                    let first = chunk_index * chunk;
                    scope.spawn(async move {
                        let mut report = AvoidReport::default();
                        for (offset, out) in slice.iter_mut().enumerate() {
                            avoid_agent(
                                &view,
                                snapshot,
                                priorities,
                                &avoid_params,
                                first + offset,
                                out,
                                &mut report,
                            );
                        }
                        report
                    });
                }
            })
        };
        self.desired = corrected;
        self.corrected = desired;
        let mut report = AvoidReport::default();
        for part in parts {
            report.pairs += part.pairs;
            report.adjusted += part.adjusted;
        }
        report
    }

    /// The desired velocity written for an agent, if it was steered.
    #[must_use]
    pub fn desired(&self, index: usize) -> Option<[f32; 3]> {
        if self.flags.get(index).copied().unwrap_or(0) & STEERS == 0 {
            return None;
        }
        self.desired.get(index).copied()
    }

    /// Steer everyone, in one pass over the cells.
    ///
    /// Returns the report; occupancy is available from [`Crowd::occupied_cells`] afterwards.
    /// Without `simd` this spreads the pass over the ECS task pool when there is one.
    pub fn steer(&mut self, params: &CrowdParams) -> CrowdReport {
        match self.road {
            SteeringRoad::Scalar => self.steer_scalar(params),
            SteeringRoad::Threads => self.steer_parallel(params),
            SteeringRoad::Wide => self.steer_wide(params),
            SteeringRoad::Auto => {
                if pool_for(self.positions.len(), self.sequential).is_some() {
                    self.steer_parallel(params)
                } else {
                    self.steer_wide(params)
                }
            }
        }
    }

    /// The wide road when `simd` is on, the scalar one otherwise: `Auto` never has to know
    /// which features the crate was built with.
    fn steer_wide(&mut self, params: &CrowdParams) -> CrowdReport {
        #[cfg(feature = "simd")]
        {
            self.steer_blocks(params)
        }
        #[cfg(not(feature = "simd"))]
        {
            self.steer_scalar(params)
        }
    }

    /// The scalar road: one agent at a time, the reference implementation (ADR 0026).
    ///
    /// This is the definition of the arithmetic. Everything else — the wide road, the threaded
    /// road — is this loop rearranged, and the tests hold them to bit equality with it.
    pub fn steer_scalar(&mut self, params: &CrowdParams) -> CrowdReport {
        if !params.is_usable() || self.is_empty() {
            return CrowdReport::default();
        }
        self.order_by_cell();
        let mut desired = std::mem::take(&mut self.desired);
        let mut report = CrowdReport::default();
        {
            let view = self.view();
            for (index, out) in desired.iter_mut().enumerate() {
                steer_agent(&view, out, index, params, &mut report);
            }
        }
        self.desired = desired;
        report.cells = u32::try_from(self.touched.len()).unwrap_or(u32::MAX);
        report
    }

    /// The same frame, cut into slices and shared across the ECS compute pool (ADR 0044).
    ///
    /// Every agent reads a snapshot and writes only its own `desired` slot, so splitting the
    /// loop cannot change a float: `a_parallel_frame_matches_the_scalar_one` asserts the two
    /// agree bit for bit at every population the benchmark uses. Falls back to
    /// [`Crowd::steer_scalar`] when there is no pool, one thread, or a crowd too small to pay
    /// for the split.
    pub fn steer_parallel(&mut self, params: &CrowdParams) -> CrowdReport {
        let Some(pool) = pool_for(self.positions.len(), self.sequential) else {
            return self.steer_scalar(params);
        };
        if !params.is_usable() || self.is_empty() {
            return CrowdReport::default();
        }
        self.order_by_cell();
        let mut desired = std::mem::take(&mut self.desired);
        // Eight slices per thread and never fewer than 512 agents each: enough slices that one
        // bucket holding a whole village cannot become the critical path, and few enough that
        // the pool's own bookkeeping stays noise.
        let chunk = desired
            .len()
            .div_ceil(pool.thread_num() * 8)
            .max(MIN_PARALLEL_CHUNK);
        let parts = {
            let view = self.view();
            pool.scope(|scope| {
                for (chunk_index, slice) in desired.chunks_mut(chunk).enumerate() {
                    let first = chunk_index * chunk;
                    scope.spawn(async move {
                        let mut report = CrowdReport::default();
                        for (offset, out) in slice.iter_mut().enumerate() {
                            steer_agent(&view, out, first + offset, params, &mut report);
                        }
                        report
                    });
                }
            })
        };
        self.desired = desired;
        let mut report = CrowdReport::default();
        for part in parts {
            report.steered += part.steered;
            report.isolated += part.isolated;
            report.pairs += part.pairs;
        }
        report.cells = u32::try_from(self.touched.len()).unwrap_or(u32::MAX);
        report
    }

    /// Zero the counts of every cell written last frame.
    ///
    /// Split out because both `begin` and `order_by_cell` need it: the order is rebuilt by the
    /// avoidance pass as well as by `steer`, and a rebuild that did not zero first would double
    /// every count and put each agent in the list twice.
    fn reset_counts(&mut self) {
        for cell in self.touched.drain(..) {
            if let Some(count) = self.counts.get_mut(cell as usize) {
                *count = 0;
            }
        }
    }

    /// Counting sort of agent indices into cell order, touching only occupied cells.
    fn order_by_cell(&mut self) {
        self.reset_counts();
        self.touched.clear();
        for cell in &self.cells {
            let index = *cell as usize;
            if let Some(count) = self.counts.get_mut(index) {
                if *count == 0 {
                    self.touched.push(*cell);
                }
                *count += 1;
            }
        }
        // The order of the blocks has to be a property of the world, not of arrival order, or
        // two identical runs could steer in different orders.
        self.touched.sort_unstable();

        self.order.resize(self.cells.len(), 0);
        let mut offset = 0_u32;
        for cell in &self.touched {
            let index = *cell as usize;
            let count = self.counts.get(index).copied().unwrap_or(0);
            if let Some(slot) = self.start.get_mut(index) {
                *slot = offset;
            }
            if let Some(cursor) = self.cursor.get_mut(index) {
                *cursor = offset;
            }
            offset += count;
        }
        for (agent, cell) in self.cells.iter().enumerate() {
            let index = *cell as usize;
            let Some(cursor) = self.cursor.get_mut(index) else {
                continue;
            };
            let slot = *cursor;
            *cursor += 1;
            if let Some(target) = self.order.get_mut(slot as usize) {
                *target = u32::try_from(agent).unwrap_or(u32::MAX);
            }
        }
        // Only now is `order` meaningful, so only now can the component arrays be filled.
        #[cfg(feature = "simd")]
        self.pack_soa();
    }

    /// Copy the crowd into cell-major rows.
    #[cfg(feature = "simd")]
    fn pack_soa(&mut self) {
        let len = self.positions.len();
        self.soa.rows.clear();
        self.soa.rows.resize(len, SoaRow::default());
        for (slot, row) in self.soa.rows.iter_mut().enumerate() {
            let agent = self.order.get(slot).copied().unwrap_or(0) as usize;
            let position = self.positions.get(agent).copied().unwrap_or([0.0; 3]);
            let velocity = self.velocities.get(agent).copied().unwrap_or([0.0; 3]);
            *row = SoaRow {
                position,
                velocity,
                agent: num::usize_to_f32(agent),
            };
        }
    }

    /// The transposed road: lanes are agents, one neighbour is broadcast (ADR 0030).
    ///
    /// Four agents that share a cell fill four lanes; every neighbour in that cell's 3×3×3
    /// neighbourhood is broadcast into all four, so one load feeds four agents that are all doing
    /// real work. Buckets that cannot fill a block fall back to the scalar scan, because a masked
    /// partial block costs more than it saves at that size.
    #[cfg(feature = "simd")]
    fn steer_blocks(&mut self, params: &CrowdParams) -> CrowdReport {
        let mut report = CrowdReport::default();
        if !params.is_usable() || self.is_empty() {
            return report;
        }
        self.order_by_cell();
        let radius_squared = params.radius * params.radius;
        let separation_squared = params.separation * params.separation;
        let dims = [
            i64::from(self.dims[0]),
            i64::from(self.dims[1]),
            i64::from(self.dims[2]),
        ];
        for bucket in 0..self.touched.len() {
            let cell = self.touched.get(bucket).copied().unwrap_or(0) as usize;
            let start = self.start.get(cell).copied().unwrap_or(0) as usize;
            let count = self.counts.get(cell).copied().unwrap_or(0) as usize;
            let mut slot = start;
            while slot + 4 <= start + count {
                // Four agents of this cell fill four lanes. If any of them is ambient-tier the
                // block is masked anyway, so the four go down the scalar road instead.
                let all_steer = (0..4).all(|lane| {
                    let agent = self.order.get(slot + lane).copied().unwrap_or(0) as usize;
                    self.flags.get(agent).copied().unwrap_or(0) & STEERS != 0
                });
                if all_steer {
                    let home = num::usize_to_u32(cell);
                    self.steer_block(
                        slot,
                        home,
                        params,
                        (radius_squared, separation_squared),
                        dims,
                        &mut report,
                    );
                } else {
                    for lane in 0..4 {
                        let agent = self.order.get(slot + lane).copied().unwrap_or(0) as usize;
                        self.scalar_agent(agent, params, &mut report);
                    }
                }
                slot += 4;
            }
            while slot < start + count {
                let agent = self.order.get(slot).copied().unwrap_or(0) as usize;
                slot += 1;
                self.scalar_agent(agent, params, &mut report);
            }
        }
        report.cells = u32::try_from(self.touched.len()).unwrap_or(u32::MAX);
        report
    }

    /// One agent, the scalar way: the tail of a bucket, and any block that cannot fill lanes.
    #[cfg(feature = "simd")]
    fn scalar_agent(&mut self, agent: usize, params: &CrowdParams, report: &mut CrowdReport) {
        if self.flags.get(agent).copied().unwrap_or(0) & STEERS == 0 {
            return;
        }
        let (count, sum_p, sum_v, sep, pairs) = self.scan_reference(agent, params);
        report.pairs += pairs;
        if count == 0 {
            report.isolated += 1;
        }
        let target = self.target(agent);
        let (Some(position), Some(velocity)) = (
            self.positions.get(agent).copied(),
            self.velocities.get(agent).copied(),
        ) else {
            return;
        };
        let steered = desired_for(
            position,
            velocity,
            target,
            NeighbourSums {
                count,
                sum_p,
                sum_v,
                sep,
            },
            params,
            resolve_weights(&self.bias, agent, params),
        );
        if let Some(slot) = self.desired.get_mut(agent) {
            *slot = steered;
        }
        report.steered += 1;
    }

    /// One block of four agents that share a cell: the scan, then the four write-backs.
    ///
    /// Both halves live here on purpose. Splitting the scan into its own function meant returning
    /// ten vectors through the ABI, which is a stack round-trip per block and measured 19% slower
    /// on a dense crowd; as locals they stay in registers.
    #[cfg(feature = "simd")]
    fn steer_block(
        &mut self,
        slot: usize,
        home: u32,
        params: &CrowdParams,
        squares: (f32, f32),
        dims: [i64; 3],
        report: &mut CrowdReport,
    ) {
        let (radius_squared, separation_squared) = squares;
        let block = self.block_lanes(slot);
        let (px, py, pz) = (block.px, block.py, block.pz);
        let identities = block.identities;
        let radius = f32x4::splat(radius_squared);
        let separation = f32x4::splat(separation_squared);
        let epsilon = f32x4::splat(1e-6);
        let one = f32x4::splat(1.0);
        let zero = f32x4::splat(0.0);
        let mut counts = zero;
        let mut sum_p = (zero, zero, zero);
        let mut sum_v = (zero, zero, zero);
        let mut sep = (zero, zero, zero);

        // Every lane is in this cell, so anything within the radius is in the cell or one of its
        // 26 neighbours — the box the band kernel walks.
        let home = home as usize;
        let origin = (
            num::usize_to_i64(home % self.dims[0] as usize),
            num::usize_to_i64(home / self.dims[0] as usize % self.dims[1] as usize),
            num::usize_to_i64(home / (self.dims[0] * self.dims[1]) as usize),
        );
        for z in (origin.2 - 1).max(0)..=(origin.2 + 1).min(dims[2] - 1) {
            for y in (origin.1 - 1).max(0)..=(origin.1 + 1).min(dims[1] - 1) {
                for x in (origin.0 - 1).max(0)..=(origin.0 + 1).min(dims[0] - 1) {
                    let at = num::i64_to_usize((z * dims[1] + y) * dims[0] + x);
                    let begin = self.start.get(at).copied().unwrap_or(0) as usize;
                    let end = begin + self.counts.get(at).copied().unwrap_or(0) as usize;
                    // One bound for the whole cell: the range comes from a bucket, and slicing
                    // it once means the neighbour loop below has no check in it at all — where a
                    // per-neighbour `get` measured 19% slower on a dense crowd.
                    let Some(rows) = self.soa.rows.get(begin..end) else {
                        continue;
                    };
                    for row in rows {
                        let ox = f32x4::splat(row.position[0]);
                        let oy = f32x4::splat(row.position[1]);
                        let oz = f32x4::splat(row.position[2]);
                        let dx = px - ox;
                        let dy = py - oy;
                        let dz = pz - oz;
                        let distance_squared = dx * dx + dy * dy + dz * dz;
                        let keep = distance_squared.simd_le(radius)
                            & f32x4::splat(row.agent).simd_ne(identities);
                        if !keep.any() {
                            continue;
                        }
                        let weight = keep.select(one, zero);
                        counts += weight;
                        sum_p.0 += ox * weight;
                        sum_p.1 += oy * weight;
                        sum_p.2 += oz * weight;
                        sum_v.0 += f32x4::splat(row.velocity[0]) * weight;
                        sum_v.1 += f32x4::splat(row.velocity[1]) * weight;
                        sum_v.2 += f32x4::splat(row.velocity[2]) * weight;
                        let close = keep & distance_squared.simd_lt(separation);
                        if close.any() {
                            let push = close.select(one, zero) / (distance_squared + epsilon);
                            sep.0 += dx * push;
                            sep.1 += dy * push;
                            sep.2 += dz * push;
                        }
                    }
                }
            }
        }

        let lanes = BlockAccumulators {
            counts,
            sum_p,
            sum_v,
            sep,
        }
        .lanes();
        for (lane_index, sums) in lanes.iter().enumerate() {
            let agent = self.order.get(slot + lane_index).copied().unwrap_or(0) as usize;
            report.pairs += u64::from(sums.count);
            if sums.count == 0 {
                report.isolated += 1;
            }
            let target = self.target(agent);
            let (Some(position), Some(velocity)) = (
                self.positions.get(agent).copied(),
                self.velocities.get(agent).copied(),
            ) else {
                continue;
            };
            let weights = resolve_weights(&self.bias, agent, params);
            let steered = desired_for(position, velocity, target, *sums, params, weights);
            if let Some(out) = self.desired.get_mut(agent) {
                *out = steered;
            }
            report.steered += 1;
        }
    }

    /// The four lanes of a block: their positions and identities, gathered once.
    #[cfg(feature = "simd")]
    #[inline]
    fn block_lanes(&self, slot: usize) -> BlockLanes {
        let row_at = |index: usize| self.soa.rows.get(index).copied().unwrap_or_default();
        let first_row = row_at(slot);
        let second_row = row_at(slot + 1);
        let third_row = row_at(slot + 2);
        let fourth_row = row_at(slot + 3);
        BlockLanes {
            px: f32x4::from([
                first_row.position[0],
                second_row.position[0],
                third_row.position[0],
                fourth_row.position[0],
            ]),
            py: f32x4::from([
                first_row.position[1],
                second_row.position[1],
                third_row.position[1],
                fourth_row.position[1],
            ]),
            pz: f32x4::from([
                first_row.position[2],
                second_row.position[2],
                third_row.position[2],
                fourth_row.position[2],
            ]),
            identities: f32x4::from([
                first_row.agent,
                second_row.agent,
                third_row.agent,
                fourth_row.agent,
            ]),
        }
    }

    /// The agent's target, if it has one.
    #[cfg(feature = "simd")]
    fn target(&self, agent: usize) -> Option<[f32; 3]> {
        if self.flags.get(agent).copied().unwrap_or(0) & HAS_TARGET == 0 {
            None
        } else {
            self.targets.get(agent).copied()
        }
    }

    /// The scalar scan, for the wide road's tails: the only caller is `simd`'s `scalar_agent`.
    #[cfg(feature = "simd")]
    fn scan_reference(
        &self,
        index: usize,
        params: &CrowdParams,
    ) -> (u32, [f32; 3], [f32; 3], [f32; 3], u64) {
        let scan = self.view().scan(index, params);
        (scan.count, scan.sum_p, scan.sum_v, scan.sep, scan.pairs)
    }

    /// The read-only halves of a steer pass, borrowed together so a chunk of agents can be
    /// handed to a thread without borrowing the buffer it writes into.
    fn view(&self) -> ScanView<'_> {
        ScanView {
            positions: &self.positions,
            velocities: &self.velocities,
            targets: &self.targets,
            flags: &self.flags,
            order: &self.order,
            start: &self.start,
            counts: &self.counts,
            bias: &self.bias,
            dims: self.dims,
            min: self.min,
            cell_size: self.cell_size,
        }
    }
}

/// What one agent's scan collected, before the per-agent arithmetic.
#[derive(Clone, Copy, Debug, Default)]
struct Scan {
    count: u32,
    sum_p: [f32; 3],
    sum_v: [f32; 3],
    sep: [f32; 3],
    pairs: u64,
}

/// Everything a scan reads, and nothing it writes.
///
/// Copy, because every thread needs its own handle to the same borrows.
#[derive(Clone, Copy)]
struct ScanView<'a> {
    positions: &'a [[f32; 3]],
    velocities: &'a [[f32; 3]],
    targets: &'a [[f32; 3]],
    flags: &'a [u8],
    order: &'a [u32],
    start: &'a [u32],
    counts: &'a [u32],
    bias: &'a [[f32; 5]],
    dims: [u32; 3],
    min: [f32; 3],
    cell_size: f32,
}

impl ScanView<'_> {
    /// Every neighbour of `index` inside the radius, summed in cell order.
    ///
    /// The arithmetic here is the domain model's, unchanged since ADR 0026: the same
    /// `distance_squared > radius_squared` rejection, the same separation falloff, the same
    /// accumulation order. Optimising this function means changing the definition of the
    /// simulation, which is why the wins elsewhere are about *when* and *where* it runs.
    fn scan(&self, index: usize, params: &CrowdParams) -> Scan {
        let mut scan = Scan::default();
        let Some(&position) = self.positions.get(index) else {
            return scan;
        };
        let radius_squared = params.radius * params.radius;
        let separation_squared = params.separation * params.separation;
        let window = self.window(position, params.radius);
        let dim_x = i64::from(self.dims[0]);
        let dim_y = i64::from(self.dims[1]);

        for z in window.z0..=window.z1 {
            let plane = (z * dim_y) * dim_x;
            for y in window.y0..=window.y1 {
                let row = plane + y * dim_x;
                for x in window.x0..=window.x1 {
                    let at = u32::try_from(row + x).unwrap_or(u32::MAX);
                    let Some(&start) = self.start.get(at as usize) else {
                        continue;
                    };
                    let length = self.counts.get(at as usize).copied().unwrap_or(0);
                    for slot in start..start + length {
                        let Some(&other) = self.order.get(slot as usize) else {
                            continue;
                        };
                        let other = other as usize;
                        if other == index {
                            continue;
                        }
                        let Some(&other_position) = self.positions.get(other) else {
                            continue;
                        };
                        let dx = position[0] - other_position[0];
                        let dy = position[1] - other_position[1];
                        let dz = position[2] - other_position[2];
                        let distance_squared = dz * dz + (dy * dy + dx * dx);
                        if distance_squared > radius_squared {
                            continue;
                        }
                        scan.pairs += 1;
                        scan.count += 1;
                        scan.sum_p[0] += other_position[0];
                        scan.sum_p[1] += other_position[1];
                        scan.sum_p[2] += other_position[2];
                        if let Some(&other_velocity) = self.velocities.get(other) {
                            scan.sum_v[0] += other_velocity[0];
                            scan.sum_v[1] += other_velocity[1];
                            scan.sum_v[2] += other_velocity[2];
                        }
                        if distance_squared < separation_squared && distance_squared > f32::EPSILON
                        {
                            let scale = 1.0 / distance_squared.max(1e-6);
                            scan.sep[0] += dx * scale;
                            scan.sep[1] += dy * scale;
                            scan.sep[2] += dz * scale;
                        }
                    }
                }
            }
        }
        scan
    }

    /// Call `visit` for every agent within `radius` of `position`, in cell order.
    ///
    /// The same window arithmetic as [`ScanView::scan`], with the accumulate left to the caller:
    /// the avoidance pass wants neighbours one at a time, not their sums, and duplicating the
    /// window walk keeps each loop readable and inlined.
    fn for_each_neighbour(
        &self,
        index: usize,
        position: [f32; 3],
        radius: f32,
        mut visit: impl FnMut(usize),
    ) {
        let radius_squared = radius * radius;
        let window = self.window(position, radius);
        let dim_x = i64::from(self.dims[0]);
        let dim_y = i64::from(self.dims[1]);
        for z in window.z0..=window.z1 {
            let plane = (z * dim_y) * dim_x;
            for y in window.y0..=window.y1 {
                let row = plane + y * dim_x;
                for x in window.x0..=window.x1 {
                    let at = u32::try_from(row + x).unwrap_or(u32::MAX);
                    let Some(&start) = self.start.get(at as usize) else {
                        continue;
                    };
                    let length = self.counts.get(at as usize).copied().unwrap_or(0);
                    for slot in start..start + length {
                        let Some(&other) = self.order.get(slot as usize) else {
                            continue;
                        };
                        let other = other as usize;
                        if other == index {
                            continue;
                        }
                        let Some(&other_position) = self.positions.get(other) else {
                            continue;
                        };
                        let dx = position[0] - other_position[0];
                        let dy = position[1] - other_position[1];
                        let dz = position[2] - other_position[2];
                        if dz * dz + (dy * dy + dx * dx) > radius_squared {
                            continue;
                        }
                        visit(other);
                    }
                }
            }
        }
    }

    /// The inclusive cell range that can hold anything within `radius` of `position`.
    ///
    /// One division per axis, not two: the previous version evaluated `axis_window` twice for
    /// each axis to take `.0` and `.1` from separate calls.
    fn window(&self, position: [f32; 3], radius: f32) -> Window {
        let (x0, x1) = axis_window(
            position[0],
            radius,
            self.min[0],
            self.cell_size,
            self.dims[0],
        );
        let (y0, y1) = axis_window(
            position[1],
            radius,
            self.min[1],
            self.cell_size,
            self.dims[1],
        );
        let (z0, z1) = axis_window(
            position[2],
            radius,
            self.min[2],
            self.cell_size,
            self.dims[2],
        );
        Window {
            x0,
            x1,
            y0,
            y1,
            z0,
            z1,
        }
    }
}

/// What an agent intends to do this tick: its desired velocity if it steers, else its own.
///
/// Avoidance reasons about intent, not about history: an agent that has decided to walk left
/// should be predicted walking left, and an ambient agent that is simply standing there should be
/// predicted standing there.
/// The whole per-agent avoidance step, so both roads call the same code.
///
/// Reads only the pre-pass snapshot in `desired` and writes only `out`, which is what makes the
/// pass safe to split across threads and bit-identical when it is.
fn avoid_agent(
    view: &ScanView<'_>,
    desired: &[[f32; 3]],
    priorities: &[f32],
    params: &AvoidanceParams,
    index: usize,
    out: &mut [f32; 3],
    report: &mut AvoidReport,
) {
    let flags = view.flags.get(index).copied().unwrap_or(0);
    if flags & STEERS == 0 {
        return;
    }
    let Some(&position) = view.positions.get(index) else {
        return;
    };
    let velocity = intended(view, desired, index);
    let priority = priorities.get(index).copied().unwrap_or(params.priority);
    let reach = params.max_adjust * params.horizon + params.floor();
    let mut correction = [0.0_f32; 3];
    let mut touched = false;
    view.for_each_neighbour(index, position, reach, |other| {
        let other_position = view.positions.get(other).copied().unwrap_or(position);
        let other_velocity = intended(view, desired, other);
        let other_priority = priorities.get(other).copied().unwrap_or(params.priority);
        report.pairs += 1;
        touched |= avoidance::solve_pair(
            params,
            AvoidBody::new(position, velocity).with_priority(priority),
            AvoidBody::new(other_position, other_velocity).with_priority(other_priority),
            &mut correction,
        );
    });
    if touched {
        *out = avoidance::apply(
            params,
            desired.get(index).copied().unwrap_or(*out),
            correction,
        );
        report.adjusted += 1;
    }
}

fn intended(view: &ScanView<'_>, desired: &[[f32; 3]], index: usize) -> [f32; 3] {
    let flags = view.flags.get(index).copied().unwrap_or(0);
    if flags & STEERS == 0 {
        return view.velocities.get(index).copied().unwrap_or([0.0; 3]);
    }
    desired
        .get(index)
        .copied()
        .or_else(|| view.velocities.get(index).copied())
        .unwrap_or([0.0; 3])
}

/// The weights one agent steers with: the configured set, scaled by its bias if it has one.
///
/// Split out of the three call sites so the scalar tail, the block kernel and the threaded chunks
/// cannot drift apart — a bias that only applied on one road would be two different crowds.
fn resolve_weights(bias: &[[f32; 5]], index: usize, params: &CrowdParams) -> [f32; 5] {
    bias.get(index).map_or(params.weights, |scales| {
        let mut weights = params.weights;
        for (weight, scale) in weights.iter_mut().zip(*scales) {
            *weight *= scale;
        }
        weights
    })
}

/// Steer one agent into `out`: the whole per-agent step, so both roads call the same code.
fn steer_agent(
    view: &ScanView<'_>,
    out: &mut [f32; 3],
    index: usize,
    params: &CrowdParams,
    report: &mut CrowdReport,
) {
    let flags = view.flags.get(index).copied().unwrap_or(0);
    if flags & STEERS == 0 {
        return;
    }
    let Some(&position) = view.positions.get(index) else {
        return;
    };
    let Some(&velocity) = view.velocities.get(index) else {
        return;
    };
    let scan = view.scan(index, params);
    report.pairs += scan.pairs;
    if scan.count == 0 {
        report.isolated += 1;
    }
    let target = if flags & HAS_TARGET == 0 {
        None
    } else {
        view.targets.get(index).copied()
    };
    *out = desired_for(
        position,
        velocity,
        target,
        NeighbourSums {
            count: scan.count,
            sum_p: scan.sum_p,
            sum_v: scan.sum_v,
            sep: scan.sep,
        },
        params,
        resolve_weights(view.bias, index, params),
    );
    report.steered += 1;
}

/// Below this many agents a frame stays on one thread: the split costs more than it saves.
const MIN_PARALLEL_AGENTS: usize = 1_024;

/// Never hand out slices smaller than this, so the pool is not paid per handful.
const MIN_PARALLEL_CHUNK: usize = 512;

/// The pool to split a frame across, or `None` to keep it on the calling thread.
///
/// `try_get` rather than `get`: a bare `App` without `TaskPoolPlugin` (every unit test, and any
/// server that builds the world by hand) must fall back to the scalar road instead of panicking.
fn pool_for(agents: usize, sequential: bool) -> Option<&'static ComputeTaskPool> {
    if sequential || agents < MIN_PARALLEL_AGENTS {
        return None;
    }
    // A host that added `DefaultPlugins` already owns this pool and we simply borrow it. A
    // headless host that added only `FlockPlugin` gets the same pool created here, with the
    // same default shape, on the first frame big enough to be worth the threads. Creating it
    // lazily rather than inserting `TaskPoolPlugin` means the plugin cannot collide with a
    // host that adds `DefaultPlugins` afterwards.
    let pool = ComputeTaskPool::try_get()
        .unwrap_or_else(|| ComputeTaskPool::get_or_init(TaskPool::default));
    (pool.thread_num() > 1).then_some(pool)
}

/// An inclusive cell range on each axis.
struct Window {
    x0: i64,
    x1: i64,
    y0: i64,
    y1: i64,
    z0: i64,
    z1: i64,
}

/// The cells on one axis that a sphere of `radius` around `value` can reach.
fn axis_window(value: f32, radius: f32, min: f32, cell_size: f32, dim: u32) -> (i64, i64) {
    (
        i64::from(clamp_axis((value - radius - min) / cell_size, dim)),
        i64::from(clamp_axis((value + radius - min) / cell_size, dim)),
    )
}

/// What one neighbour scan collected, in the form the arithmetic wants it.
///
/// The four values travel together — they are one scan's output — so they are one argument rather
/// than four, which also keeps the kernel's call sites readable.
#[derive(Clone, Copy, Default)]
struct NeighbourSums {
    /// Neighbours counted, saturated.
    count: u32,
    /// Summed neighbour positions.
    sum_p: [f32; 3],
    /// Summed neighbour velocities (the alignment term's numerator).
    sum_v: [f32; 3],
    /// The separation force, already negated.
    sep: [f32; 3],
}

/// The per-agent arithmetic: the domain model, over sums the scan already collected.
fn desired_for(
    position: [f32; 3],
    velocity: [f32; 3],
    target: Option<[f32; 3]>,
    sums: NeighbourSums,
    params: &CrowdParams,
    weights: [f32; 5],
) -> [f32; 3] {
    let NeighbourSums {
        count,
        sum_p,
        sum_v,
        sep,
    } = sums;
    let max_speed = params.max_speed;
    let here = Vec3::new(position[0], position[1], position[2]);
    let mut steering = accumulate(
        Vec3::ZERO,
        Vec3::new(sep[0], sep[1], sep[2]),
        weights[0],
        max_speed,
    );
    if count > 0 {
        let scaled = num::u32_to_f32(count);
        let average = Vec3::new(sum_v[0], sum_v[1], sum_v[2]) / scaled;
        steering = accumulate(
            steering,
            average - Vec3::new(velocity[0], velocity[1], velocity[2]),
            weights[1],
            max_speed,
        );
        let centre = Vec3::new(sum_p[0], sum_p[1], sum_p[2]) / scaled;
        steering = accumulate(steering, centre - here, weights[2], max_speed);
    }
    if let Some(target) = target {
        let offset = Vec3::new(target[0], target[1], target[2]) - here;
        let distance = offset.length();
        if distance <= params.stop {
            return [0.0; 3];
        }
        if distance <= params.arrival {
            let scale = distance / params.arrival;
            let arrival = offset.normalize_or_zero() * (max_speed * scale);
            steering = accumulate(steering, arrival, weights[4], max_speed);
        } else {
            let seek = offset.normalize_or_zero() * max_speed;
            steering = accumulate(steering, seek, weights[3], max_speed);
        }
    }
    if steering.length_squared() < params.jitter * params.jitter {
        return [0.0; 3];
    }
    let clamped = steering.clamp_length_max(max_speed);
    [clamped.x, clamped.y, clamped.z]
}

fn clamp_axis(value: f32, dim: u32) -> u32 {
    let last = dim.saturating_sub(1);
    let index = value.floor();
    if index < 0.0 {
        return 0;
    }
    let index = num::f32_to_u32(index);
    index.min(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::NpcId;
    use crate::domain::spatial::{GridEntry, Neighbour};
    use crate::domain::steering::{AgentState, desired_velocity};
    use crate::testkit::OrDie;

    fn params() -> CrowdParams {
        CrowdParams::from_steering(&SteeringParams::human_scale())
    }

    /// One agent's fixture data: position, velocity, optional target.
    type Agent = ([f32; 3], [f32; 3], Option<[f32; 3]>);

    /// A crowd that is genuinely three-dimensional: a spiral in `xz` with a slow sine in `y`, so
    /// vertical neighbours exist and a planar kernel would fail here rather than pass by accident.
    fn fixture(count: u32) -> (SpatialGrid, Vec<Agent>) {
        let grid = SpatialGrid::new(SpatialGridConfig::default()).or_die();
        let mut points = Vec::new();
        for index in 0..count {
            let step = num::u32_to_f32(index);
            let ring = 1.6 * libm::sqrtf(step);
            let angle = step * 2.399_963_2;
            let position = [
                libm::cosf(angle) * ring,
                libm::sinf(step * 0.7) * 6.0,
                libm::sinf(angle) * ring,
            ];
            let velocity = [libm::cosf(step) * 0.4, 0.0, libm::sinf(step) * 0.4];
            let target = if index % 3 == 0 {
                Some([20.0, 4.0, -15.0])
            } else {
                None
            };
            points.push((position, velocity, target));
        }
        (grid, points)
    }

    fn packed(grid: &SpatialGrid, points: &[Agent]) -> Crowd {
        let mut crowd = Crowd::default();
        crowd.configure_from_grid(grid);
        crowd.begin();
        for (position, velocity, target) in points {
            crowd.push(*position, *velocity, *target, true);
        }
        crowd
    }

    fn vec3(point: [f32; 3]) -> Vec3 {
        Vec3::new(point[0], point[1], point[2])
    }

    /// Walk a pair of agents through each other and return the closest they came, in metres.
    ///
    /// The targets are behind each other, so steering alone drives them straight through: this is
    /// the doorway case, and the only thing that can keep them apart is the avoidance pass.
    fn head_on_run(avoid: bool) -> (f32, [f32; 3], [f32; 3], u64) {
        let grid = SpatialGrid::new(SpatialGridConfig::default()).or_die();
        // Separation weight off, so the soft force cannot keep them apart and the fixture models
        // the case avoidance exists for: a doorway where pushing is outmatched by the traffic.
        let mut params = params();
        params.weights[0] = 0.0;
        let avoidance = AvoidanceParams::new()
            .with_separation(0.7)
            .with_horizon(0.8);
        let mut left = [-6.0_f32, 0.0, 0.0];
        let mut right = [6.0_f32, 0.0, 0.0];
        let mut left_velocity = [0.0_f32; 3];
        let mut right_velocity = [0.0_f32; 3];
        let step = 1.0 / 60.0;
        let mut closest = f32::MAX;
        let mut pairs = 0_u64;
        for _ in 0..360 {
            let mut crowd = Crowd::default();
            crowd.configure_from_grid(&grid);
            crowd.begin();
            crowd.push(left, left_velocity, Some([9.0, 0.0, 0.0]), true);
            crowd.push(right, right_velocity, Some([-9.0, 0.0, 0.0]), true);
            let report = crowd.steer(&params);
            assert_eq!(report.steered, 2);
            if avoid {
                // Only pairs inside the pass's reach are examined, so the count is summed over
                // the approach rather than asserted on the first tick, when they are 12 m apart.
                let rep = crowd.avoid(&avoidance);
                pairs += rep.pairs;
            }
            let desired_left = crowd.desired(0).unwrap_or([0.0; 3]);
            let desired_right = crowd.desired(1).unwrap_or([0.0; 3]);
            left_velocity = desired_left;
            right_velocity = desired_right;
            left[0] += desired_left[0] * step;
            right[0] += desired_right[0] * step;
            let dx = left[0] - right[0];
            let dy = left[1] - right[1];
            let dz = left[2] - right[2];
            closest = closest.min(libm::sqrtf(dx * dx + dy * dy + dz * dz));
        }
        (closest, left, right, pairs)
    }

    #[test]
    fn avoidance_keeps_two_agents_from_sharing_space() {
        let (without, _, _, _) = head_on_run(false);
        let (with, left, right, pairs) = head_on_run(true);
        assert!(
            pairs > 0,
            "the pass must have examined pairs during the approach"
        );
        assert!(
            without < 0.7,
            "the fixture must overlap without avoidance, else it proves nothing (got {without})"
        );
        assert!(
            with >= 0.7,
            "the separation floor must hold for the whole approach (closest {with})"
        );
        assert!(
            left[0] < right[0],
            "they must not have passed through each other"
        );
    }

    #[test]
    fn avoidance_is_bit_stable_across_runs() {
        let (first, left_a, right_a, _) = head_on_run(true);
        let (second, left_b, right_b, _) = head_on_run(true);
        assert_eq!(first.to_bits(), second.to_bits());
        assert_eq!(left_a.map(f32::to_bits), left_b.map(f32::to_bits));
        assert_eq!(right_a.map(f32::to_bits), right_b.map(f32::to_bits));
    }

    #[test]
    fn an_unusable_avoidance_config_does_nothing() {
        let grid = SpatialGrid::new(SpatialGridConfig::default()).or_die();
        let mut crowd = packed(&grid, &[([0.0, 0.0, 0.0], [0.0; 3], None)]);
        let report = crowd.avoid(&AvoidanceParams::new().with_separation(0.0));
        assert_eq!(report, AvoidReport::default());
    }

    #[test]
    fn ambient_agents_are_avoided_but_do_not_avoid() {
        let grid = SpatialGrid::new(SpatialGridConfig::default()).or_die();
        let mut crowd = Crowd::default();
        crowd.configure_from_grid(&grid);
        crowd.begin();
        // An ambient agent standing in the path, and one walker heading at it.
        crowd.push([0.0, 0.0, 0.0], [0.0; 3], None, false);
        crowd.push(
            [-1.2, 0.0, 0.0],
            [4.0, 0.0, 0.0],
            Some([9.0, 0.0, 0.0]),
            true,
        );
        let _ = crowd.steer(&params());
        let steered = crowd.desired(1).or_die();
        let avoidance = AvoidanceParams::new();
        let report = crowd.avoid(&avoidance);
        assert_eq!(report.adjusted, 1, "only the walker is corrected");
        let walker = crowd.desired(1).or_die();
        assert!(
            walker[0] < steered[0],
            "the walker must be pushed away: steered {steered:?}, avoided {walker:?}"
        );
        assert_eq!(crowd.desired(0), None, "the ambient agent is not steered");
    }

    #[test]
    fn a_lone_agent_is_isolated_and_does_not_drift() {
        let grid = SpatialGrid::new(SpatialGridConfig::default()).or_die();
        let mut crowd = packed(&grid, &[([0.0, 0.0, 0.0], [0.0; 3], None)]);
        let report = crowd.steer(&params());
        assert_eq!(report.steered, 1);
        assert_eq!(report.isolated, 1);
        assert_eq!(crowd.desired(0), Some([0.0, 0.0, 0.0]));
    }

    #[test]
    fn cells_agree_with_the_spatial_grid() {
        // The kernel's bucket ids have to be the grid's, or the two disagree about neighbours.
        let grid = SpatialGrid::new(SpatialGridConfig::default()).or_die();
        let mut state = 0x1234_5678_u64;
        let mut crowd = Crowd::default();
        crowd.configure_from_grid(&grid);
        for _ in 0..500 {
            let mut next = || {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                (num::u64_to_f32(state >> 33) / 8_388_608.0) * 2_000.0 + -1_000.0
            };
            let position = [next(), next() * 0.5, next()];
            let mine = crowd.cell_of(position);
            let theirs = grid.cell_of(vec3(position));
            assert_eq!(mine, theirs, "cell mismatch at {position:?}");
        }
    }

    #[test]
    fn ambient_agents_are_neighbours_but_not_steered() {
        let grid = SpatialGrid::new(SpatialGridConfig::default()).or_die();
        let mut crowd = Crowd::default();
        crowd.configure_from_grid(&grid);
        crowd.begin();
        crowd.push([0.0, 0.0, 0.0], [0.0; 3], None, true);
        crowd.push([3.0, 0.0, 0.0], [0.0; 3], None, false);
        crowd.push([0.0, 3.0, 0.0], [0.0; 3], None, false);
        let report = crowd.steer(&params());
        assert_eq!(report.steered, 1, "only the first agent is steered");
        assert_eq!(report.isolated, 0, "and it does see the ambient pair");
        assert_eq!(report.pairs, 2);
        assert_eq!(crowd.desired(1), None);
        assert_eq!(crowd.desired(2), None);
    }

    #[test]
    fn a_crowd_above_and_below_pulls_vertically() {
        // Three agents stacked on the y axis, 4 m apart, with nothing pulling them anywhere else.
        // A planar kernel would return a purely horizontal desire — which is exactly the bug this
        // test exists to catch.
        let grid = SpatialGrid::new(SpatialGridConfig::default()).or_die();
        let points = [
            ([0.0_f32, 0.0, 0.0], [0.0_f32; 3], None),
            ([0.0, 4.0, 0.0], [0.0; 3], None),
            ([0.0, -4.0, 0.0], [0.0; 3], None),
        ];
        let mut crowd = packed(&grid, &points);
        crowd.steer(&params());
        let middle = crowd.desired(0).or_die();
        // Separation and cohesion cancel on the axis; what survives is the sum of the two 1/d²
        // pushes, which is zero for a symmetric stack. Alignment is zero too, so this agent sits
        // still — the *outer* ones must move, and toward each other vertically.
        assert!(
            middle[1].abs() < 1e-3,
            "a symmetric stack does not move: {middle:?}"
        );
        let top = crowd.desired(1).or_die();
        assert!(
            top[1] < -1e-3,
            "the top agent should be pushed down: {top:?}"
        );
        let bottom = crowd.desired(2).or_die();
        assert!(
            bottom[1] > 1e-3,
            "the bottom agent should be pushed up: {bottom:?}"
        );
    }

    #[test]
    fn a_parallel_frame_matches_the_scalar_one() {
        // Large enough to take the threaded road, and dense enough that every agent has
        // neighbours: at this size the split is real, so a difference would be a difference.
        let (grid, points) = fixture(4_000);
        let mut scalar = packed(&grid, &points);
        let mut parallel = packed(&grid, &points);
        scalar.set_sequential(true);
        parallel.set_sequential(false);
        let params = params();
        let scalar_report = scalar.steer_scalar(&params);
        let parallel_report = parallel.steer_parallel(&params);
        assert_eq!(scalar_report, parallel_report);
        assert_eq!(scalar.len(), parallel.len());
        for index in 0..scalar.len() {
            assert_eq!(
                scalar.desired(index),
                parallel.desired(index),
                "agent {index} disagrees between the roads"
            );
        }
    }

    #[test]
    fn a_parallel_avoidance_pass_matches_the_scalar_one() {
        let (grid, points) = fixture(4_000);
        let mut scalar = packed(&grid, &points);
        let mut parallel = packed(&grid, &points);
        scalar.set_sequential(true);
        parallel.set_sequential(false);
        let params = params();
        let avoidance = AvoidanceParams::new();
        scalar.steer(&params);
        parallel.steer(&params);
        let scalar_report = scalar.avoid_scalar(&avoidance);
        let parallel_report = parallel.avoid_parallel(&avoidance);
        assert_eq!(scalar_report, parallel_report);
        assert!(
            scalar_report.pairs > 1_000,
            "the fixture must exercise the pass, got {}",
            scalar_report.pairs
        );
        for index in 0..scalar.len() {
            assert_eq!(
                scalar.desired(index),
                parallel.desired(index),
                "agent {index} disagrees between the roads"
            );
        }
    }

    #[test]
    fn the_crowd_agrees_with_the_domain_model() {
        let (grid, points) = fixture(400);
        let mut reference_grid = grid.clone();
        for (index, (position, velocity, _)) in points.iter().enumerate() {
            reference_grid.insert(GridEntry {
                id: NpcId::unique(u32::try_from(index).or_die()),
                position: vec3(*position),
                velocity: vec3(*velocity),
            });
        }
        reference_grid.build();

        let mut crowd = packed(&grid, &points);
        crowd.steer(&params());

        let reference = params();
        let mut scratch: Vec<Neighbour> = Vec::new();
        let mut worst = 0.0_f32;
        for (index, (position, velocity, target)) in points.iter().enumerate() {
            reference_grid.radius(vec3(*position), reference.radius, &mut scratch);
            // The grid returns the agent as its own nearest neighbour and the domain model
            // averages it in; the kernel excludes it. Compare like with like, and see
            // `the_kernel_does_not_average_an_agent_into_its_own_cohesion` for that difference.
            let own = NpcId::unique(u32::try_from(index).or_die());
            scratch.retain(|neighbour| neighbour.id != own);
            let expected = desired_velocity(
                AgentState {
                    position: vec3(*position),
                    velocity: vec3(*velocity),
                    target: target.map(vec3),
                },
                &scratch,
                &SteeringParams::human_scale(),
            );
            let actual = crowd.desired(index).or_die();
            worst = worst.max((vec3(actual) - expected).length());
        }
        assert!(
            worst < 1e-3,
            "crowd kernel drifted from the domain model by {worst}"
        );
    }

    #[test]
    fn the_kernel_does_not_average_an_agent_into_its_own_cohesion() {
        // The reference path asks the grid for neighbours, and the grid returns the agent itself
        // at distance zero. The old code therefore divided the cohesion and alignment sums by a
        // count that included the agent, which dilutes both by 1/n and biases every crowd toward
        // less cohesion the sparser it gets. The kernel leaves the agent out.
        let grid = SpatialGrid::new(SpatialGridConfig::default()).or_die();
        let points = [
            ([0.0_f32, 0.0, 0.0], [0.0_f32; 3], None),
            ([2.0, 1.0, 0.0], [0.0; 3], None),
        ];
        let mut crowd = packed(&grid, &points);
        crowd.steer(&params());
        let steered = crowd.desired(0).or_die();

        let mut reference_grid = grid;
        reference_grid.insert(GridEntry {
            id: NpcId::unique(0),
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
        });
        reference_grid.insert(GridEntry {
            id: NpcId::unique(1),
            position: Vec3::new(2.0, 1.0, 0.0),
            velocity: Vec3::ZERO,
        });
        reference_grid.build();
        let mut scratch = Vec::new();
        reference_grid.radius(Vec3::ZERO, 8.0, &mut scratch);
        assert_eq!(
            scratch.len(),
            2,
            "the grid really does return the agent itself"
        );
        let state = AgentState {
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            target: None,
        };
        let with_self = desired_velocity(state, &scratch, &SteeringParams::human_scale());
        scratch.retain(|neighbour| neighbour.id != NpcId::unique(0));
        let without_self = desired_velocity(state, &scratch, &SteeringParams::human_scale());
        assert!(with_self.distance(without_self) > 0.0, "the bias is real");
        assert!(
            (vec3(steered) - without_self).length() < 1e-4,
            "kernel {steered:?} should match the self-excluded model {without_self:?}"
        );
    }

    #[cfg(feature = "simd")]
    #[test]
    fn the_transposed_road_agrees_with_the_scalar_road() {
        // Lanes are agents here, so a masking bug shows up as four agents steered by somebody
        // else's neighbours. Compare every agent, and compare the counters too.
        let (grid, points) = fixture(500);
        let mut blocked = packed(&grid, &points);
        let mut scalar = packed(&grid, &points);
        let fast = blocked.steer(&params());
        let slow = scalar.steer_scalar(&params());
        assert_eq!(fast.steered, slow.steered);
        assert_eq!(fast.isolated, slow.isolated);
        assert_eq!(
            fast.pairs, slow.pairs,
            "the two roads saw different neighbours"
        );
        let mut worst = 0.0_f32;
        for index in 0..points.len() {
            let a = blocked.desired(index).or_die();
            let b = scalar.desired(index).or_die();
            worst = worst.max((vec3(a) - vec3(b)).length());
        }
        assert!(worst < 1e-4, "transposed road drifted by {worst}");
    }

    #[test]
    fn the_same_world_steers_the_same_way_twice() {
        let (grid, points) = fixture(200);
        let mut first = packed(&grid, &points);
        let mut second = packed(&grid, &points);
        first.steer(&params());
        second.steer(&params());
        for index in 0..points.len() {
            assert_eq!(
                first.desired(index),
                second.desired(index),
                "agent {index} steered differently in an identical crowd"
            );
        }
    }

    #[test]
    fn a_crowd_that_fills_the_volume_uses_more_than_one_layer() {
        // If the vertical window collapsed to a single cell, everybody would be "isolated" and
        // `cells` would equal the number of distinct xz buckets. This asserts both.
        let (grid, points) = fixture(600);
        let mut crowd = packed(&grid, &points);
        let report = crowd.steer(&params());
        assert!(report.cells > 1);
        assert!(
            report.isolated < report.steered / 4,
            "{} isolated of {} steered: the scan is missing layers",
            report.isolated,
            report.steered
        );
        let mut heights: Vec<i64> = points
            .iter()
            .map(|(p, _, _)| num::f32_to_i64(p[1]))
            .collect();
        heights.sort_unstable();
        heights.dedup();
        assert!(heights.len() > 4, "the fixture really does span the axis");
    }

    /// Four agents in a row, no targets, so the only thing that can move them is each other.
    fn row(count: u32) -> (SpatialGrid, Vec<Agent>) {
        let grid = SpatialGrid::new(SpatialGridConfig::default()).or_die();
        let points = (0..count)
            .map(|index| {
                let step = num::u32_to_f32(index);
                ([step * 2.0 + -2.0, 0.0, 0.0], [0.0; 3], None)
            })
            .collect();
        (grid, points)
    }
    #[test]
    fn a_bias_scales_the_configured_weights() {
        let (grid, points) = row(4);
        let mut crowd = packed(&grid, &points);
        let params = params();
        assert_eq!(crowd.bias(0), None);
        assert_eq!(
            crowd.weights_for(0, &params).map(f32::to_bits),
            params.weights.map(f32::to_bits)
        );
        crowd.set_bias(0, [2.0, 0.5, 1.0, 1.0, 1.0]);
        let weights = crowd.weights_for(0, &params);
        assert_eq!(weights[0].to_bits(), (params.weights[0] * 2.0).to_bits());
        assert_eq!(weights[1].to_bits(), (params.weights[1] * 0.5).to_bits());
        assert_eq!(weights[2].to_bits(), params.weights[2].to_bits());
        // A NaN or wild multiplier clamps to the documented band instead of poisoning a frame.
        crowd.set_bias(2, [f32::NAN, 99.0, 0.0, 1.0, 1.0]);
        let clamped = crowd.bias(2).or_die();
        assert_eq!(
            [clamped[0], clamped[1], clamped[2]].map(f32::to_bits),
            [1.0_f32, 4.0, 0.25].map(f32::to_bits)
        );
    }
    #[test]
    fn a_cohesive_bias_pulls_an_agent_toward_the_crowd() {
        let (grid, points) = row(4);
        let params = params();
        // Agent 0 sits left of the other three, so the crowd is to its right and cohesion points
        // along +x. Anything that makes cohesion heavier must move it further that way.
        let mut plain = packed(&grid, &points);
        let mut cohesive = packed(&grid, &points);
        let scales: Vec<[f32; 5]> = (0..4).map(|_| [1.0, 1.0, 3.0, 1.0, 1.0]).collect();
        for (index, bias) in scales.iter().enumerate() {
            cohesive.set_bias(index, *bias);
        }
        let plain_report = plain.steer(&params);
        let cohesive_report = cohesive.steer(&params);
        // Same scan, same pairs: the bias changes the weights, not the work.
        assert_eq!(plain_report.pairs, cohesive_report.pairs);
        let plain_desired = plain.desired(0).or_die();
        let cohesive_desired = cohesive.desired(0).or_die();
        assert!(
            cohesive_desired[0] > plain_desired[0] + 0.01,
            "cohesion bias must pull agent 0 toward the crowd: {plain_desired:?} vs {cohesive_desired:?}"
        );
    }
    #[test]
    fn the_roads_agree_when_agents_carry_biases() {
        let (grid, points) = fixture(4_000);
        let mut scalar = packed(&grid, &points);
        let mut parallel = packed(&grid, &points);
        // Alternate strong and neutral biases, so a road that ignored the buffer would diverge on
        // half the crowd rather than on a handful of agents the test might miss.
        for index in 0..scalar.len() {
            if index % 2 == 0 {
                let bias = [1.6, 0.7, 1.4, 0.9, 1.1];
                scalar.set_bias(index, bias);
                parallel.set_bias(index, bias);
            }
        }
        scalar.set_sequential(true);
        parallel.set_sequential(false);
        let params = params();
        let scalar_report = scalar.steer_scalar(&params);
        let parallel_report = parallel.steer_parallel(&params);
        assert_eq!(scalar_report, parallel_report);
        for index in 0..scalar.len() {
            assert_eq!(
                scalar.desired(index),
                parallel.desired(index),
                "agent {index} disagrees between the roads with a bias"
            );
        }
    }
}
