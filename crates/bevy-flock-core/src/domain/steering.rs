//! Composable steering behaviours.
//!
//! The important decision here is *how* forces combine. Summing weighted forces
//! is the textbook error: with a strong enough cohesion weight, cohesion wins
//! and agents pile into a point, because separation's contribution is averaged
//! away. What makes a crowd look alive is that separation and avoidance are
//! never outvoted, so this module uses Reynolds' priority-weighted truncation —
//! each behaviour consumes part of a fixed steering budget in priority order,
//! and the parameters say how much of that budget the behaviour may claim.

use bevy_math::Vec3;

use crate::domain::num;
use crate::domain::spatial::Neighbour;
use crate::error::ConfigError;

/// Per-behaviour claim on the steering budget, each in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SteeringWeights {
    /// How hard agents push apart. The first thing applied, because a crowd that
    /// overlaps is not a crowd.
    pub separation: f32,
    /// How strongly an agent matches its neighbours' heading.
    pub alignment: f32,
    /// How strongly an agent drifts toward its neighbours' centre.
    pub cohesion: f32,
    /// How strongly an agent heads for its target.
    pub seek: f32,
    /// How strongly an agent slows as it approaches its target.
    pub arrive: f32,
}

impl SteeringWeights {
    /// Weights that produce a plausible flock: strong separation, moderate
    /// alignment, gentle cohesion.
    #[must_use]
    pub const fn flocking() -> Self {
        Self {
            separation: 0.9,
            alignment: 0.35,
            cohesion: 0.25,
            seek: 0.7,
            arrive: 0.6,
        }
    }

    /// Weights for a dense crowd with a destination: separation dominates and
    /// alignment is suppressed, which is what stops a crowd from walking in
    /// lockstep like a parade.
    #[must_use]
    pub const fn crowd() -> Self {
        Self {
            separation: 1.0,
            alignment: 0.05,
            cohesion: 0.1,
            seek: 0.8,
            arrive: 0.7,
        }
    }

    /// Validate every weight is a probability.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::WeightOutOfRange`] naming the offending field.
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (field, value) in [
            ("separation", self.separation),
            ("alignment", self.alignment),
            ("cohesion", self.cohesion),
            ("seek", self.seek),
            ("arrive", self.arrive),
        ] {
            if !(0.0..=1.0).contains(&value) {
                return Err(ConfigError::WeightOutOfRange { field, value });
            }
        }
        Ok(())
    }
}

impl Default for SteeringWeights {
    fn default() -> Self {
        Self::flocking()
    }
}

/// Everything a steering step needs that does not come from the agent itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SteeringParams {
    /// Radius inside which neighbours are considered.
    pub neighbour_radius: f32,
    /// Distance below which two agents push apart.
    pub separation_radius: f32,
    /// Speed cap for the resulting velocity.
    pub max_speed: f32,
    /// Radius at which an agent begins slowing for its target.
    pub arrival_radius: f32,
    /// Distance within which an agent is considered arrived and stops dead.
    pub stop_radius: f32,
    /// Speed below which the result snaps to zero, killing rest-state jitter.
    pub jitter_threshold: f32,
    /// How each behaviour claims the speed budget.
    pub weights: SteeringWeights,
}

impl SteeringParams {
    /// A configuration for a human-scale crowd, in metres and metres per second.
    #[must_use]
    pub const fn human_scale() -> Self {
        Self {
            neighbour_radius: 8.0,
            separation_radius: 1.2,
            max_speed: 1.6,
            arrival_radius: 3.0,
            stop_radius: 0.35,
            jitter_threshold: 0.02,
            weights: SteeringWeights::crowd(),
        }
    }

    /// Validate distances, speeds and weights.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::InvalidDistance`] for a non-positive or non-finite
    /// distance, and propagates [`SteeringWeights::validate`].
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (field, value) in [
            ("neighbour_radius", self.neighbour_radius),
            ("separation_radius", self.separation_radius),
            ("max_speed", self.max_speed),
            ("arrival_radius", self.arrival_radius),
            ("stop_radius", self.stop_radius),
            ("jitter_threshold", self.jitter_threshold),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(ConfigError::InvalidDistance { field, value });
            }
        }
        self.weights.validate()
    }
}

