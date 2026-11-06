//! Steering over packed arrays instead of entities.
//!
//! ADR 0018 gate 8 failed for a reason that has nothing to do with thinking: the crowd path
//! costs ~1.9 ms per 1,000 steered NPCs, so a 5,000-entity manifest band breaks an 8 ms
//! frame on its own. The cost is not the arithmetic — three weighted vectors per neighbour —
//! it is reaching each agent through an ECS query, an archetype access and a change-detection
//! borrow, once per agent per tick.
//!
//! This module is the same arithmetic over contiguous slices: positions, velocities and
//! desired vectors in three `Vec`s, neighbours found through a linked-list bucket grid built
//! with two more. Nothing allocates after construction, nothing borrows per agent, and the
//! only per-agent state is an index.
//!
//! It is deliberately not a Bevy type. The band plugin (ADR 0020) owns the copy in and out;
//! keeping the kernel pure is what makes it testable without a world and benchmarkable at
//! a population no ECS would survive.

// 44 of this module's index expressions trip `indexing_slicing`. Every one is bounded by a length
// check made earlier in the same function (`pack.len()`, `bucket_ranges.len() - 1`), and converting
// them would put an unreachable arm inside every inner loop of the kernel — the one place where the
// compiler's proof of the bound is worth more than the lint. The removal is `kernel-bounds-checks`
// in `plan.toml`, and it has to come with a re-run of `benches/crowd.rs`.
#![expect(
    clippy::indexing_slicing,
    reason = "44 sites, each bounded by a length check in the same function; removing them is a \
              measured kernel change, tracked in plan.toml"
)]

use core::f32::consts::TAU;
#[cfg(feature = "simd")]
use wide::f32x4;

/// How many agents the vectorised steering path handles at once.
#[cfg(feature = "simd")]
const LANES: usize = 4;

/// Four lanes of accumulators, carried from the scan to the finalisation.
#[cfg(feature = "simd")]
struct BlockSums {
    counts: f32x4,
    close: f32x4,
    centre_x: f32x4,
    centre_y: f32x4,
    heading_x: f32x4,
    heading_y: f32x4,
    away_x: f32x4,
    away_y: f32x4,
    velocity_x: f32x4,
    velocity_y: f32x4,
}

/// Steering weights and limits. Distances in metres, speeds in metres per second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SteerParams {
    /// Neighbourhood radius for cohesion and alignment.
    pub radius: f32,
    /// Radius at which agents push apart. Should be smaller than `radius`.
    pub separation: f32,
    /// Maximum speed after steering.
    pub max_speed: f32,
    /// Maximum steering force applied in one tick.
    pub max_force: f32,
    /// Weight of cohesion.
    pub cohesion: f32,
    /// Weight of alignment.
    pub alignment: f32,
    /// Weight of separation.
    pub separation_weight: f32,
    /// The extent of the world the grid must cover, in metres.
    pub extent: f32,
}

impl Default for SteerParams {
    fn default() -> Self {
        Self {
            radius: 12.0,
            separation: 4.0,
            max_speed: 6.0,
            max_force: 0.4,
            cohesion: 0.4,
            alignment: 0.6,
            separation_weight: 1.2,
            extent: 512.0,
        }
    }
}

impl SteerParams {
    /// Whether the parameters can produce a sensible grid.
    #[must_use]
    pub fn is_usable(self) -> bool {
        self.radius > 0.0
            && self.radius.is_finite()
            && self.separation > 0.0
            && self.separation <= self.radius
            && self.max_speed > 0.0
            && self.max_force > 0.0
            && self.extent > self.radius
            && self.extent.is_finite()
    }
}

/// What one steering pass cost and produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SteerReport {
    /// Agents that were steered.
    pub steered: u32,
    /// Neighbour pairs examined, the honest measure of the work done.
    pub pairs: u64,
    /// Agents with no neighbour at all, which is the sparse case worth knowing about.
    pub isolated: u32,
}

/// A crowd as arrays.
#[derive(Debug, Clone, Default)]
pub struct Pack {
    positions: Vec<[f32; 2]>,
    velocities: Vec<[f32; 2]>,
    desired: Vec<[f32; 2]>,
    heads: Vec<i32>,
    next: Vec<i32>,
    /// Agent indices grouped by grid cell, rebuilt with the grid. The vectorised path walks
    /// this instead of `0..len`, so the four agents in a block are neighbours on the map and
    /// not merely neighbours in memory.
    order: Vec<u32>,
    cell_size: f32,
    side: usize,
}

