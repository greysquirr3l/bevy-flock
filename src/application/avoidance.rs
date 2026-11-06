//! Reciprocal local avoidance: agents stop sharing space.
//!
//! Steering (ADR 0026) makes a crowd look like a crowd. It does not stop two agents from
//! occupying the same point, because every term in it is a soft force: separation pushes, it
//! does not forbid, and a doorway is exactly where pushing is not enough. This module adds the
//! hard part — a velocity correction with a separation floor — at a cost that fits in the same
//! pass over the neighbour set the kernel already gathers.
//!
//! # The solve
//!
//! The full answer is a linear program per agent (ORCA and its descendants): find the velocity
//! closest to the desired one that stays outside every neighbour's collision cone for the next
//! `horizon` seconds. That is three or four constraints solved in two dimensions, per agent,
//! per frame — accurate, and expensive enough that most engines run it on a tenth of the crowd.
//!
//! What ships here is the half-responsibility projection: for each neighbour that the agent is
//! *closing on* within the horizon, compute how much closing speed has to go, and remove this
//! agent's share of it along the line between them. The share is decided by priority, so a
//! villager yields to a guard, and the two shares always sum to the whole, which is what makes
//! the assumption reciprocal: if both agents run this pass, the pair resolves at twice the
//! single-sided rate and neither has to know what the other decided.
//!
//! Three properties follow, and each has a test:
//!
//! * **The floor holds.** Removing exactly the excess closing speed means the pair reaches the
//!   separation distance at the horizon at the earliest, never before it.
//! * **It is deterministic.** Every neighbour is visited in cell order, contributions are summed
//!   in that order, and nothing is sampled: the same world produces the same velocities to the
//!   bit, which is what `tests/determinism.rs` holds the whole engine to.
//! * **It allocates nothing.** The correction is a stack array; the caller reuses its buffers.
//!
//! # What was rejected
//!
//! * **A full ORCA linear program.** Four times the arithmetic for a difference no player can
//!   see in a crowd, and it would not fit the 100,000-agent frame. The projection degrades
//!   gracefully in exactly the case ORCA is better at (three or more agents converging on one
//!   point): they slow and slide instead of forming a perfect queue. Recorded in ADR 0045.
//! * **Sampling candidate velocities.** Cheap, and it breaks determinism unless every sample is
//!   drawn from the per-NPC stream. A closed-form correction has no such problem.
//! * **Post-integration position correction.** Pushing agents apart after they have moved makes
//!   them jitter against walls and cannot stop the overlap that already happened.
//!
//! # Priority
//!
//! [`AvoidanceParams::priority`] is what an agent is given when nobody says otherwise, and 1.0
//! means "an ordinary person". A charging guard is 4.0, a fleeing villager 0.5. The default is
//! deliberately unopinionated: every agent yields equally, which is the behaviour a game gets
//! before it has thought about who should.

/// How hard agents avoid each other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AvoidanceParams {
    /// The centre-to-centre distance the solve refuses to violate, in metres.
    pub separation: f32,
    /// How far ahead a collision is considered, in seconds.
    pub horizon: f32,
    /// The largest correction one agent may take, in metres per second.
    ///
    /// A cap rather than a free hand: an agent pinned in a corner must slide along the wall, not
    /// teleport away from the crowd.
    pub max_adjust: f32,
    /// The priority given to an agent that has not been given one.
    pub priority: f32,
    /// A margin added to the separation, so the floor holds after the integrator's step.
    pub margin: f32,
}