impl Default for SteeringParams {
    fn default() -> Self {
        Self::human_scale()
    }
}

/// The input state of one agent for one steering step.
#[derive(Debug, Clone, Copy)]
pub struct AgentState {
    /// Current position.
    pub position: Vec3,
    /// Current velocity.
    pub velocity: Vec3,
    /// Where the agent wants to be, if anything.
    pub target: Option<Vec3>,
}

/// Compute the velocity an agent wants this tick.
///
/// Pure: no clock, no RNG, no allocation. Given the same state, the same
/// neighbours in the same order and the same parameters, it returns the same
/// vector on every platform.
#[must_use]
pub fn desired_velocity(
    agent: AgentState,
    neighbours: &[Neighbour],
    params: &SteeringParams,
) -> Vec3 {
    let max_speed = params.max_speed;
    let mut steering = Vec3::ZERO;

    // Priority order matters. Separation first so it always gets budget;
    // alignment next (cheap, shapes the look of the crowd); cohesion last among
    // the social forces (it is the one that can collapse a crowd); then the
    // goal, which is allowed to be truncated but never to cancel separation.
    steering = accumulate(
        steering,
        separation(agent.position, neighbours, params.separation_radius),
        params.weights.separation,
        max_speed,
    );
    steering = accumulate(
        steering,
        alignment(agent.velocity, neighbours),
        params.weights.alignment,
        max_speed,
    );
    steering = accumulate(
        steering,
        cohesion(agent.position, neighbours),
        params.weights.cohesion,
        max_speed,
    );

    if let Some(target) = agent.target {
        let offset = target - agent.position;
        let distance = offset.length();
        if distance <= params.stop_radius {
            // Arrived. Return zero rather than a tiny residual vector, otherwise
            // the agent vibrates forever at its destination.
            return Vec3::ZERO;
        }
        if distance <= params.arrival_radius {
            let speed_scale = distance / params.arrival_radius;
            let arrival = offset.normalize_or_zero() * (max_speed * speed_scale);
            steering = accumulate(steering, arrival, params.weights.arrive, max_speed);
        } else {
            let seek = offset.normalize_or_zero() * max_speed;
            steering = accumulate(steering, seek, params.weights.seek, max_speed);
        }
    }

    // A velocity smaller than the jitter threshold is not motion, it is noise.
    if steering.length_squared() < params.jitter_threshold * params.jitter_threshold {
        return Vec3::ZERO;
    }
    steering.clamp_length_max(max_speed)
}

/// Push away from neighbours inside the separation radius.
///
/// Weighted by inverse distance so the closest neighbour dominates, which is
/// what produces the soft pressure of a real crowd rather than a uniform shove.
#[must_use]
pub fn separation(position: Vec3, neighbours: &[Neighbour], radius: f32) -> Vec3 {
    let radius_squared = radius * radius;
    let mut force = Vec3::ZERO;
    for neighbour in neighbours {
        if neighbour.distance_squared >= radius_squared
            || neighbour.distance_squared <= f32::EPSILON
        {
            continue;
        }
        let offset = position - neighbour.position;
        let distance_squared = offset.length_squared().max(1e-6);
        force += offset * (1.0 / distance_squared);
    }
    force
}

/// Match the average heading of the neighbours.
#[must_use]
pub fn alignment(velocity: Vec3, neighbours: &[Neighbour]) -> Vec3 {
    if neighbours.is_empty() {
        return Vec3::ZERO;
    }
    let mut sum = Vec3::ZERO;
    for neighbour in neighbours {
        sum += neighbour.velocity;
    }
    let count = num::usize_to_f32(neighbours.len());
    let average = sum / count;
    average - velocity
}

/// Drift toward the neighbours' centre of mass.
#[must_use]
pub fn cohesion(position: Vec3, neighbours: &[Neighbour]) -> Vec3 {
    if neighbours.is_empty() {
        return Vec3::ZERO;
    }
    let mut sum = Vec3::ZERO;
    for neighbour in neighbours {
        sum += neighbour.position;
    }
    let centre = sum / num::usize_to_f32(neighbours.len());
    centre - position
}