impl Pack {
    /// Build a pack with room for `capacity` agents, covering `extent` metres.
    ///
    /// The grid is sized from the neighbourhood radius, so a bigger crowd over the same
    /// area does not grow the grid and a bigger area does.
    #[must_use]
    pub fn with_capacity(capacity: usize, params: SteerParams) -> Self {
        let cell_size = params.radius.max(0.001);
        let side = ((params.extent / cell_size).ceil() as usize).clamp(1, 4096);
        Self {
            positions: Vec::with_capacity(capacity),
            velocities: Vec::with_capacity(capacity),
            desired: Vec::with_capacity(capacity),
            heads: vec![-1; side * side],
            next: Vec::with_capacity(capacity),
            order: Vec::with_capacity(capacity),
            cell_size,
            side,
        }
    }

    /// How many agents the pack holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// Whether the pack holds nobody.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Add an agent, returning its index.
    pub fn push(&mut self, position: [f32; 2], velocity: [f32; 2]) -> usize {
        let index = self.positions.len();
        self.positions.push(position);
        self.velocities.push(velocity);
        self.desired.push([0.0, 0.0]);
        self.next.push(-1);
        index
    }

    /// Clear every agent, keeping the allocations.
    pub fn clear(&mut self) {
        self.positions.clear();
        self.velocities.clear();
        self.desired.clear();
        self.next.clear();
    }

    /// An agent's position.
    #[must_use]
    pub fn position(&self, index: usize) -> Option<[f32; 2]> {
        self.positions.get(index).copied()
    }

    /// An agent's velocity.
    #[must_use]
    pub fn velocity(&self, index: usize) -> Option<[f32; 2]> {
        self.velocities.get(index).copied()
    }

    /// The velocity steering asked for, before integration.
    #[must_use]
    pub fn desired(&self, index: usize) -> Option<[f32; 2]> {
        self.desired.get(index).copied()
    }

    /// Write the desired velocities back as velocities, clamped to `max_speed`.
    pub fn integrate(&mut self, max_speed: f32) {
        let limit = max_speed * max_speed;
        for (velocity, desired) in self.velocities.iter_mut().zip(self.desired.iter()) {
            let squared = desired[0] * desired[0] + desired[1] * desired[1];
            if squared > limit && squared > 0.0 {
                let scale = max_speed / squared.sqrt();
                *velocity = [desired[0] * scale, desired[1] * scale];
            } else {
                *velocity = *desired;
            }
        }
    }

    /// Move every agent by its velocity over `dt`.
    pub fn advance(&mut self, dt: f32, extent: f32) {
        for (position, velocity) in self.positions.iter_mut().zip(self.velocities.iter()) {
            position[0] = wrap(position[0] + velocity[0] * dt, extent);
            position[1] = wrap(position[1] + velocity[1] * dt, extent);
        }
    }

    /// Steer every agent from its neighbours.
    ///
    /// Three rules, the same three the ECS systems apply: cohesion toward the local centre,
    /// alignment toward the local heading, separation away from anyone too close. The result
    /// lands in [`Pack::desired`]; integration is the caller's, because a game may want to
    /// blend it with a flow field or a path.
    ///
    /// With the `simd` feature this walks four agents at a time behind one bucket scan; the
    /// arithmetic is identical, so the results are identical too.
    pub fn steer(&mut self, params: &SteerParams) -> SteerReport {
        let mut report = SteerReport::default();
        if !params.is_usable() || self.positions.is_empty() {
            return report;
        }
        self.build_grid();

        #[cfg(feature = "simd")]
        self.steer_blocks(params, &mut report);

        #[cfg(not(feature = "simd"))]
        for index in 0..self.positions.len() {
            self.steer_one(index, params, &mut report);
        }

        report
    }

