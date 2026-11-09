# 0045, Reciprocal local avoidance: half the responsibility each

- **Status:** Accepted
- **Date:** 2026-11-05
- **Supersedes:** nothing
- **Related:** ADR 0026 (cell-major kernel), ADR 0030 (transposed road), ADR 0044 (threaded steering)

## Context

Boids separation is a force. It pushes agents apart *after* they are already too close, it fights
cohesion and seek for the same velocity budget, and it has no idea where anyone is going: two
agents walking at each other in a corridor both slow down (or neither does), and the pair ends up
sharing space. Every engine a player compares us to, Unreal's Detour crowd, Unity's
NavMeshAgent avoidance, RVO2, ORCA, solves this as a *velocity* problem over a time horizon.
Without it, a crowd library looks wrong in the one situation players test first: walking at
someone.

The competitive research in `docs/dev/tuning/research/competitive-analysis.md` named this the largest
behavioural gap.

## Decision

A reciprocal local-avoidance pass runs over the neighbour set the steering kernel already
gathers, and corrects each agent's *desired* velocity for the frame.

### The solve

For a pair `(a, b)` with positions `pa, pb`, intended velocities `va, vb` and priorities
`wa, wb`:

1. `n = normalize(pb - pa)`, `d = |pb - pa|`.
2. `closing = (va - vb) · n`. A negative closing is a separating pair: nothing to do.
3. `allowed = max(0, d - separation) / horizon`. This is the closing speed that lands the pair
   exactly on the separation floor at the end of the horizon.
4. If `closing <= allowed`, nothing to do.
5. Otherwise remove `closing - allowed` from the pair's relative closing speed, split by
   priority: `Δa = (closing - allowed) · wb / (wa + wb)`, `Δb = (closing - allowed) · wa / (wa + wb)`.
6. Apply `va -= Δa · n`, `vb += Δb · n`, each clamped to `max_adjust`.

Because each agent is corrected by its own share, the pair resolves at the combined rate without
either agent needing to know what the other decided, the reciprocal assumption, and the reason
two agents do not both swerve the same way.

In the kernel the pair is visited twice (once from each agent). Both visits compute the same
`closing` and the same split, from the same pre-pass snapshot, so the result is independent of
processing order. That snapshot is what makes the pass safe to split across threads.

### Where it runs

After `Crowd::steer`, inside the same `steer_flock` system, on the same cell order. Not during
steering: correcting a desired velocity before its neighbours have been computed would make the
outcome depend on who was processed first, and the existing "wide road equals scalar road to the
bit" tests would fail for a reason nobody could debug.

`SteeringRoad` chooses the road the same way it does for steering: `Threads` and pool-worthy
`Auto` crowds use `avoid_parallel`, `Scalar`/`Wide` use `avoid_scalar`, and a parity test holds
the two to bit equality.

### Priority

`NpcAvoidancePriority` is an optional component, defaulting to 1.0. A charging guard is 4.0, a
fleeing villager 0.5. Values are clamped to `0.05..=20.0`; a non-finite or non-positive value
falls back to the default rather than creating an agent nothing can avoid. `AvoidanceParams` on
`BoidsConfig` carries the crowd-wide default.

The pass is **on by default** (`BoidsConfig::with_avoidance(Some(AvoidanceParams::new()))`).
A crowd library where walking through people is the default is a crowd library that ships the
bug; a game that solves separation itself turns it off with `None`.

### Allocation

The pass reuses the kernel's cell order and keeps one extra buffer (`corrected`), swapped with
`desired` each frame so both stay allocated. No allocation after warm-up; `tests/crowd_no_alloc.rs`
counts zero in a steady frame.

## Measurements

`cargo bench --bench crowd -- crowd_avoidance`, 1,000-iteration Criterion run on the project
sandbox, `--release`, default features (`simd` + `threads`):

| Population | `steer_only` | `steer_and_avoid` | Avoidance alone |
| ---------: | -----------: | ----------------: | --------------: |
|     10,000 |     2.25 ms  |          3.27 ms  |       +1.01 ms  |
|    100,000 |    25.74 ms  |         38.08 ms  |      +12.34 ms  |

The parallel pass cut the 10,000-agent cost from 3.96 ms to 1.01 ms (−75%) against the first,
scalar implementation, and the 100,000 case from ~50 ms to 12.3 ms. The remaining cost is
neighbour pairs, not arithmetic: it scales with density, which is why the plan calls this a
cognition-tier cost and why the band architecture leaves ambient agents unsteered (they are
avoided, but do not avoid).

## Consequences

- Two agents walking at each other now stop at the separation floor instead of passing through,
  at 45% of a 10,000-agent steering frame.
- `CrowdReport` gained `avoided` and `avoid_pairs`, and `FlockSteerReport` exposes them, so a
  game can see the pass working without instrumenting it.
- The desired velocity written to `NpcDesiredVelocity` is now the full 3D kernel output. It was
  previously flattened to `(x, 0, y)`, which silently dropped vertical and lateral motion; that
  bug is fixed here because avoidance is meaningless if the velocity it corrects is not the one
  the integrator receives.
- A game with 100,000 steered agents and no tiering pays 12 ms for avoidance alone. The band
  architecture already caps steered agents; this pass inherits that cap.

## Alternatives rejected

- **Full ORCA linear program.** Four times the arithmetic for a difference nobody sees in a
  crowd, and it does not fit the 100,000-agent frame. The projection degrades gracefully exactly
  where ORCA is better (three or more agents converging on a point): they slow and slide rather
  than forming a perfect queue.
- **Sampling candidate velocities.** Cheap, and it breaks determinism unless every sample comes
  from the per-NPC stream. A closed-form correction has no such problem.
- **Post-integration position correction.** Pushing agents apart after they have moved makes them
  jitter against walls, and it cannot prevent the overlap that already happened.
- **Avoiding only manifested agents.** Ambient agents are the crowd a player walks *through*;
  excluding them from the neighbour set is what makes a dense market square look like it has
  invisible walls. They are avoided, they just do not steer.