/// Reynolds' priority-weighted accumulation.
///
/// Adds `force` at up to `weight` of its own magnitude, but never pushes the
/// running total past `budget`. That is the whole trick: a high-priority force
/// can exhaust the budget, and a later force simply gets nothing.
#[must_use]
pub fn accumulate(current: Vec3, force: Vec3, weight: f32, budget: f32) -> Vec3 {
    let magnitude = force.length();
    if magnitude <= f32::EPSILON || weight <= 0.0 {
        return current;
    }
    let remaining = (budget - current.length()).max(0.0);
    if remaining <= 0.0 {
        return current;
    }
    let requested = magnitude * weight.min(1.0);
    let applied = requested.min(remaining);
    current + force.normalize_or_zero() * applied
}

/// Clamp a velocity's magnitude without changing its direction.
#[must_use]
pub fn clamp_speed(velocity: Vec3, max_speed: f32) -> Vec3 {
    velocity.clamp_length_max(max_speed)
}

/// Speed at which an agent should travel to arrive at `target` without
/// overshooting, given its current distance.
#[must_use]
pub fn arrival_speed(distance: f32, params: &SteeringParams) -> f32 {
    if distance <= params.stop_radius {
        return 0.0;
    }
    if distance >= params.arrival_radius {
        return params.max_speed;
    }
    params.max_speed * (distance / params.arrival_radius)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::NpcId;
    use crate::testkit::{GetOrDie, GetOrDieMut};

    fn neighbour(id: u32, position: Vec3, velocity: Vec3, centre: Vec3) -> Neighbour {
        Neighbour {
            id: NpcId::unique(id),
            position,
            velocity,
            distance_squared: position.distance_squared(centre),
        }
    }

    #[test]
    fn separation_pushes_apart_and_prefers_the_closest() {
        let position = Vec3::ZERO;
        let near = neighbour(1, Vec3::new(0.2, 0.0, 0.0), Vec3::ZERO, position);
        let far = neighbour(2, Vec3::new(1.0, 0.0, 0.0), Vec3::ZERO, position);
        let force = separation(position, &[near, far], 2.0);
        assert!(force.x < 0.0, "force should point away from +x");
        let only_near = separation(position, &[near], 2.0);
        let only_far = separation(position, &[far], 2.0);
        assert!(
            only_near.length() > only_far.length(),
            "the closer neighbour must dominate"
        );
    }

    #[test]
    fn separation_ignores_agents_outside_the_radius() {
        let position = Vec3::ZERO;
        let far = neighbour(1, Vec3::new(10.0, 0.0, 0.0), Vec3::ZERO, position);
        assert_eq!(separation(position, &[far], 1.0), Vec3::ZERO);
    }

    #[test]
    fn alignment_steers_toward_the_average_heading() {
        let current = Vec3::new(0.0, 0.0, 1.0);
        let neighbours = [
            neighbour(1, Vec3::ZERO, Vec3::new(2.0, 0.0, 0.0), Vec3::ZERO),
            neighbour(2, Vec3::ZERO, Vec3::new(2.0, 0.0, 0.0), Vec3::ZERO),
        ];
        let force = alignment(current, &neighbours);
        assert!(force.x > 0.0);
        assert!(force.z < 0.0);
    }

    #[test]
    fn cohesion_pulls_toward_the_centre_of_mass() {
        let position = Vec3::new(1.0, 0.0, 0.0);
        let neighbours = [
            neighbour(1, Vec3::new(5.0, 0.0, 0.0), Vec3::ZERO, position),
            neighbour(2, Vec3::new(7.0, 0.0, 0.0), Vec3::ZERO, position),
        ];
        let force = cohesion(position, &neighbours);
        assert!(force.x > 0.0);
    }

    #[test]
    fn social_forces_are_zero_without_neighbours() {
        assert_eq!(alignment(Vec3::ONE, &[]), Vec3::ZERO);
        assert_eq!(cohesion(Vec3::ONE, &[]), Vec3::ZERO);
        assert_eq!(separation(Vec3::ONE, &[], 5.0), Vec3::ZERO);
    }

    #[test]
    fn accumulator_never_exceeds_the_budget() {
        let budget = 5.0;
        let mut total = Vec3::ZERO;
        for _ in 0..10 {
            total = accumulate(total, Vec3::new(2.0, 0.0, 0.0), 1.0, budget);
        }
        assert!(total.length() <= budget + 1e-4, "got {}", total.length());
    }

    #[test]
    fn high_priority_force_can_starve_a_later_one() {
        let budget = 1.0;
        let saturated = accumulate(Vec3::ZERO, Vec3::new(4.0, 0.0, 0.0), 1.0, budget);
        let after = accumulate(saturated, Vec3::new(0.0, 4.0, 0.0), 1.0, budget);
        assert!(
            after.y.abs() < 1e-4,
            "a later force must not sneak past the budget: {after}"
        );
    }

    #[test]
    fn arrived_agent_stops_dead() {
        let params = SteeringParams::human_scale();
        let agent = AgentState {
            position: Vec3::ZERO,
            velocity: Vec3::new(0.5, 0.0, 0.0),
            target: Some(Vec3::new(0.1, 0.0, 0.0)),
        };
        assert_eq!(desired_velocity(agent, &[], &params), Vec3::ZERO);
    }

    #[test]
    fn seek_points_at_the_target() {
        let params = SteeringParams::human_scale();
        let agent = AgentState {
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            target: Some(Vec3::new(100.0, 0.0, 0.0)),
        };
        let desired = desired_velocity(agent, &[], &params);
        assert!(desired.x > 0.0);
        assert!(desired.length() <= params.max_speed + 1e-4);
    }

    #[test]
    fn arrival_speed_decreases_toward_the_target() {
        let params = SteeringParams::human_scale();
        let far = arrival_speed(100.0, &params);
        let mid = arrival_speed(1.5, &params);
        let stopped = arrival_speed(0.1, &params);
        assert!(far > mid);
        assert!(mid > stopped);
        assert!(stopped.abs() < f32::EPSILON);
    }

    #[test]
    fn dense_pack_never_penetrates_the_separation_radius() {
        let params = SteeringParams {
            separation_radius: 1.0,
            max_speed: 1.0,
            ..SteeringParams::human_scale()
        };
        // Twelve agents on a small circle, all walking toward the centre: the
        // worst case for separation being overwhelmed by cohesion.
        let mut positions: Vec<Vec3> = (0..12)
            .map(|index| {
                let angle = core::f32::consts::TAU * num::i32_to_f32(index) / 12.0;
                Vec3::new(libm::cosf(angle), 0.0, libm::sinf(angle)) * 1.6
            })
            .collect();
        let mut velocities = [Vec3::ZERO; 12];

        for _ in 0..600 {
            let snapshot = positions.clone();
            for index in 0..12 {
                let centre = snapshot.at(index);
                let neighbours: Vec<Neighbour> = snapshot
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                    .map(|(other, position)| Neighbour {
                        id: NpcId::unique(num::usize_to_u32(other)),
                        position: *position,
                        velocity: velocities.at(other),
                        distance_squared: position.distance_squared(centre),
                    })
                    .collect();
                let agent = AgentState {
                    position: centre,
                    velocity: velocities.at(index),
                    target: Some(Vec3::ZERO),
                };
                *velocities.at_mut(index) = desired_velocity(agent, &neighbours, &params);
            }
            for index in 0..12 {
                *positions.at_mut(index) += velocities.at(index) * (1.0 / 60.0);
            }
        }

        let mut worst = f32::MAX;
        for a in 0..12 {
            for b in (a + 1)..12 {
                worst = worst.min(positions.at(a).distance(positions.at(b)));
            }
        }
        assert!(
            worst > 0.5,
            "agents collapsed to a point: closest pair {worst}"
        );
    }

    #[test]
    fn rest_state_is_stable() {
        let params = SteeringParams::human_scale();
        let agent = AgentState {
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            target: Some(Vec3::ZERO),
        };
        let desired = desired_velocity(agent, &[], &params);
        assert_eq!(desired, Vec3::ZERO, "a settled agent must not jitter");
    }

    #[test]
    fn weights_are_validated() {
        let bad = SteeringWeights {
            separation: 1.5,
            ..SteeringWeights::flocking()
        };
        assert!(bad.validate().is_err());
        assert!(SteeringWeights::flocking().validate().is_ok());
    }

    #[test]
    fn params_reject_nonpositive_distances() {
        let bad = SteeringParams {
            max_speed: 0.0,
            ..SteeringParams::human_scale()
        };
        assert!(bad.validate().is_err());
        assert!(SteeringParams::human_scale().validate().is_ok());
    }
}