    /// One agent: the definition the vectorised path has to reproduce exactly.
    fn steer_one(&mut self, index: usize, params: &SteerParams, report: &mut SteerReport) {
        let radius_squared = params.radius * params.radius;
        let separation_squared = params.separation * params.separation;
        let position = self.positions[index];
        let velocity = self.velocities[index];

        let mut centre = [0.0_f32; 2];
        let mut heading = [0.0_f32; 2];
        let mut away = [0.0_f32; 2];
        let mut neighbours = 0_u32;
        let mut too_close = 0_u32;

        self.for_each_neighbour(position, |other| {
            if other == index {
                return;
            }
            let delta = [
                self.positions[other][0] - position[0],
                self.positions[other][1] - position[1],
            ];
            let distance_squared = delta[0] * delta[0] + delta[1] * delta[1];
            if distance_squared > radius_squared {
                return;
            }
            report.pairs += 1;
            neighbours += 1;
            centre[0] += self.positions[other][0];
            centre[1] += self.positions[other][1];
            heading[0] += self.velocities[other][0];
            heading[1] += self.velocities[other][1];
            if distance_squared < separation_squared {
                too_close += 1;
                // Weight by inverse distance so a near miss matters more than a distant
                // one; the epsilon keeps a coincident pair finite.
                let scale = 1.0 / (distance_squared + 1e-4);
                away[0] -= delta[0] * scale;
                away[1] -= delta[1] * scale;
            }
        });

        if neighbours == 0 {
            report.isolated += 1;
            self.desired[index] = velocity;
            return;
        }

        let count = neighbours as f32;
        centre[0] = centre[0] / count - position[0];
        centre[1] = centre[1] / count - position[1];
        heading[0] /= count;
        heading[1] /= count;

        let cohesion = clamp(centre, params.max_speed);
        let alignment = clamp(sub(heading, velocity), params.max_force);
        let separation = if too_close > 0 {
            clamp(away, params.max_force)
        } else {
            [0.0, 0.0]
        };
        self.desired[index] = clamp(
            [
                velocity[0]
                    + params.cohesion * cohesion[0]
                    + params.alignment * alignment[0]
                    + params.separation_weight * separation[0],
                velocity[1]
                    + params.cohesion * cohesion[1]
                    + params.alignment * alignment[1]
                    + params.separation_weight * separation[1],
            ],
            params.max_speed,
        );
        report.steered += 1;
    }

    /// Turn four lanes of sums into four desired velocities.
    ///
    /// The same arithmetic as [`Pack::steer_one`], lane by lane, because a caller comparing
    /// the two paths should find them indistinguishable rather than merely close.
    #[cfg(feature = "simd")]
    fn finish_block(
        &mut self,
        indices: [usize; LANES],
        params: &SteerParams,
        sums: &BlockSums,
        report: &mut SteerReport,
    ) {
        let count = sums.counts.to_array();
        let centre_x = sums.centre_x.to_array();
        let centre_y = sums.centre_y.to_array();
        let heading_x = sums.heading_x.to_array();
        let heading_y = sums.heading_y.to_array();
        let away_x = sums.away_x.to_array();
        let away_y = sums.away_y.to_array();
        let close = sums.close.to_array();
        let velocity_x = sums.velocity_x.to_array();
        let velocity_y = sums.velocity_y.to_array();

        for lane in 0..LANES {
            let index = indices[lane];
            if index >= self.positions.len() {
                continue;
            }
            if count[lane] == 0.0 {
                report.isolated += 1;
                self.desired[index] = [velocity_x[lane], velocity_y[lane]];
                continue;
            }
            let position = self.positions[index];
            let scaled = count[lane];
            let cohesion = clamp(
                [
                    centre_x[lane] / scaled - position[0],
                    centre_y[lane] / scaled - position[1],
                ],
                params.max_speed,
            );
            let alignment = clamp(
                sub(
                    [heading_x[lane] / scaled, heading_y[lane] / scaled],
                    [velocity_x[lane], velocity_y[lane]],
                ),
                params.max_force,
            );
            let separation = if close[lane] > 0.0 {
                clamp([away_x[lane], away_y[lane]], params.max_force)
            } else {
                [0.0, 0.0]
            };
            self.desired[index] = clamp(
                [
                    velocity_x[lane]
                        + params.cohesion * cohesion[0]
                        + params.alignment * alignment[0]
                        + params.separation_weight * separation[0],
                    velocity_y[lane]
                        + params.cohesion * cohesion[1]
                        + params.alignment * alignment[1]
                        + params.separation_weight * separation[1],
                ],
                params.max_speed,
            );
            report.steered += 1;
        }
    }