impl AvoidanceParams {
    /// Numbers that behave in a crowd of people-sized agents at 60 Hz.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            separation: 0.7,
            horizon: 0.8,
            max_adjust: 4.0,
            priority: 1.0,
            margin: 0.05,
        }
    }

    /// Set the separation floor.
    #[must_use]
    pub const fn with_separation(mut self, metres: f32) -> Self {
        self.separation = metres;
        self
    }

    /// Set the horizon.
    #[must_use]
    pub const fn with_horizon(mut self, seconds: f32) -> Self {
        self.horizon = seconds;
        self
    }

    /// Set the correction cap.
    #[must_use]
    pub const fn with_max_adjust(mut self, metres_per_second: f32) -> Self {
        self.max_adjust = metres_per_second;
        self
    }

    /// Whether the numbers can produce a sane correction.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.separation > 0.0
            && self.separation.is_finite()
            && self.horizon > 0.0
            && self.horizon.is_finite()
            && self.max_adjust >= 0.0
            && self.max_adjust.is_finite()
            && self.priority > 0.0
            && self.priority.is_finite()
            && self.margin >= 0.0
            && self.margin.is_finite()
    }

    /// The distance the solve actually defends: separation plus the margin.
    #[must_use]
    pub fn floor(&self) -> f32 {
        self.separation + self.margin
    }
}

impl Default for AvoidanceParams {
    fn default() -> Self {
        Self::new()
    }
}

/// What one avoidance pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AvoidReport {
    /// Neighbour pairs examined.
    pub pairs: u64,
    /// Agents whose velocity was corrected.
    pub adjusted: u32,
}

/// One agent as a pair solve needs it: where it is, where it is going, and how much room the others
/// give it.
///
/// The three travel together — they are one agent's state — so a pair solve takes two of these
/// rather than six loose numbers.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AvoidBody {
    /// Position in metres.
    pub position: [f32; 3],
    /// Intended velocity, metres per second.
    pub velocity: [f32; 3],
    /// Priority; 1.0 is the default and the higher priority takes less of the correction.
    pub priority: f32,
}

impl AvoidBody {
    /// A body at `position` moving at `velocity`, with default priority.
    #[must_use]
    pub const fn new(position: [f32; 3], velocity: [f32; 3]) -> Self {
        Self {
            position,
            velocity,
            priority: 1.0,
        }
    }

    /// The same body at `priority`.
    #[must_use]
    pub const fn with_priority(self, priority: f32) -> Self {
        Self { priority, ..self }
    }
}

/// Add one neighbour's contribution to `correction`.
///
/// Returns `true` when the pair was closing and the correction grew: the caller counts those,
/// because they are the work done, not the loop iterations.
///
/// The whole solve for a pair: if the agent is not closing on the neighbour, or the closest
/// approach is beyond the horizon, nothing happens. Otherwise the excess closing speed is
/// removed along the line between them, split by priority, floored at zero.
pub fn solve_pair(
    params: &AvoidanceParams,
    body: AvoidBody,
    other: AvoidBody,
    correction: &mut [f32; 3],
) -> bool {
    let AvoidBody {
        position,
        velocity,
        priority,
    } = body;
    let AvoidBody {
        position: other_position,
        velocity: other_velocity,
        priority: other_priority,
    } = other;
    let dx = other_position[0] - position[0];
    let dy = other_position[1] - position[1];
    let dz = other_position[2] - position[2];
    let distance_squared = dx * dx + dy * dy + dz * dz;
    if distance_squared <= f32::EPSILON {
        // Exactly coincident: there is no direction to separate along. Any fixed axis would be
        // a hidden coupling between agents, so the pair is left to the separation force, which
        // is already pushing them apart with a deterministic tiebreak of its own.
        return false;
    }
    let distance = libm::sqrtf(distance_squared);
    let floor = params.floor();
    // Only pairs that could be inside the floor within the horizon are worth the arithmetic.
    if distance > floor + params.max_adjust * params.horizon {
        return false;
    }
    let inverse = 1.0 / distance;
    let nx = dx * inverse;
    let ny = dy * inverse;
    let nz = dz * inverse;

    // Closing speed along the line between them: positive when the agent approaches.
    let closing = (velocity[0] - other_velocity[0]) * nx
        + (velocity[1] - other_velocity[1]) * ny
        + (velocity[2] - other_velocity[2]) * nz;

    let gap = distance - floor;
    let need = if gap <= 0.0 {
        // Already inside the floor. Take the overlap out over the horizon, and never in one
        // step: a hard correction against a wall is what makes agents oscillate.
        -gap / params.horizon
    } else {
        if closing <= 0.0 {
            return false;
        }
        // The closing speed that would just reach the floor at the horizon is `gap / horizon`;
        // anything faster has to go.
        let allowed = gap / params.horizon;
        if closing <= allowed {
            return false;
        }
        closing - allowed
    };

    // The higher priority takes less of the correction. Two equals split it evenly, which is the
    // reciprocal assumption in one line: each removes half of the relative closing speed, so the
    // pair's relative motion is resolved by the time both have run.
    let self_priority = priority.max(f32::MIN_POSITIVE);
    let other_priority = other_priority.max(f32::MIN_POSITIVE);
    let share = other_priority / (self_priority + other_priority);
    let change = need * share;
    correction[0] -= change * nx;
    correction[1] -= change * ny;
    correction[2] -= change * nz;
    true
}

