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
//! One behaviour does change, deliberately. The grid returns an agent as its own nearest
//! neighbour, and the old path averaged that zero-distance self into the alignment and cohesion
//! sums, diluting both by `1/n`. The kernel excludes the agent, which is what the equations
//! always meant: a test pins the old bias so the difference stays visible rather than
//! unexplained.

#![expect(
    clippy::indexing_slicing,
    reason = "every index is bounded by the parallel vectors pushed in lockstep by `push`, and \
              the bucket ranges come from the same counting sort"
)]

use bevy_math::Vec3;

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
    pub fn from_steering(params: &SteeringParams) -> Self {
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
}

/// Agents as arrays, bucketed by the spatial grid's cells.
///
/// Every buffer is allocated once and cleared, never freed: a steady frame does no allocator
/// work, which `tests/crowd_no_alloc.rs` holds it to.
#[derive(Debug, Clone, Default)]
pub struct Crowd {
    positions: Vec<[f32; 2]>,
    velocities: Vec<[f32; 2]>,
    targets: Vec<[f32; 2]>,
    /// Bit 0: this agent has a target. Bit 1: this agent is steered at all.
    flags: Vec<u8>,
    desired: Vec<[f32; 2]>,
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
    /// The vertical cell every agent occupies. The crowd is planar, so this is a constant:
    /// deriving it once keeps the bucket ids identical to the grid's without a per-agent `y`.
    y_cell: u32,
}

const HAS_TARGET: u8 = 1;
const STEERS: u8 = 2;

impl Crowd {
    /// Size the buckets from the grid the ECS side uses, so both see the same neighbours.
    pub fn configure(
        &mut self,
        config: &SpatialGridConfig,
        cell_size: f32,
        dims: [u32; 3],
        plane: f32,
    ) {
        self.dims = dims;
        self.min = [config.min.x, config.min.z, config.min.y];
        self.cell_size = cell_size;
        self.y_cell = clamp_axis((plane - config.min.y) / cell_size, dims[1]);
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
        self.configure_from_grid_on_plane(grid, 0.0);
    }

    /// The same, for a crowd that lives on a plane at height `plane`.
    pub fn configure_from_grid_on_plane(&mut self, grid: &SpatialGrid, plane: f32) {
        let config = grid.config();
        self.configure(&config, grid.effective_cell_size(), grid.dims(), plane);
    }

    /// How many agents are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// Whether the crowd is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// The cells that held an agent during the last pass: the occupancy signal.
    #[must_use]
    pub fn occupied_cells(&self) -> &[u32] {
        &self.touched
    }

    /// Forget last frame's agents, keeping the allocations.
    pub fn begin(&mut self) {
        for cell in self.touched.drain(..) {
            if let Some(count) = self.counts.get_mut(cell as usize) {
                *count = 0;
            }
        }
        self.positions.clear();
        self.velocities.clear();
        self.targets.clear();
        self.flags.clear();
        self.desired.clear();
        self.cells.clear();
        self.order.clear();
    }

    /// Add one agent.
    ///
    /// `steers` is false for ambient-tier agents: they are neighbours to everyone else, but
    /// their own desired velocity is not this kernel's business.
    pub fn push(
        &mut self,
        position: [f32; 2],
        velocity: [f32; 2],
        target: Option<[f32; 2]>,
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
        self.targets.push(target.unwrap_or([0.0, 0.0]));
        self.flags.push(flags);
        self.desired.push([0.0, 0.0]);
        self.cells.push(self.cell_of(position));
    }

    /// The grid cell an agent at `position` falls in, by the grid's own arithmetic.
    #[must_use]
    pub fn cell_of(&self, position: [f32; 2]) -> u32 {
        let x = clamp_axis((position[0] - self.min[0]) / self.cell_size, self.dims[0]);
        let z = clamp_axis((position[1] - self.min[1]) / self.cell_size, self.dims[2]);
        (z * self.dims[1] + self.y_cell) * self.dims[0] + x
    }

    /// The desired velocity written for an agent, if it was steered.
    #[must_use]
    pub fn desired(&self, index: usize) -> Option<[f32; 2]> {
        if self.flags.get(index).copied().unwrap_or(0) & STEERS == 0 {
            return None;
        }
        self.desired.get(index).copied()
    }