    /// Four agents at a time, behind one bucket scan (ADR 0023).
    ///
    /// The blocks come from the cell-major `order`, not from `0..len`: four consecutive
    /// agents in the pack can be four unrelated places on the map, and then the union box
    /// they share is larger than the four scans it replaces. Grouped by cell, a block is
    /// usually one cell wide and the scan is strictly less work than the scalar path.
    #[cfg(feature = "simd")]
    fn steer_blocks(&mut self, params: &SteerParams, report: &mut SteerReport) {
        let mut block = 0_usize;
        while (block + LANES) <= self.order.len() {
            let mut indices = [0_usize; LANES];
            for (lane, slot) in indices.iter_mut().enumerate() {
                *slot = self.order[block + lane] as usize;
            }
            self.steer_block(indices, params, report);
            block += LANES;
        }
        while block < self.order.len() {
            let index = self.order[block] as usize;
            self.steer_one(index, params, report);
            block += 1;
        }
    }

    /// Set one block up: four agents' lanes, their cells, and the box they share.
    #[cfg(feature = "simd")]
    fn steer_block(
        &mut self,
        indices: [usize; LANES],
        params: &SteerParams,
        report: &mut SteerReport,
    ) {
        let mut cells = [(0_usize, 0_usize); LANES];
        let mut positions = [(0.0_f32, 0.0_f32); LANES];
        let mut velocities = [(0.0_f32, 0.0_f32); LANES];
        for lane in 0..LANES {
            let Some(position) = self.positions.get(indices[lane]).copied() else {
                continue;
            };
            positions[lane] = (position[0], position[1]);
            cells[lane] = self.cell_of(position);
            if let Some(velocity) = self.velocities.get(indices[lane]) {
                velocities[lane] = (velocity[0], velocity[1]);
            }
        }

        let x_min = cells
            .iter()
            .map(|cell| cell.0)
            .min()
            .unwrap_or(0)
            .saturating_sub(1);
        let y_min = cells
            .iter()
            .map(|cell| cell.1)
            .min()
            .unwrap_or(0)
            .saturating_sub(1);
        let x_max = (cells.iter().map(|cell| cell.0).max().unwrap_or(0) + 1).min(self.side - 1);
        let y_max = (cells.iter().map(|cell| cell.1).max().unwrap_or(0) + 1).min(self.side - 1);
        if (x_max - x_min + 1) * (y_max - y_min + 1) > 3 * 3 + 3 {
            // The order was not contiguous, so the box costs more than the four scans it
            // replaces; the scalar path is the honest choice for this block.
            for index in indices {
                self.steer_one(index, params, report);
            }
            return;
        }

        let identities = f32x4::from(indices.map(|index| index as f32));
        let position = (
            f32x4::from(positions.map(|value| value.0)),
            f32x4::from(positions.map(|value| value.1)),
        );
        let velocity = (
            f32x4::from(velocities.map(|value| value.0)),
            f32x4::from(velocities.map(|value| value.1)),
        );
        let (sums, pairs) = self.scan_block(
            identities,
            position,
            velocity,
            cells,
            (x_min, y_min, x_max, y_max),
            params,
        );
        report.pairs += pairs;
        self.finish_block(indices, params, &sums, report);
    }