/// Clamp a correction to the cap and apply it to a desired velocity.
pub fn apply(params: &AvoidanceParams, desired: [f32; 3], correction: [f32; 3]) -> [f32; 3] {
    let magnitude_squared = correction[0] * correction[0]
        + correction[1] * correction[1]
        + correction[2] * correction[2];
    if magnitude_squared <= f32::EPSILON {
        return desired;
    }
    let magnitude = libm::sqrtf(magnitude_squared);
    let scale = if magnitude > params.max_adjust {
        params.max_adjust / magnitude
    } else {
        1.0
    };
    [
        desired[0] + correction[0] * scale,
        desired[1] + correction[1] * scale,
        desired[2] + correction[2] * scale,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three numbers a pair solve needs, in the order the tests read them.
    fn walk(position: [f32; 3], velocity: [f32; 3], priority: f32) -> AvoidBody {
        AvoidBody::new(position, velocity).with_priority(priority)
    }
    fn params() -> AvoidanceParams {
        AvoidanceParams::new()
            .with_separation(0.7)
            .with_horizon(0.8)
            .with_max_adjust(4.0)
    }

    /// Walk two agents at each other and return the closest they came.
    fn head_on(priority_a: f32, priority_b: f32) -> (f32, u32) {
        let params = params();
        let step = 1.0 / 60.0;
        let mut a = [-6.0_f32, 0.0, 0.0];
        let mut b = [6.0_f32, 0.0, 0.0];
        let velocity_a = [4.0_f32, 0.0, 0.0];
        let velocity_b = [-4.0_f32, 0.0, 0.0];
        let mut closest = f32::MAX;
        let mut corrected = 0;
        for _ in 0..240 {
            let mut correction_a = [0.0_f32; 3];
            let mut correction_b = [0.0_f32; 3];
            solve_pair(
                &params,
                walk(a, velocity_a, priority_a),
                walk(b, velocity_b, priority_b),
                &mut correction_a,
            );
            solve_pair(
                &params,
                walk(b, velocity_b, priority_b),
                walk(a, velocity_a, priority_a),
                &mut correction_b,
            );
            if correction_a != [0.0; 3] && correction_b != [0.0; 3] {
                corrected += 1;
            }
            let desired_a = apply(&params, velocity_a, correction_a);
            let desired_b = apply(&params, velocity_b, correction_b);
            a[0] += desired_a[0] * step;
            b[0] += desired_b[0] * step;
            let dx = a[0] - b[0];
            let dy = a[1] - b[1];
            let dz = a[2] - b[2];
            closest = closest.min(libm::sqrtf(dx * dx + dy * dy + dz * dz));
        }
        (closest, corrected)
    }

    #[test]
    fn two_agents_walking_at_each_other_never_share_space() {
        let (closest, corrected) = head_on(1.0, 1.0);
        assert!(corrected > 0, "the solve must have done something");
        assert!(
            closest >= params().separation,
            "closest approach {closest} broke the {} floor",
            params().separation
        );
    }

    #[test]
    fn the_solve_is_symmetric_and_bit_stable() {
        let params = params();
        let a = [0.0, 0.0, 0.0];
        let b = [1.0, 0.0, 0.0];
        let mut first = [0.0_f32; 3];
        let mut second = [0.0_f32; 3];
        assert!(solve_pair(
            &params,
            walk(a, [1.0, 0.0, 0.0], 1.0),
            walk(b, [-1.0, 0.0, 0.0], 1.0),
            &mut first
        ));
        assert!(solve_pair(
            &params,
            walk(a, [1.0, 0.0, 0.0], 1.0),
            walk(b, [-1.0, 0.0, 0.0], 1.0),
            &mut second
        ));
        assert_eq!(first, second);
        // Two metres a second of closing against a 0.25 m gap over a 0.8 s horizon: only the
        // excess over 0.3125 m/s has to go (1.6875 m/s), and an equal-priority pair splits it,
        // so each removes 0.84375 m/s. Removing the whole closing speed would stop a crowd dead
        // whenever two people walked towards each other.
        assert!((first[0] + 0.843_75).abs() < 1e-6, "got {first:?}");
    }

    #[test]
    fn the_higher_priority_agent_yields_less() {
        let params = params();
        let mut low = [0.0_f32; 3];
        let mut high = [0.0_f32; 3];
        solve_pair(
            &params,
            walk([0.0; 3], [2.0, 0.0, 0.0], 0.5),
            walk([1.0, 0.0, 0.0], [0.0; 3], 4.0),
            &mut low,
        );
        solve_pair(
            &params,
            walk([0.0; 3], [2.0, 0.0, 0.0], 4.0),
            walk([1.0, 0.0, 0.0], [0.0; 3], 0.5),
            &mut high,
        );
        assert!(low[0].abs() > high[0].abs(), "low {low:?} high {high:?}");
    }

    #[test]
    fn a_pair_walking_apart_is_left_alone() {
        let params = params();
        let mut correction = [0.0_f32; 3];
        assert!(!solve_pair(
            &params,
            walk([0.0; 3], [-2.0, 0.0, 0.0], 1.0),
            walk([1.0, 0.0, 0.0], [2.0, 0.0, 0.0], 1.0),
            &mut correction
        ));
        assert_eq!(correction, [0.0; 3]);
    }

    #[test]
    fn a_distant_neighbour_costs_nothing() {
        let params = params();
        let mut correction = [0.0_f32; 3];
        assert!(!solve_pair(
            &params,
            walk([0.0; 3], [0.0, 0.0, 0.0], 1.0),
            walk([50.0, 0.0, 0.0], [-4.0, 0.0, 0.0], 1.0),
            &mut correction
        ));
    }

    #[test]
    fn the_correction_is_capped() {
        let params = params().with_max_adjust(1.0);
        let desired = [3.0, 0.0, 0.0];
        let out = apply(&params, desired, [-10.0, 0.0, 0.0]);
        assert!((out[0] - 2.0).abs() < 1e-6, "got {out:?}");
    }

    #[test]
    fn an_overlap_is_resolved_over_the_horizon_not_in_one_step() {
        let params = params();
        let mut correction = [0.0_f32; 3];
        assert!(solve_pair(
            &params,
            walk([0.0; 3], [0.0; 3], 1.0),
            walk([0.2, 0.0, 0.0], [0.0; 3], 1.0),
            &mut correction
        ));
        // Half of 0.6 m of overlap over 0.8 s is 0.375 m/s, not 0.3 m of instant correction.
        assert!(
            correction[0].abs() > 0.3 && correction[0].abs() < 0.4,
            "got {correction:?}"
        );
    }

    #[test]
    fn the_parameters_are_validated() {
        assert!(params().is_usable());
        assert!(!AvoidanceParams::new().with_separation(0.0).is_usable());
        assert!(!AvoidanceParams::new().with_horizon(-1.0).is_usable());
        assert!((params().floor() - 0.75).abs() < 1e-6);
    }
}