    /// Steer everyone, in one pass over the cells.
    ///
    /// Returns the report; occupancy is available from [`Crowd::occupied_cells`] afterwards.
    pub fn steer(&mut self, params: &CrowdParams) -> CrowdReport {
        let mut report = CrowdReport::default();
        if !params.is_usable() || self.is_empty() {
            return report;
        }
        self.order_by_cell();

        for index in 0..self.positions.len() {
            if self.flags.get(index).copied().unwrap_or(0) & STEERS == 0 {
                continue;
            }
            let (count, sum_p, sum_v, sep, pairs) = self.scan(index, params);
            report.pairs += pairs;
            if count == 0 {
                report.isolated += 1;
            }
            let target = if self.flags[index] & HAS_TARGET == 0 {
                None
            } else {
                Some(self.targets[index])
            };
            self.desired[index] = desired_for(
                self.positions[index],
                self.velocities[index],
                target,
                count,
                sum_p,
                sum_v,
                sep,
                params,
            );
            report.steered += 1;
        }
        report.cells = u32::try_from(self.touched.len()).unwrap_or(u32::MAX);
        report
    }

    /// Counting sort of agent indices into cell order, touching only occupied cells.
    fn order_by_cell(&mut self) {
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
    }

    /// Accumulate every neighbour of one agent in a single pass.
    fn scan(&self, index: usize, params: &CrowdParams) -> (u32, [f32; 2], [f32; 2], [f32; 2], u64) {
        let position = self.positions[index];
        let radius_squared = params.radius * params.radius;
        let separation_squared = params.separation * params.separation;
        let mut count = 0_u32;
        let mut sum_p = [0.0_f32; 2];
        let mut sum_v = [0.0_f32; 2];
        let mut sep = [0.0_f32; 2];
        let mut pairs = 0_u64;

        let (x0, x1, z0, z1) = self.window(position, params.radius);
        for z in z0..=z1 {
            let row =
                (z * i64::from(self.dims[1]) + i64::from(self.y_cell)) * i64::from(self.dims[0]);
            for x in x0..=x1 {
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
                    let dx = position[0] - self.positions[other][0];
                    let dz = position[1] - self.positions[other][1];
                    let distance_squared = dx * dx + dz * dz;
                    if distance_squared > radius_squared {
                        continue;
                    }
                    pairs += 1;
                    count += 1;
                    sum_p[0] += self.positions[other][0];
                    sum_p[1] += self.positions[other][1];
                    sum_v[0] += self.velocities[other][0];
                    sum_v[1] += self.velocities[other][1];
                    if distance_squared < separation_squared && distance_squared > f32::EPSILON {
                        let scale = 1.0 / distance_squared.max(1e-6);
                        sep[0] += dx * scale;
                        sep[1] += dz * scale;
                    }
                }
            }
        }
        (count, sum_p, sum_v, sep, pairs)
    }

    /// The inclusive cell range that can hold anything within `radius` of `position`.
    fn window(&self, position: [f32; 2], radius: f32) -> (i64, i64, i64, i64) {
        let x0 = clamp_axis(
            (position[0] - radius - self.min[0]) / self.cell_size,
            self.dims[0],
        ) as i64;
        let x1 = clamp_axis(
            (position[0] + radius - self.min[0]) / self.cell_size,
            self.dims[0],
        ) as i64;
        let z0 = clamp_axis(
            (position[1] - radius - self.min[1]) / self.cell_size,
            self.dims[2],
        ) as i64;
        let z1 = clamp_axis(
            (position[1] + radius - self.min[1]) / self.cell_size,
            self.dims[2],
        ) as i64;
        (x0, x1, z0, z1)
    }
}