    /// The scan: every cell in the block's box, every agent in each cell, four lanes at once.
    ///
    /// Split from the setup and the finish because this is the part that has to be fast, and
    /// the other two are bookkeeping around it.
    #[cfg(feature = "simd")]
    fn scan_block(
        &self,
        identities: f32x4,
        position: (f32x4, f32x4),
        velocity: (f32x4, f32x4),
        cells: [(usize, usize); LANES],
        bounds: (usize, usize, usize, usize),
        params: &SteerParams,
    ) -> (BlockSums, u64) {
        let (position_x, position_y) = position;
        let (velocity_x, velocity_y) = velocity;
        let (x_min, y_min, x_max, y_max) = bounds;

        let radius_squared = f32x4::splat(params.radius * params.radius);
        let separation_squared = f32x4::splat(params.separation * params.separation);
        let epsilon = f32x4::splat(1e-4);
        let one = f32x4::splat(1.0);
        let zero = f32x4::splat(0.0);

        let mut centre_x = zero;
        let mut centre_y = zero;
        let mut heading_x = zero;
        let mut heading_y = zero;
        let mut away_x = zero;
        let mut away_y = zero;
        let mut counts = zero;
        let mut close_counts = zero;

        // Four agents in one cell is the common case once the order is cell-major, and then
        // every lane sees every cell in the box. Checking it once saves building four masks
        // per cell, which is exactly what made the first version slower than the scalar path.
        let same_cell = cells.iter().all(|cell| *cell == cells[0]);
        let all_inside = f32x4::splat(f32::from_bits(u32::MAX));

        for y in y_min..=y_max {
            for x in x_min..=x_max {
                let inside = if same_cell {
                    all_inside
                } else {
                    let mut inside = [0.0_f32; LANES];
                    for (lane, slot) in inside.iter_mut().enumerate() {
                        let (cx, cy) = cells[lane];
                        let near = x + 1 >= cx && x <= cx + 1 && y + 1 >= cy && y <= cy + 1;
                        *slot = if near { f32::from_bits(u32::MAX) } else { 0.0 };
                    }
                    f32x4::from(inside)
                };

                let mut cursor = self.heads[y * self.side + x];
                let mut guard = 0_usize;
                while cursor >= 0 && guard <= self.positions.len() {
                    let other = cursor as usize;
                    cursor = self.next.get(other).copied().unwrap_or(-1);
                    guard += 1;

                    let Some(other_position) = self.positions.get(other).copied() else {
                        continue;
                    };
                    let other_velocity = self.velocities.get(other).copied().unwrap_or([0.0, 0.0]);
                    let dx = f32x4::splat(other_position[0]) - position_x;
                    let dy = f32x4::splat(other_position[1]) - position_y;
                    // Deliberately not `mul_add`: a fused multiply-add rounds once where the
                    // scalar path rounds twice, which is a one-ULP difference in the distance
                    // and therefore in the result. Parity with the reference is worth more
                    // here than the instruction.
                    let distance_squared = dx * dx + dy * dy;

                    let mut keep = distance_squared.simd_le(radius_squared) & inside;
                    keep &= f32x4::splat(other as f32).simd_ne(identities);
                    if !keep.any() {
                        continue;
                    }

                    let weight = keep.select(one, zero);
                    counts += weight;
                    centre_x += f32x4::splat(other_position[0]) * weight;
                    centre_y += f32x4::splat(other_position[1]) * weight;
                    heading_x += f32x4::splat(other_velocity[0]) * weight;
                    heading_y += f32x4::splat(other_velocity[1]) * weight;

                    let close = keep & distance_squared.simd_lt(separation_squared);
                    if close.any() {
                        let inverse = one / (distance_squared + epsilon);
                        let separation_weight = close.select(one, zero);
                        close_counts += separation_weight;
                        let push = inverse * separation_weight;
                        away_x -= dx * push;
                        away_y -= dy * push;
                    }
                }
            }
        }

        let pairs = counts
            .to_array()
            .iter()
            .map(|lane| *lane as u64)
            .sum::<u64>();
        (
            BlockSums {
                counts,
                close: close_counts,
                centre_x,
                centre_y,
                heading_x,
                heading_y,
                away_x,
                away_y,
                velocity_x,
                velocity_y,
            },
            pairs,
        )
    }
    fn build_grid(&mut self) {
        for head in &mut self.heads {
            *head = -1;
        }
        let side = self.side;
        for index in 0..self.positions.len() {
            let cell = self.cell_of(self.positions[index]);
            let bucket = cell.1 * side + cell.0;
            self.next[index] = self.heads[bucket];
            self.heads[bucket] = index as i32;
        }

        // Cell-major order, free: the buckets already hold exactly this grouping.
        self.order.clear();
        for bucket in 0..self.heads.len() {
            let mut cursor = self.heads[bucket];
            let mut guard = 0_usize;
            while cursor >= 0 && guard <= self.positions.len() {
                self.order.push(cursor as u32);
                cursor = self.next.get(cursor as usize).copied().unwrap_or(-1);
                guard += 1;
            }
        }
    }

    fn cell_of(&self, position: [f32; 2]) -> (usize, usize) {
        let x = (position[0] / self.cell_size).floor();
        let y = (position[1] / self.cell_size).floor();
        let clamp = |value: f32| -> usize {
            if !value.is_finite() || value < 0.0 {
                0
            } else {
                (value as usize).min(self.side - 1)
            }
        };
        (clamp(x), clamp(y))
    }