/// The per-agent arithmetic: the domain model, over sums the scan already collected.
#[allow(clippy::too_many_arguments)]
fn desired_for(
    position: [f32; 2],
    velocity: [f32; 2],
    target: Option<[f32; 2]>,
    count: u32,
    sum_p: [f32; 2],
    sum_v: [f32; 2],
    sep: [f32; 2],
    params: &CrowdParams,
) -> [f32; 2] {
    let max_speed = params.max_speed;
    let mut steering = accumulate(
        Vec3::ZERO,
        Vec3::new(sep[0], 0.0, sep[1]),
        params.weights[0],
        max_speed,
    );
    if count > 0 {
        let scaled = count as f32;
        let average = Vec3::new(sum_v[0] / scaled, 0.0, sum_v[1] / scaled);
        steering = accumulate(
            steering,
            average - Vec3::new(velocity[0], 0.0, velocity[1]),
            params.weights[1],
            max_speed,
        );
        let centre = Vec3::new(sum_p[0] / scaled, 0.0, sum_p[1] / scaled);
        steering = accumulate(
            steering,
            centre - Vec3::new(position[0], 0.0, position[1]),
            params.weights[2],
            max_speed,
        );
    }
    if let Some(target) = target {
        let offset = Vec3::new(target[0] - position[0], 0.0, target[1] - position[1]);
        let distance = offset.length();
        if distance <= params.stop {
            return [0.0, 0.0];
        }
        if distance <= params.arrival {
            let scale = distance / params.arrival;
            let arrival = offset.normalize_or_zero() * (max_speed * scale);
            steering = accumulate(steering, arrival, params.weights[4], max_speed);
        } else {
            let seek = offset.normalize_or_zero() * max_speed;
            steering = accumulate(steering, seek, params.weights[3], max_speed);
        }
    }
    if steering.length_squared() < params.jitter * params.jitter {
        return [0.0, 0.0];
    }
    let clamped = steering.clamp_length_max(max_speed);
    [clamped.x, clamped.z]
}