    fn for_each_neighbour(&self, position: [f32; 2], mut visit: impl FnMut(usize)) {
        let (cx, cy) = self.cell_of(position);
        let side = self.side;
        let y_start = cy.saturating_sub(1);
        let x_start = cx.saturating_sub(1);
        for y in y_start..=(cy + 1).min(side - 1) {
            for x in x_start..=(cx + 1).min(side - 1) {
                let mut cursor = self.heads[y * side + x];
                // A corrupt chain would loop forever, which in a crowd system is a hang in
                // the frame; the guard is the pack's own length, not a hope.
                let mut guard = 0_usize;
                while cursor >= 0 && guard <= self.positions.len() {
                    visit(cursor as usize);
                    cursor = self.next.get(cursor as usize).copied().unwrap_or(-1);
                    guard += 1;
                }
            }
        }
    }
}

/// Wrap a coordinate into `0..extent` without a libm call in the common case.
///
/// `f32::rem_euclid` lowers to `fmod`, which the first profile of a 1M frame showed inside
/// the libm share. A position in the steady state is either already in range - one branch -
/// or one extent out of it, which a single subtraction fixes exactly.
fn wrap(value: f32, extent: f32) -> f32 {
    if (0.0..extent).contains(&value) {
        return value;
    }
    let turns = (value / extent).floor();
    let wrapped = turns.mul_add(-extent, value);
    if wrapped >= extent {
        wrapped - extent
    } else if wrapped < 0.0 {
        wrapped + extent
    } else {
        wrapped
    }
}

fn sub(left: [f32; 2], right: [f32; 2]) -> [f32; 2] {
    [left[0] - right[0], left[1] - right[1]]
}

fn clamp(vector: [f32; 2], max: f32) -> [f32; 2] {
    // Compare squared lengths so the square root is only paid when a vector is actually
    // over its limit, which is the minority: `hypotf` was 8.4% of the whole 1M frame.
    let squared = vector[0] * vector[0] + vector[1] * vector[1];
    let limit = max * max;
    if squared <= limit || squared == 0.0 {
        vector
    } else {
        let scale = max / squared.sqrt();
        [vector[0] * scale, vector[1] * scale]
    }
}

/// A deterministic scatter of agents, so tests and benches agree on the crowd.
pub fn scatter(count: usize, extent: f32, params: &mut Pack) {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut next = || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((state >> 33) as f32) / (u32::MAX >> 1) as f32
    };
    for _ in 0..count {
        let angle = next() * TAU;
        params.push(
            [next() * extent, next() * extent],
            [angle.cos(), angle.sin()],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::OrDie;

    fn params() -> SteerParams {
        SteerParams {
            radius: 8.0,
            separation: 2.0,
            extent: 64.0,
            ..SteerParams::default()
        }
    }

    #[test]
    fn separation_pushes_close_neighbours_apart() {
        let mut pack = Pack::with_capacity(2, params());
        pack.push([10.0, 10.0], [0.0, 0.0]);
        pack.push([10.5, 10.0], [0.0, 0.0]);
        let report = pack.steer(&params());
        assert_eq!(report.steered, 2);
        let left = pack.desired(0).or_die();
        let right = pack.desired(1).or_die();
        // Coincident-with-separation: the pair must move away from each other along x.
        assert!(
            left[0] < 0.0,
            "the western agent should flee west: {left:?}"
        );
        assert!(
            right[0] > 0.0,
            "the eastern agent should flee east: {right:?}"
        );
    }

    #[test]
    fn alignment_turns_a_straggler_toward_the_crowd() {
        let mut pack = Pack::with_capacity(4, params());
        for i in 0..3 {
            pack.push([10.0 + i as f32, 10.0], [1.0, 0.0]);
        }
        pack.push([10.0, 11.0], [0.0, 1.0]);
        pack.steer(&params());
        let straggler = pack.desired(3).or_die();
        assert!(
            straggler[0] > 0.0,
            "alignment should add eastward speed: {straggler:?}"
        );
    }

    #[test]
    fn a_solo_agent_keeps_the_velocity_it_had() {
        let mut pack = Pack::with_capacity(1, params());
        pack.push([5.0, 5.0], [0.7, -0.3]);
        let report = pack.steer(&params());
        assert_eq!(report.isolated, 1);
        assert_eq!(report.pairs, 0);
        assert_eq!(pack.desired(0), Some([0.7, -0.3]));
    }

    #[test]
    fn steering_is_deterministic_and_does_not_allocate() {
        let mut first = Pack::with_capacity(256, params());
        scatter(256, 64.0, &mut first);
        let mut second = first.clone();
        assert_eq!(first.steer(&params()), second.steer(&params()));
        // Capacity is the contract: the pack never grows during steering.
        let capacity = first.positions.capacity();
        first.steer(&params());
        assert_eq!(first.positions.capacity(), capacity);
    }

    #[test]
    fn the_grid_finds_the_same_neighbours_as_brute_force() {
        let mut pack = Pack::with_capacity(300, params());
        scatter(300, 64.0, &mut pack);
        let report = pack.steer(&params());

        // Count the pairs a quadratic scan would find, and insist the grid agrees. This is
        // the only test that catches a bucket boundary that drops a neighbour.
        let mut brute = 0_u64;
        for i in 0..pack.len() {
            for j in 0..pack.len() {
                if i == j {
                    continue;
                }
                let a = pack.position(i).or_die();
                let b = pack.position(j).or_die();
                if (a[0] - b[0]).hypot(a[1] - b[1]) <= 8.0 {
                    brute += 1;
                }
            }
        }
        assert_eq!(report.pairs, brute, "the grid and brute force disagree");
    }

    #[test]
    fn integration_respects_the_speed_limit() {
        let mut pack = Pack::with_capacity(1, params());
        pack.push([0.0, 0.0], [0.0, 0.0]);
        pack.steer(&params());
        // Force a fast desired velocity by hand through the public path.
        pack.integrate(3.0);
        let velocity = pack.velocity(0).or_die();
        let speed = velocity[0].hypot(velocity[1]);
        assert!(speed <= 3.0 + 1e-3, "speed {speed} exceeded the limit");
    }

    #[test]
    fn an_empty_pack_steers_to_nothing() {
        let mut pack = Pack::with_capacity(4, params());
        assert_eq!(pack.steer(&params()), SteerReport::default());
        assert!(pack.is_empty());
    }

    #[test]
    fn unusable_parameters_are_refused_rather_than_dividing_by_zero() {
        let mut pack = Pack::with_capacity(2, params());
        pack.push([1.0, 1.0], [0.0, 0.0]);
        pack.push([1.2, 1.0], [0.0, 0.0]);
        let bad = SteerParams {
            radius: 0.0,
            ..params()
        };
        assert_eq!(pack.steer(&bad), SteerReport::default());
        assert!(!bad.is_usable());
    }

    /// The vectorised path has to be the scalar path, not merely close to it.
    ///
    /// Every lane accumulates in the same cell order over the same neighbours, and a masked
    /// contribution adds exactly zero, so the four-wide kernel should land on the same bits.
    /// If this ever fails while the wide kernel still looks plausible, the masking is wrong.
    #[cfg(feature = "simd")]
    #[test]
    fn the_vectorised_kernel_agrees_with_the_scalar_one() {
        let params = params();
        let mut packed = Pack::with_capacity(256, params);
        let mut scalar = Pack::with_capacity(256, params);
        let mut state = 0x5EED_u64;
        for _ in 0..256 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let x = ((state >> 33) as f32 / (u32::MAX >> 1) as f32) * params.extent;
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let y = ((state >> 33) as f32 / (u32::MAX >> 1) as f32) * params.extent;
            packed.push([x, y], [0.5, 0.25]);
            scalar.push([x, y], [0.5, 0.25]);
        }

        let wide_report = packed.steer(&params);

        // The reference: the same grid, one agent at a time.
        scalar.build_grid();
        let mut reference = SteerReport::default();
        for index in 0..scalar.positions.len() {
            scalar.steer_one(index, &params, &mut reference);
        }

        assert_eq!(wide_report.pairs, reference.pairs, "pair counts diverged");
        assert_eq!(wide_report.steered, reference.steered);
        assert_eq!(wide_report.isolated, reference.isolated);
        for index in 0..scalar.positions.len() {
            let wide = packed.desired(index);
            let narrow = scalar.desired(index);
            assert_eq!(wide, narrow, "lane {index} diverged from the scalar path");
        }
    }
}