fn clamp_axis(value: f32, dim: u32) -> u32 {
    let last = dim.saturating_sub(1);
    let index = value.floor();
    if index < 0.0 {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let index = index as u32;
    index.min(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::NpcId;
    use crate::domain::spatial::{GridEntry, Neighbour};
    use crate::domain::steering::{AgentState, desired_velocity};

    fn params() -> CrowdParams {
        CrowdParams::from_steering(&SteeringParams::human_scale())
    }

    /// One agent's fixture data: position, velocity, optional target.
    type Agent = ([f32; 2], [f32; 2], Option<[f32; 2]>);

    fn fixture(count: u32) -> (SpatialGrid, Vec<Agent>) {
        let grid = SpatialGrid::new(SpatialGridConfig::default()).expect("grid");
        let mut points = Vec::new();
        for index in 0..count {
            let ring = 1.6 * libm::sqrtf(index as f32);
            let angle = index as f32 * 2.399_963_2;
            let position = [libm::cosf(angle) * ring, libm::sinf(angle) * ring];
            let velocity = [libm::cosf(angle) * 0.4, libm::sinf(angle) * 0.4];
            let target = if index % 3 == 0 {
                Some([40.0, 40.0])
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

    #[test]
    fn cells_agree_with_the_spatial_grid() {
        let grid = SpatialGrid::new(SpatialGridConfig::default()).expect("grid");
        let mut crowd = Crowd::default();
        crowd.configure_from_grid(&grid);
        let mut state = 0x1234_5678_u32;
        for _ in 0..500 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let x = ((state & 0xFFFF) as f32 / 65_535.0) * 2_000.0 - 1_000.0;
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let z = ((state & 0xFFFF) as f32 / 65_535.0) * 2_000.0 - 1_000.0;
            assert_eq!(
                crowd.cell_of([x, z]),
                grid.cell_of(Vec3::new(x, 0.0, z)),
                "cell mismatch at {x},{z}"
            );
        }
    }

    #[test]
    fn a_lone_agent_is_isolated_and_does_not_drift() {
        let grid = SpatialGrid::new(SpatialGridConfig::default()).expect("grid");
        let mut crowd = packed(&grid, &[([0.0, 0.0], [1.0, 0.0], None)]);
        let report = crowd.steer(&params());
        assert_eq!(report.isolated, 1);
        assert_eq!(crowd.desired(0), Some([0.0, 0.0]));
    }

    #[test]
    fn ambient_agents_are_neighbours_but_not_steered() {
        let grid = SpatialGrid::new(SpatialGridConfig::default()).expect("grid");
        let mut crowd = Crowd::default();
        crowd.configure_from_grid(&grid);
        crowd.begin();
        crowd.push([0.0, 0.0], [0.0, 0.0], None, true);
        crowd.push([1.0, 0.0], [0.0, 0.0], None, false);
        let report = crowd.steer(&params());
        assert_eq!(report.steered, 1);
        assert!(crowd.desired(1).is_none());
        // The ambient agent is still a neighbour, so the steered one is not isolated.
        assert_eq!(report.isolated, 0);
    }

    #[test]
    fn the_crowd_agrees_with_the_domain_model() {
        let (grid, points) = fixture(400);
        let mut crowd = packed(&grid, &points);
        crowd.steer(&params());

        let mut grid = grid;
        for (index, (position, velocity, _)) in points.iter().enumerate() {
            grid.insert(GridEntry {
                id: NpcId::unique(u32::try_from(index).expect("small fixture")),
                position: Vec3::new(position[0], 0.0, position[1]),
                velocity: Vec3::new(velocity[0], 0.0, velocity[1]),
            });
        }
        grid.build();

        let reference = SteeringParams::human_scale();
        let mut scratch: Vec<Neighbour> = Vec::new();
        let mut worst = 0.0_f32;
        for (index, (position, velocity, target)) in points.iter().enumerate() {
            grid.radius(
                Vec3::new(position[0], 0.0, position[1]),
                reference.neighbour_radius,
                &mut scratch,
            );
            // The grid returns the agent as its own nearest neighbour and the domain model
            // averages it in; the kernel excludes it. Compare like with like, and see
            // `the_kernel_does_not_average_an_agent_into_its_own_cohesion` for that difference.
            let own = NpcId::unique(u32::try_from(index).expect("small fixture"));
            scratch.retain(|neighbour| neighbour.id != own);
            let expected = desired_velocity(
                AgentState {
                    position: Vec3::new(position[0], 0.0, position[1]),
                    velocity: Vec3::new(velocity[0], 0.0, velocity[1]),
                    target: target.map(|target| Vec3::new(target[0], 0.0, target[1])),
                },
                &scratch,
                &reference,
            );
            let actual = crowd.desired(index).expect("steered");
            worst = worst.max(
                (expected.x - actual[0])
                    .abs()
                    .max((expected.z - actual[1]).abs()),
            );
        }
        assert!(
            worst < 1e-3,
            "crowd kernel drifted from the domain model by {worst}"
        );
    }

    #[test]
    fn the_kernel_does_not_average_an_agent_into_its_own_cohesion() {
        // The reference path asks the grid for neighbours, and the grid returns the agent
        // itself at distance zero. The old code therefore divided the cohesion and alignment
        // sums by a count that included the agent, which dilutes both by 1/n and biases every
        // crowd toward less cohesion the sparser it gets. The kernel leaves the agent out.
        let grid = SpatialGrid::new(SpatialGridConfig::default()).expect("grid");
        let points = vec![
            ([0.0_f32, 0.0], [0.0_f32, 0.0], None),
            ([2.0, 0.0], [0.0, 0.0], None),
        ];
        let mut crowd = packed(&grid, &points);
        crowd.steer(&params());
        let steered = crowd.desired(0).expect("steered");

        let mut reference_grid = grid;
        reference_grid.insert(GridEntry {
            id: NpcId::unique(0),
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
        });
        reference_grid.insert(GridEntry {
            id: NpcId::unique(1),
            position: Vec3::new(2.0, 0.0, 0.0),
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
        let with_self = desired_velocity(
            AgentState {
                position: Vec3::ZERO,
                velocity: Vec3::ZERO,
                target: None,
            },
            &scratch,
            &SteeringParams::human_scale(),
        );
        scratch.retain(|neighbour| neighbour.id != NpcId::unique(0));
        let without_self = desired_velocity(
            AgentState {
                position: Vec3::ZERO,
                velocity: Vec3::ZERO,
                target: None,
            },
            &scratch,
            &SteeringParams::human_scale(),
        );
        assert!(with_self.distance(without_self) > 0.0, "the bias is real");
        assert!(
            (steered[0] - without_self.x).abs() < 1e-4
                && (steered[1] - without_self.z).abs() < 1e-4,
            "kernel {steered:?} should match the self-excluded model {without_self:?}"
        );
    }

    #[test]
    fn the_same_world_steers_the_same_way_twice() {
        let (grid, points) = fixture(300);
        let mut first = packed(&grid, &points);
        let a = first.steer(&params());
        let mut second = packed(&grid, &points);
        let b = second.steer(&params());
        assert_eq!(a, b);
        for index in 0..points.len() {
            assert_eq!(first.desired(index), second.desired(index));
        }
    }
}
